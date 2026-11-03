//! Settings → Privacy → Content Blocking: the active Space's profile's two
//! switches, each with the lists that come with the app (Turn Off or Turn
//! On, and their attribution), the lists added from files (Remove), and Add
//! List from File…, how many cosmetic rules are past the bounds, then its
//! sites with blocking off, each with Remove.

use super::{row, section};
use crate::content_blocking::{ADD_LIST, ContentBlocking, INCLUDED, LOADING, ListState, NO_LISTS, SITES_OFF, TURNED_OFF, counts, skipped_note};
use crate::ui::style::{self, Button, Ink, Kind, Size};
use crate::ui::theme::Palette;
use crossbow_core::content_blocking::lists::ListKind;
use crossbow_core::library::Library;
use crossbow_core::store::Store;
use egui::Ui;

pub(super) fn show(host: &mut ContentBlocking, lib: &Library, space: &str, ui: &mut Ui, pal: &Palette) {
    let Some(profile) = crate::downloads::active_profile(lib, space).map(str::to_owned) else { return };
    let ink = Ink::new(pal.panel, pal.accent);
    let top = ui.cursor().top();
    section(ui, pal, "Content Blocking");
    if std::mem::take(&mut host.reveal) {
        let heading = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=ui.cursor().top());
        ui.scroll_to_rect_animation(heading, Some(egui::Align::TOP), egui::style::ScrollAnimation::none());
    }
    ui.label(style::text(format!("For {} · this device only", lib.profiles[&profile].name), 12.0, style::weight::REGULAR).color(ink.secondary));
    if let Some(notice) = host.profile(&lib.store, &profile).notice() {
        ui.add(egui::Label::new(style::text(notice, 12.0, style::weight::REGULAR).color(ink.danger)).wrap());
    }
    for kind in [ListKind::Ads, ListKind::CookieBanners] {
        let blocking = host.profile(&lib.store, &profile);
        let was = blocking.enabled(kind);
        match kind {
            ListKind::Ads => super::link_page::switch(&mut blocking.ads, &lib.store, ui, &ink, pal),
            ListKind::CookieBanners => super::link_page::switch(&mut blocking.cookies, &lib.store, ui, &ink, pal),
        }
        if blocking.enabled(kind) != was { host.apply(&profile, ui.ctx()); }
        ui.push_id(("content-blocking", kind_key(kind)), |ui| group(host, &lib.store, &profile, kind, ui, &ink, pal));
    }
    let skipped = host.profile(&lib.store, &profile).hiding_skipped();
    if skipped > 0 {
        ui.add(egui::Label::new(style::text(skipped_note(skipped), 12.5, style::weight::REGULAR).color(ink.secondary)).wrap());
    }
    sites(host, &lib.store, &profile, ui, &ink, pal);
}

fn kind_key(kind: ListKind) -> &'static str {
    match kind {
        ListKind::Ads => "ads",
        ListKind::CookieBanners => "cookie-banners",
    }
}

/// One switch's lists, the result of the last change to them, and its Add
/// List from File…. Nothing changes while a list is being added.
fn group(host: &mut ContentBlocking, store: &Store, profile: &str, kind: ListKind, ui: &mut Ui, ink: &Ink, pal: &Palette) {
    let choosing = host.choosing();
    let blocking = host.profile(store, profile);
    let busy = blocking.adding().is_some();
    let bundled = blocking.bundled(kind);
    let mut turn = None;
    for list in &bundled {
        let (detail, failed) = match &list.state {
            _ if !list.on => (TURNED_OFF.to_owned(), false),
            ListState::Off => (INCLUDED.to_owned(), false),
            ListState::Loading => (LOADING.to_owned(), false),
            ListState::Failed(error) => (error.clone(), true),
            ListState::Ready => (list.counts.map_or_else(String::new, |(rules, unsupported)| counts(rules, unsupported)), false),
        };
        ui.push_id(("content-blocking-bundled", list.id), |ui| {
            row(ui, &row_palette(pal, ink, failed), list.name, &detail, |ui| {
                let label = if list.on { "Turn Off" } else { "Turn On" };
                if Button::new(label).kind(Kind::Secondary).size(Size::Small).enabled(!busy).show(ui, ink).clicked() { turn = Some((list.id, !list.on)); }
            });
        });
    }
    // One attribution for the lists that share it.
    let mut credits: Vec<&str> = Vec::new();
    for list in &bundled {
        if !credits.contains(&list.credit) { credits.push(list.credit); }
    }
    for credit in credits {
        ui.add(egui::Label::new(style::text(credit, 12.0, style::weight::REGULAR).color(ink.secondary)).wrap());
    }
    let mut remove = None;
    for (entry, state) in blocking.lists(kind) {
        let (detail, failed) = match state {
            ListState::Loading => (LOADING.to_owned(), false),
            ListState::Failed(error) => (error, true),
            ListState::Off | ListState::Ready => (counts(entry.rules, entry.unsupported), false),
        };
        ui.push_id(&entry.id, |ui| {
            row(ui, &row_palette(pal, ink, failed), &entry.name, &detail, |ui| {
                if Button::new("Remove").kind(Kind::Secondary).size(Size::Small).enabled(!busy).show(ui, ink).clicked() { remove = Some(entry.id.clone()); }
            });
        });
    }
    if blocking.lists(kind).is_empty() && !bundled.iter().any(|list| list.on) && blocking.enabled(kind) {
        ui.label(style::text(NO_LISTS, 12.5, style::weight::REGULAR).color(ink.secondary));
    }
    if let Some((_, name)) = blocking.adding().filter(|(adding, _)| *adding == kind) {
        ui.label(style::text(format!("Adding “{name}”…"), 12.5, style::weight::REGULAR).color(ink.secondary));
    }
    if let Some((message, error)) = blocking.message_for(kind) {
        ui.add(egui::Label::new(style::text(message, 12.5, style::weight::REGULAR).color(if error { ink.danger } else { ink.secondary })).wrap());
    }
    // Kept enabled, under an id of its own, so it keeps keyboard focus while
    // the panel is open and as rows come and go; a press then does nothing.
    let id = egui::Id::unique(("content-blocking-add", kind_key(kind)));
    let add = ui.scope_builder(egui::UiBuilder::new().scope_id(id), |ui| Button::new(ADD_LIST).kind(Kind::Secondary).size(Size::Small).show(ui, ink)).inner;
    if add.clicked() && !busy && !choosing { host.add_from_file(profile, kind); }
    if let Some(id) = remove { host.remove(profile, &id, ui.ctx()); }
    if let Some((id, on)) = turn { host.set_bundled(profile, id, on, ui.ctx()); }
    ui.add_space(4.0);
}

/// The shared row's palette, with its description in the error color when
/// `failed`.
fn row_palette(pal: &Palette, ink: &Ink, failed: bool) -> Palette {
    if failed { Palette { panel_dim: ink.danger, ..pal.clone() } } else { pal.clone() }
}

/// The sites with blocking off, once there are any. Remove turns blocking
/// back on there; a failed save keeps the site and says why.
fn sites(host: &mut ContentBlocking, store: &Store, profile: &str, ui: &mut Ui, ink: &Ink, pal: &Palette) {
    let blocking = host.profile(store, profile);
    let (sites, notice) = (blocking.sites_off(), blocking.sites_notice().map(str::to_owned));
    if sites.is_empty() && notice.is_none() && blocking.sites_error.is_none() { return; }
    ui.add_space(4.0);
    ui.label(style::text(SITES_OFF, 13.5, style::weight::SEMIBOLD).color(pal.panel_text));
    if let Some(notice) = notice {
        ui.add(egui::Label::new(style::text(notice, 12.5, style::weight::REGULAR).color(ink.danger)).wrap());
    }
    let mut remove = None;
    for site in &sites {
        ui.push_id(("content-blocking-site", site), |ui| {
            row(ui, pal, site, "", |ui| {
                if Button::new("Remove").kind(Kind::Secondary).size(Size::Small).show(ui, ink).clicked() { remove = Some(site.clone()); }
            });
        });
    }
    if let Some(error) = &blocking.sites_error {
        ui.add(egui::Label::new(style::text(error.as_str(), 12.5, style::weight::REGULAR).color(ink.danger)).wrap());
    }
    if let Some(site) = remove {
        let saved = host.set_site_blocking(store, profile, &site, true, ui.ctx());
        host.profile(store, profile).sites_error = saved.err();
    }
    ui.add_space(4.0);
}

#[cfg(test)]
#[path = "content_blocking_settings_tests.rs"]
mod tests;
