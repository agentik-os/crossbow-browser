//! Tidy Tab Titles and Tidy Today against the shared fake provider, which
//! records every request, so "nothing was sent" is a count of zero.

use super::*;
use crate::ai_features::tests::{Fake, Script};
use crate::shortcuts::tests::Fixture;
use crate::tabs::FAVORITES_SPACE;
use crossbow_ai::local::{LocalEndpoint, LocalProvider};
use crossbow_core::store::TabRow;

pub(crate) const ENDPOINT: LocalEndpoint = LocalEndpoint { provider: LocalProvider::Ollama, port: 11434 };

pub(crate) struct World {
    pub(crate) fixture: Fixture,
    pub(crate) lib: Library,
    pub(crate) space: String,
    pub(crate) profile: String,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) fake: Fake,
    pub(crate) tidy: Tidy,
    pub(crate) ctx: egui::Context,
}

impl World {
    /// A profile with a saved loopback provider and model, both features off.
    pub(crate) fn new() -> Self {
        let fixture = Fixture::new();
        let (lib, space, _) = crate::chat::tests::configured_store(fixture.store(), ENDPOINT);
        let profile = lib.spaces[&space].profile_id.clone();
        World { fixture, lib, space, profile, tabs: Vec::new(), fake: Fake::default(), tidy: Tidy::default(), ctx: egui::Context::default() }
    }

    /// Turn On in Settings, for the saved destination.
    pub(crate) fn turn_on(&mut self, feature: &str) {
        ai_features::enable(&self.lib, &self.profile, feature, &ai_features::ready(&self.lib, &self.profile).unwrap()).unwrap();
    }

    pub(crate) fn tab(&mut self, space: &str, url: &str, title: &str) -> String {
        let id = format!("tab-{}", self.tabs.len());
        let order = format!("a{}", self.tabs.len());
        self.tabs.push(Tab::from_row(TabRow {
            id: id.clone(), space_id: space.into(), item_id: None, url: url.into(), title: title.into(), order, created_ms: 1, last_active_ms: 1,
        }));
        id
    }

    pub(crate) fn tick(&mut self) { self.tidy.tick(&self.lib, &self.fake, &self.ctx); }

    /// Answers the request the fake holds.
    pub(crate) fn answer(&self, result: Result<&str, LocalError>) { self.fake.task().finish(result.map(str::to_owned)); }

    fn notice(&mut self) -> Option<String> { self.tidy.notices.pop() }

    fn today(&self) -> Vec<String> { crate::tabs::today(&self.tabs, &self.space).iter().map(|tab| tab.id.clone()).collect() }

    fn tidy_title(&mut self, item: &str) { self.tidy.tidy_title(&self.lib, item, &self.fake, &self.ctx); }

    pub(crate) fn tidy_today(&mut self) { self.tidy.tidy_today(&self.lib, &self.tabs, &self.space, &self.fake, &self.ctx); }

    fn sent(&self, call: usize) -> Ready { self.fake.calls.borrow()[call].0.clone() }
}

fn pin(w: &mut World, url: &str, title: &str) -> String {
    let item = w.lib.add_pinned(&w.space, None, url, title).unwrap();
    w.tidy.pinned(&w.lib, &item);
    item
}

fn providers(w: &World, value: serde_json::Value) {
    w.lib.store.kv_set(&format!("ai.providers.v1.{}", w.profile), &value.to_string()).unwrap();
}

fn changed(name: &str) -> String {
    format!("The AI provider changed since you turned on {name}. Turn it on again in Settings → AI Providers to confirm the new destination.")
}

// -- Prompts and answers ------------------------------------------------------

#[test]
fn titles_parse_strictly() {
    assert_eq!(parse_title("Rust Book\n").as_deref(), Ok("Rust Book"));
    assert_eq!(parse_title("  Inbox (3)  ").as_deref(), Ok("Inbox (3)"));
    assert_eq!(parse_title("C# Guide: async/await").as_deref(), Ok("C# Guide: async/await"));
    assert_eq!(parse_title("\"Quoted\"").as_deref(), Ok("Quoted"), "one pair of quotes is removed, like every one-line answer");
    assert_eq!(parse_title("“Quoted”").as_deref(), Ok("Quoted"));
    assert_eq!(parse_title(&"x".repeat(MAX_TITLE_CHARS)).map(|t| t.len()), Ok(MAX_TITLE_CHARS));
    assert_eq!(parse_title("Café crème"), Ok("Café crème".into()));
    for bad in [
        "", "   ", "\"\"", "Two\nlines", "Two\r\nlines", "Tab\u{7}bell", "Tab\tname", "Right\u{202E}left", "Zero\u{200B}width",
        "**Bold**", "`code`", "# Heading", "- item", "+ item", "1. First", "2) Second", "> quote", "[link](https://x.test)",
        "<b>Tag</b>", "a | b", "__init__", "~~gone~~", "=== Title",
    ] {
        assert_eq!(parse_title(bad), Err(BAD_TITLE), "accepted {bad:?}");
    }
    assert_eq!(parse_title(&"x".repeat(MAX_TITLE_CHARS + 1)), Err(BAD_TITLE));
    assert_eq!(parse_title(&"é".repeat(MAX_TITLE_CHARS)).map(|t| t.chars().count()), Ok(MAX_TITLE_CHARS), "the limit counts characters, not bytes");
}

#[test]
fn groups_parse_strictly() {
    let good = r#" {"folders":[{"name":"Work","tabs":[3,1]},{"name":"Reading","tabs":[2]}]} "#;
    assert_eq!(parse_groups(good, 4), Ok(vec![
        Group { name: "Work".into(), tabs: vec![2, 0] },
        Group { name: "Reading".into(), tabs: vec![1] },
    ]), "unlisted tabs stay out");
    let many = format!(r#"{{"folders":[{{"name":"All","tabs":[{}]}}]}}"#, (1..=MAX_FOLDER_TABS).map(|n| n.to_string()).collect::<Vec<_>>().join(","));
    assert!(parse_groups(&many, MAX_FOLDER_TABS).is_ok());
    let too_many = format!(r#"{{"folders":[{{"name":"All","tabs":[{}]}}]}}"#, (1..=MAX_FOLDER_TABS + 1).map(|n| n.to_string()).collect::<Vec<_>>().join(","));
    let folders = |n: usize| format!(r#"{{"folders":[{}]}}"#, (1..=n).map(|i| format!(r#"{{"name":"F{i}","tabs":[{i}]}}"#)).collect::<Vec<_>>().join(","));
    assert!(parse_groups(&folders(MAX_FOLDERS), MAX_FOLDERS).is_ok());
    for bad in [
        String::new(), "{}".into(), r#"{"folders":[]}"#.into(),
        r#"{"folders":[{"name":"A","tabs":[1,1]}]}"#.into(),
        r#"{"folders":[{"name":"A","tabs":[1]},{"name":"B","tabs":[1]}]}"#.into(),
        r#"{"folders":[{"name":"A","tabs":[5]}]}"#.into(),
        r#"{"folders":[{"name":"A","tabs":[0]}]}"#.into(),
        r#"{"folders":[{"name":"A","tabs":[-1]}]}"#.into(),
        r#"{"folders":[{"name":"A","tabs":[1.5]}]}"#.into(),
        r#"{"folders":[{"name":"A","tabs":["1"]}]}"#.into(),
        r#"{"folders":[{"name":"A","tabs":[]}]}"#.into(),
        r#"{"folders":[{"name":"","tabs":[1]}]}"#.into(),
        r#"{"folders":[{"name":"**A**","tabs":[1]}]}"#.into(),
        r#"{"folders":[{"name":"A\nB","tabs":[1]}]}"#.into(),
        r#"{"folders":[{"name":"1. A","tabs":[1]}]}"#.into(),
        r#"{"folders":[{"name":"A","tabs":[1]},{"name":"a","tabs":[2]}]}"#.into(),
        r#"{"folders":[{"name":"A","tabs":[1],"color":"red"}]}"#.into(),
        r#"{"folders":[{"name":"A","tabs":[1]}],"note":"x"}"#.into(),
        r#"{"folders":[{"name":"A","name":"B","tabs":[1]}]}"#.into(),
        format!(r#"{{"folders":[{{"name":"{}","tabs":[1]}}]}}"#, "n".repeat(MAX_FOLDER_NAME_CHARS + 1)),
        "```json\n{\"folders\":[{\"name\":\"A\",\"tabs\":[1]}]}\n```".into(),
        "Here you go: {\"folders\":[{\"name\":\"A\",\"tabs\":[1]}]}".into(),
        too_many.clone(),
        folders(MAX_FOLDERS + 1),
    ] {
        let count = if bad == too_many { MAX_FOLDER_TABS + 1 } else if bad.contains("F7") { MAX_FOLDERS + 1 } else { 4 };
        assert_eq!(parse_groups(&bad, count), Err(BAD_GROUPS), "accepted {bad:?}");
    }
}

#[test]
fn prompts_carry_only_bounded_titles_and_hosts() {
    let title = title_prompt(&TabFacts { title: "Line\none\u{7}".into(), host: "evil.test\nSite: other".into() });
    assert!(title.ends_with("Tab title: Line one\nSite: evil.testSite:other"), "{title}");
    assert_eq!(title.lines().count(), 4, "neither field can add a line");
    let fallback = title_prompt(&TabFacts { title: " \n ".into(), host: "example.com".into() });
    assert!(fallback.contains("Tab title: example.com"), "an empty title falls back to the host");

    let facts = |n: usize| (0..n).map(|i| TabFacts { title: format!("Tab {i}"), host: format!("site{i}.example") }).collect::<Vec<_>>();
    assert_eq!(groups_prompt(&facts(MIN_TABS - 1)), None);
    assert_eq!(groups_prompt(&facts(MAX_TABS + 1)), None);
    let mut long = facts(MAX_TABS);
    for tab in &mut long { tab.title = "é".repeat(400); tab.host = "h".repeat(400); }
    let prompt = groups_prompt(&long).unwrap();
    assert!(prompt.len() <= crossbow_ai::chat::MAX_PROMPT_BYTES, "{} bytes", prompt.len());
    assert!(crossbow_ai::chat::validate_messages(&[crossbow_ai::Message::user(prompt.clone())]).is_ok());
    assert!(prompt.contains("\n1. ") && prompt.contains(&format!("\n{MAX_TABS}. ")) && !prompt.contains(&format!("\n{}. ", MAX_TABS + 1)));
    assert_eq!(prompt.lines().filter(|line| line.contains(" — ")).count(), MAX_TABS, "one line per tab");
}

// -- Tidy Tab Titles --------------------------------------------------------

#[test]
fn tidy_titles_send_nothing_while_off_without_a_ready_provider_or_profile() {
    let off = "Tidy Tab Titles is off. Turn it on in Settings → AI Providers.".to_owned();
    for case in ["off", "old-consent", "damaged-consent", "no-provider", "no-model", "provider-changed", "model-changed", "unavailable-profile"] {
        let mut w = World::new();
        assert!(!turned_on(&w.lib, &w.profile, TITLES), "off by default");
        if case != "off" { w.turn_on(TITLES); }
        let consent = format!("ai.feature.v1.tidy_titles.{}", w.profile);
        let expected = match case {
            "off" => off.clone(),
            "old-consent" => {
                w.lib.store.kv_set(&consent, &serde_json::json!({"version": ai_features::CONSENT_VERSION - 1, "endpoint": ENDPOINT, "model": "fixture-model"}).to_string()).unwrap();
                off.clone()
            }
            "damaged-consent" => {
                w.lib.store.kv_set(&consent, "{\"titles\":true}").unwrap();
                off.clone()
            }
            "no-provider" => {
                w.lib.store.kv_delete(&format!("ai.providers.v1.{}", w.profile)).unwrap();
                "Choose a provider and model in AI Providers, then Save Selection.".into()
            }
            "no-model" => {
                providers(&w, serde_json::json!({"endpoint": ENDPOINT, "model": null}));
                "Choose a model in AI Providers, then Save Selection.".into()
            }
            "provider-changed" => {
                providers(&w, serde_json::json!({"endpoint": LocalEndpoint { port: 11435, ..ENDPOINT }, "model": "fixture-model"}));
                changed(TITLES_NAME)
            }
            "model-changed" => {
                providers(&w, serde_json::json!({"endpoint": ENDPOINT, "model": "other-model"}));
                changed(TITLES_NAME)
            }
            "unavailable-profile" => {
                w.lib.profiles.remove(&w.profile);
                "This profile is unavailable.".into()
            }
            _ => unreachable!(),
        };
        let item = pin(&mut w, "https://example.com/a", "A long page title — Example");
        w.tick();
        if case == "off" { assert_eq!(w.notice(), None, "pinning says nothing while the feature is off"); }
        assert_eq!(w.tidy.can_tidy_title(&w.lib, &item).err(), Some(expected.clone()), "{case}");
        w.tidy.notices.clear();
        w.tidy_title(&item);
        assert_eq!(w.notice(), Some(expected), "the menu action explains {case}");
        assert_eq!(w.fake.count(), 0, "nothing was sent: {case}");
        assert!(w.lib.items[&item].custom_title.is_none());
    }
}

#[test]
fn each_feature_has_its_own_consent_per_profile_on_this_device_only() {
    let mut w = World::new();
    let other = w.lib.create_profile("Other").unwrap();
    let dirty = w.lib.store.dirty_count().unwrap();
    w.turn_on(TITLES);
    let reopened = Library::open(w.fixture.store()).unwrap();
    assert!(allowed(&reopened, &w.profile, TITLES, TITLES_NAME).is_ok(), "saved");
    assert!(!turned_on(&reopened, &w.profile, TODAY), "Tidy Today has its own consent");
    assert!(!turned_on(&reopened, &other, TITLES), "another profile stays off");
    assert_eq!(w.lib.store.dirty_count().unwrap(), dirty, "consent is never queued for sync");
    ai_features::disable(&w.lib, &w.profile, TITLES).unwrap();
    assert!(!turned_on(&Library::open(w.fixture.store()).unwrap(), &w.profile, TITLES));
}

#[test]
fn pinning_with_tidy_titles_on_suggests_a_title_and_accept_persists_and_syncs_like_a_rename() {
    let mut w = World::new();
    w.turn_on(TITLES);
    w.fake.then(Script::Hold);
    let item = pin(&mut w, "https://docs.example.com/guide/install?session=SECRET#step-2", "Installing the Toolkit — Example Docs — Version 4.2");
    assert_eq!(w.fake.count(), 0, "the request starts on the next frame");
    w.tick();
    assert_eq!(w.fake.count(), 1);
    assert_eq!(w.tidy.title_pending(), Some(item.as_str()));
    assert_eq!(w.sent(0), Ready { endpoint: ENDPOINT, model: "fixture-model".into() });
    let prompt = w.fake.prompt(0);
    assert!(prompt.contains("Tab title: Installing the Toolkit — Example Docs — Version 4.2\nSite: docs.example.com"), "{prompt}");
    for private in ["SECRET", "session", "/guide", "step-2", "https://"] { assert!(!prompt.contains(private), "{private} was sent"); }

    w.answer(Ok("Toolkit Install Guide\n"));
    w.tick();
    assert_eq!(w.tidy.title_pending(), None);
    assert_eq!(w.tidy.suggestion.as_ref().map(|s| s.text.as_str()), Some("Toolkit Install Guide"));
    assert!(w.lib.items[&item].custom_title.is_none(), "nothing changes before Accept");
    w.tidy.accept_suggestion(&mut w.lib);
    assert!(w.tidy.suggestion.is_none());
    assert_eq!(w.lib.items[&item].display_title(), "Toolkit Install Guide");
    assert_eq!(w.lib.items[&item].title, "Installing the Toolkit — Example Docs — Version 4.2", "the page title is kept for Revert");
    let queued = w.lib.store.dirty_changes(100).unwrap().into_iter().find(|change| change.id == item).expect("the pin is queued for sync");
    assert!(queued.payload.contains(r#""custom_title":"Toolkit Install Guide""#), "synced like any custom title: {}", queued.payload);
    let reopened = Library::open(w.fixture.store()).unwrap();
    assert_eq!(reopened.items[&item].custom_title.as_deref(), Some("Toolkit Install Guide"));
    w.tidy_title(&item);
    assert_eq!(w.notice().as_deref(), Some("You renamed this tab, so its title stays."), "an accepted title is the pin's own now");
    assert_eq!(w.fake.count(), 1);
    assert_eq!((TITLE_PROVIDER.max_tokens, TODAY_PROVIDER.max_tokens), (48, 768), "answers stay short");
}

#[test]
fn a_title_set_by_hand_is_never_replaced() {
    let mut w = World::new();
    w.turn_on(TITLES);
    let named = w.lib.add_pinned(&w.space, None, "https://named.test/", "Named page").unwrap();
    w.lib.update_item(&named, |item| item.custom_title = Some("Mine".into())).unwrap();
    assert_eq!(w.tidy.can_tidy_title(&w.lib, &named).err().as_deref(), Some("You renamed this tab, so its title stays."));
    w.tidy_title(&named);
    assert_eq!(w.fake.count(), 0);

    // Renamed while the request was pending.
    w.fake.then(Script::Hold);
    let item = pin(&mut w, "https://late.test/", "A very long title for a page");
    w.tick();
    w.lib.update_item(&item, |item| item.custom_title = Some("Hand title".into())).unwrap();
    w.answer(Ok("Short"));
    w.tick();
    assert_eq!(w.notice().as_deref(), Some("The tab changed, so its title stays."));
    assert!(w.tidy.suggestion.is_none());
    assert_eq!(w.lib.items[&item].custom_title.as_deref(), Some("Hand title"));

    // Renamed while the suggestion was shown.
    w.fake.then(Script::Hold);
    let item = pin(&mut w, "https://shown.test/", "Another very long title");
    w.tick();
    w.answer(Ok("Shown"));
    w.tick();
    assert!(w.tidy.suggestion.is_some());
    w.lib.update_item(&item, |item| item.custom_title = Some("Typed".into())).unwrap();
    w.tick();
    assert!(w.tidy.suggestion.is_none(), "a stale suggestion disappears");
    w.tidy.accept_suggestion(&mut w.lib);
    assert_eq!(w.lib.items[&item].custom_title.as_deref(), Some("Typed"));
}

#[test]
fn cancel_timeout_errors_and_invalid_answers_leave_the_title_untouched() {
    let bad = format!("Title unchanged. {BAD_TITLE}");
    let cases: Vec<(&str, Result<&str, LocalError>, &str)> = vec![
        ("timeout", Err(LocalError::Timeout), "Title unchanged. The local model didn't answer in time."),
        ("unreachable", Err(LocalError::Unavailable), "Title unchanged. Couldn't reach the local AI server."),
        ("http", Err(LocalError::Http(500)), "Title unchanged. The local AI server returned HTTP 500."),
        ("model", Err(LocalError::InvalidModel), "Title unchanged. The server no longer lists the saved model."),
        ("incomplete", Err(LocalError::Incomplete), "Title unchanged. The model's answer couldn't be used."),
        ("markdown", Ok("**Docs**"), &bad),
        ("list", Ok("1. Docs"), &bad),
        ("lines", Ok("Docs\nand more"), &bad),
        ("long", Ok("A title that goes on far longer than any tidy title should"), &bad),
        ("same", Ok("Docs"), "The title is already tidy."),
    ];
    for (case, answer, notice) in cases {
        let mut w = World::new();
        w.turn_on(TITLES);
        w.fake.then(Script::Answer(answer.map(str::to_owned)));
        let item = pin(&mut w, "https://docs.test/", "Docs");
        w.tick();
        assert_eq!(w.notice().as_deref(), Some(notice), "{case}");
        assert!(w.tidy.suggestion.is_none() && w.tidy.title_pending().is_none());
        assert!(w.lib.items[&item].custom_title.is_none(), "{case}");
    }

    let mut w = World::new();
    w.turn_on(TITLES);
    w.fake.then(Script::Hold);
    let item = pin(&mut w, "https://docs.test/", "Docs page");
    w.tick();
    w.tidy.cancel_title();
    assert!(w.fake.task().cancelled.get());
    assert_eq!(w.notice().as_deref(), Some("Title unchanged."));
    w.answer(Ok("Late"));
    w.tick();
    assert!(w.tidy.suggestion.is_none() && w.lib.items[&item].custom_title.is_none(), "a cancelled answer is never used");

    w.fake.then(Script::Refuse(LocalError::Busy));
    w.tidy_title(&item);
    assert_eq!(w.notice().as_deref(), Some("Title unchanged. Another AI request is running. Try again when it finishes."));
    assert!(w.tidy.title_pending().is_none());

    // Turned off while the request was pending.
    w.fake.then(Script::Hold);
    w.tidy_title(&item);
    ai_features::disable(&w.lib, &w.profile, TITLES).unwrap();
    w.answer(Ok("Docs"));
    w.tick();
    assert_eq!(w.notice().as_deref(), Some("Tidy Tab Titles is off, so the title stays."));
    assert!(w.tidy.suggestion.is_none());
}

#[test]
fn revert_keeps_the_page_title_edit_hands_over_and_a_failed_accept_keeps_the_suggestion() {
    let mut w = World::new();
    w.turn_on(TITLES);
    for _ in 0..3 { w.fake.then(Script::Answer(Ok("Docs".into()))); }
    let item = pin(&mut w, "https://docs.test/", "Docs page title");
    w.tick();
    w.tidy.revert_suggestion();
    assert!(w.tidy.suggestion.is_none());
    assert_eq!(w.lib.items[&item].display_title(), "Docs page title");

    w.tidy_title(&item);
    w.tick();
    assert_eq!(w.tidy.edit_suggestion(), Some((item.clone(), "Docs".into())));
    assert!(w.tidy.suggestion.is_none() && w.lib.items[&item].custom_title.is_none(), "Edit only fills the rename field");

    w.tidy_title(&item);
    w.tick();
    let connection = rusqlite::Connection::open(w.fixture.path()).unwrap();
    connection.execute_batch(&format!("CREATE TRIGGER reject_title BEFORE UPDATE ON entities WHEN NEW.id = '{item}' BEGIN SELECT RAISE(ABORT, 'fixture title failure'); END;")).unwrap();
    w.tidy.accept_suggestion(&mut w.lib);
    assert_eq!(w.notice().as_deref(), Some("Could not save the title. Try again."));
    assert!(w.lib.items[&item].custom_title.is_none());
    assert_eq!(w.tidy.suggestion.as_ref().map(|s| s.text.as_str()), Some("Docs"), "kept for another try");
    connection.execute_batch("DROP TRIGGER reject_title;").unwrap();
    w.tidy.accept_suggestion(&mut w.lib);
    assert_eq!(w.lib.items[&item].custom_title.as_deref(), Some("Docs"));
}

// -- Tidy Today ---------------------------------------------------------------

fn today_world(count: usize) -> World {
    let mut w = World::new();
    let space = w.space.clone();
    for i in 0..count { w.tab(&space, &format!("https://site{i}.test/path/{i}?token=T{i}"), &format!("Page {i}")); }
    w
}

const PROPOSAL: &str = r#"{"folders":[{"name":"Reading","tabs":[3,1]},{"name":"Work","tabs":[2]}]}"#;

#[test]
fn tidy_today_sends_nothing_while_off_without_a_provider_or_with_too_few_or_many_tabs() {
    for case in ["off", "no-provider", "model-changed", "unavailable-profile", "too-few", "too-many"] {
        let count = match case { "too-few" => 2, "too-many" => 41, _ => 4 };
        let mut w = today_world(count);
        if case != "off" { w.turn_on(TODAY); }
        let expected = match case {
            "off" => "Tidy Today is off. Turn it on in Settings → AI Providers.".to_owned(),
            "no-provider" => { w.lib.store.kv_delete(&format!("ai.providers.v1.{}", w.profile)).unwrap(); "Choose a provider and model in AI Providers, then Save Selection.".into() }
            "model-changed" => { providers(&w, serde_json::json!({"endpoint": ENDPOINT, "model": "other-model"})); changed(TODAY_NAME) }
            "unavailable-profile" => { w.lib.profiles.remove(&w.profile); "This profile is unavailable.".into() }
            "too-few" => "Tidy Today needs at least 3 Today tabs.".into(),
            "too-many" => "Tidy Today works with up to 40 Today tabs.".into(),
            _ => unreachable!(),
        };
        assert_eq!(w.tidy.can_tidy_today(&w.lib, &w.tabs, &w.space).err(), Some(expected.clone()), "{case}");
        w.tidy_today();
        assert_eq!(w.notice(), Some(expected));
        assert!(w.tidy.sheet.is_none());
        assert_eq!(w.fake.count(), 0, "nothing was sent: {case}");
    }
}

#[test]
fn tidy_today_sends_only_the_spaces_today_titles_and_hosts_in_sidebar_order() {
    let mut w = today_world(0);
    let space = w.space.clone();
    let other = w.lib.create_space("Other", "O", crossbow_core::model::Theme::preset(0), &w.profile).unwrap();
    w.turn_on(TODAY);
    w.tab(&space, "https://b.test/second?private=1", "Second");
    w.tab(&space, "https://a.test/first#private", "First");
    w.tab(&space, "https://c.test/third", "Third");
    w.tabs[1].order = "a0".into();
    w.tabs[0].order = "a1".into();
    let pinned = w.tab(&space, "https://pinned.test/", "Pinned tab");
    let item = w.lib.add_pinned(&space, None, "https://pinned.test/", "Pinned tab").unwrap();
    w.tabs.iter_mut().find(|tab| tab.id == pinned).unwrap().item_id = Some(item);
    w.tab(FAVORITES_SPACE, "https://favorite.test/", "Favorite tab");
    w.tab(&other, "https://elsewhere.test/", "Other space tab");
    w.fake.then(Script::Hold);
    w.tidy_today();
    assert_eq!(w.fake.count(), 1);
    assert_eq!(w.sent(0), Ready { endpoint: ENDPOINT, model: "fixture-model".into() });
    let prompt = w.fake.prompt(0);
    assert!(prompt.ends_with("Tabs:\n1. First — a.test\n2. Second — b.test\n3. Third — c.test\n"), "{prompt}");
    for private in ["private", "Pinned", "Favorite", "Other space", "/first", "https://"] { assert!(!prompt.contains(private), "{private} was sent"); }
    let sheet = w.tidy.sheet.as_ref().unwrap();
    assert!(matches!(sheet.stage, Stage::Asking));
    assert_eq!(sheet.destination, "Ollama (fixture-model) at http://127.0.0.1:11434");
    assert_eq!(w.tidy.can_tidy_today(&w.lib, &w.tabs, &space).err().as_deref(), Some("Tidy Today is already open."));
}

#[test]
fn accepting_applies_in_one_commit_and_undo_restores_the_exact_order() {
    let mut w = today_world(4);
    w.turn_on(TODAY);
    let existing = w.lib.add_pinned(&w.space, None, "https://pinned.test/", "Pinned").unwrap();
    let favorite = w.lib.add_favorite(&w.profile, "https://favorite.test/", "Favorite").unwrap();
    let (items, folders) = (w.lib.items.clone(), w.lib.folders.clone());
    let before: Vec<(String, String)> = w.tabs.iter().map(|tab| (tab.id.clone(), tab.order.clone())).collect();
    let order = w.today();
    w.fake.then(Script::Answer(Ok(PROPOSAL.into())));
    w.tidy_today();
    w.tick();
    let Some(Stage::Review(groups)) = w.tidy.sheet.as_ref().map(|sheet| &sheet.stage) else { panic!("review expected") };
    assert_eq!(groups.len(), 2);
    assert!(w.lib.folders.len() == folders.len() && w.tabs.iter().all(|tab| tab.item_id.is_none()), "nothing moves before Accept");

    // A failed save changes nothing and keeps the proposal.
    let connection = rusqlite::Connection::open(w.fixture.path()).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_tidy BEFORE INSERT ON entities WHEN NEW.payload LIKE '%site1.test%' BEGIN SELECT RAISE(ABORT, 'fixture tidy failure'); END;").unwrap();
    w.tidy.accept(&mut w.lib, &mut w.tabs);
    assert_eq!(w.notice().as_deref(), Some("Could not tidy Today. Nothing was changed. Try again."));
    assert_eq!((&w.lib.items, &w.lib.folders), (&items, &folders));
    assert!(w.tabs.iter().all(|tab| tab.item_id.is_none() && !tab.dirty) && w.tidy.undo.is_none());
    assert!(matches!(w.tidy.sheet.as_ref().map(|sheet| &sheet.stage), Some(Stage::Review(_))));
    connection.execute_batch("DROP TRIGGER reject_tidy;").unwrap();

    w.tidy.accept(&mut w.lib, &mut w.tabs);
    assert!(w.tidy.sheet.is_none());
    assert_eq!(w.tidy.undo.as_ref().map(|undo| undo.summary.as_str()), Some("Tidied 3 tabs into 2 folders."));
    assert_eq!(w.today(), [order[3].clone()], "the tab left out stays in Today");
    let root: Vec<String> = w.lib.pinned_children(&w.space, None).iter().map(|node| node.id().to_owned()).collect();
    assert_eq!(root[0], existing, "existing pins keep their place");
    let titles = |lib: &Library, folder: &str| -> Vec<String> {
        lib.pinned_children(&w.space, Some(folder)).iter().map(|node| match node {
            crossbow_core::library::PinnedNode::Item(_, item) => item.title.clone(),
            crossbow_core::library::PinnedNode::Folder(..) => panic!("no subfolders"),
        }).collect()
    };
    let reopened = Library::open(w.fixture.store()).unwrap();
    for lib in [&w.lib, &reopened] {
        assert_eq!(lib.folders[&root[1]].name, "Reading");
        assert_eq!(titles(lib, &root[1]), ["Page 0", "Page 2"], "in Today order, not the model's");
        assert_eq!(titles(lib, &root[2]), ["Page 1"]);
        assert_eq!(lib.items[&existing], items[&existing]);
        assert_eq!(lib.items[&favorite], items[&favorite]);
    }
    for tab in &w.tabs[..3] {
        let item = tab.item_id.as_ref().expect("tidied tabs open their pins");
        assert_eq!(w.lib.items[item].url, tab.url);
        assert!(tab.dirty);
    }

    w.tidy.undo(&mut w.lib, &mut w.tabs);
    assert!(w.tidy.undo.is_none());
    assert_eq!((&w.lib.items, &w.lib.folders), (&items, &folders), "the folders and pins it made are gone");
    assert_eq!(w.today(), order, "Today is back in its exact order");
    assert_eq!(w.tabs.iter().map(|tab| (tab.id.clone(), tab.order.clone())).collect::<Vec<_>>(), before);
    let reopened = Library::open(w.fixture.store()).unwrap();
    assert_eq!((&reopened.items, &reopened.folders), (&items, &folders));
}

#[test]
fn accepting_backs_up_the_library_first_and_a_failed_backup_never_stops_it() {
    use crossbow_core::backup::{self, Reason};
    for fails in [false, true] {
        let mut w = today_world(4);
        w.turn_on(TODAY);
        let folder = backup::folder(&w.fixture.path());
        // A file where the Backups folder belongs makes the backup fail.
        if fails { std::fs::write(&folder, b"not a folder").unwrap(); }
        w.fake.then(Script::Answer(Ok(PROPOSAL.into())));
        w.tidy_today();
        w.tick();
        assert!(backup::list(&w.fixture.path()).is_empty() && backup::failures(&w.lib.store).is_empty(), "nothing before Accept");
        let before = titles_in(&w.lib);
        w.tidy.accept(&mut w.lib, &mut w.tabs);
        assert_eq!(w.tidy.undo.as_ref().map(|undo| undo.summary.as_str()), Some("Tidied 3 tabs into 2 folders."), "fails: {fails}");
        if fails {
            assert!(backup::list(&w.fixture.path()).is_empty());
            assert_eq!(backup::failures(&w.lib.store)[0].reason(), Some(Reason::TidyToday));
        } else {
            let made = backup::list(&w.fixture.path());
            assert_eq!(made.iter().map(|backup| backup.reason).collect::<Vec<_>>(), [Reason::TidyToday]);
            // The backup holds the library from just before Accept; read a copy.
            let copy = made[0].path.with_file_name("copy.sqlite");
            std::fs::copy(&made[0].path, &copy).unwrap();
            let restored = Library::open(crossbow_core::store::Store::open(&copy).unwrap()).unwrap();
            assert_eq!(titles_in(&restored), before);
            assert!(restored.folders.len() < w.lib.folders.len());
        }
    }
}

fn titles_in(lib: &Library) -> Vec<(String, Option<String>)> {
    let mut items: Vec<_> = lib.items.values().map(|item| (item.title.clone(), item.folder_id.clone())).collect();
    items.sort();
    items
}

#[test]
fn undo_is_refused_once_the_tidied_tabs_or_folders_changed() {
    for change in ["tab-closed", "tab-unpinned", "pin-added"] {
        let mut w = today_world(3);
        w.turn_on(TODAY);
        w.fake.then(Script::Answer(Ok(PROPOSAL.into())));
        w.tidy_today();
        w.tick();
        w.tidy.accept(&mut w.lib, &mut w.tabs);
        let folder = w.lib.items[w.tabs[0].item_id.as_ref().unwrap()].folder_id.clone().unwrap();
        match change {
            "tab-closed" => { w.tabs.remove(0); }
            "tab-unpinned" => w.tabs[0].item_id = None,
            "pin-added" => { w.lib.add_pinned(&w.space, Some(&folder), "https://later.test/", "Later").unwrap(); }
            _ => unreachable!(),
        }
        let (items, folders) = (w.lib.items.clone(), w.lib.folders.clone());
        w.tidy.undo(&mut w.lib, &mut w.tabs);
        assert!(w.notice().unwrap().ends_with("so Undo is no longer available."), "{change}");
        assert_eq!((&w.lib.items, &w.lib.folders), (&items, &folders), "nothing removed after {change}");
        assert!(w.tidy.undo.is_none());
    }
}

#[test]
fn stale_proposals_cannot_be_applied_and_try_again_sends_the_current_tabs() {
    for change in ["closed", "moved", "renamed", "navigated", "pinned", "other-space", "profile-changed", "new-tab-is-fine"] {
        let mut w = today_world(4);
        w.turn_on(TODAY);
        w.fake.then(Script::Hold);
        w.tidy_today();
        match change {
            "closed" => { w.tabs.remove(1); }
            "moved" => w.tabs[0].order = "b9".into(),
            "renamed" => w.tabs[2].title = "Renamed".into(),
            "navigated" => w.tabs[2].url = "https://moved.test/".into(),
            "pinned" => w.tabs[1].item_id = Some("pin".into()),
            "other-space" => w.tabs[1].space_id = "elsewhere".into(),
            "profile-changed" => {
                let other = w.lib.create_profile("Other").unwrap();
                w.lib.update_space(&w.space, |space| space.profile_id = other).unwrap();
            }
            "new-tab-is-fine" => { let space = w.space.clone(); w.tab(&space, "https://new.test/", "New"); }
            _ => unreachable!(),
        }
        w.answer(Ok(PROPOSAL));
        w.tick();
        let stale = w.tidy.sheet.as_ref().unwrap().stale(&w.lib, &w.tabs);
        assert_eq!(stale, change != "new-tab-is-fine", "{change}");
        let folders = w.lib.folders.len();
        w.tidy.accept(&mut w.lib, &mut w.tabs);
        if stale {
            assert_eq!(w.lib.folders.len(), folders, "a stale proposal moves nothing: {change}");
            assert!(w.tidy.sheet.is_some() && w.tidy.undo.is_none());
            if change == "profile-changed" { continue; }
            w.fake.then(Script::Hold);
            w.tidy.try_again(&w.lib, &w.tabs, &w.fake, &w.ctx);
            assert_eq!(w.fake.count(), 2, "{change}");
            assert!(matches!(w.tidy.sheet.as_ref().unwrap().stage, Stage::Asking));
            assert_eq!(w.tidy.sheet.as_ref().unwrap().tabs.len(), crate::tabs::today(&w.tabs, &w.space).len());
        } else {
            assert_eq!(w.lib.folders.len(), folders + 2);
        }
    }
}

#[test]
fn cancel_timeout_errors_and_invalid_proposals_move_nothing() {
    let cases: Vec<(&str, Result<&str, LocalError>, &str)> = vec![
        ("timeout", Err(LocalError::Timeout), "The local model didn't answer in time."),
        ("error", Err(LocalError::Http(503)), "The local AI server returned HTTP 503."),
        ("unknown-number", Ok(r#"{"folders":[{"name":"A","tabs":[9]}]}"#), BAD_GROUPS),
        ("repeated-number", Ok(r#"{"folders":[{"name":"A","tabs":[1]},{"name":"B","tabs":[1]}]}"#), BAD_GROUPS),
        ("prose", Ok("Sure! Here are some folders."), BAD_GROUPS),
    ];
    for (case, answer, reason) in cases {
        let mut w = today_world(4);
        w.turn_on(TODAY);
        w.fake.then(Script::Answer(answer.map(str::to_owned)));
        w.tidy_today();
        w.tick();
        let Some(Stage::Failed(shown)) = w.tidy.sheet.as_ref().map(|sheet| &sheet.stage) else { panic!("{case} should fail") };
        assert_eq!(shown, reason);
        w.tidy.accept(&mut w.lib, &mut w.tabs);
        assert!(w.lib.folders.is_empty() && w.tabs.iter().all(|tab| tab.item_id.is_none()), "{case}");
    }

    let mut w = today_world(4);
    w.turn_on(TODAY);
    w.fake.then(Script::Hold);
    w.tidy_today();
    w.tidy.close_sheet();
    assert!(w.fake.task().cancelled.get() && w.tidy.sheet.is_none());
    w.answer(Ok(PROPOSAL));
    w.tick();
    assert!(w.tidy.sheet.is_none() && w.lib.folders.is_empty(), "a cancelled answer is never shown");

    w.fake.then(Script::Refuse(LocalError::Busy));
    w.tidy_today();
    assert!(matches!(w.tidy.sheet.as_ref().map(|sheet| &sheet.stage), Some(Stage::Failed(reason)) if reason.starts_with("Another AI request is running.")));
    w.tidy.close_sheet();

    // Turned off while the request was pending.
    w.fake.then(Script::Hold);
    w.tidy_today();
    ai_features::disable(&w.lib, &w.profile, TODAY).unwrap();
    w.answer(Ok(PROPOSAL));
    w.tick();
    assert!(matches!(w.tidy.sheet.as_ref().map(|sheet| &sheet.stage), Some(Stage::Failed(reason)) if reason == "Tidy Today was turned off or its provider changed."));
}
