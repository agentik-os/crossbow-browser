//! Settings → Links: whether Crossbow is the system's default browser, Air
//! Traffic Control rules that choose the Space a link from another app opens
//! in, and how Little Windows, ⌥⌘-click and Shift-click work. Everything
//! here is saved on this device only.

use super::*;
use crate::links::DefaultBrowser;
use crate::app::little::LinkWindow;
use crate::hotkey::{self, GlobalHotkey};
use crate::link_prefs::{ARCHIVE_CHOICES, ARCHIVE_TITLE, ArchiveAfter, DeviceSwitch, LinkPreferences, SwitchSpec};
use crate::shortcuts::{Chord, Shortcuts};
use super::shortcut_page::{CHIP_WIDTH, CONTROL_HEIGHT, RECORDING, UNSET, chip, lock_focus, take_recorded};
use crate::ui::style::Size;
use crossbow_core::link_routing::{Destination, Link, LinkRouting, MAX_VALUE, Match, Refusal, Routing, Rule, accept, usable_space};
use crossbow_core::store::Store;
use egui::{Event, Key, Modifiers};

const VALUE_FIELD: &str = "link-rule-value";
const TEST_FIELD: &str = "link-test";
const MENU_WIDTH: f32 = 190.0;
const MOST_RECENT: &str = "Most Recent Space";
const HOTKEY_FIELD: &str = "little-window-hotkey";

/// What this page reads from and opens in the system. Tests supply their own.
pub(super) trait System {
    /// Read only: nothing here may change the system default browser.
    fn default_browser(&mut self) -> DefaultBrowser;
    fn open_settings(&mut self) -> Result<(), String>;
}

pub(super) struct Native;

impl System for Native {
    fn default_browser(&mut self) -> DefaultBrowser {
        crate::links::default_browser()
    }

    fn open_settings(&mut self) -> Result<(), String> {
        crate::links::open_default_browser_settings()
    }
}

struct Draft {
    /// The rule being edited; None adds one.
    id: Option<String>,
    kind: Match,
    value: String,
    space: Option<String>,
    error: Option<String>,
}

struct Failure {
    candidate: Routing,
    error: String,
    reveal: bool,
}

/// Where keyboard focus goes once the control is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
enum FocusTarget {
    Add,
    Edit(String),
    Delete(String),
    Up(String),
    Down(String),
    Value,
    Confirm,
    Hotkey,
}

enum RowAction {
    Up,
    Down,
    Edit,
    Delete,
    ConfirmDelete,
    CancelDelete,
}

#[derive(Default)]
pub(super) struct Editor {
    draft: Option<Draft>,
    /// A rule waiting for Delete to be confirmed.
    deleting: Option<String>,
    failure: Option<Failure>,
    test: String,
    /// Read when the page opens and when the window comes back to the front.
    status: Option<DefaultBrowser>,
    /// The result of the last Open System Settings.
    opened: Option<Result<(), String>>,
    /// Focus to move, for at most this many more frames.
    focus: Option<(FocusTarget, u8)>,
    /// The Little Window shortcut field is waiting for a chord.
    recording_hotkey: bool,
    /// A chord the field refused, explained under it.
    hotkey_refusal: Option<String>,
}

impl Editor {
    /// Editing, confirming or recording: Escape cancels that instead of
    /// closing Settings, and keys don't run browser shortcuts.
    pub(super) fn owns_keyboard(&self) -> bool {
        self.draft.is_some() || self.deleting.is_some() || self.recording_hotkey
    }

    /// Leaving the page discards an unsaved rule; the default browser is
    /// read again on return.
    pub(super) fn leave(&mut self) {
        self.draft = None;
        self.deleting = None;
        self.status = None;
        self.opened = None;
        self.focus = None;
        self.recording_hotkey = false;
        self.hotkey_refusal = None;
    }

    fn focus_on(&mut self, target: FocusTarget) {
        self.focus = Some((target, 2));
    }

    fn take_focus(&mut self, target: FocusTarget, response: &egui::Response) {
        if self.focus.as_ref().is_some_and(|(pending, _)| *pending == target) {
            response.request_focus();
            response.scroll_to_me(None);
            self.focus = None;
        }
    }

    fn cancel(&mut self) {
        if let Some(draft) = self.draft.take() {
            self.focus_on(draft.id.map_or(FocusTarget::Add, FocusTarget::Edit));
        } else if let Some(id) = self.deleting.take() {
            self.focus_on(FocusTarget::Delete(id));
        }
    }

    /// One write; on failure the saved rules stay in use and Retry offers it again.
    fn commit(&mut self, routing: &mut LinkRouting, store: &Store, candidate: Routing) {
        match routing.save(store, candidate.clone()) {
            Ok(()) => self.failure = None,
            Err(error) => {
                self.failure = Some(Failure {
                    candidate,
                    error: format!("Could not save link rules: {error}. The previous rules are still in use. Check disk space and data-folder permissions, then Retry Save or Cancel."),
                    reveal: true,
                });
            }
        }
    }

    fn add(&mut self, lib: &Library, active_space: &str) {
        self.deleting = None;
        let space = usable_space(lib, active_space).then(|| active_space.to_string());
        self.draft = Some(Draft { id: None, kind: Match::UrlContains, value: String::new(), space, error: None });
        self.focus_on(FocusTarget::Value);
    }

    fn edit(&mut self, rule: &Rule) {
        self.deleting = None;
        self.draft = Some(Draft { id: Some(rule.id.clone()), kind: rule.kind, value: rule.value.clone(), space: Some(rule.space.clone()), error: None });
        self.focus_on(FocusTarget::Value);
    }

    /// Check the draft, then save it in place or, for a new rule, last.
    fn save_draft(&mut self, routing: &mut LinkRouting, store: &Store, lib: &Library) {
        let Some(draft) = self.draft.as_mut() else { return };
        let Some(space) = draft.space.clone().filter(|space| usable_space(lib, space)) else {
            draft.error = Some(if draft.space.is_some() { "That Space no longer exists. Choose another Space." } else { "Choose the Space these links open in." }.into());
            return;
        };
        let id = draft.id.clone().unwrap_or_else(crossbow_core::new_id);
        let rule = match Rule::new(id.clone(), draft.kind, &draft.value, space) {
            Ok(rule) => rule,
            Err(error) => {
                draft.error = Some(error);
                self.focus_on(FocusTarget::Value);
                return;
            }
        };
        let mut candidate = routing.saved().clone();
        match candidate.rules.iter_mut().find(|existing| existing.id == id) {
            Some(existing) => *existing = rule,
            None => candidate.rules.push(rule),
        }
        self.draft = None;
        self.commit(routing, store, candidate);
        self.focus_on(FocusTarget::Edit(id));
    }
}

/// A Space's name, with its profile's when there are several.
fn space_label(lib: &Library, space: &str) -> String {
    let Some(space) = lib.spaces.get(space) else { return "a deleted Space".into() };
    match lib.profiles.get(&space.profile_id) {
        Some(profile) if lib.profiles.len() > 1 => format!("{} · {}", space.name, profile.name),
        _ => space.name.clone(),
    }
}

fn destination_label(lib: &Library, destination: &Destination) -> String {
    match destination {
        Destination::MostRecent => MOST_RECENT.into(),
        Destination::Space(space) => space_label(lib, space),
    }
}

/// What Test a Link says about `input`, and whether it is a warning.
fn test_result(routing: &Routing, lib: &Library, most_recent: &str, input: &str) -> Option<(String, bool)> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    // A typed site name is tested as the https link it stands for.
    let link = match accept(input) {
        Err(Refusal::Malformed) => crossbow_core::search::input_to_url(input).map_or(Err(Refusal::Malformed), |url| accept(&url)),
        other => other,
    };
    let url = match link {
        Err(refusal) => return Some((refusal.reason(), true)),
        Ok(Link::App) => return Some(("This link brings Crossbow to the front without opening a tab.".into(), false)),
        Ok(Link::Page(url)) => url,
    };
    let Some(route) = routing.route(lib, &url, most_recent) else {
        return Some(("No Space can open links. Create a Space first.".into(), true));
    };
    let space = space_label(lib, &route.space);
    let text = match route.rule {
        Some(index) if !route.fallback => format!("Rule {} ({}) opens it in {space}.", index + 1, routing.rules[index].describe()),
        Some(index) => format!("Rule {} ({}) matches, but its Space was deleted, so it opens in {space}.", index + 1, routing.rules[index].describe()),
        None if route.fallback => format!("No rule matches. The default Space was deleted, so it opens in {space}."),
        None => format!("No rule matches, so it opens in {space}."),
    };
    Some((text, route.fallback))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn show(editor: &mut Editor, routing: &mut LinkRouting, window: &mut LinkWindow, prefs: &mut LinkPreferences, hotkey: &mut GlobalHotkey, shortcuts: &Shortcuts, lib: &Library, store: &Store, active_space: &str, system: &mut dyn System, ui: &mut Ui, pal: &Palette) {
    let ink = Ink::new(pal.panel, pal.accent);
    if editor.recording_hotkey && let Some(recorded) = take_recorded(ui) {
        record_hotkey(editor, hotkey, shortcuts, store, recorded);
    }
    if editor.owns_keyboard() && !egui::Popup::is_any_open(ui.ctx()) && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
        editor.cancel();
    }
    // Ask again when the user comes back, for example from System Settings.
    if editor.status.is_none() || ui.input(|input| input.events.iter().any(|event| matches!(event, Event::WindowFocused(true)))) {
        editor.status = Some(system.default_browser());
    }

    if let Some(notice) = &routing.notice {
        ui.label(style::text(notice, 12.0, style::weight::REGULAR).color(ink.danger));
    }
    failure_panel(editor, routing, store, ui, &ink);
    let editable = editor.failure.is_none();

    default_browser(editor, system, ui, &ink, pal);

    section(ui, pal, "Where links open");
    ui.label(style::text("Links from other apps open as a new tab, or in a Little Window when chosen below. The first rule that matches picks the Space, and with it the profile; other links use the default.", 12.0, style::weight::REGULAR).color(ink.secondary));
    switch(window, store, ui, &ink, pal);
    default_destination(editor, routing, store, lib, active_space, editable, ui, &ink, pal);

    section(ui, pal, "Rules");
    rules(editor, routing, store, lib, active_space, editable, ui, &ink, pal);

    section(ui, pal, "Test a link");
    test_link(editor, routing.saved(), lib, active_space, ui, &ink);

    ui.add_space(8.0);
    ui.label(style::text("Rules are saved on this device only and don't sync.", 12.0, style::weight::REGULAR).color(ink.secondary));

    section(ui, pal, "Little Windows");
    hotkey_row(editor, hotkey, store, ui, &ink, pal);
    archive_after(&mut prefs.archive_after, store, ui, &ink, pal);

    section(ui, pal, "Clicking links");
    switch(&mut prefs.option_click, store, ui, &ink, pal);
    switch(&mut prefs.shift_peek, store, ui, &ink, pal);
    if let Some((_, frames)) = &mut editor.focus {
        *frames = frames.saturating_sub(1);
        if *frames == 0 { editor.focus = None; }
    }
}

fn failure_panel(editor: &mut Editor, routing: &mut LinkRouting, store: &Store, ui: &mut Ui, ink: &Ink) {
    let Some(failure) = editor.failure.as_mut() else { return };
    let mut retry = false;
    let mut cancel = false;
    let response = ui.vertical(|ui| {
        ui.label(style::text(&failure.error, 12.0, style::weight::REGULAR).color(ink.danger));
        ui.horizontal_wrapped(|ui| {
            retry = Button::new("Retry Save").kind(Kind::Primary).show(ui, ink).clicked();
            cancel = Button::new("Cancel").kind(Kind::Plain).show(ui, ink).clicked();
        });
    }).response;
    if failure.reveal && !ui.is_sizing_pass() && ui.is_visible() {
        response.scroll_to_me_animation(None, egui::style::ScrollAnimation::none());
        failure.reveal = false;
    }
    if cancel {
        editor.failure = None;
    } else if retry {
        let candidate = failure.candidate.clone();
        editor.commit(routing, store, candidate);
        ui.ctx().request_repaint();
    }
}

fn default_browser(editor: &mut Editor, system: &mut dyn System, ui: &mut Ui, ink: &Ink, pal: &Palette) {
    section(ui, pal, "Default browser");
    let status = editor.status.clone().unwrap_or_else(|| DefaultBrowser::Unavailable(String::new()));
    let description = match &status {
        DefaultBrowser::Yes => "Links you open in other apps come to Crossbow.".to_string(),
        DefaultBrowser::No => "Links from other apps open in another browser.".to_string(),
        DefaultBrowser::Unavailable(reason) => reason.clone(),
    };
    row(ui, pal, "Crossbow is the default browser", &description, |ui| {
        let color = match status {
            DefaultBrowser::Yes => ink.success,
            DefaultBrowser::No => ink.text,
            DefaultBrowser::Unavailable(_) => ink.secondary,
        };
        ui.add(egui::Label::new(style::text(status.label(), 13.5, style::weight::SEMIBOLD).color(color)).halign(egui::Align::LEFT));
    });
    if cfg!(any(target_os = "macos", target_os = "windows", test)) {
        row(ui, pal, "Change the default browser", "Opens System Settings. Crossbow never changes the default itself, and checks again when you come back.", |ui| {
            if Button::new("Open System Settings").size(Size::Small).trailing(Icon::Expand).show(ui, ink).clicked() {
                editor.opened = Some(system.open_settings());
            }
        });
    } else {
        ui.label(style::text("To change the default browser, open your desktop's Default Applications settings and select Crossbow under Web Browser.", 12.0, style::weight::REGULAR).color(ink.secondary));
    }
    match &editor.opened {
        Some(Ok(())) => {
            ui.label(style::text("Choose Crossbow as the default web browser in System Settings. Opening it changed nothing.", 12.0, style::weight::REGULAR).color(ink.secondary));
        }
        Some(Err(error)) => {
            ui.label(style::text(error, 12.0, style::weight::REGULAR).color(ink.danger));
        }
        None => {}
    }
}

/// A switch in a row, with its focus ring.
pub(super) fn switch_control(ui: &mut Ui, on: &mut bool, title: &str, ink: &Ink) -> egui::Response {
    let response = toggle(ui, on, ink.accent);
    let value = *on;
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Switch, ui.is_enabled(), value, title));
    if response.has_focus() {
        ui.painter().rect_stroke(response.rect.expand(2.0), 13.0, egui::Stroke::new(2.0, ink.accent), egui::StrokeKind::Outside);
    }
    if response.gained_focus() { response.scroll_to_me_animation(None, egui::style::ScrollAnimation::none()); }
    response
}

/// Why saving `title` failed and what stays in use, with Retry Save and
/// Cancel. Returns which was clicked.
pub(super) fn save_failure(ui: &mut Ui, ink: &Ink, id: &str, title: &str, error: &str, kept: &str, reveal: &mut bool) -> (bool, bool) {
    let mut retry = false;
    let mut cancel = false;
    let response = ui.push_id((id, "failure"), |ui| {
        ui.label(style::text(format!("Could not save “{title}”: {error}. {kept}. Check disk space and data-folder permissions, then Retry Save or Cancel."), 12.0, style::weight::REGULAR).color(ink.danger));
        ui.horizontal_wrapped(|ui| {
            retry = Button::new("Retry Save").kind(Kind::Primary).show(ui, ink).clicked();
            cancel = Button::new("Cancel").kind(Kind::Plain).show(ui, ink).clicked();
        });
    }).response;
    if *reveal && !ui.is_sizing_pass() && ui.is_visible() {
        response.scroll_to_me_animation(None, egui::style::ScrollAnimation::none());
        *reveal = false;
    }
    (retry, cancel)
}

/// An on/off choice saved on this device only. A failed save keeps the
/// previous choice.
pub(super) fn switch<S: SwitchSpec>(switch: &mut DeviceSwitch<S>, store: &Store, ui: &mut Ui, ink: &Ink, pal: &Palette) {
    let mut enabled = switch.enabled();
    row(ui, pal, S::TITLE, S::DESCRIPTION, |ui| {
        switch_control(ui, &mut enabled, S::TITLE, ink);
    });
    if enabled != switch.enabled() {
        switch.choose(store, enabled);
        ui.ctx().request_repaint();
    }
    if let Some(notice) = &switch.notice {
        ui.label(style::text(notice, 12.0, style::weight::REGULAR).color(ink.danger));
    }
    if switch.failure.is_none() { return; }
    let key = switch.key().to_owned();
    let Some(failure) = switch.failure.as_mut() else { return };
    let kept = if failure.wanted { S::STILL_OFF } else { S::STILL_ON };
    let (retry, cancel) = save_failure(ui, ink, &key, S::TITLE, &failure.error, kept, &mut failure.reveal);
    if cancel {
        switch.failure = None;
    } else if retry {
        let wanted = failure.wanted;
        switch.choose(store, wanted);
        ui.ctx().request_repaint();
    }
}

/// A chord recorded in the Little Window shortcut field. Delete or
/// Backspace clears it; a refused chord is explained and changes nothing.
fn record_hotkey(editor: &mut Editor, hotkey: &mut GlobalHotkey, shortcuts: &Shortcuts, store: &Store, recorded: Option<Chord>) {
    editor.recording_hotkey = false;
    editor.focus_on(FocusTarget::Hotkey);
    // None is Escape: nothing changes.
    let Some(chord) = recorded else { return };
    let clear = chord.mods == Modifiers::NONE && matches!(chord.key, Key::Backspace | Key::Delete);
    if let Err(reason) = hotkey.set_chord(store, shortcuts, (!clear).then_some(chord)) {
        let kept = hotkey.chord().map_or("no shortcut".to_string(), Chord::label);
        editor.hotkey_refusal = Some(format!("{reason} {} keeps {kept}.", hotkey::TITLE));
    }
}

/// Settings → Links → Open a Little Window from anywhere: a switch and the
/// shortcut field, which records like the Shortcuts page.
fn hotkey_row(editor: &mut Editor, hotkey: &mut GlobalHotkey, store: &Store, ui: &mut Ui, ink: &Ink, pal: &Palette) {
    let mut on = hotkey.enabled();
    let recording = editor.recording_hotkey;
    let text = if recording { RECORDING.to_string() } else { hotkey.chord().map_or(UNSET.to_string(), Chord::label) };
    row(ui, pal, hotkey::TITLE, "Press the shortcut in any app to open a Little Window while Crossbow is running. This device only.", |ui| {
        switch_control(ui, &mut on, hotkey::TITLE, ink);
        let (rect, _) = ui.allocate_exact_size(vec2(CHIP_WIDTH, CONTROL_HEIGHT), Sense::hover());
        let field = chip(ui, ink, Id::unique(HOTKEY_FIELD), rect, &text, recording, true, format!("{} shortcut: {}", hotkey::TITLE, if text == UNSET { "none" } else { &text }));
        editor.take_focus(FocusTarget::Hotkey, &field);
        if recording {
            lock_focus(ui, &field);
            if field.clicked() { editor.recording_hotkey = false; }
        } else if field.clicked() {
            editor.recording_hotkey = true;
            editor.hotkey_refusal = None;
            field.request_focus();
        }
    });
    if on != hotkey.enabled() {
        hotkey.set_enabled(store, on);
        ui.ctx().request_repaint();
    }
    if editor.recording_hotkey {
        ui.label(style::text("Press the new shortcut. Delete clears it; Esc cancels.", 12.0, style::weight::REGULAR).color(ink.secondary));
    }
    for text in [editor.hotkey_refusal.as_ref(), hotkey.notice.as_ref()].into_iter().flatten() {
        ui.label(style::text(text, 12.0, style::weight::REGULAR).color(ink.danger));
    }
    let kept = match hotkey.chord().filter(|_| hotkey.enabled()) {
        Some(chord) => format!("{} still opens a Little Window", chord.label()),
        None => "The shortcut stays off".to_string(),
    };
    let Some(failure) = hotkey.failure.as_mut() else { return };
    let (retry, cancel) = save_failure(ui, ink, HOTKEY_FIELD, hotkey::TITLE, &failure.error, &kept, &mut failure.reveal);
    if cancel {
        hotkey.failure = None;
    } else if retry {
        hotkey.retry(store);
        ui.ctx().request_repaint();
    }
}

/// Settings → Links → Archive Little Windows after, kept apart from the
/// delay for tabs. A failed save keeps the previous delay.
fn archive_after(archive: &mut ArchiveAfter, store: &Store, ui: &mut Ui, ink: &Ink, pal: &Palette) {
    let mut hours = archive.hours();
    row(ui, pal, ARCHIVE_TITLE, "Windows you haven't used for this long close, and their pages go to the archive. Pages you use stay open until you quit. This device only.", |ui| {
        egui::ComboBox::from_id_salt("little-archive-after").selected_text(archive_label(hours)).width(150.0).show_ui(ui, |ui| {
            keep_popup_above_sheet(ui);
            for choice in ARCHIVE_CHOICES {
                ui.selectable_value(&mut hours, choice, archive_label(choice));
            }
        });
    });
    if hours != archive.hours() {
        archive.choose(store, hours);
        ui.ctx().request_repaint();
    }
    if let Some(notice) = &archive.notice {
        ui.label(style::text(notice, 12.0, style::weight::REGULAR).color(ink.danger));
    }
    let kept = match archive.hours() {
        0 => "Little Windows still stay open".to_string(),
        hours => format!("Little Windows still archive after {}", archive_label(hours)),
    };
    let Some(failure) = archive.failure.as_mut() else { return };
    let (retry, cancel) = save_failure(ui, ink, "little-archive-after", ARCHIVE_TITLE, &failure.error, &kept, &mut failure.reveal);
    if cancel {
        archive.failure = None;
    } else if retry {
        let wanted = failure.wanted;
        archive.choose(store, wanted);
        ui.ctx().request_repaint();
    }
}

fn default_destination(editor: &mut Editor, routing: &mut LinkRouting, store: &Store, lib: &Library, active_space: &str, editable: bool, ui: &mut Ui, ink: &Ink, pal: &Palette) {
    let current = routing.saved().default.clone();
    let mut chosen = current.clone();
    let description = format!("{MOST_RECENT} is the one in use, now {}.", space_label(lib, active_space));
    row(ui, pal, "Other links open in", &description, |ui| {
        ui.add_enabled_ui(editable, |ui| {
            egui::ComboBox::from_id_salt("link-default").selected_text(destination_label(lib, &current)).width(MENU_WIDTH).truncate().show_ui(ui, |ui| {
                keep_popup_above_sheet(ui);
                ui.selectable_value(&mut chosen, Destination::MostRecent, MOST_RECENT);
                for (id, _) in lib.spaces_ordered() {
                    if usable_space(lib, &id) {
                        let label = space_label(lib, &id);
                        ui.selectable_value(&mut chosen, Destination::Space(id), label);
                    }
                }
            });
        });
    });
    if matches!(&current, Destination::Space(space) if !usable_space(lib, space)) {
        ui.label(style::text("Needs attention: the chosen Space was deleted, so these links open in the most recent Space. Choose another Space.", 12.0, style::weight::REGULAR).color(ink.danger));
    }
    if chosen != current {
        let mut candidate = routing.saved().clone();
        candidate.default = chosen;
        editor.commit(routing, store, candidate);
        ui.ctx().request_repaint();
    }
}

#[allow(clippy::too_many_arguments)]
fn rules(editor: &mut Editor, routing: &mut LinkRouting, store: &Store, lib: &Library, active_space: &str, editable: bool, ui: &mut Ui, ink: &Ink, pal: &Palette) {
    let saved = routing.saved().clone();
    let adding = editor.draft.as_ref().is_some_and(|draft| draft.id.is_none());
    if saved.rules.is_empty() && !adding {
        ui.label(style::text("No rules yet. Add one to open a site's links in a chosen Space.", 12.5, style::weight::REGULAR).color(ink.secondary));
    }
    let count = saved.rules.len();
    for (index, rule) in saved.rules.iter().enumerate() {
        let mut action = None;
        ui.push_id(("link-rule", &rule.id), |ui| {
            // Buttons first, so a long rule truncates instead of pushing them out.
            ui.allocate_ui_with_layout(vec2(ui.available_width(), 26.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let button = |ui: &mut Ui, label: &str, enabled: bool| Button::new(label).kind(Kind::Plain).size(Size::Small).enabled(editable && enabled).show(ui, ink);
                let delete = button(ui, "Delete…", true);
                editor.take_focus(FocusTarget::Delete(rule.id.clone()), &delete);
                let edit = button(ui, "Edit", true);
                editor.take_focus(FocusTarget::Edit(rule.id.clone()), &edit);
                let down = button(ui, "Move Down", index + 1 < count);
                editor.take_focus(FocusTarget::Down(rule.id.clone()), &down);
                let up = button(ui, "Move Up", index > 0);
                editor.take_focus(FocusTarget::Up(rule.id.clone()), &up);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(style::text(format!("{}. {}", index + 1, rule.describe()), 13.5, style::weight::MEDIUM).color(ink.text)).truncate());
                });
                for (response, chosen) in [(up, RowAction::Up), (down, RowAction::Down), (edit, RowAction::Edit), (delete, RowAction::Delete)] {
                    if response.clicked() { action = Some(chosen); }
                }
            });
            if usable_space(lib, &rule.space) {
                ui.label(style::text(format!("Opens in {}", space_label(lib, &rule.space)), 12.0, style::weight::REGULAR).color(ink.secondary));
            } else {
                let fallback = saved.default_space(lib, active_space).map_or_else(|| "no Space".to_string(), |(space, _)| space_label(lib, &space));
                ui.label(style::text(format!("Needs attention: its Space was deleted, so matching links open in {fallback}. Edit the rule to choose another Space."), 12.0, style::weight::REGULAR).color(ink.danger));
            }
            if editor.draft.as_ref().is_some_and(|draft| draft.id.as_ref() == Some(&rule.id)) {
                draft_form(editor, routing, store, lib, ui, ink, pal);
            }
            if editor.deleting.as_ref() == Some(&rule.id) {
                ui.label(style::text(format!("Delete the rule {}? Links it matched will use the next matching rule or the default.", rule.describe()), 12.5, style::weight::REGULAR).color(ink.text));
                ui.horizontal_wrapped(|ui| {
                    let confirm = Button::new("Delete Rule").kind(Kind::Primary).size(Size::Small).show(ui, ink);
                    editor.take_focus(FocusTarget::Confirm, &confirm);
                    if confirm.clicked() { action = Some(RowAction::ConfirmDelete); }
                    if Button::new("Cancel").kind(Kind::Plain).size(Size::Small).show(ui, ink).clicked() { action = Some(RowAction::CancelDelete); }
                });
            }
        });
        separator(ui, pal);
        let id = rule.id.clone();
        let moved = |to: usize| {
            let mut candidate = saved.clone();
            candidate.rules.swap(index, to);
            candidate
        };
        match action {
            Some(RowAction::Up) => {
                editor.commit(routing, store, moved(index - 1));
                editor.focus_on(if index == 1 { FocusTarget::Down(id) } else { FocusTarget::Up(id) });
            }
            Some(RowAction::Down) => {
                editor.commit(routing, store, moved(index + 1));
                editor.focus_on(if index + 2 == count { FocusTarget::Up(id) } else { FocusTarget::Down(id) });
            }
            Some(RowAction::Edit) => editor.edit(rule),
            Some(RowAction::Delete) => {
                editor.draft = None;
                editor.deleting = Some(id);
                editor.focus_on(FocusTarget::Confirm);
            }
            Some(RowAction::ConfirmDelete) => {
                let mut candidate = saved.clone();
                candidate.rules.retain(|other| other.id != id);
                editor.deleting = None;
                editor.commit(routing, store, candidate);
                editor.focus_on(FocusTarget::Add);
            }
            Some(RowAction::CancelDelete) => editor.cancel(),
            None => {}
        }
        if action.is_some() { ui.ctx().request_repaint(); }
    }
    if adding {
        draft_form(editor, routing, store, lib, ui, ink, pal);
    } else {
        ui.add_space(4.0);
        let add = Button::new("Add Rule").size(Size::Small).icon(Icon::Plus).enabled(editable).show(ui, ink);
        editor.take_focus(FocusTarget::Add, &add);
        if add.clicked() {
            editor.add(lib, active_space);
            ui.ctx().request_repaint();
        }
    }
}

/// The add/edit form: match kind, text, destination Space, Save and Cancel.
fn draft_form(editor: &mut Editor, routing: &mut LinkRouting, store: &Store, lib: &Library, ui: &mut Ui, ink: &Ink, pal: &Palette) {
    let focus_value = editor.focus.as_ref().is_some_and(|(target, _)| *target == FocusTarget::Value);
    let Some(draft) = editor.draft.as_mut() else { return };
    let mut save = false;
    let mut cancel = false;
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        let mut kind = draft.kind;
        let kinds = Match::ALL.map(|kind| (kind, kind.label()));
        if segmented(ui, pal, &mut kind, &kinds) {
            draft.kind = kind;
            draft.error = None;
        }
        // The shared control draws no focus: show it and keep it in view for the keyboard.
        for (index, (option, label)) in kinds.into_iter().enumerate() {
            let Some(response) = ui.ctx().read_response(ui.scope_id().with(("seg", index, label))) else { continue };
            response.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, ui.is_enabled(), option == kind, label));
            if response.gained_focus() { response.scroll_to_me(None); }
            if response.has_focus() {
                ui.painter().rect_stroke(response.rect.expand(2.0), 8, egui::Stroke::new(2.0, pal.accent), egui::StrokeKind::Outside);
            }
        }
        let hint = match draft.kind {
            Match::UrlContains => "zoom.us/j/",
            Match::HostIs => "github.com",
        };
        let width = ui.available_width().clamp(160.0, 240.0);
        let field = ui.add(egui::TextEdit::singleline(&mut draft.value).id(Id::unique(VALUE_FIELD)).hint_text(hint).char_limit(MAX_VALUE).desired_width(width));
        if focus_value {
            field.request_focus();
            field.scroll_to_me(None);
        }
        if field.changed() { draft.error = None; }
        save = field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(style::text("Open in", 13.0, style::weight::REGULAR).color(ink.text));
        let selected = draft.space.as_deref().map_or_else(|| "Choose a Space".to_string(), |space| space_label(lib, space));
        egui::ComboBox::from_id_salt("link-rule-space").selected_text(selected).width(MENU_WIDTH).truncate().show_ui(ui, |ui| {
            keep_popup_above_sheet(ui);
            for (id, _) in lib.spaces_ordered() {
                if usable_space(lib, &id) {
                    let label = space_label(lib, &id);
                    if ui.selectable_label(draft.space.as_deref() == Some(id.as_str()), label).clicked() {
                        draft.space = Some(id);
                        draft.error = None;
                    }
                }
            }
        });
    });
    if let Some(error) = &draft.error {
        ui.label(style::text(error, 12.0, style::weight::REGULAR).color(ink.danger));
    }
    ui.horizontal_wrapped(|ui| {
        save |= Button::new("Save").kind(Kind::Primary).size(Size::Small).show(ui, ink).clicked();
        cancel = Button::new("Cancel").kind(Kind::Plain).size(Size::Small).show(ui, ink).clicked();
    });
    if focus_value { editor.focus = None; }
    if cancel {
        editor.cancel();
    } else if save {
        editor.save_draft(routing, store, lib);
        ui.ctx().request_repaint();
    }
}

fn test_link(editor: &mut Editor, routing: &Routing, lib: &Library, active_space: &str, ui: &mut Ui, ink: &Ink) {
    let width = ui.available_width().min(360.0);
    ui.add(egui::TextEdit::singleline(&mut editor.test).id(Id::unique(TEST_FIELD)).hint_text("Paste a link to see where it opens").desired_width(width));
    if let Some((text, warning)) = test_result(routing, lib, active_space, &editor.test) {
        ui.label(style::text(text, 12.5, style::weight::REGULAR).color(if warning { ink.danger } else { ink.text }));
    }
}

#[cfg(test)]
mod tests {
    use super::super::repair_tests::{PreferenceFrame, settings_test_frame};
    use super::*;
    use crate::app::little::LINK_TITLE;
    use crate::hotkey::DEFAULT_CHORD;
    use crate::hotkey::tests::{Fake, chord};
    use crate::link_prefs::{OptionClick, OtherAppLinks, ShiftPeek};
    use crossbow_core::model::Theme;
    use std::path::PathBuf;

    const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
    const PERSONAL: &str = "Personal · Personal";
    const WORK: &str = "Work · Personal";
    const CLIENT: &str = "Client · Client";

    /// The system as seen by the page. Nothing here touches the real default browser.
    struct FakeSystem {
        status: DefaultBrowser,
        queries: usize,
        opens: usize,
        fail_open: bool,
    }

    impl System for FakeSystem {
        fn default_browser(&mut self) -> DefaultBrowser {
            self.queries += 1;
            self.status.clone()
        }

        fn open_settings(&mut self) -> Result<(), String> {
            self.opens += 1;
            if self.fail_open { Err("Couldn't open system settings.".into()) } else { Ok(()) }
        }
    }

    /// Settings → Links on a disposable library under TMPDIR. Escape that the
    /// page leaves alone dismisses Settings, as `handle_shortcuts` does.
    struct Harness {
        dir: PathBuf,
        lib: Library,
        store: Store,
        routing: LinkRouting,
        window: LinkWindow,
        prefs: LinkPreferences,
        hotkey: GlobalHotkey,
        hotkeys: Fake,
        shortcuts: Shortcuts,
        editor: Editor,
        system: FakeSystem,
        personal: String,
        work: String,
        client: String,
        dismissed: bool,
        ctx: egui::Context,
        size: egui::Vec2,
        frame: u32,
    }

    impl Harness {
        /// Personal and Work use the Personal profile; Client uses Client.
        fn new(size: egui::Vec2) -> Self {
            let dir = std::env::temp_dir().join(format!("crossbow-links-ui-{}", crossbow_core::new_id()));
            std::fs::create_dir(&dir).unwrap();
            let mut lib = Library::open(Store::open(&dir.join("fixture.sqlite")).unwrap()).unwrap();
            lib.ensure_defaults().unwrap();
            let personal = lib.spaces_ordered()[0].0.clone();
            let profile = lib.spaces[&personal].profile_id.clone();
            let client_profile = lib.create_profile("Client").unwrap();
            let work = lib.create_space("Work", "W", Theme::preset(1), &profile).unwrap();
            let client = lib.create_space("Client", "C", Theme::preset(2), &client_profile).unwrap();
            Self::open(dir, lib, personal, work, client, size)
        }

        fn open(dir: PathBuf, lib: Library, personal: String, work: String, client: String, size: egui::Vec2) -> Self {
            let store = Store::open(&dir.join("fixture.sqlite")).unwrap();
            let routing = LinkRouting::load(&store);
            let window = LinkWindow::load(&store);
            let prefs = LinkPreferences::load(&store);
            let hotkeys = Fake::default();
            let hotkey = GlobalHotkey::load(&store, Box::new(hotkeys.clone()));
            let shortcuts = Shortcuts::load(&store);
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let system = FakeSystem { status: DefaultBrowser::No, queries: 0, opens: 0, fail_open: false };
            Self { dir, lib, store, routing, window, prefs, hotkey, hotkeys, shortcuts, editor: Editor::default(), system, personal, work, client, dismissed: false, ctx, size, frame: 0 }
        }

        /// Quit and reopen: a new library, store and page on the same file.
        fn reopen(mut self) -> Self {
            // The old harness closes without removing the folder the new one reads.
            let dir = std::mem::take(&mut self.dir);
            let lib = Library::open(Store::open(&dir.join("fixture.sqlite")).unwrap()).unwrap();
            Self::open(dir, lib, self.personal.clone(), self.work.clone(), self.client.clone(), self.size)
        }

        fn reopened_window(&self) -> bool {
            LinkWindow::load(&Store::open(&self.dir.join("fixture.sqlite")).unwrap()).enabled()
        }

        /// The Little Window switch, in view with its row's title.
        fn little_switch(&mut self) -> egui::Pos2 {
            self.switch_for(LINK_TITLE).center()
        }

        /// The switch of the row titled `label`, in view with its title.
        fn switch_for(&mut self, label: &str) -> Rect {
            // The row's only switch: right of the title, centred on title and description.
            let beside = |shown: &PreferenceFrame| -> Option<(Rect, Rect)> {
                let (_, title, _) = shown.text.iter().find(|(text, ..)| text == label)?;
                shown.switches.iter().find(|switch| switch.min.x > title.max.x && switch.top() > title.top() - 4.0 && switch.top() < title.top() + 60.0).map(|switch| (*switch, *title))
            };
            self.reveal(label, |shown| {
                let (_, title, clip) = shown.text.iter().find(|(text, ..)| text == label)?;
                // Only a switch wholly in view is listed: until then, ask for the row below the title.
                let row = beside(shown).map_or(title.union(title.translate(vec2(0.0, 50.0))), |(switch, title)| switch.union(title));
                Some((row, *clip))
            });
            beside(&self.settle()).expect("the switch is beside its title").0
        }

        /// Press Tab until a switch has focus.
        fn tab_to_switch(&mut self) {
            for _ in 0..200 {
                self.key(Key::Tab, Modifiers::NONE);
                if self.focused_rect().is_some_and(|rect| rect.size() == vec2(38.0, 22.0)) { return; }
            }
            panic!("the switch is not reachable with Tab at {:?}", self.size);
        }

        fn reopened(&self) -> Routing {
            LinkRouting::load(&Store::open(&self.dir.join("fixture.sqlite")).unwrap()).saved().clone()
        }

        /// URI mode=ro makes the real write fail without mocks.
        fn read_only(&mut self) {
            let path = self.dir.join("fixture.sqlite").to_string_lossy().replace('%', "%25").replace('?', "%3F").replace('#', "%23");
            self.store = Store::open(std::path::Path::new(&format!("file:{path}?mode=ro"))).unwrap();
        }

        fn writable(&mut self) {
            self.store = Store::open(&self.dir.join("fixture.sqlite")).unwrap();
        }

        fn seed(&mut self, rules: &[(Match, &str, &str)]) {
            let rules = rules.iter().map(|(kind, value, space)| Rule::new(crossbow_core::new_id(), *kind, value, space.to_string()).unwrap()).collect();
            let candidate = Routing { rules, ..self.routing.saved().clone() };
            self.routing.save(&self.store, candidate).unwrap();
        }

        fn frame(&mut self, events: Vec<Event>) -> PreferenceFrame {
            let personal = self.personal.clone();
            settings_test_frame(&self.ctx, Page::Links, self.size, 1.0, &mut self.frame, events, |ui, pal| {
                if !self.editor.owns_keyboard() && !egui::Popup::is_any_open(ui.ctx()) {
                    self.dismissed |= crate::app::take_shortcut(ui.ctx(), Modifiers::NONE, Key::Escape);
                }
                show(&mut self.editor, &mut self.routing, &mut self.window, &mut self.prefs, &mut self.hotkey, &self.shortcuts, &self.lib, &self.store, &personal, &mut self.system, ui, pal);
            })
        }

        /// Settle animations and check every visible text fits the sheet.
        fn settle(&mut self) -> PreferenceFrame {
            for _ in 0..6 { self.frame(vec![]); }
            let shown = self.frame(vec![]);
            let page = shown.text.iter().map(|(_, _, clip)| *clip)
                .max_by_key(|clip| shown.text.iter().filter(|(_, _, other)| other == clip).count());
            for (text, bounds, clip) in &shown.text {
                if Some(*clip) == page && bounds.intersects(*clip) {
                    assert!(bounds.min.x >= clip.min.x - 0.5 && bounds.max.x <= clip.max.x + 0.5, "{text:?} overflows the sheet at {:?}: {bounds:?} in {clip:?}", self.size);
                }
            }
            shown
        }

        /// Run frames until the wheel has finished smoothing and nothing moves,
        /// as it has by the time a person clicks or types.
        fn rest(&mut self) -> PreferenceFrame {
            let mut last = self.settle();
            for _ in 0..240 {
                let next = self.frame(vec![]);
                if next.text == last.text && !self.ctx.input(|input| input.is_scrolling()) { return next; }
                last = next;
            }
            panic!("scrolling never came to rest at {:?}", self.size);
        }

        fn reveal(&mut self, what: &str, pick: impl Fn(&PreferenceFrame) -> Option<(Rect, Rect)>) -> egui::Pos2 {
            let sheet = sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), self.size), 1.0);
            let wheel = sheet.right_bottom() - vec2(60.0, 40.0);
            // Offscreen labels are culled: search upward, then downward, in larger steps.
            // A step stays shorter than the page's height at 640×420, so no row is skipped.
            for attempt in 0..90 {
                let shown = self.rest();
                let delta = match pick(&shown) {
                    Some((bounds, clip)) if clip.expand(0.5).contains_rect(bounds) => return bounds.center(),
                    Some((bounds, clip)) => if bounds.center().y > clip.center().y { -100.0 } else { 100.0 },
                    None => if attempt < 18 { 200.0 } else { -200.0 },
                };
                self.frame(vec![
                    Event::PointerMoved(wheel),
                    Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, delta), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Modifiers::NONE },
                ]);
            }
            let shown: Vec<String> = self.settle().text.into_iter().map(|(text, ..)| text).collect();
            panic!("{what} never became visible at {:?}: {shown:?}", self.size);
        }

        fn visible(&mut self, label: &str) -> egui::Pos2 {
            self.reveal(label, |shown| shown.text.iter().find(|(text, ..)| text == label).map(|(_, bounds, clip)| (*bounds, *clip)))
        }

        fn shows(&mut self, label: &str) -> bool {
            self.settle().text.iter().any(|(text, ..)| text == label)
        }

        fn shown_starting(&mut self, prefix: &str) -> Option<String> {
            self.settle().text.into_iter().map(|(text, ..)| text).find(|text| text.starts_with(prefix))
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

        /// A button on the same line as `rule`.
        fn rule_button(&mut self, rule: &str, button: &str) -> egui::Pos2 {
            self.reveal(&format!("{button} for {rule}"), |shown| {
                let (_, row, _) = shown.text.iter().find(|(text, ..)| text == rule)?;
                shown.text.iter().find(|(text, bounds, _)| text == button && (bounds.center().y - row.center().y).abs() < 8.0).map(|(_, bounds, clip)| (*bounds, *clip))
            })
        }

        /// Open a menu showing `current` and pick `choice` in its popup.
        fn choose(&mut self, current: &str, choice: &str) {
            self.click(current);
            let pos = self.reveal(choice, |shown| shown.text.iter().rfind(|(text, ..)| text == choice).map(|(_, bounds, clip)| (*bounds, *clip)));
            self.click_at(pos);
        }

        fn key(&mut self, key: Key, modifiers: Modifiers) {
            for pressed in [true, false] {
                self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }]);
            }
            self.settle();
        }

        fn type_text(&mut self, text: &str) {
            self.frame(vec![Event::Text(text.into())]);
            self.settle();
        }

        /// Replace the focused field's text.
        fn retype(&mut self, text: &str) {
            self.key(Key::A, Modifiers::COMMAND);
            self.key(Key::Backspace, Modifiers::NONE);
            self.type_text(text);
        }

        fn editing(&self, field: &str) -> bool {
            self.ctx.memory(|memory| memory.has_focus(Id::unique(field)))
        }

        fn focused_rect(&self) -> Option<Rect> {
            self.ctx.memory(|memory| memory.focused()).and_then(|id| self.ctx.read_response(id)).map(|response| response.rect)
        }

        /// Press Tab until the control showing `label` has focus.
        fn tab_to(&mut self, label: &str) {
            for _ in 0..200 {
                let shown = self.settle();
                let focused = self.focused_rect();
                if shown.text.iter().any(|(text, bounds, _)| text == label && focused.is_some_and(|f| f.contains_rect(*bounds))) {
                    assert!(self.has_focus_on(label), "focused {label} must be scrolled into view at {:?}", self.size);
                    return;
                }
                self.key(Key::Tab, Modifiers::NONE);
            }
            panic!("{label} is not reachable with Tab at {:?}", self.size);
        }

        /// Focus is on the control showing `label`, in view once scrolling settles.
        fn has_focus_on(&mut self, label: &str) -> bool {
            (0..4).any(|_| {
                let shown = self.settle();
                let focused = self.focused_rect();
                shown.text.iter().any(|(text, bounds, clip)| text == label && focused.is_some_and(|f| f.contains(bounds.center())) && clip.expand(0.5).contains_rect(*bounds))
            })
        }

        /// The focused control is on the same line as `rule` and shows `label`.
        fn focus_in_row(&mut self, rule: &str, label: &str) -> bool {
            (0..4).any(|_| {
                let shown = self.settle();
                let Some(focused) = self.focused_rect() else { return false };
                let Some((_, row, _)) = shown.text.iter().find(|(text, ..)| text == rule) else { return false };
                shown.text.iter().any(|(text, bounds, _)| text == label && focused.contains(bounds.center()) && (bounds.center().y - row.center().y).abs() < 8.0)
            })
        }

        fn test_link(&mut self, link: &str) -> String {
            let pos = self.reveal("Test a link field", |shown| {
                let label = "TEST A LINK";
                let (_, header, _) = shown.text.iter().find(|(text, ..)| text == label)?;
                shown.text.iter().filter(|(_, bounds, _)| bounds.top() > header.bottom()).min_by(|a, b| a.1.top().total_cmp(&b.1.top())).map(|(_, bounds, clip)| (*bounds, *clip))
            });
            self.click_at(pos);
            assert!(self.editing(TEST_FIELD));
            self.retype(link);
            let result = test_result(self.routing.saved(), &self.lib, &self.personal, link).map(|(text, _)| text).unwrap_or_default();
            self.visible(&result);
            result
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn rule_ids(routing: &Routing) -> Vec<(Match, String, String)> {
        routing.rules.iter().map(|rule| (rule.kind, rule.value.clone(), rule.space.clone())).collect()
    }

    #[test]
    fn the_page_reads_the_default_browser_on_entry_and_return_and_fits_both_sizes() {
        for size in SIZES {
            let mut h = Harness::new(size);
            for label in ["DEFAULT BROWSER", "Crossbow is the default browser", "No", "Links from other apps open in another browser.",
                "Open System Settings", "Other links open in", MOST_RECENT, "No rules yet. Add one to open a site's links in a chosen Space.",
                "Add Rule", "TEST A LINK", "Rules are saved on this device only and don't sync."] {
                h.visible(label);
            }
            assert_eq!(h.system.queries, 1, "read once on entry, not every frame");
            h.frame(vec![Event::WindowFocused(true)]);
            assert_eq!(h.system.queries, 2, "read again when the window comes back");

            h.system.status = DefaultBrowser::Yes;
            h.editor.leave();
            h.visible("Yes");
            h.visible("Links you open in other apps come to Crossbow.");
            assert_eq!(h.system.queries, 3, "read again when the page is shown again");
            h.system.status = DefaultBrowser::Unavailable("Crossbow isn't running as an app bundle.".into());
            h.editor.leave();
            h.visible("Unavailable");
            h.visible("Crossbow isn't running as an app bundle.");

            // Opening System Settings changes nothing and claims nothing.
            h.click("Open System Settings");
            assert_eq!(h.system.opens, 1);
            h.visible("Choose Crossbow as the default web browser in System Settings. Opening it changed nothing.");
            h.visible("Unavailable");
            h.system.fail_open = true;
            h.click("Open System Settings");
            h.visible("Couldn't open system settings.");
            assert!(h.reopened() == Routing::default() && !h.dismissed);
        }
    }

    #[test]
    fn adding_rules_by_pointer_and_keyboard_saves_them_and_they_survive_reopening() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.click("Add Rule");
            assert!(h.editing(VALUE_FIELD), "the new rule's field takes focus");
            h.visible("Host is");
            h.type_text("zoom.us/j/");
            h.choose(PERSONAL, CLIENT);
            h.click("Save");
            h.visible("1. URL contains “zoom.us/j/”");
            h.visible(&format!("Opens in {CLIENT}"));
            assert_eq!(rule_ids(&h.reopened()), [(Match::UrlContains, "zoom.us/j/".into(), h.client.clone())]);
            assert!(h.focus_in_row("1. URL contains “zoom.us/j/”", "Edit"), "focus moves to the saved rule");

            // Keyboard only: Add Rule, Host is, type, Enter saves to the Space in use.
            h.tab_to("Add Rule");
            h.key(Key::Enter, Modifiers::NONE);
            assert!(h.editing(VALUE_FIELD));
            h.tab_to("Host is");
            h.key(Key::Enter, Modifiers::NONE);
            h.click_at(h.ctx.read_response(Id::unique(VALUE_FIELD)).unwrap().rect.center());
            h.type_text("GitHub.com");
            h.key(Key::Enter, Modifiers::NONE);
            h.visible("2. Host is “github.com”");
            assert_eq!(rule_ids(&h.reopened())[1], (Match::HostIs, "github.com".into(), h.personal.clone()));
            assert!(!h.dismissed);

            let mut h = h.reopen();
            h.visible("1. URL contains “zoom.us/j/”");
            h.visible("2. Host is “github.com”");
            h.visible(&format!("Opens in {CLIENT}"));
            assert_eq!(h.routing.saved().rules.len(), 2);
        }
    }

    #[test]
    fn invalid_rules_are_explained_and_cancel_or_escape_save_nothing() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.click("Add Rule");
            h.click("Save");
            h.visible("Enter the text a link contains.");
            h.click("Host is");
            h.click_at(h.ctx.read_response(Id::unique(VALUE_FIELD)).unwrap().rect.center());
            h.type_text("github.com/rust-lang");
            h.click("Save");
            h.visible("Host is takes a site name only, such as example.com. Use URL contains to match part of a link.");
            assert!(h.editing(VALUE_FIELD), "the field keeps focus for a fix");
            assert_eq!(h.reopened(), Routing::default(), "nothing was saved");
            h.click("Cancel");
            assert!(!h.shows("Save"));
            assert!(h.has_focus_on("Add Rule"));

            // Escape cancels the draft, not Settings; the next Escape closes Settings.
            h.click("Add Rule");
            h.type_text("example");
            h.key(Key::Escape, Modifiers::NONE);
            assert!(!h.shows("Save") && !h.dismissed);
            assert!(h.has_focus_on("Add Rule"));
            assert_eq!(h.reopened(), Routing::default());
            h.key(Key::Escape, Modifiers::NONE);
            assert!(h.dismissed);
        }
    }

    #[test]
    fn editing_and_deleting_ask_first_and_cancel_changes_nothing() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let (work, client) = (h.work.clone(), h.client.clone());
            h.seed(&[(Match::UrlContains, "docs", &work), (Match::HostIs, "github.com", &client)]);
            let first = "1. URL contains “docs”";
            let pos = h.rule_button(first, "Edit");
            h.click_at(pos);
            assert!(h.editing(VALUE_FIELD));
            h.retype("wiki");
            h.click("Cancel");
            h.visible(first);
            assert!(h.focus_in_row(first, "Edit"));
            assert_eq!(rule_ids(&h.reopened())[0], (Match::UrlContains, "docs".into(), work.clone()));

            let pos = h.rule_button(first, "Edit");
            h.click_at(pos);
            h.retype("wiki");
            h.choose(WORK, CLIENT);
            h.click("Save");
            let edited = "1. URL contains “wiki”";
            h.visible(edited);
            assert_eq!(rule_ids(&h.reopened()), [(Match::UrlContains, "wiki".into(), client.clone()), (Match::HostIs, "github.com".into(), client.clone())]);

            let question = "Delete the rule URL contains “wiki”? Links it matched will use the next matching rule or the default.";
            let pos = h.rule_button(edited, "Delete…");
            h.click_at(pos);
            h.visible(question);
            assert!(h.has_focus_on("Delete Rule"), "the question takes focus");
            h.click("Cancel");
            assert!(!h.shows(question));
            assert!(h.focus_in_row(edited, "Delete…"));
            let pos = h.rule_button(edited, "Delete…");
            h.click_at(pos);
            h.key(Key::Escape, Modifiers::NONE);
            assert!(!h.shows(question) && !h.dismissed, "Escape cancels the question, not Settings");
            assert_eq!(h.reopened().rules.len(), 2);

            let pos = h.rule_button(edited, "Delete…");
            h.click_at(pos);
            h.key(Key::Enter, Modifiers::NONE);
            assert!(!h.shows(edited));
            h.visible("1. Host is “github.com”");
            assert_eq!(rule_ids(&h.reopened()), [(Match::HostIs, "github.com".into(), client.clone())]);
        }
    }

    #[test]
    fn rules_reorder_by_keyboard_and_the_first_match_decides() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let (work, client) = (h.work.clone(), h.client.clone());
            h.seed(&[(Match::UrlContains, "github", &work), (Match::HostIs, "github.com", &client)]);
            assert_eq!(h.test_link("https://github.com/rust-lang"), format!("Rule 1 (URL contains “github”) opens it in {WORK}."));

            h.tab_to("Move Down");
            h.key(Key::Enter, Modifiers::NONE);
            assert_eq!(rule_ids(&h.reopened()), [(Match::HostIs, "github.com".into(), client.clone()), (Match::UrlContains, "github".into(), work.clone())]);
            h.visible("1. Host is “github.com”");
            h.visible("2. URL contains “github”");
            assert!(h.focus_in_row("2. URL contains “github”", "Move Up"), "focus stays with the moved rule");
            h.visible(&format!("Rule 1 (Host is “github.com”) opens it in {CLIENT}."));
            assert!(!h.dismissed);

            h.key(Key::Enter, Modifiers::NONE);
            assert_eq!(rule_ids(&h.reopened())[0], (Match::UrlContains, "github".into(), work.clone()));
            assert!(h.focus_in_row("1. URL contains “github”", "Move Down"));
            h.key(Key::Space, Modifiers::NONE);
            assert_eq!(rule_ids(&h.reopened())[0], (Match::HostIs, "github.com".into(), client.clone()), "Space presses the focused button too");
        }
    }

    #[test]
    fn the_default_destination_and_deleted_spaces_are_explicit() {
        for size in SIZES {
            let mut h = Harness::new(size);
            assert_eq!(h.test_link("example.com"), format!("No rule matches, so it opens in {PERSONAL}."));
            h.choose(MOST_RECENT, WORK);
            assert_eq!(h.reopened().default, Destination::Space(h.work.clone()));
            h.visible(&format!("No rule matches, so it opens in {WORK}."));

            let client = h.client.clone();
            h.seed(&[(Match::HostIs, "zoom.us", &client)]);
            h.lib.delete_space(&client).unwrap();
            let rule = "1. Host is “zoom.us”";
            h.visible(rule);
            h.visible(&format!("Needs attention: its Space was deleted, so matching links open in {WORK}. Edit the rule to choose another Space."));
            assert_eq!(h.test_link("https://zoom.us/j/1"), format!("Rule 1 (Host is “zoom.us”) matches, but its Space was deleted, so it opens in {WORK}."));

            let work = h.work.clone();
            h.lib.delete_space(&work).unwrap();
            h.visible("Needs attention: the chosen Space was deleted, so these links open in the most recent Space. Choose another Space.");
            assert_eq!(h.test_link("example.com"), format!("No rule matches. The default Space was deleted, so it opens in {PERSONAL}."));

            // Repair the default, then the rule: a deleted Space cannot be saved; another can.
            h.choose("a deleted Space", MOST_RECENT);
            assert!(!h.shows("Needs attention: the chosen Space was deleted, so these links open in the most recent Space. Choose another Space."));
            assert_eq!(h.reopened().default, Destination::MostRecent);
            let pos = h.rule_button(rule, "Edit");
            h.click_at(pos);
            h.click("Save");
            h.visible("That Space no longer exists. Choose another Space.");
            h.choose("a deleted Space", PERSONAL);
            h.click("Save");
            h.visible(&format!("Opens in {PERSONAL}"));
            assert!(!h.shows("That Space no longer exists. Choose another Space."));
            assert_eq!(h.reopened(), Routing { rules: vec![Rule { id: h.routing.saved().rules[0].id.clone(), kind: Match::HostIs, value: "zoom.us".into(), space: h.personal.clone() }], default: Destination::MostRecent });
        }
    }

    #[test]
    fn a_failed_save_keeps_the_previous_rules_and_retry_or_cancel_recovers() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let work = h.work.clone();
            h.seed(&[(Match::UrlContains, "docs", &work)]);
            let before = h.reopened();
            h.read_only();
            h.click("Add Rule");
            h.type_text("zoom.us");
            h.click("Save");
            let error = h.shown_starting("Could not save link rules:").expect("the failure is explained");
            assert!(error.ends_with("The previous rules are still in use. Check disk space and data-folder permissions, then Retry Save or Cancel."));
            assert!(!h.shows("2. URL contains “zoom.us”"), "the list shows the rules in use");
            assert_eq!(h.reopened(), before);
            h.click("Retry Save");
            assert!(h.shows("Retry Save"), "still failing");
            h.click("Cancel");
            assert!(!h.shows("Retry Save"));
            assert_eq!(h.reopened(), before);

            h.choose(MOST_RECENT, CLIENT);
            assert!(h.shows("Retry Save"));
            assert_eq!(h.reopened(), before, "the default stays too");
            h.writable();
            h.click("Retry Save");
            assert!(!h.shows("Retry Save"));
            assert_eq!(h.reopened().default, Destination::Space(h.client.clone()));
            assert_eq!(h.reopened().rules, before.rules);
            h.visible(CLIENT);
        }
    }

    #[test]
    fn test_a_link_explains_refused_and_typed_links() {
        let mut h = Harness::new(SIZES[0]);
        let work = h.work.clone();
        h.seed(&[(Match::HostIs, "github.com", &work)]);
        assert_eq!(h.test_link("mailto:someone@example.com"), "Crossbow doesn't open mailto: links. It opens http, https and HTML file links.");
        assert_eq!(h.test_link("file:///tmp/report.pdf"), "Only HTML files on this computer open in Crossbow.");
        assert_eq!(h.test_link("crossbow://"), "This link brings Crossbow to the front without opening a tab.");
        assert_eq!(h.test_link("github.com/rust-lang"), format!("Rule 1 (Host is “github.com”) opens it in {WORK}."));
        assert_eq!(h.test_link("not a link"), "This isn't a link Crossbow can read.");
        assert!(!h.dismissed && h.reopened().rules.len() == 1, "testing changes nothing");
    }

    const LITTLE_INTRO: &str = "Links from other apps open as a new tab, or in a Little Window when chosen below. The first rule that matches picks the Space, and with it the profile; other links use the default.";

    #[test]
    fn links_can_open_in_a_little_window_and_the_choice_is_read_back_after_reopening() {
        for size in SIZES {
            let mut h = Harness::new(size);
            assert!(!h.window.enabled() && !h.reopened_window(), "off by default");
            h.visible(LITTLE_INTRO);
            let switch = h.little_switch();
            h.click_at(switch);
            assert!(h.window.enabled() && h.reopened_window(), "on and saved at {size:?}");
            assert!(h.shown_starting("Could not save").is_none());

            let mut h = h.reopen();
            assert!(h.window.enabled(), "read back after reopening at {size:?}");
            h.tab_to_switch();
            h.key(Key::Space, Modifiers::NONE);
            assert!(!h.window.enabled() && !h.reopened_window(), "Space turns it off at {size:?}");
            let h = h.reopen();
            assert!(!h.window.enabled(), "off survives reopening too");
            assert!(h.reopened().rules.is_empty(), "routing rules are untouched");
        }
    }

    #[test]
    fn a_failed_little_window_save_keeps_tabs_and_offers_retry_or_cancel() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.read_only();
            let switch = h.little_switch();
            h.click_at(switch);
            let error = h.shown_starting(&format!("Could not save “{LINK_TITLE}”:")).expect("the failure is explained");
            assert!(error.ends_with("Links from other apps still open as tabs. Check disk space and data-folder permissions, then Retry Save or Cancel."), "{error}");
            assert!(!h.window.enabled() && !h.reopened_window(), "the previous choice stays in use at {size:?}");
            h.click("Retry Save");
            assert!(h.shown_starting("Could not save").is_some(), "still failing");
            h.click("Cancel");
            assert!(h.shown_starting("Could not save").is_none() && !h.window.enabled(), "Cancel keeps tabs");

            let switch = h.little_switch();
            h.click_at(switch);
            assert!(h.shown_starting("Could not save").is_some());
            h.writable();
            h.click("Retry Save");
            assert!(h.shown_starting("Could not save").is_none());
            assert!(h.window.enabled() && h.reopened_window(), "Retry saves it at {size:?}");
        }
    }

    #[test]
    fn an_unreadable_little_window_choice_is_explained_and_replaced_by_the_next_save() {
        let mut h = Harness::new(SIZES[0]);
        h.store.kv_set(crate::app::little::LINK_KEY, "sometimes").unwrap();
        h.window = LinkWindow::load(&h.store);
        assert!(!h.window.enabled(), "links keep opening as tabs");
        let notice = "The saved choice for links from other apps could not be read, so they open as tabs. Changing it saves over it.";
        h.visible(notice);
        let switch = h.little_switch();
        h.click_at(switch);
        assert!(!h.shows(notice) && h.window.enabled() && h.reopened_window());
    }

    const HOTKEY_DESCRIPTION: &str = "Press the shortcut in any app to open a Little Window while Crossbow is running. This device only.";
    const ARCHIVE_DESCRIPTION: &str = "Windows you haven't used for this long close, and their pages go to the archive. Pages you use stay open until you quit. This device only.";
    const RECORD_HINT: &str = "Press the new shortcut. Delete clears it; Esc cancels.";

    impl Harness {
        /// The row titled `title` is in view, and its title, description and
        /// controls don't overlap each other or anything else drawn.
        fn assert_row_clear(&mut self, title: &str, description: &str) {
            self.visible(description);
            self.visible(title);
            let shown = self.settle();
            let find = |label: &str| shown.text.iter().find(|(text, ..)| text == label).map(|(_, bounds, clip)| (*bounds, *clip)).unwrap_or_else(|| panic!("{label} is drawn at {:?}", self.size));
            for label in [title, description] {
                let (bounds, clip) = find(label);
                assert!(clip.expand(0.5).contains_rect(bounds), "{label} is not clipped at {:?}", self.size);
                for (other, rect, _) in &shown.text {
                    assert!(other == label || !rect.intersects(bounds) || rect.height() < 1.0, "{label} overlaps {other:?} at {:?}", self.size);
                }
                assert!(shown.switches.iter().all(|switch| !switch.intersects(bounds)), "{label} overlaps a switch at {:?}", self.size);
            }
        }

        fn reopened_prefs(&self) -> LinkPreferences {
            LinkPreferences::load(&Store::open(&self.dir.join("fixture.sqlite")).unwrap())
        }

        /// The hotkey as the next launch reads it: whether it is taken, and its chord.
        fn reopened_hotkey(&self) -> (bool, Option<Chord>) {
            let hotkey = GlobalHotkey::load(&Store::open(&self.dir.join("fixture.sqlite")).unwrap(), Box::new(Fake::default()));
            (hotkey.enabled(), hotkey.chord())
        }

        /// The shortcut field, in view.
        fn hotkey_field(&mut self) -> egui::Pos2 {
            let text = if self.editor.recording_hotkey { RECORDING.to_string() } else { self.hotkey.chord().map_or(UNSET.to_string(), Chord::label) };
            self.reveal(&text, |shown| {
                let (_, title, _) = shown.text.iter().find(|(label, ..)| label == hotkey::TITLE)?;
                shown.text.iter().find(|(label, bounds, _)| *label == text && bounds.min.x > title.max.x && (bounds.center().y - title.center().y).abs() < 40.0).map(|(_, bounds, clip)| (*bounds, *clip))
            })
        }

        fn press(&mut self, key: Key, command: bool, shift: bool, alt: bool, ctrl: bool) {
            self.key(key, Modifiers { alt, ctrl, shift, mac_cmd: command, command });
        }
    }

    #[test]
    fn little_window_and_click_rows_fit_both_sizes_and_are_reached_by_scrolling() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.assert_row_clear(hotkey::TITLE, HOTKEY_DESCRIPTION);
            h.assert_row_clear(ARCHIVE_TITLE, ARCHIVE_DESCRIPTION);
            h.assert_row_clear(OptionClick::TITLE, OptionClick::DESCRIPTION);
            h.assert_row_clear(ShiftPeek::TITLE, ShiftPeek::DESCRIPTION);
            h.assert_row_clear(LINK_TITLE, OtherAppLinks::DESCRIPTION);
            // Back down to the last row.
            h.visible(ShiftPeek::TITLE);
            let field = h.hotkey_field();
            let switch = h.switch_for(hotkey::TITLE);
            assert!(field.x < switch.left() && !switch.contains(field), "the field sits left of the switch at {size:?}");
        }
    }

    #[test]
    fn the_hotkey_switch_takes_the_chord_and_the_field_records_changes_and_clears_it() {
        for size in SIZES {
            let mut h = Harness::new(size);
            assert!(!h.hotkey.enabled() && h.hotkeys.taken().is_none(), "off by default");
            let switch = h.switch_for(hotkey::TITLE).center();
            h.click_at(switch);
            assert!(h.hotkey.enabled() && h.hotkeys.taken() == Some(DEFAULT_CHORD), "on takes ⌃⌥⌘N at {size:?}");
            assert_eq!(h.reopened_hotkey(), (true, Some(DEFAULT_CHORD)));

            // Escape cancels recording without closing Settings.
            let field = h.hotkey_field();
            h.click_at(field);
            assert!(h.editor.owns_keyboard() && h.shows(RECORDING));
            h.visible(RECORD_HINT);
            h.key(Key::Escape, Modifiers::NONE);
            assert!(!h.editor.owns_keyboard() && !h.dismissed && h.hotkey.chord() == Some(DEFAULT_CHORD));
            assert!(h.has_focus_on(&DEFAULT_CHORD.label()), "focus returns to the field at {size:?}");

            // Keyboard: Enter starts recording; a new chord moves the hotkey.
            h.key(Key::Enter, Modifiers::NONE);
            assert!(h.editor.recording_hotkey, "Enter on the field records at {size:?}");
            h.hotkeys.log();
            h.press(Key::M, true, false, true, true);
            assert_eq!(h.hotkey.chord(), Some(chord(Key::M, true, false, true, true)));
            assert_eq!(h.hotkeys.log(), ["unregister ⌃⌥⌘N", "register ⌃⌥⌘M"]);
            assert!(h.shows("⌃⌥⌘M") && h.hotkey.enabled());
            assert_eq!(h.reopened_hotkey(), (true, Some(chord(Key::M, true, false, true, true))), "read back after reopening");

            // A browser shortcut's chord is refused and explained.
            let field = h.hotkey_field();
            h.click_at(field);
            h.press(Key::T, true, false, false, false);
            assert!(h.shows("⌘T is the shortcut for New Tab. Choose another, or change that one in Settings → Shortcuts first. Open a Little Window from anywhere keeps ⌃⌥⌘M."));
            assert!(h.hotkeys.log().is_empty() && h.hotkey.chord() == Some(chord(Key::M, true, false, true, true)));

            // Delete clears it, which turns the hotkey off.
            let field = h.hotkey_field();
            h.click_at(field);
            assert!(!h.shows("⌘T is the shortcut for New Tab. Choose another, or change that one in Settings → Shortcuts first. Open a Little Window from anywhere keeps ⌃⌥⌘M."), "recording again clears the explanation");
            h.key(Key::Delete, Modifiers::NONE);
            assert!(h.hotkey.chord().is_none() && !h.hotkey.enabled() && h.hotkeys.taken().is_none());
            assert!(h.shows(UNSET));
            assert_eq!(h.reopened_hotkey(), (false, None));
            let switch = h.switch_for(hotkey::TITLE).center();
            h.click_at(switch);
            assert!(!h.hotkey.enabled() && h.shows("Record a shortcut first."), "no chord, nothing to turn on at {size:?}");
        }
    }

    #[test]
    fn a_hotkey_another_app_owns_or_a_failed_save_keeps_it_off_and_explains_why() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.hotkeys.owned_elsewhere(DEFAULT_CHORD);
            let switch = h.switch_for(hotkey::TITLE).center();
            h.click_at(switch);
            assert!(!h.hotkey.enabled() && h.hotkeys.taken().is_none());
            h.visible("⌃⌥⌘N can't be used: another app already uses it. The shortcut stays off.");
            assert_eq!(h.reopened_hotkey(), (false, Some(DEFAULT_CHORD)), "nothing was saved");

            let mut h = Harness::new(size);
            h.read_only();
            let switch = h.switch_for(hotkey::TITLE).center();
            h.click_at(switch);
            let error = h.shown_starting(&format!("Could not save “{}”:", hotkey::TITLE)).expect("the failure is explained");
            assert!(error.ends_with("The shortcut stays off. Check disk space and data-folder permissions, then Retry Save or Cancel."), "{error}");
            assert!(!h.hotkey.enabled() && h.hotkeys.taken().is_none(), "the chord is given back at {size:?}");
            h.click("Retry Save");
            assert!(h.shown_starting("Could not save").is_some(), "still failing");
            h.click("Cancel");
            assert!(h.shown_starting("Could not save").is_none() && !h.hotkey.enabled());
            let switch = h.switch_for(hotkey::TITLE).center();
            h.click_at(switch);
            h.writable();
            h.click("Retry Save");
            assert!(h.shown_starting("Could not save").is_none() && h.hotkey.enabled() && h.hotkeys.taken() == Some(DEFAULT_CHORD));
            assert_eq!(h.reopened_hotkey(), (true, Some(DEFAULT_CHORD)), "Retry saves it at {size:?}");
        }
    }

    #[test]
    fn click_choices_are_on_by_default_saved_read_back_and_kept_when_a_save_fails() {
        fn enabled(prefs: &LinkPreferences, title: &str) -> bool {
            if title == OptionClick::TITLE { prefs.option_click.enabled() } else { prefs.shift_peek.enabled() }
        }
        for size in SIZES {
            for title in [OptionClick::TITLE, ShiftPeek::TITLE] {
                let mut h = Harness::new(size);
                assert!(enabled(&h.prefs, title) && enabled(&h.reopened_prefs(), title), "{title} is on by default");
                let switch = h.switch_for(title).center();
                h.click_at(switch);
                assert!(!enabled(&h.prefs, title) && !enabled(&h.reopened_prefs(), title), "off and saved at {size:?}");
                let mut h = h.reopen();
                assert!(!enabled(&h.prefs, title), "read back after reopening");

                h.read_only();
                let switch = h.switch_for(title).center();
                h.click_at(switch);
                let error = h.shown_starting(&format!("Could not save “{title}”:")).expect("the failure is explained");
                assert!(error.ends_with(&format!("{}. Check disk space and data-folder permissions, then Retry Save or Cancel.", if title == OptionClick::TITLE { OptionClick::STILL_OFF } else { ShiftPeek::STILL_OFF })), "{error}");
                assert!(!enabled(&h.prefs, title), "the previous choice stays in use");
                h.click("Cancel");
                assert!(h.shown_starting("Could not save").is_none() && !enabled(&h.prefs, title));
                let switch = h.switch_for(title).center();
                h.click_at(switch);
                h.writable();
                h.click("Retry Save");
                assert!(enabled(&h.prefs, title) && enabled(&h.reopened_prefs(), title), "Retry saves it at {size:?}");
            }
        }
        let mut h = Harness::new(SIZES[0]);
        h.store.kv_set(OptionClick::KEY, "sometimes").unwrap();
        h.prefs = LinkPreferences::load(&h.store);
        assert!(h.prefs.option_click.enabled(), "an unreadable choice keeps the default");
        h.visible(OptionClick::UNREADABLE);
        let switch = h.switch_for(OptionClick::TITLE).center();
        h.click_at(switch);
        assert!(!h.shows(OptionClick::UNREADABLE) && !h.prefs.option_click.enabled() && !h.reopened_prefs().option_click.enabled());
    }

    #[test]
    fn the_little_window_archive_delay_is_chosen_saved_read_back_and_kept_when_a_save_fails() {
        for size in SIZES {
            let mut h = Harness::new(size);
            assert_eq!((h.prefs.archive_after.hours(), h.reopened_prefs().archive_after.hours()), (12, 12), "12 hours by default");
            h.visible(ARCHIVE_TITLE);
            h.choose("12 hours", "Never");
            assert_eq!((h.prefs.archive_after.hours(), h.reopened_prefs().archive_after.hours()), (0, 0), "Never at {size:?}");
            let mut h = h.reopen();
            assert!(h.prefs.archive_after.after_ms().is_none(), "read back after reopening");
            h.visible(ARCHIVE_TITLE);
            h.choose("Never", "1 hour");
            assert_eq!(h.reopened_prefs().archive_after.after_ms(), Some(3_600_000));

            h.read_only();
            h.choose("1 hour", "24 hours");
            let error = h.shown_starting(&format!("Could not save “{ARCHIVE_TITLE}”:")).expect("the failure is explained");
            assert!(error.ends_with("Little Windows still archive after 1 hour. Check disk space and data-folder permissions, then Retry Save or Cancel."), "{error}");
            assert_eq!(h.prefs.archive_after.hours(), 1, "the previous delay stays in use");
            h.click("Retry Save");
            assert!(h.shown_starting("Could not save").is_some(), "still failing");
            h.writable();
            h.click("Retry Save");
            assert_eq!((h.prefs.archive_after.hours(), h.reopened_prefs().archive_after.hours()), (24, 24), "Retry saves it at {size:?}");
            h.read_only();
            h.choose("24 hours", "Never");
            h.click("Cancel");
            assert!(h.shown_starting("Could not save").is_none() && h.prefs.archive_after.hours() == 24, "Cancel keeps the delay");
        }
        let mut h = Harness::new(SIZES[0]);
        h.store.kv_set("little_windows_archive_after_hours", "5").unwrap();
        h.prefs = LinkPreferences::load(&h.store);
        let notice = "The saved delay for archiving Little Windows could not be read, so they archive after 12 hours. Choosing an option saves over it.";
        assert_eq!(h.prefs.archive_after.hours(), 12);
        h.visible(notice);
        h.choose("12 hours", "Never");
        assert!(!h.shows(notice) && h.reopened_prefs().archive_after.hours() == 0);
    }
}
