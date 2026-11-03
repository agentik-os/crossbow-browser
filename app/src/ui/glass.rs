//! A frosted material over the space's colour. Onboarding has a smooth backdrop,
//! so translucency is sufficient; this is not a blur of web-page content.

use super::style::{self, Ink};
use egui::{Color32, Painter, Rect, Stroke, pos2};

pub fn sheet(p: &Painter, rect: Rect, dark: bool, accent: Color32, backdrop: Color32) -> Ink {
    let radius = style::radius::XL;
    let tint = if dark { Color32::from_rgb(25, 26, 32) } else { Color32::from_rgb(252, 252, 254) };
    let opacity = if dark { 0.87 } else { 0.82 };
    p.add(egui::epaint::Shadow {
        offset: [0, 18], blur: 56, spread: 0,
        color: Color32::from_black_alpha(if dark { 62 } else { 28 }),
    }.as_shape(rect, egui::CornerRadius::same(radius as u8)));
    p.rect_filled(rect, radius, tint.gamma_multiply(opacity));
    // A single light-catching upper edge, not an outline around a shadow.
    p.line_segment([
        pos2(rect.left() + radius, rect.top() + 0.5),
        pos2(rect.right() - radius, rect.top() + 0.5),
    ], Stroke::new(1.0, Color32::from_white_alpha(if dark { 40 } else { 185 })));
    Ink::new(style::mix(backdrop, tint, opacity), accent)
}
