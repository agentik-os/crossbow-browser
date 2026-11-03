//! Settings → Passwords → Addresses: the active profile's saved addresses,
//! with Add, Edit and Delete behind a confirmation. Readable only while the
//! vault is unlocked; nothing here is logged.

use super::*;
use crate::passwords::{ADDRESS_FIELD_IDS, AddressEditor, NO_PROFILE, address_field_id, address_hints, address_summary, address_title};
use crossbow_core::vault::{AddressField, SavedAddress};

const DESC: &str = "Kept in the vault for this profile, and filled into a page only when you choose one from the key button or a field's menu. \"Offer to save passwords from websites\" below also offers to save addresses. Addresses aren't synced.";
const EMPTY: &str = "No saved addresses in this profile yet.";

/// What a click asked for, applied after the list is drawn.
enum Action {
    Edit(String),
    Delete(String),
    ConfirmDelete,
    CancelDelete,
    Save,
    Cancel,
}

pub(super) fn addresses(state: &mut Passwords, ui: &mut Ui, pal: &Palette, ink: &Ink) {
    let mut add = false;
    let has_profile = !state.current_profile().is_empty();
    row(ui, pal, "Saved addresses", DESC, |ui| {
        add = in_view(Button::new("Add Address").kind(Kind::Primary).enabled(has_profile).show(ui, ink)).clicked();
    });
    if !has_profile {
        ui.label(style::text(NO_PROFILE, 12.5, style::weight::REGULAR).color(ink.secondary));
        return;
    }
    if add { state.start_add_address(); }
    let mut actions = Vec::new();
    if let Some(editor) = state.page.address_editor.as_mut().filter(|editor| editor.id.is_none()) {
        editor_form(editor, ui, pal, ink, &mut actions);
        separator(ui, pal);
    }
    let listed = state.listed_addresses();
    if listed.is_empty() {
        ui.label(style::text(EMPTY, 12.5, style::weight::REGULAR).color(ink.secondary));
    }
    let now = crossbow_core::now_ms();
    let ids: Vec<String> = listed.iter().map(|saved| saved.id.clone()).collect();
    for id in ids {
        let (vault, page) = state.parts();
        let Some(saved) = vault.and_then(|vault| vault.saved_address(&id)) else { continue };
        ui.push_id(("address", id.as_str()), |ui| {
            header(saved, now, ui, pal, ink);
            if page.confirm_delete_address.as_deref() == Some(id.as_str()) {
                delete_prompt(saved, ui, ink, &mut actions);
            } else if let Some(editor) = page.address_editor.as_mut().filter(|editor| editor.id.as_deref() == Some(id.as_str())) {
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
            Action::Edit(id) => state.start_edit_address(&id),
            Action::Delete(id) => state.request_delete_address(&id),
            Action::ConfirmDelete => state.confirm_delete_address(),
            Action::CancelDelete => state.page.confirm_delete_address = None,
            Action::Save => state.save_address_editor(),
            Action::Cancel => state.close_address_editor(),
        }
    }
}

fn header(saved: &SavedAddress, now_ms: u64, ui: &mut Ui, pal: &Palette, ink: &Ink) {
    if !saved.readable() {
        ui.label(style::text("Damaged address", 13.5, style::weight::REGULAR).color(pal.panel_text));
        ui.label(style::text(VaultError::Unreadable.to_string(), 12.0, style::weight::REGULAR).color(ink.danger));
        return;
    }
    ui.label(style::text(address_title(&saved.address), 13.5, style::weight::REGULAR).color(pal.panel_text));
    let summary = address_summary(&saved.address);
    if !summary.is_empty() { ui.label(style::text(summary, 12.0, style::weight::REGULAR).color(pal.panel_dim)); }
    ui.label(style::text(last_used(saved.used_ms, now_ms), 12.0, style::weight::REGULAR).color(pal.panel_dim));
}

fn delete_prompt(saved: &SavedAddress, ui: &mut Ui, ink: &Ink, actions: &mut Vec<Action>) {
    let prompt = if saved.readable() {
        format!("Delete the address “{}”? This can't be undone.", address_title(&saved.address))
    } else {
        "Delete this damaged address? This can't be undone.".to_owned()
    };
    ui.label(style::text(prompt, 12.5, style::weight::REGULAR).color(ink.text));
    ui.horizontal_wrapped(|ui| {
        if in_view(Button::new("Delete").kind(Kind::Primary).size(Size::Small).show(ui, ink)).clicked() { actions.push(Action::ConfirmDelete); }
        if in_view(Button::new("Cancel").kind(Kind::Plain).size(Size::Small).show(ui, ink)).clicked() { actions.push(Action::CancelDelete); }
    });
}

/// Every part is optional, and only what the vault can't hold is refused:
/// a hint under an odd-looking part never stops Save.
fn editor_form(editor: &mut AddressEditor, ui: &mut Ui, pal: &Palette, ink: &Ink, actions: &mut Vec<Action>) {
    let width = ui.available_width().min(440.0);
    section(ui, pal, if editor.id.is_some() { "Edit address" } else { "New address" });
    let hints = address_hints(&editor.address);
    let mut enter = false;
    for field in AddressField::ALL {
        style::field_label(ui, ink, field.name());
        let id = address_field_id(field);
        let value = editor.address.part_mut(field);
        let response = in_view(if field == AddressField::Street {
            ui.add(egui::TextEdit::multiline(&mut **value).id(id).hint_text("One line each").char_limit(field.limit()).desired_rows(2).desired_width(width))
        } else {
            let hint = if field == AddressField::Label { "For example, Home" } else { "Optional" };
            let response = super::field(ui, ink, id, value, hint, width, false, field.limit());
            enter |= submitted(ui, &response);
            response
        });
        if field == AddressField::Label && std::mem::take(&mut editor.focus) {
            response.request_focus();
            response.scroll_to_me(None);
        }
        for (_, hint) in hints.iter().filter(|(hinted, _)| *hinted == field) {
            ui.label(style::text(*hint, 12.0, style::weight::REGULAR).color(ink.secondary));
        }
    }
    for name in ADDRESS_FIELD_IDS { forget_undo(ui.ctx(), field_id(name)); }
    if let Some(error) = &editor.error {
        ui.label(style::text(error, 12.5, style::weight::REGULAR).color(ink.danger));
    }
    ui.horizontal_wrapped(|ui| {
        if in_view(Button::new("Save").kind(Kind::Primary).show(ui, ink)).clicked() || enter { actions.push(Action::Save); }
        if in_view(Button::new("Cancel").kind(Kind::Plain).show(ui, ink)).clicked() { actions.push(Action::Cancel); }
    });
}
