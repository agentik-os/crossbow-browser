//! Settings → General → Browsing, for the active Space's profile: search
//! suggestions behind a privacy notice, and the profile's search engines.
//! Both are kept on this Mac; see `crate::search_engines`.

use super::*;
use super::link_page::{save_failure, switch_control};
use super::profiles::decision;
use crate::search_engines::{SearchPreferences, profile_engine};
use crossbow_core::search::{CustomEngine, Engine, EngineError, EngineField, display_url, host_of};
use egui::{Key, Modifiers};

pub(super) const SUGGESTIONS: &str = "Include search engine suggestions";
pub(super) const ENGINES: &str = "Search engines and site search";
pub(super) const MANAGE: &str = "Manage Search Engines…";
pub(super) const ADD: &str = "Add Search Engine…";
const FIELDS: [(EngineField, &str, &str); 4] = [
    (EngineField::Name, "Name", "Intranet"),
    (EngineField::Keyword, "Keyword (optional)", "in"),
    (EngineField::SearchUrl, "Search URL", "https://example.com/search?q=%s"),
    (EngineField::SuggestUrl, "Suggestion URL (optional)", "https://example.com/suggest?q=%s"),
];

/// An engine being added (no id yet) or edited, as typed.
struct Draft {
    id: Option<String>,
    values: [String; 4],
    error: Option<EngineError>,
    focus: Option<EngineField>,
}

impl Draft {
    fn of(engine: Option<&CustomEngine>) -> Self {
        let values = engine.map_or_else(Default::default, |engine| [
            engine.name.clone(),
            engine.keyword.clone().unwrap_or_default(),
            engine.search_url.clone(),
            engine.suggest_url.clone().unwrap_or_default(),
        ]);
        Draft { id: engine.map(|engine| engine.id.clone()), values, error: None, focus: Some(EngineField::Name) }
    }
}

/// A save that failed, kept for Retry Save.
struct Failure {
    wanted: SearchPreferences,
    title: String,
    error: String,
    kept: String,
    done: Option<String>,
    reveal: bool,
}

#[derive(Default)]
pub(super) struct Editor {
    /// The profile `saved` belongs to; another profile starts over.
    profile: Option<String>,
    saved: Option<Result<SearchPreferences, String>>,
    /// The privacy notice waits for Turn On or Cancel.
    consent: bool,
    open: bool,
    draft: Option<Draft>,
    deleting: Option<String>,
    resetting: bool,
    failure: Option<Failure>,
    /// The latest outcome, brought into view once.
    notice: Option<String>,
    reveal_notice: bool,
    reveal: bool,
}

impl Editor {
    /// Asking, editing or confirming: Escape cancels that instead of closing
    /// Settings, and keys don't run browser shortcuts.
    pub(super) fn owns_keyboard(&self) -> bool {
        self.consent || self.draft.is_some() || self.deleting.is_some() || self.resetting
    }

    /// Leaving the page drops unsaved work; settings are read again on return.
    pub(super) fn leave(&mut self) {
        *self = Editor::default();
    }

    fn cancel(&mut self) {
        if self.draft.take().is_none() && self.deleting.take().is_none() {
            self.consent = false;
            self.resetting = false;
        }
    }

    fn saved(&mut self, lib: &Library, profile: &str) -> Result<SearchPreferences, String> {
        if self.profile.as_deref() != Some(profile) {
            *self = Editor { profile: Some(profile.into()), ..Editor::default() };
        }
        self.saved.get_or_insert_with(|| SearchPreferences::load(lib, profile)).clone()
    }

    /// Save `wanted`. Success closes what asked for it and shows `done`; a
    /// failure keeps the saved settings and any draft, and offers Retry Save.
    fn commit(&mut self, lib: &Library, profile: &str, wanted: SearchPreferences, title: String, kept: String, done: Option<String>) {
        self.consent = false;
        self.deleting = None;
        self.resetting = false;
        match wanted.save(lib, profile) {
            Ok(()) => {
                self.saved = Some(Ok(wanted));
                self.draft = None;
                self.failure = None;
                self.reveal_notice = done.is_some();
                self.notice = done;
            }
            Err(error) => {
                self.notice = None;
                self.failure = Some(Failure { wanted, title, error, kept, done, reveal: true });
            }
        }
    }
}

/// The engines added to `profile` and the one it uses, for the Search engine
/// menu. Empty while its settings can't be read.
pub(super) fn added(ed: &mut Editor, lib: &Library, profile: &str) -> (Vec<(String, String)>, Option<String>) {
    let Ok(saved) = ed.saved(lib, profile) else { return (Vec::new(), None) };
    let chosen = saved.default_engine().map(|engine| engine.id.clone());
    (saved.engines.into_iter().map(|engine| (engine.id, engine.name)).collect(), chosen)
}

/// Use the added engine `id` for `profile`, or with None its built-in choice.
pub(super) fn choose(ed: &mut Editor, lib: &Library, profile: &str, id: Option<String>) {
    let Ok(saved) = ed.saved(lib, profile) else { return };
    if saved.default == id { return; }
    let current = profile_engine(lib, profile).map_or_else(|| "its current engine".into(), |engine| engine.name().to_string());
    let mut wanted = saved;
    wanted.default = id;
    ed.commit(lib, profile, wanted, "Search engine".into(), format!("This profile still uses {current}"), None);
}

/// What `profile` falls back to without an added engine.
fn built_in_choice(lib: &Library, profile: &str) -> String {
    match lib.profiles.get(profile).and_then(|profile| profile.preferences).and_then(|preferences| preferences.search_engine) {
        Some(engine) => engine.name().to_string(),
        None => format!("the shared default ({})", lib.settings.search_engine.name()),
    }
}

pub(super) fn show(ed: &mut Editor, lib: &Library, profile: &str, ui: &mut Ui, pal: &Palette) {
    let ink = Ink::new(pal.panel, pal.accent);
    let saved = ed.saved(lib, profile);
    if ed.owns_keyboard() && !egui::Popup::is_any_open(ui.ctx()) && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
        ed.cancel();
    }
    if let Some(failure) = ed.failure.as_mut() {
        let (retry, cancel) = save_failure(ui, &ink, "search-settings", &failure.title, &failure.error, &failure.kept, &mut failure.reveal);
        if cancel {
            ed.failure = None;
        } else if retry {
            let Failure { wanted, title, kept, done, .. } = ed.failure.take().unwrap();
            ed.commit(lib, profile, wanted, title, kept, done);
            ui.ctx().request_repaint();
        }
    }
    match (saved, profile_engine(lib, profile)) {
        (Ok(saved), Some(engine)) => {
            suggestions(ed, lib, profile, &saved, &engine, ui, &ink, pal);
            engines(ed, lib, profile, &saved, &engine, ui, &ink, pal);
        }
        (Err(error), _) => unreadable(ed, lib, profile, &error, ui, &ink),
        (Ok(_), None) => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn suggestions(ed: &mut Editor, lib: &Library, profile: &str, saved: &SearchPreferences, engine: &Engine, ui: &mut Ui, ink: &Ink, pal: &Palette) {
    let mut on = saved.suggestions;
    row(ui, pal, SUGGESTIONS, &format!("Show suggestions from {} as you type in the command bar. This Mac only.", engine.name()), |ui| {
        switch_control(ui, &mut on, SUGGESTIONS, ink);
    });
    if on != saved.suggestions {
        if on {
            ed.consent = true;
            ed.reveal = true;
        } else {
            let wanted = SearchPreferences { suggestions: false, ..saved.clone() };
            ed.commit(lib, profile, wanted, SUGGESTIONS.into(), "Suggestions stay on".into(), None);
        }
        ui.ctx().request_repaint();
    }
    if (saved.suggestions || ed.consent) && engine.suggest_url("").is_none() {
        ui.label(style::text(format!("{} has no suggestion service, so nothing is sent while it is this profile's search engine.", engine.name()), 12.0, style::weight::REGULAR).color(ink.secondary));
    }
    if ed.consent {
        let text = format!("When this is on, what you type in the command bar is sent to {}, or to the engine you choose with its keyword or Tab, to get suggestions. Web addresses, file paths and text that looks like a password or key are never sent.", engine.name());
        let (confirm, cancel) = decision(ui, ink, &text, false, "Turn On", false, &mut ed.reveal);
        if cancel {
            ed.consent = false;
        } else if confirm {
            let wanted = SearchPreferences { suggestions: true, ..saved.clone() };
            ed.commit(lib, profile, wanted, SUGGESTIONS.into(), "Suggestions stay off".into(), None);
            ui.ctx().request_repaint();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn engines(ed: &mut Editor, lib: &Library, profile: &str, saved: &SearchPreferences, current: &Engine, ui: &mut Ui, ink: &Ink, pal: &Palette) {
    row(ui, pal, ENGINES, "Built-in engines, sites and ones you add. Type the start of a name, then Tab, to search just that one. Type a keyword, a space and your search to use it at once. This Mac only.", |ui| {
        if Button::new(if ed.open { "Done" } else { MANAGE }).kind(Kind::Plain).show(ui, ink).clicked() {
            ed.open = !ed.open;
            ed.draft = None;
            ed.deleting = None;
        }
    });
    if !ed.open {
        notice(ed, ui, ink);
        return;
    }
    for engine in SearchEngine::ALL {
        let host = host_of(&engine.search_url("")).unwrap_or_default();
        line(ui, ink, pal, engine.name(), &format!("Built in · {host}"), *current == Engine::BuiltIn(engine), false);
    }
    for engine in &saved.engines {
        let address = display_url(&engine.search_url);
        let detail = engine.keyword.as_ref().map_or(address.clone(), |keyword| format!("{keyword} · {address}"));
        let in_use = *current == Engine::Custom(engine.clone());
        let (edit, delete) = ui.push_id(("added-engine", &engine.id), |ui| line(ui, ink, pal, &engine.name, &detail, in_use, true)).inner;
        if edit {
            ed.draft = Some(Draft::of(Some(engine)));
            ed.deleting = None;
        }
        if delete {
            ed.deleting = Some(engine.id.clone());
            ed.draft = None;
            ed.reveal = true;
        }
        if ed.deleting.as_deref() == Some(engine.id.as_str()) {
            let chosen = saved.default.as_deref() == Some(engine.id.as_str());
            let fallback = built_in_choice(lib, profile);
            let text = if chosen {
                format!("Delete “{}”? It is this profile's search engine, so the profile then uses {fallback}.", engine.name)
            } else {
                format!("Delete “{}”?", engine.name)
            };
            let (confirm, cancel) = decision(ui, ink, &text, false, "Delete", false, &mut ed.reveal);
            if cancel {
                ed.deleting = None;
            } else if confirm {
                let mut wanted = saved.clone();
                wanted.engines.retain(|other| other.id != engine.id);
                if chosen { wanted.default = None; }
                let done = if chosen { format!("Deleted “{}”. This profile now uses {fallback}.", engine.name) } else { format!("Deleted “{}”.", engine.name) };
                ed.commit(lib, profile, wanted, engine.name.clone(), "Nothing changed".into(), Some(done));
                ui.ctx().request_repaint();
            }
        }
    }
    notice(ed, ui, ink);
    if ed.draft.is_none() {
        ui.add_space(4.0);
        if Button::new(ADD).kind(Kind::Plain).show(ui, ink).clicked() {
            ed.draft = Some(Draft::of(None));
            ed.deleting = None;
        }
    } else {
        editor(ed, lib, profile, saved, ui, ink);
    }
}

fn notice(ed: &mut Editor, ui: &mut Ui, ink: &Ink) {
    let Some(text) = &ed.notice else { return };
    let response = ui.label(style::text(text, 12.0, style::weight::REGULAR).color(ink.secondary));
    if !ui.is_sizing_pass() && std::mem::take(&mut ed.reveal_notice) { response.scroll_to_me(None); }
}

/// One engine: its name, what is in use, Edit and Delete for added ones, and
/// its address elided to one line.
fn line(ui: &mut Ui, ink: &Ink, pal: &Palette, name: &str, detail: &str, in_use: bool, editable: bool) -> (bool, bool) {
    let mut clicked = (false, false);
    // Buttons first, so a long name truncates instead of pushing them out.
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 26.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if editable {
            clicked.1 = Button::new("Delete…").kind(Kind::Plain).size(style::Size::Small).show(ui, ink).clicked();
            clicked.0 = Button::new("Edit").kind(Kind::Plain).size(style::Size::Small).show(ui, ink).clicked();
        }
        if in_use {
            // Painted at its exact size: a wrapping label misplaces its text here.
            let tag = ui.painter().layout_no_wrap("In use".into(), FontId::proportional(12.0), ink.secondary);
            let (rect, _) = ui.allocate_exact_size(tag.size(), Sense::hover());
            ui.painter().galley(rect.min, tag, ink.secondary);
        }
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add(egui::Label::new(style::text(name, 13.5, style::weight::MEDIUM).color(ink.text)).truncate());
        });
    });
    ui.add(egui::Label::new(style::text(detail, 12.0, style::weight::REGULAR).color(ink.secondary)).truncate());
    separator(ui, pal);
    clicked
}

fn editor(ed: &mut Editor, lib: &Library, profile: &str, saved: &SearchPreferences, ui: &mut Ui, ink: &Ink) {
    let Some(draft) = ed.draft.as_mut() else { return };
    let heading = match draft.id.as_deref().and_then(|id| saved.engines.iter().find(|engine| engine.id == id)) {
        Some(engine) => format!("Edit “{}”", engine.name),
        None => "Add Search Engine".into(),
    };
    ui.label(style::text(heading, 13.5, style::weight::SEMIBOLD).color(ink.text));
    let mut submit = false;
    for (index, (field, label, hint)) in FIELDS.iter().enumerate() {
        ui.label(style::text(*label, 12.5, style::weight::MEDIUM).color(ink.text));
        let width = ui.available_width().min(420.0);
        let response = ui.add(egui::TextEdit::singleline(&mut draft.values[index]).id(Id::unique(("search-engine-field", index))).hint_text(*hint).desired_width(width));
        let focus = draft.focus == Some(*field);
        if focus {
            response.request_focus();
            draft.focus = None;
        }
        if response.changed() && draft.error.as_ref().is_some_and(|error| error.field == *field) { draft.error = None; }
        submit |= response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
        let mut shown = response.rect;
        if let Some(error) = draft.error.as_ref().filter(|error| error.field == *field) {
            shown = shown.union(ui.label(style::text(format!("{}: {}", field.label(), error.reason), 12.0, style::weight::REGULAR).color(ink.danger)).rect);
        }
        // A refused field comes into view with its reason.
        if focus { ui.scroll_to_rect(shown, None); }
    }
    let mut cancel = false;
    ui.horizontal_wrapped(|ui| {
        submit |= Button::new("Save").kind(Kind::Primary).show(ui, ink).clicked();
        cancel = Button::new("Cancel").kind(Kind::Plain).show(ui, ink).clicked();
    });
    if cancel {
        ed.draft = None;
    } else if submit {
        save_draft(ed, lib, profile, saved);
        ui.ctx().request_repaint();
    }
}

/// Check the draft and save it; a refused draft says which field and why,
/// and nothing is saved.
fn save_draft(ed: &mut Editor, lib: &Library, profile: &str, saved: &SearchPreferences) {
    let Some(draft) = ed.draft.as_mut() else { return };
    let [name, keyword, search_url, suggest_url] = draft.values.clone();
    let id = draft.id.clone().unwrap_or_else(crossbow_core::new_id);
    let engine = CustomEngine { id, name, keyword: Some(keyword), search_url, suggest_url: Some(suggest_url) }.normalized();
    if let Err(error) = engine.validate(&saved.engines, &crate::ui::command_bar::command_words()) {
        draft.focus = Some(error.field);
        draft.error = Some(error);
        return;
    }
    let mut wanted = saved.clone();
    let done = match wanted.engines.iter_mut().find(|other| other.id == engine.id) {
        Some(slot) => {
            *slot = engine.clone();
            format!("Saved “{}”.", engine.name)
        }
        None => {
            wanted.engines.push(engine.clone());
            format!("Added “{}”.", engine.name)
        }
    };
    ed.commit(lib, profile, wanted, engine.name, "Nothing changed".into(), Some(done));
}

/// Saved settings that can't be read keep search off until reset.
fn unreadable(ed: &mut Editor, lib: &Library, profile: &str, error: &str, ui: &mut Ui, ink: &Ink) {
    ui.label(style::text(format!("{error}. Search from the command bar and selected text is off for this profile until they are reset."), 12.5, style::weight::REGULAR).color(ink.danger));
    if ed.resetting {
        let (confirm, cancel) = decision(ui, ink, "Reset search settings for this profile? Engines you added are removed, the default sites come back and suggestions turn off.", false, "Reset", false, &mut ed.reveal);
        if cancel {
            ed.resetting = false;
        } else if confirm {
            ed.commit(lib, profile, SearchPreferences::initial(), "search settings".into(), "The saved settings are unchanged".into(), Some("Search settings were reset.".into()));
            ui.ctx().request_repaint();
        }
    } else if Button::new("Reset Search Settings…").kind(Kind::Plain).show(ui, ink).clicked() {
        ed.resetting = true;
        ed.reveal = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::repair_tests::{PreferenceFrame, settings_test_frame};
    use crate::search_engines::tests::engine;
    use crate::shortcuts::tests::Fixture;
    use egui::Event;

    const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
    const NOTICE: &str = "When this is on, what you type in the command bar is sent to Google, or to the engine you choose with its keyword or Tab, to get suggestions. Web addresses, file paths and text that looks like a password or key are never sent.";

    /// Settings → General for the first Space's profile, on a disposable library.
    struct Harness {
        fixture: Fixture,
        lib: Library,
        space: String,
        profile: String,
        state: SettingsState,
        ctx: egui::Context,
        size: egui::Vec2,
        frame: u32,
    }

    impl Harness {
        fn new(size: egui::Vec2) -> Self {
            let fixture = Fixture::new();
            let mut lib = Library::open(fixture.store()).unwrap();
            lib.ensure_defaults().unwrap();
            let space = lib.spaces_ordered()[0].0.clone();
            let profile = lib.spaces[&space].profile_id.clone();
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            Harness { fixture, lib, space, profile, state: SettingsState::new(Page::General, 0.0), ctx, size, frame: 0 }
        }

        /// What a reopened library reads from disk.
        fn saved(&self) -> SearchPreferences {
            SearchPreferences::load(&Library::open(self.fixture.store()).unwrap(), &self.profile).unwrap()
        }

        fn seed(&mut self, preferences: SearchPreferences) {
            preferences.save(&self.lib, &self.profile).unwrap();
            self.state.search.leave();
        }

        fn read_only(&mut self) {
            self.lib = Library::open(self.fixture.read_only()).unwrap();
        }

        fn writable(&mut self) {
            self.lib = Library::open(self.fixture.store()).unwrap();
        }

        fn frame(&mut self, events: Vec<Event>) -> PreferenceFrame {
            settings_test_frame(&self.ctx, Page::General, self.size, 1.0, &mut self.frame, events, |ui, pal| {
                general_preferences(&mut self.state, &mut self.lib, &self.space, ui, pal);
            })
        }

        /// Settle animations; visible text fits the sheet and no two texts
        /// overlap. Text inside a field may scroll, so field values are skipped.
        fn settle(&mut self) -> PreferenceFrame {
            for _ in 0..6 { self.frame(vec![]); }
            let shown = self.frame(vec![]);
            let typed: Vec<String> = self.state.search.draft.as_ref().map_or(Vec::new(), |draft| draft.values.to_vec());
            let visible: Vec<_> = shown.text.iter().filter(|(text, bounds, clip)| bounds.intersects(*clip) && !text.is_empty() && !typed.contains(text)).collect();
            for (text, bounds, clip) in &visible {
                assert!(bounds.min.x >= clip.min.x - 0.5 && bounds.max.x <= clip.max.x + 0.5, "{text:?} overflows the sheet at {:?}: {bounds:?} in {clip:?}", self.size);
            }
            let open = egui::Popup::is_any_open(&self.ctx);
            for (index, (text, bounds, clip)) in visible.iter().enumerate() {
                for (other, other_bounds, other_clip) in &visible[index + 1..] {
                    let overlap = bounds.intersect(*other_bounds).intersect(clip.intersect(*other_clip));
                    assert!(open || overlap.width() <= 1.0 || overlap.height() <= 1.0, "{text:?} overlaps {other:?} at {:?}: {bounds:?} {other_bounds:?} clips {clip:?} {other_clip:?}", self.size);
                }
            }
            shown
        }

        /// Scroll until `pick` is wholly in view; its centre.
        fn reveal(&mut self, what: &str, pick: impl Fn(&PreferenceFrame) -> Option<(Rect, Rect)>) -> egui::Pos2 {
            let sheet = sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), self.size), 1.0);
            let wheel = sheet.right_bottom() - vec2(60.0, 40.0);
            for attempt in 0..60 {
                let shown = self.settle();
                let delta = match pick(&shown) {
                    Some((bounds, clip)) if clip.expand(0.5).contains_rect(bounds) => return bounds.center(),
                    Some((bounds, clip)) => if bounds.center().y > clip.center().y { -100.0 } else { 100.0 },
                    None => if attempt < 12 { -200.0 } else { 200.0 },
                };
                self.frame(vec![
                    Event::PointerMoved(wheel),
                    Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, delta), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Modifiers::NONE },
                ]);
                for _ in 0..30 { self.frame(vec![]); }
            }
            panic!("{what} never became visible at {:?}", self.size);
        }

        fn visible(&mut self, label: &str) -> egui::Pos2 {
            self.reveal(label, |shown| shown.text.iter().find(|(text, ..)| text == label).map(|(_, bounds, clip)| (*bounds, *clip)))
        }

        fn shows(&mut self, label: &str) -> bool {
            self.settle().text.iter().any(|(text, ..)| text == label)
        }

        fn switch(&mut self) -> Rect {
            let beside = |shown: &PreferenceFrame| -> Option<(Rect, Rect)> {
                let (_, title, _) = shown.text.iter().find(|(text, ..)| text == SUGGESTIONS)?;
                shown.switches.iter().find(|switch| switch.min.x > title.max.x && switch.top() > title.top() - 4.0 && switch.top() < title.top() + 60.0).map(|switch| (*switch, *title))
            };
            self.reveal(SUGGESTIONS, |shown| {
                let (_, title, clip) = shown.text.iter().find(|(text, ..)| text == SUGGESTIONS)?;
                let row = beside(shown).map_or(title.union(title.translate(vec2(0.0, 50.0))), |(switch, title)| switch.union(title));
                Some((row, *clip))
            });
            beside(&self.settle()).expect("the switch is beside its title").0
        }

        fn click_at(&mut self, pos: egui::Pos2) {
            for pressed in [true, false] {
                self.frame(vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
            }
            self.settle();
        }

        fn click(&mut self, label: &str) {
            let pos = self.visible(label);
            self.click_at(pos);
        }

        /// Click the last `label` drawn, which is the one in an open menu.
        fn click_in_menu(&mut self, label: &str) {
            let pos = self.settle().text.iter().rev().find(|(text, ..)| text == label).map(|(_, bounds, _)| bounds.center()).unwrap_or_else(|| panic!("{label} is in the menu"));
            self.click_at(pos);
        }

        fn key(&mut self, key: Key, modifiers: Modifiers) {
            for pressed in [true, false] {
                self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }]);
            }
        }

        fn type_text(&mut self, text: &str) {
            self.frame(vec![Event::Text(text.into())]);
        }

        /// Click into the field under `label`.
        fn focus_field(&mut self, label: &str) {
            let centre = self.reveal(label, |shown| shown.text.iter().find(|(text, ..)| text == label).map(|(_, title, clip)| (title.union(title.translate(vec2(0.0, 36.0))), *clip)));
            let title = self.settle().text.into_iter().find(|(text, ..)| text == label).map(|(_, title, _)| title).unwrap();
            assert!(title.center().y < centre.y);
            self.click_at(pos2(title.left() + 40.0, title.bottom() + 18.0));
        }

        /// Fill the open editor field by field with the keyboard, then press Return.
        fn fill(&mut self, values: [&str; 4]) {
            self.focus_field("Name");
            for (index, value) in values.iter().enumerate() {
                if index > 0 { self.key(Key::Tab, Modifiers::NONE); }
                self.key(Key::A, Modifiers::COMMAND);
                if value.is_empty() {
                    self.key(Key::Backspace, Modifiers::NONE);
                } else {
                    self.type_text(value);
                }
            }
            self.key(Key::Enter, Modifiers::NONE);
            self.settle();
        }

        /// After any scroll the page started has finished, `label` is wholly in view.
        fn in_view(&mut self, label: &str) -> bool {
            for _ in 0..30 { self.frame(vec![]); }
            self.settle().text.iter().any(|(text, bounds, clip)| text == label && clip.expand(0.5).contains_rect(*bounds))
        }

        fn starting(&mut self, prefix: &str) -> Option<String> {
            self.settle().text.into_iter().map(|(text, ..)| text).find(|text| text.starts_with(prefix))
        }

        /// Click `button` on the engine line named `name`.
        fn click_beside(&mut self, name: &str, button: &str) {
            self.visible(name);
            let shown = self.settle();
            let (_, line, _) = shown.text.iter().find(|(text, ..)| text == name).unwrap();
            let pos = shown.text.iter().find(|(text, bounds, _)| text == button && (bounds.center().y - line.center().y).abs() < 8.0)
                .map(|(_, bounds, _)| bounds.center()).unwrap_or_else(|| panic!("{button} beside {name}"));
            self.click_at(pos);
        }
    }

    #[test]
    fn default_sites_are_listed_edited_and_deleted_like_added_engines() {
        use crossbow_core::search::SITES;
        for size in SIZES {
            let mut h = Harness::new(size);
            assert_eq!(ENGINES, "Search engines and site search");
            h.visible(ENGINES);
            h.visible("Built-in engines, sites and ones you add. Type the start of a name, then Tab, to search just that one. Type a keyword, a space and your search to use it at once. This Mac only.");
            h.click(MANAGE);
            for (_, name, template) in SITES {
                h.visible(name);
                h.visible(&display_url(template));
            }
            assert_eq!(h.lib.store.kv_get(&format!("search.v1.{}", h.profile)).unwrap(), None, "showing the sites saves nothing");

            // Editing a site works like editing an added engine; Cancel keeps it.
            h.click_beside("Wikipedia", "Edit");
            h.visible("Edit “Wikipedia”");
            h.click("Cancel");
            assert_eq!(h.saved(), SearchPreferences::initial());
            h.click_beside("Wikipedia", "Edit");
            h.fill(["Wikipedia DE", "wd", "https://de.wikipedia.org/w/index.php?search=%s", ""]);
            assert_eq!(h.starting("Saved").as_deref(), Some("Saved “Wikipedia DE”."));
            // A taken name is refused.
            h.click(ADD);
            h.fill(["youtube", "", "https://video.example.test/?q=%s", ""]);
            assert!(h.shows("Name: “youtube” is already a search engine. Choose another name."), "at {size:?}");
            h.key(Key::Escape, Modifiers::NONE);
            // Deleting asks first.
            h.click_beside("YouTube", "Delete…");
            h.visible("Delete “YouTube”?");
            h.click("Delete");
            assert!(h.in_view("Deleted “YouTube”."), "at {size:?}");
            let names = |saved: SearchPreferences| saved.engines.into_iter().map(|engine| (engine.name, engine.keyword)).collect::<Vec<_>>();
            let expected = vec![("Wikipedia DE".to_string(), Some("wd".to_string())), ("GitHub".into(), None), ("MDN".into(), None), ("Google Maps".into(), None)];
            assert_eq!(names(h.saved()), expected);

            // Reopening reads the same list, and the deleted site stays deleted.
            h.writable();
            h.state = SettingsState::new(Page::General, 0.0);
            h.click(MANAGE);
            h.visible("Wikipedia DE");
            assert!(!h.shows("YouTube"));
            assert_eq!(names(h.saved()), expected);
        }
    }

    #[test]
    fn suggestions_ask_first_and_turning_on_survives_reopening() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let switch = h.switch().center();
            assert!(!h.saved().suggestions, "off by default");
            assert!(!h.shows(NOTICE));
            // Turning on asks; Cancel and Escape leave it off.
            h.click_at(switch);
            h.visible(NOTICE);
            assert!(!h.saved().suggestions && h.state.owns_keyboard());
            h.click("Cancel");
            assert!(!h.shows(NOTICE) && !h.saved().suggestions);
            let switch = h.switch().center();
            h.click_at(switch);
            h.visible(NOTICE);
            h.key(Key::Escape, Modifiers::NONE);
            assert!(!h.shows(NOTICE) && !h.saved().suggestions && !h.state.owns_keyboard());
            // Turn On saves it on this Mac only.
            let switch = h.switch().center();
            h.click_at(switch);
            h.click("Turn On");
            assert!(!h.shows(NOTICE));
            assert!(h.saved().suggestions);
            assert_eq!(Library::open(h.fixture.store()).unwrap().profiles[&h.profile].preferences, None);
            h.state = SettingsState::new(Page::General, 0.0);
            let switch = h.switch();
            assert!(h.settle().switches.contains(&switch));
            // Turning off needs no confirmation.
            h.click_at(switch.center());
            assert!(!h.shows(NOTICE) && !h.saved().suggestions);
        }
    }

    #[test]
    fn a_failed_suggestion_save_keeps_the_choice_with_retry_and_cancel() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.read_only();
            let switch = h.switch().center();
            h.click_at(switch);
            h.click("Turn On");
            let failure = h.starting("Could not save “Include search engine suggestions”:").expect("the failure is shown");
            assert!(failure.contains("Suggestions stay off. Check disk space"), "{failure}");
            h.click("Cancel");
            assert!(h.starting("Could not save").is_none() && !h.saved().suggestions);
            let switch = h.switch().center();
            h.click_at(switch);
            h.click("Turn On");
            h.writable();
            h.click("Retry Save");
            assert!(h.starting("Could not save").is_none());
            assert!(h.saved().suggestions);
        }
    }

    #[test]
    fn engines_are_listed_added_edited_deleted_and_survive_reopening() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.seed(SearchPreferences::default());
            h.click(MANAGE);
            for engine in SearchEngine::ALL { h.visible(engine.name()); }
            h.visible("Built in · google.com");
            h.click(ADD);
            assert!(h.state.owns_keyboard());
            h.fill(["Intranet", "in", "https://search.example.test/?q=%s", "https://search.example.test/suggest?q=%s"]);
            assert_eq!(h.starting("Added").as_deref(), Some("Added “Intranet”."));
            let saved = h.saved();
            assert_eq!(saved.engines.len(), 1);
            let added = &saved.engines[0];
            assert_eq!((added.name.as_str(), added.keyword.as_deref(), added.search_url.as_str(), added.suggest_url.as_deref()),
                ("Intranet", Some("in"), "https://search.example.test/?q=%s", Some("https://search.example.test/suggest?q=%s")));
            h.visible("in · search.example.test/?q=%s");

            // Edit, then Cancel: nothing changes.
            h.click("Edit");
            h.visible("Edit “Intranet”");
            h.fill(["Changed", "", "", ""]);
            assert!(h.starting("Name: ").is_none());
            assert_eq!(h.starting("Search URL: ").as_deref(), Some("Search URL: Enter an address with %s where the search text goes."));
            h.click("Cancel");
            assert_eq!(h.saved(), saved);
            // Edit and save.
            h.click("Edit");
            h.fill(["Intranet Search", "is", "https://search.example.test/find/%s", ""]);
            assert_eq!(h.starting("Saved").as_deref(), Some("Saved “Intranet Search”."));
            let edited = h.saved();
            assert_eq!(edited.engines.len(), 1);
            assert_eq!(edited.engines[0].id, added.id);
            assert_eq!((edited.engines[0].keyword.as_deref(), edited.engines[0].suggest_url.as_deref()), (Some("is"), None));

            // Delete asks; Cancel keeps it.
            h.click("Delete…");
            h.visible("Delete “Intranet Search”?");
            h.click("Cancel");
            assert_eq!(h.saved(), edited);
            h.click("Delete…");
            h.key(Key::Escape, Modifiers::NONE);
            assert!(!h.shows("Delete “Intranet Search”?") && !h.state.owns_keyboard());
            assert_eq!(h.saved(), edited);
            h.click("Delete…");
            h.click("Delete");
            assert_eq!(h.starting("Deleted").as_deref(), Some("Deleted “Intranet Search”."));
            assert_eq!(h.saved(), SearchPreferences::default());
        }
    }

    #[test]
    fn a_failed_engine_save_keeps_the_draft_with_retry_and_cancel() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.seed(SearchPreferences::default());
            h.click(MANAGE);
            h.click(ADD);
            h.read_only();
            h.fill(["Intranet", "in", "https://search.example.test/?q=%s", ""]);
            let failure = h.starting("Could not save “Intranet”:").expect("the failure is shown");
            assert!(failure.contains("Nothing changed. Check disk space"), "{failure}");
            assert!(h.state.search.draft.is_some(), "the typed engine is kept");
            h.click("Cancel");
            assert!(h.starting("Could not save").is_none() && h.state.search.draft.is_some());
            assert_eq!(h.saved(), SearchPreferences::default());
            h.click("Save");
            h.starting("Could not save “Intranet”:").expect("the failure is shown again");
            h.writable();
            h.click("Retry Save");
            assert!(h.in_view("Added “Intranet”."), "the outcome scrolls into view at {size:?}");
            assert!(h.state.search.draft.is_none());
            assert_eq!(h.saved().engines.iter().map(|engine| engine.name.as_str()).collect::<Vec<_>>(), ["Intranet"]);
        }
    }

    #[test]
    fn invalid_engines_are_refused_with_the_field_and_reason() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let kept = SearchPreferences { engines: vec![engine("a", "GitHub", Some("gh"), "https://github.com/search?q=%s", None)], ..Default::default() };
            h.seed(kept.clone());
            h.click(MANAGE);
            h.click(ADD);
            for (values, error) in [
                (["", "", "https://docs.example.test/?q=%s", ""], "Name: Enter a name."),
                (["Docs", "", "https://docs.example.test/", ""], "Search URL: Include %s where the search text goes."),
                (["Docs", "", "ftp://docs.example.test/%s", ""], "Search URL: Use an http or https address."),
                (["Docs", "", "https://docs.example.test/?q=%s", "javascript:%s"], "Suggestion URL: Use an http or https address."),
                (["Docs", "GH", "https://docs.example.test/?q=%s", ""], "Keyword: “GH” is already the keyword for “GitHub”."),
                (["Docs", "find", "https://docs.example.test/?q=%s", ""], "Keyword: “find” starts a command-bar command. Choose another keyword."),
            ] {
                h.fill(values);
                assert!(h.shows(error), "{error} at {size:?}");
                assert!(h.state.search.draft.is_some(), "the editor stays open");
                assert_eq!(h.saved(), kept);
            }
            h.key(Key::Escape, Modifiers::NONE);
            assert!(h.state.search.draft.is_none() && h.shows(ADD));
            assert_eq!(h.saved(), kept);
        }
    }

    #[test]
    fn the_engine_menu_offers_added_engines_and_deleting_the_one_in_use_falls_back() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.seed(SearchPreferences { engines: vec![engine("a", "Intranet", None, "https://search.example.test/?q=%s", None)], ..Default::default() });
            h.click("Google");
            h.click_in_menu("Intranet");
            assert_eq!(h.saved().default.as_deref(), Some("a"));
            assert_eq!(profile_engine(&h.lib, &h.profile).map(|engine| engine.name().to_string()).as_deref(), Some("Intranet"));
            assert_eq!(Library::open(h.fixture.store()).unwrap().profiles[&h.profile].preferences, None, "the synced choice is untouched");
            // Choosing a built-in engine again replaces the added one.
            h.click("Intranet");
            h.click_in_menu("Kagi");
            assert_eq!(h.saved().default, None);
            assert_eq!(profile_engine(&h.lib, &h.profile), Some(Engine::BuiltIn(SearchEngine::Kagi)));
            h.click("Kagi");
            h.click_in_menu("Shared default (Google)");
            h.click("Google");
            h.click_in_menu("Intranet");

            h.click(MANAGE);
            h.visible("In use");
            h.click("Delete…");
            h.visible("Delete “Intranet”? It is this profile's search engine, so the profile then uses the shared default (Google).");
            h.click("Delete");
            assert!(h.in_view("Deleted “Intranet”. This profile now uses the shared default (Google)."), "at {size:?}");
            assert_eq!(h.saved(), SearchPreferences::default());
            assert_eq!(profile_engine(&h.lib, &h.profile), Some(Engine::BuiltIn(SearchEngine::Google)));
        }
    }

    #[test]
    fn long_addresses_are_elided_and_unreadable_settings_offer_a_reset() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let long = format!("https://search.example.test/{}?q=%s", "very-long-path/".repeat(20));
            h.seed(SearchPreferences { engines: vec![engine("a", &"Long Name ".repeat(5).trim().to_string(), Some("long"), &long, None)], ..Default::default() });
            h.click(MANAGE);
            let detail = format!("long · {}", display_url(&long));
            h.visible(&detail);
            let shown = h.settle();
            let (_, bounds, clip) = shown.text.iter().find(|(text, ..)| *text == detail).unwrap();
            assert!(bounds.width() < clip.width() && bounds.height() < 20.0, "one elided line at {size:?}: {bounds:?}");
            h.click("Edit");
            h.visible("Search URL");
            h.key(Key::Escape, Modifiers::NONE);

            h.lib.store.kv_set(&format!("search.v1.{}", h.profile), "{").unwrap();
            h.state.search.leave();
            let error = format!("{}. Search from the command bar and selected text is off for this profile until they are reset.", crate::search_engines::UNREADABLE);
            h.visible(&error);
            assert!(!h.shows(SUGGESTIONS) && !h.shows(MANAGE));
            h.click("Reset Search Settings…");
            h.click("Cancel");
            assert_eq!(h.lib.store.kv_get(&format!("search.v1.{}", h.profile)).unwrap().as_deref(), Some("{"));
            h.click("Reset Search Settings…");
            h.click("Reset");
            assert!(h.in_view("Search settings were reset."), "at {size:?}");
            assert_eq!(h.saved(), SearchPreferences::initial(), "reset brings back the default sites");
            h.visible(SUGGESTIONS);
        }
    }
}
