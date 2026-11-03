//! Settings → General → Browsing → New Document opens in: the app New
//! Document opens for the active Space's profile, on this device. A named
//! service is saved at once; Custom… asks for an address and saves it only
//! once it passes the checks. A refused address or a failed save keeps the
//! previous choice and says so.

use super::link_page::save_failure;
use super::{keep_popup_above_sheet, row};
use crate::quit::SaveFailure;
use crate::ui::style::{self, Button, Ink, Kind};
use crate::ui::theme::Palette;
use crossbow_core::document_app::{self, DocumentApp};
use crossbow_core::library::Library;
use egui::{Id, Key, Modifiers, Ui};

pub(crate) const TITLE: &str = "New Document opens in";
pub(crate) const DESCRIPTION: &str = "New Document, in the command bar or with a shortcut you set, opens a new tab here. This Mac only.";
pub(crate) const CUSTOM: &str = "Custom…";
pub(crate) const HINT: &str = "https://example.com/new";

#[derive(Debug, Default)]
pub(super) struct Editor {
    profile: Option<String>,
    saved: DocumentApp,
    /// Why the saved choice was not read; cleared by the next save.
    notice: Option<&'static str>,
    /// The custom address being typed. Escape or Cancel drops it.
    draft: Option<String>,
    focus_field: bool,
    refused: Option<&'static str>,
    failure: Option<SaveFailure<DocumentApp>>,
    /// Bring the row into view and focus its menu: New Document had no app.
    reveal: bool,
}

impl Editor {
    /// Typing an address: Escape cancels it instead of closing Settings.
    pub(super) fn owns_keyboard(&self) -> bool {
        self.draft.is_some()
    }

    /// Leaving the page drops a typed address; the choice is read again on return.
    pub(super) fn leave(&mut self) {
        *self = Editor { reveal: self.reveal, ..Editor::default() };
    }

    pub(super) fn reveal(&mut self) {
        self.reveal = true;
    }

    fn load(&mut self, lib: &Library, profile: &str) {
        if self.profile.as_deref() == Some(profile) { return; }
        let (saved, notice) = document_app::load(&lib.store, profile);
        *self = Editor { profile: Some(profile.to_owned()), saved, notice, reveal: self.reveal, ..Editor::default() };
    }

    /// Save `app`. On failure the previous choice stays in use, a typed
    /// address stays in its field, and `failure` offers it again.
    fn choose(&mut self, lib: &Library, profile: &str, app: DocumentApp) {
        match document_app::save(&lib.store, profile, &app) {
            Ok(()) => {
                self.saved = app;
                self.notice = None;
                self.draft = None;
                self.refused = None;
                self.failure = None;
            }
            Err(error) => self.failure = Some(SaveFailure { wanted: app, error: error.to_string(), reveal: true }),
        }
    }

    fn kept(&self) -> String {
        match &self.saved {
            DocumentApp::None => "New Document still asks for an app".into(),
            DocumentApp::Custom(_) => "New Document still opens your custom address".into(),
            app => format!("New Document still opens {}", app.label()),
        }
    }
}

pub(super) fn show(ed: &mut Editor, lib: &Library, profile: &str, ui: &mut Ui, pal: &Palette) {
    let ink = Ink::new(pal.panel, pal.accent);
    ed.load(lib, profile);
    if ed.owns_keyboard() && !egui::Popup::is_any_open(ui.ctx()) && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
        ed.draft = None;
        ed.refused = None;
    }
    // Some(app) to save; Some(None) for Custom….
    let mut picked: Option<Option<DocumentApp>> = None;
    let mut menu = None;
    let shown = if ed.draft.is_some() { DocumentApp::Custom(String::new()).label() } else { ed.saved.label() };
    row(ui, pal, TITLE, DESCRIPTION, |ui| {
        let response = egui::ComboBox::from_id_salt(("document-app", profile)).selected_text(shown).width(150.0).show_ui(ui, |ui| {
            keep_popup_above_sheet(ui);
            for app in std::iter::once(DocumentApp::None).chain(DocumentApp::KNOWN) {
                if ui.selectable_label(ed.draft.is_none() && ed.saved == app, app.label()).clicked() {
                    picked = Some(Some(app));
                    // A keyboard choice closes the menu too, as a click does.
                    ui.close();
                }
            }
            let custom = ed.draft.is_some() || matches!(ed.saved, DocumentApp::Custom(_));
            if ui.selectable_label(custom, CUSTOM).clicked() {
                picked = Some(None);
                ui.close();
            }
        }).response;
        menu = Some(response);
    });
    if ed.reveal && !ui.is_sizing_pass() && ui.is_visible() && let Some(menu) = &menu {
        menu.scroll_to_me_animation(Some(egui::Align::Center), egui::style::ScrollAnimation::none());
        menu.request_focus();
        ed.reveal = false;
    }
    match picked {
        Some(Some(app)) => {
            ed.draft = None;
            ed.refused = None;
            if app != ed.saved { ed.choose(lib, profile, app); }
            ui.ctx().request_repaint();
        }
        Some(None) => {
            ed.draft = Some(match &ed.saved { DocumentApp::Custom(address) => address.clone(), _ => String::new() });
            ed.refused = None;
            ed.focus_field = true;
            ui.ctx().request_repaint();
        }
        None => {}
    }
    if let Some(notice) = ed.notice {
        ui.label(style::text(notice, 12.0, style::weight::REGULAR).color(ink.danger));
    }
    if let (DocumentApp::Custom(address), None) = (&ed.saved, &ed.draft) {
        ui.add(egui::Label::new(style::text(address, 12.0, style::weight::REGULAR).color(ink.secondary)).truncate()).on_hover_text(address);
    }
    let kept = ed.kept();
    if let Some(failure) = ed.failure.as_mut() {
        let (retry, cancel) = save_failure(ui, &ink, "document-app", TITLE, &failure.error, &kept, &mut failure.reveal);
        if cancel {
            ed.failure = None;
            ed.draft = None;
            ed.refused = None;
        } else if retry {
            let wanted = failure.wanted.clone();
            ed.choose(lib, profile, wanted);
            ui.ctx().request_repaint();
        }
    }
    let Some(draft) = ed.draft.as_mut() else { return };
    let width = ui.available_width().min(420.0);
    let field = ui.add(egui::TextEdit::singleline(draft).id(Id::unique(("document-app-address", profile))).hint_text(HINT).desired_width(width));
    let mut submit = field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
    if field.changed() { ed.refused = None; }
    let mut shown = field.rect;
    if let Some(reason) = ed.refused {
        shown = shown.union(ui.label(style::text(format!("{reason} {kept}."), 12.0, style::weight::REGULAR).color(ink.danger)).rect);
    }
    if ed.focus_field {
        field.request_focus();
        ui.scroll_to_rect(shown, None);
        ed.focus_field = false;
    }
    let mut cancel = false;
    ui.horizontal_wrapped(|ui| {
        submit |= Button::new("Save").kind(Kind::Primary).show(ui, &ink).clicked();
        cancel = Button::new("Cancel").kind(Kind::Plain).show(ui, &ink).clicked();
    });
    if cancel {
        ed.draft = None;
        ed.refused = None;
    } else if submit {
        let typed = ed.draft.clone().unwrap_or_default();
        match document_app::custom_address(&typed) {
            Ok(address) => ed.choose(lib, profile, DocumentApp::Custom(address)),
            Err(reason) => {
                ed.refused = Some(reason);
                ed.focus_field = true;
            }
        }
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
#[path = "document_app_settings_tests.rs"]
mod tests;
