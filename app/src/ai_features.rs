//! Opt-in browser features that send page or download data to the saved
//! loopback provider. Each feature has its own device-local, per-profile
//! switch, off by default. Turning one on records consent for one destination
//! (provider, port and model); a changed selection, a newer consent version or
//! an unreadable record reads as off. Nothing typed, extracted or returned is
//! stored or logged here.

use crate::ai;
use crossbow_ai::Message;
use crossbow_ai::chat::ChatTask;
use crossbow_ai::local::{LocalEndpoint, LocalError, valid_model_id};
use crossbow_core::library::Library;
use serde::{Deserialize, Serialize};

/// Raise when a feature starts sending more, or its consent text says it
/// sends more than it did; older consent then reads as off, for every
/// feature. 2: Translate Pages says text out of view can be sent. 3: Ask on
/// Page says so too, and that an editor's text can be. Builds with 2
/// showed Ask's earlier text, so a 2 can't tell which one was agreed to.
pub const CONSENT_VERSION: u32 = 3;

/// The saved provider and model a feature may use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ready {
    pub endpoint: LocalEndpoint,
    pub model: String,
}

impl Ready {
    pub fn provider(&self) -> &'static str { self.endpoint.provider.kind().name() }

    /// Where requests go, for consent text and answer cards.
    pub fn destination(&self) -> String { format!("{} ({}) at {}", self.provider(), self.model, self.endpoint.address()) }
}

/// The saved selection, or why a feature cannot use one.
pub fn ready(lib: &Library, profile: &str) -> Result<Ready, String> {
    ready_from(&ai::saved_selection(lib, profile)?)
}

pub fn ready_from(preferences: &ai::Preferences) -> Result<Ready, String> {
    preferences.endpoint.validate().map_err(|error| error.to_string())?;
    let model = preferences.model.clone().filter(|model| valid_model_id(model))
        .ok_or_else(|| "Choose a model in AI Providers, then Save Selection.".to_string())?;
    Ok(Ready { endpoint: preferences.endpoint, model })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Consent {
    version: u32,
    endpoint: LocalEndpoint,
    model: String,
}

fn key(feature: &str, profile: &str) -> String { format!("ai.feature.v1.{feature}.{profile}") }

/// The destination this profile consented to for `feature`, if it is on.
pub fn consented(lib: &Library, profile: &str, feature: &str) -> Result<Option<Ready>, String> {
    let raw = lib.store.kv_get(&key(feature, profile)).map_err(|_| "Could not read this setting. Reopen Settings and retry.".to_string())?;
    Ok(raw.and_then(|raw| serde_json::from_str::<Consent>(&raw).ok())
        .filter(|consent| consent.version == CONSENT_VERSION && consent.endpoint.validate().is_ok() && valid_model_id(&consent.model))
        .map(|consent| Ready { endpoint: consent.endpoint, model: consent.model }))
}

/// The destination a request may use now: the feature is on, its consent is
/// current and it names the saved selection. Otherwise why not, for display.
pub fn allowed(lib: &Library, profile: &str, feature: &str, name: &str) -> Result<Ready, String> {
    if !lib.profiles.contains_key(profile) { return Err("This profile is unavailable.".into()); }
    let consent = consented(lib, profile, feature)?.ok_or_else(|| format!("{name} is off. Turn it on in Settings → AI Providers."))?;
    let current = ready(lib, profile)?;
    if current != consent {
        return Err(format!("The AI provider changed since you turned on {name}. Turn it on again in Settings → AI Providers to confirm the new destination."));
    }
    Ok(current)
}

/// Records consent for `shown`, the destination the user just confirmed.
/// Refused if the saved selection changed since it was shown.
pub fn enable(lib: &Library, profile: &str, feature: &str, shown: &Ready) -> Result<(), String> {
    if !lib.profiles.contains_key(profile) { return Err("This profile is unavailable.".into()); }
    if ready(lib, profile)? != *shown {
        return Err("The saved provider changed. Review the new destination, then turn it on again.".into());
    }
    let consent = Consent { version: CONSENT_VERSION, endpoint: shown.endpoint, model: shown.model.clone() };
    let raw = serde_json::to_string(&consent).map_err(|_| "Could not encode this setting.".to_string())?;
    lib.store.kv_set(&key(feature, profile), &raw)
        .map_err(|_| "Could not save this setting. It is still off. Check disk space and permissions, then try again.".to_string())
}

pub fn disable(lib: &Library, profile: &str, feature: &str) -> Result<(), String> {
    lib.store.kv_delete(&key(feature, profile))
        .map_err(|_| "Could not save this setting. It is still on. Check disk space and permissions, then try again.".to_string())
}

/// Crossbow has no private or ephemeral profiles: every profile keeps a
/// persistent Chromium request context. Features still take this as an
/// input, so adding such profiles cannot silently send their data.
pub fn private_profile(_profile: &str) -> bool { false }

/// Format characters that can disguise text, such as bidirectional overrides.
pub fn invisible(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}')
}

/// A model answer as one plain line: surrounding whitespace and one pair of
/// matching quotes are removed; markup, control or invisible characters, more
/// than one line, or more than `max_chars` characters are refused, not repaired.
pub fn one_line(answer: &str, max_chars: usize) -> Result<String, &'static str> {
    let mut line = answer.trim();
    for (open, close) in [('"', '"'), ('\'', '\''), ('“', '”'), ('‘', '’')] {
        if line.chars().count() >= 2 && line.starts_with(open) && line.ends_with(close) {
            line = line[open.len_utf8()..line.len() - close.len_utf8()].trim();
            break;
        }
    }
    if line.is_empty() { return Err("The answer was empty."); }
    if line.chars().any(|c| c.is_control() || invisible(c)) { return Err("The answer was not a single plain line."); }
    if line.contains(['`', '*', '<', '>', '[', ']', '|', '{', '}']) || line.starts_with(['#', '-', '+']) {
        return Err("The answer contained formatting.");
    }
    if line.chars().count() > max_chars { return Err("The answer was too long."); }
    Ok(line.to_owned())
}

/// One request on the shared single chat worker.
pub trait Pending {
    fn poll(&self) -> Option<Result<String, LocalError>>;
    fn cancel(&self);
}

impl Pending for ChatTask {
    fn poll(&self) -> Option<Result<String, LocalError>> { ChatTask::poll(self) }
    fn cancel(&self) { ChatTask::cancel(self) }
}

/// Starts one request with one user message. Tests substitute a fake.
pub trait Provider {
    fn start(&self, ready: &Ready, prompt: String, ctx: &egui::Context) -> Result<Box<dyn Pending>, LocalError>;
}

/// The saved loopback server, through the bounded chat transport.
pub struct Local;

impl Provider for Local {
    fn start(&self, ready: &Ready, prompt: String, ctx: &egui::Context) -> Result<Box<dyn Pending>, LocalError> {
        let ctx = ctx.clone();
        let task = ChatTask::spawn(ready.endpoint, ready.model.clone(), vec![Message::user(prompt)], move || ctx.request_repaint())?;
        Ok(Box::new(task))
    }
}

/// Like `Local`, but the server must still list the saved model before the
/// prompt is sent; otherwise nothing is sent. Answers stop at `max_tokens`.
pub struct Checked {
    pub max_tokens: u16,
}

impl Provider for Checked {
    fn start(&self, ready: &Ready, prompt: String, ctx: &egui::Context) -> Result<Box<dyn Pending>, LocalError> {
        let ctx = ctx.clone();
        let task = ChatTask::spawn_checked(ready.endpoint, ready.model.clone(), prompt, self.max_tokens, move || ctx.request_repaint())?;
        Ok(Box::new(task))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crossbow_core::store::Store;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    /// What the fake provider does for the next request.
    pub(crate) enum Script {
        Answer(Result<String, LocalError>),
        /// Pending until the test calls `finish` on `Fake::last`.
        Hold,
        Refuse(LocalError),
    }

    #[derive(Default)]
    pub(crate) struct FakeTask {
        result: RefCell<Option<Result<String, LocalError>>>,
        pub(crate) cancelled: Cell<bool>,
        pub(crate) dropped: Cell<bool>,
    }

    impl FakeTask {
        pub(crate) fn finish(&self, result: Result<String, LocalError>) { *self.result.borrow_mut() = Some(result); }
    }

    struct Handle(Rc<FakeTask>);

    impl Pending for Handle {
        fn poll(&self) -> Option<Result<String, LocalError>> {
            if self.0.cancelled.get() { return Some(Err(LocalError::Cancelled)); }
            self.0.result.borrow_mut().take()
        }
        fn cancel(&self) { self.0.cancelled.set(true); }
    }

    impl Drop for Handle {
        // Like ChatTask, dropping a request cancels it.
        fn drop(&mut self) { self.0.cancelled.set(true); self.0.dropped.set(true); }
    }

    /// Counts calls and records exactly what each request would send.
    #[derive(Default)]
    pub(crate) struct Fake {
        pub(crate) calls: RefCell<Vec<(Ready, String)>>,
        script: RefCell<VecDeque<Script>>,
        pub(crate) last: RefCell<Option<Rc<FakeTask>>>,
    }

    impl Fake {
        pub(crate) fn with(script: impl IntoIterator<Item = Script>) -> Self {
            Self { script: RefCell::new(script.into_iter().collect()), ..Default::default() }
        }
        pub(crate) fn then(&self, script: Script) { self.script.borrow_mut().push_back(script); }
        pub(crate) fn count(&self) -> usize { self.calls.borrow().len() }
        pub(crate) fn prompt(&self, index: usize) -> String { self.calls.borrow()[index].1.clone() }
        pub(crate) fn task(&self) -> Rc<FakeTask> { self.last.borrow().clone().expect("a request was held") }
    }

    impl Provider for Fake {
        fn start(&self, ready: &Ready, prompt: String, _ctx: &egui::Context) -> Result<Box<dyn Pending>, LocalError> {
            let script = self.script.borrow_mut().pop_front().expect("unexpected provider call");
            if let Script::Refuse(error) = script { return Err(error); }
            self.calls.borrow_mut().push((ready.clone(), prompt));
            let task = Rc::new(FakeTask::default());
            if let Script::Answer(result) = script { task.finish(result); }
            *self.last.borrow_mut() = Some(task.clone());
            Ok(Box::new(Handle(task)))
        }
    }

    /// A library whose first profile saved `endpoint` and `fixture-model`.
    pub(crate) fn configured(store: Store, endpoint: LocalEndpoint) -> (Library, String, String) {
        let (lib, space, chat) = crate::chat::tests::configured_store(store, endpoint);
        let profile = chat.profile.clone();
        (lib, space, profile)
    }

    pub(crate) fn fixture_ready(port: u16) -> Ready {
        Ready { endpoint: LocalEndpoint { port, ..Default::default() }, model: "fixture-model".into() }
    }

    #[test]
    fn features_are_off_until_consent_names_the_saved_destination() {
        let root = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
        let path = root.path().join("features.sqlite");
        let endpoint = LocalEndpoint { port: 41_001, ..Default::default() };
        let (mut lib, _, profile) = configured(Store::open(&path).unwrap(), endpoint);
        let ready = ready(&lib, &profile).unwrap();
        assert_eq!(ready, fixture_ready(41_001));
        assert_eq!(ready.destination(), "Ollama (fixture-model) at http://127.0.0.1:41001");
        for feature in ["ask_on_page", "tidy_downloads"] {
            assert_eq!(consented(&lib, &profile, feature), Ok(None), "off by default");
            assert_eq!(allowed(&lib, &profile, feature, "Ask on Page").unwrap_err(), "Ask on Page is off. Turn it on in Settings → AI Providers.");
        }
        enable(&lib, &profile, "ask_on_page", &ready).unwrap();
        assert_eq!(allowed(&lib, &profile, "ask_on_page", "Ask on Page"), Ok(ready.clone()));
        assert!(allowed(&lib, &profile, "tidy_downloads", "Tidy Downloads").is_err(), "each feature has its own consent");
        assert!(lib.profiles.keys().any(|id| id == &profile));
        let other = lib.create_profile("Other").unwrap();
        assert!(allowed(&lib, &other, "ask_on_page", "Ask on Page").is_err(), "consent is per profile");
        assert!(enable(&lib, "missing-profile", "ask_on_page", &ready).is_err());

        // Survives reopening the library; disabling is saved too.
        drop(lib);
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        assert_eq!(allowed(&lib, &profile, "ask_on_page", "Ask on Page"), Ok(ready.clone()));
        disable(&lib, &profile, "ask_on_page").unwrap();
        drop(lib);
        let lib = Library::open(Store::open(&path).unwrap()).unwrap();
        assert_eq!(consented(&lib, &profile, "ask_on_page"), Ok(None));
    }

    #[test]
    fn changed_destination_older_consent_and_damaged_records_read_as_off() {
        let (mut lib, _, profile) = configured(Store::open_in_memory().unwrap(), LocalEndpoint { port: 41_002, ..Default::default() });
        let ready = ready(&lib, &profile).unwrap();
        enable(&lib, &profile, "ask_on_page", &ready).unwrap();
        // The user saves another model: consent named the old destination.
        let mut settings = crate::ai::SettingsDraft::load(&lib, &profile);
        settings.set_model(Some("other-model".into()));
        settings.save(&mut lib, Some(&profile));
        assert!(settings.error.is_none());
        let error = allowed(&lib, &profile, "ask_on_page", "Ask on Page").unwrap_err();
        assert!(error.contains("provider changed") && error.contains("Turn it on again"), "{error}");
        // Showing one destination and confirming after it changed is refused.
        assert!(enable(&lib, &profile, "ask_on_page", &ready).unwrap_err().contains("changed"));
        let current = super::ready(&lib, &profile).unwrap();
        enable(&lib, &profile, "ask_on_page", &current).unwrap();
        assert_eq!(allowed(&lib, &profile, "ask_on_page", "Ask on Page"), Ok(current.clone()));

        let key = format!("ai.feature.v1.ask_on_page.{profile}");
        let older = serde_json::json!({ "version": 0, "endpoint": current.endpoint, "model": current.model }).to_string();
        lib.store.kv_set(&key, &older).unwrap();
        assert_eq!(consented(&lib, &profile, "ask_on_page"), Ok(None), "older consent is off");
        let extra = format!(r#"{{"version":{CONSENT_VERSION},"endpoint":{{"provider":"Ollama","port":41002}},"model":"m","extra":1}}"#);
        for damaged in ["not json", "{}", extra.as_str()] {
            lib.store.kv_set(&key, damaged).unwrap();
            assert_eq!(consented(&lib, &profile, "ask_on_page"), Ok(None), "{damaged}");
        }

        // No provider: nothing is allowed even with a consent record.
        lib.store.kv_set(&key, &serde_json::json!({ "version": CONSENT_VERSION, "endpoint": current.endpoint, "model": current.model }).to_string()).unwrap();
        lib.store.kv_delete(&format!("ai.providers.v1.{profile}")).unwrap();
        assert!(allowed(&lib, &profile, "ask_on_page", "Ask on Page").unwrap_err().contains("Save Selection"));
        assert!(enable(&lib, &profile, "ask_on_page", &current).is_err(), "cannot turn on without a provider");
    }

    #[test]
    fn failed_saves_keep_the_previous_state_and_say_so() {
        let root = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
        let path = root.path().join("features.sqlite");
        let (lib, _, profile) = configured(Store::open(&path).unwrap(), LocalEndpoint { port: 41_003, ..Default::default() });
        let ready = ready(&lib, &profile).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TRIGGER reject_on BEFORE INSERT ON kv WHEN NEW.k LIKE 'ai.feature.%' BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
        let error = enable(&lib, &profile, "tidy_downloads", &ready).unwrap_err();
        assert!(error.starts_with("Could not save") && error.contains("still off"), "{error}");
        assert_eq!(consented(&lib, &profile, "tidy_downloads"), Ok(None));
        conn.execute_batch("DROP TRIGGER reject_on; CREATE TRIGGER reject_off BEFORE DELETE ON kv WHEN OLD.k LIKE 'ai.feature.%' BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
        enable(&lib, &profile, "tidy_downloads", &ready).unwrap();
        let error = disable(&lib, &profile, "tidy_downloads").unwrap_err();
        assert!(error.starts_with("Could not save") && error.contains("still on"), "{error}");
        assert_eq!(consented(&lib, &profile, "tidy_downloads"), Ok(Some(ready)));
    }

    #[test]
    fn one_line_answers_are_plain_bounded_and_never_repaired() {
        assert_eq!(one_line("  Quarterly report  \n", 40), Ok("Quarterly report".into()));
        assert_eq!(one_line("\"Quarterly report\"", 40), Ok("Quarterly report".into()));
        assert_eq!(one_line("“Quarterly report”", 40), Ok("Quarterly report".into()));
        assert_eq!(one_line("Café menu", 9), Ok("Café menu".into()), "characters, not bytes");
        for bad in ["", "   ", "\"\"", "two\nlines", "a\rb", "**bold**", "`code`", "# Title", "- item", "[link](x)", "<b>x</b>", "a|b", "tab\there", "nul\0", "bidi\u{202E}fdp", "zero\u{200B}width"] {
            assert!(one_line(bad, 40).is_err(), "{bad:?} must be refused");
        }
        assert!(one_line("Café menus", 9).is_err(), "too long");
    }

    #[test]
    fn the_local_provider_sends_one_user_message_to_the_saved_loopback_server() {
        let _serial = crate::chat::tests::CHAT_TEST.lock().unwrap();
        let server = crate::ai::tests::Server::sequence(&[("200 OK", r#"{"choices":[{"message":{"role":"assistant","content":"Fixture answer"},"finish_reason":"stop"}]}"#)]);
        let ready = Ready { endpoint: server.endpoint, model: "fixture-model".into() };
        let task = Local.start(&ready, "Only this prompt".into(), &egui::Context::default()).unwrap();
        let request = server.received();
        assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"), "{request}");
        assert!(request.to_ascii_lowercase().contains(&format!("host: 127.0.0.1:{}", server.endpoint.port)));
        let body: serde_json::Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["model"], "fixture-model");
        assert_eq!(body["messages"], serde_json::json!([{ "role": "user", "content": "Only this prompt" }]));
        server.finish();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let answer = loop {
            if let Some(answer) = task.poll() { break answer; }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        assert_eq!(answer, Ok("Fixture answer".into()));
    }

    fn answer(task: &dyn Pending) -> Result<String, LocalError> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if let Some(answer) = task.poll() { return answer; }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn the_checked_provider_sends_the_bounded_prompt_only_while_the_server_lists_the_saved_model() {
        let _serial = crate::chat::tests::CHAT_TEST.lock().unwrap();
        let listed = r#"{"data":[{"id":"other-model"},{"id":"fixture-model"}]}"#;
        let server = crate::ai::tests::Server::sequence(&[
            ("200 OK", listed),
            ("200 OK", r#"{"choices":[{"message":{"role":"assistant","content":"Short title"},"finish_reason":"stop"}]}"#),
        ]);
        let ready = Ready { endpoint: server.endpoint, model: "fixture-model".into() };
        let task = Checked { max_tokens: 48 }.start(&ready, "Only this prompt".into(), &egui::Context::default()).unwrap();
        let check = server.received();
        assert!(check.starts_with("GET /v1/models HTTP/1.1\r\n") && check.ends_with("\r\n\r\n"), "the check carries no prompt: {check}");
        server.release.send(()).unwrap();
        let request = server.received();
        let body: serde_json::Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body, serde_json::json!({ "model": "fixture-model", "messages": [{ "role": "user", "content": "Only this prompt" }], "stream": false, "max_tokens": 48 }));
        server.finish();
        assert_eq!(answer(task.as_ref()), Ok("Short title".into()));

        // The saved model is gone: only the model list is read.
        let server = crate::ai::tests::Server::sequence(&[("200 OK", r#"{"data":[{"id":"other-model"}]}"#)]);
        let ready = Ready { endpoint: server.endpoint, model: "fixture-model".into() };
        let task = Checked { max_tokens: 48 }.start(&ready, "Never sent".into(), &egui::Context::default()).unwrap();
        assert!(server.received().starts_with("GET /v1/models "));
        server.finish();
        assert_eq!(answer(task.as_ref()), Err(LocalError::InvalidModel));
    }

    /// Translate Pages' consent text says that text out of view can be sent
    /// (version 2), and Ask on Page's says so too, with an editor's text
    /// (version 3). A grant made under an earlier text reads as off for
    /// every feature, which shares the version, so each is asked again;
    /// turning one on records the current version.
    #[test]
    fn a_grant_made_under_the_previous_consent_text_is_asked_again() {
        assert_eq!(CONSENT_VERSION, 3);
        let (lib, _, profile) = configured(Store::open_in_memory().unwrap(), LocalEndpoint { port: 41_004, ..Default::default() });
        let current = ready(&lib, &profile).unwrap();
        for old in [1, 2] {
            for (feature, name) in [(crate::translate::FEATURE, crate::translate::NAME), (crate::link_preview::FEATURE, crate::link_preview::NAME), (crate::ask_page::FEATURE, crate::ask_page::NAME)] {
                let key = format!("ai.feature.v1.{feature}.{profile}");
                lib.store.kv_set(&key, &serde_json::json!({ "version": old, "endpoint": current.endpoint, "model": current.model }).to_string()).unwrap();
                assert_eq!(consented(&lib, &profile, feature), Ok(None), "{feature}: a version {old} grant is off");
                assert!(allowed(&lib, &profile, feature, name).unwrap_err().contains("is off. Turn it on in Settings"), "{feature} {old}");
                enable(&lib, &profile, feature, &current).unwrap();
                let saved: serde_json::Value = serde_json::from_str(&lib.store.kv_get(&key).unwrap().unwrap()).unwrap();
                assert_eq!(saved["version"], 3, "{feature} {old}");
                assert_eq!(allowed(&lib, &profile, feature, name), Ok(current.clone()), "{feature} {old}");
            }
        }
    }
}

