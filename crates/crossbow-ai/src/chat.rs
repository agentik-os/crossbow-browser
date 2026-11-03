//! Explicit text conversations with an already-running local server.
//! Streaming is optional; only a successful final result may enter history.
//! No page extraction, hidden system message, tools, CLI execution or credentials.
//! Returned text is inert: callers must display it as plain text, never execute it
//! or render it as markup. A loopback endpoint does not prove offline inference.

use crate::local::{valid_model_id, Cancellation, LocalClient, LocalEndpoint, LocalError};
use crate::{Message, Role};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

pub const MAX_PROMPT_BYTES: usize = 16384;
pub const MAX_TOTAL_BYTES: usize = 65536;
/// Eight completed user/assistant pairs followed by the next user prompt.
pub const MAX_MESSAGES: usize = 17;

/// Validates the entire supplied history without trimming or truncating it.
/// The caller must request a new conversation rather than silently drop turns.
pub fn validate_messages(messages: &[Message]) -> Result<(), LocalError> {
    if messages.is_empty() || messages.len() % 2 == 0 || messages.len() > MAX_MESSAGES {
        return Err(LocalError::InvalidConversation);
    }
    let mut total = 0;
    for (index, message) in messages.iter().enumerate() {
        let role = if index % 2 == 0 { Role::User } else { Role::Assistant };
        if message.role != role || message.content.len() > MAX_PROMPT_BYTES || !valid_text(&message.content) {
            return Err(LocalError::InvalidConversation);
        }
        total += message.content.len();
        if total > MAX_TOTAL_BYTES { return Err(LocalError::InvalidConversation); }
    }
    Ok(())
}

pub(super) fn valid_text(text: &str) -> bool {
    !text.trim().is_empty() && !text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    max_tokens: u16,
}

impl LocalClient {
    /// Sends exactly these validated text turns to the selected model, once.
    /// Uses the existing 45-second, 1 MiB bounded local transport; the returned
    /// answer is additionally limited to 16 KiB. No history is stored here.
    pub async fn chat(&self, model: &str, messages: &[Message], cancel: &Cancellation) -> Result<String, LocalError> {
        self.chat_limited(model, messages, 2048, cancel).await
    }

    async fn chat_limited(&self, model: &str, messages: &[Message], max_tokens: u16, cancel: &Cancellation) -> Result<String, LocalError> {
        if !valid_model_id(model) { return Err(LocalError::InvalidModel); }
        validate_messages(messages)?;
        let bytes = self.completion_response(&ChatRequest { model, messages, stream: false, max_tokens }, cancel).await?;
        let text = answer(&bytes)?;
        if cancel.is_cancelled() { return Err(LocalError::Cancelled); }
        Ok(text)
    }

    /// Reads the model list first and sends the prompt only if the server
    /// still lists `model`. Otherwise nothing but GET /v1/models is sent.
    pub(crate) async fn checked(&self, model: &str, messages: &[Message], max_tokens: u16, cancel: &Cancellation) -> Result<String, LocalError> {
        if !valid_model_id(model) { return Err(LocalError::InvalidModel); }
        validate_messages(messages)?;
        if !self.models(cancel).await?.iter().any(|listed| listed == model) { return Err(LocalError::InvalidModel); }
        self.chat_limited(model, messages, max_tokens, cancel).await
    }

    /// The callback receives the complete partial answer, not a token/delta.
    /// It must return promptly. Partials are uncommitted plain text: on error or
    /// cancellation they may remain visibly incomplete, never in history/retry.
    /// Success requires stop, DONE and clean HTTP EOF; a server that holds the
    /// stream open still reaches the 45-second deadline.
    pub async fn chat_streaming(&self, model: &str, messages: &[Message], cancel: &Cancellation, mut on_text: impl FnMut(&str)) -> Result<String, LocalError> {
        if !valid_model_id(model) { return Err(LocalError::InvalidModel); }
        validate_messages(messages)?;
        let mut decoder = crate::streaming::Decoder::new();
        self.completion_stream(&ChatRequest { model, messages, stream: true, max_tokens: 2048 }, cancel, |bytes| {
            decoder.push(bytes, &mut |text| { if !cancel.is_cancelled() { on_text(text); } })
        }).await?;
        if cancel.is_cancelled() { return Err(LocalError::Cancelled); }
        decoder.finish()
    }
}

fn answer(bytes: &[u8]) -> Result<String, LocalError> {
    #[derive(Deserialize)]
    struct Choice { message: serde_json::Value, finish_reason: Option<String> }
    #[derive(Deserialize)]
    struct Completion { choices: Vec<Choice> }
    let mut result: Completion = serde_json::from_slice(bytes).map_err(|_| LocalError::InvalidResponse)?;
    if result.choices.len() != 1 { return Err(LocalError::InvalidResponse); }
    let choice = result.choices.remove(0);
    if choice.finish_reason.as_deref() != Some("stop") { return Err(LocalError::Incomplete); }
    let message = choice.message.as_object().ok_or(LocalError::InvalidResponse)?;
    if message.get("role").is_some_and(|role| role.as_str() != Some("assistant"))
        || message.get("tool_calls").is_some_and(|calls| !calls.is_null() && calls.as_array().is_none_or(|calls| !calls.is_empty()))
        || message.get("function_call").is_some_and(|call| !call.is_null()) {
        return Err(LocalError::InvalidResponse);
    }
    let text = message.get("content").and_then(serde_json::Value::as_str).ok_or(LocalError::InvalidResponse)?;
    if text.len() > MAX_PROMPT_BYTES { return Err(LocalError::TooLarge); }
    if !valid_text(text) { return Err(LocalError::InvalidResponse); }
    Ok(text.to_string())
}

static CHAT_WORKER_ACTIVE: AtomicBool = AtomicBool::new(false);

struct WorkerPermit;

impl WorkerPermit {
    fn acquire() -> Result<Self, LocalError> {
        CHAT_WORKER_ACTIVE.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self).map_err(|_| LocalError::Busy)
    }
}

impl Drop for WorkerPermit {
    fn drop(&mut self) { CHAT_WORKER_ACTIVE.store(false, Ordering::Release); }
}

#[derive(Default)]
struct PartialState {
    latest: Option<String>,
    closed: bool,
}

#[derive(Default)]
struct PartialSlot(Mutex<PartialState>);

impl PartialSlot {
    fn publish(&self, text: &str, cancel: &Cancellation) -> bool {
        let mut state = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.closed || cancel.is_cancelled() || text.is_empty() || text.len() > MAX_PROMPT_BYTES { return false; }
        // At most one bounded full answer, never a queue of token events. A
        // pending wake covers replacements until the consumer takes the slot.
        let wake = state.latest.is_none();
        state.latest = Some(text.to_owned());
        wake
    }

    fn take(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).latest.take()
    }

    fn close(&self) {
        let mut state = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.closed = true;
        state.latest = None;
    }
}

struct PartialWriter(Arc<PartialSlot>);

impl Drop for PartialWriter {
    // Also suppress leftovers if a worker unwinds before it can publish a result.
    fn drop(&mut self) { self.0.close(); }
}

/// One process-wide chat worker, with no queue, fallback or automatic retry.
/// Cancellation/discard cannot release its permit until the actual HTTP work
/// and its runtime have ended. A finished result can be consumed only once.
pub struct ChatTask {
    cancel: Cancellation,
    receiver: mpsc::Receiver<Result<String, LocalError>>,
    finished: Cell<bool>,
    partial: Option<Arc<PartialSlot>>,
}

impl ChatTask {
    pub fn spawn(endpoint: LocalEndpoint, model: String, messages: Vec<Message>, wake: impl FnOnce() + Send + 'static) -> Result<Self, LocalError> {
        endpoint.validate()?;
        if !valid_model_id(&model) { return Err(LocalError::InvalidModel); }
        validate_messages(&messages)?;
        Self::start(move |cancel| {
            tokio::runtime::Builder::new_current_thread().enable_all().build()
                .map_err(|_| LocalError::Unavailable)
                .and_then(|runtime| runtime.block_on(async {
                    LocalClient::new(endpoint)?.chat(&model, &messages, cancel).await
                }))
        }, wake)
    }

    /// One prompt for a browser feature the user turned on, on the same single
    /// worker with the same deadline and limits. The saved model must still be
    /// listed by the server first. No history, retry or fallback model.
    pub fn spawn_checked(endpoint: LocalEndpoint, model: String, prompt: String, max_tokens: u16, wake: impl FnOnce() + Send + 'static) -> Result<Self, LocalError> {
        endpoint.validate()?;
        if !valid_model_id(&model) { return Err(LocalError::InvalidModel); }
        let messages = vec![Message::user(prompt)];
        validate_messages(&messages)?;
        Self::start(move |cancel| {
            tokio::runtime::Builder::new_current_thread().enable_all().build()
                .map_err(|_| LocalError::Unavailable)
                .and_then(|runtime| runtime.block_on(async {
                    LocalClient::new(endpoint)?.checked(&model, &messages, max_tokens, cancel).await
                }))
        }, wake)
    }

    /// Opt-in SSE with the same single-worker permit as non-streaming chat.
    /// `wake` must return promptly; it runs on progress and on every terminal
    /// result, including errors before the first token. Progress wakes coalesce
    /// while a partial is unread. No partial is a successful history turn.
    pub fn spawn_streaming(endpoint: LocalEndpoint, model: String, messages: Vec<Message>, wake: impl Fn() + Send + 'static) -> Result<Self, LocalError> {
        endpoint.validate()?;
        if !valid_model_id(&model) { return Err(LocalError::InvalidModel); }
        validate_messages(&messages)?;
        Self::start_streaming(move |cancel, on_text| {
            tokio::runtime::Builder::new_current_thread().enable_all().build()
                .map_err(|_| LocalError::Unavailable)
                .and_then(|runtime| runtime.block_on(async {
                    LocalClient::new(endpoint)?.chat_streaming(&model, &messages, cancel, on_text).await
                }))
        }, wake)
    }

    fn start_streaming(work: impl FnOnce(&Cancellation, &mut dyn FnMut(&str)) -> Result<String, LocalError> + Send + 'static, wake: impl Fn() + Send + 'static) -> Result<Self, LocalError> {
        let permit = WorkerPermit::acquire()?;
        let cancel = Cancellation::default();
        let worker_cancel = cancel.clone();
        let partial = Arc::new(PartialSlot::default());
        let writer = PartialWriter(Arc::clone(&partial));
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new().name("crossbow-ai-chat".into()).spawn(move || {
            let result = work(&worker_cancel, &mut |text| {
                if writer.0.publish(text, &worker_cancel) { wake(); }
            });
            drop(writer);
            // As in start(), release only after work has dropped its HTTP
            // future/runtime, and before a possibly slow terminal wake callback.
            drop(permit);
            if !worker_cancel.is_cancelled() { let _ = sender.send(result); }
            wake();
        }).map_err(|_| LocalError::Unavailable)?;
        Ok(Self { cancel, receiver, finished: Cell::new(false), partial: Some(partial) })
    }

    fn start(work: impl FnOnce(&Cancellation) -> Result<String, LocalError> + Send + 'static, wake: impl FnOnce() + Send + 'static) -> Result<Self, LocalError> {
        let permit = WorkerPermit::acquire()?;
        let cancel = Cancellation::default();
        let worker_cancel = cancel.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new().name("crossbow-ai-chat".into()).spawn(move || {
            let result = work(&worker_cancel);
            // The HTTP future and runtime are gone before another request can
            // start. Publishing or a slow wake callback must not hold capacity.
            drop(permit);
            if !worker_cancel.is_cancelled() { let _ = sender.send(result); }
            wake();
        }).map_err(|_| LocalError::Unavailable)?;
        Ok(Self { cancel, receiver, finished: Cell::new(false), partial: None })
    }

    /// Discards even a queued answer; HTTP cancellation is cooperative. The
    /// server may already have received the prompt and may continue computing.
    pub fn cancel(&self) {
        self.cancel.cancel();
        if let Some(partial) = &self.partial { partial.close(); }
    }

    /// Takes the newest complete partial once, never a delta or a final result.
    /// Cancel, drop and worker completion clear the slot. Previously taken text
    /// may remain labeled incomplete after error/cancel, but never enter history
    /// or retry. Ownership changes and session disposal must erase it.
    pub fn take_partial(&self) -> Option<String> {
        if self.finished.get() || self.cancel.is_cancelled() { return None; }
        self.partial.as_ref()?.take()
    }

    pub fn poll(&self) -> Option<Result<String, LocalError>> {
        if self.finished.get() { return None; }
        let result = if self.cancel.is_cancelled() {
            Some(Err(LocalError::Cancelled))
        } else {
            match self.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(LocalError::Unavailable)),
            }
        };
        if result.is_some() {
            self.finished.set(true);
            if let Some(partial) = &self.partial { partial.close(); }
        }
        result
    }
}

impl Drop for ChatTask {
    fn drop(&mut self) { self.cancel(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::LocalProvider;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    const FIXTURE_TIMEOUT: Duration = Duration::from_secs(3);
    // Only worker tests take this lock; ordinary async transport tests do not
    // acquire the process-wide worker permit and can safely run in parallel.
    static WORKER_TESTS: Mutex<()> = Mutex::new(());

    fn assistant(text: impl Into<String>) -> Message { Message { role: Role::Assistant, content: text.into() } }

    fn history(count: usize) -> Vec<Message> {
        (0..count).map(|index| if index % 2 == 0 { Message::user("prompt") } else { assistant("answer") }).collect()
    }

    fn endpoint(listener: &TcpListener) -> LocalEndpoint {
        LocalEndpoint { provider: LocalProvider::Ollama, port: listener.local_addr().unwrap().port() }
    }

    fn accept(listener: &TcpListener) -> TcpStream {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + FIXTURE_TIMEOUT;
        loop {
            match listener.accept() {
                Ok((socket, _)) => {
                    // Darwin inherits nonblocking mode from the listener.
                    socket.set_nonblocking(false).unwrap();
                    socket.set_read_timeout(Some(FIXTURE_TIMEOUT)).unwrap();
                    socket.set_write_timeout(Some(FIXTURE_TIMEOUT)).unwrap();
                    return socket;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
                Err(error) => panic!("fixture did not receive a connection: {error}"),
            }
        }
    }

    fn read_request(socket: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buf = [0; 4096];
        loop {
            let count = socket.read(&mut buf).unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&buf[..count]);
            assert!(request.len() <= MAX_TOTAL_BYTES * 2 + 4096);
            if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&request[..end]);
                let length = head.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").and_then(|n| n.parse::<usize>().ok()))
                    .or_else(|| head.starts_with("GET ").then_some(0)).unwrap();
                if request.len() - end - 4 >= length { break; }
            }
        }
        String::from_utf8(request).unwrap()
    }

    fn serve(reply: String) -> (LocalEndpoint, mpsc::Receiver<String>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = endpoint(&listener);
        let (sender, receiver) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut socket = accept(&listener);
            let _ = sender.send(read_request(&mut socket));
            let _ = socket.write_all(reply.as_bytes());
        });
        (endpoint, receiver, handle)
    }

    fn server(status: &str, headers: &str, body: String) -> (LocalEndpoint, mpsc::Receiver<String>, std::thread::JoinHandle<()>) {
        serve(format!("HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}", body.len()))
    }

    fn completion(text: &str) -> String {
        serde_json::json!({"choices":[{"message":{"role":"assistant","content":text},"finish_reason":"stop"}]}).to_string()
    }

    fn assert_socket_closed(socket: &mut TcpStream) {
        match socket.read(&mut [0; 16]) {
            Ok(0) => {},
            // Dropping an HTTP request with unread response bytes may reset the
            // connection rather than send a graceful FIN (notably on Darwin).
            Err(error) if matches!(error.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted) => {},
            other => panic!("cancel/timeout must close the HTTP socket: {other:?}"),
        }
    }

    async fn join_fixture(handle: std::thread::JoinHandle<()>) {
        // Permit the current-thread reactor to finish socket teardown.
        tokio::task::spawn_blocking(move || handle.join().unwrap()).await.unwrap();
    }

    #[test]
    fn histories_must_alternate_and_end_in_a_nonempty_user_prompt() {
        for messages in [
            vec![], history(2), history(MAX_MESSAGES + 2), vec![assistant("answer")],
            vec![Message::system("hidden")], vec![Message::user("one"), Message::user("two"), Message::user("three")],
            vec![Message::user("one"), Message::system("hidden"), Message::user("three")],
            vec![Message::user(" \n\r\t")], vec![Message::user("one"), assistant("\u{2003}"), Message::user("three")],
        ] {
            assert_eq!(validate_messages(&messages), Err(LocalError::InvalidConversation));
        }
        for control in ['\0', '\u{0007}', '\u{007f}', '\u{0085}'] {
            assert_eq!(validate_messages(&[Message::user(format!("text{control}"))]), Err(LocalError::InvalidConversation));
            assert_eq!(validate_messages(&[Message::user("one"), assistant(format!("text{control}")), Message::user("three")]), Err(LocalError::InvalidConversation));
        }
        assert_eq!(validate_messages(&[Message::user(" \ttext\r\n ")]), Ok(()));
        assert_eq!(validate_messages(&history(MAX_MESSAGES)), Ok(()));
    }

    #[test]
    fn history_limits_count_utf8_bytes_without_truncation() {
        let full = "é".repeat(MAX_PROMPT_BYTES / 2);
        assert_eq!(validate_messages(&[Message::user(&full)]), Ok(()));
        assert_eq!(validate_messages(&[Message::user(format!("{full}x"))]), Err(LocalError::InvalidConversation));
        assert_eq!(validate_messages(&[Message::user("one"), assistant("x".repeat(MAX_PROMPT_BYTES + 1)), Message::user("three")]), Err(LocalError::InvalidConversation));
        let mut messages = vec![Message::user(&full), assistant(&full), Message::user(&full), assistant("x".repeat(MAX_PROMPT_BYTES - 1)), Message::user("x")];
        let exact = messages.clone();
        assert_eq!(validate_messages(&messages), Ok(()));
        assert_eq!(messages, exact);
        messages.last_mut().unwrap().content.push('x');
        assert_eq!(validate_messages(&messages), Err(LocalError::InvalidConversation));
    }

    #[tokio::test]
    async fn sends_exact_history_and_model_without_credentials_or_hidden_context() {
        let text = "  Plain <b>text</b> and `code`\n\tOnly text.  ";
        let (endpoint, request, handle) = server("200 OK", "Content-Type: application/json\r\nSet-Cookie: synthetic=unused\r\n", completion(text));
        let client = LocalClient::new(endpoint).unwrap();
        let messages = vec![Message::user(" first\r\n\tprompt "), assistant("earlier answer"), Message::user("next é prompt")];
        assert_eq!(client.chat("fixture/model:tag", &messages, &Cancellation::default()).await.unwrap(), text);
        let request = request.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        let (headers, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        assert!(headers.to_ascii_lowercase().contains(&format!("host: 127.0.0.1:{}", endpoint.port)));
        for forbidden in ["authorization:", "proxy-authorization:", "cookie:", "x-api-key:"] { assert!(!headers.to_ascii_lowercase().contains(forbidden)); }
        let payload: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(payload, serde_json::json!({"model":"fixture/model:tag","messages":messages,"stream":false,"max_tokens":2048}));
        join_fixture(handle).await;
    }

    #[tokio::test]
    async fn invalid_history_model_and_pre_cancel_never_contact_a_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let client = LocalClient::new(endpoint(&listener)).unwrap();
        for messages in [vec![], history(2), history(MAX_MESSAGES + 2), vec![Message::system("hidden")], vec![Message::user("x".repeat(MAX_PROMPT_BYTES + 1))]] {
            assert_eq!(client.chat("fixture-model", &messages, &Cancellation::default()).await, Err(LocalError::InvalidConversation));
        }
        assert_eq!(client.chat("bad\nmodel", &history(1), &Cancellation::default()).await, Err(LocalError::InvalidModel));
        let cancel = Cancellation::default();
        cancel.cancel();
        assert_eq!(client.chat("fixture-model", &history(1), &cancel).await, Err(LocalError::Cancelled));
        assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn review_empty_optional_tool_fields_are_text_not_invocations() {
        for fields in [
            serde_json::json!({"tool_calls":null}),
            serde_json::json!({"tool_calls":[]}),
            serde_json::json!({"function_call":null}),
            serde_json::json!({"tool_calls":[],"function_call":null}),
        ] {
            let mut message = serde_json::json!({"role":"assistant","content":"ordinary answer"});
            message.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
            let response = serde_json::json!({"choices":[{"message":message,"finish_reason":"stop"}]}).to_string();
            assert_eq!(answer(response.as_bytes()), Ok("ordinary answer".into()));
        }
    }

    #[tokio::test]
    async fn rejects_non_text_multiple_tool_function_and_incomplete_outputs() {
        let cases = [
            ("not json", LocalError::InvalidResponse),
            (r#"{"choices":[]}"#, LocalError::InvalidResponse),
            (r#"{"choices":[{"message":{"content":"one"},"finish_reason":"stop"},{"message":{"content":"two"},"finish_reason":"stop"}]}"#, LocalError::InvalidResponse),
            (r#"{"choices":[{"message":{"content":"text","role":"user"},"finish_reason":"stop"}]}"#, LocalError::InvalidResponse),
            (r#"{"choices":[{"message":{"content":"text","role":null},"finish_reason":"stop"}]}"#, LocalError::InvalidResponse),
            (r#"{"choices":[{"message":{"content":"text","tool_calls":[{}]},"finish_reason":"stop"}]}"#, LocalError::InvalidResponse),
            (r#"{"choices":[{"message":{"content":"text","tool_calls":{}},"finish_reason":"stop"}]}"#, LocalError::InvalidResponse),
            (r#"{"choices":[{"message":{"content":"text","tool_calls":""},"finish_reason":"stop"}]}"#, LocalError::InvalidResponse),
            (r#"{"choices":[{"message":{"content":"text","function_call":{"name":"fixture","arguments":"{}"}},"finish_reason":"stop"}]}"#, LocalError::InvalidResponse),
            (r#"{"choices":[{"message":{"content":"text","function_call":{}},"finish_reason":"stop"}]}"#, LocalError::InvalidResponse),
            (r#"{"choices":[{"message":{"content":"text","function_call":[]},"finish_reason":"stop"}]}"#, LocalError::InvalidResponse),
            (r#"{"choices":[{"message":{"content":null},"finish_reason":"stop"}]}"#, LocalError::InvalidResponse),
            (r#"{"choices":[{"message":{"content":[{"type":"text","text":"text"}]},"finish_reason":"stop"}]}"#, LocalError::InvalidResponse),
            (r#"{"choices":[{"message":{"content":" \n\t\r"},"finish_reason":"stop"}]}"#, LocalError::InvalidResponse),
            (r#"{"choices":[{"message":{"content":"text\u0000"},"finish_reason":"stop"}]}"#, LocalError::InvalidResponse),
            (r#"{"choices":[{"message":{"content":"partial"},"finish_reason":"length"}]}"#, LocalError::Incomplete),
            (r#"{"choices":[{"message":{"content":"partial"},"finish_reason":"tool_calls"}]}"#, LocalError::Incomplete),
            (r#"{"choices":[{"message":{"content":"partial"},"finish_reason":"content_filter"}]}"#, LocalError::Incomplete),
            (r#"{"choices":[{"message":{"content":"partial"},"finish_reason":null}]}"#, LocalError::Incomplete),
            (r#"{"choices":[{"message":{"content":"partial"}}]}"#, LocalError::Incomplete),
        ];
        for (body, error) in cases {
            let (endpoint, _, handle) = server("200 OK", "", body.into());
            let client = LocalClient::new(endpoint).unwrap();
            assert_eq!(client.chat("fixture-model", &history(1), &Cancellation::default()).await, Err(error), "{body}");
            join_fixture(handle).await;
        }
        let (endpoint, _, handle) = server("200 OK", "", r#"{"choices":[{"message":{"content":"no role field"},"finish_reason":"stop"}]}"#.into());
        assert_eq!(LocalClient::new(endpoint).unwrap().chat("fixture-model", &history(1), &Cancellation::default()).await.unwrap(), "no role field");
        join_fixture(handle).await;
    }

    #[tokio::test]
    async fn bounds_decoded_answer_and_both_response_transfer_lengths() {
        for extra in [false, true] {
            let text = format!("{}{}", "é".repeat(MAX_PROMPT_BYTES / 2), if extra { "x" } else { "" });
            let (endpoint, _, handle) = server("200 OK", "", completion(&text));
            let result = LocalClient::new(endpoint).unwrap().chat("fixture-model", &history(1), &Cancellation::default()).await;
            assert_eq!(result, if extra { Err(LocalError::TooLarge) } else { Ok(text) });
            join_fixture(handle).await;
        }
        let oversized = "x".repeat(1024 * 1024 + 1);
        for reply in [
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", oversized.len()),
            format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{oversized}\r\n0\r\n\r\n", oversized.len()),
        ] {
            let (endpoint, _, handle) = serve(reply);
            assert_eq!(LocalClient::new(endpoint).unwrap().chat("fixture-model", &history(1), &Cancellation::default()).await, Err(LocalError::TooLarge));
            join_fixture(handle).await;
        }
    }

    #[tokio::test]
    async fn redirects_are_not_followed_and_error_bodies_are_not_reported() {
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        for (status, code) in [("302 Found", 302), ("307 Temporary Redirect", 307), ("401 Unauthorized", 401), ("500 Internal Server Error", 500)] {
            let location = format!("Location: http://127.0.0.1:{}/private-fixture\r\n", target.local_addr().unwrap().port());
            let (endpoint, _, handle) = server(status, &location, "sensitive synthetic fixture error".into());
            let error = LocalClient::new(endpoint).unwrap().chat("fixture-model", &history(1), &Cancellation::default()).await.unwrap_err();
            assert_eq!(error, LocalError::Http(code));
            assert!(!error.to_string().contains("sensitive"));
            assert!(!error.to_string().contains("private-fixture"));
            assert_eq!(target.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
            join_fixture(handle).await;
        }
    }

    #[tokio::test]
    async fn cancellation_and_timeout_close_sockets_while_waiting_for_headers_or_body() {
        for partial_body in [false, true] {
            for cancelled in [false, true] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let client = LocalClient::new(endpoint(&listener)).unwrap().with_timeout(Duration::from_millis(300));
                let cancel = Cancellation::default();
                let stop = cancel.clone();
                let handle = std::thread::spawn(move || {
                    let mut socket = accept(&listener);
                    read_request(&mut socket);
                    if partial_body { socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\n\r\n{").unwrap(); }
                    if cancelled { stop.cancel(); }
                    assert_socket_closed(&mut socket);
                });
                assert_eq!(client.chat("fixture-model", &history(1), &cancel).await, Err(if cancelled { LocalError::Cancelled } else { LocalError::Timeout }));
                join_fixture(handle).await;
            }
        }
    }

    #[test]
    fn worker_permit_outlives_cancelled_and_dropped_task_but_not_wake_callback() {
        let _serial = WORKER_TESTS.lock().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = endpoint(&listener);
        let (entered, entry) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (waking, wake_started) = mpsc::channel();
        let (finish_wake, wake_release) = mpsc::channel();
        let (exited, wake_exited) = mpsc::channel();
        let task = ChatTask::start(move |cancel| {
            entered.send(()).unwrap();
            released.recv_timeout(FIXTURE_TIMEOUT).unwrap();
            assert!(cancel.is_cancelled());
            Ok("stale answer".into())
        }, move || {
            waking.send(()).unwrap();
            wake_release.recv_timeout(FIXTURE_TIMEOUT).unwrap();
            exited.send(()).unwrap();
        }).unwrap();
        entry.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        assert!(matches!(ChatTask::spawn(endpoint, "fixture-model".into(), history(1), || {}), Err(LocalError::Busy)));
        // Validation must happen before worker admission, even when it is busy.
        assert!(matches!(ChatTask::spawn(endpoint, "fixture-model".into(), vec![], || panic!("invalid request woke")), Err(LocalError::InvalidConversation)));
        assert!(matches!(ChatTask::spawn(endpoint, "bad\nmodel".into(), history(1), || {}), Err(LocalError::InvalidModel)));
        assert!(matches!(ChatTask::spawn(LocalEndpoint { port: 0, ..endpoint }, "fixture-model".into(), history(1), || {}), Err(LocalError::InvalidPort)));
        task.cancel();
        assert_eq!(task.poll(), Some(Err(LocalError::Cancelled)));
        assert_eq!(task.poll(), None);
        assert!(matches!(ChatTask::spawn(endpoint, "fixture-model".into(), history(1), || {}), Err(LocalError::Busy)));
        drop(task);
        assert!(matches!(ChatTask::spawn(endpoint, "fixture-model".into(), history(1), || {}), Err(LocalError::Busy)));
        assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        release.send(()).unwrap();
        wake_started.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        let (done, completed) = mpsc::channel();
        let replacement = ChatTask::start(|_| Ok("replacement answer".into()), move || { done.send(()).unwrap(); }).unwrap();
        completed.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        assert_eq!(replacement.poll(), Some(Ok("replacement answer".into())));
        assert_eq!(replacement.poll(), None);
        finish_wake.send(()).unwrap();
        wake_exited.recv_timeout(FIXTURE_TIMEOUT).unwrap();
    }

    #[test]
    fn dropping_real_chat_task_closes_socket_and_releases_capacity_for_next_request() {
        let _serial = WORKER_TESTS.lock().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = endpoint(&listener);
        let (received, request_received) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut socket = accept(&listener);
            read_request(&mut socket);
            received.send(()).unwrap();
            assert_socket_closed(&mut socket);
        });
        let (waking, wake) = mpsc::channel();
        let task = ChatTask::spawn(endpoint, "fixture-model".into(), history(1), move || { let _ = waking.send(()); }).unwrap();
        request_received.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        drop(task);
        wake.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        handle.join().unwrap();
        let (endpoint, request, handle) = server("200 OK", "", completion("fresh answer"));
        let (waking, wake) = mpsc::channel();
        let next = ChatTask::spawn(endpoint, "fixture-model".into(), history(1), move || { let _ = waking.send(()); }).unwrap();
        wake.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        assert_eq!(next.poll(), Some(Ok("fresh answer".into())));
        assert_eq!(next.poll(), None);
        assert!(request.recv_timeout(FIXTURE_TIMEOUT).unwrap().starts_with("POST /v1/chat/completions "));
        handle.join().unwrap();
    }

    #[test]
    fn failed_worker_releases_capacity_and_reports_its_error_once() {
        let _serial = WORKER_TESTS.lock().unwrap();
        for error in [LocalError::Unavailable, LocalError::Incomplete] {
            let expected = error.clone();
            let (waking, wake) = mpsc::channel();
            let task = ChatTask::start(move |_| Err(error), move || { waking.send(()).unwrap(); }).unwrap();
            wake.recv_timeout(FIXTURE_TIMEOUT).unwrap();
            assert_eq!(task.poll(), Some(Err(expected)));
            assert_eq!(task.poll(), None);
        }
    }

    #[test]
    fn cancelling_a_queued_answer_cannot_surface_it_later() {
        let _serial = WORKER_TESTS.lock().unwrap();
        let (waking, wake) = mpsc::channel();
        let task = ChatTask::start(|_| Ok("discard this answer".into()), move || { waking.send(()).unwrap(); }).unwrap();
        wake.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        task.cancel();
        assert_eq!(task.poll(), Some(Err(LocalError::Cancelled)));
        assert_eq!(task.poll(), None);
    }

    fn sse_delta(text: &str, finish: Option<&str>) -> String {
        format!("data: {}\n\n", serde_json::json!({"choices":[{"index":0,"delta":{"role":"assistant","content":text},"finish_reason":finish}]}))
    }

    fn sse_answer(text: &str) -> String { format!("{}data: [DONE]\n\n", sse_delta(text, Some("stop"))) }

    fn sse_headers(socket: &mut TcpStream) {
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
    }

    fn write_chunk(socket: &mut TcpStream, bytes: &[u8]) {
        write!(socket, "{:x}\r\n", bytes.len()).unwrap();
        socket.write_all(bytes).unwrap();
        socket.write_all(b"\r\n").unwrap();
    }

    #[tokio::test]
    async fn streaming_uses_exact_request_and_reassembles_packet_split_utf8_crlf() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = endpoint(&listener);
        let (sent, request) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut socket = accept(&listener);
            sent.send(read_request(&mut socket)).unwrap();
            sse_headers(&mut socket);
            let body = format!(": comment\r\nevent: message\r\n\r\n{}{}data: [DONE]\r\n\r\n", sse_delta("é🦀", None).replace('\n', "\r\n"), sse_delta(" answer", Some("stop")));
            for byte in body.as_bytes() { write_chunk(&mut socket, std::slice::from_ref(byte)); }
            socket.write_all(b"0\r\n\r\n").unwrap();
        });
        let client = LocalClient::new(endpoint).unwrap();
        let messages = vec![Message::user(" first\r\n\tprompt "), assistant("earlier answer"), Message::user("next é prompt")];
        let mut partials = Vec::new();
        assert_eq!(client.chat_streaming("fixture/model:tag", &messages, &Cancellation::default(), |text| partials.push(text.to_string())).await, Ok("é🦀 answer".into()));
        assert_eq!(partials, ["é🦀", "é🦀 answer"]);
        let request = request.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        let (headers, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        assert!(headers.to_ascii_lowercase().contains(&format!("host: 127.0.0.1:{}", endpoint.port)));
        assert!(headers.to_ascii_lowercase().contains("accept: text/event-stream"));
        for forbidden in ["authorization:", "proxy-authorization:", "cookie:", "x-api-key:"] { assert!(!headers.to_ascii_lowercase().contains(forbidden)); }
        let payload: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(payload, serde_json::json!({"model":"fixture/model:tag","messages":messages,"stream":true,"max_tokens":2048}));
        join_fixture(handle).await;
    }

    #[tokio::test]
    async fn streaming_requires_one_event_stream_content_type_before_exposing_text() {
        for headers in [
            "", "Content-Type: application/json\r\n", "Content-Type: text/plain\r\n",
            "Content-Type: text/event-stream, text/plain\r\n",
            "Content-Type: text/event-stream\r\nContent-Type: text/plain\r\n",
            "Content-Type: text/event-stream\r\nContent-Type: text/event-stream\r\n",
        ] {
            let (endpoint, _, handle) = server("200 OK", headers, sse_answer("synthetic answer"));
            let mut exposed = false;
            let result = LocalClient::new(endpoint).unwrap().chat_streaming("fixture-model", &history(1), &Cancellation::default(), |_| exposed = true).await;
            assert_eq!(result, Err(LocalError::InvalidResponse), "unsupported headers: {headers:?}");
            assert!(!exposed, "unsupported response must fail before dispatching content");
            join_fixture(handle).await;
        }
        for content_type in ["text/event-stream", "Text/Event-Stream; charset=utf-8"] {
            let (endpoint, _, handle) = server("200 OK", &format!("Content-Type: {content_type}\r\n"), sse_answer("synthetic answer"));
            assert_eq!(LocalClient::new(endpoint).unwrap().chat_streaming("fixture-model", &history(1), &Cancellation::default(), |_| {}).await, Ok("synthetic answer".into()));
            join_fixture(handle).await;
        }
    }

    #[tokio::test]
    async fn streaming_validates_all_inputs_and_pre_cancel_before_connecting() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let client = LocalClient::new(endpoint(&listener)).unwrap();
        let aggregate: Vec<_> = (0..5).map(|index| {
            if index % 2 == 0 { Message::user("x".repeat(MAX_PROMPT_BYTES)) } else { assistant("x".repeat(MAX_PROMPT_BYTES)) }
        }).collect();
        for messages in [vec![], history(2), history(MAX_MESSAGES + 2), vec![Message::system("hidden")], vec![Message::user("bad\0prompt")], vec![Message::user("x".repeat(MAX_PROMPT_BYTES + 1))], aggregate] {
            assert_eq!(client.chat_streaming("fixture-model", &messages, &Cancellation::default(), |_| panic!("invalid input callback")).await, Err(LocalError::InvalidConversation));
        }
        assert_eq!(client.chat_streaming("bad\nmodel", &history(1), &Cancellation::default(), |_| {}).await, Err(LocalError::InvalidModel));
        let cancel = Cancellation::default();
        cancel.cancel();
        assert_eq!(client.chat_streaming("fixture-model", &history(1), &cancel, |_| panic!("cancelled callback")).await, Err(LocalError::Cancelled));
        assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }

    #[tokio::test]
    async fn streaming_protocol_errors_and_incomplete_eof_never_become_answers() {
        let partial = sse_delta("uncommitted text", None);
        for (tail, error) in [
            (String::new(), LocalError::Incomplete),
            (sse_delta("", Some("stop")), LocalError::Incomplete),
            ("data: [DONE]\n\n".into(), LocalError::Incomplete),
            ("data: invalid json\n\n".into(), LocalError::InvalidResponse),
            ("data: {\"error\":\"sensitive synthetic server diagnostic\"}\n\n".into(), LocalError::InvalidResponse),
            ("data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{}]},\"finish_reason\":null}]}\n\n".into(), LocalError::InvalidResponse),
            ("data: {\"choices\":[{\"index\":0,\"delta\":{\"function_call\":{}},\"finish_reason\":null}]}\n\n".into(), LocalError::InvalidResponse),
            (sse_delta("", Some("content_filter")), LocalError::Incomplete),
            (sse_delta("x", Some("length")), LocalError::Incomplete),
            (sse_delta(&"x".repeat(MAX_PROMPT_BYTES), None), LocalError::TooLarge),
        ] {
            let (endpoint, _, handle) = server("200 OK", "Content-Type: text/event-stream\r\n", format!("{partial}{tail}"));
            let mut partials = Vec::new();
            let result = LocalClient::new(endpoint).unwrap().chat_streaming("fixture-model", &history(1), &Cancellation::default(), |text| partials.push(text.to_string())).await;
            assert_eq!(result, Err(error.clone()));
            assert_eq!(partials, ["uncommitted text"]);
            assert!(!error.to_string().contains("sensitive"));
            join_fixture(handle).await;
        }
    }

    #[tokio::test]
    async fn streaming_shares_status_redirect_and_transfer_size_policy() {
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        for (status, code) in [("302 Found", 302), ("307 Temporary Redirect", 307), ("401 Unauthorized", 401), ("500 Internal Server Error", 500)] {
            let location = format!("Location: http://127.0.0.1:{}/sensitive\r\n", target.local_addr().unwrap().port());
            let (endpoint, _, handle) = server(status, &location, "private synthetic error".into());
            let error = LocalClient::new(endpoint).unwrap().chat_streaming("fixture-model", &history(1), &Cancellation::default(), |_| panic!("HTTP error dispatched text")).await.unwrap_err();
            assert_eq!(error, LocalError::Http(code));
            assert!(!error.to_string().contains("private"));
            assert_eq!(target.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
            join_fixture(handle).await;
        }
        let oversized = ":\n\n".repeat(crate::local::MAX_RESPONSE_BYTES / 3 + 1);
        for reply in [
            format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", oversized.len()),
            format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{oversized}\r\n0\r\n\r\n", oversized.len()),
        ] {
            let (endpoint, _, handle) = serve(reply);
            assert_eq!(LocalClient::new(endpoint).unwrap().chat_streaming("fixture-model", &history(1), &Cancellation::default(), |_| panic!("comment dispatched")).await, Err(LocalError::TooLarge));
            join_fixture(handle).await;
        }
    }

    #[tokio::test]
    async fn streaming_cancellation_and_deadline_close_headers_body_and_done_waits() {
        for phase in 0..3 {
            for cancelled in [false, true] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let client = LocalClient::new(endpoint(&listener)).unwrap().with_timeout(Duration::from_millis(300));
                let cancel = Cancellation::default();
                let stop = cancel.clone();
                let (observed, partial_observed) = mpsc::channel();
                let handle = std::thread::spawn(move || {
                    let mut socket = accept(&listener);
                    read_request(&mut socket);
                    if phase != 0 {
                        sse_headers(&mut socket);
                        let body = if phase == 1 { sse_delta("uncommitted", None) } else { sse_answer("uncommitted") };
                        write_chunk(&mut socket, body.as_bytes());
                        partial_observed.recv_timeout(FIXTURE_TIMEOUT).unwrap();
                    }
                    if cancelled { stop.cancel(); }
                    // Even DONE does not turn a stalled HTTP body into success.
                    assert_socket_closed(&mut socket);
                });
                let mut partials = Vec::new();
                let result = client.chat_streaming("fixture-model", &history(1), &cancel, |text| {
                    partials.push(text.to_string());
                    observed.send(()).unwrap();
                }).await;
                assert_eq!(result, Err(if cancelled { LocalError::Cancelled } else { LocalError::Timeout }));
                assert_eq!(partials.len(), usize::from(phase != 0));
                join_fixture(handle).await;
            }
        }
    }

    #[tokio::test]
    async fn streaming_parser_failure_closes_socket_and_truncated_http_fails_after_done() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = LocalClient::new(endpoint(&listener)).unwrap();
        let handle = std::thread::spawn(move || {
            let mut socket = accept(&listener);
            read_request(&mut socket);
            sse_headers(&mut socket);
            write_chunk(&mut socket, format!("{}data: not json\n\n", sse_delta("uncommitted", None)).as_bytes());
            assert_socket_closed(&mut socket);
        });
        assert_eq!(client.chat_streaming("fixture-model", &history(1), &Cancellation::default(), |_| {}).await, Err(LocalError::InvalidResponse));
        join_fixture(handle).await;
        let body = sse_answer("uncommitted");
        let (endpoint, _, handle) = serve(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len() + 10));
        assert_eq!(LocalClient::new(endpoint).unwrap().chat_streaming("fixture-model", &history(1), &Cancellation::default(), |_| {}).await, Err(LocalError::Unavailable));
        join_fixture(handle).await;
    }

    #[test]
    fn streaming_task_exposes_partial_before_final_and_consumes_each_only_once() {
        let _serial = WORKER_TESTS.lock().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = endpoint(&listener);
        let (release, released) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut socket = accept(&listener);
            read_request(&mut socket);
            sse_headers(&mut socket);
            write_chunk(&mut socket, sse_delta("first", None).as_bytes());
            released.recv_timeout(FIXTURE_TIMEOUT).unwrap();
            write_chunk(&mut socket, format!("{}data: [DONE]\n\n", sse_delta(" second", Some("stop"))).as_bytes());
            socket.write_all(b"0\r\n\r\n").unwrap();
        });
        let (waking, wake) = mpsc::channel();
        let task = ChatTask::spawn_streaming(endpoint, "fixture-model".into(), history(1), move || { let _ = waking.send(()); }).unwrap();
        wake.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        assert_eq!(task.poll(), None);
        assert_eq!(task.take_partial(), Some("first".into()));
        assert_eq!(task.take_partial(), None);
        release.send(()).unwrap();
        let deadline = Instant::now() + FIXTURE_TIMEOUT;
        let result = loop {
            wake.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap();
            if let Some(result) = task.poll() { break result; }
        };
        assert_eq!(result, Ok("first second".into()));
        assert_eq!(task.poll(), None);
        assert_eq!(task.take_partial(), None);
        handle.join().unwrap();
    }

    #[test]
    fn streaming_terminal_wake_without_tokens_and_after_partial_error_is_one_shot() {
        let _serial = WORKER_TESTS.lock().unwrap();
        for (body, error) in [("data: not json\n\n".into(), LocalError::InvalidResponse), (sse_delta("uncommitted", None), LocalError::Incomplete)] {
            let (endpoint, _, handle) = server("200 OK", "Content-Type: text/event-stream\r\n", body);
            let (waking, wake) = mpsc::channel();
            let task = ChatTask::spawn_streaming(endpoint, "fixture-model".into(), history(1), move || { let _ = waking.send(()); }).unwrap();
            let deadline = Instant::now() + FIXTURE_TIMEOUT;
            loop {
                wake.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap();
                if let Some(result) = task.poll() { assert_eq!(result, Err(error)); break; }
            }
            assert_eq!(task.poll(), None);
            assert_eq!(task.take_partial(), None);
            handle.join().unwrap();
        }
    }

    #[test]
    fn streaming_coalesces_to_one_bounded_slot_and_suppresses_late_cancelled_text() {
        let _serial = WORKER_TESTS.lock().unwrap();
        let (entered, entry) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (waking, wake) = mpsc::channel();
        let task = ChatTask::start_streaming(move |cancel, on_text| {
            let mut text = String::new();
            for _ in 0..128 { text.push_str(&"x".repeat(MAX_PROMPT_BYTES / 128)); on_text(&text); }
            entered.send(()).unwrap();
            released.recv_timeout(FIXTURE_TIMEOUT).unwrap();
            assert!(cancel.is_cancelled());
            on_text("late partial must be suppressed");
            Ok("late result must be suppressed".into())
        }, move || { let _ = waking.send(()); }).unwrap();
        entry.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        wake.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        assert!(matches!(wake.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert_eq!(task.take_partial(), Some("x".repeat(MAX_PROMPT_BYTES)));
        assert_eq!(task.take_partial(), None);
        assert_eq!(task.poll(), None);
        task.cancel();
        assert_eq!(task.take_partial(), None);
        assert_eq!(task.poll(), Some(Err(LocalError::Cancelled)));
        assert_eq!(task.poll(), None);
        release.send(()).unwrap();
        wake.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        assert_eq!(task.take_partial(), None);
        assert_eq!(task.poll(), None);
    }

    #[test]
    fn streaming_and_nonstreaming_share_admission_through_cancel_and_drop() {
        let _serial = WORKER_TESTS.lock().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = endpoint(&listener);
        for streaming in [false, true] {
            let (entered, entry) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let (waking, wake) = mpsc::channel();
            let work = move |cancel: &Cancellation| {
                entered.send(()).unwrap();
                released.recv_timeout(FIXTURE_TIMEOUT).unwrap();
                assert!(cancel.is_cancelled());
                Ok("stale answer".into())
            };
            let callback = move || { let _ = waking.send(()); };
            let task = if streaming { ChatTask::start_streaming(move |cancel, _| work(cancel), callback) } else { ChatTask::start(work, callback) }.unwrap();
            entry.recv_timeout(FIXTURE_TIMEOUT).unwrap();
            assert!(matches!(ChatTask::spawn(endpoint, "fixture-model".into(), history(1), || {}), Err(LocalError::Busy)));
            assert!(matches!(ChatTask::spawn_streaming(endpoint, "fixture-model".into(), history(1), || {}), Err(LocalError::Busy)));
            assert!(matches!(ChatTask::spawn_streaming(endpoint, "fixture-model".into(), vec![], || {}), Err(LocalError::InvalidConversation)));
            assert!(matches!(ChatTask::spawn_streaming(endpoint, "bad\nmodel".into(), history(1), || {}), Err(LocalError::InvalidModel)));
            assert!(matches!(ChatTask::spawn_streaming(LocalEndpoint { port: 0, ..endpoint }, "fixture-model".into(), history(1), || {}), Err(LocalError::InvalidPort)));
            task.cancel();
            assert_eq!(task.poll(), Some(Err(LocalError::Cancelled)));
            assert_eq!(task.poll(), None);
            let partial = task.partial.clone();
            drop(task);
            if let Some(partial) = partial { assert_eq!(partial.take(), None); }
            assert!(matches!(ChatTask::spawn_streaming(endpoint, "fixture-model".into(), history(1), || {}), Err(LocalError::Busy)));
            assert!(matches!(ChatTask::spawn(endpoint, "fixture-model".into(), history(1), || {}), Err(LocalError::Busy)));
            assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
            release.send(()).unwrap();
            wake.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        }
    }

    #[test]
    fn streaming_terminal_wake_cannot_hold_capacity_or_expose_completed_partials() {
        let _serial = WORKER_TESTS.lock().unwrap();
        let (waking, wake_started) = mpsc::channel();
        let (finish_wake, wake_release) = mpsc::channel();
        let (exited, wake_exited) = mpsc::channel();
        // Cell is Send but not Sync: streaming wake intentionally requires only
        // Fn + Send, because all calls belong to this one worker thread.
        let calls = Cell::new(0);
        let task = ChatTask::start_streaming(|_, on_text| { on_text("discarded partial"); Ok("final".into()) }, move || {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                waking.send(()).unwrap();
                wake_release.recv_timeout(FIXTURE_TIMEOUT).unwrap();
                exited.send(()).unwrap();
            }
        }).unwrap();
        wake_started.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        assert_eq!(task.take_partial(), None);
        assert_eq!(task.poll(), Some(Ok("final".into())));
        assert_eq!(task.poll(), None);
        let (done, completed) = mpsc::channel();
        let replacement = ChatTask::start(|_| Ok("replacement".into()), move || { done.send(()).unwrap(); }).unwrap();
        completed.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        assert_eq!(replacement.poll(), Some(Ok("replacement".into())));
        finish_wake.send(()).unwrap();
        wake_exited.recv_timeout(FIXTURE_TIMEOUT).unwrap();
    }

    #[test]
    fn dropping_streaming_task_closes_socket_clears_slot_and_allows_nonstreaming_retry() {
        let _serial = WORKER_TESTS.lock().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = endpoint(&listener);
        let handle = std::thread::spawn(move || {
            let mut socket = accept(&listener);
            read_request(&mut socket);
            sse_headers(&mut socket);
            write_chunk(&mut socket, sse_delta("uncommitted", None).as_bytes());
            assert_socket_closed(&mut socket);
        });
        let (waking, wake) = mpsc::channel();
        let task = ChatTask::spawn_streaming(endpoint, "fixture-model".into(), history(1), move || { let _ = waking.send(()); }).unwrap();
        wake.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        let partial = task.partial.as_ref().unwrap().clone();
        drop(task);
        assert_eq!(partial.take(), None);
        wake.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        assert_eq!(partial.take(), None);
        handle.join().unwrap();
        let (endpoint, _, handle) = server("200 OK", "", completion("fresh answer"));
        let (waking, wake) = mpsc::channel();
        let next = ChatTask::spawn(endpoint, "fixture-model".into(), history(1), move || { let _ = waking.send(()); }).unwrap();
        wake.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        assert_eq!(next.take_partial(), None);
        assert_eq!(next.poll(), Some(Ok("fresh answer".into())));
        assert_eq!(next.poll(), None);
        handle.join().unwrap();
    }

    fn reply(body: &str) -> String {
        format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len())
    }

    /// One connection per reply, in order; each request is reported as read.
    fn serve_each(replies: Vec<String>) -> (TcpListener, LocalEndpoint, mpsc::Receiver<String>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = endpoint(&listener);
        let serving = listener.try_clone().unwrap();
        let (sender, receiver) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            for reply in replies {
                let mut socket = accept(&serving);
                let _ = sender.send(read_request(&mut socket));
                let _ = socket.write_all(reply.as_bytes());
            }
        });
        (listener, endpoint, receiver, handle)
    }

    const LISTED: &str = r#"{"data":[{"id":"other-model"},{"id":"fixture-model"}]}"#;

    #[tokio::test]
    async fn checked_prompts_read_the_model_list_first_and_send_one_bounded_prompt() {
        let (_listener, endpoint, requests, handle) = serve_each(vec![reply(LISTED), reply(&completion("Short title"))]);
        let client = LocalClient::new(endpoint).unwrap();
        let messages = vec![Message::user("Suggest a title for: Tab")];
        assert_eq!(client.checked("fixture-model", &messages, 48, &Cancellation::default()).await, Ok("Short title".into()));
        let list = requests.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        assert!(list.starts_with("GET /v1/models HTTP/1.1\r\n") && list.ends_with("\r\n\r\n"), "the check carries no prompt: {list}");
        let request = requests.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        let (headers, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        for forbidden in ["authorization:", "cookie:", "x-api-key:"] { assert!(!headers.to_ascii_lowercase().contains(forbidden)); }
        let payload: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(payload, serde_json::json!({"model":"fixture-model","messages":messages,"stream":false,"max_tokens":48}));
        join_fixture(handle).await;
    }

    #[tokio::test]
    async fn checked_prompts_send_nothing_when_the_saved_model_is_not_listed_or_the_check_fails() {
        for (status, body, expected) in [
            ("200 OK", r#"{"data":[{"id":"other-model"}]}"#, LocalError::InvalidModel),
            ("200 OK", "not json", LocalError::InvalidResponse),
            ("503 Service Unavailable", "", LocalError::Http(503)),
        ] {
            let (listener, endpoint, requests, handle) = serve_each(vec![format!("HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len())]);
            let client = LocalClient::new(endpoint).unwrap();
            assert_eq!(client.checked("fixture-model", &[Message::user("tab titles")], 48, &Cancellation::default()).await, Err(expected));
            assert!(requests.recv_timeout(FIXTURE_TIMEOUT).unwrap().starts_with("GET /v1/models "));
            join_fixture(handle).await;
            listener.set_nonblocking(true).unwrap();
            assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock, "no prompt follows a failed check");
        }
    }

    #[tokio::test]
    async fn checked_prompts_time_out_or_cancel_during_the_check_or_the_answer() {
        for during_answer in [false, true] {
            for cancelled in [false, true] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let client = LocalClient::new(endpoint(&listener)).unwrap().with_timeout(Duration::from_millis(300));
                let cancel = Cancellation::default();
                let stop = cancel.clone();
                let handle = std::thread::spawn(move || {
                    let mut socket = accept(&listener);
                    read_request(&mut socket);
                    if during_answer {
                        socket.write_all(reply(LISTED).as_bytes()).unwrap();
                        drop(socket);
                        socket = accept(&listener);
                        read_request(&mut socket);
                    }
                    if cancelled { stop.cancel(); }
                    assert_socket_closed(&mut socket);
                });
                let result = client.checked("fixture-model", &[Message::user("tab titles")], 48, &cancel).await;
                assert_eq!(result, Err(if cancelled { LocalError::Cancelled } else { LocalError::Timeout }), "answer phase: {during_answer}");
                join_fixture(handle).await;
            }
        }
    }

    #[test]
    fn checked_tasks_share_the_single_worker_and_validate_before_admission() {
        let _serial = WORKER_TESTS.lock().unwrap();
        let (_listener, endpoint, requests, handle) = serve_each(vec![reply(LISTED), reply(&completion("Reading"))]);
        assert!(matches!(ChatTask::spawn_checked(endpoint, "bad\nmodel".into(), "x".into(), 48, || {}), Err(LocalError::InvalidModel)));
        assert!(matches!(ChatTask::spawn_checked(endpoint, "fixture-model".into(), " ".into(), 48, || {}), Err(LocalError::InvalidConversation)));
        assert!(matches!(ChatTask::spawn_checked(LocalEndpoint { port: 0, ..endpoint }, "fixture-model".into(), "x".into(), 48, || {}), Err(LocalError::InvalidPort)));
        let (waking, wake) = mpsc::channel();
        let task = ChatTask::spawn_checked(endpoint, "fixture-model".into(), "Group these tabs".into(), 768, move || { let _ = waking.send(()); }).unwrap();
        assert!(matches!(ChatTask::spawn(endpoint, "fixture-model".into(), history(1), || {}), Err(LocalError::Busy)), "Ask AI waits for the same worker");
        assert!(matches!(ChatTask::spawn_checked(endpoint, "fixture-model".into(), "x".into(), 48, || {}), Err(LocalError::Busy)));
        wake.recv_timeout(FIXTURE_TIMEOUT).unwrap();
        assert_eq!(task.poll(), Some(Ok("Reading".into())));
        assert_eq!(task.poll(), None);
        assert!(requests.recv_timeout(FIXTURE_TIMEOUT).unwrap().starts_with("GET /v1/models "));
        assert!(requests.recv_timeout(FIXTURE_TIMEOUT).unwrap().contains(r#""max_tokens":768"#));
        handle.join().unwrap();
    }
}
