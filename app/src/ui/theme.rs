//! Space themes → colours, gradient backgrounds and surface textures.

use crossbow_core::model::{Rgb, Texture, Theme, ThemeMode};
use egui::{Color32, Mesh, Painter, Pos2, Rect, Rgba, TextureHandle, TextureId, Vec2};

#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub stops: Vec<Color32>,
    pub angle_deg: f32,
    pub grain: f32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub hover: Color32,
    pub selected: Color32,
    pub selected_text: Color32,
    pub accent: Color32,
    pub divider: Color32,
    pub panel: Color32,
    pub panel_text: Color32,
    pub panel_dim: Color32,
    pub panel_border: Color32,
    pub panel_hover: Color32,
}

fn c(rgb: Rgb) -> Color32 {
    Color32::from_rgb(rgb.0, rgb.1, rgb.2)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let a = Rgba::from(a);
    let b = Rgba::from(b);
    Color32::from(a * (1.0 - t) + b * t)
}

fn luminance(col: Color32) -> f32 {
    let l = Rgba::from(col);
    0.2126 * l.r() + 0.7152 * l.g() + 0.0722 * l.b()
}

pub fn is_dark(theme: &Theme, system_dark: bool) -> bool {
    match theme.mode {
        ThemeMode::System => system_dark,
        ThemeMode::Light => false,
        ThemeMode::Dark => true,
    }
}

pub fn palette(theme: &Theme, system_dark: bool) -> Palette {
    let dark = is_dark(theme, system_dark);
    let base = if dark { Color32::from_rgb(0x1b, 0x1b, 0x21) } else { Color32::from_rgb(0xf3, 0xf2, 0xf4) };
    let intensity = theme.intensity.clamp(0.0, 1.0);
    let colors: Vec<Color32> = if theme.colors.is_empty() { vec![base] } else { theme.colors.iter().map(|&x| c(x)).collect() };
    let stops: Vec<Color32> = colors
        .iter()
        .map(|&col| {
            let col = if dark { mix(col, Color32::BLACK, 0.55) } else { col };
            mix(base, col, intensity * if dark { 0.9 } else { 0.75 })
        })
        .collect();
    let avg_lum = stops.iter().map(|&s| luminance(s)).sum::<f32>() / stops.len() as f32;
    let light_text = avg_lum < 0.35;
    let text = if light_text { Color32::from_rgb(0xf5, 0xf5, 0xf7) } else { Color32::from_rgb(0x1d, 0x1d, 0x22) };
    // White text sits on the accent (selected rows, buttons): keep it dark enough.
    let mut accent = colors[0];
    for _ in 0..8 {
        if luminance(accent) <= 0.28 {
            break;
        }
        accent = mix(accent, Color32::BLACK, 0.14);
    }
    Palette {
        dark,
        angle_deg: theme.angle_deg,
        grain: theme.grain,
        text,
        text_dim: text.gamma_multiply(0.62),
        text_faint: text.gamma_multiply(0.38),
        hover: if light_text { Color32::from_white_alpha(22) } else { Color32::from_black_alpha(14) },
        selected: if light_text { Color32::from_white_alpha(40) } else { Color32::from_white_alpha(190) },
        selected_text: if light_text { Color32::WHITE } else { Color32::from_rgb(0x12, 0x12, 0x16) },
        accent,
        divider: if light_text { Color32::from_white_alpha(28) } else { Color32::from_black_alpha(22) },
        panel: if dark { Color32::from_rgb(0x26, 0x26, 0x2d) } else { Color32::from_rgb(0xfd, 0xfd, 0xfe) },
        panel_text: if dark { Color32::from_rgb(0xee, 0xee, 0xf2) } else { Color32::from_rgb(0x1a, 0x1a, 0x1f) },
        panel_dim: if dark { Color32::from_rgb(0x9a, 0x9a, 0xa6) } else { Color32::from_rgb(0x70, 0x70, 0x7c) },
        panel_border: if dark { Color32::from_white_alpha(18) } else { Color32::from_black_alpha(18) },
        panel_hover: if dark { Color32::from_white_alpha(14) } else { Color32::from_rgba_unmultiplied(0x5b, 0x5b, 0xf0, 22) },
        stops,
    }
}

fn sample(stops: &[Color32], t: f32) -> Color32 {
    match stops.len() {
        0 => Color32::GRAY,
        1 => stops[0],
        n => {
            let x = t.clamp(0.0, 1.0) * (n - 1) as f32;
            let i = (x.floor() as usize).min(n - 2);
            mix(stops[i], stops[i + 1], x - i as f32)
        }
    }
}

/// Fill `rect` with a linear gradient at `angle_deg` (0 = left→right).
pub fn paint_gradient(painter: &Painter, rect: Rect, stops: &[Color32], angle_deg: f32, opacity: f32) {
    const N: usize = 10;
    let dir = Vec2::angled(angle_deg.to_radians());
    let corners = [rect.left_top(), rect.right_top(), rect.left_bottom(), rect.right_bottom()];
    let proj: Vec<f32> = corners.iter().map(|p| p.to_vec2().dot(dir)).collect();
    let (lo, hi) = proj.iter().fold((f32::MAX, f32::MIN), |(a, b), &p| (a.min(p), b.max(p)));
    let span = (hi - lo).max(1.0);
    let mut mesh = Mesh::default();
    for yi in 0..=N {
        for xi in 0..=N {
            let p = Pos2::new(
                rect.left() + rect.width() * xi as f32 / N as f32,
                rect.top() + rect.height() * yi as f32 / N as f32,
            );
            let t = (p.to_vec2().dot(dir) - lo) / span;
            mesh.colored_vertex(p, sample(stops, t).gamma_multiply(opacity));
        }
    }
    let w = (N + 1) as u32;
    for yi in 0..N as u32 {
        for xi in 0..N as u32 {
            let i = yi * w + xi;
            mesh.add_triangle(i, i + 1, i + w);
            mesh.add_triangle(i + 1, i + w + 1, i + w);
        }
    }
    painter.add(mesh);
}

/// Tileable noise used for the film-grain overlay.
pub fn grain_texture(ctx: &egui::Context) -> TextureHandle {
    surface_texture(ctx, Texture::Grain)
}

/// Generated once per app; the grain handle can also be shared with legacy callers.
pub struct TextureBank {
    grain: TextureHandle,
    sand: TextureHandle,
    denim: TextureHandle,
    tweed: TextureHandle,
}

impl TextureBank {
    pub fn new(ctx: &egui::Context) -> Self {
        Self {
            grain: grain_texture(ctx),
            sand: surface_texture(ctx, Texture::Sand),
            denim: surface_texture(ctx, Texture::Denim),
            tweed: surface_texture(ctx, Texture::Tweed),
        }
    }

    pub fn get(&self, texture: Texture) -> &TextureHandle {
        match texture {
            Texture::Grain => &self.grain,
            Texture::Sand => &self.sand,
            Texture::Denim => &self.denim,
            Texture::Tweed => &self.tweed,
        }
    }
}

const TEXTURE_SIZE: i32 = 128;

// Coordinate hashing wraps on both axes, including samples used by the sand filter.
fn surface_noise(x: i32, y: i32) -> i32 {
    let mut h = (x.rem_euclid(TEXTURE_SIZE) as u32)
        .wrapping_mul(0x9e37_79b9)
        ^ (y.rem_euclid(TEXTURE_SIZE) as u32).wrapping_mul(0x85eb_ca6b);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    (h & 255) as i32 - 128
}

fn surface_value(texture: Texture, x: i32, y: i32) -> u8 {
    let noise = surface_noise(x, y);
    let value = match texture {
        Texture::Grain => 128 + noise,
        Texture::Sand => {
            // Soft, irregular grains rather than a second white-noise pattern.
            let nearby = surface_noise(x - 1, y) + surface_noise(x + 1, y)
                + surface_noise(x, y - 1) + surface_noise(x, y + 1);
            128 + noise / 2 + nearby / 8
        }
        Texture::Denim => {
            // Fine warp threads under a diagonal twill; all periods divide the tile.
            let twill = if (x + y).rem_euclid(8) < 3 { 48 } else { -30 };
            let warp = if x.rem_euclid(4) == 0 { 24 } else { -8 };
            128 + twill + warp + noise / 4
        }
        Texture::Tweed => {
            // Alternating twill direction gives a coarse herringbone with yarn flecks.
            let chevron = (x.rem_euclid(16) - 8).abs();
            let weave = if (chevron + y).rem_euclid(8) < 4 { 42 } else { -42 };
            let yarn = surface_noise(x.div_euclid(4) * 4, y.div_euclid(2) * 2) / 3;
            128 + weave + yarn + noise / 5
        }
    };
    value.clamp(0, 255) as u8
}

fn surface_texture(ctx: &egui::Context, texture: Texture) -> TextureHandle {
    let pixels = (0..TEXTURE_SIZE)
        .flat_map(|y| (0..TEXTURE_SIZE).map(move |x| Color32::from_gray(surface_value(texture, x, y))))
        .collect();
    ctx.load_texture(
        format!("crossbow-{}", texture.label().to_lowercase()),
        egui::ColorImage::new([TEXTURE_SIZE as usize; 2], pixels),
        egui::TextureOptions { wrap_mode: egui::TextureWrapMode::Repeat, ..egui::TextureOptions::NEAREST },
    )
}

/// Paint any bank texture at native pixel scale using the theme's overlay strength.
pub fn paint_texture(painter: &Painter, rect: Rect, bank: &TextureBank, texture: Texture, strength: f32, ppp: f32) {
    paint_grain(painter, rect, bank.get(texture).id(), strength, ppp);
}

pub fn paint_grain(painter: &Painter, rect: Rect, tex: TextureId, strength: f32, ppp: f32) {
    if strength <= 0.01 {
        return;
    }
    let tile = TEXTURE_SIZE as f32 / ppp;
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(rect.width() / tile, rect.height() / tile));
    let alpha = (strength.clamp(0.0, 1.0) * 26.0) as u8;
    painter.image(tex, rect, uv, Color32::from_white_alpha(alpha));
}

/// Deterministic tile colour for sites without a favicon.
pub fn letter_color(seed: &str) -> Color32 {
    let h = seed.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    let hue = (h % 360) as f32 / 360.0;
    egui::ecolor::Hsva::new(hue, 0.45, 0.78, 1.0).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn textures_are_deterministic_and_periodic_on_both_axes() {
        for texture in Texture::ALL {
            for y in 0..TEXTURE_SIZE {
                for x in 0..TEXTURE_SIZE {
                    let value = surface_value(texture, x, y);
                    assert_eq!(value, surface_value(texture, x, y));
                    for offset in [-TEXTURE_SIZE, TEXTURE_SIZE] {
                        assert_eq!(value, surface_value(texture, x + offset, y));
                        assert_eq!(value, surface_value(texture, x, y + offset));
                    }
                }
            }
        }
    }

    #[test]
    fn every_texture_has_a_distinct_non_flat_pattern() {
        let tiles = Texture::ALL.map(|texture| {
            (0..TEXTURE_SIZE)
                .flat_map(|y| (0..TEXTURE_SIZE).map(move |x| surface_value(texture, x, y)))
                .collect::<Vec<_>>()
        });
        for (i, tile) in tiles.iter().enumerate() {
            assert!(tile.iter().any(|&v| v < 96));
            assert!(tile.iter().any(|&v| v > 160));
            for other in &tiles[i + 1..] {
                assert_ne!(tile, other);
            }
        }
    }
}
