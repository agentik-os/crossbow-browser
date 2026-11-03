//! Lucide 0.468.0, rendered from its bundled outline font at native scale.
//! Glyphs come from lucide-static/font/info.json; see resources/fonts for licensing.

use egui::{Color32, FontFamily, FontId, Painter, Rect, Stroke, Vec2};

pub const FONT: &str = "lucide";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Back,
    Forward,
    Reload,
    Stop,
    Plus,
    Close,
    Sidebar,
    Search,
    Gear,
    Archive,
    Globe,
    Folder,
    FolderOpen,
    Pin,
    Star,
    Split,
    Lock,
    Download,
    Speaker,
    SpeakerMuted,
    More,
    Check,
    Sync,
    Expand,
    Copy,
    Sparkle,
    Chevron,
    Key,
    Blocked,
    Shield,
    PinOff,
    Play,
    Pause,
    SeekBack,
    SeekForward,
    BackToTab,
    Trash,
}

impl Icon {
    fn glyph(self) -> char {
        match self {
            Self::Back => '\u{e04c}',       // arrow-left
            Self::Forward => '\u{e04d}',    // arrow-right
            Self::Reload => '\u{e14c}',     // rotate-cw
            Self::Stop => '\u{e16a}',       // square
            Self::Plus => '\u{e140}',       // plus
            Self::Close => '\u{e1b1}',      // x
            Self::Sidebar => '\u{e12d}',    // panel-left
            Self::Search => '\u{e154}',     // search
            Self::Gear => '\u{e157}',       // settings
            Self::Archive => '\u{e045}',    // archive
            Self::Globe => '\u{e0eb}',      // globe
            Self::Folder => '\u{e0dc}',     // folder
            Self::FolderOpen => '\u{e246}', // folder-open
            Self::Pin => '\u{e258}',        // pin
            Self::Star => '\u{e179}',       // star
            Self::Split => '\u{e09c}',      // columns-2
            Self::Lock => '\u{e535}',       // lock-keyhole
            Self::Download => '\u{e0b6}',   // download
            Self::Speaker => '\u{e1aa}',    // volume-2
            Self::SpeakerMuted => '\u{e1ab}', // volume-x
            Self::More => '\u{e0ba}',       // ellipsis
            Self::Check => '\u{e070}',      // check
            Self::Sync => '\u{e148}',       // refresh-cw
            Self::Expand => '\u{e116}',     // maximize-2
            Self::Copy => '\u{e0a2}',       // copy
            Self::Sparkle => '\u{e416}',    // sparkles
            Self::Chevron => '\u{e073}',   // chevron-right
            Self::Key => '\u{e4a7}',        // key-round
            Self::Blocked => '\u{e15c}',    // shield-ban
            Self::Shield => '\u{e15b}',     // shield
            Self::PinOff => '\u{e2b5}',     // pin-off
            Self::Play => '\u{e13f}',       // play
            Self::Pause => '\u{e131}',      // pause
            Self::SeekBack => '\u{e14b}',   // rotate-ccw
            Self::SeekForward => '\u{e14c}', // rotate-cw
            Self::BackToTab => '\u{e5a7}',  // square-arrow-out-up-left
            Self::Trash => '\u{e18d}',      // trash-2
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Back => "Back",
            Self::Forward => "Forward",
            Self::Reload => "Reload",
            Self::Stop => "Stop loading",
            Self::Plus => "Add",
            Self::Close => "Close",
            Self::Sidebar => "Toggle sidebar",
            Self::Search => "Search",
            Self::Gear => "Settings",
            Self::Archive => "Archive",
            Self::Globe => "Website",
            Self::Folder => "Folder",
            Self::FolderOpen => "Open folder",
            Self::Pin => "Pin",
            Self::Star => "Favorite",
            Self::Split => "Split view",
            Self::Lock => "Connection security",
            Self::Download => "Downloads",
            Self::Speaker => "Mute",
            Self::SpeakerMuted => "Unmute",
            Self::More => "More options",
            Self::Check => "Confirm",
            Self::Sync => "Sync",
            Self::Expand => "Expand",
            Self::Copy => "Copy",
            Self::Sparkle => "Boosts",
            Self::Chevron => "Expand item",
            Self::Key => "Passwords",
            Self::Blocked => "Site permissions",
            Self::Shield => "Privacy",
            Self::PinOff => "Unpin",
            Self::Play => "Play",
            Self::Pause => "Pause",
            Self::SeekBack => "Back 10 seconds",
            Self::SeekForward => "Forward 10 seconds",
            Self::BackToTab => "Back to tab",
            Self::Trash => "Delete",
        }
    }
}

pub fn draw(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let font = FontId::new(rect.width().min(rect.height()), FontFamily::Name(FONT.into()));
    let galley = p.layout_no_wrap(icon.glyph().to_string(), font, color);
    let origin = rect.center() - galley.mesh_bounds.center().to_vec2();
    p.galley(origin, galley, color);
}

/// A rounded icon button. Native window controls are deliberately not drawn here.
pub fn button(ui: &mut egui::Ui, rect: Rect, icon: Icon, color: Color32, hover_bg: Color32, enabled: bool) -> egui::Response {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let resp = ui.interact(rect, ui.scope_id().with(("icon", rect.min.x as i32, rect.min.y as i32, icon as u8)), sense);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, icon.label()));
    let p = ui.painter();
    if enabled && (resp.hovered() || resp.is_pointer_button_down_on()) {
        p.rect_filled(rect, 6.0, hover_bg);
    }
    if enabled && resp.has_focus() {
        p.rect_stroke(rect.expand(2.0), 8.0, Stroke::new(1.5, color), egui::StrokeKind::Outside);
    }
    let col = if enabled { color } else { color.gamma_multiply(0.35) };
    draw(p, Rect::from_center_size(rect.center(), Vec2::splat(rect.height() * 0.6)), icon, col);
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lucide_glyphs_exist_and_fit_their_icon_boxes() {
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let font = FontId::new(18.0, FontFamily::Name(FONT.into()));
            for icon in [
                Icon::Back, Icon::Forward, Icon::Reload, Icon::Stop, Icon::Plus, Icon::Close,
                Icon::Sidebar, Icon::Search, Icon::Gear, Icon::Archive, Icon::Globe,
                Icon::Folder, Icon::FolderOpen, Icon::Pin, Icon::Star, Icon::Split,
                Icon::Lock, Icon::Download, Icon::Speaker, Icon::SpeakerMuted, Icon::More,
                Icon::Check, Icon::Sync, Icon::Expand, Icon::Copy, Icon::Sparkle, Icon::Chevron, Icon::Key,
                Icon::Blocked, Icon::Shield,
                Icon::PinOff, Icon::Play, Icon::Pause, Icon::SeekBack, Icon::SeekForward, Icon::BackToTab, Icon::Trash,
            ] {
                assert!(ui.fonts_mut(|f| f.has_glyph(&font, icon.glyph())), "missing {icon:?}");
                let galley = ui.painter().layout_no_wrap(icon.glyph().to_string(), font.clone(), Color32::WHITE);
                assert!(galley.mesh_bounds.width() > 0.0 && galley.mesh_bounds.height() > 0.0);
                assert!(galley.mesh_bounds.width() <= 20.0 && galley.mesh_bounds.height() <= 20.0, "oversized {icon:?}");
            }
        });
        output.textures_delta.clear();
    }
}
