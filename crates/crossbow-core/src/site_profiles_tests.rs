use super::*;
use crate::model::Theme;

fn site(input: &str) -> Site {
    Site::parse(input, false, false).unwrap_or_else(|error| panic!("{input:?}: {error:?}"))
}

fn rule(host: &str, subdomains: bool, profile: &str) -> SiteRule {
    SiteRule { site: Site::parse(host, subdomains, true).unwrap(), profile: profile.into() }
}

fn rules(list: &[SiteRule]) -> SiteProfiles {
    SiteProfiles { rules: list.to_vec(), notice: None }
}

/// Personal and Work profiles with a Space each; Work also owns Work 2.
struct World {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    lib: Library,
    personal: String,
    work: String,
    personal_space: String,
    work_space: String,
    work_space_2: String,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
    lib.ensure_defaults().unwrap();
    let personal_space = lib.spaces_ordered()[0].0.clone();
    let personal = lib.spaces[&personal_space].profile_id.clone();
    let work = lib.create_profile("Work").unwrap();
    let work_space = lib.create_space("Work", "W", Theme::preset(1), &work).unwrap();
    let work_space_2 = lib.create_space("Work 2", "2", Theme::preset(2), &work).unwrap();
    World { _dir: dir, path, lib, personal, work, personal_space, work_space, work_space_2 }
}

impl World {
    fn read_only(&self) -> Store {
        let path = self.path.to_string_lossy().replace('%', "%25").replace('?', "%3F").replace('#', "%23");
        Store::open(std::path::Path::new(&format!("file:{path}?mode=ro"))).unwrap()
    }
}

#[test]
fn typed_sites_become_one_normal_host() {
    for (input, host) in [
        ("github.com", "github.com"),
        ("GitHub.COM", "github.com"),
        ("github.com.", "github.com"),
        ("www.github.com", "github.com"),
        ("  gist.github.com ", "gist.github.com"),
        ("https://github.com/rust-lang/rust?tab=readme#top", "github.com"),
        ("http://github.com:8080/", "github.com"),
        ("bücher.de", "xn--bcher-kva.de"),
        ("xn--bcher-kva.de", "xn--bcher-kva.de"),
        ("BÜCHER.de", "xn--bcher-kva.de"),
        ("my_host.example.com", "my_host.example.com"),
        ("www.com", "www.com"),
    ] {
        assert_eq!(site(input), Site { host: host.into(), subdomains: false }, "{input:?}");
    }
    assert_eq!(Site::parse("github.com", true, false), Ok(Site { host: "github.com".into(), subdomains: true }));
    assert_eq!(Site::parse("*.github.com", false, false), Ok(Site { host: "github.com".into(), subdomains: true }), "*. asks for subdomains");
    assert_eq!(site("bücher.de").name(), "bücher.de", "shown in its own script");
    assert_eq!(Site::parse("github.com", true, false).unwrap().label(), "github.com and subdomains");

    for input in ["", "   "] {
        assert_eq!(Site::parse(input, false, false), Err(SiteError::Empty));
    }
    for input in ["github", "git hub.com", "-bad.com", "bad-.com", "a..com", "*", "*.", "gith*b.com", "ftp://github.com", "mailto:a@github.com", "file:///tmp/a.html", "user:pw@github.com", "https://user@github.com/", &format!("{}.com", "a".repeat(64)), &format!("{}com", "a.".repeat(130))] {
        assert_eq!(Site::parse(input, false, false), Err(SiteError::Invalid), "{input:?}");
    }
    // IP addresses and this computer only when asked again.
    for (input, host, ip) in [("192.168.0.1", "192.168.0.1", true), ("http://10.0.0.2:3000/x", "10.0.0.2", true), ("[::1]", "[::1]", true), ("localhost", "localhost", false), ("app.localhost", "app.localhost", false)] {
        let refused = Site::parse(input, true, false).unwrap_err();
        assert_eq!(refused, SiteError::Local { host: host.into(), ip }, "{input:?}");
        assert!(refused.message().contains("Add it anyway?"));
        let added = Site::parse(input, true, true).unwrap();
        assert_eq!((added.host.as_str(), added.subdomains), (host, !ip), "an address never has subdomains: {input:?}");
    }
}

#[test]
fn links_match_an_exact_host_or_its_subdomains_and_the_most_specific_rule_wins() {
    let sites = rules(&[rule("github.com", true, "work"), rule("gist.github.com", false, "personal"), rule("example.org", false, "work"), rule("bücher.de", false, "personal"), rule("127.0.0.1", false, "dev")]);
    let owner = |url: &str| sites.rule_for(url).map(|rule| rule.profile.as_str());
    for (url, expected) in [
        ("https://github.com/rust-lang/rust", Some("work")),
        ("https://GITHUB.com./", Some("work")),
        ("https://www.github.com/", Some("work")),
        ("http://github.com:8443/login", Some("work")),
        ("https://api.github.com/repos", Some("work")),
        ("https://gist.github.com/x", Some("personal")),
        ("https://a.gist.github.com/x", Some("work")),
        ("https://notgithub.com/", None),
        ("https://github.com.evil.example/", None),
        ("https://example.org/", Some("work")),
        ("https://docs.example.org/", None),
        ("https://xn--bcher-kva.de/", Some("personal")),
        ("https://BÜCHER.de/kaufen", Some("personal")),
        ("http://127.0.0.1:8080/", Some("dev")),
        ("file:///tmp/github.com.html", None),
        ("not a link", None),
    ] {
        assert_eq!(owner(url), expected, "{url}");
    }
}

#[test]
fn a_rule_whose_profile_is_missing_routes_nothing() {
    let w = world();
    let sites = rules(&[rule("github.com", true, &w.work), rule("gist.github.com", false, "gone"), rule("example.org", false, "gone")]);
    assert_eq!(sites.route(&w.lib, "https://github.com/").map(|rule| rule.profile.as_str()), Some(w.work.as_str()));
    assert_eq!(sites.route(&w.lib, "https://example.org/"), None);
    assert_eq!(sites.route(&w.lib, "https://gist.github.com/"), None, "never a broader rule's profile instead");
    assert!(sites.rule_for("https://gist.github.com/").is_some(), "Settings still lists it");
}

#[test]
fn sites_are_saved_with_one_owner_and_read_back_after_reopening() {
    let w = world();
    let mut sites = SiteProfiles::load(&w.lib.store);
    assert!(sites.is_empty() && sites.notice.is_none());
    sites.add(&w.lib.store, rule("github.com", false, &w.work), false).unwrap();
    sites.add(&w.lib.store, rule("bücher.de", false, &w.personal), false).unwrap();
    // The same profile adding it again changes its subdomains.
    sites.add(&w.lib.store, rule("github.com", true, &w.work), false).unwrap();
    assert_eq!(sites.rules().len(), 2);
    // Another profile's site moves only when asked.
    assert_eq!(sites.add(&w.lib.store, rule("github.com", false, &w.personal), false), Err(AddError::Taken(rule("github.com", true, &w.work))));
    assert_eq!(sites.owner("github.com").unwrap().profile, w.work);

    let reopened = SiteProfiles::load(&Store::open(&w.path).unwrap());
    assert_eq!(reopened.rules(), sites.rules(), "read back after reopening");
    assert!(reopened.notice.is_none());

    sites.add(&w.lib.store, rule("github.com", false, &w.personal), true).unwrap();
    assert_eq!(SiteProfiles::load(&Store::open(&w.path).unwrap()).owner("github.com"), Some(&rule("github.com", false, &w.personal)), "moved");
    sites.remove(&w.lib.store, "github.com").unwrap();
    assert_eq!(SiteProfiles::load(&Store::open(&w.path).unwrap()).rules(), [rule("bücher.de", false, &w.personal)]);

    // A failed save keeps what is in use and says why.
    let read_only = w.read_only();
    let before = sites.rules().to_vec();
    assert!(matches!(sites.add(&read_only, rule("example.org", false, &w.work), false), Err(AddError::Save(_))));
    assert!(sites.remove(&read_only, "bücher.de").is_err());
    assert_eq!(sites.rules(), before.as_slice());
    assert_eq!(SiteProfiles::load(&Store::open(&w.path).unwrap()).rules(), before.as_slice());
}

#[test]
fn unreadable_or_newer_records_route_nothing_and_say_why() {
    let w = world();
    for (raw, notice, kept) in [
        ("{not json", UNREADABLE, 0),
        (r#"{"version":2,"rules":[]}"#, NEWER, 0),
        (r#"{"rules":[]}"#, UNREADABLE, 0),
        (r#"{"version":1,"rules":[{"host":"github.com","subdomains":false,"profile":"p"},{"host":"Bad Host","subdomains":false,"profile":"p"},{"host":"github.com","subdomains":true,"profile":"q"},{"host":"example.org","profile":"p"},{"host":"example.net","subdomains":false,"profile":""}]}"#, PARTLY, 1),
    ] {
        w.lib.store.kv_set(KEY, raw).unwrap();
        let sites = SiteProfiles::load(&w.lib.store);
        assert_eq!((sites.notice.as_deref(), sites.rules().len()), (Some(notice), kept), "{raw}");
    }
    // Saving replaces the record and clears the notice.
    let mut sites = SiteProfiles::load(&w.lib.store);
    sites.add(&w.lib.store, rule("example.org", false, &w.work), false).unwrap();
    let reopened = SiteProfiles::load(&w.lib.store);
    assert_eq!((reopened.notice.as_deref(), reopened.rules().len()), (None, 2));
}

#[test]
fn deleting_a_profile_removes_its_sites_in_the_same_transaction() {
    let mut w = world();
    let mut sites = SiteProfiles::default();
    sites.add(&w.lib.store, rule("github.com", true, &w.work), false).unwrap();
    sites.add(&w.lib.store, rule("example.org", false, &w.personal), false).unwrap();
    // Refused while Spaces use it: the profile and its sites stay.
    assert!(w.lib.delete_profile(&w.work).is_err());
    assert_eq!(SiteProfiles::load(&w.lib.store).rules().len(), 2);
    w.lib.delete_space(&w.work_space).unwrap();
    w.lib.delete_space(&w.work_space_2).unwrap();
    // A store that can't be written: nothing is deleted.
    let mut locked = Library::open(w.read_only()).unwrap();
    assert!(locked.delete_profile(&w.work).is_err());
    assert_eq!(SiteProfiles::load(&w.lib.store).rules().len(), 2);
    assert!(Library::open(Store::open(&w.path).unwrap()).unwrap().profiles.contains_key(&w.work));

    w.lib.delete_profile(&w.work).unwrap();
    let reopened = Library::open(Store::open(&w.path).unwrap()).unwrap();
    assert!(!reopened.profiles.contains_key(&w.work));
    assert_eq!(SiteProfiles::load(&reopened.store).rules(), [rule("example.org", false, &w.personal)], "only the deleted profile's sites went");
    forget_profile(&w.lib.store, "never-had-rules").unwrap();
    w.lib.store.kv_set(KEY, "{not json").unwrap();
    forget_profile(&w.lib.store, &w.personal).unwrap();
    assert_eq!(w.lib.store.kv_get(KEY).unwrap().as_deref(), Some("{not json"), "an unreadable record is left alone");
}

#[test]
fn sites_open_in_the_profiles_most_recent_space_else_its_first() {
    let mut w = world();
    assert_eq!(target_space(&w.lib, &w.work, []), Some(w.work_space.clone()), "first in sidebar order");
    let recent = [(w.work_space.as_str(), 10), (w.work_space_2.as_str(), 20), (w.personal_space.as_str(), 99)];
    assert_eq!(target_space(&w.lib, &w.work, recent), Some(w.work_space_2.clone()), "its own Spaces only");
    assert_eq!(target_space(&w.lib, &w.personal, recent), Some(w.personal_space.clone()));
    w.lib.delete_space(&w.work_space_2).unwrap();
    assert_eq!(target_space(&w.lib, &w.work, [(w.work_space_2.as_str(), 20)]), Some(w.work_space.clone()), "a deleted Space is skipped");
    w.lib.delete_space(&w.work_space).unwrap();
    assert_eq!(target_space(&w.lib, &w.work, []), None, "no Space has the profile");
    assert_eq!(target_space(&w.lib, "gone", []), None);
}
