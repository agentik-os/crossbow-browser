//! Bridges the background sync worker and the library: persists the session,
//! pushes dirty entities, applies pulled changes and exposes state to the UI.

use crossbow_core::backup::{self, Reason};
use crossbow_core::library::{Library, SYNC_CURSOR_KEY};
use crossbow_core::sync::e2ee::{E2eeStatus, LibraryEncryption, OTHER_ACCOUNT, PageError, RecoveryCode, SIGN_IN_STOPPED, Zeroizing};
use crossbow_core::proto::Hlc;
use crossbow_core::sync::key_store::KeyStore;
use crossbow_core::sync::deleted::{self, SYNC_EPOCH_KEY, SyncOff};
use crossbow_core::sync::{BASE_URL_KEY, DevKey, SESSION_KEY, SyncCommand, SyncEvent, SyncHandle, SyncStatus, server};
#[cfg(not(test))]
use crossbow_core::sync::token_store;
// Tests never reach the login keychain, even through the real sign-in path.
#[cfg(test)]
use tests::memory_token_store as token_store;
use crossbow_core::proto::{Device, MeResponse};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const KV_SESSION: &str = SESSION_KEY;
const KV_CURSOR: &str = SYNC_CURSOR_KEY;
const KV_BASE_URL: &str = BASE_URL_KEY;

/// Shown while sync is off because this device deleted the account's sync data.
pub const SYNC_OFF_HERE: &str = "This account's sync data was deleted from the sync server. This Mac kept its library. Turn sync on again to upload it.";
/// Shown while sync is off because another device deleted it.
pub const SYNC_OFF_ELSEWHERE: &str = "Sync data for this account was deleted from another device. Turn sync on again to upload this Mac's library.";
/// Shown while sync is off and the record of why can't be read.
pub const SYNC_OFF_UNREADABLE: &str = "Couldn't read why sync is off on this Mac, so it stays off. Turn sync on again to upload this Mac's library.";
/// Shown while sync is off because this Mac restored its library from a backup.
pub const SYNC_OFF_RESTORED: &str = "Restored from a backup. Sync is off on this Mac until you turn it on again.";
/// A server without `DELETE /v1/sync/data`, such as one not yet deployed with it.
pub const CANT_DELETE_YET: &str = "This sync server can't delete sync data yet. Nothing changed on this Mac.";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub base_url: String,
    pub account_id: String,
    pub email: String,
    pub device_id: String,
    /// The name a browser sign-in's provider gave, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AuthStep {
    Email,
    Code { challenge_id: String, email: String, dev_code: Option<String> },
}

impl AuthStep {
    pub fn instruction(&self, failed: bool) -> String {
        match self {
            Self::Email => "Use a six-digit code to sign in.".into(),
            Self::Code { challenge_id, email, .. } if challenge_id.is_empty() => {
                if failed { "No code was sent. Try again or use a different email.".into() }
                else { format!("Requesting a code for {email}") }
            }
            Self::Code { dev_code: Some(_), .. } => "Development sign-in — no email was sent. Use the test code below.".into(),
            Self::Code { email, .. } => format!("Enter the six-digit code sent to {email}."),
        }
    }
}

pub fn valid_email(email: &str) -> bool {
    let email = email.trim();
    if email.len() > 254 || email.chars().any(char::is_whitespace) { return false; }
    let Some((local, domain)) = email.split_once('@') else { return false };
    !local.is_empty() && !domain.contains('@') && domain.split('.').count() >= 2 && domain.split('.').all(|part| !part.is_empty())
}

pub fn valid_code(code: &str) -> bool {
    let code = code.trim();
    code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit())
}

pub struct SyncGlue {
    handle: SyncHandle,
    /// Browser sign-in on the Account page.
    pub browser: BrowserSignIn,
    /// Delete Account on the Account page.
    pub delete: DeleteAccount,
    data_dir: PathBuf,
    pub base_url: String,
    pub status: SyncStatus,
    pub session: Option<SessionInfo>,
    pub devices: Vec<Device>,
    pub auth: AuthStep,
    pub error: Option<String>,
    pub busy: bool,
    pub last_sync_ms: Option<u64>,
    pub pending: u64,
    push_in_flight: bool,
    last_push_check: f64,
    push_retry_at: f64,
    push_backoff: f64,
    retry_rejections: bool,
    transport_status: SyncStatus,
    pull_error: Option<String>,
    push_error: Option<String>,
    rejection_error: Option<String>,
    keys: Box<dyn KeyStore>,
    /// The signed-in account's encryption state; reloaded when the account changes.
    e2ee: Option<LibraryEncryption>,
    /// Key storage problems found while loading or signing out.
    pub e2ee_error: Option<String>,
    /// Why the last Unlock was refused, once its server check finished.
    pub unlock_error: Option<String>,
    /// A code waiting for a scan that started after it was entered (true once
    /// that scan is running); wiped on drop.
    unlock_pending: Option<(Zeroizing<String>, bool)>,
    /// The worker skips automatic pulls until the next PullNow or Resume.
    pulls_paused: bool,
    /// Plaintext uploads wait for a complete pull; this asks for one, with backoff.
    gate_pull_at: f64,
    gate_backoff: f64,
    /// The versions in the push in flight, so a stale answer names the one it outranked.
    pushing: Vec<(String, String, Hlc)>,
    /// Exact local versions a push found outranked, until a complete pull.
    outranked: Vec<(String, String, Hlc)>,
    /// A read-only scan ahead of the held cursor is running.
    scanning: bool,
    /// After a failed scan, the next one waits until then, with backoff.
    scan_at: f64,
    scan_backoff: f64,
    /// Sync stopped because the account's sync data was deleted; nothing
    /// transfers until Turn Sync On.
    pub sync_off: Option<SyncOff>,
    /// A delete of the account's sync data is waiting for the server.
    pub deleting: bool,
    /// Why the last delete didn't happen. Nothing changed here.
    pub delete_error: Option<String>,
    /// The epoch a fresh start's first pull named, while it couldn't be
    /// saved; no page is applied until it is.
    unsaved_epoch: Option<u64>,
}

/// The server a new glue starts with: the runtime `CROSSBOW_API_URL` (for QA
/// runs), then the user's saved choice, then the build's server, else none
/// (empty). A signed-in session keeps the server it signed in to.
fn initial_base_url(runtime: Option<String>, saved: Option<String>, build: Option<&str>) -> String {
    runtime.or(saved).or_else(|| build.map(str::to_owned)).unwrap_or_default().trim_end_matches('/').to_string()
}

impl SyncGlue {
    pub fn new(data_dir: &Path, lib: &Library, wake: impl Fn() + Send + Sync + 'static) -> SyncGlue {
        // Tests never reach the login keychain, even through this constructor.
        #[cfg(not(test))]
        let keys: Box<dyn KeyStore> = Box::new(crossbow_core::sync::key_store::OsKeyStore::new(data_dir));
        #[cfg(test)]
        let keys: Box<dyn KeyStore> = Box::new(crossbow_core::sync::key_store::MemoryKeyStore::default());
        Self::with_key_store(data_dir, lib, wake, keys)
    }

    pub fn with_key_store(data_dir: &Path, lib: &Library, wake: impl Fn() + Send + Sync + 'static, keys: Box<dyn KeyStore>) -> SyncGlue {
        Self::with_build_server(data_dir, lib, wake, keys, server::BUILD_URL)
    }

    /// As [`Self::with_key_store`], for a build made for `build` (or none).
    fn with_build_server(data_dir: &Path, lib: &Library, wake: impl Fn() + Send + Sync + 'static, keys: Box<dyn KeyStore>, build: Option<&str>) -> SyncGlue {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        // Tests never take a server or a dev key from the environment they
        // run in, except the dev-key test's own child process.
        #[cfg(not(test))]
        let from_env = true;
        #[cfg(test)]
        let from_env = std::env::var_os("CROSSBOW_DEV_KEY_TEST_CHILD").is_some();
        let var = |name: &str| if from_env { std::env::var(name).ok() } else { None };
        let runtime = var("CROSSBOW_API_URL");
        // A developer's dev server key, never stored: sent only to CROSSBOW_API_URL.
        let dev_key = DevKey::from_vars(runtime.clone(), var("CROSSBOW_DEV_KEY"));
        let handle = SyncHandle::spawn_with_dev_key(wake.clone(), dev_key);
        let base_url = initial_base_url(runtime, lib.store.kv_get(KV_BASE_URL).ok().flatten(), build);
        let session: Option<SessionInfo> = lib.store.kv_get(KV_SESSION).ok().flatten().and_then(|s| serde_json::from_str(&s).ok());
        let mut glue = SyncGlue {
            handle,
            browser: BrowserSignIn::new(wake),
            delete: DeleteAccount::new(),
            data_dir: data_dir.to_path_buf(),
            base_url,
            status: SyncStatus::SignedOut,
            session: None,
            devices: Vec::new(),
            auth: AuthStep::Email,
            error: None,
            busy: false,
            last_sync_ms: None,
            pending: 0,
            push_in_flight: false,
            last_push_check: 0.0,
            push_retry_at: 0.0,
            push_backoff: 1.0,
            retry_rejections: false,
            transport_status: SyncStatus::SignedOut,
            pull_error: None,
            push_error: None,
            rejection_error: None,
            keys,
            e2ee: None,
            e2ee_error: None,
            unlock_error: None,
            unlock_pending: None,
            pulls_paused: false,
            gate_pull_at: 0.0,
            gate_backoff: 1.0,
            pushing: Vec::new(),
            outranked: Vec::new(),
            scanning: false,
            scan_at: 0.0,
            scan_backoff: 1.0,
            sync_off: None,
            deleting: false,
            delete_error: None,
            unsaved_epoch: None,
        };
        if let Some(s) = session {
            match token_store::load(data_dir, &s.account_id) {
                Ok(Some(token)) => {
                    let cursor = lib.store.kv_get(KV_CURSOR).ok().flatten().and_then(|c| c.parse().ok()).unwrap_or(0);
                    // Sync stays off after the account's sync data was deleted.
                    glue.sync_off = SyncOff::read(lib, &s.account_id);
                    if let Some(off) = &glue.sync_off {
                        glue.transport_status = SyncStatus::Offline(off_text(off).into());
                        glue.status = glue.transport_status.clone();
                    } else {
                        let sync_epoch = deleted::resume_epoch(lib, cursor);
                        // A record from before records named their epoch is this one's.
                        if let Some(epoch) = sync_epoch { let _ = LibraryEncryption::adopt_epoch(lib, &s.account_id, epoch); }
                        glue.handle.send(SyncCommand::Resume { base_url: s.base_url.clone(), token, cursor, sync_epoch });
                        glue.status = SyncStatus::Connecting;
                        glue.transport_status = SyncStatus::Connecting;
                    }
                    glue.base_url = s.base_url.clone();
                    glue.session = Some(s);
                }
                Ok(None) => {
                    let _ = lib.store.kv_delete(KV_SESSION);
                }
                Err(error) => {
                    // A denied keychain is not an absent credential. Preserve
                    // the metadata, report once, and never fall back to plaintext.
                    glue.error = Some(error);
                }
            }
        }
        glue
    }

    pub fn signed_in(&self) -> bool {
        self.session.is_some()
    }

    /// Signed in, and sync isn't off after a delete of the account's sync data.
    fn syncing(&self) -> bool {
        self.signed_in() && self.sync_off.is_none()
    }

    /// Whether there is a sync server to sign in to. A build without
    /// `CROSSBOW_API_URL` has none until the user enters one.
    pub fn has_server(&self) -> bool {
        !self.base_url.is_empty()
    }

    /// Saves the user's choice of server: an https:// address, or http:// on
    /// this Mac only. Anything else is refused with the reason, and the
    /// server stays as it was.
    pub fn set_base_url(&mut self, lib: &Library, url: &str) -> Result<(), &'static str> {
        let url = url.trim();
        server::check(url)?;
        self.base_url = url.trim_end_matches('/').to_string();
        let _ = lib.store.kv_set(KV_BASE_URL, &self.base_url);
        Ok(())
    }

    pub fn start_auth(&mut self, email: &str) {
        if self.busy || self.signed_in() { return; }
        if !self.has_server() {
            self.error = Some(server::NO_SERVER.into());
            return;
        }
        if !valid_email(email) {
            self.error = Some("Enter a valid email address.".into());
            return;
        }
        self.error = None;
        self.busy = true;
        self.handle.send(SyncCommand::StartAuth { base_url: self.base_url.clone(), email: email.trim().to_string() });
        self.auth = AuthStep::Code { challenge_id: String::new(), email: email.trim().to_string(), dev_code: None };
    }

    pub fn verify(&mut self, code: &str) {
        if self.busy || !valid_code(code) { return; }
        let AuthStep::Code { challenge_id, .. } = &self.auth else { return };
        if challenge_id.is_empty() {
            return;
        }
        self.error = None;
        self.busy = true;
        self.handle.send(SyncCommand::VerifyAuth {
            base_url: self.base_url.clone(),
            challenge_id: challenge_id.clone(),
            code: code.trim().to_string(),
            device_name: device_name(),
            platform: crossbow_core::platform().to_string(),
        });
    }

    pub fn cancel_auth(&mut self) {
        if self.signed_in() { return; }
        self.handle.send(SyncCommand::CancelAuth);
        self.auth = AuthStep::Email;
        self.error = None;
        self.busy = false;
    }

    pub fn sync_now(&mut self) {
        if self.syncing() {
            // Pulling would only download the refused page again; a locked
            // device scans for a newer marker instead.
            if self.pulls_paused && self.e2ee.as_ref().is_some_and(|e2ee| e2ee.status() == E2eeStatus::Locked) {
                self.rescan();
            } else {
                self.pull_now();
            }
            self.retry_rejections = true;
            self.push_retry_at = 0.0;
            self.last_push_check = f64::NEG_INFINITY;
        }
    }

    /// The user's own Sign Out. The settings page asks first when it would
    /// remove an encryption key.
    pub fn sign_out(&mut self, lib: &Library) {
        self.error = None;
        self.handle.send(SyncCommand::SignOut);
        self.forget(lib, true);
    }

    /// End the session. Only the user's own Sign Out removes the library key;
    /// a server-ended session keeps it. The record that the account is
    /// encrypted always stays, so signing in again never uploads plaintext.
    fn forget(&mut self, lib: &Library, explicit: bool) {
        if let Some(s) = self.session.take() {
            if let Err(error) = token_store::clear(&self.data_dir, &s.account_id) {
                self.error = Some(format!("Signed out locally. Couldn't remove the stored credential: {error}"));
            }
            if explicit {
                if let Err(error) = LibraryEncryption::forget_key(&s.account_id, &*self.keys) {
                    self.error = Some(format!("Signed out locally. Couldn't remove this account's library key: {error}"));
                }
            }
        }
        self.browser.reset();
        self.delete.close();
        self.e2ee = None;
        self.e2ee_error = None;
        self.unlock_error = None;
        self.unlock_pending = None;
        self.pulls_paused = false;
        self.busy = false;
        let _ = lib.store.kv_delete(KV_SESSION);
        let _ = lib.store.kv_delete(KV_CURSOR);
        let _ = lib.store.kv_delete(SYNC_EPOCH_KEY);
        let _ = SyncOff::clear(lib);
        self.sync_off = None;
        self.deleting = false;
        self.delete_error = None;
        self.unsaved_epoch = None;
        self.devices.clear();
        self.status = SyncStatus::SignedOut;
        self.transport_status = SyncStatus::SignedOut;
        self.pull_error = None;
        self.push_error = None;
        self.rejection_error = None;
        self.retry_rejections = false;
        self.push_retry_at = 0.0;
        self.push_backoff = 1.0;
        self.last_sync_ms = None;
        self.auth = AuthStep::Email;
        self.push_in_flight = false;
        self.gate_pull_at = 0.0;
        self.gate_backoff = 1.0;
        self.pushing.clear();
        self.outranked.clear();
        self.scanning = false;
        self.scan_at = 0.0;
        self.scan_backoff = 1.0;
    }

    pub fn shutdown(&mut self) {
        self.handle.send(SyncCommand::Shutdown);
    }

    /// Drain worker events and push local changes. Called once per frame.
    pub fn poll(&mut self, lib: &mut Library, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let mut refresh_rejections = false;
        self.poll_browser();
        while let Ok(e) = self.handle.try_recv() {
            match e {
                SyncEvent::AuthStarted { challenge_id: id, dev_code } => {
                    self.busy = false;
                    if let AuthStep::Code { email, .. } = &self.auth {
                        self.auth = AuthStep::Code { challenge_id: id, email: email.clone(), dev_code };
                    }
                }
                SyncEvent::AuthError(err) => {
                    self.busy = false;
                    if !self.browser_sign_in_failed(&err) {
                        self.error = Some(friendly_error(&err));
                    }
                }
                SyncEvent::SignedIn(s) => {
                    self.busy = false;
                    self.error = None;
                    let by_browser = self.take_browser_sign_in();
                    self.delete.done = None;
                    let refusal = match LibraryEncryption::owner(lib) {
                        // This library is encrypted for another account: never
                        // queue it for this one.
                        Ok(Some(owner)) if owner != s.account.id => Some(OTHER_ACCOUNT),
                        // Whether this library must wait for encryption is unknown.
                        Ok(_) if deleted::encrypt_first(lib, &s.account.id).is_none() => Some(SIGN_IN_STOPPED),
                        Ok(_) => None,
                        // Fail closed: which account owns it is unknown.
                        Err(stopped) => Some(stopped),
                    };
                    if let Some(refusal) = refusal {
                        // End the new session unused; nothing is saved or queued.
                        let _ = token_store::clear(&self.data_dir, &s.account.id);
                        self.error = Some(refusal.into());
                        self.auth = AuthStep::Email;
                        self.handle.send(SyncCommand::SignOut);
                        continue;
                    }
                    if let Err(e) = token_store::save(&self.data_dir, &s.account.id, &s.token) {
                        self.error = Some(format!("Couldn't store the session securely: {e}"));
                        self.handle.send(SyncCommand::SignOut);
                        continue;
                    }
                    let info = SessionInfo { base_url: s.base_url.clone(), account_id: s.account.id.clone(), email: s.account.email.clone(), device_id: s.device.id.clone(), display_name: s.display_name.clone() };
                    let saved = (|| -> Result<(), String> {
                        let data = serde_json::to_string(&info).map_err(|_| "Couldn't encode the session.".to_string())?;
                        lib.store.kv_set(KV_SESSION, &data).map_err(|_| "Couldn't save the session.".to_string())?;
                        lib.store.kv_set(KV_CURSOR, "0").map_err(|_| "Couldn't save the sync cursor.".to_string())?;
                        lib.store.kv_delete(SYNC_EPOCH_KEY).map_err(|_| "Couldn't save the sync cursor.".to_string())?;
                        // Signing in again uploads this library whole, so sync is on.
                        SyncOff::clear(lib)?;
                        lib.store.mark_all_dirty().map_err(|_| "Couldn't prepare local changes for sync. Sign in again to retry.".to_string())?;
                        lib.store.retry_sync_rejections().map_err(|_| "Couldn't prepare the sync queue. Sign in again to retry.".to_string())?;
                        Ok(())
                    })();
                    if let Err(error) = saved {
                        let _ = token_store::clear(&self.data_dir, &s.account.id);
                        let _ = lib.store.kv_delete(KV_SESSION);
                        let _ = lib.store.kv_delete(KV_CURSOR);
                        self.error = Some(error);
                        self.handle.send(SyncCommand::SignOut);
                        continue;
                    }
                    // Credential persistence is the gate before any library transfer.
                    self.handle.send(SyncCommand::Resume { base_url: s.base_url, token: s.token, cursor: 0, sync_epoch: None });
                    self.sync_off = None;
                    self.pulls_paused = false;
                    self.session = Some(info);
                    self.auth = AuthStep::Email;
                    if by_browser { self.browser.notice = Some(Notice::SignedIn); }
                    self.status = SyncStatus::Connecting;
                    self.transport_status = SyncStatus::Connecting;
                    self.last_push_check = f64::NEG_INFINITY;
                }
                SyncEvent::Me(MeResponse { account, device, devices, browser_sign_in }) => {
                    if let Some(s) = self.session.as_mut() {
                        s.email = account.email;
                        s.device_id = device.id;
                    }
                    self.devices = devices;
                    self.browser.linked = Some(browser_sign_in);
                }
                SyncEvent::LinkPending { attempt, link_id, email } => self.link_pending(attempt, link_id, email),
                SyncEvent::Linked { attempt } => self.linked(attempt),
                SyncEvent::LinkFailed { attempt, error } => self.link_failed(attempt, &error),
                // Sync stopped after a delete: a page read before that is moot.
                SyncEvent::Pulled { .. } if self.sync_off.is_some() => {}
                SyncEvent::Pulled { batch_id, changes, cursor, more } => {
                    // A page in the epoch a delete was sent from: it didn't happen.
                    if !self.deleting && self.session.as_ref().is_some_and(|s| deleted::deleting_here(lib, &s.account_id)) { let _ = deleted::delete_settled(lib); }
                    self.apply_pulled(lib, batch_id, &changes, cursor, more);
                    refresh_rejections = true;
                }
                SyncEvent::SyncEpoch(epoch) => {
                    // An encryption record from before a delete elsewhere is
                    // retired before anything transfers in this epoch.
                    if self.retire_outdated(lib, epoch, true) {
                        self.unsaved_epoch = lib.store.kv_set(SYNC_EPOCH_KEY, &epoch.to_string()).is_err().then_some(epoch);
                    }
                }
                SyncEvent::SyncDataDeleted { here } => self.stop_for_deleted_data(lib, here),
                SyncEvent::StepUpCode { attempt, challenge_id, dev_code } => self.step_up_code(attempt, challenge_id, dev_code),
                SyncEvent::AccountDeleted { attempt } => self.account_deleted(lib, attempt),
                SyncEvent::DeleteAccountFailed { attempt, error } => self.delete_account_failed(lib, attempt, &error),
                SyncEvent::DeleteFailed(error) => {
                    self.deleting = false;
                    // The server didn't delete; anything else may have, and a pull tells.
                    if ["unsupported", "bad response", "network: not connected"].iter().any(|refused| error.starts_with(refused)) { let _ = deleted::delete_settled(lib); }
                    self.delete_error = Some(delete_failure(&error));
                }
                SyncEvent::Scanned { changes, more } => self.scanned(lib, &changes, more),
                SyncEvent::ScanFailed(_) => {
                    self.scanning = false;
                    match self.unlock_pending.as_ref().map(|(_, started)| *started) {
                        // The check the code waited for couldn't run.
                        Some(true) => self.finish_unlock(lib, false),
                        // That scan began before the code was entered; one more, now.
                        Some(false) => self.scan_at = now,
                        None => {
                            self.scan_at = now + self.scan_backoff;
                            self.scan_backoff = (self.scan_backoff * 2.0).min(60.0);
                        }
                    }
                }
                // The server changed while pulls are paused: a locked device
                // looks for a newer marker.
                SyncEvent::RemoteChanged => self.rescan(),
                SyncEvent::Pushed { accepted, stale, .. } => {
                    let mut saved = true;
                    for c in &accepted {
                        if lib.store.mark_clean(&c.kind, &c.id, &c.hlc).is_err() { saved = false; }
                    }
                    if saved {
                        self.last_sync_ms = Some(crossbow_core::now_ms());
                    } else {
                        self.push_failed("Couldn't save sync progress; kept locally. Sync Now retries.".into(), now);
                    }
                    if !stale.is_empty() {
                        // The server holds newer versions; fetch and commit
                        // them, remembering exactly which local version lost.
                        let lost = self.pushing.iter().filter(|(kind, id, hlc)| stale.iter().any(|s| s.kind == *kind && s.id == *id && s.server_hlc > *hlc));
                        self.outranked.extend(lost.cloned());
                        self.pull_now();
                    }
                    refresh_rejections = true;
                }
                SyncEvent::PushRejected { change, reason } => {
                    if lib.store.reject_sync_change(&change, &reason).is_err() {
                        self.push_failed("Couldn't save sync queue; kept locally. Sync Now retries.".into(), now);
                    }
                    refresh_rejections = true;
                }
                SyncEvent::PushFailed(err) => {
                    if let Some(e2ee) = self.e2ee.as_mut() { e2ee.reconnecting(); }
                    self.push_failed(format!("Sync failed; local changes kept. {}", friendly_error(&err)), now);
                }
                SyncEvent::PushFinished => {
                    self.push_in_flight = false;
                    self.pushing.clear();
                    if self.push_error.is_none() { self.push_backoff = 1.0; }
                }
                SyncEvent::Status(s) => {
                    // A dropped or new connection may have missed changes.
                    if matches!(s, SyncStatus::Offline(_) | SyncStatus::Connecting) {
                        if let Some(e2ee) = self.e2ee.as_mut() { e2ee.reconnecting(); }
                    }
                    self.transport_status = s;
                }
                SyncEvent::SignedOut => self.forget(lib, false),
            }
        }

        if now - self.last_push_check >= 1.0 {
            self.last_push_check = now;
            refresh_rejections = true;
            match lib.store.dirty_count() {
                Ok(n) => self.pending = n,
                Err(_) => self.push_failed("Couldn't read sync queue; local data kept. Sync Now retries.".into(), now),
            }
            // Plaintext uploads and Turn On wait for a complete pull after any
            // reconnect, even with nothing queued; ask for one, with backoff.
            if self.syncing() {
                self.ensure_e2ee(lib);
                if self.upload_hold().is_some() && now >= self.gate_pull_at {
                    self.pull_now();
                    self.gate_pull_at = now + self.gate_backoff;
                    self.gate_backoff = (self.gate_backoff * 2.0).min(60.0);
                }
            }
            let held = self.syncing() && self.encrypt_first(lib) != Some(false);
            if self.syncing() && !held && !self.push_in_flight && now >= self.push_retry_at {
                if self.retry_rejections {
                    match lib.store.retry_sync_rejections() {
                        Ok(()) => self.retry_rejections = false,
                        Err(_) => self.push_failed("Couldn't retry sync queue; data kept. Sync Now retries.".into(), now),
                    }
                }
                if !self.retry_rejections && self.pending > 0 && now >= self.push_retry_at {
                    // Encrypted when on; empty while locked or before the first pull.
                    let outgoing = self.e2ee.as_ref().map(|e2ee| e2ee.outgoing(lib, crossbow_core::proto::MAX_PUSH_CHANGES));
                    match outgoing.unwrap_or_else(|| Ok(Vec::new())) {
                        Ok(changes) if !changes.is_empty() => {
                            self.push_error = None;
                            self.push_in_flight = true;
                            self.pushing = changes.iter().map(|c| (c.kind.clone(), c.id.clone(), c.hlc.clone())).collect();
                            self.handle.send(SyncCommand::Push(changes));
                        }
                        Ok(_) => {}
                        Err(error) => self.push_failed(error, now),
                    }
                }
            }
        }
        if self.signed_in() {
            if refresh_rejections {
                match lib.store.sync_rejection_reason() {
                    Ok(reason) => self.rejection_error = reason,
                    Err(_) => self.push_failed("Couldn't read sync queue; local data kept. Sync Now retries.".into(), now),
                }
            }
            // Settings already renders Offline status while signed in. Keep a
            // durable sync problem visible even when a healthy socket says Live.
            self.error = self.pull_error.clone().or_else(|| self.rejection_error.clone()).or_else(|| self.push_error.clone());
            self.status = self.error.clone().map(SyncStatus::Offline).unwrap_or_else(|| self.transport_status.clone());
            self.ensure_e2ee(lib);
            if self.syncing() { self.scan_if_needed(now); }
        }
    }

    fn apply_pulled(&mut self, lib: &mut Library, batch_id: u64, changes: &[crossbow_core::proto::ServerChange], cursor: u64, more: bool) {
        // Every page is in the stored epoch, which no encryption record may predate.
        if let Some(epoch) = deleted::stored_epoch(lib) {
            if !self.retire_outdated(lib, epoch, false) {
                self.handle.send(SyncCommand::PullApplied { batch_id, success: false });
                return;
            }
        }
        self.ensure_e2ee(lib);
        let Some(e2ee) = self.e2ee.as_mut() else {
            // No signed-in account: nothing may be applied.
            self.handle.send(SyncCommand::PullApplied { batch_id, success: false });
            return;
        };
        // The epoch goes with the cursor: a cursor saved without it would be
        // read as epoch 0 on the next start. A page this device refuses
        // anyway moves no cursor, and keeps its own reason.
        if let Some(epoch) = self.unsaved_epoch {
            let refused = matches!(e2ee.status(), E2eeStatus::Locked | E2eeStatus::Unreadable | E2eeStatus::Unavailable | E2eeStatus::OtherAccount);
            if lib.store.kv_set(SYNC_EPOCH_KEY, &epoch.to_string()).is_ok() {
                self.unsaved_epoch = None;
            } else if !refused {
                self.pull_error = Some(PageError::Storage.to_string());
                self.handle.send(SyncCommand::PullApplied { batch_id, success: false });
                return;
            }
        }
        // Decryption happens before the library sees anything; a refused page
        // holds the cursor and keeps its reason visible.
        let (success, pause) = match e2ee.apply_page(lib, changes, cursor, more) {
            Ok(_) => {
                self.pull_error = None;
                self.last_sync_ms = Some(crossbow_core::now_ms());
                self.gate_backoff = 1.0;
                (true, false)
            }
            Err(error) => {
                self.pull_error = Some(error.to_string());
                // Re-downloading a page that stays refused gains nothing until
                // the user unlocks, resets, tries again or presses Sync Now.
                let pause = matches!(error, PageError::Unreadable | PageError::Unavailable | PageError::OtherAccount) || error == PageError::Locked && e2ee.has_check_row();
                (false, pause)
            }
        };
        self.handle.send(SyncCommand::PullApplied { batch_id, success });
        if pause { self.pause_pulls(); }
        if success && !more {
            // A version a push found outranked, still the local one after a
            // complete pull, can never win: the newer one is sealed under a
            // key this device lacks, or withheld. Exactly that version stops
            // uploading (mark_clean matches the clock); an edit made since
            // keeps syncing. Best effort: on failure it stays queued.
            for (kind, id, hlc) in std::mem::take(&mut self.outranked) {
                let _ = lib.store.mark_clean(&kind, &id, &hlc);
            }
        }
    }

    /// A page from a read-only scan: nothing is applied and the cursor stays.
    fn scanned(&mut self, lib: &Library, changes: &[crossbow_core::proto::ServerChange], more: bool) {
        self.ensure_e2ee(lib);
        let Some(e2ee) = self.e2ee.as_mut() else { return };
        e2ee.scan_page(lib, changes, more);
        if !more {
            self.scanning = false;
            self.scan_backoff = 1.0;
            match self.unlock_pending.as_ref().map(|(_, started)| *started) {
                // The code is checked against the newest marker this scan found.
                Some(true) => self.finish_unlock(lib, true),
                // This scan began before the code was entered; read again.
                Some(false) => self.rescan(),
                None => {}
            }
            // The check row is now the account's newest marker, so pulling
            // the refused page again gains nothing until the user acts.
            if self.e2ee.as_ref().is_some_and(LibraryEncryption::has_check_row) { self.pause_pulls(); }
        }
    }

    /// While locked or unreadable, and before a reset is confirmed, read the
    /// stream to its end without applying it, so this device learns the
    /// account's newest marker even past pages it holds and can't apply.
    fn scan_if_needed(&mut self, now: f64) {
        // A code waits only while this device is locked.
        if self.unlock_pending.is_some() && !self.e2ee.as_ref().is_some_and(|e2ee| e2ee.status() == E2eeStatus::Locked) {
            self.unlock_pending = None;
        }
        if !self.signed_in() || self.scanning || now < self.scan_at { return; }
        if self.e2ee.as_ref().is_some_and(LibraryEncryption::needs_scan) {
            self.scanning = true;
            self.handle.send(SyncCommand::ScanAhead);
            if let Some((_, started)) = self.unlock_pending.as_mut() { *started = true; }
        }
    }

    /// The account's newest marker may have changed: scan again now.
    fn rescan(&mut self) {
        if let Some(e2ee) = self.e2ee.as_mut() { e2ee.rescan(); }
        self.scan_at = 0.0;
        self.scan_backoff = 1.0;
    }

    fn pull_now(&mut self) {
        self.handle.send(SyncCommand::PullNow);
        self.pulls_paused = false;
    }

    fn pause_pulls(&mut self) {
        self.handle.send(SyncCommand::PausePulls);
        self.pulls_paused = true;
    }

    /// The scan the entered code waited for has ended: check the code against
    /// the newest marker it found, or, if it couldn't run, without calling
    /// the code wrong.
    fn finish_unlock(&mut self, lib: &Library, scanned: bool) {
        let Some((code, _)) = self.unlock_pending.take() else { return };
        let Some(e2ee) = self.e2ee.as_mut() else { return };
        let unlocked = if scanned { e2ee.unlock(&code, lib, &*self.keys) } else { e2ee.unlock_unchecked(&code, lib, &*self.keys) };
        match unlocked {
            Ok(()) => {
                self.pull_error = None;
                self.e2ee_error = None;
                // Retry the held page and the held local changes now.
                self.sync_now();
            }
            Err(error) => self.unlock_error = Some(error),
        }
    }

    fn ensure_e2ee(&mut self, lib: &Library) {
        let Some(session) = &self.session else {
            self.e2ee = None;
            return;
        };
        // Never reuse one account's state or key for another account.
        if self.e2ee.as_ref().is_some_and(|e2ee| e2ee.account() == session.account_id) { return; }
        let (e2ee, warning) = LibraryEncryption::load(&session.account_id, lib, &*self.keys);
        self.e2ee = Some(e2ee);
        self.e2ee_error = warning;
    }

    pub fn encryption_status(&mut self, lib: &Library) -> Option<E2eeStatus> {
        self.ensure_e2ee(lib);
        self.e2ee.as_ref().map(LibraryEncryption::status)
    }

    /// Turning on needs one completed pull, so this device knows the account
    /// isn't already encrypted elsewhere.
    pub fn encryption_ready(&self) -> bool {
        self.e2ee.as_ref().is_some_and(LibraryEncryption::caught_up) && self.pull_error.is_none()
    }

    pub fn begin_encryption(&mut self, lib: &Library) -> Result<(), String> {
        self.ensure_e2ee(lib);
        let e2ee = self.e2ee.as_mut().ok_or("Sign in to turn on encryption.")?;
        e2ee.begin(lib, &*self.keys)?;
        // Confirm waits for this pull, so another device's new key locks this
        // one instead of competing with it.
        self.pull_now();
        Ok(())
    }

    /// Turn On can finish: a complete pull ran after it began.
    pub fn encryption_confirmable(&self) -> bool {
        self.e2ee.as_ref().is_some_and(LibraryEncryption::confirmable) && self.pull_error.is_none()
    }

    pub fn cancel_encryption(&mut self) -> Result<(), String> {
        match self.e2ee.as_mut() {
            Some(e2ee) => e2ee.cancel(&*self.keys),
            None => Ok(()),
        }
    }

    pub fn confirm_encryption(&mut self, lib: &mut Library) -> Result<(), String> {
        let e2ee = self.e2ee.as_mut().ok_or("Sign in to turn on encryption.")?;
        if e2ee.confirmable() { backup::before(&lib.store, Reason::TurnOnEncryption); }
        e2ee.confirm(lib)?;
        // Upload the marker and the re-encrypted library right away.
        self.last_push_check = f64::NEG_INFINITY;
        self.push_retry_at = 0.0;
        Ok(())
    }

    /// The code is checked against the account's newest marker, so a scan
    /// that starts after this call reads the server first, even when an
    /// earlier scan finished. The answer arrives when it ends: On, or
    /// `unlock_error`. A typo is refused here.
    pub fn unlock_encryption(&mut self, lib: &Library, code: &str) -> Result<(), String> {
        self.ensure_e2ee(lib);
        let e2ee = self.e2ee.as_mut().ok_or("Sign in to unlock sync.")?;
        if e2ee.status() != E2eeStatus::Locked { return Err("This library isn't locked.".into()); }
        RecoveryCode::parse(code).map_err(|error| error.to_string())?;
        self.unlock_error = None;
        self.unlock_pending = Some((Zeroizing::new(code.to_string()), false));
        self.rescan();
        Ok(())
    }

    pub fn recovery_code(&self) -> Option<RecoveryCode> {
        self.e2ee.as_ref().and_then(LibraryEncryption::recovery_code)
    }

    /// Reset can't be confirmed until a scan has read the stream to its end.
    pub fn begin_reset_encryption(&mut self, lib: &Library) -> Result<(), String> {
        self.ensure_e2ee(lib);
        self.e2ee.as_mut().ok_or("Sign in to reset encryption.")?.begin_reset(lib)?;
        self.unlock_error = None;
        self.scan_at = 0.0;
        self.scan_backoff = 1.0;
        Ok(())
    }

    /// A read-only scan for the account's newest marker is running, or an
    /// entered code waits for one.
    pub fn checking_encryption(&self) -> bool {
        self.scanning || self.unlock_pending.is_some()
    }

    /// An entered code waits for a scan to the end of the stream.
    pub fn unlock_waiting(&self) -> bool {
        self.unlock_pending.is_some()
    }

    /// The pending reset can be confirmed: the scan reached the end of the stream.
    pub fn reset_ready(&self) -> bool {
        self.e2ee.as_ref().is_some_and(LibraryEncryption::reset_ready)
    }

    /// Sign Out would remove a key that may be stored here, so it asks first.
    pub fn encryption_may_hold_key(&mut self, lib: &Library) -> bool {
        self.ensure_e2ee(lib);
        self.e2ee.as_ref().is_some_and(LibraryEncryption::may_hold_key)
    }

    pub fn dismiss_encryption_notice(&mut self, lib: &Library) -> Result<(), String> {
        match self.e2ee.as_mut() {
            Some(e2ee) => e2ee.dismiss_notice(lib),
            None => Ok(()),
        }
    }

    pub fn cancel_reset_encryption(&mut self) {
        if let Some(e2ee) = self.e2ee.as_mut() { e2ee.cancel_reset(); }
    }

    /// Replace the account's key from this device. What it has is sealed again
    /// under the new key after the next complete pull; nothing is deleted.
    pub fn confirm_reset_encryption(&mut self, lib: &mut Library) -> Result<(), String> {
        let e2ee = self.e2ee.as_mut().ok_or("Sign in to reset encryption.")?;
        if e2ee.reset_ready() { backup::before(&lib.store, Reason::ResetEncryption); }
        e2ee.confirm_reset(lib, &*self.keys)?;
        self.pull_error = None;
        self.e2ee_error = None;
        self.sync_now();
        Ok(())
    }

    /// Read this account's encryption record and key again, after a failed
    /// read, an unparseable record or a declined keychain prompt.
    pub fn reload_encryption(&mut self, lib: &Library) {
        let Some(session) = &self.session else { return };
        let (e2ee, warning) = LibraryEncryption::load(&session.account_id, lib, &*self.keys);
        self.e2ee = Some(e2ee);
        self.e2ee_error = warning;
        self.unlock_error = None;
        self.unlock_pending = None;
        self.pull_error = None;
        self.sync_now();
    }

    /// What recent pages ignored or replaced.
    pub fn encryption_notice(&self) -> Option<&str> {
        self.e2ee.as_ref().and_then(LibraryEncryption::notice)
    }

    /// Why local changes are waiting to upload, while the first-pull gate holds them.
    pub fn upload_hold(&self) -> Option<&'static str> {
        self.e2ee.as_ref().filter(|_| self.signed_in()).and_then(LibraryEncryption::upload_hold)
    }

    /// Resume against a loopback fixture without credential IO: no keychain
    /// entry, real account or environment setting is read, written or deleted.
    #[cfg(test)]
    pub(crate) fn resume_fixture(&mut self, base_url: &str, account_id: &str) {
        self.session = Some(SessionInfo { base_url: base_url.into(), account_id: account_id.into(), email: "fixture@example.com".into(), device_id: "fixture-device".into(), display_name: None });
        self.transport_status = SyncStatus::Connecting;
        self.handle.send(SyncCommand::Resume { base_url: base_url.into(), token: "loopback-fixture-token".into(), cursor: 0, sync_epoch: None });
        self.pulls_paused = false;
    }

    /// No scan for the account's newest marker is running or due.
    #[cfg(test)]
    pub(crate) fn encryption_scanned(&self) -> bool {
        !self.checking_encryption() && !self.e2ee.as_ref().is_some_and(LibraryEncryption::needs_scan)
    }

    #[cfg(test)]
    pub(crate) fn has_library_key(&self, account: &str) -> bool {
        self.keys.load(account).unwrap().is_some()
    }

    /// Delete the account's sync data on the server. Only a delete the
    /// server confirms turns sync off here; anything else changes nothing.
    /// The delete is noted first, so this device knows the new epoch is its
    /// own even if it quits, or the readback fails, before that.
    pub fn delete_sync_data(&mut self, lib: &Library) {
        if !self.syncing() || self.deleting { return; }
        let Some(account) = self.session.as_ref().map(|s| s.account_id.clone()) else { return };
        // Taken now: a failed backup never stops the delete, and is listed
        // in Restore Data.
        backup::before(&lib.store, Reason::DeleteSyncData);
        if let Err(error) = deleted::begin_delete(lib, &account) {
            self.delete_error = Some(error);
            return;
        }
        self.deleting = true;
        self.delete_error = None;
        self.handle.send(SyncCommand::DeleteSyncData);
    }

    /// Why sync is off, while it is.
    pub fn sync_off_text(&self) -> Option<&'static str> {
        self.sync_off.as_ref().map(off_text)
    }

    /// The account's sync data is gone from the server. Keep the library,
    /// record that sync is off and retire this account's key, whose recovery
    /// codes now protect nothing. The session stays for Turn Sync On.
    fn stop_for_deleted_data(&mut self, lib: &mut Library, here: bool) {
        let Some(account) = self.session.as_ref().map(|s| s.account_id.clone()) else { return };
        // After a quit, a failed readback or a refused upload, only the note
        // made before sending says this device deleted it.
        let here = here || deleted::deleting_here(lib, &account);
        let (off, failure) = match SyncOff::record(lib, &account, here) {
            Ok(off) => {
                if let Err(error) = LibraryEncryption::forget_key(&account, &*self.keys) {
                    self.e2ee_error = Some(format!("Sync is off. Couldn't remove this account's old library key: {error}"));
                }
                (off, None)
            }
            // Off for this session; the next start reads the new epoch and records it then.
            Err(error) => (SyncOff { account, here, encrypted: false, restored: false, unreadable: false }, Some(error)),
        };
        self.deleting = false;
        self.delete_error = None;
        self.e2ee = None;
        self.unlock_error = None;
        self.unlock_pending = None;
        self.pulls_paused = false;
        self.unsaved_epoch = None;
        self.push_in_flight = false;
        self.pushing.clear();
        self.outranked.clear();
        self.scanning = false;
        self.push_error = None;
        self.rejection_error = None;
        self.pull_error = failure;
        self.pending = lib.store.dirty_count().unwrap_or(0);
        self.transport_status = SyncStatus::Offline(off_text(&off).into());
        self.sync_off = Some(off);
    }

    /// The user confirmed uploading this Mac's library again: a fresh start
    /// on the same session, as a first sign-in is.
    pub fn turn_sync_on(&mut self, lib: &mut Library) -> Result<(), String> {
        let Some(s) = self.session.clone().filter(|_| self.sync_off.is_some()) else { return Ok(()) };
        let token = match token_store::load(&self.data_dir, &s.account_id) {
            Ok(Some(token)) => token,
            Ok(None) => return Err("This Mac's sign-in for this account is gone. Sign out, then sign in again.".into()),
            Err(error) => return Err(format!("Couldn't read the stored sign-in: {error}")),
        };
        backup::before(&lib.store, Reason::TurnSyncOn);
        SyncOff::turn_on(lib)?;
        self.sync_off = None;
        self.e2ee = None;
        self.e2ee_error = None;
        self.pull_error = None;
        self.pulls_paused = false;
        self.handle.send(SyncCommand::Resume { base_url: s.base_url, token, cursor: 0, sync_epoch: None });
        self.status = SyncStatus::Connecting;
        self.transport_status = SyncStatus::Connecting;
        self.last_push_check = f64::NEG_INFINITY;
        Ok(())
    }

    /// Uploads wait: this library was encrypted before the account's sync
    /// data was deleted, and encryption isn't on again yet. Once it is, under
    /// a new key, the hold ends.
    pub fn encryption_hold(&mut self, lib: &Library) -> bool {
        self.encrypt_first(lib) == Some(true)
    }

    /// Whether uploads wait for encryption; `None` if that can't be read
    /// just now, when they wait too.
    fn encrypt_first(&mut self, lib: &Library) -> Option<bool> {
        let Some(account) = self.session.as_ref().map(|s| s.account_id.clone()) else { return Some(false) };
        if deleted::encrypt_first(lib, &account)? {
            self.ensure_e2ee(lib);
            if self.e2ee.as_ref().is_some_and(|e2ee| e2ee.status() == E2eeStatus::On) {
                return Some(deleted::release_encrypt_first(lib).is_err());
            }
            return Some(true);
        }
        Some(false)
    }

    /// This library was encrypted for the account when its sync data was
    /// deleted, and encryption isn't on again yet.
    pub fn encrypted_before_delete(&self, lib: &Library) -> bool {
        self.session.as_ref().is_some_and(|s| deleted::encrypt_first(lib, &s.account_id) == Some(true))
    }

    /// The user chose to upload this library without encryption, to this
    /// account only.
    pub fn upload_without_encryption(&mut self, lib: &Library) -> Result<(), String> {
        let Some(account) = self.session.as_ref().map(|s| s.account_id.clone()) else { return Ok(()) };
        deleted::upload_unencrypted(lib, &account)?;
        self.last_push_check = f64::NEG_INFINITY;
        Ok(())
    }

    /// This account's data is in `epoch`. An encryption record from an
    /// earlier one, kept while this device was signed out or its session had
    /// ended, is retired with its key, as meeting the new epoch while signed
    /// in does: nothing is sealed under that key into this epoch, its marker
    /// never uploads, its recovery code opens nothing, and uploads wait for
    /// encryption. If that can't be saved, sync stops here instead: false.
    fn retire_outdated(&mut self, lib: &mut Library, epoch: u64, fresh: bool) -> bool {
        let Some(account) = self.session.as_ref().map(|s| s.account_id.clone()) else { return true };
        match deleted::retire_outdated(lib, &account, epoch, fresh) {
            Ok(false) => true,
            Ok(true) => {
                if let Err(error) = LibraryEncryption::forget_key(&account, &*self.keys) {
                    self.e2ee_error = Some(format!("Couldn't remove this account's old library key: {error}"));
                }
                self.e2ee = None;
                self.unlock_pending = None;
                self.unlock_error = None;
                true
            }
            Err(error) => {
                self.stop_for_deleted_data(lib, false);
                self.pull_error.get_or_insert(error);
                self.pause_pulls();
                false
            }
        }
    }

    fn push_failed(&mut self, error: String, now: f64) {
        self.push_error = Some(error);
        self.push_retry_at = now + self.push_backoff;
        self.push_backoff = (self.push_backoff * 2.0).min(60.0);
    }
}

fn off_text(off: &SyncOff) -> &'static str {
    if off.unreadable { SYNC_OFF_UNREADABLE } else if off.restored { SYNC_OFF_RESTORED } else if off.here { SYNC_OFF_HERE } else { SYNC_OFF_ELSEWHERE }
}

/// Why a delete didn't happen; each says that nothing changed here.
fn delete_failure(e: &str) -> String {
    if e.starts_with("unsupported") || e.starts_with("bad response") {
        CANT_DELETE_YET.into()
    } else if e.starts_with("unconfirmed") {
        "The sync server may have deleted the data but didn't confirm it, so nothing changed on this Mac. Try again.".into()
    } else if e.starts_with("network:") {
        "Can't reach the sync service, so nothing was deleted and nothing changed on this Mac. Check your connection, then try again.".into()
    } else {
        format!("The sync server didn't delete the data ({e}). Nothing changed on this Mac. Try again.")
    }
}

fn friendly_error(e: &str) -> String {
    use crossbow_core::proto::error_codes as c;
    if e.starts_with(c::INVALID_CODE) {
        "That code isn't right. Check the code and try again.".into()
    } else if e.starts_with(c::CODE_EXPIRED) {
        "That code has expired. Request a new one.".into()
    } else if e.starts_with(c::RATE_LIMITED) || e.starts_with(c::TOO_MANY_ATTEMPTS) {
        "Too many attempts. Wait a little and try again.".into()
    } else if e.starts_with(c::BAD_REQUEST) {
        "Enter a valid email address.".into()
    } else if e.starts_with(c::MAIL_UNAVAILABLE) {
        "The sign-in email couldn't be sent. Try again shortly.".into()
    } else if e.starts_with("network:") {
        "Can't reach the sync service. Check your connection.".into()
    } else {
        e.to_string()
    }
}

pub fn device_name() -> String {
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    #[cfg(target_os = "windows")]
    if let Ok(s) = std::env::var("COMPUTERNAME") {
        return s;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Ok(s) = std::fs::read_to_string("/etc/hostname") {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    format!("Crossbow on {}", crossbow_core::platform())
}

#[path = "sync_glue_browser.rs"]
mod browser;
pub use browser::{BrowserSignIn, Notice, Offer as BrowserOffer, Step as BrowserStep};
#[path = "sync_glue_delete.rs"]
mod delete;
pub use delete::{DeleteAccount, Step as DeleteStep};
#[cfg(test)]
pub use delete::{ACCOUNT_DELETED, SESSION_ENDED};

#[cfg(test)]
#[allow(dead_code)] // Shared fixture also exercises stalled requests in core tests.
#[path = "../../crates/crossbow-core/src/sync/test_server.rs"]
pub(crate) mod sync_test_server;

#[cfg(test)]
mod tests {
    use super::*;
    use super::sync_test_server::{Reply, Request, Server, SyncService};
    use crate::test_codes::Hidden;
    use crossbow_core::store::Store;
    use crossbow_core::proto::{Account, Change, Encoding, Hlc, PullResponse, PushRequest, PushResponse, ServerChange};
    use std::time::{Duration, Instant};

    /// Session tokens for tests: process memory keyed by data directory and
    /// account, with the same interface as `token_store`.
    pub(super) mod memory_token_store {
        use std::collections::BTreeMap;
        use std::path::{Path, PathBuf};
        use std::sync::Mutex;

        static TOKENS: Mutex<BTreeMap<(PathBuf, String), String>> = Mutex::new(BTreeMap::new());

        pub fn save(data_dir: &Path, account: &str, token: &str) -> Result<(), String> {
            TOKENS.lock().unwrap().insert((data_dir.into(), account.into()), token.into());
            Ok(())
        }

        pub fn load(data_dir: &Path, account: &str) -> Result<Option<String>, String> {
            Ok(TOKENS.lock().unwrap().get(&(data_dir.into(), account.into())).cloned())
        }

        pub fn clear(data_dir: &Path, account: &str) -> Result<(), String> {
            TOKENS.lock().unwrap().remove(&(data_dir.into(), account.into()));
            Ok(())
        }
    }

    struct FixtureDir(PathBuf);
    impl FixtureDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("crossbow-sync-glue-{}", crossbow_core::new_id()));
            std::fs::create_dir(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for FixtureDir {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn me_fixture() -> MeResponse {
        MeResponse {
            account: Account { id: "isolated-fixture".into(), email: "fixture@example.com".into(), created_at: 1 },
            device: Device { id: "fixture-device".into(), name: "Fixture".into(), platform: "macos".into(), created_at: 1, last_seen_at: 1 },
            devices: vec![],
            browser_sign_in: false,
        }
    }

    fn default_reply(request: &Request) -> Reply {
        match request.path.as_str() {
            "/v1/me" => Reply::json(200, serde_json::to_string(&me_fixture()).unwrap()),
            "/v1/sync/ws" => Reply::socket(),
            "/v1/sync/push" => {
                let body: PushRequest = serde_json::from_str(&request.body).unwrap();
                assert!(body.changes.iter().all(|c| c.validate().is_ok()));
                Reply::json(200, serde_json::to_string(&PushResponse { accepted: body.changes.len() as u32, stale: vec![], cursor: 0 }).unwrap())
            }
            path if path.starts_with("/v1/sync/pull?") => Reply::json(200, r#"{"changes":[],"cursor":0,"more":false}"#),
            path => panic!("unexpected fixture request: {path}"),
        }
    }

    fn resume_fixture(glue: &mut SyncGlue, url: &str) {
        glue.resume_fixture(url, &me_fixture().account.id);
    }

    fn poll_until(glue: &mut SyncGlue, lib: &mut Library, done: impl Fn(&SyncGlue, &Library) -> bool) {
        poll_for(glue, lib, Duration::from_secs(5), done);
    }

    fn poll_for(glue: &mut SyncGlue, lib: &mut Library, limit: Duration, done: impl Fn(&SyncGlue, &Library) -> bool) {
        // One clock for every call, so the glue's push timer keeps advancing.
        static CLOCK: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let clock = *CLOCK.get_or_init(Instant::now);
        let ctx = egui::Context::default();
        let start = Instant::now();
        loop {
            ctx.begin_pass(egui::RawInput { time: Some(clock.elapsed().as_secs_f64() + 10.0), ..Default::default() });
            glue.poll(lib, &ctx);
            let mut output = ctx.end_pass();
            output.textures_delta.clear(); // No renderer in this source-only fixture.
            if done(glue, lib) { return; }
            assert!(start.elapsed() < limit, "sync glue fixture timed out: {:?} / {:?}", glue.status, glue.error);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The app reads CROSSBOW_DEV_KEY once, at launch, so each case runs in
    /// its own process; the fixture server stays in this one.
    #[test]
    fn the_dev_key_from_the_environment_goes_with_sign_in_starts() {
        const TEST: &str = "sync_glue::tests::the_dev_key_from_the_environment_goes_with_sign_in_starts";
        if std::env::var("CROSSBOW_DEV_KEY_TEST_CHILD").is_ok() {
            let dir = FixtureDir::new();
            let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
            let mut glue = SyncGlue::new(&dir.0, &lib, || {});
            glue.start_auth("ada@example.com");
            poll_until(&mut glue, &mut lib, |g, _| matches!(&g.auth, AuthStep::Code { challenge_id, .. } if !challenge_id.is_empty()));
            let AuthStep::Code { dev_code, .. } = &glue.auth else { unreachable!() };
            assert_eq!(dev_code.as_deref(), Some("123456"));
            return;
        }
        for key in [Some("fixture-dev-key"), None] {
            let server = Server::start(|request| match request.path.as_str() {
                "/v1/auth/start" => Reply::json(200, r#"{"challenge_id":"chl_fixture","expires_in":600,"dev_code":"123456"}"#),
                "/v1/health" => Reply::json(200, r#"{"ok":true,"protocol":1,"service":"crossbow-api","version":"fixture"}"#),
                path => panic!("unexpected fixture request: {path}"),
            });
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", TEST]).env("CROSSBOW_DEV_KEY_TEST_CHILD", "1").env("CROSSBOW_API_URL", &server.url).env_remove("CROSSBOW_DEV_KEY");
            if let Some(key) = key { child.env("CROSSBOW_DEV_KEY", key); }
            let output = child.output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(output.status.success() && stdout.contains("1 passed"), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
            let starts: Vec<_> = server.requests.lock().unwrap().iter().filter(|r| r.path == "/v1/auth/start").map(|r| r.dev_key.clone()).collect();
            assert_eq!(starts, vec![key.map(String::from)]);
        }
    }

    #[test]
    fn a_source_build_names_no_sync_server_and_signs_in_nowhere() {
        assert_eq!(crossbow_core::sync::server::BUILD_URL, None, "the test build sets no CROSSBOW_API_URL");
        let dir = FixtureDir::new();
        let mut lib = library(&dir);
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        assert_eq!(glue.base_url, "", "a source build names no server");
        glue.start_auth("fixture@example.com");
        assert!(!glue.busy && matches!(glue.auth, AuthStep::Email));
        assert_eq!(glue.error.as_deref(), Some(crossbow_core::sync::server::NO_SERVER));
        poll_until(&mut glue, &mut lib, |_, _| true);
        assert!(matches!(glue.status, SyncStatus::SignedOut));
    }

    #[test]
    fn a_build_server_is_used_unless_the_runtime_override_or_a_saved_choice_names_another() {
        let build = Some("https://sync.example.com/");
        assert_eq!(initial_base_url(None, None, None), "");
        assert_eq!(initial_base_url(None, None, build), "https://sync.example.com");
        assert_eq!(initial_base_url(None, Some("http://127.0.0.1:9".into()), build), "http://127.0.0.1:9");
        assert_eq!(initial_base_url(Some("http://127.0.0.1:8/".into()), Some("http://127.0.0.1:9".into()), build), "http://127.0.0.1:8");

        // A build made for a server signs in there with nothing saved.
        let service = Arc::new(SyncService::default());
        let (server, _network) = account_server(&service, "fixture-account-a", "fixture-device");
        let dir = FixtureDir::new();
        let mut lib = library(&dir);
        let keys = Box::new(crossbow_core::sync::key_store::MemoryKeyStore::default());
        let mut glue = SyncGlue::with_build_server(&dir.0, &lib, || {}, keys, Some(&server.url));
        assert_eq!(glue.base_url, server.url);
        glue.start_auth("fixture@example.com");
        poll_until(&mut glue, &mut lib, |g, _| matches!(&g.auth, AuthStep::Code { challenge_id, .. } if !challenge_id.is_empty()));
        glue.verify("123456");
        poll_until(&mut glue, &mut lib, |g, _| g.signed_in() && matches!(g.status, SyncStatus::Live));
        assert_eq!(glue.session.as_ref().unwrap().base_url, server.url);
    }

    #[test]
    fn a_signed_in_library_keeps_its_own_server_whatever_the_build_names() {
        let service = Arc::new(SyncService::default());
        let (server, _network) = account_server(&service, "fixture-account-a", "fixture-device");
        let dir = FixtureDir::new();
        let mut lib = library(&dir);
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        sign_in(&mut glue, &mut lib, &server.url);
        poll_until(&mut glue, &mut lib, |g, _| matches!(g.status, SyncStatus::Live));
        drop(glue);
        // Without the saved choice of server, the session's own server is still the one.
        lib.store.kv_delete(KV_BASE_URL).unwrap();
        let mark = server.requests.lock().unwrap().len();
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        assert_eq!(glue.base_url, server.url);
        poll_until(&mut glue, &mut lib, |g, _| matches!(g.status, SyncStatus::Live));
        assert!(server.requests.lock().unwrap()[mark..].iter().any(|r| r.path.starts_with("/v1/sync/")));
    }

    #[test]
    fn a_server_the_user_enters_must_be_https_or_on_this_mac() {
        let dir = FixtureDir::new();
        let lib = library(&dir);
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        assert_eq!(glue.set_base_url(&lib, "http://sync.example.com"), Err("plain http:// is allowed only on 127.0.0.1, [::1] or localhost"));
        assert_eq!(glue.set_base_url(&lib, "sync.example.com"), Err("the address must start with https://"));
        assert!(!glue.has_server());
        assert_eq!(lib.store.kv_get(KV_BASE_URL).unwrap(), None, "a refused server is not saved");
        assert_eq!(glue.set_base_url(&lib, " https://sync.example.com/ "), Ok(()));
        assert_eq!(glue.base_url, "https://sync.example.com");
        assert_eq!(lib.store.kv_get(KV_BASE_URL).unwrap().as_deref(), Some("https://sync.example.com"));
        assert_eq!(glue.set_base_url(&lib, "http://127.0.0.1:8787"), Ok(()));
    }

    #[test]
    fn read_only_application_does_not_ack_and_sync_now_retries_after_reopen() {
        let server = Server::start(|request| {
            if request.path.starts_with("/v1/sync/pull?") {
                let seq = if request.path.contains("since=0&") { 1 } else { 2 };
                let change = ServerChange { seq, device_id: "remote-fixture".into(), change: Change {
                    kind: "profile".into(), id: format!("remote-{seq}"), hlc: Hlc::new(100 + seq, 0, "fixture"), deleted: false,
                    enc: Encoding::Json, payload: r#"{"name":"Remote fixture","created_ms":1}"#.into(),
                } };
                Reply::json(200, serde_json::to_string(&PullResponse { changes: vec![change], cursor: seq, more: seq == 1, sync_epoch: 0 }).unwrap())
            } else { default_reply(request) }
        });
        let dir = FixtureDir::new();
        let path = dir.0.join("library.sqlite");
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        lib.create_profile("Local fixture").unwrap();
        for c in lib.store.dirty_changes(10).unwrap() { lib.store.mark_clean(&c.kind, &c.id, &c.hlc).unwrap(); }
        lib.store.kv_set(KV_CURSOR, "0").unwrap();
        drop(lib);
        let read_only = PathBuf::from(format!("file:{}?mode=ro", path.display()));
        let mut lib = Library::open(Store::open(&read_only).unwrap()).unwrap();
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        resume_fixture(&mut glue, &server.url);
        poll_until(&mut glue, &mut lib, |g, _| g.pull_error.is_some());
        assert!(matches!(&glue.status, SyncStatus::Offline(e) if e.contains("Sync Now")));
        assert_eq!(glue.last_sync_ms, None);
        assert_eq!(lib.store.kv_get(KV_CURSOR).unwrap().as_deref(), Some("0"));
        assert_eq!(lib.profiles.len(), 1);
        assert!(!server.requests.lock().unwrap().iter().any(|r| r.path.contains("since=1&")));
        drop(lib);
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        glue.sync_now();
        poll_until(&mut glue, &mut lib, |g, l| g.pull_error.is_none() && l.store.kv_get(KV_CURSOR).unwrap().as_deref() == Some("2"));
        assert!(glue.last_sync_ms.is_some());
        assert!(lib.profiles.contains_key("remote-1") && lib.profiles.contains_key("remote-2"));
        assert_eq!(lib.profiles.len(), 3);
        drop(glue);
        drop(lib);
        let reopened = Library::open(Store::open(&path).unwrap()).unwrap();
        assert_eq!(reopened.profiles.len(), 3);
        assert_eq!(reopened.store.kv_get(KV_CURSOR).unwrap().as_deref(), Some("2"));
    }

    fn signed_out_reply(request: &Request) -> Reply {
        if request.path == "/v1/auth/logout" { Reply::json(200, "{}") } else { default_reply(request) }
    }

    #[test]
    fn sign_out_removes_only_the_key_and_another_account_never_syncs_this_library() {
        let encrypted = Arc::new(SyncService::default());
        let shared = encrypted.clone();
        let server = Server::start(move |request| shared.reply(request, "fixture-device").unwrap_or_else(|| signed_out_reply(request)));
        let other = Arc::new(SyncService::default());
        let shared = other.clone();
        let other_server = Server::start(move |request| shared.reply(request, "fixture-device").unwrap_or_else(|| signed_out_reply(request)));
        let dir = FixtureDir::new();
        let mut lib = Library::open(Store::open(&dir.0.join("library.sqlite")).unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        let account = me_fixture().account.id;
        resume_fixture(&mut glue, &server.url);
        let code = turn_on(&mut glue, &mut lib);
        assert!(glue.keys.load(&account).unwrap().is_some());

        glue.sign_out(&lib);
        assert!(glue.error.is_none());
        assert!(glue.keys.load(&account).unwrap().is_none(), "the user's own sign-out removes this account's library key");
        assert!(glue.recovery_code().is_none());
        assert!(lib.store.kv_get("e2ee_library").unwrap().is_some(), "the device still knows the account is encrypted");

        // Another account on this device never syncs the encrypted library.
        glue.resume_fixture(&other_server.url, "fixture-other-account");
        poll_until(&mut glue, &mut lib, |g, _| g.pull_error.is_some());
        assert_eq!(glue.encryption_status(&lib), Some(E2eeStatus::OtherAccount));
        assert!(glue.error.as_deref().unwrap().contains(OTHER_ACCOUNT));
        assert!(glue.recovery_code().is_none());
        assert!(glue.keys.load("fixture-other-account").unwrap().is_none());
        assert!(other.rows().is_empty(), "nothing of A's library reaches B");
        glue.sign_out(&lib);

        // Signing back in to A needs the code again, then sync resumes encrypted.
        resume_fixture(&mut glue, &server.url);
        poll_until(&mut glue, &mut lib, |g, _| g.e2ee.as_ref().is_some_and(|e| e.status() == E2eeStatus::Locked) && !g.checking_encryption());
        assert_eq!(lib.store.kv_get(KV_CURSOR).unwrap(), None);
        glue.unlock_encryption(&lib, code.reveal()).unwrap();
        poll_until(&mut glue, &mut lib, |g, l| g.pull_error.is_none() && l.store.kv_get(KV_CURSOR).unwrap().as_deref() == Some(encrypted.cursor().to_string().as_str()));
        assert!(encrypted.rows().iter().all(|r| r["deleted"] == true || r["enc"] == "xchacha20_v1"));
    }

    #[test]
    fn local_changes_wait_visibly_while_the_first_pull_fails() {
        let failing = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let flag = failing.clone();
        let server = Server::start(move |request| {
            if request.path.starts_with("/v1/sync/pull?") && flag.load(std::sync::atomic::Ordering::SeqCst) {
                Reply::json(500, r#"{"error":{"code":"internal","message":"fixture pull failure"}}"#)
            } else { default_reply(request) }
        });
        let dir = FixtureDir::new();
        let mut lib = library(&dir);
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        resume_fixture(&mut glue, &server.url);
        let pulls = |server: &Server| server.requests.lock().unwrap().iter().filter(|r| r.path.starts_with("/v1/sync/pull?")).count();
        poll_for(&mut glue, &mut lib, Duration::from_secs(10), |g, _| g.pending > 0 && pulls(&server) >= 2);
        assert_eq!(glue.upload_hold(), Some("Waiting for the first sync before uploading."));
        assert!(!server.requests.lock().unwrap().iter().any(|r| r.path == "/v1/sync/push"), "nothing uploads before a complete pull");

        failing.store(false, std::sync::atomic::Ordering::SeqCst);
        poll_for(&mut glue, &mut lib, Duration::from_secs(20), |g, l| l.store.dirty_count().unwrap() == 0 && g.upload_hold().is_none());
        assert!(server.requests.lock().unwrap().iter().any(|r| r.path == "/v1/sync/push"));
    }

    /// Switches a test flips to act as a hostile server or a lost network.
    #[derive(Default)]
    struct Network {
        reject_session: std::sync::atomic::AtomicBool,
        withhold_rows: std::sync::atomic::AtomicBool,
        offline: std::sync::atomic::AtomicBool,
        /// Refuses the sync socket's upgrade with 503 while pulls and pushes
        /// still work.
        refuse_socket: std::sync::atomic::AtomicBool,
        /// While set, pull answers wait until this switch turns true.
        pull_gate: std::sync::Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>,
        /// Becomes the pull gate when the next upload arrives.
        gate_after_push: std::sync::Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>,
    }

    impl Network {
        fn set(flag: &std::sync::atomic::AtomicBool, value: bool) {
            flag.store(value, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// One account's loopback service with the real sign-in endpoints.
    fn account_server(service: &Arc<SyncService>, account: &str, device: &str) -> (Server, Arc<Network>) {
        use std::sync::atomic::Ordering::SeqCst;
        let network = Arc::new(Network::default());
        let (shared, flags, account, device) = (service.clone(), network.clone(), account.to_string(), device.to_string());
        let server = Server::start(move |request| {
            if flags.offline.load(SeqCst) {
                return Reply::json(503, r#"{"error":{"code":"unavailable","message":"fixture network down"}}"#);
            }
            let me = MeResponse {
                account: Account { id: account.clone(), email: "fixture@example.com".into(), created_at: 1 },
                device: Device { id: device.clone(), name: device.clone(), platform: "macos".into(), created_at: 1, last_seen_at: 1 },
                devices: vec![],
                browser_sign_in: false,
            };
            match request.path.as_str() {
                "/v1/auth/start" => Reply::json(200, r#"{"challenge_id":"fixture-challenge","expires_in":600,"dev_code":"123456"}"#),
                "/v1/auth/verify" => Reply::json(200, serde_json::json!({ "session_token": format!("fixture-token-{account}"), "account": me.account, "device": me.device }).to_string()),
                "/v1/auth/logout" => Reply::json(200, "{}"),
                "/v1/me" => Reply::json(200, serde_json::to_string(&me).unwrap()),
                "/v1/sync/ws" if flags.refuse_socket.load(SeqCst) => Reply::json(503, r#"{"error":{"code":"unavailable","message":"fixture socket refused"}}"#),
                "/v1/sync/ws" => Reply::socket(),
                path if path.starts_with("/v1/sync/") && flags.reject_session.load(SeqCst) => Reply::json(401, r#"{"error":{"code":"unauthorized","message":"fixture session rejected"}}"#),
                path if path.starts_with("/v1/sync/pull?") && flags.withhold_rows.load(SeqCst) => Reply::json(200, r#"{"changes":[],"cursor":0,"more":false}"#),
                path => {
                    if path == "/v1/sync/push" {
                        if let Some(gate) = flags.gate_after_push.lock().unwrap().take() { *flags.pull_gate.lock().unwrap() = Some(gate); }
                    }
                    let mut reply = shared.reply(request, &device).unwrap_or_else(|| panic!("unexpected fixture request: {}", request.path));
                    if path.starts_with("/v1/sync/pull?") { reply.gate = flags.pull_gate.lock().unwrap().clone(); }
                    reply
                }
            }
        });
        (server, network)
    }

    fn library(dir: &FixtureDir) -> Library {
        let mut lib = Library::open(Store::open(&dir.0.join("library.sqlite")).unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        lib
    }

    /// The real sign-in path: email, development code, SignedIn, Resume.
    fn sign_in(glue: &mut SyncGlue, lib: &mut Library, url: &str) {
        glue.set_base_url(lib, url).unwrap();
        glue.start_auth("fixture@example.com");
        poll_until(glue, lib, |g, _| matches!(&g.auth, AuthStep::Code { challenge_id, .. } if !challenge_id.is_empty()));
        glue.verify("123456");
        poll_until(glue, lib, |g, _| !g.busy);
    }

    /// Turn encryption on once a fresh pull allows it, then upload.
    fn turn_on(glue: &mut SyncGlue, lib: &mut Library) -> Hidden<String> {
        poll_until(glue, lib, |g, l| g.encryption_ready() && l.store.dirty_count().unwrap() == 0);
        glue.begin_encryption(lib).unwrap();
        let code = Hidden::from(&glue.recovery_code().unwrap());
        let start = Instant::now();
        while glue.confirm_encryption(lib).is_err() {
            assert!(start.elapsed() < Duration::from_secs(5), "turn-on never became possible");
            poll_until(glue, lib, |_, _| true);
            std::thread::sleep(Duration::from_millis(5));
        }
        // A large library re-uploads in 500-row pushes paced a second apart.
        poll_for(glue, lib, Duration::from_secs(30), |_, l| l.store.dirty_count().unwrap() == 0);
        code
    }

    fn pushed_since(server: &Server, mark: usize) -> Vec<Change> {
        server.requests.lock().unwrap()[mark..].iter().filter(|r| r.path == "/v1/sync/push")
            .flat_map(|r| serde_json::from_str::<PushRequest>(&r.body).unwrap().changes).collect()
    }

    #[test]
    fn a_server_sign_out_and_an_empty_page_never_turn_an_encrypted_library_into_plaintext() {
        let service = Arc::new(SyncService::default());
        let (server, network) = account_server(&service, "fixture-account-a", "fixture-device");
        let dir = FixtureDir::new();
        let mut lib = library(&dir);
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        sign_in(&mut glue, &mut lib, &server.url);
        assert!(glue.signed_in());
        turn_on(&mut glue, &mut lib);
        let space = lib.spaces_ordered()[0].0.clone();
        lib.update_space(&space, |s| s.name = "POST-ENCRYPTION-SECRET".into()).unwrap();
        poll_until(&mut glue, &mut lib, |_, l| l.store.dirty_count().unwrap() == 0);
        assert!(service.rows().iter().all(|r| r["deleted"] == true || r["enc"] == "xchacha20_v1"));

        // The server rejects the session (401), then withholds every row.
        Network::set(&network.reject_session, true);
        glue.sync_now();
        poll_until(&mut glue, &mut lib, |g, _| !g.signed_in());
        Network::set(&network.reject_session, false);
        Network::set(&network.withhold_rows, true);
        assert!(glue.has_library_key("fixture-account-a"), "a server-side sign-out keeps the library key");

        let mark = server.requests.lock().unwrap().len();
        sign_in(&mut glue, &mut lib, &server.url);
        assert!(glue.signed_in());
        poll_until(&mut glue, &mut lib, |_, l| l.store.dirty_count().unwrap() == 0);
        let pushed = pushed_since(&server, mark);
        assert!(!pushed.is_empty(), "the re-signed-in library is uploaded again");
        assert!(pushed.iter().all(|c| c.deleted || c.enc == Encoding::Xchacha20V1), "a live row reached the server unencrypted");
        assert!(!pushed.iter().any(|c| c.payload.contains("POST-ENCRYPTION-SECRET")));
        assert_eq!(glue.encryption_status(&lib), Some(E2eeStatus::On));
    }

    #[test]
    fn another_account_never_receives_an_encrypted_accounts_library() {
        let a_service = Arc::new(SyncService::default());
        let (a_server, _) = account_server(&a_service, "fixture-account-a", "fixture-device");
        let b_service = Arc::new(SyncService::default());
        let (b_server, _) = account_server(&b_service, "fixture-account-b", "fixture-device");
        let dir = FixtureDir::new();
        let mut lib = library(&dir);
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        sign_in(&mut glue, &mut lib, &a_server.url);
        turn_on(&mut glue, &mut lib);
        let space = lib.spaces_ordered()[0].0.clone();
        lib.update_space(&space, |s| s.name = "ENCRYPTED-ACCOUNT-SECRET".into()).unwrap();
        poll_until(&mut glue, &mut lib, |_, l| l.store.dirty_count().unwrap() == 0);
        glue.sign_out(&lib);
        poll_until(&mut glue, &mut lib, |g, _| !g.signed_in() && !g.busy);

        sign_in(&mut glue, &mut lib, &b_server.url);
        poll_until(&mut glue, &mut lib, |g, l| !g.signed_in() && g.error.is_some() || g.signed_in() && l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);
        assert!(b_service.rows().is_empty(), "account B's server received account A's library");
        assert!(pushed_since(&b_server, 0).is_empty());
        assert!(!glue.signed_in());
        assert_eq!(glue.error.as_deref(), Some("This device's library belongs to another account that uses end-to-end encryption. Sign in to that account."));
        // B's new session is ended on the server and never stored here.
        b_server.wait_for("/v1/auth/logout", 1);
        assert!(memory_token_store::load(&dir.0, "fixture-account-b").unwrap().is_none());
        assert!(!b_server.requests.lock().unwrap().iter().any(|r| r.path.starts_with("/v1/sync/")));

        // The encrypted account itself still signs in, Locked until its code.
        sign_in(&mut glue, &mut lib, &a_server.url);
        assert!(glue.signed_in());
        poll_until(&mut glue, &mut lib, |g, _| g.e2ee.as_ref().is_some_and(|e| e.status() == E2eeStatus::Locked));
    }

    /// The account fixture keeps the sync socket open and silent, as the
    /// service does between changes. So while a test holds pulls, no refused
    /// reconnect reports Offline and holds uploads for a catch-up pull that
    /// would wait too. Five seconds of holding cover the reconnects a refused
    /// socket makes after 1 and 3 seconds.
    #[test]
    fn an_account_fixture_holding_pulls_never_reports_offline_or_holds_uploads() {
        let service = Arc::new(SyncService::default());
        let (server, network) = account_server(&service, "fixture-account", "device-a");
        let dir = FixtureDir::new();
        let mut lib = library(&dir);
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        sign_in(&mut glue, &mut lib, &server.url);
        poll_until(&mut glue, &mut lib, |g, l| g.encryption_ready() && l.store.dirty_count().unwrap() == 0 && g.upload_hold().is_none());
        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        *network.pull_gate.lock().unwrap() = Some(released.clone());
        let held_until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < held_until {
            poll_until(&mut glue, &mut lib, |_, _| true);
            assert!(!matches!(glue.status, SyncStatus::Offline(_)) && glue.upload_hold().is_none(), "the held device went offline or held uploads: status={:?}", glue.status);
            std::thread::sleep(Duration::from_millis(10));
        }
        *network.pull_gate.lock().unwrap() = None;
        Network::set(&released, true);
    }

    #[test]
    fn turn_on_becomes_possible_again_after_a_reconnect_with_nothing_queued() {
        let service = Arc::new(SyncService::default());
        let (server, _) = account_server(&service, "fixture-account", "device-a");
        let dir = FixtureDir::new();
        let mut lib = library(&dir);
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        sign_in(&mut glue, &mut lib, &server.url);
        poll_until(&mut glue, &mut lib, |g, l| g.encryption_ready() && l.store.dirty_count().unwrap() == 0);
        // A dropped socket: Turn On waits for another complete pull, and the
        // next automatic one may be a minute away.
        glue.e2ee.as_mut().unwrap().reconnecting();
        assert!(!glue.encryption_ready());
        poll_until(&mut glue, &mut lib, |g, _| g.encryption_ready());
        glue.begin_encryption(&lib).unwrap();
        poll_until(&mut glue, &mut lib, |g, _| g.encryption_confirmable());
        glue.e2ee.as_mut().unwrap().reconnecting();
        assert!(glue.confirm_encryption(&mut lib).is_err());
        poll_until(&mut glue, &mut lib, |g, _| g.encryption_confirmable());
        glue.confirm_encryption(&mut lib).unwrap();
        poll_until(&mut glue, &mut lib, |_, l| l.store.dirty_count().unwrap() == 0);
        assert!(service.rows().iter().all(|r| r["enc"] == "xchacha20_v1" || r["deleted"] == true));
    }

    #[test]
    fn an_encrypted_device_pulls_after_a_reconnect_before_it_uploads() {
        let service = Arc::new(SyncService::default());
        let (server, _) = account_server(&service, "fixture-account", "device-a");
        let dir = FixtureDir::new();
        let mut lib = library(&dir);
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        sign_in(&mut glue, &mut lib, &server.url);
        turn_on(&mut glue, &mut lib);
        // A dropped socket: another device may have reset the key meanwhile,
        // and the next automatic pull may be a minute away.
        glue.e2ee.as_mut().unwrap().reconnecting();
        let mark = server.requests.lock().unwrap().len();
        let space = lib.spaces_ordered()[0].0.clone();
        lib.update_space(&space, |s| s.name = "AFTER-RECONNECT".into()).unwrap();
        assert_eq!(glue.upload_hold(), Some("Waiting to catch up with the server before uploading."));
        poll_until(&mut glue, &mut lib, |g, l| l.store.dirty_count().unwrap() == 0 && g.upload_hold().is_none());
        let paths: Vec<String> = server.requests.lock().unwrap()[mark..].iter().map(|r| r.path.clone()).collect();
        let pull = paths.iter().position(|p| p.starts_with("/v1/sync/pull?")).expect("the waiting device asks for a pull");
        let push = paths.iter().position(|p| p == "/v1/sync/push").expect("the edit uploads after catching up");
        assert!(pull < push, "an encrypted device uploaded before catching up: {paths:?}");
        assert!(service.rows().iter().all(|r| r["deleted"] == true || r["enc"] == "xchacha20_v1"));
    }

    /// The account fixture keeps its socket open, so this test drops it and
    /// refuses the reconnects, as a lost connection does. The device reports
    /// Offline, holds uploads until a catch-up pull, keeps uploading after
    /// pulls while the socket stays refused, and reports Live once a
    /// reconnect is accepted.
    #[test]
    fn a_dropped_socket_reports_offline_and_holds_uploads_until_a_catch_up_pull() {
        let service = Arc::new(SyncService::default());
        let (server, network) = account_server(&service, "fixture-account", "device-a");
        let dir = FixtureDir::new();
        let mut lib = library(&dir);
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        sign_in(&mut glue, &mut lib, &server.url);
        turn_on(&mut glue, &mut lib);
        poll_until(&mut glue, &mut lib, |g, l| matches!(g.status, SyncStatus::Live) && l.store.dirty_count().unwrap() == 0 && g.upload_hold().is_none());

        Network::set(&network.refuse_socket, true);
        let mark = server.requests.lock().unwrap().len();
        let deadline = Instant::now() + Duration::from_secs(5);
        while server.drop_sockets() == 0 {
            assert!(Instant::now() < deadline, "no open socket to drop");
            std::thread::sleep(Duration::from_millis(5));
        }
        poll_until(&mut glue, &mut lib, |g, _| g.upload_hold() == Some("Waiting to catch up with the server before uploading."));
        let space = lib.spaces_ordered()[0].0.clone();
        lib.update_space(&space, |s| s.name = "AFTER-THE-DROP".into()).unwrap();
        poll_for(&mut glue, &mut lib, Duration::from_secs(15), |g, l| l.store.dirty_count().unwrap() == 0 && matches!(&g.status, SyncStatus::Offline(e) if e.contains("503")));
        let paths: Vec<String> = server.requests.lock().unwrap()[mark..].iter().map(|r| r.path.clone()).collect();
        let pull = paths.iter().position(|p| p.starts_with("/v1/sync/pull?")).expect("the held device asks for a pull");
        let push = paths.iter().position(|p| p == "/v1/sync/push").expect("the edit uploads while the socket is refused");
        assert!(pull < push, "the device uploaded before catching up: {paths:?}");
        assert!(paths.iter().any(|p| p == "/v1/sync/ws"), "the device never retried the socket: {paths:?}");

        Network::set(&network.refuse_socket, false);
        poll_for(&mut glue, &mut lib, Duration::from_secs(20), |g, _| matches!(g.status, SyncStatus::Live));
        assert!(service.rows().iter().all(|r| r["deleted"] == true || r["enc"] == "xchacha20_v1"));
    }

    #[test]
    fn an_outranked_version_stops_uploading_but_an_edit_made_after_the_stale_answer_still_syncs() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        let service = Arc::new(SyncService::default());
        let (server, network) = account_server(&service, "fixture-account", "device-a");
        let dir = FixtureDir::new();
        let mut lib = library(&dir);
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        sign_in(&mut glue, &mut lib, &server.url);
        turn_on(&mut glue, &mut lib);
        poll_until(&mut glue, &mut lib, |g, l| l.store.dirty_count().unwrap() == 0 && g.upload_hold().is_none() && !g.push_in_flight);
        // The server holds a newer version this device can't open, as from a
        // device still on an earlier recovery code. This device hasn't pulled it.
        let space = lib.spaces_ordered()[0].0.clone();
        let ahead = Hlc::new(crossbow_core::now_ms() + 3_600_000, 0, "server");
        service.rewrite("space", &space, |row| row["hlc"] = serde_json::json!(ahead.encode()));
        let released = Arc::new(AtomicBool::new(false));
        *network.gate_after_push.lock().unwrap() = Some(released.clone());
        let pushes = |server: &Server| server.requests.lock().unwrap().iter().filter(|r| r.path == "/v1/sync/push").count();
        let pulled_after_push = |server: &Server| {
            let requests = server.requests.lock().unwrap();
            requests.iter().rposition(|r| r.path == "/v1/sync/push").is_some_and(|i| requests[i..].iter().any(|r| r.path.starts_with("/v1/sync/pull?")))
        };

        // An edit loses last-writer-wins. The stale answer asks for a pull,
        // which the server holds while the user edits again.
        lib.update_space(&space, |s| s.name = "OUTRANKED-EDIT".into()).unwrap();
        poll_until(&mut glue, &mut lib, |g, _| !g.outranked.is_empty() && pulled_after_push(&server));
        lib.update_space(&space, |s| s.name = "EDIT-AFTER-THE-STALE-ANSWER".into()).unwrap();
        let edited = lib.store.entity_hlc("space", &space).unwrap().unwrap();
        *network.pull_gate.lock().unwrap() = None;
        released.store(true, SeqCst);
        // Let the held page reach the glue before it looks at the queue again.
        std::thread::sleep(Duration::from_millis(300));
        let mark = server.requests.lock().unwrap().len();
        poll_for(&mut glue, &mut lib, Duration::from_secs(10), |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight && g.outranked.is_empty());
        assert!(pushed_since(&server, mark).iter().any(|c| c.kind == "space" && c.id == space && c.hlc == edited), "the edit made after the stale answer was never uploaded");

        // That version stops after its own stale answer; the local copy stays.
        let settled = pushes(&server);
        let until = Instant::now() + Duration::from_secs(3);
        poll_for(&mut glue, &mut lib, Duration::from_secs(5), |_, _| Instant::now() >= until);
        assert_eq!(pushes(&server), settled, "an outranked version was uploaded again");
        assert_eq!(lib.spaces[&space].name, "EDIT-AFTER-THE-STALE-ANSWER");

        // The skipped row's clock was observed, so the next edit wins.
        lib.update_space(&space, |s| s.name = "EDIT-AFTER-THE-PULL".into()).unwrap();
        let next = lib.store.entity_hlc("space", &space).unwrap().unwrap();
        assert!(next > ahead, "{} sorts below the skipped {}", next.encode(), ahead.encode());
        poll_for(&mut glue, &mut lib, Duration::from_secs(10), |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);
        let row = service.rows().into_iter().find(|r| r["id"] == space.as_str()).unwrap();
        assert_eq!(row["hlc"], next.encode());
        assert_eq!(row["enc"], "xchacha20_v1");
    }

    /// Poll and retry an action the UI offers again, such as Unlock.
    fn retry_until(glue: &mut SyncGlue, lib: &mut Library, limit: Duration, mut action: impl FnMut(&mut SyncGlue, &mut Library) -> Result<(), String>) {
        let start = Instant::now();
        loop {
            match action(glue, lib) {
                Ok(()) => return,
                Err(error) => assert!(start.elapsed() < limit, "still refused after {limit:?}: {error}"),
            }
            poll_until(glue, lib, |_, _| true);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn locked(glue: &SyncGlue) -> bool {
        glue.e2ee.as_ref().is_some_and(|e| e.status() == E2eeStatus::Locked)
    }

    #[test]
    fn a_third_device_finds_the_newest_marker_past_a_page_of_rows_under_a_lost_key() {
        let service = Arc::new(SyncService::default());
        // Device A holds a large library under a key that is then lost with it.
        let (a_server, _) = account_server(&service, "fixture-account", "device-a");
        let a_dir = FixtureDir::new();
        let mut a_lib = library(&a_dir);
        let space = a_lib.spaces_ordered()[0].0.clone();
        for i in 0..1005 { a_lib.add_pinned(&space, None, &format!("https://example.com/{i}"), &format!("Item {i}")).unwrap(); }
        let mut a = SyncGlue::new(&a_dir.0, &a_lib, || {});
        sign_in(&mut a, &mut a_lib, &a_server.url);
        poll_for(&mut a, &mut a_lib, Duration::from_secs(30), |g, l| g.encryption_ready() && l.store.dirty_count().unwrap() == 0);
        turn_on(&mut a, &mut a_lib);
        poll_for(&mut a, &mut a_lib, Duration::from_secs(30), |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);
        drop(a);
        assert!(service.rows().iter().filter(|r| r["enc"] == "xchacha20_v1").count() > 1005);

        // A new Mac resets without A's code.
        let (b_server, _) = account_server(&service, "fixture-account", "device-b");
        let b_dir = FixtureDir::new();
        let mut b_lib = library(&b_dir);
        let b_space = b_lib.spaces_ordered()[0].0.clone();
        b_lib.update_space(&b_space, |s| s.name = "Library Of The Reset Device".into()).unwrap();
        let mut b = SyncGlue::new(&b_dir.0, &b_lib, || {});
        sign_in(&mut b, &mut b_lib, &b_server.url);
        poll_for(&mut b, &mut b_lib, Duration::from_secs(30), |g, _| locked(g));
        b.begin_reset_encryption(&b_lib).unwrap();
        let code = b.recovery_code().unwrap().as_str().to_string();
        retry_until(&mut b, &mut b_lib, Duration::from_secs(10), |g, l| g.confirm_reset_encryption(l));
        poll_for(&mut b, &mut b_lib, Duration::from_secs(30), |g, l| l.store.dirty_count().unwrap() == 0 && g.upload_hold().is_none() && !g.push_in_flight);
        assert_eq!(b.encryption_status(&b_lib), Some(E2eeStatus::On));

        // A third Mac: its first page holds only rows under the lost key.
        let (c_server, _) = account_server(&service, "fixture-account", "device-c");
        let c_dir = FixtureDir::new();
        let mut c_lib = library(&c_dir);
        let mut c = SyncGlue::new(&c_dir.0, &c_lib, || {});
        sign_in(&mut c, &mut c_lib, &c_server.url);
        poll_for(&mut c, &mut c_lib, Duration::from_secs(30), |g, _| locked(g));
        retry_until(&mut c, &mut c_lib, Duration::from_secs(10), |g, l| g.unlock_encryption(l, &code));
        poll_for(&mut c, &mut c_lib, Duration::from_secs(30), |_, l| l.spaces.get(&b_space).is_some_and(|s| s.name == "Library Of The Reset Device"));
        assert!(c_lib.items.is_empty(), "rows under the lost key are never applied");
        assert_eq!(c.encryption_status(&c_lib), Some(E2eeStatus::On));
        assert!(service.rows().iter().all(|r| r["deleted"] == true || r["enc"] == "xchacha20_v1"));
    }

    /// Device D locks, finishes its scan and stops pulling. Only then does
    /// device A reset; D isn't told. Returns D and A's new code.
    fn locked_before_a_later_reset(service: &Arc<SyncService>) -> (FixtureDir, Library, SyncGlue, Server, Arc<Network>, String) {
        let (a_server, _) = account_server(service, "fixture-account", "device-a");
        let a_dir = FixtureDir::new();
        let mut a_lib = library(&a_dir);
        let space = a_lib.spaces_ordered()[0].0.clone();
        a_lib.update_space(&space, |s| s.name = "Library Of Device A".into()).unwrap();
        let mut a = SyncGlue::new(&a_dir.0, &a_lib, || {});
        sign_in(&mut a, &mut a_lib, &a_server.url);
        turn_on(&mut a, &mut a_lib);

        let (d_server, d_network) = account_server(service, "fixture-account", "device-d");
        let d_dir = FixtureDir::new();
        let mut d_lib = library(&d_dir);
        let mut d = SyncGlue::new(&d_dir.0, &d_lib, || {});
        sign_in(&mut d, &mut d_lib, &d_server.url);
        poll_until(&mut d, &mut d_lib, |g, _| locked(g) && !g.checking_encryption() && g.e2ee.as_ref().is_some_and(LibraryEncryption::has_check_row));
        let pulls = |server: &Server| server.requests.lock().unwrap().iter().filter(|r| r.path.starts_with("/v1/sync/pull?")).count();
        let settled = Instant::now() + Duration::from_millis(300);
        poll_until(&mut d, &mut d_lib, |_, _| Instant::now() >= settled);
        let paused = pulls(&d_server);

        a.begin_reset_encryption(&a_lib).unwrap();
        let code = a.recovery_code().unwrap().as_str().to_string();
        retry_until(&mut a, &mut a_lib, Duration::from_secs(10), |g, l| g.confirm_reset_encryption(l));
        poll_for(&mut a, &mut a_lib, Duration::from_secs(10), |g, l| l.store.dirty_count().unwrap() == 0 && g.upload_hold().is_none() && !g.push_in_flight);
        let settled = Instant::now() + Duration::from_millis(300);
        poll_until(&mut d, &mut d_lib, |_, _| Instant::now() >= settled);
        assert_eq!(pulls(&d_server), paused, "the locked device kept pulling");
        assert!(locked(&d));
        (d_dir, d_lib, d, d_server, d_network, code)
    }

    #[test]
    fn a_locked_device_takes_the_code_of_a_later_reset_without_sync_now() {
        let service = Arc::new(SyncService::default());
        let (_dir, mut lib, mut d, _server, _, code) = locked_before_a_later_reset(&service);
        d.unlock_encryption(&lib, &code).expect("Unlock looks at the server before judging the code");
        poll_for(&mut d, &mut lib, Duration::from_secs(10), |g, _| !g.checking_encryption());
        assert_eq!(d.unlock_error, None);
        assert_eq!(d.encryption_status(&lib), Some(E2eeStatus::On));
        poll_for(&mut d, &mut lib, Duration::from_secs(10), |_, l| l.spaces.values().any(|s| s.name == "Library Of Device A"));
    }

    #[test]
    fn offline_a_locked_device_never_calls_a_code_it_couldnt_check_wrong() {
        use crossbow_core::sync::e2ee::{LibraryKey, UNCHECKED};
        let service = Arc::new(SyncService::default());
        let (_dir, mut lib, mut d, _server, network, code) = locked_before_a_later_reset(&service);
        Network::set(&network.offline, true);
        d.unlock_encryption(&lib, &code).expect("Unlock looks at the server before judging the code");
        poll_for(&mut d, &mut lib, Duration::from_secs(10), |g, _| !g.checking_encryption());
        assert_eq!(d.unlock_error.as_deref(), Some(UNCHECKED));
        assert!(locked(&d));
        // A code that matches nothing known here is not called wrong either.
        let other = RecoveryCode::for_key(&LibraryKey::generate().unwrap()).as_str().to_string();
        d.unlock_encryption(&lib, &other).unwrap();
        poll_for(&mut d, &mut lib, Duration::from_secs(10), |g, _| !g.checking_encryption());
        assert_eq!(d.unlock_error.as_deref(), Some(UNCHECKED));
        // A typo needs no server.
        assert!(d.unlock_encryption(&lib, "CB1-not-a-code").is_err());

        Network::set(&network.offline, false);
        d.unlock_encryption(&lib, &code).unwrap();
        poll_for(&mut d, &mut lib, Duration::from_secs(10), |g, _| !g.checking_encryption());
        assert_eq!(d.unlock_error, None);
        assert_eq!(d.encryption_status(&lib), Some(E2eeStatus::On));
        poll_for(&mut d, &mut lib, Duration::from_secs(10), |_, l| l.spaces.values().any(|s| s.name == "Library Of Device A"));
    }

    #[test]
    fn sync_now_on_a_locked_device_with_paused_pulls_scans_instead_of_pulling() {
        let service = Arc::new(SyncService::default());
        let (_dir, mut lib, mut d, _server, network, code) = locked_before_a_later_reset(&service);
        // Hold the next read so the test sees which request Sync Now sent.
        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        *network.pull_gate.lock().unwrap() = Some(released.clone());
        d.sync_now();
        poll_until(&mut d, &mut lib, |_, _| true);
        assert!(d.checking_encryption(), "Sync Now pulled the refused page again instead of scanning");
        *network.pull_gate.lock().unwrap() = None;
        Network::set(&released, true);
        poll_for(&mut d, &mut lib, Duration::from_secs(10), |g, _| !g.checking_encryption());
        assert!(locked(&d));
        d.unlock_encryption(&lib, &code).unwrap();
        poll_for(&mut d, &mut lib, Duration::from_secs(10), |g, _| g.e2ee.as_ref().is_some_and(|e| e.status() == E2eeStatus::On));
    }

    #[test]
    fn a_failed_read_of_the_encryption_record_stops_a_sign_in() {
        let a_service = Arc::new(SyncService::default());
        let (a_server, _) = account_server(&a_service, "fixture-account-a", "fixture-device");
        let b_service = Arc::new(SyncService::default());
        let (b_server, _) = account_server(&b_service, "fixture-account-b", "fixture-device");
        let dir = FixtureDir::new();
        let mut lib = library(&dir);
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        sign_in(&mut glue, &mut lib, &a_server.url);
        turn_on(&mut glue, &mut lib);
        glue.sign_out(&lib);
        poll_until(&mut glue, &mut lib, |g, _| !g.signed_in() && !g.busy);
        let record = lib.store.kv_get("e2ee_library").unwrap().unwrap();

        // Reading account A's record fails while account B's sign-in completes.
        let other = rusqlite::Connection::open(dir.0.join("library.sqlite")).unwrap();
        other.execute("UPDATE kv SET v = X'FF' WHERE k = 'e2ee_library'", []).unwrap();
        assert!(lib.store.kv_get("e2ee_library").is_err());
        sign_in(&mut glue, &mut lib, &b_server.url);
        assert!(!glue.signed_in());
        assert_eq!(glue.error.as_deref(), Some("Couldn't read this device's encryption state, so this sign-in was stopped. Try again."));
        b_server.wait_for("/v1/auth/logout", 1);
        assert!(lib.store.kv_get(KV_SESSION).unwrap().is_none(), "the session was saved");
        assert!(memory_token_store::load(&dir.0, "fixture-account-b").unwrap().is_none());
        assert_eq!(lib.store.dirty_count().unwrap(), 0, "the library was queued for another account");
        assert!(!b_server.requests.lock().unwrap().iter().any(|r| r.path.starts_with("/v1/sync/")));

        // Once the record reads again, B is refused as another account.
        other.execute("UPDATE kv SET v = ?1 WHERE k = 'e2ee_library'", [&record]).unwrap();
        sign_in(&mut glue, &mut lib, &b_server.url);
        assert!(!glue.signed_in());
        assert_eq!(glue.error.as_deref(), Some(OTHER_ACCOUNT));
        assert!(b_service.rows().is_empty());
    }

    #[test]
    fn a_device_that_was_offline_pulls_before_it_uploads_plaintext() {
        let service = Arc::new(SyncService::default());
        let (a_server, _) = account_server(&service, "fixture-account", "device-a");
        let (b_server, b_network) = account_server(&service, "fixture-account", "device-b");
        let (a_dir, b_dir) = (FixtureDir::new(), FixtureDir::new());
        let (mut a_lib, mut b_lib) = (library(&a_dir), library(&b_dir));
        let mut a = SyncGlue::new(&a_dir.0, &a_lib, || {});
        let mut b = SyncGlue::new(&b_dir.0, &b_lib, || {});
        sign_in(&mut a, &mut a_lib, &a_server.url);
        sign_in(&mut b, &mut b_lib, &b_server.url);
        poll_until(&mut a, &mut a_lib, |g, l| g.encryption_ready() && l.store.dirty_count().unwrap() == 0);
        poll_until(&mut b, &mut b_lib, |g, l| g.encryption_ready() && l.store.dirty_count().unwrap() == 0);
        a.sync_now();
        poll_until(&mut a, &mut a_lib, |g, l| g.encryption_ready() && l.spaces.len() == 2);

        // B loses its network and edits; A turns encryption on meanwhile.
        let offline = b_server.requests.lock().unwrap().len();
        Network::set(&b_network.offline, true);
        let space = b_lib.spaces_ordered()[0].0.clone();
        b_lib.update_space(&space, |s| s.name = "OFFLINE-EDIT-SECRET".into()).unwrap();
        poll_until(&mut b, &mut b_lib, |_, _| b_server.requests.lock().unwrap()[offline..].iter().any(|r| r.path == "/v1/sync/push" || r.path.starts_with("/v1/sync/pull?")));
        let code = turn_on(&mut a, &mut a_lib);

        // Back online, B must learn the account is encrypted before uploading.
        let mark = b_server.requests.lock().unwrap().len();
        Network::set(&b_network.offline, false);
        poll_for(&mut b, &mut b_lib, Duration::from_secs(15), |g, l| g.e2ee.as_ref().is_some_and(|e| e.status() == E2eeStatus::Locked) || l.store.dirty_count().unwrap() == 0);
        let pushed = pushed_since(&b_server, mark);
        assert!(pushed.iter().all(|c| c.deleted || c.enc == Encoding::Xchacha20V1), "B uploaded plaintext after reconnecting: {pushed:?}");
        assert!(!service.rows().iter().any(|r| r["enc"] == "json" && r["deleted"] == false));
        assert_eq!(b.encryption_status(&b_lib), Some(E2eeStatus::Locked));

        // With the code, B's offline edit is uploaded sealed and reaches A.
        poll_until(&mut b, &mut b_lib, |g, _| !g.checking_encryption());
        b.unlock_encryption(&b_lib, code.reveal()).unwrap();
        poll_until(&mut b, &mut b_lib, |g, l| g.pull_error.is_none() && l.store.dirty_count().unwrap() == 0);
        a.sync_now();
        poll_until(&mut a, &mut a_lib, |_, l| l.spaces.get(&space).is_some_and(|s| s.name == "OFFLINE-EDIT-SECRET"));
        assert!(!service.rows().iter().any(|r| r["payload"].as_str().unwrap().contains("OFFLINE-EDIT-SECRET")));
    }

    #[test]
    fn oversized_boost_stays_visible_and_local_while_valid_data_syncs() {
        let server = Server::start(default_reply);
        let dir = FixtureDir::new();
        let path = dir.0.join("library.sqlite");
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        let mut boost = crossbow_core::model::Boost { name: "Fixture Boost".into(), host: "fixture.example".into(), css: "x".repeat(crossbow_core::proto::MAX_PAYLOAD_BYTES + 1), js: String::new(), enabled: true };
        let id = lib.put_boost(None, &boost).unwrap();
        lib.create_profile("Valid fixture").unwrap();
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        resume_fixture(&mut glue, &server.url);
        poll_until(&mut glue, &mut lib, |g, l| g.rejection_error.is_some() && !g.push_in_flight && l.store.dirty_count().unwrap() == 1);
        assert!(matches!(&glue.status, SyncStatus::Offline(e) if e.contains("64 KiB") && e.contains("Sync Now")));
        assert_eq!(lib.boosts[&id], boost);
        assert_eq!(server.requests.lock().unwrap().iter().filter(|r| r.path == "/v1/sync/push").count(), 1);
        // An explicit retry of unchanged invalid data remains local and bounded.
        glue.sync_now();
        poll_until(&mut glue, &mut lib, |g, l| !g.retry_rejections && !g.push_in_flight && l.store.sync_rejection_reason().unwrap().is_some());
        assert_eq!(server.requests.lock().unwrap().iter().filter(|r| r.path == "/v1/sync/push").count(), 1);
        drop(glue);
        drop(lib);
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        assert_eq!(lib.boosts[&id], boost);
        assert_eq!(lib.store.dirty_count().unwrap(), 1);
        assert!(lib.store.sync_changes(10).unwrap().is_empty());
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        resume_fixture(&mut glue, &server.url);
        poll_until(&mut glue, &mut lib, |g, _| g.rejection_error.is_some());
        assert!(glue.error.as_deref().unwrap().contains("kept locally"));
        boost.css = "body { color: red; }".into();
        lib.put_boost(Some(&id), &boost).unwrap();
        glue.last_push_check = f64::NEG_INFINITY;
        poll_until(&mut glue, &mut lib, |g, l| g.rejection_error.is_none() && !g.push_in_flight && l.store.dirty_count().unwrap() == 0);
        assert!(glue.error.is_none());
        assert_eq!(server.requests.lock().unwrap().iter().filter(|r| r.path == "/v1/sync/push").count(), 2);
        drop(glue);
        drop(lib);
        let reopened = Library::open(Store::open(&path).unwrap()).unwrap();
        assert_eq!(reopened.boosts[&id], boost);
        assert_eq!(reopened.store.dirty_count().unwrap(), 0);
    }


    // -- deleting the account's sync data ------------------------------------------

    /// What the user sees of the library: Spaces, profiles and pins.
    fn sidebar(lib: &Library) -> (Vec<(String, String)>, usize, Vec<(String, String)>) {
        let spaces = lib.spaces_ordered().into_iter().map(|(id, space)| (id, space.name)).collect();
        let mut items: Vec<_> = lib.items.values().map(|item| (item.url.clone(), item.title.clone())).collect();
        items.sort();
        (spaces, lib.profiles.len(), items)
    }

    /// A signed-in device with one pin, synced.
    fn synced_device(service: &Arc<SyncService>, account: &str, device: &str) -> (FixtureDir, Library, SyncGlue, Server, Arc<Network>) {
        let (server, network) = account_server(service, account, device);
        let dir = FixtureDir::new();
        let mut lib = library(&dir);
        let space = lib.spaces_ordered()[0].0.clone();
        lib.add_pinned(&space, None, &format!("https://example.test/{device}"), &format!("Pinned on {device}")).unwrap();
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        sign_in(&mut glue, &mut lib, &server.url);
        poll_until(&mut glue, &mut lib, |g, l| l.store.dirty_count().unwrap() == 0 && g.upload_hold().is_none() && !g.push_in_flight);
        (dir, lib, glue, server, network)
    }

    fn sync_requests_since(server: &Server, mark: usize) -> Vec<String> {
        server.requests.lock().unwrap()[mark..].iter().filter(|r| r.path.starts_with("/v1/sync/") && r.path != "/v1/sync/ws").map(|r| format!("{} {}", r.method, r.path)).collect()
    }

    /// Poll for a while, as a running app would.
    fn idle(glue: &mut SyncGlue, lib: &mut Library, time: Duration) {
        let until = Instant::now() + time;
        poll_until(glue, lib, |_, _| Instant::now() >= until);
    }

    #[test]
    fn deleting_sync_data_keeps_this_library_and_stays_off_through_a_reopen() {
        let service = Arc::new(SyncService::default());
        let (dir, mut lib, mut glue, server, _) = synced_device(&service, "fixture-account-a", "device-a");
        let other = Arc::new(SyncService::default());
        let (_other_dir, _other_lib, _other_glue, _other_server, _) = synced_device(&other, "fixture-account-b", "device-b");
        let theirs = other.rows();
        let before = sidebar(&lib);
        assert!(!service.rows().is_empty());

        glue.delete_sync_data(&lib);
        poll_until(&mut glue, &mut lib, |g, _| g.sync_off.is_some());
        assert!(service.rows().is_empty(), "the server copy is gone");
        assert_eq!(service.sync_epoch(), 1);
        assert_eq!(other.rows(), theirs, "another account's sync data was touched");
        assert_eq!(sidebar(&lib), before, "this Mac's library changed");
        assert_eq!(lib.store.dirty_count().unwrap(), 0);
        assert_eq!(glue.sync_off_text(), Some(SYNC_OFF_HERE));
        assert!(glue.signed_in(), "the session stays for Turn Sync On");
        assert!(glue.delete_error.is_none());

        // An edit made now waits; nothing reaches the server.
        let mark = server.requests.lock().unwrap().len();
        let space = lib.spaces_ordered()[0].0.clone();
        lib.update_space(&space, |s| s.name = "Edited While Off".into()).unwrap();
        glue.sync_now();
        idle(&mut glue, &mut lib, Duration::from_millis(1500));
        assert_eq!(sync_requests_since(&server, mark), Vec::<String>::new());
        assert!(service.rows().is_empty());

        // Reopen: still off, library intact, nothing sent.
        let before = sidebar(&lib);
        drop(glue);
        drop(lib);
        let mut lib = library(&dir);
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        idle(&mut glue, &mut lib, Duration::from_millis(1200));
        assert_eq!(glue.sync_off_text(), Some(SYNC_OFF_HERE));
        assert_eq!(sidebar(&lib), before);
        assert_eq!(sync_requests_since(&server, mark), Vec::<String>::new());
        assert!(service.rows().is_empty());
    }

    #[test]
    fn a_delete_that_fails_or_that_the_server_cant_do_changes_nothing_and_can_be_retried() {
        let service = Arc::new(SyncService::default());
        let (_dir, mut lib, mut glue, _server, network) = synced_device(&service, "fixture-account", "device-a");
        let (rows, before) = (service.rows(), sidebar(&lib));
        let unchanged = |glue: &SyncGlue, lib: &Library| {
            assert!(glue.sync_off.is_none());
            assert_eq!(service.rows(), rows);
            assert_eq!(service.sync_epoch(), 0);
            assert_eq!(sidebar(lib), before);
        };

        Network::set(&network.offline, true);
        glue.delete_sync_data(&lib);
        poll_until(&mut glue, &mut lib, |g, _| !g.deleting);
        assert!(glue.delete_error.as_deref().unwrap().contains("Nothing changed on this Mac"), "{:?}", glue.delete_error);
        unchanged(&glue, &lib);
        Network::set(&network.offline, false);

        // The deployed Worker has no such route yet: 404.
        service.lack_delete();
        glue.delete_sync_data(&lib);
        poll_until(&mut glue, &mut lib, |g, _| !g.deleting);
        assert_eq!(glue.delete_error.as_deref(), Some(CANT_DELETE_YET));
        unchanged(&glue, &lib);
        // Sync carries on as before.
        let space = lib.spaces_ordered()[0].0.clone();
        lib.update_space(&space, |s| s.name = "Still Syncing".into()).unwrap();
        poll_until(&mut glue, &mut lib, |_, l| l.store.dirty_count().unwrap() == 0);
        assert!(service.rows().iter().any(|r| r["payload"].as_str().unwrap().contains("Still Syncing")));
    }

    #[test]
    fn a_device_holding_deleted_data_stops_with_the_reason_and_never_republishes_it() {
        let service = Arc::new(SyncService::default());
        let (_a_dir, mut a_lib, mut a, _a_server, _) = synced_device(&service, "fixture-account", "device-a");
        let (b_dir, mut b_lib, mut b, b_server, _) = synced_device(&service, "fixture-account", "device-b");
        a.delete_sync_data(&a_lib);
        poll_until(&mut a, &mut a_lib, |g, _| g.sync_off.is_some());

        // B edits and syncs: its upload is refused and it stops.
        let before = sidebar(&b_lib);
        let space = b_lib.spaces_ordered()[0].0.clone();
        b_lib.update_space(&space, |s| s.name = "Stale Edit".into()).unwrap();
        b.sync_now();
        poll_until(&mut b, &mut b_lib, |g, _| g.sync_off.is_some());
        assert_eq!(b.sync_off_text(), Some(SYNC_OFF_ELSEWHERE));
        assert!(b.signed_in());
        assert!(service.rows().is_empty(), "deleted data came back");
        assert_ne!(sidebar(&b_lib), before, "the stale device keeps its own edit");
        let mark = b_server.requests.lock().unwrap().len();
        idle(&mut b, &mut b_lib, Duration::from_millis(1500));
        assert_eq!(sync_requests_since(&b_server, mark), Vec::<String>::new(), "it kept syncing");

        // Reopened, it stays off.
        drop(b);
        drop(b_lib);
        let mut b_lib = library(&b_dir);
        let mut b = SyncGlue::new(&b_dir.0, &b_lib, || {});
        idle(&mut b, &mut b_lib, Duration::from_millis(1200));
        assert_eq!(b.sync_off_text(), Some(SYNC_OFF_ELSEWHERE));
        assert!(service.rows().is_empty());
    }

    #[test]
    fn turning_sync_on_again_uploads_this_library_as_a_fresh_start_on_the_same_session() {
        let service = Arc::new(SyncService::default());
        let (_dir, mut lib, mut glue, server, _) = synced_device(&service, "fixture-account", "device-a");
        glue.delete_sync_data(&lib);
        poll_until(&mut glue, &mut lib, |g, _| g.sync_off.is_some());
        let mark = server.requests.lock().unwrap().len();
        glue.turn_sync_on(&mut lib).unwrap();
        assert!(glue.sync_off.is_none());
        poll_until(&mut glue, &mut lib, |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight && !service.rows().is_empty());
        let local: i64 = rusqlite::Connection::open(_dir.0.join("library.sqlite")).unwrap().query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0)).unwrap();
        assert_eq!(service.rows().len() as i64, local, "the whole library uploads again");
        let requests = server.requests.lock().unwrap()[mark..].iter().map(|r| r.path.clone()).collect::<Vec<_>>();
        assert!(!requests.iter().any(|p| p.starts_with("/v1/auth/")), "no new sign-in: {requests:?}");
        // Its uploads named the new epoch, so the server took them.
        let pushes: Vec<PushRequest> = server.requests.lock().unwrap()[mark..].iter().filter(|r| r.path == "/v1/sync/push").map(|r| serde_json::from_str(&r.body).unwrap()).collect();
        assert!(!pushes.is_empty() && pushes.iter().all(|p| p.sync_epoch == Some(1)));
    }

    #[test]
    fn an_encrypted_device_holding_deleted_data_cant_upload_its_old_ciphertext() {
        let service = Arc::new(SyncService::default());
        let (_a_dir, mut a_lib, mut a, _a_server, _) = synced_device(&service, "fixture-account", "device-a");
        let old_code = turn_on(&mut a, &mut a_lib);
        let (b_server, _) = account_server(&service, "fixture-account", "device-b");
        let b_dir = FixtureDir::new();
        let mut b_lib = library(&b_dir);
        let mut b = SyncGlue::new(&b_dir.0, &b_lib, || {});
        sign_in(&mut b, &mut b_lib, &b_server.url);
        poll_until(&mut b, &mut b_lib, |g, _| locked(g));
        retry_until(&mut b, &mut b_lib, Duration::from_secs(10), |g, l| g.unlock_encryption(l, old_code.reveal()));
        poll_for(&mut b, &mut b_lib, Duration::from_secs(30), |g, l| g.e2ee.as_ref().is_some_and(|e| e.status() == E2eeStatus::On) && l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);

        a.delete_sync_data(&a_lib);
        poll_until(&mut a, &mut a_lib, |g, _| g.sync_off.is_some());
        assert!(a.sync_off.as_ref().unwrap().encrypted);
        assert!(!a.has_library_key("fixture-account"), "the old key was kept");

        // B seals an edit under the old key; the server refuses it.
        let mark = b_server.requests.lock().unwrap().len();
        let space = b_lib.spaces_ordered()[0].0.clone();
        b_lib.update_space(&space, |s| s.name = "Sealed Under The Old Key".into()).unwrap();
        // Its upload goes before any pull could tell it about the delete.
        b.last_push_check = f64::NEG_INFINITY;
        poll_until(&mut b, &mut b_lib, |g, _| g.sync_off.is_some());
        let refused = pushed_since(&b_server, mark);
        assert!(refused.iter().any(|c| c.enc == Encoding::Xchacha20V1), "the stale device tried to upload ciphertext");
        assert!(service.rows().is_empty(), "old ciphertext reached the new epoch");
        assert!(!b.has_library_key("fixture-account"));

        // A turns sync on: nothing uploads until encryption is on again.
        a.turn_sync_on(&mut a_lib).unwrap();
        idle(&mut a, &mut a_lib, Duration::from_millis(1500));
        assert!(a.encryption_hold(&a_lib));
        assert!(service.rows().is_empty(), "the library uploaded unencrypted");
        // Turning encryption on again makes a new key, and the library uploads sealed.
        poll_until(&mut a, &mut a_lib, |g, _| g.encryption_ready());
        a.begin_encryption(&a_lib).unwrap();
        let new_code = Hidden::from(&a.recovery_code().unwrap());
        assert_ne!(new_code, old_code, "turning encryption on again makes a new code");
        retry_until(&mut a, &mut a_lib, Duration::from_secs(10), |g, l| g.confirm_encryption(l));
        poll_for(&mut a, &mut a_lib, Duration::from_secs(30), |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);
        assert!(!a.encryption_hold(&a_lib));
        assert!(service.rows().iter().any(|r| r["kind"] == "e2ee"));
        assert!(service.rows().iter().all(|r| r["deleted"] == true || r["enc"] == "xchacha20_v1"));

        // B turns sync on: the old code no longer opens the account.
        b.turn_sync_on(&mut b_lib).unwrap();
        poll_until(&mut b, &mut b_lib, |g, _| locked(g));
        poll_until(&mut b, &mut b_lib, |g, _| g.encryption_scanned());
        b.unlock_encryption(&b_lib, old_code.reveal()).unwrap();
        poll_until(&mut b, &mut b_lib, |g, _| !g.checking_encryption());
        assert!(locked(&b), "an old recovery code opened the new key");
        assert!(service.rows().iter().all(|r| r["deleted"] == true || r["enc"] == "xchacha20_v1"));
    }

    #[test]
    fn a_fresh_start_applies_nothing_until_the_epoch_its_first_pull_named_is_saved() {
        let service = Arc::new(SyncService::default());
        let (_a_dir, _a_lib, _a, _a_server, _) = synced_device(&service, "fixture-account", "device-a");
        let (server, _) = account_server(&service, "fixture-account", "device-b");
        let dir = FixtureDir::new();
        let mut lib = library(&dir);
        let connection = rusqlite::Connection::open(dir.0.join("library.sqlite")).unwrap();
        connection.execute_batch("CREATE TRIGGER refuse_epoch BEFORE INSERT ON kv WHEN NEW.k = 'sync_epoch' BEGIN SELECT RAISE(FAIL, 'fixture write failure'); END;").unwrap();
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        sign_in(&mut glue, &mut lib, &server.url);
        poll_until(&mut glue, &mut lib, |g, _| g.pull_error.is_some());
        idle(&mut glue, &mut lib, Duration::from_millis(500));
        // A cursor saved without its epoch would read as epoch 0 next time.
        assert!(!lib.items.values().any(|item| item.url.contains("device-a")), "a page was applied without its epoch saved");
        assert_eq!(lib.store.kv_get(KV_CURSOR).unwrap().as_deref(), Some("0"));

        connection.execute_batch("DROP TRIGGER refuse_epoch").unwrap();
        glue.sync_now();
        poll_until(&mut glue, &mut lib, |_, l| l.items.values().any(|item| item.url.contains("device-a")));
        assert_eq!(deleted::stored_epoch(&lib), Some(0));
        assert!(glue.pull_error.is_none());
    }

    /// Residual 32: signing in again after a delete is the existing fresh
    /// start, so the library uploads without Turn Sync On. An encrypted
    /// library still waits for encryption: the hold survives sign-out.
    #[test]
    fn signing_in_again_after_a_delete_still_waits_for_encryption() {
        let service = Arc::new(SyncService::default());
        let (_dir, mut lib, mut glue, server, _) = synced_device(&service, "fixture-account", "device-a");
        turn_on(&mut glue, &mut lib);
        glue.delete_sync_data(&lib);
        poll_until(&mut glue, &mut lib, |g, _| g.sync_off.is_some());
        glue.sign_out(&lib);
        sign_in(&mut glue, &mut lib, &server.url);
        assert!(glue.signed_in() && glue.sync_off.is_none(), "signing in again turns sync on");
        idle(&mut glue, &mut lib, Duration::from_millis(1500));
        assert!(glue.encryption_hold(&lib));
        assert!(lib.store.dirty_count().unwrap() > 0, "the library no longer waits to upload");
        assert!(service.rows().is_empty(), "the library uploaded unencrypted after signing in again");

        // Turning encryption on again uploads it sealed, under a new key.
        poll_until(&mut glue, &mut lib, |g, _| g.encryption_ready());
        glue.begin_encryption(&lib).unwrap();
        retry_until(&mut glue, &mut lib, Duration::from_secs(10), |g, l| g.confirm_encryption(l));
        poll_for(&mut glue, &mut lib, Duration::from_secs(30), |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);
        assert!(!glue.encryption_hold(&lib));
        assert!(!service.rows().is_empty());
        assert!(service.rows().iter().all(|r| r["deleted"] == true || r["enc"] == "xchacha20_v1"));
    }

    // -- after a delete: what this library and this device remember (recheck 9) --

    /// Live rows on the server that anyone can read.
    fn plaintext_rows(service: &SyncService) -> usize {
        service.rows().iter().filter(|r| r["deleted"] == false && r["enc"] != "xchacha20_v1").count()
    }

    /// An encrypted library, its account's sync data deleted from this Mac.
    fn deleted_encrypted_library() -> (FixtureDir, Library, SyncGlue, Arc<SyncService>, Server, Arc<Network>) {
        let service = Arc::new(SyncService::default());
        let (dir, mut lib, mut glue, server, network) = synced_device(&service, "fixture-account-a", "device-a");
        turn_on(&mut glue, &mut lib);
        let space = lib.spaces_ordered()[0].0.clone();
        lib.update_space(&space, |s| s.name = "Fixture Encrypted Secret".into()).unwrap();
        poll_until(&mut glue, &mut lib, |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);
        glue.delete_sync_data(&lib);
        poll_until(&mut glue, &mut lib, |g, _| g.sync_off.is_some());
        assert!(glue.sync_off.as_ref().unwrap().encrypted);
        (dir, lib, glue, service, server, network)
    }

    /// Recheck 9, AB1: the hold belongs to this library, not to the account
    /// whose data was deleted. However this Mac comes to sync another
    /// account, with or without a restart, the library uploads no plaintext.
    #[test]
    fn a_library_that_was_encrypted_uploads_no_plaintext_to_another_account() {
        for (way, restart) in [("sign out", false), ("sign out", true), ("server ends the session", false), ("sign-in gone at the next launch", true)] {
            let (dir, mut lib, mut glue, a_service, _a_server, a_network) = deleted_encrypted_library();
            match way {
                "sign out" => glue.sign_out(&lib),
                "server ends the session" => {
                    // No Sign Out: sync is turned on again, then the server ends the session.
                    glue.turn_sync_on(&mut lib).unwrap();
                    Network::set(&a_network.reject_session, true);
                    glue.sync_now();
                    poll_until(&mut glue, &mut lib, |g, _| !g.signed_in());
                }
                _ => memory_token_store::clear(&dir.0, "fixture-account-a").unwrap(),
            }
            if restart {
                drop(glue);
                drop(lib);
                lib = library(&dir);
                glue = SyncGlue::new(&dir.0, &lib, || {});
                assert!(!glue.signed_in(), "{way}");
            }
            let b_service = Arc::new(SyncService::default());
            let (b_server, _) = account_server(&b_service, "fixture-account-b", "device-a");
            sign_in(&mut glue, &mut lib, &b_server.url);
            assert!(glue.signed_in(), "{way}: signing in to another account was refused");
            poll_until(&mut glue, &mut lib, |g, _| g.encryption_ready());
            idle(&mut glue, &mut lib, Duration::from_millis(1200));
            assert_eq!(plaintext_rows(&b_service), 0, "{way}, restart {restart}: the library reached another account unencrypted");
            assert_eq!(plaintext_rows(&a_service), 0, "{way}: the library reached its own account unencrypted");
            assert!(glue.encryption_hold(&lib), "{way}: no hold for the other account");
            assert!(lib.store.dirty_count().unwrap() > 0, "{way}: the library no longer waits to upload");
            // Reopened while signed in to B, it still waits.
            drop(glue);
            drop(lib);
            let mut lib = library(&dir);
            let mut glue = SyncGlue::new(&dir.0, &lib, || {});
            poll_until(&mut glue, &mut lib, |g, _| g.encryption_ready());
            idle(&mut glue, &mut lib, Duration::from_millis(600));
            assert_eq!(plaintext_rows(&b_service), 0, "{way}: the library reached another account unencrypted after a reopen");
            assert!(glue.encryption_hold(&lib));
        }
    }

    /// Recheck 9, AB1: signing in reads the hold, and a hold that can't be
    /// read stops the sign-in, as an unreadable encryption record does (F2).
    #[test]
    fn a_sign_in_stops_when_the_hold_cant_be_read() {
        let (dir, mut lib, mut glue, _a_service, _a_server, _) = deleted_encrypted_library();
        glue.sign_out(&lib);
        let other = rusqlite::Connection::open(dir.0.join("library.sqlite")).unwrap();
        other.execute("UPDATE kv SET v = X'FF' WHERE k = 'sync_encrypt_first'", []).unwrap();
        assert!(lib.store.kv_get("sync_encrypt_first").is_err());
        let b_service = Arc::new(SyncService::default());
        let (b_server, _) = account_server(&b_service, "fixture-account-b", "device-a");
        sign_in(&mut glue, &mut lib, &b_server.url);
        assert!(!glue.signed_in(), "a sign-in went ahead without knowing whether the library must wait for encryption");
        assert_eq!(glue.error.as_deref(), Some(SIGN_IN_STOPPED));
        idle(&mut glue, &mut lib, Duration::from_millis(600));
        assert_eq!(plaintext_rows(&b_service), 0);
    }

    /// Upload Without Encryption lets the library go unencrypted to the
    /// account it was chosen for, and to no other; turning encryption on for
    /// an account ends the hold.
    #[test]
    fn upload_without_encryption_is_for_that_account_only() {
        let (dir, mut lib, mut glue, _a_service, _a_server, _) = deleted_encrypted_library();
        glue.sign_out(&lib);
        let b_service = Arc::new(SyncService::default());
        let (b_server, _) = account_server(&b_service, "fixture-account-b", "device-a");
        sign_in(&mut glue, &mut lib, &b_server.url);
        poll_until(&mut glue, &mut lib, |g, _| g.encryption_ready());
        glue.upload_without_encryption(&lib).unwrap();
        poll_until(&mut glue, &mut lib, |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);
        assert!(plaintext_rows(&b_service) > 0, "the user's choice for this account didn't upload");
        assert!(!glue.encryption_hold(&lib));

        glue.sign_out(&lib);
        drop(glue);
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        let c_service = Arc::new(SyncService::default());
        let (c_server, _) = account_server(&c_service, "fixture-account-c", "device-a");
        sign_in(&mut glue, &mut lib, &c_server.url);
        poll_until(&mut glue, &mut lib, |g, _| g.encryption_ready());
        idle(&mut glue, &mut lib, Duration::from_millis(1200));
        assert_eq!(plaintext_rows(&c_service), 0, "a choice made for one account let the library reach another unencrypted");
        assert!(glue.encryption_hold(&lib));

        // Turning encryption on for this account uploads it sealed and ends the hold.
        poll_until(&mut glue, &mut lib, |g, _| g.encryption_ready());
        glue.begin_encryption(&lib).unwrap();
        retry_until(&mut glue, &mut lib, Duration::from_secs(10), |g, l| g.confirm_encryption(l));
        poll_for(&mut glue, &mut lib, Duration::from_secs(30), |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);
        assert!(!glue.encryption_hold(&lib));
        assert!(!c_service.rows().is_empty());
        assert_eq!(plaintext_rows(&c_service), 0);
    }

    /// Recheck 9, AB2: a device signed out, or whose session the server
    /// ended, before another device deleted the account's sync data retires
    /// its old key when it signs in again, as a signed-in device does when it
    /// meets the new epoch: nothing sealed under that key, and not its
    /// marker, reaches the new epoch; uploads wait; and the old recovery
    /// code unlocks nothing, before or after encryption is on again.
    #[test]
    fn a_device_away_during_a_delete_retires_the_old_key_when_it_signs_in_again() {
        for user_sign_out in [true, false] {
            let service = Arc::new(SyncService::default());
            let (_a_dir, mut a_lib, mut a, _a_server, _) = synced_device(&service, "fixture-account", "device-a");
            let old_code = turn_on(&mut a, &mut a_lib);
            let (c_server, c_network) = account_server(&service, "fixture-account", "device-c");
            let c_dir = FixtureDir::new();
            let mut c_lib = library(&c_dir);
            let mut c = SyncGlue::new(&c_dir.0, &c_lib, || {});
            sign_in(&mut c, &mut c_lib, &c_server.url);
            poll_until(&mut c, &mut c_lib, |g, _| locked(g));
            retry_until(&mut c, &mut c_lib, Duration::from_secs(10), |g, l| g.unlock_encryption(l, old_code.reveal()));
            poll_for(&mut c, &mut c_lib, Duration::from_secs(30), |g, l| g.e2ee.as_ref().is_some_and(|e| e.status() == E2eeStatus::On) && l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);
            if user_sign_out {
                c.sign_out(&c_lib);
            } else {
                Network::set(&c_network.reject_session, true);
                c.sync_now();
                poll_until(&mut c, &mut c_lib, |g, _| !g.signed_in());
                Network::set(&c_network.reject_session, false);
                assert!(c.has_library_key("fixture-account"), "a server-ended session keeps the key");
            }
            a.delete_sync_data(&a_lib);
            poll_until(&mut a, &mut a_lib, |g, _| g.sync_off.is_some());

            let mark = c_server.requests.lock().unwrap().len();
            sign_in(&mut c, &mut c_lib, &c_server.url);
            assert!(c.signed_in());
            // Its first pull has run, whatever it found.
            poll_for(&mut c, &mut c_lib, Duration::from_secs(10), |g, _| g.encryption_ready() || locked(g) && g.encryption_scanned());
            idle(&mut c, &mut c_lib, Duration::from_millis(1200));
            let what = if user_sign_out { "after Sign Out" } else { "after the server ended the session" };
            assert_eq!(pushed_since(&c_server, mark).len(), 0, "{what}: the device uploaded into the new epoch");
            assert!(service.rows().is_empty(), "{what}: old ciphertext or its marker reached the new epoch");
            assert_eq!(c.encryption_status(&c_lib), Some(E2eeStatus::Off), "{what}: the retired key's record stayed");
            assert!(!c.has_library_key("fixture-account"), "{what}: the retired key stayed");
            assert!(c.encryption_hold(&c_lib), "{what}: uploads don't wait for encryption");
            assert!(c.unlock_encryption(&c_lib, old_code.reveal()).is_err(), "{what}: the retired code was taken");

            // A turns sync on: nothing locks it; encryption on again makes a new key.
            a.turn_sync_on(&mut a_lib).unwrap();
            poll_until(&mut a, &mut a_lib, |g, _| g.encryption_ready());
            assert_eq!(a.encryption_status(&a_lib), Some(E2eeStatus::Off), "{what}");
            a.begin_encryption(&a_lib).unwrap();
            assert_ne!(Hidden::from(&a.recovery_code().unwrap()), old_code, "{what}: turning encryption on again makes a new code");
            retry_until(&mut a, &mut a_lib, Duration::from_secs(10), |g, l| g.confirm_encryption(l));
            poll_for(&mut a, &mut a_lib, Duration::from_secs(30), |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);

            // C meets the new key: Locked, and the old code still opens nothing.
            c.sync_now();
            poll_for(&mut c, &mut c_lib, Duration::from_secs(10), |g, _| locked(g) && g.encryption_scanned());
            c.unlock_encryption(&c_lib, old_code.reveal()).unwrap();
            poll_until(&mut c, &mut c_lib, |g, _| !g.checking_encryption());
            assert!(locked(&c), "{what}: the retired code opened the new key");
            assert_eq!(plaintext_rows(&service), 0);
        }
    }

    /// Recheck 9, AB3: a sync-off record that can't be read keeps sync off
    /// and says so, and Turn Sync On still uploads the library.
    #[test]
    fn an_unreadable_sync_off_record_keeps_sync_off_and_says_so() {
        let service = Arc::new(SyncService::default());
        let (dir, mut lib, mut glue, server, _) = synced_device(&service, "fixture-account", "device-a");
        glue.delete_sync_data(&lib);
        poll_until(&mut glue, &mut lib, |g, _| g.sync_off.is_some());
        let (_b_dir, _b_lib, _b, _b_server, _) = synced_device(&service, "fixture-account", "device-b");
        let rows = service.rows();
        let space = lib.spaces_ordered()[0].0.clone();
        lib.update_space(&space, |s| s.name = "Edited While Off".into()).unwrap();
        drop(glue);
        lib.store.kv_set("sync_off", "{not json").unwrap();
        drop(lib);

        let mut lib = library(&dir);
        let mark = server.requests.lock().unwrap().len();
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        idle(&mut glue, &mut lib, Duration::from_millis(1500));
        assert!(glue.sync_off.is_some(), "sync came back on");
        assert_eq!(sync_requests_since(&server, mark), Vec::<String>::new(), "it synced");
        assert_eq!(service.rows(), rows);
        assert!(glue.sync_off_text().is_some_and(|text| text.starts_with("Couldn't read")), "{:?}", glue.sync_off_text());

        glue.turn_sync_on(&mut lib).unwrap();
        poll_until(&mut glue, &mut lib, |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight && service.rows().iter().any(|r| r["payload"].as_str().unwrap().contains("Edited While Off")));
        assert!(lib.items.values().any(|item| item.url.contains("device-b")), "the other device's rows weren't merged");
    }

    /// Recheck 9, AB4: the server deletes, then Crossbow quits before it
    /// records that. The next launch stops, keeps nothing to upload and says
    /// this Mac deleted the data.
    #[test]
    fn after_a_crash_the_device_that_deleted_says_so() {
        let service = Arc::new(SyncService::default());
        let (dir, mut lib, mut glue, server, _) = synced_device(&service, "fixture-account", "device-a");
        turn_on(&mut glue, &mut lib);
        let space = lib.spaces_ordered()[0].0.clone();
        lib.update_space(&space, |s| s.name = "Pending Edit".into()).unwrap();
        glue.delete_sync_data(&lib);
        let start = Instant::now();
        while service.sync_epoch() == 0 {
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::sleep(Duration::from_millis(200));
        // No poll: the glue never sees the worker's answer.
        drop(glue);
        drop(lib);
        let mut lib = library(&dir);
        let mark = server.requests.lock().unwrap().len();
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        poll_for(&mut glue, &mut lib, Duration::from_secs(10), |g, _| g.sync_off.is_some());
        idle(&mut glue, &mut lib, Duration::from_millis(1000));
        assert_eq!(glue.sync_off_text(), Some(SYNC_OFF_HERE));
        assert!(sync_requests_since(&server, mark).iter().all(|r| !r.contains("push")), "it uploaded");
        assert!(service.rows().is_empty());
        assert!(lib.store.kv_get("e2ee_library").unwrap().is_none(), "the retired key's record stayed");
        assert!(glue.encryption_hold(&lib));
    }

    /// Recheck 9, AB4: the delete lands but its readback fails; the next
    /// upload meets the new epoch. The device knows it deleted the data.
    #[test]
    fn after_an_unconfirmed_readback_the_device_that_deleted_says_so() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        let service = Arc::new(SyncService::default());
        let fail_readback = Arc::new(AtomicBool::new(false));
        let (hold_pull, released) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let (shared, flag, hold, gate) = (service.clone(), fail_readback.clone(), hold_pull.clone(), released.clone());
        let server = Server::start(move |request| {
            let me = MeResponse {
                account: Account { id: "fixture-account".into(), email: "fixture@example.com".into(), created_at: 1 },
                device: Device { id: "device-a".into(), name: "device-a".into(), platform: "macos".into(), created_at: 1, last_seen_at: 1 },
                devices: vec![],
                browser_sign_in: false,
            };
            match request.path.as_str() {
                "/v1/auth/start" => Reply::json(200, r#"{"challenge_id":"fixture-challenge","expires_in":600,"dev_code":"123456"}"#),
                "/v1/auth/verify" => Reply::json(200, serde_json::json!({ "session_token": "fixture-token", "account": me.account, "device": me.device }).to_string()),
                "/v1/auth/logout" => Reply::json(200, "{}"),
                "/v1/me" => Reply::json(200, serde_json::to_string(&me).unwrap()),
                "/v1/sync/ws" => Reply::socket(),
                path if path.starts_with("/v1/sync/pull?") && flag.load(SeqCst) => Reply::json(503, r#"{"error":{"code":"unavailable","message":"fixture readback lost"}}"#),
                path => {
                    let mut reply = shared.reply(request, "device-a").unwrap();
                    if path.starts_with("/v1/sync/pull?") && hold.swap(false, SeqCst) { reply.gate = Some(gate.clone()); }
                    reply
                }
            }
        });
        let dir = FixtureDir::new();
        let mut lib = library(&dir);
        let mut glue = SyncGlue::new(&dir.0, &lib, || {});
        sign_in(&mut glue, &mut lib, &server.url);
        poll_until(&mut glue, &mut lib, |g, l| l.store.dirty_count().unwrap() == 0 && g.upload_hold().is_none() && !g.push_in_flight);
        // The worker waits in a pull of its own while the delete and the
        // upload are queued behind it. The fixture socket stays open, so
        // nothing the worker reports leaves the device to catch up first,
        // which no pull could do once the readback fails.
        let pulls = server.requests.lock().unwrap().iter().filter(|r| r.path.starts_with("/v1/sync/pull?")).count();
        released.store(false, SeqCst);
        hold_pull.store(true, SeqCst);
        glue.pull_now();
        server.wait_for("/v1/sync/pull?", pulls + 1);
        // Everything the worker sent before it began that pull.
        poll_until(&mut glue, &mut lib, |_, _| true);
        assert!(glue.upload_hold().is_none(), "the device had to catch up before the delete: status={:?}", glue.status);
        fail_readback.store(true, SeqCst);
        let mark = server.requests.lock().unwrap().len();
        glue.delete_sync_data(&lib);
        // An edit's upload goes right behind the delete, before any pull.
        let space = lib.spaces_ordered()[0].0.clone();
        lib.update_space(&space, |s| s.name = "After The Unconfirmed Delete".into()).unwrap();
        glue.last_push_check = f64::NEG_INFINITY;
        poll_until(&mut glue, &mut lib, |_, _| true);
        assert!(glue.push_in_flight, "the edit's upload didn't go");
        released.store(true, SeqCst);
        poll_for(&mut glue, &mut lib, Duration::from_secs(10), |g, _| g.sync_off.is_some());
        let paths: Vec<String> = server.requests.lock().unwrap()[mark..].iter().map(|r| r.path.clone()).collect();
        let readback = paths.iter().position(|p| p.starts_with("/v1/sync/pull?since=0")).expect("no readback");
        assert!(paths[readback..].iter().any(|p| p == "/v1/sync/push"), "the upload didn't follow the failed readback: {paths:?}");
        assert_eq!(glue.sync_off_text(), Some(SYNC_OFF_HERE));
        assert!(service.rows().is_empty());
    }

    /// A delete that never reached the server is no reason to call a later
    /// delete from another device this Mac's own.
    #[test]
    fn a_delete_that_never_landed_doesnt_claim_a_later_one_from_another_device() {
        let service = Arc::new(SyncService::default());
        let (_a_dir, mut a_lib, mut a, _a_server, a_network) = synced_device(&service, "fixture-account", "device-a");
        let (_b_dir, mut b_lib, mut b, _b_server, _) = synced_device(&service, "fixture-account", "device-b");
        Network::set(&a_network.offline, true);
        a.delete_sync_data(&a_lib);
        poll_until(&mut a, &mut a_lib, |g, _| !g.deleting);
        assert!(a.delete_error.is_some());
        Network::set(&a_network.offline, false);
        // A syncs again in the same epoch, so its delete didn't happen.
        let space = a_lib.spaces_ordered()[0].0.clone();
        a_lib.update_space(&space, |s| s.name = "Synced After".into()).unwrap();
        a.sync_now();
        poll_for(&mut a, &mut a_lib, Duration::from_secs(10), |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight && g.pull_error.is_none());
        poll_for(&mut a, &mut a_lib, Duration::from_secs(10), |_, l| l.items.values().any(|item| item.url.contains("device-b")));
        b.delete_sync_data(&b_lib);
        poll_until(&mut b, &mut b_lib, |g, _| g.sync_off.is_some());
        a.sync_now();
        poll_for(&mut a, &mut a_lib, Duration::from_secs(10), |g, _| g.sync_off.is_some());
        assert_eq!(a.sync_off_text(), Some(SYNC_OFF_ELSEWHERE));
    }

    #[test]
    fn development_code_never_claims_an_email_was_sent() {
        let step = AuthStep::Code { challenge_id: "fixture".into(), email: "fixture@example.com".into(), dev_code: Some("123456".into()) };
        assert!(step.instruction(false).contains("no email was sent"));
        assert!(!step.instruction(false).contains("fixture@example.com"));
    }

    #[test]
    fn invalid_code_does_not_assume_email_delivery() {
        let message = friendly_error(crossbow_core::proto::error_codes::INVALID_CODE);
        assert_eq!(message, "That code isn't right. Check the code and try again.");
        assert!(!message.contains("email"));
    }

    #[test]
    fn failed_start_is_not_an_infinite_sending_state() {
        let step = AuthStep::Code { challenge_id: String::new(), email: "fixture@example.com".into(), dev_code: None };
        assert!(step.instruction(true).contains("No code was sent"));
        assert!(step.instruction(false).starts_with("Requesting"));
    }

    #[test]
    fn delivered_code_and_validation_are_explicit() {
        let step = AuthStep::Code { challenge_id: "fixture".into(), email: "fixture@example.com".into(), dev_code: None };
        assert!(step.instruction(false).contains("sent to fixture@example.com"));
        assert!(valid_code("123456"));
        for invalid in ["12345", "1234567", "abcdef", "１２３４５６", "123 56"] { assert!(!valid_code(invalid)); }
        for invalid in ["a@@b.com", "a@.com", "a b@example.com", "@example.com"] { assert!(!valid_email(invalid)); }
    }

    // -- Backups and Restore Data ------------------------------------------

    use crossbow_core::sync::key_store::MemoryKeyStore;

    fn backup_reasons(dir: &FixtureDir) -> Vec<backup::Reason> {
        let mut backups = backup::list(&dir.0.join("library.sqlite"));
        backups.reverse();
        backups.into_iter().map(|backup| backup.reason).collect()
    }

    /// A signed-in device whose key store the test keeps, synced.
    fn keyed_device(service: &Arc<SyncService>, keys: &MemoryKeyStore) -> (FixtureDir, Library, SyncGlue, Server) {
        let (server, _) = account_server(service, "fixture-account", "device-a");
        let dir = FixtureDir::new();
        let mut lib = library(&dir);
        let space = lib.spaces_ordered()[0].0.clone();
        lib.add_pinned(&space, None, "https://example.test/kept", "Pinned before the backup").unwrap();
        lib.add_pinned(&space, None, "https://example.test/later", "Edited later elsewhere").unwrap();
        let mut glue = SyncGlue::with_key_store(&dir.0, &lib, || {}, Box::new(keys.clone()));
        sign_in(&mut glue, &mut lib, &server.url);
        poll_until(&mut glue, &mut lib, |g, l| l.store.dirty_count().unwrap() == 0 && g.upload_hold().is_none() && !g.push_in_flight);
        (dir, lib, glue, server)
    }

    /// Quit with a restore waiting and open again: it finishes before the
    /// library opens, and a new glue starts on the same key store.
    fn relaunch_restored(dir: &FixtureDir, glue: SyncGlue, lib: Library, keys: &MemoryKeyStore) -> (Library, SyncGlue) {
        drop(glue);
        drop(lib);
        assert_eq!(backup::finish_restore(&dir.0.join("library.sqlite")), Ok(true));
        let lib = library(dir);
        let glue = SyncGlue::with_key_store(&dir.0, &lib, || {}, Box::new(keys.clone()));
        (lib, glue)
    }

    fn item_titled(lib: &Library, title: &str) -> String {
        lib.items.iter().find(|(_, item)| item.title == title).map(|(id, _)| id.clone()).unwrap_or_else(|| panic!("no item titled {title}"))
    }

    #[test]
    fn risky_sync_operations_back_up_the_library_first_and_a_failed_backup_never_stops_them() {
        let service = Arc::new(SyncService::default());
        let (dir, mut lib, mut glue, _server, _) = synced_device(&service, "fixture-account", "device-a");
        assert!(backup_reasons(&dir).is_empty());
        turn_on(&mut glue, &mut lib);
        assert_eq!(backup_reasons(&dir), [backup::Reason::TurnOnEncryption]);
        poll_until(&mut glue, &mut lib, |g, _| g.encryption_scanned());
        glue.begin_reset_encryption(&lib).unwrap();
        poll_until(&mut glue, &mut lib, |g, _| g.reset_ready());
        glue.confirm_reset_encryption(&mut lib).unwrap();
        assert_eq!(backup_reasons(&dir), [backup::Reason::TurnOnEncryption, backup::Reason::ResetEncryption]);
        poll_until(&mut glue, &mut lib, |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);
        glue.delete_sync_data(&lib);
        poll_until(&mut glue, &mut lib, |g, _| g.sync_off.is_some());
        assert_eq!(backup_reasons(&dir).last(), Some(&backup::Reason::DeleteSyncData));
        // The backup before Turn Sync On fails: sync still turns on, and the failure is noted.
        let folder = backup::folder(&dir.0.join("library.sqlite"));
        std::fs::remove_dir_all(&folder).unwrap();
        std::fs::write(&folder, b"not a folder").unwrap();
        glue.turn_sync_on(&mut lib).unwrap();
        assert!(glue.sync_off.is_none(), "a failed backup stopped Turn Sync On");
        let failure = &backup::failures(&lib.store)[0];
        assert_eq!(failure.reason(), Some(backup::Reason::TurnSyncOn));
        std::fs::remove_file(&folder).unwrap();
    }

    #[test]
    fn a_restored_library_stays_off_until_turned_on_then_its_items_win_and_later_edits_win_too() {
        let service = Arc::new(SyncService::default());
        let keys = MemoryKeyStore::default();
        let (dir, mut lib, mut glue, server) = keyed_device(&service, &keys);
        let (kept, later) = (item_titled(&lib, "Pinned before the backup"), item_titled(&lib, "Edited later elsewhere"));
        let chosen = backup::create(&lib.store, backup::Reason::Daily).unwrap().unwrap();

        // After the backup, another device renames a pin and adds one.
        let (_b_dir, mut b_lib, mut b, _b_server, _) = synced_device(&service, "fixture-account", "device-b");
        poll_until(&mut b, &mut b_lib, |_, l| l.items.contains_key(&kept));
        b_lib.update_item(&kept, |item| item.title = "Renamed on B before the restore".into()).unwrap();
        poll_until(&mut b, &mut b_lib, |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);
        glue.sync_now();
        poll_until(&mut glue, &mut lib, |_, l| l.items[&kept].title == "Renamed on B before the restore" && l.items.values().any(|i| i.title == "Pinned on device-b"));

        backup::restore(&lib.store, &chosen).unwrap();
        let (mut lib, mut glue) = relaunch_restored(&dir, glue, lib, &keys);
        assert_eq!(glue.sync_off_text(), Some(SYNC_OFF_RESTORED));
        assert!(glue.signed_in());
        assert_eq!(lib.items[&kept].title, "Pinned before the backup");
        assert!(!lib.items.values().any(|i| i.title == "Pinned on device-b"), "the pin added after the backup is gone here");
        // Nothing uploads by itself.
        let (mark, rows) = (server.requests.lock().unwrap().len(), service.rows());
        glue.sync_now();
        idle(&mut glue, &mut lib, Duration::from_millis(1500));
        assert_eq!(sync_requests_since(&server, mark), Vec::<String>::new());
        assert_eq!(service.rows(), rows);

        // An edit on the other device after the restore is newer than it.
        std::thread::sleep(Duration::from_millis(5));
        b_lib.update_item(&later, |item| item.title = "Renamed on B after the restore".into()).unwrap();
        poll_until(&mut b, &mut b_lib, |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);

        glue.turn_sync_on(&mut lib).unwrap();
        poll_for(&mut glue, &mut lib, Duration::from_secs(30), |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight
            && l.items[&later].title == "Renamed on B after the restore" && l.items.values().any(|i| i.title == "Pinned on device-b"));
        assert_eq!(lib.items[&kept].title, "Pinned before the backup", "the restored pin replaced the synced copy");
        b.sync_now();
        poll_until(&mut b, &mut b_lib, |_, l| l.items[&kept].title == "Pinned before the backup");
        assert_eq!(b_lib.items[&later].title, "Renamed on B after the restore");
        assert!(b_lib.items.values().any(|i| i.title == "Pinned on device-b"), "items added since the backup are kept");
    }

    #[test]
    fn a_restored_encrypted_library_keeps_this_macs_key_and_uploads_only_ciphertext() {
        let service = Arc::new(SyncService::default());
        let keys = MemoryKeyStore::default();
        let (dir, mut lib, mut glue, server) = keyed_device(&service, &keys);
        turn_on(&mut glue, &mut lib);
        let chosen = backup::create(&lib.store, backup::Reason::Daily).unwrap().unwrap();
        let space = lib.spaces_ordered()[0].0.clone();
        lib.add_pinned(&space, None, "https://example.test/after", "Added after the backup").unwrap();
        poll_until(&mut glue, &mut lib, |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);

        backup::restore(&lib.store, &chosen).unwrap();
        let (mut lib, mut glue) = relaunch_restored(&dir, glue, lib, &keys);
        assert_eq!(glue.encryption_status(&lib), Some(E2eeStatus::On), "this Mac's key still opens the library");
        assert!(!glue.encryption_hold(&lib));
        let mark = server.requests.lock().unwrap().len();
        glue.turn_sync_on(&mut lib).unwrap();
        poll_for(&mut glue, &mut lib, Duration::from_secs(30), |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight && g.e2ee.as_ref().is_some_and(|e| e.caught_up()));
        let pushed = pushed_since(&server, mark);
        assert!(!pushed.is_empty());
        assert!(pushed.iter().all(|c| c.deleted || c.enc == Encoding::Xchacha20V1), "a restored row went up in plaintext");
        assert!(service.rows().iter().all(|r| r["deleted"] == true || r["enc"] == "xchacha20_v1"));
    }

    #[test]
    fn a_backup_from_before_a_delete_waits_for_a_new_key_and_never_brings_back_the_old_one() {
        let service = Arc::new(SyncService::default());
        let keys = MemoryKeyStore::default();
        let (dir, mut lib, mut glue, server) = keyed_device(&service, &keys);
        let old_code = turn_on(&mut glue, &mut lib);
        let chosen = backup::create(&lib.store, backup::Reason::Daily).unwrap().unwrap();
        glue.delete_sync_data(&lib);
        poll_until(&mut glue, &mut lib, |g, _| g.sync_off.is_some());
        assert!(!glue.has_library_key("fixture-account"));

        backup::restore(&lib.store, &chosen).unwrap();
        let (mut lib, mut glue) = relaunch_restored(&dir, glue, lib, &keys);
        assert_eq!(glue.sync_off_text(), Some(SYNC_OFF_RESTORED));
        // The backup's encryption record isn't back, so its old code can't unlock anything.
        assert_eq!(glue.encryption_status(&lib), Some(E2eeStatus::Off));
        assert_eq!(glue.unlock_encryption(&lib, old_code.reveal()), Err("This library isn't locked.".into()));
        assert!(glue.encrypted_before_delete(&lib));

        let mark = server.requests.lock().unwrap().len();
        glue.turn_sync_on(&mut lib).unwrap();
        idle(&mut glue, &mut lib, Duration::from_millis(1500));
        assert!(glue.encryption_hold(&lib));
        assert!(service.rows().is_empty(), "the restored library uploaded unencrypted");
        assert!(pushed_since(&server, mark).is_empty());

        poll_until(&mut glue, &mut lib, |g, _| g.encryption_ready());
        glue.begin_encryption(&lib).unwrap();
        let new_code = Hidden::from(&glue.recovery_code().unwrap());
        assert_ne!(new_code, old_code, "turning encryption on again makes a new code");
        retry_until(&mut glue, &mut lib, Duration::from_secs(10), |g, l| g.confirm_encryption(l));
        poll_for(&mut glue, &mut lib, Duration::from_secs(30), |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);
        assert!(!glue.encryption_hold(&lib));
        assert!(!service.rows().is_empty());
        assert!(service.rows().iter().all(|r| r["deleted"] == true || r["enc"] == "xchacha20_v1"));
        assert!(pushed_since(&server, mark).iter().all(|c| c.deleted || c.enc == Encoding::Xchacha20V1));
    }

    /// An encrypted backup restored while signed out brings back the
    /// "this library was encrypted" hold for the library, not for the
    /// backup's account: signing in to another account then uploads no
    /// plaintext, across a restart, and neither does the backup's own
    /// account, though the user had let the library go unencrypted to it
    /// before the restore.
    #[test]
    fn a_signed_out_restore_of_an_encrypted_backup_uploads_nothing_unencrypted_to_another_account() {
        let service = Arc::new(SyncService::default());
        let keys = MemoryKeyStore::default();
        let (dir, mut lib, mut glue, a_server) = keyed_device(&service, &keys);
        turn_on(&mut glue, &mut lib);
        let chosen = backup::create(&lib.store, backup::Reason::Daily).unwrap().unwrap();
        glue.delete_sync_data(&lib);
        poll_until(&mut glue, &mut lib, |g, _| g.sync_off.is_some());
        // The user lets this library go to its account unencrypted, then signs out.
        glue.turn_sync_on(&mut lib).unwrap();
        glue.upload_without_encryption(&lib).unwrap();
        poll_until(&mut glue, &mut lib, |g, l| l.store.dirty_count().unwrap() == 0 && !g.push_in_flight);
        glue.sign_out(&lib);

        backup::restore(&lib.store, &chosen).unwrap();
        let (mut lib, mut glue) = relaunch_restored(&dir, glue, lib, &keys);
        assert!(!glue.signed_in());
        let unencrypted = |service: &SyncService| service.rows().iter().filter(|r| r["deleted"] == false && r["enc"] != "xchacha20_v1").count();
        let b_service = Arc::new(SyncService::default());
        let (b_server, _) = account_server(&b_service, "fixture-account-b", "device-a");
        sign_in(&mut glue, &mut lib, &b_server.url);
        assert!(glue.signed_in(), "signing in to another account was refused");
        poll_until(&mut glue, &mut lib, |g, _| g.encryption_ready());
        idle(&mut glue, &mut lib, Duration::from_millis(1500));
        assert_eq!(unencrypted(&b_service), 0, "the restored encrypted library reached another account unencrypted");
        assert!(pushed_since(&b_server, 0).iter().all(|c| c.deleted || c.enc == Encoding::Xchacha20V1));
        assert!(glue.encryption_hold(&lib), "no hold for the other account");
        assert!(lib.store.dirty_count().unwrap() > 0, "the library no longer waits to upload");
        drop(glue);
        drop(lib);
        let mut lib = library(&dir);
        let mut glue = SyncGlue::with_key_store(&dir.0, &lib, || {}, Box::new(keys.clone()));
        poll_until(&mut glue, &mut lib, |g, _| g.encryption_ready());
        idle(&mut glue, &mut lib, Duration::from_millis(600));
        assert_eq!(unencrypted(&b_service), 0, "after a restart");

        // The backup's own account waits again too: the restore brought the
        // encrypted library back, and the earlier choice was for the library
        // as it was then.
        glue.sign_out(&lib);
        let mark = a_server.requests.lock().unwrap().len();
        sign_in(&mut glue, &mut lib, &a_server.url);
        assert!(glue.signed_in());
        poll_until(&mut glue, &mut lib, |g, _| g.encryption_ready());
        idle(&mut glue, &mut lib, Duration::from_millis(1500));
        assert!(glue.encryption_hold(&lib), "no hold for the backup's account");
        assert!(pushed_since(&a_server, mark).iter().all(|c| c.deleted || c.enc == Encoding::Xchacha20V1), "the restored library uploaded unencrypted to its own account");
    }
}
