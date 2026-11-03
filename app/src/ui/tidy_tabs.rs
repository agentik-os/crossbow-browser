//! Tidy surfaces: the small card under a pin or Today's divider, and the Tidy
//! Today review sheet. Model text is drawn as plain, elided labels only.

use crate::app::{CrossbowApp, RenameTarget};
use crate::tidy_tabs::{Sheet, Stage, TODAY_PROVIDER};
use crate::ui::icons::{self, Icon};
use crate::ui::style::{self, Button, Ink, Kind};
use crate::ui::theme::Palette;
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align2, Color32, FontId, Id, Key, Rect, Sense, Stroke, Ui, pos2, vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardAction { Accept, Edit, Revert, Cancel, Undo, Dismiss }

const CARD_INSET: f32 = 10.0;
const BUTTON_H: f32 = 22.0;
const GAP: f32 = 6.0;

/// One line cut with "…" to fit `max_width`.
fn one_line(ui: &Ui, text: &str, size: f32, color: Color32, max_width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::single_section(text.to_owned(), TextFormat::simple(FontId::proportional(size), color));
    job.wrap = TextWrapping { max_width, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    ui.painter().layout_job(job)
}

/// A card of one text line above its buttons, which wrap to fit the width.
pub fn card(ui: &mut Ui, pal: &Palette, left: f32, width: f32, text: &str, dim: bool, buttons: &[(&str, CardAction)]) -> Option<CardAction> {
    let labels: Vec<_> = buttons.iter().map(|(label, _)| one_line(ui, label, 12.0, pal.text, width - 2.0 * CARD_INSET)).collect();
    let mut places = Vec::new();
    let (mut x, mut row) = (CARD_INSET, 0.0f32);
    for label in &labels {
        let w = label.size().x + 14.0;
        if x > CARD_INSET && x + w > width - CARD_INSET {
            x = CARD_INSET;
            row += 1.0;
        }
        places.push(vec2(x, 30.0 + row * (BUTTON_H + 4.0)));
        x += w + GAP;
    }
    let height = 30.0 + (row + 1.0) * (BUTTON_H + 4.0) + 4.0;
    let (outer, _) = ui.allocate_exact_size(vec2(ui.available_width(), height + 4.0), Sense::hover());
    let rect = Rect::from_min_size(pos2(outer.left() + left, outer.top() + 2.0), vec2(width, height));
    ui.painter().rect_filled(rect, 8, pal.hover);
    icons::draw(ui.painter(), Rect::from_center_size(pos2(rect.left() + CARD_INSET + 6.0, rect.top() + 16.0), vec2(12.0, 12.0)), Icon::Sparkle, pal.text_dim);
    let galley = one_line(ui, text, 12.5, if dim { pal.text_dim } else { pal.text }, width - 2.0 * CARD_INSET - 18.0);
    ui.painter().galley(pos2(rect.left() + CARD_INSET + 18.0, rect.top() + 16.0 - galley.size().y / 2.0), galley, pal.text);
    let mut chosen = None;
    for ((label, action), (galley, place)) in buttons.iter().zip(labels.into_iter().zip(places)) {
        let r = Rect::from_min_size(rect.min + place, vec2(galley.size().x + 14.0, BUTTON_H));
        let response = ui.interact(r, ui.scope_id().with(("tidy-card", *label)), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, *label));
        if response.hovered() || response.has_focus() { ui.painter().rect_filled(r, 6, pal.selected); }
        if response.has_focus() { ui.painter().rect_stroke(r, 6, Stroke::new(1.5, pal.accent), egui::StrokeKind::Inside); }
        ui.painter().galley(r.center() - galley.size() / 2.0, galley, pal.text);
        if response.clicked() { chosen = Some(*action); }
    }
    chosen
}

/// Tidy Tab Titles' card under its pin: the pending request, or the suggestion.
pub fn title_card(app: &mut CrossbowApp, ui: &mut Ui, item: &str, left: f32, width: f32, pal: &Palette) {
    let action = if app.tidy_tabs.title_pending() == Some(item) {
        card(ui, pal, left, width, "Suggesting a title…", true, &[("Cancel", CardAction::Cancel)])
    } else if let Some(text) = app.tidy_tabs.suggestion.as_ref().filter(|s| s.basis.item == item).map(|s| format!("Suggested: {}", s.text)) {
        card(ui, pal, left, width, &text, false, &[("Accept", CardAction::Accept), ("Edit", CardAction::Edit), ("Revert", CardAction::Revert)])
    } else {
        return;
    };
    match action {
        Some(CardAction::Accept) => app.tidy_tabs.accept_suggestion(&mut app.lib),
        Some(CardAction::Edit) => {
            if let Some((item, text)) = app.tidy_tabs.edit_suggestion() { app.ui.renaming = Some((RenameTarget::Item(item), text)); }
        }
        Some(CardAction::Revert) => app.tidy_tabs.revert_suggestion(),
        Some(CardAction::Cancel) => app.tidy_tabs.cancel_title(),
        _ => {}
    }
}

/// After Tidy Today, under the Space's divider: one Undo.
pub fn undo_card(app: &mut CrossbowApp, ui: &mut Ui, space: &str, left: f32, width: f32, pal: &Palette) {
    let Some(summary) = app.tidy_tabs.undo.as_ref().filter(|undo| undo.space == space).map(|undo| undo.summary.clone()) else { return };
    match card(ui, pal, left, width, &summary, false, &[("Undo", CardAction::Undo), ("Dismiss", CardAction::Dismiss)]) {
        Some(CardAction::Undo) => app.tidy_tabs.undo(&mut app.lib, &mut app.tabs),
        Some(CardAction::Dismiss) => app.tidy_tabs.undo = None,
        _ => {}
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetAction { Accept, Cancel, TryAgain }

const SHEET_WIDTH: f32 = 420.0;
/// Shown before Accept: pinned tabs are never auto-archived.
const PINNED_NOTE: &str = "Folders are added to Pinned; their tabs no longer archive automatically.";

fn status(sheet: &Sheet, stale: bool) -> String {
    match &sheet.stage {
        Stage::Asking => format!("Asking {} to sort {} Today tabs into folders…", sheet.destination, sheet.tabs.len()),
        Stage::Review(_) if stale => "Some tabs changed since this proposal, so it can't be applied. Try Again uses the current tabs.".into(),
        Stage::Review(groups) => {
            let used: usize = groups.iter().map(|group| group.tabs.len()).sum();
            let folders = if groups.len() == 1 { "1 folder".to_owned() } else { format!("{} folders", groups.len()) };
            format!("{folders} for {used} of {} tabs. Nothing changes until you accept.", sheet.tabs.len())
        }
        Stage::Failed(reason) => format!("{reason} Nothing was changed."),
    }
}

/// Escape answers Cancel before shortcuts or a page can take it.
pub fn take_escape(ctx: &egui::Context) -> bool {
    ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Escape))
}

/// The review sheet above everything else.
pub fn sheet(ctx: &egui::Context, pal: &Palette, sheet: &Sheet, stale: bool) -> Option<SheetAction> {
    let screen = ctx.content_rect();
    let backdrop = egui::Area::new(Id::unique("tidy-backdrop")).order(egui::Order::Foreground).fixed_pos(screen.min).sense(Sense::hover());
    let layer = backdrop.layer();
    ctx.move_to_top(layer);
    ctx.memory_mut(|memory| memory.set_modal_layer(layer));
    backdrop.show(ctx, |ui| {
        ui.allocate_rect(screen, Sense::CLICK);
        ui.painter().rect_filled(screen, 0, Color32::from_black_alpha(80));
    });
    let card = egui::Area::new(Id::unique("tidy-sheet")).order(egui::Order::Foreground).pivot(Align2::CENTER_CENTER).fixed_pos(screen.center()).sense(Sense::hover());
    ctx.set_sublayer(layer, card.layer());
    let ink = Ink::new(pal.panel, pal.accent);
    let width = (screen.width() - 32.0).min(SHEET_WIDTH);
    card.show(ctx, |ui| {
        let frame = egui::Frame::new().fill(pal.panel).stroke(Stroke::new(1.0, pal.panel_border)).corner_radius(14).inner_margin(20)
            .shadow(egui::epaint::Shadow { offset: [0, 20], blur: 56, spread: 0, color: Color32::from_black_alpha(90) });
        frame.show(ui, |ui| {
            ui.set_width(width - 40.0);
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(style::text("Tidy Today", 15.0, style::weight::SEMIBOLD).color(ink.text));
            ui.label(style::text(status(sheet, stale), 12.5, style::weight::REGULAR).color(if stale { ink.danger } else { ink.secondary }));
            if let (Stage::Review(groups), false) = (&sheet.stage, stale) {
                ui.label(style::text(PINNED_NOTE, 12.5, style::weight::REGULAR).color(ink.text));
                // The list scrolls; the title, status and buttons stay in view.
                let list_height = (screen.height() - 48.0 - 234.0).max(72.0);
                egui::ScrollArea::vertical().id_salt("tidy-proposal").max_height(list_height).auto_shrink([false, true]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for group in groups {
                        ui.horizontal(|ui| {
                            let (icon, _) = ui.allocate_exact_size(vec2(14.0, 18.0), Sense::hover());
                            icons::draw(ui.painter(), Rect::from_center_size(icon.center(), vec2(13.0, 13.0)), Icon::Folder, ink.secondary);
                            ui.add(egui::Label::new(style::text(&group.name, 13.0, style::weight::SEMIBOLD).color(ink.text)).truncate());
                        });
                        let mut tabs = group.tabs.clone();
                        tabs.sort_unstable();
                        for i in tabs {
                            ui.horizontal(|ui| {
                                ui.add_space(22.0);
                                ui.add(egui::Label::new(style::text(&sheet.tabs[i].title, 12.5, style::weight::REGULAR).color(ink.secondary)).truncate());
                            });
                        }
                        ui.add_space(6.0);
                    }
                    let left = sheet.tabs.len() - groups.iter().map(|group| group.tabs.len()).sum::<usize>();
                    if left > 0 {
                        let text = if left == 1 { "1 tab stays in Today.".to_owned() } else { format!("{left} tabs stay in Today.") };
                        ui.label(style::text(text, 12.0, style::weight::REGULAR).color(ink.secondary));
                    }
                });
            }
            ui.add_space(4.0);
            ui.allocate_ui_with_layout(vec2(ui.available_width(), 32.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let primary = match (&sheet.stage, stale) {
                    (Stage::Asking, _) => None,
                    (Stage::Review(_), false) => Some(("Accept", SheetAction::Accept)),
                    _ => Some(("Try Again", SheetAction::TryAgain)),
                };
                let mut chosen = None;
                if let Some((label, action)) = primary {
                    if Button::new(label).kind(Kind::Primary).show(ui, &ink).clicked() { chosen = Some(action); }
                }
                if Button::new("Cancel").kind(Kind::Secondary).show(ui, &ink).clicked() { chosen = Some(SheetAction::Cancel); }
                chosen
            }).inner
        }).inner
    }).inner
}

/// Draw the sheet for the app and run what it answers.
pub fn show_sheet(app: &mut CrossbowApp, ctx: &egui::Context, escape: bool) {
    let Some(open) = app.tidy_tabs.sheet.as_ref() else { return };
    let stale = matches!(open.stage, Stage::Review(_)) && open.stale(&app.lib, &app.tabs);
    let answer = sheet(ctx, &app.palette(), open, stale).or(escape.then_some(SheetAction::Cancel));
    match answer {
        Some(SheetAction::Accept) => app.tidy_tabs.accept(&mut app.lib, &mut app.tabs),
        Some(SheetAction::TryAgain) => app.tidy_tabs.try_again(&app.lib, &app.tabs, &TODAY_PROVIDER, ctx),
        Some(SheetAction::Cancel) => app.tidy_tabs.close_sheet(),
        None => {}
    }
}

#[cfg(test)]
#[path = "tidy_tabs_ui_tests.rs"]
mod tests;
