//! The Picture in Picture player: one tab's video, cut from the tab's own
//! frames, in a small floating window. Its controls show while the pointer
//! is over it, a control has focus or the video is paused or over. It runs
//! in its own deferred viewport and only touches its [`PlayerSurface`]; the
//! root owns the page and sends the controls on.

use crate::app::pip::Control;
use crate::ui::icons::{self, Icon};
use crate::ui::style;
use egui::{Color32, Id, Key, Modifiers, Rect, Sense, Stroke, TextureId, Ui, Vec2, pos2, vec2};

const BUTTON: f32 = 28.0;
const CENTRE_BUTTON: f32 = 32.0;
const INSET: f32 = 8.0;
const PITCH: f32 = 4.0;
const CENTRE_GAP: f32 = 12.0;
pub const ENDED: &str = "Video ended";
pub const OUT_OF_VIEW: &str = "Video out of view";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Playing,
    /// No part of the video is on the tab's page, and scrolling to it failed.
    OutOfView,
    Ended,
}

/// What the root publishes for the player to draw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerFrame {
    /// The tab's texture and its size in pixels.
    pub texture: Option<(TextureId, Vec2)>,
    /// The video's part of the texture, in pixels.
    pub crop: Option<Rect>,
    pub status: Status,
    pub paused: bool,
    pub muted: bool,
    pub on_top: bool,
    /// Why the last Keep on Top choice was not saved.
    pub notice: Option<String>,
}

/// What the player asks of the root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerInput {
    Control(Control),
    KeepOnTop(bool),
}

/// State shared by the player's callback and the root.
#[derive(Debug, Default)]
pub struct PlayerSurface {
    /// Published by the root.
    pub frame: PlayerFrame,
    /// The window's frame on screen, in points, as the player last saw it.
    pub outer: Option<Rect>,
    inputs: Vec<PlayerInput>,
}

impl PlayerSurface {
    /// A new frame to draw. True when it differs from the last.
    pub fn publish(&mut self, frame: PlayerFrame) -> bool {
        std::mem::replace(&mut self.frame, frame) != self.frame
    }

    /// What the player asked for since the last call.
    pub fn take(&mut self) -> Vec<PlayerInput> {
        std::mem::take(&mut self.inputs)
    }
}

/// Where the controls sit in a window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub on_top: Rect,
    pub back_to_tab: Rect,
    pub close: Rect,
    pub back: Rect,
    pub play: Rect,
    pub forward: Rect,
    pub mute: Rect,
}

pub fn layout(window: Rect) -> Layout {
    let square = |min: egui::Pos2, side: f32| Rect::from_min_size(min, Vec2::splat(side));
    let close = square(pos2(window.right() - INSET - BUTTON, window.top() + INSET), BUTTON);
    let centre = window.center();
    let play = Rect::from_center_size(centre, Vec2::splat(CENTRE_BUTTON));
    Layout {
        on_top: square(window.min + vec2(INSET, INSET), BUTTON),
        back_to_tab: close.translate(vec2(-(BUTTON + PITCH), 0.0)),
        close,
        back: play.translate(vec2(-(CENTRE_BUTTON + CENTRE_GAP), 0.0)),
        play,
        forward: play.translate(vec2(CENTRE_BUTTON + CENTRE_GAP, 0.0)),
        mute: square(window.max - vec2(INSET + BUTTON, INSET + BUTTON), BUTTON),
    }
}

/// `content` scaled to fit inside `window`, centred.
fn letterbox(content: Vec2, window: Rect) -> Rect {
    if content.x <= 0.0 || content.y <= 0.0 { return window; }
    let scale = (window.width() / content.x).min(window.height() / content.y);
    Rect::from_center_size(window.center(), content * scale)
}

/// One pass of the player. Everything the root must act on goes into `surface`.
pub fn show(ui: &mut Ui, surface: &mut PlayerSurface) {
    let ctx = ui.ctx().clone();
    let window = ui.max_rect();
    let frame = surface.frame.clone();
    let before = surface.inputs.len();
    surface.outer = ctx.input(|input| input.viewport().outer_rect);
    if ctx.input(|input| input.viewport().close_requested()) { surface.inputs.push(PlayerInput::Control(Control::Close)); }
    let video = frame.status != Status::Ended;

    let painter = ui.painter().clone();
    painter.rect_filled(window, 0.0, Color32::BLACK);
    if let (Some((texture, size)), Some(crop), true) = (frame.texture, frame.crop, frame.status == Status::Playing) {
        if size.x >= 1.0 && size.y >= 1.0 {
            let uv = Rect::from_min_max(pos2(crop.min.x / size.x, crop.min.y / size.y), pos2(crop.max.x / size.x, crop.max.y / size.y));
            painter.image(texture, letterbox(crop.size(), window), uv, Color32::WHITE);
        }
    }
    // Under the controls: dragging anywhere else moves the window. Not a
    // focus stop, so Tab goes straight to the controls.
    let drag = ui.interact(window, Id::unique("pip-player-drag"), Sense::CLICK | Sense::DRAG);
    if drag.drag_started() { ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag); }

    let take = |mods: Modifiers, key: Key| crate::app::take_shortcut(&ctx, mods, key);
    if take(Modifiers::COMMAND, Key::W) || take(Modifiers::NONE, Key::Escape) { surface.inputs.push(PlayerInput::Control(Control::Close)); }
    if video && ctx.memory(|memory| memory.focused().is_none()) && take(Modifiers::NONE, Key::Space) { surface.inputs.push(PlayerInput::Control(Control::PlayPause)); }

    let hovered = ctx.input(|input| input.pointer.hover_pos()).is_some_and(|pos| window.contains(pos));
    let focused = ctx.memory(|memory| memory.focused().is_some());
    let shown = hovered || focused || frame.paused || frame.status != Status::Playing || frame.notice.is_some();
    if shown { painter.rect_filled(window, 0.0, Color32::from_black_alpha(96)); }
    // Hidden controls still take focus, so Tab shows them.
    let at = layout(window);
    let mut clicked = |rect: Rect, icon: Icon, label: &str, selected: Option<bool>, enabled: bool, input: PlayerInput| {
        if control(ui, rect, icon, label, selected, enabled, shown) { surface.inputs.push(input); }
    };
    let on_top = (if frame.on_top { Icon::Pin } else { Icon::PinOff }, !frame.on_top);
    clicked(at.on_top, on_top.0, "Keep on Top", Some(frame.on_top), true, PlayerInput::KeepOnTop(on_top.1));
    clicked(at.back_to_tab, Icon::BackToTab, "Back to Tab", None, true, PlayerInput::Control(Control::BackToTab));
    clicked(at.close, Icon::Close, "Close", None, true, PlayerInput::Control(Control::Close));
    if video {
        let play = if frame.paused { (Icon::Play, "Play") } else { (Icon::Pause, "Pause") };
        clicked(at.back, Icon::SeekBack, "Back 10 Seconds", None, true, PlayerInput::Control(Control::Back));
        clicked(at.play, play.0, play.1, None, true, PlayerInput::Control(Control::PlayPause));
        clicked(at.forward, Icon::SeekForward, "Forward 10 Seconds", None, true, PlayerInput::Control(Control::Forward));
        let mute = if frame.muted { (Icon::SpeakerMuted, "Unmute") } else { (Icon::Speaker, "Mute") };
        clicked(at.mute, mute.0, mute.1, None, true, PlayerInput::Control(Control::Mute));
    }
    let status = match frame.status {
        Status::Ended => Some(ENDED),
        Status::OutOfView => Some(OUT_OF_VIEW),
        Status::Playing => None,
    };
    if let (true, Some(notice)) = (shown, &frame.notice) {
        let width = at.mute.left() - window.left() - 2.0 * INSET;
        style::paint_wrapped(&painter, pos2(window.left() + INSET, at.mute.top() - 14.0), width.max(40.0), false, notice, 11.0, style::weight::REGULAR, Color32::WHITE, 14.0);
    } else if let (true, Some(status)) = (shown, status) {
        // Over: in the middle, where the playback controls were. Out of view: beside Mute.
        let galley = painter.layout_job(egui::epaint::text::LayoutJob::single_section(status.into(), style::format(13.0, style::weight::MEDIUM, Color32::WHITE)));
        let pos = if frame.status == Status::Ended { window.center() - galley.size() / 2.0 } else { pos2(window.left() + INSET + 2.0, at.mute.center().y - galley.size().y / 2.0) };
        painter.galley(pos, galley, Color32::WHITE);
    }
    if surface.inputs.len() != before { ctx.request_repaint_of(egui::ViewportId::ROOT); }
}

/// A round icon control, white on the video, with its name as a tooltip.
fn control(ui: &mut Ui, rect: Rect, icon: Icon, label: &str, selected: Option<bool>, enabled: bool, shown: bool) -> bool {
    let response = ui.interact(rect, Id::unique(("pip-control", label)), if enabled { Sense::click() } else { Sense::hover() });
    response.widget_info(|| match selected {
        Some(on) => egui::WidgetInfo::selected(egui::Role::Button, enabled, on, label),
        None => egui::WidgetInfo::labeled(egui::Role::Button, enabled, label),
    });
    if !shown { return false; }
    let painter = ui.painter();
    if enabled && (response.hovered() || response.is_pointer_button_down_on()) {
        painter.rect_filled(rect, rect.height() / 2.0, Color32::from_white_alpha(40));
    }
    if response.has_focus() {
        painter.rect_stroke(rect.expand(2.0), rect.height() / 2.0 + 2.0, Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Outside);
    }
    let color = if enabled { Color32::WHITE } else { Color32::from_white_alpha(90) };
    icons::draw(painter, Rect::from_center_size(rect.center(), Vec2::splat(rect.height() * 0.6)), icon, color);
    if enabled && response.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
    let response = response.on_hover_text(label);
    enabled && response.clicked()
}

#[cfg(test)]
#[path = "pip_tests.rs"]
mod tests;
