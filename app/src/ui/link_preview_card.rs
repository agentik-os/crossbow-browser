//! The 5-Second Previews card, by the hovered link. It holds no controls and
//! never takes focus, so the page keeps the keyboard; Escape reaches it
//! through the app's dismissal chain only while it is shown.

use crate::app::CrossbowApp;
use crate::link_preview::{Card, Engine, Input, PROVIDER, Shown};
use crate::ui::style;
use crate::ui::theme::Palette;
use crossbow_core::link_preview::SystemResolver;
use egui::{Id, Pos2, Rect, Vec2, pos2, vec2};

pub(crate) const WIDTH: f32 = 320.0;
/// Between the pointer and the card.
const GAP: f32 = 18.0;

/// This frame's input to the previews, then the card if one is shown.
pub(crate) fn show(app: &mut CrossbowApp, ctx: &egui::Context, pal: &Palette) {
    let hover = app.link_previews.seen.take();
    let (now, shift, pointer, pressed, scrolled) = ctx.input(|input| {
        let m = input.modifiers;
        let scrolled = input.smooth_scroll_delta != Vec2::ZERO || input.events.iter().any(|event| matches!(event, egui::Event::MouseWheel { .. }));
        (input.time, m.shift && !m.command && !m.alt && !m.ctrl, input.pointer.hover_pos(), input.pointer.any_pressed(), scrolled)
    });
    let over_card = pointer.zip(app.link_previews.card_rect).is_some_and(|(pointer, card)| card.contains(pointer));
    let live: Vec<(u64, u64)> = app.tabs.iter().filter_map(|tab| tab.view.as_ref()).chain(app.ui.peek.as_ref().map(|peek| &peek.view))
        .map(|view| (view.serial(), view.navigations())).collect();
    let interrupted = pressed || scrolled || app.web_overlay() || app.ui.menu.is_some();
    let input = Input { now, hover, shift, interrupted, over_card, live: &live };
    app.link_previews.tick(input, &app.lib, &Engine(&app.data_dir), &SystemResolver, &PROVIDER, ctx);
    app.link_previews.card_rect = app.link_previews.shown().map(|shown| card(ctx, &shown, pal));
}

/// Below the pointer, or above it when there's no room, kept inside the page.
pub(crate) fn placement(anchor: Pos2, area: Rect, size: Vec2) -> Pos2 {
    let x = (anchor.x - 12.0).min(area.right() - size.x - 8.0).max(area.left() + 8.0);
    let below = anchor.y + GAP;
    let y = if below + size.y <= area.bottom() - 8.0 { below } else { (anchor.y - GAP - size.y).max(area.top() + 8.0) };
    pos2(x, y)
}

/// Draws the card and returns where it is.
pub(crate) fn card(ctx: &egui::Context, shown: &Shown, pal: &Palette) -> Rect {
    let id = Id::unique("link-preview-card");
    let width = WIDTH.min(shown.area.width() - 16.0).max(160.0);
    // Last frame's size places this one; a new card starts small.
    let size = ctx.memory(|memory| memory.area_rect(id)).map_or(vec2(width, 72.0), |rect| rect.size());
    let ink = style::Ink::new(pal.panel, pal.accent);
    let label = |text: String, size: f32, color| egui::Label::new(style::text(text, size, style::weight::REGULAR).color(color)).selectable(false).wrap();
    egui::Area::new(id)
        .order(egui::Order::Tooltip)
        .fixed_pos(placement(shown.anchor, shown.area, size))
        .constrain(true)
        .sense(egui::Sense::hover())
        .show(ctx, |ui| {
            super::overlays::menu_frame(pal).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
                ui.set_width(width - 24.0);
                ui.spacing_mut().item_spacing.y = 6.0;
                if !shown.host.is_empty() { ui.add(label(shown.host.clone(), 11.5, ink.secondary)); }
                match &shown.card {
                    Card::Working(text) => { ui.add(label(text.clone(), 12.5, ink.secondary)); }
                    Card::Summary { text, provider } => {
                        ui.add(label(text.clone(), 13.0, pal.panel_text));
                        ui.add(label(format!("Summary by {provider} · {}", crate::link_preview::CAUTION), 11.5, ink.secondary));
                    }
                    Card::Notice(text) => { ui.add(label(text.clone(), 12.5, pal.panel_text)); }
                }
            });
        })
        .response
        .rect
}

#[cfg(test)]
#[path = "link_preview_card_tests.rs"]
mod tests;
