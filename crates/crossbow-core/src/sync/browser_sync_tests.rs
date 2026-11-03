//! The sync worker's side of browser sign-in: the code goes to the sync
//! service, never anywhere else, and only its session comes back.
use super::*;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn exchange() -> ClerkExchangeRequest {
    ClerkExchangeRequest {
        code: "code_fixture".into(),
        code_verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into(),
        redirect_uri: "http://127.0.0.1:47231/callback".into(),
        nonce: "nonce-fixture-0123456789".into(),
        device_name: "Fixture".into(),
        platform: "macos".into(),
    }
}

fn verified(token: &str) -> String {
    serde_json::json!({
        "session_token": token,
        "account": { "id": "acc_fixture", "email": "ada@example.com", "created_at": 1 },
        "device": { "id": "dev_fixture", "name": "Fixture", "platform": "macos", "created_at": 1, "last_seen_at": 1 },
        "display_name": "Ada Lovelace",
    })
    .to_string()
}

fn next_event(handle: &SyncHandle, predicate: impl Fn(&SyncEvent) -> bool) -> SyncEvent {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        while let Ok(event) = handle.try_recv() {
            if predicate(&event) { return event; }
        }
        assert!(Instant::now() < deadline, "worker event timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_browser_sign_in_redeems_its_code_at_the_service_and_signs_in() {
    let server = Server::start(|request| match request.path.as_str() {
        "/v1/auth/clerk" => Reply::json(200, verified("cbs_browser")),
        path => panic!("unexpected fixture request: {path}"),
    });
    let handle = SyncHandle::spawn(Arc::new(|| {}));
    handle.send(SyncCommand::BrowserSignIn { base_url: server.url.clone(), exchange: exchange() });
    let SyncEvent::SignedIn(session) = next_event(&handle, |e| matches!(e, SyncEvent::SignedIn(_) | SyncEvent::AuthError(_))) else { panic!("not signed in") };
    assert_eq!((session.token.as_str(), session.account.id.as_str(), session.display_name.as_deref()), ("cbs_browser", "acc_fixture", Some("Ada Lovelace")));
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "nothing transfers before the session is saved");
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].authorization, None, "no session yet");
    let sent: ClerkExchangeRequest = serde_json::from_str(&requests[0].body).unwrap();
    assert_eq!(sent, exchange(), "the code, verifier, redirect and nonce exactly as the browser's attempt made them");
}

#[test]
fn a_refused_browser_sign_in_reports_the_services_error() {
    for (status, body, expected) in [
        (409, r#"{"error":{"code":"link_required","message":"an account already uses this email"}}"#, "link_required: an account already uses this email"),
        (503, r#"{"error":{"code":"clerk_unavailable","message":"not here"}}"#, "clerk_unavailable: not here"),
        (401, r#"{"error":{"code":"sign_in_rejected","message":"start again"}}"#, "sign_in_rejected: start again"),
    ] {
        let server = Server::start(move |_| Reply::json(status, body));
        let handle = SyncHandle::spawn(Arc::new(|| {}));
        handle.send(SyncCommand::BrowserSignIn { base_url: server.url.clone(), exchange: exchange() });
        match next_event(&handle, |e| matches!(e, SyncEvent::SignedIn(_) | SyncEvent::AuthError(_))) {
            SyncEvent::AuthError(error) => assert_eq!(error, expected),
            other => panic!("{other:?}"),
        }
    }
    // A service that drops the connection unanswered: a network failure.
    let server = Server::start(|_| Reply::close());
    let handle = SyncHandle::spawn(Arc::new(|| {}));
    handle.send(SyncCommand::BrowserSignIn { base_url: server.url.clone(), exchange: exchange() });
    match next_event(&handle, |e| matches!(e, SyncEvent::SignedIn(_) | SyncEvent::AuthError(_))) {
        SyncEvent::AuthError(error) => assert!(error.starts_with("network: "), "{error}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_canceled_browser_sign_in_revokes_the_late_session_and_never_signs_in() {
    let gate = Arc::new(AtomicBool::new(false));
    let gate_for_server = gate.clone();
    let server = Server::start(move |request| match request.path.as_str() {
        "/v1/auth/clerk" => Reply { gate: Some(gate_for_server.clone()), ..Reply::json(200, verified("cbs_late")) },
        "/v1/auth/logout" => Reply::json(204, ""),
        "/v1/auth/start" => Reply::json(200, r#"{"challenge_id":"retry","expires_in":600,"dev_code":"123456"}"#),
        path => panic!("unexpected fixture request: {path}"),
    });
    let handle = SyncHandle::spawn(Arc::new(|| {}));
    handle.send(SyncCommand::BrowserSignIn { base_url: server.url.clone(), exchange: exchange() });
    server.wait_for("/v1/auth/clerk", 1);
    handle.send(SyncCommand::CancelAuth);
    handle.send(SyncCommand::StartAuth { base_url: server.url.clone(), email: "retry@example.com".into() });
    next_event(&handle, |e| matches!(e, SyncEvent::AuthStarted { .. }));
    gate.store(true, Ordering::SeqCst);
    server.wait_for("/v1/auth/logout", 1);
    let logout = server.requests.lock().unwrap().iter().find(|r| r.path == "/v1/auth/logout").cloned().unwrap();
    assert_eq!(logout.authorization.as_deref(), Some("Bearer cbs_late"), "the late session is the one revoked");
    while let Ok(event) = handle.try_recv() { assert!(!matches!(event, SyncEvent::SignedIn(_))); }
}

#[test]
fn linking_holds_the_identity_then_confirms_it_with_this_session() {
    let server = Server::start(|request| match request.path.as_str() {
        "/v1/auth/clerk/link" => Reply::json(200, r#"{"link_id":"lnk_fixture","email":"ada@gmail.example","expires_in":300}"#),
        "/v1/auth/clerk/link/confirm" => Reply::json(204, ""),
        path => panic!("unexpected fixture request: {path}"),
    });
    let handle = SyncHandle::spawn(Arc::new(|| {}));
    handle.send(SyncCommand::LinkBrowserSignIn { attempt: 7, base_url: server.url.clone(), token: "cbs_session".into(), exchange: exchange() });
    match next_event(&handle, |e| matches!(e, SyncEvent::LinkPending { .. } | SyncEvent::LinkFailed { .. })) {
        SyncEvent::LinkPending { attempt, link_id, email } => assert_eq!((attempt, link_id.as_deref(), email.as_deref()), (7, Some("lnk_fixture"), Some("ada@gmail.example"))),
        other => panic!("{other:?}"),
    }
    handle.send(SyncCommand::ConfirmLink { attempt: 7, base_url: server.url.clone(), token: "cbs_session".into(), link_id: "lnk_fixture".into() });
    match next_event(&handle, |e| matches!(e, SyncEvent::Linked { .. } | SyncEvent::LinkFailed { .. })) {
        SyncEvent::Linked { attempt } => assert_eq!(attempt, 7),
        other => panic!("{other:?}"),
    }
    let requests = server.requests.lock().unwrap().clone();
    assert!(requests.iter().all(|r| r.method == "POST" && r.authorization.as_deref() == Some("Bearer cbs_session")), "{requests:?}");
    assert_eq!(serde_json::from_str::<ClerkExchangeRequest>(&requests[0].body).unwrap(), exchange());
    assert_eq!(serde_json::from_str::<ClerkLinkConfirmRequest>(&requests[1].body).unwrap().link_id, "lnk_fixture");
}

#[test]
fn a_refused_link_names_its_attempt_and_the_reason() {
    for (path, status, body, expected) in [
        ("/v1/auth/clerk/link", 409, r#"{"error":{"code":"identity_in_use","message":"linked to another account"}}"#, "identity_in_use: linked to another account"),
        ("/v1/auth/clerk/link", 401, "not JSON", "unauthorized: session rejected"),
        ("/v1/auth/clerk/link/confirm", 404, r#"{"error":{"code":"not_found","message":"not found"}}"#, "not_found: not found"),
    ] {
        let server = Server::start(move |_| Reply::json(status, body));
        let handle = SyncHandle::spawn(Arc::new(|| {}));
        if path.ends_with("confirm") {
            handle.send(SyncCommand::ConfirmLink { attempt: 3, base_url: server.url.clone(), token: "cbs_session".into(), link_id: "lnk_gone".into() });
        } else {
            handle.send(SyncCommand::LinkBrowserSignIn { attempt: 3, base_url: server.url.clone(), token: "cbs_session".into(), exchange: exchange() });
        }
        match next_event(&handle, |e| matches!(e, SyncEvent::LinkPending { .. } | SyncEvent::Linked { .. } | SyncEvent::LinkFailed { .. })) {
            SyncEvent::LinkFailed { attempt, error } => assert_eq!((attempt, error.as_str()), (3, expected), "{path}"),
            other => panic!("{path}: {other:?}"),
        }
        assert_eq!(server.requests.lock().unwrap()[0].path, path);
    }
}
