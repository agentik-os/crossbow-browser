//! Little Window: a small window holding one page, with a compact toolbar
//! (Back, Forward, Reload, the address, Open in Space…, Keep Window on Top
//! and Close) and the Space chooser. It runs in its own deferred viewport
//! and only touches the window's [`Surface`]; the root owns the page.

use crate::input;
use crate::ui::capture::{self as capture_ui, CardAction, CardView, Outcome, Selection};
use crate::ui::content::{self, GAP, WebInput as Target};
use crate::ui::icons::{self, Icon};
use crate::ui::style::{self, Button, Ink, Kind, Size};
use crate::ui::theme::{self, Palette};
use crate::ui::window::{ControlRow, NATIVE_CONTROL_ROW, ROW_CONTROL};
use crate::window_host::{Chooser, SpaceChoice, Surface, ViewInput, WindowCommand};
use egui::epaint::text::{LayoutJob, TextWrapping};
use egui::{Id, Key, Modifiers, Pos2, Rect, Sense, Ui, pos2, vec2};

pub const OPEN_IN_SPACE: &str = "Open in Space…";
pub const REOPEN: &str = "Reopen in Space";
pub const KEEP_ON_TOP: &str = "Keep Window on Top";
const ADDRESS_FIELD: &str = "little-address";
const ADDRESS_HINT: &str = "Search or enter address";
const CHOOSER_WIDTH: f32 = 300.0;

/// Where the toolbar's controls sit in a window `width` points wide.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Toolbar {
    /// The window's top row: the toolbar, beside the traffic lights.
    pub row: Rect,
    pub back: Rect,
    pub forward: Rect,
    pub reload: Rect,
    pub address: Rect,
    pub open: Rect,
    pub on_top: Rect,
    pub close: Rect,
}

/// The toolbar shares the traffic lights' row, as the main window's sidebar
/// toolbar does: the same line, inset, control size and pitch.
pub fn toolbar_layout(ctx: &egui::Context, window: Rect) -> Toolbar {
    let native = cfg!(target_os = "macos") && !ctx.input(|input| input.viewport().fullscreen.unwrap_or(false));
    let row = Rect::from_min_size(window.min, vec2(window.width(), NATIVE_CONTROL_ROW));
    let controls = ControlRow::new(window, native, NATIVE_CONTROL_ROW);
    let (back, forward, reload) = (controls.leading_control(0), controls.leading_control(1), controls.leading_control(2));
    let close = controls.trailing_control(0);
    let on_top = controls.trailing_control(1);
    // A small Button is as wide as its label and padding.
    let open_width = ctx.fonts_mut(|fonts| {
        let job = LayoutJob::single_section(OPEN_IN_SPACE.into(), style::format(12.0, style::weight::MEDIUM, egui::Color32::WHITE));
        fonts.layout_job(job).size().x
    }) + 22.0;
    let open = Rect::from_min_size(pos2(on_top.left() - GAP - open_width, close.top()), vec2(open_width, ROW_CONTROL));
    let address = Rect::from_min_max(pos2(reload.right() + GAP, close.top()), pos2((open.left() - GAP).max(reload.right() + GAP), close.bottom()));
    Toolbar { row, back, forward, reload, address, open, on_top, close }
}

/// The area the page fills, below the toolbar and any notice, framed as the
/// main window frames its content.
fn page_rect(ctx: &egui::Context, window: Rect, top: f32) -> Rect {
    let (edge, _) = content::card_frame(ctx);
    Rect::from_min_max(pos2(window.left() + edge, top), pos2(window.right() - edge, (window.bottom() - edge).max(top + 1.0)))
}

/// One pass of the window. Everything the root must act on goes into `surface`.
pub fn show(ui: &mut Ui, surface: &mut Surface) {
    let ctx = ui.ctx().clone();
    let window = ui.max_rect();
    let pal = surface.frame.palette.clone().unwrap_or_else(|| theme::palette(&crossbow_core::model::Theme::preset(0), ctx.theme() == egui::Theme::Dark));
    let ink = Ink::new(pal.stops.first().copied().unwrap_or(pal.panel), pal.accent);
    let focused = ctx.input(|input| input.focused);
    if ctx.input(|input| input.viewport().close_requested()) { surface.request_close(); }
    // Decided before any control takes or releases focus this pass.
    let page_keys = focused && surface.chrome.address.is_none() && surface.chrome.chooser.is_none() && surface.chrome.capture.is_none() && ctx.memory(|memory| memory.focused().is_none());
    if page_keys {
        // Tab and the arrows move within the page, not to the toolbar.
        ctx.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
    }
    shortcuts(&ctx, surface);

    theme::paint_gradient(ui.painter(), window, &pal.stops, pal.angle_deg, 1.0);
    if let Some(texture) = surface.frame.surface_texture {
        theme::paint_grain(ui.painter(), window, texture, pal.grain, ctx.pixels_per_point());
    }
    let layout = toolbar_layout(&ctx, window);
    title_bar(ui, layout.row);
    toolbar(ui, surface, &layout, &pal, &ink);
    let mut top = layout.row.bottom();
    if let Some(notice) = surface.frame.notice.clone() {
        let shown = style::paint_wrapped(ui.painter(), pos2(window.left() + GAP + 4.0, top), (window.width() - 2.0 * (GAP + 4.0)).max(40.0), false, &notice, 12.0, style::weight::REGULAR, ink.danger, 16.0);
        top = shown.bottom() + 6.0;
    }
    let page = page_rect(&ctx, window, top);
    let mut changed = surface.report_size(page.size(), ctx.pixels_per_point());
    page_area(ui, surface, page, focused, page_keys);
    if surface.chrome.chooser.is_some() {
        chooser(ui, surface, &layout, window, &ink);
    }
    capture_card(&ctx, surface, window, &pal);
    changed |= surface.report_focus(focused);
    if changed || surface.pending() {
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }
}

fn open_chooser(surface: &mut Surface) {
    surface.chrome.chooser = Some(Chooser { confirm: None, focus: true });
    surface.chrome.address = None;
}

fn edit_address(surface: &mut Surface) {
    let url = &surface.frame.page.url;
    surface.chrome.address = Some(if url == crate::app::NEW_TAB_URL { String::new() } else { url.clone() });
    surface.chrome.focus_address = true;
    surface.chrome.chooser = None;
}

/// The window's own shortcuts, taken before the page sees them.
fn shortcuts(ctx: &egui::Context, surface: &mut Surface) {
    let take = |mods: Modifiers, key: Key| crate::app::take_shortcut(ctx, mods, key);
    if take(Modifiers::COMMAND, Key::W) { surface.command(WindowCommand::Close); }
    if take(Modifiers::COMMAND, Key::O) { open_chooser(surface); }
    if take(Modifiers::COMMAND, Key::L) { edit_address(surface); }
    if take(Modifiers::COMMAND, Key::R) { surface.command(WindowCommand::Reload); }
    if take(Modifiers::COMMAND, Key::OpenBracket) { surface.command(WindowCommand::Back); }
    if take(Modifiers::COMMAND, Key::CloseBracket) { surface.command(WindowCommand::Forward); }
    // Escape steps back out of the chooser; the address field handles its own.
    if let Some(chooser) = &mut surface.chrome.chooser && !egui::Popup::is_any_open(ctx) && take(Modifiers::NONE, Key::Escape) {
        if chooser.confirm.take().is_some() {
            chooser.focus = true;
        } else {
            surface.chrome.chooser = None;
        }
    }
}

/// The row under the controls stands in for the hidden title bar: dragging
/// it moves the window and a double click zooms it, as in the main window.
fn title_bar(ui: &mut Ui, row: Rect) {
    let response = ui.interact(row, Id::unique("little-title-bar"), Sense::click_and_drag());
    if response.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if response.double_clicked() {
        let maximized = ui.ctx().input(|input| input.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }
}

fn toolbar(ui: &mut Ui, surface: &mut Surface, layout: &Toolbar, pal: &Palette, ink: &Ink) {
    let page = surface.frame.page.clone();
    let reload = if page.loading { (Icon::Stop, WindowCommand::Stop) } else { (Icon::Reload, WindowCommand::Reload) };
    for (rect, icon, command, enabled) in [
        (layout.back, Icon::Back, WindowCommand::Back, page.can_go_back),
        (layout.forward, Icon::Forward, WindowCommand::Forward, page.can_go_forward),
        (layout.reload, reload.0, reload.1, true),
        (layout.close, Icon::Close, WindowCommand::Close, true),
    ] {
        if icons::button(ui, rect, icon, pal.text_dim, pal.hover, enabled).clicked() {
            surface.command(command);
        }
    }
    if keep_on_top(ui, layout.on_top, surface.frame.on_top, pal).clicked() {
        surface.command(WindowCommand::KeepOnTop(!surface.frame.on_top));
    }
    if Button::new(OPEN_IN_SPACE).size(Size::Small).show_at(ui, ink, layout.open).clicked() {
        if surface.chrome.chooser.is_some() { surface.chrome.chooser = None } else { open_chooser(surface) }
    }
    if layout.address.width() >= 40.0 {
        address(ui, surface, layout.address, ink);
    }
}

/// Keep Window on Top: a pin, pressed in while the window floats.
fn keep_on_top(ui: &mut Ui, rect: Rect, on: bool, pal: &Palette) -> egui::Response {
    let response = ui.interact(rect, Id::unique("little-keep-on-top"), Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, KEEP_ON_TOP));
    let painter = ui.painter();
    if on || response.hovered() || response.is_pointer_button_down_on() {
        painter.rect_filled(rect, 6.0, pal.hover);
    }
    if response.has_focus() {
        painter.rect_stroke(rect.expand(2.0), 8.0, egui::Stroke::new(1.5, pal.text_dim), egui::StrokeKind::Outside);
    }
    let (icon, color) = if on { (Icon::Pin, pal.text) } else { (Icon::PinOff, pal.text_dim) };
    icons::draw(painter, Rect::from_center_size(rect.center(), egui::Vec2::splat(rect.height() * 0.6)), icon, color);
    if response.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    response.on_hover_text(KEEP_ON_TOP)
}

/// The page's address; a click (or ⌘L) turns it into a field.
fn address(ui: &mut Ui, surface: &mut Surface, rect: Rect, ink: &Ink) {
    let id = Id::unique(ADDRESS_FIELD);
    let ctx = ui.ctx().clone();
    let inner = rect.shrink2(vec2(10.0, 0.0));
    let Some(text) = surface.chrome.address.as_mut() else {
        let response = ui.interact(rect, id.with("shown"), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Edit address"));
        let fill = if response.hovered() { ink.fill_hover } else { ink.fill };
        ui.painter().rect_filled(rect, style::radius::SM, fill);
        if response.has_focus() {
            ui.painter().rect_stroke(rect.expand(2.0), style::radius::SM + 2.0, egui::Stroke::new(2.0, ink.accent), egui::StrokeKind::Outside);
        }
        let page = &surface.frame.page;
        let (shown, color) = if page.url.is_empty() || page.url == crate::app::NEW_TAB_URL {
            (ADDRESS_HINT.to_string(), ink.secondary)
        } else {
            (crossbow_core::search::display_url(&page.url), ink.text)
        };
        let mut job = LayoutJob::single_section(shown, style::format(13.0, style::weight::REGULAR, color));
        job.wrap = TextWrapping { max_width: inner.width(), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        let galley = ui.painter().layout_job(job);
        ui.painter().galley(pos2(inner.left(), rect.center().y - galley.size().y / 2.0), galley, color);
        if response.clicked() {
            edit_address(surface);
            ctx.request_repaint();
        }
        return;
    };
    ui.painter().rect_filled(rect, style::radius::SM, ink.fill_hover);
    ui.painter().rect_stroke(rect, style::radius::SM, egui::Stroke::new(1.5, ink.accent), egui::StrokeKind::Inside);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let edit = egui::TextEdit::singleline(text)
        .id(id)
        .frame(egui::Frame::NONE)
        .font(egui::FontId::proportional(13.0))
        .text_color(ink.text)
        .hint_text(style::text(ADDRESS_HINT, 13.0, style::weight::REGULAR).color(ink.secondary))
        .desired_width(inner.width());
    let response = child.add(edit);
    if std::mem::take(&mut surface.chrome.focus_address) {
        response.request_focus();
        let mut state = egui::TextEdit::load_state(&ctx, id).unwrap_or_default();
        let n = text.chars().count();
        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(n))));
        state.store(&ctx, id);
    } else if response.lost_focus() || !response.has_focus() {
        // Enter opens what was typed; Escape or a click elsewhere keeps the page.
        let typed = text.trim().to_string();
        if response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) && !typed.is_empty() {
            surface.command(WindowCommand::Navigate(typed));
        }
        surface.chrome.address = None;
        ctx.request_repaint();
    }
}

fn page_area(ui: &mut Ui, surface: &mut Surface, page: Rect, focused: bool, page_keys: bool) {
    let ctx = ui.ctx().clone();
    let ppp = ctx.pixels_per_point();
    let frame = surface.frame.clone();
    let painter = ui.painter().clone();
    let (_, radius) = content::card_frame(&ctx);
    content::paint_pane_surface(&painter, page, frame.texture.map(|(id, size, _)| (id, size)), ppp, 1.0, radius);
    content::paint_progress(&painter, page, radius, frame.page.progress, if frame.page.loading { 1.0 } else { 0.0 }, frame.palette.as_ref().map_or(egui::Color32::GRAY, |pal| pal.accent));
    let response = ui.interact(page, Id::unique("little-page"), Sense::click_and_drag());
    let capturing = focused && capture(&ctx, surface, page, &frame, response.hovered());
    let page_keys = page_keys && !capturing;
    if surface.chrome.chooser.is_some() || !focused || capturing {
        if std::mem::take(&mut surface.chrome.captured) { surface.push_input(ViewInput::CaptureLost); }
        if std::mem::take(&mut surface.chrome.pointer_over) { surface.push_input(ViewInput::Pointer(content::PointerInput::Leave)); }
    } else {
        let popup = frame.popup.map(|texture| content::pane_popup(&ctx, "little", page.min, texture, ppp));
        let target = Target { page: &response, popup: popup.as_ref() };
        let captured = surface.chrome.captured;
        for event in content::pointer_inputs(&ctx, page, &target, captured, &mut surface.chrome.last_press, &mut surface.chrome.pointer_over) {
            if let content::PointerInput::Button { pressed, .. } = event {
                // A release whose press closed the chooser or the field stays here.
                if !pressed && !surface.chrome.captured { continue; }
                surface.chrome.captured = pressed;
                if pressed {
                    surface.chrome.address = None;
                    ctx.memory_mut(|memory| memory.stop_text_input());
                }
            }
            surface.push_input(ViewInput::Pointer(event));
        }
        if response.hovered() || popup.as_ref().is_some_and(egui::Response::hovered) {
            ctx.set_cursor_icon(content::cursor_icon(frame.page.cursor));
            if let Some(tip) = frame.page.tooltip {
                egui::Tooltip::always_open(ctx.clone(), ui.layer_id(), Id::unique("little-tooltip"), egui::PopupAnchor::Pointer).show(|ui| ui.label(tip));
            }
        }
    }
    if !focused { surface.chrome.keys_down.clear(); }
    let events = if focused { ctx.input(|input| input.events.clone()) } else { Vec::new() };
    for key in input::web_key_inputs(&events) {
        // The page gets the release of each key it saw pressed, and nothing
        // else while the window's own controls have the keyboard.
        let send = match &key {
            input::WebInput::Key(k) if !k.pressed => match surface.chrome.keys_down.iter().position(|held| *held == k.key) {
                Some(index) => {
                    surface.chrome.keys_down.swap_remove(index);
                    true
                }
                None => false,
            },
            input::WebInput::Key(k) if page_keys => {
                if !surface.chrome.keys_down.contains(&k.key) { surface.chrome.keys_down.push(k.key); }
                true
            }
            _ => page_keys,
        };
        if send { surface.push_input(ViewInput::Keyboard(key)); }
    }
    if page_keys {
        // Let the platform IME follow the page's caret while a field is focused.
        if frame.page.editable {
            let caret = Rect::from_min_size(page.center(), vec2(1.0, 18.0));
            ctx.output_mut(|output| output.ime = Some(egui::output::IMEOutput { purpose: Default::default(), rect: page, cursor_rect: caret, should_interrupt_composition: false }));
        }
    }
}

/// Capture in this window: ⌘⇧2, or Command-Shift drag when it's on, starts a
/// selection over the page; a chosen one goes to the root as pixels. True
/// while selecting, when the page gets no input.
fn capture(ctx: &egui::Context, surface: &mut Surface, page: Rect, frame: &crate::window_host::Frame, hovered: bool) -> bool {
    let ppp = ctx.pixels_per_point();
    if surface.chrome.capture.is_none() && surface.chrome.chooser.is_none() {
        let events = ctx.input(|input| input.events.clone());
        if crate::app::take_shortcut(ctx, Modifiers::COMMAND | Modifiers::SHIFT, Key::Num2) || (hovered && capture_ui::starts(&events, page, frame.capture_drag).is_some()) {
            surface.chrome.capture = Some(Selection::new(page));
            surface.chrome.address = None;
            ctx.memory_mut(|memory| memory.stop_text_input());
        }
    }
    let Some(mut selection) = surface.chrome.capture.take() else { return false };
    // A resized window ends the selection.
    if selection.page != page { return false; }
    let texture = frame.texture.map(|(_, size, _)| (size.x as u32, size.y as u32));
    let (events, space) = ctx.input(|input| (input.events.clone(), input.key_down(Key::Space)));
    match selection.input(&events, space) {
        Outcome::Selecting => {
            capture_ui::paint(ctx, &selection, texture, ppp);
            surface.chrome.capture = Some(selection);
        }
        Outcome::Cancelled => {}
        Outcome::Chosen(rect) => match texture.and_then(|texture| crate::capture::pixels(rect, page, texture, ppp)) {
            Some(region) => {
                surface.chrome.capture_anchor = Some(rect.intersect(page));
                surface.chrome.card_seen_at = None;
                surface.command(WindowCommand::Capture(region));
            }
            // Nothing drawn yet: the root says so.
            None => surface.command(WindowCommand::Capture(crate::capture::PixelRect { x: 0, y: 0, width: 0, height: 0 })),
        },
    }
    // Input this pass stays with the selection, even the press that ended it.
    true
}

/// The card of this window's last capture, which hides after a while
/// without the pointer over it.
fn capture_card(ctx: &egui::Context, surface: &mut Surface, window: Rect, pal: &Palette) {
    let (Some(status), Some(anchor)) = (surface.frame.capture_card.clone(), surface.chrome.capture_anchor) else {
        surface.chrome.card_seen_at = None;
        return;
    };
    let now = ctx.input(|input| input.time);
    let seen = *surface.chrome.card_seen_at.get_or_insert(now);
    let view = CardView { anchor, bounds: window, saved: status.saved, notice: status.notice };
    let (action, hovered) = capture_ui::card(ctx, &view, pal);
    if hovered { surface.chrome.card_seen_at = Some(now); }
    if let Some(action) = action {
        surface.command(WindowCommand::Card(action));
    } else if !hovered && now - seen >= capture_ui::CARD_FOR {
        surface.command(WindowCommand::Card(CardAction::Close));
    } else {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64((capture_ui::CARD_FOR - (now - seen)).max(0.05)));
    }
}

/// A Space's row text: its name, and its profile's when there are several.
fn choice_label(choice: &SpaceChoice, profiles: bool) -> String {
    if profiles { format!("{} · {}", choice.name, choice.profile) } else { choice.name.clone() }
}

/// The card's position under Open in Space…, kept inside the window.
pub fn chooser_rect(layout: &Toolbar, window: Rect) -> Rect {
    let width = CHOOSER_WIDTH.min(window.width() - 2.0 * GAP);
    let left = (layout.open.right() - width).clamp(window.left() + GAP, window.right() - GAP - width);
    let top = layout.open.bottom() + 6.0;
    Rect::from_min_max(pos2(left, top), pos2(left + width, (window.bottom() - GAP).max(top + 60.0)))
}

/// What a click in the chooser asks for.
enum Pick {
    Move(WindowCommand),
    /// A Space under another profile: ask before reopening there.
    Confirm(String),
    Cancel,
}

fn chooser(ui: &mut Ui, surface: &mut Surface, layout: &Toolbar, window: Rect, ink: &Ink) {
    let ctx = ui.ctx().clone();
    let bounds = chooser_rect(layout, window);
    let card_ink = Ink::new(surface.frame.palette.as_ref().map_or(ink.surface, |pal| pal.panel), ink.accent);
    let spaces = surface.frame.spaces.clone();
    let profiles = spaces.iter().any(|choice| !choice.same_profile);
    let Some(state) = surface.chrome.chooser.as_mut() else { return };
    let focus = std::mem::take(&mut state.focus);
    let confirming = state.confirm.clone().and_then(|id| spaces.iter().find(|choice| choice.id == id).cloned());
    let mut pick = None;
    let area = egui::Area::new(Id::unique("little-chooser")).order(egui::Order::Foreground).fixed_pos(bounds.min).constrain(false).fade_in(false).show(&ctx, |ui| {
        ui.set_width(bounds.width());
        egui::Frame::NONE.fill(card_ink.surface).corner_radius(style::radius::MD).stroke(egui::Stroke::new(1.0, card_ink.separator))
            .shadow(egui::epaint::Shadow { offset: [0, 6], blur: 20, spread: 0, color: egui::Color32::from_black_alpha(50) })
            .inner_margin(10.0).show(ui, |ui| {
                ui.set_width(bounds.width() - 20.0);
                style::apply_visuals(ui, &card_ink);
                match &confirming {
                    Some(choice) => {
                        ui.label(style::text(format!("Reopen in {}?", choice.name), 13.5, style::weight::SEMIBOLD).color(card_ink.text));
                        ui.label(style::text(format!("{} uses the {} profile. The page reloads there, without this profile's sign-ins or anything typed into it.", choice.name, choice.profile), 12.0, style::weight::REGULAR).color(card_ink.secondary));
                        ui.horizontal_wrapped(|ui| {
                            let confirm = Button::new(REOPEN).kind(Kind::Primary).size(Size::Small).show(ui, &card_ink);
                            if focus { confirm.request_focus(); }
                            if confirm.clicked() { pick = Some(Pick::Move(WindowCommand::OpenInSpace { space: choice.id.clone(), reopen: true })); }
                            if Button::new("Cancel").kind(Kind::Plain).size(Size::Small).show(ui, &card_ink).clicked() { pick = Some(Pick::Cancel); }
                        });
                    }
                    None => {
                        ui.label(style::text("Move this page to a Space", 12.0, style::weight::SEMIBOLD).color(card_ink.secondary));
                        egui::ScrollArea::vertical().max_height((bounds.height() - 80.0).max(36.0)).auto_shrink([false, true]).show(ui, |ui| {
                            for (index, choice) in spaces.iter().enumerate() {
                                let label = choice_label(choice, profiles);
                                let detail = (!choice.same_profile).then(|| format!("Reopens in {}", choice.profile));
                                let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), if detail.is_some() { 44.0 } else { 30.0 }), Sense::click());
                                response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
                                if focus && index == 0 { response.request_focus(); }
                                if response.hovered() || response.has_focus() {
                                    ui.painter().rect_filled(rect, style::radius::SM, card_ink.fill_hover);
                                }
                                if response.has_focus() {
                                    ui.painter().rect_stroke(rect, style::radius::SM, egui::Stroke::new(2.0, card_ink.accent), egui::StrokeKind::Inside);
                                }
                                let width = rect.width() - 20.0;
                                line(ui, pos2(rect.left() + 10.0, rect.top() + 15.0), width, &label, 13.0, style::weight::MEDIUM, card_ink.text);
                                if let Some(detail) = &detail {
                                    line(ui, pos2(rect.left() + 10.0, rect.top() + 32.0), width, detail, 11.5, style::weight::REGULAR, card_ink.secondary);
                                }
                                if response.clicked() {
                                    pick = Some(if choice.same_profile {
                                        Pick::Move(WindowCommand::OpenInSpace { space: choice.id.clone(), reopen: false })
                                    } else {
                                        Pick::Confirm(choice.id.clone())
                                    });
                                }
                            }
                        });
                        if Button::new("Cancel").kind(Kind::Plain).size(Size::Small).show(ui, &card_ink).clicked() { pick = Some(Pick::Cancel); }
                    }
                }
            });
    });
    // A press outside the card and its button cancels.
    let card = area.response.rect;
    if pick.is_none() && ctx.input(|input| input.pointer.any_pressed() && input.pointer.interact_pos().is_some_and(|pos| !card.contains(pos) && !layout.open.contains(pos))) {
        pick = Some(Pick::Cancel);
    }
    match pick {
        Some(Pick::Move(command)) => {
            surface.command(command);
            surface.chrome.chooser = None;
        }
        Some(Pick::Confirm(space)) => surface.chrome.chooser = Some(Chooser { confirm: Some(space), focus: true }),
        Some(Pick::Cancel) => surface.chrome.chooser = None,
        None => return,
    }
    ctx.request_repaint();
}

/// One line of text, cut with an ellipsis to `width`, centred on `left_center`.
fn line(ui: &Ui, left_center: Pos2, width: f32, text: &str, size: f32, weight: f32, color: egui::Color32) {
    let mut job = LayoutJob::single_section(text.to_string(), style::format(size, weight, color));
    job.wrap = TextWrapping { max_width: width, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(pos2(left_center.x, left_center.y - galley.size().y / 2.0), galley, color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window_host::tests::Recorder;
    use crate::window_host::{Frame, Host, PageState};
    use egui::{Event, PointerButton, Vec2, ViewportId, ViewportInfo};

    const SIZES: [Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
    const LONG_URL: &str = "https://docs.example.com/guides/getting-started/with/a/very/long/path/that/never/fits/in/the/address/field?query=1234567890&more=abcdefghij";
    const WORK: &str = "Work · Work";
    const PERSONAL: &str = "Personal · Personal";

    /// One Little Window: its UI runs over the host's shared surface, and the
    /// host delivers what it collected to a recording page.
    struct Window {
        ctx: egui::Context,
        host: Host<Recorder>,
        page: Recorder,
        /// What the page saw when it opened.
        opened: Vec<String>,
        size: Vec2,
        scale: f32,
        focused: bool,
        fullscreen: bool,
        time: f64,
    }

    struct Shown {
        text: Vec<(String, Rect, Rect)>,
        /// Each mesh's texture, bounds and vertex colours.
        meshes: Vec<(egui::TextureId, Rect, Vec<egui::Color32>)>,
        /// What the window asked of its native window.
        commands: Vec<egui::ViewportCommand>,
        window: Rect,
    }

    impl Shown {
        fn find(&self, label: &str) -> Option<(Rect, Rect)> {
            self.text.iter().rfind(|(text, ..)| text == label).map(|(_, bounds, clip)| (*bounds, *clip))
        }

        fn at(&self, label: &str) -> egui::Pos2 {
            self.find(label).unwrap_or_else(|| panic!("{label:?} is not shown: {:?}", self.text.iter().map(|(text, ..)| text).collect::<Vec<_>>())).0.center()
        }

        /// Every text is whole: inside the window and its clip.
        fn assert_unclipped(&self) {
            for (text, bounds, clip) in &self.text {
                if text.trim().is_empty() { continue; }
                assert!(self.window.expand(0.5).contains_rect(*bounds) && clip.expand(0.5).contains_rect(*bounds), "{text:?} is clipped: {bounds:?} in {clip:?} at {:?}", self.window);
            }
        }
    }

    impl Window {
        fn new(size: Vec2) -> Self {
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let host = Host::new("fixture");
            host.publish(Frame {
                page: PageState { url: "https://example.com/start".into(), title: "Start".into(), can_go_back: true, ..Default::default() },
                spaces: vec![
                    SpaceChoice { id: "personal".into(), name: "Personal".into(), profile: "Personal".into(), same_profile: true },
                    SpaceChoice { id: "work".into(), name: "Work".into(), profile: "Work".into(), same_profile: false },
                ],
                ..Default::default()
            });
            let mut window = Window { ctx, host, page: Recorder::default(), opened: Vec::new(), size, scale: 1.0, focused: true, fullscreen: false, time: 0.0 };
            window.settle();
            window.pump();
            window.opened = window.page.take();
            window
        }

        fn frame_with(&mut self, events: Vec<Event>, close: bool) -> Shown {
            self.time += 1.0 / 60.0;
            let mut raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)), time: Some(self.time), focused: self.focused, events, ..Default::default() };
            let info = ViewportInfo { native_pixels_per_point: Some(self.scale), fullscreen: Some(self.fullscreen), events: if close { vec![egui::ViewportEvent::Close] } else { vec![] }, ..Default::default() };
            raw.viewports.insert(ViewportId::ROOT, info);
            let host = &self.host;
            let mut output = self.ctx.run_ui(raw, |ui| show(ui, &mut host.lock()));
            output.textures_delta.clear();
            let text = output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()), shape.clip_rect)),
                _ => None,
            }).collect();
            let meshes = output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::Mesh(mesh) => Some((mesh.texture_id, mesh.calc_bounds(), mesh.vertices.iter().map(|vertex| vertex.color).collect())),
                _ => None,
            }).collect();
            let commands = output.viewport_output.remove(&ViewportId::ROOT).map(|viewport| viewport.commands).unwrap_or_default();
            Shown { text, meshes, commands, window: Rect::from_min_size(Pos2::ZERO, self.size) }
        }

        fn frame(&mut self, events: Vec<Event>) -> Shown {
            self.frame_with(events, false)
        }

        fn settle(&mut self) -> Shown {
            for _ in 0..4 { self.frame(vec![]); }
            self.frame(vec![])
        }

        /// The root's next pass: the page opens at the reported size, then
        /// receives what the window collected.
        fn pump(&mut self) -> Vec<WindowCommand> {
            let page = self.page.clone();
            self.host.pump(|size, scale| {
                page.0.borrow_mut().push(format!("open {}x{} @{scale}", size.x, size.y));
                Some(page)
            })
        }

        fn click_at(&mut self, pos: egui::Pos2) {
            for pressed in [true, false] {
                self.frame(vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
            }
            self.settle();
        }

        fn click(&mut self, label: &str) {
            let pos = self.settle().at(label);
            self.click_at(pos);
        }

        fn key(&mut self, key: Key, modifiers: Modifiers) {
            for pressed in [true, false] {
                self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }]);
            }
            self.settle();
        }

        fn page_rect(&self) -> Rect {
            let window = Rect::from_min_size(Pos2::ZERO, self.size);
            page_rect(&self.ctx, window, self.layout().row.bottom())
        }

        fn layout(&self) -> Toolbar {
            toolbar_layout(&self.ctx, Rect::from_min_size(Pos2::ZERO, self.size))
        }

        /// The page's presses, in its own coordinates.
        fn presses(&self) -> Vec<String> {
            self.page.take().into_iter().filter(|line| line.starts_with("button") || line.starts_with("key") || line.starts_with("text") || line.starts_with("compose")).collect()
        }

        fn chooser_open(&self) -> bool {
            self.host.lock().chrome.chooser.is_some()
        }
    }

    #[test]
    fn the_toolbar_fits_without_overlap_or_clipped_text_and_the_page_fills_the_rest_at_both_sizes() {
        for size in SIZES {
            let mut w = Window::new(size);
            let window = Rect::from_min_size(Pos2::ZERO, size);
            let layout = toolbar_layout(&w.ctx, window);
            let controls = [layout.back, layout.forward, layout.reload, layout.address, layout.open, layout.on_top, layout.close];
            for (i, a) in controls.iter().enumerate() {
                assert!(window.contains_rect(*a) && layout.row.contains_rect(*a), "control {i} inside the toolbar at {size:?}");
                for b in &controls[i + 1..] {
                    assert!(!a.intersects(*b), "{a:?} overlaps {b:?} at {size:?}");
                }
                assert!((a.center().y - layout.row.center().y).abs() < 0.5, "control {i} centred in its row");
            }
            assert!(layout.address.width() >= 240.0, "room to read the address at {size:?}");
            for url in ["https://example.com/start", LONG_URL] {
                let frame = w.host.lock().frame.clone();
                w.host.publish(Frame { page: PageState { url: url.into(), loading: url == LONG_URL, ..Default::default() }, ..frame });
                let shown = w.settle();
                shown.assert_unclipped();
                let (label, _) = shown.find(OPEN_IN_SPACE).expect("Open in Space… is shown");
                assert!(layout.open.contains_rect(label), "the button's label fits it at {size:?}");
                let (address, _) = shown.find(&crossbow_core::search::display_url(url)).expect("the address is shown");
                assert!(layout.address.shrink2(vec2(10.0, 0.0)).expand(0.5).contains_rect(address), "{url} stays in its field at {size:?}: {address:?}");
            }
            assert_eq!(w.opened, [format!("open {}x{} @1", size.x - 2.0 * GAP, size.y - layout.row.height() - GAP), "focus true".to_string()], "the page fills the rest at {size:?}");

            // Resizing and a new backing scale follow the window.
            w.size = size + vec2(80.0, 40.0);
            w.scale = 2.0;
            w.settle();
            w.pump();
            let page = w.page_rect();
            assert_eq!(w.page.take(), [format!("resize {}x{} @2", page.width(), page.height())]);
        }
    }

    #[test]
    fn the_chooser_moves_within_a_profile_at_once_and_asks_before_reopening_under_another() {
        for size in SIZES {
            let mut w = Window::new(size);
            w.key(Key::O, Modifiers::COMMAND);
            assert!(w.chooser_open(), "⌘O opens the chooser");
            let shown = w.settle();
            shown.assert_unclipped();
            let (work, _) = shown.find(WORK).expect("each Space is listed");
            let (detail, _) = shown.find("Reopens in Work").expect("another profile is named");
            let (personal, _) = shown.find(PERSONAL).unwrap();
            assert!(personal.bottom() <= work.top() && work.bottom() <= detail.top(), "rows in order without overlap at {size:?}");
            assert!(shown.find("Reopens in Personal").is_none(), "the same profile moves as it is");
            assert!(personal.top() > w.layout().open.bottom(), "below the toolbar");

            // Another profile asks first; Cancel changes nothing.
            w.click(WORK);
            let shown = w.settle();
            shown.assert_unclipped();
            assert!(shown.find("Reopen in Work?").is_some() && shown.find(REOPEN).is_some(), "asks before reopening at {size:?}");
            w.click("Cancel");
            assert!(!w.chooser_open() && w.pump().is_empty(), "Cancel moves nothing");

            // Escape steps back from the question to the list, then closes.
            w.key(Key::O, Modifiers::COMMAND);
            w.click(WORK);
            w.key(Key::Escape, Modifiers::NONE);
            assert!(w.settle().find(WORK).is_some() && w.chooser_open(), "back to the list");
            w.key(Key::Escape, Modifiers::NONE);
            assert!(!w.chooser_open() && w.pump().is_empty());

            // Confirmed: reopen in Work.
            w.click(OPEN_IN_SPACE);
            w.click(WORK);
            w.click(REOPEN);
            assert_eq!(w.pump(), [WindowCommand::OpenInSpace { space: "work".into(), reopen: true }]);
            assert!(!w.chooser_open());

            // Same profile: one click moves the live page.
            w.click(OPEN_IN_SPACE);
            w.click(PERSONAL);
            assert_eq!(w.pump(), [WindowCommand::OpenInSpace { space: "personal".into(), reopen: false }]);

            // A click outside, or the button again, closes it.
            w.click(OPEN_IN_SPACE);
            let page = w.page_rect();
            w.click_at(page.left_bottom() + vec2(20.0, -20.0));
            assert!(!w.chooser_open());
            w.click(OPEN_IN_SPACE);
            w.click(OPEN_IN_SPACE);
            assert!(!w.chooser_open() && w.pump().is_empty());
            let presses = w.presses();
            assert!(presses.is_empty(), "the page received nothing while the chooser was open: {presses:?}");
        }
    }

    #[test]
    fn the_address_opens_what_is_typed_on_enter_and_escape_keeps_the_page() {
        for size in SIZES {
            let mut w = Window::new(size);
            w.click("example.com/start");
            assert!(w.host.lock().chrome.address.is_some(), "a click turns the address into a field");
            w.key(Key::A, Modifiers::COMMAND);
            w.frame(vec![Event::Text("rust lang".into())]);
            w.key(Key::Enter, Modifiers::NONE);
            assert_eq!(w.pump(), [WindowCommand::Navigate("rust lang".into())]);
            assert!(w.host.lock().chrome.address.is_none());

            w.key(Key::L, Modifiers::COMMAND);
            assert_eq!(w.host.lock().chrome.address.as_deref(), Some("https://example.com/start"), "⌘L edits the address");
            w.frame(vec![Event::Text("x".into())]);
            w.key(Key::Escape, Modifiers::NONE);
            let editing = w.host.lock().chrome.address.is_some();
            assert!(!editing && w.pump().is_empty(), "Escape keeps the page");
            let presses = w.presses();
            assert!(presses.is_empty(), "typing in the field never reaches the page: {presses:?}");
            assert!(w.settle().find("example.com/start").is_some());
        }
    }

    #[test]
    fn page_input_arrives_in_page_coordinates_and_window_shortcuts_stay_with_the_window() {
        let mut w = Window::new(SIZES[0]);
        let page = w.page_rect();
        w.click_at(page.min + vec2(30.0, 40.0));
        w.key(Key::K, Modifiers::NONE);
        w.frame(vec![Event::Ime(egui::ImeEvent::Preedit { text: "に".into(), active_range_chars: None }), Event::Ime(egui::ImeEvent::Commit("日本".into()))]);
        w.key(Key::Tab, Modifiers::NONE);
        w.key(Key::J, Modifiers::NONE);
        w.pump();
        assert_eq!(w.presses(), ["button 30,40 true", "button 30,40 false", "key Letter('K')", "key Letter('K') up", "compose に", "text 日本", "key Tab", "key Tab up", "key Letter('J')", "key Letter('J') up"],
            "presses and releases, IME and Tab reach the page; Tab does not move focus to the toolbar");

        for (key, command) in [(Key::W, WindowCommand::Close), (Key::R, WindowCommand::Reload), (Key::OpenBracket, WindowCommand::Back), (Key::CloseBracket, WindowCommand::Forward)] {
            w.key(key, Modifiers::COMMAND);
            let commands = w.pump();
            assert!(commands.contains(&command), "{key:?}: {commands:?}");
            let presses = w.presses();
            assert!(presses.is_empty(), "⌘{key:?} stays with the window: {presses:?}");
            w.host.keep_open();
        }

        // An inactive window sends nothing to its page but, once, that the
        // pointer left it.
        w.focused = false;
        w.settle();
        w.click_at(page.center());
        w.key(Key::K, Modifiers::NONE);
        w.pump();
        assert_eq!(w.page.take(), ["focus false", "Leave"]);
    }

    #[test]
    fn the_close_button_and_the_title_bar_ask_the_root_to_close_once() {
        let mut w = Window::new(SIZES[1]);
        let layout = toolbar_layout(&w.ctx, Rect::from_min_size(Pos2::ZERO, SIZES[1]));
        w.click_at(layout.close.center());
        assert_eq!(w.pump(), [WindowCommand::Close]);
        w.host.keep_open();
        w.frame_with(vec![], true);
        assert_eq!(w.pump(), [WindowCommand::Close], "the title bar's close button");
        w.frame_with(vec![], true);
        assert!(w.pump().is_empty(), "asked once while closing");
    }

    #[test]
    fn keep_window_on_top_is_a_pin_with_a_tooltip_that_asks_once_and_shows_its_state_at_both_sizes() {
        for size in SIZES {
            for on in [false, true] {
                let mut w = Window::new(size);
                let frame = w.host.lock().frame.clone();
                w.host.publish(Frame { on_top: on, ..frame });
                let layout = w.layout();
                let glyph = if on { '\u{e258}' } else { '\u{e2b5}' }.to_string();
                let shown = w.settle();
                assert!(shown.text.iter().any(|(text, bounds, _)| *text == glyph && layout.on_top.contains_rect(*bounds)), "pin{} at {size:?}", if on { "" } else { "-off" });
                // Resting on it names it.
                w.frame(vec![Event::PointerMoved(layout.on_top.center())]);
                let mut named = false;
                for _ in 0..90 {
                    named |= w.frame(vec![]).text.iter().any(|(text, ..)| text == KEEP_ON_TOP);
                }
                assert!(named, "a tooltip at {size:?}");
                w.click_at(layout.on_top.center());
                assert_eq!(w.pump(), [WindowCommand::KeepOnTop(!on)], "one command at {size:?}");
            }
        }
    }

    #[test]
    fn the_toolbar_sits_on_the_main_windows_top_row_clear_of_the_traffic_lights_at_both_sizes() {
        use crate::ui::{sidebar::SidebarHeader, window};
        for size in SIZES {
            let w = Window::new(size);
            let layout = w.layout();
            // The main window's toolbar: its sidebar's top row, docked on the left.
            let sidebar = Rect::from_min_size(Pos2::ZERO, vec2(252.0, size.y));
            let main = SidebarHeader::new(sidebar, true);
            assert_eq!(layout.row, Rect::from_min_size(Pos2::ZERO, vec2(size.x, window::NATIVE_CONTROL_ROW)), "one top row, the native one, with no strip above it at {size:?}");
            assert_eq!([layout.back, layout.forward, layout.reload], [0, 1, 2].map(|index| main.row.leading_control(index)), "the main toolbar's inset, control size and pitch at {size:?}");
            assert_eq!(size.x - layout.close.right(), sidebar.right() - main.row.trailing_control(0).right(), "and its trailing inset at {size:?}");
            let lights = Rect::from_min_max(Pos2::ZERO, pos2(window::traffic_light_inset(), window::NATIVE_CONTROL_ROW));
            assert_eq!(layout.on_top, main.row.trailing_control(1).translate(vec2(size.x - sidebar.right(), 0.0)), "Keep Window on Top at the toolbar's pitch beside Close at {size:?}");
            for (name, control) in [("back", layout.back), ("forward", layout.forward), ("reload", layout.reload), ("address", layout.address), ("open", layout.open), ("on top", layout.on_top), ("close", layout.close)] {
                assert_eq!(control.center().y, main.row.center_y, "{name} on the main toolbar's centre line at {size:?}");
                assert_eq!(control.height(), window::ROW_CONTROL, "{name} has the main toolbar's control height at {size:?}");
                assert!(layout.row.contains_rect(control), "{name} inside the row at {size:?}");
                assert!(!control.intersect(lights).is_positive(), "{name} clear of the traffic lights at {size:?}: {control:?}");
            }
            assert_eq!(w.page_rect(), Rect::from_min_max(pos2(GAP, window::NATIVE_CONTROL_ROW), pos2(size.x - GAP, size.y - GAP)), "the page starts under the row, framed like the main window's content at {size:?}");
        }
        // Full screen hides the traffic lights: the toolbar starts at the edge and the page meets it, as in the main window.
        let mut w = Window::new(SIZES[0]);
        w.fullscreen = true;
        w.settle();
        let layout = w.layout();
        assert_eq!((layout.back.left(), layout.back.center().y), (window::ROW_PAD, window::NATIVE_CONTROL_ROW / 2.0));
        assert_eq!(w.page_rect(), Rect::from_min_max(pos2(0.0, window::NATIVE_CONTROL_ROW), SIZES[0].to_pos2()));
    }

    #[test]
    fn the_top_row_moves_and_zooms_the_window_as_the_main_windows_does() {
        let press = |pos: Pos2, pressed: bool| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        for size in SIZES {
            let mut w = Window::new(size);
            let layout = w.layout();
            let spot = pos2(layout.reload.right() + GAP / 2.0, layout.row.center().y);
            let mut commands = w.frame(vec![Event::PointerMoved(spot), press(spot, true)]).commands;
            for step in 1..=3 {
                commands.extend(w.frame(vec![Event::PointerMoved(spot + vec2(4.0 * step as f32, 0.0))]).commands);
            }
            commands.extend(w.frame(vec![press(spot + vec2(12.0, 0.0), false)]).commands);
            assert!(commands.contains(&egui::ViewportCommand::StartDrag), "dragging the row moves the window at {size:?}: {commands:?}");
            w.settle();
            let mut commands = Vec::new();
            for pressed in [true, false, true, false] {
                commands.extend(w.frame(vec![Event::PointerMoved(spot), press(spot, pressed)]).commands);
            }
            assert!(commands.contains(&egui::ViewportCommand::Maximized(true)), "a double click zooms it at {size:?}: {commands:?}");
            w.settle();
            assert!(w.pump().is_empty() && w.presses().is_empty(), "neither reaches the page or the toolbar");
            w.click_at(layout.reload.center());
            assert_eq!(w.pump(), [WindowCommand::Reload], "the row's controls keep their clicks");
        }
    }

    #[test]
    fn the_window_paints_its_spaces_gradient_and_surface_texture_like_the_main_window() {
        let window = Rect::from_min_size(Pos2::ZERO, SIZES[0]);
        let mut w = Window::new(SIZES[0]);
        let texture = egui::TextureId::User(41);
        for dark in [false, true] {
            let mut palette = theme::palette(&crossbow_core::model::Theme::preset(2), dark);
            palette.grain = 0.8;
            let frame = w.host.lock().frame.clone();
            w.host.publish(Frame { palette: Some(palette.clone()), surface_texture: Some(texture), ..frame });
            let shown = w.settle();
            let gradient = shown.meshes.iter().find(|(id, rect, _)| *id == egui::TextureId::default() && *rect == window).expect("a gradient fills the window, top row included");
            for stop in [palette.stops.first(), palette.stops.last()].into_iter().flatten() {
                assert!(gradient.2.contains(stop), "the Space's {} palette, at full opacity: {stop:?}", if dark { "dark" } else { "light" });
            }
            let grain = shown.meshes.iter().find(|(id, ..)| *id == texture).expect("the Space's surface texture is painted");
            assert_eq!(grain.1, window, "across the whole window");
            assert!(grain.2.iter().all(|color| *color == egui::Color32::from_white_alpha((0.8_f32 * 26.0) as u8)), "at the palette's strength");
        }
        let frame = w.host.lock().frame.clone();
        w.host.publish(Frame { surface_texture: None, ..frame });
        assert!(!w.settle().meshes.iter().any(|(id, ..)| *id == texture), "no texture, nothing painted");
    }

    /// The page painted at the window's scale, with Command-Shift drag `drag`.
    fn painted(w: &mut Window, drag: bool) -> Rect {
        let page = w.page_rect();
        let frame = w.host.lock().frame.clone();
        w.host.publish(Frame { texture: Some((egui::TextureId::User(7), page.size() * w.scale, 1)), capture_drag: drag, ..frame });
        page
    }

    const COMMAND_SHIFT: Modifiers = Modifiers { alt: false, ctrl: false, shift: true, mac_cmd: true, command: true };

    fn button(pos: egui::Pos2, pressed: bool, modifiers: Modifiers) -> Event {
        Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers }
    }

    #[test]
    fn command_shift_drag_in_a_little_window_captures_its_pixels_and_the_page_gets_nothing() {
        for size in SIZES {
            let mut w = Window::new(size);
            w.scale = 2.0;
            w.settle();
            w.pump();
            w.page.take();
            let page = painted(&mut w, true);
            let from = page.min + vec2(30.0, 20.0);
            let to = from + vec2(100.5, 50.0);
            w.frame(vec![Event::PointerMoved(from), button(from, true, COMMAND_SHIFT)]);
            w.frame(vec![Event::PointerMoved(to)]);
            let shown = w.frame(vec![]);
            assert!(shown.find("201 × 100").is_some(), "the size in pixels at {size:?}");
            w.frame(vec![button(to, false, Modifiers::NONE)]);
            assert_eq!(w.pump(), [WindowCommand::Capture(crate::capture::PixelRect { x: 60, y: 40, width: 201, height: 100 })], "at {size:?}");
            assert!(w.presses().is_empty(), "the page got none of it at {size:?}");
            assert!(w.host.lock().chrome.capture.is_none());
        }
    }

    #[test]
    fn with_command_shift_drag_off_the_page_gets_the_press_unchanged() {
        let mut w = Window::new(SIZES[0]);
        let page = painted(&mut w, false);
        w.settle();
        w.pump();
        w.page.take();
        let at = page.min + vec2(30.0, 20.0);
        w.frame(vec![Event::PointerMoved(at), button(at, true, COMMAND_SHIFT)]);
        w.frame(vec![button(at, false, COMMAND_SHIFT)]);
        assert!(w.pump().is_empty() && w.host.lock().chrome.capture.is_none());
        let presses = w.presses();
        assert_eq!(presses.len(), 2, "{presses:?}");
        assert!(presses[0].starts_with("button 30,20 true") && presses[0].contains("shift: true") && presses[0].contains("command: true"), "{presses:?}");
    }

    #[test]
    fn command_shift_2_in_a_little_window_selects_and_escape_or_return_ends_it() {
        let mut w = Window::new(SIZES[1]);
        w.scale = 2.0;
        w.settle();
        w.pump();
        let page = painted(&mut w, true);
        w.page.take();
        w.key(Key::Num2, COMMAND_SHIFT);
        assert!(w.settle().find(capture_ui::HINT).is_some() && w.host.lock().chrome.capture.is_some());
        w.key(Key::Escape, Modifiers::NONE);
        assert!(w.host.lock().chrome.capture.is_none(), "Escape cancels");
        assert!(w.pump().is_empty() && w.presses().is_empty(), "nothing captured, and the page saw no keys");
        // Return with no drag captures the whole visible page.
        w.key(Key::Num2, COMMAND_SHIFT);
        w.key(Key::Enter, Modifiers::NONE);
        let texture = page.size() * 2.0;
        assert_eq!(w.pump(), [WindowCommand::Capture(crate::capture::PixelRect { x: 0, y: 0, width: texture.x as u32, height: texture.y as u32 })]);
        assert!(w.presses().is_empty());
    }

    #[test]
    fn a_little_windows_capture_card_offers_its_actions_and_closes_after_six_seconds() {
        for size in SIZES {
            let mut w = Window::new(size);
            let page = painted(&mut w, true);
            let from = page.min + vec2(30.0, 20.0);
            w.frame(vec![Event::PointerMoved(from), button(from, true, COMMAND_SHIFT)]);
            w.frame(vec![Event::PointerMoved(from + vec2(200.0, 120.0))]);
            w.frame(vec![button(from + vec2(200.0, 120.0), false, Modifiers::NONE)]);
            assert_eq!(w.pump().len(), 1);
            w.page.take();
            let frame = w.host.lock().frame.clone();
            w.host.publish(Frame { capture_card: Some(capture_ui::CardStatus { saved: true, notice: None }), ..frame });
            let shown = w.settle();
            shown.assert_unclipped();
            assert!(shown.find(capture_ui::COPIED_AND_SAVED).is_some(), "at {size:?}");
            w.click(capture_ui::COPY);
            assert_eq!(w.pump(), [WindowCommand::Card(CardAction::Copy)]);
            assert!(w.presses().is_empty(), "the card's click stays off the page");
            w.frame(vec![Event::PointerMoved(page.min + vec2(4.0, 4.0))]);
            assert!(w.pump().is_empty(), "still shown");
            w.time += capture_ui::CARD_FOR;
            w.frame(vec![]);
            assert_eq!(w.pump(), [WindowCommand::Card(CardAction::Close)], "hidden after six seconds at {size:?}");
        }
    }
}
