//! Offers and fills through the controller, on a disposable vault with the
//! memory key store. Every username and password is a synthetic fixture.

use super::*;
use crate::passwords::tests::Harness;

const SITE: &str = "https://login.example";
const A: &str = "fixture-profile-a";
const B: &str = "fixture-profile-b";
const TAB: &str = "tab-1";
const USER: &str = "fixture-user";
const PASSWORD: &str = "fixture-page-password-1";
const NEWER: &str = "fixture-page-password-2";

/// Both fixture profiles, with "Offer to save passwords from websites" on.
fn harness() -> Harness {
    let mut h = Harness::new();
    h.passwords.set_profiles(A, [(A.to_owned(), "Personal".to_owned()), (B.to_owned(), "Work".to_owned())]);
    offering(&mut h);
    h
}

/// A vault that has been unlocked once, so it has a key, then locked.
fn locked() -> Harness {
    let mut h = harness();
    h.passwords.status().unwrap();
    h.passwords.unlock();
    h.passwords.lock(None);
    assert!(!h.passwords.unlocked());
    h
}

fn unlocked() -> Harness {
    let mut h = harness();
    h.passwords.status().unwrap();
    h.passwords.unlock();
    assert!(h.passwords.unlocked());
    h
}

fn add(h: &mut Harness, origin: &str, username: &str, password: &str, profile: Option<&str>) -> String {
    h.passwords.vault_mut().unwrap().add(&EntryInput { origin, username, password, notes: "fixture note", profile }).unwrap()
}

fn saved(h: &Harness, origin: &str, profile: &str) -> Vec<(String, String)> {
    let vault = h.passwords.vault().unwrap();
    vault.matching(origin, profile).into_iter().map(|entry| (entry.username.to_string(), vault.password(&entry.id).unwrap().to_string())).collect()
}

/// Turn the switch on for both fixture profiles, as the app would after loading it.
fn offering(h: &mut Harness) {
    for profile in [A, B] { h.passwords.set_offer_to_save(profile, true); }
}

fn reopen_unlocked(h: &mut Harness) {
    h.reopen();
    h.passwords.set_profiles(A, [(A.to_owned(), "Personal".to_owned()), (B.to_owned(), "Work".to_owned())]);
    offering(h);
    h.passwords.status().unwrap();
    h.passwords.unlock();
    assert!(h.passwords.unlocked());
}

#[test]
fn one_submit_gives_exactly_one_offer_per_tab() {
    let mut h = unlocked();
    h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
    h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
    assert_eq!(h.passwords.offers.len(), 1, "the same login twice is one offer");
    let offer = h.passwords.offer(TAB).unwrap();
    assert_eq!((offer.origin.as_str(), offer.profile.as_str(), offer.username.as_str(), &offer.kind), (SITE, A, USER, &OfferKind::Save));
    h.passwords.offer_login(TAB, A, SITE, "other-user", PASSWORD);
    assert_eq!(h.passwords.offers.len(), 1, "a newer login in the same tab replaces it");
    assert_eq!(h.passwords.offer(TAB).unwrap().username.as_str(), "other-user");
    h.passwords.offer_login("tab-2", A, SITE, USER, PASSWORD);
    assert_eq!(h.passwords.offers.len(), 2, "each tab has its own");
    h.passwords.keep_offers(|tab| tab == "tab-2");
    assert!(h.passwords.offer(TAB).is_none(), "a closed tab's offer is wiped");
}

#[test]
fn nothing_is_offered_for_non_web_origins_empty_passwords_or_tabs_without_a_profile() {
    let mut h = unlocked();
    for (profile, origin, password) in [
        (A, "file://", PASSWORD), (A, "data:text/html", PASSWORD), (A, "chrome://settings", PASSWORD), (A, "login.example", PASSWORD),
        (A, "https://login.example/path", PASSWORD), (A, "https://LOGIN.example", PASSWORD), (A, SITE, ""), ("", SITE, PASSWORD),
    ] {
        h.passwords.offer_login(TAB, profile, origin, USER, password);
        assert!(h.passwords.offer(TAB).is_none(), "{profile:?} {origin:?} {}", password.is_empty());
    }
}

#[test]
fn not_now_saves_nothing_and_the_next_submit_offers_again_after_a_reopen() {
    let mut h = unlocked();
    h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
    h.passwords.dismiss_offer(TAB);
    assert!(h.passwords.offer(TAB).is_none());
    reopen_unlocked(&mut h);
    assert!(h.passwords.vault().unwrap().entries().is_empty(), "nothing saved");
    assert!(h.passwords.never_saved(A).is_empty(), "and the site isn't refused");
    h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
    assert!(h.passwords.offer(TAB).is_some());
}

#[test]
fn never_for_this_site_persists_per_profile_and_can_be_removed() {
    let mut h = locked();
    h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
    assert_eq!(h.passwords.never_for_offer(TAB), Some(NEVER_SAVED), "works while locked");
    assert!(h.passwords.offer(TAB).is_none());
    h.reopen();
    offering(&mut h);
    assert_eq!(h.passwords.never_saved(A), [SITE]);
    h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
    assert!(h.passwords.offer(TAB).is_none(), "never offered again in this profile");
    h.passwords.offer_login(TAB, A, "https://login.example:8443", USER, PASSWORD);
    assert!(h.passwords.offer(TAB).is_some(), "another port is another site");
    h.passwords.dismiss_offer(TAB);
    h.passwords.offer_login(TAB, B, SITE, USER, PASSWORD);
    assert!(h.passwords.offer(TAB).is_some(), "another profile still offers");
    h.passwords.dismiss_offer(TAB);
    h.passwords.remove_never_save(A, SITE);
    h.reopen();
    offering(&mut h);
    assert!(h.passwords.never_saved(A).is_empty());
    h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
    assert!(h.passwords.offer(TAB).is_some(), "offered again once removed");
}

#[test]
fn save_stores_the_edited_username_in_the_tab_profile_and_survives_a_reopen() {
    let mut h = unlocked();
    h.passwords.offer_login(TAB, B, SITE, USER, PASSWORD);
    let offer = h.passwords.offer_mut(TAB).unwrap();
    offer.username.clear();
    offer.username.push_str("edited-user");
    assert_eq!(h.passwords.save_offer(TAB), Some(SAVED));
    assert!(h.passwords.offer(TAB).is_none());
    reopen_unlocked(&mut h);
    assert_eq!(saved(&h, SITE, B), [("edited-user".to_owned(), PASSWORD.to_owned())]);
    assert!(saved(&h, SITE, A).is_empty(), "never in another profile");
}

#[test]
fn a_locked_vault_asks_to_unlock_then_saves() {
    let mut h = locked();
    h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
    assert_eq!(h.passwords.offer(TAB).unwrap().kind, OfferKind::Save);
    assert_eq!(h.passwords.save_offer(TAB), None, "Save asks to unlock first");
    assert!(h.passwords.offer(TAB).unwrap().unlocking);
    assert!(!h.passwords.unlocked());
    // An unlock that fails keeps the login and says why.
    h.keys.fail_reads(true);
    assert_eq!(h.passwords.unlock_for_offer(TAB), None);
    assert!(h.passwords.offer(TAB).unwrap().error.is_some());
    h.keys.fail_reads(false);
    assert_eq!(h.passwords.unlock_for_offer(TAB), Some(SAVED));
    assert!(h.passwords.offer(TAB).is_none());
    reopen_unlocked(&mut h);
    assert_eq!(saved(&h, SITE, A), [(USER.to_owned(), PASSWORD.to_owned())]);
}

#[test]
fn a_locked_offer_of_a_saved_login_ends_as_already_saved_and_one_with_a_new_password_asks_to_update() {
    let mut h = unlocked();
    add(&mut h, SITE, USER, PASSWORD, Some(A));
    h.passwords.lock(None);
    h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
    h.passwords.save_offer(TAB);
    assert_eq!(h.passwords.unlock_for_offer(TAB), Some(ALREADY_SAVED));
    assert_eq!(h.passwords.vault().unwrap().entries().len(), 1);
    h.passwords.lock(None);
    h.passwords.offer_login(TAB, A, SITE, USER, NEWER);
    h.passwords.save_offer(TAB);
    assert_eq!(h.passwords.unlock_for_offer(TAB), None, "an update is never made without showing it");
    assert!(matches!(h.passwords.offer(TAB).unwrap().kind, OfferKind::Update(_)));
    assert_eq!(h.passwords.save_offer(TAB), Some(UPDATED));
    assert_eq!(saved(&h, SITE, A), [(USER.to_owned(), NEWER.to_owned())]);
}

#[test]
fn update_replaces_only_the_matching_entry_and_keeps_its_notes() {
    let mut h = unlocked();
    let alice = add(&mut h, SITE, "alice", PASSWORD, Some(A));
    let bob = add(&mut h, SITE, "bob", "fixture-bob-password", Some(A));
    let work = add(&mut h, SITE, "alice", "fixture-work-password", Some(B));
    let port = add(&mut h, "https://login.example:8443", "alice", "fixture-port-password", Some(A));
    let untagged = add(&mut h, SITE, "alice", "fixture-untagged-password", None);
    h.passwords.offer_login(TAB, A, SITE, "alice", NEWER);
    assert_eq!(h.passwords.offer(TAB).unwrap().kind, OfferKind::Update(alice.clone()));
    assert_eq!(h.passwords.save_offer(TAB), Some(UPDATED));
    reopen_unlocked(&mut h);
    let vault = h.passwords.vault().unwrap();
    let password = |id: &str| vault.password(id).unwrap().to_string();
    assert_eq!(password(&alice), NEWER);
    assert_eq!(vault.secret(&alice).unwrap().notes.as_str(), "fixture note", "notes are kept");
    assert_eq!(vault.entry(&alice).unwrap().profile.as_deref(), Some(A));
    for (id, expected) in [(&bob, "fixture-bob-password"), (&work, "fixture-work-password"), (&port, "fixture-port-password"), (&untagged, "fixture-untagged-password")] {
        assert_eq!(password(id), expected, "only the matching entry changed");
    }
    assert_eq!(vault.entries().len(), 5, "nothing added");
}

#[test]
fn a_saved_login_is_not_offered_and_a_username_edited_into_a_saved_one_shows_update_first() {
    let mut h = unlocked();
    let alice = add(&mut h, SITE, "alice", PASSWORD, Some(A));
    h.passwords.offer_login(TAB, A, SITE, "alice", PASSWORD);
    assert!(h.passwords.offer(TAB).is_none(), "the same login is already saved");
    h.passwords.offer_login(TAB, A, SITE, "new-user", NEWER);
    let offer = h.passwords.offer_mut(TAB).unwrap();
    assert_eq!(offer.kind, OfferKind::Save);
    offer.username.clear();
    offer.username.push_str("alice");
    assert_eq!(h.passwords.save_offer(TAB), None, "the first click shows Update Password");
    assert_eq!(h.passwords.offer(TAB).unwrap().kind, OfferKind::Update(alice.clone()));
    assert_eq!(h.passwords.vault().unwrap().password(&alice).unwrap().as_str(), PASSWORD, "nothing replaced yet");
    assert_eq!(h.passwords.save_offer(TAB), Some(UPDATED));
    assert_eq!(h.passwords.vault().unwrap().entries().len(), 1);
}

#[test]
fn offers_are_wiped_after_two_minutes_and_when_the_account_changes() {
    let mut h = locked();
    h.passwords.tick_at(Some("account-a"), false);
    h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
    let wait = h.passwords.tick_at(Some("account-a"), false).unwrap();
    assert!(wait <= OFFER_TIMEOUT + Duration::from_millis(50), "a tick is due when it expires: {wait:?}");
    h.advance(OFFER_TIMEOUT - Duration::from_secs(1));
    h.passwords.tick_at(Some("account-a"), false);
    assert!(h.passwords.offer(TAB).is_some());
    h.advance(Duration::from_secs(1));
    h.passwords.tick_at(Some("account-a"), false);
    assert!(h.passwords.offer(TAB).is_none(), "wiped after 2 minutes");
    h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
    h.passwords.tick_at(Some("account-b"), false);
    assert!(h.passwords.offer(TAB).is_none(), "and when the account changes");
    // The vault's idle lock doesn't end an offer; Save then asks to unlock.
    let mut h = unlocked();
    h.advance(IDLE_TIMEOUT - Duration::from_secs(60));
    h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
    h.advance(Duration::from_secs(90));
    h.passwords.tick_at(None, false);
    assert!(!h.passwords.unlocked(), "the vault locked while idle");
    assert!(h.passwords.offer(TAB).is_some(), "the offer keeps its own 2 minutes");
    assert_eq!(h.passwords.save_offer(TAB), None);
    assert!(h.passwords.offer(TAB).unwrap().unlocking);
}

#[test]
fn only_exact_origin_entries_of_the_tab_profile_are_filled_and_each_fill_is_recorded() {
    let mut h = unlocked();
    let wanted = add(&mut h, SITE, USER, PASSWORD, Some(A));
    let work = add(&mut h, SITE, USER, "fixture-work-password", Some(B));
    let untagged = add(&mut h, SITE, "legacy", "fixture-legacy-password", None);
    let ids = |h: &Harness, origin: &str, profile: &str| h.passwords.logins_for(origin, profile).into_iter().map(|(id, _)| id).collect::<Vec<_>>();
    assert_eq!(ids(&h, SITE, A), [wanted.clone()]);
    assert_eq!(ids(&h, SITE, B), [work.clone()]);
    for origin in ["http://login.example", "https://login.example:8443", "https://www.login.example", "https://example"] {
        assert!(ids(&h, origin, A).is_empty(), "{origin}");
        assert_eq!(h.passwords.login_to_fill(&wanted, origin, A), Err(FILL_GONE), "{origin}");
    }
    assert_eq!(h.passwords.login_to_fill(&wanted, SITE, B), Err(FILL_GONE), "never another profile's entry");
    assert_eq!(h.passwords.login_to_fill(&untagged, SITE, A), Err(FILL_GONE), "never an entry in no profile");
    let (username, password) = h.passwords.login_to_fill(&wanted, SITE, A).unwrap();
    assert_eq!((username.as_str(), password.as_str()), (USER, PASSWORD));
    let used = h.passwords.vault().unwrap().entry(&wanted).unwrap().used_ms;
    assert!(used.is_some_and(|ms| ms.abs_diff(crossbow_core::now_ms()) < 60_000));
    reopen_unlocked(&mut h);
    assert_eq!(h.passwords.vault().unwrap().entry(&wanted).unwrap().used_ms, used, "last use survives a reopen");
    assert_eq!(h.passwords.vault().unwrap().entry(&work).unwrap().used_ms, None);
    // Locked, nothing is listed or filled.
    h.passwords.lock(None);
    assert!(ids(&h, SITE, A).is_empty());
    assert_eq!(h.passwords.login_to_fill(&wanted, SITE, A), Err(FILL_GONE));
}

#[test]
fn the_key_shows_for_a_page_with_its_own_logins_or_while_locked_on_a_login_field() {
    let mut h = unlocked();
    assert_eq!(h.passwords.key_state(SITE, A, true), None, "no entries");
    add(&mut h, SITE, USER, PASSWORD, Some(A));
    add(&mut h, SITE, "second", NEWER, Some(A));
    add(&mut h, "https://other.example", USER, PASSWORD, Some(B));
    assert_eq!(h.passwords.key_state(SITE, A, false), Some(KeyState::Logins(2)));
    assert_eq!(h.passwords.key_state(SITE, B, true), None, "another profile's page");
    assert_eq!(h.passwords.key_state("https://login.example:8443", A, true), None);
    h.passwords.lock(None);
    assert_eq!(h.passwords.key_state(SITE, A, false), None, "locked, only on a login field");
    assert_eq!(h.passwords.key_state(SITE, A, true), Some(KeyState::Locked));
    let mut empty = locked();
    assert_eq!(empty.passwords.key_state(SITE, A, true), None, "an empty vault has nothing to fill");
}

#[test]
fn use_in_current_profile_makes_an_entry_from_before_profiles_fillable_here_only() {
    let mut h = unlocked();
    let legacy = add(&mut h, SITE, USER, PASSWORD, None);
    assert!(h.passwords.logins_for(SITE, A).is_empty());
    h.passwords.use_in_current_profile(&legacy);
    assert_eq!(h.passwords.logins_for(SITE, A).len(), 1);
    assert!(h.passwords.logins_for(SITE, B).is_empty());
    reopen_unlocked(&mut h);
    assert_eq!(h.passwords.vault().unwrap().entry(&legacy).unwrap().profile.as_deref(), Some(A));
}

#[test]
fn settings_add_tags_the_active_profile_and_edit_keeps_the_entry_profile() {
    let mut h = unlocked();
    h.passwords.start_add();
    let editor = h.passwords.page.editor.as_mut().unwrap();
    editor.origin.push_str(SITE);
    editor.password.push_str(PASSWORD);
    h.passwords.save_editor();
    let id = h.passwords.vault().unwrap().entries()[0].id.clone();
    assert_eq!(h.passwords.vault().unwrap().entry(&id).unwrap().profile.as_deref(), Some(A));
    h.passwords.set_profiles(B, []);
    h.passwords.start_edit(&id);
    h.passwords.page.editor.as_mut().unwrap().password.push_str("-edited");
    h.passwords.save_editor();
    assert_eq!(h.passwords.vault().unwrap().entry(&id).unwrap().profile.as_deref(), Some(A), "editing in another profile doesn't move it");
}

#[test]
fn debug_output_of_an_offer_never_shows_its_login() {
    let mut h = unlocked();
    h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
    let shown = format!("{:?} {:#?}", h.passwords.offer(TAB).unwrap(), h.passwords.offers);
    assert!(!shown.contains(USER) && !shown.contains(PASSWORD), "{shown}");
}

/// Every log record while the tests below run, at every level. Shared with
/// the address tests: a process has one logger.
pub(crate) struct Capture(pub(crate) std::sync::Mutex<Vec<String>>);

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        self.0.lock().unwrap().push(format!("{} {} {}", record.level(), record.target(), record.args()));
    }

    fn flush(&self) {}
}

pub(crate) fn captured_logs() -> &'static Capture {
    static CAPTURE: std::sync::OnceLock<&'static Capture> = std::sync::OnceLock::new();
    CAPTURE.get_or_init(|| {
        let capture: &'static Capture = Box::leak(Box::new(Capture(std::sync::Mutex::new(Vec::new()))));
        // Only this binary's tests log; if another logger got there first, the
        // test fails below rather than passing without seeing anything.
        let _ = log::set_logger(capture);
        log::set_max_level(log::LevelFilter::Trace);
        capture
    })
}

#[test]
fn no_username_or_password_reaches_the_log_through_offers_saves_fills_or_errors() {
    let capture = captured_logs();
    log::info!("capture-probe");
    assert!(capture.0.lock().unwrap().iter().any(|line| line.ends_with("capture-probe")), "the capturing logger is installed");
    let mut h = locked();
    h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
    h.passwords.save_offer(TAB);
    h.keys.fail_reads(true);
    h.passwords.unlock_for_offer(TAB);
    h.keys.fail_reads(false);
    h.passwords.unlock_for_offer(TAB);
    h.passwords.offer_login(TAB, A, SITE, USER, NEWER);
    h.passwords.save_offer(TAB);
    let id = h.passwords.logins_for(SITE, A)[0].0.clone();
    let _ = h.passwords.login_to_fill(&id, SITE, A);
    let _ = h.passwords.login_to_fill(&id, "https://elsewhere.example", A);
    h.passwords.offer_login("tab-2", B, SITE, USER, PASSWORD);
    h.passwords.never_for_offer("tab-2");
    let lines = capture.0.lock().unwrap().clone();
    for secret in [USER, PASSWORD, NEWER] {
        assert!(lines.iter().all(|line| !line.contains(secret)), "{secret:?} was logged: {lines:?}");
    }
}

/// "Offer to save passwords from websites" is off by default: a login a page
/// submitted is never offered, in any profile, and nothing of it is kept.
#[test]
fn with_offer_to_save_off_by_default_no_login_is_offered() {
    let mut h = Harness::new();
    h.passwords.set_profiles(A, [(A.to_owned(), "Personal".to_owned()), (B.to_owned(), "Work".to_owned())]);
    h.passwords.status().unwrap();
    h.passwords.unlock();
    for profile in [A, B] { h.passwords.offer_login(TAB, profile, SITE, USER, PASSWORD); }
    assert!(h.passwords.offer(TAB).is_none());
    assert!(h.passwords.offers.is_empty());
}

fn library(path: &std::path::Path) -> Library {
    let mut lib = Library::open(crossbow_core::store::Store::open(path).unwrap()).unwrap();
    lib.ensure_defaults().unwrap();
    lib
}

/// A controller showing `lib`'s profiles, unlocked, after the app's first logic pass.
fn controller_for(lib: &Library) -> Harness {
    let mut h = Harness::new();
    let current = lib.default_profile_id();
    h.passwords.set_profiles(&current, lib.profiles.iter().map(|(id, profile)| (id.clone(), profile.name.clone())));
    h.passwords.status().unwrap();
    h.passwords.unlock();
    sync_offer_to_save(&mut h.passwords, lib);
    h
}

#[test]
fn the_switch_is_per_profile_device_local_and_survives_a_relaunch() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("library.sqlite");
    let mut lib = library(&db);
    let a = lib.default_profile_id();
    let b = lib.create_profile("Work").unwrap();
    let entities = lib.store.all_entities().unwrap();
    let mut h = controller_for(&lib);
    let mut settings = h.passwords.offer_to_save_settings();
    settings.sort();
    let mut expected = vec![(a.clone(), false), (b.clone(), false)];
    expected.sort();
    assert_eq!(settings, expected, "every profile is loaded, off");
    h.passwords.set_offer_to_save(&a, true);
    sync_offer_to_save(&mut h.passwords, &lib);
    assert_eq!(h.passwords.offer_to_save_error(&a), None);
    h.passwords.offer_login(TAB, &a, SITE, USER, PASSWORD);
    h.passwords.offer_login("tab-2", &b, SITE, USER, PASSWORD);
    assert!(h.passwords.offer(TAB).is_some() && h.passwords.offer("tab-2").is_none(), "only the profile that turned it on");
    assert_eq!(lib.store.all_entities().unwrap(), entities, "the switch never enters the sync stream");
    drop(lib);
    // Relaunch: a new library connection and a new controller.
    let lib = library(&db);
    let h = controller_for(&lib);
    assert!(h.passwords.offer_to_save(&a) && !h.passwords.offer_to_save(&b));
    // Any other stored value is off.
    lib.store.kv_set(&format!("passwords.offer_to_save.v1.{a}"), "on").unwrap();
    assert!(!controller_for(&lib).passwords.offer_to_save(&a));
}

#[test]
fn turning_the_switch_off_wipes_waiting_offers_and_offers_nothing_after() {
    let root = tempfile::tempdir().unwrap();
    let lib = library(&root.path().join("library.sqlite"));
    let a = lib.default_profile_id();
    let mut h = controller_for(&lib);
    h.passwords.set_offer_to_save(&a, true);
    sync_offer_to_save(&mut h.passwords, &lib);
    h.passwords.offer_login(TAB, &a, SITE, USER, PASSWORD);
    assert!(h.passwords.offer(TAB).is_some());
    h.passwords.set_offer_to_save(&a, false);
    assert!(h.passwords.offers.is_empty(), "the waiting login is wiped at once");
    h.passwords.offer_login(TAB, &a, SITE, USER, PASSWORD);
    assert!(h.passwords.offer(TAB).is_none());
    sync_offer_to_save(&mut h.passwords, &lib);
    assert!(!controller_for(&lib).passwords.offer_to_save(&a), "saved off");
}

#[test]
fn a_switch_that_cannot_be_read_or_saved_is_off_and_says_why() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("library.sqlite");
    let lib = library(&db);
    let a = lib.default_profile_id();
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_switch BEFORE INSERT ON kv WHEN NEW.k LIKE 'passwords.offer_to_save.v1.%' BEGIN SELECT RAISE(ABORT, 'fixture denial'); END;").unwrap();
    // Turned on, it can't be saved: it goes back off.
    let mut h = controller_for(&lib);
    h.passwords.set_offer_to_save(&a, true);
    sync_offer_to_save(&mut h.passwords, &lib);
    assert!(!h.passwords.offer_to_save(&a));
    assert!(h.passwords.offer_to_save_error(&a).is_some_and(|error| error.starts_with("Could not save this setting: ") && error.ends_with("It is still off.")));
    h.passwords.offer_login(TAB, &a, SITE, USER, PASSWORD);
    assert!(h.passwords.offer(TAB).is_none());
    // Saved on earlier, then turned off but not saved: off until Crossbow quits.
    connection.execute_batch("DROP TRIGGER reject_switch").unwrap();
    lib.store.kv_set(&format!("passwords.offer_to_save.v1.{a}"), "1").unwrap();
    let mut h = controller_for(&lib);
    assert!(h.passwords.offer_to_save(&a));
    connection.execute_batch("CREATE TRIGGER reject_switch BEFORE INSERT ON kv WHEN NEW.k LIKE 'passwords.offer_to_save.v1.%' BEGIN SELECT RAISE(ABORT, 'fixture denial'); END;").unwrap();
    h.passwords.set_offer_to_save(&a, false);
    sync_offer_to_save(&mut h.passwords, &lib);
    assert!(!h.passwords.offer_to_save(&a));
    assert!(h.passwords.offer_to_save_error(&a).is_some_and(|error| error.ends_with("It is off until Crossbow quits.")));
    // Unreadable: off, with the reason.
    connection.execute_batch("DROP TRIGGER reject_switch; ALTER TABLE kv RENAME TO kv_moved;").unwrap();
    let h = controller_for(&lib);
    assert!(!h.passwords.offer_to_save(&a));
    assert!(h.passwords.offer_to_save_error(&a).is_some_and(|error| error.starts_with("Could not read this setting: ") && error.ends_with("It is off.")));
}

/// The key button and the fill need no switch: off, a saved entry is still
/// listed and filled on its own origin and profile, and only there.
#[test]
fn with_the_switch_off_the_key_and_the_fill_still_work_for_saved_entries_only() {
    let mut h = unlocked();
    for profile in [A, B] { h.passwords.set_offer_to_save(profile, false); }
    let id = add(&mut h, SITE, USER, PASSWORD, Some(A));
    assert_eq!(h.passwords.key_state(SITE, A, true), Some(KeyState::Logins(1)));
    assert_eq!(h.passwords.key_state("https://other.example", A, true), None, "no entry, no key");
    assert_eq!(h.passwords.key_state(SITE, B, true), None, "another profile's entry, no key");
    let (username, password) = h.passwords.login_to_fill(&id, SITE, A).unwrap();
    assert_eq!((username.as_str(), password.as_str()), (USER, PASSWORD));
    assert_eq!(h.passwords.login_to_fill(&id, SITE, B).err(), Some(FILL_GONE));
}
