//! Crossbow's design system: the type scale on Inter's variable weights, ink
//! colours computed for contrast against whatever surface they sit on, motion
//! and radius tokens, and the controls every surface is drawn with (capsule
//! buttons, fields, the six-digit code input, segmented controls, sliders,
//! swatches). See DESIGN.md for the rules behind the numbers.

use crate::ui::anim;
use crate::ui::icons::{self, Icon};
use egui::epaint::text::{LayoutJob, TextFormat, VariationCoords};
use egui::{Align2, Color32, FontFamily, FontId, Id, Painter, Pos2, Rect, Response, RichText, Rgba, Sense, Stroke, Ui, Vec2, pos2, vec2};

// -- tokens ----------------------------------------------------------------------------

pub mod weight {
    pub const REGULAR: f32 = 400.0;
    pub const MEDIUM: f32 = 500.0;
    pub const SEMIBOLD: f32 = 600.0;
    pub const BOLD: f32 = 700.0;
}

pub mod radius {
    /// Rows, chips and small controls.
    pub const SM: f32 = 8.0;
    /// Fields, list groups.
    pub const MD: f32 = 12.0;
    /// Popovers and menus.
    pub const LG: f32 = 16.0;
    /// Sheets and the onboarding card.
    pub const XL: f32 = 26.0;
}

pub mod motion {
    /// Hover and press responses.
    pub const FAST: f32 = 0.12;
    /// Exits.
    pub const EXIT: f32 = 0.15;
    /// Entrances of panels and step content.
    pub const ENTER: f32 = 0.22;
    /// Larger spatial changes (sidebar, sheets).
    pub const SPATIAL: f32 = 0.28;
}

// -- type ------------------------------------------------------------------------------

/// Inter already includes optical-size spacing; keep its native tracking.
pub fn tracking(_size: f32) -> f32 {
    0.0
}

pub fn coords(size: f32, wght: f32) -> VariationCoords {
    VariationCoords::new([(b"wght", wght), (b"opsz", size)])
}

/// Text at a size and weight, with the matching optical size and tracking.
pub fn text(s: impl Into<String>, size: f32, wght: f32) -> RichText {
    RichText::new(s).size(size).variation(b"wght", wght).variation(b"opsz", size).extra_letter_spacing(tracking(size))
}

pub fn format(size: f32, wght: f32, color: Color32) -> TextFormat {
    let mut f = TextFormat::simple(FontId::proportional(size), color);
    f.coords = coords(size, wght);
    f.extra_letter_spacing = tracking(size);
    f
}

pub fn mono_format(size: f32, wght: f32, color: Color32) -> TextFormat {
    let mut f = TextFormat::simple(FontId::new(size, FontFamily::Monospace), color);
    f.coords = VariationCoords::new([(b"wght", wght)]);
    f
}

/// Paint one line of text; returns the rect it occupies.
pub fn paint(p: &Painter, pos: Pos2, align: Align2, s: &str, size: f32, wght: f32, color: Color32) -> Rect {
    let job = LayoutJob::single_section(s.to_owned(), format(size, wght, color));
    let galley = p.layout_job(job);
    let rect = align.anchor_size(pos, galley.size());
    p.galley(rect.min, galley, color);
    rect
}

/// Paint wrapped, centred or left-aligned text inside `width`; returns its rect.
pub fn paint_wrapped(p: &Painter, top_left: Pos2, width: f32, centered: bool, s: &str, size: f32, wght: f32, color: Color32, line_height: f32) -> Rect {
    let mut f = format(size, wght, color);
    f.line_height = Some(line_height);
    let mut job = LayoutJob::single_section(s.to_owned(), f);
    job.wrap.max_width = width;
    if centered {
        job.halign = egui::Align::Center;
    }
    let galley = p.layout_job(job);
    let x = if centered { top_left.x + width / 2.0 } else { top_left.x };
    let rect = Rect::from_min_size(pos2(if centered { x - galley.size().x / 2.0 } else { x }, top_left.y), galley.size());
    p.galley(pos2(x, top_left.y), galley, color);
    rect
}

pub fn measure(p: &Painter, s: &str, size: f32, wght: f32) -> Vec2 {
    p.layout_job(LayoutJob::single_section(s.to_owned(), format(size, wght, Color32::WHITE))).size()
}

// -- colour ----------------------------------------------------------------------------

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    Color32::from(Rgba::from(a) * (1.0 - t) + Rgba::from(b) * t)
}

pub fn luminance(c: Color32) -> f32 {
    let l = Rgba::from(c);
    0.2126 * l.r() + 0.7152 * l.g() + 0.0722 * l.b()
}

/// WCAG contrast ratio between two opaque colours.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Composite a translucent colour over an opaque one.
pub fn over(top: Color32, under: Color32) -> Color32 {
    let a = top.a() as f32 / 255.0;
    let [r, g, b, _] = top.to_srgba_unmultiplied();
    mix(under, Color32::from_rgb(r, g, b), a)
}

/// Move `fg` toward `target` until it reaches `ratio` against `bg`.
fn ensure_contrast(mut fg: Color32, target: Color32, bg: Color32, ratio: f32) -> Color32 {
    for _ in 0..24 {
        if contrast(fg, bg) >= ratio {
            break;
        }
        fg = mix(fg, target, 0.12);
    }
    fg
}

/// Text and control colours for one surface. `surface` is the opaque colour
/// the text actually lands on (glass composited over what is behind it).
#[derive(Clone, Copy, Debug)]
pub struct Ink {
    pub dark: bool,
    pub surface: Color32,
    pub text: Color32,
    pub secondary: Color32,
    pub tertiary: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    /// Filled controls (secondary buttons, fields, segmented tracks).
    pub fill: Color32,
    pub fill_hover: Color32,
    pub fill_strong: Color32,
    pub separator: Color32,
    /// High-contrast primary buttons: ink on the surface.
    pub primary: Color32,
    pub on_primary: Color32,
    pub danger: Color32,
    pub success: Color32,
}

impl Ink {
    pub fn new(surface: Color32, accent: Color32) -> Ink {
        let dark = luminance(surface) < 0.22;
        let text = if dark { Color32::from_rgb(0xf5, 0xf5, 0xf7) } else { Color32::from_rgb(0x1d, 0x1d, 0x1f) };
        // Secondary text is the ink pulled toward the surface and tinted with
        // the accent's hue, then pushed back until it clears 4.5:1.
        let tinted = mix(mix(text, surface, 0.42), accent, 0.10);
        let secondary = ensure_contrast(tinted, text, surface, 4.6);
        let tertiary = ensure_contrast(mix(text, surface, 0.55), text, surface, 3.0);
        // The accent has to read as a fill under white text.
        let mut acc = accent;
        for _ in 0..10 {
            if contrast(Color32::WHITE, acc) >= 4.5 {
                break;
            }
            acc = mix(acc, Color32::BLACK, 0.1);
        }
        let (fill, fill_hover, fill_strong, separator) = if dark {
            (Color32::from_white_alpha(20), Color32::from_white_alpha(30), Color32::from_white_alpha(44), Color32::from_white_alpha(22))
        } else {
            (Color32::from_black_alpha(10), Color32::from_black_alpha(17), Color32::from_black_alpha(28), Color32::from_black_alpha(18))
        };
        Ink {
            dark,
            surface,
            text,
            secondary,
            tertiary,
            accent: acc,
            on_accent: Color32::WHITE,
            fill,
            fill_hover,
            fill_strong,
            separator,
            primary: text,
            on_primary: if dark { Color32::from_rgb(0x1d, 0x1d, 0x1f) } else { Color32::WHITE },
            danger: if dark { Color32::from_rgb(0xff, 0x6b, 0x6b) } else { Color32::from_rgb(0xc8, 0x2a, 0x2a) },
            success: if dark { Color32::from_rgb(0x4c, 0xd9, 0x7b) } else { Color32::from_rgb(0x1a, 0x8f, 0x4a) },
        }
    }
}

/// Apply the ink to egui's own widgets (text edits, scroll bars, selection).
pub fn apply_visuals(ui: &mut Ui, ink: &Ink) {
    let mut v = if ink.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.override_text_color = Some(ink.text);
    v.selection.bg_fill = ink.accent.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, ink.text);
    v.text_cursor.stroke = Stroke::new(2.0, ink.accent);
    v.hyperlink_color = ink.accent;
    v.extreme_bg_color = Color32::TRANSPARENT;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, ink.separator);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = egui::CornerRadius::same(radius::SM as u8);
        w.bg_stroke = Stroke::NONE;
    }
    v.widgets.inactive.weak_bg_fill = ink.fill;
    v.widgets.inactive.bg_fill = ink.fill;
    v.widgets.hovered.weak_bg_fill = ink.fill_hover;
    v.widgets.hovered.bg_fill = ink.fill_hover;
    v.widgets.active.weak_bg_fill = ink.fill_strong;
    v.widgets.active.bg_fill = ink.fill_strong;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, ink.text);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, ink.text);
    v.widgets.active.fg_stroke = Stroke::new(1.0, ink.text);
    ui.style_mut().visuals = v;
    ui.style_mut().spacing.scroll.floating = true;
    ui.style_mut().spacing.scroll.bar_width = 6.0;
}

// -- controls --------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Ink fill: the one action a surface is for.
    Primary,
    /// Accent fill: confirmations tied to the space colour.
    Accent,
    /// Quiet fill.
    Secondary,
    /// Text only.
    Plain,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Large,
    Regular,
    Small,
}

impl Size {
    fn metrics(self) -> (f32, f32, f32, f32) {
        // height, font size, horizontal padding, weight
        match self {
            Size::Large => (44.0, 15.0, 22.0, weight::SEMIBOLD),
            Size::Regular => (32.0, 13.0, 14.0, weight::MEDIUM),
            Size::Small => (26.0, 12.0, 11.0, weight::MEDIUM),
        }
    }
}

pub struct Button<'a> {
    label: &'a str,
    kind: Kind,
    size: Size,
    icon: Option<Icon>,
    trailing_icon: Option<Icon>,
    min_width: f32,
    enabled: bool,
    loading: bool,
}

impl<'a> Button<'a> {
    pub fn new(label: &'a str) -> Self {
        Button { label, kind: Kind::Secondary, size: Size::Regular, icon: None, trailing_icon: None, min_width: 0.0, enabled: true, loading: false }
    }
    pub fn kind(mut self, k: Kind) -> Self {
        self.kind = k;
        self
    }
    pub fn size(mut self, s: Size) -> Self {
        self.size = s;
        self
    }
    pub fn icon(mut self, i: Icon) -> Self {
        self.icon = Some(i);
        self
    }
    pub fn trailing(mut self, i: Icon) -> Self {
        self.trailing_icon = Some(i);
        self
    }
    pub fn min_width(mut self, w: f32) -> Self {
        self.min_width = w;
        self
    }
    pub fn enabled(mut self, e: bool) -> Self {
        self.enabled = e;
        self
    }
    pub fn loading(mut self, l: bool) -> Self {
        self.loading = l;
        self
    }

    fn width(&self, p: &Painter) -> f32 {
        let (h, fs, pad, w) = self.size.metrics();
        let text_w = if self.label.is_empty() { 0.0 } else { measure(p, self.label, fs, w).x };
        let icons = [self.icon, self.trailing_icon].iter().flatten().count() as f32 * (fs + 6.0);
        if self.label.is_empty() { h } else { (text_w + icons + pad * 2.0).max(self.min_width) }
    }

    pub fn show(self, ui: &mut Ui, ink: &Ink) -> Response {
        let w = self.width(ui.painter());
        let (h, ..) = self.size.metrics();
        let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
        self.show_at(ui, ink, rect)
    }

    pub fn show_at(self, ui: &mut Ui, ink: &Ink, rect: Rect) -> Response {
        let (h, fs, _, wght) = self.size.metrics();
        let id = ui.next_auto_id().with("btn");
        ui.skip_ahead_auto_ids(1);
        let active = self.enabled && !self.loading;
        let resp = ui.interact(rect, id, if active { Sense::click() } else { Sense::hover() });
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, active, self.label));
        if resp.gained_focus() {
            ui.scroll_to_rect(rect.expand(5.0), None);
        }
        let hovered = active && resp.hovered();
        let pressed = active && resp.is_pointer_button_down_on();
        // Press feedback lands on pointer-down: a 0.97 squeeze, released with a spring.
        let squeeze = anim::spring(ui.ctx(), id.with("press"), if pressed { 1.0 } else { 0.0 }, 900.0);
        let hover = anim::spring(ui.ctx(), id.with("hover"), if hovered { 1.0 } else { 0.0 }, 700.0);
        let r = Rect::from_center_size(rect.center(), rect.size() * (1.0 - 0.03 * squeeze));
        let p = ui.painter();
        let alpha = if self.enabled { 1.0 } else { 0.38 };
        let (bg, fg) = match self.kind {
            Kind::Primary => (mix(ink.primary, ink.surface, 0.12 * hover), ink.on_primary),
            Kind::Accent => (mix(ink.accent, Color32::BLACK, 0.10 * hover), ink.on_accent),
            Kind::Secondary => (mix(ink.fill, ink.fill_strong, hover), ink.text),
            Kind::Plain => (if hover > 0.01 { ink.fill.gamma_multiply(hover) } else { Color32::TRANSPARENT }, ink.text),
        };
        p.rect_filled(r, r.height() / 2.0, bg.gamma_multiply(alpha));
        if resp.has_focus() {
            p.rect_stroke(r.shrink(1.0), r.height() / 2.0 - 1.0, Stroke::new(2.0, ink.accent), egui::StrokeKind::Inside);
        }
        let fg = fg.gamma_multiply(alpha);
        if self.loading {
            spinner(p, r.center(), fs * 0.55, fg, ui.input(|i| i.time));
            ui.ctx().request_repaint();
        } else {
            let text_w = if self.label.is_empty() { 0.0 } else { measure(p, self.label, fs, wght).x };
            let icon_w = fs + 6.0;
            let total = text_w + [self.icon, self.trailing_icon].iter().flatten().count() as f32 * icon_w - if self.label.is_empty() { 6.0 } else { 0.0 };
            let mut x = r.center().x - total / 2.0;
            if let Some(i) = self.icon {
                icons::draw(p, Rect::from_center_size(pos2(x + fs / 2.0, r.center().y), Vec2::splat(fs * 0.95)), i, fg);
                x += icon_w;
            }
            if !self.label.is_empty() {
                paint(p, pos2(x, r.center().y), Align2::LEFT_CENTER, self.label, fs, wght, fg);
                x += text_w + 6.0;
            }
            if let Some(i) = self.trailing_icon {
                icons::draw(p, Rect::from_center_size(pos2(x + fs / 2.0, r.center().y), Vec2::splat(fs * 0.8)), i, fg);
            }
        }
        let _ = h;
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }
}

/// A rotating arc, used inside buttons and next to async work.
pub fn spinner(p: &Painter, c: Pos2, r: f32, color: Color32, time: f64) {
    let start = (time * 5.0) as f32;
    let pts: Vec<Pos2> = (0..=20)
        .map(|i| {
            let a = start + i as f32 / 20.0 * std::f32::consts::PI * 1.4;
            c + vec2(a.cos(), a.sin()) * r
        })
        .collect();
    p.add(egui::Shape::line(pts, Stroke::new((r * 0.22).max(1.5), color)));
}

/// Shared label-to-field spacing, independent of the surrounding row spacing.
pub fn field_label(ui: &mut Ui, ink: &Ink, label: &str) {
    ui.label(text(label, 13.0, weight::SEMIBOLD).color(ink.text));
    ui.add_space((8.0 - ui.spacing().item_spacing.y).max(0.0));
}

/// A large rounded text field drawn in the house style. Returns the TextEdit response.
pub fn field(ui: &mut Ui, ink: &Ink, id: Id, value: &mut String, hint: &str, width: f32, error: bool) -> Response {
    let h = 46.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, h), Sense::hover());
    let p = ui.painter().clone();
    let focused = ui.memory(|m| m.has_focus(id));
    let f = anim::spring(ui.ctx(), id.with("focus"), if focused { 1.0 } else { 0.0 }, 900.0);
    p.rect_filled(rect, radius::MD, mix(ink.fill, ink.fill_hover, f));
    let ring = if error { ink.danger } else { ink.accent };
    if f > 0.01 || error {
        let a = if error { 1.0 } else { f };
        p.rect_stroke(rect, radius::MD, Stroke::new(1.5, ring.gamma_multiply(a)), egui::StrokeKind::Inside);
    }
    let inner = rect.shrink2(vec2(14.0, 0.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let te = egui::TextEdit::singleline(value)
        .id(id)
        .frame(egui::Frame::NONE)
        .font(FontId::proportional(16.0))
        .text_color(ink.text)
        .hint_text(text(hint, 16.0, weight::REGULAR).color(ink.secondary))
        .desired_width(inner.width())
        .align(Align2::LEFT_CENTER);
    child.add(te)
}

/// Six boxes for a one-time code. Typing, pasting and backspace all work;
/// returns (response of the hidden field, true when all digits are filled).
pub fn code_input(ui: &mut Ui, ink: &Ink, id: Id, value: &mut String, len: usize, error: bool) -> (Response, bool) {
    let count = len.max(1) as f32;
    let total = (48.0 * count + 10.0 * (count - 1.0)).min(ui.available_width().max(0.0));
    let gap = 10.0_f32.min(total / (count * 4.0));
    let box_w = (total - gap * (count - 1.0)) / count;
    let (rect, _) = ui.allocate_exact_size(vec2(total, 58.0), Sense::hover());
    // A hidden single-line edit captures keys, IME and paste.
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    child.style_mut().visuals.text_cursor.stroke = Stroke::NONE;
    child.style_mut().visuals.selection.bg_fill = Color32::TRANSPARENT;
    let resp = child.add(
        egui::TextEdit::singleline(value)
            .id(id)
            .frame(egui::Frame::NONE)
            .text_color(Color32::TRANSPARENT)
            .desired_width(total)
            .min_size(rect.size())
            .char_limit(64),
    );
    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).take(len).collect();
    if *value != digits {
        *value = digits;
    }
    let p = ui.painter();
    // Cover the hidden edit's caret and selection.
    let focused = resp.has_focus();
    let t = ui.input(|i| i.time);
    for i in 0..len {
        let r = Rect::from_min_size(pos2(rect.left() + i as f32 * (box_w + gap), rect.top()), vec2(box_w, 58.0));
        let current = focused && i == value.len().min(len - 1);
        p.rect_filled(r, radius::MD, if current { ink.fill_hover } else { ink.fill });
        let ring = if error { Some(ink.danger) } else if current { Some(ink.accent) } else { None };
        if let Some(c) = ring {
            p.rect_stroke(r, radius::MD, Stroke::new(1.5, c), egui::StrokeKind::Inside);
        }
        if let Some(ch) = value.chars().nth(i) {
            paint(p, r.center(), Align2::CENTER_CENTER, &ch.to_string(), 24.0, weight::SEMIBOLD, ink.text);
        } else if current && (t * 2.0) as i64 % 2 == 0 {
            p.line_segment([pos2(r.center().x, r.center().y - 11.0), pos2(r.center().x, r.center().y + 11.0)], Stroke::new(2.0, ink.accent));
        }
    }
    if focused {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
    }
    let done = value.len() == len;
    (resp, done)
}

/// iOS-style segmented control with a thumb that slides between options.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, ink: &Ink, id: Id, value: &mut T, options: &[(T, &str, Option<Icon>)], width: f32) -> bool {
    let h = 32.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, h), Sense::hover());
    let p = ui.painter().clone();
    p.rect_filled(rect, h / 2.0, ink.fill);
    let n = options.len().max(1) as f32;
    let seg_w = (rect.width() - 4.0) / n;
    let idx = options.iter().position(|(v, ..)| v == value).unwrap_or(0) as f32;
    let x = anim::spring(ui.ctx(), id.with("thumb"), idx, 600.0);
    let thumb = Rect::from_min_size(pos2(rect.left() + 2.0 + x * seg_w, rect.top() + 2.0), vec2(seg_w, h - 4.0));
    p.add(egui::epaint::Shadow { offset: [0, 1], blur: 4, spread: 0, color: Color32::from_black_alpha(if ink.dark { 60 } else { 22 }) }.as_shape(thumb, egui::CornerRadius::same(((h - 4.0) / 2.0) as u8)));
    p.rect_filled(thumb, (h - 4.0) / 2.0, if ink.dark { Color32::from_white_alpha(46) } else { Color32::WHITE });
    let mut changed = false;
    for (i, (v, label, icon)) in options.iter().enumerate() {
        let r = Rect::from_min_size(pos2(rect.left() + 2.0 + i as f32 * seg_w, rect.top() + 2.0), vec2(seg_w, h - 4.0));
        let resp = ui.interact(r, id.with(i), Sense::click());
        let sel = v == value;
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, ui.is_enabled(), sel, *label));
        if resp.gained_focus() {
            ui.scroll_to_rect_animation(r.expand(5.0), None, egui::style::ScrollAnimation::none());
        }
        if resp.has_focus() {
            p.rect_stroke(r.expand(2.0), (h - 4.0) / 2.0 + 2.0, Stroke::new(2.0, ink.accent), egui::StrokeKind::Outside);
        }
        let col = if sel { ink.text } else { ink.secondary };
        let tw = if label.is_empty() { 0.0 } else { measure(&p, label, 12.5, if sel { weight::SEMIBOLD } else { weight::MEDIUM }).x };
        let iw = if icon.is_some() { 14.0 + if label.is_empty() { 0.0 } else { 6.0 } } else { 0.0 };
        let mut cx = r.center().x - (tw + iw) / 2.0;
        if let Some(ic) = icon {
            icons::draw(&p, Rect::from_center_size(pos2(cx + 7.0, r.center().y), Vec2::splat(14.0)), *ic, col);
            cx += iw;
        }
        if !label.is_empty() {
            paint(&p, pos2(cx, r.center().y), Align2::LEFT_CENTER, label, 12.5, if sel { weight::SEMIBOLD } else { weight::MEDIUM }, col);
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked() && !sel {
            *value = *v;
            changed = true;
        }
    }
    changed
}

/// A slider with a capsule track and a white knob that tracks the pointer 1:1.
/// `track` optionally paints the track as a gradient (e.g. intensity).
pub fn slider(ui: &mut Ui, ink: &Ink, id: Id, value: &mut f32, width: f32, track: Option<(Color32, Color32)>) -> bool {
    let h = 24.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, h), Sense::hover());
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let track_r = Rect::from_center_size(rect.center(), vec2(rect.width() - 20.0, 6.0));
    let mut changed = false;
    if let Some(pos) = resp.interact_pointer_pos() {
        if resp.dragged() || resp.clicked() || resp.is_pointer_button_down_on() {
            let v = ((pos.x - track_r.left()) / track_r.width()).clamp(0.0, 1.0);
            if (v - *value).abs() > f32::EPSILON {
                *value = v;
                changed = true;
            }
        }
    }
    if resp.has_focus() {
        let step = ui.input(|i| (i.key_pressed(egui::Key::ArrowRight) as i32 - i.key_pressed(egui::Key::ArrowLeft) as i32) as f32 * 0.05);
        if step != 0.0 {
            *value = (*value + step).clamp(0.0, 1.0);
            changed = true;
        }
    }
    let p = ui.painter();
    match track {
        Some((a, b)) => {
            let mut mesh = egui::Mesh::default();
            mesh.colored_vertex(track_r.left_top(), a);
            mesh.colored_vertex(track_r.right_top(), b);
            mesh.colored_vertex(track_r.left_bottom(), a);
            mesh.colored_vertex(track_r.right_bottom(), b);
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(1, 3, 2);
            p.add(mesh);
            p.circle_filled(track_r.left_center(), 3.0, a);
            p.circle_filled(track_r.right_center(), 3.0, b);
        }
        None => {
            p.rect_filled(track_r, 3.0, ink.fill_strong);
            let fill = Rect::from_min_max(track_r.min, pos2(track_r.left() + track_r.width() * *value, track_r.bottom()));
            p.rect_filled(fill, 3.0, ink.accent);
        }
    }
    let held = resp.is_pointer_button_down_on() || resp.dragged();
    let grow = anim::spring(ui.ctx(), id.with("knob"), if held { 1.0 } else { 0.0 }, 800.0);
    let c = pos2(track_r.left() + track_r.width() * *value, rect.center().y);
    let kr = 10.0 + grow * 1.5;
    p.add(egui::epaint::Shadow { offset: [0, 1], blur: 6, spread: 0, color: Color32::from_black_alpha(55) }.as_shape(Rect::from_center_size(c, Vec2::splat(kr * 2.0)), egui::CornerRadius::same(kr as u8)));
    p.circle_filled(c, kr, Color32::WHITE);
    if resp.has_focus() {
        p.circle_stroke(c, kr + 3.0, Stroke::new(2.0, ink.accent.gamma_multiply(0.7)));
    }
    if resp.hovered() || held {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    changed
}

/// A circular swatch; the selection ring is concentric with the swatch.
pub fn swatch(ui: &mut Ui, ink: &Ink, id: Id, colors: &[Color32], selected: bool, d: f32) -> Response {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(d + 10.0), Sense::hover());
    let resp = ui.interact(rect, id, Sense::click());
    let pressed = resp.is_pointer_button_down_on();
    let s = anim::spring(ui.ctx(), id.with("press"), if pressed { 1.0 } else { 0.0 }, 900.0);
    let sel = anim::spring(ui.ctx(), id.with("sel"), if selected { 1.0 } else { 0.0 }, 500.0);
    let c = rect.center();
    let r = d / 2.0 * (1.0 - 0.06 * s) - 2.5 * sel;
    let p = ui.painter();
    circle_gradient(p, c, r, colors);
    p.circle_stroke(c, r, Stroke::new(1.0, Color32::from_black_alpha(if ink.dark { 60 } else { 26 })));
    if sel > 0.01 {
        p.circle_stroke(c, d / 2.0 + 2.0, Stroke::new(2.0, ink.text.gamma_multiply(sel)));
    } else if resp.hovered() {
        p.circle_stroke(c, d / 2.0 + 2.0, Stroke::new(2.0, ink.fill_strong));
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// A disc filled with a diagonal gradient through `colors`.
pub fn circle_gradient(p: &Painter, c: Pos2, r: f32, colors: &[Color32]) {
    let n = 48;
    let mut mesh = egui::Mesh::default();
    let sample = |t: f32| -> Color32 {
        match colors.len() {
            0 => Color32::GRAY,
            1 => colors[0],
            k => {
                let x = t.clamp(0.0, 1.0) * (k - 1) as f32;
                let i = (x.floor() as usize).min(k - 2);
                mix(colors[i], colors[i + 1], x - i as f32)
            }
        }
    };
    mesh.colored_vertex(c, sample(0.5));
    for i in 0..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        let v = vec2(a.cos(), a.sin());
        // Project on the 135° diagonal: top-left → bottom-right.
        let t = 0.5 + (v.x + v.y) * 0.3536;
        mesh.colored_vertex(c + v * r, sample(t));
    }
    for i in 1..=n as u32 {
        mesh.add_triangle(0, i, i + 1);
    }
    p.add(mesh);
}

/// A keycap: rounded, slightly raised.
pub fn keycap(p: &Painter, ink: &Ink, rect: Rect, label: &str, lit: f32) {
    let base = mix(ink.fill_hover, ink.accent, lit * 0.9);
    p.rect_filled(rect.translate(vec2(0.0, 1.5)), radius::SM, ink.fill_strong);
    p.rect_filled(rect, radius::SM, base);
    let col = mix(ink.text, Color32::WHITE, lit);
    paint(p, rect.center(), Align2::CENTER_CENTER, label, 15.0, weight::MEDIUM, col);
}

/// A filled circle with a check, drawn on as `t` goes 0→1.
pub fn check_badge(p: &Painter, c: Pos2, r: f32, bg: Color32, fg: Color32, t: f32) {
    let t = t.clamp(0.0, 1.0);
    p.circle_filled(c, r * (0.9 + 0.1 * t), bg.gamma_multiply(t.min(1.0)));
    let k = anim::ease_out_strong(t);
    let a = c + vec2(-0.42, 0.02) * r;
    let b = c + vec2(-0.12, 0.32) * r;
    let e = c + vec2(0.44, -0.3) * r;
    let mut pts = vec![a];
    if k < 0.4 {
        pts.push(a + (b - a) * (k / 0.4));
    } else {
        pts.push(b);
        pts.push(b + (e - b) * ((k - 0.4) / 0.6));
    }
    p.add(egui::Shape::line(pts, Stroke::new((r * 0.2).max(1.6), fg)));
}

#[cfg(test)]
mod focus_tests {
    use super::*;

    #[test]
    fn keyboard_focus_reveals_a_clipped_button() {
        let ctx = egui::Context::default();
        let ink = Ink::new(Color32::WHITE, Color32::BLUE);
        let mut bounds = (Rect::NOTHING, Rect::NOTHING);
        let mut focused = false;
        let mut offset = 0.0;
        for frame in 0..30 {
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 420.0))),
                time: Some(frame as f64 / 30.0),
                ..Default::default()
            };
            if frame == 1 {
                input.events.push(egui::Event::Key {
                    key: egui::Key::Tab, physical_key: None, pressed: true,
                    repeat: false, modifiers: egui::Modifiers::NONE,
                });
            }
            ctx.run_ui(input, |ui| {
                let scroll = egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                    ui.add_space(180.0);
                    let response = Button::new("Get a Sign-in Code").size(Size::Large).show(ui, &ink);
                    ui.add_space(12.0);
                    response
                });
                bounds = (scroll.inner.rect, scroll.inner_rect);
                focused = scroll.inner.has_focus();
                offset = scroll.state.offset().y;
            }).textures_delta.clear();
            if frame == 0 {
                assert!(!bounds.1.contains_rect(bounds.0));
                assert_eq!(offset, 0.0);
            }
        }
        assert!(focused, "Tab must reach the actual button");
        assert!(offset > 0.0, "Keyboard focus must scroll the container");
        assert!(bounds.1.contains_rect(bounds.0), "The whole button and inset focus ring must be visible");
        assert!(bounds.0.bottom() + 4.0 <= bounds.1.bottom(), "Keep space below the focused button");
    }
}
