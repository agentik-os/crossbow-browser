//! Content blocking on a disposable library and data folder, with a fake
//! file panel and a record of what the engine would get.

use super::*;
use crate::shortcuts::tests::Fixture;
use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

thread_local! {
    /// How many element hidings were built on this thread: the UI thread's
    /// count, in a test, must stay 0.
    pub(crate) static HIDINGS_BUILT_HERE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(crate) const ADS: &str = "! Fixture ads list\n||ads.example^\n||track.example^$third-party\n##.banner\nnews.example#?#.promo:has-text(Sponsored)\n";
pub(crate) const MORE_ADS: &str = "||more-ads.example^\n";
pub(crate) const COOKIES: &str = "||consent.example^\n";

/// The rules each profile was last given.
#[derive(Clone, Default)]
pub(crate) struct Installed(Arc<Mutex<HashMap<String, Option<Arc<dyn ContentRules>>>>>);

impl Install for Installed {
    fn install(&mut self, profile: &str, rules: Option<Arc<dyn ContentRules>>) {
        self.0.lock().unwrap().insert(profile.into(), rules);
    }
}

impl Installed {
    pub(crate) fn has(&self, profile: &str) -> bool {
        self.0.lock().unwrap().get(profile).is_some_and(Option::is_some)
    }

    /// Whether `profile`'s pages would cancel `url`, asked by a script on `page`.
    pub(crate) fn blocks(&self, profile: &str, url: &str, page: &str) -> bool {
        self.blocks_kind(profile, url, ResourceKind::Script, page)
    }

    pub(crate) fn blocks_kind(&self, profile: &str, url: &str, kind: ResourceKind, page: &str) -> bool {
        let rules = self.0.lock().unwrap().get(profile).cloned().flatten();
        rules.is_some_and(|rules| rules.blocks(&ResourceRequest { url, kind, context: page, top: page }))
    }

    /// The element hiding a document at `document`, inside a tab on `top`,
    /// would get: its host's rules, then the shared ones.
    pub(crate) fn hides(&self, profile: &str, document: &str, top: &str) -> Option<String> {
        let rules = self.0.lock().unwrap().get(profile).cloned().flatten()?;
        rules.hiding(document, top).map(|sheet| format!("{}{}", sheet.specific, sheet.generic))
    }

    /// Whether a frame on `context`, inside a tab on `top`, would have `url` cancelled.
    pub(crate) fn blocks_in(&self, profile: &str, url: &str, context: &str, top: &str) -> bool {
        let rules = self.0.lock().unwrap().get(profile).cloned().flatten();
        rules.is_some_and(|rules| rules.blocks(&ResourceRequest { url, kind: ResourceKind::Script, context, top }))
    }
}

/// A file panel the test answers: each open waits for [`Panel::answer`].
#[derive(Clone, Default)]
pub(crate) struct Panel(Arc<Mutex<Vec<Sender<Result<Option<PathBuf>, String>>>>>);

impl FilePanel for Panel {
    fn choose(&mut self) -> Receiver<Result<Option<PathBuf>, String>> {
        let (send, receive) = std::sync::mpsc::channel();
        self.0.lock().unwrap().push(send);
        receive
    }
}

impl Panel {
    pub(crate) fn opened(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    pub(crate) fn answer(&self, answer: Result<Option<PathBuf>, String>) {
        let send = self.0.lock().unwrap().last().cloned().expect("the panel is open");
        send.send(answer).unwrap();
    }
}

pub(crate) struct Harness {
    pub(crate) fixture: Fixture,
    pub(crate) lib: Library,
    pub(crate) data: tempfile::TempDir,
    pub(crate) host: ContentBlocking,
    pub(crate) installed: Installed,
    pub(crate) panel: Panel,
    pub(crate) ctx: egui::Context,
    /// The default profile and a second one.
    pub(crate) profile: String,
    pub(crate) other: String,
    /// The lists that come with this host.
    pub(crate) bundled: &'static [&'static Bundled],
}

impl Harness {
    pub(crate) fn new() -> Self {
        Self::with_bundled(&[])
    }

    /// With `bundled` as the lists that come with the app.
    pub(crate) fn with_bundled(bundled: &'static [&'static Bundled]) -> Self {
        let fixture = Fixture::new();
        let mut lib = Library::open(fixture.store()).unwrap();
        lib.ensure_defaults().unwrap();
        let profile = lib.default_profile_id();
        let other = lib.create_profile("Client").unwrap();
        let data = tempfile::tempdir().unwrap();
        let (installed, panel) = (Installed::default(), Panel::default());
        let host = ContentBlocking::new(data.path().into(), Box::new(installed.clone()), Box::new(panel.clone())).bundled(bundled);
        Harness { fixture, lib, data, host, installed, panel, ctx: egui::Context::default(), profile, other, bundled }
    }

    /// Quit and reopen: a new library, host and engine record on the same files.
    pub(crate) fn reopen(&mut self) {
        self.lib = Library::open(self.fixture.store()).unwrap();
        self.installed = Installed::default();
        self.panel = Panel::default();
        self.host = ContentBlocking::new(self.data.path().into(), Box::new(self.installed.clone()), Box::new(self.panel.clone())).bundled(self.bundled);
        self.host.launch(&self.lib, &self.ctx);
        self.settle();
    }

    /// Waits for finished work, but never for the open panel.
    pub(crate) fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.host.poll(&self.ctx) && !self.host.choosing() {
            assert!(Instant::now() < deadline, "content blocking work never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A file the person could choose.
    pub(crate) fn file(&self, name: &str, text: &str) -> PathBuf {
        let folder = self.data.path().join("chosen");
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    /// Add List from File…, choosing `path`.
    pub(crate) fn add(&mut self, kind: ListKind, path: &Path) {
        let profile = self.profile.clone();
        self.host.profile(&self.lib.store, &profile);
        self.host.add_from_file(&profile, kind);
        self.panel.answer(Ok(Some(path.to_owned())));
        self.settle();
    }

    pub(crate) fn switch(&mut self, kind: ListKind, on: bool) {
        let (store, profile) = (&self.lib.store, self.profile.clone());
        let blocking = self.host.profile(store, &profile);
        match kind {
            ListKind::Ads => blocking.ads.choose(store, on),
            ListKind::CookieBanners => blocking.cookies.choose(store, on),
        }
        self.host.apply(&profile, &self.ctx);
        self.settle();
    }

    pub(crate) fn lists(&mut self, kind: ListKind) -> Vec<(String, usize, usize, ListState)> {
        let profile = self.profile.clone();
        self.host.profile(&self.lib.store, &profile).lists(kind).into_iter().map(|(entry, state)| (entry.name, entry.rules, entry.unsupported, state)).collect()
    }

    /// `kind`'s bundled lists: name, whether this profile uses it, state and counts.
    pub(crate) fn bundled(&mut self, kind: ListKind) -> Vec<(&'static str, bool, ListState, Option<(usize, usize)>)> {
        let profile = self.profile.clone();
        self.host.profile(&self.lib.store, &profile).bundled(kind).into_iter().map(|row| (row.name, row.on, row.state, row.counts)).collect()
    }

    pub(crate) fn message(&mut self) -> Option<(String, bool)> {
        let profile = self.profile.clone();
        self.host.profile(&self.lib.store, &profile).message.clone()
    }

    pub(crate) fn blocks(&self, url: &str, page: &str) -> bool {
        self.installed.blocks(&self.profile, url, page)
    }
}

#[test]
fn both_switches_start_off_and_each_profile_keeps_its_own_choice() {
    let mut h = Harness::new();
    let (profile, other) = (h.profile.clone(), h.other.clone());
    for id in [&profile, &other] {
        let blocking = h.host.profile(&h.lib.store, id);
        assert!(!blocking.ads.enabled() && !blocking.cookies.enabled(), "off by default");
        assert!(!blocking.enabled(ListKind::Ads) && !blocking.enabled(ListKind::CookieBanners));
    }
    assert_eq!(h.host.profile(&h.lib.store, &profile).ads.key(), format!("block_ads.{profile}"));
    assert_eq!(h.host.profile(&h.lib.store, &profile).cookies.key(), format!("block_cookie_banners.{profile}"));
    h.switch(ListKind::Ads, true);
    h.reopen();
    assert!(h.host.profile(&h.lib.store, &profile).enabled(ListKind::Ads), "read back after reopening");
    assert!(!h.host.profile(&h.lib.store, &profile).enabled(ListKind::CookieBanners));
    assert!(!h.host.profile(&h.lib.store, &other).enabled(ListKind::Ads), "another profile keeps its own choice");
    assert!(!h.installed.has(&profile) && !h.installed.has(&other), "a switch with no lists blocks nothing");
}

#[test]
fn an_added_list_blocks_while_its_switch_is_on() {
    let mut h = Harness::new();
    let page = "https://news.example/story";
    let ads = h.file("ads.txt", ADS);
    h.add(ListKind::Ads, &ads);
    assert_eq!(h.panel.opened(), 1);
    assert_eq!(h.message(), Some(("Added “ads.txt”.".to_owned(), false)));
    assert_eq!(h.lists(ListKind::Ads), [("ads.txt".to_owned(), 3, 1, ListState::Off)]);
    assert!(h.lists(ListKind::CookieBanners).is_empty());
    assert!(!h.blocks("https://ads.example/a.js", page), "off by default");

    h.switch(ListKind::Ads, true);
    assert_eq!(h.lists(ListKind::Ads)[0].3, ListState::Ready);
    assert!(h.blocks("https://ads.example/a.js", page));
    assert!(h.blocks("https://cdn.ads.example/a.js", page), "and its subdomains");
    assert!(!h.blocks("https://news.example/a.js", page), "the page's own script");
    assert!(h.blocks("https://track.example/t.js", page), "a third-party tracker");
    assert!(!h.blocks("https://track.example/t.js", "https://www.track.example/"), "but not on its own site");
    assert!(!h.installed.has(&h.other.clone()), "another profile is untouched");

    // A cookie banner list applies only under its own switch.
    let cookies = h.file("cookies.txt", COOKIES);
    h.add(ListKind::CookieBanners, &cookies);
    assert_eq!(h.lists(ListKind::CookieBanners), [("cookies.txt".to_owned(), 1, 0, ListState::Off)]);
    assert!(!h.blocks("https://consent.example/cmp.js", page));
    h.switch(ListKind::CookieBanners, true);
    assert!(h.blocks("https://consent.example/cmp.js", page) && h.blocks("https://ads.example/a.js", page));

    h.switch(ListKind::Ads, false);
    assert_eq!(h.lists(ListKind::Ads)[0].3, ListState::Off);
    assert!(!h.blocks("https://ads.example/a.js", page), "off again");
    assert!(h.blocks("https://consent.example/cmp.js", page));
    h.switch(ListKind::CookieBanners, false);
    assert!(!h.installed.has(&h.profile.clone()), "no rules at all once both are off");
}

#[test]
fn lists_open_at_launch_and_a_changed_copy_is_marked_while_the_others_apply() {
    let mut h = Harness::new();
    let page = "https://news.example/";
    let (ads, more) = (h.file("ads.txt", ADS), h.file("more.txt", MORE_ADS));
    h.add(ListKind::Ads, &ads);
    h.add(ListKind::Ads, &more);
    h.switch(ListKind::Ads, true);
    h.reopen();
    assert!(h.blocks("https://ads.example/a.js", page) && h.blocks("https://more-ads.example/a.js", page), "both apply after reopening");
    assert!(!h.installed.has(&h.other.clone()));

    let saved = lists::load(h.data.path(), &h.profile).lists;
    let copy = std::fs::read_dir(h.data.path().join(lists::FOLDER)).unwrap().flatten().next().unwrap().path().join(format!("{}.txt", saved[0].id));
    std::fs::write(&copy, "||changed.example^\n").unwrap();
    h.reopen();
    assert_eq!(h.lists(ListKind::Ads), [
        ("ads.txt".to_owned(), 3, 1, ListState::Failed(lists::CHANGED.into())),
        ("more.txt".to_owned(), 1, 0, ListState::Ready),
    ]);
    assert!(!h.blocks("https://ads.example/a.js", page) && !h.blocks("https://changed.example/a.js", page), "the changed copy isn't used");
    assert!(h.blocks("https://more-ads.example/a.js", page), "the other list still is");
}

#[test]
fn a_refused_file_leaves_the_lists_and_the_rules_as_they_were() {
    let mut h = Harness::new();
    let page = "https://news.example/";
    let ads = h.file("ads.txt", ADS);
    h.add(ListKind::Ads, &ads);
    h.switch(ListKind::Ads, true);
    let before = h.lists(ListKind::Ads);

    let refused = [
        (h.file("comments.txt", "! nothing to use\n#?#.banner:has-text(x)\n"), AddError::NoRules.to_string()),
        (h.file("again.txt", ADS), AddError::Duplicate.to_string()),
        (h.data.path().join("chosen/missing.txt"), AddError::Unreadable.to_string()),
        (h.file("binary.txt", "||a.example^\0"), AddError::NotText.to_string()),
    ];
    for (path, message) in refused {
        h.add(ListKind::Ads, &path);
        assert_eq!(h.message(), Some((message, true)));
        assert_eq!(h.lists(ListKind::Ads), before);
        assert!(h.blocks("https://ads.example/a.js", page), "the rules in use stay");
    }

    // The panel can't open; then Cancel, which says nothing new.
    let profile = h.profile.clone();
    h.host.add_from_file(&profile, ListKind::Ads);
    h.panel.answer(Err("The file panel couldn't open. Try again.".into()));
    h.settle();
    assert_eq!(h.message(), Some(("The file panel couldn't open. Try again.".to_owned(), true)));
    h.host.add_from_file(&profile, ListKind::Ads);
    assert!(h.host.choosing());
    h.host.add_from_file(&profile, ListKind::CookieBanners);
    assert_eq!(h.panel.opened(), 7, "one panel at a time");
    h.panel.answer(Ok(None));
    h.settle();
    assert!(!h.host.choosing());
    assert_eq!(h.message(), Some(("The file panel couldn't open. Try again.".to_owned(), true)));

    // A save that fails: the manifest's partial file can't be written.
    let folder = std::fs::read_dir(h.data.path().join(lists::FOLDER)).unwrap().flatten().next().unwrap().path();
    std::fs::create_dir(folder.join(".lists.json.partial")).unwrap();
    let more = h.file("more.txt", MORE_ADS);
    h.add(ListKind::Ads, &more);
    let (message, error) = h.message().unwrap();
    assert!(error && message.starts_with("Couldn't save the list (") && message.ends_with("then try again."), "{message}");
    assert_eq!(h.lists(ListKind::Ads), before);
    assert!(!h.blocks("https://more-ads.example/a.js", page));

    // Retry once the folder is writable.
    std::fs::remove_dir(folder.join(".lists.json.partial")).unwrap();
    h.add(ListKind::Ads, &more);
    assert_eq!(h.message(), Some(("Added “more.txt”.".to_owned(), false)));
    assert!(h.blocks("https://more-ads.example/a.js", page));
}

#[test]
fn removing_a_list_stops_its_rules_and_stays_removed_after_reopening() {
    let mut h = Harness::new();
    let page = "https://news.example/";
    let (ads, more) = (h.file("ads.txt", ADS), h.file("more.txt", MORE_ADS));
    h.add(ListKind::Ads, &ads);
    h.add(ListKind::Ads, &more);
    h.switch(ListKind::Ads, true);
    let profile = h.profile.clone();
    let id = lists::load(h.data.path(), &profile).lists[0].id.clone();

    // A failed save keeps it.
    let folder = std::fs::read_dir(h.data.path().join(lists::FOLDER)).unwrap().flatten().next().unwrap().path();
    std::fs::create_dir(folder.join(".lists.json.partial")).unwrap();
    h.host.remove(&profile, &id, &h.ctx.clone());
    let (message, error) = h.message().unwrap();
    assert!(error && message.starts_with("Couldn't remove the list."), "{message}");
    assert_eq!(h.lists(ListKind::Ads).len(), 2);
    assert!(h.blocks("https://ads.example/a.js", page));
    std::fs::remove_dir(folder.join(".lists.json.partial")).unwrap();

    h.host.remove(&profile, &id, &h.ctx.clone());
    h.settle();
    assert_eq!(h.message(), Some(("Removed “ads.txt”.".to_owned(), false)));
    assert_eq!(h.lists(ListKind::Ads), [("more.txt".to_owned(), 1, 0, ListState::Ready)]);
    assert!(!h.blocks("https://ads.example/a.js", page) && h.blocks("https://more-ads.example/a.js", page));
    h.reopen();
    assert_eq!(h.lists(ListKind::Ads), [("more.txt".to_owned(), 1, 0, ListState::Ready)], "read back after reopening");

    let id = lists::load(h.data.path(), &profile).lists[0].id.clone();
    h.host.remove(&profile, &id, &h.ctx.clone());
    h.settle();
    assert!(h.lists(ListKind::Ads).is_empty());
    assert!(!h.installed.has(&profile), "no lists, no rules");
}

#[test]
fn a_damaged_manifest_uses_no_list_and_says_so_until_a_list_is_added() {
    let mut h = Harness::new();
    let ads = h.file("ads.txt", ADS);
    h.add(ListKind::Ads, &ads);
    h.switch(ListKind::Ads, true);
    let folder = std::fs::read_dir(h.data.path().join(lists::FOLDER)).unwrap().flatten().next().unwrap().path();
    std::fs::write(folder.join("lists.json"), "{not json").unwrap();
    h.reopen();
    let profile = h.profile.clone();
    let notice = h.host.profile(&h.lib.store, &profile).notice().map(str::to_owned);
    assert_eq!(notice.as_deref(), Some("Saved content blocking lists couldn't be read, so none are in use. Add them again."));
    assert!(h.lists(ListKind::Ads).is_empty() && !h.installed.has(&profile));
    h.add(ListKind::Ads, &ads);
    assert_eq!(h.host.profile(&h.lib.store, &profile).notice(), None);
    assert!(h.blocks("https://ads.example/a.js", "https://news.example/"));
}

#[test]
fn each_request_kind_reaches_its_type_option_and_only_web_addresses_match() {
    let text = "||script.example^$script\n||image.example^$image\n||frame.example^$subdocument\n||css.example^$stylesheet\n||font.example^$font\n||object.example^$object\n||media.example^$media\n||xhr.example^$xmlhttprequest\n||ping.example^$ping\n||other.example^$other\n||socket.example^$websocket\n";
    let lists = vec![matcher::compile(text)];
    let rules = Rules::new(lists.clone(), Default::default(), Arc::new(Hiding::new(&lists)));
    let ask = |url: &str, kind| rules.blocks(&ResourceRequest { url, kind, context: "https://news.example/", top: "https://news.example/" });
    let kinds = [
        ("script", ResourceKind::Script),
        ("image", ResourceKind::Image),
        ("frame", ResourceKind::Subdocument),
        ("css", ResourceKind::Stylesheet),
        ("font", ResourceKind::Font),
        ("object", ResourceKind::Object),
        ("media", ResourceKind::Media),
        ("xhr", ResourceKind::Xhr),
        ("ping", ResourceKind::Ping),
        ("other", ResourceKind::Other),
    ];
    for (host, kind) in kinds {
        for (_, asked) in kinds {
            assert_eq!(ask(&format!("https://{host}.example/x"), asked), asked == kind, "{host} asked as {asked:?}");
        }
    }
    assert!(ask("wss://socket.example/live", ResourceKind::Other), "a socket by its address");
    assert!(!ask("https://socket.example/live", ResourceKind::Other));
    for url in ["data:text/javascript,1", "blob:https://script.example/1", "file:///script.example/a.js", "chrome://script.example/"] {
        assert!(!ask(url, ResourceKind::Script), "{url}");
    }
}

#[test]
fn counts_read_naturally() {
    assert_eq!(counts(12_345, 321), "12,345 rules · 321 unsupported, skipped");
    assert_eq!(counts(1, 0), "1 rule");
    assert_eq!(counts(2, 1), "2 rules · 1 unsupported, skipped");
    assert_eq!(counts(1_000_000, 1_000), "1,000,000 rules · 1,000 unsupported, skipped");
}

#[test]
fn a_deleted_profile_s_rules_are_dropped() {
    let mut h = Harness::new();
    let ads = h.file("ads.txt", ADS);
    h.add(ListKind::Ads, &ads);
    h.switch(ListKind::Ads, true);
    let profile = h.profile.clone();
    assert!(h.installed.has(&profile));
    h.host.forget(&profile);
    assert!(!h.installed.has(&profile), "the engine lets go of them");
    assert!(!h.host.profiles.contains_key(&profile));
}


#[test]
fn turning_blocking_off_on_a_site_lets_its_pages_load_everything_and_survives_a_reopen() {
    let mut h = Harness::new();
    let ads = h.file("fixture-ads.txt", ADS);
    h.add(ListKind::Ads, &ads);
    h.switch(ListKind::Ads, true);
    let (profile, other) = (h.profile.clone(), h.other.clone());
    assert_eq!(h.host.site_blocking(&h.lib.store, &profile, "https://www.news.example/today"), Some(("news.example".to_owned(), true)));
    assert!(h.blocks("https://ads.example/a.js", "https://www.news.example/today"));

    h.host.set_site_blocking(&h.lib.store, &profile, "news.example", false, &h.ctx).unwrap();
    assert_eq!(h.host.site_blocking(&h.lib.store, &profile, "https://live.news.example/"), Some(("news.example".to_owned(), false)), "the whole site");
    assert!(!h.installed.blocks_in(&profile, "https://ads.example/a.js", "https://www.news.example/today", "https://www.news.example/today"), "its pages load everything");
    assert!(!h.installed.blocks_in(&profile, "https://ads.example/a.js", "https://ads.example/frame", "https://news.example/"), "frames inside it too");
    assert!(h.installed.blocks_in(&profile, "https://ads.example/a.js", "https://news.example/embed", "https://other.example/"), "its frame inside another site is still blocked");
    assert!(h.blocks("https://ads.example/a.js", "https://other.example/"), "other sites still block");
    assert_eq!(h.host.profile(&h.lib.store, &profile).sites_off(), ["news.example"]);
    assert_eq!(h.host.profile(&h.lib.store, &other).sites_off(), Vec::<String>::new(), "each profile's own");

    h.reopen();
    assert_eq!(h.host.site_blocking(&h.lib.store, &profile, "https://news.example/"), Some(("news.example".to_owned(), false)), "read back after reopening");
    assert!(!h.blocks("https://ads.example/a.js", "https://news.example/"));
    h.host.set_site_blocking(&h.lib.store, &profile, "news.example", true, &h.ctx).unwrap();
    assert!(h.blocks("https://ads.example/a.js", "https://news.example/"), "on again");
    assert!(h.host.profile(&h.lib.store, &profile).sites_off().is_empty());
    for page in ["file:///fixture/notes.html", "about:blank", ""] {
        assert_eq!(h.host.site_blocking(&h.lib.store, &profile, page), None, "{page:?} has no site");
    }
}

#[test]
fn a_failed_save_keeps_blocking_as_it_was_and_says_so_and_a_retry_works() {
    let mut h = Harness::new();
    let ads = h.file("fixture-ads.txt", ADS);
    h.add(ListKind::Ads, &ads);
    h.switch(ListKind::Ads, true);
    let profile = h.profile.clone();
    let conn = rusqlite::Connection::open(h.fixture.path()).unwrap();
    conn.execute_batch("CREATE TRIGGER refuse_exceptions BEFORE INSERT ON kv WHEN NEW.k LIKE 'content_blocking.%' BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
    let refused = h.host.set_site_blocking(&h.lib.store, &profile, "news.example", false, &h.ctx);
    assert_eq!(refused, Err(crossbow_core::content_blocking::exceptions::SAVE_FAILED.to_owned()));
    assert_eq!(h.host.site_blocking(&h.lib.store, &profile, "https://news.example/"), Some(("news.example".to_owned(), true)), "still on");
    assert!(h.blocks("https://ads.example/a.js", "https://news.example/"), "and still blocking");
    conn.execute_batch("DROP TRIGGER refuse_exceptions").unwrap();
    h.host.set_site_blocking(&h.lib.store, &profile, "news.example", false, &h.ctx).unwrap();
    assert!(!h.blocks("https://ads.example/a.js", "https://news.example/"), "the retry works");
}

#[test]
fn unreadable_sites_with_blocking_off_leave_blocking_on_and_say_so() {
    let mut h = Harness::new();
    let ads = h.file("fixture-ads.txt", ADS);
    h.add(ListKind::Ads, &ads);
    h.switch(ListKind::Ads, true);
    let profile = h.profile.clone();
    h.lib.store.kv_set(&crossbow_core::content_blocking::exceptions::key(&profile), "{not json").unwrap();
    h.reopen();
    assert_eq!(h.host.profile(&h.lib.store, &profile).sites_notice(), Some(crossbow_core::content_blocking::exceptions::UNREADABLE));
    assert!(h.blocks("https://ads.example/a.js", "https://news.example/"));
    h.host.set_site_blocking(&h.lib.store, &profile, "news.example", false, &h.ctx).unwrap();
    assert_eq!(h.host.profile(&h.lib.store, &profile).sites_notice(), None, "saving over it");
}

#[test]
fn element_hiding_follows_each_switch_and_the_site_s_blocking() {
    let mut h = Harness::new();
    let ads = h.file("fixture-ads.txt", "##.banner\nnews.example##.sponsor\nshop.example##.deal\n");
    let cookies = h.file("fixture-cookies.txt", "###cookie-banner\n");
    h.add(ListKind::Ads, &ads);
    h.add(ListKind::CookieBanners, &cookies);
    let profile = h.profile.clone();
    let news = "https://www.news.example/story";
    assert_eq!(h.installed.hides(&profile, news, news), None, "both switches off");

    h.switch(ListKind::Ads, true);
    let sheet = h.installed.hides(&profile, news, news).unwrap();
    assert!(sheet.contains(".sponsor{display:none!important}") && sheet.contains(".banner{display:none!important}"), "{sheet}");
    assert!(!sheet.contains("deal") && !sheet.contains("cookie-banner"), "only its own host's rules, and only lists whose switch is on: {sheet}");
    let elsewhere = h.installed.hides(&profile, "https://other.example/", "https://other.example/").unwrap();
    assert!(elsewhere.contains(".banner") && !elsewhere.contains("sponsor"));
    for page in ["file:///fixture/ad.html", "about:blank"] { assert_eq!(h.installed.hides(&profile, page, page), None, "{page}"); }

    h.switch(ListKind::CookieBanners, true);
    assert!(h.installed.hides(&profile, news, news).unwrap().contains("#cookie-banner{display:none!important}"));

    // Blocking off on the tab's site: nothing is hidden in it, its frames
    // included; the site's own frames elsewhere still are.
    h.host.set_site_blocking(&h.lib.store, &profile, "news.example", false, &h.ctx).unwrap();
    assert_eq!(h.installed.hides(&profile, news, news), None);
    assert_eq!(h.installed.hides(&profile, "https://ads.example/frame", news), None);
    assert!(h.installed.hides(&profile, "https://news.example/embed", "https://other.example/").is_some_and(|sheet| sheet.contains(".sponsor")));
    h.host.set_site_blocking(&h.lib.store, &profile, "news.example", true, &h.ctx).unwrap();
    assert!(h.installed.hides(&profile, news, news).is_some());
}

#[test]
fn hiding_rules_past_the_bounds_are_counted_for_settings() {
    let mut h = Harness::new();
    let many: String = (0..crossbow_core::content_blocking::hiding::MAX_SELECTORS + 3).map(|i| format!("##.g{i}\n")).collect();
    let ads = h.file("fixture-many.txt", &many);
    h.add(ListKind::Ads, &ads);
    let profile = h.profile.clone();
    assert_eq!(h.host.profile(&h.lib.store, &profile).hiding_skipped(), 0, "not while the switch is off");
    h.switch(ListKind::Ads, true);
    assert_eq!(h.host.profile(&h.lib.store, &profile).hiding_skipped(), 3);
    assert_eq!(skipped_note(3), "3 cosmetic rules skipped: too many");
    assert_eq!(skipped_note(1), "1 cosmetic rule skipped: too many");
    assert_eq!(skipped_note(12_345), "12,345 cosmetic rules skipped: too many");
}

/// The fingerprinting disclosure, in each switch's description.
const HIDING_VISIBLE: &str = "A page can read the hiding rules it gets, so a site could learn some of the rules you use.";

#[test]
fn the_switches_say_what_hiding_shows_a_page_and_that_a_hidden_banner_is_no_answer() {
    for description in [BlockAds::DESCRIPTION, BlockCookieBanners::DESCRIPTION] {
        assert!(description.contains(HIDING_VISIBLE), "{description}");
    }
    assert!(BlockCookieBanners::DESCRIPTION.contains("never answers a banner, so a hidden banner gives no consent"));
}

/// Two lists that come with this test build, as EasyList and EasyPrivacy
/// come with the app.
pub(crate) static BUNDLED_ADS: Bundled = Bundled::new("fixture-ads", "Fixture Ads", ListKind::Ads, "! Fixture bundled list\n||bundled-ads.example^\n##.bundled-banner\n", "Fixture lists by the Crossbow tests.");
pub(crate) static BUNDLED_TRACKERS: Bundled = Bundled::new("fixture-trackers", "Fixture Trackers", ListKind::Ads, "||bundled-tracker.example^\n/fixture-unsupported/$popup\n", "Fixture lists by the Crossbow tests.");
pub(crate) static FIXTURE_BUNDLED: [&Bundled; 2] = [&BUNDLED_ADS, &BUNDLED_TRACKERS];

#[test]
fn bundled_lists_apply_while_the_switch_is_on_and_each_turns_off_per_profile() {
    let mut h = Harness::with_bundled(&FIXTURE_BUNDLED);
    let (profile, other) = (h.profile.clone(), h.other.clone());
    let (page, ad, tracker) = ("https://news.example/", "https://bundled-ads.example/a.js", "https://bundled-tracker.example/t.js");
    assert_eq!(h.bundled(ListKind::Ads), [("Fixture Ads", true, ListState::Off, None), ("Fixture Trackers", true, ListState::Off, None)], "listed, not compiled, while the switch is off");
    assert!(h.bundled(ListKind::CookieBanners).is_empty());
    assert!(!h.blocks(ad, page) && !h.installed.has(&profile));

    h.switch(ListKind::Ads, true);
    assert_eq!(h.bundled(ListKind::Ads), [("Fixture Ads", true, ListState::Ready, Some((2, 0))), ("Fixture Trackers", true, ListState::Ready, Some((1, 1)))]);
    assert!(h.blocks(ad, page) && h.blocks(tracker, page));
    assert!(h.installed.hides(&profile, page, page).is_some_and(|sheet| sheet.contains(".bundled-banner{display:none!important}")));
    assert!(!h.lists(ListKind::Ads).iter().any(|(name, ..)| name.starts_with("Fixture")), "not among the lists added from files");

    h.host.set_bundled(&profile, "fixture-trackers", false, &h.ctx);
    h.settle();
    assert_eq!(h.message(), Some(("Turned off “Fixture Trackers”.".to_owned(), false)));
    assert_eq!(h.bundled(ListKind::Ads)[1], ("Fixture Trackers", false, ListState::Off, None));
    assert!(h.blocks(ad, page) && !h.blocks(tracker, page));
    h.reopen();
    assert!(h.blocks(ad, page) && !h.blocks(tracker, page), "kept after reopening");

    h.host.profile(&h.lib.store, &other).ads.choose(&h.lib.store, true);
    h.host.apply(&other, &h.ctx);
    h.settle();
    assert!(h.installed.blocks(&other, tracker, page), "another profile keeps its own choice");

    h.host.set_bundled(&profile, "fixture-trackers", true, &h.ctx);
    h.settle();
    assert_eq!(h.message(), Some(("Turned on “Fixture Trackers”.".to_owned(), false)));
    assert!(h.blocks(tracker, page));
}

#[test]
fn a_failed_turn_off_keeps_the_bundled_list_on_and_says_why_until_a_retry_works() {
    let mut h = Harness::with_bundled(&FIXTURE_BUNDLED);
    let profile = h.profile.clone();
    let (page, tracker) = ("https://news.example/", "https://bundled-tracker.example/t.js");
    h.switch(ListKind::Ads, true);
    // The lists folder's place is taken by a file, so nothing can be saved there.
    let folder = h.data.path().join(lists::FOLDER);
    std::fs::write(&folder, "").unwrap();
    h.host.set_bundled(&profile, "fixture-trackers", false, &h.ctx);
    h.settle();
    let (message, error) = h.message().unwrap();
    assert!(error && message.starts_with("Couldn't save the change."), "{message}");
    assert_eq!(h.bundled(ListKind::Ads)[1].1, true, "still on");
    assert!(h.blocks(tracker, page));
    std::fs::remove_file(&folder).unwrap();
    h.host.set_bundled(&profile, "fixture-trackers", false, &h.ctx);
    h.settle();
    assert!(!h.blocks(tracker, page), "the retry works");
    assert_eq!(lists::load(h.data.path(), &profile).bundled_off, ["fixture-trackers".to_owned()].into());
}

/// While bundled lists compile, at launch or when a switch turns on,
/// requests load and nothing is counted as blocked; Settings and the shield
/// say the lists are loading. The UI never waits for them.
#[test]
fn requests_load_unblocked_until_the_bundled_lists_are_ready() {
    let mut h = Harness::with_bundled(&FIXTURE_BUNDLED);
    let profile = h.profile.clone();
    let (page, ad) = ("https://news.example/", "https://bundled-ads.example/a.js");
    h.host.profile(&h.lib.store, &profile).ads.choose(&h.lib.store, true);
    h.host.apply(&profile, &h.ctx);
    // Nothing has been taken from the compiling thread yet.
    assert!(h.host.profile(&h.lib.store, &profile).loading());
    assert_eq!(h.bundled(ListKind::Ads)[0].2, ListState::Loading);
    assert!(!h.blocks(ad, page), "requests load meanwhile");
    let shield = crate::ui::content_blocking_shield::shield(&mut h.host, &h.lib.store, &profile, page, 0).unwrap();
    assert_eq!(crate::ui::content_blocking_shield::status(&shield), LOADING_LISTS);
    h.settle();
    assert!(!h.host.profile(&h.lib.store, &profile).loading());
    assert!(h.blocks(ad, page));

    // At launch, the same until they are ready.
    h.lib = crossbow_core::library::Library::open(h.fixture.store()).unwrap();
    h.installed = Installed::default();
    h.host = ContentBlocking::new(h.data.path().into(), Box::new(h.installed.clone()), Box::new(h.panel.clone())).bundled(&FIXTURE_BUNDLED);
    h.host.launch(&h.lib, &h.ctx);
    assert!(!h.blocks(ad, page) && h.host.profile(&h.lib.store, &profile).loading());
    h.settle();
    assert!(h.blocks(ad, page));
}

#[test]
fn profiles_share_one_compiled_copy_of_a_bundled_list() {
    static SHARED: Bundled = Bundled::new("fixture-shared", "Fixture Shared", ListKind::Ads, "||shared.example^\n##.shared\n", "Fixture lists by the Crossbow tests.");
    static LISTS: [&Bundled; 1] = [&SHARED];
    let mut h = Harness::with_bundled(&LISTS);
    let (profile, other) = (h.profile.clone(), h.other.clone());
    let before = SHARED.compiles();
    // Both profiles start opening it before either finishes.
    for id in [&profile, &other] {
        h.host.profile(&h.lib.store, id).ads.choose(&h.lib.store, true);
        h.host.apply(id, &h.ctx);
    }
    h.settle();
    assert_eq!(SHARED.compiles(), before + 1, "compiled once for both");
    let page = "https://news.example/";
    assert!(h.installed.blocks(&profile, "https://shared.example/a.js", page) && h.installed.blocks(&other, "https://shared.example/a.js", page));
    assert!(h.installed.hides(&profile, page, page).is_some() && h.installed.hides(&other, page, page).is_some());
    let hiding = |id: &String| h.host.profiles[id].hiding.as_ref().map(|(_, hiding)| hiding.clone()).expect("element hiding");
    assert!(Arc::ptr_eq(&hiding(&profile), &hiding(&other)), "and one element hiding for the same lists");
}

/// The app's own lists: EasyList and EasyPrivacy, compiled for real.
#[test]
fn the_app_comes_with_easylist_and_easyprivacy() {
    assert!(include_str!("content_blocking.rs").contains(".bundled(&bundled::LISTS)"), "the app's host uses the bundled lists");
    let mut h = Harness::with_bundled(&bundled::LISTS);
    h.switch(ListKind::Ads, true);
    let rows = h.bundled(ListKind::Ads);
    assert_eq!(rows.iter().map(|(name, on, state, _)| (*name, *on, state.clone())).collect::<Vec<_>>(), [("EasyList", true, ListState::Ready), ("EasyPrivacy", true, ListState::Ready)]);
    assert!(rows.iter().all(|(.., counts)| counts.is_some_and(|(rules, _)| rules > 10_000)), "{rows:?}");
    let page = "https://news.example/";
    assert!(h.blocks("https://pagead2.googlesyndication.com/pagead/js/adsbygoogle.js", page));
    assert!(h.blocks("https://www.google-analytics.com/analytics.js", page));
    assert!(!h.blocks("https://news.example/app.js", page));
    assert!(h.bundled(ListKind::CookieBanners).is_empty(), "no cookie banner list comes with the app");
}

/// The UI thread's share of turning on EasyList and EasyPrivacy once they
/// are compiled: `apply` and each `poll` until the rules are installed.
/// A measurement, not a check: `cargo test -p crossbow -- --ignored
/// --nocapture ui_thread_cost`.
#[test]
#[ignore = "a timing measurement"]
fn ui_thread_cost_of_turning_on_the_bundled_lists() {
    // Kept compiled by a profile that uses them, so the runs below time
    // only what follows; each run's host builds its own element hiding.
    let mut warm = Harness::with_bundled(&bundled::LISTS);
    warm.switch(ListKind::Ads, true);
    for run in 1..=3 {
        let mut h = Harness::with_bundled(&bundled::LISTS);
        let profile = h.profile.clone();
        h.host.profile(&h.lib.store, &profile).ads.choose(&h.lib.store, true);
        let start = Instant::now();
        let mut calls = vec![{
            let t = Instant::now();
            h.host.apply(&profile, &h.ctx);
            t.elapsed()
        }];
        loop {
            let t = Instant::now();
            let waiting = h.host.poll(&h.ctx);
            calls.push(t.elapsed());
            if !waiting { break; }
            assert!(start.elapsed() < Duration::from_secs(60));
            std::thread::sleep(Duration::from_millis(5));
        }
        let ready = start.elapsed();
        assert!(h.installed.hides(&profile, "https://news.example/", "https://news.example/").is_some());
        let total: Duration = calls.iter().sum();
        let longest = calls.iter().max().unwrap();
        println!("run {run}: UI thread {:.1} ms in {} calls, longest call {:.1} ms; rules with hiding installed after {:.1} ms", total.as_secs_f64() * 1e3, calls.len(), longest.as_secs_f64() * 1e3, ready.as_secs_f64() * 1e3);
    }
}

/// Element hiding is built where the lists open, never on the UI thread:
/// at launch, when a list turns off, and when one is added. A turned-off
/// list's requests stop at once; pages keep the last hiding until the new
/// one is handed over.
#[test]
fn element_hiding_is_built_off_the_ui_thread() {
    let before = HIDINGS_BUILT_HERE.with(std::cell::Cell::get);
    let mut h = Harness::with_bundled(&FIXTURE_BUNDLED);
    let profile = h.profile.clone();
    let (page, tracker) = ("https://news.example/", "https://bundled-tracker.example/t.js");
    h.switch(ListKind::Ads, true);
    let first = h.host.profiles[&profile].hiding.clone().expect("element hiding").1;
    assert!(h.installed.hides(&profile, page, page).is_some_and(|sheet| sheet.contains(".bundled-banner{display:none!important}")));

    h.host.set_bundled(&profile, "fixture-trackers", false, &h.ctx);
    assert!(!h.blocks(tracker, page), "its requests stop at once");
    assert!(h.installed.hides(&profile, page, page).is_some_and(|sheet| sheet.contains(".bundled-banner")), "the last hiding meanwhile");
    h.settle();
    let second = h.host.profiles[&profile].hiding.clone().expect("element hiding").1;
    assert!(!Arc::ptr_eq(&first, &second), "rebuilt for the lists still on");

    let added = h.file("fixture-hiding.txt", "##.added-banner\n");
    h.add(ListKind::Ads, &added);
    assert!(h.installed.hides(&profile, page, page).is_some_and(|sheet| sheet.contains(".added-banner{display:none!important}") && sheet.contains(".bundled-banner")));
    h.reopen();
    assert!(h.installed.hides(&profile, page, page).is_some_and(|sheet| sheet.contains(".added-banner")), "and at launch");
    assert_eq!(HIDINGS_BUILT_HERE.with(std::cell::Cell::get), before, "none was built on the UI thread");
}
