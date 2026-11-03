//! Window layout and the web-content compositor: the space gradient, the
//! sidebar, and one rounded card per visible tab (two to four in split view).

use crate::app::{Action, CrossbowApp, Focus};
use crate::input;
use crate::tabs::Split;
use crate::ui::{anim, icons, icons::Icon, sidebar, theme};
use crossbow_engine::{CursorKind, MouseButton};
use egui::epaint::{RectShape, Shadow};
use egui::{Color32, CornerRadius, CursorIcon, Id, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use std::collections::HashMap;

pub const GAP: f32 = 8.0;
pub const RADIUS: u8 = 10;

/// Margin around the content cards and their corner radius. Full screen drops both,
/// so pages meet the display edges with no frame around them.
pub(super) fn card_frame(ctx: &egui::Context) -> (f32, u8) {
    if ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) { (0.0, 0) } else { (GAP, RADIUS) }
}

pub fn layout_and_draw(app: &mut CrossbowApp, ui: &mut Ui) {
    app.ui.address = None;
    let ctx = ui.ctx().clone();
    let full = ui.max_rect();
    let pal = app.palette();
    let texture = app.lib.spaces.get(&app.active_space).map(|s| s.theme.texture).unwrap_or_default();
    let now = ctx.input(|i| i.time);

    // Background: the space gradient, cross-faded during space switches.
    let painter = ui.painter().clone();
    let mut space_t = 1.0;
    if let Some(a) = &mut app.ui.space_anim {
        if a.start < 0.0 {
            a.start = now;
        }
        space_t = anim::ease_out_cubic(anim::progress(&ctx, a.start, 0.32));
    }
    let tint = app.chrome_tint_opacity();
    if let Some(a) = app.ui.space_anim.as_ref().filter(|_| space_t < 1.0) {
        if let Some(from) = app.lib.spaces.get(&a.from) {
            let from_texture = from.theme.texture;
            let from = theme::palette(&from.theme, app.system_dark);
            let (old_alpha, new_alpha) = chrome_crossfade(tint, space_t);
            theme::paint_gradient(&painter, full, &from.stops, from.angle_deg, old_alpha);
            theme::paint_gradient(&painter, full, &pal.stops, pal.angle_deg, new_alpha);
            theme::paint_texture(&painter, full, &app.texture_bank, from_texture, from.grain * (1.0 - space_t) * tint, ctx.pixels_per_point());
        } else {
            theme::paint_gradient(&painter, full, &pal.stops, pal.angle_deg, tint);
        }
    } else {
        app.ui.space_anim = None;
        theme::paint_gradient(&painter, full, &pal.stops, pal.angle_deg, tint);
    }
    theme::paint_texture(&painter, full, &app.texture_bank, texture, pal.grain * space_t * tint, ctx.pixels_per_point());

    let shown = sidebar_shown(&ctx, app.ui.sidebar_visible);
    let on_right = app.lib.settings.sidebar_on_right;
    let native_strip = cfg!(target_os = "macos") && !ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
    let (edge, radius) = card_frame(&ctx);
    let (content, docked_sidebar) = sidebar_layout(&ctx, full, &mut app.ui.sidebar_width, shown, on_right, native_strip, edge);
    let w = app.ui.sidebar_width.clamp(200.0, 420.0);

    // Window drag strip above the content when the sidebar is hidden.
    if shown < 0.5 {
        draw_top_strip(app, ui, Rect::from_min_max(full.min, pos2(full.right(), content.top())), &pal);
    }

    draw_panes(app, ui, content, space_t, radius);

    // Docked sidebar.
    if shown > 0.01 {
        sidebar::show(app, ui, docked_sidebar, &pal, false);
        resize_handle(ui, docked_sidebar, on_right);
    } else {
        // Hidden: hovering the window edge floats the sidebar over the content.
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let edge = pointer.is_some_and(|p| if on_right { p.x > full.right() - 8.0 } else { p.x < full.left() + 8.0 });
        if edge {
            app.ui.sidebar_peek = true;
        }
        let peek_t = anim::spring(&ctx, Id::unique("sidebar-peek"), if app.ui.sidebar_peek { 1.0 } else { 0.0 }, 300.0);
        if peek_t > 0.01 {
            let rect = floating_sidebar_rect(full, w, peek_t, on_right, native_strip);
            show_floating_sidebar(ui, rect, |ui| {
                ui.painter().add(Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(70) }.as_shape(rect, CornerRadius::same(12)));
                let clip = ui.painter().clone().with_clip_rect(rect);
                theme::paint_gradient(&clip, rect, &pal.stops, pal.angle_deg, 1.0);
                theme::paint_texture(&clip, rect, &app.texture_bank, texture, pal.grain, ctx.pixels_per_point());
                sidebar::show(app, ui, rect, &pal, true);
            });
            // Moving into a hover preview beside it keeps the sidebar open.
            if !pointer.is_some_and(|p| rect.expand(24.0).contains(p) || app.previews.contains(p)) && !edge && app.ui.menu.is_none() {
                app.ui.sidebar_peek = false;
            }
        }
    }
}

fn chrome_crossfade(tint: f32, progress: f32) -> (f32, f32) {
    let tint = tint.clamp(0.0, 1.0);
    let progress = progress.clamp(0.0, 1.0);
    let incoming = tint * progress;
    // Source-over layers must keep the selected opacity throughout a Space switch.
    let outgoing = if incoming < 1.0 { tint * (1.0 - progress) / (1.0 - incoming) } else { 0.0 };
    (outgoing, incoming)
}

fn sidebar_shown(ctx: &egui::Context, visible: bool) -> f32 {
    anim::bounded_transition(ctx, Id::unique("sidebar-shown"), if visible { 1.0 } else { 0.0 }, if visible { 0.22 } else { 0.15 })
}

fn sidebar_layout(ctx: &egui::Context, full: Rect, width: &mut f32, shown: f32, on_right: bool, native_strip: bool, edge: f32) -> (Rect, Rect) {
    // egui resolves this pass's response before drawing. Consume it before sizing
    // Chromium, but register the handle after the sidebar to preserve hit priority.
    let id = Id::unique("sidebar-resize");
    if shown > 0.01 && let Some(resp) = ctx.read_response(id).filter(|resp| resp.dragged()) {
        let frame = ctx.cumulative_frame_nr();
        let applied = id.with("applied-frame");
        if ctx.data(|d| d.get_temp::<u64>(applied)) != Some(frame) {
            *width = (*width + resp.drag_delta().x * if on_right { -1.0 } else { 1.0 }).clamp(200.0, 420.0);
            ctx.data_mut(|d| d.insert_temp(applied, frame));
        }
    }
    let w = width.clamp(200.0, 420.0);
    let docked_w = w * shown;
    let inset = if native_strip {
        // Keep the native row intact while draw_top_strip still exposes its controls.
        let reveal = ((shown - 0.5) * 2.0).clamp(0.0, 1.0);
        if on_right { super::window::NATIVE_CONTROL_ROW } else { anim::lerp(super::window::NATIVE_CONTROL_ROW, edge, reveal) }
    } else { edge };
    let content_top = full.top() + inset;
    let content = if on_right {
        Rect::from_min_max(pos2(full.left() + edge, content_top), pos2(full.right() - docked_w.max(edge), full.bottom() - edge))
    } else {
        Rect::from_min_max(pos2(full.left() + docked_w.max(edge), content_top), pos2(full.right() - edge, full.bottom() - edge))
    };
    let x = if on_right { full.right() - docked_w } else { full.left() - w * (1.0 - shown) };
    (content, Rect::from_min_size(pos2(x, full.top()), vec2(w, full.height())))
}

fn floating_sidebar_rect(full: Rect, width: f32, shown: f32, on_right: bool, native_strip: bool) -> Rect {
    let x = if on_right { full.right() - (width + GAP) * shown } else { full.left() + GAP - (width + GAP) * (1.0 - shown) };
    // A left macOS sidebar shares the native title row, even while floating.
    let top = full.top() + if native_strip && !on_right { 0.0 } else { GAP };
    Rect::from_min_max(pos2(x, top), pos2(x + width, full.bottom() - GAP))
}

pub(super) fn show_floating_sidebar(ui: &mut Ui, rect: Rect, content: impl FnOnce(&mut Ui)) {
    egui::Area::new(Id::unique("sidebar-float")).order(egui::Order::Foreground)
        .fixed_pos(rect.min).default_size(rect.size()).constrain(false).fade_in(false).sense(Sense::hover())
        .show(ui.ctx(), |ui| {
            // Register the painted bounds for wheel hit testing and modal stacking.
            ui.set_min_size(rect.size());
            content(ui);
        });
}

fn resize_handle(ui: &mut Ui, sidebar: Rect, on_right: bool) {
    let x = if on_right { sidebar.left() } else { sidebar.right() - 2.0 };
    let r = Rect::from_min_max(pos2(x - 3.0, sidebar.top() + 40.0), pos2(x + 3.0, sidebar.bottom() - 40.0));
    let resp = ui.interact(r, Id::unique("sidebar-resize"), Sense::drag());
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    }
}

fn draw_top_strip(app: &mut CrossbowApp, ui: &mut Ui, strip: Rect, pal: &theme::Palette) {
    if strip.height() < 12.0 {
        return;
    }
    window_drag(ui, strip, Id::unique("top-drag"));
    // Compact navigation controls next to the traffic lights.
    let nav = app.focused_view().map(|v| v.nav()).unwrap_or_default();
    for (r, (icon, action, enabled)) in top_strip_controls(strip).into_iter().zip([
        (Icon::Sidebar, Action::ToggleSidebar, true),
        (Icon::Back, Action::Back, nav.can_go_back),
        (Icon::Forward, Action::Forward, nav.can_go_forward),
        (Icon::Reload, Action::Reload, true),
    ]) {
        if icons::button(ui, r, icon, pal.text_dim, pal.hover, enabled).clicked() {
            app.actions.push(action);
        }
    }
    // The key button, the blocked icon and the shield, at the strip's trailing
    // end, where the sidebar's URL pill is hidden. Permission prompts hang from there.
    let row = super::window::ControlRow::new(strip, true, strip.height());
    let key = crate::ui::logins::key_button(app, ui, row.trailing_control(0), pal.text_dim, pal.hover);
    let blocked = crate::ui::site_permissions::blocked_button(app, ui, row.trailing_control(usize::from(key)), pal.text_dim, pal.hover);
    crate::ui::content_blocking_shield::button(app, ui, row.trailing_control(usize::from(key) + usize::from(blocked)), pal.text_dim, pal.hover);
    app.ui.address = Some(row.trailing_control(0));
    if let Some(t) = app.focused_tab_id().and_then(|t| app.tab(&t)) {
        let title = t.display_title();
        ui.painter().text(strip.center(), egui::Align2::CENTER_CENTER, title, egui::FontId::proportional(12.5), pal.text_dim);
    }
}

/// Dragging `rect` moves the window and a double click zooms it, as the
/// title row does.
fn window_drag(ui: &mut Ui, rect: Rect, id: Id) {
    let response = ui.interact(rect, id, Sense::click_and_drag());
    if response.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if response.double_clicked() {
        let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }
}

/// How much of a page's top moves the window when Settings → Advanced allows
/// it: less than the space above most sites' first line of links.
pub(super) const PAGE_DRAG_STRIP: f32 = 6.0;

/// The band at the top of a page that moves the window: only on a pane whose
/// top is the content's top edge, and never in full screen.
pub(super) fn page_drag_strip(pane: Rect, content_top: f32, enabled: bool, fullscreen: bool) -> Option<Rect> {
    (enabled && !fullscreen && (pane.top() - content_top).abs() < 0.5).then(|| Rect::from_min_size(pane.min, vec2(pane.width(), PAGE_DRAG_STRIP)))
}

/// The panes of a split, side by side across `content`.
fn split_rects(content: Rect, ratios: &[f32]) -> Vec<Rect> {
    let total_gap = GAP * (ratios.len() as f32 - 1.0);
    let mut x = content.left();
    ratios.iter().map(|ratio| {
        let w = (content.width() - total_gap) * ratio;
        let rect = Rect::from_min_size(pos2(x, content.top()), vec2(w, content.height()));
        x += w + GAP;
        rect
    }).collect()
}

/// The strip's Sidebar, Back, Forward and Reload, where the sidebar's header
/// puts its controls: the toggle stays in place as the sidebar hides.
pub(super) fn top_strip_controls(strip: Rect) -> [Rect; 4] {
    let row = super::window::ControlRow::new(strip, true, strip.height());
    std::array::from_fn(|index| row.leading_control(index))
}

fn draw_panes(app: &mut CrossbowApp, ui: &mut Ui, content: Rect, space_t: f32, radius: u8) {
    let ctx = ui.ctx().clone();
    let ppp = ctx.pixels_per_point();
    let active = app.active_tab_id();
    let split = app.splits.get(&app.active_space).filter(|s| active.as_ref().is_some_and(|a| s.tabs.contains(a))).cloned();

    let mut panes: Vec<(String, Rect)> = Vec::new();
    match (&split, &active) {
        (Some(s), _) => panes.extend(s.tabs.iter().cloned().zip(split_rects(content, &s.ratios[..s.tabs.len()]))),
        (None, Some(a)) => panes.push((a.clone(), content)),
        (None, None) => {}
    }

    // Space switch: slide the new content in.
    let offset = if let Some(a) = &app.ui.space_anim { (1.0 - space_t) * 36.0 * a.dir } else { 0.0 };
    let alpha = space_t;
    app.ui.panes = panes.iter().map(|(id, r)| (id.clone(), r.translate(vec2(offset, 0.0)))).collect();

    if panes.is_empty() {
        empty_state(app, ui, content, alpha, radius);
        return;
    }

    let painter = ui.painter().clone();
    let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
    let page_drag = app.feel.prefs.page_drag.enabled() && !page_blocked(app);
    for (i, (tab_id, rect)) in panes.iter().enumerate() {
        let rect = rect.translate(vec2(offset, 0.0));
        app.ensure_view(tab_id, (rect.width(), rect.height()), ppp);
        if let Some(v) = app.tab(tab_id).and_then(|t| t.view.clone()) {
            app.update_view_position(&v, rect.min, ppp);
            v.resize(rect.width(), rect.height(), ppp);
        }
        paint_pane_surface(&painter, rect, app.texture_for(tab_id), ppp, alpha, radius);
        let focused = split.is_some() && app.ui.focused_pane.as_deref() == Some(tab_id.as_str());
        if focused {
            let pal = app.palette();
            painter.rect_stroke(rect, radius, Stroke::new(2.0, pal.accent), egui::StrokeKind::Outside);
        }
        pane_input(app, ui, tab_id, rect, i);
        // After the page, so the band is on top; before the pane's own controls.
        if let Some(strip) = page_drag_strip(rect, content.top(), page_drag, fullscreen) {
            window_drag(ui, strip, Id::unique(("page-top-drag", i)));
        }
        pane_chrome(app, ui, tab_id, rect, split.is_some(), radius);
    }

    // Draggable dividers between split panes.
    if let Some(s) = &split {
        for i in 0..s.tabs.len() - 1 {
            let x = panes[i].1.right() + GAP / 2.0 + offset;
            let r = Rect::from_min_max(pos2(x - 4.0, content.top()), pos2(x + 4.0, content.bottom()));
            let resp = ui.interact(r, Id::unique(("split-divider", i)), Sense::drag());
            if resp.hovered() || resp.dragged() {
                ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            if resp.dragged() {
                let d = resp.drag_delta().x / content.width();
                if let Some(sp) = app.splits.get_mut(&app.active_space) {
                    let (a, b) = (sp.ratios[i] + d, sp.ratios[i + 1] - d);
                    if a > 0.15 && b > 0.15 {
                        sp.ratios[i] = a;
                        sp.ratios[i + 1] = b;
                    }
                }
            }
        }
    }
}

pub(super) fn paint_pane_surface(painter: &egui::Painter, rect: Rect, texture: Option<(egui::TextureId, egui::Vec2)>, ppp: f32, shadow_alpha: f32, radius: u8) {
    painter.add(Shadow { offset: [0, 2], blur: 14, spread: 0, color: Color32::from_black_alpha((38.0 * shadow_alpha) as u8) }.as_shape(rect, CornerRadius::same(radius)));
    painter.rect_filled(rect, radius, Color32::WHITE);
    if let Some((tex, size_px)) = texture {
        let size = size_px / ppp;
        let shown = Rect::from_min_size(rect.min, size).intersect(rect);
        let uv = Rect::from_min_max(
            pos2((shown.min.x - rect.min.x) / size.x, (shown.min.y - rect.min.y) / size.y),
            pos2((shown.max.x - rect.min.x) / size.x, (shown.max.y - rect.min.y) / size.y),
        );
        painter.add(RectShape::filled(shown, CornerRadius::same(radius), Color32::WHITE).with_texture(tex, uv));
    }
}

fn empty_state(app: &mut CrossbowApp, ui: &mut Ui, content: Rect, alpha: f32, radius: u8) {
    let pal = app.palette();
    let p = ui.painter();
    p.rect_filled(content, radius, pal.panel.gamma_multiply(0.55 * alpha));
    let c = content.center();
    p.text(c - vec2(0.0, 14.0), egui::Align2::CENTER_CENTER, "No tab open", egui::FontId::proportional(20.0), pal.text.gamma_multiply(alpha));
    let hint = format!("Press {} to open a tab", input::shortcut_label(true, false, false, false, "T"));
    p.text(c + vec2(0.0, 14.0), egui::Align2::CENTER_CENTER, hint, egui::FontId::proportional(13.0), pal.text_dim.gamma_multiply(alpha));
    let resp = ui.interact(content, Id::unique("empty-content"), Sense::click());
    if resp.clicked() {
        app.actions.push(Action::OpenCommandBar { edit_current: false });
    }
}

pub(super) fn cursor_icon(c: CursorKind) -> CursorIcon {
    match c {
        CursorKind::Default => CursorIcon::Default,
        CursorKind::Pointer => CursorIcon::PointingHand,
        CursorKind::Text => CursorIcon::Text,
        CursorKind::Crosshair => CursorIcon::Crosshair,
        CursorKind::Wait => CursorIcon::Wait,
        CursorKind::Progress => CursorIcon::Progress,
        CursorKind::Help => CursorIcon::Help,
        CursorKind::Move => CursorIcon::Move,
        CursorKind::NotAllowed => CursorIcon::NotAllowed,
        CursorKind::Grab => CursorIcon::Grab,
        CursorKind::Grabbing => CursorIcon::Grabbing,
        CursorKind::ResizeEw => CursorIcon::ResizeHorizontal,
        CursorKind::ResizeNs => CursorIcon::ResizeVertical,
        CursorKind::ResizeNesw => CursorIcon::ResizeNeSw,
        CursorKind::ResizeNwse => CursorIcon::ResizeNwSe,
        CursorKind::ZoomIn => CursorIcon::ZoomIn,
        CursorKind::ZoomOut => CursorIcon::ZoomOut,
        CursorKind::None => CursorIcon::None,
    }
}

fn mouse_button(b: egui::PointerButton) -> Option<MouseButton> {
    match b {
        egui::PointerButton::Primary => Some(MouseButton::Left),
        egui::PointerButton::Secondary => Some(MouseButton::Right),
        egui::PointerButton::Middle => Some(MouseButton::Middle),
        _ => None,
    }
}

fn popup_rect(origin: Pos2, texture: (egui::TextureId, egui::Vec2, Rect), ppp: f32) -> Rect {
    Rect::from_min_size(origin + texture.2.min.to_vec2(), texture.1 / ppp)
}

pub(super) fn popup_surface(ui: &mut Ui, key: &str, origin: Pos2, texture: (egui::TextureId, egui::Vec2, Rect), ppp: f32) -> egui::Response {
    let rect = popup_rect(origin, texture, ppp);
    ui.painter().image(texture.0, rect, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
    ui.interact(rect, Id::unique(("web-popup-input", key)), Sense::click_and_drag())
}

pub(super) fn pane_popup(ctx: &egui::Context, key: &str, origin: Pos2, texture: (egui::TextureId, egui::Vec2, Rect), ppp: f32) -> egui::Response {
    let rect = popup_rect(origin, texture, ppp);
    egui::Area::new(Id::unique(("popup", key))).order(egui::Order::Foreground)
        .fixed_pos(rect.min).default_size(rect.size()).constrain(false).fade_in(false).sense(Sense::hover())
        .show(ctx, |ui| {
            ui.set_min_size(rect.size());
            popup_surface(ui, key, origin, texture, ppp)
        }).inner
}

/// Forward a pane and its popup through one input route, keeping page coordinates.
pub fn pane_input(app: &mut CrossbowApp, ui: &mut Ui, tab_id: &str, rect: Rect, index: usize) {
    let ctx = ui.ctx().clone();
    let resp = ui.interact(rect, Id::unique(("pane", index)), Sense::click_and_drag());
    let Some(view) = app.tab(tab_id).and_then(|t| t.view.clone()) else { return };
    if page_blocked(app) || !ctx.input(|input| input.focused) {
        suspend_pointer(&mut app.ui.pointer_capture, tab_id, || view.capture_lost());
        leave_pointer(&mut app.ui.pointer_over, tab_id, || view.mouse_leave());
        return;
    }
    // Command-Shift drag captures, when on; otherwise the page gets the press.
    if app.capture_press(&ctx, tab_id, rect, resp.hovered()) {
        suspend_pointer(&mut app.ui.pointer_capture, tab_id, || view.capture_lost());
        leave_pointer(&mut app.ui.pointer_over, tab_id, || view.mouse_leave());
        return;
    }
    let popup = app.popup_texture_for(tab_id).map(|texture| pane_popup(&ctx, tab_id, rect.min, texture, ctx.pixels_per_point()));
    forward_pointer(app, ui, &view, tab_id, rect, &WebInput { page: &resp, popup: popup.as_ref() }, index);
}

/// A menu, overlay or question covers the pages: they and their top bands
/// take no pointer input.
fn page_blocked(app: &CrossbowApp) -> bool {
    app.ui.menu.is_some() || app.ui.command.is_some() || app.ui.peek.is_some() || app.ui.settings.is_some() || app.quit.borrow().asking() || app.capture.selecting.is_some()
}

pub(super) fn suspend_pointer(capture: &mut Option<String>, owner: &str, cancel: impl FnOnce()) {
    if capture.as_deref() == Some(owner) {
        *capture = None;
        cancel();
    }
}

pub(super) struct WebInput<'a> {
    pub page: &'a egui::Response,
    pub popup: Option<&'a egui::Response>,
}

impl WebInput<'_> {
    fn responses(&self) -> impl Iterator<Item = &egui::Response> {
        std::iter::once(self.page).chain(self.popup)
    }

    fn hit(&self, position: Pos2) -> bool {
        self.responses().any(|response| response.hovered() && response.interact_rect.contains(position))
    }

    pub(super) fn wheel_position(&self, ctx: &egui::Context) -> Option<Pos2> {
        ctx.input(|input| input.pointer.hover_pos()).filter(|position| {
            self.responses().any(|response| response.contains_pointer() && response.interact_rect.contains(*position))
        })
    }

    pub(super) fn events(&self, ctx: &egui::Context, mut captured: bool) -> Vec<egui::Event> {
        let events = ctx.input(|input| input.events.clone());
        events.into_iter().filter(|event| match event {
            egui::Event::PointerMoved(position) => captured || self.hit(*position),
            egui::Event::PointerButton { pos, button, pressed, .. } => {
                if mouse_button(*button).is_some() && (self.hit(*pos) || (captured && !pressed)) {
                    captured = *pressed;
                    true
                } else {
                    false
                }
            }
            egui::Event::MouseWheel { .. } => self.wheel_position(ctx).is_some(),
            egui::Event::PointerGone => true,
            _ => false,
        }).collect()
    }
}

fn view_position(rect: Rect, position: Pos2) -> Pos2 {
    (position - rect.min).to_pos2()
}

/// One pointer event for a page, in its view coordinates.
#[derive(Clone, Debug, PartialEq)]
pub enum PointerInput {
    Move { x: f32, y: f32, mods: crossbow_engine::Modifiers, held: Vec<MouseButton> },
    Button { x: f32, y: f32, button: MouseButton, pressed: bool, count: i32, mods: crossbow_engine::Modifiers },
    Wheel { x: f32, y: f32, dx: f32, dy: f32, mods: crossbow_engine::Modifiers },
    Leave,
}

impl PointerInput {
    pub fn send(&self, view: &crossbow_engine::WebView) {
        match self {
            PointerInput::Move { x, y, mods, held } => view.mouse_move(*x, *y, *mods, held),
            PointerInput::Button { x, y, button, pressed, count, mods } => view.mouse_button(*x, *y, *button, !*pressed, *count, *mods),
            PointerInput::Wheel { x, y, dx, dy, mods } => view.mouse_wheel(*x, *y, *dx, *dy, *mods),
            PointerInput::Leave => view.mouse_leave(),
        }
    }
}

/// This pass's pointer events for one page, in view coordinates. `captured`:
/// the page owns the pointer; `last_press` counts double and triple clicks;
/// `over`: the page has heard the pointer over it and not yet that it left.
///
/// A page keeps the last place it saw the pointer, and with it hover and a
/// tooltip, until it hears the pointer leave. So it hears that once the
/// pointer is over Crossbow's own controls or anything drawn over the page,
/// not only when it leaves the window; a drag the page started keeps the
/// pointer until its release.
pub(super) fn pointer_inputs(ctx: &egui::Context, rect: Rect, target: &WebInput<'_>, captured: bool, last_press: &mut Option<(f64, Pos2, egui::PointerButton, i32)>, over: &mut bool) -> Vec<PointerInput> {
    let events = target.events(ctx, captured);
    let mut owned = captured;
    let (held, now) = ctx.input(|i| {
        let mut held = Vec::new();
        if i.pointer.primary_down() {
            held.push(MouseButton::Left);
        }
        if i.pointer.secondary_down() {
            held.push(MouseButton::Right);
        }
        if i.pointer.middle_down() {
            held.push(MouseButton::Middle);
        }
        (held, i.time)
    });
    let mods = ctx.input(|i| input::modifiers(i.modifiers));
    let mut out = Vec::new();
    for e in events {
        match e {
            egui::Event::PointerMoved(p) => {
                let p = view_position(rect, p);
                out.push(PointerInput::Move { x: p.x, y: p.y, mods, held: held.clone() });
            }
            egui::Event::PointerButton { pos, button, pressed, modifiers } => {
                let Some(b) = mouse_button(button) else { continue };
                let count = if pressed {
                    let count = match *last_press {
                        Some((t, p, lb, c)) if lb == button && now - t < 0.45 && p.distance(pos) < 5.0 => (c + 1).min(3),
                        _ => 1,
                    };
                    *last_press = Some((now, pos, button, count));
                    count
                } else {
                    last_press.map(|l| l.3).unwrap_or(1)
                };
                let p = view_position(rect, pos);
                owned = pressed;
                out.push(PointerInput::Button { x: p.x, y: p.y, button: b, pressed, count, mods: input::modifiers(modifiers) });
            }
            egui::Event::MouseWheel { unit, delta, modifiers, .. } => {
                let Some(p) = target.wheel_position(ctx) else { continue };
                let p = view_position(rect, p);
                let scale = match unit {
                    egui::MouseWheelUnit::Point => 1.0,
                    egui::MouseWheelUnit::Line => 40.0,
                    egui::MouseWheelUnit::Page => rect.height(),
                };
                out.push(PointerInput::Wheel { x: p.x, y: p.y, dx: delta.x * scale, dy: delta.y * scale, mods: input::modifiers(modifiers) });
            }
            egui::Event::PointerGone => out.push(PointerInput::Leave),
            _ => {}
        }
    }
    if owned || target.responses().any(|response| response.hovered()) {
        *over = true;
    } else if std::mem::take(over) && out.last() != Some(&PointerInput::Leave) {
        out.push(PointerInput::Leave);
    }
    if out.last() == Some(&PointerInput::Leave) { *over = false; }
    out
}

/// The page at `key` has the pointer no more: it hears it leave, once.
pub(super) fn leave_pointer(over: &mut std::collections::HashSet<String>, key: &str, leave: impl FnOnce()) {
    if over.remove(key) { leave(); }
}

/// Popup hits may overflow `rect`; all CEF coordinates still use the page origin.
/// `key` identifies the one capture owner (a tab id, or "peek").
pub(super) fn forward_pointer(app: &mut CrossbowApp, ui: &mut Ui, view: &crossbow_engine::WebView, key: &str, rect: Rect, target: &WebInput<'_>, index: usize) {
    let ctx = ui.ctx().clone();
    let tab_id = key;
    let mut over = app.ui.pointer_over.contains(tab_id);
    let inputs = pointer_inputs(&ctx, rect, target, app.ui.pointer_capture.as_deref() == Some(tab_id), &mut app.ui.last_press, &mut over);
    if over { app.ui.pointer_over.insert(tab_id.to_owned()); } else { app.ui.pointer_over.remove(tab_id); }
    for event in inputs {
        match &event {
            PointerInput::Button { pressed: true, .. } => {
                app.ui.pointer_capture = Some(tab_id.to_string());
                app.ui.focus = Focus::Web;
                if tab_id != "peek" {
                    app.ui.focused_pane = Some(tab_id.to_string());
                    app.note_activity(tab_id);
                }
                app.ui.menu = None;
                app.ui.sidebar_peek = false;
                ctx.memory_mut(|mem| mem.stop_text_input());
                view.set_focus(true);
            }
            PointerInput::Button { pressed: false, .. } => app.ui.pointer_capture = None,
            PointerInput::Wheel { .. } => app.note_activity(tab_id),
            _ => {}
        }
        event.send(view);
    }
    if target.responses().any(|response| response.hovered()) {
        let nav = view.nav();
        ctx.set_cursor_icon(cursor_icon(nav.cursor));
        if let Some(t) = nav.tooltip {
            egui::Tooltip::always_open(ctx.clone(), ui.layer_id(), Id::unique(("web-tooltip", index)), egui::PopupAnchor::Pointer)
                .show(|ui| ui.label(t));
        }
    }
}

/// Clip a rounded card, not a square bar: both top corners keep their silhouette.
pub(super) fn paint_progress(painter: &egui::Painter, rect: Rect, radius: u8, progress: f32, visibility: f32, accent: Color32) {
    if visibility <= 0.02 || !progress.is_finite() || !visibility.is_finite() {
        return;
    }
    let clip = Rect::from_min_size(rect.min, vec2(rect.width() * progress.clamp(0.0, 1.0), 2.5));
    painter.with_clip_rect(clip.intersect(rect)).rect_filled(rect, radius, accent.gamma_multiply(visibility.clamp(0.0, 1.0)));
}

#[cfg(test)]
mod popup_tests {
    use super::*;

    #[test]
    fn suspended_pointer_capture_cancels_only_its_owner_once() {
        for owner in ["tab-fixture", "peek"] {
            let mut capture = Some(owner.to_string());
            let mut cancelled = 0;
            suspend_pointer(&mut capture, "other-tab", || cancelled += 1);
            assert_eq!(capture.as_deref(), Some(owner));
            assert_eq!(cancelled, 0);
            suspend_pointer(&mut capture, owner, || cancelled += 1);
            assert!(capture.is_none());
            assert_eq!(cancelled, 1);
            suspend_pointer(&mut capture, owner, || cancelled += 1);
            assert_eq!(cancelled, 1, "a covered or unfocused view must not send repeated cancellation");
        }
    }

    #[test]
    fn pane_popup_matches_paint_to_hit_bounds_without_changing_page_coordinates() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for scale in [1.0, 2.0, 2.5] {
                for button in [egui::PointerButton::Primary, egui::PointerButton::Secondary, egui::PointerButton::Middle] {
                    let ctx = egui::Context::default();
                    let screen = Rect::from_min_size(Pos2::ZERO, size);
                    let page_rect = Rect::from_min_size(pos2(80.0, 50.0), size * vec2(0.6, 0.7));
                    let popup = Rect::from_min_size(pos2(page_rect.width() - 20.0, 50.0), vec2(100.0, 100.0));
                    let texture = (egui::TextureId::User(91), popup.size() * scale, popup);
                    let bounds = popup_rect(page_rect.min, texture, scale);
                    let over = bounds.center();
                    let outside = pos2(page_rect.right() + 30.0, page_rect.top() + 20.0);
                    assert!(!page_rect.contains(over));
                    assert!(!page_rect.contains(outside) && !bounds.contains(outside));
                    let mut captured = false;
                    let mut buttons = Vec::new();
                    let mut wheels = Vec::new();
                    let mut moves = Vec::new();
                    let mut inspected = 0;
                    for frame in 0..13 {
                        let pointer = if (5..8).contains(&frame) || frame >= 10 { outside } else { over };
                        let mut events = vec![egui::Event::PointerMoved(pointer)];
                        if [4, 7, 10, 11].contains(&frame) {
                            events.push(egui::Event::PointerButton { pos: pointer, button, pressed: frame == 4 || frame == 10, modifiers: Default::default() });
                        }
                        if [3, 6, 8, 10].contains(&frame) {
                            events.push(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -40.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() });
                        }
                        let mut input = egui::RawInput { screen_rect: Some(screen), events, time: Some(frame as f64 / 60.0), ..Default::default() };
                        input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(scale);
                        let mut output = ctx.run_ui(input, |ui| {
                            let page = ui.interact(page_rect, Id::unique(("pane", 0)), Sense::click_and_drag());
                            let popup = pane_popup(ui.ctx(), "pane-fixture", page_rect.min, texture, scale);
                            assert_eq!(popup.rect, bounds);
                            let target = WebInput { page: &page, popup: Some(&popup) };
                            for event in target.events(ui.ctx(), captured) {
                                match event {
                                    egui::Event::PointerButton { pos, pressed, .. } => { buttons.push((view_position(page_rect, pos), pressed)); captured = pressed; }
                                    egui::Event::PointerMoved(pos) => moves.push((frame, view_position(page_rect, pos))),
                                    egui::Event::MouseWheel { .. } => wheels.push(view_position(page_rect, target.wheel_position(ui.ctx()).unwrap())),
                                    _ => {}
                                }
                            }
                        });
                        if let Some(mesh) = output.shapes.iter().find_map(|shape| match &shape.shape {
                            egui::Shape::Mesh(mesh) if mesh.texture_id == texture.0 => Some(mesh),
                            _ => None,
                        }) {
                            inspected += 1;
                            assert!(mesh.vertices.iter().all(|vertex| vertex.color == Color32::WHITE), "popup must not fade on first visible Area frames");
                            let painted = mesh.vertices.iter().fold(Rect::NOTHING, |rect, vertex| rect.union(Rect::from_min_max(vertex.pos, vertex.pos)));
                            assert_eq!(painted, bounds);
                        }
                        output.textures_delta.clear();
                    }
                    let over_local = view_position(page_rect, over);
                    let outside_local = view_position(page_rect, outside);
                    assert!(over_local.x > page_rect.width(), "do not clamp popup input or enlarge CEF's page viewport");
                    assert_eq!(buttons, [(over_local, true), (outside_local, false)]);
                    assert_eq!(wheels, [over_local, over_local], "wheel never follows capture into empty space");
                    assert!(moves.contains(&(5, outside_local)));
                    assert!(!moves.iter().any(|(frame, _)| *frame >= 10), "bounding-box gaps do not belong to the page or popup");
                    assert!(!captured);
                    assert!(inspected >= 10);
                }
            }
        }
    }
}

#[cfg(test)]
mod viewport_tests {
    use super::*;

    fn shown_at(ctx: &egui::Context, time: f64, visible: bool) -> f32 {
        let mut shown = 0.0;
        ctx.run_ui(egui::RawInput { time: Some(time), ..Default::default() }, |ui| {
            shown = sidebar_shown(ui.ctx(), visible);
        }).textures_delta.clear();
        shown
    }

    #[test]
    fn sidebar_toggle_reaches_its_endpoint_within_the_motion_budget() {
        for visible in [false, true] {
            for step in [1.0 / 120.0, 1.0 / 60.0, 0.05] {
                let ctx = egui::Context::default();
                shown_at(&ctx, 0.0, !visible);
                shown_at(&ctx, 1.0, visible);
                let duration = if visible { 0.22 } else { 0.15 };
                let mut time = step;
                while time < duration {
                    assert!((0.0..=1.0).contains(&shown_at(&ctx, 1.0 + time, visible)));
                    time += step;
                }
                assert_eq!(shown_at(&ctx, 1.0 + duration, visible), if visible { 1.0 } else { 0.0 }, "visible={visible}, step={step}");
            }
        }
    }

    #[test]
    fn space_crossfade_preserves_chrome_opacity_including_opaque_fallback() {
        for tint in [0.0, 0.12, 0.35, 0.75, 1.0] {
            for step in 0..=100 {
                let progress = step as f32 / 100.0;
                let (outgoing, incoming) = chrome_crossfade(tint, progress);
                assert!((0.0..=1.0).contains(&outgoing));
                assert!((0.0..=1.0).contains(&incoming));
                assert!((incoming + outgoing * (1.0 - incoming) - tint).abs() < 0.00001);
                assert!((outgoing * (1.0 - incoming) - tint * (1.0 - progress)).abs() < 0.00001);
            }
        }
    }

    #[test]
    fn sidebar_toggle_finishes_after_an_idle_or_delayed_frame() {
        let ctx = egui::Context::default();
        assert_eq!(shown_at(&ctx, 0.0, false), 0.0);
        shown_at(&ctx, 1.0, true);
        assert_eq!(shown_at(&ctx, 2.0, true), 1.0);
    }

    #[test]
    fn sidebar_content_inset_is_continuous_at_the_old_halfway_threshold() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for right in [false, true] {
                for native in [false, true] {
                    let ctx = egui::Context::default();
                    let full = Rect::from_min_size(pos2(17.0, 23.0), size);
                    let mut width = 252.0;
                    let before = sidebar_layout(&ctx, full, &mut width, 0.4999, right, native, GAP).0;
                    let after = sidebar_layout(&ctx, full, &mut width, 0.5001, right, native, GAP).0;
                    assert!((before.top() - after.top()).abs() < 0.01, "right={right}, native={native}: {before:?} -> {after:?}");
                    if native {
                        for shown in [0.0, 0.25, 0.4999] {
                            let content = sidebar_layout(&ctx, full, &mut width, shown, right, native, GAP).0;
                            assert_eq!(content.top(), full.top() + super::super::window::NATIVE_CONTROL_ROW, "visible title-row controls must not overlap the page");
                        }
                    }
                    for shown in [0.0, 1.0] {
                        let content = sidebar_layout(&ctx, full, &mut width, shown, right, native, GAP).0;
                        let inset = if native && (shown == 0.0 || right) { super::super::window::NATIVE_CONTROL_ROW } else { GAP };
                        assert_eq!(content.top(), full.top() + inset);
                        assert_eq!(content.bottom(), full.bottom() - GAP);
                        assert_eq!(content.width(), size.x - if shown == 0.0 { 2.0 * GAP } else { width + GAP });
                    }
                }
            }
        }
    }

    #[test]
    fn full_screen_pages_meet_every_display_edge_with_square_corners() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0), vec2(1800.0, 1169.0)] {
            let full = Rect::from_min_size(Pos2::ZERO, size);
            for fullscreen in [false, true] {
                let ctx = egui::Context::default();
                let mut input = egui::RawInput { screen_rect: Some(full), ..Default::default() };
                input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(fullscreen);
                let mut frame = (f32::NAN, u8::MAX);
                ctx.run_ui(input, |ui| frame = card_frame(ui.ctx())).textures_delta.clear();
                assert_eq!(frame, if fullscreen { (0.0, 0) } else { (GAP, RADIUS) });
                if !fullscreen { continue; }
                let (edge, radius) = frame;
                for right in [false, true] {
                    let mut width = 252.0;
                    // Full screen has no native title row, so the strip is off.
                    let hidden = sidebar_layout(&ctx, full, &mut width, 0.0, right, false, edge).0;
                    assert_eq!(hidden, full, "a hidden sidebar leaves the page the whole display at {size:?}");
                    let docked = sidebar_layout(&ctx, full, &mut width, 1.0, right, false, edge).0;
                    assert_eq!((docked.top(), docked.bottom(), docked.width()), (full.top(), full.bottom(), size.x - width));
                    assert_eq!(if right { docked.left() } else { docked.right() }, if right { full.left() } else { full.right() });
                }
                let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(full), ..Default::default() }, |ui| paint_pane_surface(ui.painter(), full, None, 1.0, 1.0, radius));
                output.textures_delta.clear();
                for shape in &output.shapes {
                    if let egui::Shape::Rect(rect) = &shape.shape {
                        assert_eq!(rect.corner_radius, CornerRadius::ZERO, "no rounded corner shows the theme behind a full-screen page");
                    }
                }
            }
        }
    }

    #[test]
    fn sidebar_drag_updates_this_pass_without_stealing_page_input_or_reapplying_a_delta() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for right in [false, true] {
                for multipass in [false, true] {
                    let ctx = egui::Context::default();
                    let full = Rect::from_min_size(Pos2::ZERO, size);
                    let mut width = 252.0;
                    let x = if right { size.x - width } else { width - 2.0 };
                    for frame in 0..8 {
                        let distance = match frame { 5 => 20.0, 6.. => 40.0, _ => 0.0 };
                        let pointer = pos2(x + distance * if right { -1.0 } else { 1.0 }, 200.0);
                        let mut events = vec![egui::Event::PointerMoved(pointer)];
                        if frame == 4 || frame == 7 {
                            events.push(egui::Event::PointerButton { pos: pointer, button: egui::PointerButton::Primary, pressed: frame == 4, modifiers: Default::default() });
                        }
                        let mut passes = 0;
                        ctx.run_ui(egui::RawInput { screen_rect: Some(full), events, time: Some(frame as f64 / 60.0), ..Default::default() }, |ui| {
                            passes += 1;
                            let (content, sidebar) = sidebar_layout(ui.ctx(), full, &mut width, 1.0, right, true, GAP);
                            assert_eq!(width, 252.0 + distance, "frame={frame}, pass={passes}, right={right}");
                            assert_eq!(content.width(), size.x - width - GAP, "current pointer delta must reach this pass's web rectangle");
                            let page = ui.interact(content, Id::unique(("pane", 0)), Sense::click_and_drag());
                            resize_handle(ui, sidebar, right);
                            if (5..7).contains(&frame) {
                                assert!(!page.dragged() && !page.hovered(), "the late-registered resize handle must retain the pointer");
                                assert!(ui.ctx().read_response(Id::unique("sidebar-resize")).unwrap().dragged());
                                if multipass && passes == 1 { ui.ctx().request_discard("sidebar drag multipass"); }
                            }
                        }).textures_delta.clear();
                        if (5..7).contains(&frame) { assert_eq!(passes, if multipass { 2 } else { 1 }); }
                        assert_eq!(width, 252.0 + distance, "repeated passes must not accumulate the same delta");
                    }
                }
            }
        }
    }

    #[test]
    fn delayed_same_scale_frames_are_clipped_not_stretched_to_new_viewports() {
        for ppp in [1.0, 1.5, 2.0] {
            for (old, new) in [(vec2(380.0, 404.0), vec2(624.0, 380.0)), (vec2(624.0, 380.0), vec2(380.0, 404.0))] {
                let ctx = egui::Context::default();
                let rect = Rect::from_min_size(pos2(8.0, 32.0), new);
                let texture = Some((egui::TextureId::User(102), old * ppp));
                let mut output = ctx.run_ui(Default::default(), |ui| paint_pane_surface(ui.painter(), rect, texture, ppp, 1.0, RADIUS));
                output.textures_delta.clear();
                let image = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(shape) if shape.brush.is_some() => Some(shape),
                    _ => None,
                }).unwrap();
                assert_eq!(image.rect, Rect::from_min_size(rect.min, old.min(new)));
                let uv = image.brush.as_ref().unwrap().uv;
                assert_eq!(uv.min, Pos2::ZERO);
                assert_eq!(uv.max, pos2(old.x.min(new.x) / old.x, old.y.min(new.y) / old.y));
            }
        }
    }

    #[test]
    fn page_backing_and_texture_stay_opaque_through_space_motion() {
        for ppp in [1.0, 1.5, 2.0] {
            for progress in [0.0, 0.25, 0.75, 1.0] {
                for has_texture in [false, true] {
                    let ctx = egui::Context::default();
                    let rect = Rect::from_min_size(pos2(252.0 + (1.0 - progress) * 36.0, 8.0), vec2(380.0, 404.0));
                    let texture = has_texture.then_some((egui::TextureId::User(102), rect.size() * ppp));
                    let mut output = ctx.run_ui(Default::default(), |ui| paint_pane_surface(ui.painter(), rect, texture, ppp, progress, RADIUS));
                    output.textures_delta.clear();
                    let surfaces = output.shapes.iter().filter_map(|shape| match &shape.shape {
                        egui::Shape::Rect(shape) if shape.rect == rect && shape.blur_width == 0.0 => Some(shape),
                        _ => None,
                    }).collect::<Vec<_>>();
                    assert_eq!(surfaces.len(), 1 + usize::from(has_texture));
                    assert!(surfaces.iter().all(|shape| shape.fill == Color32::WHITE), "space motion must not fade Chromium or its opaque backing: progress={progress}");
                }
            }
        }
    }
}

#[cfg(test)]
mod page_drag_tests {
    use super::*;

    const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];

    /// What a pointer gesture did: the window commands sent and the page
    /// input forwarded, in view coordinates.
    #[derive(Default)]
    struct Gesture {
        commands: Vec<egui::ViewportCommand>,
        page: Vec<PointerInput>,
    }

    impl Gesture {
        fn moved_window(&self) -> bool {
            self.commands.contains(&egui::ViewportCommand::StartDrag)
        }

        fn zoomed(&self) -> bool {
            self.commands.iter().any(|command| matches!(command, egui::ViewportCommand::Maximized(_)))
        }

        fn page_presses(&self) -> Vec<(f32, bool)> {
            self.page.iter().filter_map(|input| match input {
                PointerInput::Button { y, pressed, .. } => Some((*y, *pressed)),
                _ => None,
            }).collect()
        }
    }

    /// Run `steps` (pointer position, button change) over a page at `pane`
    /// with its band, registered as `draw_panes` does.
    fn gesture(size: egui::Vec2, pane: Rect, strip: Option<Rect>, steps: &[(Pos2, Option<bool>)]) -> Gesture {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, size);
        let mut done = Gesture::default();
        let (mut captured, mut last_press, mut over) = (false, None, false);
        let frames = std::iter::once((steps[0].0, None)).chain(steps.iter().copied()).chain(std::iter::once((steps[steps.len() - 1].0, None)));
        for (frame, (pos, button)) in frames.enumerate() {
            let mut events = vec![egui::Event::PointerMoved(pos)];
            if let Some(pressed) = button {
                events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
            }
            let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), events, time: Some(frame as f64 / 60.0), ..Default::default() }, |ui| {
                let page = ui.interact(pane, Id::unique(("pane", 0)), Sense::click_and_drag());
                if let Some(strip) = strip { window_drag(ui, strip, Id::unique(("page-top-drag", 0))); }
                for input in pointer_inputs(ui.ctx(), pane, &WebInput { page: &page, popup: None }, captured, &mut last_press, &mut over) {
                    if let PointerInput::Button { pressed, .. } = input { captured = pressed; }
                    done.page.push(input);
                }
            });
            output.textures_delta.clear();
            done.commands.extend(output.viewport_output.get(&egui::ViewportId::ROOT).map(|viewport| viewport.commands.clone()).unwrap_or_default());
        }
        done
    }

    /// What a page at `pane` hears from one frame per step: a pointer
    /// position, and a button change there. `held`: the page holds a press
    /// from before the first step.
    fn heard(size: egui::Vec2, pane: Rect, held: bool, steps: &[(Pos2, Option<bool>)]) -> Vec<Vec<PointerInput>> {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, size);
        let (mut captured, mut last_press, mut over) = (held, None, held);
        let mut frames = Vec::new();
        for (frame, (pos, button)) in steps.iter().copied().enumerate() {
            let mut events = vec![egui::Event::PointerMoved(pos)];
            if let Some(pressed) = button {
                events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
            }
            let mut heard = Vec::new();
            let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), events, time: Some(frame as f64 / 60.0), ..Default::default() }, |ui| {
                let page = ui.interact(pane, Id::unique(("pane", 0)), Sense::click_and_drag());
                for input in pointer_inputs(ui.ctx(), pane, &WebInput { page: &page, popup: None }, captured, &mut last_press, &mut over) {
                    if let PointerInput::Button { pressed, .. } = input { captured = pressed; }
                    heard.push(input);
                }
            });
            output.textures_delta.clear();
            frames.push(heard);
        }
        frames
    }

    #[test]
    fn a_page_hears_the_pointer_leave_for_crossbow_s_own_controls_once_and_not_mid_drag() {
        for size in SIZES {
            for (layout, _, panes) in layouts(size) {
                let pane = panes[0];
                // Clear of the page's interact radius: the sidebar, or the strip above it.
                let outside = if pane.left() > 40.0 { pos2(pane.left() - 24.0, pane.center().y) } else { pos2(pane.center().x, pane.top() - 24.0) };
                let (inside, further) = (pane.center(), outside - vec2(6.0, 6.0));
                assert!(outside.x > 0.0 && outside.y > 0.0, "{layout} at {size:?}: {pane:?}");
                let leaves = |inputs: &[PointerInput]| inputs.iter().filter(|input| **input == PointerInput::Leave).count();
                // Onto the sidebar or a menu beside the page, and about there.
                let frames = heard(size, pane, false, &[(inside, None), (inside, None), (outside, None), (further, None), (inside, None)]);
                assert_eq!(leaves(&frames[1]), 0, "{layout} at {size:?}");
                assert_eq!(leaves(&frames[2]), 1, "{layout} at {size:?}: the page hears the pointer leave: {:?}", frames[2]);
                assert_eq!(leaves(&frames[3]), 0, "{layout} at {size:?}: once");
                assert!(frames[4].iter().any(|input| matches!(input, PointerInput::Move { .. })), "{layout} at {size:?}: back over the page");

                // A drag the page started keeps the pointer until the release.
                let frames = heard(size, pane, false, &[(inside, None), (inside, Some(true)), (outside, None), (further, None), (further, Some(false)), (further, None)]);
                assert!(frames[2..4].iter().all(|inputs| leaves(inputs) == 0), "{layout} at {size:?}: not mid-drag: {frames:?}");
                assert_eq!(frames[4..].iter().map(|inputs| leaves(inputs)).sum::<usize>(), 1, "{layout} at {size:?}: once released: {frames:?}");

                // So does a press egui no longer tracks as a drag, as when the
                // pane is drawn at another index before the release.
                let frames = heard(size, pane, true, &[(outside, None), (further, None), (further, Some(false)), (further, None)]);
                assert!(frames[..2].iter().all(|inputs| leaves(inputs) == 0), "{layout} at {size:?}: not while held: {frames:?}");
                assert_eq!(frames[2..].iter().map(|inputs| leaves(inputs)).sum::<usize>(), 1, "{layout} at {size:?}: once released: {frames:?}");
            }
        }
    }

    /// Each layout's panes: sidebar hidden or shown, one page or a split.
    fn layouts(size: egui::Vec2) -> Vec<(String, Rect, Vec<Rect>)> {
        let ctx = egui::Context::default();
        let full = Rect::from_min_size(Pos2::ZERO, size);
        let mut out = Vec::new();
        for shown in [0.0, 1.0] {
            let mut width = 252.0;
            let content = sidebar_layout(&ctx, full, &mut width, shown, false, true, GAP).0;
            out.push((format!("sidebar {shown}"), content, vec![content]));
            out.push((format!("sidebar {shown}, split"), content, split_rects(content, &[0.4, 0.6])));
        }
        out
    }

    #[test]
    fn the_band_is_a_thin_strip_atop_each_top_pane_and_off_in_full_screen_or_when_unchosen() {
        for size in SIZES {
            for (layout, content, panes) in layouts(size) {
                for pane in &panes {
                    let strip = page_drag_strip(*pane, content.top(), true, false).unwrap_or_else(|| panic!("{layout} at {size:?}"));
                    assert_eq!(strip, Rect::from_min_size(pane.min, vec2(pane.width(), PAGE_DRAG_STRIP)));
                    assert_eq!(page_drag_strip(*pane, content.top(), false, false), None, "off: no band");
                    assert_eq!(page_drag_strip(*pane, content.top(), true, true), None, "full screen: no band");
                }
                if panes.len() > 1 {
                    assert!(panes[0].right() < panes[1].left(), "split bands stay apart at {size:?}");
                }
                // A pane below the content's top edge, as a stacked split would have, gets none.
                let below = pane_below(content);
                assert_eq!(page_drag_strip(below, content.top(), true, false), None);
            }
            let full = Rect::from_min_size(Pos2::ZERO, size);
            let mut width = 252.0;
            let ctx = egui::Context::default();
            let content = sidebar_layout(&ctx, full, &mut width, 0.0, false, false, 0.0).0;
            assert_eq!(page_drag_strip(content, content.top(), true, true), None, "full screen at {size:?}");
        }
    }

    fn pane_below(content: Rect) -> Rect {
        content.with_min_y(content.center().y)
    }

    #[test]
    fn dragging_the_band_moves_the_window_and_a_double_click_zooms_without_reaching_the_page() {
        for size in SIZES {
            for (layout, content, panes) in layouts(size) {
                for pane in panes {
                    let strip = page_drag_strip(pane, content.top(), true, false);
                    let at = pos2(pane.center().x, pane.top() + PAGE_DRAG_STRIP / 2.0);
                    // macOS moves the window with the pointer, so it stays over the band.
                    let drag = gesture(size, pane, strip, &[(at, Some(true)), (at + vec2(10.0, 1.0), None), (at + vec2(20.0, 1.0), None), (at + vec2(20.0, 1.0), Some(false))]);
                    assert!(drag.moved_window(), "{layout} at {size:?}: {:?}", drag.commands);
                    assert!(drag.page.is_empty(), "{layout}: the page sees none of the band's gesture: {:?}", drag.page);

                    let double = gesture(size, pane, strip, &[(at, Some(true)), (at, Some(false)), (at, Some(true)), (at, Some(false))]);
                    assert!(double.commands.contains(&egui::ViewportCommand::Maximized(true)), "{layout} at {size:?}: {:?}", double.commands);
                    assert!(!double.moved_window() && double.page_presses().is_empty());
                }
            }
        }
    }

    #[test]
    fn presses_below_the_band_or_without_it_stay_with_the_page_even_when_dragged_into_it() {
        for size in SIZES {
            for (layout, content, panes) in layouts(size) {
                for pane in panes {
                    let strip = page_drag_strip(pane, content.top(), true, false);
                    let x = pane.center().x;
                    // A link or selection just under the band, dragged up across it.
                    let below = pos2(x, pane.top() + PAGE_DRAG_STRIP + 2.0);
                    let into = pos2(x - 30.0, pane.top() + 2.0);
                    let select = gesture(size, pane, strip, &[(below, Some(true)), (pos2(x - 15.0, pane.top() + 4.0), None), (into, None), (into, Some(false))]);
                    assert!(!select.moved_window(), "{layout} at {size:?}: {:?}", select.commands);
                    assert_eq!(select.page_presses(), [(PAGE_DRAG_STRIP + 2.0, true), (2.0, false)], "{layout}: the page keeps its capture");
                    assert!(select.page.iter().any(|input| matches!(input, PointerInput::Move { y, .. } if *y == 2.0)), "{layout}: moves inside the band reach the capturing page");

                    let middle = pane.center();
                    let click = gesture(size, pane, strip, &[(middle, Some(true)), (middle, Some(false))]);
                    assert_eq!(click.page_presses().len(), 2);
                    assert!(!click.moved_window() && !click.zoomed());

                    // Off, or in full screen: the top of the page is the page's.
                    let top = pos2(x, pane.top() + 2.0);
                    let off = gesture(size, pane, None, &[(top, Some(true)), (top + vec2(30.0, 30.0), None), (top + vec2(30.0, 30.0), Some(false))]);
                    assert!(!off.moved_window() && off.page_presses().len() == 2, "{layout} at {size:?}");
                }
            }
        }
    }
}

#[cfg(test)]
mod floating_tests {
    use super::*;

    #[test]
    fn floating_sidebar_reserves_the_native_header_only_on_the_left() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let full = Rect::from_min_size(Pos2::ZERO, size);
            for native in [false, true] {
                for right in [false, true] {
                    let rect = floating_sidebar_rect(full, 252.0, 1.0, right, native);
                    assert_eq!(rect.top(), if native && !right { full.top() } else { full.top() + GAP });
                    assert_eq!(rect.bottom(), full.bottom() - GAP);
                    assert!(full.contains_rect(rect));
                }
            }
        }
    }

    #[test]
    fn floating_sidebar_wheel_does_not_reach_the_page_beneath_it() {
        for height in [420.0, 860.0] {
            let ctx = egui::Context::default();
            let screen = Rect::from_min_size(Pos2::ZERO, vec2(640.0, height));
            let card = Rect::from_min_size(pos2(GAP, GAP), vec2(252.0, height - 2.0 * GAP));
            for frame in 0..8 {
                let over_sidebar = frame < 6;
                let pointer = if over_sidebar { card.center() } else { pos2(500.0, 200.0) };
                ctx.run_ui(egui::RawInput { screen_rect: Some(screen), events: vec![egui::Event::PointerMoved(pointer)], ..Default::default() }, |ui| {
                    // Production draws the page before its floating sidebar.
                    let page = ui.interact(screen, Id::unique("page-wheel-target"), Sense::click_and_drag());
                    if frame >= 4 {
                        assert_eq!(WebInput { page: &page, popup: None }.wheel_position(ui.ctx()), (!over_sidebar).then_some(pointer));
                    }
                    show_floating_sidebar(ui, card, |ui| { ui.label("Floating chrome"); });
                }).textures_delta.clear();
            }
        }
    }

    #[test]
    fn floating_sidebar_blocks_page_wheel_during_a_captured_page_drag() {
        for height in [420.0, 860.0] {
            let ctx = egui::Context::default();
            let screen = Rect::from_min_size(Pos2::ZERO, vec2(640.0, height));
            let card = Rect::from_min_size(pos2(GAP, GAP), vec2(252.0, height - 2.0 * GAP));
            for frame in 0..10 {
                let over_sidebar = (5..8).contains(&frame);
                let pointer = if over_sidebar { card.center() } else { pos2(500.0, 200.0) };
                let mut events = vec![egui::Event::PointerMoved(pointer)];
                if frame == 4 || frame == 9 {
                    events.push(egui::Event::PointerButton { pos: pointer, button: egui::PointerButton::Primary, pressed: frame == 4, modifiers: Default::default() });
                }
                if frame >= 5 {
                    events.push(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -80.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() });
                }
                ctx.run_ui(egui::RawInput { screen_rect: Some(screen), events, ..Default::default() }, |ui| {
                    let page = ui.interact(screen, Id::unique("page-wheel-target"), Sense::click_and_drag());
                    if (6..9).contains(&frame) {
                        assert!(page.dragged(), "the page still owns the held pointer gesture");
                        assert_eq!(WebInput { page: &page, popup: None }.wheel_position(ui.ctx()), (!over_sidebar).then_some(pointer), "wheel ownership must follow the surface under the pointer, not drag capture");
                    }
                    show_floating_sidebar(ui, card, |ui| { ui.label("Floating chrome"); });
                }).textures_delta.clear();
            }
        }
    }

    #[test]
    fn floating_sidebar_receives_wheel_input_inside_its_painted_bounds() {
        for height in [420.0, 860.0] {
            let ctx = egui::Context::default();
            let screen = Rect::from_min_size(Pos2::ZERO, vec2(640.0, height));
            let card = Rect::from_min_size(pos2(GAP, GAP), vec2(252.0, height - 2.0 * GAP));
            let viewport = Rect::from_min_size(card.min + vec2(10.0, 92.0), vec2(232.0, 44.0));
            let mut offset = 0.0;
            for frame in 0..12 {
                let mut events = vec![egui::Event::PointerMoved(viewport.center())];
                if frame == 4 {
                    events.push(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -200.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() });
                }
                ctx.run_ui(egui::RawInput { screen_rect: Some(screen), events, time: Some(frame as f64 / 30.0), ..Default::default() }, |ui| {
                    show_floating_sidebar(ui, card, |ui| {
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(viewport));
                        offset = egui::ScrollArea::vertical().min_scrolled_height(0.0).auto_shrink([false, false]).show(&mut child, |ui| {
                            ui.allocate_exact_size(vec2(232.0, 400.0), Sense::hover());
                        }).state.clamped_offset().y;
                    });
                }).textures_delta.clear();
            }
            assert!(offset > 90.0, "floating sidebar must receive wheel input, offset={offset}");
        }
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[test]
    fn progress_uses_content_rounding_at_every_width_and_scale() {
        for width in [120.0, 372.0, 1092.0] {
            for scale in [1.0, 1.5, 2.0] {
                for progress in [0.0, 0.01, 0.08, 0.5, 1.0, 1.2] {
                    let ctx = egui::Context::default();
                    ctx.set_pixels_per_point(scale);
                    let rect = Rect::from_min_size(pos2(18.0, 8.0), vec2(width, 300.0));
                    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                        paint_progress(ui.painter(), rect, RADIUS, progress, 1.0, Color32::RED);
                    });
                    let shape = output.shapes.iter().find_map(|s| match &s.shape {
                        egui::Shape::Rect(r) if r.fill == Color32::RED => Some((s.clip_rect, r)),
                        _ => None,
                    });
                    if progress > 0.0 {
                        let (clip, shape) = shape.expect("visible progress shape");
                        assert_eq!(shape.rect, rect);
                        assert_eq!(shape.corner_radius, CornerRadius::same(RADIUS));
                        assert!(clip.left() >= rect.left() && clip.right() <= rect.right());
                        assert!(clip.bottom() <= rect.top() + 2.5);
                    }
                    output.textures_delta.clear();
                }
            }
        }
    }
}

fn detach_split_pane(splits: &mut HashMap<String, Split>, space: &str, tab_id: &str, active: Option<&str>, focused: Option<&str>) -> Option<Action> {
    let split = splits.get_mut(space)?;
    let index = split.tabs.iter().position(|id| id == tab_id)?;
    split.remove(tab_id);
    let survivor = if active == Some(tab_id) || focused == Some(tab_id) {
        focused.filter(|id| split.tabs.iter().any(|tab| tab == id))
            .or_else(|| active.filter(|id| split.tabs.iter().any(|tab| tab == id)))
            .map(str::to_owned)
            .or_else(|| split.tabs.get(index).or_else(|| split.tabs.last()).cloned())
    } else {
        None
    };
    if split.tabs.len() < 2 {
        splits.remove(space);
    }
    // Activation also restores web focus and marks activity dirty for session saves.
    survivor.map(Action::ActivateTab)
}

#[cfg(test)]
mod split_tests {
    use super::*;

    fn splits(tabs: &[&str]) -> HashMap<String, Split> {
        HashMap::from([("space".into(), Split {
            tabs: tabs.iter().map(|id| id.to_string()).collect(),
            ratios: vec![1.0 / tabs.len().max(1) as f32; tabs.len()],
        })])
    }

    fn activation(action: Option<Action>) -> Option<String> {
        action.map(|action| match action {
            Action::ActivateTab(id) => id,
            other => panic!("detaching must only activate a survivor, not {other:?}"),
        })
    }

    fn assert_split(split: &Split, tabs: &[&str]) {
        assert_eq!(split.tabs, tabs);
        assert_eq!(split.ratios.len(), tabs.len());
        assert!(split.ratios.iter().all(|ratio| ratio.is_finite() && *ratio > 0.0));
        assert!((split.ratios.iter().sum::<f32>() - 1.0).abs() < 0.00001);
    }

    #[test]
    fn detaching_active_pane_selects_a_survivor_for_the_remaining_split() {
        for (removed, remaining, selected) in [
            ("A", ["B", "C"], "B"),
            ("B", ["A", "C"], "C"),
            ("C", ["A", "B"], "B"),
        ] {
            let mut splits = splits(&["A", "B", "C"]);
            let action = detach_split_pane(&mut splits, "space", removed, Some(removed), Some(removed));
            assert_split(&splits["space"], &remaining);
            assert_eq!(activation(action).as_deref(), Some(selected), "active pane {removed} must not hide its surviving split");
        }
    }

    #[test]
    fn detaching_active_pane_preserves_a_different_surviving_focus() {
        let mut splits = splits(&["A", "B", "C"]);
        let action = detach_split_pane(&mut splits, "space", "A", Some("A"), Some("C"));
        assert_split(&splits["space"], &["B", "C"]);
        assert_eq!(activation(action).as_deref(), Some("C"));
    }

    #[test]
    fn detaching_non_active_pane_keeps_surviving_active_and_focus() {
        for focused in [None, Some("A"), Some("C")] {
            let mut splits = splits(&["A", "B", "C"]);
            let action = detach_split_pane(&mut splits, "space", "B", Some("A"), focused);
            assert_split(&splits["space"], &["A", "C"]);
            assert!(action.is_none(), "surviving selection and focus must not be reset");
        }
    }

    #[test]
    fn detaching_focused_non_active_pane_returns_focus_to_the_active_survivor() {
        let mut splits = splits(&["A", "B", "C"]);
        let action = detach_split_pane(&mut splits, "space", "B", Some("A"), Some("B"));
        assert_split(&splits["space"], &["A", "C"]);
        assert_eq!(activation(action).as_deref(), Some("A"));
    }

    #[test]
    fn detaching_from_two_panes_dissolves_the_split_and_keeps_the_survivor() {
        for (removed, active, focused, selected) in [
            ("A", "A", "A", Some("B")),
            ("A", "A", "B", Some("B")),
            ("B", "A", "B", Some("A")),
            ("B", "A", "A", None),
        ] {
            let mut splits = splits(&["A", "B"]);
            let action = detach_split_pane(&mut splits, "space", removed, Some(active), Some(focused));
            assert!(!splits.contains_key("space"));
            assert_eq!(activation(action).as_deref(), selected);
        }
    }

    #[test]
    fn detaching_the_last_pane_keeps_its_tab_as_standalone_content() {
        let mut splits = splits(&["A"]);
        let action = detach_split_pane(&mut splits, "space", "A", Some("A"), Some("A"));
        assert!(!splits.contains_key("space"));
        assert!(action.is_none(), "no survivor exists; the detached tab is still open");
    }

    #[test]
    fn detaching_missing_members_is_a_no_op_and_never_changes_other_spaces() {
        let mut splits = splits(&["A", "B", "C"]);
        splits.insert("other".into(), Split::new("D".into(), "E".into()));
        for (space, removed) in [("space", "missing"), ("absent", "A")] {
            assert!(detach_split_pane(&mut splits, space, removed, Some("A"), Some("A")).is_none());
            assert_split(&splits["space"], &["A", "B", "C"]);
            assert_split(&splits["other"], &["D", "E"]);
        }
        let action = detach_split_pane(&mut splits, "space", "A", Some("A"), None);
        assert_split(&splits["other"], &["D", "E"]);
        assert_eq!(activation(action).as_deref(), Some("B"));
    }
}

/// Progress bar, hovered-link bubble and error card on top of a pane.
fn pane_chrome(app: &mut CrossbowApp, ui: &mut Ui, tab_id: &str, rect: Rect, in_split: bool, radius: u8) {
    let ctx = ui.ctx().clone();
    let pal = app.palette();
    let Some(tab) = app.tab(tab_id) else { return };
    let nav = tab.nav.clone();
    // 5-Second Previews: only a view in its tab's own profile, and not under Peek.
    let view = tab.view.clone().filter(|view| crate::app::tab_profile(&app.lib, tab) == Some(view.profile_id()));
    if let (Some(view), Some(pointer), None) = (view, ctx.input(|input| input.pointer.hover_pos()).filter(|pointer| rect.contains(*pointer)), &app.ui.peek) {
        app.link_previews.seen = Some(crate::link_preview::hover(&view, &nav, pointer, rect));
    }
    let painter = ui.painter().clone().with_clip_rect(rect);

    // Thin progress line along the top edge.
    let target = if nav.loading { nav.progress.max(0.08) as f32 } else { 1.0 };
    let prog = anim::spring(&ctx, Id::unique(("progress", tab_id)), target, 140.0);
    let vis = anim::spring(&ctx, Id::unique(("progress-vis", tab_id)), if nav.loading { 1.0 } else { 0.0 }, 90.0);
    paint_progress(&painter, rect, radius, prog, vis, pal.accent);

    // Hovered link.
    if let Some(status) = nav.status.filter(|s| !s.is_empty()) {
        let font = egui::FontId::proportional(11.5);
        let galley = painter.layout_no_wrap(crossbow_core::search::display_url(&status), font, Color32::from_gray(40));
        let r = Rect::from_min_size(pos2(rect.left() + 6.0, rect.bottom() - galley.size().y - 14.0), galley.size() + vec2(14.0, 8.0));
        painter.rect(r, 6, Color32::from_rgba_unmultiplied(250, 250, 252, 240), Stroke::new(1.0, Color32::from_black_alpha(25)), egui::StrokeKind::Inside);
        painter.galley(r.min + vec2(7.0, 4.0), galley, Color32::from_gray(40));
    }

    // Split view pane header: close button on hover.
    if in_split && ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| rect.contains(p) && p.y < rect.top() + 28.0) {
        let r = Rect::from_min_size(pos2(rect.right() - 30.0, rect.top() + 6.0), vec2(22.0, 22.0));
        ui.painter().rect_filled(r, 6, Color32::from_black_alpha(90));
        if icons::button(ui, r, Icon::Close, Color32::WHITE, Color32::from_black_alpha(60), true).clicked() {
            if let Some(action) = detach_split_pane(&mut app.splits, &app.active_space, tab_id, app.active_tab.get(&app.active_space).map(String::as_str), app.ui.focused_pane.as_deref()) {
                app.actions.push(action);
            }
        }
    }

    // Load error card.
    if let Some((url, code, text)) = app.errors.get(tab_id).cloned() {
        let card = Rect::from_center_size(rect.center(), vec2(420.0f32.min(rect.width() - 40.0), 190.0));
        let p = ui.painter();
        p.rect_filled(rect, radius, Color32::from_rgb(0xf7, 0xf7, 0xf9));
        p.text(card.center_top() + vec2(0.0, 30.0), egui::Align2::CENTER_CENTER, "This page couldn't load", egui::FontId::proportional(20.0), Color32::from_gray(30));
        p.text(card.center_top() + vec2(0.0, 62.0), egui::Align2::CENTER_CENTER, crossbow_core::search::display_url(&url), egui::FontId::proportional(13.0), Color32::from_gray(90));
        p.text(card.center_top() + vec2(0.0, 86.0), egui::Align2::CENTER_CENTER, format!("{} ({code})", text.trim_start_matches("net::")), egui::FontId::monospace(11.5), Color32::from_gray(120));
        let b = Rect::from_center_size(card.center_top() + vec2(0.0, 128.0), vec2(120.0, 32.0));
        let resp = ui.interact(b, Id::unique(("retry", tab_id)), Sense::click());
        p.rect_filled(b, 8, if resp.hovered() { pal.accent } else { pal.accent.gamma_multiply(0.9) });
        p.text(b.center(), egui::Align2::CENTER_CENTER, "Try again", egui::FontId::proportional(13.0), Color32::WHITE);
        if resp.clicked() {
            app.actions.push(Action::ActivateTab(tab_id.to_string()));
            app.actions.push(Action::Reload);
        }
    }
}
