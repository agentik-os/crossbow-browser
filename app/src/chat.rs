//! Explicit, ephemeral conversations. Only the visible messages are sent;
//! provider text cannot request tools, read pages or change browser state.

use crate::ai::{Preferences, saved_selection};
use crossbow_ai::chat::{ChatTask, validate_messages};
use crossbow_ai::local::LocalError;
use crossbow_ai::{Message, Role};
use crossbow_core::library::Library;

/// Account IDs are local ownership guards, never part of an inference request.
pub type AccountScope = Option<(String, String)>;

/// Opening the command bar captures an owner, not consent or browsing context.
#[derive(Clone, Debug)]
pub struct Entry {
    space: String,
    profile: String,
    account: AccountScope,
}

impl Entry {
    pub fn capture(lib: &Library, space: &str, account: &AccountScope) -> Option<Self> {
        Some(Self { space: space.into(), profile: target_profile(lib, space)?.into(), account: account.clone() })
    }

    pub fn open(&self, lib: &mut Library, space: &str, account: &AccountScope) -> Result<Session, &'static str> {
        if space != self.space || target_profile(lib, space) != Some(self.profile.as_str()) || account != &self.account {
            return Err("The space, profile or account changed. Open Ask AI again in the intended profile.");
        }
        Ok(Session::load(lib, &self.profile, &self.account))
    }
}

struct Pending {
    prompt: String,
    task: ChatTask,
}

// egui retains undo snapshots separately from the draft. Own and erase them with
// this chat, including when Settings is closed without rendering another frame.
struct Editor {
    id: egui::Id,
    scroll_id: Option<egui::Id>,
    ctx: Option<egui::Context>,
}

impl Editor {
    fn clear(&self) {
        if let Some(ctx) = &self.ctx {
            ctx.data_mut(|data| {
                data.remove::<egui::text_edit::TextEditState>(self.id);
                if let Some(id) = self.scroll_id { data.remove::<egui::scroll_area::State>(id); }
            });
            ctx.memory_mut(|memory| memory.surrender_focus(self.id));
        }
    }
}

impl Drop for Editor {
    fn drop(&mut self) { self.clear(); }
}

pub struct Session {
    pub profile: String,
    account: AccountScope,
    pub selection: Option<Preferences>,
    pub draft: String,
    pub messages: Vec<Message>,
    /// A per-conversation opt-in, never a saved provider preference.
    pub stream: bool,
    /// Incomplete text is display-only, never part of messages sent next time.
    pub partial: String,
    pub error: Option<String>,
    pub reveal_status: bool,
    pending: Option<Pending>,
    editor: Editor,
}

impl Session {
    pub fn load(lib: &mut Library, profile: &str, account: &AccountScope) -> Self {
        let selection = lib.reload_profile(profile)
            .map_err(|_| "This profile is unavailable. Select an available profile before using AI.".to_string())
            .and_then(|_| saved_selection(lib, profile));
        Self {
            profile: profile.into(), account: account.clone(), selection: selection.as_ref().ok().cloned(),
            draft: String::new(), messages: Vec::new(), stream: false, partial: String::new(),
            error: selection.err(), reveal_status: false, pending: None,
            editor: Editor { id: egui::Id::unique(("chat-message", crossbow_core::new_id())), scroll_id: None, ctx: None },
        }
    }

    pub fn editor_id(&mut self, ctx: &egui::Context) -> egui::Id {
        self.editor.ctx = Some(ctx.clone());
        self.editor.id
    }

    pub fn track_composer(&mut self, id: egui::Id) {
        if let Some(old) = self.editor.scroll_id.replace(id).filter(|old| *old != id) {
            if let Some(ctx) = &self.editor.ctx { ctx.data_mut(|data| data.remove::<egui::scroll_area::State>(old)); }
        }
    }

    pub fn busy(&self) -> bool { self.pending.is_some() }

    /// Display the in-flight prompt without adding an unfinished turn to history.
    pub fn streaming_prompt(&self) -> Option<&str> {
        self.pending.as_ref().filter(|_| self.stream).map(|pending| pending.prompt.as_str())
    }

    fn fail(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
        self.reveal_status = true;
    }

    fn invalidate(&mut self, error: &str) {
        self.pending = None;
        self.selection = None;
        self.messages.clear();
        self.partial.clear();
        self.stream = false;
        self.draft.clear();
        self.editor.clear();
        self.fail(error);
    }

    // Painting only checks in-memory ownership/policy, never a write transaction
    // or whole-library snapshot. Invalidation is a one-time transition.
    fn check_context(&mut self, lib: &Library, space: &str, account: &AccountScope) -> bool {
        if self.selection.is_none() { return false; }
        if target_profile(lib, space) != Some(self.profile.as_str()) || account != &self.account
            || lib.effective_profile_preferences(&self.profile).is_none() {
            self.invalidate("The profile or account changed or became unavailable. Start a new chat in the intended profile.");
            return false;
        }
        true
    }

    // Re-read saved policy/destination before dispatch and result acceptance. A
    // changed destination never silently inherits the old conversation's consent.
    fn check_saved_context(&mut self, lib: &Library, space: &str, account: &AccountScope) -> bool {
        if !self.check_context(lib, space, account) { return false; }
        if lib.read_profile(&self.profile).is_err() {
            self.invalidate("The profile or account changed or became unavailable. Start a new chat in the intended profile.");
            return false;
        }
        if saved_selection(lib, &self.profile).as_ref().ok() != self.selection.as_ref() {
            self.invalidate("The saved provider changed or could not be read. This chat was cleared. Check AI Providers, then start a new chat.");
            return false;
        }
        true
    }

    pub fn send(&mut self, lib: &Library, space: &str, account: &AccountScope, ctx: &egui::Context) {
        if self.busy() || !self.check_saved_context(lib, space, account) { return; }
        let mut messages = self.messages.clone();
        messages.push(Message::user(self.draft.clone()));
        if let Err(error) = validate_messages(&messages) { self.fail(error.to_string()); return; }
        let selection = self.selection.as_ref().unwrap();
        let model = selection.model.clone().unwrap();
        let ctx = ctx.clone();
        let wake = move || ctx.request_repaint();
        let task = if self.stream {
            ChatTask::spawn_streaming(selection.endpoint, model, messages, wake)
        } else {
            ChatTask::spawn(selection.endpoint, model, messages, wake)
        };
        match task {
            Ok(task) => {
                self.pending = Some(Pending { prompt: self.draft.clone(), task });
                self.partial.clear();
                self.error = None;
                self.reveal_status = true;
            }
            Err(error) => self.fail(error.to_string()),
        }
    }

    pub fn cancel(&mut self) {
        if self.pending.take().is_some() { self.fail(LocalError::Cancelled.to_string()); }
    }

    pub fn poll(&mut self, lib: &Library, space: &str, account: &AccountScope) {
        if !self.check_context(lib, space, account) { return; }
        let Some(pending) = self.pending.as_ref() else { return };
        let partial = pending.task.take_partial();
        let result = pending.task.poll();
        if partial.is_none() && result.is_none() { return; }
        // Progress crosses the same ownership boundary as the final answer.
        // Idle frames still perform no storage read.
        if !self.check_saved_context(lib, space, account) { return; }
        if let Some(partial) = partial { self.partial = partial; }
        let Some(result) = result else { return };
        let pending = self.pending.take().unwrap();
        match result {
            Ok(answer) => {
                self.partial.clear();
                self.messages.push(Message::user(pending.prompt));
                self.messages.push(Message { role: Role::Assistant, content: answer });
                self.draft.clear();
                self.error = None;
                // A streaming reader may already be deep inside the answer.
                // Completion must not pull them back to the composer/actions.
                self.reveal_status = !self.stream;
            }
            Err(error) => self.fail(error.to_string()),
        }
    }
}

// Recovery follows the Space's exact owner even when its display cache was
// evicted. This identifies a target only; sending still requires valid policy.
pub fn target_profile<'a>(lib: &'a Library, space: &str) -> Option<&'a str> {
    let id = lib.spaces.get(space)?.profile_id.as_str();
    (!id.is_empty()).then_some(id)
}

/// Closing/leaving this page or changing profile/account drops all chat content
/// and cancels the request. Dropping does not undo anything a server received.
pub fn reconcile(slot: &mut Option<Session>, lib: &Library, space: &str, account: &AccountScope, visible: bool) {
    let profile = target_profile(lib, space);
    if !visible || slot.as_ref().is_some_and(|session| Some(session.profile.as_str()) != profile || &session.account != account) {
        *slot = None;
    }
}

#[cfg(test)]
#[path = "chat_stream_tests.rs"]
pub(crate) mod streaming_tests;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ai::SettingsDraft;
    use crossbow_ai::local::LocalEndpoint;
    use crossbow_core::store::Store;
    use std::time::{Duration, Instant};

    pub(crate) fn configured(endpoint: LocalEndpoint) -> (Library, String, Session) {
        configured_store(Store::open_in_memory().unwrap(), endpoint)
    }

    pub(crate) fn configured_store(store: Store, endpoint: LocalEndpoint) -> (Library, String, Session) {
        let mut lib = Library::open(store).unwrap();
        lib.ensure_defaults().unwrap();
        let space = lib.spaces_ordered()[0].0.clone();
        let profile = lib.spaces[&space].profile_id.clone();
        let mut settings = SettingsDraft::load(&lib, &profile);
        settings.set_endpoint(endpoint);
        settings.set_model(Some("fixture-model".into()));
        settings.save(&mut lib, Some(&profile));
        assert!(settings.error.is_none());
        let chat = Session::load(&mut lib, &profile, &None);
        (lib, space, chat)
    }

    fn settle(chat: &mut Session, lib: &mut Library, space: &str) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while chat.busy() {
            chat.poll(lib, space, &None);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    // Only this test (and the UI sequence guarded by the same lock) starts chat
    // workers in the app test process, preserving the production single-worker cap.
    pub(crate) static CHAT_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn command_chat_entry_rejects_owner_drift_before_loading() {
        for fault in ["space", "profile", "deleted-space", "empty-owner", "sign-in", "sign-out", "account", "server"] {
            let (mut lib, space, chat) = configured(LocalEndpoint::default());
            let original = chat.profile.clone();
            let account = (fault != "sign-in").then(|| ("https://sync.example.test".into(), "account-a".into()));
            let entry = Entry::capture(&lib, &space, &account).unwrap();
            let mut current_account = account.clone();
            let mut current_space = space.clone();
            match fault {
                "space" => current_space = lib.create_space("Other space", "S", crossbow_core::model::Theme::preset(0), &original).unwrap(),
                "profile" => {
                    let other = lib.create_profile("Other profile").unwrap();
                    lib.update_space(&space, |space| space.profile_id = other.clone()).unwrap();
                }
                "deleted-space" => { lib.spaces.remove(&space); }
                "empty-owner" => lib.spaces.get_mut(&space).unwrap().profile_id.clear(),
                "sign-in" => current_account = Some(("https://sync.example.test".into(), "account-a".into())),
                "sign-out" => current_account = None,
                "account" => current_account.as_mut().unwrap().1 = "account-b".into(),
                "server" => current_account.as_mut().unwrap().0 = "https://other.example.test".into(),
                _ => unreachable!(),
            }
            let revision = lib.revision;
            assert!(entry.open(&mut lib, &current_space, &current_account).is_err(), "retargeted entry after {fault}");
            assert_eq!(lib.revision, revision, "rejected entry must not reload another owner");
        }
    }

    #[test]
    fn command_chat_entry_recovers_exact_owner_without_granting_send() {
        let (mut lib, space, chat) = configured(LocalEndpoint::default());
        let profile = chat.profile.clone();
        let entry = Entry::capture(&lib, &space, &None).unwrap();
        // An unavailable display cache must not hide explicit recovery.
        lib.profiles.remove(&profile);
        assert!(Entry::capture(&lib, &space, &None).is_some());
        let recovered = entry.open(&mut lib, &space, &None).unwrap();
        assert_eq!(recovered.profile, profile);
        assert!(recovered.selection.is_some());
        let row = lib.store.all_entities().unwrap().into_iter().find(|row| row.kind == "profile" && row.id == profile).unwrap();
        lib.store.put_entity(&crossbow_core::store::EntityRow { deleted: true, ..row }, false).unwrap();
        let mut unavailable = entry.open(&mut lib, &space, &None).unwrap();
        assert_eq!(unavailable.profile, profile);
        assert!(unavailable.selection.is_none() && unavailable.error.is_some());
        unavailable.draft = "Not authorized to send".into();
        unavailable.send(&lib, &space, &None, &egui::Context::default());
        assert!(!unavailable.busy());
        assert!(unavailable.messages.is_empty());
        lib.spaces.get_mut(&space).unwrap().profile_id.clear();
        assert!(Entry::capture(&lib, &space, &None).is_none());
        assert!(Entry::capture(&lib, "missing-space", &None).is_none());
    }

    #[test]
    fn explicit_conversation_retries_without_duplicate_turns_and_rejects_changed_destination() {
        let _serial = CHAT_TEST.lock().unwrap();
        let server = crate::ai::tests::Server::sequence(&[
            ("503 Unavailable", "private fixture failure"),
            ("200 OK", r#"{"choices":[{"message":{"role":"assistant","content":"First answer"},"finish_reason":"stop"}]}"#),
            ("200 OK", r#"{"choices":[{"message":{"content":"Late answer"},"finish_reason":"stop"}]}"#),
        ]);
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("chat.sqlite");
        let (mut lib, space, mut chat) = configured_store(Store::open(&path).unwrap(), server.endpoint);
        let ctx = egui::Context::default();
        chat.draft = "Only the question I typed".into();
        chat.send(&mut lib, &space, &None, &ctx);
        assert!(chat.busy());
        let payload = |request: String| -> serde_json::Value { serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap() };
        let first = payload(server.received());
        assert_eq!(first["messages"], serde_json::json!([{"role":"user","content":"Only the question I typed"}]));
        chat.send(&mut lib, &space, &None, &ctx);
        server.release.send(()).unwrap();
        settle(&mut chat, &mut lib, &space);
        assert_eq!(chat.draft, "Only the question I typed");
        assert!(chat.messages.is_empty());
        assert!(chat.error.as_ref().unwrap().contains("503"));
        assert!(!chat.error.as_ref().unwrap().contains("private fixture"));
        chat.send(&mut lib, &space, &None, &ctx);
        assert_eq!(payload(server.received()), first);
        server.release.send(()).unwrap();
        settle(&mut chat, &mut lib, &space);
        assert_eq!(chat.messages.len(), 2);
        assert_eq!(chat.messages[1].content, "First answer");
        assert!(chat.draft.is_empty());
        chat.draft = "Follow-up".into();
        chat.send(&mut lib, &space, &None, &ctx);
        let followup = payload(server.received());
        assert_eq!(followup["messages"].as_array().unwrap().len(), 3);
        assert_eq!(followup["messages"][1]["content"], "First answer");
        let profile = chat.profile.clone();
        let mut settings = SettingsDraft::load(&lib, &profile);
        settings.set_model(Some("different-model".into()));
        settings.save(&mut lib, Some(&profile));
        chat.poll(&mut lib, &space, &None);
        assert!(chat.busy()); // idle polls do not reread storage
        server.release.send(()).unwrap();
        settle(&mut chat, &mut lib, &space);
        assert!(chat.selection.is_none() && chat.messages.is_empty() && chat.draft.is_empty());
        server.join();
        chat.poll(&mut lib, &space, &None);
        assert!(chat.messages.is_empty());
        drop(chat);
        drop(lib);
        let conn = rusqlite::Connection::open(&path).unwrap();
        let rows: i64 = conn.query_row("SELECT count(*) FROM kv WHERE v LIKE '%Only the question%' OR v LIKE '%First answer%' OR v LIKE '%Follow-up%'", [], |row| row.get(0)).unwrap();
        assert_eq!(rows, 0);
        let mut reopened = Library::open(Store::open(&path).unwrap()).unwrap();
        let chat = Session::load(&mut reopened, &profile, &None);
        assert!(chat.draft.is_empty() && chat.messages.is_empty());
        assert_eq!(chat.selection.unwrap().model.as_deref(), Some("different-model"));
    }

    #[test]
    fn missing_invalid_selection_and_profile_never_use_defaults() {
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let profile = lib.default_profile_id();
        let chat = Session::load(&mut lib, &profile, &None);
        assert!(chat.selection.is_none());
        assert!(chat.error.unwrap().contains("Save Selection"));
        lib.store.kv_set(&format!("ai.providers.v1.{profile}"), "not json").unwrap();
        assert!(Session::load(&mut lib, &profile, &None).selection.is_none());
        assert!(Session::load(&mut lib, "missing", &None).selection.is_none());
    }

    #[test]
    fn leaving_profile_or_account_discards_chat_without_persistence() {
        for boundary in 0..3 {
            let (lib, space, mut chat) = configured(LocalEndpoint::default());
            chat.draft = "unsent synthetic text".into();
            chat.messages = vec![Message::user("synthetic conversation")];
            let mut slot = Some(chat);
            reconcile(&mut slot, &lib, if boundary == 1 { "missing-space" } else { &space },
                &if boundary == 2 { Some(("fixture-api".into(), "other-account".into())) } else { None }, boundary != 0);
            assert!(slot.is_none());
            let mut lib = lib;
            let profile = lib.default_profile_id();
            let reopened = Session::load(&mut lib, &profile, &None);
            assert!(reopened.messages.is_empty() && reopened.draft.is_empty());
        }
    }

    #[test]
    fn recovery_target_is_exact_but_does_not_authorize_an_unavailable_profile() {
        for boundary in 0..4 {
            let (mut lib, space, mut chat) = configured(LocalEndpoint::default());
            let original = chat.profile.clone();
            let other = lib.create_profile("Other synthetic profile").unwrap();
            lib.profiles.remove(&original);
            assert_eq!(target_profile(&lib, &space), Some(original.as_str()));
            chat.draft = "Never retarget this draft".into();
            let mut slot = Some(chat);
            reconcile(&mut slot, &lib, &space, &None, true);
            let chat = slot.as_mut().expect("unavailable owner must retain recovery controls");
            chat.send(&lib, &space, &None, &egui::Context::default());
            assert!(chat.selection.is_none() && chat.draft.is_empty() && !chat.busy());
            assert_eq!(chat.profile, original);
            let mut account = None;
            match boundary {
                0 => lib.spaces.get_mut(&space).unwrap().profile_id = other,
                1 => lib.spaces.get_mut(&space).unwrap().profile_id.clear(),
                2 => { lib.spaces.remove(&space); }
                _ => account = Some(("fixture-api".into(), "other-account".into())),
            }
            reconcile(&mut slot, &lib, &space, &account, true);
            assert!(slot.is_none(), "owner change must still clear recovery session {boundary}");
        }
    }

    #[test]
    fn owner_changes_before_dispatch_clear_content_instead_of_retargeting() {
        for boundary in 0..3 {
            let (mut lib, space, mut chat) = configured(LocalEndpoint::default());
            chat.draft = "private synthetic draft".into();
            chat.messages = vec![Message::user("old question"), Message { role: Role::Assistant, content: "old answer".into() }];
            if boundary == 2 { lib.store.kv_set(&format!("ai.providers.v1.{}", chat.profile), "unreadable").unwrap(); }
            chat.send(&mut lib, if boundary == 0 { "missing-space" } else { &space },
                &if boundary == 1 { Some(("new-api".into(), "new-account".into())) } else { None }, &egui::Context::default());
            assert!(!chat.busy());
            assert!(chat.selection.is_none() && chat.messages.is_empty() && chat.draft.is_empty());
            assert!(chat.error.is_some());
        }
    }

    #[test]
    fn review_invalid_context_reveals_once_then_allows_scroll_away() {
        let (mut lib, space, mut chat) = configured(LocalEndpoint::default());
        let row = lib.store.all_entities().unwrap().into_iter().find(|row| row.kind == "profile" && row.id == chat.profile).unwrap();
        let mut payload: serde_json::Value = serde_json::from_str(&row.payload).unwrap();
        payload["preferences"] = serde_json::json!({"auto_archive_hours":2});
        lib.store.put_entity(&crossbow_core::store::EntityRow { payload: payload.to_string(), ..row }, false).unwrap();
        chat.draft = "synthetic unsent prompt".into();
        chat.send(&mut lib, &space, &None, &egui::Context::default());
        assert!(chat.selection.is_none() && chat.reveal_status);
        chat.reveal_status = false;
        chat.poll(&mut lib, &space, &None);
        assert!(!chat.reveal_status, "an invalidated chat must not re-arm error scrolling every frame");
    }

    #[test]
    fn review_idle_poll_never_waits_for_a_database_writer() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("chat.sqlite");
        let (mut lib, space, mut chat) = configured_store(Store::open(&path).unwrap(), LocalEndpoint::default());
        chat.draft = "keep this unsent prompt".into();
        let (ready, waiting) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let conn = rusqlite::Connection::open(path).unwrap();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            ready.send(()).unwrap();
            let _ = released.recv_timeout(Duration::from_millis(800));
            conn.execute_batch("ROLLBACK").unwrap();
        });
        waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        let started = Instant::now();
        chat.poll(&mut lib, &space, &None);
        let elapsed = started.elapsed();
        let _ = release.send(());
        writer.join().unwrap();
        assert!(elapsed < Duration::from_millis(300), "idle paint waited for database writer: {elapsed:?}");
        assert_eq!(chat.draft, "keep this unsent prompt");
    }

    #[test]
    fn editor_state_is_erased_on_invalidation_and_account_drop_without_another_frame() {
        for invalid in [false, true] {
            let (lib, space, mut chat) = configured(LocalEndpoint::default());
            let ctx = egui::Context::default();
            let editor = chat.editor_id(&ctx);
            chat.draft = "private synthetic draft".into();
            let mut scroll = None;
            ctx.run_ui(egui::RawInput::default(), |ui| {
                let composer = egui::ScrollArea::vertical().id_salt(("chat-input-scroll", editor)).show(ui, |ui| {
                    ui.add(egui::TextEdit::multiline(&mut chat.draft).id(editor));
                });
                chat.track_composer(composer.id);
                scroll = Some(composer.id);
            }).textures_delta.clear();
            let scroll = scroll.unwrap();
            assert!(egui::TextEdit::load_state(&ctx, editor).is_some());
            assert!(egui::scroll_area::State::load(&ctx, scroll).is_some());
            let mut slot = Some(chat);
            if invalid {
                let chat = slot.as_mut().unwrap();
                lib.store.kv_set(&format!("ai.providers.v1.{}", chat.profile), "unreadable").unwrap();
                chat.send(&lib, &space, &None, &ctx);
                assert!(chat.selection.is_none() && chat.draft.is_empty());
            } else {
                reconcile(&mut slot, &lib, &space, &Some(("fixture-api".into(), "other-account".into())), true);
                assert!(slot.is_none());
            }
            assert!(egui::TextEdit::load_state(&ctx, editor).is_none());
            assert!(egui::scroll_area::State::load(&ctx, scroll).is_none());
        }
    }

    #[test]
    fn saved_profile_changes_reject_dispatch_and_late_answers_without_retargeting() {
        let _serial = CHAT_TEST.lock().unwrap();
        for after_send in [false, true] {
            let server = crate::ai::tests::Server::sequence(if after_send {
                &[("200 OK", r#"{"choices":[{"message":{"content":"Late synthetic answer"},"finish_reason":"stop"}]}"#)]
            } else { &[] });
            let (mut lib, space, mut chat) = configured(server.endpoint);
            let ctx = egui::Context::default();
            chat.draft = "Only my synthetic prompt".into();
            if after_send {
                chat.send(&lib, &space, &None, &ctx);
                assert!(chat.busy());
                assert!(server.received().contains("Only my synthetic prompt"));
            }
            // An external store change is absent from the in-memory library.
            let row = lib.store.all_entities().unwrap().into_iter().find(|row| row.kind == "profile" && row.id == chat.profile).unwrap();
            lib.store.put_entity(&crossbow_core::store::EntityRow { deleted: true, ..row }, false).unwrap();
            assert!(lib.profiles.contains_key(&chat.profile));
            if after_send {
                server.release.send(()).unwrap();
                settle(&mut chat, &mut lib, &space);
            } else {
                chat.send(&lib, &space, &None, &ctx);
            }
            assert!(!chat.busy());
            assert!(chat.selection.is_none() && chat.messages.is_empty() && chat.draft.is_empty());
            assert!(chat.error.as_ref().unwrap().contains("unavailable"));
            server.join();
        }
    }

    #[test]
    fn invalid_question_keeps_draft_and_never_starts_worker() {
        let (mut lib, space, mut chat) = configured(LocalEndpoint::default());
        for prompt in [" ".into(), "a\0b".into(), "é".repeat(crossbow_ai::chat::MAX_PROMPT_BYTES)] {
            chat.draft = prompt.clone();
            chat.send(&mut lib, &space, &None, &egui::Context::default());
            assert!(!chat.busy());
            assert!(chat.error.is_some());
            assert_eq!(chat.draft, prompt);
        }
    }
}
