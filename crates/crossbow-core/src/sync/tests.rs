use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
#[path = "test_server.rs"]
mod test_server;
use test_server::{Reply, Server};
#[path = "repair_tests.rs"]
mod repair;
#[path = "e2ee_sync_tests.rs"]
mod e2ee_sync;
#[path = "delete_tests.rs"]
mod delete;
#[path = "browser_sync_tests.rs"]
mod browser;
#[path = "dev_key_tests.rs"]
mod dev_key;
#[path = "account_delete_tests.rs"]
mod account_delete;
use super::test_secrets::Hidden;

/// A recovery code's text, hidden, as tests compare it.
impl From<&e2ee::RecoveryCode> for Hidden<String> {
    fn from(code: &e2ee::RecoveryCode) -> Hidden<String> {
        Hidden::new(code.as_str().to_owned())
    }
}

/// A key's bytes, hidden, as tests compare them.
impl From<&e2ee::LibraryKey> for Hidden<[u8; 32]> {
    fn from(key: &e2ee::LibraryKey) -> Hidden<[u8; 32]> {
        Hidden::new(*key.as_bytes())
    }
}

fn worker_at(url: &str) -> (Worker, std_mpsc::Receiver<(u64, SyncEvent)>) {
    let (_tx, rx) = mpsc::unbounded_channel();
    let (events, incoming) = std_mpsc::channel();
    let mut worker = Worker::new(rx, events, Arc::new(|| {}), Arc::new(AtomicU64::new(0)), Arc::new(Notify::new()));
    worker.live = Some(Live { base_url: url.into(), token: "fixture-token".into(), device_id: "fixture-device".into(), cursor: 0, ready: true, sync_epoch: Some(0) });
    (worker, incoming)
}

fn change(id: &str) -> Change {
    Change { kind: kinds::PROFILE.into(), id: id.into(), hlc: Hlc::new(100, 0, "fixture"), deleted: false, enc: Encoding::Json, payload: r#"{"name":"Remote fixture","created_ms":1}"#.into() }
}

#[tokio::test]
async fn repair_pull_keeps_cursor_when_sqlite_application_fails() {
    let remote = ServerChange { seq: 1, device_id: "other".into(), change: change("remote") };
    let page = serde_json::to_string(&PullResponse { changes: vec![remote], cursor: 1, more: false, sync_epoch: 0 }).unwrap();
    let server = Server::start(move |_| Reply::json(200, page.clone()));
    let (mut worker, incoming) = worker_at(&server.url);
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("library.sqlite");
    let mut lib = crate::library::Library::open(crate::store::Store::open(&db).unwrap()).unwrap();
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_remote BEFORE INSERT ON entities BEGIN SELECT RAISE(FAIL, 'fixture write failure'); END;").unwrap();
    worker.pull_all().await;
    let SyncEvent::Pulled { changes, .. } = incoming.try_recv().unwrap().1 else { panic!("missing page") };
    assert!(lib.apply_sync_page(&changes, 1).is_err());
    assert_eq!(lib.store.kv_get("sync_cursor").unwrap(), None);
    assert_eq!(worker.live.as_ref().unwrap().cursor, 0, "worker must await durable application, not skip the failed record");
}

#[tokio::test]
async fn repair_invalid_payload_does_not_block_valid_change() {
    let server = Server::start(|request| {
        let push: PushRequest = serde_json::from_str(&request.body).unwrap();
        if push.changes.iter().any(|c| c.validate().is_err()) {
            Reply::json(413, r#"{"error":{"code":"payload_too_large","message":"fixture invalid payload"}}"#)
        } else {
            Reply::json(200, r#"{"accepted":1,"stale":[],"cursor":1}"#)
        }
    });
    let (mut worker, incoming) = worker_at(&server.url);
    let mut invalid = change("oversized");
    invalid.payload = "x".repeat(MAX_PAYLOAD_BYTES + 1);
    worker.push(vec![invalid, change("valid")]).await;
    assert!(incoming.try_iter().any(|(_, e)| matches!(e, SyncEvent::Pushed { accepted, .. } if accepted.iter().any(|c| c.id == "valid"))), "valid record must make progress past invalid data");
}

#[tokio::test]
async fn repair_websocket_handshake_is_bounded() {
    let server = Server::start(|_| Reply::stall());
    let outcome = tokio::time::timeout(Duration::from_secs(6), connect_ws(&server.url, "fixture-token")).await;
    assert!(outcome.is_ok(), "WebSocket upgrade exceeded its own deadline");
    assert!(outcome.unwrap().is_err());
}

/// The fixture's socket passes the client's handshake check, and its accept
/// key matches RFC 6455's example. It stays open and silent until a test
/// sends an event, and forgets the socket once the client closes it.
#[tokio::test]
async fn the_fixture_socket_stays_open_and_carries_only_what_a_test_sends() {
    assert_eq!(test_server::accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    let server = Server::start(|_| Reply::socket());
    let mut ws = Some(connect_ws(&server.url, "fixture-token").await.expect("the handshake is accepted"));
    assert!(tokio::time::timeout(Duration::from_millis(1500), next_ws(&mut ws)).await.is_err(), "the socket closed or spoke");
    let changed = serde_json::to_string(&ServerEvent::Changed { cursor: 7, by_device: "another-device".into() }).unwrap();
    assert_eq!(server.notify(&changed), 1);
    let Some(Ok(tungstenite::Message::Text(text))) = tokio::time::timeout(Duration::from_secs(5), next_ws(&mut ws)).await.expect("the event arrived") else { panic!("not a text frame") };
    assert_eq!(serde_json::from_str::<ServerEvent>(&text).unwrap(), ServerEvent::Changed { cursor: 7, by_device: "another-device".into() });
    drop(ws);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while server.notify(&changed) > 0 {
        assert!(std::time::Instant::now() < deadline, "the closed socket stayed open");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn repair_pull_401_invalidates_live_session() {
    let server = Server::start(|_| Reply::json(401, r#"{"error":{"code":"unauthorized","message":"fixture revoked"}}"#));
    let (mut worker, incoming) = worker_at(&server.url);
    worker.pull_all().await;
    assert!(worker.live.is_none(), "revoked pull session must not remain eligible for transfer");
    assert!(incoming.try_iter().any(|(_, e)| matches!(e, SyncEvent::SignedOut)));
}

fn fixture_handle() -> (SyncHandle, std_mpsc::Sender<(u64, SyncEvent)>, mpsc::UnboundedReceiver<(u64, SyncCommand)>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (events, incoming) = std_mpsc::channel();
    (SyncHandle { tx, events: incoming, generation: Arc::new(AtomicU64::new(0)), invalidated: Arc::new(Notify::new()) }, events, rx)
}

#[test]
fn cancel_discards_already_queued_auth_errors_and_successes() {
    let (handle, events, _commands) = fixture_handle();
    handle.send(SyncCommand::StartAuth { base_url: "http://127.0.0.1".into(), email: "fixture@example.com".into() });
    events.send((1, SyncEvent::AuthStarted { challenge_id: "old-challenge".into(), dev_code: Some("123456".into()) })).unwrap();
    events.send((1, SyncEvent::AuthError("old error".into()))).unwrap();
    handle.send(SyncCommand::CancelAuth);
    assert!(matches!(handle.try_recv(), Err(std_mpsc::TryRecvError::Empty)));
}

#[test]
fn a_new_attempt_never_receives_the_previous_attempts_code() {
    let (handle, events, _commands) = fixture_handle();
    handle.send(SyncCommand::StartAuth { base_url: "http://127.0.0.1".into(), email: "first@example.com".into() });
    handle.send(SyncCommand::CancelAuth);
    handle.send(SyncCommand::StartAuth { base_url: "http://127.0.0.1".into(), email: "second@example.com".into() });
    events.send((1, SyncEvent::AuthStarted { challenge_id: "first".into(), dev_code: Some("111111".into()) })).unwrap();
    events.send((3, SyncEvent::AuthStarted { challenge_id: "second".into(), dev_code: Some("222222".into()) })).unwrap();
    assert!(matches!(handle.try_recv().unwrap(), SyncEvent::AuthStarted { challenge_id, .. } if challenge_id == "second"));
}

#[test]
fn sign_out_discards_queued_library_changes() {
    let (handle, events, _commands) = fixture_handle();
    events.send((0, SyncEvent::Pulled { batch_id: 1, changes: vec![], cursor: 42, more: false })).unwrap();
    handle.send(SyncCommand::SignOut);
    assert!(matches!(handle.try_recv(), Err(std_mpsc::TryRecvError::Empty)));
}

#[tokio::test]
async fn pending_credential_storage_cannot_trigger_a_pull_or_push() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let (_tx, rx) = mpsc::unbounded_channel();
    let (events, _incoming) = std_mpsc::channel();
    let mut worker = Worker::new(rx, events, Arc::new(|| {}), Arc::new(AtomicU64::new(0)), Arc::new(Notify::new()));
    worker.live = Some(Live {
        base_url: format!("http://{}", listener.local_addr().unwrap()), token: "fixture-token".into(),
        device_id: "fixture-device".into(), cursor: 0, ready: false, sync_epoch: Some(0),
    });
    worker.pull_all().await;
    worker.push(vec![change("not-ready")]).await;
    assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
}

#[test]
fn real_http_auth_response_is_discarded_after_cancel() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (received, request_seen) = std_mpsc::channel();
    let (release, wait_release) = std_mpsc::channel();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(std::time::Instant::now() < deadline, "HTTP request timed out");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("fixture server: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut request = [0; 4096];
        let count = stream.read(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request[..count]).starts_with("POST /v1/auth/start "));
        received.send(()).unwrap();
        wait_release.recv_timeout(Duration::from_secs(3)).unwrap();
        let body = r#"{"challenge_id":"late-fixture","expires_in":600,"dev_code":"123456"}"#;
        let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
    });
    let (wake, wake_events) = std_mpsc::channel();
    let handle = SyncHandle::spawn(Arc::new(move || { let _ = wake.send(()); }));
    handle.send(SyncCommand::StartAuth { base_url, email: "fixture@example.com".into() });
    request_seen.recv_timeout(Duration::from_secs(5)).unwrap();
    handle.send(SyncCommand::CancelAuth);
    release.send(()).unwrap();
    server.join().unwrap();
    // SignOut supplies a completion fence after the delayed response. It must
    // be the only visible event, never the old challenge or an old error.
    handle.send(SyncCommand::SignOut);
    wake_events.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(handle.try_recv().unwrap(), SyncEvent::SignedOut));
    while let Ok(event) = handle.try_recv() {
        assert!(matches!(event, SyncEvent::Status(SyncStatus::SignedOut)));
    }
    handle.send(SyncCommand::Shutdown);
}
