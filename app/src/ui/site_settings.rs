//! Settings → Spaces → Profiles: the sites each profile opens, with Remove,
//! and Add Site…. A site another profile has asks before it moves; an IP
//! address or localhost asks before it is added. Sites whose profile is not
//! on this device are listed apart: they route nothing.

use super::*;
use crossbow_core::link_routing::{Routing, usable_space};
use crossbow_core::site_profiles::{AddError, Site, SiteError, SiteProfiles, SiteRule};

const NOT_HERE: &str = "Profile not on this device";

enum Problem {
    Message(String),
    /// An IP address or localhost: Add Anyway.
    Local(String),
    /// Another profile has the site: Move Here.
    Taken(String),
}

/// Add Site…, open for one profile.
struct Draft {
    profile: String,
    text: String,
    subdomains: bool,
    problem: Option<Problem>,
    focus: bool,
}

impl Draft {
    fn new(profile: &str, text: &str) -> Self {
        Draft { profile: profile.into(), text: text.into(), subdomains: false, problem: None, focus: true }
    }
}

#[derive(Default)]
pub(super) struct SitesState {
    draft: Option<Draft>,
    /// Why the last Remove failed: the profile, and the text.
    failure: Option<(String, String)>,
}

impl SitesState {
    /// Open Add Site… for `profile` with `host` filled in, as a tab's menu asks.
    pub(super) fn prefill(&mut self, profile: &str, host: &str) {
        self.draft = Some(Draft::new(profile, host));
    }

    /// Add Site… is open: Escape cancels it instead of closing Settings.
    pub(super) fn owns_keyboard(&self) -> bool {
        self.draft.is_some()
    }

    /// Escape, or leaving the page: an unsaved site is discarded.
    pub(super) fn cancel(&mut self) {
        self.draft = None;
    }
}

fn profile_name(lib: &Library, profile: &str) -> String {
    lib.profiles.get(profile).map(|profile| profile.name.clone()).unwrap_or_else(|| "a profile not on this device".into())
}

/// Where Air Traffic Control sends this site's links from other apps, when
/// a rule there picks another profile's Space: it wins for those links.
fn traffic_note(lib: &Library, routing: &Routing, rule: &SiteRule) -> Option<String> {
    let route = routing.route(lib, &format!("https://{}/", rule.site.host), "")?;
    if route.rule.is_none() || route.fallback { return None; }
    let space = lib.spaces.get(&route.space).filter(|space| space.profile_id != rule.profile)?;
    Some(format!("Air Traffic Control sends this site's links from other apps to “{}”.", space.name))
}

fn site_row(ui: &mut Ui, ink: &Ink, text: &str, note: Option<&str>) -> bool {
    let mut remove = false;
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 24.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        remove = Button::new("Remove").kind(Kind::Plain).size(style::Size::Small).show(ui, ink).clicked();
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add(egui::Label::new(style::text(text, 13.0, style::weight::REGULAR).color(ink.text)).truncate());
        });
    });
    if let Some(note) = note {
        ui.label(style::text(note, 12.0, style::weight::REGULAR).color(ink.secondary));
    }
    remove
}

fn remove(st: &mut SitesState, sites: &mut SiteProfiles, lib: &Library, rule: &SiteRule, profile: &str) {
    st.failure = sites.remove(&lib.store, &rule.site.host).err()
        .map(|error| (profile.to_string(), format!("Could not remove {} ({error}), so it still opens in {}.", rule.site.name(), profile_name(lib, &rule.profile))));
}

/// The sites `profile` opens, and Add Site… for it.
pub(super) fn rows(st: &mut SitesState, sites: &mut SiteProfiles, lib: &Library, routing: &Routing, profile: &str, ui: &mut Ui, ink: &Ink) {
    let mine: Vec<SiteRule> = sites.rules().iter().filter(|rule| rule.profile == profile).cloned().collect();
    for rule in &mine {
        let note = traffic_note(lib, routing, rule);
        if ui.push_id(("site", &rule.site.host), |ui| site_row(ui, ink, &rule.site.label(), note.as_deref())).inner {
            remove(st, sites, lib, rule, profile);
        }
    }
    let homeless = !lib.spaces.iter().any(|(id, space)| space.profile_id == profile && usable_space(lib, id));
    if !mine.is_empty() && homeless {
        ui.label(style::text("No Space uses this profile, so its sites open where you are.", 12.0, style::weight::REGULAR).color(ink.secondary));
    }
    if let Some((_, text)) = st.failure.as_ref().filter(|(owner, _)| owner == profile) {
        ui.label(style::text(text, 12.0, style::weight::REGULAR).color(ink.danger));
    }
    if !st.draft.as_ref().is_some_and(|draft| draft.profile == profile) {
        if Button::new("Add Site…").kind(Kind::Plain).size(style::Size::Small).show(ui, ink).clicked() {
            st.draft = Some(Draft::new(profile, ""));
        }
        return;
    }
    form(st, sites, lib, profile, ui, ink);
}

/// What Add, Add Anyway or Move Here does with the draft.
fn submit(st: &mut SitesState, sites: &mut SiteProfiles, lib: &Library, allow_local: bool, take_over: bool) {
    let Some(draft) = st.draft.as_mut() else { return };
    let site = match Site::parse(&draft.text, draft.subdomains, allow_local) {
        Ok(site) => site,
        Err(error @ SiteError::Local { .. }) => {
            draft.problem = Some(Problem::Local(error.message()));
            return;
        }
        Err(error) => {
            draft.problem = Some(Problem::Message(error.message()));
            return;
        }
    };
    match sites.add(&lib.store, SiteRule { site, profile: draft.profile.clone() }, take_over) {
        Ok(()) => st.draft = None,
        Err(AddError::Taken(other)) => draft.problem = Some(Problem::Taken(format!("Already opens in {}. Move it here?", profile_name(lib, &other.profile)))),
        Err(AddError::Save(error)) => draft.problem = Some(Problem::Message(format!("Could not save ({error}). Nothing changed."))),
    }
}

fn form(st: &mut SitesState, sites: &mut SiteProfiles, lib: &Library, profile: &str, ui: &mut Ui, ink: &Ink) {
    let Some(draft) = st.draft.as_mut() else { return };
    let (mut add, mut cancel) = (false, false);
    ui.horizontal_wrapped(|ui| {
        let field = ui.add(egui::TextEdit::singleline(&mut draft.text).id(Id::unique(("site-field", profile))).hint_text("example.com").desired_width(200.0));
        if std::mem::take(&mut draft.focus) {
            field.request_focus();
            field.scroll_to_me(Some(egui::Align::Center));
        }
        if field.changed() { draft.problem = None; }
        add = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.checkbox(&mut draft.subdomains, "and subdomains").changed() { draft.problem = None; }
        add |= Button::new("Add").kind(Kind::Primary).size(style::Size::Small).enabled(!draft.text.trim().is_empty()).show(ui, ink).clicked();
        cancel = Button::new("Cancel").kind(Kind::Plain).size(style::Size::Small).show(ui, ink).clicked();
    });
    let (mut anyway, mut take_over) = (false, false);
    match &draft.problem {
        None => {}
        Some(Problem::Message(text)) => { ui.label(style::text(text, 12.0, style::weight::REGULAR).color(ink.danger)); }
        Some(Problem::Local(text)) => {
            ui.label(style::text(text, 12.0, style::weight::REGULAR).color(ink.danger));
            anyway = Button::new("Add Anyway").kind(Kind::Plain).size(style::Size::Small).show(ui, ink).clicked();
        }
        Some(Problem::Taken(text)) => {
            ui.label(style::text(text, 12.0, style::weight::REGULAR).color(ink.secondary));
            take_over = Button::new("Move Here").kind(Kind::Plain).size(style::Size::Small).show(ui, ink).clicked();
        }
    }
    if cancel { st.draft = None; }
    // A site that reached Move Here was already accepted, local or not.
    else if take_over { submit(st, sites, lib, true, true); }
    else if anyway { submit(st, sites, lib, true, false); }
    else if add { submit(st, sites, lib, false, false); }
}

/// Saved sites that can't be read, and sites whose profile is not on this
/// device, which route nothing until removed.
pub(super) fn others(st: &mut SitesState, sites: &mut SiteProfiles, lib: &Library, ui: &mut Ui, ink: &Ink) {
    if let Some(notice) = &sites.notice {
        ui.label(style::text(notice, 12.0, style::weight::REGULAR).color(ink.danger));
    }
    let orphans: Vec<SiteRule> = sites.rules().iter().filter(|rule| !lib.profiles.contains_key(&rule.profile)).cloned().collect();
    for rule in &orphans {
        let text = format!("{} · {NOT_HERE}", rule.site.label());
        if ui.push_id(("orphan-site", &rule.site.host), |ui| site_row(ui, ink, &text, None)).inner {
            remove(st, sites, lib, rule, "");
        }
    }
    if let Some((_, text)) = st.failure.as_ref().filter(|(owner, _)| owner.is_empty()) {
        ui.label(style::text(text, 12.0, style::weight::REGULAR).color(ink.danger));
    }
}
