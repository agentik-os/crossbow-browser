//! The Translate Page card at the top right of a pane, where the password
//! offer sits: the offer, progress, the result and a stopped translation,
//! each with its buttons. It is non-modal, waits while a password offer or a
//! permission prompt is on the same page, and takes no clicks for
//! [`INPUT_DELAY`] after it appears or changes, so a click aimed at a page
//! that just loaded never chooses Translate.

use crate::app::{Action, CrossbowApp};
use crate::translate::{Card, Choice};
use crate::ui::style::{self, Button, Ink, Kind, Size};
use crate::ui::theme::Palette;
use egui::{Id, Rect, Ui, vec2};

pub const INPUT_DELAY: f64 = 0.5;
pub const TRANSLATE_PAGE: &str = "Translate Page";
pub const SHOW_ORIGINAL: &str = "Show Original";

/// The card as shown, since when.
#[derive(Clone)]
struct Armed {
    card: Card,
    since: f64,
}

/// The card's buttons this frame, for Escape: see [`dismiss_focused_card`].
#[derive(Clone, Default)]
struct Focusable {
    tab: String,
    ids: Vec<Id>,
}

fn focusable_id() -> Id {
    Id::unique("translate-card-widgets")
}

/// One tab's card. Returns the button chosen.
pub fn card(ctx: &egui::Context, at: Rect, tab: &str, card: &Card, pal: &Palette) -> Option<Choice> {
    let ink = Ink::new(pal.panel, pal.accent);
    let now = ctx.input(|input| input.time);
    let armed_id = Id::unique(("translate-card-armed", tab));
    let previous: Option<Armed> = ctx.data(|data| data.get_temp(armed_id));
    let armed = previous.filter(|armed| armed.card.title == card.title && armed.card.buttons == card.buttons).unwrap_or(Armed { card: card.clone(), since: now });
    let ready = now - armed.since >= INPUT_DELAY;
    if !ready { ctx.request_repaint_after(std::time::Duration::from_secs_f64((armed.since + INPUT_DELAY - now).max(0.0))); }
    ctx.data_mut(|data| data.insert_temp(armed_id, armed));
    let mut picked = None;
    let mut ids = Vec::new();
    egui::Area::new(Id::unique(("translate-card", tab))).order(egui::Order::Foreground).fixed_pos(at.min).show(ctx, |ui| {
        let frame = crate::ui::overlays::menu_frame(pal).inner_margin(egui::Margin::same(12));
        frame.show(ui, |ui| {
            style::apply_visuals(ui, &ink);
            ui.set_width(at.width() - frame.total_margin().sum().x);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.add(egui::Label::new(style::text(&card.title, 13.5, style::weight::SEMIBOLD).color(pal.panel_text)).wrap());
            if let Some(detail) = &card.detail {
                ui.add(egui::Label::new(style::text(detail, 12.5, style::weight::REGULAR).color(ink.secondary)).wrap());
            }
            if let Some(error) = &card.error {
                ui.add(egui::Label::new(style::text(error, 12.5, style::weight::REGULAR).color(ink.danger)).wrap());
            }
            ui.horizontal_wrapped(|ui| {
                for (index, (choice, label)) in card.buttons.iter().enumerate() {
                    let kind = if index == 0 && card.primary { Kind::Primary } else if *choice == Choice::Close { Kind::Plain } else { Kind::Secondary };
                    let response = Button::new(label).kind(kind).size(Size::Small).enabled(ready).show(ui, &ink);
                    ids.push(response.id);
                    if response.clicked() { picked = Some(choice.clone()); }
                }
            });
        });
    });
    ctx.data_mut(|data| data.insert_temp(focusable_id(), Focusable { tab: tab.to_owned(), ids }));
    picked
}

/// Escape from a card button the keyboard is on, which the app's shortcuts
/// take before the card is drawn: the tab whose card closes.
pub fn dismiss_focused_card(ctx: &egui::Context) -> Option<String> {
    let focusable: Focusable = ctx.data(|data| data.get_temp(focusable_id()))?;
    let focused = focusable.ids.iter().any(|id| ctx.memory(|memory| memory.has_focus(*id) || memory.had_focus_last_frame(*id)));
    if !focused { return None; }
    ctx.data_mut(|data| data.remove::<Focusable>(focusable_id()));
    Some(focusable.tab)
}

/// Whether something goes before the Translate card at `at` on `tab`'s
/// pane, so two cards never share a place: a login or address offer on the
/// same page, which draws there, or a permission prompt or notice on that
/// page or drawn over `at` (`prompt`, the focused tab's column: see
/// [`crate::ui::site_permissions::goes_first`]).
fn offered(passwords: &crate::passwords::Passwords, perms: &crate::site_permissions::SitePermissions, tab: &str, at: Rect, prompt: Option<Rect>) -> bool {
    passwords.offer(tab).is_some() || passwords.address_offer(tab).is_some() || crate::ui::site_permissions::goes_first(perms, tab, at, prompt)
}

/// Every visible pane's card, while nothing covers the window.
pub fn cards(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette) {
    if app.ui.settings.is_some() || app.ui.command.is_some() || app.ui.onboarding.is_some() || app.ui.peek.is_some() { return; }
    let ctx = ui.ctx().clone();
    let find_tab = app.ui.find.is_some().then(|| app.focused_tab_id()).flatten();
    let prompt = crate::ui::site_permissions::shown_column(app, ctx.content_rect());
    for (tab, pane) in app.ui.panes.clone() {
        let at = crate::ui::logins::offer_rect(pane, find_tab.as_deref() == Some(tab.as_str()));
        if offered(&app.passwords, &app.site_permissions, &tab, at, prompt) {
            // Back after waiting, it takes no clicks for a moment, as after any change.
            ctx.data_mut(|data| data.remove::<Armed>(Id::unique(("translate-card-armed", tab.as_str()))));
            continue;
        }
        let Some(shown) = app.translator.card(&tab) else { continue };
        let Some(choice) = card(&ctx, at, &tab, &shown, pal) else { continue };
        app.actions.push(Action::Translate(tab, choice));
        ctx.request_repaint();
    }
}

#[cfg(test)]
#[path = "translate_tests.rs"]
mod tests;
