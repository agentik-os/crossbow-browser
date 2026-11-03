//! Delete Account in the sync worker: the step-up code, the delete, and
//! the readback that alone confirms it.
use super::*;
use std::sync::Mutex;

const TOKEN: &str = "fixture-token";

fn request() -> DeleteAccountRequest {
    DeleteAccountRequest { confirm: "ada@example.com".into(), step_up: StepUp::EmailCode { challenge_id: "chl_fixture".into(), code: "123456".into() } }
}

fn events(incoming: &std_mpsc::Receiver<(u64, SyncEvent)>) -> Vec<SyncEvent> {
    incoming.try_iter().map(|(_, event)| event).collect()
}

fn refused(status: u16, code: &str) -> Reply {
    Reply::json(status, serde_json::json!({ "error": { "code": code, "message": "fixture" } }).to_string())
}

/// A service whose `DELETE /v1/account` answers `delete`; after a delete it
/// answered 200 to, the session is gone, as on the Worker.
fn service(delete: impl Fn() -> Reply + Send + Sync + 'static) -> (Server, Arc<Mutex<bool>>) {
    let deleted = Arc::new(Mutex::new(false));
    let gone = deleted.clone();
    let server = Server::start(move |request| match (request.method.as_str(), request.path.as_str()) {
        ("DELETE", "/v1/account") => {
            let reply = delete();
            if reply.status == 200 { *gone.lock().unwrap() = true; }
            reply
        }
        ("GET", "/v1/me") if *gone.lock().unwrap() => refused(401, "unauthorized"),
        ("GET", "/v1/me") => Reply::json(200, r#"{"account":{"id":"acc_a","email":"ada@example.com","created_at":1},"device":{"id":"dev_a","name":"Mac","platform":"macos","created_at":1,"last_seen_at":1},"devices":[]}"#),
        (method, path) => panic!("unexpected fixture request: {method} {path}"),
    });
    (server, deleted)
}

fn paths(server: &Server) -> Vec<String> {
    server.requests.lock().unwrap().iter().map(|r| format!("{} {}", r.method, r.path)).collect()
}

#[tokio::test]
async fn a_step_up_code_goes_to_the_accounts_email_without_a_session() {
    let server = Server::start(|request| match request.path.as_str() {
        "/v1/auth/start" => Reply::json(200, r#"{"challenge_id":"chl_fixture","expires_in":600,"dev_code":"123456"}"#),
        path => panic!("unexpected fixture request: {path}"),
    });
    let (worker, incoming) = worker_at(&server.url);
    worker.step_up_code(7, &server.url, "ada@example.com").await;
    let seen = events(&incoming);
    assert!(matches!(seen.as_slice(), [SyncEvent::StepUpCode { attempt: 7, challenge_id, dev_code: Some(code) }] if challenge_id == "chl_fixture" && code == "123456"), "{seen:?}");
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].authorization, None, "a sign-in code needs no session");
    assert_eq!(serde_json::from_str::<AuthStartRequest>(&requests[0].body).unwrap().email, "ada@example.com");
    assert!(worker.live.is_some(), "asking for a code leaves live sync alone");

    // Refused, as when too many codes went to this email: said, with its attempt.
    let server = Server::start(|_| refused(429, "rate_limited"));
    worker.step_up_code(8, &server.url, "ada@example.com").await;
    let seen = events(&incoming);
    assert!(matches!(seen.as_slice(), [SyncEvent::DeleteAccountFailed { attempt: 8, error }] if error == "rate_limited: fixture"), "{seen:?}");
}

#[tokio::test]
async fn a_step_up_code_start_carries_the_dev_key_as_a_sign_in_start_does() {
    let server = Server::start(|request| match request.path.as_str() {
        "/v1/auth/start" => Reply::json(200, r#"{"challenge_id":"chl_fixture","expires_in":600,"dev_code":"123456"}"#),
        path => panic!("unexpected fixture request: {path}"),
    });
    let (mut worker, incoming) = worker_at(&server.url);
    worker.dev_key = DevKey::from_vars(Some(server.url.clone()), Some("fixture-dev-key".into()));
    worker.step_up_code(7, &server.url, "ada@example.com").await;
    assert!(matches!(events(&incoming).as_slice(), [SyncEvent::StepUpCode { attempt: 7, .. }]));
    worker.dev_key = DevKey::from_vars(Some("https://sync.example.com".into()), Some("fixture-dev-key".into()));
    worker.step_up_code(8, &server.url, "ada@example.com").await;
    let sent: Vec<_> = server.requests.lock().unwrap().iter().map(|r| r.dev_key.clone()).collect();
    assert_eq!(sent, vec![Some("fixture-dev-key".to_string()), None], "only to the key's own server");
}

#[tokio::test]
async fn only_a_delete_the_readback_confirms_is_counted_and_it_ends_live_sync() {
    let (server, deleted) = service(|| Reply::json(200, r#"{"deleted_rows":3}"#));
    let (mut worker, incoming) = worker_at(&server.url);
    worker.delete_account(3, &server.url, TOKEN, &request()).await;
    let seen = events(&incoming);
    assert!(matches!(seen.as_slice(), [SyncEvent::AccountDeleted { attempt: 3 }]), "{seen:?}");
    assert!(*deleted.lock().unwrap());
    assert_eq!(paths(&server), ["DELETE /v1/account", "GET /v1/me"], "the delete, then a readback with the same session");
    let requests = server.requests.lock().unwrap().clone();
    assert!(requests.iter().all(|r| r.authorization.as_deref() == Some("Bearer fixture-token")));
    assert_eq!(serde_json::from_str::<DeleteAccountRequest>(&requests[0].body).unwrap(), request(), "the typed email and the step-up, as given");
    // This device's live sync used the deleted session: it stops, with no
    // "session rejected" sign-out on top.
    assert!(worker.live.is_none());
    worker.pull_now().await;
    assert_eq!(paths(&server).len(), 2, "nothing more goes to the server");
    assert!(events(&incoming).is_empty(), "{:?}", events(&incoming));
}

#[tokio::test]
async fn a_delete_the_readback_doesnt_confirm_changes_nothing() {
    // The session still works after a 200: not deleted, as far as anyone can tell.
    let server = Server::start(|request| match request.path.as_str() {
        "/v1/account" => Reply::json(200, r#"{"deleted_rows":0}"#),
        "/v1/me" => Reply::json(200, r#"{"account":{"id":"acc_a","email":"ada@example.com","created_at":1},"device":{"id":"dev_a","name":"Mac","platform":"macos","created_at":1,"last_seen_at":1},"devices":[]}"#),
        path => panic!("unexpected fixture request: {path}"),
    });
    let (mut worker, incoming) = worker_at(&server.url);
    worker.delete_account(1, &server.url, TOKEN, &request()).await;
    let seen = events(&incoming);
    assert!(matches!(seen.as_slice(), [SyncEvent::DeleteAccountFailed { attempt: 1, error }] if error.starts_with("unconfirmed: ")), "{seen:?}");
    assert!(worker.live.is_some());

    // The readback's connection drops.
    let server = Server::start(|request| match request.path.as_str() {
        "/v1/account" => Reply::json(200, r#"{"deleted_rows":0}"#),
        _ => Reply::close(),
    });
    worker.delete_account(2, &server.url, TOKEN, &request()).await;
    let seen = events(&incoming);
    assert!(matches!(seen.as_slice(), [SyncEvent::DeleteAccountFailed { attempt: 2, error }] if error.starts_with("unconfirmed: ")), "{seen:?}");
    assert!(worker.live.is_some());
}

#[tokio::test]
async fn a_refused_delete_keeps_its_reason_and_reads_nothing_back() {
    let cases: [(Reply, &str); 6] = [
        (refused(403, "invalid_code"), "invalid_code: fixture"),
        (refused(403, "code_expired"), "code_expired: fixture"),
        (refused(400, "confirmation_mismatch"), "confirmation_mismatch: fixture"),
        // A Worker from before Delete Account doesn't know the route.
        (refused(404, "not_found"), "unsupported: http 404"),
        // The session ended, perhaps by an earlier delete whose answer was lost.
        (refused(401, "unauthorized"), "unauthorized: session rejected"),
        (refused(500, "internal"), "internal: fixture"),
    ];
    for (reply, expected) in cases {
        let reply = Arc::new(Mutex::new(Some(reply)));
        let server = Server::start(move |request| match request.path.as_str() {
            "/v1/account" => reply.lock().unwrap().take().unwrap(),
            path => panic!("{expected}: unexpected fixture request: {path}"),
        });
        let (mut worker, incoming) = worker_at(&server.url);
        worker.delete_account(4, &server.url, TOKEN, &request()).await;
        let seen = events(&incoming);
        assert!(matches!(seen.as_slice(), [SyncEvent::DeleteAccountFailed { attempt: 4, error }] if error == expected), "{expected}: {seen:?}");
        assert!(worker.live.is_some(), "{expected}: live sync is left as it was");
    }
    let server = Server::start(|_| Reply::close());
    let (mut worker, incoming) = worker_at(&server.url);
    worker.delete_account(5, &server.url, TOKEN, &request()).await;
    let seen = events(&incoming);
    assert!(matches!(seen.as_slice(), [SyncEvent::DeleteAccountFailed { attempt: 5, error }] if error.starts_with("network: ")), "{seen:?}");
}

#[test]
fn delete_commands_reach_the_worker_without_ending_the_session() {
    let (server, _) = service(|| Reply::json(200, r#"{"deleted_rows":0}"#));
    let handle = SyncHandle::spawn(Arc::new(|| {}));
    let before = handle.generation.load(Ordering::SeqCst);
    handle.send(SyncCommand::DeleteAccount { attempt: 9, base_url: server.url.clone(), token: TOKEN.into(), request: request() });
    assert_eq!(handle.generation.load(Ordering::SeqCst), before, "a delete supersedes nothing in progress");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(event) = handle.try_recv() {
            assert!(matches!(event, SyncEvent::AccountDeleted { attempt: 9 }), "{event:?}");
            break;
        }
        assert!(std::time::Instant::now() < deadline, "no answer");
        std::thread::sleep(Duration::from_millis(2));
    }
}
