//! A dev server's key goes with sign-in starts to that server and nowhere
//! else, so only a holder of its `DEV_CODE_KEY` gets codes back from it.
use super::*;
use std::time::Instant;

const KEY: &str = "fixture-dev-key";

/// The x-crossbow-dev-key header of one sign-in start by a worker given
/// `dev_key` for the fixture server's URL.
fn sent_key(dev_key: impl FnOnce(&str) -> Option<DevKey>) -> Option<String> {
    let server = Server::start(|request| match request.path.as_str() {
        "/v1/auth/start" => Reply::json(200, r#"{"challenge_id":"chl_fixture","expires_in":600,"dev_code":"123456"}"#),
        path => panic!("unexpected fixture request: {path}"),
    });
    let handle = SyncHandle::spawn_with_dev_key(Arc::new(|| {}), dev_key(&server.url));
    handle.send(SyncCommand::StartAuth { base_url: server.url.clone(), email: "ada@example.com".into() });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match handle.try_recv() {
            Ok(SyncEvent::AuthStarted { challenge_id, .. }) => { assert_eq!(challenge_id, "chl_fixture"); break }
            Ok(SyncEvent::AuthError(e)) => panic!("start failed: {e}"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "no answer to the start");
        std::thread::sleep(Duration::from_millis(2));
    }
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    requests[0].dev_key.clone()
}

#[test]
fn the_dev_key_goes_with_sign_in_starts_to_its_own_server_only() {
    assert_eq!(sent_key(|url| DevKey::from_vars(Some(url.into()), Some(KEY.into()))).as_deref(), Some(KEY));
    assert_eq!(
        sent_key(|url| DevKey::from_vars(Some(format!("{url}/")), Some(format!(" {KEY}\n")))).as_deref(),
        Some(KEY),
        "a trailing slash and spaces read as the same server and key",
    );
    assert_eq!(sent_key(|_| DevKey::from_vars(Some("https://sync.example.com".into()), Some(KEY.into()))), None, "another server's key isn't sent");
    assert_eq!(sent_key(|_| None), None);
}

#[test]
fn a_dev_key_needs_its_server_and_a_key() {
    assert!(DevKey::from_vars(None, Some(KEY.into())).is_none());
    assert!(DevKey::from_vars(Some("http://127.0.0.1:9".into()), None).is_none());
    assert!(DevKey::from_vars(Some("http://127.0.0.1:9".into()), Some(" \n".into())).is_none());
    assert!(DevKey::from_vars(Some("/".into()), Some(KEY.into())).is_none());
}
