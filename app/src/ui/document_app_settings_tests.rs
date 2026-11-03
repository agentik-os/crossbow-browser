use super::super::Page;
use super::super::repair_tests::{PreferenceFrame, settings_test_frame};
use super::*;
use crate::shortcuts::tests::Fixture;
use crossbow_core::model::Theme;
use egui::vec2;

const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];

/// The row on a disposable library: Personal and Client are Spaces with
/// their own profiles.
struct Harness {
    fixture: Fixture,
    lib: Library,
    ed: Editor,
    space: String,
    personal: String,
    client: String,
    ctx: egui::Context,
    size: egui::Vec2,
    frame: u32,
}

impl Harness {
    fn new(size: egui::Vec2) -> Self {
        let fixture = Fixture::new();
        let mut lib = Library::open(fixture.store()).unwrap();
        lib.ensure_defaults().unwrap();
        let personal = lib.spaces_ordered()[0].0.clone();
        let client_profile = lib.create_profile("Client").unwrap();
        let client = lib.create_space("Client", "C", Theme::preset(2), &client_profile).unwrap();
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        Harness { fixture, lib, ed: Editor::default(), space: personal.clone(), personal, client, ctx, size, frame: 0 }
    }

    fn profile(&self, space: &str) -> String {
        self.lib.spaces[space].profile_id.clone()
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> PreferenceFrame {
        let profile = self.profile(&self.space.clone());
        settings_test_frame(&self.ctx, Page::General, self.size, 1.0, &mut self.frame, events, |ui, pal| {
            show(&mut self.ed, &self.lib, &profile, ui, pal);
        })
    }

    fn settle(&mut self) -> PreferenceFrame {
        for _ in 0..6 { self.frame(vec![]); }
        self.frame(vec![])
    }

    /// Settled with no menu open: every label whole across and clear of the others.
    fn tidy(&mut self) -> PreferenceFrame {
        let shown = self.settle();
        for (i, (text, bounds, clip)) in shown.text.iter().enumerate() {
            assert!(clip.min.x - 0.5 <= bounds.min.x && bounds.max.x <= clip.max.x + 0.5 && !text.ends_with('…') || text == CUSTOM, "{text:?} is clipped at {:?}", self.size);
            for (other, other_bounds, _) in &shown.text[i + 1..] {
                assert!(!bounds.shrink(0.5).intersects(other_bounds.shrink(0.5)), "{text:?} overlaps {other:?} at {:?}", self.size);
            }
        }
        shown
    }

    fn shows(&mut self, wanted: &str) -> bool {
        self.settle().text.iter().any(|(text, ..)| text == wanted)
    }

    fn message(&mut self, prefix: &str) -> Option<String> {
        self.settle().text.into_iter().map(|(text, ..)| text).find(|text| text.starts_with(prefix))
    }

    fn click(&mut self, label: &str) {
        let shown = self.settle();
        let (_, bounds, _) = shown.text.iter().rev().find(|(text, _, _)| text == label).unwrap_or_else(|| panic!("no {label:?} at {:?}", self.size));
        let pos = bounds.center();
        for pressed in [true, false] {
            self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }]);
        }
        self.settle();
    }

    /// Open the menu, showing the current choice, and pick `label`.
    fn pick(&mut self, current: &str, label: &str) {
        self.click(current);
        self.click(label);
    }

    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.frame(vec![egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }]);
        }
        self.settle();
    }

    fn type_text(&mut self, text: &str) {
        self.frame(vec![egui::Event::Text(text.into())]);
        self.settle();
    }

    fn saved(&self, space: &str) -> DocumentApp {
        document_app::load(&self.fixture.store(), &self.profile(space)).0
    }

    fn field_focused(&self) -> bool {
        let field = Id::unique(("document-app-address", self.profile(&self.space)));
        self.ctx.memory(|memory| memory.has_focus(field))
    }
}

#[test]
fn a_named_app_is_chosen_per_profile_and_read_back_after_reopening() {
    for size in SIZES {
        let mut h = Harness::new(size);
        let shown = h.tidy();
        for label in [TITLE, DESCRIPTION, "None"] {
            assert!(shown.text.iter().any(|(text, ..)| text == label), "{label:?} at {size:?}");
        }
        assert_eq!(h.saved(&h.personal.clone()), DocumentApp::None, "None by default");

        h.click("None");
        let menu = h.settle();
        for app in DocumentApp::KNOWN {
            assert!(menu.text.iter().any(|(text, ..)| text == app.label()), "{} is offered at {size:?}", app.label());
        }
        assert!(menu.text.iter().any(|(text, ..)| text == CUSTOM));
        h.click("Google Sheets");
        assert_eq!(h.saved(&h.personal.clone()), DocumentApp::GoogleSheets, "saved at once at {size:?}");
        assert_eq!(h.saved(&h.client.clone()), DocumentApp::None, "another profile never changes");
        h.tidy();

        h.space = h.client.clone();
        h.pick("None", "Notion");
        assert_eq!((h.saved(&h.personal.clone()), h.saved(&h.client.clone())), (DocumentApp::GoogleSheets, DocumentApp::Notion));

        // Quit and reopen.
        h.lib = Library::open(h.fixture.store()).unwrap();
        h.ed = Editor::default();
        assert!(h.shows("Notion"));
        h.space = h.personal.clone();
        assert!(h.shows("Google Sheets"), "read back after reopening at {size:?}");
        h.pick("Google Sheets", "None");
        assert_eq!(h.saved(&h.personal.clone()), DocumentApp::None, "None can be chosen again");
    }
}

#[test]
fn a_custom_address_is_typed_checked_and_saved_and_escape_or_cancel_keeps_the_old_choice() {
    for size in SIZES {
        let mut h = Harness::new(size);
        h.pick("None", "Google Docs");
        h.pick("Google Docs", CUSTOM);
        assert!(h.ed.owns_keyboard() && h.field_focused(), "the field takes the keyboard at {size:?}");

        // Refused: the reason and what stays in use, and nothing is saved.
        h.type_text("http://pad.example/new");
        h.key(Key::Enter);
        let refused = h.message(document_app::NOT_HTTPS).unwrap_or_else(|| panic!("the reason is shown at {size:?}"));
        assert_eq!(refused, format!("{} New Document still opens Google Docs.", document_app::NOT_HTTPS));
        assert_eq!(h.saved(&h.personal.clone()), DocumentApp::GoogleDocs);
        assert!(h.field_focused(), "the field keeps the keyboard");
        for (typed, reason) in [("javascript:alert(1)", document_app::UNSAFE), ("https://me:secret@pad.example/new", document_app::CREDENTIALS)] {
            h.ed.draft = Some(String::new());
            h.type_text(typed);
            h.click("Save");
            assert!(h.message(reason).is_some(), "{typed:?} is refused at {size:?}");
            assert_eq!(h.saved(&h.personal.clone()), DocumentApp::GoogleDocs);
        }

        // Escape drops the typed address, not Settings, and keeps the old choice.
        h.key(Key::Escape);
        assert!(!h.ed.owns_keyboard() && !h.shows("Save"));
        assert_eq!(h.saved(&h.personal.clone()), DocumentApp::GoogleDocs);
        assert!(h.shows("Google Docs"));

        // Return saves a good one.
        h.pick("Google Docs", CUSTOM);
        h.type_text("https://Pad.Example/new");
        h.key(Key::Enter);
        assert_eq!(h.saved(&h.personal.clone()), DocumentApp::Custom("https://pad.example/new".into()), "saved normalized at {size:?}");
        let shown = h.tidy();
        assert!(shown.text.iter().any(|(text, ..)| text == "Custom") && shown.text.iter().any(|(text, ..)| text == "https://pad.example/new"));
        assert!(!h.ed.owns_keyboard());

        // Editing it starts from the saved address; Cancel keeps it.
        h.pick("Custom", CUSTOM);
        assert_eq!(h.ed.draft.as_deref(), Some("https://pad.example/new"));
        h.click("Cancel");
        assert!(!h.ed.owns_keyboard());
        assert_eq!(h.saved(&h.personal.clone()), DocumentApp::Custom("https://pad.example/new".into()));
    }
}

#[test]
fn the_menu_and_the_field_work_from_the_keyboard() {
    for size in SIZES {
        let mut h = Harness::new(size);
        h.settle();
        h.key(Key::Tab);
        h.key(Key::Space);
        assert!(h.shows("Google Docs"), "Space opens the menu at {size:?}");
        // Tab moves through the menu: None, Google Docs, Google Sheets.
        for _ in 0..3 { h.key(Key::Tab); }
        h.key(Key::Space);
        assert_eq!(h.saved(&h.personal.clone()), DocumentApp::GoogleSheets, "Space picks the focused app at {size:?}");
        assert!(!h.shows("Google Docs"), "and closes the menu");
    }
}

#[test]
fn a_failed_save_keeps_the_choice_and_says_so_until_retry_or_cancel() {
    for size in SIZES {
        let mut h = Harness::new(size);
        h.lib = Library::open(h.fixture.read_only()).unwrap();
        h.pick("None", "Microsoft Word");
        let error = h.message(&format!("Could not save “{TITLE}”:")).unwrap_or_else(|| panic!("the failure is explained at {size:?}"));
        assert!(error.contains("New Document still asks for an app. Check disk space"), "{error}");
        assert_eq!(h.saved(&h.personal.clone()), DocumentApp::None, "nothing was written");
        assert!(h.shows("None"), "the old choice stays shown");

        h.click("Cancel");
        assert!(!h.shows("Retry Save"));
        h.pick("None", CUSTOM);
        h.type_text("https://pad.example/new");
        h.key(Key::Enter);
        assert!(h.message("Could not save").is_some());
        assert_eq!(h.ed.draft.as_deref(), Some("https://pad.example/new"), "the typed address stays");
        h.lib = Library::open(h.fixture.store()).unwrap();
        h.click("Retry Save");
        assert_eq!(h.saved(&h.personal.clone()), DocumentApp::Custom("https://pad.example/new".into()), "Retry Save writes it at {size:?}");
        assert!(!h.shows("Retry Save") && !h.ed.owns_keyboard());
    }
}

#[test]
fn an_unreadable_saved_choice_reads_as_none_and_says_so() {
    let mut h = Harness::new(SIZES[0]);
    let profile = h.profile(&h.personal.clone());
    h.fixture.store().kv_set(&format!("document_app.{profile}"), "custom:javascript:alert(1)").unwrap();
    assert!(h.shows(document_app::UNREADABLE) && h.shows("None"));
    h.pick("None", "Notion");
    assert!(!h.shows(document_app::UNREADABLE));
    assert_eq!(h.saved(&h.personal.clone()), DocumentApp::Notion);
}

#[test]
fn choose_a_document_app_opens_general_with_the_menu_in_view_and_focused() {
    use super::super::{SettingsState, general_preferences};
    for size in SIZES {
        let h = Harness::new(size);
        let mut lib = Library::open(h.fixture.store()).unwrap();
        let mut st = SettingsState::new(Page::General, 0.0);
        st.reveal_document_app();
        let ctx = h.ctx.clone();
        let mut frame = 0;
        let mut shown = None;
        for _ in 0..8 {
            shown = Some(settings_test_frame(&ctx, Page::General, size, 1.0, &mut frame, vec![], |ui, pal| general_preferences(&mut st, &mut lib, &h.personal, ui, pal)));
        }
        let shown = shown.unwrap();
        let (_, title, clip) = shown.text.iter().find(|(text, ..)| text == TITLE).unwrap_or_else(|| panic!("the row is drawn at {size:?}"));
        assert!(clip.contains_rect(*title), "the row is scrolled into view at {size:?}: {title:?} in {clip:?}");
        assert!(ctx.memory(|memory| memory.focused()).is_some(), "its menu has the keyboard at {size:?}");
        assert!(!st.document.reveal, "once");
    }
}
