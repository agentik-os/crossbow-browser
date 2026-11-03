//! Favicon cache: egui textures keyed by host, persisted as PNG files so the
//! sidebar shows icons right after launch without loading every tab.

use std::collections::HashMap;
use std::path::PathBuf;

pub struct Favicons {
    dir: PathBuf,
    textures: HashMap<String, Option<egui::TextureHandle>>,
}

impl Favicons {
    pub fn new(data_dir: &std::path::Path) -> Favicons {
        let dir = data_dir.join("favicons");
        let _ = std::fs::create_dir_all(&dir);
        Favicons { dir, textures: HashMap::new() }
    }

    fn file(&self, host: &str) -> PathBuf {
        let safe: String = host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
        self.dir.join(format!("{safe}.png"))
    }

    /// Texture for the page's host, loading it from disk on first use.
    pub fn get(&mut self, ctx: &egui::Context, url: &str) -> Option<egui::TextureHandle> {
        let host = crossbow_core::search::host_of(url)?;
        if let Some(t) = self.textures.get(&host) {
            return t.clone();
        }
        let tex = std::fs::read(self.file(&host)).ok().and_then(|bytes| {
            let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).ok()?.to_rgba8();
            let size = [img.width() as usize, img.height() as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
            Some(ctx.load_texture(format!("favicon:{host}"), color, egui::TextureOptions::LINEAR))
        });
        self.textures.insert(host, tex.clone());
        tex
    }

    /// Store a freshly downloaded favicon (premultiplied RGBA from Chromium).
    pub fn put(&mut self, ctx: &egui::Context, page_url: &str, width: u32, height: u32, rgba: &[u8]) {
        let Some(host) = crossbow_core::search::host_of(page_url) else { return };
        let mut straight = rgba[..(width * height * 4) as usize].to_vec();
        for px in straight.chunks_exact_mut(4) {
            let a = px[3] as u32;
            if a > 0 && a < 255 {
                for c in &mut px[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        let color = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &straight);
        let tex = ctx.load_texture(format!("favicon:{host}"), color, egui::TextureOptions::LINEAR);
        self.textures.insert(host.clone(), Some(tex));
        if let Some(img) = image::RgbaImage::from_raw(width, height, straight) {
            let _ = img.save_with_format(self.file(&host), image::ImageFormat::Png);
        }
    }
}
