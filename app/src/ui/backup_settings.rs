//! Settings → Advanced → Backups: this Mac's library backups and Restore Data.

use super::*;
use crossbow_core::backup::{self, Backup, Check, Failure, RestoreError};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

pub(crate) const KEPT_HERE: &str = "Backups are kept on this Mac only.";
pub(crate) const SCOPE: &str = "A backup holds Spaces, profiles, pins, folders, favorites, tabs, the archive, history, settings and sync state. Saved passwords, website data such as cookies and site storage, downloads and captures aren't backed up.";
pub(crate) const NEWER: &str = "This backup was made by a newer Crossbow.";
pub(crate) const DAMAGED: &str = "Damaged: this backup can't be read, so it can't be restored.";
pub(crate) const NONE_YET: &str = "No backups yet. Crossbow makes one when it opens, once a day, and before big changes.";
pub(crate) const AFTER: &str = "Sync turns off on this Mac until you turn it on again. Site permissions and links to other apps stay as they are on this Mac. Crossbow quits to finish; open it again to see the restored library.";
pub(crate) const WAITING: &str = "A restore is ready. It finishes when Crossbow next opens.";
const CHECKING: &str = "Checking…";
const DELETE_ALL: &str = "Delete All Backups…";

/// The Restore Data panel: the backups, each one's check, and the one chosen.
#[derive(Default)]
pub(crate) struct Restore {
    open: bool,
    backups: Vec<Backup>,
    failures: Vec<Failure>,
    checks: HashMap<PathBuf, Check>,
    receiver: Option<mpsc::Receiver<(PathBuf, Check)>>,
    /// The backup waiting for "Restore and Quit".
    confirm: Option<PathBuf>,
    error: Option<String>,
    /// Scroll the confirmation or the error into view once.
    reveal: bool,
    /// Delete All Backups… asks first: how many backups it deletes.
    delete: Option<usize>,
    /// What Delete All Backups did.
    deleted: Option<(String, bool)>,
}

impl Restore {
    /// A confirmation is open: Escape cancels it, not Settings.
    pub(crate) fn owns_keyboard(&self) -> bool {
        self.confirm.is_some() || self.delete.is_some()
    }

    /// List the backups and check each on its own thread, read-only.
    fn load(&mut self, store: &crossbow_core::store::Store, library: &Path) {
        self.backups = backup::list(library);
        self.failures = backup::failures(store);
        self.checks.clear();
        let (sender, receiver) = mpsc::channel();
        let paths: Vec<PathBuf> = self.backups.iter().map(|backup| backup.path.clone()).collect();
        let spawned = std::thread::Builder::new().name("crossbow-backup-check".into()).spawn(move || {
            for path in paths {
                let check = backup::inspect(&path);
                if sender.send((path, check)).is_err() { return; }
            }
        });
        match spawned {
            Ok(_) => self.receiver = Some(receiver),
            Err(_) => for backup in &self.backups { self.checks.insert(backup.path.clone(), backup::inspect(&backup.path)); },
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(receiver) = &self.receiver else { return };
        loop {
            match receiver.try_recv() {
                Ok((path, check)) => { self.checks.insert(path, check); }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                    return;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.receiver = None;
                    return;
                }
            }
        }
    }
}

/// What the panel asks of the app.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Request {
    /// A restore waits for the next launch: quit now.
    Quit,
    /// Show this file or folder in Finder.
    Reveal(PathBuf),
}

fn preview_text(check: Option<&Check>) -> (String, bool) {
    match check {
        None => (CHECKING.into(), false),
        Some(Check::Newer) => (NEWER.into(), true),
        Some(Check::Damaged) => (DAMAGED.into(), true),
        Some(Check::Ready(p)) => (format!(
            "{} · {} · {} · {} · {} · {}",
            count(p.spaces, "Space", "Spaces"), count(p.profiles, "profile", "profiles"), count(p.pins, "pin", "pins"),
            count(p.folders, "folder", "folders"), count(p.tabs, "tab", "tabs"), count(p.archived, "archived tab", "archived tabs"),
        ), false),
    }
}

fn count(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn restore_error(error: RestoreError) -> String {
    match error {
        RestoreError::Backup(error) => format!("Couldn't back up this Mac's library first, so nothing was restored. {error}"),
        RestoreError::Refused(Check::Newer) => NEWER.into(),
        RestoreError::Refused(_) => DAMAGED.into(),
        RestoreError::Stage(error) => format!("Couldn't prepare the restore, so nothing changed. {error}"),
        RestoreError::Clock => backup::CLOCK_LIMIT.into(),
    }
}

fn reveal(ui: &Ui, response: &egui::Response, pending: &mut bool) {
    if *pending && !ui.is_sizing_pass() && ui.is_visible() {
        response.scroll_to_me_animation(Some(egui::Align::Center), egui::style::ScrollAnimation::none());
        *pending = false;
    }
}

/// The Backups section. `launch_error` says why a restore didn't finish at launch.
pub(super) fn show(st: &mut Restore, lib: &Library, launch_error: Option<&str>, ui: &mut Ui, pal: &Palette) -> Option<Request> {
    let ink = Ink::new(pal.panel, pal.accent);
    let library = lib.store.path()?;
    let text = |ui: &mut Ui, message: &str, color: Color32| { ui.label(style::text(message, 12.5, style::weight::REGULAR).color(color)); };
    section(ui, pal, "Backups");
    if let Some(record) = backup::restored(&lib.store) {
        text(ui, &format!("Restored from the backup from {}.", crate::backups::date(record.backup_ms)), ink.text);
        for id in &record.fresh_profiles {
            let name = lib.profiles.get(id).map_or("a profile", |profile| profile.name.as_str());
            text(ui, &format!("Website data for “{name}” wasn't on this Mac, so it starts empty. Sites may ask you to sign in again."), ink.secondary);
        }
    }
    let mut request = None;
    if let Some(error) = launch_error {
        text(ui, error, ink.danger);
        if Button::new("Reveal in Finder").kind(Kind::Plain).show(ui, &ink).clicked() { request = Some(Request::Reveal(library.clone())); }
    }
    if backup::pending(&library).is_some() {
        text(ui, WAITING, ink.text);
        if Button::new("Cancel Restore").kind(Kind::Plain).show(ui, &ink).clicked() {
            st.error = backup::cancel_pending(&library).err();
        }
    }
    row(ui, pal, "Restore Data", &format!("Replace this Mac's library with a backup. {KEPT_HERE}"), |ui| {
        if !st.open && Button::new("Restore Data…").show(ui, &ink).clicked() {
            st.open = true;
            st.load(&lib.store, &library);
        }
    });
    if st.open { request = panel(st, lib, &library, ui, pal, ink).or(request); }
    delete_all(st, lib, &library, ui, pal, ink).or(request)
}

/// The open Restore Data panel: the backups, each one's check, the one
/// chosen, and the backups that failed.
fn panel(st: &mut Restore, lib: &Library, library: &Path, ui: &mut Ui, pal: &Palette, ink: Ink) -> Option<Request> {
    let text = |ui: &mut Ui, message: &str, color: Color32| { ui.label(style::text(message, 12.5, style::weight::REGULAR).color(color)); };
    st.poll(ui.ctx());
    if st.confirm.is_some() && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        st.confirm = None;
        st.error = None;
    }
    let mut request = None;
    let mut close = false;
    let frame = egui::Frame::new().fill(ink.secondary.gamma_multiply(0.06)).stroke(Stroke::new(1.0, pal.panel_border)).corner_radius(style::radius::SM as u8).inner_margin(egui::Margin::symmetric(12, 10));
    frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(style::text("Restore Data", 13.0, style::weight::SEMIBOLD).color(ink.text));
        text(ui, KEPT_HERE, ink.text);
        text(ui, SCOPE, ink.secondary);
        if st.backups.is_empty() { text(ui, NONE_YET, ink.secondary); }
        for (i, backup) in st.backups.clone().iter().enumerate() {
            ui.push_id(("backup", i), |ui| {
                ui.add_space(4.0);
                let title = format!("{} · {} · {}", crate::backups::date(backup.created_ms), backup.reason.label(), crate::ui::overlays::human_size(backup.bytes as i64));
                ui.label(style::text(title, 12.5, style::weight::SEMIBOLD).color(ink.text));
                let check = st.checks.get(&backup.path).copied();
                let (preview, bad) = preview_text(check.as_ref());
                text(ui, &preview, if bad { ink.danger } else { ink.secondary });
                if st.confirm.as_ref() == Some(&backup.path) {
                    let confirmation = ui.vertical(|ui| {
                        text(ui, &format!("Replace this Mac's library with the backup from {}? Your current library is backed up first.", crate::backups::date(backup.created_ms)), ink.text);
                        text(ui, AFTER, ink.secondary);
                        ui.horizontal_wrapped(|ui| {
                            if Button::new("Restore and Quit").kind(Kind::Primary).show(ui, &ink).clicked() {
                                match backup::restore(&lib.store, backup) {
                                    Ok(()) => {
                                        st.confirm = None;
                                        st.error = None;
                                        request = Some(Request::Quit);
                                    }
                                    Err(error) => {
                                        st.error = Some(restore_error(error));
                                        st.reveal = true;
                                    }
                                }
                            }
                            if Button::new("Cancel").kind(Kind::Plain).show(ui, &ink).clicked() {
                                st.confirm = None;
                                st.error = None;
                            }
                        });
                        if let Some(error) = &st.error { text(ui, error, ink.danger); }
                    }).response;
                    reveal(ui, &confirmation, &mut st.reveal);
                } else if matches!(check, Some(Check::Ready(_))) && Button::new("Restore…").show(ui, &ink).clicked() {
                    st.confirm = Some(backup.path.clone());
                    st.delete = None;
                    st.error = None;
                    st.reveal = true;
                }
            });
        }
        for failure in &st.failures {
            let reason = failure.reason().map_or("Backup", |reason| reason.label());
            text(ui, &format!("{} · {reason} · Failed: {}", crate::backups::date(failure.at_ms), failure.error), ink.danger);
        }
        if !st.failures.is_empty() && Button::new("Reveal in Finder").kind(Kind::Plain).show(ui, &ink).clicked() {
            let folder = backup::folder(library);
            request = Some(Request::Reveal(if folder.exists() { folder } else { library.to_path_buf() }));
        }
        if st.confirm.is_none() {
            if let Some(error) = &st.error { text(ui, error, ink.danger); }
        }
        ui.add_space(4.0);
        close = Button::new("Close").kind(Kind::Plain).show(ui, &ink).clicked();
    });
    if close {
        let (delete, deleted) = (st.delete.take(), st.deleted.take());
        *st = Restore { delete, deleted, ..Restore::default() };
    }
    request
}

/// Delete All Backups…: every backup on this Mac and a restore waiting for
/// the next launch, once the user confirms.
fn delete_all(st: &mut Restore, lib: &Library, library: &Path, ui: &mut Ui, pal: &Palette, ink: Ink) -> Option<Request> {
    let text = |ui: &mut Ui, message: &str, color: Color32| { ui.label(style::text(message, 12.5, style::weight::REGULAR).color(color)); };
    if st.delete.is_some() && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        st.delete = None;
    }
    row(ui, pal, "Delete Backups", "Remove every backup of this library from this Mac.", |ui| {
        if st.delete.is_none() && ui.button(RichText::new(DELETE_ALL).color(ink.danger)).clicked() {
            let count = backup::list(library).len();
            (st.confirm, st.error, st.deleted) = (None, None, None);
            if count == 0 && backup::pending(library).is_none() {
                st.deleted = Some(("There are no backups to delete.".into(), false));
            } else {
                st.delete = Some(count);
                st.reveal = true;
            }
        }
    });
    let mut request = None;
    if let Some(count) = st.delete {
        let waiting = backup::pending(library).is_some();
        let mut removed = None;
        let confirmation = egui::Frame::new().fill(ink.danger.gamma_multiply(0.10)).stroke(Stroke::new(1.0, ink.danger.gamma_multiply(0.45))).corner_radius(style::radius::SM as u8).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            let (ask, after) = match count {
                0 => ("Cancel the restore waiting for the next launch?".to_string(), "Its copy of the backup is deleted."),
                1 => ("Delete the backup on this Mac?".to_string(), "It can't be restored afterwards."),
                n => (format!("Delete all {n} backups on this Mac?"), "They can't be restored afterwards."),
            };
            ui.label(style::text(ask, 13.0, style::weight::SEMIBOLD).color(ink.text));
            match (count, waiting) {
                (1.., true) => text(ui, &format!("{after} The restore waiting for the next launch is cancelled too."), ink.text),
                _ => text(ui, after, ink.text),
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button(RichText::new("Delete All Backups").color(ink.danger)).clicked() {
                    removed = Some(backup::delete_all(library));
                }
                if Button::new("Cancel").kind(Kind::Plain).show(ui, &ink).clicked() { st.delete = None; }
            });
        }).response;
        reveal(ui, &confirmation, &mut st.reveal);
        if let Some(removed) = removed {
            let deleted = match removed.deleted {
                0 => "Cancelled the restore waiting for the next launch.".to_string(),
                1 => "Deleted 1 backup.".to_string(),
                n => format!("Deleted {n} backups."),
            };
            st.deleted = Some(match removed.left {
                0 => (deleted, false),
                1 => (format!("{deleted} 1 couldn't be deleted, so it is still in the Crossbow data folder."), true),
                n => (format!("{deleted} {n} couldn't be deleted, so they are still in the Crossbow data folder."), true),
            });
            st.delete = None;
            if st.open { st.load(&lib.store, library); }
        }
    }
    if let Some((message, failed)) = &st.deleted {
        text(ui, message, if *failed { ink.danger } else { ink.secondary });
        if *failed && Button::new("Reveal in Finder").kind(Kind::Plain).show(ui, &ink).clicked() {
            request = Some(Request::Reveal(backup::folder(library)));
        }
    }
    request
}

#[cfg(test)]
#[path = "backup_settings_tests.rs"]
mod tests;
