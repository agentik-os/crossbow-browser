//! Browser sign-in for a native app (RFC 8252). The system browser signs in
//! at the provider, which sends an authorization code back to a listener on
//! 127.0.0.1 that answers one request. PKCE (S256), `state` and `nonce` bind
//! that answer to this attempt. The sync service redeems the code with the
//! verifier; this device never holds a provider token.

use crossbow_proto::{ClerkConfig, HealthResponse};
use data_encoding::BASE64URL_NOPAD;
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use sha2::{Digest, Sha256};
use std::net::{Ipv4Addr, TcpListener as StdListener};
use std::sync::Arc;
use std::time::Duration;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

/// The loopback ports registered with the provider, tried in this order.
pub const PORTS: [u16; 3] = [47231, 47232, 47233];
/// How long the browser has to send the answer back.
pub const TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// What the browser shows once its answer has arrived.
pub const DONE_PAGE: &str = "You can close this tab and return to Crossbow.";
const SCOPES: &str = "openid email profile";
const CALLBACK: &str = "/callback";
const MAX_REQUEST_BYTES: usize = 8 * 1024;
/// Connections read at once; a page can't hold the listener with stalled ones.
const MAX_CONNECTIONS: usize = 8;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(15);

/// One attempt's secrets: the PKCE verifier, which only the sync service
/// receives, and the `state` and `nonce` the answer and ID token must carry.
pub struct Attempt {
    pub verifier: String,
    pub state: String,
    pub nonce: String,
}

impl Attempt {
    pub fn new() -> Result<Attempt, String> {
        let mut bytes = [0u8; 80];
        getrandom::fill(&mut bytes).map_err(|e| format!("random: {e}"))?;
        // 32 bytes of verifier (43 characters), 24 each of state and nonce.
        Ok(Attempt {
            verifier: BASE64URL_NOPAD.encode(&bytes[..32]),
            state: BASE64URL_NOPAD.encode(&bytes[32..56]),
            nonce: BASE64URL_NOPAD.encode(&bytes[56..]),
        })
    }

    pub fn challenge(&self) -> String {
        challenge(&self.verifier)
    }
}

/// The S256 code challenge: BASE64URL(SHA-256(verifier)).
pub fn challenge(verifier: &str) -> String {
    BASE64URL_NOPAD.encode(&Sha256::digest(verifier.as_bytes()))
}

/// An https URL, or plain http only on a loopback host (a test issuer).
fn trusted(raw: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(raw).map_err(|_| "the sign-in address isn't a URL".to_string())?;
    let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback) {
        return Err("the sign-in address isn't https".into());
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err("the sign-in address carries more than a path".into());
    }
    Ok(url)
}

/// Manage Account: the profile page of the provider's Clerk Account Portal.
/// Its host is the issuer's with the label `clerk` (or `clerk.accounts`)
/// turned into `accounts`, as Clerk's own `buildAccountsBaseUrl` derives it:
/// `x.clerk.accounts.dev` → `x.accounts.dev`, `clerk.example.com` →
/// `accounts.example.com`. None for any other issuer, so the control stays
/// hidden.
pub fn account_portal_url(config: &ClerkConfig) -> Option<String> {
    let issuer = url::Url::parse(&config.issuer).ok()?;
    if issuer.scheme() != "https" || issuer.port().is_some() || issuer.path() != "/" {
        return None;
    }
    let mut labels: Vec<&str> = issuer.host_str()?.split('.').collect();
    let at = labels.iter().position(|label| *label == "clerk")?;
    if labels.get(at + 1) == Some(&"accounts") { labels.remove(at); } else { labels[at] = "accounts"; }
    // A domain must follow `accounts`.
    if labels.len() <= at + 1 {
        return None;
    }
    Some(format!("https://{}/user", labels.join(".")))
}

/// The provider's sign-in page for this attempt, at the issuer's own origin.
pub fn authorize_url(config: &ClerkConfig, redirect_uri: &str, attempt: &Attempt) -> Result<String, String> {
    let issuer = trusted(&config.issuer)?;
    let mut url = trusted(&config.authorize_endpoint)?;
    if url.origin() != issuer.origin() {
        return Err("the sign-in page isn't the issuer's".into());
    }
    if config.client_id.is_empty() || !config.client_id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-') {
        return Err("the sign-in client isn't valid".into());
    }
    if !PORTS.iter().any(|port| redirect_uri == format!("http://127.0.0.1:{port}{CALLBACK}")) {
        return Err("the redirect isn't a registered loopback address".into());
    }
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", SCOPES)
        .append_pair("state", &attempt.state)
        .append_pair("nonce", &attempt.nonce)
        .append_pair("code_challenge", &attempt.challenge())
        .append_pair("code_challenge_method", "S256");
    Ok(url.into())
}

#[derive(Debug)]
pub enum BindError {
    /// All three registered ports are in use.
    Busy,
    Failed(String),
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindError::Busy => write!(f, "ports {}, {} and {} on 127.0.0.1 are all in use", PORTS[0], PORTS[1], PORTS[2]),
            BindError::Failed(e) => write!(f, "couldn't listen on 127.0.0.1: {e}"),
        }
    }
}

/// A listener on one registered port, not yet answering.
pub struct Listener {
    listener: StdListener,
    redirect_uri: String,
}

/// Listen on the first free registered port, on 127.0.0.1 only.
pub fn bind() -> Result<Listener, BindError> {
    bind_on(&PORTS)
}

pub(crate) fn bind_on(ports: &[u16]) -> Result<Listener, BindError> {
    let mut failure = None;
    for &port in ports {
        match StdListener::bind((Ipv4Addr::LOCALHOST, port)) {
            Ok(listener) => return Ok(Listener { listener, redirect_uri: format!("http://127.0.0.1:{port}{CALLBACK}") }),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {}
            Err(e) => failure = Some(e.to_string()),
        }
    }
    Err(failure.map_or(BindError::Busy, BindError::Failed))
}

/// How a wait ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The provider's code, for this attempt's state.
    Code(String),
    /// The person canceled in the browser (`error=access_denied`).
    Denied,
    /// The provider answered with another error (its code only), or with no code.
    Failed(String),
    /// The answer named another attempt, or none.
    StateMismatch,
    TimedOut,
    Cancelled,
}

/// A wait in progress. Cancel, or drop, closes the listener before it returns.
pub struct Waiting {
    cancel: Arc<Notify>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Waiting {
    pub fn cancel(self) {
        drop(self);
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        self.cancel.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Listener {
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Answer the browser on a thread of its own until a `/callback` arrives,
    /// `timeout` passes or the wait is canceled, then close the listener and
    /// call `done` once.
    pub fn wait(self, state: String, timeout: Duration, done: impl FnOnce(Outcome) + Send + 'static) -> Result<Waiting, String> {
        self.listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let cancel = Arc::new(Notify::new());
        let canceled = cancel.clone();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
        let thread = std::thread::Builder::new()
            .name("crossbow-browser-sign-in".into())
            .spawn(move || {
                let outcome = runtime.block_on(async move {
                    let Ok(listener) = TcpListener::from_std(self.listener) else { return Outcome::Failed("listener".into()) };
                    tokio::select! {
                        biased;
                        _ = canceled.notified() => Outcome::Cancelled,
                        _ = tokio::time::sleep(timeout) => Outcome::TimedOut,
                        outcome = serve(&listener, &state) => outcome,
                    }
                    // The listener drops here, before anyone hears the outcome.
                });
                done(outcome);
            })
            .map_err(|e| e.to_string())?;
        Ok(Waiting { cancel, thread: Some(thread) })
    }
}

/// Accept connections until one is a `/callback`; every other request is
/// refused and the wait goes on.
async fn serve(listener: &TcpListener, state: &str) -> Outcome {
    let mut reading = FuturesUnordered::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) if reading.len() < MAX_CONNECTIONS => reading.push(answer(stream, state)),
                Ok(_) => {}
                // Out of descriptors and the like: don't spin.
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            },
            Some(outcome) = reading.next() => if let Some(outcome) = outcome { return outcome },
        }
    }
}

/// Read one request and answer it. `Some` for a `/callback`, which ends the wait.
async fn answer(mut stream: TcpStream, state: &str) -> Option<Outcome> {
    let target = match tokio::time::timeout(READ_TIMEOUT, request_target(&mut stream)).await {
        Ok(Ok(target)) => target,
        Ok(Err(status)) => {
            reply(&mut stream, status, "").await;
            return None;
        }
        Err(_) => return None,
    };
    let query = match target.strip_prefix(CALLBACK) {
        Some("") => "",
        Some(rest) => match rest.strip_prefix('?') {
            Some(query) => query,
            None => {
                reply(&mut stream, "404 Not Found", "").await;
                return None;
            }
        },
        None => {
            reply(&mut stream, "404 Not Found", "").await;
            return None;
        }
    };
    let outcome = callback(query, state);
    reply(&mut stream, "200 OK", DONE_PAGE).await;
    Some(outcome)
}

/// The target of a `GET`, or the status to refuse the request with.
async fn request_target(stream: &mut TcpStream) -> Result<String, &'static str> {
    let mut bytes = Vec::with_capacity(1024);
    let mut buffer = [0u8; 1024];
    while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
        if bytes.len() >= MAX_REQUEST_BYTES {
            return Err("431 Request Header Fields Too Large");
        }
        let n = stream.read(&mut buffer).await.map_err(|_| "400 Bad Request")?;
        if n == 0 {
            return Err("400 Bad Request");
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    let head = std::str::from_utf8(&bytes).map_err(|_| "400 Bad Request")?;
    let mut line = head.lines().next().unwrap_or_default().split(' ');
    match (line.next(), line.next(), line.next()) {
        (Some("GET"), Some(target), Some(version)) if version.starts_with("HTTP/1.") => Ok(target.to_string()),
        (Some(_), Some(_), Some(_)) => Err("405 Method Not Allowed"),
        _ => Err("400 Bad Request"),
    }
}

/// What the provider's answer says, for this attempt's `state`.
fn callback(query: &str, state: &str) -> Outcome {
    let (mut code, mut sent_state, mut error) = (None, None, None);
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let slot = match &*key {
            "code" => &mut code,
            "state" => &mut sent_state,
            "error" => &mut error,
            _ => continue,
        };
        if slot.replace(value.into_owned()).is_some() {
            return Outcome::Failed("malformed answer".into());
        }
    }
    let matches = sent_state.is_some_and(|sent: String| !sent.is_empty() && bool::from(sent.as_bytes().ct_eq(state.as_bytes())));
    if !matches {
        return Outcome::StateMismatch;
    }
    match (error, code) {
        (Some(error), _) if error == "access_denied" => Outcome::Denied,
        // Only a short, plain error code is shown; the description is the page's text.
        (Some(error), _) if !error.is_empty() && error.len() <= 64 && error.bytes().all(|c| c.is_ascii_lowercase() || c == b'_') => Outcome::Failed(error),
        (Some(_), _) => Outcome::Failed("unknown error".into()),
        (None, Some(code)) if !code.is_empty() => Outcome::Code(code),
        (None, _) => Outcome::Failed("no code".into()),
    }
}

async fn reply(stream: &mut TcpStream, status: &str, text: &str) {
    let body = if text.is_empty() {
        String::new()
    } else {
        format!("<!doctype html><meta charset=\"utf-8\"><title>Crossbow</title><p style=\"font:16px -apple-system,system-ui,sans-serif;margin:4em auto;max-width:28em\">{text}</p>")
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = tokio::time::timeout(READ_TIMEOUT, async {
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(body.as_bytes()).await?;
        stream.shutdown().await
    })
    .await;
}

/// Ask the sync service whether it offers browser sign-in, on a thread of
/// its own: `Ok(None)` when it doesn't, `Err("network: …")` when it can't be
/// reached.
pub fn check_server(base_url: String, done: impl FnOnce(Result<Option<ClerkConfig>, String>) + Send + 'static) {
    let spawned = std::thread::Builder::new().name("crossbow-server-check".into()).spawn(move || {
        let result = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string()).and_then(|runtime| {
            runtime.block_on(async {
                let http = reqwest::Client::builder()
                    .user_agent(concat!("Crossbow/", env!("CARGO_PKG_VERSION")))
                    .timeout(HEALTH_TIMEOUT)
                    .build()
                    .map_err(|e| format!("network: {e}"))?;
                super::parse::<HealthResponse>(http.get(format!("{base_url}/v1/health")).send().await).await.map(|health| health.clerk)
            })
        });
        done(result);
    });
    if let Err(e) = spawned {
        log::warn!("couldn't start the server check: {e}");
    }
}

#[cfg(test)]
#[path = "browser_sign_in_tests.rs"]
mod tests;
