use super::*;
use crate::library::{Library, SYNC_CURSOR_KEY};
use crate::store::{EntityRow, Store};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn sign_in_error(body: String) -> String {
    let server = Server::start(move |request| {
        assert_eq!(request.path, "/v1/auth/verify");
        Reply::json(401, body.clone())
    });
    let handle = SyncHandle::spawn(Arc::new(|| {}));
    handle.send(SyncCommand::VerifyAuth {
        base_url: server.url.clone(), challenge_id: "fixture-challenge".into(),
        code: "000000".into(), device_name: "Synthetic fixture".into(), platform: "macos".into(),
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(event) = handle.try_recv() {
            return match event {
                SyncEvent::AuthError(error) => error,
                other => panic!("unexpected sign-in event: {other:?}"),
            };
        }
        assert!(Instant::now() < deadline, "missing sign-in error");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn wrong_code_http_401_preserves_the_code_error() {
    assert_eq!(sign_in_error(r#"{"error":{"code":"invalid_code","message":"the code is not valid for this sign-in"}}"#.into()),
        "invalid_code: the code is not valid for this sign-in");
}

#[test]
fn expired_code_http_401_preserves_the_recovery_error() {
    assert_eq!(sign_in_error(r#"{"error":{"code":"code_expired","message":"the code has expired; start again"}}"#.into()),
        "code_expired: the code has expired; start again");
}

#[test]
fn sign_in_error_body_is_bounded_and_malformed_is_not_a_revoked_session() {
    assert_eq!(sign_in_error("x".repeat(64 * 1024 + 1)), "bad response: error body too large");
    let malformed = sign_in_error("not json".into());
    assert!(malformed.starts_with("http 401"), "{malformed}");
    assert!(!unauthorized(&malformed));
}

fn remote(seq: u64, id: &str) -> ServerChange {
    ServerChange { seq, device_id: "other-fixture".into(), change: change(id) }
}

fn take_page(incoming: &std_mpsc::Receiver<(u64, SyncEvent)>) -> (u64, Vec<ServerChange>, u64) {
    match incoming.try_recv().expect("missing pull event").1 {
        SyncEvent::Pulled { batch_id, changes, cursor, .. } => (batch_id, changes, cursor),
        event => panic!("expected page, received {event:?}"),
    }
}

#[tokio::test]
async fn sqlite_failures_retry_the_same_page_then_persist_every_record() {
    // Failure after one entity, failure at HLC persistence, both cursor-write
    // forms, and deferred constraint failure at COMMIT exercise real SQLite.
    for fault in ["entity", "clock", "cursor_insert", "cursor_update", "commit"] {
        let server = Server::start(|request| {
            let (changes, cursor, more) = if request.path.contains("since=0&") {
                (vec![remote(1, "first"), remote(2, "second")], 2, true)
            } else if request.path.contains("since=2&") {
                (vec![remote(3, "third")], 3, false)
            } else { panic!("skipped page: {}", request.path) };
            Reply::json(200, serde_json::to_string(&PullResponse { changes, cursor, more, sync_epoch: 0 }).unwrap())
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        let local = lib.create_profile("Preserved local profile").unwrap();
        if fault != "cursor_insert" { lib.store.kv_set(SYNC_CURSOR_KEY, "0").unwrap(); }
        let before = lib.store.all_entities().unwrap();
        let clock = lib.store.kv_get("hlc_last").unwrap();
        let cursor = lib.store.kv_get(SYNC_CURSOR_KEY).unwrap();
        let revision = lib.revision;
        let injector = rusqlite::Connection::open(&path).unwrap();
        let sql = match fault {
            "entity" => "CREATE TRIGGER fixture_failure BEFORE INSERT ON entities WHEN NEW.id = 'second' BEGIN SELECT RAISE(FAIL, 'fixture entity failure'); END;",
            "clock" => "CREATE TRIGGER fixture_failure BEFORE INSERT ON kv WHEN NEW.k = 'hlc_last' BEGIN SELECT RAISE(FAIL, 'fixture clock failure'); END;",
            "cursor_insert" => "CREATE TRIGGER fixture_failure BEFORE INSERT ON kv WHEN NEW.k = 'sync_cursor' BEGIN SELECT RAISE(FAIL, 'fixture cursor insert failure'); END;",
            "cursor_update" => "CREATE TRIGGER fixture_failure BEFORE UPDATE ON kv WHEN NEW.k = 'sync_cursor' BEGIN SELECT RAISE(FAIL, 'fixture cursor update failure'); END;",
            "commit" => "CREATE TABLE fixture_parent (id INTEGER PRIMARY KEY); CREATE TABLE fixture_deferred (id INTEGER REFERENCES fixture_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER fixture_failure AFTER INSERT ON entities WHEN NEW.id = 'second' BEGIN INSERT INTO fixture_deferred VALUES (99); END;",
            _ => unreachable!(),
        };
        injector.execute_batch(sql).unwrap();
        let (mut worker, incoming) = worker_at(&server.url);
        worker.pull_all().await;
        let (batch, changes, end) = take_page(&incoming);
        worker.pull_all().await; // A notification while applying coalesces.
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        worker.acknowledge_pull(batch + 1, true).await; // Not this page.
        assert_eq!(worker.live.as_ref().unwrap().cursor, 0);
        assert!(lib.apply_sync_page(&changes, end).is_err(), "missing {fault} failure");
        worker.acknowledge_pull(batch, false).await;
        assert_eq!(worker.live.as_ref().unwrap().cursor, 0, "advanced on {fault}");
        assert!(worker.pull_retry_at.is_some(), "failed page needs a retry");
        assert_eq!(lib.store.all_entities().unwrap(), before, "partial {fault} write");
        assert_eq!(lib.store.kv_get(SYNC_CURSOR_KEY).unwrap(), cursor);
        assert_eq!(lib.store.kv_get("hlc_last").unwrap(), clock);
        assert_eq!(lib.revision, revision);
        assert_eq!(lib.profiles.len(), 1);
        drop(lib);
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        assert_eq!(lib.store.all_entities().unwrap(), before);
        assert_eq!(lib.store.kv_get(SYNC_CURSOR_KEY).unwrap(), cursor);
        injector.execute_batch("DROP TRIGGER fixture_failure;").unwrap();
        worker.pull_all().await;
        let (retry, changes, end) = take_page(&incoming);
        assert_ne!(batch, retry);
        worker.acknowledge_pull(batch, true).await; // Late success cannot ack retry.
        assert_eq!(worker.live.as_ref().unwrap().cursor, 0);
        assert_eq!(lib.apply_sync_page(&changes, end).unwrap(), 2);
        worker.acknowledge_pull(retry, true).await;
        assert_eq!(worker.live.as_ref().unwrap().cursor, 2);
        let (last, changes, end) = take_page(&incoming);
        assert_eq!(lib.apply_sync_page(&changes, end).unwrap(), 1);
        worker.acknowledge_pull(last, true).await;
        assert_eq!(worker.live.as_ref().unwrap().cursor, 3);
        assert!(worker.pending_pull.is_none());
        let paths: Vec<_> = server.requests.lock().unwrap().iter().map(|r| r.path.clone()).collect();
        assert_eq!(paths.len(), 3);
        assert!(paths[0].contains("since=0&") && paths[1].contains("since=0&") && paths[2].contains("since=2&"));
        drop(lib);
        let lib = Library::open(Store::open(&path).unwrap()).unwrap();
        assert_eq!(lib.store.kv_get(SYNC_CURSOR_KEY).unwrap().as_deref(), Some("3"));
        assert_eq!(lib.profiles.len(), 4);
        for id in [local.as_str(), "first", "second", "third"] { assert!(lib.profiles.contains_key(id)); }
        assert_eq!(lib.store.dirty_count().unwrap(), 1, "local profile must stay dirty");
    }
}

fn put(store: &Store, change: &Change) {
    store.put_entity(&EntityRow { kind: change.kind.clone(), id: change.id.clone(), hlc: change.hlc.clone(), deleted: change.deleted, payload: change.payload.clone() }, true).unwrap();
}

fn persist_push_events(store: &Store, incoming: &std_mpsc::Receiver<(u64, SyncEvent)>) {
    for (_, event) in incoming.try_iter() {
        match event {
            SyncEvent::Pushed { accepted, .. } => for change in accepted { store.mark_clean(&change.kind, &change.id, &change.hlc).unwrap(); },
            SyncEvent::PushRejected { change, reason } => store.reject_sync_change(&change, &reason).unwrap(),
            SyncEvent::PushFinished => {}
            other => panic!("unexpected push event: {other:?}"),
        }
    }
}

#[tokio::test]
async fn rejected_pages_do_not_starve_valid_rows_and_survive_reopening() {
    let server = Server::start(|request| {
        let push: PushRequest = serde_json::from_str(&request.body).unwrap();
        assert!(push.changes.iter().all(|c| c.validate().is_ok()));
        Reply::json(200, serde_json::to_string(&PushResponse { accepted: push.changes.len() as u32, stale: vec![], cursor: 1 }).unwrap())
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outbox.sqlite");
    let store = Store::open(&path).unwrap();
    for i in 0..=MAX_PUSH_CHANGES {
        let mut bad = change(&format!("invalid-{i:04}"));
        bad.kind = "InvalidKind".into();
        put(&store, &bad);
    }
    let valid = change("valid");
    put(&store, &valid);
    let (mut worker, incoming) = worker_at(&server.url);
    for _ in 0..2 {
        let page = store.sync_changes(MAX_PUSH_CHANGES).unwrap();
        assert!(!page.is_empty());
        worker.push(page).await;
        persist_push_events(&store, &incoming);
    }
    assert!(store.sync_changes(MAX_PUSH_CHANGES).unwrap().is_empty());
    assert_eq!(server.requests.lock().unwrap().len(), 1, "locally invalid data must never be sent");
    assert_eq!(store.dirty_count().unwrap(), (MAX_PUSH_CHANGES + 1) as u64);
    let before = store.all_entities().unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.all_entities().unwrap(), before);
    assert_eq!(store.dirty_changes(MAX_PUSH_CHANGES + 10).unwrap().len(), MAX_PUSH_CHANGES + 1);
    assert!(store.sync_changes(MAX_PUSH_CHANGES).unwrap().is_empty());
    assert!(store.sync_rejection_reason().unwrap().unwrap().contains("kept locally"));
    store.retry_sync_rejections().unwrap();
    assert_eq!(store.sync_changes(MAX_PUSH_CHANGES).unwrap().len(), MAX_PUSH_CHANGES);
}

#[test]
fn rejection_is_version_specific_and_editing_reenables_sync() {
    let store = Store::open_in_memory().unwrap();
    let mut oversized = change("edit-me");
    oversized.payload = "x".repeat(MAX_PAYLOAD_BYTES + 1);
    put(&store, &oversized);
    store.reject_sync_change(&oversized, invalid_outgoing(&oversized).unwrap()).unwrap();
    assert!(store.sync_changes(1).unwrap().is_empty());
    assert_eq!(store.all_entities().unwrap()[0].payload, oversized.payload);
    let mut fixed = change("edit-me");
    fixed.hlc = Hlc::new(200, 0, "fixture");
    put(&store, &fixed);
    store.reject_sync_change(&oversized, "late rejection").unwrap();
    assert_eq!(store.sync_changes(1).unwrap(), vec![fixed]);
    assert!(store.sync_rejection_reason().unwrap().is_none());
    assert_eq!(store.dirty_count().unwrap(), 1);
}

#[tokio::test]
async fn server_batch_rejection_isolates_only_the_rejected_singleton() {
    let server = Server::start(|request| {
        let input: PushRequest = serde_json::from_str(&request.body).unwrap();
        if input.changes.iter().any(|c| c.id == "server-rejected") {
            Reply::json(400, r#"{"error":{"code":"bad_request","message":"fixture server validation"}}"#)
        } else { Reply::json(200, r#"{"accepted":1,"stale":[],"cursor":1}"#) }
    });
    let store = Store::open_in_memory().unwrap();
    for id in ["server-rejected", "valid"] { put(&store, &change(id)); }
    let (mut worker, incoming) = worker_at(&server.url);
    worker.push(store.sync_changes(MAX_PUSH_CHANGES).unwrap()).await;
    persist_push_events(&store, &incoming);
    assert_eq!(server.requests.lock().unwrap().len(), 3);
    assert!(store.sync_changes(10).unwrap().is_empty());
    assert_eq!(store.dirty_changes(10).unwrap(), vec![change("server-rejected")]);
    assert_eq!(store.all_entities().unwrap().len(), 2);
}

#[tokio::test]
async fn server_validation_work_has_a_request_budget() {
    let server = Server::start(|_| Reply::json(413, "not JSON"));
    let (mut worker, incoming) = worker_at(&server.url);
    worker.push((0..64).map(|n| change(&format!("id-{n}"))).collect()).await;
    assert_eq!(server.requests.lock().unwrap().len(), MAX_PUSH_REQUESTS);
    let events: Vec<_> = incoming.try_iter().map(|(_, e)| e).collect();
    assert!(events.iter().any(|e| matches!(e, SyncEvent::PushFailed(_))));
    assert!(!events.iter().any(|e| matches!(e, SyncEvent::Pushed { .. })));
    assert!(matches!(events.last(), Some(SyncEvent::PushFinished)));
}

#[tokio::test]
async fn transient_errors_and_incomplete_acknowledgements_never_quarantine_or_clean() {
    for (status, body) in [(503, "unavailable"), (200, r#"{"accepted":0,"stale":[],"cursor":42}"#)] {
        let server = Server::start(move |_| Reply::json(status, body));
        let (mut worker, incoming) = worker_at(&server.url);
        worker.push(vec![change("keep-dirty")]).await;
        let events: Vec<_> = incoming.try_iter().map(|(_, e)| e).collect();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], SyncEvent::PushFailed(_)));
        assert!(matches!(&events[1], SyncEvent::PushFinished));
        assert_eq!(worker.live.as_ref().unwrap().cursor, 0, "push cursor cannot acknowledge a pull");
    }
}

fn me() -> String {
    serde_json::to_string(&MeResponse {
        account: Account { id: "fixture-account".into(), email: "fixture@example.com".into(), created_at: 1 },
        device: Device { id: "fixture-device".into(), name: "Fixture".into(), platform: "macos".into(), created_at: 1, last_seen_at: 1 },
        devices: vec![],
        browser_sign_in: false,
    }).unwrap()
}

fn standard(request: &test_server::Request) -> Reply {
    match request.path.as_str() {
        "/v1/me" => Reply::json(200, me()),
        "/v1/auth/start" => Reply::json(200, r#"{"challenge_id":"retry-fixture","expires_in":600,"dev_code":"123456"}"#),
        "/v1/auth/logout" | "/v1/sync/ws" => Reply::stall(),
        path if path.starts_with("/v1/sync/pull?") => Reply::json(200, r#"{"changes":[],"cursor":0,"more":false}"#),
        path => panic!("unexpected fixture path: {path}"),
    }
}

fn wait_event(handle: &SyncHandle, timeout: Duration, predicate: impl Fn(&SyncEvent) -> bool) -> SyncEvent {
    let deadline = Instant::now() + timeout;
    loop {
        while let Ok(event) = handle.try_recv() {
            if predicate(&event) { return event; }
        }
        assert!(Instant::now() < deadline, "worker event timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn resume(handle: &SyncHandle, url: &str) {
    handle.send(SyncCommand::Resume { base_url: url.into(), token: "fixture-token".into(), cursor: 0, sync_epoch: Some(0) });
}

#[test]
fn signout_and_new_auth_interrupt_stalled_websocket_and_logout() {
    let server = Server::start(standard);
    let handle = SyncHandle::spawn(Arc::new(|| {}));
    resume(&handle, &server.url);
    server.wait_for("/v1/sync/ws", 1);
    // Even commands queued just before sign-out must not transfer anything.
    handle.send(SyncCommand::Push(vec![change("must-not-transfer")]));
    handle.send(SyncCommand::PullNow);
    handle.send(SyncCommand::SignOut);
    wait_event(&handle, Duration::from_millis(750), |e| matches!(e, SyncEvent::SignedOut));
    server.wait_for("/v1/auth/logout", 1); // remote logout itself stays stalled
    handle.send(SyncCommand::StartAuth { base_url: server.url.clone(), email: "retry@example.com".into() });
    wait_event(&handle, Duration::from_millis(750), |e| matches!(e, SyncEvent::AuthStarted { .. }));
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.iter().filter(|r| r.path.starts_with("/v1/sync/pull?")).count(), 1);
    assert!(!requests.iter().any(|r| r.path == "/v1/sync/push"));
}

#[test]
fn cancel_and_retry_interrupt_stalled_http_operations() {
    for stalled in ["/v1/me", "/v1/sync/pull?", "/v1/sync/push", "/v1/auth/start"] {
        let stalled_once = Arc::new(AtomicBool::new(false));
        let seen = stalled_once.clone();
        let server = Server::start(move |request| {
            if request.path.starts_with(stalled) && !seen.swap(true, Ordering::SeqCst) { Reply::stall() }
            else if request.path == "/v1/sync/ws" { Reply::json(503, "fixture socket unavailable") }
            else { standard(request) }
        });
        let handle = SyncHandle::spawn(Arc::new(|| {}));
        if stalled == "/v1/auth/start" {
            handle.send(SyncCommand::StartAuth { base_url: server.url.clone(), email: "first@example.com".into() });
        } else {
            resume(&handle, &server.url);
            if stalled == "/v1/sync/push" {
                handle.send(SyncCommand::Push(vec![change("fixture")]));
            }
        }
        server.wait_for(stalled, 1);
        handle.send(SyncCommand::CancelAuth);
        handle.send(SyncCommand::StartAuth { base_url: server.url.clone(), email: "retry@example.com".into() });
        wait_event(&handle, Duration::from_millis(750), |e| matches!(e, SyncEvent::AuthStarted { .. }));
    }
}

#[test]
fn canceled_verification_revokes_late_session_without_starting_transfer() {
    let gate = Arc::new(AtomicBool::new(false));
    let gate_for_server = gate.clone();
    let server = Server::start(move |request| {
        if request.path == "/v1/auth/verify" {
            let me: MeResponse = serde_json::from_str(&me()).unwrap();
            Reply { gate: Some(gate_for_server.clone()), ..Reply::json(200, serde_json::to_string(&AuthVerifyResponse { session_token: "late-fixture-token".into(), account: me.account, device: me.device, display_name: None }).unwrap()) }
        } else { standard(request) }
    });
    let handle = SyncHandle::spawn(Arc::new(|| {}));
    handle.send(SyncCommand::VerifyAuth { base_url: server.url.clone(), challenge_id: "fixture".into(), code: "123456".into(), device_name: "Fixture".into(), platform: "macos".into() });
    server.wait_for("/v1/auth/verify", 1);
    handle.send(SyncCommand::CancelAuth);
    handle.send(SyncCommand::StartAuth { base_url: server.url.clone(), email: "retry@example.com".into() });
    wait_event(&handle, Duration::from_millis(750), |e| matches!(e, SyncEvent::AuthStarted { .. }));
    gate.store(true, Ordering::SeqCst);
    server.wait_for("/v1/auth/logout", 1);
    handle.send(SyncCommand::PullNow);
    handle.send(SyncCommand::Push(vec![change("not-authorized")]));
    wait_event(&handle, Duration::from_millis(750), |e| matches!(e, SyncEvent::PushFinished));
    assert!(!server.requests.lock().unwrap().iter().any(|r| r.path == "/v1/me" || r.path.starts_with("/v1/sync/")));
    while let Ok(event) = handle.try_recv() { assert!(!matches!(event, SyncEvent::SignedIn(_))); }
}

#[test]
fn verified_but_unpersisted_session_cannot_transfer() {
    let server = Server::start(|request| {
        if request.path == "/v1/auth/verify" {
            let me: MeResponse = serde_json::from_str(&me()).unwrap();
            Reply::json(200, serde_json::to_string(&AuthVerifyResponse { session_token: "unpersisted-fixture-token".into(), account: me.account, device: me.device, display_name: None }).unwrap())
        } else { standard(request) }
    });
    let handle = SyncHandle::spawn(Arc::new(|| {}));
    handle.send(SyncCommand::VerifyAuth { base_url: server.url.clone(), challenge_id: "fixture".into(), code: "123456".into(), device_name: "Fixture".into(), platform: "macos".into() });
    wait_event(&handle, Duration::from_secs(5), |e| matches!(e, SyncEvent::SignedIn(_)));
    handle.send(SyncCommand::PullNow);
    handle.send(SyncCommand::Push(vec![change("unpersisted")]));
    wait_event(&handle, Duration::from_millis(750), |e| matches!(e, SyncEvent::PushFinished));
    handle.send(SyncCommand::CancelAuth);
    server.wait_for("/v1/auth/logout", 1);
    assert!(!server.requests.lock().unwrap().iter().any(|r| r.path == "/v1/me" || r.path.starts_with("/v1/sync/")));
}

#[tokio::test]
async fn raw_http_401_on_push_pull_and_me_invalidates_queued_events() {
    for route in ["push", "pull", "me"] {
        let server = Server::start(|_| Reply::json(401, "not a JSON error body"));
        let (mut worker, incoming) = worker_at(&server.url);
        worker.emit(SyncEvent::Pulled { batch_id: 999, changes: vec![remote(9, "old-account")], cursor: 9, more: false });
        match route {
            "push" => worker.push(vec![change("test")]).await,
            "pull" => worker.pull_all().await,
            "me" => { worker.refresh_me().await; }
            _ => unreachable!(),
        }
        assert!(worker.live.is_none());
        let generation = worker.current_generation.clone();
        let (tx, _rx) = mpsc::unbounded_channel();
        let handle = SyncHandle { tx, events: incoming, generation, invalidated: worker.invalidated.clone() };
        let events: Vec<_> = std::iter::from_fn(|| handle.try_recv().ok()).collect();
        assert!(events.iter().any(|e| matches!(e, SyncEvent::SignedOut)));
        assert!(!events.iter().any(|e| matches!(e, SyncEvent::Pulled { .. } | SyncEvent::Pushed { .. })));
        worker.pull_all().await;
        worker.push(vec![change("after-revocation")]).await;
        assert_eq!(server.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn failed_page_is_retried_by_the_running_worker_without_sync_now() {
    let server = Server::start(|request| {
        if request.path == "/v1/sync/ws" { Reply::json(503, "fixture socket unavailable") }
        else if request.path.starts_with("/v1/sync/pull?") {
            assert!(request.path.contains("since=0&"), "retry skipped unapplied page");
            Reply::json(200, serde_json::to_string(&PullResponse { changes: vec![remote(1, "retry")], cursor: 1, more: false, sync_epoch: 0 }).unwrap())
        } else { standard(request) }
    });
    let handle = SyncHandle::spawn(Arc::new(|| {}));
    resume(&handle, &server.url);
    let page = wait_event(&handle, Duration::from_secs(5), |e| matches!(e, SyncEvent::Pulled { .. }));
    let SyncEvent::Pulled { batch_id, .. } = page else { unreachable!() };
    handle.send(SyncCommand::PullApplied { batch_id, success: false });
    let started = Instant::now();
    let retry = wait_event(&handle, Duration::from_secs(3), |e| matches!(e, SyncEvent::Pulled { .. }));
    assert!(started.elapsed() >= Duration::from_millis(900), "failure retry must back off");
    assert!(matches!(retry, SyncEvent::Pulled { batch_id: retry_id, cursor: 1, .. } if retry_id != batch_id));
    assert_eq!(server.requests.lock().unwrap().iter().filter(|r| r.path.starts_with("/v1/sync/pull?")).count(), 2);
}

#[test]
fn websocket_http_401_revokes_and_stops_transfer() {
    let server = Server::start(|request| {
        if request.path == "/v1/sync/ws" { Reply::json(401, "fixture revoked without JSON") }
        else { standard(request) }
    });
    let handle = SyncHandle::spawn(Arc::new(|| {}));
    resume(&handle, &server.url);
    wait_event(&handle, Duration::from_secs(5), |e| matches!(e, SyncEvent::SignedOut));
    handle.send(SyncCommand::PullNow);
    handle.send(SyncCommand::Push(vec![change("not-authorized")]));
    wait_event(&handle, Duration::from_millis(750), |e| matches!(e, SyncEvent::PushFinished));
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.iter().filter(|r| r.path.starts_with("/v1/sync/pull?")).count(), 1);
    assert!(!requests.iter().any(|r| r.path == "/v1/sync/push"));
}

/// The account fixtures keep the socket open, so these two tests are the
/// ones that refuse and drop it. Each records when the fixture saw every
/// upgrade request.
fn socket_server(refusals: usize) -> (Server, Arc<std::sync::Mutex<Vec<Instant>>>) {
    let attempts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = attempts.clone();
    let server = Server::start(move |request| {
        if request.path != "/v1/sync/ws" { return standard(request); }
        let mut seen = seen.lock().unwrap();
        seen.push(Instant::now());
        if seen.len() <= refusals { Reply::json(503, "fixture socket refused") } else { Reply::socket() }
    });
    (server, attempts)
}

/// Resume, and apply the first pull so the next one isn't coalesced into it.
fn resume_applied(handle: &SyncHandle, url: &str) {
    resume(handle, url);
    let SyncEvent::Pulled { batch_id, .. } = wait_event(handle, Duration::from_secs(5), |e| matches!(e, SyncEvent::Pulled { .. })) else { unreachable!() };
    handle.send(SyncCommand::PullApplied { batch_id, success: true });
}

/// The service greets every accepted socket with its cursor; a newer one
/// than the device's brings the catch-up pull.
fn hello_brings_a_pull(server: &Server, handle: &SyncHandle) {
    let pulls = || server.requests.lock().unwrap().iter().filter(|r| r.path.starts_with("/v1/sync/pull?")).count();
    let before = pulls();
    let hello = serde_json::to_string(&ServerEvent::Hello { cursor: 1 }).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.notify(&hello) == 0 {
        assert!(Instant::now() < deadline, "no open socket for the hello");
        std::thread::sleep(Duration::from_millis(5));
    }
    wait_event(handle, Duration::from_secs(5), |e| matches!(e, SyncEvent::Pulled { .. }));
    assert_eq!(pulls(), before + 1, "the hello brought one catch-up pull");
}

#[test]
fn a_refused_socket_reports_offline_and_retries_further_apart_until_one_is_accepted() {
    let (server, attempts) = socket_server(2);
    let handle = SyncHandle::spawn(Arc::new(|| {}));
    resume_applied(&handle, &server.url);
    for refusal in 1..=2 {
        let offline = wait_event(&handle, Duration::from_secs(10), |e| matches!(e, SyncEvent::Status(SyncStatus::Offline(_)) | SyncEvent::Status(SyncStatus::Live)));
        assert!(matches!(&offline, SyncEvent::Status(SyncStatus::Offline(error)) if error.contains("503")), "refusal {refusal} reported {offline:?}");
    }
    wait_event(&handle, Duration::from_secs(10), |e| matches!(e, SyncEvent::Status(SyncStatus::Live)));
    let attempts = attempts.lock().unwrap().clone();
    assert_eq!(attempts.len(), 3, "two refused upgrades, then the accepted one");
    assert!(attempts[1] - attempts[0] >= Duration::from_millis(990), "the first retry came {:?} after the refusal", attempts[1] - attempts[0]);
    assert!(attempts[2] - attempts[1] >= Duration::from_millis(1990), "the second retry came {:?} after the refusal", attempts[2] - attempts[1]);
    hello_brings_a_pull(&server, &handle);
}

#[test]
fn a_socket_the_service_drops_reports_offline_and_reconnects() {
    let (server, attempts) = socket_server(0);
    let handle = SyncHandle::spawn(Arc::new(|| {}));
    resume_applied(&handle, &server.url);
    wait_event(&handle, Duration::from_secs(5), |e| matches!(e, SyncEvent::Status(SyncStatus::Live)));
    let deadline = Instant::now() + Duration::from_secs(5);
    let dropped_at = loop {
        let at = Instant::now();
        if server.drop_sockets() > 0 { break at; }
        assert!(Instant::now() < deadline, "no open socket to drop");
        std::thread::sleep(Duration::from_millis(5));
    };
    let lost = wait_event(&handle, Duration::from_secs(5), |e| matches!(e, SyncEvent::Status(_)));
    assert!(matches!(&lost, SyncEvent::Status(SyncStatus::Offline(error)) if error == "connection lost"), "the drop reported {lost:?}");
    wait_event(&handle, Duration::from_secs(10), |e| matches!(e, SyncEvent::Status(SyncStatus::Live)));
    let attempts = attempts.lock().unwrap().clone();
    assert_eq!(attempts.len(), 2, "one reconnect");
    assert!(attempts[1] - dropped_at >= Duration::from_millis(990), "the reconnect came {:?} after the drop", attempts[1] - dropped_at);
    hello_brings_a_pull(&server, &handle);
}

#[test]
fn shutdown_interrupts_stalled_websocket() {
    let server = Server::start(standard);
    let handle = SyncHandle::spawn(Arc::new(|| {}));
    resume(&handle, &server.url);
    server.wait_for("/v1/sync/ws", 1);
    handle.send(SyncCommand::Shutdown);
    let deadline = Instant::now() + Duration::from_millis(750);
    loop {
        if matches!(handle.try_recv(), Err(std_mpsc::TryRecvError::Disconnected)) { break; }
        assert!(Instant::now() < deadline, "shutdown blocked by WebSocket upgrade");
        std::thread::sleep(Duration::from_millis(2));
    }
}
