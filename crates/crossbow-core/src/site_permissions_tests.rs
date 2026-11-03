//! Site permissions on disposable library files. Every origin is a fixture.

use super::*;
use crate::library::Library;

const A: &str = "fixture-profile-a";
const B: &str = "fixture-profile-b";
const SITE: &str = "https://meet.example";
const OTHER: &str = "https://news.example";

fn store(dir: &tempfile::TempDir) -> Store {
    Store::open(&dir.path().join("library.sqlite")).unwrap()
}

#[test]
fn decisions_are_per_profile_origin_top_level_site_and_permission_and_survive_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(&dir);
    for permission in Permission::ALL {
        assert_eq!(decision(&s, A, SITE, SITE, permission).unwrap(), None, "Ask until decided: {permission:?}");
    }
    set_decision(&s, A, SITE, SITE, Permission::Camera, Some(Decision::Allow), 1).unwrap();
    set_decision(&s, A, SITE, SITE, Permission::Location, Some(Decision::Block), 2).unwrap();
    drop(s);
    let s = store(&dir);
    assert_eq!(decision(&s, A, SITE, SITE, Permission::Camera).unwrap(), Some(Decision::Allow));
    assert_eq!(decision(&s, A, SITE, SITE, Permission::Location).unwrap(), Some(Decision::Block));
    assert_eq!(decision(&s, A, SITE, SITE, Permission::Microphone).unwrap(), None, "per permission");
    assert_eq!(decision(&s, B, SITE, SITE, Permission::Camera).unwrap(), None, "per profile");
    assert_eq!(decision(&s, A, "https://meet.example:8443", "https://meet.example:8443", Permission::Camera).unwrap(), None, "per port");
    assert_eq!(decision(&s, A, "http://meet.example", "http://meet.example", Permission::Camera).unwrap(), None, "per scheme");
    // The site's grant as a page never reaches it as a frame on another site, nor the reverse.
    assert_eq!(decision(&s, A, SITE, OTHER, Permission::Camera).unwrap(), None);
    set_decision(&s, A, SITE, OTHER, Permission::Microphone, Some(Decision::Allow), 3).unwrap();
    assert_eq!(decision(&s, A, SITE, SITE, Permission::Microphone).unwrap(), None);
    // Changing and forgetting.
    set_decision(&s, A, SITE, SITE, Permission::Camera, Some(Decision::Block), 4).unwrap();
    assert_eq!(decision(&s, A, SITE, SITE, Permission::Camera).unwrap(), Some(Decision::Block));
    set_decision(&s, A, SITE, SITE, Permission::Camera, None, 5).unwrap();
    assert_eq!(decision(&s, A, SITE, SITE, Permission::Camera).unwrap(), None);
}

#[test]
fn decisions_stay_out_of_the_sync_stream() {
    let dir = tempfile::tempdir().unwrap();
    let mut lib = Library::open(store(&dir)).unwrap();
    lib.ensure_defaults().unwrap();
    let before = lib.store.all_entities().unwrap();
    let profile = lib.default_profile_id();
    set_decision(&lib.store, &profile, SITE, SITE, Permission::Notifications, Some(Decision::Allow), 1).unwrap();
    set_opens_links(&lib.store, &profile, SITE, "zoommtg", true, 2).unwrap();
    assert_eq!(lib.store.all_entities().unwrap(), before);
}

#[test]
fn records_list_every_decision_and_link_grant_and_resets_are_per_site_and_per_profile() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(&dir);
    set_decision(&s, A, SITE, SITE, Permission::Camera, Some(Decision::Allow), 1).unwrap();
    set_decision(&s, A, SITE, OTHER, Permission::Microphone, Some(Decision::Block), 1).unwrap();
    set_decision(&s, A, OTHER, OTHER, Permission::Notifications, Some(Decision::Block), 1).unwrap();
    set_opens_links(&s, A, SITE, "zoommtg", true, 1).unwrap();
    set_decision(&s, B, SITE, SITE, Permission::Camera, Some(Decision::Block), 1).unwrap();
    assert_eq!(records(&s, A).unwrap(), [
        SiteRecord { origin: SITE.into(), top_origin: SITE.into(), remembered: Remembered::Permission(Permission::Camera, Decision::Allow) },
        SiteRecord { origin: SITE.into(), top_origin: SITE.into(), remembered: Remembered::OpenLinks("zoommtg".into()) },
        SiteRecord { origin: SITE.into(), top_origin: OTHER.into(), remembered: Remembered::Permission(Permission::Microphone, Decision::Block) },
        SiteRecord { origin: OTHER.into(), top_origin: OTHER.into(), remembered: Remembered::Permission(Permission::Notifications, Decision::Block) },
    ]);
    // A row this build can't read is left out, not an error.
    s.connection().execute("INSERT INTO site_permissions VALUES (?1, ?2, ?2, 'vr-session', 'allow', 1), (?1, ?2, ?2, 'camera', 'maybe', 1)", params![A, "https://odd.example"]).unwrap();
    assert_eq!(records(&s, A).unwrap().len(), 4);
    assert_eq!(decision(&s, A, "https://odd.example", "https://odd.example", Permission::Camera).unwrap(), None, "an unreadable value is Ask");
    assert_eq!(reset_site(&s, A, SITE).unwrap(), 3, "the page's decisions, its link grant and its decisions as a frame");
    assert_eq!(records(&s, A).unwrap().len(), 1);
    assert!(!opens_links(&s, A, SITE, "zoommtg").unwrap());
    assert_eq!(decision(&s, B, SITE, SITE, Permission::Camera).unwrap(), Some(Decision::Block), "another profile keeps its own");
    reset_all(&s, A).unwrap();
    assert!(records(&s, A).unwrap().is_empty());
    assert_eq!(records(&s, B).unwrap().len(), 1);
}

#[test]
fn link_grants_are_per_profile_top_level_site_and_scheme() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(&dir);
    assert!(!opens_links(&s, A, SITE, "zoommtg").unwrap(), "off until chosen");
    set_opens_links(&s, A, SITE, "zoommtg", true, 1).unwrap();
    assert!(opens_links(&s, A, SITE, "zoommtg").unwrap());
    assert!(!opens_links(&s, A, SITE, "msteams").unwrap());
    assert!(!opens_links(&s, A, OTHER, "zoommtg").unwrap());
    assert!(!opens_links(&s, B, SITE, "zoommtg").unwrap());
    set_opens_links(&s, A, SITE, "zoommtg", false, 2).unwrap();
    assert!(!opens_links(&s, A, SITE, "zoommtg").unwrap());
}

#[test]
fn a_failed_write_leaves_the_old_decision() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(&dir);
    set_decision(&s, A, SITE, SITE, Permission::Camera, Some(Decision::Allow), 1).unwrap();
    s.connection().execute_batch("CREATE TRIGGER reject_change BEFORE UPDATE ON site_permissions BEGIN SELECT RAISE(ABORT, 'fixture denial'); END;").unwrap();
    assert!(set_decision(&s, A, SITE, SITE, Permission::Camera, Some(Decision::Block), 2).is_err());
    assert_eq!(decision(&s, A, SITE, SITE, Permission::Camera).unwrap(), Some(Decision::Allow));
}

#[test]
fn origins_are_exact_and_only_https_or_this_mac_is_secure() {
    for (url, origin) in [
        ("https://Meet.Example/room?id=1", Some("https://meet.example")), ("https://meet.example:443/", Some("https://meet.example")),
        ("https://meet.example:8443/x", Some("https://meet.example:8443")), ("http://localhost:3000/", Some("http://localhost:3000")),
        ("file:///tmp/a.html", None), ("data:text/html,x", None), ("about:blank", None), ("zoommtg://zoom.us/join", None),
    ] {
        assert_eq!(origin_of(url).as_deref(), origin, "{url}");
    }
    for (origin, secure) in [
        ("https://meet.example", true), ("http://meet.example", false), ("http://localhost:3000", true), ("http://app.localhost", true),
        ("http://127.0.0.1:8080", true), ("http://127.9.9.9", true), ("http://[::1]:8000", true), ("http://10.0.0.2", false),
        ("http://localhost.example", false), ("file:///tmp", false), ("", false),
    ] {
        assert_eq!(is_secure_origin(origin), secure, "{origin}");
    }
    assert!(Permission::ALL.iter().filter(|permission| permission.needs_secure_origin()).eq([Permission::Camera, Permission::Microphone, Permission::ScreenShare, Permission::Location].iter()));
}

#[test]
fn prompts_name_every_permission_in_plain_words() {
    assert_eq!(phrase(&[Permission::Camera]), "use your camera");
    assert_eq!(phrase(&[Permission::Camera, Permission::Microphone]), "use your camera and microphone");
    assert_eq!(phrase(&[Permission::Location, Permission::Notifications]), "know your location and show notifications");
    assert_eq!(phrase(&[Permission::Clipboard, Permission::Location, Permission::Notifications]), "see text and images you copy, know your location and show notifications");
    for permission in Permission::ALL {
        assert_eq!(Permission::from_key(permission.key()), Some(permission));
        assert!(!permission.label().is_empty() && !permission.phrase().is_empty());
    }
}

#[test]
fn dangerous_schemes_and_apps_are_refused_with_a_reason() {
    for url in ["file:///Applications/Calculator.app", "JavaScript:alert(1)", "vbscript:x", "data:text/html,x", "x-apple.systempreferences:com.apple.preference.security", "applescript://com.apple.scripteditor?action=new", "shortcuts://run-shortcut?name=x", "x-man-page://ls", "ssh://host", "smb://share", "help:runscript"] {
        let scheme = link_scheme(url).unwrap();
        assert!(refused_scheme(&scheme).is_some(), "{url}");
    }
    for url in ["zoommtg://zoom.us/join?confno=1", "msteams:/l/meetup-join/x", "mailto:someone@example.com", "slack://open", "tel:+15550100"] {
        let scheme = link_scheme(url).unwrap();
        assert!(refused_scheme(&scheme).is_none(), "{url}");
    }
    for bundle in ["com.apple.Terminal", "com.apple.terminal", "com.googlecode.iterm2", "com.apple.ScriptEditor2", "com.apple.shortcuts", "com.apple.installer", "com.apple.AppStore", "com.apple.systempreferences", "com.apple.finder"] {
        assert!(refused_handler(bundle).is_some(), "{bundle}");
    }
    for bundle in ["us.zoom.xos", "com.microsoft.teams2", "com.apple.mail", "com.tinyspeck.slackmacgap"] {
        assert!(refused_handler(bundle).is_none(), "{bundle}");
    }
    assert_eq!(link_scheme("zoommtg://x").as_deref(), Some("zoommtg"));
    assert_eq!(link_scheme("Web+Coffee:x").as_deref(), Some("web+coffee"));
    for malformed in ["", ":x", "1abc:x", "zoom mtg:x", "no-colon"] {
        assert_eq!(link_scheme(malformed), None, "{malformed}");
    }
}

#[test]
fn several_permissions_are_remembered_together_or_not_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(&dir);
    set_decisions(&s, A, SITE, SITE, &[Permission::Camera, Permission::Microphone], Some(Decision::Allow), 1).unwrap();
    assert_eq!(decision(&s, A, SITE, SITE, Permission::Camera).unwrap(), Some(Decision::Allow));
    assert_eq!(decision(&s, A, SITE, SITE, Permission::Microphone).unwrap(), Some(Decision::Allow));
    // The microphone's write fails after the camera's succeeded: neither changes.
    s.connection().execute_batch("CREATE TRIGGER reject_microphone BEFORE UPDATE ON site_permissions WHEN new.permission = 'microphone' BEGIN SELECT RAISE(ABORT, 'fixture denial'); END;").unwrap();
    assert!(set_decisions(&s, A, SITE, SITE, &[Permission::Camera, Permission::Microphone], Some(Decision::Block), 2).is_err());
    assert_eq!(decision(&s, A, SITE, SITE, Permission::Camera).unwrap(), Some(Decision::Allow));
    assert_eq!(decision(&s, A, SITE, SITE, Permission::Microphone).unwrap(), Some(Decision::Allow));
    s.connection().execute_batch("DROP TRIGGER reject_microphone;").unwrap();
    set_decisions(&s, A, SITE, SITE, &[Permission::Camera, Permission::Microphone], None, 3).unwrap();
    assert!(records(&s, A).unwrap().is_empty());
}
