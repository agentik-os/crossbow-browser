//! Device-local application icons. The library/sync store never sees image bytes.
//! A single versioned file contains either a built-in choice or its sanitized PNG,
//! so an atomic rename commits the selection and custom image together.

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use image::{DynamicImage, ImageDecoder, ImageFormat, Limits, Rgba, RgbaImage, codecs::png::PngDecoder, metadata::Orientation};
use std::fs::{File, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, mpsc};

const MAX_PNG_BYTES: usize = 5 * 1024 * 1024;
const MAX_DIMENSION: u32 = 2048;
const MAX_DECODE_BYTES: u64 = 16 * 1024 * 1024;
const ICON_DIMENSION: u32 = 1024;
const PREVIEW_DIMENSION: u32 = 256;
const HEADER: &[u8] = b"CBICON\x01";
const MAX_RECORD_BYTES: usize = MAX_PNG_BYTES + HEADER.len() + 1;
const SAVE_RECOVERY: &str = "Check free disk space and the Crossbow data folder's permissions, then retry your change. Your current icon is unchanged.";
const COLOR_RECOVERY: &str = "This PNG has an unsupported color profile. Convert its pixels to sRGB, then export a PNG without an embedded ICC profile and try again. Removing a profile without converting the colors is not enough.";
pub const IMAGE_REQUIREMENTS: &str = "Square sRGB PNG, 64–2048 px, up to 5 MiB. Images stay on this device.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconChoice { Black, Graphite, Pearl, Custom }

impl IconChoice {
    pub const BUILT_INS: [Self; 3] = [Self::Black, Self::Graphite, Self::Pearl];

    pub fn name(self) -> &'static str {
        match self {
            Self::Black => "Black",
            Self::Graphite => "Graphite",
            Self::Pearl => "Pearl",
            Self::Custom => "Custom",
        }
    }

    fn tag(self) -> u8 {
        match self { Self::Black => 0, Self::Graphite => 1, Self::Pearl => 2, Self::Custom => 3 }
    }
}

struct IconImage {
    choice: IconChoice,
    png: Arc<[u8]>,
    rgba: RgbaImage,
    preview: ColorImage,
}

static BUILT_INS: [LazyLock<Arc<IconImage>>; 3] = [
    LazyLock::new(|| bundled(IconChoice::Black, include_bytes!("../resources/icon-1024.png"), include_bytes!("../resources/icon-256.png"))),
    LazyLock::new(|| bundled(IconChoice::Graphite, include_bytes!("../resources/icon-graphite-1024.png"), include_bytes!("../resources/icon-graphite-256.png"))),
    LazyLock::new(|| bundled(IconChoice::Pearl, include_bytes!("../resources/icon-pearl-1024.png"), include_bytes!("../resources/icon-pearl-256.png"))),
];

fn built_in(choice: IconChoice) -> Arc<IconImage> {
    BUILT_INS[choice.tag().min(2) as usize].clone()
}

fn bundled(choice: IconChoice, png: &'static [u8], thumbnail: &'static [u8]) -> Arc<IconImage> {
    let rgba = image::load_from_memory_with_format(png, ImageFormat::Png).expect("valid bundled icon").into_rgba8();
    let small = image::load_from_memory_with_format(thumbnail, ImageFormat::Png).expect("valid bundled preview").into_rgba8();
    Arc::new(IconImage { choice, png: Arc::from(png), rgba, preview: preview(&small) })
}

fn downsample(image: &RgbaImage, max: u32) -> RgbaImage {
    let width = image.width().min(max);
    let height = image.height().min(max);
    if (width, height) == image.dimensions() { return image.clone(); }
    let sx = f64::from(image.width()) / f64::from(width);
    let sy = f64::from(image.height()) / f64::from(height);
    // Area-filter premultiplied colors, then unpremultiply. Transparent RGB
    // cannot bleed into the edge; no full-size floating-point buffer is needed.
    RgbaImage::from_fn(width, height, |x, y| {
        let (left, right) = (f64::from(x) * sx, f64::from(x + 1) * sx);
        let (top, bottom) = (f64::from(y) * sy, f64::from(y + 1) * sy);
        let mut sum = [0.0; 4];
        for iy in top.floor() as u32..(bottom.ceil() as u32).min(image.height()) {
            let wy = (bottom.min(f64::from(iy + 1)) - top.max(f64::from(iy))).max(0.0);
            for ix in left.floor() as u32..(right.ceil() as u32).min(image.width()) {
                let wx = (right.min(f64::from(ix + 1)) - left.max(f64::from(ix))).max(0.0);
                let pixel = image.get_pixel(ix, iy);
                let alpha = f64::from(pixel[3]) * wx * wy;
                for channel in 0..3 { sum[channel] += f64::from(pixel[channel]) * alpha; }
                sum[3] += alpha;
            }
        }
        let alpha = (sum[3] / (sx * sy)).round() as u8;
        if alpha == 0 { return Rgba([0; 4]); }
        Rgba([(sum[0] / sum[3]).round() as u8, (sum[1] / sum[3]).round() as u8, (sum[2] / sum[3]).round() as u8, alpha])
    })
}

fn preview(image: &RgbaImage) -> ColorImage {
    let rgba = downsample(image, PREVIEW_DIMENSION);
    ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw())
}

fn png_crc(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 { crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1)); }
    }
    !crc
}

fn png_orientation(bytes: &[u8]) -> Result<Orientation, String> {
    const DAMAGED: &str = "This PNG has damaged metadata. Export a new PNG and try again.";
    let mut rest = bytes.strip_prefix(b"\x89PNG\r\n\x1a\n").ok_or(DAMAGED)?;
    let mut orientation = None;
    // Inspect bounded chunk slices before the decoder can inflate ICC data.
    // eXIf can follow IDAT, beyond the metadata exposed by PngDecoder::new.
    loop {
        let header = rest.get(..8).ok_or(DAMAGED)?;
        let len = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let end = len.checked_add(12).ok_or(DAMAGED)?;
        let chunk = rest.get(..end).ok_or(DAMAGED)?;
        let data = &chunk[8..end - 4];
        match &header[4..8] {
            b"iCCP" => return Err(COLOR_RECOVERY.into()),
            b"gAMA" if data != 45455u32.to_be_bytes() => return Err(COLOR_RECOVERY.into()),
            b"cHRM" => {
                let srgb = [31270u32, 32900, 64000, 33000, 30000, 60000, 15000, 6000];
                if data.len() != 32 || data.chunks_exact(4).zip(srgb).any(|(value, expected)| value != expected.to_be_bytes()) {
                    return Err(COLOR_RECOVERY.into());
                }
            }
            b"cICP" if data != [1, 13, 0, 1] => return Err(COLOR_RECOVERY.into()),
            b"eXIf" => {
                if orientation.is_some() || data.len() > 64 * 1024 || png_crc(&chunk[4..end - 4]).to_be_bytes() != chunk[end - 4..] {
                    return Err(DAMAGED.into());
                }
                // EXIF without an Orientation tag needs no pixel transform.
                orientation = Some(Orientation::from_exif_chunk(data).unwrap_or(Orientation::NoTransforms));
            }
            b"IEND" => return Ok(orientation.unwrap_or(Orientation::NoTransforms)),
            _ => {},
        }
        rest = &rest[end..];
    }
}

fn read_bounded(path: &Path, max: usize) -> Result<Vec<u8>, std::io::Error> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Refuse symlinks and special files without blocking on a FIFO/device.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > max as u64 {
        return Err(std::io::Error::other("not a regular file within the size limit"));
    }
    let mut bytes = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(std::io::Error::other("file exceeds the size limit"));
    }
    Ok(bytes)
}

fn decode_custom(bytes: &[u8]) -> Result<Arc<IconImage>, String> {
    if bytes.len() > MAX_PNG_BYTES {
        return Err("This image exceeds 5 MiB. Choose a smaller PNG.".into());
    }
    let orientation = png_orientation(bytes)?;
    // Limits apply before PNG headers/metadata are read, not after allocation.
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    let decoder = PngDecoder::with_limits(Cursor::new(bytes), limits)
        .map_err(|_| "Could not read this PNG within the image limits. Choose a square PNG, 64–2048 px, up to 5 MiB.".to_string())?;
    let (width, height) = decoder.dimensions();
    if width != height || !(64..=MAX_DIMENSION).contains(&width) {
        return Err("Choose a square PNG between 64 and 2048 pixels on each side.".into());
    }
    if decoder.total_bytes() > MAX_DECODE_BYTES {
        return Err("This PNG needs too much decoded memory. Export it as an 8-bit PNG and try again.".into());
    }
    if decoder.is_apng().map_err(|_| "This PNG is damaged. Export a new PNG and try again.".to_string())? {
        return Err("Animated PNGs are not supported. Choose a still PNG.".into());
    }
    let mut decoded = DynamicImage::from_decoder(decoder)
        .map_err(|_| "This PNG is damaged. Export a new PNG and try again.".to_string())?;
    decoded.apply_orientation(orientation);
    let image = downsample(&decoded.into_rgba8(), ICON_DIMENSION);
    if !image.pixels().any(|pixel| pixel[3] > 0) {
        return Err("This PNG is fully transparent. Choose an image with a visible icon.".into());
    }
    // Re-encoding strips metadata and trailing/ancillary source data. Neither
    // the original path nor an external file reference is persisted.
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, ImageFormat::Png).map_err(|_| "Could not prepare this PNG. Export a new image and try again.".to_string())?;
    let png = png.into_inner();
    if png.len() > MAX_PNG_BYTES {
        return Err("The prepared icon exceeds 5 MiB. Choose a smaller PNG.".into());
    }
    Ok(Arc::new(IconImage { choice: IconChoice::Custom, png: png.into(), preview: preview(&image), rgba: image }))
}

fn import_png(path: &Path) -> Result<Arc<IconImage>, String> {
    let bytes = read_bounded(path, MAX_PNG_BYTES)
        .map_err(|_| "Could not open this image. Choose a local PNG file up to 5 MiB that you have permission to read.".to_string())?;
    decode_custom(&bytes)
}

fn record(image: &IconImage) -> Vec<u8> {
    let mut bytes = HEADER.to_vec();
    bytes.push(image.choice.tag());
    if image.choice == IconChoice::Custom {
        bytes.extend_from_slice(&image.png);
    }
    bytes
}

fn decode_record(bytes: &[u8]) -> Result<Arc<IconImage>, String> {
    let Some(payload) = bytes.strip_prefix(HEADER) else { return Err("unrecognized icon preference".into()) };
    match payload.split_first() {
        Some((0, [])) => Ok(built_in(IconChoice::Black)),
        Some((1, [])) => Ok(built_in(IconChoice::Graphite)),
        Some((2, [])) => Ok(built_in(IconChoice::Pearl)),
        Some((3, png)) => decode_custom(png),
        _ => Err("incomplete or unrecognized icon preference".into()),
    }
}

struct Loaded {
    image: Arc<IconImage>,
    warning: Option<String>,
}

fn load(path: &Path) -> Loaded {
    let result = match read_bounded(path, MAX_RECORD_BYTES) {
        Ok(bytes) => decode_record(&bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Loaded { image: built_in(IconChoice::Black), warning: None };
        }
        Err(_) => Err("unreadable icon preference".into()),
    };
    match result {
        Ok(image) => Loaded { image, warning: None },
        Err(_) => Loaded {
            image: built_in(IconChoice::Black),
            warning: Some("Your saved icon could not be loaded. Using Black for now; choose an icon and Apply, or Reset to replace the saved preference.".into()),
        },
    }
}

struct Saved {
    warning: Option<String>,
}

fn save(path: &Path, image: &IconImage) -> Result<Saved, String> {
    let write = || -> Result<Saved, std::io::Error> {
        let dir = path.parent().ok_or_else(|| std::io::Error::other("missing parent directory"))?;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir)?;
        if !std::fs::symlink_metadata(dir)?.is_dir() {
            return Err(std::io::Error::other("icon directory is not a directory"));
        }
        // The tempfile has mode 0600 on Unix and is on the destination volume.
        let mut temp = tempfile::NamedTempFile::new_in(dir)?;
        temp.write_all(&record(image))?;
        temp.as_file().sync_all()?;
        #[cfg(unix)]
        let directory = File::open(dir)?;
        #[cfg(unix)]
        directory.sync_all()?;
        temp.persist(path).map_err(|error| error.error)?;
        // Rename is the commit point. A failure after it is a durability
        // warning, never a false claim that the old selection is still saved.
        #[cfg(unix)]
        if directory.sync_all().is_err() {
            return Ok(Saved { warning: Some("Icon applied, but the disk could not confirm the final save. Check the data folder's storage, then Apply again before quitting.".into()) });
        }
        Ok(Saved { warning: None })
    };
    write().map_err(|_| SAVE_RECOVERY.to_string())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Job { Loading, Picking, Importing, Saving }

enum Message {
    Loaded(Loaded),
    Picked(Result<Option<PathBuf>, String>),
    Imported(Result<Arc<IconImage>, String>),
    Saved(Result<Saved, String>),
}

pub struct AppIcon {
    path: PathBuf,
    current: Arc<IconImage>,
    draft: Arc<IconImage>,
    built_in_textures: [Option<TextureHandle>; 3],
    draft_texture: Option<TextureHandle>,
    sender: mpsc::Sender<Message>,
    receiver: mpsc::Receiver<Message>,
    job: Option<Job>,
    pending: Option<(Arc<IconImage>, native::PreparedIcon)>,
    needs_save: bool,
    reveal_error: bool,
    pub message: Option<String>,
    pub has_error: bool,
}

impl AppIcon {
    pub fn new(data_dir: &Path) -> Self {
        let image = built_in(IconChoice::Black);
        let (sender, receiver) = mpsc::channel();
        Self {
            path: data_dir.join("appearance/app-icon.bin"),
            current: image.clone(), draft: image,
            built_in_textures: [None, None, None], draft_texture: None,
            sender, receiver, job: None, pending: None, needs_save: false, reveal_error: false, message: None, has_error: false,
        }
    }

    pub fn supported() -> bool { cfg!(target_os = "macos") }
    pub fn busy(&self) -> bool { self.job.is_some() }
    pub fn saving(&self) -> bool { self.job == Some(Job::Saving) }
    pub fn choice(&self) -> IconChoice { self.draft.choice }
    pub fn current_name(&self) -> &'static str { self.current.choice.name() }
    pub fn changed(&self) -> bool { !Arc::ptr_eq(&self.current, &self.draft) }
    pub fn can_apply(&self) -> bool { self.changed() || self.needs_save }
    pub fn can_reset(&self) -> bool { self.current.choice != IconChoice::Black || self.can_apply() }

    pub fn busy_label(&self) -> &'static str {
        match self.job {
            Some(Job::Loading) => "Loading saved icon…",
            Some(Job::Picking) => "Choose a PNG in the file picker…",
            Some(Job::Importing) => "Preparing preview…",
            Some(Job::Saving) => "Saving icon…",
            None => "",
        }
    }

    fn error(&mut self, message: String) {
        self.message = Some(message);
        self.has_error = true;
        self.reveal_error = true;
    }

    /// One request per failed attempt, even when the error text is unchanged.
    pub fn take_error_reveal(&mut self) -> bool { std::mem::take(&mut self.reveal_error) }

    fn stage(&mut self, image: Arc<IconImage>) {
        self.draft = image;
        self.draft_texture = None;
        self.message = None;
        self.has_error = false;
        self.reveal_error = false;
    }

    pub fn choose_builtin(&mut self, choice: IconChoice) {
        if !self.busy() && IconChoice::BUILT_INS.contains(&choice) {
            self.stage(built_in(choice));
        }
    }

    pub fn cancel_changes(&mut self) {
        if !self.busy() { self.stage(self.current.clone()); }
    }

    pub fn builtin_texture(&mut self, ctx: &Context, choice: IconChoice) -> TextureHandle {
        self.built_in_textures[choice.tag().min(2) as usize].get_or_insert_with(|| {
            ctx.load_texture(format!("app-icon-{}", choice.name()), built_in(choice).preview.clone(), TextureOptions::LINEAR)
        }).clone()
    }

    pub fn preview_texture(&mut self, ctx: &Context) -> TextureHandle {
        self.draft_texture.get_or_insert_with(|| {
            ctx.load_texture("app-icon-preview", self.draft.preview.clone(), TextureOptions::LINEAR)
        }).clone()
    }

    fn worker(&mut self, ctx: &Context, job: Job, work: impl FnOnce() -> Message + Send + 'static) {
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        match std::thread::Builder::new().name("crossbow-icon".into()).spawn(move || {
            let message = work();
            let _ = sender.send(message);
            ctx.request_repaint();
        }) {
            Ok(_) => self.job = Some(job),
            Err(_) => {
                self.pending = None;
                self.job = None;
                self.error("Could not start the icon task. Close unused applications and try again. Your current icon is unchanged.".into());
            }
        }
    }

    /// Called once after the native application/window exists. Disk I/O and
    /// custom decoding run off-thread; AppKit calls stay on the main thread.
    pub fn restore(&mut self, ctx: &Context) {
        if !Self::supported() { return; }
        match native::PreparedIcon::new(&self.current.rgba) {
            Ok(icon) => icon.apply(),
            Err(message) => { self.error(message); return; }
        }
        let path = self.path.clone();
        self.worker(ctx, Job::Loading, move || Message::Loaded(load(&path)));
    }

    pub fn choose_png(&mut self, ctx: &Context) {
        if self.busy() { return; }
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        match native::pick_png(move |result| {
            let _ = sender.send(Message::Picked(result));
            ctx.request_repaint();
        }) {
            Ok(()) => self.job = Some(Job::Picking),
            Err(message) => self.error(message),
        }
    }

    pub fn apply(&mut self, ctx: &Context) {
        if self.busy() { return; }
        self.save_candidate(ctx, self.draft.clone());
    }

    /// Reset is an explicit apply of Black, not a destructive edit to the
    /// current image before persistence succeeds.
    pub fn reset(&mut self, ctx: &Context) {
        if self.busy() { return; }
        self.save_candidate(ctx, built_in(IconChoice::Black));
    }

    fn save_candidate(&mut self, ctx: &Context, candidate: Arc<IconImage>) {
        let icon = match native::PreparedIcon::new(&candidate.rgba) {
            Ok(icon) => icon,
            Err(message) => { self.error(message); return; }
        };
        let path = self.path.clone();
        self.pending = Some((candidate.clone(), icon));
        self.message = None;
        self.has_error = false;
        self.reveal_error = false;
        self.worker(ctx, Job::Saving, move || Message::Saved(save(&path, &candidate)));
    }

    fn picked(&mut self, result: Result<Option<PathBuf>, String>, ctx: &Context) {
        self.job = None;
        match result {
            Ok(Some(path)) => self.worker(ctx, Job::Importing, move || Message::Imported(import_png(&path))),
            Ok(None) => {}, // Native Cancel leaves even an unsaved preview intact.
            Err(message) => self.error(message),
        }
    }

    fn imported(&mut self, result: Result<Arc<IconImage>, String>) {
        self.job = None;
        match result {
            Ok(image) => self.stage(image),
            Err(message) => self.error(message),
        }
    }

    fn committed(&mut self, candidate: Arc<IconImage>, saved: Saved) {
        self.current = candidate.clone();
        self.stage(candidate);
        self.has_error = saved.warning.is_some();
        self.reveal_error = self.has_error;
        self.needs_save = self.has_error;
        self.message = saved.warning.or_else(|| Some("Icon applied to this Mac. The Finder bundle icon is unchanged.".into()));
    }

    /// Cheap channel polling only. Never decode or reapply icons each frame.
    pub fn poll(&mut self, ctx: &Context) {
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                Message::Loaded(loaded) => {
                    self.job = None;
                    if !Arc::ptr_eq(&self.current, &loaded.image) {
                        match native::PreparedIcon::new(&loaded.image.rgba) {
                            Ok(icon) => { icon.apply(); self.current = loaded.image.clone(); self.stage(loaded.image); }
                            Err(message) => { self.needs_save = true; self.error(message); continue; }
                        }
                    }
                    if let Some(warning) = loaded.warning { self.needs_save = true; self.error(warning); }
                }
                Message::Picked(result) => self.picked(result, ctx),
                Message::Imported(result) => self.imported(result),
                Message::Saved(result) => {
                    self.job = None;
                    if let Some((candidate, icon)) = self.pending.take() {
                        match result {
                            Ok(saved) => { icon.apply(); self.committed(candidate, saved); }
                            Err(message) => self.error(message),
                        }
                    }
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use block2::RcBlock;
    use objc2::{AnyThread, MainThreadMarker, rc::Retained};
    use objc2_app_kit::{NSApplication, NSBitmapFormat, NSBitmapImageRep, NSColorSpace, NSDeviceRGBColorSpace, NSImage, NSModalResponseCancel, NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::{NSArray, NSSize, NSString};

    pub struct PreparedIcon { image: Retained<NSImage>, main: MainThreadMarker }

    pub(super) fn bitmap(rgba: &RgbaImage) -> Result<Retained<NSBitmapImageRep>, String> {
        const FAILED: &str = "macOS could not prepare this icon. Choose another PNG; your current icon is unchanged.";
        let (width, height) = rgba.dimensions();
        if width == 0 || height == 0 || width > ICON_DIMENSION || height > ICON_DIMENSION {
            return Err(FAILED.into());
        }
        let row_bytes = width as usize * 4;
        if rgba.as_raw().len() != row_bytes * height as usize { return Err(FAILED.into()); }
        // As in eframe's macOS workaround, never ask NSImage to decode PNG.
        // Null planes give AppKit owned storage, not a pointer into a Rust Vec.
        let rep = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(), std::ptr::null_mut(), width as isize, height as isize,
                8, 4, true, false, NSDeviceRGBColorSpace, NSBitmapFormat::AlphaNonpremultiplied,
                row_bytes as isize, 32,
            )
        }.ok_or(FAILED)?;
        let stride = usize::try_from(rep.bytesPerRow()).map_err(|_| FAILED)?;
        let required = stride.checked_mul(height as usize).ok_or(FAILED)?;
        let allocated = usize::try_from(rep.bytesPerPlane()).map_err(|_| FAILED)?;
        let data = rep.bitmapData();
        if data.is_null() || stride < row_bytes || allocated < required { return Err(FAILED.into()); }
        // SAFETY: rep owns the checked nonplanar RGBA8 buffer. Each row fits
        // its stride/allocation, and the separately owned source cannot alias it.
        for (row, pixels) in rgba.as_raw().chunks_exact(row_bytes).enumerate() {
            unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), data.add(row * stride), row_bytes); }
        }
        rep.bitmapImageRepByRetaggingWithColorSpace(&NSColorSpace::sRGBColorSpace()).ok_or_else(|| FAILED.into())
    }

    impl PreparedIcon {
        pub fn new(rgba: &RgbaImage) -> Result<Self, String> {
            let main = MainThreadMarker::new().ok_or("The app icon must be changed from the main window. Try again from Settings.")?;
            let rep = bitmap(rgba)?;
            let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(f64::from(rgba.width()), f64::from(rgba.height())));
            image.addRepresentation(&rep);
            Ok(Self { image, main })
        }

        pub fn apply(self) {
            let app = NSApplication::sharedApplication(self.main);
            // Public runtime API only: a non-null, retained image on the main
            // thread. This never writes to the signed bundle or Finder caches.
            unsafe { app.setApplicationIconImage(Some(&self.image)); }
        }
    }

    pub fn pick_png(done: impl Fn(Result<Option<PathBuf>, String>) + 'static) -> Result<(), String> {
        let main = MainThreadMarker::new().ok_or("The file picker must open from the main window. Try again from Settings.")?;
        let panel = NSOpenPanel::openPanel(main);
        panel.setTitle(Some(&NSString::from_str("Choose an application icon")));
        panel.setPrompt(Some(&NSString::from_str("Preview")));
        panel.setCanChooseFiles(true);
        panel.setCanChooseDirectories(false);
        panel.setAllowsMultipleSelection(false);
        panel.setResolvesAliases(true);
        // Public extension filter retained for compatibility with the cached
        // AppKit bindings. The bounded decoder, not this filter, validates PNG.
        #[allow(deprecated)]
        panel.setAllowedFileTypes(Some(&NSArray::from_slice(&[&*NSString::from_str("png")])));
        let selected_panel = panel.clone();
        let completion = RcBlock::new(move |response| {
            let result = if response == NSModalResponseOK {
                selected_panel.URL().and_then(|url| url.to_file_path()).map(Some)
                    .ok_or_else(|| "This image is not a readable local file. Choose a local PNG and try again.".into())
            } else if response == NSModalResponseCancel {
                Ok(None)
            } else {
                Err("The file picker could not open. Try Choose PNG again; your current icon is unchanged.".into())
            };
            done(result);
        });
        if let Some(window) = NSApplication::sharedApplication(main).keyWindow() {
            panel.beginSheetModalForWindow_completionHandler(&window, &completion);
        } else {
            panel.beginWithCompletionHandler(&completion);
        }
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    use super::*;
    const UNSUPPORTED: &str = "Application icon customization is available on macOS only. This platform keeps the bundled icon.";
    pub struct PreparedIcon;
    impl PreparedIcon {
        pub fn new(_: &RgbaImage) -> Result<Self, String> { Err(UNSUPPORTED.into()) }
        pub fn apply(self) {}
    }
    pub fn pick_png(_: impl Fn(Result<Option<PathBuf>, String>) + 'static) -> Result<(), String> { Err(UNSUPPORTED.into()) }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn saved_icon_with_draft(data_dir: &Path) -> AppIcon {
        let mut icon = AppIcon::new(data_dir);
        save(&icon.path, &icon.current).unwrap();
        icon.choose_builtin(IconChoice::Pearl);
        icon
    }

    pub(crate) fn receive_fixture_error(icon: &mut AppIcon, ctx: &Context, save_failure: bool) {
        let current = icon.current.clone();
        let draft = icon.draft.clone();
        let before = std::fs::read(&icon.path).unwrap();
        if save_failure {
            let blocker = icon.path.with_extension("blocked");
            std::fs::write(&blocker, b"synthetic write blocker").unwrap();
            // Exercise the real writer and the same error handler used by
            // Saved(Err), without constructing/applying a native main-thread icon.
            let error = save(&blocker.join("app-icon.bin"), &icon.draft).err().unwrap();
            icon.error(error);
        } else {
            icon.sender.send(Message::Imported(decode_custom(b"synthetic invalid PNG"))).unwrap();
            icon.poll(ctx);
        }
        assert!(Arc::ptr_eq(&icon.current, &current));
        assert!(Arc::ptr_eq(&icon.draft, &draft));
        assert_eq!(std::fs::read(&icon.path).unwrap(), before);
        assert!(icon.has_error);
    }

    fn fixture() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("crossbow-icon-test-").tempdir().unwrap()
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        encoded(&RgbaImage::from_pixel(width, height, Rgba([42, 105, 180, 255])))
    }

    fn encoded(image: &RgbaImage) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn with_chunk(mut png: Vec<u8>, kind: &[u8; 4], data: &[u8], after_idat: bool) -> Vec<u8> {
        let mut chunk = kind.to_vec();
        chunk.extend_from_slice(data);
        let mut framed = (data.len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(&chunk);
        framed.extend_from_slice(&png_crc(&chunk).to_be_bytes());
        let offset = if after_idat { png.len() - 12 } else { 33 };
        png.splice(offset..offset, framed);
        png
    }

    fn exif(orientation: u16) -> Vec<u8> {
        let mut bytes = b"II\x2a\x00\x08\x00\x00\x00\x01\x00\x12\x01\x03\x00\x01\x00\x00\x00".to_vec();
        bytes.extend_from_slice(&orientation.to_le_bytes());
        bytes.extend_from_slice(&[0; 6]);
        bytes
    }

    #[test]
    fn small_icons_are_never_upscaled_and_pixels_survive_reopen() {
        let dir = fixture();
        let path = dir.path().join("appearance/app-icon.bin");
        for size in [64, 128, 256, 512, 1024] {
            let source = RgbaImage::from_fn(size, size, |x, y| Rgba([x as u8, y as u8, 180, 255]));
            let icon = decode_custom(&encoded(&source)).unwrap();
            assert_eq!(icon.rgba, source);
            assert_eq!(icon.preview.size, [size.min(256) as usize; 2]);
            assert_eq!(PngDecoder::new(Cursor::new(&icon.png)).unwrap().dimensions(), (size, size));
            save(&path, &icon).unwrap();
            let loaded = load(&path);
            assert!(loaded.warning.is_none());
            assert_eq!(loaded.image.rgba, source);
            assert_eq!(loaded.image.preview, icon.preview);
        }
    }

    #[test]
    fn normalization_and_preview_filter_premultiplied_alpha_without_fringes() {
        for hidden in [[0, 0, 0, 0], [255, 0, 0, 0]] {
            // 2048 exercises normalization + preview; 512 exercises preview only.
            for size in [2048, 512] {
                let source = RgbaImage::from_fn(size, size, |x, _| Rgba(if x % 2 == 0 { [255; 4] } else { hidden }));
                let icon = decode_custom(&encoded(&source)).unwrap();
                if size == 2048 {
                    assert!(icon.rgba.pixels().all(|pixel| pixel.0 == [255, 255, 255, 128]));
                } else {
                    assert_eq!(icon.rgba, source);
                }
                let expected = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 128);
                assert!(icon.preview.pixels.iter().all(|pixel| *pixel == expected));
                assert_eq!(decode_record(&record(&icon)).unwrap().preview, icon.preview);
            }
        }
    }

    #[test]
    fn alpha_filter_preserves_low_alpha_color_and_fractional_coverage() {
        let faint = RgbaImage::from_fn(2, 2, |x, _| Rgba(if x == 0 { [255, 64, 32, 1] } else { [0, 255, 0, 0] }));
        assert_eq!(downsample(&faint, 1).get_pixel(0, 0).0, [255, 64, 32, 1]);
        let fractional = RgbaImage::from_fn(3, 3, |x, _| Rgba(if x == 1 { [255; 4] } else { [255, 0, 0, 0] }));
        assert!(downsample(&fractional, 2).pixels().all(|pixel| pixel.0 == [255, 255, 255, 85]));
    }

    #[test]
    fn exif_rotations_and_mirrors_are_applied_before_sanitizing_and_reopening() {
        let dir = fixture();
        let path = dir.path().join("appearance/app-icon.bin");
        let colors = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255, 255, 0, 255]];
        let quadrants = |corners: [usize; 4]| RgbaImage::from_fn(64, 64, |x, y| Rgba(colors[corners[usize::from(y >= 32) * 2 + usize::from(x >= 32)]]));
        let source = encoded(&quadrants([0, 1, 2, 3]));
        let expected = [[0, 1, 2, 3], [1, 0, 3, 2], [3, 2, 1, 0], [2, 3, 0, 1], [0, 2, 1, 3], [2, 0, 3, 1], [3, 1, 2, 0], [1, 3, 0, 2]];
        for (index, corners) in expected.into_iter().enumerate() {
            for after_idat in [false, true] {
                let input = with_chunk(source.clone(), b"eXIf", &exif(index as u16 + 1), after_idat);
                let icon = decode_custom(&input).unwrap();
                assert_eq!(icon.rgba, quadrants(corners), "orientation {} / after IDAT {after_idat}", index + 1);
                assert_eq!(icon.preview, preview(&quadrants(corners)));
                let mut decoder = PngDecoder::new(Cursor::new(&icon.png)).unwrap();
                assert!(decoder.exif_metadata().unwrap().is_none());
                assert_eq!(decoder.orientation().unwrap(), Orientation::NoTransforms);
                save(&path, &icon).unwrap();
                let loaded = load(&path);
                assert!(loaded.warning.is_none());
                assert_eq!(loaded.image.rgba, icon.rgba);
            }
        }
    }

    #[test]
    fn embedded_icc_is_rejected_before_inflation_with_conversion_guidance() {
        use image::{ImageEncoder, codecs::png::PngEncoder};
        let source = RgbaImage::from_pixel(64, 64, Rgba([255, 32, 128, 255]));
        let mut profile = vec![0; 128];
        profile[..4].copy_from_slice(&128u32.to_be_bytes());
        profile[12..24].copy_from_slice(b"mntrRGB XYZ ");
        profile[36..40].copy_from_slice(b"acsp");
        let mut png = Vec::new();
        let mut encoder = PngEncoder::new(&mut png);
        encoder.set_icc_profile(profile.clone()).unwrap();
        encoder.write_image(source.as_raw(), 64, 64, image::ExtendedColorType::Rgba8).unwrap();
        assert_eq!(PngDecoder::new(Cursor::new(&png)).unwrap().icc_profile().unwrap(), Some(profile));
        assert_eq!(decode_custom(&png).err().unwrap(), COLOR_RECOVERY);
        let dir = fixture();
        let mut state = AppIcon::new(dir.path());
        save(&state.path, &state.current).unwrap();
        state.choose_builtin(IconChoice::Pearl);
        let before = std::fs::read(&state.path).unwrap();
        let draft = state.draft.clone();
        state.imported(decode_custom(&png));
        assert_eq!(state.current.choice, IconChoice::Black);
        assert!(Arc::ptr_eq(&state.draft, &draft));
        assert_eq!(std::fs::read(&state.path).unwrap(), before);
        assert_eq!(state.message.as_deref(), Some(COLOR_RECOVERY));
        assert!(state.has_error);
        // Even malformed or late compressed profiles are rejected, never decoded.
        for after_idat in [false, true] {
            let invalid = with_chunk(encoded(&source), b"iCCP", b"profile\0\0not-zlib", after_idat);
            assert_eq!(decode_custom(&invalid).err().unwrap(), COLOR_RECOVERY);
        }
        assert!(COLOR_RECOVERY.contains("Convert its pixels to sRGB"));
        assert!(COLOR_RECOVERY.contains("without an embedded ICC profile"));
    }

    #[test]
    fn explicit_non_srgb_metadata_is_rejected_and_standard_srgb_is_accepted() {
        let base = png(64, 64);
        let srgb_chrm: Vec<u8> = [31270u32, 32900, 64000, 33000, 30000, 60000, 15000, 6000].into_iter().flat_map(u32::to_be_bytes).collect();
        for (kind, data) in [(b"sRGB", vec![0]), (b"gAMA", 45455u32.to_be_bytes().to_vec()), (b"cHRM", srgb_chrm), (b"cICP", vec![1, 13, 0, 1])] {
            assert!(decode_custom(&with_chunk(base.clone(), kind, &data, false)).is_ok());
        }
        for (kind, data) in [(b"gAMA", 100000u32.to_be_bytes().to_vec()), (b"cHRM", vec![0; 32]), (b"cICP", vec![9, 16, 0, 1])] {
            assert_eq!(decode_custom(&with_chunk(base.clone(), kind, &data, false)).err().unwrap(), COLOR_RECOVERY);
        }
    }

    #[test]
    fn metadata_scan_rejects_truncated_duplicate_oversized_and_bad_crc_chunks() {
        assert_eq!(png_crc(b"123456789"), 0xcbf43926);
        let base = png(64, 64);
        let mut truncated = base.clone();
        truncated[33..37].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode_custom(&truncated).is_err());
        let valid = with_chunk(base.clone(), b"eXIf", &exif(6), false);
        assert!(decode_custom(&with_chunk(valid, b"eXIf", &exif(2), true)).is_err());
        assert!(decode_custom(&with_chunk(base.clone(), b"eXIf", &vec![0; 64 * 1024 + 1], true)).is_err());
        let mut corrupt = with_chunk(base, b"eXIf", &exif(6), true);
        let crc = corrupt.len() - 13;
        corrupt[crc] ^= 1;
        assert!(decode_custom(&corrupt).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_raw_bitmap_owns_straight_rgba_after_source_drop_without_png_decode() {
        use objc2_app_kit::{NSBitmapFormat, NSColorSpace};
        // AppKit bitmap allocation only: no NSApplication, window, or icon apply.
        let pixels = [[255, 32, 64, 128], [7, 8, 9, 0], [0, 255, 0, 255], [11, 12, 13, 1]];
        let source = RgbaImage::from_fn(2, 2, |x, y| Rgba(pixels[(y * 2 + x) as usize]));
        let rep = native::bitmap(&source).unwrap();
        drop(source);
        assert_eq!(rep.pixelsWide(), 2);
        assert_eq!(rep.pixelsHigh(), 2);
        assert_eq!(rep.bitsPerSample(), 8);
        assert_eq!(rep.samplesPerPixel(), 4);
        assert!(rep.bitmapFormat().contains(NSBitmapFormat::AlphaNonpremultiplied));
        assert_eq!(rep.colorSpace(), NSColorSpace::sRGBColorSpace());
        let stride = rep.bytesPerRow() as usize;
        for y in 0..2 {
            let row = unsafe { std::slice::from_raw_parts(rep.bitmapData().add(y * stride), 8) };
            assert_eq!(row, pixels[y * 2..y * 2 + 2].concat());
        }
        assert!(native::bitmap(&RgbaImage::new(0, 0)).is_err());
        assert!(native::bitmap(&RgbaImage::new(1025, 1)).is_err());
    }

    #[test]
    fn error_reveal_is_once_per_attempt_and_cleared_with_feedback() {
        let dir = fixture();
        let mut icon = AppIcon::new(dir.path());
        assert!(!icon.take_error_reveal());
        for _ in 0..2 {
            icon.error("Repeated synthetic failure; retry the change.".into());
            assert!(icon.take_error_reveal());
            assert!(!icon.take_error_reveal());
        }
        icon.error("Discarded with a new draft.".into());
        icon.choose_builtin(IconChoice::Pearl);
        assert!(!icon.take_error_reveal());
        icon.error("Discarded with Cancel.".into());
        icon.cancel_changes();
        assert!(!icon.take_error_reveal());
        icon.committed(icon.current.clone(), Saved { warning: Some("Synthetic durability warning; apply again.".into()) });
        assert!(icon.has_error);
        assert!(icon.take_error_reveal());
        assert!(!icon.take_error_reveal());
        icon.committed(icon.current.clone(), Saved { warning: None });
        assert!(!icon.has_error);
        assert!(!icon.take_error_reveal());
    }

    #[test]
    fn no_preference_and_old_data_use_black_without_writing() {
        let dir = fixture();
        std::fs::write(dir.path().join("old-settings.json"), br#"{"theme":"dark"}"#).unwrap();
        let path = dir.path().join("appearance/app-icon.bin");
        let loaded = load(&path);
        assert_eq!(loaded.image.choice, IconChoice::Black);
        assert!(loaded.warning.is_none());
        assert!(!path.exists());
        assert_eq!(std::fs::read(dir.path().join("old-settings.json")).unwrap(), br#"{"theme":"dark"}"#);
    }

    #[test]
    fn built_in_choices_and_reset_survive_reopen() {
        let dir = fixture();
        let path = dir.path().join("appearance/app-icon.bin");
        for choice in [IconChoice::Graphite, IconChoice::Pearl, IconChoice::Black] {
            save(&path, &built_in(choice)).unwrap();
            let loaded = load(&path);
            assert_eq!(loaded.image.choice, choice);
            assert!(loaded.warning.is_none());
            assert_eq!(std::fs::read(&path).unwrap().len(), HEADER.len() + 1);
        }
    }

    #[test]
    fn custom_is_reencoded_owned_and_independent_of_original_file() {
        let dir = fixture();
        let source = dir.path().join("source.png");
        let path = dir.path().join("appearance/app-icon.bin");
        let mut bytes = png(128, 128);
        bytes.extend_from_slice(b"private trailing source data");
        std::fs::write(&source, &bytes).unwrap();
        let image = import_png(&source).unwrap();
        assert!(!image.png.windows(8).any(|bytes| bytes == b"private "));
        save(&path, &image).unwrap();
        std::fs::remove_file(source).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.image.choice, IconChoice::Custom);
        assert_eq!(loaded.image.preview, image.preview);
        assert!(loaded.warning.is_none());
        save(&path, &built_in(IconChoice::Black)).unwrap();
        assert_eq!(load(&path).image.choice, IconChoice::Black);
        assert_eq!(std::fs::read(path).unwrap().len(), HEADER.len() + 1);
    }

    #[test]
    fn invalid_oversized_nonsquare_and_invisible_images_are_rejected() {
        assert!(decode_custom(b"not PNG").is_err());
        assert!(decode_custom(&vec![0; MAX_PNG_BYTES + 1]).is_err());
        assert!(decode_custom(&png(32, 32)).is_err());
        assert!(decode_custom(&png(64, 128)).is_err());
        assert!(decode_custom(&png(2049, 2049)).is_err());
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image::RgbaImage::new(64, 64)).write_to(&mut out, ImageFormat::Png).unwrap();
        assert!(decode_custom(out.get_ref()).err().unwrap().contains("transparent"));
    }

    #[test]
    fn animated_png_is_rejected_even_with_a_valid_still_preview() {
        let mut bytes = png(64, 64);
        // acTL precedes IDAT; a default image need not be an animation frame.
        let mut chunk = b"acTL".to_vec();
        chunk.extend_from_slice(&1u32.to_be_bytes());
        chunk.extend_from_slice(&0u32.to_be_bytes());
        let mut crc = !0u32;
        for byte in &chunk {
            crc ^= u32::from(*byte);
            for _ in 0..8 { crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1)); }
        }
        let mut framed = 8u32.to_be_bytes().to_vec();
        framed.extend_from_slice(&chunk);
        framed.extend_from_slice(&(!crc).to_be_bytes());
        bytes.splice(33..33, framed);
        let error = decode_custom(&bytes).err().expect("APNG must not be accepted as a static image");
        assert!(error.contains("Animated PNG"), "{error}");
    }

    #[test]
    fn oversized_source_and_preference_files_are_rejected_before_decoding() {
        let dir = fixture();
        let source = dir.path().join("oversized.png");
        File::create(&source).unwrap().set_len(MAX_PNG_BYTES as u64 + 1).unwrap();
        assert!(import_png(&source).is_err());
        let saved = dir.path().join("app-icon.bin");
        File::create(&saved).unwrap().set_len(MAX_RECORD_BYTES as u64 + 1).unwrap();
        let loaded = load(&saved);
        assert_eq!(loaded.image.choice, IconChoice::Black);
        assert!(loaded.warning.is_some());
    }

    #[test]
    fn decoded_allocation_limit_and_normalized_size_are_bounded() {
        let image = image::ImageBuffer::from_pixel(2048, 2048, image::Rgba([u16::MAX; 4]));
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgba16(image).write_to(&mut out, ImageFormat::Png).unwrap();
        assert!(decode_custom(out.get_ref()).is_err(), "32 MiB decoded RGBA16 must be rejected");
        let accepted = decode_custom(&png(2048, 2048)).unwrap();
        let decoder = PngDecoder::new(Cursor::new(&accepted.png)).unwrap();
        assert_eq!(decoder.dimensions(), (1024, 1024));
        assert_eq!(accepted.preview.size, [256, 256]);
    }

    #[test]
    fn missing_corrupt_or_unknown_saved_data_falls_back_without_overwrite() {
        let dir = fixture();
        let path = dir.path().join("appearance/app-icon.bin");
        save(&path, &built_in(IconChoice::Graphite)).unwrap();
        for bad in [b"bad".to_vec(), [HEADER, &[3]].concat(), [HEADER, &[3], b"broken PNG"].concat(), b"CBICON\x02\x01".to_vec()] {
            std::fs::write(&path, &bad).unwrap();
            let loaded = load(&path);
            assert_eq!(loaded.image.choice, IconChoice::Black);
            assert!(loaded.warning.is_some());
            assert_eq!(std::fs::read(&path).unwrap(), bad);
        }
        std::fs::remove_file(&path).unwrap();
        assert_eq!(load(&path).image.choice, IconChoice::Black);
    }

    #[test]
    fn choosing_black_after_fallback_still_allows_repairing_saved_preference() {
        let dir = fixture();
        let mut state = AppIcon::new(dir.path());
        std::fs::create_dir_all(state.path.parent().unwrap()).unwrap();
        std::fs::write(&state.path, [HEADER, &[3]].concat()).unwrap();
        state.sender.send(Message::Loaded(load(&state.path))).unwrap();
        state.poll(&Context::default());
        assert!(state.has_error);
        state.choose_builtin(IconChoice::Black);
        assert!(!state.changed());
        assert!(state.can_apply());
        assert!(state.can_reset());
        let saved = save(&state.path, &state.draft).unwrap();
        state.committed(state.draft.clone(), saved);
        assert!(!state.can_apply());
        assert!(load(&state.path).warning.is_none());
    }

    #[test]
    fn native_cancel_and_failed_import_preserve_current_and_pending_preview() {
        let dir = fixture();
        let mut state = AppIcon::new(dir.path());
        state.choose_builtin(IconChoice::Pearl);
        let current = state.current.clone();
        let draft = state.draft.clone();
        state.job = Some(Job::Picking);
        state.picked(Ok(None), &Context::default());
        assert!(!state.busy());
        assert!(Arc::ptr_eq(&state.current, &current));
        assert!(Arc::ptr_eq(&state.draft, &draft));
        state.imported(Err("Invalid PNG".into()));
        assert!(state.has_error);
        assert!(Arc::ptr_eq(&state.current, &current));
        assert!(Arc::ptr_eq(&state.draft, &draft));
        state.cancel_changes();
        assert!(!state.changed());
        assert_eq!(state.choice(), IconChoice::Black);
        assert!(!state.path.exists());
    }

    #[test]
    fn failed_write_preserves_previous_record_and_selection_for_retry() {
        let dir = fixture();
        let mut state = AppIcon::new(dir.path());
        save(&state.path, &state.current).unwrap();
        let before = std::fs::read(&state.path).unwrap();
        state.choose_builtin(IconChoice::Pearl);
        // A regular file where the staging directory belongs forces a real
        // write error, independent of the account's permission privileges.
        let blocker = dir.path().join("blocked");
        std::fs::write(&blocker, b"fixture").unwrap();
        let error = save(&blocker.join("app-icon.bin"), &state.draft).err().unwrap();
        state.error(error);
        assert_eq!(state.current.choice, IconChoice::Black);
        assert_eq!(state.choice(), IconChoice::Pearl);
        assert_eq!(std::fs::read(&state.path).unwrap(), before);
        let saved = save(&state.path, &state.draft).unwrap();
        state.committed(state.draft.clone(), saved);
        assert_eq!(load(&state.path).image.choice, IconChoice::Pearl);
        assert!(!state.changed());
    }

    #[test]
    fn failed_atomic_rename_cleans_temporary_file() {
        let dir = fixture();
        let parent = dir.path().join("appearance");
        let target = parent.join("app-icon.bin");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep"), b"fixture").unwrap();
        assert!(save(&target, &built_in(IconChoice::Pearl)).is_err());
        assert_eq!(std::fs::read(target.join("keep")).unwrap(), b"fixture");
        assert_eq!(std::fs::read_dir(parent).unwrap().count(), 1);
    }

    #[test]
    fn preview_texture_is_cached_across_frames_and_reset_on_change() {
        let dir = fixture();
        let mut state = AppIcon::new(dir.path());
        let ctx = Context::default();
        let first = state.preview_texture(&ctx);
        for _ in 0..20 { assert_eq!(state.preview_texture(&ctx).id(), first.id()); }
        state.choose_builtin(IconChoice::Pearl);
        assert_ne!(state.preview_texture(&ctx).id(), first.id());
        let chosen = state.builtin_texture(&ctx, IconChoice::Pearl);
        assert_eq!(state.builtin_texture(&ctx, IconChoice::Pearl).id(), chosen.id());
    }

    #[cfg(unix)]
    #[test]
    fn local_record_is_private_and_symlink_sources_are_rejected() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = fixture();
        let path = dir.path().join("appearance/app-icon.bin");
        save(&path, &built_in(IconChoice::Pearl)).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        let source = dir.path().join("source.png");
        let link = dir.path().join("link.png");
        std::fs::write(&source, png(64, 64)).unwrap();
        symlink(&source, &link).unwrap();
        assert!(import_png(&link).is_err());
        assert!(import_png(&source).is_ok());
    }

}
