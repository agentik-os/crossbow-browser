//! The shared, live theme controls used by onboarding and Settings.

use crate::ui::style::{self, Button, Ink, Size, weight};
use crate::ui::theme as colors;
use crossbow_core::model::{Rgb, Texture, Theme, ThemeMode};
use egui::{Align2, Color32, Rect, Response, Role, Sense, Stroke, Ui, WidgetInfo, pos2, vec2};

const PRESET_NAMES: [&str; 8] = [
    "Iris", "Lagoon", "Ember", "Fern", "Aurora", "Linen", "Dusk", "Citrus",
];
const LABEL_WIDTH: f32 = 88.0;
const ROW_HEIGHT: f32 = 32.0;

/// Edit a theme in place, returning whether its stored values changed.
///
/// The caller owns persistence and should provide a distinct parent UI ID for
/// each space. There is no internal scrolling: embed this in the host's scroll
/// area. The preview uses the same gradient and texture as the live space.
pub fn show(ui: &mut egui::Ui, theme: &mut crossbow_core::model::Theme, system_dark: bool, textures: &colors::TextureBank) -> bool {
    show_with_popup_parent(ui, theme, system_dark, textures, None)
}

pub(super) fn show_with_popup_parent(ui: &mut egui::Ui, theme: &mut crossbow_core::model::Theme, system_dark: bool, textures: &colors::TextureBank, popup_parent: Option<egui::LayerId>) -> bool {
    let before = theme.clone();
    // Older/imported themes can have no stops. Keep every editor operation in
    // the model's supported one-to-three-stop range.
    if theme.colors.is_empty() {
        theme.colors.push(Theme::preset(0).colors[0]);
    }
    theme.colors.truncate(3);

    ui.push_id("theme-editor", |ui| {
        let palette = colors::palette(theme, system_dark);
        let ink = Ink::new(palette.panel, palette.accent);
        style::apply_visuals(ui, &ink);
        ui.spacing_mut().item_spacing = vec2(8.0, 6.0);

        // Reserve the preview first, then paint it after the controls so even a
        // colour-picker drag is reflected in the same frame.
        let (preview, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::hover());
        response.widget_info(|| {
            WidgetInfo::labeled(Role::Image, ui.is_enabled(), "Live gradient preview")
        });
        response.on_hover_text("Live colour and texture preview.");

        presets(ui, &ink, theme);
        ui.add_space(2.0);

        control_row(ui, &ink, "Colours", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().interact_size = vec2(30.0, 26.0);
                for (index, stop) in theme.colors.iter_mut().enumerate() {
                    ui.push_id(("colour-stop", index), |ui| {
                        let mut rgb = [stop.0, stop.1, stop.2];
                        // egui's colour editor captures this auto ID before allocating
                        // its button; it does not use Popup::default_response_id.
                        let popup_id = ui.auto_id_with("popup");
                        let response = ui.color_edit_button_srgb(&mut rgb);
                        if let Some(parent) = popup_parent.filter(|_| egui::Popup::is_id_open(ui.ctx(), popup_id)) {
                            let popup = egui::LayerId::new(egui::Order::Foreground, popup_id);
                            ui.ctx().set_sublayer(parent, popup);
                            ui.ctx().move_to_top(popup);
                        }
                        if response.changed() {
                            *stop = Rgb(rgb[0], rgb[1], rgb[2]);
                        }
                        let label = format!("Edit colour stop {}", index + 1);
                        response.widget_info(|| {
                            let mut info =
                                WidgetInfo::labeled(Role::ColorWell, ui.is_enabled(), &label);
                            info.current_text_value = Some(stop.hex());
                            info
                        });
                        focus_ring(ui, &ink, &response);
                        response.on_hover_text(format!("{label} ({})", stop.hex()));
                    });
                }
                let can_add = theme.colors.len() < 3;
                let add = Button::new("Add")
                    .size(Size::Small)
                    .enabled(can_add)
                    .show(ui, &ink);
                button_label(&add, "Add colour stop", can_add);
                if add.on_hover_text("Add a colour stop (up to three)").clicked() {
                    let last = *theme.colors.last().unwrap();
                    theme.colors.push(Rgb(last.2, last.0, last.1));
                }
                let can_remove = theme.colors.len() > 1;
                let remove = Button::new("Remove")
                    .size(Size::Small)
                    .enabled(can_remove)
                    .show(ui, &ink);
                button_label(&remove, "Remove last colour stop", can_remove);
                if remove.on_hover_text("Remove the last colour stop").clicked() {
                    theme.colors.pop();
                }
            });
        });

        control_row(ui, &ink, "Appearance", |ui| {
            choices(
                ui,
                &ink,
                "appearance",
                "Appearance",
                &mut theme.mode,
                &[
                    (ThemeMode::Light, "Light"),
                    (ThemeMode::System, "System"),
                    (ThemeMode::Dark, "Dark"),
                ],
            );
        });
        let neutral = if palette.dark {
            Color32::from_rgb(0x1b, 0x1b, 0x21)
        } else {
            Color32::from_rgb(0xf3, 0xf2, 0xf4)
        };
        let first = theme.colors[0];
        slider_row(
            ui,
            &ink,
            "Intensity",
            &mut theme.intensity,
            100.0,
            "%",
            Some((neutral, Color32::from_rgb(first.0, first.1, first.2))),
        );

        let mut angle = theme.angle_deg / 360.0;
        if slider_row(ui, &ink, "Angle", &mut angle, 360.0, "°", None) {
            theme.angle_deg = (angle * 360.0).round();
        }
        control_row(ui, &ink, "Texture", |ui| {
            choices(
                ui,
                &ink,
                "texture",
                "Texture",
                &mut theme.texture,
                &Texture::ALL.map(|texture| (texture, texture.label())),
            );
        });
        slider_row(ui, &ink, "Strength", &mut theme.grain, 100.0, "%", None);

        let palette = colors::palette(theme, system_dark);
        let gradient = Rect::from_min_max(preview.min, pos2(preview.right(), preview.bottom() - 20.0));
        preview_surface(ui, gradient, theme, &palette, textures);
        style::paint(
            ui.painter(),
            pos2(preview.left(), preview.bottom() - 6.0),
            Align2::LEFT_CENTER,
            "Colour & texture preview",
            11.0,
            weight::REGULAR,
            ink.secondary,
        );
        style::paint(
            ui.painter(),
            pos2(preview.right(), preview.bottom() - 6.0),
            Align2::RIGHT_CENTER,
            if palette.dark { "Dark" } else { "Light" },
            11.0,
            weight::MEDIUM,
            ink.secondary,
        );
    });

    *theme != before
}

#[derive(Clone)]
struct Preview {
    theme: Theme,
    dark: bool,
    size: egui::Vec2,
    image: egui::TextureHandle,
}

fn preview_surface(ui: &Ui, rect: Rect, theme: &Theme, palette: &colors::Palette, textures: &colors::TextureBank) {
    let id = ui.scope_id().with("theme-preview-image");
    let cached = ui.ctx().data(|d| d.get_temp::<Preview>(id));
    let preview = if let Some(cached) = cached.filter(|p| p.theme == *theme && p.dark == palette.dark && p.size == rect.size()) {
        cached
    } else {
        let (width, height) = (256, 24);
        let dir = egui::Vec2::angled(theme.angle_deg.to_radians());
        let dx = rect.width() * dir.x;
        let dy = rect.height() * dir.y;
        let lo = dx.min(0.0) + dy.min(0.0);
        let span = (dx.abs() + dy.abs()).max(0.001);
        let mut pixels = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let t = ((x as f32 / (width - 1) as f32 * dx + y as f32 / (height - 1) as f32 * dy - lo) / span).clamp(0.0, 1.0);
                let n = palette.stops.len();
                let colour = if n == 1 { palette.stops[0] } else {
                    let at = t * (n - 1) as f32;
                    let i = (at.floor() as usize).min(n - 2);
                    style::mix(palette.stops[i], palette.stops[i + 1], at - i as f32)
                };
                pixels.push(colour);
            }
        }
        let image = egui::ColorImage::new([width, height], pixels);
        let image = ui.ctx().load_texture("theme-preview", image, egui::TextureOptions::LINEAR);
        let preview = Preview { theme: theme.clone(), dark: palette.dark, size: rect.size(), image };
        ui.ctx().data_mut(|d| d.insert_temp(id, preview.clone()));
        preview
    };
    let unit = Rect::from_min_max(egui::Pos2::ZERO, pos2(1.0, 1.0));
    ui.painter().add(egui::epaint::RectShape::filled(rect, style::radius::SM, Color32::WHITE).with_texture(preview.image.id(), unit));
    let texture = textures.get(theme.texture);
    let tile = texture.size_vec2() / ui.ctx().pixels_per_point();
    let uv = Rect::from_min_max(egui::Pos2::ZERO, (rect.size() / tile).to_pos2());
    let alpha = (theme.grain.clamp(0.0, 1.0) * 26.0) as u8;
    ui.painter().add(egui::epaint::RectShape::filled(rect, style::radius::SM, Color32::from_white_alpha(alpha)).with_texture(texture.id(), uv));
}

fn presets(ui: &mut Ui, ink: &Ink, theme: &mut Theme) {
    const DIAMETER: f32 = 30.0;
    const HIT_SIZE: f32 = DIAMETER + 10.0;
    let width = ui.available_width();
    let fit = ((width + 4.0) / (HIT_SIZE + 4.0)).floor() as usize;
    // Two balanced rows, rather than seven swatches followed by a lone eighth.
    let columns = if fit >= 8 { 8 } else { fit.clamp(1, 4) };
    let gap = ((width - HIT_SIZE * columns as f32) / columns.saturating_sub(1).max(1) as f32)
        .max(0.0);
    for start in (0..PRESET_NAMES.len()).step_by(columns) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (index, name) in PRESET_NAMES.iter().enumerate().skip(start).take(columns) {
                let preset = Theme::preset(index);
                let stops: Vec<_> = preset
                    .colors
                    .iter()
                    .map(|c| Color32::from_rgb(c.0, c.1, c.2))
                    .collect();
                let selected = preset.colors == theme.colors;
                let id = ui.scope_id().with(("preset", index));
                let response = style::swatch(ui, ink, id, &stops, selected, DIAMETER);
                response.widget_info(|| {
                    WidgetInfo::selected(
                        Role::RadioButton,
                        ui.is_enabled(),
                        selected,
                        format!("{name} colour preset"),
                    )
                });
                if response.has_focus() {
                    ui.painter().circle_stroke(
                        response.rect.center(),
                        DIAMETER / 2.0 + 4.0,
                        Stroke::new(2.0, ink.accent),
                    );
                }
                if response.on_hover_text(*name).clicked() {
                    // A colour preset must not reset appearance or texture.
                    theme.colors = preset.colors;
                }
            }
        });
    }
}

fn control_row(ui: &mut Ui, ink: &Ink, label: &str, control: impl FnOnce(&mut Ui)) {
    let title = style::text(label, 13.0, weight::MEDIUM).color(ink.text);
    if ui.available_width() >= 340.0 {
        ui.horizontal(|ui| {
            let (label_rect, _) = ui.allocate_exact_size(vec2(LABEL_WIDTH, ROW_HEIGHT), Sense::hover());
            style::paint(ui.painter(), label_rect.left_center(), Align2::LEFT_CENTER, label, 13.0, weight::MEDIUM, ink.text);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), control);
        });
    } else {
        ui.label(title);
        ui.horizontal(control);
    }
}

fn choices<T: Copy + PartialEq>(
    ui: &mut Ui,
    ink: &Ink,
    salt: &str,
    label: &str,
    value: &mut T,
    options: &[(T, &str)],
) {
    let id = ui.scope_id().with(salt);
    let segments: Vec<_> = options.iter().map(|(value, label)| (*value, *label, None)).collect();
    let width = ui.available_width();
    style::segmented(ui, ink, id, value, &segments, width);
    // The design-system control returns only a changed flag; enrich its real
    // responses instead of creating overlapping, duplicate hit targets.
    for (index, (option, title)) in options.iter().enumerate() {
        if let Some(response) = ui.ctx().read_response(id.with(index)) {
            response.widget_info(|| {
                WidgetInfo::selected(
                    Role::RadioButton,
                    ui.is_enabled(),
                    *option == *value,
                    format!("{label}: {title}"),
                )
            });
            if response.has_focus() {
                ui.painter().rect_stroke(
                    response.rect.shrink(1.0),
                    response.rect.height() / 2.0,
                    Stroke::new(2.0, ink.accent),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }
}

fn slider_row(
    ui: &mut Ui,
    ink: &Ink,
    label: &str,
    value: &mut f32,
    scale: f32,
    suffix: &str,
    track: Option<(Color32, Color32)>,
) -> bool {
    let mut changed = false;
    control_row(ui, ink, label, |ui| {
        let id = ui.scope_id().with(("theme-slider", label));
        let width = (ui.available_width() - 42.0 - ui.spacing().item_spacing.x).max(30.0);
        changed = style::slider(ui, ink, id, value, width, track);
        if let Some(response) = ui.ctx().read_response(id) {
            response.widget_info(|| {
                WidgetInfo::slider(
                    ui.is_enabled(),
                    (*value * scale) as f64,
                    if label == "Strength" { "Texture strength" } else { label },
                )
            });
            response.on_hover_text(if label == "Strength" {
                "Texture strength; 0% turns texture off. Use arrow keys to adjust."
            } else {
                "Use arrow keys to adjust."
            });
        }
        let (rect, _) = ui.allocate_exact_size(vec2(42.0, 24.0), Sense::hover());
        style::paint(
            ui.painter(),
            rect.right_center(),
            Align2::RIGHT_CENTER,
            &format!("{:.0}{suffix}", *value * scale),
            12.0,
            weight::MEDIUM,
            ink.secondary,
        );
    });
    changed
}

fn button_label(response: &Response, label: &str, enabled: bool) {
    response.widget_info(|| WidgetInfo::labeled(Role::Button, response.enabled() && enabled, label));
}

fn focus_ring(ui: &Ui, ink: &Ink, response: &Response) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect.expand(2.0),
            style::radius::SM + 2.0,
            Stroke::new(2.0, ink.accent),
            egui::StrokeKind::Outside,
        );
    }
}
