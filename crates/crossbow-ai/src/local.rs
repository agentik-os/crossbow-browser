//! Opt-in transport and diagnostics for already-running, OpenAI-compatible local servers.
//! The host is a literal loopback address, with no proxy, redirects, credentials,
//! server startup or model download. This does not certify local-only inference.

use crate::providers::ProviderKind;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub(super) const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
pub const TEST_PROMPT: &str = "Reply with: Crossbow connection ready.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalProvider { Ollama, LmStudio, LlamaCpp }

impl LocalProvider {
    pub const ALL: [Self; 3] = [Self::Ollama, Self::LmStudio, Self::LlamaCpp];
    pub fn kind(self) -> ProviderKind {
        match self { Self::Ollama => ProviderKind::Ollama, Self::LmStudio => ProviderKind::LmStudio, Self::LlamaCpp => ProviderKind::LlamaCpp }
    }
    pub fn default_port(self) -> u16 {
        match self { Self::Ollama => 11434, Self::LmStudio => 1234, Self::LlamaCpp => 8080 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalEndpoint {
    pub provider: LocalProvider,
    pub port: u16,
}

impl Default for LocalEndpoint {
    fn default() -> Self { Self { provider: LocalProvider::Ollama, port: LocalProvider::Ollama.default_port() } }
}

impl LocalEndpoint {
    pub fn validate(self) -> Result<(), LocalError> {
        if self.port == 0 { return Err(LocalError::InvalidPort); }
        Ok(())
    }
    pub fn address(self) -> String { format!("http://127.0.0.1:{}", self.port) }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum LocalError {
    #[error("Choose a port between 1 and 65535.")]
    InvalidPort,
    #[error("Select a valid model from the refreshed model list.")]
    InvalidModel,
    #[error("Use nonempty alternating user/assistant turns ending with a user prompt: at most 17 messages, 16 KiB each and 64 KiB total, without unsupported control characters.")]
    InvalidConversation,
    #[error("Another local conversation request is running or stopping. Wait for it to finish, then send again.")]
    Busy,
    #[error("The request was cancelled. The server may already have received it.")]
    Cancelled,
    #[error("The local server did not respond before the time limit. Check it, then retry.")]
    Timeout,
    #[error("Could not contact the local server. Start it yourself, check its port, then retry.")]
    Unavailable,
    #[error("The local server returned HTTP {0}. No credentials were sent; redirects are not followed.")]
    Http(u16),
    #[error("The local server returned an unsupported or invalid response.")]
    InvalidResponse,
    #[error("The local server response exceeded the supported size limit.")]
    TooLarge,
    #[error("The local server returned an incomplete answer. Ask for a shorter answer, then try again.")]
    Incomplete,
}

#[derive(Clone)]
pub struct Cancellation(tokio::sync::watch::Sender<bool>);

impl Default for Cancellation {
    fn default() -> Self { Self(tokio::sync::watch::channel(false).0) }
}

impl Cancellation {
    pub fn cancel(&self) { self.0.send_replace(true); }
    pub fn is_cancelled(&self) -> bool { *self.0.borrow() }
    async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        loop {
            if *receiver.borrow_and_update() { return; }
            if receiver.changed().await.is_err() { return; }
        }
    }
}

pub fn valid_model_id(model: &str) -> bool {
    !model.is_empty() && model.len() <= 256 && model.trim() == model && !model.chars().any(char::is_control)
}

pub struct LocalClient {
    endpoint: LocalEndpoint,
    http: reqwest::Client,
    timeout: Duration,
}

impl LocalClient {
    pub fn new(endpoint: LocalEndpoint) -> Result<Self, LocalError> {
        endpoint.validate()?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(2))
            .user_agent(concat!("Crossbow/", env!("CARGO_PKG_VERSION")))
            .build().map_err(|_| LocalError::Unavailable)?;
        Ok(Self { endpoint, http, timeout: Duration::from_secs(45) })
    }

    /// Sends no prompt or browsing content. It does not fetch or load a model.
    pub async fn models(&self, cancel: &Cancellation) -> Result<Vec<String>, LocalError> {
        let request = self.http.get(format!("{}/v1/models", self.endpoint.address()));
        let bytes = self.request(request, cancel, self.timeout.min(Duration::from_secs(8))).await?;
        #[derive(Deserialize)]
        struct Model { id: String }
        #[derive(Deserialize)]
        struct Catalogue { data: Vec<Model> }
        let catalogue: Catalogue = serde_json::from_slice(&bytes).map_err(|_| LocalError::InvalidResponse)?;
        if catalogue.data.len() > 256 { return Err(LocalError::TooLarge); }
        if catalogue.data.iter().any(|model| !valid_model_id(&model.id)) { return Err(LocalError::InvalidResponse); }
        let mut models: Vec<_> = catalogue.data.into_iter().map(|model| model.id).collect();
        models.sort();
        models.dedup();
        Ok(models)
    }

    /// Only the fixed public test phrase is sent. No browser context, tools,
    /// hidden retries, fallback model or account credentials are included.
    pub async fn test_connection(&self, model: &str, cancel: &Cancellation) -> Result<String, LocalError> {
        if !valid_model_id(model) { return Err(LocalError::InvalidModel); }
        let bytes = self.completion_response(&serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": TEST_PROMPT}],
            "stream": false,
            "max_tokens": 32
        }), cancel).await?;
        #[derive(Deserialize)]
        struct Answer { content: Option<String>, #[serde(default)] tool_calls: Vec<serde_json::Value> }
        #[derive(Deserialize)]
        struct Choice { message: Answer, finish_reason: Option<String> }
        #[derive(Deserialize)]
        struct Completion { choices: Vec<Choice> }
        let mut result: Completion = serde_json::from_slice(&bytes).map_err(|_| LocalError::InvalidResponse)?;
        if result.choices.len() != 1 { return Err(LocalError::InvalidResponse); }
        let choice = result.choices.remove(0);
        if choice.finish_reason.as_deref() != Some("stop") { return Err(LocalError::Incomplete); }
        if !choice.message.tool_calls.is_empty() { return Err(LocalError::InvalidResponse); }
        let text = choice.message.content.ok_or(LocalError::InvalidResponse)?;
        if text.trim().is_empty() || text.len() > 4096 || text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
            return Err(LocalError::InvalidResponse);
        }
        Ok(text)
    }

    pub(super) async fn completion_response(&self, payload: &impl Serialize, cancel: &Cancellation) -> Result<Vec<u8>, LocalError> {
        let request = self.http.post(format!("{}/v1/chat/completions", self.endpoint.address())).json(payload);
        self.request(request, cancel, self.timeout).await
    }

    pub(super) async fn completion_stream(&self, payload: &impl Serialize, cancel: &Cancellation, on_bytes: impl FnMut(&[u8]) -> Result<(), LocalError>) -> Result<(), LocalError> {
        let request = self.http.post(format!("{}/v1/chat/completions", self.endpoint.address()))
            .header(reqwest::header::ACCEPT, "text/event-stream").json(payload);
        self.request_chunks(request, cancel, self.timeout, true, on_bytes).await
    }

    #[cfg(test)]
    pub(super) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    async fn request(&self, request: reqwest::RequestBuilder, cancel: &Cancellation, timeout: Duration) -> Result<Vec<u8>, LocalError> {
        let mut bytes = Vec::new();
        self.request_chunks(request, cancel, timeout, false, |part| { bytes.extend_from_slice(part); Ok(()) }).await?;
        Ok(bytes)
    }

    // Both response modes use the same client, status/size checks and deadline.
    // Yield between bounded pieces even when a server keeps its socket readable,
    // so an all-ready stream cannot starve cancellation or the timeout timer.
    async fn request_chunks(&self, request: reqwest::RequestBuilder, cancel: &Cancellation, timeout: Duration, event_stream: bool, mut on_bytes: impl FnMut(&[u8]) -> Result<(), LocalError>) -> Result<(), LocalError> {
        if cancel.is_cancelled() { return Err(LocalError::Cancelled); }
        let transfer = async {
            let response = request.send().await.map_err(network_error)?;
            if !response.status().is_success() { return Err(LocalError::Http(response.status().as_u16())); }
            if response.content_length().is_some_and(|size| size > MAX_RESPONSE_BYTES as u64) { return Err(LocalError::TooLarge); }
            if event_stream {
                let mut types = response.headers().get_all(reqwest::header::CONTENT_TYPE).iter();
                let supported = types.next().and_then(|value| value.to_str().ok())
                    .and_then(|value| value.split(';').next()).is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"));
                if !supported || types.next().is_some() { return Err(LocalError::InvalidResponse); }
            }
            let mut received = 0usize;
            let mut stream = response.bytes_stream();
            while let Some(part) = stream.next().await {
                let part = part.map_err(network_error)?;
                received = received.saturating_add(part.len());
                if received > MAX_RESPONSE_BYTES { return Err(LocalError::TooLarge); }
                for piece in part.chunks(4096) {
                    if cancel.is_cancelled() { return Err(LocalError::Cancelled); }
                    on_bytes(piece)?;
                    tokio::task::yield_now().await;
                }
            }
            Ok(())
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(LocalError::Cancelled),
            result = tokio::time::timeout(timeout, transfer) => result.map_err(|_| LocalError::Timeout)?,
        }
    }
}

fn network_error(error: reqwest::Error) -> LocalError {
    if error.is_timeout() { LocalError::Timeout } else { LocalError::Unavailable }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Diagnostic { Models, ConnectionTest { model: String } }

#[derive(Debug, PartialEq, Eq)]
pub enum DiagnosticResult { Models(Vec<String>), Answer(String) }

/// One cancellable background job, owned by its initiating Settings draft.
/// Dropping the draft closes the request; there is no automatic retry.
pub struct DiagnosticTask {
    cancel: Cancellation,
    receiver: std::sync::mpsc::Receiver<Result<DiagnosticResult, LocalError>>,
}

impl DiagnosticTask {
    pub fn spawn(endpoint: LocalEndpoint, diagnostic: Diagnostic, wake: impl FnOnce() + Send + 'static) -> Result<Self, String> {
        endpoint.validate().map_err(|error| error.to_string())?;
        let cancel = Cancellation::default();
        let worker_cancel = cancel.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("crossbow-ai-diagnostic".into()).spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread().enable_all().build()
                .map_err(|_| LocalError::Unavailable)
                .and_then(|runtime| runtime.block_on(async {
                    let client = LocalClient::new(endpoint)?;
                    match diagnostic {
                        Diagnostic::Models => client.models(&worker_cancel).await.map(DiagnosticResult::Models),
                        Diagnostic::ConnectionTest { model } => client.test_connection(&model, &worker_cancel).await.map(DiagnosticResult::Answer),
                    }
                }));
            let _ = sender.send(result);
            wake();
        }).map_err(|_| "Could not start the diagnostic. Try again.".to_string())?;
        Ok(Self { cancel, receiver })
    }

    pub fn poll(&self) -> Option<Result<DiagnosticResult, LocalError>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(LocalError::Unavailable)),
        }
    }
}

impl Drop for DiagnosticTask {
    fn drop(&mut self) { self.cancel.cancel(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn accept(listener: &TcpListener) -> std::net::TcpStream {
        listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            match listener.accept() {
                Ok((socket, _)) => {
                    // Darwin inherits the listener's nonblocking mode.
                    socket.set_nonblocking(false).unwrap();
                    return socket;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
                Err(error) => panic!("fixture connection not received before deadline: {error}"),
            }
        }
    }

    async fn join_fixture(handle: std::thread::JoinHandle<()>) {
        // Socket teardown still needs the async reactor after the request returns.
        tokio::task::spawn_blocking(move || handle.join().unwrap()).await.unwrap();
    }

    fn server(status: &str, headers: &str, body: String) -> (LocalClient, mpsc::Receiver<String>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = LocalClient::new(LocalEndpoint { provider: LocalProvider::Ollama, port: listener.local_addr().unwrap().port() }).unwrap();
        let reply = format!("HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}", body.len());
        let (sender, receiver) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut socket = accept(&listener);
            socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            socket.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 4096];
            loop {
                let count = socket.read(&mut buf).unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buf[..count]);
                let text = String::from_utf8_lossy(&request);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").and_then(|n| n.parse::<usize>().ok())).unwrap_or(0);
                    if body.len() >= length { break; }
                }
            }
            let _ = sender.send(String::from_utf8(request).unwrap());
            let _ = socket.write_all(reply.as_bytes());
        });
        (client, receiver, handle)
    }

    #[tokio::test]
    async fn model_discovery_uses_loopback_without_credentials_or_prompt() {
        let (client, request, handle) = server("200 OK", "Content-Type: application/json\r\n", r#"{"data":[{"id":"z-model"},{"id":"a-model"},{"id":"z-model"}]}"#.into());
        assert_eq!(client.models(&Cancellation::default()).await.unwrap(), ["a-model", "z-model"]);
        let request = request.recv().unwrap();
        assert!(request.starts_with("GET /v1/models HTTP/1.1\r\n"));
        assert!(!request.to_lowercase().contains("authorization"));
        assert!(!request.to_lowercase().contains("cookie"));
        assert!(request.ends_with("\r\n\r\n"));
        join_fixture(handle).await;
    }

    #[tokio::test]
    async fn explicit_test_sends_only_fixed_phrase_and_returns_plain_text() {
        let response = r#"{"choices":[{"message":{"content":"Crossbow connection ready."},"finish_reason":"stop"}]}"#;
        let (client, request, handle) = server("200 OK", "", response.into());
        assert_eq!(client.test_connection("fixture-model", &Cancellation::default()).await.unwrap(), "Crossbow connection ready.");
        let request = request.recv().unwrap();
        assert!(request.starts_with("POST /v1/chat/completions "));
        let payload: serde_json::Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(payload, serde_json::json!({"model":"fixture-model","messages":[{"role":"user","content":TEST_PROMPT}],"stream":false,"max_tokens":32}));
        assert!(!request.to_lowercase().contains("authorization"));
        join_fixture(handle).await;
    }

    #[tokio::test]
    async fn never_follows_redirects_or_exposes_error_bodies() {
        let (client, _, handle) = server("302 Found", "Location: https://example.com/private\r\n", "sensitive fixture error".into());
        assert_eq!(client.models(&Cancellation::default()).await, Err(LocalError::Http(302)));
        join_fixture(handle).await;
        let (client, _, handle) = server("401 Unauthorized", "", "sensitive fixture error".into());
        let error = client.models(&Cancellation::default()).await.unwrap_err();
        assert_eq!(error, LocalError::Http(401));
        assert!(!error.to_string().contains("sensitive"));
        join_fixture(handle).await;
    }

    #[tokio::test]
    async fn invalid_catalogues_are_not_silently_accepted_or_truncated() {
        for body in [r#"{"data":[{"id":"bad\nmodel"}]}"#.to_owned(), r#"{"models":[]}"#.to_owned(), "not json".into()] {
            let (client, _, handle) = server("200 OK", "", body);
            assert_eq!(client.models(&Cancellation::default()).await, Err(LocalError::InvalidResponse));
            join_fixture(handle).await;
        }
        let (client, _, handle) = server("200 OK", "", "x".repeat(MAX_RESPONSE_BYTES + 1));
        assert_eq!(client.models(&Cancellation::default()).await, Err(LocalError::TooLarge));
        join_fixture(handle).await;
    }

    #[tokio::test]
    async fn rejects_tool_calls_and_incomplete_answers() {
        for (body, error) in [
            (r#"{"choices":[{"message":{"content":"partial"},"finish_reason":"length"}]}"#, LocalError::Incomplete),
            (r#"{"choices":[{"message":{"content":"text","tool_calls":[{}]},"finish_reason":"stop"}]}"#, LocalError::InvalidResponse),
            (r#"{"choices":[{"message":{"content":null},"finish_reason":"stop"}]}"#, LocalError::InvalidResponse),
        ] {
            let (client, _, handle) = server("200 OK", "", body.into());
            assert_eq!(client.test_connection("fixture-model", &Cancellation::default()).await, Err(error));
            join_fixture(handle).await;
        }
    }

    #[tokio::test]
    async fn cancellation_before_dispatch_sends_nothing() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let client = LocalClient::new(LocalEndpoint { provider: LocalProvider::Ollama, port: listener.local_addr().unwrap().port() }).unwrap();
        let cancel = Cancellation::default();
        cancel.cancel();
        assert_eq!(client.models(&cancel).await, Err(LocalError::Cancelled));
        assert_eq!(client.test_connection("fixture-model", &cancel).await, Err(LocalError::Cancelled));
        assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }

    #[tokio::test]
    async fn waiting_requests_are_cancellable_and_have_a_deadline() {
        for cancelled in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut client = LocalClient::new(LocalEndpoint { provider: LocalProvider::Ollama, port: listener.local_addr().unwrap().port() }).unwrap();
            client.timeout = Duration::from_millis(100);
            let cancel = Cancellation::default();
            let stop = cancel.clone();
            let thread = std::thread::spawn(move || {
                let mut socket = accept(&listener);
                socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut bytes = [0; 4096];
                assert!(socket.read(&mut bytes).unwrap() > 0);
                if cancelled { stop.cancel(); }
                // EOF proves the client dropped the socket, rather than leaving
                // a detached diagnostic behind after cancellation or timeout.
                assert_eq!(socket.read(&mut bytes).unwrap(), 0);
            });
            assert_eq!(client.models(&cancel).await, Err(if cancelled { LocalError::Cancelled } else { LocalError::Timeout }));
            join_fixture(thread).await;
        }
    }

    #[test]
    fn endpoint_cannot_be_a_remote_url_and_ids_are_bounded() {
        assert_eq!(LocalEndpoint::default().address(), "http://127.0.0.1:11434");
        assert_eq!(LocalEndpoint { port: 0, ..Default::default() }.validate(), Err(LocalError::InvalidPort));
        for id in ["", " padded", "line\nbreak", &"x".repeat(257)] { assert!(!valid_model_id(id)); }
        assert!(valid_model_id("namespace/model:tag"));
    }
}
