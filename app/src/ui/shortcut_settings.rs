//! Settings → Shortcuts: every registry action with its primary and
//! alternate chord. Search by name or by pressing a chord, record or clear a
//! binding, resolve conflicts and reset, choose whether web pages receive
//! shortcuts first, and choose what the tab numbers count. Saved on this
//! device only.

use super::*;
use crate::numbered_tabs::{NINTH_ACTION, NINTH_CHORD, NINTH_TITLE, Ninth, NinthChoice, NumberedTabs};
use crate::shortcuts::{Category, Chord, Command, Overrides, Priority, Proposal, REGISTRY, Shortcuts, Slot};
use crate::ui::style::Size;
use crossbow_core::store::Store;
use egui::{Event, Key, Modifiers};

pub(super) const CHIP_WIDTH: f32 = 92.0;
pub(super) const CONTROL_HEIGHT: f32 = 26.0;
const RESET_WIDTH: f32 = 56.0;
const GAP: f32 = 8.0;
pub(super) const RECORDING: &str = "Press keys…";
pub(super) const UNSET: &str = "—";
const PRIORITY_WIDTH: f32 = 460.0;
const NINTH_WIDTH: f32 = 200.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Slot(usize, Slot),
    Search,
}

/// Where keyboard focus goes once the widget is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusTarget {
    Chip(usize, Slot),
    Reset(usize),
    Press,
    Prompt,
    ResetAll,
}

enum Prompt {
    /// A recorded chord that other slots use; Reassign saves `reassigned`.
    Conflict { index: usize, slot: Slot, chord: Chord, owners: Vec<(usize, Slot)>, reassigned: Overrides },
    /// Restoring an action's defaults takes chords from other actions.
    Reset { index: usize, owners: Vec<(usize, Slot)>, candidate: Overrides },
    ResetAll,
}

struct Failure {
    candidate: Overrides,
    error: String,
    reveal: bool,
}

/// A shortcut priority that could not be saved; the previous one stays in use.
struct PriorityFailure {
    choice: Priority,
    error: String,
    reveal: bool,
}

#[derive(Default)]
pub(super) struct Editor {
    query: String,
    chord_filter: Option<Chord>,
    recording: Option<Target>,
    prompt: Option<Prompt>,
    /// A refused chord, explained under its row.
    refusal: Option<(usize, String)>,
    failure: Option<Failure>,
    priority_failure: Option<PriorityFailure>,
    /// Focus to move, for at most this many more frames.
    focus: Option<(FocusTarget, u8)>,
    /// ⌘9 goes to the ninth tab, so Go to Last Tab is named for it.
    ninth: bool,
}

impl Editor {
    /// Recording or confirming: keys belong to this page, not to browser shortcuts.
    pub(super) fn owns_keyboard(&self) -> bool {
        self.recording.is_some() || self.prompt.is_some()
    }

    /// Leaving the page cancels recording and confirmations; nothing is saved.
    pub(super) fn leave(&mut self) {
        self.recording = None;
        self.prompt = None;
        self.refusal = None;
        self.focus = None;
    }

    fn start(&mut self, target: Target) {
        self.recording = Some(target);
        self.prompt = None;
        self.refusal = None;
        self.focus = None;
    }

    fn focus_on(&mut self, target: FocusTarget) {
        self.focus = Some((target, 2));
    }

    /// Request focus if `target` is the pending one.
    fn take_focus(&mut self, target: FocusTarget, response: &egui::Response) {
        if self.focus.is_some_and(|(pending, _)| pending == target) {
            response.request_focus();
            response.scroll_to_me(None);
            self.focus = None;
        }
    }

    /// `global` is the chord that opens a Little Window from anywhere, while on.
    fn finish(&mut self, target: Target, recorded: Option<Chord>, shortcuts: &mut Shortcuts, store: &Store, global: Option<Chord>) {
        self.recording = None;
        let (index, slot) = match target {
            Target::Search => {
                self.focus_on(FocusTarget::Press);
                if let Some(chord) = recorded {
                    self.chord_filter = Some(chord);
                    self.query.clear();
                }
                return;
            }
            Target::Slot(index, slot) => (index, slot),
        };
        self.focus_on(FocusTarget::Chip(index, slot));
        let Some(chord) = recorded else { return };
        let clear = chord.mods == Modifiers::NONE && matches!(chord.key, Key::Backspace | Key::Delete);
        // macOS gives that chord to the Little Window from every app, Crossbow included.
        let taken = global.filter(|global| !clear && global.overlaps(chord)).map(|global| Proposal::Refused(format!("{} opens a Little Window from anywhere. Choose another, or change that one in Settings → Links first.", global.label())));
        match taken.unwrap_or_else(|| shortcuts.propose(index, slot, (!clear).then_some(chord))) {
            Proposal::Unchanged => {}
            Proposal::Refused(reason) => {
                let kept = shortcuts.saved().binding(index, slot).map_or("no shortcut".to_string(), Chord::label);
                self.refusal = Some((index, format!("{reason} {} keeps {kept}.", action_name(index, self.ninth))));
            }
            Proposal::Ready(candidate) => self.commit(shortcuts, store, candidate),
            Proposal::Conflict { owners, reassigned } => {
                self.prompt = Some(Prompt::Conflict { index, slot, chord, owners, reassigned });
                self.focus_on(FocusTarget::Prompt);
            }
        }
    }

    /// One write; on failure the saved shortcuts stay in use and Retry offers it again.
    fn commit(&mut self, shortcuts: &mut Shortcuts, store: &Store, candidate: Overrides) {
        self.prompt = None;
        self.refusal = None;
        match shortcuts.save(store, candidate.clone()) {
            Ok(()) => self.failure = None,
            Err(error) => {
                self.failure = Some(Failure {
                    candidate,
                    error: format!("Could not save shortcuts: {error}. The previous shortcuts are still in use. Check disk space and data-folder permissions, then Retry Save or Cancel."),
                    reveal: true,
                });
            }
        }
    }

    /// One write; on failure the previous priority stays in use and Retry offers it again.
    fn save_priority(&mut self, shortcuts: &mut Shortcuts, store: &Store, choice: Priority) {
        self.priority_failure = match shortcuts.save_priority(store, choice) {
            Ok(()) => None,
            Err(error) => Some(PriorityFailure {
                choice,
                error: format!("Could not save “{}”: {error}. “{}” is still in use. Check disk space and data-folder permissions, then Retry Save or Cancel.", choice.label(), shortcuts.priority().label()),
                reveal: true,
            }),
        };
    }

    fn cancel_prompt(&mut self) {
        match self.prompt.take() {
            Some(Prompt::Conflict { index, slot, .. }) => self.focus_on(FocusTarget::Chip(index, slot)),
            Some(Prompt::Reset { index, .. }) => self.focus_on(FocusTarget::Reset(index)),
            Some(Prompt::ResetAll) => self.focus_on(FocusTarget::ResetAll),
            None => {}
        }
    }

    fn matches(&self, shortcuts: &Shortcuts, index: usize) -> bool {
        if let Some(chord) = self.chord_filter {
            return Slot::BOTH.iter().any(|slot| shortcuts.saved().binding(index, *slot).is_some_and(|bound| bound.overlaps(chord)));
        }
        let query = self.query.trim().to_lowercase();
        query.is_empty() || action_name(index, self.ninth).to_lowercase().contains(&query)
    }
}

/// Take this frame's first key press for recording and drop every other
/// keyboard event, so nothing types, clicks or runs. None means Escape.
pub(super) fn take_recorded(ui: &Ui) -> Option<Option<Chord>> {
    ui.input_mut(|input| {
        let mut recorded = None;
        input.events.retain(|event| {
            let chord = match event {
                Event::Key { key, pressed: true, repeat: false, modifiers, .. } => Some(Chord::from_event(*key, *modifiers)),
                // Clipboard chords arrive as their own events.
                Event::Copy => Some(Chord::from_event(Key::C, Modifiers::COMMAND)),
                Event::Cut => Some(Chord::from_event(Key::X, Modifiers::COMMAND)),
                Event::Paste(_) => Some(Chord::from_event(Key::V, Modifiers::COMMAND)),
                Event::Key { .. } | Event::Text(_) | Event::Ime(_) => None,
                _ => return true,
            };
            if recorded.is_none() {
                recorded = chord.map(|chord| (chord.key != Key::Escape || chord.mods != Modifiers::NONE).then_some(chord));
            }
            false
        });
        recorded
    })
}

/// An action's name on this page. Go to Last Tab says where ⌘9 goes.
fn action_name(index: usize, ninth: bool) -> &'static str {
    if ninth && REGISTRY[index].command == Command::LastTab { NINTH_ACTION } else { REGISTRY[index].label }
}

fn slot_name(index: usize, slot: Slot, ninth: bool) -> String {
    match slot {
        Slot::Primary => action_name(index, ninth).to_string(),
        Slot::Alternate => format!("{} (alternate)", action_name(index, ninth)),
    }
}

fn names(owners: &[(usize, Slot)], ninth: bool) -> String {
    owners.iter().map(|(index, slot)| slot_name(*index, *slot, ninth)).collect::<Vec<_>>().join(" and ")
}

pub(super) fn show(editor: &mut Editor, shortcuts: &mut Shortcuts, numbered: &mut NumberedTabs, store: &Store, global: Option<Chord>, ui: &mut Ui, pal: &Palette) {
    let ink = Ink::new(pal.panel, pal.accent);
    editor.ninth = numbered.ninth_choice(store).choice() == Ninth::Ninth;
    if let Some(target) = editor.recording {
        if let Some(recorded) = take_recorded(ui) {
            editor.finish(target, recorded, shortcuts, store, global);
        }
    } else if editor.prompt.is_some() && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
        editor.cancel_prompt();
    }

    if let Some(notice) = &shortcuts.notice {
        ui.label(style::text(notice, 12.0, style::weight::REGULAR).color(ink.danger));
    }
    failure_panel(editor, shortcuts, store, ui, &ink);
    search_bar(editor, shortcuts, ui, &ink);

    let editable = editor.failure.is_none();
    let chip_width = chip_width(ui, shortcuts);
    for category in Category::ALL {
        let indices: Vec<usize> = (0..REGISTRY.len()).filter(|index| REGISTRY[*index].category == category && editor.matches(shortcuts, *index)).collect();
        if indices.is_empty() { continue; }
        section(ui, pal, category.title());
        for index in indices {
            action_row(editor, shortcuts, store, index, chip_width, editable, ui, &ink, pal);
        }
    }
    ui.add_space(8.0);
    reset_all(editor, shortcuts, store, editable, ui, &ink);
    priority(editor, shortcuts, store, ui, &ink, pal);
    ui.label(style::text("Shortcuts are saved on this device only and don't sync.", 12.0, style::weight::REGULAR).color(ink.secondary));
    tab_numbers(numbered, store, ui, &ink, pal);
    if let Some((_, frames)) = &mut editor.focus {
        *frames = frames.saturating_sub(1);
        if *frames == 0 { editor.focus = None; }
    }
}

fn failure_panel(editor: &mut Editor, shortcuts: &mut Shortcuts, store: &Store, ui: &mut Ui, ink: &Ink) {
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
        editor.commit(shortcuts, store, candidate);
        ui.ctx().request_repaint();
    }
}

/// Who has a browser shortcut first while a web page has the keyboard.
fn priority(editor: &mut Editor, shortcuts: &mut Shortcuts, store: &Store, ui: &mut Ui, ink: &Ink, pal: &Palette) {
    section(ui, pal, "Shortcut priority");
    let mut choice = shortcuts.priority();
    let options = Priority::ALL.map(|priority| (priority, priority.label(), None));
    let width = ui.available_width().min(PRIORITY_WIDTH);
    if style::segmented(ui, ink, Id::unique("shortcut-priority"), &mut choice, &options, width) {
        editor.save_priority(shortcuts, store, choice);
    }
    let description = match shortcuts.priority() {
        Priority::Browser => "Crossbow runs its shortcuts before a web page sees them.",
        Priority::Websites => "While a web page has the keyboard, it receives each shortcut first. Crossbow runs the shortcut only if the page doesn't use it.",
        Priority::Twice => "While a web page has the keyboard, it receives a shortcut's first press. Press the same shortcut again within half a second to run Crossbow's instead. If the page doesn't use the first press, Crossbow runs it at once.",
    };
    ui.label(style::text(description, 12.0, style::weight::REGULAR).color(ink.secondary));
    let protected = shortcuts.protected().into_iter()
        .map(|(action, chord)| if chord.is_empty() { action.to_string() } else { format!("{action} {chord}") })
        .collect::<Vec<_>>().join(" · ");
    ui.label(style::text(format!("Always handled by Crossbow: {protected}"), 12.0, style::weight::REGULAR).color(ink.secondary));
    if let Some(notice) = &shortcuts.priority_notice {
        ui.label(style::text(notice, 12.0, style::weight::REGULAR).color(ink.danger));
    }
    let Some(failure) = editor.priority_failure.as_mut() else { return };
    let mut retry = false;
    let mut cancel = false;
    let response = ui.push_id("shortcut-priority-failure", |ui| {
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
        editor.priority_failure = None;
    } else if retry {
        let choice = failure.choice;
        editor.save_priority(shortcuts, store, choice);
        ui.ctx().request_repaint();
    }
}

/// What ⌘1–⌘9 count: whether Favorites come first, and whether ⌘9 goes
/// to the last tab or the ninth. A failed save keeps the previous choice.
fn tab_numbers(numbered: &mut NumberedTabs, store: &Store, ui: &mut Ui, ink: &Ink, pal: &Palette) {
    section(ui, pal, "Tab numbers");
    super::link_page::switch(numbered.favorites_switch(store), store, ui, ink, pal);
    let ninth = numbered.ninth_choice(store);
    let mut choice = ninth.choice();
    let options = Ninth::ALL.map(|choice| (choice, choice.label(), None));
    row(ui, pal, NINTH_TITLE, "Last tab goes to the end of the list, however long it is. Ninth tab does nothing when there are fewer than nine.", |ui| {
        if style::segmented(ui, ink, Id::unique("numbered-ninth"), &mut choice, &options, NINTH_WIDTH.min(ui.available_width())) {
            ninth.choose(store, choice);
            ui.ctx().request_repaint();
        }
    });
    ninth_failure(ninth, store, ui, ink);
}

fn ninth_failure(ninth: &mut NinthChoice, store: &Store, ui: &mut Ui, ink: &Ink) {
    if let Some(notice) = &ninth.notice {
        ui.label(style::text(notice, 12.0, style::weight::REGULAR).color(ink.danger));
    }
    let kept = format!("{NINTH_CHORD} still goes to the {}", ninth.choice().label().to_lowercase());
    let Some(failure) = ninth.failure.as_mut() else { return };
    let (retry, cancel) = super::link_page::save_failure(ui, ink, "numbered-ninth", NINTH_TITLE, &failure.error, &kept, &mut failure.reveal);
    if cancel {
        ninth.failure = None;
    } else if retry {
        let wanted = failure.wanted;
        ninth.choose(store, wanted);
        ui.ctx().request_repaint();
    }
}

fn search_bar(editor: &mut Editor, shortcuts: &Shortcuts, ui: &mut Ui, ink: &Ink) {
    let searching = editor.recording == Some(Target::Search);
    ui.horizontal(|ui| {
        let press_label = if searching { RECORDING } else { "Press Shortcut" };
        let press_width = style::measure(ui.painter(), press_label, 13.0, style::weight::MEDIUM).x + 28.0;
        let field_width = (ui.available_width() - press_width - GAP).clamp(140.0, 320.0);
        let field = ui.add(egui::TextEdit::singleline(&mut editor.query).id(Id::unique("shortcut-search")).hint_text("Search shortcuts").desired_width(field_width));
        if field.changed() {
            editor.chord_filter = None;
            editor.prompt = None;
            editor.refusal = None;
        }
        let press = Button::new(press_label).kind(if searching { Kind::Accent } else { Kind::Secondary }).show(ui, ink);
        editor.take_focus(FocusTarget::Press, &press);
        if searching {
            lock_focus(ui, &press);
            if press.clicked() {
                editor.recording = None;
            }
        } else if press.clicked() {
            editor.start(Target::Search);
            press.request_focus();
        }
    });
    if searching {
        ui.label(style::text("Press the shortcut to look up. Esc cancels.", 12.0, style::weight::REGULAR).color(ink.secondary));
    } else if let Some(chord) = editor.chord_filter {
        let found = (0..REGISTRY.len()).any(|index| editor.matches(shortcuts, index));
        ui.horizontal_wrapped(|ui| {
            let message = if found { format!("Actions using {}", chord.label()) } else { format!("No action uses {}.", chord.label()) };
            ui.label(style::text(message, 13.0, style::weight::MEDIUM).color(ink.text));
            let clear = Button::new("Clear").size(Size::Small).kind(Kind::Plain).show(ui, ink);
            if clear.clicked() {
                editor.chord_filter = None;
                editor.focus_on(FocusTarget::Press);
            }
        });
    } else if !(0..REGISTRY.len()).any(|index| editor.matches(shortcuts, index)) {
        ui.label(style::text(format!("No shortcuts match “{}”.", editor.query.trim()), 13.0, style::weight::REGULAR).color(ink.secondary));
    }
}

/// Keep Tab, arrows and Escape on a recording control.
pub(super) fn lock_focus(ui: &Ui, response: &egui::Response) {
    if !response.has_focus() { response.request_focus(); }
    ui.memory_mut(|memory| memory.set_focus_lock_filter(response.id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true }));
}

fn chip_text(shortcuts: &Shortcuts, index: usize, slot: Slot) -> String {
    shortcuts.saved().binding(index, slot).map_or(UNSET.to_string(), Chord::label)
}

/// One width for every chip, so the columns line up.
fn chip_width(ui: &Ui, shortcuts: &Shortcuts) -> f32 {
    let widest = (0..REGISTRY.len()).flat_map(|index| Slot::BOTH.map(|slot| chip_text(shortcuts, index, slot)))
        .chain([RECORDING.to_string()])
        .map(|text| style::measure(ui.painter(), &text, 12.5, style::weight::MEDIUM).x)
        .fold(0.0, f32::max);
    (widest + 20.0).max(CHIP_WIDTH)
}

#[allow(clippy::too_many_arguments)]
fn action_row(editor: &mut Editor, shortcuts: &mut Shortcuts, store: &Store, index: usize, chip_width: f32, editable: bool, ui: &mut Ui, ink: &Ink, pal: &Palette) {
    let spec = &REGISTRY[index];
    ui.push_id(spec.id, |ui| {
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), CONTROL_HEIGHT + 6.0), Sense::hover());
        let control = |right: f32, width: f32| Rect::from_min_size(pos2(right - width, row.center().y - CONTROL_HEIGHT / 2.0), vec2(width, CONTROL_HEIGHT));
        let reset = control(row.right(), RESET_WIDTH);
        let alternate = control(reset.left() - GAP, chip_width);
        let primary = control(alternate.left() - GAP, chip_width);
        let label = Rect::from_min_max(pos2(row.left(), row.top()), pos2(primary.left() - GAP, row.bottom()));
        ui.scope_builder(egui::UiBuilder::new().max_rect(label).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
            ui.add(egui::Label::new(style::text(action_name(index, editor.ninth), 13.5, style::weight::REGULAR).color(ink.text)).truncate());
        });
        for (slot, rect) in [(Slot::Primary, primary), (Slot::Alternate, alternate)] {
            if spec.fixed && slot == Slot::Alternate { continue; }
            let recording = editor.recording == Some(Target::Slot(index, slot));
            let text = if recording { RECORDING.to_string() } else { chip_text(shortcuts, index, slot) };
            let enabled = editable && !spec.fixed;
            let response = chip(ui, ink, Id::unique(("shortcut-chip", spec.id, slot)), rect, &text, recording, enabled, format!("{} shortcut: {}", slot_name(index, slot, editor.ninth), if text == UNSET { "none" } else { &text }));
            editor.take_focus(FocusTarget::Chip(index, slot), &response);
            if recording {
                lock_focus(ui, &response);
                if response.clicked() {
                    editor.recording = None;
                }
            } else if response.clicked() {
                editor.start(Target::Slot(index, slot));
                response.request_focus();
            }
        }
        if spec.fixed {
            ui.painter().text(alternate.center(), Align2::CENTER_CENTER, "Fixed", FontId::proportional(12.0), ink.secondary);
        } else if shortcuts.saved().changed(index) && editable {
            let response = Button::new("Reset").size(Size::Small).kind(Kind::Plain).show_at(ui, ink, reset);
            editor.take_focus(FocusTarget::Reset(index), &response);
            if response.clicked() {
                let (candidate, owners) = shortcuts.propose_reset(index);
                if owners.is_empty() {
                    editor.commit(shortcuts, store, candidate);
                    editor.focus_on(FocusTarget::Chip(index, Slot::Primary));
                } else {
                    editor.prompt = Some(Prompt::Reset { index, owners, candidate });
                    editor.recording = None;
                    editor.focus_on(FocusTarget::Prompt);
                }
            }
        }
        if editor.recording == Some(Target::Slot(index, Slot::Primary)) || editor.recording == Some(Target::Slot(index, Slot::Alternate)) {
            ui.label(style::text("Press the new shortcut. Delete clears it; Esc cancels.", 12.0, style::weight::REGULAR).color(ink.secondary));
        }
        if let Some((_, reason)) = editor.refusal.as_ref().filter(|(refused, _)| *refused == index) {
            ui.label(style::text(reason, 12.0, style::weight::REGULAR).color(ink.danger));
        }
        row_prompt(editor, shortcuts, store, index, ui, ink);
    });
    separator(ui, pal);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn chip(ui: &mut Ui, ink: &Ink, id: Id, rect: Rect, text: &str, recording: bool, enabled: bool, description: String) -> egui::Response {
    let response = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, &description));
    if response.gained_focus() {
        ui.scroll_to_rect(rect.expand(5.0), None);
    }
    let p = ui.painter();
    let (fill, color) = if recording {
        (ink.accent, ink.on_accent)
    } else if !enabled {
        (Color32::TRANSPARENT, ink.secondary)
    } else {
        (if response.hovered() { ink.fill_hover } else { ink.fill }, if text == UNSET { ink.tertiary } else { ink.text })
    };
    p.rect_filled(rect, style::radius::SM, fill);
    if !enabled {
        p.rect_stroke(rect, style::radius::SM, Stroke::new(1.0, ink.separator), egui::StrokeKind::Inside);
    }
    if response.has_focus() {
        p.rect_stroke(rect.expand(2.0), style::radius::SM + 2.0, Stroke::new(2.0, ink.accent), egui::StrokeKind::Outside);
    }
    style::paint(p, rect.center(), Align2::CENTER_CENTER, text, 12.5, style::weight::MEDIUM, color);
    if enabled && response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

fn row_prompt(editor: &mut Editor, shortcuts: &mut Shortcuts, store: &Store, index: usize, ui: &mut Ui, ink: &Ink) {
    let (message, confirm) = match &editor.prompt {
        Some(Prompt::Conflict { index: target, slot, chord, owners, .. }) if *target == index => (
            format!("{} is used by {}. Reassign it to {}?", chord.label(), names(owners, editor.ninth), slot_name(index, *slot, editor.ninth)),
            "Reassign",
        ),
        Some(Prompt::Reset { index: target, owners, .. }) if *target == index => {
            let chords: Vec<String> = Slot::BOTH.iter().filter_map(|slot| REGISTRY[index].default_binding(*slot)).map(Chord::label).collect();
            let owners = names(owners, editor.ninth);
            (format!("Resetting {} restores {}, which {owners} uses now. {owners} will lose it.", action_name(index, editor.ninth), chords.join(" and ")), "Reset")
        }
        _ => return,
    };
    let mut choice = None;
    ui.label(style::text(message, 12.5, style::weight::REGULAR).color(ink.text));
    ui.horizontal_wrapped(|ui| {
        let confirm = Button::new(confirm).kind(Kind::Primary).size(Size::Small).show(ui, ink);
        editor.take_focus(FocusTarget::Prompt, &confirm);
        if confirm.clicked() { choice = Some(true); }
        if Button::new("Cancel").kind(Kind::Plain).size(Size::Small).show(ui, ink).clicked() { choice = Some(false); }
    });
    match choice {
        Some(true) => {
            let focus = match &editor.prompt {
                Some(Prompt::Conflict { slot, .. }) => FocusTarget::Chip(index, *slot),
                _ => FocusTarget::Chip(index, Slot::Primary),
            };
            let candidate = match editor.prompt.take() {
                Some(Prompt::Conflict { reassigned, .. }) => reassigned,
                Some(Prompt::Reset { candidate, .. }) => candidate,
                _ => unreachable!(),
            };
            editor.commit(shortcuts, store, candidate);
            editor.focus_on(focus);
        }
        Some(false) => editor.cancel_prompt(),
        None => {}
    }
}

fn reset_all(editor: &mut Editor, shortcuts: &mut Shortcuts, store: &Store, editable: bool, ui: &mut Ui, ink: &Ink) {
    let changed = shortcuts.saved().changed_actions();
    let confirming = matches!(editor.prompt, Some(Prompt::ResetAll));
    let button = Button::new("Reset All Shortcuts").enabled(editable && changed > 0 && !confirming).show(ui, ink);
    editor.take_focus(FocusTarget::ResetAll, &button);
    if button.clicked() {
        editor.recording = None;
        editor.prompt = Some(Prompt::ResetAll);
        editor.focus_on(FocusTarget::Prompt);
    }
    if !confirming { return; }
    let noun = if changed == 1 { "shortcut" } else { "shortcuts" };
    ui.label(style::text(format!("Reset all {changed} changed {noun} to their defaults?"), 12.5, style::weight::REGULAR).color(ink.text));
    let mut choice = None;
    ui.horizontal_wrapped(|ui| {
        let confirm = Button::new("Reset All").kind(Kind::Primary).size(Size::Small).show(ui, ink);
        editor.take_focus(FocusTarget::Prompt, &confirm);
        if confirm.clicked() { choice = Some(true); }
        if Button::new("Cancel").kind(Kind::Plain).size(Size::Small).show(ui, ink).clicked() { choice = Some(false); }
    });
    match choice {
        Some(true) => editor.commit(shortcuts, store, Overrides::default()),
        Some(false) => editor.cancel_prompt(),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::repair_tests::{PreferenceFrame, settings_test_frame};
    use super::*;
    use crate::shortcuts::Command;
    use crate::shortcuts::tests::Fixture;

    const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
    const CMD: Modifiers = Modifiers::COMMAND;

    /// Settings → Shortcuts on a disposable store. Each frame first runs browser
    /// dispatch the way `CrossbowApp::handle_shortcuts` does, so a test sees
    /// what a keystroke would have run.
    struct Harness {
        fixture: Fixture,
        store: Store,
        shortcuts: Shortcuts,
        numbered: NumberedTabs,
        editor: Editor,
        ran: Vec<Command>,
        /// The Little Window chord, while it is on.
        global: Option<Chord>,
        dismissed: bool,
        ctx: egui::Context,
        size: egui::Vec2,
        frame: u32,
    }

    impl Harness {
        fn new(size: egui::Vec2) -> Self {
            let fixture = Fixture::new();
            let store = fixture.store();
            let shortcuts = Shortcuts::load(&store);
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            Self { fixture, store, shortcuts, numbered: NumberedTabs::default(), editor: Editor::default(), ran: Vec::new(), global: None, dismissed: false, ctx, size, frame: 0 }
        }

        fn reopened(&self) -> Shortcuts {
            Shortcuts::load(&self.fixture.store())
        }

        fn frame(&mut self, events: Vec<Event>) -> PreferenceFrame {
            settings_test_frame(&self.ctx, Page::Shortcuts, self.size, 1.0, &mut self.frame, events, |ui, pal| {
                let ctx = ui.ctx().clone();
                if !self.editor.owns_keyboard() {
                    let editing = ctx.memory(|memory| memory.focused().is_some());
                    self.ran.extend(self.shortcuts.take(&ctx, editing));
                    self.dismissed |= crate::app::take_shortcut(&ctx, Modifiers::NONE, Key::Escape);
                }
                show(&mut self.editor, &mut self.shortcuts, &mut self.numbered, &self.store, self.global, ui, pal);
            })
        }

        /// Settle animations and check every visible text fits the sheet and
        /// overlaps no other text.
        fn settle(&mut self) -> PreferenceFrame {
            for _ in 0..6 { self.frame(vec![]); }
            let shown = self.frame(vec![]);
            let page = shown.text.iter().map(|(_, _, clip)| *clip)
                .max_by_key(|clip| shown.text.iter().filter(|(_, _, other)| other == clip).count());
            for (i, (text, bounds, clip)) in shown.text.iter().enumerate() {
                if Some(*clip) == page && bounds.intersects(*clip) {
                    assert!(bounds.min.x >= clip.min.x - 0.5 && bounds.max.x <= clip.max.x + 0.5, "{text:?} overflows the sheet at {:?}: {bounds:?} in {clip:?}", self.size);
                    for (other, other_bounds, other_clip) in &shown.text[i + 1..] {
                        if other_clip == clip {
                            assert!(!bounds.shrink(0.5).intersects(other_bounds.shrink(0.5)), "{text:?} overlaps {other:?} at {:?}", self.size);
                        }
                    }
                }
            }
            shown
        }

        fn reveal(&mut self, what: &str, pick: impl Fn(&PreferenceFrame) -> Option<(Rect, Rect)>) -> egui::Pos2 {
            self.reveal_frame(what, pick).0
        }

        fn reveal_frame(&mut self, what: &str, pick: impl Fn(&PreferenceFrame) -> Option<(Rect, Rect)>) -> (egui::Pos2, PreferenceFrame) {
            let sheet = sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), self.size), 1.0);
            let wheel = sheet.right_bottom() - vec2(60.0, 40.0);
            // Offscreen labels are culled: search upward, then downward, in larger steps.
            // A step stays shorter than the list's height at 640×420, so no row is skipped.
            for attempt in 0..90 {
                let shown = self.settle();
                let delta = match pick(&shown) {
                    Some((bounds, clip)) if clip.expand(0.5).contains_rect(bounds) => return (bounds.center(), shown),
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

        /// The chip text in `action`'s row, primary first.
        fn chips(&mut self, action: &str) -> Vec<String> {
            let (_, shown) = self.reveal_frame(action, |shown| shown.text.iter().find(|(text, ..)| text == action).map(|(_, bounds, clip)| (*bounds, *clip)));
            let (_, row, _) = shown.text.iter().find(|(text, ..)| text == action).unwrap();
            let mut chips: Vec<_> = shown.text.iter()
                .filter(|(text, bounds, _)| text != action && bounds.left() > row.right() && (bounds.center().y - row.center().y).abs() < 6.0)
                .map(|(text, bounds, _)| (bounds.left(), text.clone())).collect();
            chips.sort_by(|a, b| a.0.total_cmp(&b.0));
            chips.into_iter().map(|(_, text)| text).collect()
        }

        /// A control in `action`'s row: its primary chip, alternate chip or Reset.
        fn row_control(&mut self, action: &str, nth: usize) -> egui::Pos2 {
            self.reveal(&format!("{action} control {nth}"), |shown| {
                let (_, row, _) = shown.text.iter().find(|(text, ..)| text == action)?;
                let mut chips: Vec<_> = shown.text.iter()
                    .filter(|(text, bounds, _)| text != action && bounds.left() > row.right() && (bounds.center().y - row.center().y).abs() < 6.0)
                    .collect();
                chips.sort_by(|a, b| a.1.left().total_cmp(&b.1.left()));
                chips.get(nth).map(|(_, bounds, clip)| (*bounds, *clip))
            })
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

        fn key(&mut self, key: Key, modifiers: Modifiers) {
            for pressed in [true, false] {
                self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }]);
            }
            self.settle();
        }

        fn search(&mut self, text: &str) {
            let query = self.editor.query.clone();
            let field = self.reveal("search field", |shown| shown.text.iter().find(|(t, ..)| t == "Search shortcuts" || (!t.is_empty() && *t == query)).map(|(_, bounds, clip)| (*bounds, *clip)));
            self.click_at(field);
            self.key(Key::A, CMD);
            self.key(Key::Backspace, Modifiers::NONE);
            if !text.is_empty() { self.frame(vec![Event::Text(text.into())]); }
            self.settle();
        }

        fn focused_rect(&self) -> Option<Rect> {
            self.ctx.memory(|memory| memory.focused()).and_then(|id| self.ctx.read_response(id)).map(|response| response.rect)
        }

        /// Press Tab until the control showing `label` has focus; one reached
        /// by Tab must be scrolled into view.
        fn tab_to(&mut self, label: &str) {
            for attempt in 0..200 {
                let shown = self.settle();
                let focused = self.focused_rect();
                if shown.text.iter().any(|(text, bounds, _)| text == label && focused.is_some_and(|f| f.contains_rect(*bounds))) {
                    assert!(attempt == 0 || self.has_focus_on(label), "focused {label} must be scrolled into view at {:?}", self.size);
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

        fn saved(&self, command: Command, slot: Slot) -> Option<String> {
            self.reopened().binding(command, slot).map(Chord::label)
        }
    }

    fn label(key: Key, modifiers: Modifiers) -> String {
        Chord::from_event(key, modifiers).label()
    }

    #[test]
    fn every_action_is_listed_from_the_registry_and_fits_both_sizes() {
        for size in SIZES {
            let mut h = Harness::new(size);
            for spec in REGISTRY {
                h.visible(spec.label);
                let expected: Vec<String> = Slot::BOTH.iter().filter(|slot| !spec.fixed || **slot == Slot::Primary)
                    .map(|slot| spec.default_binding(*slot).map_or(UNSET.to_string(), Chord::label)).collect();
                assert_eq!(h.chips(spec.label)[..expected.len()], expected[..], "{} at {size:?}", spec.id);
            }
            h.visible("Fixed");
            h.visible("Shortcuts are saved on this device only and don't sync.");
            assert!(!h.shows("Reset"), "nothing is changed yet");
        }
    }

    #[test]
    fn searching_by_name_filters_and_says_when_nothing_matches() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.search("reload");
            assert!(h.shows("Reload") && h.shows("Hard Reload"));
            assert!(!h.shows("Back") && !h.shows("New Tab"));
            h.search("zzz");
            h.visible("No shortcuts match “zzz”.");
            h.search("");
            h.visible("New Tab");
            assert!(h.ran.is_empty() && !h.dismissed, "typing a query runs nothing");
        }
    }

    #[test]
    fn pressing_a_shortcut_finds_its_actions_without_running_them() {
        for size in SIZES {
            for keyboard in [false, true] {
                let mut h = Harness::new(size);
                let start = |h: &mut Harness| if keyboard {
                    h.tab_to("Press Shortcut");
                    h.key(Key::Enter, Modifiers::NONE);
                } else {
                    h.click("Press Shortcut");
                };
                start(&mut h);
                h.visible(RECORDING);
                h.visible("Press the shortcut to look up. Esc cancels.");
                h.key(Key::W, CMD);
                assert!(h.ran.is_empty(), "the captured chord never runs");
                h.visible(&format!("Actions using {}", label(Key::W, CMD)));
                assert!(h.shows("Close Tab or Peek") && !h.shows("Reload"));
                assert!(h.has_focus_on("Press Shortcut"), "focus returns to the button");

                start(&mut h);
                h.key(Key::J, CMD);
                h.visible(&format!("No action uses {}.", label(Key::J, CMD)));
                h.click("Clear");
                h.visible("Reload");

                // Escape cancels capture, keeps Settings open and returns focus.
                start(&mut h);
                h.key(Key::Escape, Modifiers::NONE);
                assert!(!h.shows(RECORDING));
                assert!(h.has_focus_on("Press Shortcut"));
                h.visible("Reload");
                assert!(!h.dismissed && h.ran.is_empty());
                assert!(!h.editor.owns_keyboard(), "no capture stays held");
                assert_eq!(h.reopened().saved(), &Overrides::default());
            }
        }
    }

    #[test]
    fn recording_sets_clears_and_cancels_a_binding_by_pointer_or_keyboard() {
        for size in SIZES {
            for keyboard in [false, true] {
                let mut h = Harness::new(size);
                h.search("reload");
                let record = |h: &mut Harness, nth: usize, chip: &str| if keyboard {
                    h.tab_to(chip);
                    h.key(Key::Enter, Modifiers::NONE);
                } else {
                    let pos = h.row_control("Reload", nth);
                    h.click_at(pos);
                };
                let r = label(Key::R, CMD);
                record(&mut h, 0, &r);
                assert_eq!(h.chips("Reload")[0], RECORDING);
                h.visible("Press the new shortcut. Delete clears it; Esc cancels.");
                h.key(Key::K, CMD);
                let k = label(Key::K, CMD);
                assert_eq!(h.chips("Reload")[..2], [k.clone(), UNSET.to_string()]);
                assert!(h.ran.is_empty(), "recording never runs the chord");
                assert!(h.has_focus_on(&k), "focus stays on the edited binding");
                assert_eq!(h.saved(Command::Reload, Slot::Primary), Some(k.clone()));
                assert!(h.shows("Reset"));

                // Escape cancels without saving or closing Settings.
                record(&mut h, 0, &k);
                h.key(Key::Escape, Modifiers::NONE);
                assert_eq!(h.chips("Reload")[0], k);
                assert!(!h.dismissed);
                assert!(h.has_focus_on(&k));

                // The alternate is independent.
                record(&mut h, 1, UNSET);
                h.key(Key::F5, Modifiers::NONE);
                assert_eq!(h.chips("Reload")[..2], [k.clone(), "F5".to_string()]);
                assert_eq!(h.saved(Command::Reload, Slot::Alternate).as_deref(), Some("F5"));

                // Delete clears only the primary mapping.
                record(&mut h, 0, &k);
                h.key(Key::Backspace, Modifiers::NONE);
                assert_eq!(h.chips("Reload")[..2], [UNSET.to_string(), "F5".to_string()]);
                let reopened = h.reopened();
                assert_eq!(reopened.binding(Command::Reload, Slot::Primary), None);
                assert_eq!(reopened.binding(Command::Reload, Slot::Alternate), Some(Chord { key: Key::F5, mods: Modifiers::NONE }));

                // Browser dispatch outside recording uses the saved bindings.
                h.ran.clear();
                h.key(Key::F5, Modifiers::NONE);
                assert_eq!(h.ran, [Command::Reload]);
            }
        }
    }

    #[test]
    fn a_conflict_names_both_actions_and_reassign_or_cancel_changes_both_or_neither() {
        for size in SIZES {
            for keyboard in [false, true] {
                let mut h = Harness::new(size);
                h.search("reload");
                let w = label(Key::W, CMD);
                let question = format!("{w} is used by Close Tab or Peek. Reassign it to Reload?");
                let pos = h.row_control("Reload", 0);
                h.click_at(pos);
                h.key(Key::W, CMD);
                h.visible(&question);
                assert!(h.ran.is_empty() && h.editor.owns_keyboard());
                assert_eq!(h.reopened().saved(), &Overrides::default(), "asking writes nothing");
                if keyboard {
                    h.key(Key::Escape, Modifiers::NONE);
                    assert!(!h.dismissed, "Escape cancels the question, not Settings");
                } else {
                    h.click("Cancel");
                }
                assert!(!h.shows(&question));
                assert_eq!(h.chips("Reload")[0], label(Key::R, CMD));
                assert_eq!(h.reopened().saved(), &Overrides::default());

                let pos = h.row_control("Reload", 0);
                h.click_at(pos);
                h.key(Key::W, CMD);
                if keyboard {
                    assert!(h.has_focus_on("Reassign"), "the question takes focus");
                    h.key(Key::Enter, Modifiers::NONE);
                } else {
                    h.click("Reassign");
                }
                assert_eq!(h.chips("Reload")[0], w);
                let reopened = h.reopened();
                assert_eq!(reopened.binding(Command::Reload, Slot::Primary), Some(Chord { key: Key::W, mods: CMD }));
                assert_eq!(reopened.binding(Command::CloseTab, Slot::Primary), None);
                h.search("close tab");
                assert_eq!(h.chips("Close Tab or Peek")[0], UNSET);
                h.ran.clear();
                h.key(Key::Escape, Modifiers::NONE);
                h.dismissed = false;
                h.key(Key::W, CMD);
                assert_eq!(h.ran, [Command::Reload], "the winner runs once; the loser does not");
            }
        }
    }

    #[test]
    fn reserved_and_bare_chords_are_explained_and_keep_the_previous_binding() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.search("reload");
            let r = label(Key::R, CMD);
            let mut refused = vec![(Key::K, Modifiers::NONE, format!("K needs {}; only function keys work alone. Reload keeps {r}.", if cfg!(target_os = "macos") { "⌘ or ⌃" } else { "Ctrl" }))];
            if cfg!(target_os = "macos") {
                refused.push((Key::Q, CMD, format!("⌘Q is reserved: Crossbow quits with it. Reload keeps {r}.")));
                refused.push((Key::Space, CMD, format!("⌘Space is reserved: macOS opens Spotlight with it. Reload keeps {r}.")));
            }
            refused.push((Key::Comma, CMD, format!("{} is reserved: Crossbow opens Settings with it. Reload keeps {r}.", label(Key::Comma, CMD))));
            for (key, modifiers, explanation) in refused {
                let pos = h.row_control("Reload", 0);
                h.click_at(pos);
                h.key(key, modifiers);
                h.visible(&explanation);
                assert_eq!(h.chips("Reload")[0], r);
                assert!(h.ran.is_empty());
            }
            assert_eq!(h.reopened().saved(), &Overrides::default());
            h.search("settings");
            assert!(h.shows("Fixed"));
            let pos = h.row_control("Settings", 0);
            h.click_at(pos);
            assert!(!h.shows(RECORDING), "a fixed shortcut cannot be recorded");
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_little_window_chord_is_refused_while_it_opens_a_little_window_from_anywhere() {
        let little = Modifiers { alt: true, ctrl: true, shift: false, mac_cmd: true, command: true };
        for size in SIZES {
            let mut h = Harness::new(size);
            h.global = Some(crate::hotkey::DEFAULT_CHORD);
            h.search("reload");
            let r = label(Key::R, CMD);
            let pos = h.row_control("Reload", 0);
            h.click_at(pos);
            h.key(Key::N, little);
            h.visible(&format!("⌃⌥⌘N opens a Little Window from anywhere. Choose another, or change that one in Settings → Links first. Reload keeps {r}."));
            assert_eq!(h.chips("Reload")[0], r);
            assert_eq!(h.reopened().saved(), &Overrides::default());
            // While the hotkey is off, the chord is free.
            h.global = None;
            let pos = h.row_control("Reload", 0);
            h.click_at(pos);
            h.key(Key::N, little);
            assert_eq!(h.chips("Reload")[0], "⌃⌥⌘N");
            assert!(h.ran.is_empty(), "recording runs nothing at {size:?}");
        }
    }

    #[test]
    fn reset_one_resolves_its_conflict_and_reset_all_asks_first() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let Proposal::Conflict { reassigned, .. } = h.shortcuts.propose(crate::shortcuts::index_of(Command::Reload), Slot::Primary, Some(Chord { key: Key::W, mods: CMD })) else { unreachable!() };
            h.shortcuts.save(&h.store, reassigned).unwrap();
            let w = label(Key::W, CMD);
            h.search("close tab");
            let question = format!("Resetting Close Tab or Peek restores {w}, which Reload uses now. Reload will lose it.");
            h.click("Reset");
            h.visible(&question);
            h.click("Cancel");
            assert!(!h.shows(&question));
            assert_eq!(h.reopened().binding(Command::Reload, Slot::Primary), Some(Chord { key: Key::W, mods: CMD }));
            h.click("Reset");
            let confirm = h.reveal("Reset in the question", |shown| shown.text.iter().rfind(|(text, ..)| text == "Reset").map(|(_, b, c)| (*b, *c)));
            h.click_at(confirm);
            assert_eq!(h.chips("Close Tab or Peek")[0], w);
            let reopened = h.reopened();
            assert_eq!(reopened.binding(Command::CloseTab, Slot::Primary), Some(Chord { key: Key::W, mods: CMD }));
            assert_eq!(reopened.binding(Command::Reload, Slot::Primary), None);

            h.search("");
            h.click("Reset All Shortcuts");
            h.visible("Reset all 1 changed shortcut to their defaults?");
            h.click("Cancel");
            assert_eq!(h.reopened().saved().changed_actions(), 1, "Cancel deletes none");
            h.click("Reset All Shortcuts");
            h.key(Key::Escape, Modifiers::NONE);
            assert!(!h.dismissed && !h.shows("Reset All"));
            assert!(h.has_focus_on("Reset All Shortcuts"));
            h.click("Reset All Shortcuts");
            h.click("Reset All");
            assert_eq!(h.reopened().saved(), &Overrides::default());
            assert!(!h.shows("Reset"));
            h.search("reload");
            assert_eq!(h.chips("Reload")[0], label(Key::R, CMD));
        }
    }

    #[test]
    fn a_failed_save_keeps_the_previous_shortcuts_and_retry_or_cancel_recovers() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.search("reload");
            h.store = h.fixture.read_only();
            let pos = h.row_control("Reload", 0);
            h.click_at(pos);
            h.key(Key::K, CMD);
            let error = h.settle().text.into_iter().map(|(text, ..)| text).find(|text| text.starts_with("Could not save shortcuts:")).expect("the failure is explained");
            assert!(error.ends_with("The previous shortcuts are still in use. Check disk space and data-folder permissions, then Retry Save or Cancel."));
            h.visible("Retry Save");
            assert_eq!(h.chips("Reload")[0], label(Key::R, CMD), "the previous binding stays");
            h.key(Key::R, CMD);
            assert_eq!(h.ran, [Command::Reload], "and still runs");
            h.click("Retry Save");
            assert!(h.shows("Retry Save"), "still failing");
            h.click("Cancel");
            assert!(!h.shows("Retry Save"));
            assert_eq!(h.reopened().saved(), &Overrides::default());

            let pos = h.row_control("Reload", 0);
            h.click_at(pos);
            h.key(Key::K, CMD);
            h.store = h.fixture.store();
            h.click("Retry Save");
            assert!(!h.shows("Retry Save"));
            assert_eq!(h.saved(Command::Reload, Slot::Primary), Some(label(Key::K, CMD)));
        }
    }

    const BROWSER_FIRST: &str = "Crossbow runs its shortcuts before a web page sees them.";
    const WEBSITES_FIRST: &str = "While a web page has the keyboard, it receives each shortcut first. Crossbow runs the shortcut only if the page doesn't use it.";
    const PRESS_TWICE: &str = "While a web page has the keyboard, it receives a shortcut's first press. Press the same shortcut again within half a second to run Crossbow's instead. If the page doesn't use the first press, Crossbow runs it at once.";

    impl Harness {
        fn shown(&mut self, prefix: &str) -> Option<String> {
            self.settle().text.into_iter().map(|(text, ..)| text).find(|text| text.starts_with(prefix))
        }

        /// Click the switch beside `title`, scrolled until both are in view.
        fn toggle(&mut self, title: &str) {
            let scroll = sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), self.size), 1.0).right_bottom() - vec2(60.0, 40.0);
            self.visible(title);
            for _ in 0..12 {
                let shown = self.settle();
                let (_, row, clip) = shown.text.iter().find(|(text, _, _)| text == title).expect("the title stays drawn").clone();
                // The row's only switch: right of the title, centred on title and description.
                let beside = shown.switches.iter().find(|switch| switch.left() > row.right() && switch.top() > row.top() - 4.0 && switch.top() < row.top() + 60.0).copied();
                let wanted = beside.map_or(row.union(row.translate(vec2(0.0, 50.0))), |switch| switch.union(row));
                if let Some(switch) = beside.filter(|_| clip.contains_rect(wanted)) {
                    return self.click_at(switch.center());
                }
                let delta = if wanted.bottom() > clip.bottom() { -40.0 } else { 40.0 };
                self.frame(vec![
                    Event::PointerMoved(scroll),
                    Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, delta), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Modifiers::NONE },
                ]);
            }
            panic!("no switch for {title} in view at {:?}", self.size);
        }

        /// Click once wheel scrolling has come to rest, as a person would.
        fn click_at_rest(&mut self, label: &str) {
            self.visible(label);
            for _ in 0..60 { self.frame(vec![]); }
            self.click(label);
        }
    }

    #[test]
    fn shortcut_priority_is_chosen_by_pointer_or_keyboard_and_names_what_stays_with_crossbow() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.visible("SHORTCUT PRIORITY");
            for priority in Priority::ALL { h.visible(priority.label()); }
            let shown = h.settle();
            let bounds = |label: &str| shown.text.iter().find(|(text, ..)| text == label).map(|(_, bounds, _)| *bounds).unwrap();
            let labels = Priority::ALL.map(|priority| bounds(priority.label()));
            // Each label is centred in its third, so the centres are one segment apart.
            let segment = labels[1].center().x - labels[0].center().x;
            assert!((labels[2].center().x - labels[1].center().x - segment).abs() < 1.0, "three equal segments at {size:?}: {labels:?}");
            for (label, bounds) in Priority::ALL.iter().zip(labels) {
                assert!(bounds.width() < segment - 12.0, "{:?} fits its segment at {size:?}: {bounds:?}", label.label());
                assert!((bounds.center().y - labels[0].center().y).abs() < 1.0, "one row of three options at {size:?}");
            }
            assert!(labels[0].max.x < labels[1].min.x && labels[1].max.x < labels[2].min.x, "labels never overlap at {size:?}");
            h.visible(BROWSER_FIRST);
            assert_eq!(h.shortcuts.priority(), Priority::Browser, "browser shortcuts win by default");

            // The line under the description may sit just below the view at 640×420.
            h.reveal("the protected shortcuts", |shown| shown.text.iter().find(|(text, ..)| text.starts_with("Always handled by Crossbow: ")).map(|(_, bounds, clip)| (*bounds, *clip)));
            let always = h.shown("Always handled by Crossbow: ").expect("protected shortcuts are listed");
            for chord in [label(Key::L, CMD), label(Key::T, CMD), label(Key::W, CMD), label(Key::Comma, CMD)] {
                assert!(always.contains(&chord), "{chord} is listed: {always}");
            }
            if cfg!(target_os = "macos") { assert!(always.contains("Quit Crossbow ⌘Q"), "{always}"); }
            assert!(always.ends_with("Esc while a Crossbow overlay is open"), "{always}");

            h.click(Priority::Websites.label());
            assert_eq!(h.reopened().priority(), Priority::Websites, "saved at {size:?}");
            assert!(h.shows(WEBSITES_FIRST) && !h.shows(BROWSER_FIRST));
            h.click(Priority::Twice.label());
            assert_eq!(h.reopened().priority(), Priority::Twice, "press twice is saved at {size:?}");
            h.visible(PRESS_TWICE);
            assert!(!h.shows(WEBSITES_FIRST));

            h.tab_to(Priority::Browser.label());
            assert!(h.has_focus_on(Priority::Browser.label()));
            h.key(Key::Space, Modifiers::NONE);
            assert_eq!(h.reopened().priority(), Priority::Browser, "Space chooses the focused option");
            h.tab_to(Priority::Websites.label());
            h.key(Key::Enter, Modifiers::NONE);
            assert_eq!(h.reopened().priority(), Priority::Websites, "Enter chooses the focused option");
            h.tab_to(Priority::Twice.label());
            h.key(Key::Space, Modifiers::NONE);
            assert_eq!(h.reopened().priority(), Priority::Twice, "the third option by keyboard too");
            assert!(h.ran.is_empty(), "choosing runs no shortcut: {:?}", h.ran);

            // An unreadable record says so and falls back until an option is chosen.
            h.store.kv_set("shortcut_priority", "sideways").unwrap();
            h.shortcuts = h.reopened();
            let notice = "The saved shortcut priority could not be read, so Crossbow's shortcuts come first. Choosing an option saves over it.";
            h.visible(notice);
            h.click(Priority::Websites.label());
            assert!(!h.shows(notice));
            assert_eq!(h.reopened().priority(), Priority::Websites);
        }
    }

    #[test]
    fn a_failed_priority_save_keeps_the_previous_choice_and_retry_or_cancel_recovers() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.store = h.fixture.read_only();
            h.click_at_rest(Priority::Websites.label());
            let error = h.shown("Could not save “Websites first”:").unwrap_or_else(|| panic!("the failure is explained at {size:?}: {:?}", h.settle().text.into_iter().map(|(text, ..)| text).collect::<Vec<_>>()));
            assert!(error.ends_with("“Crossbow first” is still in use. Check disk space and data-folder permissions, then Retry Save or Cancel."), "{error}");
            assert_eq!(h.shortcuts.priority(), Priority::Browser, "the previous choice stays in use");
            assert!(h.shows(BROWSER_FIRST));
            h.click("Retry Save");
            assert!(h.shows("Retry Save"), "still failing");
            h.click("Cancel");
            assert!(!h.shows("Retry Save"));
            assert_eq!(h.reopened().priority(), Priority::Browser);

            h.click_at_rest(Priority::Websites.label());
            h.store = h.fixture.store();
            h.click("Retry Save");
            assert!(!h.shows("Retry Save"));
            assert_eq!(h.reopened().priority(), Priority::Websites, "Retry saves the choice at {size:?}");
            assert!(h.shows(WEBSITES_FIRST));
        }
    }

    #[test]
    fn leaving_the_page_while_recording_saves_nothing_and_releases_the_keyboard() {
        let mut h = Harness::new(SIZES[0]);
        h.search("reload");
        let pos = h.row_control("Reload", 0);
        h.click_at(pos);
        assert!(h.editor.owns_keyboard());
        h.editor.leave();
        h.settle();
        assert!(!h.editor.owns_keyboard() && !h.shows(RECORDING));
        h.key(Key::R, CMD);
        assert_eq!(h.ran, [Command::Reload]);
        assert_eq!(h.reopened().saved(), &Overrides::default());
    }

    /// Whether Favorites count is a switch, and where ⌘9 goes a choice of
    /// two; both reopen as chosen, and Go to Last Tab is named for the choice.
    #[test]
    fn tab_numbers_choose_favorites_and_the_ninth_number_and_name_its_shortcut() {
        use crate::link_prefs::SwitchSpec;
        use crate::numbered_tabs::IncludeFavorites;
        let last = REGISTRY[crate::shortcuts::index_of(Command::LastTab)].label;
        for size in SIZES {
            let mut h = Harness::new(size);
            h.visible("TAB NUMBERS");
            h.visible(IncludeFavorites::TITLE);
            h.visible(IncludeFavorites::DESCRIPTION);
            h.visible(NINTH_TITLE);
            let shown = h.settle();
            let bounds = |label: &str| shown.text.iter().find(|(text, ..)| text == label).map(|(_, bounds, _)| *bounds).unwrap_or_else(|| panic!("{label:?} at {size:?}"));
            let (last_tab, ninth_tab, title) = (bounds(Ninth::Last.label()), bounds(Ninth::Ninth.label()), bounds(NINTH_TITLE));
            assert!(last_tab.max.x < ninth_tab.min.x && (last_tab.center().y - ninth_tab.center().y).abs() < 1.0, "two options in one row at {size:?}");
            assert!(last_tab.min.x > title.max.x, "beside its title at {size:?}");
            // Searched, so the row is drawn whatever the window's height.
            h.search("last tab");
            assert!(h.shows(last), "named for the last tab by default at {size:?}");
            h.search("tab 9");
            assert!(!h.shows(NINTH_ACTION));
            h.search("");

            h.click_at_rest(Ninth::Ninth.label());
            assert_eq!(NumberedTabs::default().ninth_choice(&h.fixture.store()).choice(), Ninth::Ninth, "saved at {size:?}");
            h.search("tab 9");
            assert!(h.shows(NINTH_ACTION), "Go to Last Tab now says where ⌘9 goes at {size:?}");
            h.search("last tab");
            assert!(!h.shows(last) && h.shows("No shortcuts match “last tab”."));
            h.search("");

            h.toggle(IncludeFavorites::TITLE);
            assert!(!NumberedTabs::default().favorites_switch(&h.fixture.store()).enabled(), "saved off at {size:?}");

            h.click_at_rest(Ninth::Last.label());
            assert_eq!(NumberedTabs::default().ninth_choice(&h.fixture.store()).choice(), Ninth::Last);
            h.search("last tab");
            assert!(h.shows(last));
        }
    }

    #[test]
    fn a_failed_tab_numbers_save_keeps_the_mapping_and_retry_or_cancel_recovers() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.store = h.fixture.read_only();
            h.click_at_rest(Ninth::Ninth.label());
            let error = h.shown(&format!("Could not save “{NINTH_TITLE}”:")).unwrap_or_else(|| panic!("the failure is explained at {size:?}"));
            assert!(error.ends_with(&format!("{NINTH_CHORD} still goes to the last tab. Check disk space and data-folder permissions, then Retry Save or Cancel.")), "{error}");
            assert_eq!(h.numbered.ninth_choice(&h.store).choice(), Ninth::Last, "the previous choice stays in use");
            h.click("Retry Save");
            assert!(h.shows("Retry Save"), "still failing");
            h.click("Cancel");
            assert!(!h.shows("Retry Save"));

            h.click_at_rest(Ninth::Ninth.label());
            h.store = h.fixture.store();
            h.click("Retry Save");
            assert!(!h.shows("Retry Save"));
            assert_eq!(NumberedTabs::default().ninth_choice(&h.fixture.store()).choice(), Ninth::Ninth, "saved by Retry at {size:?}");
        }
    }
}
