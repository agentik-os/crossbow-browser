//! Sites that open in a chosen profile. A new tab that a page, the command
//! bar or another app opens for a site with a rule goes to a Space of that
//! profile, and a one-line notice offers Undo. Same-tab navigation never
//! moves: a tab keeps its profile, and with it its cookies. Nothing here logs
//! a URL or host.

use super::*;
use crossbow_core::link_routing::{Routing, usable_space};
use crossbow_core::site_profiles::{Site, SiteProfiles, link_host, target_space};

/// How long the notice stays, unless the pointer rests on it.
pub(crate) const NOTICE_SECONDS: f64 = 6.0;

/// Where a new tab for a link opens, by its site's rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SiteRoute {
    /// No rule moves it: it opens where it was going.
    Here,
    /// The site opens in `profile`, in `space`.
    Elsewhere { space: String, profile: String },
    /// The site's profile has no Space, so it opens where it was going.
    NoSpace { profile: String },
}

/// Each Space's tab uses, for the Space a profile used last.
pub(crate) fn recency(tabs: &[Tab]) -> impl Iterator<Item = (&str, u64)> {
    tabs.iter().filter(|tab| tab.space_id != FAVORITES_SPACE).map(|tab| (tab.space_id.as_str(), tab.last_active_ms))
}

/// Where a new tab for `url`, headed for a Space of the `here` profile, opens.
pub(crate) fn site_route(lib: &Library, sites: &SiteProfiles, tabs: &[Tab], url: &str, here: &str) -> SiteRoute {
    let Some(rule) = sites.route(lib, url).filter(|rule| rule.profile != here) else { return SiteRoute::Here };
    match target_space(lib, &rule.profile, recency(tabs)) {
        Some(space) => SiteRoute::Elsewhere { space, profile: rule.profile.clone() },
        None => SiteRoute::NoSpace { profile: rule.profile.clone() },
    }
}

/// A link from another app: an Air Traffic Control rule picks its Space
/// first, then its site's rule, then Air Traffic Control's default.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LinkRoute {
    pub space: String,
    /// The Air Traffic Control rule that matched, as `Routing::route` says.
    pub rule: Option<usize>,
    pub fallback: bool,
    pub site: SiteRoute,
    /// The Space Air Traffic Control alone picked, which Undo returns to.
    pub from: String,
}

pub(crate) fn route_link(lib: &Library, routing: &Routing, sites: &SiteProfiles, tabs: &[Tab], most_recent: &str, url: &str) -> Option<LinkRoute> {
    let route = routing.route(lib, url, most_recent)?;
    // A matching rule whose Space is gone falls back, and then the site's rule may apply.
    let site = if route.rule.is_some() && !route.fallback {
        SiteRoute::Here
    } else {
        site_route(lib, sites, tabs, url, &lib.spaces[&route.space].profile_id)
    };
    let space = match &site {
        SiteRoute::Elsewhere { space, .. } => space.clone(),
        _ => route.space.clone(),
    };
    Some(LinkRoute { space, rule: route.rule, fallback: route.fallback, site, from: route.space })
}

/// Undo for a moved tab: close it and reopen its link where it was going.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Undo {
    pub tab: String,
    pub url: String,
    pub space: String,
}

/// "Opened in Work", with Undo; or why a site's rule could not apply.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SiteNotice {
    pub text: String,
    pub undo: Option<Undo>,
    /// When it was first drawn, or last had the pointer.
    pub shown: Option<f64>,
}

fn profile_name(lib: &Library, profile: &str) -> String {
    lib.profiles.get(profile).map(|profile| profile.name.clone()).unwrap_or_else(|| "another profile".into())
}

/// The notice for a link whose site rule moved it, or could not.
pub(crate) fn notice_for(lib: &Library, route: &SiteRoute, url: &str, undo: Option<Undo>) -> Option<SiteNotice> {
    let text = match route {
        SiteRoute::Here => return None,
        SiteRoute::Elsewhere { profile, .. } => format!("Opened in {}", profile_name(lib, profile)),
        SiteRoute::NoSpace { profile } => {
            let site = link_host(url).map(|host| Site { host, subdomains: false }.name()).unwrap_or_else(|| "This site".into());
            format!("{site} opens in {}, which has no Space, so it opened here.", profile_name(lib, profile))
        }
    };
    let undo = undo.filter(|_| matches!(route, SiteRoute::Elsewhere { .. }));
    Some(SiteNotice { text, undo, shown: None })
}

/// Where a new tab for a link went.
#[derive(Debug, PartialEq)]
pub(crate) enum Placement {
    /// Added in another profile's Space.
    Moved { tab: String, space: String, notice: SiteNotice },
    /// Not added: the caller opens it as before, with this notice if any.
    Unmoved(Option<SiteNotice>),
}

/// Add `url` as the first Today tab of its site's profile's Space, when a
/// rule moves it from `from_space` (of the `here` profile).
pub(crate) fn place_site_tab(lib: &Library, sites: &SiteProfiles, tabs: &mut Vec<Tab>, url: &str, here: &str, from_space: &str) -> Placement {
    let route = site_route(lib, sites, tabs, url, here);
    let SiteRoute::Elsewhere { space, .. } = &route else { return Placement::Unmoved(notice_for(lib, &route, url, None)) };
    let space = space.clone();
    let tab = super::link_open::add_first_today_tab(tabs, &space, url, "");
    let undo = Undo { tab: tab.clone(), url: url.into(), space: from_space.into() };
    let notice = notice_for(lib, &route, url, Some(undo)).expect("a moved link has a notice");
    Placement::Moved { tab, space, notice }
}

/// Undo a moved tab: remove it without archiving, as it never should have
/// opened there, and add its link as the first Today tab of `undo.space`,
/// or of `active_space` if that Space is gone. Returns the removed tab, if
/// still open, and the new tab's id.
pub(crate) fn undo_tabs(lib: &Library, tabs: &mut Vec<Tab>, undo: &Undo, active_space: &str) -> Result<(Option<Tab>, String), String> {
    let space = if usable_space(lib, &undo.space) { undo.space.clone() } else { active_space.to_string() };
    if !usable_space(lib, &space) { return Err("Its Space is no longer available.".into()); }
    let removed = match tabs.iter().position(|tab| tab.id == undo.tab) {
        Some(index) => {
            lib.store.close_tab(&tabs[index].row(), None, now_ms()).map_err(|error| error.to_string())?;
            Some(tabs.remove(index))
        }
        None => None,
    };
    let tab = super::link_open::add_first_today_tab(tabs, &space, &undo.url, "");
    Ok((removed, tab))
}

/// The tab menu's site entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SiteEntry {
    /// "Always Open github.com in Work…": opens Settings to add it.
    Always { label: String, host: String, profile: String },
    /// "Stop Opening github.com in Work": removes the rule.
    Stop { label: String, host: String },
}

/// What a tab's menu offers for its site: to stop the rule that matches
/// it, and to always open it in the tab's own profile unless that rule does.
pub(crate) fn tab_site_entries(lib: &Library, sites: &SiteProfiles, tab: &Tab) -> Vec<SiteEntry> {
    let Some(host) = link_host(&tab.url) else { return Vec::new() };
    let mut entries = Vec::new();
    let rule = sites.rule_for(&tab.url);
    if let Some(profile) = tab_profile(lib, tab).filter(|profile| rule.is_none_or(|rule| rule.profile != *profile)) {
        let site = Site { host: host.clone(), subdomains: false };
        entries.push(SiteEntry::Always { label: format!("Always Open {} in {}…", site.name(), profile_name(lib, profile)), host, profile: profile.to_string() });
    }
    if let Some(rule) = rule {
        let whose = if lib.profiles.contains_key(&rule.profile) { profile_name(lib, &rule.profile) } else { "a Profile Not on This Device".into() };
        entries.push(SiteEntry::Stop { label: format!("Stop Opening {} in {whose}", rule.site.name()), host: rule.site.host.clone() });
    }
    entries
}

impl CrossbowApp {
    /// Open `url` in a new tab in its site's profile, when a rule moves it
    /// from the Space in use, and show the notice. False when no rule moves
    /// it, so the caller opens it as before.
    pub(super) fn open_site_tab(&mut self, url: &str, background: bool) -> bool {
        let here = self.current_profile();
        let from = self.active_space.clone();
        match place_site_tab(&self.lib, &self.site_profiles, &mut self.tabs, url, &here, &from) {
            Placement::Moved { tab, notice, .. } => {
                if !background { self.activate(&tab); }
                self.site_notice = Some(notice);
                true
            }
            Placement::Unmoved(notice) => {
                if notice.is_some() { self.site_notice = notice; }
                false
            }
        }
    }

    /// The notice's Undo: close the moved tab and reopen its link where it
    /// was going, as a new tab in that Space's profile.
    pub(super) fn undo_site_open(&mut self, now: f64) {
        let Some(undo) = self.site_notice.take().and_then(|notice| notice.undo) else { return };
        let selection = self.tab_selection();
        match undo_tabs(&self.lib, &mut self.tabs, &undo, &self.active_space) {
            Ok((removed, tab)) => {
                if let Some(removed) = removed { self.finish_closed_tabs(vec![removed], selection); }
                self.activate(&tab);
            }
            Err(error) => self.ui.toasts.push(Toast { text: format!("Could not undo. {error}"), at: now }),
        }
    }

    /// Stop opening `host` in its profile, from a tab's menu.
    pub(super) fn stop_site_profile(&mut self, host: &str, now: f64) {
        let Some(rule) = self.site_profiles.owner(host).cloned() else { return };
        let text = match self.site_profiles.remove(&self.lib.store, host) {
            Ok(()) => format!("{} no longer opens in {}", rule.site.name(), profile_name(&self.lib, &rule.profile)),
            Err(error) => format!("Could not save ({error}), so {} still opens in {}.", rule.site.name(), profile_name(&self.lib, &rule.profile)),
        };
        self.ui.toasts.push(Toast { text, at: now });
    }
}

#[cfg(test)]
#[path = "site_open_tests.rs"]
mod tests;
