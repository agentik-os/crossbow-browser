//! The shield in the address pill and the top strip, after the key and the
//! blocked icon. It shows on web pages while a content blocking switch is on,
//! or while the page's site has blocking off. Its menu names the site, says
//! how many requests were blocked on the page, and turns blocking off or on
//! for the site, reloading the page.

use super::icons::{self, Icon};
use super::overlays::MenuUi;
use crate::app::{Action, CrossbowApp, MenuKind, MenuState};
use crate::content_blocking::{ContentBlocking, LOADING_LISTS, NO_LISTS, grouped};
use crossbow_core::store::Store;
use egui::{FontId, Key, Rect, Ui, pos2};

pub const TURN_OFF: &str = "Turn Off Blocking on This Site";
pub const TURN_ON: &str = "Turn On Blocking on This Site";
pub const OFF_HERE: &str = "Blocking is off on this site";
pub const NONE_BLOCKED: &str = "No requests blocked on this page";
pub const SETTINGS: &str = "Content Blocking Settings…";

/// What the shield shows for a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shield {
    pub site: String,
    /// Whether blocking is on for the site.
    pub on: bool,
    /// Whether a switch that is on has a list.
    pub lists: bool,
    /// Whether its lists are still loading and none is ready, so nothing
    /// is blocked yet.
    pub loading: bool,
    /// Requests cancelled on the page so far.
    pub blocked: u32,
}

/// The shield for `page` in `profile`, with `blocked` requests cancelled on
/// it; None when it doesn't show.
pub fn shield(host: &mut ContentBlocking, store: &Store, profile: &str, page: &str, blocked: u32) -> Option<Shield> {
    let (site, on) = host.site_blocking(store, profile, page)?;
    let blocking = host.profile(store, profile);
    (blocking.any_enabled() || !on).then(|| Shield { site, on, lists: blocking.has_lists(), loading: blocking.loading() && !blocking.ready(), blocked })
}

/// The shield's tooltip and the line under its menu's site.
pub fn status(shield: &Shield) -> String {
    match shield.blocked {
        _ if !shield.on => OFF_HERE.to_owned(),
        _ if !shield.lists => NO_LISTS.to_owned(),
        _ if shield.loading => LOADING_LISTS.to_owned(),
        0 => NONE_BLOCKED.to_owned(),
        1 => "1 request blocked on this page".to_owned(),
        n => format!("{} requests blocked on this page", grouped(n as usize)),
    }
}

/// What a click in the shield's menu asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Toggle,
    Settings,
}

/// The site, the page's count, Turn Off or Turn On Blocking on This Site,
/// and Content Blocking Settings….
pub fn menu_entries(m: &MenuUi<'_>, ui: &mut Ui, shield: &Shield) -> Option<Pick> {
    let mut pick = None;
    // A site too long for the heading wraps, never cut.
    let width = ui.painter().layout_no_wrap(shield.site.clone(), FontId::proportional(11.5), m.pal.panel_dim).size().x;
    if width <= m.width - 20.0 { m.heading(ui, &shield.site) } else { m.note(ui, &shield.site) }
    m.note(ui, &status(shield));
    if m.item(ui, if shield.on { TURN_OFF } else { TURN_ON }, "", true) { pick = Some(Pick::Toggle); }
    m.sep(ui);
    if m.item(ui, SETTINGS, "", true) { pick = Some(Pick::Settings); }
    pick
}

/// What applying a pick asks of the app.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub close: bool,
    pub toast: Option<String>,
    /// Open Settings → Privacy, at Content Blocking.
    pub settings: bool,
    /// Reload the page, so it loads with the site's new choice.
    pub reload: bool,
}

/// Apply a pick for `shield`'s site in `profile`. A failed save changes
/// nothing and says why.
pub fn apply_pick(host: &mut ContentBlocking, store: &Store, profile: &str, shield: &Shield, pick: Pick, ctx: &egui::Context) -> Outcome {
    match pick {
        Pick::Toggle => match host.set_site_blocking(store, profile, &shield.site, !shield.on, ctx) {
            Ok(()) => Outcome { close: true, reload: true, ..Outcome::default() },
            Err(error) => Outcome { close: true, toast: Some(error), ..Outcome::default() },
        },
        Pick::Settings => {
            host.reveal = true;
            Outcome { close: true, settings: true, ..Outcome::default() }
        }
    }
}

/// The shield at `rect` for `tab`'s page: the menu to open when it is
/// clicked, focusing its first entry when the keyboard clicked it.
pub fn draw_button(ui: &mut Ui, rect: Rect, shield: &Shield, tab: &str, color: egui::Color32, hover: egui::Color32) -> Option<MenuState> {
    if !icons::button(ui, rect, Icon::Shield, color, hover, true).on_hover_text(status(shield)).clicked() { return None; }
    let keyboard = ui.input(|input| input.key_pressed(Key::Enter) || input.key_pressed(Key::Space));
    Some(crate::ui::folders::open_menu(ui.ctx(), pos2(rect.left(), rect.bottom() + 4.0), MenuKind::ContentBlocking { tab: tab.to_owned() }, keyboard))
}

/// `tab`'s profile, address and blocked count.
fn page(app: &CrossbowApp, tab: &str) -> Option<(String, String, u32)> {
    let tab = app.tab(tab)?;
    let profile = crate::app::tab_profile(&app.lib, tab)?.to_owned();
    Some((profile, tab.url.clone(), tab.view.as_ref().map_or(0, |view| view.blocked())))
}

/// The shield at `rect`, for the focused tab's page. Returns whether it is drawn.
pub fn button(app: &mut CrossbowApp, ui: &mut Ui, rect: Rect, color: egui::Color32, hover: egui::Color32) -> bool {
    let Some(tab) = app.focused_tab_id() else { return false };
    let Some((profile, url, blocked)) = page(app, &tab) else { return false };
    let Some(shown) = shield(&mut app.content_blocking, &app.lib.store, &profile, &url, blocked) else { return false };
    if let Some(menu) = draw_button(ui, rect, &shown, &tab, color, hover) { app.ui.menu = Some(menu); }
    true
}

/// The shield's menu, for the tab it opened on. Returns true when it should close.
pub fn menu(app: &mut CrossbowApp, m: &MenuUi<'_>, ui: &mut Ui, tab: &str) -> bool {
    let Some((profile, url, blocked)) = page(app, tab) else { return true };
    let Some(shown) = shield(&mut app.content_blocking, &app.lib.store, &profile, &url, blocked) else { return true };
    let Some(pick) = menu_entries(m, ui, &shown) else { return false };
    let outcome = apply_pick(&mut app.content_blocking, &app.lib.store, &profile, &shown, pick, ui.ctx());
    if let Some(toast) = outcome.toast { app.actions.push(Action::Toast(toast)); }
    if outcome.reload && let Some(view) = app.tab(tab).and_then(|tab| tab.view.as_ref()) { view.reload(); }
    if outcome.settings {
        app.ui.settings_page_hint = Some(crate::ui::settings::Page::Privacy);
        app.actions.push(Action::OpenSettings);
    }
    outcome.close
}

#[cfg(test)]
#[path = "content_blocking_shield_tests.rs"]
mod tests;
