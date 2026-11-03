//! Translate Page against a fake page and the fake provider: detection
//! order and the sample limit, batches and ids, errors mid-page, Show
//! Original, navigation, choices and their failures, and consent. Every
//! page and answer is a synthetic fixture; nothing reaches a model.

use super::*;
use crate::ai_features::tests::{Fake, Script, configured};
use crossbow_ai::local::{LocalEndpoint, LocalError};
use crossbow_core::store::Store;
use std::cell::{Cell, RefCell};

pub(crate) const TAB: &str = "tab-a";
pub(crate) const URL: &str = "https://journal.example/article?id=SECRET_QUERY";

pub(crate) struct FakePage {
    owner: RefCell<Owner>,
    navigations: Cell<u64>,
    url: RefCell<String>,
    loading: RefCell<bool>,
    header: RefCell<Option<String>>,
    sent: RefCell<Vec<TranslateCommand>>,
    reachable: bool,
    /// The number the document's script chose, which its reports carry: 9
    /// for a tab's first document, then one more for each new one.
    number: Cell<u64>,
    /// What each document's script did, by its number, as the script keeps
    /// it from the reports it sends (`saw`).
    scripts: RefCell<HashMap<u64, Earlier>>,
}

impl FakePage {
    /// Each fake is a view of its own: its documents' numbers start at 9,
    /// where a real page's script picks one at random, so a document one
    /// fake left must never come back in another.
    pub(crate) fn new(profile: &str, url: &str, loads: u64) -> Self {
        let owner = Owner { tab: TAB.into(), view: loads, profile: profile.into(), loads };
        Self { owner: RefCell::new(owner), navigations: Cell::new(loads), url: RefCell::new(url.into()), loading: RefCell::new(false), header: RefCell::new(None), sent: RefCell::new(Vec::new()), reachable: true, number: Cell::new(9), scripts: RefCell::new(HashMap::new()) }
    }

    pub(crate) fn number(&self) -> u64 { self.number.get() }

    /// What the document shown now did before, as its probe and hello say.
    pub(crate) fn earlier(&self) -> Earlier { self.scripts.borrow().get(&self.number()).copied().unwrap_or_default() }

    /// The document's script sent `report`: what it keeps changes as
    /// translate.js's does, within the same limits.
    fn saw(&self, report: &TranslateReport) {
        let mut scripts = self.scripts.borrow_mut();
        let state = scripts.entry(report.page()).or_default();
        match report {
            TranslateReport::Probe { sample, .. } => state.sampled |= !sample.is_empty(),
            TranslateReport::Segments { pass, segments, .. } => {
                state.nodes = (state.nodes + segments.len()).min(MAX_TRANSLATE_SEGMENTS);
                state.units = (state.units + segments.iter().map(|segment| segment.text.encode_utf16().count()).sum::<usize>()).min(MAX_TRANSLATE_CHARS);
                if state.shown == 0 { state.shown = *pass; }
            }
            TranslateReport::Restored { .. } => state.shown = 0,
            _ => {}
        }
    }

    pub(crate) fn take(&self) -> Vec<TranslateCommand> { std::mem::take(&mut *self.sent.borrow_mut()) }

    /// A new document at `url`, as a load or reload starts one.
    pub(crate) fn navigate(&self, url: &str) {
        self.owner.borrow_mut().loads += 1;
        self.navigations.set(self.navigations.get() + 1);
        *self.url.borrow_mut() = url.into();
        self.number.set(self.number.get() + 1);
    }

    /// Back or Forward to a document the back/forward cache kept: CEF
    /// starts a load in the main frame, so the view counts one, while the
    /// document's script is the one that chose `number`.
    pub(crate) fn restore(&self, url: &str, number: u64) {
        self.navigate(url);
        self.number.set(number);
    }

    /// The same document at another address, as history.pushState or a
    /// #fragment link changes it: the view counts a navigation, not a load.
    pub(crate) fn same_document(&self, url: &str) {
        self.navigations.set(self.navigations.get() + 1);
        *self.url.borrow_mut() = url.into();
    }
}

impl Page for FakePage {
    fn owner(&self) -> Owner { self.owner.borrow().clone() }
    fn navigations(&self) -> u64 { self.navigations.get() }
    fn url(&self) -> String { self.url.borrow().clone() }
    fn loading(&self) -> bool { *self.loading.borrow() }
    fn send(&self, command: &TranslateCommand) -> bool {
        self.sent.borrow_mut().push(command.clone());
        self.reachable
    }
    fn content_language(&self) -> Option<String> { self.header.borrow().clone() }
}

pub(crate) fn segments(texts: &[&str]) -> Vec<Segment> {
    texts.iter().enumerate().map(|(i, text)| Segment { id: i as u32 + 1, text: (*text).into() }).collect()
}

pub(crate) struct World {
    pub(crate) lib: Library,
    pub(crate) profile: String,
    pub(crate) translator: Translator,
    pub(crate) fake: Fake,
    pub(crate) ctx: egui::Context,
    pub(crate) now: Instant,
    path: std::path::PathBuf,
    loads: Cell<u64>,
    _root: tempfile::TempDir,
}

impl World {
    pub(crate) fn new() -> Self {
        let root = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
        let path = root.path().join("translate.sqlite");
        let (lib, _, profile) = configured(Store::open(&path).unwrap(), LocalEndpoint { port: 41_210, ..Default::default() });
        World { lib, profile, translator: Translator::new(Language::parse("en-US").unwrap()), fake: Fake::default(), ctx: egui::Context::default(), now: Instant::now(), path, loads: Cell::new(0), _root: root }
    }

    pub(crate) fn turn_on(&self) {
        let ready = ai_features::ready(&self.lib, &self.profile).unwrap();
        ai_features::enable(&self.lib, &self.profile, FEATURE, &ready).unwrap();
    }

    /// The tab showing a new document at `url`.
    pub(crate) fn page(&self, url: &str) -> FakePage {
        self.loads.set(self.loads.get() + 1000);
        FakePage::new(&self.profile, url, self.loads.get())
    }

    pub(crate) fn tick(&mut self, page: &FakePage) {
        self.translator.tick(TAB, page, &self.lib, &self.fake, &self.ctx, self.now);
    }

    pub(crate) fn report(&mut self, page: &FakePage, report: TranslateReport) {
        page.saw(&report);
        self.translator.report(TAB, report, page, &self.lib, &self.fake, &self.ctx, self.now);
    }

    pub(crate) fn probe(&mut self, page: &FakePage, lang: &str, sample: &str) {
        self.report(page, TranslateReport::Probe { page: page.number(), earlier: page.earlier(), lang: lang.into(), sample: sample.into() });
    }

    pub(crate) fn choose(&mut self, page: &FakePage, choice: Choice) -> Option<String> {
        self.translator.choose(TAB, choice, page, &self.lib, &self.fake, &self.ctx, self.now)
    }

    /// What the page was sent, each write and restore confirmed as the page
    /// script confirms them.
    pub(crate) fn confirm(&mut self, page: &FakePage) -> Vec<TranslateCommand> {
        let sent = page.take();
        for command in &sent {
            match command {
                TranslateCommand::Apply { pass, texts } => self.report(page, TranslateReport::Applied { page: page.number(), pass: *pass, written: texts.len() as u32, skipped: 0 }),
                TranslateCommand::Restore { pass } => self.report(page, TranslateReport::Restored { page: page.number(), pass: *pass, restored: 1 }),
                _ => {}
            }
        }
        sent
    }

    pub(crate) fn card(&self) -> Option<Card> { self.translator.card(TAB) }

    pub(crate) fn title(&self) -> Option<String> { self.card().map(|card| card.title) }

    pub(crate) fn reject_choices(&self, operation: &str) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(&self.path).unwrap();
        let row = if operation == "DELETE" { "OLD" } else { "NEW" };
        conn.execute_batch(&format!("CREATE TRIGGER reject_choices BEFORE {operation} ON kv WHEN {row}.k LIKE 'translate.choices.%' BEGIN SELECT RAISE(ABORT, 'fixture'); END;")).unwrap();
        conn
    }

    /// A page in French that offered, then Translate, then its text.
    pub(crate) fn translating(&mut self, texts: &[&str]) -> FakePage {
        self.turn_on();
        let page = self.page(URL);
        self.tick(&page);
        self.probe(&page, "fr", "");
        assert_eq!(self.title().as_deref(), Some("Translate this page from French?"));
        page.take();
        assert_eq!(self.choose(&page, Choice::Translate), None);
        assert_eq!(page.take(), [TranslateCommand::Collect], "Translate asks the page for its text");
        self.report(&page, TranslateReport::Segments { page: 9, pass: 4, truncated: false, segments: segments(texts) });
        page
    }
}

#[test]
fn manual_translate_starts_after_enabling_on_an_already_loaded_or_never_site() {
    for never in [false, true] {
        let mut w = World::new();
        let page = w.page(URL);
        if never {
            w.turn_on();
            change(&w.lib, &w.profile, |choices| { choices.never_sites.insert("journal.example".into()); }).unwrap();
        }
        w.tick(&page);
        assert!(page.take().is_empty());
        w.turn_on();
        assert_eq!(w.choose(&page, Choice::Translate), None);
        assert_eq!(page.take(), [TranslateCommand::Probe { sample: SAMPLE_UNITS }], "manual starts even when auto detection was skipped");
        w.probe(&page, "fr", "");
        assert_eq!(page.take(), [TranslateCommand::Collect]);
    }
}

#[test]
fn changing_consent_cancels_pending_detection_and_translation_before_an_answer_is_used() {
    for detection in [false, true] {
        for changed_provider in [false, true] {
            let mut w = World::new();
            w.fake.then(Script::Hold);
            let page = if detection {
                w.turn_on();
                let page = w.page(URL);
                w.tick(&page);
                w.probe(&page, "", "Bonjour");
                page
            } else { w.translating(&["Bonjour"]) };
            page.take();
            let task = w.fake.task();
            if changed_provider {
                let mut settings = crate::ai::SettingsDraft::load(&w.lib, &w.profile);
                settings.set_model(Some("other-model".into()));
                settings.save(&mut w.lib, Some(&w.profile.clone()));
                assert!(settings.error.is_none());
            } else { ai_features::disable(&w.lib, &w.profile, FEATURE).unwrap(); }
            w.tick(&page);
            assert!(task.cancelled.get(), "pending work is cancelled immediately: detection {detection}, changed provider {changed_provider}");
            task.finish(Ok(if detection { r#"{"language":"fr","confidence":1}"# } else { r#"{"translations":[{"id":1,"text":"Hello"}]}"# }.into()));
            w.tick(&page);
            assert!(page.take().is_empty(), "a late result is not applied and sends nothing else");
            if detection { assert!(w.card().is_none(), "an automatic probe stays quiet"); }
        }
    }
}

#[test]
fn renderer_acknowledgements_not_sends_decide_success_and_show_original_can_be_retried() {
    let mut w = World::new();
    w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Hello"},{"id":2,"text":"World"}]}"#.into())));
    let mut page = w.translating(&["Bonjour", "le monde"]);
    assert_eq!(w.title().as_deref(), Some("Translating from French…"), "a sent command is not a completed write");
    page.take();
    w.report(&page, TranslateReport::Applied { page: 9, pass: 3, written: 2, skipped: 0 });
    assert_eq!(w.title().as_deref(), Some("Translating from French…"), "another pass cannot finish this one");
    w.report(&page, TranslateReport::Applied { page: 9, pass: 4, written: 1, skipped: 1 });
    assert_eq!(w.title().as_deref(), Some("Translated from French"));
    assert_eq!(w.card().unwrap().detail.as_deref(), Some("1 passage was left untranslated."));
    page.reachable = false;
    w.choose(&page, Choice::ShowOriginal);
    assert!(w.translator.translated(TAB) && w.card().unwrap().error.is_some(), "failed dispatch keeps the restore control and explains why");
    page.reachable = true;
    w.choose(&page, Choice::ShowOriginal);
    assert!(w.translator.translated(TAB), "wait for the page to confirm restoration");
    w.report(&page, TranslateReport::Restored { page: 9, pass: 4, restored: 1 });
    assert!(!w.translator.translated(TAB) && w.card().is_none());
}

#[test]
fn languages_parse_strictly_compare_by_language_and_name_themselves() {
    for (raw, tag) in [("fr", "fr"), ("FR", "fr"), (" en-us ", "en-US"), ("pt_br", "pt-BR"), ("zh-hant-tw", "zh-Hant-TW"), ("es-419", "es-419"), ("de-CH-1996", "de-CH"), ("en-US-x-private", "en-US"), ("fil", "fil")] {
        assert_eq!(Language::parse(raw).as_ref().map(Language::tag), Some(tag), "{raw:?}");
    }
    for raw in ["", "f", "fren", "und", "mul", "zxx", "mis", "en--US", "en-US!", "<b>", "en-123456789", "i-klingon", "x-private", "fr-abc"] {
        assert_eq!(Language::parse(raw), None, "{raw:?}");
    }
    assert_eq!(Language::from_header("de-DE").map(|l| l.tag().to_owned()).as_deref(), Some("de-DE"));
    assert_eq!(Language::from_header("en-GB, en").map(|l| l.tag().to_owned()).as_deref(), Some("en-GB"), "one language in two regions");
    assert_eq!(Language::from_header("de, fr"), None, "several languages are no answer");
    assert_eq!(Language::from_header("de, nonsense!"), None);
    let lang = |raw: &str| Language::parse(raw).unwrap();
    assert!(lang("en-GB").same(&lang("en-US")) && !lang("en").same(&lang("fr")));
    assert!(!lang("zh-Hans").same(&lang("zh-Hant")) && lang("zh").same(&lang("zh-Hant")));
    assert_eq!((lang("fr-CA").name(), lang("zh-Hant").name(), lang("tlh").name()), ("French".into(), "Chinese (Traditional)".into(), "tlh".into()));
    assert!(Language::parse(&preferred_language().tag().to_owned()).is_some(), "the Mac's language reads as a tag");
}

#[test]
fn detection_reads_lang_then_content_language_then_a_sample_that_never_exceeds_the_limit() {
    let mut w = World::new();
    w.turn_on();
    // The document's lang: no request.
    let page = w.page(URL);
    *page.loading.borrow_mut() = true;
    w.tick(&page);
    assert!(page.take().is_empty(), "nothing is read while the page loads");
    *page.loading.borrow_mut() = false;
    w.tick(&page);
    assert_eq!(page.take(), [TranslateCommand::Probe { sample: SAMPLE_UNITS }]);
    w.probe(&page, "fr-CA", "Bonjour");
    assert_eq!(w.fake.count(), 0);
    assert_eq!(w.title().as_deref(), Some("Translate this page from French?"));
    assert_eq!(w.translator.get(TAB).unwrap().language.as_ref().map(Language::tag), Some("fr-CA"));

    // No lang: the response's Content-Language, still no request.
    page.navigate("https://zeitung.example/");
    *page.header.borrow_mut() = Some("de-DE".into());
    w.tick(&page);
    w.probe(&page, "", "Guten Tag");
    assert_eq!((w.fake.count(), w.title().as_deref()), (0, Some("Translate this page from German?")));

    // Neither, or an unreadable one: the sample, and nothing more.
    page.navigate("https://diario.example/");
    *page.header.borrow_mut() = Some("es, ca".into());
    w.tick(&page);
    w.fake.then(Script::Answer(Ok("```json\n{\"language\":\"es\",\"confidence\":0.93}\n```".into())));
    let sample = format!("{}\u{0007}\u{202E}", "Hola, ¿qué tal? ".repeat(60));
    w.probe(&page, "not a tag", &sample);
    assert_eq!(w.fake.count(), 1, "one request for the sample");
    let prompt = w.fake.prompt(0);
    let text = &prompt[prompt.find("Text:\n").unwrap() + 6..];
    assert!(text.len() <= SAMPLE_BYTES && text.len() > SAMPLE_BYTES - 20, "{} bytes", text.len());
    assert!(!prompt.contains('\u{0007}') && !prompt.contains('\u{202E}') && !prompt.contains("diario.example"), "no controls and no address");
    assert_eq!(w.title(), None, "nothing is offered until the answer");
    w.tick(&page);
    assert_eq!(w.title().as_deref(), Some("Translate this page from Spanish?"));
    assert!(page.take().iter().all(|command| matches!(command, TranslateCommand::Probe { .. })), "no text was collected");

    // Unsure, malformed or failed answers leave it unknown and quiet.
    for answer in [Ok(r#"{"language":"es","confidence":0.5}"#.to_string()), Ok(r#"{"language":"es","confidence":0.95,"why":"x"}"#.into()), Ok("Spanish".into()), Ok(r#"{"language":"und","confidence":1}"#.into()), Err(LocalError::Unavailable)] {
        page.navigate("https://other.example/");
        w.tick(&page);
        w.fake.then(Script::Answer(answer.clone()));
        w.probe(&page, "", "Texto de la página");
        w.tick(&page);
        assert_eq!(w.title(), None, "{answer:?}");
        assert_eq!(w.translator.get(TAB).unwrap().language, None);
    }
    // A page in the user's language, or with no text, offers nothing.
    page.navigate("https://news.example/");
    w.tick(&page);
    w.probe(&page, "en-GB", "Hello");
    assert_eq!(w.title(), None);
    let calls = w.fake.count();
    page.navigate("https://blank.example/");
    w.tick(&page);
    w.probe(&page, "", "");
    assert_eq!((w.fake.count(), w.title()), (calls, None), "no sample, no request");
}

#[test]
fn private_data_pages_and_pages_with_the_feature_off_are_never_read() {
    let mut w = World::new();
    for url in [URL, "https://other.example/"] {
        let page = w.page(url);
        w.tick(&page);
        assert!(page.take().is_empty(), "off by default: nothing is read");
    }
    w.turn_on();
    for url in ["file:///Users/fixture/notes.html", "data:text/html,<p>Bonjour</p>", "crossbow://settings", "about:blank", "view-source:https://journal.example/", "chrome://version"] {
        let page = w.page(url);
        w.tick(&page);
        assert!(page.take().is_empty(), "{url} is never read");
        assert_eq!(w.translator.translate(TAB, &page, &w.lib, w.now).unwrap_err(), "Translate Page works on web pages (http and https) only.");
        assert!(page.take().is_empty());
    }
    assert_eq!(w.fake.count(), 0);
    // Peek is a view of its own, never a tab: the app ticks tabs only.
    let app = include_str!("app.rs");
    assert!(app.contains("let page = crate::translate::TabPage { tab: &tab_id, view };"), "the translator sees tab views only");
}

#[test]
fn batches_keep_document_order_and_size_limits() {
    let big = "x".repeat(BATCH_BYTES);
    let texts: Vec<String> = (0..95).map(|i| format!("Phrase {i}")).chain([big]).chain((0..3).map(|i| format!("Fin {i}"))).collect();
    let all = segments(&texts.iter().map(String::as_str).collect::<Vec<_>>());
    let (batches, skipped) = batches(&all);
    assert_eq!(skipped, 1, "an item too large for any batch is left");
    assert_eq!(batches.iter().map(|batch| batch.items.len()).collect::<Vec<_>>(), [40, 40, 18]);
    let ids: Vec<u32> = batches.iter().flat_map(|batch| batch.items.iter().map(|item| item.id)).collect();
    assert_eq!(ids, (1..=95).chain(97..=99).collect::<Vec<_>>());
    let heavy: Vec<String> = (0..10).map(|i| format!("{i}{}", "é".repeat(700))).collect();
    let (batches, _) = super::batches(&segments(&heavy.iter().map(String::as_str).collect::<Vec<_>>()));
    for batch in &batches {
        let prompt = translate_prompt(batch, &Language::parse("en").unwrap());
        assert!(batch.items.iter().map(item_bytes).sum::<usize>() <= BATCH_BYTES && prompt.len() <= crossbow_ai::chat::MAX_PROMPT_BYTES);
    }
}

#[test]
fn ids_map_strictly_and_invented_dropped_repeated_or_unusable_answers_leave_nodes_alone() {
    let batch = Batch { items: segments(&["Bonjour", "le monde", "Au revoir", "Merci", "Oui"]) };
    let answer = r#"{"translations":[
        {"id":3,"text":"Goodbye"}, {"id":1,"text":"  Hello  "}, {"id":99,"text":"invented"},
        {"id":4,"text":"Thanks"}, {"id":4,"text":"Thank you"},
        {"id":5,"text":"Yes\u0007"}
    ]}"#;
    assert_eq!(parse_translations(answer, &batch), Ok(vec![(1, "Hello".into()), (3, "Goodbye".into())]), "2 dropped, 4 twice, 5 unusable");
    let long = format!(r#"{{"translations":[{{"id":5,"text":"{}"}}]}}"#, "y".repeat(3 * 4 + 65));
    assert_eq!(parse_translations(&long, &batch), Ok(vec![]), "far longer than its source");
    let bidi = r#"{"translations":[{"id":2,"text":"the \u202Eworld"}]}"#;
    assert_eq!(parse_translations(bidi, &batch), Ok(vec![]), "a bidirectional override the source lacked");
    let markup = r#"{"translations":[{"id":2,"text":"<b>the world</b>"}]}"#;
    assert_eq!(parse_translations(markup, &batch), Ok(vec![(2, "<b>the world</b>".into())]), "kept as text; the page writes it only as text");
    for bad in ["", "Hello", "[]", r#"{"translations":[{"id":1}]}"#, r#"{"translations":[],"extra":1}"#, r#"{"translations":[{"id":-1,"text":"x"}]}"#, "Here you go:\n```json\n{\"translations\":[]}\n```"] {
        assert!(parse_translations(bad, &batch).is_err(), "{bad:?}");
    }
}

#[test]
fn a_translation_sends_numbered_batches_and_writes_each_answer_as_it_arrives() {
    let mut w = World::new();
    let texts: Vec<String> = (1..=45).map(|i| format!("Ligne {i}")).collect();
    w.fake.then(Script::Hold);
    let page = w.translating(&texts.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(w.fake.count(), 1, "one batch at a time");
    let first = w.fake.prompt(0);
    assert!(first.contains("into English (en-US)") && first.contains(r#"{"id":1,"text":"Ligne 1"}"#) && first.contains(r#"{"id":40,"text":"Ligne 40"}"#));
    assert!(!first.contains("Ligne 41") && !first.contains("journal.example") && !first.contains("SECRET_QUERY"), "only its own items, never the address");
    assert_eq!(w.title().as_deref(), Some("Translating from French…"));
    assert_eq!(w.card().unwrap().detail.as_deref(), Some("0 of 2 parts"));
    // Reordered, with one dropped and one invented.
    let answer = format!(r#"{{"translations":[{}]}}"#, (1..=40).rev().filter(|id| *id != 7).map(|id| format!(r#"{{"id":{id},"text":"Line {id}"}}"#)).chain([r#"{"id":41,"text":"stolen"}"#.to_string()]).collect::<Vec<_>>().join(","));
    w.fake.then(Script::Hold);
    w.fake.task().finish(Ok(answer));
    w.tick(&page);
    let sent: [TranslateCommand; 1] = w.confirm(&page).try_into().expect("one apply");
    let [TranslateCommand::Apply { pass: 4, texts: written }] = sent else { panic!("an apply for pass 4") };
    assert_eq!(written.len(), 39);
    assert_eq!((written[0].clone(), written[5].clone(), written[6].clone()), ((1, "Line 1".into()), (6, "Line 6".into()), (8, "Line 8".into())), "in page order, 7 left alone");
    assert_eq!(w.card().unwrap().detail.as_deref(), Some("1 of 2 parts"), "written before the rest arrives");
    assert!(w.fake.prompt(1).contains(r#"{"id":41,"text":"Ligne 41"}"#));
    w.fake.task().finish(Ok(r#"{"translations":[{"id":41,"text":"Line 41"},{"id":42,"text":"Line 42"},{"id":43,"text":"Line 43"},{"id":44,"text":"Line 44"},{"id":45,"text":"Line 45"}]}"#.into()));
    w.tick(&page);
    assert!(matches!(w.confirm(&page).as_slice(), [TranslateCommand::Apply { pass: 4, texts }] if texts.len() == 5));
    let card = w.card().unwrap();
    assert_eq!((card.title.as_str(), card.detail.as_deref()), ("Translated from French", Some("1 passage was left untranslated.")));
    assert_eq!(card.buttons.iter().map(|(_, label)| label.as_str()).collect::<Vec<_>>(), ["Show Original", "Always Translate French", "Close"]);
    assert!(w.translator.translated(TAB));
}

#[test]
fn a_provider_error_mid_page_keeps_what_was_translated_and_retry_goes_on() {
    let mut w = World::new();
    let texts: Vec<String> = (1..=100).map(|i| format!("Ligne {i}")).collect();
    w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Line 1"}]}"#.into())));
    w.fake.then(Script::Answer(Err(LocalError::Unavailable)));
    let page = w.translating(&texts.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(matches!(w.confirm(&page).as_slice(), [TranslateCommand::Apply { texts, .. }] if texts == &[(1, "Line 1".to_string())]));
    let card = w.card().unwrap();
    assert_eq!(card.title, "Translation stopped");
    assert!(card.detail.as_deref().unwrap().starts_with(&LocalError::Unavailable.to_string()) && card.detail.as_deref().unwrap().contains("What was translated stays. 39 passages were left untranslated."), "{card:?}");
    assert_eq!(card.buttons.iter().map(|(choice, _)| choice.clone()).collect::<Vec<_>>(), [Choice::Retry, Choice::ShowOriginal, Choice::Close]);
    assert!(w.translator.translated(TAB), "a partial translation is still shown");
    // A busy worker refuses at once: the same notice path.
    w.fake.then(Script::Refuse(LocalError::Busy));
    w.choose(&page, Choice::Retry);
    assert_eq!(w.title().as_deref(), Some("Translation stopped"));
    // Retry sends the batch that failed, then the rest.
    w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":41,"text":"Line 41"}]}"#.into())));
    w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":81,"text":"Line 81"}]}"#.into())));
    w.choose(&page, Choice::Retry);
    assert!(w.fake.prompt(w.fake.count() - 2).contains(r#"{"id":41,"text":"Ligne 41"}"#));
    assert_eq!(w.confirm(&page).len(), 2);
    assert_eq!(w.title().as_deref(), Some("Translated from French"));
    // Turning the feature off stops the next batch before anything is sent.
    let mut w = World::new();
    w.fake.then(Script::Hold);
    let page = w.translating(&texts.iter().map(String::as_str).collect::<Vec<_>>());
    ai_features::disable(&w.lib, &w.profile, FEATURE).unwrap();
    w.fake.task().finish(Ok(r#"{"translations":[]}"#.into()));
    w.tick(&page);
    assert_eq!(w.fake.count(), 1, "no batch after the switch went off");
    assert!(w.card().unwrap().detail.unwrap().starts_with("Translate Pages is off."));
    page.take();
}

#[test]
fn show_original_and_a_new_load_end_the_translation_and_drop_late_answers() {
    let mut w = World::new();
    w.fake.then(Script::Hold);
    let page = w.translating(&["Bonjour", "le monde"]);
    let task = w.fake.task();
    w.choose(&page, Choice::ShowOriginal);
    assert!(task.cancelled.get(), "the pending batch is cancelled");
    assert_eq!(w.confirm(&page), [TranslateCommand::Restore { pass: 4 }]);
    assert!(!w.translator.translated(TAB) && w.card().is_none());
    task.finish(Ok(r#"{"translations":[{"id":1,"text":"Hello"}]}"#.into()));
    w.tick(&page);
    assert!(page.take().is_empty(), "a late answer is never written");
    // Translate again reads the page afresh.
    assert_eq!(w.translator.translate(TAB, &page, &w.lib, w.now), Ok(()));
    assert_eq!(page.take(), [TranslateCommand::Collect]);

    let mut w = World::new();
    w.fake.then(Script::Hold);
    let page = w.translating(&["Bonjour"]);
    let task = w.fake.task();
    page.navigate(URL);
    w.tick(&page);
    assert!(task.cancelled.get(), "a reload ends it");
    assert!(!w.translator.translated(TAB));
    // A report from the old document is ignored.
    w.report(&page, TranslateReport::Segments { page: 8, pass: 5, truncated: false, segments: segments(&["vieux"]) });
    assert_eq!(w.fake.count(), 1);
}

/// The page text a detection prompt carries.
fn detection_sample(prompt: &str) -> String {
    prompt[prompt.find("Text:\n").unwrap() + 6..].to_owned()
}

#[test]
fn a_document_sends_at_most_one_pre_consent_sample_however_often_its_address_changes() {
    let mut w = World::new();
    w.turn_on();
    let page = w.page(URL);
    w.tick(&page);
    assert_eq!(page.take(), [TranslateCommand::Probe { sample: SAMPLE_UNITS }]);
    w.fake.then(Script::Answer(Ok(r#"{"language":"en","confidence":0.95}"#.into())));
    w.probe(&page, "", &format!("Secret0 {}", "汉字".repeat(150)));
    w.tick(&page);
    for i in 1..5 {
        // history.pushState or a #fragment link: the same document, other visible text.
        page.same_document(&format!("{URL}#part-{i}"));
        w.tick(&page);
        assert!(page.take().is_empty(), "change {i}: nothing is read again");
        w.probe(&page, "", &format!("Secret{i} {}", "汉字".repeat(150)));
        w.tick(&page);
    }
    assert_eq!(w.fake.count(), 1, "one detection request for one document");
    let sample = detection_sample(&w.fake.prompt(0));
    assert!(sample.starts_with("Secret0") && sample.len() <= SAMPLE_BYTES, "{} bytes", sample.len());
    assert!(w.card().is_none(), "the user was never asked, and nothing more was sent");
    // A new document is a new page: it is read once in turn.
    page.navigate("https://journal.example/next");
    w.tick(&page);
    assert_eq!(page.take(), [TranslateCommand::Probe { sample: SAMPLE_UNITS }]);
    w.fake.then(Script::Answer(Ok(r#"{"language":"en","confidence":0.95}"#.into())));
    w.probe(&page, "", "Secret5");
    assert_eq!(w.fake.count(), 2);
}

#[test]
fn a_same_document_change_keeps_the_translation_and_show_original_restores_it() {
    let mut w = World::new();
    w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Hello"}]}"#.into())));
    let page = w.translating(&["Bonjour"]);
    assert!(matches!(w.confirm(&page).as_slice(), [TranslateCommand::Apply { pass: 4, .. }]));
    assert_eq!(w.title().as_deref(), Some("Translated from French"));
    // A table-of-contents link: the same document, still showing "Hello".
    page.same_document(&format!("{URL}#section-2"));
    w.tick(&page);
    assert!(page.take().is_empty(), "nothing is read again or restored");
    assert!(w.translator.translated(TAB));
    assert_eq!(w.title().as_deref(), Some("Translated from French"));
    w.now += Duration::from_secs(10);
    w.tick(&page);
    assert!(page.take().is_empty(), "without Always, nothing more is read on its own");
    w.choose(&page, Choice::ShowOriginal);
    assert_eq!(w.confirm(&page), [TranslateCommand::Restore { pass: 4 }]);
    assert!(!w.translator.translated(TAB) && w.card().is_none());
}

#[test]
fn an_always_language_sends_only_new_text_after_a_same_document_change() {
    let mut w = World::new();
    w.turn_on();
    change(&w.lib, &w.profile, |choices| { choices.always_languages.insert("fr".into()); }).unwrap();
    let page = w.page(URL);
    w.tick(&page);
    assert_eq!(page.take(), [TranslateCommand::Probe { sample: SAMPLE_UNITS }]);
    w.probe(&page, "fr", "");
    assert_eq!(page.take(), [TranslateCommand::Collect]);
    w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Page text"}]}"#.into())));
    w.report(&page, TranslateReport::Segments { page: 9, pass: 1, truncated: false, segments: segments(&["Texte de la page"]) });
    assert!(matches!(w.confirm(&page).as_slice(), [TranslateCommand::Apply { pass: 1, .. }]));
    assert_eq!(w.title().as_deref(), Some("Translated from French"));

    // history.pushState, and the page shows its next part.
    page.same_document(&format!("{URL}#more"));
    w.tick(&page);
    assert!(page.take().is_empty(), "no new sample, and no read while the page settles");
    w.now += Duration::from_secs(2);
    w.tick(&page);
    assert_eq!(page.take(), [TranslateCommand::Collect], "then one read of what is new");
    w.tick(&page);
    assert!(page.take().is_empty(), "once per change");
    let card = w.card().unwrap();
    assert!(w.translator.translated(TAB) && card.buttons.iter().any(|(choice, _)| *choice == Choice::ShowOriginal), "Show Original stays while the page answers: {card:?}");
    w.fake.then(Script::Hold);
    w.report(&page, TranslateReport::Segments { page: 9, pass: 2, truncated: false, segments: segments(&["Nouveau texte"]) });
    assert_eq!(w.fake.count(), 2);
    assert!(w.fake.prompt(1).contains("Nouveau texte") && !w.fake.prompt(1).contains("Texte de la page"), "only what the page reported as new");
    // Show Original covers every pass of the document.
    w.choose(&page, Choice::ShowOriginal);
    assert_eq!(w.confirm(&page), [TranslateCommand::Restore { pass: 2 }]);
    assert!(!w.translator.translated(TAB));

    // Nothing new: the translation and its card stay as they were.
    let mut w = World::new();
    w.turn_on();
    change(&w.lib, &w.profile, |choices| { choices.always_languages.insert("fr".into()); }).unwrap();
    let page = w.page(URL);
    w.tick(&page);
    w.probe(&page, "fr", "");
    w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Page text"}]}"#.into())));
    w.report(&page, TranslateReport::Segments { page: 9, pass: 1, truncated: false, segments: segments(&["Texte de la page"]) });
    w.confirm(&page);
    page.same_document(&format!("{URL}#again"));
    w.tick(&page);
    w.now += Duration::from_secs(2);
    w.tick(&page);
    assert_eq!(page.take(), [TranslateCommand::Collect]);
    w.report(&page, TranslateReport::Segments { page: 9, pass: 2, truncated: true, segments: vec![] });
    let card = w.card().unwrap();
    assert_eq!((card.title.as_str(), card.detail.as_deref()), ("Translated from French", Some("Only the first part of this long page was translated.")));
    assert_eq!(w.fake.count(), 1);
    w.choose(&page, Choice::ShowOriginal);
    assert_eq!(w.confirm(&page), [TranslateCommand::Restore { pass: 2 }]);

    // The new text translated too: Show Original asks for the latest pass,
    // which the script restores with every pass before it.
    let mut w = World::new();
    w.turn_on();
    change(&w.lib, &w.profile, |choices| { choices.always_languages.insert("fr".into()); }).unwrap();
    let page = w.page(URL);
    w.tick(&page);
    w.probe(&page, "fr", "");
    w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Page text"}]}"#.into())));
    w.report(&page, TranslateReport::Segments { page: 9, pass: 1, truncated: false, segments: segments(&["Texte de la page"]) });
    w.confirm(&page);
    page.same_document(&format!("{URL}#more"));
    w.tick(&page);
    w.now += Duration::from_secs(2);
    w.tick(&page);
    assert_eq!(page.take(), [TranslateCommand::Collect]);
    w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"New text"}]}"#.into())));
    w.report(&page, TranslateReport::Segments { page: 9, pass: 2, truncated: false, segments: segments(&["Nouveau texte"]) });
    assert!(matches!(w.confirm(&page).as_slice(), [TranslateCommand::Apply { pass: 2, .. }]));
    assert!(w.translator.translated(TAB) && w.card().unwrap().buttons.iter().any(|(choice, _)| *choice == Choice::ShowOriginal));
    w.choose(&page, Choice::ShowOriginal);
    assert_eq!(w.confirm(&page), [TranslateCommand::Restore { pass: 2 }]);
    assert!(!w.translator.translated(TAB) && w.card().is_none());
}

#[test]
fn show_original_while_new_text_is_read_ends_the_translation_and_a_silent_page_keeps_it() {
    let start = |w: &mut World| -> FakePage {
        w.turn_on();
        change(&w.lib, &w.profile, |choices| { choices.always_languages.insert("fr".into()); }).unwrap();
        let page = w.page(URL);
        w.tick(&page);
        w.probe(&page, "fr", "");
        w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Page text"}]}"#.into())));
        w.report(&page, TranslateReport::Segments { page: 9, pass: 1, truncated: false, segments: segments(&["Texte de la page"]) });
        w.confirm(&page);
        page.same_document(&format!("{URL}#more"));
        w.tick(&page);
        w.now += Duration::from_secs(2);
        w.tick(&page);
        assert_eq!(page.take(), [TranslateCommand::Collect]);
        page
    };
    // Show Original before the page answers: the pass it knows, which the
    // page accepts for every pass of the translation.
    let mut w = World::new();
    let page = start(&mut w);
    w.choose(&page, Choice::ShowOriginal);
    assert_eq!(page.take(), [TranslateCommand::Restore { pass: 1 }]);
    w.report(&page, TranslateReport::Segments { page: 9, pass: 2, truncated: false, segments: segments(&["Nouveau texte"]) });
    assert_eq!(w.fake.count(), 1, "the answer to the read is dropped");
    w.report(&page, TranslateReport::Restored { page: 9, pass: 1, restored: 1 });
    assert!(!w.translator.translated(TAB) && w.card().is_none());
    // A page that never answers keeps what is translated, and isn't asked again for the same change.
    let mut w = World::new();
    let page = start(&mut w);
    w.now += Duration::from_secs(6);
    w.tick(&page);
    assert_eq!(w.title().as_deref(), Some("Translated from French"));
    w.now += Duration::from_secs(6);
    w.tick(&page);
    assert!(page.take().is_empty());
    w.choose(&page, Choice::ShowOriginal);
    assert_eq!(page.take(), [TranslateCommand::Restore { pass: 1 }]);
}

#[test]
fn a_page_that_asks_not_to_be_translated_is_never_offered_and_says_so_when_asked() {
    const DECLINED: &str = "This page asks not to be translated.";
    let mut w = World::new();
    w.turn_on();
    change(&w.lib, &w.profile, |choices| { choices.always_languages.insert("fr".into()); }).unwrap();
    let page = w.page(URL);
    w.tick(&page);
    assert_eq!(page.take(), [TranslateCommand::Probe { sample: SAMPLE_UNITS }]);
    w.report(&page, TranslateReport::Declined { page: 9 });
    assert!(page.take().is_empty() && w.card().is_none() && w.fake.count() == 0, "no offer, no request, and Always reads nothing");
    // From the command bar: the page is asked again, and the card says why nothing happens.
    assert_eq!(w.translator.translate(TAB, &page, &w.lib, w.now), Ok(()));
    assert_eq!(page.take(), [TranslateCommand::Collect]);
    w.report(&page, TranslateReport::Declined { page: 9 });
    let card = w.card().unwrap();
    assert_eq!((card.title.as_str(), card.buttons.iter().map(|(choice, _)| choice.clone()).collect::<Vec<_>>()), (DECLINED, vec![Choice::Close]));
    assert_eq!(w.fake.count(), 0);

    // Translate chosen while the language is read: the same.
    let mut w = World::new();
    w.turn_on();
    let page = w.page(URL);
    *page.loading.borrow_mut() = true;
    assert_eq!(w.translator.translate(TAB, &page, &w.lib, w.now), Ok(()));
    *page.loading.borrow_mut() = false;
    w.tick(&page);
    assert_eq!(page.take(), [TranslateCommand::Probe { sample: SAMPLE_UNITS }]);
    w.report(&page, TranslateReport::Declined { page: 9 });
    assert_eq!(w.title().as_deref(), Some(DECLINED));
    assert!(page.take().is_empty() && w.fake.count() == 0);

    // A page that opts out after it was translated keeps the translation and Show Original.
    let mut w = World::new();
    w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Hello"}]}"#.into())));
    let page = w.translating(&["Bonjour"]);
    w.confirm(&page);
    assert_eq!(w.translator.translate(TAB, &page, &w.lib, w.now), Ok(()));
    assert_eq!(page.take(), [TranslateCommand::Collect]);
    w.report(&page, TranslateReport::Declined { page: 9 });
    assert_eq!(w.title().as_deref(), Some("Translated from French"));
    w.choose(&page, Choice::ShowOriginal);
    assert_eq!(w.confirm(&page), [TranslateCommand::Restore { pass: 4 }]);
}

#[test]
fn never_and_always_are_kept_per_profile_on_this_device_and_a_failed_save_changes_nothing() {
    let mut w = World::new();
    w.turn_on();
    let page = w.page(URL);
    w.tick(&page);
    page.take();
    w.probe(&page, "fr", "");
    let card = w.card().unwrap();
    assert_eq!(card.buttons.iter().map(|(_, label)| label.as_str()).collect::<Vec<_>>(), ["Translate", "Never for journal.example", "Never for French", "Close"]);

    // A failed save keeps the old choices and says so on the card.
    let conn = w.reject_choices("INSERT");
    assert_eq!(w.choose(&page, Choice::NeverSite), None);
    let card = w.card().unwrap();
    assert!(card.error.as_deref().unwrap().starts_with("Could not save this choice. Nothing changed."), "{card:?}");
    assert_eq!(choices(&w.lib, &w.profile), Ok(Choices::default()));
    conn.execute_batch("DROP TRIGGER reject_choices;").unwrap();
    w.choose(&page, Choice::NeverSite);
    assert_eq!(w.card(), None);
    assert_eq!(choices(&w.lib, &w.profile).unwrap().never_sites, BTreeSet::from(["journal.example".to_string()]));
    // That site is never read again, not even for its language.
    page.navigate("https://journal.example/other");
    w.tick(&page);
    assert_eq!(page.take(), []);

    // Never for French, on another site.
    let page = w.page("https://autre.example/");
    w.tick(&page);
    w.probe(&page, "fr", "");
    w.choose(&page, Choice::NeverLanguage);
    page.navigate("https://troisieme.example/");
    w.tick(&page);
    w.probe(&page, "fr-BE", "");
    assert_eq!(w.card(), None, "no offer for French anywhere");

    // Always for German translates without asking, and moves it out of Never.
    let page = w.page("https://zeitung.example/");
    w.tick(&page);
    w.probe(&page, "de", "");
    page.take();
    w.choose(&page, Choice::Translate);
    w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Hello"}]}"#.into())));
    w.report(&page, TranslateReport::Segments { page: 9, pass: 1, truncated: false, segments: segments(&["Hallo"]) });
    w.confirm(&page);
    assert!(w.card().unwrap().buttons.iter().any(|(choice, _)| *choice == Choice::Always), "offered after a translation");
    w.choose(&page, Choice::Always);
    assert!(!w.card().unwrap().buttons.iter().any(|(choice, _)| *choice == Choice::Always), "said once");
    page.navigate("https://andere.example/");
    w.tick(&page);
    page.take();
    w.probe(&page, "de-AT", "");
    assert_eq!(page.take(), [TranslateCommand::Collect], "Always: no offer, straight to the text");
    let saved = choices(&w.lib, &w.profile).unwrap();
    assert_eq!((saved.never_languages.clone(), saved.always_languages.clone(), saved.count()), (BTreeSet::from(["fr".to_string()]), BTreeSet::from(["de".to_string()]), 3));

    // Another profile has its own choices; the record lives in this device's kv.
    assert_eq!(choices(&w.lib, "fixture-other-profile"), Ok(Choices::default()));
    assert!(w.lib.store.kv_get(&format!("translate.choices.v1.{}", w.profile)).unwrap().is_some());

    // Reset: a failure keeps every choice; success clears them all.
    let conn = w.reject_choices("DELETE");
    assert!(reset_choices(&w.lib, &w.profile).unwrap_err().starts_with("Could not reset translation choices. They are unchanged."));
    assert_eq!(choices(&w.lib, &w.profile).unwrap().count(), 3);
    conn.execute_batch("DROP TRIGGER reject_choices;").unwrap();
    reset_choices(&w.lib, &w.profile).unwrap();
    assert_eq!(choices(&w.lib, &w.profile), Ok(Choices::default()));
    // And the offer comes back.
    page.navigate("https://journal.example/again");
    w.tick(&page);
    w.probe(&page, "fr", "");
    assert_eq!(w.title().as_deref(), Some("Translate this page from French?"));
    // An unreadable record is not mistaken for no choices: nothing is offered.
    w.lib.store.kv_set(&format!("translate.choices.v1.{}", w.profile), "{\"never_sites\":7}").unwrap();
    page.navigate("https://journal.example/third");
    w.tick(&page);
    // Only the earlier probe, then a hello, which reads nothing: this view
    // left a translated document, which Back could bring back.
    assert_eq!(page.take(), [TranslateCommand::Probe { sample: SAMPLE_UNITS }, TranslateCommand::Hello]);
    assert!(choices(&w.lib, &w.profile).is_err());
}

#[test]
fn consent_is_required_follows_the_provider_and_every_destination_is_loopback() {
    let mut w = World::new();
    let page = w.page(URL);
    assert_eq!(w.translator.translate(TAB, &page, &w.lib, w.now).unwrap_err(), "Translate Pages is off. Turn it on in Settings → AI Providers.");
    assert!(page.take().is_empty() && w.fake.count() == 0);
    w.turn_on();
    // The provider changed after consent: paused, nothing read or sent.
    let mut settings = crate::ai::SettingsDraft::load(&w.lib, &w.profile);
    settings.set_model(Some("other-model".into()));
    settings.save(&mut w.lib, Some(&w.profile.clone()));
    assert!(settings.error.is_none());
    assert!(w.translator.translate(TAB, &page, &w.lib, w.now).unwrap_err().starts_with("The AI provider changed since you turned on Translate Pages."));
    w.tick(&page);
    assert!(page.take().is_empty(), "no detection while paused");
    // By hand, with consent: the language is still read first, and a page
    // without one is translated anyway.
    let mut w = World::new();
    w.turn_on();
    let page = w.page(URL);
    *page.loading.borrow_mut() = true;
    assert_eq!(w.translator.translate(TAB, &page, &w.lib, w.now), Ok(()));
    assert!(page.take().is_empty(), "waits for the load");
    *page.loading.borrow_mut() = false;
    w.tick(&page);
    assert_eq!(page.take(), [TranslateCommand::Probe { sample: SAMPLE_UNITS }]);
    w.fake.then(Script::Answer(Ok(r#"{"language":"xx","confidence":0.2}"#.into())));
    w.probe(&page, "", "Mixed text");
    w.tick(&page);
    assert_eq!(page.take(), [TranslateCommand::Collect]);
    w.fake.then(Script::Hold);
    w.report(&page, TranslateReport::Segments { page: 9, pass: 1, truncated: true, segments: segments(&["Texte"]) });
    assert_eq!(w.title().as_deref(), Some("Translating…"));
    // crossbow-ai only offers servers on this Mac: there is no remote host to ask about.
    for provider in crossbow_ai::local::LocalProvider::ALL {
        assert!(LocalEndpoint { provider, port: 1 }.address().starts_with("http://127.0.0.1:"));
    }
}

/// Answer what the page was asked, as its script would, under the page's
/// own number: a hello, then a probe with this language and sample.
fn answer(w: &mut World, page: &FakePage, lang: &str, sample: &str) -> Vec<TranslateCommand> {
    let mut sent = page.take();
    let mut at = 0;
    while at < sent.len() {
        match sent[at] {
            TranslateCommand::Hello => {
                w.report(page, TranslateReport::Hello { page: page.number(), earlier: page.earlier() });
                sent.extend(page.take());
            }
            TranslateCommand::Probe { .. } => w.probe(page, lang, sample),
            _ => {}
        }
        at += 1;
    }
    sent
}

/// AI-10 (second AI review): Back or Forward to a page the back/forward
/// cache kept. CEF calls OnLoadStart after any committed navigation but a
/// same-page one (cef_load_handler.h), so the view counts a load; the
/// document, its script and the number that script chose are the ones the
/// user left (a frame entering the cache is detached and later reattached,
/// cef_frame_handler.h). The translation comes back with Show Original, and
/// nothing goes to the model again: not the sample, not the page's text.
#[test]
fn a_page_back_from_the_back_forward_cache_keeps_show_original_and_sends_nothing_again() {
    // Translated, away, and back.
    let mut w = World::new();
    w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Hello"}]}"#.into())));
    let page = w.translating(&["Bonjour"]);
    w.confirm(&page);
    assert!(w.translator.translated(TAB));
    page.navigate("https://other.example/");
    w.tick(&page);
    answer(&mut w, &page, "en", "Other");
    page.restore(URL, 9);
    w.tick(&page);
    answer(&mut w, &page, "fr", "Hello");
    w.tick(&page);
    assert!(w.translator.translated(TAB), "the restored page still shows its translation");
    let card = w.card().expect("its card");
    assert_eq!(card.title, "Translated from French");
    assert!(card.buttons.iter().any(|(choice, _)| *choice == Choice::ShowOriginal));
    assert_eq!(w.fake.count(), 1, "nothing more went to the model");
    w.choose(&page, Choice::ShowOriginal);
    assert_eq!(w.confirm(&page), [TranslateCommand::Restore { pass: 4 }]);
    assert!(!w.translator.translated(TAB));

    // Not translated and without a lang: its one sample went before it was left.
    let mut w = World::new();
    w.turn_on();
    let page = w.page(URL);
    w.tick(&page);
    w.fake.then(Script::Answer(Ok(r#"{"language":"en","confidence":0.95}"#.into())));
    answer(&mut w, &page, "", "Secret du document");
    w.tick(&page);
    assert_eq!(w.fake.count(), 1);
    page.navigate("https://other.example/");
    w.tick(&page);
    answer(&mut w, &page, "en", "Other");
    page.restore(URL, 9);
    w.tick(&page);
    answer(&mut w, &page, "", "Secret du document, toujours le même");
    w.tick(&page);
    assert_eq!(w.fake.count(), 1, "one sample for one document");

    // Translated by hand on a site the user chose Never for, which is
    // never probed on its own.
    let mut w = World::new();
    w.turn_on();
    change(&w.lib, &w.profile, |choices| { choices.never_sites.insert("journal.example".into()); }).unwrap();
    let page = w.page(URL);
    w.tick(&page);
    assert!(page.take().is_empty());
    w.choose(&page, Choice::Translate);
    answer(&mut w, &page, "fr", "");
    assert_eq!(page.take(), [TranslateCommand::Collect]);
    w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Hello"}]}"#.into())));
    w.report(&page, TranslateReport::Segments { page: 9, pass: 1, truncated: false, segments: segments(&["Bonjour"]) });
    w.confirm(&page);
    assert!(w.translator.translated(TAB));
    page.navigate("https://other.example/");
    w.tick(&page);
    answer(&mut w, &page, "en", "Other");
    page.restore(URL, 9);
    w.tick(&page);
    answer(&mut w, &page, "fr", "");
    w.tick(&page);
    assert!(w.translator.translated(TAB), "Show Original stays on a Never site too");
    assert_eq!(w.fake.count(), 1);

    // A new document at the same address, as a reload or a Back the cache
    // didn't keep gives, is a new page: read once in turn.
    let mut w = World::new();
    w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Hello"}]}"#.into())));
    let page = w.translating(&["Bonjour"]);
    w.confirm(&page);
    page.navigate(URL);
    w.tick(&page);
    answer(&mut w, &page, "fr", "");
    assert!(!w.translator.translated(TAB));
    assert_eq!(w.title().as_deref(), Some("Translate this page from French?"));
}

/// AI-10, continued: a document left while something was under way. While
/// it is away nothing more is sent for it; back, it shows what it had, and
/// goes on only when the user asks.
#[test]
fn a_page_left_mid_request_comes_back_paused_and_sends_nothing_until_asked() {
    // Its text was with the model: stopped, with Retry and Show Original.
    let mut w = World::new();
    w.fake.then(Script::Hold);
    let page = w.translating(&["Bonjour"]);
    let task = w.fake.task();
    page.navigate("https://other.example/");
    w.tick(&page);
    assert!(task.cancelled.get(), "leaving cancels the request");
    answer(&mut w, &page, "en", "Other");
    page.restore(URL, 9);
    w.tick(&page);
    answer(&mut w, &page, "fr", "");
    w.tick(&page);
    assert_eq!(w.fake.count(), 1);
    let card = w.card().expect("its card");
    assert_eq!(card.title, "Translation stopped");
    assert!(card.detail.unwrap_or_default().starts_with("You left the page before it was translated."));
    assert!(card.buttons.iter().any(|(choice, _)| *choice == Choice::ShowOriginal));
    w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Hello"}]}"#.into())));
    w.choose(&page, Choice::Retry);
    w.confirm(&page);
    assert_eq!(w.title().as_deref(), Some("Translated from French"));

    // Show Original was on its way: asked again, and the page answers.
    let mut w = World::new();
    w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Hello"}]}"#.into())));
    let page = w.translating(&["Bonjour"]);
    w.confirm(&page);
    w.choose(&page, Choice::ShowOriginal);
    assert_eq!(page.take(), [TranslateCommand::Restore { pass: 4 }], "its answer is lost with the page");
    page.navigate("https://other.example/");
    w.tick(&page);
    answer(&mut w, &page, "en", "Other");
    page.restore(URL, 9);
    w.tick(&page);
    answer(&mut w, &page, "fr", "");
    assert_eq!(w.confirm(&page), [TranslateCommand::Restore { pass: 4 }]);
    assert!(!w.translator.translated(TAB) && w.card().is_none());

    // Its sample was with the model: that answer is dropped, and the sample
    // doesn't go again; Translate still works.
    let mut w = World::new();
    w.turn_on();
    let page = w.page(URL);
    w.tick(&page);
    w.fake.then(Script::Hold);
    answer(&mut w, &page, "", "Secret du document");
    let task = w.fake.task();
    page.navigate("https://other.example/");
    w.tick(&page);
    assert!(task.cancelled.get());
    answer(&mut w, &page, "en", "Other");
    page.restore(URL, 9);
    w.tick(&page);
    answer(&mut w, &page, "", "Secret du document");
    w.tick(&page);
    answer(&mut w, &page, "", "Secret du document");
    w.tick(&page);
    assert_eq!((w.fake.count(), w.title()), (1, None), "one sample for one document");
    w.choose(&page, Choice::Translate);
    assert_eq!(page.take(), [TranslateCommand::Collect]);
}

/// The UTF-16 units of every item the provider was asked to translate.
fn units_sent(fake: &Fake) -> usize {
    (0..fake.count()).filter_map(|i| {
        let prompt = fake.prompt(i);
        let at = prompt.find("Items:\n")?;
        let items: Vec<serde_json::Value> = serde_json::from_str(&prompt[at + 7..]).unwrap();
        Some(items.iter().map(|item| item["text"].as_str().unwrap().encode_utf16().count()).sum::<usize>())
    }).sum()
}

/// AI-11 (second AI review): the browser keeps each document's own total,
/// whatever its page reports. The review's renderer, as a page that
/// tampered with the script could make it, sends 99,290 units on each of
/// four Always collects for one document: the browser takes one budget in
/// all and asks the model for nothing past it. Nodes count the same way,
/// and a translation after Show Original counts again.
#[test]
fn the_browser_takes_at_most_one_budget_of_text_per_document_whatever_the_page_sends() {
    let mut w = World::new();
    w.turn_on();
    change(&w.lib, &w.profile, |choices| { choices.always_languages.insert("fr".into()); }).unwrap();
    let page = w.page(URL);
    w.tick(&page);
    answer(&mut w, &page, "fr", "");
    assert_eq!(page.take(), [TranslateCommand::Collect]);
    let heavy: Vec<String> = (0..100).map(|i| format!("{i} {}", "a".repeat(990))).collect();
    let heavy: Vec<&str> = heavy.iter().map(String::as_str).collect();
    let mut reported = 0;
    for pass in 1..=4u64 {
        reported += heavy.iter().map(|text| text.encode_utf16().count()).sum::<usize>();
        for _ in 0..batches(&segments(&heavy)).0.len() { w.fake.then(Script::Answer(Ok(r#"{"translations":[]}"#.into()))); }
        w.report(&page, TranslateReport::Segments { page: 9, pass, truncated: false, segments: segments(&heavy) });
        for _ in 0..30 { w.tick(&page); w.confirm(&page); }
        page.same_document(&format!("{URL}#more-{pass}"));
        w.tick(&page);
        w.now += Duration::from_secs(2);
        w.tick(&page);
        page.take();
    }
    assert_eq!(reported, 397_160);
    let sent = units_sent(&w.fake);
    assert!(sent <= crossbow_engine::MAX_TRANSLATE_CHARS, "the model got {sent} UTF-16 units for one document");

    // Nodes: 1,500 one-word nodes, then 1,000 more: 500 are taken.
    let mut w = World::new();
    let words: Vec<String> = (0..2500).map(|i| format!("mot{i}")).collect();
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    for _ in 0..200 { w.fake.then(Script::Answer(Ok(r#"{"translations":[]}"#.into()))); }
    let page = w.translating(&words[..1500]);
    for _ in 0..80 { w.tick(&page); w.confirm(&page); }
    assert_eq!(w.title().as_deref(), Some("Translated from French"));
    let first = w.fake.count();
    page.same_document(&format!("{URL}#next"));
    w.choose(&page, Choice::Translate);
    assert_eq!(page.take(), [TranslateCommand::Collect]);
    w.report(&page, TranslateReport::Segments { page: 9, pass: 5, truncated: false, segments: segments(&words[1500..]) });
    for _ in 0..80 { w.tick(&page); w.confirm(&page); }
    let items: usize = (first..w.fake.count()).map(|i| { let prompt = w.fake.prompt(i); serde_json::from_str::<Vec<serde_json::Value>>(&prompt[prompt.find("Items:\n").unwrap() + 7..]).unwrap().len() }).sum();
    assert_eq!(items, crossbow_engine::MAX_TRANSLATE_SEGMENTS - 1500, "only what is left of the document's nodes");
    assert!(w.card().unwrap().detail.unwrap_or_default().contains("Only the first part of this long page was translated."));

    // Show Original, then Translate: what goes again counts again.
    w.choose(&page, Choice::ShowOriginal);
    w.confirm(&page);
    w.choose(&page, Choice::Translate);
    assert_eq!(page.take(), [TranslateCommand::Collect]);
    let before = w.fake.count();
    w.report(&page, TranslateReport::Segments { page: 9, pass: 6, truncated: false, segments: segments(&words[..10]) });
    for _ in 0..5 { w.tick(&page); w.confirm(&page); }
    assert_eq!(w.fake.count(), before, "the document's budget is spent");
    assert_eq!(w.title().as_deref(), Some("This page reached the most text Crossbow translates for one page. Reload it to translate it again."));
}

/// Visit `documents` other pages in the tab, each in English and read.
fn away(w: &mut World, page: &FakePage, documents: usize) {
    for i in 0..documents {
        page.navigate(&format!("https://site{i}.example/"));
        w.tick(page);
        answer(w, page, "en", "Other page");
        w.tick(page);
    }
}

/// AI-17 (third AI review): the tab keeps the last `LEFT` (8) documents it
/// left. A document the back/forward cache brings back after more than
/// that is no new one: its sample doesn't go again, its budget doesn't
/// start over, and the translation it shows keeps Show Original, whether
/// the feature is on or off. Within the 8 the tab's own record holds.
#[test]
fn a_document_back_after_more_documents_than_the_tab_keeps_goes_on_where_it_was() {
    for between in [7usize, 8, 9, 20] {
        // Not translated, no lang: its one sample went before it was left.
        let mut w = World::new();
        w.turn_on();
        let page = w.page(URL);
        w.tick(&page);
        w.fake.then(Script::Answer(Ok(r#"{"language":"en","confidence":0.95}"#.into())));
        answer(&mut w, &page, "", "Secret du document A");
        w.tick(&page);
        assert_eq!(w.fake.count(), 1);
        away(&mut w, &page, between);
        page.restore(URL, 9);
        w.tick(&page);
        answer(&mut w, &page, "", "Secret du document A");
        w.tick(&page);
        assert_eq!(w.fake.count(), 1, "one sample for one document, {between} documents between");

        // Translated: back, with Show Original.
        let mut w = World::new();
        w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Hello"}]}"#.into())));
        let page = w.translating(&["Bonjour"]);
        w.confirm(&page);
        away(&mut w, &page, between);
        page.restore(URL, 9);
        w.tick(&page);
        answer(&mut w, &page, "fr", "");
        w.tick(&page);
        assert!(w.translator.translated(TAB), "the page still shows its translation, {between} documents between");
        let card = w.card().expect("its card");
        assert!(card.title.starts_with("Translated") && card.buttons.iter().any(|(choice, _)| *choice == Choice::ShowOriginal), "{card:?}");
        w.choose(&page, Choice::ShowOriginal);
        assert_eq!(w.confirm(&page), [TranslateCommand::Restore { pass: 4 }]);
        assert!(!w.translator.translated(TAB) && w.card().is_none());
        assert_eq!(w.fake.count(), 1, "nothing more went to the model");

        // The same with the feature turned off meanwhile: asked only for
        // its number, and Show Original still works.
        let mut w = World::new();
        w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Hello"}]}"#.into())));
        let page = w.translating(&["Bonjour"]);
        w.confirm(&page);
        ai_features::disable(&w.lib, &w.profile, FEATURE).unwrap();
        away(&mut w, &page, between);
        page.restore(URL, 9);
        w.tick(&page);
        assert_eq!(answer(&mut w, &page, "fr", ""), [TranslateCommand::Hello], "{between} documents between");
        assert!(w.translator.translated(TAB), "{between} documents between, feature off");
        w.choose(&page, Choice::ShowOriginal);
        assert_eq!(w.confirm(&page), [TranslateCommand::Restore { pass: 4 }]);
        assert!(!w.translator.translated(TAB));

        // An Always language: the browser's total for the document goes on.
        let mut w = World::new();
        w.turn_on();
        change(&w.lib, &w.profile, |choices| { choices.always_languages.insert("fr".into()); }).unwrap();
        let page = w.page(URL);
        w.tick(&page);
        answer(&mut w, &page, "fr", "");
        assert_eq!(page.take(), [TranslateCommand::Collect]);
        let heavy: Vec<String> = (0..100).map(|i| format!("{i} {}", "a".repeat(990))).collect();
        let heavy: Vec<&str> = heavy.iter().map(String::as_str).collect();
        for _ in 0..batches(&segments(&heavy)).0.len() { w.fake.then(Script::Answer(Ok(r#"{"translations":[]}"#.into()))); }
        w.report(&page, TranslateReport::Segments { page: 9, pass: 1, truncated: false, segments: segments(&heavy) });
        for _ in 0..30 { w.tick(&page); w.confirm(&page); }
        let first = units_sent(&w.fake);
        assert!(first > crossbow_engine::MAX_TRANSLATE_CHARS - 1000);
        away(&mut w, &page, between);
        page.restore(URL, 9);
        w.tick(&page);
        let sent = answer(&mut w, &page, "fr", "");
        let mut collects = sent.iter().chain(page.take().iter()).filter(|command| matches!(command, TranslateCommand::Collect)).count();
        if collects == 0 {
            // Back as it was: a same-document change asks for what is new.
            page.same_document(&format!("{URL}#more"));
            w.tick(&page);
            w.now += Duration::from_secs(2);
            w.tick(&page);
            collects = page.take().iter().filter(|command| matches!(command, TranslateCommand::Collect)).count();
        }
        assert_eq!(collects, 1, "{between} documents between");
        for _ in 0..batches(&segments(&heavy)).0.len() { w.fake.then(Script::Answer(Ok(r#"{"translations":[]}"#.into()))); }
        w.report(&page, TranslateReport::Segments { page: 9, pass: 2, truncated: false, segments: segments(&heavy) });
        for _ in 0..30 { w.tick(&page); w.confirm(&page); }
        assert_eq!(units_sent(&w.fake), first, "the document's budget doesn't start over, {between} documents between");
    }
}
