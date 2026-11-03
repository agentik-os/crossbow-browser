//! Search-engine suggestions: which typed text may be sent, one request to an
//! OpenSearch suggestion endpoint, and a worker that runs them one at a time.
//! Neither the typed text nor the request URL is ever logged; reqwest errors
//! carry the URL, so they are reduced to a kind before leaving this module.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

/// Suggestions shown for one request.
pub const MAX_SUGGESTIONS: usize = 4;
/// Longer replies are refused, not truncated.
pub const MAX_BODY: usize = 64 * 1024;
/// Covers connecting, the reply and its body.
pub const TIMEOUT: Duration = Duration::from_secs(2);
/// Longer text is pasted, not typed as a query.
pub const MAX_TEXT: usize = 100;

/// Words that mark credentials or keys anywhere in the text.
const SECRET_MARKERS: [&str; 16] = [
    "password", "passwd", "passcode", "secret", "token", "apikey", "api_key", "api-key", "bearer ", "authorization",
    "private key", "-----begin", "ssh-rsa", "ssh-ed25519", "ssh-dss", "ecdsa-sha2",
];

/// Which never-send rule refuses a text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Address,
    Path,
    Secret,
}

/// Why `text` must stay on this Mac, whatever its length: see [`may_send`].
pub fn refusal(text: &str) -> Option<Refusal> {
    let text = text.trim();
    let lower = text.to_lowercase();
    if crate::search::input_to_url(text).is_some() || lower.contains("://") || lower.starts_with("www.") || scheme_like(&lower) {
        return Some(Refusal::Address);
    }
    if text.starts_with(['/', '~']) || text.starts_with("./") || text.starts_with("../") || text.contains('\\') {
        return Some(Refusal::Path);
    }
    let secret = SECRET_MARKERS.iter().any(|marker| lower.contains(marker)) || text.contains(['@', '='])
        || text.chars().filter(char::is_ascii_digit).count() >= 8 || text.split_whitespace().any(key_like);
    secret.then_some(Refusal::Secret)
}

/// Whether `text` may be sent to a search engine for suggestions. Refused:
/// web addresses and scheme-like text (`mailto:x`, `c:/`), local paths, text
/// with a credential word, `@` or `=`, eight or more digits, a word that looks
/// like a key, and anything over [`MAX_TEXT`] characters.
pub fn may_send(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty() && text.chars().count() <= MAX_TEXT && refusal(text).is_none()
}

/// `word:rest` with no space after the colon, as in `mailto:me` or `c:/temp`.
fn scheme_like(lower: &str) -> bool {
    let Some((scheme, rest)) = lower.split_once(':') else { return false };
    scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        && rest.starts_with(|c: char| !c.is_whitespace())
}

/// A word of 16 or more characters with a digit or mixed case, or any word of
/// 32 or more: the shape of keys, tokens and one-time links.
fn key_like(word: &str) -> bool {
    let length = word.chars().count();
    let digit = word.chars().any(|c| c.is_ascii_digit());
    let mixed = word.chars().filter(|c| c.is_uppercase()).count() >= 2 && word.chars().filter(|c| c.is_lowercase()).count() >= 2;
    length >= 32 || (length >= 16 && (digit || mixed))
}

/// The suggestions in an OpenSearch reply: `["text", ["one", "two"], …]`.
/// Blank, overlong, repeated and control-character entries are skipped.
pub fn parse(body: &[u8]) -> Option<Vec<String>> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let mut found: Vec<String> = Vec::new();
    for entry in value.as_array()?.get(1)?.as_array()? {
        let Some(text) = entry.as_str().map(str::trim) else { continue };
        if text.is_empty() || text.chars().count() > 200 || text.chars().any(char::is_control) { continue; }
        if found.iter().any(|seen| seen.to_lowercase() == text.to_lowercase()) { continue; }
        found.push(text.to_string());
        if found.len() == MAX_SUGGESTIONS { break; }
    }
    Some(found)
}

/// Why a request produced nothing. None of these carries the URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchError {
    /// Offline, refused or reset.
    Network,
    Timeout,
    Status(u16),
    TooLarge,
    Invalid,
}

/// The client for suggestion requests: no cookies, no retries, and redirects
/// only within the same host and scheme.
pub fn client() -> reqwest::Result<reqwest::Client> {
    builder().build()
}

fn builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(concat!("Crossbow/", env!("CARGO_PKG_VERSION")))
        .retry(reqwest::retry::never())
        .connect_timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let first = attempt.previous().first();
            let same = first.is_some_and(|first| first.host_str() == attempt.url().host_str() && first.scheme() == attempt.url().scheme());
            if same && attempt.previous().len() <= 3 { attempt.follow() } else { attempt.stop() }
        }))
}

/// Ask `url` for suggestions, giving up after `timeout`.
pub async fn fetch(client: &reqwest::Client, url: &str, timeout: Duration) -> Result<Vec<String>, FetchError> {
    let request = async {
        let mut response = client.get(url)
            .header(reqwest::header::ACCEPT, "application/x-suggestions+json, application/json;q=0.9")
            .send().await.map_err(kind)?;
        if !response.status().is_success() { return Err(FetchError::Status(response.status().as_u16())); }
        if response.content_length().is_some_and(|length| length > MAX_BODY as u64) { return Err(FetchError::TooLarge); }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(kind)? {
            if body.len() + chunk.len() > MAX_BODY { return Err(FetchError::TooLarge); }
            body.extend_from_slice(&chunk);
        }
        parse(&body).ok_or(FetchError::Invalid)
    };
    tokio::time::timeout(timeout, request).await.map_err(|_| FetchError::Timeout)?
}

fn kind(error: reqwest::Error) -> FetchError {
    if error.is_timeout() { FetchError::Timeout } else { FetchError::Network }
}

/// One finished request.
#[derive(Debug, PartialEq, Eq)]
pub struct Reply {
    pub seq: u64,
    pub result: Result<Vec<String>, FetchError>,
}

/// A thread that runs requests one after another and calls `wake` after each
/// reply. Dropping it ends the thread once a running request finishes.
pub struct Worker {
    requests: Sender<(u64, String)>,
    replies: Receiver<Reply>,
}

impl Worker {
    pub fn spawn(wake: impl Fn() + Send + 'static) -> std::io::Result<Worker> {
        Self::start(client, TIMEOUT, wake)
    }

    fn start(client: impl FnOnce() -> reqwest::Result<reqwest::Client> + Send + 'static, timeout: Duration, wake: impl Fn() + Send + 'static) -> std::io::Result<Worker> {
        let (requests, queue) = mpsc::channel::<(u64, String)>();
        let (answer, replies) = mpsc::channel();
        std::thread::Builder::new().name("crossbow-suggest".into()).spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().ok();
            let client = client().ok();
            while let Ok((seq, url)) = queue.recv() {
                let result = match (&runtime, &client) {
                    (Some(runtime), Some(client)) => runtime.block_on(fetch(client, &url, timeout)),
                    _ => Err(FetchError::Network),
                };
                if answer.send(Reply { seq, result }).is_err() { break; }
                wake();
            }
        })?;
        Ok(Worker { requests, replies })
    }

    pub fn send(&self, seq: u64, url: String) {
        let _ = self.requests.send((seq, url));
    }

    pub fn try_recv(&self) -> Option<Reply> {
        self.replies.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn addresses_paths_and_secret_looking_text_are_never_sent() {
        for text in [
            "rust lang", "how to reset a router", "weather in lisbon", "c++ templates", "état de l'art", "iphone 15 pro",
            "note: buy milk", "state-of-the-art", "2026 calendar",
        ] {
            assert!(may_send(text), "{text:?} may be sent");
        }
        for text in [
            "", "   ",
            // Addresses.
            "github.com", "https://example.com/a b", "localhost:3000", "192.168.1.1", "www.example", "see http://x y", "mailto:me", "about:blank", "javascript:alert(1)",
            // Local paths.
            "/Users/me/notes.txt", "~/Documents", "./build.sh", "../secrets", "C:\\Users\\me", "c:/temp", "folder\\file",
            // Credentials and personal data.
            "my password is hunter2", "Passwd reset", "client secret", "token abc", "api_key 123", "apikey", "Bearer abc", "-----BEGIN PRIVATE KEY-----", "ssh-ed25519 AAAA",
            "user=admin", "me@example.com", "4111 1111 1111 1111", "+1 415 555 0100",
            // Token-shaped samples (documentation examples) are joined at run time, so
            // no literal in this file matches a provider's secret-scanning pattern.
            &["ghp", "_16C7e42F292c6912E7710c838347Ae178B4a"].concat(), &["sk-", "proj-abc123def456"].concat(),
            &["AKIA", "IOSFODNN7EXAMPLE"].concat(), &["eyJ", "hbGciOiJIUzI1NiJ9"].concat(), "MixedCaseTokenValue", &"a".repeat(32),
            // Long text is pasted, not a query.
            &"word ".repeat(21),
        ] {
            assert!(!may_send(text), "{text:?} is never sent");
        }
    }

    #[test]
    fn refusals_name_the_rule_and_do_not_depend_on_length() {
        for (text, rule) in [
            ("github.com/rust-lang", Refusal::Address), ("https://example.com/a b", Refusal::Address), ("mailto:me", Refusal::Address), ("localhost:3000", Refusal::Address),
            ("/Users/me/notes.txt", Refusal::Path), ("~/Documents", Refusal::Path), ("../secrets", Refusal::Path), ("folder\\file", Refusal::Path),
            ("my password is hunter2", Refusal::Secret), ("me@example.com", Refusal::Secret), ("user=admin", Refusal::Secret), ("4111 1111 1111 1111", Refusal::Secret), (&*["sk-", "proj-abc123def456"].concat(), Refusal::Secret),
        ] {
            assert_eq!(refusal(text), Some(rule), "{text:?}");
            assert!(!may_send(text));
        }
        for text in ["", "rust lang", "explain the borrow checker in two lines", "état de l'art"] {
            assert_eq!(refusal(text), None, "{text:?}");
        }
        let long = "why is the sky blue ".repeat(10);
        assert_eq!(refusal(&long), None, "a long question breaks no rule");
        assert!(!may_send(&long), "but it is too long for suggestions");
    }

    #[test]
    fn opensearch_replies_give_at_most_four_clean_suggestions() {
        assert_eq!(parse(br#"["rust",["rust lang","rustup"]]"#), Some(vec!["rust lang".into(), "rustup".into()]));
        assert_eq!(parse(br#"["rust",["a","b","c","d","e"],[],[]]"#), Some(vec!["a".into(), "b".into(), "c".into(), "d".into()]));
        assert_eq!(parse(br#"["rust",[" Rust ","rust",7,"","line\nbreak","ok"]]"#), Some(vec!["Rust".into(), "ok".into()]));
        assert_eq!(parse(br#"["rust",[]]"#), Some(vec![]));
        let long = format!(r#"["q",["{}","fine"]]"#, "x".repeat(201));
        assert_eq!(parse(long.as_bytes()), Some(vec!["fine".into()]));
        for body in [&b"not json"[..], br#"{"q":["a"]}"#, br#"["rust"]"#, br#"["rust","a"]"#, b"\xff\xfe"] {
            assert_eq!(parse(body), None, "{}", String::from_utf8_lossy(body));
        }
    }

    /// A loopback engine for one connection: what it received, after it replies.
    struct Engine {
        port: u16,
        request: Receiver<String>,
        release: Sender<()>,
        thread: std::thread::JoinHandle<()>,
    }

    impl Engine {
        /// Replies with `reply` once released, or straight away when `hold` is false.
        fn start(reply: String, hold: bool) -> Engine {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (sender, request) = mpsc::channel();
            let (release, released) = mpsc::channel::<()>();
            let thread = std::thread::spawn(move || {
                let mut socket = accept(&listener);
                socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut head = Vec::new();
                let mut buf = [0; 4096];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    let count = socket.read(&mut buf).unwrap();
                    if count == 0 { break; }
                    head.extend_from_slice(&buf[..count]);
                }
                let _ = sender.send(String::from_utf8(head).unwrap());
                if hold { let _ = released.recv(); }
                let _ = socket.write_all(reply.as_bytes());
            });
            Engine { port, request, release, thread }
        }

        fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{path}", self.port)
        }

        fn finish(self) {
            let _ = self.release.send(());
            self.thread.join().unwrap();
        }
    }

    fn accept(listener: &TcpListener) -> TcpStream {
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

    fn http(status: &str, headers: &str, body: &str) -> String {
        format!("HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}", body.len())
    }

    /// The production client, kept off any proxy so only loopback is reached.
    fn loopback() -> reqwest::Client {
        builder().no_proxy().build().unwrap()
    }

    fn fetch_now(url: &str, timeout: Duration) -> Result<Vec<String>, FetchError> {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(fetch(&loopback(), url, timeout))
    }

    #[test]
    fn a_loopback_engine_answers_with_suggestions_and_no_cookies() {
        let engine = Engine::start(http("200 OK", "Content-Type: application/x-suggestions+json\r\n", r#"["rust lang",["rust language","rust lang book"]]"#), false);
        let url = engine.url("/ac?q=rust%20lang&type=list");
        assert_eq!(fetch_now(&url, TIMEOUT), Ok(vec!["rust language".into(), "rust lang book".into()]));
        let request = engine.request.recv().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /ac?q=rust%20lang&type=list http/1.1\r\n"), "{request}");
        assert!(request.contains("\r\naccept: application/x-suggestions+json"), "{request}");
        assert!(!request.contains("\r\ncookie:") && !request.contains("\r\nauthorization:"), "{request}");
        engine.finish();
    }

    #[test]
    fn failures_come_back_as_a_kind_without_the_url() {
        let cases = [
            (http("500 Internal Server Error", "", "oops"), FetchError::Status(500)),
            (http("200 OK", "", "<html>not suggestions</html>"), FetchError::Invalid),
            (http("200 OK", "", &format!(r#"["q",["{}"]]"#, "x".repeat(MAX_BODY))), FetchError::TooLarge),
            // Redirects that leave the host are not followed.
            (http("302 Found", "Location: http://localhost:9/ac?q=rust\r\n", ""), FetchError::Status(302)),
        ];
        for (reply, expected) in cases {
            let engine = Engine::start(reply, false);
            assert_eq!(fetch_now(&engine.url("/ac?q=rust"), TIMEOUT), Err(expected));
            engine.finish();
        }
        // A body longer than its announced length is refused too.
        let engine = Engine::start(format!("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n[\"q\",[\"{}\"]]", "y".repeat(MAX_BODY)), false);
        assert_eq!(fetch_now(&engine.url("/ac?q=rust"), TIMEOUT), Err(FetchError::TooLarge));
        engine.finish();

        let engine = Engine::start(http("200 OK", "", r#"["q",["late"]]"#), true);
        assert_eq!(fetch_now(&engine.url("/ac?q=rust"), Duration::from_millis(200)), Err(FetchError::Timeout));
        engine.finish();

        // Nothing listens: offline.
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        assert_eq!(fetch_now(&format!("http://127.0.0.1:{port}/ac?q=rust"), TIMEOUT), Err(FetchError::Network));
    }

    #[test]
    fn the_worker_answers_each_request_in_order_and_wakes_the_app() {
        let first = Engine::start(http("200 OK", "", r#"["a",["alpha"]]"#), true);
        let second = Engine::start(http("503 Service Unavailable", "", ""), false);
        let (woke, wakes) = mpsc::channel();
        let worker = Worker::start(|| builder().no_proxy().build(), TIMEOUT, move || { let _ = woke.send(()); }).unwrap();
        worker.send(1, first.url("/s?q=a"));
        worker.send(2, second.url("/s?q=ab"));
        first.request.recv_timeout(Duration::from_secs(3)).unwrap();
        // The second waits until the first is answered: one request at a time.
        assert!(second.request.recv_timeout(Duration::from_millis(200)).is_err());
        assert_eq!(worker.try_recv(), None);
        first.finish();
        wakes.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(worker.try_recv(), Some(Reply { seq: 1, result: Ok(vec!["alpha".into()]) }));
        wakes.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(worker.try_recv(), Some(Reply { seq: 2, result: Err(FetchError::Status(503)) }));
        second.finish();
    }
}
