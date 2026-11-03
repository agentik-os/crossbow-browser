//! Captures of a page's own pixels: a part of the latest frame CEF painted
//! into the page's GPU texture, read back and encoded as PNG. Nothing here
//! reads the screen, so Crossbow never needs Screen Recording permission and
//! a capture can only ever hold that page's pixels.

use egui::{Rect, pos2, vec2};

/// A part of a page's texture, in device pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Where a page's frame is drawn: `page` in points, with the frame's
/// `texture` size in pixels from its top-left corner at `ppp` pixels per
/// point, as the panes and Little Windows draw it. Page zoom changes the
/// layout CEF paints, not this mapping.
pub fn shown(page: Rect, texture: (u32, u32), ppp: f32) -> Rect {
    Rect::from_min_size(page.min, vec2(texture.0 as f32, texture.1 as f32) / ppp).intersect(page)
}

/// The texture pixels under `selection`, a rect in points, clamped to what
/// is shown of the frame. None when that leaves less than a pixel.
pub fn pixels(selection: Rect, page: Rect, texture: (u32, u32), ppp: f32) -> Option<PixelRect> {
    let selected = selection.intersect(shown(page, texture, ppp));
    if !(selected.width() > 0.0 && selected.height() > 0.0) { return None; }
    // Edges that fall between pixels go to the nearest one.
    let edge = |points: f32, limit: u32| (((points * ppp).round()).max(0.0) as u32).min(limit);
    let (left, top) = (edge(selected.min.x - page.min.x, texture.0), edge(selected.min.y - page.min.y, texture.1));
    let (right, bottom) = (edge(selected.max.x - page.min.x, texture.0), edge(selected.max.y - page.min.y, texture.1));
    (right > left && bottom > top).then(|| PixelRect { x: left, y: top, width: right - left, height: bottom - top })
}

/// The selection a drag from `from` to `to` makes, kept within `bounds`.
pub fn dragged(from: egui::Pos2, to: egui::Pos2, bounds: Rect) -> Rect {
    Rect::from_two_pos(bounds.clamp(from), bounds.clamp(to))
}

/// `selection` moved by `delta`, kept whole within `bounds`.
pub fn moved(selection: Rect, delta: egui::Vec2, bounds: Rect) -> Rect {
    let size = selection.size().min(bounds.size());
    let min = selection.min + delta;
    let min = pos2(min.x.clamp(bounds.min.x, bounds.max.x - size.x), min.y.clamp(bounds.min.y, bounds.max.y - size.y));
    Rect::from_min_size(min, size)
}

/// Straight-alpha RGBA pixels, row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Read `region` of `texture` back from the GPU. The texture must allow
/// copies from it. This waits for the copy, about a frame.
pub fn read(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture, region: PixelRect) -> Result<Image, String> {
    let bgra = match texture.format() {
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => true,
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => false,
        _ => return Err("This page's frame is in a format Crossbow can't capture.".into()),
    };
    let fits = region.width > 0 && region.height > 0
        && region.x.checked_add(region.width).is_some_and(|right| right <= texture.width())
        && region.y.checked_add(region.height).is_some_and(|bottom| bottom <= texture.height());
    if !fits { return Err("The page changed size during the capture. Try again.".into()); }
    let row = region.width * 4;
    let padded = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("capture readback"),
        size: u64::from(padded) * u64::from(region.height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("capture copy") });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d { x: region.x, y: region.y, z: 0 }, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(region.height) } },
        wgpu::Extent3d { width: region.width, height: region.height, depth_or_array_layers: 1 },
    );
    let submitted = queue.submit(Some(encoder.finish()));
    let (send, receive) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| { let _ = send.send(result); });
    let waited = device.poll(wgpu::PollType::Wait { submission_index: Some(submitted), timeout: Some(std::time::Duration::from_secs(5)) });
    let mapped = waited.is_ok() && matches!(receive.recv_timeout(std::time::Duration::from_secs(5)), Ok(Ok(())));
    if !mapped { return Err("The page's pixels couldn't be read. Try again.".into()); }
    let mut rgba = Vec::with_capacity(row as usize * region.height as usize);
    {
        let Ok(data) = buffer.slice(..).get_mapped_range() else { return Err("The page's pixels couldn't be read. Try again.".into()) };
        for line in data.chunks_exact(padded as usize) { rgba.extend_from_slice(&line[..row as usize]); }
    }
    buffer.unmap();
    for pixel in rgba.chunks_exact_mut(4) {
        if bgra { pixel.swap(0, 2); }
        // CEF's frames carry premultiplied alpha; PNG stores straight alpha.
        let alpha = u32::from(pixel[3]);
        if alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = if alpha == 0 { 0 } else { ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8 };
            }
        }
    }
    Ok(Image { width: region.width, height: region.height, rgba })
}

/// `image` as PNG, marked with its pixel density so that a capture at 2x
/// pastes at the size it was seen.
pub fn png(image: &Image, ppp: f32) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&image.rgba, image.width, image.height, image::ExtendedColorType::Rgba8)
        .map_err(|_| "The capture couldn't be encoded. Try again.".to_string())?;
    Ok(with_density(out, ppp))
}

/// PNG's signature and IHDR chunk come first; pHYs may follow IHDR.
const AFTER_HEADER: usize = 8 + 4 + 4 + 13 + 4;

/// Insert a pHYs chunk: 72 dpi times `ppp`, in pixels per metre.
fn with_density(mut png: Vec<u8>, ppp: f32) -> Vec<u8> {
    if png.len() < AFTER_HEADER || &png[12..16] != b"IHDR" { return png; }
    let per_metre = (f64::from(ppp.clamp(0.5, 8.0)) * 72.0 / 0.0254).round() as u32;
    let mut chunk = Vec::with_capacity(21);
    chunk.extend_from_slice(&9u32.to_be_bytes());
    chunk.extend_from_slice(b"pHYs");
    chunk.extend_from_slice(&per_metre.to_be_bytes());
    chunk.extend_from_slice(&per_metre.to_be_bytes());
    chunk.push(1);
    let crc = crc32(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());
    png.splice(AFTER_HEADER..AFTER_HEADER, chunk);
    png
}

/// The CRC-32 PNG chunks carry (ISO 3309).
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 { crc = if crc & 1 == 1 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 }; }
    }
    !crc
}

pub const COPY_FAILED: &str = "Couldn't copy the capture. Try Copy again.";
pub const SAVE_AS_FAILED: &str = "Couldn't save the file there. Choose another folder and try again.";

/// Where a capture is copied. The production one is the macOS general
/// pasteboard; tests use a fake or a private pasteboard, never the person's
/// clipboard.
pub trait Board {
    /// Replace the contents with the PNG and, for a capture saved to
    /// Captures, its file, so a paste in Finder gives the file.
    fn copy(&mut self, png: &[u8], file: Option<&std::path::Path>) -> Result<(), String>;
}

/// Asks where Save As… writes. The production one is the system save panel.
pub trait SavePanel {
    /// Offer `name`; the receiver gets the chosen path, or None on Cancel.
    fn choose(&mut self, name: &str) -> std::sync::mpsc::Receiver<Result<Option<std::path::PathBuf>, String>>;
}

/// Write a Save As… copy where the person chose.
pub fn write_copy(path: &std::path::Path, png: &[u8]) -> Result<(), String> {
    std::fs::write(path, png).map_err(|_| SAVE_AS_FAILED.to_string())
}

pub fn system_board() -> Box<dyn Board> {
    #[cfg(target_os = "macos")]
    if let Some(board) = mac::Pasteboard::general() { return Box::new(board); }
    Box::new(NoBoard)
}

pub fn system_save_panel() -> Box<dyn SavePanel> {
    #[cfg(target_os = "macos")]
    { Box::new(mac::Panel) }
    #[cfg(not(target_os = "macos"))]
    { Box::new(NoPanel) }
}

struct NoBoard;

impl Board for NoBoard {
    fn copy(&mut self, _: &[u8], _: Option<&std::path::Path>) -> Result<(), String> {
        Err(COPY_FAILED.into())
    }
}

#[cfg(not(target_os = "macos"))]
struct NoPanel;

#[cfg(not(target_os = "macos"))]
impl SavePanel for NoPanel {
    fn choose(&mut self, _: &str) -> std::sync::mpsc::Receiver<Result<Option<std::path::PathBuf>, String>> {
        let (send, receive) = std::sync::mpsc::channel();
        let _ = send.send(Err("Save As… is available on macOS only in this preview.".into()));
        receive
    }
}

#[cfg(target_os = "macos")]
pub(crate) mod mac {
    use super::{Board, COPY_FAILED, SavePanel};
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{MainThreadMarker, class, msg_send};
    use objc2_app_kit::{NSApplication, NSModalResponseCancel, NSModalResponseOK, NSSavePanel};
    use objc2_foundation::{NSString, NSURL};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;

    pub(crate) const PNG: &str = "public.png";
    pub(crate) const FILE_URL: &str = "public.file-url";

    pub struct Pasteboard(Retained<AnyObject>);

    impl Pasteboard {
        pub fn general() -> Option<Pasteboard> {
            let board: Option<Retained<AnyObject>> = unsafe { msg_send![class!(NSPasteboard), generalPasteboard] };
            board.map(Pasteboard)
        }

        /// A private, uniquely named pasteboard, so tests never touch the clipboard.
        #[cfg(test)]
        pub fn unique() -> Option<Pasteboard> {
            let board: Option<Retained<AnyObject>> = unsafe { msg_send![class!(NSPasteboard), pasteboardWithUniqueName] };
            board.map(Pasteboard)
        }

        /// Each item's types and the bytes or text under each, for tests.
        #[cfg(test)]
        pub fn items(&self) -> Vec<Vec<(String, Vec<u8>)>> {
            let items: Option<Retained<AnyObject>> = unsafe { msg_send![&*self.0, pasteboardItems] };
            let Some(items) = items else { return Vec::new() };
            let count: usize = unsafe { msg_send![&*items, count] };
            (0..count).map(|index| {
                let item: Retained<AnyObject> = unsafe { msg_send![&*items, objectAtIndex: index] };
                let types: Retained<AnyObject> = unsafe { msg_send![&*item, types] };
                let count: usize = unsafe { msg_send![&*types, count] };
                (0..count).map(|index| {
                    let kind: Retained<NSString> = unsafe { msg_send![&*types, objectAtIndex: index] };
                    let data: Option<Retained<AnyObject>> = unsafe { msg_send![&*item, dataForType: &*kind] };
                    let bytes = data.map(|data| {
                        let length: usize = unsafe { msg_send![&*data, length] };
                        let start: *const std::ffi::c_void = unsafe { msg_send![&*data, bytes] };
                        if start.is_null() { Vec::new() } else { unsafe { std::slice::from_raw_parts(start.cast::<u8>(), length) }.to_vec() }
                    }).unwrap_or_default();
                    (kind.to_string(), bytes)
                }).collect()
            }).collect()
        }

        #[cfg(test)]
        pub fn release(self) {
            let _: () = unsafe { msg_send![&*self.0, releaseGlobally] };
        }

        /// Replace the contents with one item holding each text, as UTF-8,
        /// under its type.
        pub(crate) fn write_texts(&mut self, texts: &[(&str, &str)]) -> bool {
            let item: Retained<AnyObject> = unsafe { msg_send![class!(NSPasteboardItem), new] };
            for (kind, text) in texts {
                let data: Option<Retained<AnyObject>> = unsafe { msg_send![class!(NSData), dataWithBytes: text.as_ptr().cast::<std::ffi::c_void>(), length: text.len()] };
                let Some(data) = data else { return false };
                let kind = NSString::from_str(kind);
                let set: bool = unsafe { msg_send![&*item, setData: &*data, forType: &*kind] };
                if !set { return false; }
            }
            let items: Retained<AnyObject> = unsafe { msg_send![class!(NSArray), arrayWithObject: &*item] };
            let _: isize = unsafe { msg_send![&*self.0, clearContents] };
            unsafe { msg_send![&*self.0, writeObjects: &*items] }
        }
    }

    impl Board for Pasteboard {
        fn copy(&mut self, png: &[u8], file: Option<&Path>) -> Result<(), String> {
            // One item, written in one call: the image, then the saved file.
            let item: Retained<AnyObject> = unsafe { msg_send![class!(NSPasteboardItem), new] };
            let data: Option<Retained<AnyObject>> = unsafe { msg_send![class!(NSData), dataWithBytes: png.as_ptr().cast::<std::ffi::c_void>(), length: png.len()] };
            let data = data.ok_or(COPY_FAILED)?;
            let kind = NSString::from_str(PNG);
            let set: bool = unsafe { msg_send![&*item, setData: &*data, forType: &*kind] };
            if !set { return Err(COPY_FAILED.into()); }
            if let Some(url) = file.and_then(NSURL::from_file_path).and_then(|url| url.absoluteString()) {
                let kind = NSString::from_str(FILE_URL);
                let set: bool = unsafe { msg_send![&*item, setString: &*url, forType: &*kind] };
                if !set { return Err(COPY_FAILED.into()); }
            }
            let items: Retained<AnyObject> = unsafe { msg_send![class!(NSArray), arrayWithObject: &*item] };
            let _: isize = unsafe { msg_send![&*self.0, clearContents] };
            let copied: bool = unsafe { msg_send![&*self.0, writeObjects: &*items] };
            if copied { Ok(()) } else { Err(COPY_FAILED.into()) }
        }
    }

    pub struct Panel;

    impl SavePanel for Panel {
        fn choose(&mut self, name: &str) -> mpsc::Receiver<Result<Option<PathBuf>, String>> {
            let (send, receive) = mpsc::channel();
            let Some(main) = MainThreadMarker::new() else {
                let _ = send.send(Err("Save As… opens from the main window. Try again there.".into()));
                return receive;
            };
            let panel = NSSavePanel::savePanel(main);
            panel.setNameFieldStringValue(&NSString::from_str(name));
            panel.setCanCreateDirectories(true);
            let chosen = panel.clone();
            let completion = RcBlock::new(move |response| {
                let _ = send.send(if response == NSModalResponseOK {
                    chosen.URL().and_then(|url| url.to_file_path()).map(Some).ok_or_else(|| "Choose a folder on this Mac and try again.".into())
                } else if response == NSModalResponseCancel {
                    Ok(None)
                } else {
                    Err("The save panel couldn't open. Try Save As… again.".into())
                });
            });
            if let Some(window) = NSApplication::sharedApplication(main).keyWindow() {
                panel.beginSheetModalForWindow_completionHandler(&window, &completion);
            } else {
                panel.beginWithCompletionHandler(&completion);
            }
            receive
        }
    }
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
