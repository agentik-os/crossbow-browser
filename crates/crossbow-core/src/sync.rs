//! Sync client. Runs on its own thread with a small Tokio runtime and talks to
//! the UI thread through channels, so the UI never blocks on the network.
//!
//! The UI owns the [`Library`](crate::library::Library). A pulled page is not
//! acknowledged until its entities AND cursor have committed to local SQLite.

use crossbow_proto::*;
use futures_util::{SinkExt, StreamExt};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;
use tokio::sync::{Notify, mpsc};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};

/// The library key for the signed-in session (the app's `SessionInfo`); its token
/// is kept in the token store, not in the library.
pub const SESSION_KEY: &str = "sync_session";
/// The library key for the sync server this device uses.
pub const BASE_URL_KEY: &str = "sync_base_url";
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const LOGOUT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PUSH_REQUESTS: usize = 32;

/// A dev sync server's `DEV_CODE_KEY`, from the environment, so that server
/// returns sign-in codes as `dev_code`. Sent only on sign-in starts to that
/// one server; never stored, logged or shown.
#[derive(Clone)]
pub struct DevKey {
    base_url: String,
    key: String,
}

impl DevKey {
    /// From `CROSSBOW_API_URL` and `CROSSBOW_DEV_KEY`: both must be set.
    pub fn from_vars(api_url: Option<String>, key: Option<String>) -> Option<DevKey> {
        let base_url = api_url?.trim_end_matches('/').to_string();
        let key = key?.trim().to_string();
        (!base_url.is_empty() && !key.is_empty()).then_some(DevKey { base_url, key })
    }
}

#[derive(Clone, Debug)]
pub struct Session {
    pub base_url: String,
    pub token: String,
    pub account: Account,
    pub device: Device,
    /// The name a browser sign-in's provider gave, if any.
    pub display_name: Option<String>,
}

#[derive(Debug)]
pub enum SyncCommand {
    StartAuth { base_url: String, email: String },
    VerifyAuth { base_url: String, challenge_id: String, code: String, device_name: String, platform: String },
    /// Redeem a browser sign-in's code at the sync service, which signs in
    /// as `VerifyAuth` does. A new sign-in: it supersedes any other.
    BrowserSignIn { base_url: String, exchange: ClerkExchangeRequest },
    /// Redeem a browser sign-in to link it to the session's account. The
    /// service holds it until `ConfirmLink`; `attempt` comes back with the
    /// answer.
    LinkBrowserSignIn { attempt: u64, base_url: String, token: String, exchange: ClerkExchangeRequest },
    ConfirmLink { attempt: u64, base_url: String, token: String, link_id: String },
    /// Resume a stored session and start live sync from `cursor`. `sync_epoch`
    /// is the epoch that cursor belongs to; `None` takes the one the first
    /// pull names (a fresh start from cursor 0).
    Resume { base_url: String, token: String, cursor: u64, sync_epoch: Option<u64> },
    Push(Vec<Change>),
    PullNow,
    /// Stop automatic pulls (poll, notifications, retries) until PullNow or
    /// Resume: a locked device would only download the same refused page again.
    /// A notification of a newer change is reported as `RemoteChanged` instead.
    PausePulls,
    /// Local durability acknowledgement, NOT an acknowledgement from the server.
    /// A failed page is retried from the last committed cursor with backoff.
    PullApplied { batch_id: u64, success: bool },
    /// Read the stream from the committed cursor to its end without applying
    /// or acknowledging anything; the cursor never moves. Each page arrives
    /// as `Scanned`.
    ScanAhead,
    /// Delete the account's sync data on the server, then read it back. Only
    /// a verified delete stops sync here (`SyncDataDeleted { here: true }`);
    /// anything else is `DeleteFailed` and changes nothing.
    DeleteSyncData,
    /// Email a code to `email` to confirm Delete Account. It touches no
    /// session or sign-in in progress; `attempt` comes back with the answer.
    StepUpCode { attempt: u64, base_url: String, email: String },
    /// Delete the account `token` signs in to, then read back that the
    /// session is gone. Only that is `AccountDeleted`, and it ends live sync
    /// on this session; anything else is `DeleteAccountFailed`.
    DeleteAccount { attempt: u64, base_url: String, token: String, request: DeleteAccountRequest },
    CancelAuth,
    SignOut,
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncStatus {
    SignedOut,
    Connecting,
    Live,
    Offline(String),
}

#[derive(Debug)]
pub enum SyncEvent {
    AuthStarted { challenge_id: String, dev_code: Option<String> },
    AuthError(String),
    SignedIn(Session),
    /// The service holds a browser sign-in for the account until it is
    /// confirmed; `link_id` is `None` when it is the account's already.
    LinkPending { attempt: u64, link_id: Option<String>, email: Option<String> },
    Linked { attempt: u64 },
    LinkFailed { attempt: u64, error: String },
    Me(MeResponse),
    /// `more` is false when this page reached the server's cursor at pull time.
    Pulled { batch_id: u64, changes: Vec<ServerChange>, cursor: u64, more: bool },
    /// A page read by `ScanAhead`, never to be applied; `more` is false at
    /// the end of the stream.
    Scanned { changes: Vec<ServerChange>, more: bool },
    /// `ScanAhead` stopped before the end of the stream.
    ScanFailed(String),
    /// While pulls are paused, the server notified a change past both the held
    /// cursor and the last scan; once per change. A locked device scans for a
    /// newer marker instead of pulling the refused page again.
    RemoteChanged,
    Pushed { accepted: Vec<Change>, stale: Vec<StaleChange>, cursor: u64 },
    /// These versions remain dirty and local; the UI durably pauses them until
    /// edited or explicitly retried. No payload is discarded or marked clean.
    PushRejected { change: Change, reason: String },
    PushFailed(String),
    PushFinished,
    /// The first pull of a fresh start named the account's sync epoch; keep it
    /// with the cursor. Sent before that page's `Pulled`.
    SyncEpoch(u64),
    /// The account's sync data is gone from the server: deleted by this
    /// device (`here`) or another. Sync stopped and nothing more transfers;
    /// the session stays.
    SyncDataDeleted { here: bool },
    /// The delete didn't happen or couldn't be confirmed; nothing stopped.
    DeleteFailed(String),
    StepUpCode { attempt: u64, challenge_id: String, dev_code: Option<String> },
    /// The account is gone from the sync server, and its sessions with it.
    AccountDeleted { attempt: u64 },
    /// Delete Account, or its step-up code, didn't happen or couldn't be
    /// confirmed; `error` is `"<code>: <message>"` as for sign-in.
    DeleteAccountFailed { attempt: u64, error: String },
    Status(SyncStatus),
    SignedOut,
}

pub struct SyncHandle {
    tx: mpsc::UnboundedSender<(u64, SyncCommand)>,
    events: std_mpsc::Receiver<(u64, SyncEvent)>,
    generation: Arc<AtomicU64>,
    invalidated: Arc<Notify>,
}

impl SyncHandle {
    /// Spawn the sync thread. `wake` is called after every event (e.g. to
    /// request a UI repaint).
    pub fn spawn(wake: Arc<dyn Fn() + Send + Sync>) -> SyncHandle {
        Self::spawn_with_dev_key(wake, None)
    }

    /// As [`spawn`](Self::spawn), with a dev server's key for its sign-in starts.
    pub fn spawn_with_dev_key(wake: Arc<dyn Fn() + Send + Sync>, dev_key: Option<DevKey>) -> SyncHandle {
        let (tx, rx) = mpsc::unbounded_channel();
        let (etx, erx) = std_mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = generation.clone();
        let invalidated = Arc::new(Notify::new());
        let worker_invalidated = invalidated.clone();
        std::thread::Builder::new()
            .name("crossbow-sync".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
                let mut worker = Worker::new(rx, etx, wake, worker_generation, worker_invalidated);
                worker.dev_key = dev_key;
                rt.block_on(worker.run());
            })
            .expect("spawn sync thread");
        SyncHandle { tx, events: erx, generation, invalidated }
    }

    pub fn send(&self, cmd: SyncCommand) {
        let invalidates = matches!(cmd, SyncCommand::StartAuth { .. } | SyncCommand::BrowserSignIn { .. } | SyncCommand::Resume { .. } | SyncCommand::CancelAuth | SyncCommand::SignOut | SyncCommand::Shutdown);
        let generation = if invalidates {
            self.generation.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            self.generation.load(Ordering::SeqCst)
        };
        let _ = self.tx.send((generation, cmd));
        if invalidates { self.invalidated.notify_one(); }
    }

    pub fn try_recv(&self) -> Result<SyncEvent, std_mpsc::TryRecvError> {
        loop {
            let (generation, event) = self.events.try_recv()?;
            if generation == self.generation.load(Ordering::SeqCst) { return Ok(event); }
        }
    }
}

impl Drop for SyncHandle {
    fn drop(&mut self) {
        self.send(SyncCommand::Shutdown);
    }
}

struct Worker {
    rx: mpsc::UnboundedReceiver<(u64, SyncCommand)>,
    events: std_mpsc::Sender<(u64, SyncEvent)>,
    generation: u64,
    current_generation: Arc<AtomicU64>,
    invalidated: Arc<Notify>,
    wake: Arc<dyn Fn() + Send + Sync>,
    http: reqwest::Client,
    live: Option<Live>,
    pending_pull: Option<PendingPull>,
    next_batch_id: u64,
    pull_again: bool,
    pull_retry_at: Option<tokio::time::Instant>,
    pull_backoff: Duration,
    pulls_paused: bool,
    /// The server cursor the last complete scan read to, or the last change
    /// reported while paused, so each change is reported once.
    reported_to: u64,
    /// The epoch this device's own delete started, so a pull that names it
    /// after an unconfirmed delete is known as this device's.
    deleted_to: Option<u64>,
    dev_key: Option<DevKey>,
}

struct PendingPull {
    batch_id: u64,
    cursor: u64,
    more: bool,
}

struct Live {
    base_url: String,
    token: String,
    device_id: String,
    /// Only the locally committed cursor, never a merely downloaded cursor.
    cursor: u64,
    ready: bool,
    /// The sync epoch of the data this device holds; `None` until a fresh
    /// start's first pull names it. Nothing is pushed before then.
    sync_epoch: Option<u64>,
}

impl Worker {
    fn new(
        rx: mpsc::UnboundedReceiver<(u64, SyncCommand)>,
        events: std_mpsc::Sender<(u64, SyncEvent)>,
        wake: Arc<dyn Fn() + Send + Sync>,
        current_generation: Arc<AtomicU64>,
        invalidated: Arc<Notify>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("Crossbow/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("http client");
        Worker {
            rx, events, wake, http, live: None, generation: 0, current_generation, invalidated,
            pending_pull: None, next_batch_id: 0, pull_again: false, pull_retry_at: None,
            pull_backoff: Duration::from_secs(1), pulls_paused: false, reported_to: 0, deleted_to: None, dev_key: None,
        }
    }

    fn is_current(&self) -> bool {
        self.generation == self.current_generation.load(Ordering::SeqCst)
    }

    fn emit(&self, e: SyncEvent) {
        if self.is_current() {
            let _ = self.events.send((self.generation, e));
            (self.wake)();
        }
    }

    /// Commands still execute serially, but generation invalidation interrupts
    /// any network wait immediately. Notify stores a permit across the race
    /// between the atomic check and the await; stale permits are harmless.
    async fn current<F: Future>(&self, operation: F) -> Option<F::Output> {
        tokio::select! {
            biased;
            _ = async {
                loop {
                    let notified = self.invalidated.notified();
                    if !self.is_current() { return; }
                    notified.await;
                }
            } => None,
            value = operation => Some(value),
        }
    }

    fn reset_pull(&mut self) {
        self.pending_pull = None;
        self.pull_again = false;
        self.pull_retry_at = None;
        self.pull_backoff = Duration::from_secs(1);
        self.pulls_paused = false;
        self.reported_to = 0;
    }

    fn invalidate_session(&mut self) {
        self.live = None;
        self.reset_pull();
        // Reject already queued data events/commands from the revoked session,
        // just as an explicit sign-out does. Do not invalidate a newer login.
        if self.current_generation.compare_exchange(self.generation, self.generation + 1, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            self.generation += 1;
            self.emit(SyncEvent::SignedOut);
            self.emit(SyncEvent::Status(SyncStatus::SignedOut));
            self.emit(SyncEvent::AuthError("Your session was rejected. Sign in again.".into()));
        }
    }

    fn revoke_later(&self, live: Live) {
        let http = self.http.clone();
        tokio::spawn(async move { revoke(&http, &live.base_url, &live.token).await; });
    }

    async fn run(mut self) {
        let mut ws: Option<WsStream> = None;
        let mut backoff = Duration::from_secs(1);
        let mut reconnect_at: Option<tokio::time::Instant> = None;
        let mut poll = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(60), Duration::from_secs(60));
        loop {
            if !self.is_current() || self.live.is_none() {
                ws = None;
                reconnect_at = None;
            }
            tokio::select! {
                cmd = self.rx.recv() => {
                    let Some((generation, cmd)) = cmd else { return };
                    if generation != self.current_generation.load(Ordering::SeqCst) { continue; }
                    if generation != self.generation {
                        ws = None;
                        reconnect_at = None;
                        backoff = Duration::from_secs(1);
                        self.reset_pull();
                        self.generation = generation;
                    }
                    match cmd {
                        SyncCommand::Shutdown => return,
                        SyncCommand::CancelAuth => {
                            if let Some(l) = self.live.take() { self.revoke_later(l); }
                        }
                        SyncCommand::SignOut => {
                            if let Some(l) = self.live.take() { self.revoke_later(l); }
                            self.emit(SyncEvent::SignedOut);
                            self.emit(SyncEvent::Status(SyncStatus::SignedOut));
                        }
                        SyncCommand::StartAuth { base_url, email } => {
                            if let Some(l) = self.live.take() { self.revoke_later(l); }
                            self.start_auth(&base_url, &email).await;
                        }
                        SyncCommand::VerifyAuth { base_url, challenge_id, code, device_name, platform } => {
                            let input = AuthVerifyRequest { challenge_id, code: code.trim().into(), device_name, platform };
                            self.sign_in(base_url, "/v1/auth/verify", input).await;
                        }
                        SyncCommand::BrowserSignIn { base_url, exchange } => {
                            if let Some(l) = self.live.take() { self.revoke_later(l); }
                            self.sign_in(base_url, "/v1/auth/clerk", exchange).await;
                        }
                        SyncCommand::LinkBrowserSignIn { attempt, base_url, token, exchange } => {
                            let result = self.current(async {
                                parse_session::<ClerkLinkResponse>(self.http.post(format!("{base_url}/v1/auth/clerk/link")).bearer_auth(&token).json(&exchange).send().await).await
                            }).await;
                            match result {
                                Some(Ok(r)) => self.emit(SyncEvent::LinkPending { attempt, link_id: r.link_id, email: r.email }),
                                Some(Err(error)) => self.emit(SyncEvent::LinkFailed { attempt, error }),
                                None => {}
                            }
                        }
                        SyncCommand::ConfirmLink { attempt, base_url, token, link_id } => {
                            let result = self.current(async {
                                let res = self.http.post(format!("{base_url}/v1/auth/clerk/link/confirm")).bearer_auth(&token).json(&ClerkLinkConfirmRequest { link_id }).send().await;
                                match res {
                                    Ok(res) if res.status().is_success() => Ok(()),
                                    res => parse_session::<serde_json::Value>(res).await.map(|_| ()),
                                }
                            }).await;
                            match result {
                                Some(Ok(())) => self.emit(SyncEvent::Linked { attempt }),
                                Some(Err(error)) => self.emit(SyncEvent::LinkFailed { attempt, error }),
                                None => {}
                            }
                        }
                        SyncCommand::Resume { base_url, token, cursor, sync_epoch } => {
                            self.pulls_paused = false;
                            self.deleted_to = None;
                            self.live = Some(Live { base_url, token, device_id: String::new(), cursor, ready: true, sync_epoch });
                            self.emit(SyncEvent::Status(SyncStatus::Connecting));
                            if self.refresh_me().await {
                                self.pull_all().await;
                                if self.live.is_some() && self.is_current() { reconnect_at = Some(tokio::time::Instant::now()); }
                            }
                        }
                        SyncCommand::Push(changes) => self.push(changes).await,
                        SyncCommand::PullNow => self.pull_now().await,
                        SyncCommand::PausePulls => self.pulls_paused = true,
                        SyncCommand::PullApplied { batch_id, success } => self.acknowledge_pull(batch_id, success).await,
                        SyncCommand::ScanAhead => self.scan_ahead().await,
                        SyncCommand::DeleteSyncData => self.delete_sync_data().await,
                        SyncCommand::StepUpCode { attempt, base_url, email } => self.step_up_code(attempt, &base_url, &email).await,
                        SyncCommand::DeleteAccount { attempt, base_url, token, request } => self.delete_account(attempt, &base_url, &token, &request).await,
                    }
                }
                msg = next_ws(&mut ws) => {
                    match msg {
                        Some(Ok(tungstenite::Message::Text(t))) => {
                            match serde_json::from_str::<ServerEvent>(&t) {
                                Ok(ServerEvent::Hello { cursor }) | Ok(ServerEvent::Changed { cursor, .. }) => {
                                    backoff = Duration::from_secs(1);
                                    self.server_cursor(cursor).await;
                                }
                                Err(e) => log::debug!("ignoring ws message: {e}"),
                            }
                        }
                        Some(Ok(tungstenite::Message::Ping(p))) => {
                            if let Some(s) = ws.as_mut() {
                                let _ = self.current(tokio::time::timeout(WS_CONNECT_TIMEOUT, s.send(tungstenite::Message::Pong(p)))).await;
                            }
                        }
                        Some(Ok(tungstenite::Message::Close(Some(frame)))) if u16::from(frame.code) == 4401 => self.invalidate_session(),
                        Some(Ok(tungstenite::Message::Close(_))) | Some(Err(_)) | None => {
                            ws = None;
                            if self.live.is_some() {
                                self.emit(SyncEvent::Status(SyncStatus::Offline("connection lost".into())));
                                reconnect_at = Some(tokio::time::Instant::now() + backoff);
                                backoff = (backoff * 2).min(Duration::from_secs(60));
                            }
                        }
                        Some(Ok(_)) => {}
                    }
                }
                _ = sleep_until(reconnect_at) => {
                    reconnect_at = None;
                    if let Some(l) = &self.live {
                        if l.ready {
                            match self.current(connect_ws(&l.base_url, &l.token)).await {
                                Some(Ok(s)) => {
                                    ws = Some(s);
                                    self.emit(SyncEvent::Status(SyncStatus::Live));
                                }
                                Some(Err(e)) if unauthorized(&e) => self.invalidate_session(),
                                Some(Err(e)) => {
                                    self.emit(SyncEvent::Status(SyncStatus::Offline(e)));
                                    reconnect_at = Some(tokio::time::Instant::now() + backoff);
                                    backoff = (backoff * 2).min(Duration::from_secs(60));
                                }
                                None => {}
                            }
                        }
                    }
                }
                _ = sleep_until(self.pull_retry_at) => {
                    self.pull_retry_at = None;
                    self.pull_auto().await;
                }
                _ = poll.tick() => self.pull_auto().await,
            }
        }
    }

    /// `POST /v1/auth/start`, with the dev key only when it is this server's.
    fn auth_start(&self, base_url: &str, email: &str) -> reqwest::RequestBuilder {
        let request = self.http.post(format!("{base_url}/v1/auth/start")).json(&AuthStartRequest { email: email.into() });
        let Some(dev) = self.dev_key.as_ref().filter(|dev| dev.base_url == base_url) else { return request };
        match reqwest::header::HeaderValue::from_str(&dev.key) {
            Ok(mut value) => {
                value.set_sensitive(true);
                request.header(DEV_KEY_HEADER, value)
            }
            Err(_) => request,
        }
    }

    async fn start_auth(&self, base_url: &str, email: &str) {
        let result = self.current(async {
            parse::<AuthStartResponse>(self.auth_start(base_url, email).send().await).await
        }).await;
        match result {
            Some(Ok(r)) => self.emit(SyncEvent::AuthStarted { challenge_id: r.challenge_id, dev_code: r.dev_code }),
            Some(Err(e)) => self.emit(SyncEvent::AuthError(e)),
            None => {}
        }
    }

    /// Post a sign-in (an email code or a browser sign-in's code) and keep
    /// its session, not yet ready: only Resume, after the app has stored the
    /// credential, transfers anything. A session that arrives after the
    /// sign-in was canceled is revoked unused.
    async fn sign_in(&mut self, base_url: String, path: &'static str, input: impl serde::Serialize + Send + 'static) {
        let Some(session) = self.redeem(&base_url, path, input).await else { return };
        if !self.is_current() {
            let http = self.http.clone();
            tokio::spawn(async move { revoke(&http, &session.base_url, &session.token).await; });
            return;
        }
        self.live = Some(Live { base_url, token: session.token.clone(), device_id: session.device.id.clone(), cursor: 0, ready: false, sync_epoch: None });
        self.emit(SyncEvent::SignedIn(session));
    }

    async fn redeem(&self, base_url: &str, path: &'static str, input: impl serde::Serialize + Send + 'static) -> Option<Session> {
        let http = self.http.clone();
        let url = base_url.to_string();
        // Keep a canceled sign-in alive only for bounded credential cleanup:
        // if the server issued a token, revoke it without ever authorizing data.
        let mut request = tokio::spawn(async move {
            parse::<AuthVerifyResponse>(http.post(format!("{url}{path}")).json(&input).send().await).await
        });
        match self.current(&mut request).await {
            Some(Ok(Ok(r))) => Some(Session { base_url: base_url.into(), token: r.session_token, account: r.account, device: r.device, display_name: r.display_name }),
            Some(Ok(Err(e))) => { self.emit(SyncEvent::AuthError(e)); None }
            Some(Err(_)) => { self.emit(SyncEvent::AuthError("Sign-in request stopped. Try again.".into())); None }
            None => {
                let http = self.http.clone();
                let url = base_url.to_string();
                tokio::spawn(async move {
                    if let Ok(Ok(r)) = request.await { revoke(&http, &url, &r.session_token).await; }
                });
                None
            }
        }
    }

    async fn refresh_me(&mut self) -> bool {
        let Some(l) = &self.live else { return false };
        let result = self.current(async {
            parse_session::<MeResponse>(self.http.get(format!("{}/v1/me", l.base_url)).bearer_auth(&l.token).send().await).await
        }).await;
        match result {
            Some(Ok(me)) => {
                if let Some(l) = self.live.as_mut() { l.device_id = me.device.id.clone(); }
                self.emit(SyncEvent::Me(me));
                true
            }
            Some(Err(e)) if unauthorized(&e) => { self.invalidate_session(); false }
            Some(Err(e)) => {
                self.emit(SyncEvent::Status(SyncStatus::Offline(e)));
                true // Keep the session; authenticated pull/reconnect can retry.
            }
            None => false,
        }
    }

    /// An explicit request: it also ends a pause.
    async fn pull_now(&mut self) {
        self.pulls_paused = false;
        self.pull_all().await;
    }

    /// Polls, notifications and retries, unless paused.
    async fn pull_auto(&mut self) {
        if !self.pulls_paused { self.pull_all().await; }
    }

    /// A WebSocket Hello or Changed naming the server's cursor. While paused,
    /// a change the last scan didn't read is reported instead of pulled.
    async fn server_cursor(&mut self, cursor: u64) {
        if !self.live.as_ref().is_some_and(|l| cursor > l.cursor) { return; }
        if !self.pulls_paused {
            self.pull_all().await;
        } else if cursor > self.reported_to {
            self.reported_to = cursor;
            self.emit(SyncEvent::RemoteChanged);
        }
    }

    /// At most one unacknowledged page. Notifications received while the UI is
    /// applying it are coalesced, not allowed to move the durable cursor ahead.
    async fn pull_all(&mut self) {
        let Some(l) = &self.live else { return };
        if !l.ready || !self.is_current() { return; }
        if self.pending_pull.is_some() { self.pull_again = true; return; }
        self.pull_retry_at = None;
        let url = format!("{}/v1/sync/pull?since={}&limit={}", l.base_url, l.cursor, MAX_PULL_CHANGES);
        let result = self.current(async {
            parse_session::<PullResponse>(self.http.get(url).bearer_auth(&l.token).send().await).await
        }).await;
        match result {
            Some(Ok(p)) => {
                if !valid_page(&p, self.live.as_ref().unwrap().cursor) {
                    self.emit(SyncEvent::Status(SyncStatus::Offline("bad response: invalid pull cursor".into())));
                    self.retry_pull();
                    return;
                }
                if !self.same_epoch(p.sync_epoch) { return; }
                self.next_batch_id += 1;
                self.pending_pull = Some(PendingPull { batch_id: self.next_batch_id, cursor: p.cursor, more: p.more });
                self.emit(SyncEvent::Pulled { batch_id: self.next_batch_id, changes: p.changes, cursor: p.cursor, more: p.more });
            }
            Some(Err(e)) if unauthorized(&e) => self.invalidate_session(),
            Some(Err(e)) => {
                self.emit(SyncEvent::Status(SyncStatus::Offline(e)));
                self.retry_pull();
            }
            None => {}
        }
    }

    /// Page forward from the committed cursor with a scan position of its
    /// own, until the end of the stream. Nothing is applied or acknowledged
    /// and the committed cursor never moves, so a device that refuses the page
    /// it holds can still learn what lies beyond it. The UI receives each page.
    async fn scan_ahead(&mut self) {
        let Some(mut since) = self.live.as_ref().filter(|l| l.ready).map(|l| l.cursor) else {
            self.emit(SyncEvent::ScanFailed("not connected".into()));
            return;
        };
        loop {
            let Some(l) = &self.live else { return };
            if !self.is_current() { return; }
            let url = format!("{}/v1/sync/pull?since={since}&limit={}", l.base_url, MAX_PULL_CHANGES);
            let result = self.current(async {
                parse_session::<PullResponse>(self.http.get(url).bearer_auth(&l.token).send().await).await
            }).await;
            match result {
                Some(Ok(p)) if valid_page(&p, since) => {
                    if !self.same_epoch(p.sync_epoch) { return; }
                    since = p.cursor;
                    let more = p.more;
                    self.emit(SyncEvent::Scanned { changes: p.changes, more });
                    if !more {
                        self.reported_to = self.reported_to.max(since);
                        return;
                    }
                }
                Some(Ok(_)) => {
                    self.emit(SyncEvent::ScanFailed("bad response: invalid pull cursor".into()));
                    return;
                }
                Some(Err(e)) if unauthorized(&e) => {
                    self.invalidate_session();
                    return;
                }
                Some(Err(e)) => {
                    self.emit(SyncEvent::ScanFailed(e));
                    return;
                }
                None => return,
            }
        }
    }

    fn retry_pull(&mut self) {
        self.pull_retry_at = Some(tokio::time::Instant::now() + self.pull_backoff);
        self.pull_backoff = (self.pull_backoff * 2).min(Duration::from_secs(60));
    }

    async fn acknowledge_pull(&mut self, batch_id: u64, success: bool) {
        if !self.is_current() || !self.pending_pull.as_ref().is_some_and(|p| p.batch_id == batch_id) { return; }
        let page = self.pending_pull.take().unwrap();
        if !success {
            self.retry_pull();
            return;
        }
        let Some(l) = self.live.as_mut() else { return };
        l.cursor = page.cursor;
        self.pull_backoff = Duration::from_secs(1);
        self.pull_retry_at = None;
        let again = page.more || self.pull_again;
        self.pull_again = false;
        if again { self.pull_all().await; }
    }

    async fn push(&mut self, changes: Vec<Change>) {
        self.push_batches(changes).await;
        self.emit(SyncEvent::PushFinished);
    }

    async fn push_batches(&mut self, changes: Vec<Change>) {
        if !self.live.as_ref().is_some_and(|l| l.ready) || !self.is_current() { return; }
        // A fresh start uploads only after its first pull named the epoch.
        if self.live.as_ref().is_some_and(|l| l.sync_epoch.is_none()) {
            self.emit(SyncEvent::PushFailed("Waiting for the first sync before uploading.".into()));
            return;
        }
        let mut valid = Vec::new();
        for change in changes {
            if let Some(reason) = invalid_outgoing(&change) {
                self.emit(SyncEvent::PushRejected { change, reason: reason.into() });
            } else { valid.push(change); }
        }
        let mut batches: Vec<_> = valid.chunks(MAX_PUSH_CHANGES).rev().map(<[Change]>::to_vec).collect();
        let mut requests = 0;
        while let Some(chunk) = batches.pop() {
            let Some(l) = &self.live else { return };
            if !l.ready || !self.is_current() { return; }
            if requests == MAX_PUSH_REQUESTS {
                self.emit(SyncEvent::PushFailed("Sync retry limit reached; local changes kept. Retry shortly.".into()));
                return;
            }
            requests += 1;
            let result = self.current(async {
                parse_session::<PushResponse>(self.http.post(format!("{}/v1/sync/push", l.base_url))
                    .bearer_auth(&l.token).json(&PushRequest { changes: chunk.clone(), sync_epoch: l.sync_epoch }).send().await).await
            }).await;
            match result {
                Some(Ok(r)) => {
                    // The protocol acknowledges the entire batch or rejects it.
                    // Never invent acceptance for an incomplete/malformed reply.
                    let mut keys = std::collections::HashSet::new();
                    let consistent = r.accepted as usize + r.stale.len() == chunk.len()
                        && r.stale.iter().all(|s| keys.insert((&s.kind, &s.id)) && chunk.iter().any(|c| c.kind == s.kind && c.id == s.id && c.hlc <= s.server_hlc));
                    if !consistent {
                        self.emit(SyncEvent::PushFailed("Sync acknowledgement was incomplete; local changes kept.".into()));
                        return;
                    }
                    // An equal HLC is a replay the server already stores. A
                    // strictly newer remote version must be durably pulled first.
                    let accepted = chunk.into_iter().filter(|c| !r.stale.iter().any(|s| s.kind == c.kind && s.id == c.id && s.server_hlc > c.hlc)).collect();
                    self.emit(SyncEvent::Pushed { accepted, stale: r.stale, cursor: r.cursor });
                }
                Some(Err(e)) if unauthorized(&e) => { self.invalidate_session(); return; }
                Some(Err(e)) if e.starts_with(error_codes::SYNC_EPOCH_CHANGED) => { self.stop_for_deleted_data(false); return; }
                Some(Err(e)) if permanent_push_error(&e) => {
                    if chunk.len() == 1 {
                        self.emit(SyncEvent::PushRejected { change: chunk.into_iter().next().unwrap(), reason: "Sync rejected data; kept locally. Edit it, then Sync Now.".into() });
                    } else {
                        // A batch rejection does NOT identify its offending row.
                        // Split within a request budget; only reject a singleton
                        // after its own rejection. Unsent rows remain dirty.
                        let middle = chunk.len() / 2;
                        batches.push(chunk[middle..].to_vec());
                        batches.push(chunk[..middle].to_vec());
                    }
                }
                Some(Err(e)) => { self.emit(SyncEvent::PushFailed(e)); return; }
                None => return,
            }
        }
    }
}

impl Worker {
    /// A page names the account's sync epoch. A fresh start takes it; a
    /// device that holds another epoch's data holds data that was deleted
    /// from the server, so it stops instead of applying or uploading anything.
    fn same_epoch(&mut self, server: u64) -> bool {
        let Some(held) = self.live.as_ref().map(|l| l.sync_epoch) else { return false };
        match held {
            Some(epoch) if epoch == server => true,
            Some(_) => {
                self.stop_for_deleted_data(self.deleted_to == Some(server));
                false
            }
            None => {
                if let Some(l) = self.live.as_mut() { l.sync_epoch = Some(server); }
                self.emit(SyncEvent::SyncEpoch(server));
                true
            }
        }
    }

    async fn step_up_code(&self, attempt: u64, base_url: &str, email: &str) {
        let result = self.current(async {
            parse::<AuthStartResponse>(self.auth_start(base_url, email).send().await).await
        }).await;
        match result {
            Some(Ok(r)) => self.emit(SyncEvent::StepUpCode { attempt, challenge_id: r.challenge_id, dev_code: r.dev_code }),
            Some(Err(error)) => self.emit(SyncEvent::DeleteAccountFailed { attempt, error }),
            None => {}
        }
    }

    /// Delete the account, then read `/v1/me` back with the same session:
    /// only a `401` there confirms it. A `404` is a server without the
    /// route; a `401` to the delete means the session had ended already,
    /// perhaps by an earlier delete whose answer was lost. Neither, nor any
    /// other refusal, changes anything here.
    async fn delete_account(&mut self, attempt: u64, base_url: &str, token: &str, request: &DeleteAccountRequest) {
        let result = self.current(async {
            let res = self.http.delete(format!("{base_url}/v1/account")).bearer_auth(token).json(request).send().await;
            match res.as_ref().ok().map(|res| res.status().as_u16()) {
                Some(404) => return Err("unsupported: http 404".to_string()),
                Some(401) => return Err("unauthorized: session rejected".to_string()),
                _ => {}
            }
            parse::<DeleteAccountResponse>(res).await?;
            match self.http.get(format!("{base_url}/v1/me")).bearer_auth(token).send().await {
                Ok(res) if res.status().as_u16() == 401 => Ok(()),
                Ok(res) => Err(format!("unconfirmed: the session still answers http {}", res.status().as_u16())),
                Err(e) => Err(format!("unconfirmed: network: {e}")),
            }
        }).await;
        match result {
            Some(Ok(())) => {
                // Live sync on the deleted session stops here, before its
                // socket's close reads as a rejected session.
                if self.live.as_ref().is_some_and(|l| l.token == token) {
                    self.live = None;
                    self.reset_pull();
                }
                self.emit(SyncEvent::AccountDeleted { attempt });
            }
            Some(Err(error)) => self.emit(SyncEvent::DeleteAccountFailed { attempt, error }),
            None => {}
        }
    }

    /// Nothing more transfers in this session; the token stays for Turn Sync On.
    fn stop_for_deleted_data(&mut self, here: bool) {
        self.live = None;
        self.reset_pull();
        self.emit(SyncEvent::SyncDataDeleted { here });
    }

    /// Delete the account's sync data, then read the stream back from its
    /// start: the new epoch, past the one this device holds, with no rows.
    /// A server without the route, an error, or any reply outside that
    /// contract is `DeleteFailed`, and sync carries on as it was.
    async fn delete_sync_data(&mut self) {
        let Some(l) = self.live.as_ref().filter(|l| l.ready) else {
            self.emit(SyncEvent::DeleteFailed("network: not connected".into()));
            return;
        };
        let held = l.sync_epoch;
        let result = self.current(async {
            let res = self.http.delete(format!("{}/v1/sync/data", l.base_url)).bearer_auth(&l.token).send().await;
            // A 4xx other than these, however worded, is a server without
            // this route, such as one not yet deployed with it.
            if let Some(status) = res.as_ref().ok().map(|res| res.status().as_u16()) {
                if (400..500).contains(&status) && ![401, 408, 429].contains(&status) {
                    return Err(format!("unsupported: http {status}"));
                }
            }
            parse_session::<DeleteSyncDataResponse>(res).await
        }).await;
        let deleted = match result {
            Some(Ok(d)) if held.is_none_or(|epoch| d.sync_epoch > epoch) => d,
            Some(Ok(_)) => {
                self.emit(SyncEvent::DeleteFailed("bad response: the sync epoch didn't move on".into()));
                return;
            }
            Some(Err(e)) if unauthorized(&e) => { self.invalidate_session(); return; }
            Some(Err(e)) => { self.emit(SyncEvent::DeleteFailed(e)); return; }
            None => return,
        };
        self.deleted_to = Some(deleted.sync_epoch);
        let Some(l) = &self.live else { return };
        let url = format!("{}/v1/sync/pull?since=0&limit={}", l.base_url, MAX_PULL_CHANGES);
        let readback = self.current(async {
            parse_session::<PullResponse>(self.http.get(url).bearer_auth(&l.token).send().await).await
        }).await;
        match readback {
            Some(Ok(p)) if p.sync_epoch == deleted.sync_epoch && p.changes.is_empty() && !p.more => self.stop_for_deleted_data(true),
            Some(Ok(_)) => self.emit(SyncEvent::DeleteFailed("unconfirmed: the readback didn't show the deletion".into())),
            Some(Err(e)) if unauthorized(&e) => self.invalidate_session(),
            Some(Err(e)) => self.emit(SyncEvent::DeleteFailed(format!("unconfirmed: {e}"))),
            None => {}
        }
    }
}

/// A page read from `since` moves forward, and only `more` may stop short.
fn valid_page(p: &PullResponse, since: u64) -> bool {
    p.cursor >= since && !(p.more && p.cursor == since) && p.changes.iter().all(|c| c.seq > since && c.seq <= p.cursor)
}

fn invalid_outgoing(change: &Change) -> Option<&'static str> {
    if change.payload.len() > MAX_PAYLOAD_BYTES {
        return Some("Edit data over 64 KiB; kept locally. Sync Now retries.");
    }
    if change.validate().is_err() || Hlc::parse(&change.hlc.encode()).is_err() {
        return Some("Invalid record kept locally. Edit it, then Sync Now.");
    }
    None
}

fn unauthorized(error: &str) -> bool {
    error == error_codes::UNAUTHORIZED || error.starts_with("unauthorized:")
}

fn permanent_push_error(error: &str) -> bool {
    error.starts_with("bad_request:") || error.starts_with("payload_too_large:") || error.starts_with("http 400") || error.starts_with("http 413")
}

async fn revoke(http: &reqwest::Client, base_url: &str, token: &str) {
    // Local sign-out is immediate; a stalled remote logout cannot block it.
    let _ = tokio::time::timeout(LOGOUT_TIMEOUT, http.post(format!("{base_url}/v1/auth/logout")).bearer_auth(token).send()).await;
}

type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect_ws(base_url: &str, token: &str) -> Result<WsStream, String> {
    let ws_url = base_url.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1) + "/v1/sync/ws";
    let mut req = ws_url.into_client_request().map_err(|e| e.to_string())?;
    req.headers_mut().insert("Authorization", format!("Bearer {token}").parse().map_err(|_| "bad token".to_string())?);
    let (stream, _) = tokio::time::timeout(WS_CONNECT_TIMEOUT, tokio_tungstenite::connect_async(req)).await
        .map_err(|_| "WebSocket connection timed out".to_string())?
        .map_err(|e| match e {
            tungstenite::Error::Http(ref response) if response.status().as_u16() == 401 => "unauthorized: session rejected".into(),
            _ => e.to_string(),
        })?;
    Ok(stream)
}

async fn next_ws(ws: &mut Option<WsStream>) -> Option<Result<tungstenite::Message, tungstenite::Error>> {
    match ws { Some(s) => s.next().await, None => std::future::pending().await }
}

async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending().await }
}

/// A 401 rejects an existing session even if its error body is malformed or
/// stalls. Sign-in has no session yet: invalid/expired codes keep their errors.
async fn parse_session<T: serde::de::DeserializeOwned>(res: reqwest::Result<reqwest::Response>) -> Result<T, String> {
    let res = res.map_err(|e| format!("network: {e}"))?;
    if res.status().as_u16() == 401 { return Err("unauthorized: session rejected".into()); }
    parse(Ok(res)).await
}

/// Decode a JSON body or turn a bounded error body into `"<code>: <message>"`.
async fn parse<T: serde::de::DeserializeOwned>(res: reqwest::Result<reqwest::Response>) -> Result<T, String> {
    let mut res = res.map_err(|e| format!("network: {e}"))?;
    let status = res.status();
    if status.is_success() {
        let text = res.text().await.map_err(|e| format!("network: {e}"))?;
        return serde_json::from_str(&text).map_err(|e| format!("bad response: {e}"));
    }
    const MAX_ERROR_BYTES: usize = 64 * 1024;
    if res.content_length().is_some_and(|len| len > MAX_ERROR_BYTES as u64) {
        return Err("bad response: error body too large".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| format!("network: {e}"))? {
        if chunk.len() > MAX_ERROR_BYTES - bytes.len() {
            return Err("bad response: error body too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    if let Ok(body) = serde_json::from_slice::<ErrorBody>(&bytes) {
        Err(format!("{}: {}", body.error.code, body.error.message))
    } else {
        Err(format!("http {status}"))
    }
}

pub mod browser_sign_in;
pub mod deleted;
pub mod e2ee;
pub mod key_store;
pub mod server;
pub mod token_store;

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod test_secrets;
