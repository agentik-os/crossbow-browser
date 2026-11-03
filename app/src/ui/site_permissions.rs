//! Site permissions on pages: the prompt under the address of the focused
//! tab, the quiet "tried to open" notice in the same place, and the blocked
//! icon among the address's controls.

use crate::app::{Action, CrossbowApp};
use crate::site_permissions::{Choice, Indicator, Outcome, Question, SitePermissions, Waiting, site};
use crate::ui::icons::{self, Icon};
use crate::ui::site_permission_settings;
use crate::ui::style::{self, Button, Ink, Kind, Size};
use crate::ui::theme::Palette;
use crossbow_core::store::Store;
use egui::{Id, Rect, Ui, pos2, vec2};

pub const CARD_WIDTH: f32 = 320.0;
/// The gap between the card and the window's edges.
const EDGE: f32 = 10.0;

/// Where a prompt or notice sits: under the address area, left-aligned to
/// the sidebar's pill or right-aligned to the top strip's controls; at the
/// pane's top left when no address shows, as in full screen.
pub fn card_rect(anchor: Option<Rect>, pane: Rect, screen: Rect) -> Rect {
    let width = CARD_WIDTH.min(screen.width() - 2.0 * EDGE).max(0.0);
    let (left, top) = match anchor {
        Some(anchor) if anchor.center().x > screen.center().x => (anchor.right() - width, anchor.bottom() + 6.0),
        Some(anchor) => (anchor.left(), anchor.bottom() + 6.0),
        None => (pane.left() + EDGE, pane.top() + EDGE),
    };
    let left = left.clamp(screen.left() + EDGE, (screen.right() - EDGE - width).max(screen.left() + EDGE));
    Rect::from_min_size(pos2(left, top), vec2(width, 0.0))
}

/// The focused tab and where its prompt or notice draws, when a pane shows
/// that tab: a background tab's prompt waits.
pub fn prompt_place(app: &CrossbowApp, screen: Rect) -> Option<(String, Rect)> {
    let tab = app.focused_tab_id()?;
    let pane = app.ui.panes.iter().find(|(id, _)| *id == tab).map(|(_, rect)| *rect)?;
    let at = card_rect(app.ui.address, pane, screen);
    Some((tab, at))
}

/// The column a prompt or notice placed at `at` may take: its width, down
/// to the window's bottom, whatever it says.
pub fn prompt_column(at: Rect, screen: Rect) -> Rect {
    Rect::from_min_max(at.min, pos2(at.max.x, screen.bottom().max(at.min.y)))
}

/// Where the focused tab's prompt or notice is drawn this frame, as a
/// column, while it shows one: another card there waits (see
/// `translate::offered`).
pub fn shown_column(app: &CrossbowApp, screen: Rect) -> Option<Rect> {
    let (tab, at) = prompt_place(app, screen)?;
    let shows = app.site_permissions.question(&tab).is_some() || app.site_permissions.indicator(&tab).is_some();
    shows.then(|| prompt_column(at, screen))
}

/// Whether a prompt or notice goes before another card at `at` on `tab`'s
/// pane: one on that page, or the focused tab's drawn over `at` (`shown`,
/// from [`shown_column`]). In split view with the sidebar hidden, the
/// focused tab's hangs from the top strip's right end, over the other
/// pane's top right.
pub fn goes_first(perms: &SitePermissions, tab: &str, at: Rect, shown: Option<Rect>) -> bool {
    perms.question(tab).is_some() || perms.indicator(tab).is_some() || shown.is_some_and(|shown| shown.intersects(at))
}

/// The prompt's widgets this frame, for Escape: see [`dismiss_focused_prompt`].
#[derive(Clone, Default)]
struct Focusable {
    tab: String,
    ids: Vec<Id>,
}

fn focusable_id() -> Id {
    Id::unique("site-permission-prompt-widgets")
}

/// One question's card. Clicks count only after [`crate::site_permissions::INPUT_DELAY`].
pub fn prompt_card(ctx: &egui::Context, at: Rect, waiting: &mut Waiting, now: f64, pal: &Palette) -> Option<Choice> {
    let ink = Ink::new(pal.panel, pal.accent);
    let shown = *waiting.shown_at.get_or_insert(now);
    let ready = waiting.ready(now);
    if !ready { ctx.request_repaint_after(std::time::Duration::from_secs_f64((shown + crate::site_permissions::INPUT_DELAY - now).max(0.0))); }
    let (deny, allow, remember) = match &waiting.question {
        Question::Permission { .. } => ("Don't Allow", Some("Allow"), Some("Remember for this site".to_owned())),
        Question::Link { top_origin, can_always, .. } => ("Cancel", Some("Open"), can_always.then(|| format!("Always allow {} to open links of this type", top_origin.as_deref().map_or("this site", site)))),
        Question::NoApp { .. } => ("OK", None, None),
    };
    let mut choice = None;
    let mut ids = Vec::new();
    egui::Area::new(Id::unique(("site-permission-prompt", &waiting.tab))).order(egui::Order::Foreground).fixed_pos(at.min).show(ctx, |ui| {
        let frame = crate::ui::overlays::menu_frame(pal).inner_margin(egui::Margin::same(12));
        frame.show(ui, |ui| {
            style::apply_visuals(ui, &ink);
            ui.set_width(at.width() - frame.total_margin().sum().x);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.add(egui::Label::new(style::text(waiting.question.title(), 13.5, style::weight::SEMIBOLD).color(pal.panel_text)).wrap());
            if let Some(detail) = waiting.question.detail() {
                ui.add(egui::Label::new(style::text(detail, 12.5, style::weight::REGULAR).color(ink.secondary)).wrap());
            }
            if let Some(label) = remember {
                let response = ui.checkbox(&mut waiting.remember, style::text(label, 12.5, style::weight::REGULAR).color(ink.text));
                ids.push(response.id);
            }
            ui.horizontal_wrapped(|ui| {
                // Equal weight for a permission's two answers; Open leads a link's.
                let primary = if matches!(waiting.question, Question::Permission { .. }) { Kind::Secondary } else { Kind::Primary };
                let response = Button::new(deny).kind(Kind::Secondary).size(Size::Small).enabled(ready).show(ui, &ink);
                ids.push(response.id);
                if response.clicked() { choice = Some(Choice::Deny); }
                if let Some(allow) = allow {
                    let response = Button::new(allow).kind(primary).size(Size::Small).enabled(ready).show(ui, &ink);
                    ids.push(response.id);
                    if response.clicked() { choice = Some(Choice::Allow); }
                }
            });
        });
    });
    ctx.data_mut(|data| data.insert_temp(focusable_id(), Focusable { tab: waiting.tab.clone(), ids }));
    choice
}

/// Escape from a prompt the keyboard is on, which the app's shortcuts take
/// before the card is drawn and after egui has dropped its focus: Don't
/// Allow, never remembered.
pub fn dismiss_focused_prompt(ctx: &egui::Context, perms: &mut SitePermissions, store: &Store) -> Option<Outcome> {
    let focusable: Focusable = ctx.data(|data| data.get_temp(focusable_id()))?;
    let focused = focusable.ids.iter().any(|id| ctx.memory(|memory| memory.has_focus(*id) || memory.had_focus_last_frame(*id)));
    if !focused || perms.question(&focusable.tab).is_none() { return None; }
    ctx.data_mut(|data| data.remove::<Focusable>(focusable_id()));
    Some(perms.choose(store, &focusable.tab, Choice::Dismiss, crossbow_core::now_ms()))
}

/// The quiet notice for a link a page opened by itself. Some(true) opens the
/// app, Some(false) closes the notice.
pub fn indicator_card(ctx: &egui::Context, at: Rect, tab: &str, indicator: &Indicator, pal: &Palette) -> Option<bool> {
    let ink = Ink::new(pal.panel, pal.accent);
    let mut picked = None;
    egui::Area::new(Id::unique(("site-link-notice", tab))).order(egui::Order::Foreground).fixed_pos(at.min).show(ctx, |ui| {
        let frame = crate::ui::overlays::menu_frame(pal).inner_margin(egui::Margin { left: 12, right: 32, top: 10, bottom: 10 });
        frame.show(ui, |ui| {
            ui.set_width(at.width() - frame.total_margin().sum().x);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.add(egui::Label::new(style::text(indicator.text(), 12.5, style::weight::REGULAR).color(ink.secondary)).wrap());
            if Button::new(&format!("Open {}", indicator.app.name)).kind(Kind::Secondary).size(Size::Small).show(ui, &ink).clicked() { picked = Some(true); }
        });
        let close = Rect::from_center_size(pos2(at.right() - 20.0, at.top() + 20.0), vec2(22.0, 22.0));
        if icons::button(ui, close, Icon::Close, ink.secondary, ink.fill_hover, true).on_hover_text("Close").clicked() { picked = Some(false); }
    });
    picked
}

/// The focused tab's prompt or notice, while nothing covers the window.
pub fn prompts(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette) {
    // Waits while Settings, the command bar or Peek covers the page.
    if app.ui.settings.is_some() || app.ui.command.is_some() || app.ui.onboarding.is_some() || app.ui.peek.is_some() { return; }
    let ctx = ui.ctx().clone();
    // Only a tab the user can see: a background tab's prompt waits.
    let Some((tab, at)) = prompt_place(app, ctx.content_rect()) else { return };
    let now = ctx.input(|input| input.time);
    if let Some(waiting) = app.site_permissions.question_mut(&tab) {
        let Some(choice) = prompt_card(&ctx, at, waiting, now, pal) else { return };
        let outcome = app.site_permissions.choose(&app.lib.store, &tab, choice, crossbow_core::now_ms());
        apply(app, outcome);
        ctx.request_repaint();
    } else if let Some(indicator) = app.site_permissions.indicator(&tab).cloned() {
        match indicator_card(&ctx, at, &tab, &indicator, pal) {
            Some(true) => {
                let notice = app.site_permissions.open_indicator(&tab);
                apply(app, Outcome { reply: None, notice });
            }
            Some(false) => app.site_permissions.dismiss_indicator(&tab),
            None => {}
        }
    }
}

/// Send an answer to its view and show what needs saying.
pub fn apply(app: &mut CrossbowApp, outcome: Outcome) {
    if let Some(reply) = outcome.reply
        && let Some(view) = app.tabs.iter().filter_map(|tab| tab.view.as_ref()).find(|view| view.serial() == reply.serial)
    {
        view.answer_permission(reply.id, reply.allow);
    }
    if let Some(notice) = outcome.notice { app.actions.push(Action::Toast(notice)); }
}

/// The blocked icon at `rect`, when a remembered Block refused something on
/// the focused page. It opens the site in Site Settings. Returns whether it
/// is drawn.
pub fn blocked_button(app: &mut CrossbowApp, ui: &mut Ui, rect: Rect, color: egui::Color32, hover: egui::Color32) -> bool {
    let Some(tab) = app.focused_tab_id() else { return false };
    let Some(blocked) = app.site_permissions.blocked(&tab).cloned() else { return false };
    let names: Vec<&str> = blocked.permissions.iter().map(|permission| permission.label()).collect();
    let tip = format!("{} blocked on {}", names.join(", "), site(&blocked.origin));
    if icons::button(ui, rect, Icon::Blocked, color, hover, true).on_hover_text(tip).clicked() {
        app.site_permissions.reveal = Some(blocked.origin);
        app.ui.settings_page_hint = Some(crate::ui::settings::Page::Privacy);
        app.actions.push(Action::OpenSettings);
    }
    true
}

/// Settings → Privacy for the active Space's profile.
pub fn settings_page(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette) {
    let profile = app.current_profile();
    let name = app.lib.profiles.get(&profile).map(|profile| profile.name.clone()).unwrap_or_default();
    let Some(settings) = app.ui.settings.as_mut() else { return };
    site_permission_settings::show(&mut settings.site_settings, &mut app.site_permissions, &app.lib.store, &profile, &name, ui, pal);
}

#[cfg(test)]
#[path = "site_permissions_tests.rs"]
mod tests;
