//! Settings → Account → Delete Account… through simulated input at both
//! sizes, against a loopback fake sync service that deletes the account as
//! the Worker does: after a typed email and a fresh email code, the account
//! and its sessions are gone and `/v1/me` answers 401.

use super::repair_tests::{EncryptionFixture, danger, settings_test_frame, sizes, type_into};
use super::*;
use crate::sync_glue::sync_test_server::{Reply, Request, Server, SyncService};
use crate::sync_glue::{ACCOUNT_DELETED, DeleteStep, SESSION_ENDED};
use crossbow_core::proto::{DeleteAccountRequest, StepUp};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

const EMAIL: &str = "fixture@example.com";
const STEP_UP_CODE: &str = "222222";

/// How the fake service answers `DELETE /v1/account`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Delete {
    /// As the Worker: checks the typed email and the step-up code.
    Checks,
    /// A Worker from before Delete Account.
    Unknown,
    /// Answers 200 but deletes nothing, so the readback still signs in.
    Pretends,
    /// Drops the connection unanswered.
    Close,
    /// The session ended before the request.
    SessionGone,
}

struct Service {
    server: Option<Server>,
    delete: Arc<Mutex<Delete>>,
    /// 0 for the sign-in code; each step-up code after it counts up.
    starts: Arc<AtomicU32>,
    /// Starts get `429 rate_limited` while set.
    limited: Arc<Mutex<bool>>,
    deleted: Arc<Mutex<bool>>,
}

impl Service {
    fn start() -> Service {
        let (delete, starts, limited, deleted) = (Arc::new(Mutex::new(Delete::Checks)), Arc::new(AtomicU32::new(0)), Arc::new(Mutex::new(false)), Arc::new(Mutex::new(false)));
        let sync = Arc::new(SyncService::default());
        let shared = (delete.clone(), starts.clone(), limited.clone(), deleted.clone());
        let server = Server::start(move |request: &Request| {
            let (delete, starts, limited, deleted) = &shared;
            let refused = |status: u16, code: &str| Reply::json(status, serde_json::json!({ "error": { "code": code, "message": "fixture" } }).to_string());
            let gone = *deleted.lock().unwrap();
            if gone && request.authorization.is_some() { return refused(401, "unauthorized"); }
            if let Some(reply) = sync.reply(request, "device-a") { return reply; }
            let identity = serde_json::json!({
                "account": { "id": "fixture-account", "email": EMAIL, "created_at": 1 },
                "device": { "id": "device-a", "name": "device-a", "platform": "macos", "created_at": 1, "last_seen_at": 1 },
            });
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/v1/health") => Reply::json(200, r#"{"ok":true,"protocol":1,"service":"crossbow-api","version":"fixture"}"#),
                ("POST", "/v1/auth/start") if *limited.lock().unwrap() => refused(429, "rate_limited"),
                ("POST", "/v1/auth/start") => {
                    let n = starts.fetch_add(1, Ordering::SeqCst);
                    let code = if n == 0 { "123456".to_string() } else { STEP_UP_CODE.to_string() };
                    Reply::json(200, serde_json::json!({ "challenge_id": format!("chl_{n}"), "expires_in": 600, "dev_code": code }).to_string())
                }
                ("POST", "/v1/auth/verify") => {
                    let mut reply = identity.clone();
                    reply["session_token"] = "cbs_email".into();
                    Reply::json(200, reply.to_string())
                }
                ("GET", "/v1/me") => {
                    let mut me = identity.clone();
                    me["devices"] = serde_json::json!([identity["device"]]);
                    Reply::json(200, me.to_string())
                }
                ("DELETE", "/v1/account") => match *delete.lock().unwrap() {
                    Delete::Checks => {
                        let input: DeleteAccountRequest = serde_json::from_str(&request.body).unwrap();
                        let latest = format!("chl_{}", starts.load(Ordering::SeqCst) - 1);
                        match input.step_up {
                            _ if !input.confirm.trim().eq_ignore_ascii_case(EMAIL) => refused(400, "confirmation_mismatch"),
                            StepUp::EmailCode { challenge_id, code } if challenge_id == latest && code == STEP_UP_CODE => {
                                *deleted.lock().unwrap() = true;
                                Reply::json(200, r#"{"deleted_rows":1}"#)
                            }
                            _ => refused(403, "invalid_code"),
                        }
                    }
                    Delete::Unknown => refused(404, "not_found"),
                    Delete::Pretends => Reply::json(200, r#"{"deleted_rows":0}"#),
                    Delete::Close => Reply::close(),
                    Delete::SessionGone => refused(401, "unauthorized"),
                },
                ("POST", "/v1/auth/logout") => Reply::json(200, "{}"),
                ("GET", "/v1/sync/ws") => Reply::socket(),
                (method, path) => panic!("unexpected fixture request: {method} {path}"),
            }
        });
        Service { server: Some(server), delete, starts, limited, deleted }
    }

    fn take(&mut self) -> Server {
        self.server.take().unwrap()
    }
}

/// Signed in by email code, with the first upload done.
fn signed_in(service: &mut Service, size: egui::Vec2) -> EncryptionFixture {
    let mut f = EncryptionFixture::signed_out(service.take(), size);
    f.sign_in();
    f.until("signed in", |f| f.sync.signed_in() && !f.sync.devices.is_empty());
    f
}

fn requests(f: &EncryptionFixture, path: &str) -> Vec<Request> {
    f._server.requests.lock().unwrap().iter().filter(|r| r.path == path).cloned().collect()
}

/// Open the sheet, type the email and ask for a code.
fn up_to_the_code(f: &mut EncryptionFixture) {
    f.click(DELETE_ACCOUNT);
    type_into(f, "settings-delete-account-email", EMAIL);
    f.click("Send a Code");
    f.until("step-up code", |f| matches!(f.sync.delete.step, DeleteStep::Code { .. }));
}

/// Escape as the app dispatches it; true when the sheet would close.
fn escape(f: &mut EncryptionFixture) -> bool {
    let mut closes = false;
    for pressed in [true, false] {
        let events = vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }];
        settings_test_frame(&f.ctx, Page::Account, f.size, 1.0, &mut f.frame, events, |ui, pal| {
            let ctx = ui.ctx().clone();
            f.sync.poll(&mut f.lib, &ctx);
            if crate::app::take_dismiss_shortcut(&ctx) && !cancel_account_step(Some(&f.state), &mut f.sync) {
                closes = true;
            }
            let now = ctx.input(|i| i.time);
            account_page(&mut f.state, &mut f.sync, &mut f.lib, ui, pal, now);
        });
    }
    f.settle();
    closes
}

#[test]
fn delete_account_needs_the_typed_email_and_a_fresh_code_and_keeps_this_library() {
    for size in sizes() {
        let mut service = Service::start();
        let mut f = signed_in(&mut service, size);
        let spaces = f.lib.spaces_ordered().len();
        assert_eq!(f.visible(DELETE_ACCOUNT).color(DELETE_ACCOUNT), danger(), "a destructive action is drawn in the danger colour");
        f.click(DELETE_ACCOUNT);
        f.visible(&format!("Delete the account {EMAIL}?"));
        for fact in DELETE_ACCOUNT_FACTS { f.visible(fact); }
        f.visible("Your Clerk user for browser sign-in isn't deleted. Delete it with Clerk if you want it gone.");
        f.fits_width();

        // No code goes out until the account's email is typed.
        f.click("Send a Code");
        assert_eq!(f.sync.delete.step, DeleteStep::Confirm);
        type_into(&mut f, "settings-delete-account-email", "someone@example.com");
        f.click("Send a Code");
        assert_eq!(f.sync.delete.step, DeleteStep::Confirm);
        assert_eq!(service.starts.load(Ordering::SeqCst), 1, "only the sign-in's code was sent");
        type_into(&mut f, "settings-delete-account-email", " Fixture@Example.COM ");
        f.click("Send a Code");
        f.until("step-up code", |f| matches!(f.sync.delete.step, DeleteStep::Code { .. }));
        let start = requests(&f, "/v1/auth/start").pop().unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&start.body).unwrap()["email"], EMAIL, "the code goes to the account's own email");
        f.visible(&format!("Development test code: {STEP_UP_CODE}"));
        f.fits_width();

        // Delete waits for a whole code, and is the danger colour.
        f.click("Delete Account");
        assert!(requests(&f, "/v1/account").is_empty());
        type_into(&mut f, "settings-delete-account-code", STEP_UP_CODE);
        // An email changed after the code went out stops it again.
        type_into(&mut f, "settings-delete-account-email", "someone@example.com");
        f.click("Delete Account");
        assert!(requests(&f, "/v1/account").is_empty());
        type_into(&mut f, "settings-delete-account-email", EMAIL);
        assert_eq!(f.visible("Delete Account").color("Delete Account"), danger());
        f.click("Delete Account");
        f.until("account deleted", |f| !f.sync.signed_in());
        f.visible(ACCOUNT_DELETED);
        f.visible("Get a Sign-in Code");
        assert!(*service.deleted.lock().unwrap());
        let sent: DeleteAccountRequest = serde_json::from_str(&requests(&f, "/v1/account")[0].body).unwrap();
        assert_eq!(sent.step_up, StepUp::EmailCode { challenge_id: "chl_1".into(), code: STEP_UP_CODE.into() });
        assert_eq!(sent.confirm.trim().to_ascii_lowercase(), EMAIL);
        assert!(requests(&f, "/v1/me").last().unwrap().authorization.is_some(), "the delete was read back with its session");

        // This Mac kept its library and holds no sign-in, through a reopen.
        assert_eq!(f.lib.spaces_ordered().len(), spaces);
        f.sync = SyncGlue::with_key_store(&f._database.dir, &f.lib, || {}, Box::new(crossbow_core::sync::key_store::MemoryKeyStore::default()));
        assert!(!f.sync.signed_in());
        assert_eq!(f.lib.spaces_ordered().len(), spaces);
        f.visible("Get a Sign-in Code");
        assert!(!f.settle().labels().iter().any(|l| l == ACCOUNT_DELETED), "the notice isn't persisted");
    }
}

#[test]
fn cancel_and_escape_delete_nothing_and_a_reopened_sheet_starts_empty() {
    for size in sizes() {
        let mut service = Service::start();
        let mut f = signed_in(&mut service, size);
        up_to_the_code(&mut f);
        type_into(&mut f, "settings-delete-account-code", STEP_UP_CODE);
        f.click("Cancel");
        assert_eq!(f.sync.delete.step, DeleteStep::Closed);
        assert!(!f.settle().labels().iter().any(|l| l == DELETE_ACCOUNT_FACTS[0]));

        // A reopened sheet asks again for both.
        f.click(DELETE_ACCOUNT);
        assert!(f.state.delete_account_email.is_empty() && f.state.delete_account_code.is_empty());
        assert_eq!(f.sync.delete.step, DeleteStep::Confirm);
        assert!(!escape(&mut f), "Escape closes the delete sheet first");
        assert_eq!(f.sync.delete.step, DeleteStep::Closed);

        // Escape with a code sent, too; then, with nothing open, it closes Settings.
        up_to_the_code(&mut f);
        assert!(!escape(&mut f));
        assert_eq!(f.sync.delete.step, DeleteStep::Closed);
        assert!(escape(&mut f), "with nothing in progress, Escape closes Settings");

        assert!(requests(&f, "/v1/account").is_empty(), "nothing was deleted");
        assert!(f.sync.signed_in());
    }
}

#[test]
fn every_failure_says_what_happened_and_this_mac_stays_signed_in() {
    for size in sizes() {
        // A wrong code: said, and a new code then works.
        let mut service = Service::start();
        let mut f = signed_in(&mut service, size);
        up_to_the_code(&mut f);
        type_into(&mut f, "settings-delete-account-code", "999999");
        f.click("Delete Account");
        f.until("wrong code", |f| f.sync.delete.error.is_some());
        f.visible("That code isn't right. Check it, or send a new code.");
        assert!(f.sync.signed_in());
        f.fits_width();
        f.click("Send a New Code");
        f.until("new code", |f| matches!(&f.sync.delete.step, DeleteStep::Code { challenge_id, .. } if challenge_id == "chl_2"));
        assert!(f.sync.delete.error.is_none(), "a new code clears the last error");
        type_into(&mut f, "settings-delete-account-code", STEP_UP_CODE);
        f.click("Delete Account");
        f.until("account deleted", |f| !f.sync.signed_in());
        f.visible(ACCOUNT_DELETED);

        for (what, delete, expected) in [
            ("a server without the route", Delete::Unknown, "This sync server can't delete accounts yet. Nothing was deleted."),
            ("a dropped connection", Delete::Close, "Couldn't reach the sync server. This Mac is still signed in; try again."),
            ("a delete the readback doesn't show", Delete::Pretends, "The sync server didn't confirm the delete. This Mac is still signed in; try again."),
        ] {
            let mut service = Service::start();
            let mut f = signed_in(&mut service, size);
            *service.delete.lock().unwrap() = delete;
            up_to_the_code(&mut f);
            type_into(&mut f, "settings-delete-account-code", STEP_UP_CODE);
            f.click("Delete Account");
            f.until(what, |f| f.sync.delete.error.is_some());
            f.visible(expected);
            assert!(f.sync.signed_in(), "{what}");
            assert!(matches!(f.sync.delete.step, DeleteStep::Code { .. }), "{what}: the sheet stays, ready to try again");
            f.fits_width();
        }

        // Too many codes for this email: nothing to enter, and said so.
        let mut service = Service::start();
        let mut f = signed_in(&mut service, size);
        *service.limited.lock().unwrap() = true;
        f.click(DELETE_ACCOUNT);
        type_into(&mut f, "settings-delete-account-email", EMAIL);
        f.click("Send a Code");
        f.until("rate limited", |f| f.sync.delete.error.is_some());
        f.visible("Too many codes went to this email. Try again later.");
        assert_eq!(f.sync.delete.step, DeleteStep::Confirm);

        // The session had ended (an earlier delete's answer lost): signed out, and why.
        let mut service = Service::start();
        let mut f = signed_in(&mut service, size);
        *service.delete.lock().unwrap() = Delete::SessionGone;
        up_to_the_code(&mut f);
        type_into(&mut f, "settings-delete-account-code", STEP_UP_CODE);
        f.click("Delete Account");
        f.until("session ended", |f| !f.sync.signed_in());
        f.visible(SESSION_ENDED);
        f.fits_width();
    }
}
