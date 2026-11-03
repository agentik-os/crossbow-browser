//! Capture's own UI: the selection over one page, the card a finished
//! capture shows near it, and the Captures list's rows and viewer. The host
//! (the main window or a Little Window) owns the page and runs the actions.

use crate::capture;
use crate::ui::icons::{self, Icon};
use crate::ui::style;
use crate::ui::theme::Palette;
use egui::epaint::Shadow;
use egui::text::{LayoutJob, TextWrapping};
use egui::{Align2, Color32, CornerRadius, CursorIcon, Event, Id, Key, LayerId, Order, PointerButton, Pos2, Rect, Sense, Stroke, TextureId, Ui, Vec2, pos2, vec2};

pub const HINT: &str = "Drag to capture an area, or click to capture the page. Esc cancels.";
pub const COPY: &str = "Copy";
pub const SAVE_AS: &str = "Save As…";
pub const OPEN_IN_CAPTURES: &str = "Open in Captures";
pub const COPIED: &str = "Copied";
pub const COPIED_AND_SAVED: &str = "Copied · Saved to Captures";
pub const FILE_MISSING: &str = "File missing";
pub const REMOVE: &str = "Remove";
pub const MOVE_ASIDE: &str = "Move Aside";
/// On the card when a capture couldn't be saved because Captures is damaged.
pub const NOT_SAVED_DAMAGED: &str = "Not saved: Captures can't be read.";
/// The card hides after this long without the pointer over it.
pub const CARD_FOR: f64 = 6.0;
const DIM: Color32 = Color32::from_black_alpha(110);
/// A drag shorter than this both ways is a click: the visible page.
const MIN_DRAG: f32 = 4.0;
const CARD_WIDTH: f32 = 304.0;
const CARD_HEIGHT: f32 = 64.0;
const GAP: f32 = 8.0;

/// A primary press with Command and Shift on `page` this pass, when
/// Command-Shift drag captures. Otherwise the page gets the press as it is.
pub fn starts(events: &[Event], page: Rect, drag_captures: bool) -> Option<Pos2> {
    if !drag_captures { return None; }
    events.iter().find_map(|event| match *event {
        Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers } if modifiers.command && modifiers.shift && !modifiers.alt && page.contains(pos) => Some(pos),
        _ => None,
    })
}

/// A capture being chosen over one page.
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    /// The page, in points, when the selection began.
    pub page: Rect,
    /// Where the drag began.
    anchor: Option<Pos2>,
    pub rect: Option<Rect>,
    last: Option<Pos2>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    Selecting,
    Cancelled,
    /// The part of the page to capture, in points.
    Chosen(Rect),
}

impl Selection {
    pub fn new(page: Rect) -> Selection {
        Selection { page, anchor: None, rect: None, last: None }
    }

    /// Take this pass's pointer and keys. A drag selects, and moves the
    /// selection while Space is held; a release or Return captures it, or
    /// the visible page after a plain click; Escape or a press outside the
    /// page cancels.
    pub fn input(&mut self, events: &[Event], space_down: bool) -> Outcome {
        for event in events {
            match *event {
                Event::Key { key: Key::Escape, pressed: true, .. } => return Outcome::Cancelled,
                Event::Key { key: Key::Enter, pressed: true, .. } => return Outcome::Chosen(self.chosen()),
                Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, .. } => {
                    if !self.page.contains(pos) { return Outcome::Cancelled; }
                    (self.anchor, self.rect, self.last) = (Some(pos), None, Some(pos));
                }
                Event::PointerMoved(pos) => {
                    let (Some(anchor), Some(last)) = (self.anchor, self.last) else { continue };
                    match self.rect {
                        Some(rect) if space_down => {
                            let moved = capture::moved(rect, pos - last, self.page);
                            self.anchor = Some(anchor + (moved.min - rect.min));
                            self.rect = Some(moved);
                        }
                        _ => self.rect = Some(capture::dragged(anchor, pos, self.page)),
                    }
                    self.last = Some(pos);
                }
                Event::PointerButton { button: PointerButton::Primary, pressed: false, .. } if self.anchor.take().is_some() => {
                    return Outcome::Chosen(self.chosen());
                }
                _ => {}
            }
        }
        Outcome::Selecting
    }

    fn chosen(&self) -> Rect {
        self.rect.filter(|rect| rect.width() >= MIN_DRAG || rect.height() >= MIN_DRAG).unwrap_or(self.page)
    }
}

/// Dim the page around the selection, outline it and label its size in
/// pixels, or show the hint before a drag. Drawn over the page only.
pub fn paint(ctx: &egui::Context, selection: &Selection, texture: Option<(u32, u32)>, ppp: f32) {
    let page = selection.page;
    let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::unique("capture-selection"))).with_clip_rect(page);
    match selection.rect {
        Some(rect) => {
            for dim in [
                Rect::from_min_max(page.min, pos2(page.max.x, rect.min.y)),
                Rect::from_min_max(pos2(page.min.x, rect.max.y), page.max),
                Rect::from_min_max(pos2(page.min.x, rect.min.y), pos2(rect.min.x, rect.max.y)),
                Rect::from_min_max(pos2(rect.max.x, rect.min.y), pos2(page.max.x, rect.max.y)),
            ] {
                if dim.is_positive() { painter.rect_filled(dim, 0.0, DIM); }
            }
            painter.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::from_black_alpha(140)), egui::StrokeKind::Outside);
            painter.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::WHITE), egui::StrokeKind::Inside);
            let size = texture.and_then(|texture| capture::pixels(rect, page, texture, ppp));
            if let Some(size) = size {
                let label = format!("{} × {}", size.width, size.height);
                pill(&painter, page, &label, |galley| {
                    let below = pos2(rect.max.x - galley.x, rect.max.y + 6.0);
                    if below.y + galley.y <= page.max.y { below } else { pos2(rect.max.x - galley.x - 6.0, rect.max.y - galley.y - 6.0) }
                });
            }
        }
        None => {
            painter.rect_filled(page, 0.0, DIM);
            pill(&painter, page, HINT, |galley| pos2(page.center().x - galley.x / 2.0, page.min.y + 16.0));
        }
    }
    if ctx.input(|input| input.pointer.hover_pos()).is_some_and(|at| page.contains(at)) {
        ctx.set_cursor_icon(CursorIcon::Crosshair);
    }
}

/// White text on a dark pill at `at(size)`, kept within `page`.
fn pill(painter: &egui::Painter, page: Rect, text: &str, at: impl FnOnce(Vec2) -> Pos2) {
    let pad = vec2(8.0, 4.0);
    let mut job = LayoutJob::single_section(text.to_owned(), style::format(12.0, style::weight::MEDIUM, Color32::WHITE));
    job.wrap = TextWrapping { max_width: (page.width() - 2.0 * pad.x - 16.0).max(40.0), ..Default::default() };
    let galley = painter.layout_job(job);
    let size = galley.size() + 2.0 * pad;
    let min = at(size);
    let min = pos2(min.x.clamp(page.min.x + 4.0, (page.max.x - size.x - 4.0).max(page.min.x + 4.0)), min.y.clamp(page.min.y + 4.0, (page.max.y - size.y - 4.0).max(page.min.y + 4.0)));
    let rect = Rect::from_min_size(min, size);
    painter.rect_filled(rect, style::radius::SM, Color32::from_black_alpha(190));
    painter.galley(rect.min + pad, galley, Color32::WHITE);
}

/// A finished capture's state, as a Little Window's card shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CardStatus {
    pub saved: bool,
    pub notice: Option<String>,
}

/// What a finished capture's card shows.
#[derive(Clone, Debug, PartialEq)]
pub struct CardView {
    /// The selection it came from, in points.
    pub anchor: Rect,
    /// The window's content, which the card stays inside.
    pub bounds: Rect,
    /// It was saved to Captures, so Open in Captures can show it.
    pub saved: bool,
    /// Why the copy or the save failed.
    pub notice: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardAction {
    Copy,
    SaveAs,
    OpenInCaptures,
    Close,
}

/// Where the card goes: under the selection's right edge, else above it,
/// else inside its bottom, always within `bounds`.
pub fn card_rect(anchor: Rect, bounds: Rect) -> Rect {
    let size = vec2(CARD_WIDTH.min(bounds.width() - 2.0 * GAP), CARD_HEIGHT);
    let x = (anchor.max.x - size.x).clamp(bounds.min.x + GAP, (bounds.max.x - size.x - GAP).max(bounds.min.x + GAP));
    let y = if anchor.max.y + GAP + size.y <= bounds.max.y - GAP {
        anchor.max.y + GAP
    } else if anchor.min.y - GAP - size.y >= bounds.min.y + GAP {
        anchor.min.y - GAP - size.y
    } else {
        (anchor.max.y - size.y - GAP).clamp(bounds.min.y + GAP, bounds.max.y - size.y - GAP)
    };
    Rect::from_min_size(pos2(x, y), size)
}

/// The card: what happened, then Copy, Save As…, Open in Captures and
/// Close. Its controls take no keyboard focus, so the page keeps it.
/// Returns the action chosen and whether the pointer is over the card.
pub fn card(ctx: &egui::Context, view: &CardView, pal: &Palette) -> (Option<CardAction>, bool) {
    let rect = card_rect(view.anchor, view.bounds);
    let mut chosen = None;
    let mut hovered = false;
    egui::Area::new(Id::unique("capture-card")).order(Order::Foreground).fixed_pos(rect.min).constrain(false).show(ctx, |ui| {
        ui.set_min_size(rect.size());
        let painter = ui.painter().clone();
        painter.add(Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(60) }.as_shape(rect, CornerRadius::same(style::radius::MD as u8)));
        painter.rect_filled(rect, style::radius::MD, pal.panel);
        painter.rect_stroke(rect, style::radius::MD, Stroke::new(1.0, pal.panel_border), egui::StrokeKind::Inside);
        hovered = ui.rect_contains_pointer(rect);
        let inner = rect.shrink2(vec2(12.0, 8.0));
        let (status, color) = match &view.notice {
            Some(notice) => (notice.as_str(), pal.panel_text),
            None => (if view.saved { COPIED_AND_SAVED } else { COPIED }, pal.panel_dim),
        };
        let mut job = LayoutJob::single_section(status.to_owned(), style::format(12.0, style::weight::MEDIUM, color));
        job.wrap = TextWrapping { max_width: inner.width() - 28.0, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        painter.galley(inner.min, painter.layout_job(job), color);
        let close = Rect::from_min_size(pos2(inner.max.x - 20.0, inner.min.y - 2.0), vec2(20.0, 20.0));
        if card_icon(ui, close, Icon::Close, "Close", pal) { chosen = Some(CardAction::Close); }
        let mut x = inner.min.x - 6.0;
        let row = inner.max.y - 26.0;
        for (label, action, enabled) in [(COPY, CardAction::Copy, true), (SAVE_AS, CardAction::SaveAs, true), (OPEN_IN_CAPTURES, CardAction::OpenInCaptures, view.saved)] {
            let galley = painter.layout_no_wrap(label.to_owned(), egui::FontId::proportional(12.5), pal.panel_text);
            let button = Rect::from_min_size(pos2(x, row), vec2(galley.size().x + 16.0, 26.0));
            x = button.max.x + 4.0;
            if text_button(ui, button, label, enabled, pal) { chosen = Some(action); }
        }
    });
    (chosen, hovered)
}

/// A text button that never takes keyboard focus.
fn text_button(ui: &mut Ui, rect: Rect, label: &str, enabled: bool, pal: &Palette) -> bool {
    let response = ui.interact(rect, Id::unique(("capture-card-button", label)), if enabled { Sense::CLICK } else { Sense::hover() });
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    if enabled && response.hovered() {
        ui.painter().rect_filled(rect, style::radius::SM, pal.panel_hover);
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let color = if enabled { pal.panel_text } else { pal.panel_dim.gamma_multiply(0.6) };
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, egui::FontId::proportional(12.5), color);
    enabled && response.clicked()
}

/// An icon button that never takes keyboard focus, with a tooltip.
fn card_icon(ui: &mut Ui, rect: Rect, icon: Icon, label: &str, pal: &Palette) -> bool {
    let response = ui.interact(rect, Id::unique(("capture-card-icon", label)), Sense::CLICK);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if response.hovered() {
        ui.painter().rect_filled(rect, 6.0, pal.panel_hover);
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    icons::draw(ui.painter(), Rect::from_center_size(rect.center(), Vec2::splat(rect.height() * 0.6)), icon, pal.panel_dim);
    response.on_hover_text(label).clicked()
}

/// One row of the Captures list.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: i64,
    pub title: String,
    /// The page's host and the capture's time.
    pub detail: String,
    pub missing: bool,
    pub thumbnail: Option<(TextureId, Vec2)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListAction {
    Open(i64),
    Copy(i64),
    Reveal(i64),
    SaveAs(i64),
    Delete(i64),
    Remove(i64),
}

/// Captures can't be listed: why, centered, then Move Aside when the file
/// is damaged. Whether Move Aside was clicked.
pub fn unreadable(ui: &mut Ui, message: &str, damaged: bool, pal: &Palette) -> bool {
    use crate::ui::style::{Button, Ink, Size};
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new(message).size(12.5).color(pal.panel_dim));
        if !damaged { return false; }
        ui.add_space(10.0);
        Button::new(MOVE_ASIDE).size(Size::Small).show(ui, &Ink::new(pal.panel, pal.accent)).clicked()
    }).inner
}

pub const ROW_HEIGHT: f32 = 52.0;
const THUMB: Vec2 = vec2(64.0, 40.0);

/// A capture's row: thumbnail, title, host and time. Its actions show while
/// the pointer is over it; a missing file offers only Remove.
pub fn row(ui: &mut Ui, row: &Row, pal: &Palette) -> Option<ListAction> {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    let hovered = ui.rect_contains_pointer(rect);
    if hovered { ui.painter().rect_filled(rect, 6, pal.panel_hover); }
    let thumb = Rect::from_min_size(pos2(rect.min.x + 6.0, rect.center().y - THUMB.y / 2.0), THUMB);
    ui.painter().rect_filled(thumb, 4, pal.panel_border);
    if let Some((texture, size)) = row.thumbnail.filter(|_| !row.missing) {
        let fit = (THUMB.x / size.x).min(THUMB.y / size.y);
        let shown = Rect::from_center_size(thumb.center(), size * fit);
        ui.painter().image(texture, shown, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
    }
    let actions: &[(Icon, &str, ListAction)] = if row.missing {
        &[]
    } else {
        &[(Icon::Copy, COPY, ListAction::Copy(row.id)), (Icon::FolderOpen, "Reveal in Finder", ListAction::Reveal(row.id)), (Icon::Download, SAVE_AS, ListAction::SaveAs(row.id)), (Icon::Trash, "Delete", ListAction::Delete(row.id))]
    };
    let mut right = rect.max.x - 6.0;
    let mut chosen = None;
    if row.missing {
        let galley = ui.painter().layout_no_wrap(REMOVE.to_owned(), egui::FontId::proportional(12.5), pal.panel_text);
        let button = Rect::from_min_size(pos2(right - galley.size().x - 16.0, rect.center().y - 13.0), vec2(galley.size().x + 16.0, 26.0));
        right = button.min.x - 4.0;
        if text_button(ui, button, REMOVE, true, pal) { chosen = Some(ListAction::Remove(row.id)); }
    } else if hovered || response.has_focus() {
        for (icon, label, action) in actions.iter().rev() {
            let button = Rect::from_min_size(pos2(right - 26.0, rect.center().y - 13.0), vec2(26.0, 26.0));
            right = button.min.x - 2.0;
            if icons::button(ui, button, *icon, pal.panel_dim, pal.panel_border, true).on_hover_text(*label).clicked() { chosen = Some(*action); }
        }
    }
    let text_left = thumb.max.x + 10.0;
    let width = (right - text_left - 4.0).max(20.0);
    let line = |text: &str, size: f32, color: Color32| {
        let mut job = LayoutJob::single_section(text.to_owned(), style::format(size, style::weight::REGULAR, color));
        job.wrap = TextWrapping { max_width: width, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        ui.painter().layout_job(job)
    };
    let title = line(&row.title, 13.0, pal.panel_text);
    let detail = line(if row.missing { FILE_MISSING } else { &row.detail }, 11.5, pal.panel_dim);
    ui.painter().galley(pos2(text_left, rect.center().y - title.size().y), title, pal.panel_text);
    ui.painter().galley(pos2(text_left, rect.center().y + 1.0), detail, pal.panel_dim);
    if chosen.is_none() && !row.missing && response.clicked() { chosen = Some(ListAction::Open(row.id)); }
    chosen
}

/// The in-app viewer: one capture over the window, fitted, with its title
/// and Close. Returns true when it should close.
pub fn viewer(ctx: &egui::Context, title: &str, image: Option<(TextureId, Vec2)>, pal: &Palette) -> bool {
    let screen = ctx.content_rect();
    let mut close = false;
    egui::Area::new(Id::unique("capture-viewer")).order(Order::Foreground).fixed_pos(screen.min).constrain(false).show(ctx, |ui| {
        ui.set_min_size(screen.size());
        let backdrop = ui.interact(screen, Id::unique("capture-viewer-backdrop"), Sense::click());
        ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(200));
        let bar = Rect::from_min_size(screen.min, vec2(screen.width(), 44.0));
        let close_rect = Rect::from_center_size(pos2(bar.max.x - 26.0, bar.center().y), vec2(26.0, 26.0));
        let mut job = LayoutJob::single_section(title.to_owned(), style::format(13.0, style::weight::MEDIUM, Color32::WHITE));
        job.wrap = TextWrapping { max_width: (close_rect.min.x - bar.min.x - 32.0).max(40.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        let galley = ui.painter().layout_job(job);
        ui.painter().galley(pos2(bar.min.x + 16.0, bar.center().y - galley.size().y / 2.0), galley, Color32::WHITE);
        if icons::button(ui, close_rect, Icon::Close, Color32::WHITE, pal.panel_hover.gamma_multiply(0.5), true).on_hover_text("Close").clicked() { close = true; }
        let area = Rect::from_min_max(pos2(screen.min.x + 24.0, bar.max.y), screen.max - vec2(24.0, 24.0));
        match image {
            Some((texture, size)) => {
                // Never larger than its pixels.
                let fit = (area.width() / size.x).min(area.height() / size.y).min(1.0 / ctx.pixels_per_point());
                let shown = Rect::from_center_size(area.center(), size * fit);
                ui.painter().image(texture, shown, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
                if backdrop.clicked() && !ui.input(|input| input.pointer.interact_pos()).is_some_and(|at| shown.contains(at)) { close = true; }
            }
            None => {
                ui.painter().text(area.center(), Align2::CENTER_CENTER, FILE_MISSING, egui::FontId::proportional(13.0), Color32::WHITE);
                if backdrop.clicked() { close = true; }
            }
        }
    });
    close
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
