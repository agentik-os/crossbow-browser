//! Settings → General → Appearance → Website appearance: Automatic, Light
//! or Dark for the active Space's profile, on this device. It follows the
//! sidebar rows, under their profile caption. A failed save keeps the
//! previous choice in use and offers it again.

use super::{row, segmented};
use crate::ui::style::{self, Ink};
use crate::ui::theme::Palette;
use crate::website_appearance::{Appearance, CHOICES, DESCRIPTION, TITLE, WebsiteAppearance, label};
use crossbow_core::library::Library;
use egui::Ui;

pub(super) fn show(appearance: &mut WebsiteAppearance, lib: &Library, space: &str, ui: &mut Ui, pal: &Palette) {
    let Some(profile) = crate::downloads::active_profile(lib, space) else { return };
    let ink = Ink::new(pal.panel, pal.accent);
    let current = appearance.get(&lib.store, profile).choice();
    let mut choice = current;
    let options = CHOICES.map(|choice| (choice, label(choice)));
    row(ui, pal, TITLE, DESCRIPTION, |ui| {
        segmented(ui, pal, &mut choice, &options);
    });
    if choice != current {
        appearance.choose(&lib.store, profile, choice);
        ui.ctx().request_repaint();
    }
    let saved = appearance.get(&lib.store, profile);
    if let Some(notice) = &saved.notice {
        ui.label(style::text(notice, 12.0, style::weight::REGULAR).color(ink.danger));
    }
    let kept = match saved.choice() {
        Appearance::Automatic => "Websites still follow the system",
        Appearance::Light => "Websites still show their light version",
        Appearance::Dark => "Websites still show their dark version",
    };
    let Some(failure) = saved.failure.as_mut() else { return };
    let (retry, cancel) = super::link_page::save_failure(ui, &ink, "website-appearance", TITLE, &failure.error, kept, &mut failure.reveal);
    if cancel {
        saved.failure = None;
    } else if retry {
        let wanted = failure.wanted;
        appearance.choose(&lib.store, profile, wanted);
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
#[path = "website_appearance_settings_tests.rs"]
mod tests;
