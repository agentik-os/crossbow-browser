use super::*;
use crossbow_core::link_routing::{Destination, Match, Rule};
use crossbow_core::site_profiles::SiteRule;
use crossbow_core::store::TabRow;

/// Personal's Space uses Personal; Work and Work 2 use Work, and Work 2
/// was used last.
struct World {
    dir: tempfile::TempDir,
    lib: Library,
    sites: SiteProfiles,
    tabs: Vec<Tab>,
    personal: String,
    work: String,
    home: String,
    desk: String,
    desk_2: String,
}

fn tab(id: &str, space: &str, used: u64) -> Tab {
    Tab::from_row(TabRow {
        id: id.into(), space_id: space.into(), item_id: None, url: format!("https://{id}.example/"),
        title: id.into(), order: "V".into(), created_ms: 1, last_active_ms: used,
    })
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let mut lib = Library::open(Store::open(&dir.path().join("library.sqlite")).unwrap()).unwrap();
    lib.ensure_defaults().unwrap();
    let home = lib.spaces_ordered()[0].0.clone();
    let personal = lib.spaces[&home].profile_id.clone();
    let work = lib.create_profile("Work").unwrap();
    let desk = lib.create_space("Work", "W", crossbow_core::model::Theme::preset(1), &work).unwrap();
    let desk_2 = lib.create_space("Work 2", "2", crossbow_core::model::Theme::preset(2), &work).unwrap();
    let tabs = vec![tab("old", &desk, 2), tab("recent", &desk_2, 5), tab("mine", &home, 9)];
    World { dir, lib, sites: SiteProfiles::default(), tabs, personal, work, home, desk, desk_2 }
}

impl World {
    fn rule(&mut self, input: &str, subdomains: bool, profile: &str) {
        let site = Site::parse(input, subdomains, false).unwrap();
        self.sites.add(&self.lib.store, SiteRule { site, profile: profile.into() }, false).unwrap();
    }

    fn reopen(&self) -> Library {
        Library::open(Store::open(&self.dir.path().join("library.sqlite")).unwrap()).unwrap()
    }

    fn opened(&self, id: &str) -> &Tab {
        self.tabs.iter().find(|tab| tab.id == id).expect("the link has a tab")
    }

    /// The session saving its tabs.
    fn save_tabs(&self) {
        for tab in &self.tabs { self.lib.store.put_tab(&tab.row()).unwrap(); }
    }
}

fn atc(value: &str, space: &str) -> Rule {
    Rule::new(crossbow_core::new_id(), Match::HostIs, value, space.into()).unwrap()
}

#[test]
fn a_new_tab_for_a_site_with_a_rule_opens_in_its_profiles_most_recent_space() {
    let mut w = world();
    let work = w.work.clone();
    w.rule("github.com", true, &work);
    let url = "https://gist.github.com/rust-lang/1";
    assert_eq!(site_route(&w.lib, &w.sites, &w.tabs, url, &w.personal), SiteRoute::Elsewhere { space: w.desk_2.clone(), profile: w.work.clone() });
    assert_eq!(site_route(&w.lib, &w.sites, &w.tabs, url, &w.work), SiteRoute::Here, "already in its profile");
    assert_eq!(site_route(&w.lib, &w.sites, &w.tabs, "https://example.com/", &w.personal), SiteRoute::Here, "no rule");
    w.tabs[0].last_active_ms = 50;
    assert_eq!(site_route(&w.lib, &w.sites, &w.tabs, url, &w.personal), SiteRoute::Elsewhere { space: w.desk.clone(), profile: w.work.clone() }, "the Space used last");

    let Placement::Moved { tab, space, notice } = place_site_tab(&w.lib, &w.sites, &mut w.tabs, url, &w.personal, &w.home) else { panic!("moved") };
    assert_eq!(space, w.desk);
    let opened = w.opened(&tab);
    assert_eq!((opened.space_id.as_str(), opened.url.as_str(), opened.item_id.as_deref()), (w.desk.as_str(), url, None));
    assert_eq!(tab_profile(&w.lib, opened), Some(w.work.as_str()), "its web view opens in the site's profile");
    assert!(opened.order.as_str() < "V" && opened.dirty, "first in Today, and saved with the session");
    assert_eq!(notice, SiteNotice { text: "Opened in Work".into(), undo: Some(Undo { tab: tab.clone(), url: url.into(), space: w.home.clone() }), shown: None });

    let before = w.tabs.len();
    assert_eq!(place_site_tab(&w.lib, &w.sites, &mut w.tabs, "https://example.com/", &w.personal, &w.home), Placement::Unmoved(None));
    assert_eq!(place_site_tab(&w.lib, &w.sites, &mut w.tabs, url, &w.work, &w.desk), Placement::Unmoved(None));
    assert_eq!(w.tabs.len(), before, "the caller opens unmoved links as before");
}

#[test]
fn a_site_whose_profile_has_no_space_or_is_not_on_this_device_opens_where_it_was_going() {
    let mut w = world();
    let spare = w.lib.create_profile("Spare").unwrap();
    w.rule("example.org", false, &spare);
    let before = w.tabs.len();
    let Placement::Unmoved(Some(notice)) = place_site_tab(&w.lib, &w.sites, &mut w.tabs, "https://www.example.org/a", &w.personal, &w.home) else { panic!("a notice") };
    assert_eq!((notice.text.as_str(), notice.undo), ("example.org opens in Spare, which has no Space, so it opened here.", None));
    assert_eq!(w.tabs.len(), before);

    // A profile not on this device routes nothing and says nothing.
    w.rule("example.net", false, "not-on-this-device");
    assert_eq!(site_route(&w.lib, &w.sites, &w.tabs, "https://example.net/", &w.personal), SiteRoute::Here);
    assert_eq!(place_site_tab(&w.lib, &w.sites, &mut w.tabs, "https://example.net/", &w.personal, &w.home), Placement::Unmoved(None));

    // Deleting a profile takes its sites with it.
    w.lib.delete_profile(&spare).unwrap();
    w.sites = SiteProfiles::load(&w.lib.store);
    assert!(w.sites.owner("example.org").is_none() && w.sites.owner("example.net").is_some());
    assert_eq!(place_site_tab(&w.lib, &w.sites, &mut w.tabs, "https://example.org/", &w.personal, &w.home), Placement::Unmoved(None));
}

#[test]
fn links_from_other_apps_follow_air_traffic_control_rules_then_site_rules_then_its_default() {
    let mut w = world();
    let work = w.work.clone();
    w.rule("github.com", false, &work);
    w.rule("example.com", false, &work);

    // An Air Traffic Control rule wins over a site's rule.
    let routing = Routing { rules: vec![atc("github.com", &w.home)], default: Destination::MostRecent };
    let route = route_link(&w.lib, &routing, &w.sites, &w.tabs, &w.home, "https://github.com/a").unwrap();
    assert_eq!((route.space.as_str(), route.rule, route.fallback, &route.site), (w.home.as_str(), Some(0), false, &SiteRoute::Here));

    // A site's rule wins over the default; Undo returns to the default's Space.
    let route = route_link(&w.lib, &routing, &w.sites, &w.tabs, &w.home, "https://example.com/").unwrap();
    assert_eq!((route.space.as_str(), route.rule, route.from.as_str()), (w.desk_2.as_str(), None, w.home.as_str()));
    assert_eq!(route.site, SiteRoute::Elsewhere { space: w.desk_2.clone(), profile: w.work.clone() });
    let chosen = Routing { default: Destination::Space(w.home.clone()), ..routing.clone() };
    assert_eq!(route_link(&w.lib, &chosen, &w.sites, &w.tabs, &w.desk, "https://example.com/").unwrap().space, w.desk_2, "a chosen default too");
    let in_work = Routing { default: Destination::Space(w.desk.clone()), ..routing.clone() };
    let route = route_link(&w.lib, &in_work, &w.sites, &w.tabs, &w.home, "https://example.com/").unwrap();
    assert_eq!((route.space.as_str(), route.site), (w.desk.as_str(), SiteRoute::Here), "a default in the site's profile keeps it");
    let route = route_link(&w.lib, &routing, &w.sites, &w.tabs, &w.home, "https://elsewhere.example/").unwrap();
    assert_eq!((route.space.as_str(), route.site), (w.home.as_str(), SiteRoute::Here), "no rule: the default");

    // A rule whose Space is gone falls back, and then the site's rule applies.
    let gone = Routing { rules: vec![atc("github.com", "gone")], default: Destination::MostRecent };
    let route = route_link(&w.lib, &gone, &w.sites, &w.tabs, &w.home, "https://github.com/").unwrap();
    assert_eq!((route.space.as_str(), route.fallback), (w.desk_2.as_str(), true));

    // Tabs from other apps take the same route, with a notice and Undo.
    let placed = super::super::link_open::place_link(&w.lib, &routing, &w.sites, &mut w.tabs, &w.home, "https://example.com/x").unwrap();
    assert_eq!((placed.space.as_str(), placed.from.as_str()), (w.desk_2.as_str(), w.home.as_str()));
    assert_eq!(tab_profile(&w.lib, w.opened(&placed.tab)), Some(w.work.as_str()));
    let undo = Undo { tab: placed.tab.clone(), url: "https://example.com/x".into(), space: placed.from.clone() };
    let notice = notice_for(&w.lib, &placed.site, "https://example.com/x", Some(undo.clone())).unwrap();
    assert_eq!((notice.text.as_str(), notice.undo), ("Opened in Work", Some(undo)));
    assert_eq!(notice_for(&w.lib, &SiteRoute::Here, "https://example.com/x", None), None, "an unmoved link has no notice");
}

#[test]
fn undo_closes_the_moved_tab_without_archiving_and_reopens_the_link_where_it_was_going() {
    let mut w = world();
    let work = w.work.clone();
    w.rule("github.com", false, &work);
    let url = "https://github.com/rust-lang/rust";
    let Placement::Moved { tab, notice, .. } = place_site_tab(&w.lib, &w.sites, &mut w.tabs, url, &w.personal, &w.home) else { panic!("moved") };
    w.save_tabs();
    let undo = notice.undo.unwrap();
    let (removed, reopened) = undo_tabs(&w.lib, &mut w.tabs, &undo, &w.desk).unwrap();
    assert_eq!(removed.map(|tab| tab.id), Some(tab.clone()));
    assert!(!w.tabs.iter().any(|open| open.id == tab));
    let back = w.opened(&reopened);
    assert_eq!((back.space_id.as_str(), back.url.as_str()), (w.home.as_str(), url));
    assert_eq!(tab_profile(&w.lib, back), Some(w.personal.as_str()), "a new tab in the original profile; no cookies move");
    assert!(w.lib.store.archive(&w.work, 10).unwrap().is_empty() && w.lib.store.archive(&w.personal, 10).unwrap().is_empty(), "nothing archived");
    w.save_tabs();
    let rows = w.reopen().store.tabs().unwrap();
    assert!(!rows.iter().any(|row| row.id == tab), "the moved tab stays closed after reopening");
    assert!(rows.iter().any(|row| row.id == reopened && row.space_id == w.home));

    // Its Space gone: the Space in use. Its tab already closed: it still reopens.
    let undo = Undo { tab: "closed".into(), url: url.into(), space: "gone".into() };
    let (removed, reopened) = undo_tabs(&w.lib, &mut w.tabs, &undo, &w.desk).unwrap();
    assert!(removed.is_none());
    assert_eq!(w.opened(&reopened).space_id, w.desk);
    let before = w.tabs.len();
    assert!(undo_tabs(&w.lib, &mut w.tabs, &undo, "gone").is_err());
    assert_eq!(w.tabs.len(), before, "nothing changes when no Space can take it");
}

#[test]
fn a_tabs_menu_offers_to_always_open_its_site_in_its_profile_or_to_stop() {
    let mut w = world();
    let personal = w.lib.profiles[&w.personal].name.clone();
    let entries = |w: &World, url: &str, space: &str| {
        let mut shown = tab("shown", space, 1);
        shown.url = url.into();
        tab_site_entries(&w.lib, &w.sites, &shown)
    };
    let always = |site: &str, host: &str, profile: &str| SiteEntry::Always { label: format!("Always Open {site} in {personal}…"), host: host.into(), profile: profile.into() };
    assert_eq!(entries(&w, "https://www.GitHub.com/x", &w.home), [always("github.com", "github.com", &w.personal)]);
    assert_eq!(entries(&w, "https://xn--bcher-kva.example/", &w.home), [always("bücher.example", "xn--bcher-kva.example", &w.personal)]);
    assert!(entries(&w, "about:blank", &w.home).is_empty() && entries(&w, "file:///tmp/a.html", &w.home).is_empty());

    let work = w.work.clone();
    w.rule("github.com", true, &work);
    let stop = SiteEntry::Stop { label: "Stop Opening github.com in Work".into(), host: "github.com".into() };
    assert_eq!(entries(&w, "https://gist.github.com/x", &w.home), [always("gist.github.com", "gist.github.com", &w.personal), stop.clone()]);
    assert_eq!(entries(&w, "https://github.com/", &w.desk), [stop], "its own profile already has it");

    // Stop removes it, for good.
    w.sites.remove(&w.lib.store, "github.com").unwrap();
    assert!(SiteProfiles::load(&w.reopen().store).is_empty());
    assert_eq!(entries(&w, "https://github.com/", &w.desk).len(), 1);

    w.rule("github.com", false, "not-on-this-device");
    assert_eq!(entries(&w, "https://github.com/", &w.home)[1], SiteEntry::Stop { label: "Stop Opening github.com in a Profile Not on This Device".into(), host: "github.com".into() });
}

/// Only new tabs and windows consult site rules. A same-tab navigation stays
/// in its tab's view, and with it its profile; source checks, since the app
/// and its web views can't run in tests.
#[test]
fn site_rules_apply_to_new_tabs_only_never_to_same_tab_navigation() {
    let app = include_str!("app.rs");
    let calls: Vec<&str> = app.lines().map(str::trim).filter(|line| line.contains("open_site_tab(")).collect();
    assert_eq!(calls, [
        // A page's new tab or window, after Little Window and Peek clicks; ⌥ keeps it here.
        "if !modifiers.alt && self.open_site_tab(&url, background) { continue; }",
        // The page menu's Open Link in New Tab; Open Link in New Tab Here passes `here`.
        "if here || !self.open_site_tab(&url, background) { self.open_new_tab(&url, background); }",
        // The command bar's addresses, not New Tab Inside a folder.
        "if route && origin.pin_folder().is_none() && self.open_site_tab(&url, false) { continue; }",
    ]);
    let opens = &app[app.find("for (from, url, background, popup, modifiers) in opens {").unwrap()..];
    let opens = &opens[..opens.find("Action::NewTab { url, background }").unwrap()];
    assert!(opens.find("little::Click::Usual => {}").unwrap() < opens.find("open_site_tab").unwrap(), "Little Window and Peek clicks first");
    // The engine reports only CEF's new-tab and new-window requests as OpenUrl.
    let view = include_str!("../../crates/crossbow-engine/src/view.rs");
    assert_eq!(view.matches("self.h.0.push(WebEvent::OpenUrl").count(), 1);
    assert!(view.contains("1 // never let CEF create its own window"));
    for source in [include_str!("navigation.rs"), include_str!("little.rs")] {
        assert!(!source.contains("open_site_tab") && !source.contains("site_route("), "same-tab, Peek and Little Window loads stay put");
    }
    let bar = include_str!("ui/command_bar.rs");
    assert!(bar.contains("let route = new_tab && !here && is_address(&s);"), "editing the current tab's address loads in place");
    let menu = include_str!("ui/overlays.rs");
    assert!(menu.contains("if moved && m.item(ui, \"Open Link in New Tab Here\""));
}
