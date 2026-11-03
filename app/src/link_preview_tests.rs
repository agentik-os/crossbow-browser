//! The preview's life with a fake clock, a fake fetcher, a fake resolver and
//! the fake provider: no network, no name lookup and no model.

use super::*;
use crate::ai_features::tests::{Fake, Script, configured, fixture_ready};
use crossbow_ai::local::LocalEndpoint;
use crossbow_core::link_preview::{LOCAL, Lookup, UNRESOLVED};
use crossbow_core::store::Store;
use egui::pos2;
use std::cell::{Cell, RefCell};
use std::net::IpAddr;
use std::rc::Rc;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

const PAGE_URL: &str = "https://news.example.test/today";
const LINK: &str = "https://fixture-user:fixture-secret@example.test/article?id=7";
const ADDRESS: &str = "https://example.test/article?id=7";
const OTHER: &str = "https://example.test/other";
/// What the fake resolver answers unless told otherwise.
const PUBLIC_IP: &str = "93.184.215.14";
const FIXTURE: &str = "http://127.0.0.1:8123/index.html";

#[derive(Default)]
struct FakeLookup {
    found: RefCell<Option<Vec<IpAddr>>>,
    dropped: Cell<bool>,
}

struct Asked(Rc<FakeLookup>);

impl Lookup for Asked {
    fn poll(&self) -> Option<Vec<IpAddr>> { self.0.found.borrow().clone() }
}

impl Drop for Asked {
    fn drop(&mut self) { self.0.dropped.set(true); }
}

/// Answers every name at once with a public address unless told otherwise,
/// and records each name asked.
#[derive(Default)]
struct FakeResolver {
    asked: RefCell<Vec<String>>,
    answers: RefCell<Vec<(String, Option<Vec<IpAddr>>)>>,
    lookups: RefCell<Vec<Rc<FakeLookup>>>,
}

impl FakeResolver {
    fn answer(&self, host: &str, found: &[&str]) {
        self.answers.borrow_mut().push((host.into(), Some(found.iter().map(|ip| ip.parse().unwrap()).collect())));
    }
    /// The lookup waits until the test answers it.
    fn hold(&self, host: &str) { self.answers.borrow_mut().push((host.into(), None)); }
    fn last(&self) -> Rc<FakeLookup> { self.lookups.borrow().last().cloned().expect("a lookup started") }
    fn asked(&self) -> Vec<String> { self.asked.borrow().clone() }
}

impl Resolver for FakeResolver {
    fn lookup(&self, host: &str) -> Box<dyn Lookup> {
        self.asked.borrow_mut().push(host.into());
        let found = self.answers.borrow().iter().rev().find(|(name, _)| name == host).map_or_else(|| Some(vec![PUBLIC_IP.parse().unwrap()]), |(_, found)| found.clone());
        let lookup = Rc::new(FakeLookup { found: RefCell::new(found), ..Default::default() });
        self.lookups.borrow_mut().push(lookup.clone());
        Box::new(Asked(lookup))
    }
}

/// Holds the page reader until opened, and records the thread it ran on.
/// The tests that use it take turns.
struct Gate {
    open: bool,
    ran_on: Option<std::thread::ThreadId>,
}

static GATE: Mutex<Gate> = Mutex::new(Gate { open: false, ran_on: None });
static GATE_TURN: Mutex<()> = Mutex::new(());

fn gate() -> std::sync::MutexGuard<'static, Gate> { GATE.lock().unwrap_or_else(PoisonError::into_inner) }

fn gated_text(body: &[u8], charset: &str) -> Result<PageText, &'static str> {
    gate().ran_on = Some(std::thread::current().id());
    let started = Instant::now();
    while !gate().open && started.elapsed() < Duration::from_secs(10) { std::thread::sleep(Duration::from_millis(1)); }
    read_text(body, charset)
}

fn close_gate() {
    *gate() = Gate { open: false, ran_on: None };
}

#[derive(Default)]
struct FakeFetch {
    outcome: RefCell<Option<FetchOutcome>>,
    dropped: Cell<bool>,
}

struct Handle(Rc<FakeFetch>);

impl Fetching for Handle {
    fn poll(&self) -> Option<FetchOutcome> { self.0.outcome.borrow_mut().take() }
}

impl Drop for Handle {
    // Like the engine's fetch, dropping it cancels it.
    fn drop(&mut self) { self.0.dropped.set(true); }
}

/// Records each fetch's profile and address.
#[derive(Default)]
struct FakeFetcher {
    started: RefCell<Vec<(String, String)>>,
    fetches: RefCell<Vec<Rc<FakeFetch>>>,
    refuse: Cell<bool>,
}

impl FakeFetcher {
    fn last(&self) -> Rc<FakeFetch> { self.fetches.borrow().last().cloned().expect("a fetch started") }
    fn answer(&self, outcome: FetchOutcome) { *self.last().outcome.borrow_mut() = Some(outcome); }
    fn count(&self) -> usize { self.started.borrow().len() }
}

impl Fetcher for FakeFetcher {
    fn start(&self, profile: &str, url: &str) -> Result<Box<dyn Fetching>, String> {
        if self.refuse.get() { return Err("This profile's website data isn't available.".into()); }
        self.started.borrow_mut().push((profile.to_owned(), url.to_owned()));
        let fetch = Rc::new(FakeFetch::default());
        self.fetches.borrow_mut().push(fetch.clone());
        Ok(Box::new(Handle(fetch)))
    }
}

fn html(body: &str) -> FetchOutcome {
    FetchOutcome::Page { url: ADDRESS.into(), charset: "utf-8".into(), body: body.as_bytes().to_vec(), truncated: false }
}

const ARTICLE: &str = "<title>Fixture Article</title><meta name=description content='A fixture.'><p>Fixture body text.</p><script>secret()</script>";

/// A library with the provider saved and previews on for its profile, the
/// previews, a hovered page and a clock.
struct World {
    lib: Library,
    profile: String,
    previews: Previews,
    fetcher: FakeFetcher,
    resolver: FakeResolver,
    fake: Fake,
    ctx: egui::Context,
    page: Page,
    /// Every frame's document; the page's own address when None.
    documents: Option<Vec<String>>,
    now: f64,
    link: Option<String>,
    shift: bool,
    interrupted: bool,
    over_card: bool,
    live: Vec<(u64, u64)>,
}

impl World {
    fn new(on: bool) -> World {
        let (lib, _, profile) = configured(Store::open_in_memory().unwrap(), LocalEndpoint { port: 41_500, ..Default::default() });
        if on { ai_features::enable(&lib, &profile, FEATURE, &fixture_ready(41_500)).unwrap(); }
        let page = Page { view: 1, navigations: 3, profile: profile.clone(), url: PAGE_URL.into(), private: false };
        World {
            lib, profile, previews: Previews::default(), fetcher: FakeFetcher::default(), resolver: FakeResolver::default(), fake: Fake::default(),
            ctx: egui::Context::default(), live: vec![(page.view, page.navigations)], page, documents: None, now: 10.0, link: Some(LINK.into()),
            shift: true, interrupted: false, over_card: false,
        }
    }

    /// One frame, then more at the same moment until the page's text, read
    /// on its own thread, has arrived.
    fn tick(&mut self) {
        self.tick_once();
        let started = Instant::now();
        while self.reading_text() && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(1));
            self.tick_once();
        }
    }

    fn tick_once(&mut self) {
        let documents = self.documents.clone().unwrap_or_else(|| vec![self.page.url.clone()]);
        let hover = Hover { page: self.page.clone(), link: self.link.clone(), documents, pointer: pos2(200.0, 150.0), area: Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 600.0)) };
        let input = Input { now: self.now, hover: Some(hover), shift: self.shift, interrupted: self.interrupted, over_card: self.over_card, live: &self.live };
        self.previews.tick(input, &self.lib, &self.fetcher, &self.resolver, &self.fake, &self.ctx);
    }

    fn reading_text(&self) -> bool {
        self.previews.current.as_ref().is_some_and(|preview| matches!(preview.phase, Phase::Extracting { .. }))
    }

    fn wait(&mut self, seconds: f64) {
        self.now += seconds;
        self.tick();
    }

    /// Shift held long enough: the first fetch has started.
    fn dwell(&mut self) {
        self.tick();
        self.wait(DWELL);
    }

    fn card(&self) -> Option<Card> { self.previews.shown().map(|shown| shown.card) }
    fn notice(&self) -> String {
        match self.card() { Some(Card::Notice(text)) => text, other => panic!("no notice: {other:?}") }
    }
}

#[test]
fn shift_held_on_a_link_for_half_a_second_fetches_it_in_the_pages_profile() {
    let mut w = World::new(true);
    w.tick();
    w.wait(DWELL - 0.01);
    assert_eq!(w.fetcher.count(), 0, "not before half a second");
    assert_eq!(w.card(), None, "no card while waiting");
    w.wait(0.01);
    assert_eq!(*w.fetcher.started.borrow(), [(w.profile.clone(), ADDRESS.to_owned())], "once, signed out, in the page's own profile");
    assert_eq!(w.card(), Some(Card::Working(READING.into())));
    assert_eq!(w.previews.shown().unwrap().host, "example.test");
    w.wait(1.0);
    assert_eq!(w.fetcher.count(), 1, "one preview at a time");
}

#[test]
fn releasing_shift_moving_off_a_click_or_a_scroll_before_then_fetch_nothing() {
    for case in ["release", "move off", "click or scroll", "file link"] {
        let mut w = World::new(true);
        w.tick();
        w.wait(0.2);
        match case {
            "release" => w.shift = false,
            "move off" => w.link = None,
            "click or scroll" => w.interrupted = true,
            _ => w.link = Some("file:///fixture/notes.html".into()),
        }
        w.wait(0.2);
        // A click or scroll ends the wait; holding on starts a new one.
        w.interrupted = false;
        w.wait(0.2);
        assert_eq!(w.fetcher.count(), 0, "{case}");
        assert_eq!(w.card(), None, "{case}");
    }
    // Another link starts its own wait.
    let mut w = World::new(true);
    w.tick();
    w.wait(0.3);
    w.link = Some(OTHER.into());
    w.wait(0.3);
    assert_eq!(w.fetcher.count(), 0, "the wait starts again on the other link");
    w.wait(DWELL);
    assert_eq!(w.fetcher.started.borrow()[0].1, OTHER);
}

#[test]
fn with_the_feature_off_shift_over_links_shows_and_fetches_nothing() {
    let mut w = World::new(false);
    w.dwell();
    for _ in 0..20 { w.wait(0.5); }
    assert_eq!(w.fetcher.count(), 0);
    assert_eq!(w.card(), None);
    assert!(!w.previews.busy(), "it stops checking until Shift or the link changes");
}

#[test]
fn private_profiles_never_preview() {
    let mut w = World::new(true);
    w.page.private = true;
    w.live = vec![(w.page.view, w.page.navigations)];
    w.dwell();
    assert_eq!(w.notice(), PRIVATE);
    assert_eq!(w.fetcher.count(), 0, "nothing is fetched");
    assert_eq!(w.fake.count(), 0);
}

#[test]
fn local_network_links_are_refused_except_on_their_own_local_page() {
    for link in [
        "http://192.168.1.1/admin", "http://localhost:3000/", "http://[::1]/", "http://printer.local/",
        // From the review (ai-review1): names that routers and local resolvers answer.
        "http://router.lan/", "http://nas.home.arpa/", "http://printer.home/", "http://localhost.localdomain/", "http://127.0.0.1../",
    ] {
        let mut w = World::new(true);
        w.link = Some(link.into());
        w.dwell();
        assert_eq!(w.notice(), LOCAL, "{link}");
        assert_eq!(w.fetcher.count(), 0, "{link}");
        assert!(w.resolver.asked().is_empty(), "{link} isn't looked up");
    }
    let mut w = World::new(true);
    w.page.url = FIXTURE.into();
    w.link = Some("http://127.0.0.1:8123/article.html".into());
    w.dwell();
    assert_eq!(w.fetcher.started.borrow()[0].1, "http://127.0.0.1:8123/article.html", "the loopback fixture previews its own links");
    assert!(w.resolver.asked().is_empty());
}

/// From the review (ai-review1): public names that answer with a local
/// address passed the check on the name alone and were fetched.
#[test]
fn a_name_is_looked_up_first_and_fetched_only_when_every_address_is_public() {
    let mut w = World::new(true);
    w.dwell();
    assert_eq!(w.resolver.asked(), ["example.test"]);
    assert_eq!(w.fetcher.count(), 1, "a public answer is fetched in the same frame");

    for (link, host, found) in [
        ("http://localtest.me/", "localtest.me", &["127.0.0.1"][..]),
        ("http://127.0.0.1.nip.io:11434/", "127.0.0.1.nip.io", &["127.0.0.1"]),
        ("http://127-0-0-1.sslip.io/", "127-0-0-1.sslip.io", &["127.0.0.1"]),
        ("http://192.168.1.1.nip.io/admin", "192.168.1.1.nip.io", &["192.168.1.1"]),
        ("https://mixed.example.test/", "mixed.example.test", &[PUBLIC_IP, "10.0.0.1"]),
        ("https://six.example.test/", "six.example.test", &["::1"]),
        ("https://mapped.example.test/", "mapped.example.test", &["::ffff:169.254.169.254"]),
        ("https://docs.example.test/", "docs.example.test", &["2001:db8::1"]),
    ] {
        let mut w = World::new(true);
        w.link = Some(link.into());
        w.resolver.answer(host, found);
        w.dwell();
        assert_eq!(w.resolver.asked(), [host], "{link}");
        assert_eq!(w.notice(), LOCAL, "{link}");
        assert_eq!(w.fetcher.count(), 0, "{link} is never fetched");
        assert_eq!(w.fake.count(), 0);
    }

    let mut w = World::new(true);
    w.resolver.answer("example.test", &[]);
    w.dwell();
    assert_eq!(w.notice(), UNRESOLVED, "a failed lookup means no preview");
    assert_eq!(w.fetcher.count(), 0);

    let mut w = World::new(true);
    w.link = Some("http://8.8.8.8/x".into());
    w.dwell();
    assert!(w.resolver.asked().is_empty(), "an address literal needs no lookup");
    assert_eq!(w.fetcher.count(), 1);
}

#[test]
fn a_lookup_slower_than_a_second_ends_the_preview() {
    let mut w = World::new(true);
    w.resolver.hold("example.test");
    w.dwell();
    assert_eq!(w.card(), Some(Card::Working(READING.into())), "the card shows while the name is looked up");
    w.wait(LOOKUP_SECS / 2.0);
    assert_eq!(w.card(), Some(Card::Working(READING.into())));
    assert_eq!(w.fetcher.count(), 0);
    w.wait(LOOKUP_SECS / 2.0);
    assert_eq!(w.notice(), SLOW_LOOKUP);
    assert!(w.resolver.last().dropped.get(), "the lookup is let go");
    *w.resolver.last().found.borrow_mut() = Some(vec![PUBLIC_IP.parse().unwrap()]);
    w.wait(1.0);
    assert_eq!(w.fetcher.count(), 0, "a late answer fetches nothing");

    // An answer within the second fetches, and the lookup counts against
    // the page's 4 seconds.
    let mut w = World::new(true);
    w.resolver.hold("example.test");
    w.dwell();
    w.wait(0.5);
    *w.resolver.last().found.borrow_mut() = Some(vec![PUBLIC_IP.parse().unwrap()]);
    w.tick();
    assert_eq!(w.fetcher.count(), 1);
    w.wait(3.0);
    assert_eq!(w.card(), Some(Card::Working(READING.into())));
    w.wait(0.5);
    assert_eq!(w.notice(), SLOW_PAGE);

    // Leaving the link lets the lookup go.
    let mut w = World::new(true);
    w.resolver.hold("example.test");
    w.dwell();
    w.link = None;
    w.tick();
    assert_eq!(w.card(), None);
    assert!(w.resolver.last().dropped.get());
}

#[test]
fn each_redirect_hop_is_looked_up_the_same_way() {
    let redirect = |location: &str| FetchOutcome::Redirect { status: 302, location: location.into() };
    let mut w = World::new(true);
    w.resolver.answer("localtest.me", &["127.0.0.1"]);
    w.dwell();
    w.fetcher.answer(redirect("https://www.example.test/two"));
    w.tick();
    assert_eq!(w.fetcher.count(), 2);
    w.fetcher.answer(redirect("https://localtest.me/"));
    w.tick();
    assert_eq!(w.notice(), LOCAL);
    assert_eq!(w.fetcher.count(), 2, "the local hop is never fetched");
    assert_eq!(w.resolver.asked(), ["example.test", "www.example.test", "localtest.me"]);

    let mut w = World::new(true);
    w.resolver.answer("gone.example.test", &[]);
    w.dwell();
    w.fetcher.answer(redirect("https://gone.example.test/"));
    w.tick();
    assert_eq!(w.notice(), UNRESOLVED);
    assert_eq!(w.fetcher.count(), 1);

    let mut w = World::new(true);
    w.resolver.hold("slow.example.test");
    w.dwell();
    w.fetcher.answer(redirect("https://slow.example.test/"));
    w.tick();
    w.wait(LOOKUP_SECS);
    assert_eq!(w.notice(), SLOW_LOOKUP);
    assert_eq!(w.fetcher.count(), 1);

    let mut w = World::new(true);
    w.dwell();
    w.fetcher.answer(redirect("https://8.8.8.8/"));
    w.tick();
    assert_eq!(w.fetcher.count(), 2);
    assert_eq!(w.resolver.asked(), ["example.test"], "an address literal hop needs no lookup");
}

/// Hovering doesn't say which frame holds the link, so the local exception
/// needs every frame's document on the link's origin.
#[test]
fn a_local_link_is_previewed_only_when_every_frame_shares_its_origin() {
    let link = "http://127.0.0.1:8123/article.html";
    for (documents, fetched) in [
        (vec![FIXTURE], true),
        (vec![FIXTURE, "http://127.0.0.1:8123/frame.html"], true),
        (vec![FIXTURE, "https://ads.example.test/frame"], false),
        (vec![PAGE_URL, "http://127.0.0.1:8123/frame.html"], false),
        (vec![FIXTURE, "http://127.0.0.1:8124/frame.html"], false),
    ] {
        let mut w = World::new(true);
        w.page.url = documents[0].into();
        w.documents = Some(documents.iter().map(|document| document.to_string()).collect());
        w.link = Some(link.into());
        w.dwell();
        if fetched {
            assert_eq!(w.fetcher.started.borrow()[0].1, link, "{documents:?}");
        } else {
            assert_eq!(w.notice(), LOCAL, "{documents:?}");
            assert_eq!(w.fetcher.count(), 0, "{documents:?}");
        }
        assert!(w.resolver.asked().is_empty());
    }
    for other in ["http://127.0.0.1:22/", "https://127.0.0.1:8123/article.html"] {
        let mut w = World::new(true);
        w.page.url = FIXTURE.into();
        w.link = Some(other.into());
        w.dwell();
        assert_eq!(w.notice(), LOCAL, "another port or scheme: {other}");
    }
    // A redirect on the fixture stays on its origin.
    let mut w = World::new(true);
    w.page.url = FIXTURE.into();
    w.link = Some(link.into());
    w.dwell();
    w.fetcher.answer(FetchOutcome::Redirect { status: 302, location: "http://127.0.0.1:22/".into() });
    w.tick();
    assert_eq!(w.notice(), LOCAL);
}

/// From the review (ai-review1): a 1 MiB body held the window for 36 s
/// while its text was read.
#[test]
fn the_page_text_is_read_off_the_ui_thread_which_only_receives_the_result() {
    let _turn = GATE_TURN.lock().unwrap_or_else(PoisonError::into_inner);
    close_gate();
    let mut w = World::new(true);
    w.previews.text = gated_text;
    w.fake.then(Script::Hold);
    w.dwell();
    w.fetcher.answer(html(ARTICLE));
    let started = Instant::now();
    w.tick_once();
    assert!(started.elapsed() < Duration::from_secs(5), "the frame didn't wait for the reader");
    let waited = Instant::now();
    let ran_on = loop {
        if let Some(thread) = gate().ran_on { break thread; }
        assert!(waited.elapsed() < Duration::from_secs(5), "the reader never ran");
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_ne!(ran_on, std::thread::current().id(), "read on another thread");
    for _ in 0..3 { w.tick_once(); }
    assert_eq!(w.card(), Some(Card::Working(READING.into())), "the card waits for the text");
    assert_eq!(w.fake.count(), 0);
    gate().open = true;
    w.tick();
    assert_eq!(w.fake.count(), 1, "the result arrives");
    assert!(w.fake.prompt(0).contains("Page text:\nFixture body text."));

    // A reader that outlasts the page's 4 seconds ends the preview.
    close_gate();
    let mut w = World::new(true);
    w.previews.text = gated_text;
    w.dwell();
    w.fetcher.answer(html(ARTICLE));
    w.tick_once();
    w.now += FETCH_SECS;
    w.tick_once();
    assert_eq!(w.notice(), SLOW_PAGE);
    gate().open = true;
    assert_eq!(w.fake.count(), 0);
}

#[test]
fn a_body_at_the_size_limit_is_read_quickly_off_the_ui_thread() {
    let mut w = World::new(true);
    w.fake.then(Script::Hold);
    w.dwell();
    w.fetcher.answer(FetchOutcome::Page { url: ADDRESS.into(), charset: "utf-8".into(), body: vec![b'&'; crossbow_engine::PREVIEW_BODY_BYTES], truncated: true });
    let started = Instant::now();
    w.tick();
    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    assert_eq!(w.fake.count(), 1);
}

#[test]
fn a_page_is_summarized_from_its_text_and_title_without_its_address() {
    let mut w = World::new(true);
    w.fake.then(Script::Hold);
    w.dwell();
    w.fetcher.answer(html(ARTICLE));
    w.tick();
    assert_eq!(w.fake.count(), 1);
    assert_eq!(w.card(), Some(Card::Working("Summarizing with Ollama…".into())));
    let prompt = w.fake.prompt(0);
    assert!(prompt.contains("Site: example.test\nTitle: Fixture Article\nDescription: A fixture.\n\nPage text:\nFixture body text."), "{prompt}");
    assert!(!prompt.contains("article?id=7") && !prompt.contains("fixture-secret") && !prompt.contains("secret()"), "no address, login or script: {prompt}");
    assert_eq!(w.fake.calls.borrow()[0].0, fixture_ready(41_500), "the consented destination");
    w.fake.task().finish(Ok("A fixture article about testing previews.".into()));
    w.wait(0.1);
    assert_eq!(w.card(), Some(Card::Summary { text: "A fixture article about testing previews.".into(), provider: "Ollama".into() }));
    assert!(!w.previews.busy());

    // It stays while the pointer is on the link or the card.
    w.link = None;
    w.over_card = true;
    w.wait(2.0);
    assert!(w.card().is_some(), "kept while the pointer is on the card");
    w.over_card = false;
    w.wait(0.1);
    assert_eq!(w.card(), None, "leaving both dismisses it");
}

#[test]
fn the_text_sent_is_capped_at_its_byte_limit() {
    let mut w = World::new(true);
    w.fake.then(Script::Hold);
    w.dwell();
    w.fetcher.answer(html(&format!("<p>{}</p>", "word ".repeat(5_000))));
    w.tick();
    let prompt = w.fake.prompt(0);
    let text = prompt.split_once("Page text:\n").unwrap().1;
    assert!(text.len() <= TEXT_BYTES && text.len() > TEXT_BYTES - 10, "{}", text.len());
}

#[test]
fn redirects_are_followed_three_times_and_each_hop_is_checked() {
    let redirect = |location: &str| FetchOutcome::Redirect { status: 302, location: location.into() };
    let mut w = World::new(true);
    w.fake.then(Script::Hold);
    w.dwell();
    for hop in ["/one", "https://www.example.test/two", "three"] {
        w.fetcher.answer(redirect(hop));
        w.tick();
    }
    let started: Vec<String> = w.fetcher.started.borrow().iter().map(|(_, url)| url.clone()).collect();
    assert_eq!(started, [ADDRESS, "https://example.test/one", "https://www.example.test/two", "https://www.example.test/three"]);
    assert_eq!(w.previews.shown().unwrap().host, "example.test", "the card names the last hop's site");
    w.fetcher.answer(redirect("/four"));
    w.tick();
    assert_eq!(w.notice(), crossbow_core::link_preview::TOO_MANY);
    assert_eq!(w.fetcher.count(), 4, "at most three redirects");
    assert_eq!(w.fake.count(), 0);

    for (location, reason) in [
        ("http://example.test/plain", crossbow_core::link_preview::DOWNGRADE),
        ("https://192.168.0.1/", crossbow_core::link_preview::LOCAL),
        ("https://localhost/", crossbow_core::link_preview::LOCAL),
        ("javascript:alert(1)", crossbow_core::link_preview::BAD_REDIRECT),
    ] {
        let mut w = World::new(true);
        w.dwell();
        w.fetcher.answer(redirect(location));
        w.tick();
        assert_eq!(w.notice(), reason, "{location}");
        assert_eq!(w.fetcher.count(), 1, "{location} is never fetched");
    }
}

#[test]
fn every_other_outcome_has_a_truthful_card_and_sends_nothing_to_the_model() {
    for (outcome, expected) in [
        (FetchOutcome::NotHtml { mime: "application/pdf".into(), attachment: false }, "This link is a PDF, not a web page. Nothing was sent to the model.".to_owned()),
        (FetchOutcome::NotHtml { mime: "image/png".into(), attachment: false }, "This link is an image, not a web page. Nothing was sent to the model.".into()),
        (FetchOutcome::NotHtml { mime: "text/html".into(), attachment: true }, DOWNLOAD.into()),
        (FetchOutcome::Status(404), "The site answered with HTTP status 404. Nothing was sent to the model.".into()),
        (FetchOutcome::Failed(-106), OFFLINE.into()),
        (FetchOutcome::Failed(-105), "The page couldn't load (error -105). Nothing was sent to the model.".into()),
        (html("<script>only()</script><style>p{}</style>"), NO_TEXT.into()),
        (FetchOutcome::Page { url: ADDRESS.into(), charset: "shift_jis".into(), body: b"<p>x</p>".to_vec(), truncated: false }, crossbow_core::link_preview::CHARSET.into()),
    ] {
        let mut w = World::new(true);
        w.dwell();
        w.fetcher.answer(outcome);
        w.tick();
        assert_eq!(w.notice(), expected);
        assert_eq!(w.fake.count(), 0, "{expected}");
    }
    let mut w = World::new(true);
    w.fetcher.refuse.set(true);
    w.dwell();
    assert_eq!(w.notice(), "This profile's website data isn't available.");
}

#[test]
fn a_slow_page_or_model_is_cancelled_and_says_so() {
    let mut w = World::new(true);
    w.dwell();
    w.wait(FETCH_SECS - 0.1);
    assert_eq!(w.card(), Some(Card::Working(READING.into())));
    w.wait(0.1);
    assert_eq!(w.notice(), SLOW_PAGE);
    assert!(w.fetcher.last().dropped.get(), "the fetch is cancelled");

    let mut w = World::new(true);
    w.fake.then(Script::Hold);
    w.dwell();
    w.fetcher.answer(html(ARTICLE));
    w.tick();
    w.wait(MODEL_SECS);
    assert_eq!(w.notice(), SLOW_MODEL);
    assert!(w.fake.task().cancelled.get(), "the request is cancelled");
}

#[test]
fn model_errors_and_unusable_answers_are_shown_as_such() {
    for (script, expected) in [
        (Script::Refuse(LocalError::Busy), BUSY.to_owned()),
        (Script::Refuse(LocalError::Unavailable), "Ollama isn't answering at http://127.0.0.1:41500.".into()),
        (Script::Answer(Err(LocalError::InvalidModel)), "The server no longer lists the saved model. Choose a model in AI Providers.".into()),
        (Script::Answer(Err(LocalError::Http(500))), "The local AI server returned HTTP 500.".into()),
        (Script::Answer(Ok("Line one\nline two".into())), UNUSABLE.into()),
        (Script::Answer(Ok("**Bold** summary".into())), UNUSABLE.into()),
        (Script::Answer(Ok("x".repeat(SUMMARY_CHARS + 1))), UNUSABLE.into()),
        (Script::Answer(Ok("Hidden\u{202E}text".into())), UNUSABLE.into()),
    ] {
        let mut w = World::new(true);
        w.fake.then(script);
        w.dwell();
        w.fetcher.answer(html(ARTICLE));
        w.tick();
        w.tick();
        assert_eq!(w.notice(), expected);
    }
}

#[test]
fn leaving_navigating_or_switching_away_cancels_what_is_under_way() {
    for case in ["leave", "navigate", "tab switch", "click", "scroll"] {
        let mut w = World::new(true);
        w.fake.then(Script::Hold);
        w.dwell();
        let fetch = w.fetcher.last();
        match case {
            "leave" => w.link = None,
            "navigate" => {
                w.page.navigations += 1;
                w.live = vec![(w.page.view, w.page.navigations)];
            }
            "tab switch" => {
                w.page.view = 2;
                w.live = vec![(2, 0)];
            }
            _ => w.interrupted = true,
        }
        w.over_card = case == "navigate" || case == "click";
        w.tick();
        assert_eq!(w.card(), None, "{case}");
        assert!(fetch.dropped.get(), "{case}: the fetch is cancelled");
    }

    // While the model works.
    let mut w = World::new(true);
    w.fake.then(Script::Hold);
    w.dwell();
    w.fetcher.answer(html(ARTICLE));
    w.tick();
    w.link = None;
    w.tick();
    assert!(w.fake.task().cancelled.get() && w.fake.task().dropped.get(), "the model request is cancelled");
}

#[test]
fn a_late_answer_never_shows_on_another_link() {
    let mut w = World::new(true);
    w.fake.then(Script::Hold);
    w.dwell();
    w.fetcher.answer(html(ARTICLE));
    w.tick();
    let first = w.fake.task();
    let generation = w.previews.shown().unwrap().generation;

    w.link = Some(OTHER.into());
    w.dwell();
    assert!(first.cancelled.get(), "the first request was cancelled");
    first.finish(Ok("The first article.".into()));
    w.tick();
    let shown = w.previews.shown().unwrap();
    assert!(shown.generation > generation);
    assert_eq!(shown.card, Card::Working(READING.into()), "the other link shows only its own state");
    assert_eq!(w.fetcher.started.borrow().last().unwrap().1, OTHER);
}

#[test]
fn escape_closes_a_shown_card_only() {
    let mut w = World::new(true);
    w.tick();
    assert!(!w.previews.escape(), "while waiting, Escape goes on to the page");
    w.wait(DWELL);
    assert_eq!(w.fetcher.count(), 0, "and the wait ended");
    assert!(!w.previews.escape(), "nothing to close");

    w.shift = false;
    w.tick();
    w.shift = true;
    w.dwell();
    assert!(w.card().is_some());
    assert!(w.previews.escape(), "Escape closes the card");
    assert!(w.fetcher.last().dropped.get());
    assert_eq!(w.card(), None);
    w.wait(2.0);
    assert_eq!(w.fetcher.count(), 1, "not again while Shift stays on the same link");
    w.shift = false;
    w.tick();
    w.shift = true;
    w.dwell();
    assert_eq!(w.fetcher.count(), 2, "pressing Shift again starts a new one");
}

#[test]
fn consent_for_another_destination_shows_why_and_fetches_nothing() {
    let mut w = World::new(true);
    let profile = w.profile.clone();
    let mut settings = crate::ai::SettingsDraft::load(&w.lib, &profile);
    settings.set_model(Some("other-model".into()));
    settings.save(&mut w.lib, Some(&profile));
    w.dwell();
    assert!(w.notice().contains("The AI provider changed since you turned on 5-Second Previews"), "{}", w.notice());
    assert_eq!(w.fetcher.count(), 0);
}
