//! Window setup: native traffic lights on macOS, bundled Inter and Lucide.

use egui::{FontData, FontDefinitions, FontFamily, Rect, ViewportBuilder, pos2, vec2};
use std::sync::Arc;

pub fn main_viewport() -> ViewportBuilder {
    let vb = ViewportBuilder::default()
        .with_title("Crossbow")
        .with_app_id("app.crossbow.Crossbow")
        .with_inner_size([1360.0, 860.0])
        .with_min_inner_size([640.0, 420.0]);
    let vb = match image::load_from_memory(include_bytes!("../../resources/icon-256.png")) {
        Ok(img) => {
            let rgba = img.into_rgba8();
            let (width, height) = rgba.dimensions();
            vb.with_icon(egui::IconData { rgba: rgba.into_raw(), width, height })
        }
        Err(_) => vb,
    };
    window_chrome(vb)
}

/// Crossbow's own chrome for any of its windows: on macOS the content fills
/// the window under a transparent title bar, so only the traffic lights show.
pub fn window_chrome(vb: ViewportBuilder) -> ViewportBuilder {
    #[cfg(target_os = "macos")]
    let vb = vb
        .with_fullsize_content_view(true)
        .with_titlebar_shown(false)
        .with_title_shown(false)
        .with_transparent(true);
    #[cfg(not(target_os = "macos"))]
    let vb = vb.with_decorations(false).with_transparent(true);
    vb
}

/// The viewport icon is not a macOS Dock override. Restore that separately
/// after AppKit has created the main application, without touching its bundle.
pub fn restore_app_icon(icon: &mut crate::app_icon::AppIcon, ctx: &egui::Context) {
    icon.restore(ctx);
}

/// Native macOS buttons center at 16 points in a full-size content window.
/// All neighboring controls use this row, not the taller sidebar section.
pub const NATIVE_CONTROL_ROW: f32 = 32.0;
pub const SIDEBAR_HEADER: f32 = 44.0;

/// Left inset reserved for the macOS traffic lights in the sidebar's top row.
pub fn traffic_light_inset() -> f32 {
    if cfg!(target_os = "macos") { 74.0 } else { 0.0 }
}

/// Square icon controls in a top row, on a 28-point pitch.
pub const ROW_CONTROL: f32 = 26.0;
pub const ROW_PITCH: f32 = 28.0;
/// Inset of a top row's controls from its edges.
pub const ROW_PAD: f32 = 10.0;

/// The line a top row's controls sit on. When the native buttons share the
/// row, the controls centre on their line and start clear of them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlRow {
    pub center_y: f32,
    /// Left edge of the first leading control.
    pub leading: f32,
    /// Right edge of the last trailing control.
    pub trailing: f32,
}

impl ControlRow {
    /// `height` places the line when the native buttons are not in the row.
    pub fn new(rect: Rect, native_controls: bool, height: f32) -> Self {
        let center_y = rect.top() + if native_controls { NATIVE_CONTROL_ROW / 2.0 } else { height / 2.0 };
        let leading = if native_controls { (rect.left() + ROW_PAD).max(traffic_light_inset()) } else { rect.left() + ROW_PAD };
        Self { center_y, leading, trailing: rect.right() - ROW_PAD - 2.0 }
    }

    /// The `index`th control from the left.
    pub fn leading_control(&self, index: usize) -> Rect {
        Rect::from_min_size(pos2(self.leading + index as f32 * ROW_PITCH, self.center_y - ROW_CONTROL / 2.0), vec2(ROW_CONTROL, ROW_CONTROL))
    }

    /// The `index`th control from the right.
    pub fn trailing_control(&self, index: usize) -> Rect {
        let right = self.trailing - index as f32 * ROW_PITCH;
        Rect::from_min_max(pos2(right - ROW_CONTROL, self.center_y - ROW_CONTROL / 2.0), pos2(right, self.center_y + ROW_CONTROL / 2.0))
    }
}

/// Bundled fonts keep chrome consistent and usable without a network request.
/// Medium is the default for existing widgets; long-form prose opts into 400.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let inter = FontData::from_static(include_bytes!("../../resources/fonts/Inter.ttf")).tweak(egui::FontTweak {
        coords: egui::epaint::text::VariationCoords::new([(b"wght", 500.0), (b"opsz", 14.0)]),
        ..Default::default()
    });
    fonts.font_data.insert("inter".into(), Arc::new(inter));
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "inter".into());
    fonts.font_data.insert(
        super::icons::FONT.into(),
        Arc::new(FontData::from_static(include_bytes!("../../resources/fonts/lucide.ttf"))),
    );
    fonts.families.insert(FontFamily::Name(super::icons::FONT.into()), vec![super::icons::FONT.into()]);
    #[cfg(target_os = "macos")]
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf") {
        fonts.font_data.insert("system-mono".into(), Arc::new(FontData::from_owned(bytes)));
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "system-mono".into());
    }
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    #[test]
    fn inter_has_real_weight_and_optical_size_axes() {
        let font = egui::FontData::from_static(include_bytes!("../../resources/fonts/Inter.ttf"));
        let axes = font.variation_axes();
        assert!(axes.iter().any(|a| a.tag == egui::epaint::text::Tag::new(b"wght") && a.range.contains(600.0)));
        assert!(axes.iter().any(|a| a.tag == egui::epaint::text::Tag::new(b"opsz")));
    }
}
