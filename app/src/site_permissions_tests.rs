//! Permission prompts and links to other apps on disposable library files,
//! with fake apps. Every origin and link is a fixture.

use super::*;
use std::cell::RefCell;
use std::rc::Rc;

pub(crate) const A: &str = "fixture-profile-a";
const B: &str = "fixture-profile-b";
pub(crate) const MEET: &str = "https://meet.example/room/1";
const NEWS: &str = "https://news.example/today";
const ZOOM: &str = "zoommtg://zoom.us/join?confno=1&pwd=fixture-token";

#[derive(Default)]
pub(crate) struct FakeState {
    pub handlers: HashMap<String, Handler>,
    pub lookups: Vec<String>,
    pub opened: Vec<String>,
    pub refuse_open: bool,
    /// Info.plist keys the fake bundle lacks.
    pub undeclared: Vec<String>,
}

/// Apps by scheme; records what was looked up and opened.
#[derive(Clone, Default)]
pub(crate) struct FakeApps(pub Rc<RefCell<FakeState>>);

impl FakeApps {
    pub(crate) fn with_zoom() -> FakeApps {
        let apps = FakeApps::default();
        apps.add("zoommtg", "Zoom", "us.zoom.xos");
        apps.add("x-own", "Crossbow", "com.crossbow.fixture");
        apps.add("x-fixture-shell", "Terminal", "com.apple.Terminal");
        apps
    }

    pub(crate) fn add(&self, scheme: &str, name: &str, bundle_id: &str) {
        self.0.borrow_mut().handlers.insert(scheme.into(), Handler { name: name.into(), bundle_id: bundle_id.into() });
    }

    pub(crate) fn opened(&self) -> Vec<String> {
        self.0.borrow().opened.clone()
    }
}

impl Apps for FakeApps {
    fn handler(&self, url: &str) -> Option<Handler> {
        let scheme = url.split_once(':')?.0.to_ascii_lowercase();
        self.0.borrow_mut().lookups.push(scheme.clone());
        self.0.borrow().handlers.get(&scheme).cloned()
    }

    fn open(&mut self, url: &str) -> bool {
        let mut state = self.0.borrow_mut();
        state.opened.push(url.into());
        !state.refuse_open
    }

    fn own_bundle(&self) -> Option<String> {
        Some("com.crossbow.fixture".into())
    }

    fn declares(&self, key: &str) -> bool {
        !self.0.borrow().undeclared.iter().any(|undeclared| undeclared == key)
    }
}

pub(crate) struct Fixture {
    pub _dir: tempfile::TempDir,
    pub store: Store,
    pub apps: FakeApps,
    pub perms: SitePermissions,
}

impl Fixture {
    pub(crate) fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("library.sqlite")).unwrap();
        let apps = FakeApps::with_zoom();
        Fixture { _dir: dir, store, perms: SitePermissions::new(Box::new(apps.clone())), apps }
    }

    /// SQL on a second connection to the same library, as a fixture.
    pub(crate) fn sql(&self, sql: &str) -> rusqlite::Result<()> {
        rusqlite::Connection::open(self._dir.path().join("library.sqlite"))?.execute_batch(sql)
    }

    fn reopen(&mut self) {
        let path = self._dir.path().join("library.sqlite");
        self.store = Store::open(&path).unwrap();
    }

    pub(crate) fn ask(&mut self, tab: &str, serial: u64, profile: &str, request: &PermissionRequest) -> Option<bool> {
        self.perms.request(&self.store, Page { tab, serial, load: request.load }, Some(profile), request)
    }

    fn link(&mut self, tab: &str, link: &ExternalLink, focused: bool, now: f64) -> Option<String> {
        self.perms.link(&self.store, Page { tab, serial: 1, load: link.load }, Some(A), link, focused, now)
    }
}

/// A request for `permission` the way CEF sends it.
pub(crate) fn request(id: u64, permission: Permission, origin: &str, page: &str, load: u64) -> PermissionRequest {
    let (is_media, bits) = bits(permission);
    PermissionRequest { id, media: is_media, bits, origin: origin.into(), page: page.into(), load }
}

fn bits(permission: Permission) -> (bool, u32) {
    match permission {
        Permission::Camera => (true, media::CAMERA),
        Permission::Microphone => (true, media::MICROPHONE),
        Permission::ScreenShare => (true, media::SCREEN_VIDEO),
        Permission::Location => (false, prompt::GEOLOCATION),
        Permission::Notifications => (false, prompt::NOTIFICATIONS),
        Permission::Clipboard => (false, prompt::CLIPBOARD),
        Permission::MultipleDownloads => (false, prompt::MULTIPLE_DOWNLOADS),
        Permission::Midi => (false, prompt::MIDI_SYSEX),
        Permission::WindowManagement => (false, prompt::WINDOW_MANAGEMENT),
        Permission::LocalFonts => (false, prompt::LOCAL_FONTS),
        Permission::IdleDetection => (false, prompt::IDLE_DETECTION),
        Permission::StorageAccess => (false, prompt::STORAGE_ACCESS),
        Permission::MotionSensors => (false, prompt::SENSORS),
        Permission::LocalNetwork => (false, prompt::LOCAL_NETWORK),
        Permission::FileEditing => (false, prompt::FILE_SYSTEM_ACCESS),
        Permission::ProtocolHandlers => (false, prompt::REGISTER_PROTOCOL_HANDLER),
        Permission::PointerLock => (false, prompt::POINTER_LOCK),
        Permission::KeyboardLock => (false, prompt::KEYBOARD_LOCK),
    }
}

fn link(url: &str, gesture: bool, page: &str, load: u64) -> ExternalLink {
    ExternalLink { url: url.into(), gesture, page: page.into(), frame: page.into(), load }
}

fn reply(id: u64, allow: bool) -> Option<Reply> {
    Some(Reply { serial: 1, id, allow })
}

#[test]
fn every_permission_prompts_and_its_answer_is_remembered_or_not_as_chosen() {
    for permission in Permission::ALL {
        let mut f = Fixture::new();
        let ask = request(1, permission, "https://meet.example/", MEET, 1);
        assert_eq!(requested(ask.media, ask.bits), Some(vec![permission]));
        assert_eq!(f.ask("t1", 1, A, &ask), None, "{permission:?} waits for the user");
        let shown = f.perms.question("t1").unwrap();
        assert_eq!(shown.question.title(), format!("meet.example wants to {}", permission.phrase()));
        assert!(shown.remember, "Remember for this site is on by default");
        // Allow, remembered: the next request is allowed silently, after a reopen too.
        assert_eq!(f.perms.choose(&f.store, "t1", Choice::Allow, 1), Outcome { reply: reply(1, true), notice: None });
        assert!(f.perms.question("t1").is_none());
        f.reopen();
        assert_eq!(f.ask("t1", 1, A, &request(2, permission, "https://meet.example", MEET, 1)), Some(true), "{permission:?}");
        // Allow, not remembered: asked again.
        let other = "https://other.example/";
        assert_eq!(f.ask("t1", 1, A, &request(3, permission, other, other, 1)), None);
        f.perms.question_mut("t1").unwrap().remember = false;
        assert_eq!(f.perms.choose(&f.store, "t1", Choice::Allow, 2).reply, reply(3, true));
        assert_eq!(f.ask("t1", 1, A, &request(4, permission, other, other, 1)), None, "{permission:?} unremembered");
        // Don't Allow, not remembered: denied once, asked again.
        f.perms.question_mut("t1").unwrap().remember = false;
        assert_eq!(f.perms.choose(&f.store, "t1", Choice::Deny, 3).reply, reply(4, false));
        assert_eq!(f.ask("t1", 1, A, &request(5, permission, other, other, 1)), None);
        // Don't Allow, remembered: refused silently from then on, after a reopen too.
        assert_eq!(f.perms.choose(&f.store, "t1", Choice::Deny, 4).reply, reply(5, false));
        f.reopen();
        assert_eq!(f.ask("t1", 1, A, &request(6, permission, other, other, 1)), Some(false));
        assert_eq!(f.perms.blocked("t1").unwrap().permissions, [permission]);
        // Escape is Don't Allow, never remembered, even with the box on.
        let third = "https://third.example/";
        assert_eq!(f.ask("t1", 1, A, &request(7, permission, third, third, 1)), None);
        assert_eq!(f.perms.choose(&f.store, "t1", Choice::Dismiss, 5).reply, reply(7, false));
        assert_eq!(f.ask("t1", 1, A, &request(8, permission, third, third, 1)), None);
    }
}

#[test]
fn camera_and_microphone_together_ask_once_and_only_about_what_is_undecided() {
    let mut f = Fixture::new();
    let both = PermissionRequest { id: 1, media: true, bits: media::CAMERA | media::MICROPHONE, origin: "https://meet.example/".into(), page: MEET.into(), load: 1 };
    assert_eq!(f.ask("t1", 1, A, &both), None);
    assert_eq!(f.perms.question("t1").unwrap().question.title(), "meet.example wants to use your camera and microphone");
    f.perms.question_mut("t1").unwrap().remember = false;
    f.perms.choose(&f.store, "t1", Choice::Allow, 1);
    rules::set_decision(&f.store, A, "https://meet.example", "https://meet.example", Permission::Camera, Some(Decision::Allow), 1).unwrap();
    assert_eq!(f.ask("t1", 1, A, &PermissionRequest { id: 2, ..both.clone() }), None);
    assert_eq!(f.perms.question("t1").unwrap().question.title(), "meet.example wants to use your microphone");
    f.perms.choose(&f.store, "t1", Choice::Allow, 2);
    assert_eq!(f.ask("t1", 1, A, &PermissionRequest { id: 3, ..both.clone() }), Some(true));
    // One remembered Block refuses the whole request.
    rules::set_decision(&f.store, A, "https://meet.example", "https://meet.example", Permission::Microphone, Some(Decision::Block), 3).unwrap();
    assert_eq!(f.ask("t1", 1, A, &PermissionRequest { id: 4, ..both }), Some(false));
    assert_eq!(f.perms.blocked("t1").unwrap().permissions, [Permission::Microphone]);
}

#[test]
fn an_embedded_frame_is_decided_apart_from_its_page_and_names_both() {
    let mut f = Fixture::new();
    // A frame from widget.example on news.example asks for the camera.
    assert_eq!(f.ask("t1", 1, A, &request(1, Permission::Camera, "https://widget.example/", NEWS, 1)), None);
    let shown = &f.perms.question("t1").unwrap().question;
    assert_eq!(shown.title(), "widget.example wants to use your camera");
    assert_eq!(shown.detail().as_deref(), Some("Embedded on news.example"));
    f.perms.choose(&f.store, "t1", Choice::Allow, 1);
    assert_eq!(f.ask("t1", 1, A, &request(2, Permission::Camera, "https://widget.example/", NEWS, 1)), Some(true), "the same frame on the same page");
    // The grant reaches neither widget.example as a page nor the page it was embedded on.
    assert_eq!(f.ask("t2", 2, A, &request(3, Permission::Camera, "https://widget.example/", "https://widget.example/", 1)), None);
    assert_eq!(f.ask("t3", 3, A, &request(4, Permission::Camera, "https://news.example/", NEWS, 1)), None);
    // Nor widget.example embedded on another site.
    assert_eq!(f.ask("t4", 4, A, &request(5, Permission::Camera, "https://widget.example/", MEET, 1)), None);
    // A page's own request names no embedding site.
    assert_eq!(f.perms.question("t3").unwrap().question.detail(), None);
}

#[test]
fn insecure_pages_never_prompt_for_camera_microphone_screen_or_location() {
    let mut f = Fixture::new();
    let insecure = "http://news.example/";
    for (id, permission) in [Permission::Camera, Permission::Microphone, Permission::ScreenShare, Permission::Location].into_iter().enumerate() {
        assert_eq!(f.ask("t1", 1, A, &request(id as u64, permission, insecure, insecure, 1)), Some(false), "{permission:?}");
        // An https frame inside an http page is no secure context either.
        assert_eq!(f.ask("t1", 1, A, &request(id as u64, permission, "https://widget.example/", insecure, 1)), Some(false), "{permission:?}");
        // This Mac's own pages are secure.
        assert_eq!(f.ask("t2", 2, A, &request(id as u64, permission, "http://localhost:3000/", "http://localhost:3000/", 1)), None, "{permission:?}");
    }
    assert!(f.perms.question("t1").is_none());
    // Others are asked about if Chromium passes them on.
    assert_eq!(f.ask("t1", 1, A, &request(9, Permission::Notifications, insecure, insecure, 1)), None);
    // Not a web page: nothing to remember it by.
    assert_eq!(f.ask("t3", 3, A, &request(10, Permission::Camera, "file:///", "file:///tmp/a.html", 1)), Some(false));
    assert_eq!(f.perms.request(&f.store, Page { tab: "t4", serial: 4, load: 1 }, None, &request(11, Permission::Camera, "https://meet.example", MEET, 1)), Some(false), "no profile");
}

#[test]
fn requests_crossbow_never_offers_are_refused_without_a_prompt() {
    let mut f = Fixture::new();
    for bits in [prompt::AR_SESSION, prompt::VR_SESSION, prompt::HAND_TRACKING, prompt::PROTECTED_MEDIA_IDENTIFIER, prompt::WEB_APP_INSTALLATION, prompt::IDENTITY_PROVIDER, prompt::DISK_QUOTA, prompt::CAMERA_STREAM | prompt::AR_SESSION, 1 << 30, 0] {
        let ask = PermissionRequest { id: 1, media: false, bits, origin: "https://meet.example/".into(), page: MEET.into(), load: 1 };
        assert_eq!(f.ask("t1", 1, A, &ask), Some(false), "{bits:#x}");
    }
    assert_eq!(f.ask("t1", 1, A, &PermissionRequest { id: 2, media: true, bits: 1 << 9, origin: "https://meet.example/".into(), page: MEET.into(), load: 1 }), Some(false));
    assert!(f.perms.question("t1").is_none());
    assert_eq!(requested(false, prompt::CAMERA_STREAM | prompt::CAMERA_PAN_TILT_ZOOM | prompt::MIC_STREAM), Some(vec![Permission::Camera, Permission::Microphone]));
    assert_eq!(requested(true, media::SCREEN_AUDIO | media::SCREEN_VIDEO), Some(vec![Permission::ScreenShare]));
}

#[test]
fn one_prompt_per_tab_shows_and_the_rest_wait_their_turn() {
    let mut f = Fixture::new();
    assert_eq!(f.ask("t1", 1, A, &request(1, Permission::Camera, MEET, MEET, 1)), None);
    assert_eq!(f.ask("t1", 1, A, &request(2, Permission::Location, MEET, MEET, 1)), None);
    assert_eq!(f.ask("t2", 2, A, &request(1, Permission::Notifications, NEWS, NEWS, 1)), None);
    assert_eq!(f.perms.question("t1").unwrap().question.title(), "meet.example wants to use your camera");
    assert_eq!(f.perms.question("t2").unwrap().question.title(), "news.example wants to show notifications");
    f.perms.choose(&f.store, "t1", Choice::Deny, 1);
    assert_eq!(f.perms.question("t1").unwrap().question.title(), "meet.example wants to know your location");
    // A flood is refused once a tab has enough waiting.
    for id in 3..10 { assert_eq!(f.ask("t1", 1, A, &request(id, Permission::Clipboard, MEET, MEET, 1)), None); }
    assert_eq!(f.ask("t1", 1, A, &request(99, Permission::Clipboard, MEET, MEET, 1)), Some(false));
}

#[test]
fn leaving_the_page_closing_the_tab_or_switching_spaces_answers_dont_allow_unremembered() {
    let mut f = Fixture::new();
    let pages = |load_one: u64| [Page { tab: "t1", serial: 1, load: load_one }, Page { tab: "t2", serial: 2, load: 1 }];
    assert!(f.perms.tick(&pages(1), "space-1").is_empty(), "the first tick only learns the Space");
    f.ask("t1", 1, A, &request(1, Permission::Camera, MEET, MEET, 1));
    f.ask("t2", 2, A, &request(7, Permission::Camera, NEWS, NEWS, 1));
    assert!(f.perms.tick(&pages(1), "space-1").is_empty());
    // t1 navigated: its question goes, answered Don't Allow.
    assert_eq!(f.perms.tick(&pages(2), "space-1"), [Reply { serial: 1, id: 1, allow: false }]);
    assert!(f.perms.question("t1").is_none() && f.perms.question("t2").is_some());
    // t2 closed: its view answers for itself.
    assert!(f.perms.tick(&pages(2)[..1], "space-1").is_empty());
    assert!(f.perms.question("t2").is_none());
    // A Space switch answers everything still waiting.
    f.ask("t1", 1, A, &request(2, Permission::Location, MEET, MEET, 2));
    f.ask("t1", 1, A, &request(3, Permission::Notifications, MEET, MEET, 2));
    assert_eq!(f.perms.tick(&pages(2), "space-2"), [Reply { serial: 1, id: 2, allow: false }, Reply { serial: 1, id: 3, allow: false }]);
    assert!(f.perms.question("t1").is_none());
    assert!(rules::records(&f.store, A).unwrap().is_empty(), "nothing was remembered");
    // CEF dropping a request removes its question.
    f.ask("t1", 1, A, &request(4, Permission::Camera, MEET, MEET, 2));
    f.perms.dismissed(1, 4);
    assert!(f.perms.question("t1").is_none());
}

#[test]
fn a_remembered_block_marks_the_tab_until_its_next_page() {
    let mut f = Fixture::new();
    rules::set_decision(&f.store, A, "https://meet.example", "https://meet.example", Permission::Camera, Some(Decision::Block), 1).unwrap();
    rules::set_decision(&f.store, A, "https://meet.example", "https://meet.example", Permission::Location, Some(Decision::Block), 1).unwrap();
    assert_eq!(f.ask("t1", 1, A, &request(1, Permission::Camera, MEET, MEET, 1)), Some(false));
    assert_eq!(f.ask("t1", 1, A, &request(2, Permission::Location, MEET, MEET, 1)), Some(false));
    assert_eq!(f.perms.blocked("t1"), Some(&Blocked { load: 1, origin: "https://meet.example".into(), permissions: vec![Permission::Camera, Permission::Location] }));
    f.perms.tick(&[Page { tab: "t1", serial: 1, load: 1 }], "s");
    assert!(f.perms.blocked("t1").is_some());
    f.perms.tick(&[Page { tab: "t1", serial: 1, load: 2 }], "s");
    assert!(f.perms.blocked("t1").is_none());
}

#[test]
fn each_profile_keeps_its_own_decisions() {
    let mut f = Fixture::new();
    f.ask("t1", 1, A, &request(1, Permission::Camera, MEET, MEET, 1));
    f.perms.choose(&f.store, "t1", Choice::Allow, 1);
    assert_eq!(f.ask("t1", 1, A, &request(2, Permission::Camera, MEET, MEET, 1)), Some(true));
    assert_eq!(f.ask("t2", 2, B, &request(3, Permission::Camera, MEET, MEET, 1)), None, "another profile is asked");
    assert!(!f.perms.remembered_only(&f.store, B, &request(4, Permission::Camera, MEET, MEET, 1)));
    assert!(f.perms.remembered_only(&f.store, A, &request(5, Permission::Camera, MEET, MEET, 1)));
}

#[test]
fn a_failed_save_still_answers_the_page_keeps_the_old_decision_and_says_so() {
    let mut f = Fixture::new();
    rules::set_decision(&f.store, A, "https://meet.example", "https://meet.example", Permission::Camera, Some(Decision::Allow), 1).unwrap();
    rules::set_decision(&f.store, A, "https://meet.example", "https://meet.example", Permission::Camera, None, 1).unwrap();
    f.sql("CREATE TRIGGER reject BEFORE INSERT ON site_permissions BEGIN SELECT RAISE(ABORT, 'fixture denial'); END;").unwrap();
    f.ask("t1", 1, A, &request(1, Permission::Camera, MEET, MEET, 1));
    let outcome = f.perms.choose(&f.store, "t1", Choice::Allow, 2);
    assert_eq!(outcome.reply, reply(1, true));
    assert_eq!(outcome.notice.as_deref(), Some("Couldn't remember this for meet.example. It applies to this page only; nothing saved before changed."));
    assert_eq!(f.ask("t1", 1, A, &request(2, Permission::Camera, MEET, MEET, 1)), None, "still Ask");
}

#[test]
fn a_clicked_link_asks_before_opening_its_app_and_always_allow_skips_the_question() {
    let mut f = Fixture::new();
    assert_eq!(f.link("t1", &link(ZOOM, true, MEET, 1), true, 0.0), None);
    let shown = f.perms.question("t1").unwrap();
    assert_eq!(shown.question.title(), "Open Zoom?");
    assert_eq!(shown.question.detail().as_deref(), Some("meet.example wants to open Zoom."));
    assert!(!shown.remember, "Always allow is off by default");
    assert!(matches!(shown.question, Question::Link { can_always: true, .. }));
    assert!(f.apps.opened().is_empty(), "nothing opens before Open");
    // Cancel opens nothing.
    f.perms.choose(&f.store, "t1", Choice::Deny, 1);
    assert!(f.apps.opened().is_empty());
    // Open, once.
    f.link("t1", &link(ZOOM, true, MEET, 1), true, 1.0);
    assert_eq!(f.perms.choose(&f.store, "t1", Choice::Allow, 2), Outcome::default());
    assert_eq!(f.apps.opened(), [ZOOM]);
    f.link("t1", &link(ZOOM, true, MEET, 1), true, 2.0);
    assert!(f.perms.question("t1").is_some(), "asked again");
    // Open with Always allow: from then on a click opens it directly.
    f.perms.question_mut("t1").unwrap().remember = true;
    f.perms.choose(&f.store, "t1", Choice::Allow, 3);
    f.reopen();
    assert!(rules::opens_links(&f.store, A, "https://meet.example", "zoommtg").unwrap());
    assert_eq!(f.link("t1", &link(ZOOM, true, MEET, 2), true, 3.0), None);
    assert!(f.perms.question("t1").is_none());
    assert_eq!(f.apps.opened().len(), 3);
    // Per site and per scheme.
    f.apps.add("msteams", "Microsoft Teams", "com.microsoft.teams2");
    f.link("t1", &link("msteams:/l/meetup-join/fixture", true, MEET, 2), true, 4.0);
    assert_eq!(f.perms.question("t1").unwrap().question.title(), "Open Microsoft Teams?");
    f.perms.choose(&f.store, "t1", Choice::Deny, 4);
    f.link("t2", &link(ZOOM, true, NEWS, 1), true, 4.0);
    assert!(f.perms.question("t2").is_some());
    // Removing the grant asks again.
    rules::set_opens_links(&f.store, A, "https://meet.example", "zoommtg", false, 5).unwrap();
    f.link("t1", &link(ZOOM, true, MEET, 2), true, 5.0);
    assert!(f.perms.question("t1").is_some());
}

#[test]
fn a_link_without_a_click_shows_a_quiet_notice_rate_limited_per_tab() {
    let mut f = Fixture::new();
    assert_eq!(f.link("t1", &link(ZOOM, false, MEET, 1), true, 0.0), None);
    assert!(f.perms.question("t1").is_none(), "no prompt without a click");
    assert!(f.apps.opened().is_empty());
    assert_eq!(f.perms.indicator("t1").unwrap().text(), "meet.example tried to open Zoom");
    // Within 10 seconds a second one is dropped; the first stays.
    f.perms.dismiss_indicator("t1");
    f.link("t1", &link(ZOOM, false, MEET, 1), true, 9.0);
    assert!(f.perms.indicator("t1").is_none());
    // Another tab has its own limit.
    f.link("t2", &link(ZOOM, false, NEWS, 1), true, 9.0);
    assert!(f.perms.indicator("t2").is_some());
    f.link("t1", &link(ZOOM, false, MEET, 1), true, 10.5);
    assert!(f.perms.indicator("t1").is_some());
    // Its button opens the app: that click is the consent.
    assert_eq!(f.perms.open_indicator("t1"), None);
    assert_eq!(f.apps.opened(), [ZOOM]);
    assert!(f.perms.indicator("t1").is_none());
    // A new page clears an unanswered notice.
    f.perms.tick(&[Page { tab: "t2", serial: 1, load: 2 }], "s");
    assert!(f.perms.indicator("t2").is_none());
}

#[test]
fn always_allow_without_a_click_opens_once_per_page_load_from_the_focused_tab_only() {
    let mut f = Fixture::new();
    rules::set_opens_links(&f.store, A, "https://meet.example", "zoommtg", true, 1).unwrap();
    // The meeting page opens Zoom by itself: allowed once on this load.
    f.link("t1", &link(ZOOM, false, MEET, 1), true, 0.0);
    assert_eq!(f.apps.opened().len(), 1);
    assert!(f.perms.indicator("t1").is_none());
    // A second launch on the same page load falls back to the notice, even after 10 seconds.
    f.link("t1", &link(ZOOM, false, MEET, 1), true, 30.0);
    assert_eq!(f.apps.opened().len(), 1);
    assert!(f.perms.indicator("t1").is_some());
    // The next page load opens it again, but not within 10 seconds of the last.
    f.perms.dismiss_indicator("t1");
    f.link("t1", &link(ZOOM, false, MEET, 2), true, 32.0);
    assert_eq!(f.apps.opened().len(), 2);
    f.link("t1", &link(ZOOM, false, MEET, 3), true, 36.0);
    assert_eq!(f.apps.opened().len(), 2);
    assert!(f.perms.indicator("t1").is_none(), "the notice is rate limited too");
    f.link("t1", &link(ZOOM, false, MEET, 4), true, 43.0);
    assert_eq!(f.apps.opened().len(), 3);
    // A background tab never opens it by itself.
    f.link("t2", &link(ZOOM, false, MEET, 1), false, 60.0);
    assert_eq!(f.apps.opened().len(), 3);
    assert_eq!(f.perms.indicator("t2").unwrap().text(), "meet.example tried to open Zoom");
    // A click still opens it directly.
    f.link("t2", &link(ZOOM, true, MEET, 1), false, 61.0);
    assert_eq!(f.apps.opened().len(), 4);
}

#[test]
fn a_frame_from_another_site_never_uses_always_allow() {
    let mut f = Fixture::new();
    rules::set_opens_links(&f.store, A, "https://meet.example", "zoommtg", true, 1).unwrap();
    let framed = ExternalLink { frame: "https://ads.example/frame".into(), ..link(ZOOM, true, MEET, 1) };
    f.link("t1", &framed, true, 0.0);
    assert!(f.apps.opened().is_empty());
    assert!(matches!(f.perms.question("t1").unwrap().question, Question::Link { can_always: false, .. }));
    f.perms.question_mut("t1").unwrap().remember = true;
    f.perms.choose(&f.store, "t1", Choice::Allow, 2);
    assert_eq!(f.apps.opened().len(), 1);
    f.link("t1", &ExternalLink { gesture: false, ..framed }, true, 20.0);
    assert_eq!(f.apps.opened().len(), 1, "without a click it only gets the notice");
    assert!(f.perms.indicator("t1").is_some());
}

#[test]
fn dangerous_links_and_apps_are_refused_without_a_prompt() {
    let mut f = Fixture::new();
    for url in ["file:///Applications/Calculator.app", "x-apple.systempreferences:com.apple.preference.security", "applescript://com.apple.scripteditor?action=new", "shortcuts://run-shortcut?name=fixture", "x-man-page://ls", "vbscript:fixture", "ssh://fixture.example", "smb://fixture.example/share", "itms-services://?action=download-manifest"] {
        for gesture in [true, false] {
            assert_eq!(f.link("t1", &link(url, gesture, MEET, 1), true, 0.0), None, "{url}");
        }
    }
    // An app that runs commands, and Crossbow itself.
    for url in ["x-fixture-shell://run", "x-own://open"] {
        f.link("t1", &link(url, true, MEET, 1), true, 0.0);
    }
    assert!(f.apps.opened().is_empty());
    assert!(f.perms.question("t1").is_none() && f.perms.indicator("t1").is_none());
    assert_eq!(f.apps.0.borrow().lookups, ["x-fixture-shell", "x-own"], "refused schemes aren't even looked up");
}

#[test]
fn a_link_no_app_opens_says_so_and_hands_off_nothing() {
    let mut f = Fixture::new();
    f.link("t1", &link("x-fixture-nobody://x", true, MEET, 1), true, 0.0);
    let shown = f.perms.question("t1").unwrap();
    assert_eq!(shown.question.title(), "No app on this Mac can open this link");
    assert_eq!(f.perms.choose(&f.store, "t1", Choice::Deny, 1), Outcome::default());
    f.link("t2", &link("x-fixture-nobody://x", false, MEET, 1), true, 0.0);
    assert!(f.perms.question("t2").is_none() && f.perms.indicator("t2").is_none());
    assert!(f.apps.opened().is_empty());
}

#[test]
fn a_newer_link_replaces_the_older_question_but_permission_prompts_keep_their_turn() {
    let mut f = Fixture::new();
    f.apps.add("msteams", "Microsoft Teams", "com.microsoft.teams2");
    f.ask("t1", 1, A, &request(1, Permission::Camera, MEET, MEET, 1));
    f.link("t1", &link(ZOOM, true, MEET, 1), true, 0.0);
    f.link("t1", &link("msteams:/l/fixture", true, MEET, 1), true, 0.0);
    assert_eq!(f.perms.question("t1").unwrap().question.title(), "meet.example wants to use your camera");
    f.perms.choose(&f.store, "t1", Choice::Deny, 1);
    assert_eq!(f.perms.question("t1").unwrap().question.title(), "Open Microsoft Teams?");
    f.perms.choose(&f.store, "t1", Choice::Deny, 1);
    assert!(f.perms.question("t1").is_none());
}

#[test]
fn a_refused_open_or_a_failed_grant_save_says_so() {
    let mut f = Fixture::new();
    f.apps.0.borrow_mut().refuse_open = true;
    f.link("t1", &link(ZOOM, true, MEET, 1), true, 0.0);
    assert_eq!(f.perms.choose(&f.store, "t1", Choice::Allow, 1).notice.as_deref(), Some("Couldn't open Zoom."));
    f.apps.0.borrow_mut().refuse_open = false;
    f.sql("CREATE TRIGGER reject BEFORE INSERT ON site_permissions BEGIN SELECT RAISE(ABORT, 'fixture denial'); END;").unwrap();
    f.link("t1", &link(ZOOM, true, MEET, 1), true, 0.0);
    f.perms.question_mut("t1").unwrap().remember = true;
    assert_eq!(f.perms.choose(&f.store, "t1", Choice::Allow, 2).notice.as_deref(), Some("Couldn't remember this for meet.example. It will ask again next time."));
    assert_eq!(f.apps.opened().len(), 2, "it still opened");
    assert!(!rules::opens_links(&f.store, A, "https://meet.example", "zoommtg").unwrap());
}

#[test]
fn a_new_prompt_ignores_clicks_for_a_moment() {
    let mut f = Fixture::new();
    f.ask("t1", 1, A, &request(1, Permission::Camera, MEET, MEET, 1));
    let waiting = f.perms.question_mut("t1").unwrap();
    assert!(!waiting.ready(100.0), "not ready before it showed");
    waiting.shown_at = Some(100.0);
    assert!(!waiting.ready(100.3));
    assert!(waiting.ready(100.0 + INPUT_DELAY));
}

/// Every view hands its requests and links over, and every surface shows or
/// answers them. A WebView needs CEF, so this is checked in the source.
#[test]
fn every_view_hands_its_requests_and_links_over() {
    let app = include_str!("app.rs");
    let events = &app[app.find("fn process_view_events(").unwrap()..];
    let events = &events[..events.find("\n    }\n").unwrap()];
    assert!(events.contains("WebEvent::Permission(request) =>") && events.contains("self.site_permissions.request(&self.lib.store, page, profile, &request)") && events.contains("view.answer_permission(request.id, allow)"));
    assert!(events.contains("WebEvent::PermissionDismissed(id) => self.site_permissions.dismissed("));
    assert!(events.contains("self.site_permissions.link(&self.lib.store, page, profile, &link, focused, now)"));
    assert!(events.contains("let front = self.focused_tab_id().filter(|tab| window_focused && self.ui.panes.iter().any(|(id, _)| id == tab));"), "focused means the window, the tab and its pane");
    assert!(events.contains("self.site_permissions.tick(&pages, &self.active_space)") && events.contains("view.answer_permission(reply.id, reply.allow)"));
    assert!(events.contains("remembered_only(&self.lib.store, peek.view.profile_id(), &request)") && events.contains("notices.extend(self.site_permissions.take_notices())") && events.contains("FROM_TABS_ONLY"), "Peek");
    let little = include_str!("little.rs");
    assert!(little.contains("remembered_only(&self.lib.store, view.profile_id(), &request)") && little.contains("WebEvent::ExternalLink(_) => self.littles[index].notice = Some(crate::site_permissions::FROM_TABS_ONLY.into())"), "Little Windows");
    assert!(app.contains("ui::site_permissions::dismiss_focused_prompt(ctx, &mut self.site_permissions, &self.lib.store)"), "Escape");
    assert!(include_str!("ui/overlays.rs").contains("crate::ui::site_permissions::prompts(app, ui, &pal);"));
    let (sidebar, content) = (include_str!("ui/sidebar.rs"), include_str!("ui/content.rs"));
    assert!(sidebar.contains("if !floating { app.ui.address = Some(pill); }") && sidebar.contains("site_permissions::blocked_button(app, ui, pill_control(pill, usize::from(key))"));
    assert!(content.contains("app.ui.address = None;") && content.contains("app.ui.address = Some(row.trailing_control(0));") && content.contains("site_permissions::blocked_button(app, ui, row.trailing_control(usize::from(key))"));
}

#[test]
fn camera_microphone_and_location_are_refused_while_the_build_does_not_declare_them_to_macos() {
    let mut f = Fixture::new();
    f.apps.0.borrow_mut().undeclared = vec!["NSCameraUsageDescription".into(), "NSLocationWhenInUseUsageDescription".into()];
    // Allowing would let macOS end Crossbow for touching the camera undeclared.
    assert_eq!(f.ask("t1", 1, A, &request(1, Permission::Camera, MEET, MEET, 1)), Some(false));
    assert!(f.perms.question("t1").is_none());
    assert_eq!(f.perms.take_notices(), ["Camera access isn't available in this build of Crossbow."]);
    assert_eq!(f.ask("t1", 1, A, &request(2, Permission::Camera, MEET, MEET, 1)), Some(false));
    assert!(f.perms.take_notices().is_empty(), "said once");
    // Even a remembered Allow, in a tab or in Peek and Little Windows.
    rules::set_decision(&f.store, A, "https://meet.example", "https://meet.example", Permission::Camera, Some(Decision::Allow), 1).unwrap();
    assert_eq!(f.ask("t1", 1, A, &request(3, Permission::Camera, MEET, MEET, 1)), Some(false));
    assert!(!f.perms.remembered_only(&f.store, A, &request(4, Permission::Camera, MEET, MEET, 1)));
    // Camera and microphone together are refused whole.
    let both = PermissionRequest { id: 5, media: true, bits: media::CAMERA | media::MICROPHONE, origin: MEET.into(), page: MEET.into(), load: 1 };
    assert_eq!(f.ask("t1", 1, A, &both), Some(false));
    // The microphone is declared; location is declared under its older key.
    assert_eq!(f.ask("t1", 1, A, &request(6, Permission::Microphone, MEET, MEET, 1)), None);
    f.perms.choose(&f.store, "t1", Choice::Deny, 1);
    assert_eq!(f.ask("t1", 1, A, &request(7, Permission::Location, MEET, MEET, 1)), None);
    f.apps.0.borrow_mut().undeclared.push("NSLocationUsageDescription".into());
    assert_eq!(f.ask("t2", 2, A, &request(8, Permission::Location, MEET, MEET, 1)), Some(false));
    assert_eq!(f.perms.take_notices(), ["Location access isn't available in this build of Crossbow."]);
    // Nothing else needs a key.
    assert!(Permission::ALL.iter().filter(|permission| !usage_keys(**permission).is_empty()).eq([Permission::Camera, Permission::Microphone, Permission::Location].iter()));
}
