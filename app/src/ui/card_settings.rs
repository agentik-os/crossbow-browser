//! Settings → Passwords → Cards: the active profile's saved cards, with Add,
//! Edit and Delete behind a confirmation. Readable only while the vault is
//! unlocked. A card is named by its network and last four digits; its full
//! number is never drawn, except as the user types a new one, and there is
//! no field for a security code. Nothing here is logged.

use super::*;
use crate::passwords::{CARD_FIELD_IDS, CARD_FIELD_LIMITS, CardEditor, MONTH, NAME, NICKNAME, NO_CARD_PROFILE, NUMBER, YEAR, card_field_id, card_title, this_month};
use crossbow_core::vault::SavedCard;

const DESC: &str = "Kept in the vault for this profile, and filled into an https page only when you choose one from the key button or a field's menu. The full number is never shown, and pages don't offer to save cards. Cards aren't synced.";
const EMPTY: &str = "No saved cards in this profile yet.";
const NO_SECURITY_CODE: &str = "The security code is never saved.";

/// What a click asked for, applied after the list is drawn.
enum Action {
    Edit(String),
    Delete(String),
    ConfirmDelete,
    CancelDelete,
    Save,
    Cancel,
}

pub(super) fn cards(state: &mut Passwords, ui: &mut Ui, pal: &Palette, ink: &Ink) {
    let mut add = false;
    let has_profile = !state.current_profile().is_empty();
    row(ui, pal, "Saved cards", DESC, |ui| {
        add = in_view(Button::new("Add Card").kind(Kind::Primary).enabled(has_profile).show(ui, ink)).clicked();
    });
    if !has_profile {
        ui.label(style::text(NO_CARD_PROFILE, 12.5, style::weight::REGULAR).color(ink.secondary));
        return;
    }
    if add { state.start_add_card(); }
    let mut actions = Vec::new();
    if let Some(editor) = state.page.card_editor.as_mut().filter(|editor| editor.id.is_none()) {
        editor_form(editor, ui, pal, ink, &mut actions);
        separator(ui, pal);
    }
    let listed = state.listed_cards();
    if listed.is_empty() {
        ui.label(style::text(EMPTY, 12.5, style::weight::REGULAR).color(ink.secondary));
    }
    let now = crossbow_core::now_ms();
    let ids: Vec<String> = listed.iter().map(|saved| saved.id.clone()).collect();
    for id in ids {
        let (vault, page) = state.parts();
        let Some(saved) = vault.and_then(|vault| vault.saved_card(&id)) else { continue };
        ui.push_id(("card", id.as_str()), |ui| {
            header(saved, now, ui, pal, ink);
            if page.confirm_delete_card.as_deref() == Some(id.as_str()) {
                delete_prompt(saved, ui, ink, &mut actions);
            } else if let Some(editor) = page.card_editor.as_mut().filter(|editor| editor.id.as_deref() == Some(id.as_str())) {
                editor_form(editor, ui, pal, ink, &mut actions);
            } else {
                ui.horizontal_wrapped(|ui| {
                    if saved.readable() && in_view(Button::new("Edit").size(Size::Small).show(ui, ink)).clicked() { actions.push(Action::Edit(id.clone())); }
                    if in_view(Button::new("Delete…").size(Size::Small).kind(Kind::Plain).show(ui, ink)).clicked() { actions.push(Action::Delete(id.clone())); }
                });
            }
            messages(page, Some(&id), ui, ink);
        });
        separator(ui, pal);
    }
    for action in actions {
        match action {
            Action::Edit(id) => state.start_edit_card(&id),
            Action::Delete(id) => state.request_delete_card(&id),
            Action::ConfirmDelete => state.confirm_delete_card(),
            Action::CancelDelete => state.page.confirm_delete_card = None,
            Action::Save => state.save_card_editor(),
            Action::Cancel => state.close_card_editor(),
        }
    }
}

/// The network and last four digits, then the nickname and expiry, then the last use.
fn header(saved: &SavedCard, now_ms: u64, ui: &mut Ui, pal: &Palette, ink: &Ink) {
    if !saved.readable() {
        ui.label(style::text("Damaged card", 13.5, style::weight::REGULAR).color(pal.panel_text));
        ui.label(style::text(VaultError::Unreadable.to_string(), 12.0, style::weight::REGULAR).color(ink.danger));
        return;
    }
    ui.label(style::text(card_title(saved), 13.5, style::weight::REGULAR).color(pal.panel_text));
    let expires = format!("Expires {}", saved.expiry());
    let summary = if saved.nickname.trim().is_empty() { expires } else { format!("{} · {expires}", saved.nickname.trim()) };
    ui.label(style::text(summary, 12.0, style::weight::REGULAR).color(pal.panel_dim));
    ui.label(style::text(last_used(saved.used_ms, now_ms), 12.0, style::weight::REGULAR).color(pal.panel_dim));
}

fn delete_prompt(saved: &SavedCard, ui: &mut Ui, ink: &Ink, actions: &mut Vec<Action>) {
    let prompt = if saved.readable() {
        format!("Delete the card “{}”? This can't be undone.", card_title(saved))
    } else {
        "Delete this damaged card? This can't be undone.".to_owned()
    };
    ui.label(style::text(prompt, 12.5, style::weight::REGULAR).color(ink.text));
    ui.horizontal_wrapped(|ui| {
        if in_view(Button::new("Delete").kind(Kind::Primary).size(Size::Small).show(ui, ink)).clicked() { actions.push(Action::ConfirmDelete); }
        if in_view(Button::new("Cancel").kind(Kind::Plain).size(Size::Small).show(ui, ink)).clicked() { actions.push(Action::CancelDelete); }
    });
}

/// The number shows only while its field has focus, as it is typed; else
/// it is masked. An edit starts with it blank, naming the saved one by its
/// network and last four. Only what the vault can't hold is refused: a hint
/// under an odd-looking number or a past expiry never stops Save.
fn editor_form(editor: &mut CardEditor, ui: &mut Ui, pal: &Palette, ink: &Ink, actions: &mut Vec<Action>) {
    let width = ui.available_width().min(440.0);
    section(ui, pal, if editor.id.is_some() { "Edit card" } else { "New card" });
    let hints = editor.hints(this_month());
    let mut enter = false;
    let fields: [(usize, &str, &str, f32); 5] = [
        (NUMBER, "Card number", if editor.saved.is_some() { "Unchanged" } else { "Required" }, width),
        (NAME, "Name on card", "Optional", width),
        (MONTH, "Expiry month", "MM", width.min(120.0)),
        (YEAR, "Expiry year", "YYYY", width.min(120.0)),
        (NICKNAME, "Nickname", "For example, Travel", width),
    ];
    for (field, label, hint, field_width) in fields {
        style::field_label(ui, ink, label);
        let id = card_field_id(field);
        let typing = ui.memory(|memory| memory.has_focus(id));
        let value = match field {
            NUMBER => &mut editor.number,
            NAME => &mut editor.name,
            MONTH => &mut editor.month,
            YEAR => &mut editor.year,
            _ => &mut editor.nickname,
        };
        let response = in_view(super::field(ui, ink, id, value, hint, field_width, field == NUMBER && !typing, CARD_FIELD_LIMITS[field]));
        enter |= submitted(ui, &response);
        if field == NUMBER {
            if std::mem::take(&mut editor.focus) {
                response.request_focus();
                response.scroll_to_me(None);
            }
            // egui keeps the field's state, its cursor and undo history, apart
            // from the draft. Once the number loses focus, by Escape, Enter or
            // a click elsewhere, none is kept.
            if !ui.memory(|memory| memory.has_focus(id)) {
                ui.ctx().data_mut(|data| data.remove::<egui::text_edit::TextEditState>(id));
            }
            if let Some(saved) = &editor.saved {
                ui.label(style::text(format!("Leave the number blank to keep {saved}."), 12.0, style::weight::REGULAR).color(ink.secondary));
            }
        }
        for (_, hint) in hints.iter().filter(|(hinted, _)| *hinted == field) {
            ui.label(style::text(hint.message(), 12.0, style::weight::REGULAR).color(ink.secondary));
        }
    }
    for name in CARD_FIELD_IDS { forget_undo(ui.ctx(), field_id(name)); }
    ui.label(style::text(NO_SECURITY_CODE, 12.0, style::weight::REGULAR).color(ink.secondary));
    if let Some(error) = &editor.error {
        ui.label(style::text(error, 12.5, style::weight::REGULAR).color(ink.danger));
    }
    ui.horizontal_wrapped(|ui| {
        if in_view(Button::new("Save").kind(Kind::Primary).show(ui, ink)).clicked() || enter { actions.push(Action::Save); }
        if in_view(Button::new("Cancel").kind(Kind::Plain).show(ui, ink)).clicked() { actions.push(Action::Cancel); }
    });
}
