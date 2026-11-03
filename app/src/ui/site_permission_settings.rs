//! Settings → Privacy → Site Settings: what the active Space's profile
//! remembered for each site, editable, and the sites that may open links in
//! other apps without asking.

use crate::site_permissions::{SitePermissions, site};
use crate::ui::settings::{row, section, segmented, separator};
use crate::ui::style::{self, Button, Ink, Kind, Size};
use crate::ui::theme::Palette;
use crossbow_core::site_permissions::{self as rules, Decision, Permission, Remembered};
use crossbow_core::store::Store;
use egui::Ui;

#[derive(Default)]
pub struct Editor {
    /// Choices set to Ask on this visit: they stay listed until the page is left.
    asked: Vec<(String, String, Permission)>,
    confirm_reset_all: bool,
    /// The last result, and whether it is an error.
    message: Option<(String, bool)>,
}

impl Editor {
    pub fn leave(&mut self) {
        *self = Editor::default();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Setting {
    Allow,
    Block,
    Ask,
}

enum Line {
    Permission(Permission, Setting),
    OpenLinks(String),
}

/// One remembered choice, under its site.
struct Entry {
    origin: String,
    top_origin: String,
    line: Line,
}

fn entries(editor: &Editor, records: Vec<rules::SiteRecord>) -> Vec<Entry> {
    let mut entries: Vec<Entry> = records.into_iter().map(|record| Entry {
        line: match record.remembered {
            Remembered::Permission(permission, Decision::Allow) => Line::Permission(permission, Setting::Allow),
            Remembered::Permission(permission, Decision::Block) => Line::Permission(permission, Setting::Block),
            Remembered::OpenLinks(scheme) => Line::OpenLinks(scheme),
        },
        origin: record.origin,
        top_origin: record.top_origin,
    }).collect();
    for (origin, top_origin, permission) in &editor.asked {
        let listed = entries.iter().any(|entry| entry.origin == *origin && entry.top_origin == *top_origin && matches!(entry.line, Line::Permission(listed, _) if listed == *permission));
        if !listed { entries.push(Entry { origin: origin.clone(), top_origin: top_origin.clone(), line: Line::Permission(*permission, Setting::Ask) }); }
    }
    // By site; its own choices before those as a frame elsewhere; links last.
    entries.sort_by(|a, b| {
        let key = |entry: &Entry| (entry.origin.clone(), entry.origin != entry.top_origin, entry.top_origin.clone(), match &entry.line {
            Line::Permission(permission, _) => (0, Some(*permission), String::new()),
            Line::OpenLinks(scheme) => (1, None, scheme.clone()),
        });
        key(a).cmp(&key(b))
    });
    entries
}

pub fn show(editor: &mut Editor, perms: &mut SitePermissions, store: &Store, profile: &str, profile_name: &str, ui: &mut Ui, pal: &Palette) {
    let ink = Ink::new(pal.panel, pal.accent);
    section(ui, pal, "Site Settings");
    ui.add(egui::Label::new(style::text(format!("For the “{profile_name}” profile, used by this Space. Other profiles keep their own."), 12.0, style::weight::REGULAR).color(pal.panel_dim)).wrap());
    let records = match rules::records(store, profile) {
        Ok(records) => records,
        Err(_) => {
            ui.label(style::text("Couldn't read site permissions. Reopen Settings to try again.", 12.5, style::weight::REGULAR).color(ink.danger));
            Vec::new()
        }
    };
    let entries = entries(editor, records);
    if entries.is_empty() {
        ui.label(style::text("No site has a remembered choice yet.", 12.5, style::weight::REGULAR).color(ink.secondary));
    }
    let now_ms = crossbow_core::now_ms();
    let mut origin: Option<&str> = None;
    for entry in &entries {
        if origin != Some(entry.origin.as_str()) {
            origin = Some(&entry.origin);
            let header = ui.horizontal(|ui| {
                ui.label(style::text(site(&entry.origin), 13.5, style::weight::SEMIBOLD).color(pal.panel_text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if Button::new("Reset").kind(Kind::Secondary).size(Size::Small).show(ui, &ink).clicked() {
                        editor.message = Some(match rules::reset_site(store, profile, &entry.origin) {
                            Ok(_) => {
                                editor.asked.retain(|(asked, _, _)| *asked != entry.origin);
                                (format!("Site permissions reset for {}.", site(&entry.origin)), false)
                            }
                            Err(_) => (format!("Couldn't reset {}. Nothing changed.", site(&entry.origin)), true),
                        });
                    }
                });
            });
            if perms.reveal.as_deref() == Some(entry.origin.as_str()) {
                header.response.scroll_to_me(Some(egui::Align::TOP));
                perms.reveal = None;
            }
            separator(ui, pal);
        }
        let embedded = (entry.origin != entry.top_origin).then(|| site(&entry.top_origin));
        match &entry.line {
            Line::Permission(permission, setting) => {
                let title = match embedded {
                    Some(top) => format!("{} on {top}", permission.label()),
                    None => permission.label().to_owned(),
                };
                let mut value = *setting;
                let mut changed = false;
                // Segments are keyed on the scope: one scope per choice.
                ui.push_id((&entry.origin, &entry.top_origin, permission.key()), |ui| {
                    row(ui, pal, &title, "", |ui| { changed = segmented(ui, pal, &mut value, &[(Setting::Allow, "Allow"), (Setting::Block, "Block"), (Setting::Ask, "Ask")]); });
                });
                if changed {
                    let decision = match value {
                        Setting::Allow => Some(Decision::Allow),
                        Setting::Block => Some(Decision::Block),
                        Setting::Ask => None,
                    };
                    let key = (entry.origin.clone(), entry.top_origin.clone(), *permission);
                    match rules::set_decision(store, profile, &entry.origin, &entry.top_origin, *permission, decision, now_ms) {
                        Ok(()) => {
                            editor.asked.retain(|asked| *asked != key);
                            if decision.is_none() { editor.asked.push(key); }
                            editor.message = None;
                        }
                        Err(_) => editor.message = Some((format!("Couldn't save. {} keeps its previous setting.", site(&entry.origin)), true)),
                    }
                }
            }
            Line::OpenLinks(scheme) => {
                let mut remove = false;
                row(ui, pal, &format!("Opens {scheme}: links without asking"), "", |ui| { remove = Button::new("Remove").kind(Kind::Secondary).size(Size::Small).show(ui, &ink).clicked(); });
                if remove {
                    editor.message = rules::set_opens_links(store, profile, &entry.origin, scheme, false, now_ms).err().map(|_| {
                        (format!("Couldn't remove it. {} still opens {scheme}: links without asking.", site(&entry.origin)), true)
                    });
                }
            }
        }
    }
    if let Some((message, error)) = &editor.message {
        ui.add(egui::Label::new(style::text(message.as_str(), 12.5, style::weight::REGULAR).color(if *error { ink.danger } else { ink.secondary })).wrap());
    }
    ui.add_space(4.0);
    if Button::new("Reset All Site Permissions…").kind(Kind::Secondary).size(Size::Small).enabled(!entries.is_empty() || editor.confirm_reset_all).show(ui, &ink).clicked() {
        editor.confirm_reset_all = true;
    }
    if editor.confirm_reset_all {
        let confirmation = ui.vertical(|ui| {
            ui.add(egui::Label::new(style::text(format!("Reset every site permission for “{profile_name}”? Sites will ask again, and links to other apps will ask before opening."), 12.5, style::weight::REGULAR).color(ink.text)).wrap());
            let mut picked = None;
            ui.horizontal_wrapped(|ui| {
                if Button::new("Reset All").kind(Kind::Primary).size(Size::Small).show(ui, &ink).clicked() { picked = Some(true); }
                if Button::new("Cancel").kind(Kind::Plain).size(Size::Small).show(ui, &ink).clicked() { picked = Some(false); }
            });
            picked
        });
        match confirmation.inner {
            Some(true) => {
                editor.confirm_reset_all = false;
                editor.message = Some(match rules::reset_all(store, profile) {
                    Ok(_) => {
                        editor.asked.clear();
                        (format!("All site permissions reset for “{profile_name}”."), false)
                    }
                    Err(_) => ("Couldn't reset site permissions. Nothing changed.".to_owned(), true),
                });
            }
            Some(false) => editor.confirm_reset_all = false,
            None => {}
        }
    }
}
