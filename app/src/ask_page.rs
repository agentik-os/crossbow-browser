//! Ask on Page: an opt-in Ask mode in the Find bar (⌘F, then Tab). Enter sends
//! the question and the page's text to the saved loopback model, once: the
//! renderer's innerText, so out of view, covered and editor text too.
//! The answer is session-only and belongs to one load of one tab: Escape,
//! Cancel, closing Find, navigation, a tab switch or closing the tab drops it,
//! and a late answer is discarded with it.

use crate::ai_features::{self, Pending, Provider, Ready};
use crossbow_core::library::Library;
use crossbow_engine::PageText;
use std::time::{Duration, Instant};

pub const FEATURE: &str = "ask_on_page";
pub const NAME: &str = "Ask on Page";
/// Page text sent with a question. With the question and the instructions it
/// stays inside one 16 KiB chat message.
pub const PAGE_BYTES: usize = 12_000;
/// Raw text kept from the renderer before spaces and blank lines collapse.
pub const RAW_BYTES: usize = 4 * PAGE_BYTES;
pub const QUESTION_BYTES: usize = 1_000;
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// One load of one tab. Any change makes a question or answer stale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub tab: String,
    pub view: u64,
    pub navigations: u64,
    pub profile: String,
}

pub trait Page {
    fn owner(&self) -> Owner;
    fn url(&self) -> String;
    fn read_text(&self, cap: usize) -> Option<PageText>;
}

pub struct TabPage<'a> {
    pub tab: &'a str,
    pub view: &'a crossbow_engine::WebView,
}

impl Page for TabPage<'_> {
    fn owner(&self) -> Owner {
        Owner { tab: self.tab.into(), view: self.view.serial(), navigations: self.view.navigations(), profile: self.view.profile_id().into() }
    }
    fn url(&self) -> String { self.view.nav().url }
    fn read_text(&self, cap: usize) -> Option<PageText> { self.view.read_text(cap) }
}

/// Web pages only: never file:, data:, about:, view-source: or internal pages.
pub fn supported(url: &str) -> Result<(), &'static str> {
    const REFUSED: &str = "Ask on Page works on web pages (http and https) only. Find still works here.";
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) { return Err(REFUSED); }
    crossbow_core::search::host_of(url).map(|_| ()).ok_or(REFUSED)
}

#[derive(Debug, PartialEq, Eq)]
pub struct Extract {
    pub text: String,
    pub truncated: bool,
}

/// Plain text for the model: control and invisible formatting characters
/// removed, runs of spaces and blank lines collapsed, and at most `cap` bytes
/// kept on a character boundary. `cut` says the renderer's text was cut.
pub fn extract(raw: &str, cut: bool, cap: usize) -> Extract {
    let mut text = String::new();
    for line in raw.lines() {
        let line: String = line.chars().filter(|&c| !(c.is_control() && c != '\t') && !ai_features::invisible(c)).collect();
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() { continue; }
        let separator = usize::from(!text.is_empty());
        if text.len() + separator + line.len() > cap {
            let room = cap.saturating_sub(text.len() + separator);
            let mut end = room.min(line.len());
            while !line.is_char_boundary(end) { end -= 1; }
            let part = line[..end].trim_end();
            if !part.is_empty() {
                if separator == 1 { text.push('\n'); }
                text.push_str(part);
            }
            return Extract { text, truncated: true };
        }
        if separator == 1 { text.push('\n'); }
        text.push_str(&line);
    }
    Extract { text, truncated: cut }
}

pub fn prompt(question: &str, page: &Extract) -> String {
    format!(
        "Answer the question using only the page text below. If the page text does not contain the answer, say so. Reply in plain text.\n\nQuestion: {question}\n\nPage text{}:\n{}",
        if page.truncated { " (first part only)" } else { "" },
        page.text,
    )
}

/// Why a question cannot go out from this page, checked before any read.
fn gate(page: &dyn Page, lib: &Library, private: bool) -> Result<Ready, String> {
    if private { return Err(format!("{NAME} is unavailable in private profiles.")); }
    supported(&page.url())?;
    ai_features::allowed(lib, &page.owner().profile, FEATURE, NAME)
}

pub enum Phase {
    Idle,
    Reading { text: PageText, question: String, ready: Ready, since: Instant },
    Waiting { task: Box<dyn Pending>, ready: Ready, truncated: bool },
    Answer { text: String, from: String, truncated: bool },
    Notice(String),
}

pub struct Ask {
    owner: Owner,
    pub question: String,
    pub phase: Phase,
    /// Why nothing can be sent from this page, checked when Ask mode opens.
    pub blocked: Option<String>,
    /// Where a question would go, for the empty card.
    pub destination: Option<String>,
}

impl Ask {
    pub fn open(page: &dyn Page, lib: &Library, private: bool) -> Self {
        let (blocked, destination) = match gate(page, lib, private) {
            Ok(ready) => (None, Some(ready.destination())),
            Err(error) => (Some(error), None),
        };
        Self { owner: page.owner(), question: String::new(), phase: Phase::Idle, blocked, destination }
    }
    pub fn busy(&self) -> bool { matches!(self.phase, Phase::Reading { .. } | Phase::Waiting { .. }) }
    /// Reads the page for the typed question; the request starts in `poll`
    /// once the text arrives. Does nothing for an empty question or while busy.
    pub fn send(&mut self, page: &dyn Page, lib: &Library, private: bool, now: Instant) {
        let question = self.question.trim();
        if question.is_empty() || self.busy() { return; }
        if page.owner() != self.owner {
            self.phase = Phase::Notice("This page changed. Ask again.".into());
            return;
        }
        if question.len() > QUESTION_BYTES {
            self.phase = Phase::Notice("This question is too long. Shorten it, then press Enter again.".into());
            return;
        }
        if question.chars().any(|c| c.is_control() || ai_features::invisible(c)) {
            self.phase = Phase::Notice("Remove hidden or control characters from the question.".into());
            return;
        }
        let ready = match gate(page, lib, private) {
            Ok(ready) => ready,
            Err(error) => {
                self.blocked = Some(error.clone());
                self.phase = Phase::Notice(error);
                return;
            }
        };
        self.blocked = None;
        self.phase = match page.read_text(RAW_BYTES) {
            Some(text) => Phase::Reading { text, question: question.to_owned(), ready, since: now },
            None => Phase::Notice("Crossbow could not read this page. Find still works.".into()),
        };
    }
    pub fn poll(&mut self, provider: &dyn Provider, ctx: &egui::Context, now: Instant) {
        match &self.phase {
            Phase::Reading { text, question, ready, since } => {
                let Some((raw, cut)) = text.take() else {
                    let waited = now.saturating_duration_since(*since);
                    if waited >= READ_TIMEOUT {
                        self.phase = Phase::Notice("The page did not return its text in time. Find still works; press Enter to try again.".into());
                    } else {
                        ctx.request_repaint_after(READ_TIMEOUT - waited);
                    }
                    return;
                };
                let page = extract(&raw, cut, PAGE_BYTES);
                if page.text.is_empty() {
                    self.phase = Phase::Notice("This page has no readable text to send.".into());
                    return;
                }
                let ready = ready.clone();
                self.phase = match provider.start(&ready, prompt(question, &page), ctx) {
                    Ok(task) => Phase::Waiting { task, ready, truncated: page.truncated },
                    Err(error) => Phase::Notice(error.to_string()),
                };
            }
            Phase::Waiting { task, ready, truncated } => {
                let Some(result) = task.poll() else { return };
                self.phase = match result {
                    Ok(text) => Phase::Answer { text, from: format!("{} ({})", ready.provider(), ready.model), truncated: *truncated },
                    Err(error) => Phase::Notice(error.to_string()),
                };
            }
            _ => {}
        }
    }
    /// Discards a pending question; a late answer is never shown.
    pub fn cancel(&mut self) {
        if let Phase::Waiting { task, .. } = &self.phase { task.cancel(); }
        if self.busy() {
            self.phase = Phase::Notice("Canceled. No answer will be shown; the server may already have received the question.".into());
        }
    }
    pub fn dismiss(&mut self) {
        if !self.busy() { self.phase = Phase::Idle; }
    }
}

#[cfg(test)]
impl Ask {
    /// An Ask already in `phase`, for drawing tests.
    pub(crate) fn fixture(phase: Phase, blocked: Option<String>, destination: Option<String>) -> Self {
        let owner = Owner { tab: "tab-a".into(), view: 1, navigations: 1, profile: "profile-a".into() };
        Self { owner, question: String::new(), phase, blocked, destination }
    }
}

/// Whether this profile turned the feature on (one store read).
pub fn enabled(lib: &Library, profile: &str) -> bool {
    matches!(ai_features::consented(lib, profile, FEATURE), Ok(Some(_)))
}

/// Drops the Ask state, and with it any pending request, when Find closed or
/// the focused tab, its page load or its view changed.
pub fn reconcile(slot: &mut Option<Ask>, find_open: bool, current: Option<Owner>) {
    if slot.as_ref().is_some_and(|ask| !find_open || current.as_ref() != Some(&ask.owner)) { *slot = None; }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_features::tests::{Fake, Script, configured};
    use crossbow_ai::local::{LocalEndpoint, LocalError};
    use crossbow_core::store::Store;
    use std::cell::{Cell, RefCell};

    struct FakePage {
        owner: RefCell<Owner>,
        url: String,
        readable: bool,
        reads: Cell<usize>,
        last: RefCell<Option<PageText>>,
    }

    impl FakePage {
        fn new(profile: &str, url: &str) -> Self {
            let owner = Owner { tab: "tab-a".into(), view: 7, navigations: 1, profile: profile.into() };
            Self { owner: RefCell::new(owner), url: url.into(), readable: true, reads: Cell::new(0), last: RefCell::new(None) }
        }
        fn deliver(&self, text: &str) { self.last.borrow().as_ref().expect("the page was read").deliver(Some(text)); }
    }

    impl Page for FakePage {
        fn owner(&self) -> Owner { self.owner.borrow().clone() }
        fn url(&self) -> String { self.url.clone() }
        fn read_text(&self, cap: usize) -> Option<PageText> {
            self.reads.set(self.reads.get() + 1);
            if !self.readable { return None; }
            let text = PageText::new(cap);
            *self.last.borrow_mut() = Some(text.clone());
            Some(text)
        }
    }

    const PAGE_URL: &str = "http://127.0.0.1:8123/fixture?token=SECRET_QUERY";

    fn enabled_library() -> (Library, String) {
        let (lib, _, profile) = configured(Store::open_in_memory().unwrap(), LocalEndpoint { port: 41_100, ..Default::default() });
        let ready = crate::ai_features::ready(&lib, &profile).unwrap();
        crate::ai_features::enable(&lib, &profile, FEATURE, &ready).unwrap();
        (lib, profile)
    }

    fn notice(ask: &Ask) -> &str {
        match &ask.phase { Phase::Notice(text) => text, _ => panic!("expected a notice") }
    }

    #[test]
    fn extraction_drops_controls_collapses_space_and_caps_bytes() {
        let raw = "  Heading\u{00A0}\u{00A0}one\t\ttwo  \r\n\n\n\nBody \u{0007}text\u{202E} here\u{200B}.\n   \n\u{FEFF}Last line ";
        assert_eq!(extract(raw, false, 1_000), Extract { text: "Heading one two\nBody text here.\nLast line".into(), truncated: false });
        assert_eq!(extract(raw, true, 1_000).truncated, true, "a cut in the renderer is reported");
        let capped = extract("aaaa bbbb\ncccc é€", false, 15);
        assert_eq!(capped, Extract { text: "aaaa bbbb\ncccc".into(), truncated: true });
        let boundary = extract("abcdefghé€", false, 10);
        assert_eq!(boundary, Extract { text: "abcdefghé".into(), truncated: true }, "never splits a character");
        let long = "word ".repeat(10_000);
        let page = extract(&long, false, PAGE_BYTES);
        assert!(page.truncated && page.text.len() <= PAGE_BYTES && page.text.len() > PAGE_BYTES - 5);
        assert_eq!(extract(" \n\t\u{200B}\n ", false, PAGE_BYTES).text, "");
    }

    #[test]
    fn only_web_pages_are_supported() {
        for url in ["http://127.0.0.1:8123/fixture", "https://example.test/a?b=c", "HTTPS://Example.test/"] {
            assert_eq!(supported(url), Ok(()), "{url}");
        }
        for url in ["", "file:///Users/fixture/notes.html", "data:text/html,<p>x</p>", "about:blank", "chrome://settings", "view-source:https://example.test/", "crossbow://settings", "javascript:alert(1)", "blob:https://example.test/0000", "https://", "http:example"] {
            assert!(supported(url).is_err(), "{url} must be refused");
        }
    }

    #[test]
    fn the_prompt_holds_only_the_question_and_page_text_in_one_message() {
        let page = extract(&"x".repeat(RAW_BYTES), false, PAGE_BYTES);
        let question = "q".repeat(QUESTION_BYTES);
        let prompt = prompt(&question, &page);
        assert!(prompt.len() <= crossbow_ai::chat::MAX_PROMPT_BYTES, "{} bytes", prompt.len());
        crossbow_ai::chat::validate_messages(&[crossbow_ai::Message::user(prompt.clone())]).unwrap();
        assert!(prompt.contains("first part only"), "truncation is stated to the model");
        let small = super::prompt("What is the fixture?", &Extract { text: "Fixture body".into(), truncated: false });
        assert_eq!(small, "Answer the question using only the page text below. If the page text does not contain the answer, say so. Reply in plain text.\n\nQuestion: What is the fixture?\n\nPage text:\nFixture body");
    }

    #[test]
    fn nothing_is_read_or_sent_when_off_private_without_provider_or_on_unsupported_pages() {
        let ctx = egui::Context::default();
        let now = Instant::now();
        let fake = Fake::default();
        // Off by default.
        let (lib, _, profile) = configured(Store::open_in_memory().unwrap(), LocalEndpoint::default());
        assert!(!enabled(&lib, &profile));
        let page = FakePage::new(&profile, PAGE_URL);
        let mut ask = Ask::open(&page, &lib, false);
        assert!(ask.blocked.as_deref().unwrap().contains("off"));
        ask.question = "Question".into();
        ask.send(&page, &lib, false, now);
        ask.poll(&fake, &ctx, now);
        assert!(notice(&ask).contains("off"));
        // On, but private, unsupported, or with no provider.
        let (lib, profile) = enabled_library();
        assert!(enabled(&lib, &profile));
        for (private, url) in [(true, PAGE_URL), (false, "file:///Users/fixture/page.html"), (false, "about:blank")] {
            let page = FakePage::new(&profile, url);
            let mut ask = Ask::open(&page, &lib, private);
            assert!(ask.blocked.is_some(), "{url}");
            ask.question = "Question".into();
            ask.send(&page, &lib, private, now);
            ask.poll(&fake, &ctx, now);
            assert!(matches!(ask.phase, Phase::Notice(_)));
            assert_eq!(page.reads.get(), 0, "{url} is never read");
        }
        lib.store.kv_delete(&format!("ai.providers.v1.{profile}")).unwrap();
        let page = FakePage::new(&profile, PAGE_URL);
        let mut ask = Ask::open(&page, &lib, false);
        assert!(ask.blocked.as_deref().unwrap().contains("Save Selection"));
        ask.question = "Question".into();
        ask.send(&page, &lib, false, now);
        assert!(notice(&ask).contains("Save Selection"));
        assert_eq!(page.reads.get(), 0);
        assert_eq!(fake.count(), 0, "zero provider calls");
    }

    #[test]
    fn a_question_reads_the_page_once_and_answers_with_the_provider_name() {
        let ctx = egui::Context::default();
        let now = Instant::now();
        let (lib, profile) = enabled_library();
        let page = FakePage::new(&profile, PAGE_URL);
        let fake = Fake::with([Script::Hold]);
        let mut ask = Ask::open(&page, &lib, false);
        assert!(ask.blocked.is_none());
        assert_eq!(ask.destination.as_deref(), Some("Ollama (fixture-model) at http://127.0.0.1:41100"));
        ask.send(&page, &lib, false, now);
        assert!(matches!(ask.phase, Phase::Idle) && page.reads.get() == 0, "an empty question sends nothing");
        ask.question = "  What does the fixture say?  ".into();
        ask.send(&page, &lib, false, now);
        assert!(ask.busy() && page.reads.get() == 1);
        ask.send(&page, &lib, false, now);
        assert_eq!(page.reads.get(), 1, "Enter while waiting does not ask twice");
        ask.poll(&fake, &ctx, now);
        assert_eq!(fake.count(), 0, "nothing goes out before the page answers");
        page.deliver("Fixture heading\n\nThe fixture says hello.");
        ask.poll(&fake, &ctx, now);
        assert_eq!(fake.count(), 1);
        let sent = fake.prompt(0);
        assert!(sent.contains("Question: What does the fixture say?\n") && sent.ends_with("Page text:\nFixture heading\nThe fixture says hello."), "{sent}");
        assert!(!sent.contains("127.0.0.1:8123") && !sent.contains("SECRET_QUERY"), "the address is never sent");
        fake.task().finish(Ok("It says hello.".into()));
        ask.poll(&fake, &ctx, now);
        match &ask.phase {
            Phase::Answer { text, from, truncated } => {
                assert_eq!((text.as_str(), from.as_str(), *truncated), ("It says hello.", "Ollama (fixture-model)", false));
            }
            _ => panic!("expected an answer"),
        }
        assert_eq!(ask.question, "  What does the fixture say?  ", "the question stays for a follow-up");
        ask.dismiss();
        assert!(matches!(ask.phase, Phase::Idle));

        // A long page is cut, and the card says so.
        let fake = Fake::with([Script::Answer(Ok("Long answer".into()))]);
        ask.send(&page, &lib, false, now);
        page.deliver(&"word ".repeat(20_000));
        ask.poll(&fake, &ctx, now);
        assert!(fake.prompt(0).contains("Page text (first part only):"));
        ask.poll(&fake, &ctx, now);
        assert!(matches!(ask.phase, Phase::Answer { truncated: true, .. }));
    }

    #[test]
    fn cancel_timeout_errors_and_unreadable_pages_leave_a_notice_and_the_question() {
        let ctx = egui::Context::default();
        let now = Instant::now();
        let (lib, profile) = enabled_library();
        let page = FakePage::new(&profile, PAGE_URL);
        let mut ask = Ask::open(&page, &lib, false);
        ask.question = "Keep this question".into();

        let fake = Fake::with([Script::Hold, Script::Answer(Err(LocalError::Timeout)), Script::Refuse(LocalError::Busy), Script::Answer(Err(LocalError::Http(500)))]);
        ask.send(&page, &lib, false, now);
        page.deliver("Text");
        ask.poll(&fake, &ctx, now);
        let held = fake.task();
        ask.cancel();
        assert!(held.cancelled.get());
        assert!(notice(&ask).starts_with("Canceled."));
        held.finish(Ok("Late answer".into()));
        ask.poll(&fake, &ctx, now);
        assert!(notice(&ask).starts_with("Canceled."), "a late answer never appears");

        for expected in ["did not respond before the time limit", "Another local conversation request", "HTTP 500"] {
            ask.send(&page, &lib, false, now);
            page.deliver("Text");
            ask.poll(&fake, &ctx, now);
            ask.poll(&fake, &ctx, now);
            assert!(notice(&ask).contains(expected), "{}", notice(&ask));
            assert_eq!(ask.question, "Keep this question");
        }
        assert_eq!(fake.count(), 3, "a refused start is no call");

        // The renderer never answers, or the page has nothing to read.
        let fake = Fake::default();
        ask.send(&page, &lib, false, now);
        ask.poll(&fake, &ctx, now + READ_TIMEOUT / 2);
        assert!(ask.busy());
        ask.poll(&fake, &ctx, now + READ_TIMEOUT);
        assert!(notice(&ask).contains("in time"));
        ask.send(&page, &lib, false, now);
        page.deliver(" \n \u{200B} ");
        ask.poll(&fake, &ctx, now);
        assert!(notice(&ask).contains("no readable text"));
        let unreadable = FakePage { readable: false, ..FakePage::new(&profile, PAGE_URL) };
        ask.send(&unreadable, &lib, false, now);
        assert!(notice(&ask).contains("could not read"));
        ask.question = "x".repeat(QUESTION_BYTES + 1);
        ask.send(&page, &lib, false, now);
        assert!(notice(&ask).contains("too long"));
        assert_eq!(fake.count(), 0);
    }

    #[test]
    fn navigation_tab_switch_or_closing_discards_a_pending_or_shown_answer() {
        let ctx = egui::Context::default();
        let now = Instant::now();
        let (lib, profile) = enabled_library();
        for change in ["navigation", "tab", "view", "profile", "closed tab", "closed find"] {
            let page = FakePage::new(&profile, PAGE_URL);
            let fake = Fake::with([Script::Hold]);
            let mut slot = Some(Ask::open(&page, &lib, false));
            let ask = slot.as_mut().unwrap();
            ask.question = "Question".into();
            ask.send(&page, &lib, false, now);
            page.deliver("Text");
            ask.poll(&fake, &ctx, now);
            reconcile(&mut slot, true, Some(page.owner()));
            assert!(slot.is_some(), "the same page keeps its request");
            let mut current = Some(page.owner());
            let mut find_open = true;
            match change {
                "navigation" => current.as_mut().unwrap().navigations += 1,
                "tab" => current.as_mut().unwrap().tab = "tab-b".into(),
                "view" => current.as_mut().unwrap().view += 1,
                "profile" => current.as_mut().unwrap().profile = "other".into(),
                "closed tab" => current = None,
                _ => find_open = false,
            }
            reconcile(&mut slot, find_open, current);
            assert!(slot.is_none(), "{change} discards the request");
            let task = fake.task();
            assert!(task.cancelled.get() && task.dropped.get(), "{change} cancels the request");
        }
        // A shown answer belongs to its page too.
        let page = FakePage::new(&profile, PAGE_URL);
        let fake = Fake::with([Script::Answer(Ok("Answer".into()))]);
        let mut slot = Some(Ask::open(&page, &lib, false));
        let ask = slot.as_mut().unwrap();
        ask.question = "Question".into();
        ask.send(&page, &lib, false, now);
        page.deliver("Text");
        ask.poll(&fake, &ctx, now);
        ask.poll(&fake, &ctx, now);
        assert!(matches!(ask.phase, Phase::Answer { .. }));
        page.owner.borrow_mut().navigations += 1;
        // A send after the page changed is refused even before reconcile runs.
        let ask = slot.as_mut().unwrap();
        ask.send(&page, &lib, false, now);
        assert!(notice(ask).contains("page changed"));
        reconcile(&mut slot, true, Some(page.owner()));
        assert!(slot.is_none());
    }

    #[test]
    fn the_loopback_server_receives_only_the_question_and_page_text() {
        let _serial = crate::chat::tests::CHAT_TEST.lock().unwrap();
        let server = crate::ai::tests::Server::sequence(&[("200 OK", r#"{"choices":[{"message":{"role":"assistant","content":"Loopback answer"},"finish_reason":"stop"}]}"#)]);
        let (lib, _, profile) = configured(Store::open_in_memory().unwrap(), server.endpoint);
        let ready = crate::ai_features::ready(&lib, &profile).unwrap();
        crate::ai_features::enable(&lib, &profile, FEATURE, &ready).unwrap();
        let page = FakePage::new(&profile, PAGE_URL);
        let ctx = egui::Context::default();
        let mut ask = Ask::open(&page, &lib, false);
        ask.question = "Which fixture?".into();
        ask.send(&page, &lib, false, Instant::now());
        page.deliver("Loopback fixture page text");
        ask.poll(&crate::ai_features::Local, &ctx, Instant::now());
        let request = server.received();
        let body: serde_json::Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        let content = body["messages"][0]["content"].as_str().unwrap();
        assert!(content.contains("Question: Which fixture?") && content.ends_with("Loopback fixture page text"));
        assert!(!request.contains("SECRET_QUERY") && !request.to_ascii_lowercase().contains("cookie"));
        server.finish();
        let deadline = Instant::now() + Duration::from_secs(3);
        while ask.busy() {
            ask.poll(&crate::ai_features::Local, &ctx, Instant::now());
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(matches!(&ask.phase, Phase::Answer { text, .. } if text == "Loopback answer"));
    }
}
