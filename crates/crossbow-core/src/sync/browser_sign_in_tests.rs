use super::*;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener as StdListener, TcpStream};
use std::sync::{Mutex, mpsc};
use std::time::Instant;
#[allow(dead_code)] // Shared fixture; this file needs only plain replies.
#[path = "test_server.rs"]
mod test_server;
use test_server::{Reply, Server};

/// Tests that bind the registered ports take turns.
static REGISTERED_PORTS: Mutex<()> = Mutex::new(());

fn config(issuer: &str) -> ClerkConfig {
    ClerkConfig { issuer: issuer.into(), client_id: "fixtureClient001".into(), authorize_endpoint: format!("{issuer}/oauth/authorize") }
}

/// Three ports nothing listens on right now.
fn free_ports() -> [u16; 3] {
    let held: Vec<StdListener> = (0..3).map(|_| StdListener::bind("127.0.0.1:0").unwrap()).collect();
    let ports: Vec<u16> = held.iter().map(|l| l.local_addr().unwrap().port()).collect();
    [ports[0], ports[1], ports[2]]
}

fn port_of(listener: &Listener) -> u16 {
    listener.redirect_uri().trim_start_matches("http://127.0.0.1:").trim_end_matches("/callback").parse().unwrap()
}

/// One browser request; the status line and the whole answer.
fn get(port: u16, target: &str) -> (String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(stream, "GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUser-Agent: fixture\r\n\r\n").unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    (answer.lines().next().unwrap_or_default().to_string(), answer)
}

fn closed(port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(500)).is_err()
}

/// Wait on a listener at a fresh port; outcomes arrive on the receiver.
fn waiting(state: &str, timeout: Duration) -> (u16, Waiting, mpsc::Receiver<Outcome>) {
    let listener = bind_on(&free_ports()).unwrap();
    let port = port_of(&listener);
    let (tx, rx) = mpsc::channel();
    let waiting = listener.wait(state.into(), timeout, move |outcome| { let _ = tx.send(outcome); }).unwrap();
    (port, waiting, rx)
}

fn outcome(rx: &mpsc::Receiver<Outcome>) -> Outcome {
    rx.recv_timeout(Duration::from_secs(10)).expect("an outcome")
}

#[test]
fn pkce_is_s256_and_each_attempt_has_fresh_url_safe_secrets() {
    // RFC 7636 appendix B.
    assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    let a = Attempt::new().unwrap();
    let b = Attempt::new().unwrap();
    let url_safe = |s: &str| s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_');
    for attempt in [&a, &b] {
        assert_eq!(attempt.verifier.len(), 43, "32 random bytes");
        assert!(attempt.state.len() >= 32 && attempt.nonce.len() >= 32, "24 random bytes each");
        assert!(url_safe(&attempt.verifier) && url_safe(&attempt.state) && url_safe(&attempt.nonce));
        assert_eq!(attempt.challenge(), challenge(&attempt.verifier));
    }
    assert_ne!(a.verifier, b.verifier);
    assert_ne!(a.state, b.state);
    assert_ne!(a.nonce, b.nonce);
    assert!(a.verifier != a.state && a.state != a.nonce, "independent values");
}

#[test]
fn the_authorize_url_asks_for_a_code_with_s256_for_the_exact_redirect() {
    let attempt = Attempt::new().unwrap();
    let redirect = "http://127.0.0.1:47232/callback";
    let url = url::Url::parse(&authorize_url(&config("https://fixture-instance-0.clerk.accounts.dev"), redirect, &attempt).unwrap()).unwrap();
    assert_eq!(url.as_str().split('?').next().unwrap(), "https://fixture-instance-0.clerk.accounts.dev/oauth/authorize");
    let mut pairs: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    pairs.sort();
    let mut expected = vec![
        ("client_id".to_string(), "fixtureClient001".to_string()),
        ("code_challenge".into(), attempt.challenge()),
        ("code_challenge_method".into(), "S256".into()),
        ("nonce".into(), attempt.nonce.clone()),
        ("redirect_uri".into(), redirect.into()),
        ("response_type".into(), "code".into()),
        ("scope".into(), "openid email profile".into()),
        ("state".into(), attempt.state.clone()),
    ];
    expected.sort();
    assert_eq!(pairs, expected, "exactly these parameters: no verifier, no offline_access");
    assert!(!url.as_str().contains(&attempt.verifier), "the verifier never leaves this device in the URL");

    // A loopback issuer (a test fixture) may be plain HTTP; anything else is refused.
    assert!(authorize_url(&config("http://127.0.0.1:9"), redirect, &attempt).is_ok());
    for refused in [
        config("http://issuer.example"),
        ClerkConfig { authorize_endpoint: "https://elsewhere.example/oauth/authorize".into(), ..config("https://issuer.example") },
        ClerkConfig { authorize_endpoint: "https://user:pass@issuer.example/oauth/authorize".into(), ..config("https://issuer.example") },
        ClerkConfig { authorize_endpoint: "https://issuer.example/oauth/authorize?prompt=none".into(), ..config("https://issuer.example") },
        ClerkConfig { client_id: "a b".into(), ..config("https://issuer.example") },
        ClerkConfig { client_id: String::new(), ..config("https://issuer.example") },
        config("not a url"),
    ] {
        assert!(authorize_url(&refused, redirect, &attempt).is_err(), "{refused:?}");
    }
    assert!(authorize_url(&config("https://issuer.example"), "http://127.0.0.1:1/callback", &attempt).is_err(), "only a registered redirect");
}

#[test]
fn the_listener_takes_the_first_free_registered_port_on_127_0_0_1() {
    assert_eq!(PORTS, [47231, 47232, 47233]);
    let _turn = REGISTERED_PORTS.lock().unwrap_or_else(|e| e.into_inner());
    let first = bind().expect("a registered port");
    assert_eq!(first.redirect_uri(), "http://127.0.0.1:47231/callback", "47231 first");
    assert_eq!(first.listener.local_addr().unwrap(), SocketAddr::from(([127, 0, 0, 1], 47231)), "loopback only");
    // 47231 is held: the next registered port, with its exact redirect.
    let second = bind().expect("the next port");
    assert_eq!(second.redirect_uri(), "http://127.0.0.1:47232/callback");
    let third = bind().expect("the last port");
    assert_eq!(third.redirect_uri(), "http://127.0.0.1:47233/callback");
    match bind() {
        Err(BindError::Busy) => {}
        other => panic!("all three busy must say so, got {:?}", other.map(|l| l.redirect_uri().to_string())),
    }
    assert!(BindError::Busy.to_string().contains("47231"), "{}", BindError::Busy);
    drop((first, second, third));
}

#[test]
fn a_callback_with_the_state_returns_the_code_answers_once_and_closes() {
    let (port, _waiting, rx) = waiting("state-fixture-0123456789", Duration::from_secs(30));
    let (status, page) = get(port, "/callback?code=code_fixture&state=state-fixture-0123456789");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(page.contains(DONE_PAGE), "{page}");
    let headers = page.split("\r\n\r\n").next().unwrap().to_ascii_lowercase();
    assert!(headers.contains("cache-control: no-store") && headers.contains("connection: close") && headers.contains("referrer-policy: no-referrer"), "{headers}");
    assert_eq!(outcome(&rx), Outcome::Code("code_fixture".into()));
    assert!(closed(port), "one request: the listener is gone");
}

#[test]
fn a_state_mismatch_or_a_missing_state_ends_the_wait_without_a_code() {
    for query in ["code=c&state=someone-else", "code=c", "code=c&state="] {
        let (port, _waiting, rx) = waiting("state-fixture-0123456789", Duration::from_secs(30));
        let (status, page) = get(port, &format!("/callback?{query}"));
        assert_eq!(status, "HTTP/1.1 200 OK", "{query}");
        assert!(page.contains(DONE_PAGE));
        assert_eq!(outcome(&rx), Outcome::StateMismatch, "{query}");
        assert!(closed(port));
    }
}

#[test]
fn an_error_parameter_is_a_cancel_or_a_failure_and_never_a_code() {
    let cases = [
        ("error=access_denied&state=S", Outcome::Denied),
        ("error=access_denied&code=c&state=S", Outcome::Denied),
        ("error=server_error&error_description=Ignore%20this%3Cb%3E&state=S", Outcome::Failed("server_error".into())),
        ("error=%3Cscript%3E&state=S", Outcome::Failed("unknown error".into())),
        ("state=S", Outcome::Failed("no code".into())),
        ("code=a&code=b&state=S", Outcome::Failed("malformed answer".into())),
        ("code=a&state=S&state=S", Outcome::Failed("malformed answer".into())),
    ];
    for (query, expected) in cases {
        let (port, _waiting, rx) = waiting("S", Duration::from_secs(30));
        get(port, &format!("/callback?{query}"));
        assert_eq!(outcome(&rx), expected, "{query}");
        assert!(closed(port));
    }
}

#[test]
fn other_paths_and_methods_are_refused_and_the_wait_goes_on() {
    let (port, _waiting, rx) = waiting("S", Duration::from_secs(30));
    assert_eq!(get(port, "/favicon.ico").0, "HTTP/1.1 404 Not Found");
    assert_eq!(get(port, "/callbackx?code=c&state=S").0, "HTTP/1.1 404 Not Found");
    let mut post = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(post, "POST /callback?code=c&state=S HTTP/1.1\r\nContent-Length: 0\r\n\r\n").unwrap();
    let mut answer = String::new();
    post.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 405"), "{answer}");
    assert!(rx.try_recv().is_err(), "nothing decided yet");
    get(port, "/callback?code=late&state=S");
    assert_eq!(outcome(&rx), Outcome::Code("late".into()));
}

#[test]
fn a_stalled_or_oversized_request_cannot_hold_the_listener() {
    let (port, _waiting, rx) = waiting("S", Duration::from_secs(30));
    let _stalled = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut huge = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let _ = huge.write_all(format!("GET /callback?code=c&state=S&pad={} HTTP/1.1\r\n", "x".repeat(16 * 1024)).as_bytes());
    let started = Instant::now();
    get(port, "/callback?code=quick&state=S");
    assert_eq!(outcome(&rx), Outcome::Code("quick".into()));
    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
}

#[test]
fn the_wait_times_out_and_closes() {
    let (port, _waiting, rx) = waiting("S", Duration::from_millis(300));
    assert_eq!(outcome(&rx), Outcome::TimedOut);
    assert!(closed(port));
}

#[test]
fn cancel_closes_the_listener_at_once() {
    let (port, waiting, rx) = waiting("S", Duration::from_secs(300));
    let started = Instant::now();
    waiting.cancel();
    assert!(closed(port), "closed when cancel returns");
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
    assert_eq!(outcome(&rx), Outcome::Cancelled);
    // Dropping the handle cancels as well.
    let (port, waiting, rx) = self::waiting("S", Duration::from_secs(300));
    drop(waiting);
    assert!(closed(port));
    assert_eq!(outcome(&rx), Outcome::Cancelled);
}

#[test]
fn the_server_check_reads_the_clerk_configuration_from_health() {
    let with = Server::start(|_| Reply::json(200, r#"{"ok":true,"protocol":1,"service":"crossbow-api","version":"0.1.0","clerk":{"issuer":"https://issuer.example","client_id":"client","authorize_endpoint":"https://issuer.example/oauth/authorize"}}"#));
    let without = Server::start(|_| Reply::json(200, r#"{"ok":true,"protocol":1,"service":"crossbow-api","version":"0.1.0"}"#));
    let broken = Server::start(|_| Reply::json(500, r#"{"error":{"code":"internal","message":"internal error"}}"#));
    let check = |url: &str| {
        let (tx, rx) = mpsc::channel();
        check_server(url.to_string(), move |result| { let _ = tx.send(result); });
        rx.recv_timeout(Duration::from_secs(20)).unwrap()
    };
    assert_eq!(check(&with.url).unwrap().map(|c| c.client_id), Some("client".into()));
    assert_eq!(check(&without.url).unwrap(), None, "no clerk: not configured");
    assert!(check(&broken.url).unwrap_err().starts_with("internal"));
    let gone = { let l = StdListener::bind("127.0.0.1:0").unwrap(); format!("http://{}", l.local_addr().unwrap()) };
    assert!(check(&gone).unwrap_err().starts_with("network:"));
    assert_eq!(with.requests.lock().unwrap()[0].path, "/v1/health");
}

#[test]
fn manage_account_opens_the_account_portal_clerk_derives_from_the_issuer() {
    let portal = |issuer: &str| account_portal_url(&config(issuer));
    // A Clerk-hosted development instance.
    assert_eq!(portal("https://fixture-instance-0.clerk.accounts.dev").as_deref(), Some("https://fixture-instance-0.accounts.dev/user"));
    // A production instance on its own domain, with or without a final slash.
    assert_eq!(portal("https://clerk.example.com").as_deref(), Some("https://accounts.example.com/user"));
    assert_eq!(portal("https://clerk.example.com/").as_deref(), Some("https://accounts.example.com/user"));
    // Anything else has no portal Crossbow can name, so no control.
    for issuer in [
        "http://127.0.0.1:47000",
        "http://clerk.example.com",
        "https://clerk.example.com:8443",
        "https://clerk.example.com/tenant",
        "https://auth.example.com",
        "https://myclerk.example.com",
        "https://example.clerk",
        "https://clerk.accounts",
        "not a url",
    ] {
        assert_eq!(portal(issuer), None, "{issuer}");
    }
}
