//! Settings → General → Capture: whether Command-Shift drag on a page
//! captures, for this device, and whether captures are kept in Captures, for
//! each profile. Both are kept on this device only.

use super::section;
use crate::app::capture_host::CapturePrefs;
use crate::ui::style::{self, Ink};
use crate::ui::theme::Palette;
use crossbow_core::library::Library;
use egui::Ui;

pub(super) fn show(prefs: &mut CapturePrefs, lib: &Library, space: &str, ui: &mut Ui, pal: &Palette) {
    let Some(profile) = crate::downloads::active_profile(lib, space) else { return };
    let ink = Ink::new(pal.panel, pal.accent);
    section(ui, pal, "Capture");
    ui.label(style::text(format!("For {} · this device only", lib.profiles[profile].name), 12.0, style::weight::REGULAR).color(ink.secondary));
    super::link_page::switch(prefs.save_switch(&lib.store, profile), &lib.store, ui, &ink, pal);
    super::link_page::switch(prefs.drag_switch(&lib.store), &lib.store, ui, &ink, pal);
}

#[cfg(test)]
#[path = "capture_settings_tests.rs"]
mod tests;
