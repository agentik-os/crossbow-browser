//! Settings → General → Updates: Automatically check for updates (this
//! device only), Check Now with the last check and its result, Release
//! Notes, the download's progress and Cancel, Restart to Update and Show
//! Download. A build without an updater says so and offers nothing else.

use super::*;
use crate::updates::{Restart, State, Updates};
use crossbow_core::store::Store;
use crossbow_core::update;

pub(super) const SECTION: &str = "Updates";

/// The group asks Crossbow to quit so the helper can install.
pub(super) struct Quit;

pub(super) fn show(updates: &mut Updates, store: &Store, browser_downloads: usize, ui: &mut Ui, pal: &Palette) -> Option<Quit> {
    let ink = Ink::new(pal.panel, pal.accent);
    if updates.reveal && !ui.is_sizing_pass() && ui.is_visible() {
        ui.scroll_to_cursor(Some(egui::Align::Min));
        updates.reveal = false;
    }
    section(ui, pal, SECTION);
    if let Some(outcome) = &updates.outcome {
        ui.label(style::text(outcome, 12.0, style::weight::REGULAR).color(ink.secondary));
    }
    let title = format!("Crossbow {}", update::VERSION);
    if !updates.has_updater() {
        row(ui, pal, &title, update::NO_UPDATER, |_| {});
        return None;
    }
    link_page::switch(&mut updates.auto, store, ui, &ink, pal);
    let (status, busy) = (updates.status(), updates.busy());
    let mut check = false;
    row(ui, pal, &title, &status, |ui| {
        check = Button::new("Check Now").kind(Kind::Plain).enabled(!busy).show(ui, &ink).clicked();
    });
    if check {
        updates.check_now();
        ui.ctx().request_repaint();
    }
    if let Some(line) = updates.last_line() {
        ui.label(style::text(line, 12.0, style::weight::REGULAR).color(ink.secondary));
    }
    let notes = match &updates.state {
        State::Available(release) | State::Downloading { release, .. } => Some(release.notes.clone()),
        State::Ready { ready, .. } | State::Waiting(ready) => Some(ready.release.notes.clone()),
        _ => None,
    };
    let button = match &updates.state {
        State::Available(_) => Some(("Download", Kind::Primary, Click::Download)),
        State::Downloading { .. } | State::Waiting(_) => Some(("Cancel", Kind::Plain, Click::Cancel)),
        State::Ready { refusal: None, .. } if updates.asking.is_none() => Some(("Restart to Update", Kind::Primary, Click::Restart)),
        State::Ready { refusal: Some(_), .. } => Some(("Show Download", Kind::Plain, Click::Reveal)),
        _ => None,
    };
    let mut click = None;
    if let Some((label, kind, action)) = button {
        ui.horizontal_wrapped(|ui| {
            if Button::new(label).kind(kind).show(ui, &ink).clicked() {
                click = Some(action);
            }
        });
    }
    if let Some(count) = updates.asking {
        let downloads = if count == 1 { "1 download is".to_string() } else { format!("{count} downloads are") };
        ui.label(style::text(format!("{downloads} in progress. Restarting now cancels them."), 12.0, style::weight::REGULAR).color(ink.danger));
        ui.horizontal_wrapped(|ui| {
            for (label, kind, action) in [("Restart Now", Kind::Primary, Click::RestartNow), ("Wait for Downloads", Kind::Plain, Click::Wait), ("Cancel", Kind::Plain, Click::Cancel)] {
                if Button::new(label).kind(kind).show(ui, &ink).clicked() {
                    click = Some(action);
                }
            }
        });
    }
    if let Some(notes) = notes {
        let label = if updates.show_notes { "Hide Release Notes" } else { "Release Notes" };
        if Button::new(label).kind(Kind::Plain).show(ui, &ink).clicked() {
            updates.show_notes = !updates.show_notes;
        }
        if updates.show_notes {
            ui.label(style::text(notes, 12.0, style::weight::REGULAR).color(ink.secondary));
        }
    }
    if updates.busy() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
    let restart = |updates: &mut Updates, answered| matches!(updates.restart(browser_downloads, answered), Restart::Quit).then_some(Quit);
    match click? {
        Click::Download => updates.download(),
        Click::Cancel => updates.cancel(),
        Click::Reveal => updates.reveal_download(),
        Click::Wait => updates.wait_for_downloads(),
        Click::Restart => return restart(updates, false),
        Click::RestartNow => return restart(updates, true),
    }
    ui.ctx().request_repaint();
    None
}

#[derive(Clone, Copy)]
enum Click {
    Download,
    Cancel,
    Reveal,
    Restart,
    RestartNow,
    Wait,
}

#[cfg(test)]
#[path = "update_settings_tests.rs"]
mod tests;
