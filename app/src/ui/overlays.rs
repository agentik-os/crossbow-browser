//! Everything that floats above the window content: Peek, the find bar,
//! context menus, the library popover (archive, history, downloads) and toasts.

use crate::app::{Action, CrossbowApp, MenuKind, RenameTarget};
use crate::app::pip::PICTURE_IN_PICTURE;
use crate::translate::Choice;
use crate::ui::translate::{SHOW_ORIGINAL, TRANSLATE_PAGE};
use crate::shortcuts::Command;
use crate::ui::icons::{self, Icon};
use crate::ui::theme::Palette;
use crate::ui::{anim, content};
use egui::epaint::{RectShape, Shadow};
use egui::{Align2, Color32, CornerRadius, FontId, Id, Key, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};

pub fn show(app: &mut CrossbowApp, ui: &mut Ui) {
    let pal = app.palette();
    peek(app, ui, &pal);
    find_bar(app, ui, &pal);
    crate::ui::logins::offers(app, ui, &pal);
    crate::ui::site_permissions::prompts(app, ui, &pal);
    crate::ui::translate::cards(app, ui, &pal);
    library(app, ui, &pal);
    downloads_card(app, ui, &pal);
    menu(app, ui, &pal);
    crate::ui::link_preview_card::show(app, ui.ctx(), &pal);
    app.show_capture(ui.ctx());
    let notice = crate::ui::site_notice::show_app(app, ui.ctx(), &pal);
    toasts(app, ui, &pal, notice);
}

fn panel_frame(pal: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(pal.panel)
        .stroke(Stroke::new(1.0, pal.panel_border))
        .corner_radius(10)
        .inner_margin(egui::Margin::same(5))
        .shadow(Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(55) })
}

// -- Peek -----------------------------------------------------------------------

fn peek(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette) {
    app.capture.peek_page = None;
    let Some(p) = &app.ui.peek else { return };
    let ctx = ui.ctx().clone();
    let (opened_at, closing_at, view) = (p.opened_at, p.closing_at, p.view.clone());
    let open_t = anim::ease_out_strong(anim::progress(&ctx, opened_at, 0.22));
    let close_t = closing_at.map(|c| anim::ease_out_strong(anim::progress(&ctx, c, 0.15))).unwrap_or(0.0);
    if close_t >= 1.0 {
        let p = app.ui.peek.take().unwrap();
        crate::app::retire_peek_textures(&app.texture_retirement, p.texture, p.popup_texture);
        p.view.close();
        app.ui.pointer_over.remove("peek");
        if app.ui.pointer_capture.as_deref() == Some("peek") {
            app.ui.pointer_capture = None;
        }
        return;
    }
    let vis = (open_t.min(1.0) * (1.0 - close_t)).clamp(0.0, 1.0);
    let ppp = ctx.pixels_per_point();
    let shown = peek_viewport(&ctx, open_t, close_t, |rect, scale| {
        app.update_view_position(&view, rect.min, scale);
        view.resize(rect.width(), rect.height(), scale);
    });

    let texture = peek_texture(app);
    let popup = peek_popup_texture(app);
    let covered = app.ui.settings.is_some() || app.ui.command.is_some();
    let input_enabled = !covered && ctx.input(|input| input.focused);
    // Captured only once it's fully open, where the page is drawn as laid out.
    if closing_at.is_none() && open_t >= 1.0 { app.capture.peek_page = Some(shown); }
    let capturing = app.capture.selecting.as_ref().is_some_and(|(target, _)| *target == crate::app::capture_host::Target::Peek);
    if !input_enabled {
        content::suspend_pointer(&mut app.ui.pointer_capture, "peek", || view.capture_lost());
        content::leave_pointer(&mut app.ui.pointer_over, "peek", || view.mouse_leave());
    }
    let dismiss = show_peek_surface(&ctx, vis, !covered, |ui| {
        let p = ui.painter().clone();
        p.add(Shadow { offset: [0, 20], blur: 60, spread: 0, color: Color32::from_black_alpha((110.0 * vis) as u8) }.as_shape(shown, CornerRadius::same(12)));
        paint_peek_page(&p, shown, texture, ppp);
        // Reserve the page even before input is enabled, so a loading/closing
        // page click cannot be mistaken for a backdrop dismissal.
        let resp = ui.interact(shown, Id::unique("peek-input"), Sense::click_and_drag());
        let nav = view.nav();
        // 5-Second Previews: Peek's links preview in Peek's own profile.
        if let Some(pointer) = ctx.input(|input| input.pointer.hover_pos()).filter(|pointer| shown.contains(*pointer)) && closing_at.is_none() {
            app.link_previews.seen = Some(crate::link_preview::hover(&view, &nav, pointer, shown));
        }
        if nav.loading {
            content::paint_progress(&p, shown, 12, nav.progress.max(0.08) as f32, vis, pal.accent);
        }
        if let Some(action) = peek_toolbar(ui, shown, &nav, closing_at.is_none(), vis) {
            app.actions.push(action);
        }
        let popup = popup.map(|texture| content::popup_surface(ui, "peek", shown.min, texture, ppp));
        if capturing || (input_enabled && closing_at.is_none() && open_t >= 1.0 && app.capture_press(&ctx, "peek", shown, resp.hovered())) {
            content::suspend_pointer(&mut app.ui.pointer_capture, "peek", || view.capture_lost());
            content::leave_pointer(&mut app.ui.pointer_over, "peek", || view.mouse_leave());
        } else if input_enabled && closing_at.is_none() && open_t > 0.6 {
            content::forward_pointer(app, ui, &view, "peek", shown, &content::WebInput { page: &resp, popup: popup.as_ref() }, 999);
        }
    });
    // A press outside the page while capturing cancels the capture, not Peek.
    if dismiss && input_enabled && closing_at.is_none() && !capturing {
        app.actions.push(Action::ClosePeek);
    }
}

pub(super) fn show_peek_surface(ctx: &egui::Context, visibility: f32, enabled: bool, contents: impl FnOnce(&mut Ui)) -> bool {
    let screen = ctx.content_rect();
    let area = egui::Area::new(Id::unique("peek-card")).order(egui::Order::Foreground)
        .fixed_pos(screen.min).default_size(screen.size()).constrain(false).fade_in(false).sense(Sense::hover())
        .enabled(enabled).interactable(enabled);
    if enabled {
        ctx.memory_mut(|memory| memory.set_modal_layer(area.layer()));
    }
    area.show(ctx, |ui| {
            ui.set_min_size(screen.size());
            // One layer owns both the backdrop and card. Later card/popup widgets
            // win hits, while uncovered backdrop clicks remain dismissible.
            let backdrop = ui.interact(screen, Id::unique("peek-backdrop"), Sense::CLICK);
            ui.painter().rect_filled(screen, 0, Color32::from_black_alpha((90.0 * visibility) as u8));
            contents(ui);
            backdrop.clicked()
        }).inner
}

fn peek_rect(card: Rect, open_t: f32, close_t: f32) -> Rect {
    // Translate, never scale a page independently of its CEF viewport and input.
    card.translate(vec2(0.0, (1.0 - open_t.clamp(0.0, 1.0)) * 16.0 + close_t.clamp(0.0, 1.0) * 8.0))
}

fn peek_viewport(ctx: &egui::Context, open_t: f32, close_t: f32, update: impl FnOnce(Rect, f32)) -> Rect {
    let screen = ctx.content_rect();
    let size = (screen.size() * vec2(0.78, 0.84)).round();
    let card = Rect::from_center_size(screen.center() + vec2(0.0, 12.0), size);
    let shown = peek_rect(card, open_t, close_t);
    // WebView owns size-and-scale deduplication, including scale-only changes.
    update(shown, ctx.pixels_per_point());
    shown
}

#[cfg(test)]
mod peek_resize_tests {
    use super::*;

    #[test]
    fn peek_motion_keeps_page_input_and_native_geometry_aligned_and_opaque() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for scale in [1.0, 2.0, 2.5] {
                for (opening, closing) in [(0.0, 0.0), (0.65, 0.0), (1.0, 0.0), (1.0, 0.7)] {
                    for loaded in [false, true] {
                        let ctx = egui::Context::default();
                        let screen = Rect::from_min_size(Pos2::ZERO, size);
                        let mut inspected = 0;
                        for frame in 0..4 {
                            let mut input = egui::RawInput { screen_rect: Some(screen), time: Some(frame as f64 / 60.0), ..Default::default() };
                            input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(scale);
                            let mut configured = None;
                            let mut shown = Rect::NOTHING;
                            let mut output = ctx.run_ui(input, |ui| {
                                shown = peek_viewport(ui.ctx(), opening, closing, |rect, ppp| configured = Some((rect, ppp)));
                                show_peek_surface(ui.ctx(), opening * (1.0 - closing), true, |ui| {
                                    let page = loaded.then_some((egui::TextureId::User(90), shown.size() * scale));
                                    paint_peek_page(ui.painter(), shown, page, scale);
                                    if loaded {
                                        let popup = (egui::TextureId::User(91), vec2(180.0, 96.0) * scale, Rect::from_min_size(pos2(36.0, 82.0), vec2(180.0, 96.0)));
                                        content::popup_surface(ui, "peek", shown.min, popup, scale);
                                    }
                                });
                            });
                            assert_eq!(configured, Some((shown, scale)), "native geometry must use the painted/input rectangle");
                            assert_eq!(shown.size(), (size * vec2(0.78, 0.84)).round(), "motion must not scale or crop the page");
                            let fills = output.shapes.iter().filter_map(|shape| match &shape.shape {
                                egui::Shape::Rect(rect) if rect.rect == shown => Some(rect.fill),
                                _ => None,
                            }).collect::<Vec<_>>();
                            if !fills.is_empty() {
                                inspected += 1;
                                assert_eq!(fills.len(), if loaded { 2 } else { 1 });
                                assert!(fills.iter().all(|fill| *fill == Color32::WHITE), "backing and page must be opaque from the first visible Area frame");
                                if loaded {
                                    let popup = output.shapes.iter().find_map(|shape| match &shape.shape {
                                        egui::Shape::Mesh(mesh) if mesh.texture_id == egui::TextureId::User(91) => Some(mesh),
                                        _ => None,
                                    }).expect("popup paint");
                                    assert!(popup.vertices.iter().all(|vertex| vertex.color == Color32::WHITE));
                                }
                            }
                            output.textures_delta.clear();
                        }
                        assert!(inspected >= 2, "inspect actual Area output, not an invisible sizing pass");
                    }
                }
            }
        }
    }

    #[test]
    fn peek_surface_dismisses_only_uncovered_backdrop() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for scale in [1.0, 2.0] {
                for opening in [0.0, 0.65, 1.0] {
                    for region in 0..5 {
                        let ctx = egui::Context::default();
                        crate::ui::window::install_fonts(&ctx);
                        let screen = Rect::from_min_size(Pos2::ZERO, size);
                        let mut dismissals = 0;
                        let mut page_clicks = 0;
                        let mut popup_clicks = 0;
                        let mut actions = Vec::new();
                        for frame in 0..7 {
                            let card = peek_rect(Rect::from_center_size(screen.center() + vec2(0.0, 12.0), (size * vec2(0.78, 0.84)).round()), opening, 0.0);
                            let popup = Rect::from_min_size(pos2(card.width() - 20.0, card.height() * 0.5), vec2(60.0, 40.0));
                            let pointer = match region {
                                0 => pos2(12.0, screen.center().y),
                                1 => card.center() - vec2(40.0, 0.0),
                                2 => pos2(card.right() + 20.0, card.top() + popup.center().y),
                                3 => pos2(card.right() - 133.0, card.top() - 24.0),
                                _ => pos2(card.left() + 10.0, card.top() - 24.0),
                            };
                            let mut events = vec![egui::Event::PointerMoved(pointer)];
                            if frame == 4 || frame == 5 {
                                events.push(egui::Event::PointerButton { pos: pointer, button: egui::PointerButton::Primary, pressed: frame == 4, modifiers: Default::default() });
                            }
                            let mut input = egui::RawInput { screen_rect: Some(screen), events, time: Some(frame as f64 / 60.0), ..Default::default() };
                            input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(scale);
                            ctx.run_ui(input, |ui| {
                                dismissals += usize::from(show_peek_surface(ui.ctx(), opening, true, |ui| {
                                    paint_peek_page(ui.painter(), card, None, scale);
                                    page_clicks += usize::from(ui.interact(card, Id::unique("peek-input"), Sense::click_and_drag()).clicked());
                                    let nav = crossbow_engine::NavState { url: "https://example.test/page".into(), ..Default::default() };
                                    if let Some(action) = peek_toolbar(ui, card, &nav, true, opening) { actions.push(action); }
                                    popup_clicks += usize::from(content::popup_surface(ui, "peek", card.min, (egui::TextureId::User(91), popup.size() * scale, popup), scale).clicked());
                                }));
                            }).textures_delta.clear();
                        }
                        assert_eq!(dismissals, usize::from(region == 0), "size={size:?}, scale={scale}, opening={opening}, region={region}");
                        assert_eq!(page_clicks, usize::from(region == 1));
                        assert_eq!(popup_clicks, usize::from(region == 2));
                        assert!(actions.is_empty(), "toolbar gaps and origin are not buttons or backdrop");
                    }
                }
            }
        }
    }

    #[test]
    fn peek_popup_overflow_routes_capture_and_wheel_without_dismissal() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for scale in [1.0, 2.0] {
                let ctx = egui::Context::default();
                let screen = Rect::from_min_size(Pos2::ZERO, size);
                let card = Rect::from_center_size(screen.center() + vec2(0.0, 12.0), (size * vec2(0.78, 0.84)).round());
                let popup = Rect::from_min_size(pos2(card.width() - 20.0, card.height() * 0.5), vec2(60.0, 40.0));
                let over = pos2(card.right() + 20.0, card.top() + popup.center().y);
                let outside = pos2(12.0, screen.center().y);
                let mut captured = false;
                let mut buttons = Vec::new();
                let mut wheels = Vec::new();
                let mut moves = Vec::new();
                for frame in 0..10 {
                    let pointer = if (5..8).contains(&frame) { outside } else { over };
                    let mut events = vec![egui::Event::PointerMoved(pointer)];
                    if frame == 4 || frame == 7 {
                        events.push(egui::Event::PointerButton { pos: pointer, button: egui::PointerButton::Primary, pressed: frame == 4, modifiers: Default::default() });
                    }
                    if [3, 6, 8].contains(&frame) {
                        events.push(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -40.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() });
                    }
                    let mut input = egui::RawInput { screen_rect: Some(screen), events, time: Some(frame as f64 / 60.0), ..Default::default() };
                    input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(scale);
                    ctx.run_ui(input, |ui| {
                        assert!(!show_peek_surface(ui.ctx(), 1.0, true, |ui| {
                            let page = ui.interact(card, Id::unique("peek-input"), Sense::click_and_drag());
                            let popup = content::popup_surface(ui, "peek", card.min, (egui::TextureId::User(91), popup.size() * scale, popup), scale);
                            let target = content::WebInput { page: &page, popup: Some(&popup) };
                            for event in target.events(ui.ctx(), captured) {
                                match event {
                                    egui::Event::PointerButton { pos, pressed, .. } => { buttons.push((pos, pressed)); captured = pressed; }
                                    egui::Event::PointerMoved(pos) => moves.push((frame, pos)),
                                    egui::Event::MouseWheel { .. } => wheels.push(target.wheel_position(ui.ctx()).unwrap()),
                                    _ => {}
                                }
                            }
                        }));
                    }).textures_delta.clear();
                }
                assert_eq!(buttons, [(over, true), (outside, false)], "one page-owned press/release, including captured release outside");
                assert_eq!(wheels, [over, over], "capture must not send an outside wheel to the popup");
                assert!(moves.contains(&(5, outside)), "drag movement must retain capture outside both rectangles");
                assert!(!captured);
            }
        }
    }

    #[test]
    fn peek_keyboard_focus_excludes_backdrop_passive_shields_and_background_chrome() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let screen = Rect::from_min_size(Pos2::ZERO, size);
            let page = Rect::from_center_size(screen.center() + vec2(0.0, 12.0), (size * vec2(0.78, 0.84)).round());
            let excluded = ["peek-backdrop", "peek-toolbar-surface", "peek-origin-surface", "background-chrome"].map(Id::unique);
            let mut observed_focus = false;
            for frame in 0..20 {
                let events = if frame >= 4 {
                    vec![egui::Event::Key { key: Key::Tab, physical_key: None, pressed: frame % 2 == 0, repeat: false, modifiers: Default::default() }]
                } else { Vec::new() };
                ctx.run_ui(egui::RawInput { screen_rect: Some(screen), events, time: Some(frame as f64 / 60.0), ..Default::default() }, |ui| {
                    ui.interact(Rect::from_min_size(pos2(2.0, 50.0), vec2(50.0, 30.0)), excluded[3], Sense::click());
                    show_peek_surface(ui.ctx(), 1.0, true, |ui| {
                        let nav = crossbow_engine::NavState { url: "https://example.test/page".into(), ..Default::default() };
                        peek_toolbar(ui, page, &nav, true, 1.0);
                    });
                    if frame >= 4 {
                        if let Some(focused) = ui.ctx().memory(|memory| memory.focused()) {
                            assert!(!excluded.contains(&focused), "only actual Peek controls belong in keyboard traversal");
                            observed_focus = true;
                        }
                    }
                }).textures_delta.clear();
            }
            assert!(observed_focus, "actual toolbar controls must remain keyboard reachable");
        }
    }

    #[test]
    fn peek_forwards_scale_changes_at_unchanged_logical_size() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let ctx = egui::Context::default();
            let screen = Rect::from_min_size(Pos2::ZERO, size);
            let expected = (size * vec2(0.78, 0.84)).round();
            let mut calls = Vec::new();
            for scale in [1.0, 2.0, 2.0, 1.0] {
                let mut input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
                input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(scale);
                ctx.run_ui(input, |ui| {
                    let card = peek_viewport(ui.ctx(), 1.0, 0.0, |rect, scale| calls.push((rect.size(), scale)));
                    assert_eq!(card.size(), expected);
                    assert_eq!(card.center(), screen.center() + vec2(0.0, 12.0));
                }).textures_delta.clear();
            }
            assert_eq!(calls, [1.0, 2.0, 2.0, 1.0].map(|scale| (expected, scale)), "CEF, not Peek's size-only gate, must deduplicate size and scale");
        }
    }
}

fn peek_toolbar(ui: &mut Ui, shown: Rect, nav: &crossbow_engine::NavState, enabled: bool, visibility: f32) -> Option<Action> {
    let p = ui.painter();
    let bar = Rect::from_min_size(pos2(shown.right() - 136.0, shown.top() - 40.0), vec2(136.0, 32.0));
    let fill = Color32::from_black_alpha((120.0 * visibility) as u8);
    let ink = Color32::WHITE.gamma_multiply(visibility);
    ui.interact(bar, Id::unique("peek-toolbar-surface"), Sense::CLICK);
    p.rect_filled(bar, 9, fill);
    let host = crossbow_core::search::host_of(&nav.url).unwrap_or_default();
    if !host.is_empty() {
        let mut job = egui::text::LayoutJob::simple(host, FontId::proportional(12.5), ink, (bar.left() - shown.left() - 42.0).max(0.0));
        job.wrap.max_rows = 1;
        let galley = p.layout_job(job);
        let hb = Rect::from_min_size(pos2(shown.left(), bar.top()), vec2(galley.size().x + 24.0, bar.height()));
        ui.interact(hb, Id::unique("peek-origin-surface"), Sense::CLICK);
        p.rect_filled(hb, 9, fill);
        p.galley(hb.center() - galley.size() * 0.5, galley, ink);
    }
    let (load_icon, load_action, load_label) = if nav.loading {
        (Icon::Stop, Action::Stop, "Stop loading")
    } else {
        (Icon::Reload, Action::Reload, "Reload")
    };
    let mut action = None;
    for (index, (icon, next, label)) in [
        (load_icon, load_action, load_label),
        (Icon::Expand, Action::PeekToTab, "Open as tab"),
        (Icon::Close, Action::ClosePeek, "Close (Esc)"),
    ].into_iter().enumerate() {
        let rect = Rect::from_center_size(pos2(bar.left() + 24.0 + index as f32 * 44.0, bar.center().y), vec2(28.0, 26.0));
        if icons::button(ui, rect, icon, ink, Color32::from_white_alpha(30), enabled).on_hover_text(label).clicked() {
            action = Some(next);
        }
    }
    action
}

fn paint_peek_page(painter: &egui::Painter, shown: Rect, texture: Option<(egui::TextureId, egui::Vec2)>, ppp: f32) {
    painter.rect_filled(shown, 12, Color32::WHITE);
    if let Some((id, tex_size)) = texture {
        let img = Rect::from_min_size(shown.min, (tex_size / ppp).min(shown.size()));
        let uv = Rect::from_min_max(Pos2::ZERO, pos2(img.width() * ppp / tex_size.x, img.height() * ppp / tex_size.y));
        painter.add(RectShape::filled(img, CornerRadius::same(12), Color32::WHITE).with_texture(id, uv));
    }
}

fn peek_popup_texture(app: &mut CrossbowApp) -> Option<(egui::TextureId, egui::Vec2, Rect)> {
    let rs = app.render_state.clone();
    let peek = app.ui.peek.as_mut()?;
    let entry = peek.popup_texture;
    let out = peek.view.with_popup(|popup| {
        let (tex, (x, y, w, h)) = popup?;
        let id = match entry {
            Some((id, generation)) if generation == tex.generation => id,
            Some((id, _)) => {
                rs.renderer.write().update_egui_texture_from_wgpu_texture(&rs.device, &tex.view, wgpu::FilterMode::Linear, id);
                id
            }
            None => rs.renderer.write().register_native_texture(&rs.device, &tex.view, wgpu::FilterMode::Linear),
        };
        Some((id, tex.generation, vec2(tex.width as f32, tex.height as f32), Rect::from_min_size(pos2(x as f32, y as f32), vec2(w as f32, h as f32))))
    })?;
    peek.popup_texture = Some((out.0, out.1));
    Some((out.0, out.2, out.3))
}

#[cfg(test)]
mod peek_paint_tests {
    use super::*;

    #[test]
    fn toolbar_loading_and_settled_actions_work_at_both_sizes_and_disable_while_closing() {
        for size in [vec2(640.0, 420.0), vec2(1180.0, 780.0)] {
            for loading in [false, true] {
                for enabled in [false, true] {
                    for index in 0..3 {
                        let ctx = egui::Context::default();
                        crate::ui::window::install_fonts(&ctx);
                        let screen = Rect::from_min_size(Pos2::ZERO, size);
                        let card = Rect::from_center_size(screen.center() + vec2(0.0, 12.0), (size * vec2(0.78, 0.84)).round());
                        let target = pos2(card.right() - 112.0 + index as f32 * 44.0, card.top() - 24.0);
                        let nav = crossbow_engine::NavState { loading, url: "https://example.test/page".into(), ..Default::default() };
                        let mut actions = Vec::new();
                        for frame in 0..7 {
                            let mut events = vec![egui::Event::PointerMoved(target)];
                            if frame == 4 || frame == 5 {
                                events.push(egui::Event::PointerButton { pos: target, button: egui::PointerButton::Primary, pressed: frame == 4, modifiers: Default::default() });
                            }
                            ctx.run_ui(egui::RawInput { screen_rect: Some(screen), events, ..Default::default() }, |ui| {
                                show_peek_surface(ui.ctx(), 1.0, true, |ui| {
                                    if let Some(action) = peek_toolbar(ui, card, &nav, enabled, 1.0) {
                                        actions.push(action);
                                    }
                                });
                            }).textures_delta.clear();
                        }
                        assert_eq!(actions.len(), usize::from(enabled), "{size:?}, loading={loading}, enabled={enabled}, control={index}");
                        if let Some(action) = actions.first() {
                            assert!(match index {
                                0 if loading => matches!(action, Action::Stop),
                                0 => matches!(action, Action::Reload),
                                1 => matches!(action, Action::PeekToTab),
                                _ => matches!(action, Action::ClosePeek),
                            });
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn toolbar_origin_and_controls_share_a_row_without_long_host_overlap() {
        for size in [vec2(640.0, 420.0), vec2(1180.0, 780.0)] {
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let screen = Rect::from_min_size(Pos2::ZERO, size);
            let card = Rect::from_center_size(screen.center() + vec2(0.0, 12.0), (size * vec2(0.78, 0.84)).round());
            let nav = crossbow_engine::NavState { url: format!("https://{}.example.test/page", "long-origin-".repeat(14)), ..Default::default() };
            let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| {
                peek_toolbar(ui, card, &nav, true, 1.0);
            });
            let blocks = output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect) if rect.fill == Color32::from_black_alpha(120) => Some(rect.rect),
                _ => None,
            }).collect::<Vec<_>>();
            assert_eq!(blocks.len(), 2);
            let (bar, host) = (blocks[0], blocks[1]);
            assert_eq!(host.top(), bar.top());
            assert_eq!(host.height(), bar.height());
            assert!(host.right() <= bar.left() - 17.0, "long origin must not cover controls");
            assert!(screen.contains_rect(bar) && screen.contains_rect(host));
            output.textures_delta.clear();
        }
    }

    #[test]
    fn loading_progress_follows_peek_rounding_at_every_width_and_scale() {
        for width in [499.0, 920.0] {
            for scale in [1.0, 1.5, 2.0] {
                for progress in [0.08, 0.5, 1.0] {
                    let ctx = egui::Context::default();
                    ctx.set_pixels_per_point(scale);
                    let card = Rect::from_min_size(pos2(70.0, 46.0), vec2(width, 350.0));
                    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                        content::paint_progress(ui.painter(), card, 12, progress, 1.0, Color32::RED);
                    });
                    let (clip, shape) = output.shapes.iter().find_map(|shape| match &shape.shape {
                        egui::Shape::Rect(rect) if rect.fill == Color32::RED => Some((shape.clip_rect, rect)),
                        _ => None,
                    }).expect("Peek must show its loading progress");
                    assert_eq!(shape.rect, card);
                    assert_eq!(shape.corner_radius, CornerRadius::same(12));
                    assert!(clip.left() >= card.left() && clip.right() <= card.right());
                    assert!(clip.bottom() <= card.top() + 2.5);
                    output.textures_delta.clear();
                }
            }
        }
    }

    #[test]
    fn popup_is_painted_above_peek_in_view_points_at_both_scales() {
        for screen_size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for ppp in [1.0, 2.0] {
                let ctx = egui::Context::default();
                let screen = Rect::from_min_size(Pos2::ZERO, screen_size);
                let card = Rect::from_center_size(screen.center(), screen_size * vec2(0.78, 0.84));
                let popup_rect = Rect::from_min_size(pos2(36.0, 82.0), vec2(180.0, 96.0));
                let main = egui::TextureId::User(90);
                let popup = egui::TextureId::User(91);
                let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| {
                    paint_peek_page(ui.painter(), card, Some((main, card.size() * ppp)), ppp);
                    content::popup_surface(ui, "peek", card.min, (popup, popup_rect.size() * ppp, popup_rect), ppp);
                });
                let meshes = ctx.tessellate(std::mem::take(&mut output.shapes), ppp).into_iter().filter_map(|shape| {
                    match shape.primitive { egui::epaint::Primitive::Mesh(mesh) if mesh.texture_id == main || mesh.texture_id == popup => Some(mesh), _ => None }
                }).collect::<Vec<_>>();
                output.textures_delta.clear();
                assert_eq!(meshes.iter().map(|mesh| mesh.texture_id).collect::<Vec<_>>(), [main, popup]);
                let popup_bounds = meshes[1].vertices.iter().fold(Rect::NOTHING, |rect, vertex| rect.union(Rect::from_min_max(vertex.pos, vertex.pos)));
                assert_eq!(popup_bounds, popup_rect.translate(card.min.to_vec2()));
                output.textures_delta.clear();
            }
        }
    }
}

fn peek_texture(app: &mut CrossbowApp) -> Option<(egui::TextureId, egui::Vec2)> {
    let rs = app.render_state.clone();
    let peek = app.ui.peek.as_mut()?;
    let entry = peek.texture;
    let out = peek.view.with_texture(|tex| {
        let tex = tex?;
        let id = match entry {
            Some((id, generation)) if generation == tex.generation => id,
            Some((id, _)) => {
                rs.renderer.write().update_egui_texture_from_wgpu_texture(&rs.device, &tex.view, wgpu::FilterMode::Linear, id);
                id
            }
            None => rs.renderer.write().register_native_texture(&rs.device, &tex.view, wgpu::FilterMode::Linear),
        };
        Some((id, tex.generation, vec2(tex.width as f32, tex.height as f32)))
    })?;
    peek.texture = Some((out.0, out.1));
    Some((out.0, out.2))
}

// -- Find on page ---------------------------------------------------------------------

fn show_find_surface(ctx: &egui::Context, rect: Rect, covered: bool, content: impl FnOnce(&mut Ui)) {
    if covered {
        ctx.memory_mut(|memory| memory.surrender_focus(Id::unique("find-field")));
        return;
    }
    egui::Area::new(Id::unique("find-bar")).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, content);
}

/// What the Find bar asks of the page, and of Ask on Page, this frame.
#[derive(Debug, PartialEq)]
enum FindRequest {
    Find { query: String, forward: bool, next: bool },
    Stop,
    Close,
    /// Turn Ask mode on or off.
    Ask(bool),
    Send,
    Cancel,
    Dismiss,
}

const FIND_WIDTH: f32 = 330.0;
/// Adds room for the Ask button; the field and the Find controls keep their sizes.
const ASK_FIND_WIDTH: f32 = 356.0;

/// The Find bar's place at the top right of the page's pane.
fn find_rect(pane: Rect, available: bool) -> Rect {
    let width = if available { ASK_FIND_WIDTH } else { FIND_WIDTH };
    Rect::from_min_size(pos2(pane.right() - width - 10.0, pane.top() + 10.0), vec2(width, 40.0))
}

fn find_bar(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette) {
    if app.ui.find.is_none() {
        app.ask = None;
        return;
    }
    let ctx = ui.ctx().clone();
    let (Some(tab), Some(view)) = (app.focused_tab_id(), app.focused_view()) else {
        app.ui.find = None;
        app.ask = None;
        return;
    };
    let page = crate::ask_page::TabPage { tab: &tab, view: &view };
    crate::ask_page::reconcile(&mut app.ask, true, Some(crate::ask_page::Page::owner(&page)));
    // One store read when Find opens or Settings stops covering it, not every frame.
    let covered = app.ui.settings.is_some();
    let was_covered = ctx.data_mut(|data| std::mem::replace(data.get_temp_mut_or_default::<bool>(Id::unique("find-covered")), covered));
    let refresh = app.ui.find.as_ref().is_some_and(|find| find.just_opened) || (was_covered && !covered);
    let available = app.ask_available(view.profile_id(), refresh);
    if !available { app.ask = None; }
    let private = crate::ai_features::private_profile(view.profile_id());
    if let Some(ask) = &mut app.ask { ask.poll(&crate::ai_features::Local, &ctx, std::time::Instant::now()); }
    let pane = app.ui.panes.iter().find(|(id, _)| *id == tab).map(|(_, r)| *r).unwrap_or(ctx.content_rect());
    let rect = find_rect(pane, available);
    let count = view.nav().find;
    let requests = find_surface(&ctx, rect, pane, covered, app.ui.find.as_mut().unwrap(), app.ask.as_mut(), available, count, pal);
    for request in requests {
        match request {
            FindRequest::Find { query, forward, next } => view.find(&query, forward, next),
            FindRequest::Stop => view.stop_find(),
            FindRequest::Close => {
                app.ask = None;
                crate::app::dismiss_find(&ctx, &mut app.ui.find, &mut app.ui.focus, || view.dismiss_find());
            }
            FindRequest::Ask(true) => app.ask = Some(crate::ask_page::Ask::open(&page, &app.lib, private)),
            FindRequest::Ask(false) => app.ask = None,
            FindRequest::Send => {
                if let Some(ask) = &mut app.ask { ask.send(&page, &app.lib, private, std::time::Instant::now()); }
                ctx.request_repaint();
            }
            FindRequest::Cancel => if let Some(ask) = &mut app.ask { ask.cancel(); },
            FindRequest::Dismiss => if let Some(ask) = &mut app.ask { ask.dismiss(); },
        }
    }
}

/// The Find bar, and the Ask on Page card under it in Ask mode. The field
/// keeps one id in both modes, so switching never moves focus; each mode
/// edits its own text, so Find keeps its query while you ask.
#[allow(clippy::too_many_arguments)]
fn find_surface(ctx: &egui::Context, rect: Rect, pane: Rect, covered: bool, find: &mut crate::app::FindBar, mut ask: Option<&mut crate::ask_page::Ask>, available: bool, count: Option<(i32, i32)>, pal: &Palette) -> Vec<FindRequest> {
    let field_id = Id::unique("find-field");
    let focused = !covered && ctx.memory(|m| m.has_focus(field_id));
    let (enter, shift_enter, tab) = if focused {
        ctx.input_mut(|i| {
            // A plain-key match also takes Shift, so Shift+Return goes first.
            let shift_enter = i.consume_key(egui::Modifiers::SHIFT, Key::Enter);
            // Tab and Shift+Tab both switch between the two modes.
            (i.consume_key(egui::Modifiers::NONE, Key::Enter), shift_enter, available && i.consume_key(egui::Modifiers::NONE, Key::Tab))
        })
    } else {
        (false, false, false)
    };
    let asking = ask.is_some();
    let mut requests = Vec::new();
    if tab { requests.push(FindRequest::Ask(!asking)); }
    show_find_surface(ctx, rect, covered, |ui| {
        let p = ui.painter().clone();
        p.add(Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(50) }.as_shape(rect, CornerRadius::same(10)));
        p.rect_filled(rect, 10, pal.panel);
        p.rect_stroke(rect, 10, Stroke::new(1.0, pal.panel_border), egui::StrokeKind::Inside);
        let edit_right = rect.right() - if asking { 60.0 } else if available { 176.0 } else { 150.0 };
        let edit = Rect::from_min_max(pos2(rect.left() + 12.0, rect.top() + 10.0), pos2(edit_right, rect.bottom() - 10.0));
        let (text, hint) = match ask.as_deref_mut() {
            Some(ask) => (&mut ask.question, "Ask about this page"),
            None => (&mut find.query, "Find on page"),
        };
        let resp = ui.put(
            edit,
            egui::TextEdit::singleline(text)
                .id(field_id)
                .hint_text(egui::RichText::new(hint).color(pal.panel_dim))
                .text_color(pal.panel_text)
                .font(FontId::proportional(13.5))
                .frame(egui::Frame::NONE)
                // Keeps Tab for switching modes instead of moving focus.
                .lock_focus(available),
        );
        if find.just_opened {
            resp.request_focus();
            find.just_opened = false;
        }
        if asking {
            if enter { requests.push(FindRequest::Send); }
        } else {
            if find.query != find.last_sent {
                find.last_sent = find.query.clone();
                requests.push(if find.query.is_empty() { FindRequest::Stop } else { FindRequest::Find { query: find.query.clone(), forward: true, next: false } });
            }
            if (enter || shift_enter) && !find.query.is_empty() {
                requests.push(FindRequest::Find { query: find.query.clone(), forward: !shift_enter, next: true });
            }
            let count = match count {
                Some((n, i)) if !find.query.is_empty() => format!("{} of {n}", if n == 0 { 0 } else { i }),
                _ => String::new(),
            };
            let tools = rect.right() - if available { 26.0 } else { 0.0 };
            p.text(pos2(tools - 142.0, rect.center().y), Align2::LEFT_CENTER, count, FontId::proportional(12.0), pal.panel_dim);
            let mut x = tools - 82.0;
            for (icon, forward) in [(Icon::Back, false), (Icon::Forward, true)] {
                let r = Rect::from_center_size(pos2(x, rect.center().y), vec2(24.0, 24.0));
                if icons::button(ui, r, icon, pal.panel_text, pal.panel_hover, !find.query.is_empty()).clicked() {
                    requests.push(FindRequest::Find { query: find.query.clone(), forward, next: true });
                }
                x += 26.0;
            }
        }
        if available {
            let r = Rect::from_center_size(pos2(rect.right() - 46.0, rect.center().y), vec2(24.0, 24.0));
            let color = if asking { pal.accent } else { pal.panel_text };
            let hover = if asking { "Back to Find (Tab)" } else { "Ask on Page (Tab)" };
            if icons::button(ui, r, Icon::Sparkle, color, pal.panel_hover, true).on_hover_text(hover).clicked() {
                requests.push(FindRequest::Ask(!asking));
                // Typing continues in the field after a click.
                find.just_opened = true;
            }
        }
        let r = Rect::from_center_size(pos2(rect.right() - 20.0, rect.center().y), vec2(24.0, 24.0));
        if icons::button(ui, r, Icon::Close, pal.panel_text, pal.panel_hover, true).clicked() {
            requests.push(FindRequest::Close);
        }
    });
    if let Some(request) = ask.filter(|_| !covered).and_then(|ask| ask_card(ctx, rect, pane, ask, pal)) {
        // Typing continues in the field after Cancel or Dismiss.
        find.just_opened = true;
        requests.push(request);
    }
    requests
}

/// Under the Find bar in Ask mode: what a question would send and where,
/// progress with Cancel, the answer with its provider, or why nothing was sent.
fn ask_card(ctx: &egui::Context, bar: Rect, pane: Rect, ask: &crate::ask_page::Ask, pal: &Palette) -> Option<FindRequest> {
    use crate::ask_page::Phase;
    use crate::ui::style::{self, Button, Ink, Kind, Size};
    let top = bar.bottom() + 6.0;
    let height = (pane.bottom() - top - 10.0).max(96.0);
    let ink = Ink::new(pal.panel, pal.accent);
    let secondary = |text: String| style::text(text, 12.0, style::weight::REGULAR).color(ink.secondary);
    let mut request = None;
    egui::Area::new(Id::unique("ask-card")).order(egui::Order::Foreground).fixed_pos(pos2(bar.left(), top)).show(ctx, |ui| {
        let frame = panel_frame(pal).inner_margin(egui::Margin::same(12));
        frame.show(ui, |ui| {
            // The card lines up exactly with the bar's edges.
            ui.set_width(bar.width() - frame.total_margin().sum().x);
            // An Area keeps last frame's size, so a long answer needs its room set here.
            ui.set_max_height(height - frame.total_margin().sum().y);
            ui.spacing_mut().item_spacing.y = 6.0;
            match &ask.phase {
                Phase::Idle => match (&ask.blocked, &ask.destination) {
                    (Some(blocked), _) => { ui.label(secondary(blocked.clone())); }
                    (None, destination) => {
                        let to = destination.as_deref().unwrap_or("the saved provider");
                        ui.label(secondary(format!("Return sends your question and up to 12,000 bytes of this page's text to {to}. Nothing is saved.")));
                    }
                },
                Phase::Reading { .. } | Phase::Waiting { .. } => {
                    let status = match &ask.phase {
                        Phase::Waiting { ready, .. } => format!("Waiting for {}… (up to 45 seconds)", ready.provider()),
                        _ => "Reading this page…".to_string(),
                    };
                    ui.label(secondary(status));
                    if Button::new("Cancel").kind(Kind::Plain).size(Size::Small).show(ui, &ink).clicked() { request = Some(FindRequest::Cancel); }
                }
                Phase::Answer { text, from, truncated } => {
                    let (header, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
                    let close = Rect::from_center_size(pos2(header.right() - 10.0, header.center().y), vec2(20.0, 20.0));
                    let title = egui::UiBuilder::new().max_rect(header.with_max_x(close.left() - 6.0)).layout(egui::Layout::left_to_right(egui::Align::Center));
                    ui.scope_builder(title, |ui| ui.add(egui::Label::new(style::text(format!("Answer from {from}"), 12.0, style::weight::SEMIBOLD).color(ink.secondary)).truncate()));
                    if icons::button(ui, close, Icon::Close, pal.panel_text, pal.panel_hover, true).on_hover_text("Dismiss answer").clicked() {
                        request = Some(FindRequest::Dismiss);
                    }
                    egui::ScrollArea::vertical().id_salt("ask-answer").max_height((height - 92.0).max(48.0)).auto_shrink([false, true]).show(ui, |ui| {
                        // Plain text: provider output is untrusted and never rendered as markup.
                        ui.label(egui::RichText::new(text).size(13.0).color(pal.panel_text));
                    });
                    if *truncated { ui.label(secondary("Based on the first 12,000 bytes of this page's text.".into())); }
                }
                Phase::Notice(text) => { ui.label(egui::RichText::new(text).size(12.5).color(pal.panel_text)); }
            }
        });
    });
    request
}

#[cfg(test)]
mod find_surface_tests {
    use super::*;

    #[test]
    fn covered_find_preserves_its_query_and_returns_to_typing() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let ctx = egui::Context::default();
            let field = Id::unique("find-field");
            let mut query = String::new();
            let rect = Rect::from_min_size(pos2(280.0, 20.0), vec2(330.0, 40.0));
            for frame in 0..14 {
                let covered = (4..10).contains(&frame);
                let events = match frame {
                    3 => vec![egui::Event::Text("KEEP".into())],
                    4 => vec![egui::Event::Text("_FIRST_LEAK".into())],
                    7 => vec![egui::Event::Text("_SETTLED_LEAK".into())],
                    8 => vec![egui::Event::Ime(egui::ImeEvent::Commit("_IME_LEAK".into()))],
                    11 => vec![egui::Event::Text("_RETURN".into())],
                    _ => Vec::new(),
                };
                ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), events, ..Default::default() }, |ui| {
                    show_find_surface(ui.ctx(), rect, covered, |ui| {
                        if frame <= 2 || frame == 10 { ui.memory_mut(|memory| memory.request_focus(field)); }
                        ui.add(egui::TextEdit::singleline(&mut query).id(field));
                    });
                    if covered {
                        assert_eq!(query, "KEEP", "frame {frame} at {size:?}");
                        assert!(!ui.memory(|memory| memory.has_focus(field)));
                    }
                }).textures_delta.clear();
            }
            assert_eq!(query, "KEEP_RETURN", "{size:?}");
        }
    }

    use crate::ask_page::{Ask, Phase};
    use egui::Vec2;

    const SIZES: [Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
    const DESTINATION: &str = "Ollama (fixture-model) at http://127.0.0.1:41200";

    /// The Find bar on a pane beside the sidebar, with requests applied the
    /// way `find_bar` applies Ask mode; the rest are kept for the test.
    struct Bar {
        ctx: egui::Context,
        size: Vec2,
        available: bool,
        find: crate::app::FindBar,
        ask: Option<Ask>,
        count: Option<(i32, i32)>,
        frame: u32,
        requests: Vec<FindRequest>,
    }

    struct Drawn {
        text: Vec<(String, Rect, Rect)>,
    }

    impl Drawn {
        fn shows(&self, prefix: &str) -> bool { self.text.iter().any(|(text, ..)| text.starts_with(prefix)) }
        fn bounds(&self, text: &str) -> Rect { self.text.iter().find(|(shown, ..)| shown == text).unwrap_or_else(|| panic!("{text:?} is not drawn")).1 }
    }

    impl Bar {
        fn new(size: Vec2, available: bool) -> Self {
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let find = crate::app::FindBar { query: String::new(), last_sent: String::new(), just_opened: true };
            let mut bar = Self { ctx, size, available, find, ask: None, count: None, frame: 0, requests: Vec::new() };
            bar.settle();
            bar
        }

        fn pane(&self) -> Rect { Rect::from_min_max(pos2(248.0, 8.0), pos2(self.size.x - 8.0, self.size.y - 8.0)) }

        fn frame(&mut self, events: Vec<egui::Event>) -> Drawn {
            let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
            let (pane, available, count) = (self.pane(), self.available, self.count);
            let rect = find_rect(pane, available);
            let mut requests = Vec::new();
            self.frame += 1;
            let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)), time: Some(f64::from(self.frame) / 60.0), events, ..Default::default() };
            let mut output = self.ctx.run_ui(input, |ui| {
                requests = find_surface(ui.ctx(), rect, pane, false, &mut self.find, self.ask.as_mut(), available, count, &pal);
            });
            output.textures_delta.clear();
            for request in requests {
                match request {
                    FindRequest::Ask(true) => self.ask = Some(Ask::fixture(Phase::Idle, None, Some(DESTINATION.into()))),
                    FindRequest::Ask(false) => self.ask = None,
                    other => self.requests.push(other),
                }
            }
            let text = output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()), shape.clip_rect)),
                _ => None,
            }).collect();
            Drawn { text }
        }

        fn settle(&mut self) -> Drawn {
            for _ in 0..3 { self.frame(Vec::new()); }
            self.frame(Vec::new())
        }

        fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) -> Drawn {
            for pressed in [true, false] {
                self.frame(vec![egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }]);
            }
            self.settle()
        }

        fn type_text(&mut self, text: &str) -> Drawn {
            self.frame(vec![egui::Event::Text(text.into())]);
            self.settle()
        }

        fn click(&mut self, pos: Pos2) -> Drawn {
            self.frame(vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }]);
            }
            self.settle()
        }

        fn focused(&self) -> bool { self.ctx.memory(|memory| memory.has_focus(Id::unique("find-field"))) }

        fn take(&mut self) -> Vec<FindRequest> { std::mem::take(&mut self.requests) }

        /// The card under the bar, when Ask mode shows one.
        fn card(&self) -> Option<Rect> {
            self.ask.as_ref()?;
            self.ctx.memory(|memory| memory.area_rect(Id::unique("ask-card")))
        }

        /// Every label stays whole inside its clip, the screen and the pane,
        /// and clear of the others; the card lines up with the bar.
        fn check_layout(&mut self, drawn: &Drawn) {
            let screen = Rect::from_min_size(Pos2::ZERO, self.size);
            let field = [self.find.query.as_str(), self.ask.as_ref().map_or("", |ask| ask.question.as_str())];
            let answer = match self.ask.as_ref().map(|ask| &ask.phase) { Some(Phase::Answer { text, .. }) => text.as_str(), _ => "" };
            let labels: Vec<_> = drawn.text.iter().filter(|(text, ..)| !text.is_empty() && !field.contains(&text.as_str()) && text != answer).collect();
            // A long answer scrolls: it wraps to the card's width in a viewport that keeps real room.
            if let Some((_, bounds, clip)) = drawn.text.iter().find(|(text, ..)| !answer.is_empty() && text == answer) {
                let card = self.card().unwrap();
                let viewport = clip.intersect(card);
                assert!(bounds.left() >= viewport.left() && bounds.right() <= viewport.right(), "the answer fits the card's width at {:?}: {bounds:?} in {card:?}", self.size);
                assert!(viewport.top() <= bounds.top() + 0.5 && viewport.height() >= bounds.height().min(200.0), "the answer viewport {viewport:?} shows {bounds:?} at {:?}", self.size);
            }
            for (text, bounds, clip) in &labels {
                assert!(clip.expand(0.5).contains_rect(*bounds) && screen.contains_rect(*bounds) && self.pane().contains_rect(*bounds), "{text:?} is clipped at {:?}: {bounds:?} in {clip:?}", self.size);
            }
            for (i, (text, bounds, _)) in labels.iter().enumerate() {
                for (other, other_bounds, _) in &labels[i + 1..] {
                    assert!(!bounds.shrink(0.5).intersects(other_bounds.shrink(0.5)), "{text:?} overlaps {other:?} at {:?}", self.size);
                }
            }
            if let Some(card) = self.card() {
                let bar = find_rect(self.pane(), self.available);
                assert!((card.left() - bar.left()).abs() < 0.01 && (card.width() - bar.width()).abs() < 0.01, "the card {card:?} lines up with the bar {bar:?} at {:?}", self.size);
                assert!(card.top() > bar.bottom() && self.pane().contains_rect(card), "the card {card:?} stays in the pane at {:?}", self.size);
            }
        }
    }

    fn ready() -> crate::ai_features::Ready { crate::ai_features::tests::fixture_ready(41_200) }

    #[test]
    fn tab_switches_between_find_and_ask_and_each_keeps_its_text_and_the_focus() {
        for size in SIZES {
            let mut bar = Bar::new(size, true);
            assert!(bar.focused(), "Find opens focused at {size:?}");
            bar.type_text("needle");
            assert_eq!(bar.take(), [FindRequest::Find { query: "needle".into(), forward: true, next: false }], "typing finds as before at {size:?}");
            bar.key(egui::Key::Enter, egui::Modifiers::NONE);
            bar.key(egui::Key::Enter, egui::Modifiers::SHIFT);
            assert_eq!(bar.take(), [
                FindRequest::Find { query: "needle".into(), forward: true, next: true },
                FindRequest::Find { query: "needle".into(), forward: false, next: true },
            ]);

            let drawn = bar.key(egui::Key::Tab, egui::Modifiers::NONE);
            assert!(bar.ask.is_some() && bar.focused(), "Tab opens Ask mode and keeps focus at {size:?}");
            assert!(drawn.shows("Ask about this page") && drawn.shows(&format!("Return sends your question and up to 12,000 bytes of this page's text to {DESTINATION}.")));
            bar.check_layout(&drawn);
            bar.type_text("What is the price?");
            assert_eq!(bar.ask.as_ref().unwrap().question, "What is the price?");
            assert_eq!(bar.find.query, "needle", "the Find query is kept");
            assert!(bar.take().is_empty(), "a question never runs Find");
            bar.key(egui::Key::Enter, egui::Modifiers::NONE);
            assert_eq!(bar.take(), [FindRequest::Send]);

            // An error leaves Find working: Tab back, type, Find runs.
            bar.ask.as_mut().unwrap().phase = Phase::Notice("Crossbow could not read this page. Find still works.".into());
            let drawn = bar.settle();
            bar.check_layout(&drawn);
            bar.key(egui::Key::Tab, egui::Modifiers::NONE);
            assert!(bar.ask.is_none() && bar.focused(), "Tab returns to Find with focus at {size:?}");
            bar.type_text("s");
            assert_eq!(bar.take(), [FindRequest::Find { query: "needles".into(), forward: true, next: false }]);
        }
    }

    #[test]
    fn the_ask_button_and_escape_leave_find_and_its_query_in_place() {
        for size in SIZES {
            let mut bar = Bar::new(size, true);
            bar.type_text("needle");
            bar.take();
            let rect = find_rect(bar.pane(), true);
            let ask_button = pos2(rect.right() - 46.0, rect.center().y);
            bar.click(ask_button);
            assert!(bar.ask.is_some() && bar.focused(), "the Ask button opens Ask mode and typing continues at {size:?}");
            bar.type_text("Why?");
            // Escape: the field loses focus as usual, then the app leaves Ask mode first.
            bar.key(egui::Key::Escape, egui::Modifiers::NONE);
            assert!(crate::app::leave_ask(&mut bar.ask, Some(&mut bar.find)));
            bar.settle();
            assert!(bar.ask.is_none() && bar.focused() && bar.find.query == "needle", "Find stays open, focused, with its query at {size:?}");
            assert!(!crate::app::leave_ask(&mut bar.ask, Some(&mut bar.find)), "a second Escape is Find's");
            bar.click(ask_button);
            assert!(bar.ask.is_some());
            bar.click(ask_button);
            assert!(bar.ask.is_none() && bar.focused(), "the button turns Ask mode off too");
            bar.click(pos2(rect.right() - 20.0, rect.center().y));
            assert_eq!(bar.take(), [FindRequest::Close]);
        }
    }

    #[test]
    fn without_ask_on_page_the_bar_and_tab_behave_as_before() {
        for size in SIZES {
            let mut bar = Bar::new(size, false);
            bar.count = Some((5, 2));
            let drawn = bar.type_text("needle");
            let rect = find_rect(bar.pane(), false);
            assert_eq!(rect.width(), FIND_WIDTH);
            assert_eq!(drawn.bounds("2 of 5").left(), rect.right() - 142.0, "the count keeps its place at {size:?}");
            assert!(!drawn.shows("Ask about this page"));
            bar.check_layout(&drawn);
            bar.take();
            bar.key(egui::Key::Tab, egui::Modifiers::NONE);
            assert!(bar.ask.is_none() && bar.take().is_empty(), "Tab is not taken at {size:?}");
            assert!(!bar.focused(), "Tab moves focus on, as before, at {size:?}");

            // On, the field and the Find controls keep their sizes; Ask adds one button.
            let mut on = Bar::new(size, true);
            on.count = Some((5, 2));
            let drawn = on.type_text("needle");
            let wide = find_rect(on.pane(), true);
            assert_eq!((wide.right(), wide.width() - rect.width()), (rect.right(), 26.0));
            assert_eq!(drawn.bounds("2 of 5").left(), wide.right() - 26.0 - 142.0);
            on.check_layout(&drawn);
        }
    }

    #[test]
    fn ask_card_states_fit_and_offer_cancel_and_dismiss() {
        let fake = crate::ai_features::tests::Fake::with([crate::ai_features::tests::Script::Hold]);
        let ctx = egui::Context::default();
        let long = "The fixture page lists three plans. ".repeat(80);
        for size in SIZES {
            let blocked = "Ask on Page works on web pages (http and https) only. Find still works here.";
            let states = [
                (Phase::Idle, Some(blocked.to_string()), blocked.to_string()),
                (Phase::Reading { text: crossbow_engine::PageText::new(12_000), question: "Q".into(), ready: ready(), since: std::time::Instant::now() }, None, "Reading this page…".into()),
                (Phase::Waiting { task: crate::ai_features::Provider::start(&fake, &ready(), "Q".into(), &ctx).unwrap(), ready: ready(), truncated: false }, None, "Waiting for Ollama… (up to 45 seconds)".into()),
                (Phase::Answer { text: "Three plans: Free, Pro and Team.".into(), from: "Ollama (fixture-model)".into(), truncated: true }, None, "Answer from Ollama (fixture-model)".into()),
                (Phase::Answer { text: long.clone(), from: "Ollama (fixture-model)".into(), truncated: false }, None, "Answer from Ollama (fixture-model)".into()),
                (Phase::Notice("The page did not return its text in time. Find still works; press Enter to try again.".into()), None, "The page did not return its text in time.".into()),
            ];
            for (phase, blocked, expected) in states {
                fake.then(crate::ai_features::tests::Script::Hold);
                let mut bar = Bar::new(size, true);
                bar.key(egui::Key::Tab, egui::Modifiers::NONE);
                let ask = bar.ask.as_mut().unwrap();
                ask.phase = phase;
                ask.blocked = blocked;
                let drawn = bar.settle();
                assert!(drawn.shows(&expected), "{expected:?} is shown at {size:?}: {:?}", drawn.text.iter().map(|(text, ..)| text).collect::<Vec<_>>());
                bar.check_layout(&drawn);
                match &bar.ask.as_ref().unwrap().phase {
                    Phase::Reading { .. } | Phase::Waiting { .. } => {
                        let cancel = drawn.bounds("Cancel").center();
                        bar.click(cancel);
                        assert_eq!(bar.take(), [FindRequest::Cancel], "{expected} offers Cancel at {size:?}");
                    }
                    Phase::Answer { truncated, .. } => {
                        assert_eq!(drawn.shows("Based on the first 12,000 bytes of this page's text."), *truncated);
                        let header = drawn.bounds("Answer from Ollama (fixture-model)");
                        let card = bar.card().unwrap();
                        bar.click(pos2(card.right() - 12.0 - 10.0 - 1.0, header.center().y));
                        assert_eq!(bar.take(), [FindRequest::Dismiss], "the answer can be dismissed at {size:?}");
                    }
                    _ => assert!(bar.take().is_empty()),
                }
                assert!(bar.focused(), "the card never takes the field's focus at {size:?}");
            }
        }
    }
}

// -- Context menus ---------------------------------------------------------------------

pub(crate) struct MenuUi<'a> {
    pub(crate) pal: &'a Palette,
    pub(crate) width: f32,
    /// A menu opened from the keyboard focuses its first enabled entry.
    focus_first: std::cell::Cell<bool>,
}

impl<'a> MenuUi<'a> {
    pub(crate) fn new(pal: &'a Palette, focus_first: bool) -> Self {
        Self { pal, width: 236.0, focus_first: std::cell::Cell::new(focus_first) }
    }

    /// Hovered or keyboard-focused entries share the accent highlight.
    fn lit(&self, resp: &egui::Response, enabled: bool) -> bool {
        if enabled && self.focus_first.replace(false) {
            resp.request_focus();
        }
        if resp.gained_focus() {
            resp.scroll_to_me_animation(None, egui::style::ScrollAnimation::none());
        }
        enabled && (resp.hovered() || resp.has_focus())
    }

    pub(crate) fn item(&self, ui: &mut Ui, label: &str, shortcut: &str, enabled: bool) -> bool {
        self.item_at(ui, label, shortcut, enabled, None)
    }

    /// An entry optionally led by an icon at an indent, e.g. a nested folder.
    pub(crate) fn item_at(&self, ui: &mut Ui, label: &str, shortcut: &str, enabled: bool, lead: Option<(Icon, f32)>) -> bool {
        let (r, resp) = ui.allocate_exact_size(vec2(self.width, 26.0), if enabled { Sense::click() } else { Sense::hover() });
        let lit = self.lit(&resp, enabled);
        let p = ui.painter();
        if lit {
            p.rect_filled(r, 6, self.pal.accent.gamma_multiply(0.92));
        }
        let col = if !enabled {
            self.pal.panel_dim.gamma_multiply(0.6)
        } else if lit {
            Color32::WHITE
        } else {
            self.pal.panel_text
        };
        let mut text_x = r.left() + 10.0;
        if let Some((icon, indent)) = lead {
            icons::draw(p, Rect::from_center_size(pos2(text_x + indent + 6.0, r.center().y), vec2(12.0, 12.0)), icon, col);
            text_x += indent + 18.0;
        }
        p.with_clip_rect(r.shrink2(vec2(10.0, 0.0)).with_max_x(r.right() - 10.0 - shortcut.len() as f32 * 7.0))
            .text(pos2(text_x, r.center().y), Align2::LEFT_CENTER, label, FontId::proportional(13.0), col);
        if !shortcut.is_empty() {
            let sc = if lit { Color32::from_white_alpha(200) } else { self.pal.panel_dim };
            p.text(pos2(r.right() - 10.0, r.center().y), Align2::RIGHT_CENTER, shortcut, FontId::proportional(12.0), sc);
        }
        enabled && resp.clicked()
    }

    /// An item led by a Space icon, which may be a Lucide glyph rather than text.
    fn item_with_icon(&self, ui: &mut Ui, icon: &str, label: &str) -> bool {
        let (r, resp) = ui.allocate_exact_size(vec2(self.width, 26.0), Sense::click());
        let lit = self.lit(&resp, true);
        let p = ui.painter();
        if lit {
            p.rect_filled(r, 6, self.pal.accent.gamma_multiply(0.92));
        }
        let col = if lit { Color32::WHITE } else { self.pal.panel_text };
        crate::ui::space_icon::paint(p, Rect::from_center_size(pos2(r.left() + 18.0, r.center().y), vec2(16.0, 16.0)), icon, 13.0, col);
        p.with_clip_rect(r.shrink2(vec2(10.0, 0.0)))
            .text(pos2(r.left() + 32.0, r.center().y), Align2::LEFT_CENTER, label, FontId::proportional(13.0), col);
        resp.clicked()
    }

    pub(crate) fn sep(&self, ui: &mut Ui) {
        let (r, _) = ui.allocate_exact_size(vec2(self.width, 9.0), Sense::hover());
        ui.painter().line_segment([pos2(r.left() + 8.0, r.center().y), pos2(r.right() - 8.0, r.center().y)], Stroke::new(1.0, self.pal.panel_border));
    }

    /// Why the entry above is disabled, wrapped to the menu's width.
    pub(crate) fn note(&self, ui: &mut Ui, text: &str) {
        let galley = ui.painter().layout(text.to_owned(), FontId::proportional(11.5), self.pal.panel_dim, self.width - 20.0);
        let (r, _) = ui.allocate_exact_size(vec2(self.width, galley.size().y + 6.0), Sense::hover());
        ui.painter().galley(pos2(r.left() + 10.0, r.top() + 1.0), galley, self.pal.panel_dim);
    }

    pub(crate) fn heading(&self, ui: &mut Ui, text: &str) {
        let (r, _) = ui.allocate_exact_size(vec2(self.width, 22.0), Sense::hover());
        ui.painter().text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, text, FontId::proportional(11.5), self.pal.panel_dim);
    }
}

pub(crate) fn selection_search_engine(lib: &crossbow_core::library::Library, tab: &crate::tabs::Tab, space: &str, origin_profile: &str) -> Option<crossbow_core::search::Engine> {
    let owner = crate::app::tab_profile(lib, tab)?;
    if owner != origin_profile || crate::downloads::active_profile(lib, space)? != owner { return None; }
    crate::search_engines::profile_engine(lib, owner)
}

#[cfg(test)]
mod profile_search_tests {
    use super::*;
    use crossbow_core::{library::Library, model::{ProfilePreferences, SearchEngine, Theme}, store::{Store, TabRow}};

    #[test]
    fn selected_text_search_retains_menu_origin_after_unload_and_metadata_reassignment() {
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        let a = lib.create_profile("Origin A").unwrap();
        let b = lib.create_profile("Origin B").unwrap();
        let space = lib.create_space("Origin", "O", Theme::preset(0), &a).unwrap();
        let tab = crate::tabs::Tab::from_row(TabRow {
            id: "origin-tab".into(), space_id: space.clone(), item_id: None,
            url: "https://example.test/".into(), title: "Origin fixture".into(), order: "a0".into(), created_ms: 1, last_active_ms: 2,
        });
        let favorite = lib.add_favorite(&a, "https://example.test/favorite", "Origin favorite").unwrap();
        let favorite_tab = crate::tabs::Tab::from_row(TabRow {
            id: "origin-favorite".into(), space_id: crate::tabs::FAVORITES_SPACE.into(), item_id: Some(favorite.clone()),
            url: "https://example.test/favorite".into(), title: "Origin favorite".into(), order: "a0".into(), created_ms: 1, last_active_ms: 2,
        });
        lib.set_profile_preferences(&a, Some(ProfilePreferences { search_engine: Some(SearchEngine::Kagi), auto_archive_hours: None })).unwrap();
        lib.set_profile_preferences(&b, Some(ProfilePreferences { search_engine: Some(SearchEngine::Bing), auto_archive_hours: None })).unwrap();
        for tab in [&tab, &favorite_tab] {
            assert_eq!(selection_search_engine(&lib, tab, &space, &a), Some(SearchEngine::Kagi.into()));
        }
        lib.update_space(&space, |space| space.profile_id = b.clone()).unwrap();
        lib.update_item(&favorite, |item| item.profile_id = b.clone()).unwrap();
        for tab in [&tab, &favorite_tab] {
            assert!(tab.view.is_none(), "an unloaded source must still retain the menu's original owner");
            assert_eq!(selection_search_engine(&lib, tab, &space, &a), None, "old A text cannot use B's provider/context");
            assert_eq!(selection_search_engine(&lib, tab, &space, &b), Some(SearchEngine::Bing.into()));
            assert_eq!(selection_search_engine(&lib, tab, &space, "missing-profile"), None);
        }
    }

    #[test]
    fn selected_text_search_uses_its_owner_and_rejects_stale_profile_targets() {
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        let a = lib.create_profile("Selection A").unwrap();
        let b = lib.create_profile("Selection B").unwrap();
        let sa = lib.create_space("A", "A", Theme::preset(0), &a).unwrap();
        let sb = lib.create_space("B", "B", Theme::preset(0), &b).unwrap();
        let favorite = lib.add_favorite(&a, "https://example.test/", "Selection favorite").unwrap();
        let mut tab = crate::tabs::Tab::from_row(TabRow {
            id: "selection-tab".into(), space_id: sa.clone(), item_id: None,
            url: "https://example.test/".into(), title: "Selection fixture".into(), order: "a0".into(), created_ms: 1, last_active_ms: 2,
        });
        lib.set_profile_preferences(&a, Some(ProfilePreferences { search_engine: Some(SearchEngine::Kagi), auto_archive_hours: None })).unwrap();
        assert_eq!(selection_search_engine(&lib, &tab, &sa, &a), Some(SearchEngine::Kagi.into()));
        assert_eq!(selection_search_engine(&lib, &tab, &sb, &a), None, "never open A's selection in B");
        assert_eq!(selection_search_engine(&lib, &tab, "missing-space", &a), None);
        tab.space_id = crate::tabs::FAVORITES_SPACE.into();
        tab.item_id = Some(favorite);
        assert_eq!(selection_search_engine(&lib, &tab, &sa, &a), Some(SearchEngine::Kagi.into()));
        assert_eq!(selection_search_engine(&lib, &tab, &sb, &a), None);
        lib.profiles.remove(&a);
        assert_eq!(selection_search_engine(&lib, &tab, &sa, &a), None);
    }
}

/// Which entry for selected text was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionPick {
    Copy,
    Quote,
    Search,
}

/// Selected text outside a text field: Copy, Copy as Quote while `quote`
/// is on, and a search with the profile's engine.
pub(crate) fn selection_entries(m: &MenuUi, ui: &mut Ui, info: &crossbow_engine::ContextMenuInfo, quote: bool, engine: Option<&crossbow_core::search::Engine>) -> Option<SelectionPick> {
    let mut pick = None;
    if m.item(ui, "Copy", &crate::input::shortcut_label(true, false, false, false, "C"), true) {
        pick = Some(SelectionPick::Copy);
    }
    if crate::quote::offered(quote, info) && m.item(ui, crate::quote::LABEL, "", true) {
        pick = Some(SelectionPick::Quote);
    }
    let snippet: String = info.selection_text.chars().take(24).collect();
    let more = if info.selection_text.chars().count() > 24 { "…" } else { "" };
    let label = engine.map(|engine| format!("Search {} for “{snippet}{more}”", engine.name())).unwrap_or_else(|| "Search selected text".into());
    if m.item(ui, &label, "", engine.is_some()) {
        pick = Some(SelectionPick::Search);
    }
    pick
}

#[cfg(test)]
#[path = "selection_menu_tests.rs"]
mod selection_menu_tests;

/// Where context menus open: above everything, kept on screen.
pub(crate) fn menu_surface(pos: Pos2) -> egui::Area {
    egui::Area::new(Id::unique("context-menu")).order(egui::Order::Tooltip).fixed_pos(pos).constrain(true)
}

/// The context-menu card, shared by every menu.
pub(crate) fn menu_frame(pal: &Palette) -> egui::Frame {
    panel_frame(pal)
}

fn menu(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette) {
    let Some(state) = &app.ui.menu else { return };
    let ctx = ui.ctx().clone();
    let (pos, kind) = (state.pos, state.kind.clone());
    let m = MenuUi::new(pal, crate::ui::folders::take_menu_focus(&ctx));
    let mut done = false;
    let mut pick = None;
    let sc = |key: &str, shift: bool, alt: bool| crate::input::shortcut_label(true, shift, alt, false, key);
    let shortcuts = app.shortcuts.clone();
    let hint = |command: Command| shortcuts.hint(command);
    let area = menu_surface(pos).show(&ctx, |ui| {
        menu_frame(pal).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            match &kind {
                MenuKind::Web { tab, profile, info } => {
                    let view = app.tab(tab).and_then(|t| t.view.clone());
                    if !info.link_url.is_empty() {
                        if m.item(ui, "Open Link in New Tab", "", true) {
                            app.actions.push(Action::OpenLinkTab { url: info.link_url.clone(), background: true, here: false });
                            done = true;
                        }
                        // Its site opens in another profile: offer this Space too.
                        let moved = matches!(crate::app::site_open::site_route(&app.lib, &app.site_profiles, &app.tabs, &info.link_url, &app.current_profile()), crate::app::site_open::SiteRoute::Elsewhere { .. });
                        if moved && m.item(ui, "Open Link in New Tab Here", "", true) {
                            app.actions.push(Action::OpenLinkTab { url: info.link_url.clone(), background: true, here: true });
                            done = true;
                        }
                        if m.item(ui, "Open Link in Peek", "", true) {
                            app.actions.push(Action::OpenInPeek(info.link_url.clone()));
                            done = true;
                        }
                        if m.item(ui, "Open Link in Split View", "", true) {
                            app.actions.push(Action::OpenInSplit(info.link_url.clone()));
                            done = true;
                        }
                        if m.item(ui, "Copy Link", "", true) {
                            ctx.copy_text(info.link_url.clone());
                            app.actions.push(Action::Toast("Copied link".into()));
                            app.cue(crate::feel::Cue::LinkCopied);
                            done = true;
                        }
                        if m.item(ui, "Download Linked File", "", true) {
                            if let Some(v) = &view {
                                v.start_download(&info.link_url);
                            }
                            done = true;
                        }
                        m.sep(ui);
                    }
                    if !info.image_url.is_empty() {
                        if m.item(ui, "Open Image in New Tab", "", true) {
                            app.actions.push(Action::NewTab { url: info.image_url.clone(), background: true });
                            done = true;
                        }
                        if m.item(ui, "Save Image", "", true) {
                            if let Some(v) = &view {
                                v.start_download(&info.image_url);
                            }
                            done = true;
                        }
                        if m.item(ui, "Copy Image Address", "", true) {
                            ctx.copy_text(info.image_url.clone());
                            done = true;
                        }
                        m.sep(ui);
                    }
                    if info.is_editable {
                        for (label, key, f) in [
                            ("Cut", "X", 0),
                            ("Copy", "C", 1),
                            ("Paste", "V", 2),
                            ("Select All", "A", 3),
                        ] {
                            if m.item(ui, label, &sc(key, false, false), true) {
                                if let Some(v) = &view {
                                    match f {
                                        0 => v.cut(),
                                        1 => v.copy(),
                                        2 => v.paste(),
                                        _ => v.select_all(),
                                    }
                                }
                                done = true;
                            }
                        }
                        // Once an address field on the page had focus; the address
                        // menu replaces this one.
                        if crate::ui::logins::can_fill_address(app, tab) && m.item(ui, crate::ui::logins::FILL_ADDRESS, "›", true) {
                            match crate::ui::logins::freeze_address_menu(app, tab) {
                                Some(menu) => pick = Some(crate::ui::folders::Pick::Open(MenuKind::Addresses(menu))),
                                None => {
                                    app.actions.push(Action::Toast(crate::ui::logins::ADDRESS_MOVED.into()));
                                    done = true;
                                }
                            }
                        }
                        // Once a card field on an https page had focus; the card menu replaces this one.
                        if crate::ui::logins::can_fill_card(app, tab) && m.item(ui, crate::ui::logins::FILL_CARD, "›", true) {
                            match crate::ui::logins::freeze_card_menu(app, tab) {
                                Some(menu) => pick = Some(crate::ui::folders::Pick::Open(MenuKind::Cards(menu))),
                                None => {
                                    app.actions.push(Action::Toast(crate::ui::logins::CARD_MOVED.into()));
                                    done = true;
                                }
                            }
                        }
                        m.sep(ui);
                    } else if !info.selection_text.is_empty() {
                        let engine = app.tab(tab).and_then(|tab| selection_search_engine(&app.lib, tab, &app.active_space, profile));
                        let quote = app.quote.enabled(&app.lib.store);
                        match selection_entries(&m, ui, info, quote, engine.as_ref()) {
                            Some(SelectionPick::Copy) => {
                                if let Some(v) = &view {
                                    v.copy();
                                }
                                done = true;
                            }
                            Some(SelectionPick::Quote) => {
                                let nav = view.as_ref().map(|v| v.nav()).unwrap_or_default();
                                let (title, page_url) = crate::quote::source(info, &nav.url, &nav.title);
                                app.actions.push(Action::CopyAsQuote { text: info.selection_text.clone(), title, page_url });
                                done = true;
                            }
                            Some(SelectionPick::Search) => {
                                app.actions.push(Action::SearchSelection { tab: tab.clone(), profile: profile.clone(), text: info.selection_text.clone() });
                                done = true;
                            }
                            None => {}
                        }
                        m.sep(ui);
                    }
                    let nav = view.as_ref().map(|v| v.nav()).unwrap_or_default();
                    if m.item(ui, "Back", &hint(Command::Back), nav.can_go_back) {
                        app.actions.push(Action::Back);
                        done = true;
                    }
                    if m.item(ui, "Forward", &hint(Command::Forward), nav.can_go_forward) {
                        app.actions.push(Action::Forward);
                        done = true;
                    }
                    if m.item(ui, "Reload", &hint(Command::Reload), true) {
                        app.actions.push(Action::Reload);
                        done = true;
                    }
                    m.sep(ui);
                    if m.item(ui, "Copy Page URL", &hint(Command::CopyUrl), true) {
                        app.actions.push(Action::CopyUrl);
                        done = true;
                    }
                    if m.item(ui, "Print…", &hint(Command::Print), true) {
                        app.actions.push(Action::Print);
                        done = true;
                    }
                    if m.item(ui, "View Page Source", &hint(Command::ViewSource), true) {
                        app.actions.push(Action::ViewSource);
                        done = true;
                    }
                    if m.item(ui, "Inspect", &hint(Command::DevTools), true) {
                        app.actions.push(Action::DevTools);
                        done = true;
                    }
                }
                MenuKind::Tab(tab) => {
                    let Some(t) = app.tab(tab) else {
                        done = true;
                        return;
                    };
                    let muted = t.muted;
                    if m.item(ui, "Pin Tab", &hint(Command::PinTab), true) {
                        app.actions.push(Action::PinTab(tab.clone()));
                        done = true;
                    }
                    if let Some(chosen) = crate::ui::folders::tab_entries(&m, ui, &crate::ui::folders::MenuCx::of(app), tab) {
                        pick = Some(chosen);
                    }
                    if m.item(ui, "Add to Favorites", &hint(Command::Favorite), true) {
                        app.actions.push(Action::FavoriteTab(tab.clone()));
                        done = true;
                    }
                    if m.item(ui, "Open in Split View", "", true) {
                        app.actions.push(Action::SplitWith(tab.clone()));
                        done = true;
                    }
                    if m.item(ui, "Duplicate Tab", "", true) {
                        app.actions.push(Action::Duplicate(tab.clone()));
                        done = true;
                    }
                    if m.item(ui, "Copy URL", "", true) {
                        if let Some(t) = app.tab(tab) {
                            ctx.copy_text(t.url.clone());
                        }
                        app.actions.push(Action::Toast("Copied URL".into()));
                        done = true;
                    }
                    if m.item(ui, if muted { "Unmute Tab" } else { "Mute Tab" }, "", true) {
                        app.actions.push(Action::ToggleMute(tab.clone()));
                        done = true;
                    }
                    if app.tab_has_video(tab) && m.item(ui, PICTURE_IN_PICTURE, "", true) {
                        app.actions.push(Action::PictureInPicture(tab.clone()));
                        done = true;
                    }
                    // Once Translate Pages is on for the tab's profile, like Tidy Title.
                    let translate = app.tab(tab).and_then(|t| t.view.as_ref()).is_some_and(|view| crate::translate::enabled(&app.lib, view.profile_id()));
                    let translated = app.translator.translated(tab);
                    if translate && m.item(ui, if translated { SHOW_ORIGINAL } else { TRANSLATE_PAGE }, "", true) {
                        app.actions.push(Action::Translate(tab.clone(), if translated { Choice::ShowOriginal } else { Choice::Translate }));
                        done = true;
                    }
                    if m.item(ui, crate::app::capture_host::CAPTURE, &hint(Command::Capture), true) {
                        app.actions.push(Action::Capture(Some(tab.clone())));
                        done = true;
                    }
                    let spaces = app.lib.spaces_ordered();
                    if spaces.len() > 1 {
                        m.sep(ui);
                        m.heading(ui, "Move to Space");
                        for (sid, s) in spaces.iter().filter(|(sid, _)| *sid != app.active_space) {
                            if m.item_with_icon(ui, &s.icon, &s.name) {
                                app.actions.push(Action::MoveTabToSpace { tab: tab.clone(), space: sid.clone() });
                                done = true;
                            }
                        }
                    }
                    let sites = app.tab(tab).map(|t| crate::app::site_open::tab_site_entries(&app.lib, &app.site_profiles, t)).unwrap_or_default();
                    if !sites.is_empty() { m.sep(ui); }
                    for entry in sites {
                        let (label, action) = match entry {
                            crate::app::site_open::SiteEntry::Always { label, host, profile } => (label, Action::AlwaysOpenSite { host, profile }),
                            crate::app::site_open::SiteEntry::Stop { label, host } => (label, Action::StopSiteProfile(host)),
                        };
                        if m.item(ui, &label, "", true) {
                            app.actions.push(action);
                            done = true;
                        }
                    }
                    m.sep(ui);
                    if m.item(ui, "Close Tab", &hint(Command::CloseTab), true) {
                        app.actions.push(Action::CloseTab(tab.clone()));
                        done = true;
                    }
                }
                MenuKind::Space(space) => {
                    let Some(s) = app.lib.spaces.get(space).cloned() else {
                        done = true;
                        return;
                    };
                    if m.item(ui, "Rename Space…", "", true) {
                        app.actions.push(Action::SwitchSpace(space.clone()));
                        app.ui.renaming = Some((RenameTarget::Space(space.clone()), s.name.clone()));
                        done = true;
                    }
                    if m.item(ui, "Edit Theme…", "", true) {
                        app.actions.push(Action::SwitchSpace(space.clone()));
                        app.ui.settings_page_hint = Some(crate::ui::settings::Page::Spaces);
                        app.actions.push(Action::OpenSettings);
                        done = true;
                    }
                    if m.item(ui, "New Folder", "", true) {
                        app.actions.push(Action::SwitchSpace(space.clone()));
                        app.actions.push(Action::NewFolder);
                        done = true;
                    }
                    m.heading(ui, "Icon");
                    if let Some(icon) = crate::ui::space_icon::quick_picks(ui, m.pal, Id::unique(("space-menu-icons", space)), &s.icon, m.width) {
                        app.actions.push(Action::SetSpaceIcon(space.clone(), icon));
                        done = true;
                    }
                    if m.item(ui, "More Icons…", "", true) {
                        app.actions.push(Action::SwitchSpace(space.clone()));
                        app.ui.settings_page_hint = Some(crate::ui::settings::Page::Spaces);
                        app.actions.push(Action::OpenSettings);
                        done = true;
                    }
                    m.sep(ui);
                    if m.item(ui, "Delete Space", "", app.lib.spaces.len() > 1) {
                        app.actions.push(Action::DeleteSpace(space.clone()));
                        done = true;
                    }
                }
                MenuKind::Today(space) => {
                    let ready = app.tidy_tabs.can_tidy_today(&app.lib, &app.tabs, space);
                    if m.item(ui, "Tidy Today…", "", ready.is_ok()) {
                        app.actions.push(Action::TidyToday(space.clone()));
                        done = true;
                    }
                    if let Err(why) = &ready { m.note(ui, why); }
                    if app.tidy_tabs.undo.as_ref().is_some_and(|undo| undo.space == *space) && m.item(ui, "Undo Tidy Today", "", true) {
                        app.actions.push(Action::UndoTidyToday);
                        done = true;
                    }
                    m.sep(ui);
                    if m.item(ui, "Archive Today's Tabs", &hint(Command::ClearToday), !app.today_tabs(space).is_empty()) {
                        app.actions.push(Action::ClearToday);
                        done = true;
                    }
                }
                MenuKind::Logins { tab } => done = crate::ui::logins::menu(app, &m, ui, tab),
                MenuKind::Addresses(menu) => done = crate::ui::logins::address_menu(app, &m, ui, menu),
                MenuKind::Cards(menu) => done = crate::ui::logins::card_menu(app, &m, ui, menu),
                MenuKind::ContentBlocking { tab } => done = crate::ui::content_blocking_shield::menu(app, &m, ui, tab),
                // The sidebar's add, item, folder and folder-picker menus.
                kind => pick = crate::ui::folders::menu(&m, ui, &crate::ui::folders::MenuCx::of(app), kind),
            }
        });
    });
    if let Some(pick) = pick {
        done = crate::ui::folders::apply_pick(app, &ctx, pos, pick);
    }
    if done || pressed_outside(&ctx, pos, area.response.rect) {
        app.ui.menu = None;
    }
}

/// Whether this pass's press closes the menu opened at `pos` and shown at
/// `rect`: one outside it, unless the menu opened in this pass. When a
/// click's press and release arrive in one pass, as while the app is busy,
/// the press on the button that opened the menu is outside it. Called once
/// a pass while a menu is shown.
pub(crate) fn pressed_outside(ctx: &egui::Context, pos: Pos2, rect: Rect) -> bool {
    let (shown, pass) = (Id::unique("context-menu-shown"), ctx.cumulative_pass_nr());
    let last = ctx.data_mut(|data| {
        let last = data.get_temp::<(u64, Pos2)>(shown);
        data.insert_temp(shown, (pass, pos));
        last
    });
    let opened_now = last.is_none_or(|(last_pass, last_pos)| last_pass + 1 < pass || last_pos != pos);
    !opened_now && ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p)))
}

// -- Library: archive, history, downloads, captures ---------------------------------------

#[derive(Clone, Copy, PartialEq, Default)]
enum LibTab {
    #[default]
    Archive,
    History,
    Downloads,
    Captures,
}

/// The Library's tabs. Each is as wide as its name, so four fit beside Close.
const LIBRARY_TABS: [(LibTab, &str); 4] = [(LibTab::Archive, "Archive"), (LibTab::History, "History"), (LibTab::Downloads, "Downloads"), (LibTab::Captures, "Captures")];
/// Space on each side of a tab's name.
const LIBRARY_TAB_PAD: f32 = 9.0;

fn library(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette) {
    if !app.ui.show_archive {
        return;
    }
    let ctx = ui.ctx().clone();
    let screen = ctx.content_rect();
    let on_right = app.lib.settings.sidebar_on_right;
    let size = vec2(360.0, (screen.height() - 120.0).min(520.0));
    let pos = if on_right { pos2(screen.right() - size.x - 12.0, screen.bottom() - size.y - 52.0) } else { pos2(screen.left() + 12.0, screen.bottom() - size.y - 52.0) };
    let tab_id = Id::unique("library-tab");
    let mut tab: LibTab = ctx.data(|d| d.get_temp(tab_id)).unwrap_or_default();
    if std::mem::take(&mut app.capture.list.show) { tab = LibTab::Captures; }
    let mut close = false;
    let area = egui::Area::new(Id::unique("library")).order(egui::Order::Foreground).fixed_pos(pos).show(&ctx, |ui| {
        panel_frame(pal).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
            ui.set_width(size.x - 20.0);
            ui.set_height(size.y - 20.0);
            close = library_tabs(ui, &mut tab, pal);
            ui.add_space(6.0);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                match tab {
                    LibTab::Archive => {
                        let rows = crate::app::archive_for_space(&app.lib, &app.active_space, 300).unwrap_or_default();
                        if rows.is_empty() {
                            empty(ui, pal, "Closed and auto-archived tabs appear here.");
                        }
                        let mut last_day = String::new();
                        for a in rows {
                            let day = day_label(a.archived_ms);
                            if day != last_day {
                                ui.add_space(4.0);
                                ui.label(egui::RichText::new(&day).size(11.5).color(pal.panel_dim));
                                last_day = day;
                            }
                            let title = if a.title.is_empty() { crossbow_core::search::display_url(&a.url) } else { a.title.clone() };
                            if link_row(app, ui, pal, &a.url, &title) {
                                app.actions.push(Action::RestoreArchived(a.rowid));
                            }
                        }
                    }
                    LibTab::History => {
                        let profile = app.current_profile();
                        let mut rows = app.lib.store.recent_history(&profile, 300).unwrap_or_default();
                        if rows.is_empty() {
                            empty(ui, pal, "Pages visited in this profile appear here.");
                        } else if ui.add(egui::Button::new(egui::RichText::new("Clear History").size(12.0)).corner_radius(6)).clicked() {
                            match app.lib.clear_history(&profile) {
                                Ok(removed) => {
                                    rows.clear();
                                    if let Some(note) = crate::backups::removed_note(removed) {
                                        app.actions.push(Action::Toast(format!("History cleared. {note}")));
                                    }
                                }
                                Err(_) => app.actions.push(Action::Toast("Could not clear history. Try again.".into())),
                            }
                        }
                        for h in rows {
                            let title = if h.title.is_empty() { crossbow_core::search::display_url(&h.url) } else { h.title.clone() };
                            if link_row(app, ui, pal, &h.url, &title) {
                                app.actions.push(Action::NewTab { url: h.url.clone(), background: false });
                            }
                        }
                    }
                    LibTab::Downloads => {
                        app.actions.extend(library_downloads(ui, pal, &app.downloads, crate::downloads::active_profile(&app.lib, &app.active_space), &app.tidy));
                    }
                    LibTab::Captures => library_captures(app, ui, pal),
                }
            });
        });
    });
    ctx.data_mut(|d| d.insert_temp(tab_id, tab));
    // The capture viewer sits over the panel; its presses aren't outside it.
    let outside_press = app.capture.list.viewer.is_none() && ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p) && p.y < screen.bottom() - 46.0));
    if close {
        app.dismiss_library(&ctx, crate::app::LibraryDismissal::RestoreFocus);
    } else if outside_press {
        app.dismiss_library(&ctx, crate::app::LibraryDismissal::OutsidePointer);
    }
}

/// The Library's tab row and Close. True when Close was clicked.
fn library_tabs(ui: &mut Ui, tab: &mut LibTab, pal: &Palette) -> bool {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (t, label) in LIBRARY_TABS {
            let color = if *tab == t { pal.panel_text } else { pal.panel_dim };
            let galley = ui.painter().layout_no_wrap(label.to_owned(), FontId::proportional(12.5), color);
            let (r, resp) = ui.allocate_exact_size(vec2(galley.size().x + 2.0 * LIBRARY_TAB_PAD, 26.0), Sense::click());
            if *tab == t {
                ui.painter().rect_filled(r, 7, pal.panel_hover.gamma_multiply(2.0));
            }
            ui.painter().galley(r.center() - galley.size() / 2.0, galley, color);
            if resp.clicked() {
                *tab = t;
            }
        }
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
        let cr = Rect::from_center_size(pos2(r.right() - 12.0, r.center().y), vec2(22.0, 22.0));
        icons::button(ui, cr, Icon::Close, pal.panel_dim, pal.panel_hover, true).clicked()
    }).inner
}

#[cfg(test)]
mod library_tab_tests {
    use super::*;

    /// The panel is 360 points wide at every window size; its four tabs and
    /// Close share one row, each name whole and apart from the next.
    #[test]
    fn four_library_tabs_and_close_fit_one_row_and_captures_can_be_chosen() {
        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let mut tab = LibTab::Archive;
            let row = |events: Vec<egui::Event>, tab: &mut LibTab| {
                let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), events, ..Default::default() };
                let mut close = false;
                let mut output = ctx.run_ui(input, |ui| {
                    egui::Area::new(Id::unique("library-test")).fixed_pos(pos2(12.0, 40.0)).show(ui.ctx(), |ui| {
                        panel_frame(&pal).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                            ui.set_width(340.0);
                            close = library_tabs(ui, tab, &pal);
                        });
                    });
                });
                output.textures_delta.clear();
                let text: Vec<(String, Rect)> = output.shapes.iter().filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()))),
                    _ => None,
                }).collect();
                (text, close)
            };
            row(vec![], &mut tab);
            let (text, _) = row(vec![], &mut tab);
            let panel = Rect::from_min_size(pos2(22.0, 50.0), vec2(340.0, 26.0));
            let mut last = panel.min.x;
            for (_, label) in LIBRARY_TABS {
                let (_, bounds) = text.iter().find(|(text, _)| text == label).unwrap_or_else(|| panic!("{label:?} at {size:?}"));
                assert!(bounds.min.x >= last + LIBRARY_TAB_PAD - 0.5 && bounds.max.x <= panel.max.x - 24.0 - LIBRARY_TAB_PAD, "{label:?} {bounds:?} at {size:?}");
                last = bounds.max.x + LIBRARY_TAB_PAD;
            }
            let (_, captures) = text.iter().find(|(text, _)| text == "Captures").unwrap().clone();
            for pressed in [true, false] {
                row(vec![egui::Event::PointerMoved(captures.center()), egui::Event::PointerButton { pos: captures.center(), button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }], &mut tab);
            }
            assert!(tab == LibTab::Captures, "Captures chosen at {size:?}");
            let close = pos2(panel.max.x - 12.0, panel.center().y);
            let mut closed = false;
            for pressed in [true, false] {
                closed |= row(vec![egui::Event::PointerMoved(close), egui::Event::PointerButton { pos: close, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }], &mut tab).1;
            }
            assert!(closed, "Close still sits at the row's end at {size:?}");
        }
    }
}

/// The current profile's captures, newest first.
fn library_captures(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette) {
    let profile = app.current_profile();
    let data_dir = app.data_dir.clone();
    let rows = match app.capture.rows(ui.ctx(), &data_dir, &profile) {
        Ok(rows) => rows,
        Err(error) => {
            let damaged = error == crossbow_core::captures::CapturesError::Damaged;
            if crate::ui::capture::unreadable(ui, error.message(), damaged, pal) { app.capture.move_aside(&data_dir); }
            return;
        }
    };
    if rows.is_empty() {
        empty(ui, pal, crate::app::capture_host::EMPTY);
    }
    for row in rows {
        let Some(action) = crate::ui::capture::row(ui, &row, pal) else { continue };
        if let Some(path) = app.capture.list_action(&data_dir, &profile, action) {
            reveal(&path.to_string_lossy());
        }
    }
}

fn empty(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| ui.label(egui::RichText::new(text).size(12.5).color(pal.panel_dim)));
}

fn link_row(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette, url: &str, title: &str) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, 6, pal.panel_hover);
    }
    crate::ui::sidebar::favicon(app, ui, url, Rect::from_center_size(pos2(r.left() + 14.0, r.center().y), vec2(16.0, 16.0)), pal);
    let p = ui.painter().with_clip_rect(r.shrink2(vec2(30.0, 0.0)).with_min_x(r.left() + 30.0));
    p.text(pos2(r.left() + 30.0, r.center().y), Align2::LEFT_CENTER, title, FontId::proportional(13.0), pal.panel_text);
    resp.on_hover_text(url).clicked()
}

fn library_downloads(ui: &mut Ui, pal: &Palette, rows: &[crossbow_engine::DownloadInfo], profile: Option<&str>, tidy: &crate::tidy_downloads::Tidy) -> Vec<Action> {
    let mut rows = crate::downloads::for_profile(rows, profile).peekable();
    if rows.peek().is_none() {
        empty(ui, pal, "Downloads from this profile appear here during this session. Choose a folder in Settings → Downloads.");
    }
    rows.filter_map(|download| download_row(ui, pal, download, tidy.entry(&download.profile_id, download.id))).collect()
}

#[cfg(test)]
fn download_status(d: &crossbow_engine::DownloadInfo) -> String {
    download_status_with_control(d, crate::downloads::control_status(d))
}

fn download_status_with_control(d: &crossbow_engine::DownloadInfo, control: crossbow_engine::DownloadControlStatus) -> String {
    if let Some(error) = &d.destination_error { return error.clone(); }
    if let Some(pending) = control.pending {
        return if pending.overdue { format!("{} not confirmed. Retry or return to the original page.", pending.command.label()) }
            else { format!("{} requested…", pending.command.label()) };
    }
    if let Some(error) = d.failure() { return error.to_string(); }
    if d.canceled { return "Canceled · Retry from the original page".into(); }
    if d.interrupted { return "Interrupted · Retry from the original page".into(); }
    if d.complete {
        return if d.path.is_empty() { "Completed · File location unavailable".into() }
            else { format!("{} · Show in folder", human_size(d.received.max(d.total))) };
    }
    if d.control.is_some() && !control.available { return "Transfer controls unavailable. Retry from the original page.".into(); }
    if d.paused { return format!("Paused · {} received", human_size(d.received)); }
    if d.path.is_empty() { return "Preparing download…".into(); }
    if d.total > 0 { format!("{} of {}", human_size(d.received), human_size(d.total)) }
    else { human_size(d.received) }
}

fn download_has_controls(d: &crossbow_engine::DownloadInfo) -> bool {
    d.control.is_some() && !d.complete && !d.canceled && d.destination_error.is_none()
}

fn download_row_height(show_controls: bool) -> f32 {
    if show_controls { 74.0 } else { 42.0 }
}

/// The Tidy Downloads block under a row: up to two lines of status and, when
/// there is something to choose, one row of small buttons.
fn tidy_height(entry: Option<&crate::tidy_downloads::Entry>) -> f32 {
    match entry.map(|entry| tidy_buttons(&entry.state).is_empty()) {
        None => 0.0,
        Some(true) => 38.0,
        Some(false) => 68.0,
    }
}

fn tidy_buttons(state: &crate::tidy_downloads::State) -> &'static [(&'static str, crate::tidy_downloads::Command)] {
    use crate::tidy_downloads::{Command, State};
    match state {
        State::Suggesting(_) | State::Queued => &[("Cancel", Command::Cancel)],
        State::Offered(_) => &[("Accept", Command::Accept), ("Dismiss", Command::Dismiss)],
        State::Renamed { .. } => &[("Revert", Command::Revert)],
        State::Notice { retry: true, .. } => &[("Retry", Command::Retry)],
        State::Notice { .. } => &[],
    }
}

fn tidy_text(entry: &crate::tidy_downloads::Entry) -> String {
    use crate::tidy_downloads::State;
    match &entry.state {
        State::Suggesting(_) => "Suggesting a tidy name…".into(),
        State::Queued => "Waiting to suggest a tidy name…".into(),
        State::Offered(name) => format!("Tidy name: {name}"),
        State::Renamed { error: Some(error), .. } => error.clone(),
        State::Renamed { original, .. } => format!("Renamed from {}", original.file_name().map(|name| name.to_string_lossy()).unwrap_or_default()),
        State::Notice { text, .. } => text.clone(),
    }
}

fn tidy_block(ui: &mut Ui, pal: &Palette, rect: Rect, d: &crossbow_engine::DownloadInfo, entry: &crate::tidy_downloads::Entry) -> Option<Action> {
    use crate::ui::style::{Button, Ink, Kind, Size};
    let text = tidy_text(entry);
    let mut job = egui::text::LayoutJob::single_section(text.clone(), egui::TextFormat::simple(FontId::proportional(11.5), pal.panel_dim));
    job.wrap = egui::text::TextWrapping { max_width: rect.width(), max_rows: 2, break_anywhere: false, overflow_character: Some('…') };
    let galley = ui.painter().layout_job(job);
    let text_rect = Rect::from_min_size(rect.min + vec2(0.0, 3.0), vec2(rect.width(), 32.0));
    ui.painter().galley(text_rect.min, galley, pal.panel_dim);
    ui.interact(text_rect, ui.scope_id().with("tidy-text"), Sense::hover()).on_hover_text(&text);
    let ink = Ink::new(pal.panel, pal.accent);
    let mut selected = None;
    let buttons = tidy_buttons(&entry.state);
    if buttons.is_empty() { return None; }
    ui.scope_builder(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(rect.left(), rect.top() + 38.0), rect.max)), |ui| {
        ui.horizontal(|ui| {
            for (label, command) in buttons {
                let kind = if *command == crate::tidy_downloads::Command::Accept { Kind::Primary } else { Kind::Secondary };
                if Button::new(label).size(Size::Small).kind(kind).show(ui, &ink).on_hover_text(format!("{label}: {}", entry.file_name())).clicked() {
                    selected = Some(Action::TidyDownload { profile: d.profile_id.clone(), id: d.id, command: *command });
                }
            }
        });
    });
    selected
}

fn download_row(ui: &mut Ui, pal: &Palette, d: &crossbow_engine::DownloadInfo, tidy: Option<&crate::tidy_downloads::Entry>) -> Option<Action> {
    download_row_with_tidy(ui, pal, d, crate::downloads::control_status(d), download_has_controls(d), tidy)
}

#[cfg(test)]
fn download_row_with_control(ui: &mut Ui, pal: &Palette, d: &crossbow_engine::DownloadInfo, control: crossbow_engine::DownloadControlStatus, show_controls: bool) -> Option<Action> {
    download_row_with_tidy(ui, pal, d, control, show_controls, None)
}

fn download_row_with_tidy(ui: &mut Ui, pal: &Palette, d: &crossbow_engine::DownloadInfo, control: crossbow_engine::DownloadControlStatus, show_controls: bool, tidy: Option<&crate::tidy_downloads::Entry>) -> Option<Action> {
    let row_id = ui.make_persistent_id(("download-row", &d.profile_id, d.id));
    ui.scope_builder(egui::UiBuilder::new().scope_id(row_id), |ui| {
        // After a tidy rename the row shows, and reveals, the file's current name.
        let path = tidy.map(|entry| entry.path.to_string_lossy().into_owned()).unwrap_or_else(|| d.path.clone());
        let can_reveal = d.complete && d.failure().is_none() && !path.is_empty();
        let base = download_row_height(show_controls);
        let (full, _) = ui.allocate_exact_size(vec2(ui.available_width(), base + tidy_height(tidy)), Sense::hover());
        let r = Rect::from_min_size(full.min, vec2(full.width(), 42.0));
        let resp = ui.interact(r, ui.scope_id().with("download"), if can_reveal { Sense::click() } else { Sense::hover() });
        if resp.hovered() && can_reveal { ui.painter().rect_filled(r, 6, pal.panel_hover); }
        if resp.has_focus() {
            ui.painter().rect_stroke(r.shrink(1.0), 6, Stroke::new(2.0, pal.accent), egui::StrokeKind::Inside);
        }
        if resp.gained_focus() { resp.scroll_to_me_animation(None, egui::style::ScrollAnimation::none()); }
        let p = ui.painter();
        icons::draw(p, Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), vec2(14.0, 14.0)), Icon::Download, pal.panel_dim);
        let current = tidy.map(crate::tidy_downloads::Entry::file_name).filter(|name| !name.is_empty());
        let name = current.as_deref().unwrap_or(if d.file_name.is_empty() { "Download" } else { &d.file_name });
        let text_painter = p.with_clip_rect(r.with_max_x(r.right() - 8.0).intersect(ui.clip_rect()));
        text_painter.text(pos2(r.left() + 34.0, r.top() + 13.0), Align2::LEFT_CENTER, name, FontId::proportional(13.0), pal.panel_text);
        let status = download_status_with_control(d, control);
        text_painter.text(pos2(r.left() + 34.0, r.bottom() - 12.0), Align2::LEFT_CENTER, &status, FontId::proportional(11.5), pal.panel_dim);
        if d.active() && d.in_progress && !d.paused && d.percent >= 0 {
            let bar = Rect::from_min_size(pos2(r.left() + 34.0, r.bottom() - 4.0), vec2((r.width() - 44.0).max(0.0) * d.percent.clamp(0, 100) as f32 / 100.0, 2.0));
            p.rect_filled(bar, 1, pal.accent);
        }
        resp.widget_info(|| egui::WidgetInfo::labeled(if can_reveal { egui::Role::Button } else { egui::Role::Label }, ui.is_enabled(), format!("{name}: {status}")));
        if resp.on_hover_text(format!("{name}\n{status}")).clicked() && can_reveal { reveal(&path); }
        let tidied = tidy.and_then(|entry| {
            let block = Rect::from_min_max(pos2(full.left() + 34.0, full.top() + base), pos2(full.right() - 8.0, full.bottom()));
            tidy_block(ui, pal, block, d, entry)
        });
        if !show_controls { return tidied; }
        if control.pending.is_some_and(|pending| !pending.overdue) {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        use crossbow_engine::DownloadCommand;
        use crate::ui::style::{Button, Ink, Size};
        let command = control.pending.filter(|pending| pending.command != DownloadCommand::Cancel).map(|pending| pending.command)
            .unwrap_or(if d.paused || d.interrupted { DownloadCommand::Resume } else { DownloadCommand::Pause });
        let ink = Ink::new(pal.panel, pal.accent);
        let mut selected = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(full.left() + 34.0, r.bottom() + 3.0), full.max)), |ui| {
            ui.horizontal(|ui| {
                for action in [command, DownloadCommand::Cancel] {
                    let retry = control.pending.is_some_and(|pending| pending.overdue && pending.command == action);
                    let label = if retry { format!("Retry {}", action.label()) } else { action.label().to_string() };
                    if Button::new(&label).size(Size::Small).enabled(control.allows(action)).show(ui, &ink)
                        .on_hover_text(format!("{} {name}", action.label())).clicked() {
                        selected = Some(Action::DownloadControl { profile: d.profile_id.clone(), id: d.id, command: action });
                    }
                }
            });
        });
        selected
    }).inner
}

#[cfg(test)]
mod download_tests {
    use super::*;
    use crossbow_engine::DownloadInfo;
    use egui::Vec2;

    /// One frame of `rows` stacked as in the downloads card, at `width` inside `screen`.
    fn tidy_frame(ctx: &egui::Context, screen: Vec2, width: f32, rows: &[(DownloadInfo, crate::tidy_downloads::Entry)], frame: u32, events: Vec<egui::Event>) -> (Vec<(String, Rect, Rect)>, Vec<Rect>, Vec<Action>) {
        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, screen)), time: Some(f64::from(frame) / 30.0), events, ..Default::default() };
        let mut actions = Vec::new();
        let mut allocated = Vec::new();
        let mut output = ctx.run_ui(input, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 1.0;
            for (download, entry) in rows {
                let top = ui.cursor().top();
                if let Some(action) = download_row(ui, &pal, download, Some(entry)) { actions.push(action); }
                allocated.push(Rect::from_min_max(pos2(ui.min_rect().left(), top), pos2(ui.min_rect().left() + width, ui.cursor().top() - 1.0)));
            }
        });
        output.textures_delta.clear();
        let text = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()), shape.clip_rect)),
            _ => None,
        }).collect();
        (text, allocated, actions)
    }

    fn tidy_rows(states: Vec<crate::tidy_downloads::State>) -> Vec<(DownloadInfo, crate::tidy_downloads::Entry)> {
        states.into_iter().enumerate().map(|(index, state)| {
            let id = 40 + index as u32;
            let download = DownloadInfo { id, profile_id: "A".into(), path: format!("/synthetic/download-{id}.pdf"), file_name: format!("download-{id}.pdf"), complete: true, received: 10, total: 10, percent: 100, ..Default::default() };
            let entry = crate::tidy_downloads::Entry::fixture("A", id, &format!("/synthetic/Tidied {id}.pdf"), state);
            (download, entry)
        }).collect()
    }

    #[test]
    fn tidy_rows_fit_in_the_card_and_library_and_send_their_commands() {
        use crate::tidy_downloads::{Command, State};
        let fake = crate::ai_features::tests::Fake::with([crate::ai_features::tests::Script::Hold]);
        let ready = crate::ai_features::tests::fixture_ready(41_400);
        let long = "Quarterly revenue report for the northern region with appendices and a very long descriptive title.pdf";
        for screen in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for width in [284.0, 340.0] {
                let ctx = egui::Context::default();
                crate::ui::window::install_fonts(&ctx);
                let pending = crate::ai_features::Provider::start(&fake, &ready, "fixture".into(), &ctx).unwrap();
                fake.then(crate::ai_features::tests::Script::Hold);
                let states = vec![
                    State::Suggesting(pending),
                    State::Queued,
                    State::Offered("Quarterly report.pdf".into()),
                    State::Offered(long.into()),
                    State::Renamed { original: "/synthetic/download-44.pdf".into(), error: None },
                    State::Renamed { original: "/synthetic/download-45.pdf".into(), error: Some("Another file now uses the original name. Nothing was changed.".into()) },
                    State::Notice { text: "The local AI server did not respond before the time limit. The name is unchanged.".into(), retry: true },
                    State::Notice { text: "Kept the original name.".into(), retry: false },
                ];
                let expected = [
                    ("Suggesting a tidy name…", &["Cancel"][..], Some(Command::Cancel)),
                    ("Waiting to suggest a tidy name…", &["Cancel"][..], Some(Command::Cancel)),
                    ("Tidy name: Quarterly report.pdf", &["Accept", "Dismiss"][..], Some(Command::Accept)),
                    ("Tidy name: Quarterly revenue", &["Accept", "Dismiss"][..], Some(Command::Dismiss)),
                    ("Renamed from download-44.pdf", &["Revert"][..], Some(Command::Revert)),
                    ("Another file now uses the original name.", &["Revert"][..], Some(Command::Revert)),
                    ("The local AI server did not respond", &["Retry"][..], Some(Command::Retry)),
                    ("Kept the original name.", &[][..], None),
                ];
                let all = tidy_rows(states);
                for (chunk, (rows, expected)) in all.chunks(3).zip(expected.chunks(3)).enumerate() {
                let ctx = egui::Context::default();
                crate::ui::window::install_fonts(&ctx);
                let mut frame = 0;
                let (mut text, mut allocated, _) = tidy_frame(&ctx, screen, width, &rows, frame, Vec::new());
                for _ in 0..3 {
                    frame += 1;
                    (text, allocated, _) = tidy_frame(&ctx, screen, width, &rows, frame, Vec::new());
                }
                // The card's height formula matches what the rows take.
                for ((download, entry), rect) in rows.iter().zip(&allocated) {
                    assert_eq!(rect.height(), download_row_height(download_has_controls(download)) + tidy_height(Some(entry)), "row {} at {width}", download.id);
                }
                let card = 20.0 + rows.iter().map(|(d, e)| download_row_height(download_has_controls(d)) + tidy_height(Some(e)) + 1.0).sum::<f32>();
                assert!(card + 52.0 <= screen.y, "three tidy rows fit the downloads card at {screen:?}");
                for (index, ((download, entry), (status, buttons, _))) in rows.iter().zip(expected).enumerate() {
                    let row = allocated[index];
                    let mine: Vec<_> = text.iter().filter(|(_, bounds, _)| row.expand(0.5).contains_rect(*bounds)).collect();
                    assert!(mine.iter().any(|(shown, ..)| shown == &entry.file_name()), "the row shows the current name at {width}");
                    let status_text = mine.iter().find(|(shown, ..)| shown.starts_with(status)).unwrap_or_else(|| panic!("{status:?} in row {} at {width}: {mine:?}", download.id));
                    assert!(status_text.1.height() <= 32.0, "{status:?} wraps to at most two lines at {width}");
                    for label in *buttons { assert!(mine.iter().any(|(shown, ..)| shown == label), "{label} in row {} at {width}", download.id); }
                    for (i, (shown, bounds, clip)) in mine.iter().enumerate() {
                        assert!(clip.expand(0.5).contains_rect(*bounds) && bounds.right() <= row.right() + 0.5, "{shown:?} is clipped at {width} in {screen:?}: {bounds:?} in {clip:?}, row {row:?}");
                        for (other, other_bounds, _) in &mine[i + 1..] {
                            assert!(!bounds.shrink(0.5).intersects(other_bounds.shrink(0.5)), "{shown:?} overlaps {other:?} at {width}");
                        }
                    }
                }
                if chunk == 1 {
                    // About 110 characters need four lines at this width; two are drawn and the rest elided.
                    let (_, bounds, _) = text.iter().find(|(shown, ..)| shown.starts_with("Tidy name: Quarterly revenue")).unwrap();
                    assert!((20.0..=32.0).contains(&bounds.height()), "a long suggestion stops at two lines at {width}: {bounds:?}");
                }

                // Pointer: each button sends its command with the row's profile and id.
                for (index, (_, _, command)) in expected.iter().enumerate() {
                    let Some(command) = command else { continue };
                    let label = match command { Command::Accept => "Accept", Command::Dismiss => "Dismiss", Command::Revert => "Revert", Command::Retry => "Retry", Command::Cancel => "Cancel" };
                    let row = allocated[index];
                    let target = text.iter().find(|(shown, bounds, _)| shown == label && row.contains_rect(*bounds)).unwrap().1.center();
                    let mut sent = Vec::new();
                    for (pressed, events) in [(None, vec![egui::Event::PointerMoved(target)]), (Some(true), vec![]), (Some(false), vec![])] {
                        let events = match pressed { Some(pressed) => vec![egui::Event::PointerButton { pos: target, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }], None => events };
                        frame += 1;
                        sent.extend(tidy_frame(&ctx, screen, width, &rows, frame, events).2);
                    }
                    let id = rows[index].0.id;
                    assert!(matches!(sent.as_slice(), [Action::TidyDownload { profile, id: sent_id, command: sent_command }] if profile == "A" && *sent_id == id && sent_command == command), "{label} in row {id} at {width}: {sent:?}");
                }

                // Keyboard: Tab reaches Accept, Return sends it.
                if chunk != 0 { continue; }
                let accept = text.iter().find(|(shown, bounds, _)| shown == "Accept" && allocated[2].contains_rect(*bounds)).unwrap().1;
                let mut sent = Vec::new();
                for _ in 0..12 {
                    for pressed in [true, false] {
                        frame += 1;
                        tidy_frame(&ctx, screen, width, &rows, frame, vec![egui::Event::Key { key: Key::Tab, physical_key: None, pressed, repeat: false, modifiers: Default::default() }]);
                    }
                    let focused = ctx.memory(|memory| memory.focused()).and_then(|id| ctx.read_response(id)).map(|response| response.rect);
                    if focused.is_some_and(|rect| rect.contains_rect(accept)) {
                        for pressed in [true, false] {
                            frame += 1;
                            sent.extend(tidy_frame(&ctx, screen, width, &rows, frame, vec![egui::Event::Key { key: Key::Enter, physical_key: None, pressed, repeat: false, modifiers: Default::default() }]).2);
                        }
                        break;
                    }
                }
                assert!(matches!(sent.as_slice(), [Action::TidyDownload { id: 42, command: Command::Accept, .. }]), "keyboard Accept at {width}: {sent:?}");
                }
            }
        }
    }

    #[test]
    fn downloads_status_distinguishes_failure_cancellation_preparation_and_completion() {
        let pending = DownloadInfo { file_name: "fixture.txt".into(), in_progress: true, percent: -1, ..Default::default() };
        assert_eq!(download_status(&pending), "Preparing download…");
        assert!(pending.active());
        let failed = DownloadInfo { destination_error: Some("Folder unavailable. Choose another folder.".into()), ..pending.clone() };
        assert_eq!(download_status(&failed), failed.destination_error.clone().unwrap());
        assert!(!failed.active());
        let canceled = DownloadInfo { canceled: true, ..pending.clone() };
        assert!(download_status(&canceled).starts_with("Canceled"));
        assert!(!canceled.active());
        let interrupted = DownloadInfo { interrupted: true, ..pending.clone() };
        assert!(download_status(&interrupted).starts_with("Interrupted"));
        assert!(!interrupted.active());
        let complete = DownloadInfo { complete: true, received: 100, ..pending };
        assert!(!download_status(&complete).contains("Show in folder"));
        assert!(!complete.active());
        assert!(download_status(&DownloadInfo { path: "/synthetic/fixture.txt".into(), ..complete }).contains("Show in folder"));
    }

    #[test]
    fn downloads_status_follows_resumed_and_canceled_engine_updates() {
        let current = DownloadInfo { id: 1, profile_id: "A".into(), path: "/synthetic/fixture.txt".into(), file_name: "fixture.txt".into(), received: 50, total: 100, percent: 50, in_progress: true, ..Default::default() };
        let interrupted = DownloadInfo { interrupted: true, in_progress: false, error: Some("Network interruption".into()), ..current.clone() };
        let mut rows = vec![interrupted.clone()];
        crate::downloads::update(&mut rows, current.clone());
        assert_eq!(download_status(&rows[0]), format!("{} of {}", human_size(50), human_size(100)));
        assert!(rows[0].active() && rows[0].in_progress && rows[0].percent >= 0);
        crate::downloads::update(&mut rows, interrupted);
        crate::downloads::update(&mut rows, DownloadInfo { canceled: true, in_progress: false, ..current });
        assert!(download_status(&rows[0]).starts_with("Canceled"));
        assert!(!rows[0].active());
    }

    #[test]
    fn download_control_status_never_confuses_requested_with_confirmed() {
        use crossbow_engine::{DownloadCommand, DownloadControlStatus, DownloadPending};
        let d = DownloadInfo { path: "/synthetic/fixture.txt".into(), in_progress: true, received: 50, total: 100, ..Default::default() };
        for command in [DownloadCommand::Pause, DownloadCommand::Resume, DownloadCommand::Cancel] {
            let waiting = DownloadControlStatus { available: true, pending: Some(DownloadPending { command, overdue: false }), ..Default::default() };
            assert_eq!(download_status_with_control(&d, waiting), format!("{} requested…", command.label()));
            let overdue = DownloadControlStatus { pending: Some(DownloadPending { command, overdue: true }), ..waiting };
            assert!(download_status_with_control(&d, overdue).contains("not confirmed"));
        }
        let paused = DownloadInfo { paused: true, ..d };
        assert!(download_status(&paused).starts_with("Paused"));
        assert!(paused.active(), "a paused transfer must remain available in the progress card");
    }

    #[test]
    fn download_controls_pointer_and_keyboard_actions_keep_profile_and_id() {
        use crossbow_engine::{DownloadCommand, DownloadControlStatus};
        for width in [284.0, 340.0] {
            for scale in [1.0, 2.0] {
                for (paused, command, label) in [(false, DownloadCommand::Pause, "Pause"), (true, DownloadCommand::Resume, "Resume")] {
                    for keyboard in [false, true] {
                        let ctx = egui::Context::default();
                        crate::ui::window::install_fonts(&ctx);
                        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
                        let download = DownloadInfo { id: 47, profile_id: "A".into(), file_name: "fixture.txt".into(), paused, in_progress: true, ..Default::default() };
                        let status = DownloadControlStatus { pause: !paused, resume: paused, cancel: true, available: true, pending: None };
                        let mut target = Pos2::ZERO;
                        let mut actions = Vec::new();
                        for frame in 0..8 {
                            let mut input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width + 20.0, 420.0))), time: Some(frame as f64 / 30.0), ..Default::default() };
                            input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(scale);
                            if keyboard && (2..=5).contains(&frame) {
                                input.events.push(egui::Event::Key { key: if frame <= 3 { Key::Tab } else { Key::Enter }, physical_key: None, pressed: frame % 2 == 0, repeat: false, modifiers: Default::default() });
                            } else if !keyboard && (4..=5).contains(&frame) {
                                input.events.push(egui::Event::PointerMoved(target));
                                input.events.push(egui::Event::PointerButton { pos: target, button: egui::PointerButton::Primary, pressed: frame == 4, modifiers: Default::default() });
                            }
                            let mut output = ctx.run_ui(input, |ui| {
                                ui.set_width(width);
                                if let Some(action) = download_row_with_control(ui, &pal, &download, status, true) { actions.push(action); }
                            });
                            let labels: Vec<_> = output.shapes.iter().filter_map(|shape| match &shape.shape {
                                egui::Shape::Text(text) if [label, "Cancel"].contains(&text.galley.job.text.as_str()) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()))),
                                _ => None,
                            }).collect();
                            assert_eq!(labels.len(), 2);
                            assert_eq!(labels[0].1.center().y, labels[1].1.center().y, "buttons share one measured baseline");
                            assert!(labels[0].1.right() < labels[1].1.left());
                            assert!(labels[1].1.right() < width + 20.0);
                            if frame == 2 { target = labels.iter().find(|(text, _)| text == label).unwrap().1.center(); }
                            output.textures_delta.clear();
                        }
                        assert!(matches!(actions.as_slice(), [Action::DownloadControl { profile, id: 47, command: actual }] if profile == "A" && *actual == command), "keyboard={keyboard}, paused={paused}: {actions:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn download_controls_keep_keyboard_owner_when_new_rows_are_prepended() {
        use crossbow_engine::{DownloadCommand, DownloadControlStatus};
        for width in [284.0, 340.0] {
            for scale in [1.0, 2.0] {
                for new_complete in [false, true] {
                    let ctx = egui::Context::default();
                    crate::ui::window::install_fonts(&ctx);
                    let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
                    let original = DownloadInfo { id: 47, profile_id: "A".into(), file_name: "original.txt".into(), in_progress: true, paused: true, ..Default::default() };
                    let mut rows = vec![original.clone()];
                    let status = DownloadControlStatus { resume: true, cancel: true, available: true, ..Default::default() };
                    let mut actions = Vec::new();
                    let mut focused = None;
                    for frame in 0..10 {
                        if frame == 4 {
                            crate::downloads::update(&mut rows, DownloadInfo { id: 48, file_name: "new.txt".into(), complete: new_complete, ..original.clone() });
                        }
                        if frame == 6 { rows.remove(0); }
                        let mut input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width + 20.0, 420.0))), time: Some(frame as f64 / 30.0), ..Default::default() };
                        input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(scale);
                        if (2..=3).contains(&frame) || (8..=9).contains(&frame) {
                            input.events.push(egui::Event::Key { key: if frame < 4 { Key::Tab } else { Key::Enter }, physical_key: None, pressed: frame % 2 == 0, repeat: false, modifiers: Default::default() });
                        }
                        ctx.run_ui(input, |ui| {
                            ui.set_width(width);
                            for row in &rows {
                                if let Some(action) = download_row_with_control(ui, &pal, row, status, !row.complete) { actions.push(action); }
                            }
                        }).textures_delta.clear();
                        if frame == 3 {
                            focused = ctx.memory(|memory| memory.focused());
                            assert!(focused.is_some(), "Tab must focus the original Resume button");
                        }
                        if (4..=7).contains(&frame) {
                            assert_eq!(ctx.memory(|memory| memory.focused()), focused, "focus survives sibling insertion/removal: frame={frame}, new_complete={new_complete}");
                        }
                    }
                    assert!(matches!(actions.as_slice(), [Action::DownloadControl { profile, id: 47, command: DownloadCommand::Resume }] if profile == "A"), "{actions:?}");
                }
            }
        }
    }

    #[test]
    fn pending_download_controls_block_duplicate_keyboard_submission() {
        use crossbow_engine::{DownloadCommand, DownloadControlStatus, DownloadPending};
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
        let download = DownloadInfo { id: 47, profile_id: "A".into(), in_progress: true, ..Default::default() };
        let status = DownloadControlStatus { available: true, pending: Some(DownloadPending { command: DownloadCommand::Cancel, overdue: false }), ..Default::default() };
        let mut actions = Vec::new();
        for frame in 0..8 {
            let mut input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(304.0, 420.0))), time: Some(frame as f64 / 30.0), ..Default::default() };
            if (2..=5).contains(&frame) {
                input.events.push(egui::Event::Key { key: if frame <= 3 { Key::Tab } else { Key::Enter }, physical_key: None, pressed: frame % 2 == 0, repeat: false, modifiers: Default::default() });
            }
            ctx.run_ui(input, |ui| {
                ui.set_width(284.0);
                if let Some(action) = download_row_with_control(ui, &pal, &download, status, true) { actions.push(action); }
            }).textures_delta.clear();
        }
        assert!(actions.is_empty());
    }

    #[test]
    fn downloads_library_paints_only_the_selected_existing_profile() {
        let rows = [
            DownloadInfo { id: 1, profile_id: "A".into(), file_name: "PROFILE_A.txt".into(), error: Some("Synthetic folder unavailable".into()), ..Default::default() },
            DownloadInfo { id: 1, profile_id: "B".into(), file_name: "PROFILE_B.txt".into(), canceled: true, ..Default::default() },
        ];
        for width in [284.0, 340.0] {
            for scale in [1.0, 2.0] {
                let ctx = egui::Context::default();
                crate::ui::window::install_fonts(&ctx);
                let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
                for profile in [Some("A"), Some("B"), None, Some("missing")] {
                    let mut input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width + 20.0, 420.0))), ..Default::default() };
                    input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(scale);
                    let mut output = ctx.run_ui(input, |ui| {
                        ui.set_width(width);
                        library_downloads(ui, &pal, &rows, profile, &Default::default());
                    });
                    let text: Vec<_> = output.shapes.iter().filter_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                        _ => None,
                    }).collect();
                    for name in ["A", "B"] {
                        assert_eq!(text.iter().any(|text| *text == format!("PROFILE_{name}.txt")), profile == Some(name));
                    }
                    if profile == Some("A") { assert!(text.contains(&"Synthetic folder unavailable")); }
                    if profile.is_none() || profile == Some("missing") {
                        assert!(text.iter().any(|text| text.contains("Downloads from this profile")));
                    }
                    output.textures_delta.clear();
                }
            }
        }
    }
}

pub(crate) fn human_size(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{b:.0} B")
    }
}

fn day_label(ms: u64) -> String {
    let now = crossbow_core::now_ms();
    let days = now.saturating_sub(ms) / 86_400_000;
    match days {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        n if n < 7 => format!("{n} days ago"),
        n if n < 30 => format!("{} weeks ago", n / 7),
        n => format!("{} months ago", n / 30),
    }
}

/// Show a file in the platform file manager.
pub fn reveal(path: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(format!("/select,{path}")).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Some(dir) = std::path::Path::new(path).parent() {
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

/// Small card over the sidebar corner while downloads are running.
fn downloads_card(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette) {
    if !app.ui.show_downloads || app.ui.show_archive {
        return;
    }
    let ctx = ui.ctx().clone();
    let now = ctx.input(|i| i.time);
    let profile = crate::downloads::active_profile(&app.lib, &app.active_space);
    let shown: Vec<_> = crate::downloads::for_profile(&app.downloads, profile).take(3).collect();
    if shown.is_empty() { return; }
    let idle_id = Id::unique(("downloads-idle-since", profile));
    let active = crate::downloads::for_profile(&app.downloads, profile).any(|d| d.active() || crate::downloads::control_status(d).pending.is_some())
        // A tidy name being suggested or offered keeps the card open.
        || profile.is_some_and(|profile| app.tidy.waiting(profile));
    if !active {
        let since: f64 = ctx.data(|d| d.get_temp(idle_id)).unwrap_or(now);
        ctx.data_mut(|d| d.insert_temp(idle_id, since));
        if now - since > 4.0 {
            app.ui.show_downloads = false;
            ctx.data_mut(|d| d.remove::<f64>(idle_id));
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    } else {
        ctx.data_mut(|d| d.remove::<f64>(idle_id));
    }
    let screen = ctx.content_rect();
    let h = 20.0 + shown.iter().map(|d| download_row_height(download_has_controls(d)) + tidy_height(app.tidy.entry(&d.profile_id, d.id)) + 1.0).sum::<f32>();
    let x = if app.lib.settings.sidebar_on_right { screen.right() - 312.0 } else { screen.left() + 12.0 };
    egui::Area::new(Id::unique("downloads-card")).order(egui::Order::Foreground).fixed_pos(pos2(x, screen.bottom() - h - 52.0)).show(&ctx, |ui| {
        panel_frame(pal).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
            ui.set_width(284.0);
            ui.spacing_mut().item_spacing.y = 1.0;
            for d in &shown {
                if let Some(action) = download_row(ui, pal, d, app.tidy.entry(&d.profile_id, d.id)) { app.actions.push(action); }
            }
        });
    });
}

// -- Toasts --------------------------------------------------------------------------------

/// Toasts stack up from the bottom, or from above the site notice.
fn toasts(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette, below: Option<Rect>) {
    if app.ui.toasts.is_empty() {
        return;
    }
    let ctx = ui.ctx().clone();
    let now = ctx.input(|i| i.time);
    let screen = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::unique("toasts")));
    let mut y = below.map_or(screen.bottom() - 28.0, |notice| notice.top() - 8.0);
    for t in app.ui.toasts.iter().rev().take(3) {
        let age = (now - t.at) as f32;
        let a = (age / 0.15).min(1.0) * ((3.0 - age) / 0.4).clamp(0.0, 1.0);
        let galley = painter.layout_no_wrap(t.text.clone(), FontId::proportional(13.0), Color32::WHITE);
        let size = galley.size() + vec2(28.0, 16.0);
        let rise = (1.0 - anim::ease_out_cubic((age / 0.25).min(1.0))) * 12.0;
        let r = Rect::from_center_size(pos2(screen.center().x, y - size.y / 2.0 + rise), size);
        painter.add(Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha((60.0 * a) as u8) }.as_shape(r, CornerRadius::same(10)));
        let bg = if pal.dark { Color32::from_rgb(0x3a, 0x3a, 0x44) } else { Color32::from_rgb(0x22, 0x22, 0x28) };
        painter.rect_filled(r, 10, bg.gamma_multiply(a));
        painter.galley(r.min + vec2(14.0, 8.0), galley, Color32::WHITE.gamma_multiply(a));
        y -= size.y + 8.0;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(16));
}
