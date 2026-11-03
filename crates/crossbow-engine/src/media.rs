//! A page's videos, for Crossbow's own Picture in Picture player.
//!
//! In each renderer, `media.js` is evaluated when an http(s) top frame's
//! script context is created, before any page script. It reports the
//! frame's largest videos (at most [`MAX_VIDEOS`]) to the browser as
//! [`STATE`] messages, on media events, scrolling and resizing, at most every
//! quarter second, and runs the [`MediaCommand`]s the browser sends back.
//!
//! Everything in a report is the page's word. The browser parses each value
//! strictly and drops a report with anything out of range; a rect only ever
//! selects part of the tab's own pixels. Nothing here logs a report.
//!
//! Frames inside the page, such as embedded players from other sites, are
//! not reported.

use cef::*;
use std::cell::RefCell;
use std::collections::HashMap;

pub(crate) const STATE: &str = "crossbow.media.state";
pub(crate) const COMMAND: &str = "crossbow.media.command";
const SCRIPT: &str = include_str!("media.js");
const SCRIPT_URL: &str = "crossbow://media";
const VERSION: &str = "1";
/// The largest videos a report lists.
pub const MAX_VIDEOS: usize = 4;
const HEADER: usize = 7;
const PER_VIDEO: usize = 10;
/// Larger than any real viewport or element, in CSS pixels.
const MAX_CSS: f64 = 1.0e6;
const MAX_SEEK: f64 = 3600.0;

/// A rectangle in CSS pixels, relative to the top frame's viewport.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CssRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl CssRect {
    fn area(&self) -> f64 {
        self.width * self.height
    }
}

/// One `<video>` as the page reported it.
#[derive(Clone, Debug, PartialEq)]
pub struct Video {
    /// The page script's number for it, valid while the page stays.
    pub id: u32,
    pub paused: bool,
    pub ended: bool,
    pub muted: bool,
    /// 0 to 1.
    pub volume: f64,
    /// Its playback began during a user activation (a click or key press).
    pub user_started: bool,
    pub rect: CssRect,
}

impl Video {
    pub fn playing(&self) -> bool {
        !self.paused && !self.ended
    }

    /// Heard: not muted by the page or by Crossbow, at a volume above 0.
    /// CEF reports no audio level, so this is as close as the page can say.
    pub fn audible(&self, tab_muted: bool) -> bool {
        !self.muted && self.volume > 0.0 && !tab_muted
    }
}

/// A page's latest report.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaReport {
    /// A number the page's script chose when it started: video numbers are
    /// its own, and a new document has a new one.
    pub page: u64,
    /// Largest first.
    pub videos: Vec<Video>,
    pub device_pixel_ratio: f64,
    /// The top frame's viewport, in CSS pixels.
    pub viewport: (f64, f64),
    /// Its scroll position, in CSS pixels.
    pub scroll: (f64, f64),
}

/// A part of a texture, in its pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PixelRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl MediaReport {
    /// The video automatic Picture in Picture follows: the largest one playing
    /// audibly, or else the largest playing one the user started. A muted
    /// video that started itself, such as a background loop, never counts.
    pub fn automatic(&self, tab_muted: bool) -> Option<&Video> {
        let playing = || self.videos.iter().filter(|video| video.playing());
        largest(playing().filter(|video| video.audible(tab_muted))).or_else(|| largest(playing().filter(|video| video.user_started)))
    }

    /// The video asked for by name: the automatic one, else the largest
    /// playing, else the largest of any.
    pub fn manual(&self, tab_muted: bool) -> Option<&Video> {
        self.automatic(tab_muted)
            .or_else(|| largest(self.videos.iter().filter(|video| video.playing())))
            .or_else(|| largest(self.videos.iter()))
    }

    pub fn video(&self, id: u32) -> Option<&Video> {
        self.videos.iter().find(|video| video.id == id)
    }

    /// Whether all of `video` is inside the viewport.
    pub fn in_view(&self, video: &Video) -> bool {
        let r = video.rect;
        r.x >= -0.5 && r.y >= -0.5 && r.x + r.width <= self.viewport.0 + 0.5 && r.y + r.height <= self.viewport.1 + 0.5
    }

    /// The part of a `texture`-sized frame of the tab that shows `video`:
    /// its rect at the page's pixel ratio, cut to the frame. None when none
    /// of it is on screen.
    pub fn crop(&self, video: &Video, texture: (u32, u32)) -> Option<PixelRect> {
        let ratio = self.device_pixel_ratio;
        let (w, h) = (f64::from(texture.0), f64::from(texture.1));
        let left = (video.rect.x * ratio).clamp(0.0, w);
        let top = (video.rect.y * ratio).clamp(0.0, h);
        let right = ((video.rect.x + video.rect.width) * ratio).clamp(0.0, w);
        let bottom = ((video.rect.y + video.rect.height) * ratio).clamp(0.0, h);
        (right - left >= 1.0 && bottom - top >= 1.0).then(|| PixelRect { x: left as f32, y: top as f32, width: (right - left) as f32, height: (bottom - top) as f32 })
    }
}

fn largest<'a>(videos: impl Iterator<Item = &'a Video>) -> Option<&'a Video> {
    videos.filter(|video| video.rect.area() > 0.0).max_by(|a, b| a.rect.area().total_cmp(&b.rect.area()))
}

fn number(text: &str, range: std::ops::RangeInclusive<f64>) -> Option<f64> {
    let value: f64 = text.parse().ok()?;
    (value.is_finite() && range.contains(&value)).then_some(value)
}

fn flag(text: &str) -> Option<bool> {
    match text {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

/// Read a [`STATE`] message's strings. Anything malformed or out of range
/// drops the whole report.
pub fn parse_report(args: &[String]) -> Option<MediaReport> {
    let (header, rest) = args.split_at_checked(HEADER)?;
    if header[0] != VERSION || rest.len() % PER_VIDEO != 0 || rest.len() / PER_VIDEO > MAX_VIDEOS { return None; }
    let page: u64 = header[1].parse().ok().filter(|page| *page > 0)?;
    let device_pixel_ratio = number(&header[2], 0.05..=16.0)?;
    let viewport = (number(&header[3], 1.0..=MAX_CSS)?, number(&header[4], 1.0..=MAX_CSS)?);
    let scroll = (number(&header[5], -MAX_CSS..=MAX_CSS)?, number(&header[6], -MAX_CSS..=MAX_CSS)?);
    let mut videos: Vec<Video> = Vec::new();
    for chunk in rest.chunks(PER_VIDEO) {
        let id: u32 = chunk[0].parse().ok().filter(|id| *id > 0)?;
        if videos.iter().any(|video| video.id == id) { return None; }
        let rect = CssRect {
            x: number(&chunk[6], -MAX_CSS..=MAX_CSS)?,
            y: number(&chunk[7], -MAX_CSS..=MAX_CSS)?,
            width: number(&chunk[8], 0.0..=MAX_CSS)?,
            height: number(&chunk[9], 0.0..=MAX_CSS)?,
        };
        videos.push(Video {
            id,
            paused: flag(&chunk[1])?,
            ended: flag(&chunk[2])?,
            muted: flag(&chunk[3])?,
            volume: number(&chunk[4], 0.0..=1.0)?,
            user_started: flag(&chunk[5])?,
            rect,
        });
    }
    videos.sort_by(|a, b| b.rect.area().total_cmp(&a.rect.area()));
    Some(MediaReport { page, videos, device_pixel_ratio, viewport, scroll })
}

/// What the player asks of a page's video. Each becomes exactly one message.
#[derive(Clone, Debug, PartialEq)]
pub enum MediaCommand {
    Play(u32),
    Pause(u32),
    /// Move playback by this many seconds, within the video.
    Seek { video: u32, seconds: f64 },
    SetMuted { video: u32, muted: bool },
    /// Scroll the video into view, centred.
    Reveal(u32),
    /// Scroll the page back to where it was, in CSS pixels.
    Restore { x: f64, y: f64 },
}

impl MediaCommand {
    /// The command as a name and two numbers: the video and a value, or a
    /// restore's x and y.
    pub fn encode(&self) -> [String; 3] {
        let (name, first, second) = match *self {
            MediaCommand::Play(video) => ("play", f64::from(video), 0.0),
            MediaCommand::Pause(video) => ("pause", f64::from(video), 0.0),
            MediaCommand::Seek { video, seconds } => ("seek", f64::from(video), seconds.clamp(-MAX_SEEK, MAX_SEEK)),
            MediaCommand::SetMuted { video, muted } => ("mute", f64::from(video), if muted { 1.0 } else { 0.0 }),
            MediaCommand::Reveal(video) => ("reveal", f64::from(video), 0.0),
            MediaCommand::Restore { x, y } => ("restore", x.clamp(-MAX_CSS, MAX_CSS), y.clamp(-MAX_CSS, MAX_CSS)),
        };
        [name.to_string(), format!("{first}"), format!("{second}")]
    }

    /// The renderer's reading of [`Self::encode`], every part checked.
    pub fn decode(args: &[String]) -> Option<(&'static str, f64, f64)> {
        let [name, first, second] = args else { return None };
        let name = ["play", "pause", "seek", "mute", "reveal", "restore"].into_iter().find(|known| known == name)?;
        Some((name, number(first, -MAX_CSS..=MAX_CSS)?, number(second, -MAX_CSS..=MAX_CSS)?))
    }
}

// ---------------------------------------------------------------------------
// Renderer process
// ---------------------------------------------------------------------------

thread_local! {
    /// Each top frame's script context and the command function `media.js`
    /// returned, by frame id. Renderer main thread only.
    static COMMANDS: RefCell<HashMap<String, (V8Context, V8Value)>> = RefCell::new(HashMap::new());
}

pub(crate) fn strings(message: &ProcessMessage) -> Option<Vec<String>> {
    let list = message.argument_list()?;
    (0..list.size()).map(|i| (list.get_type(i) == ValueType::STRING).then(|| CefString::from(&list.string(i)).to_string())).collect()
}

pub(crate) fn outgoing(name: &str, args: &[String]) -> Option<ProcessMessage> {
    let message = process_message_create(Some(&CefString::from(name)))?;
    let list = message.argument_list()?;
    list.set_size(args.len());
    for (i, arg) in args.iter().enumerate() {
        list.set_string(i, Some(&CefString::from(arg.as_str())));
    }
    Some(message)
}

wrap_v8_handler! {
    struct ReportBuilder;

    impl V8Handler {
        fn execute(
            &self,
            _name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            arguments: Option<&[Option<V8Value>]>,
            _retval: Option<&mut Option<V8Value>>,
            _exception: Option<&mut CefString>,
        ) -> ::std::os::raw::c_int {
            report(arguments.unwrap_or_default());
            1
        }
    }
}

/// A report from `media.js`, sent on from the calling context's own frame,
/// which must be the top one. The browser checks it again.
fn report(arguments: &[Option<V8Value>]) {
    let Some(context) = v8_context_get_current_context() else { return };
    let Some(frame) = context.frame().filter(|frame| frame.is_main() == 1) else { return };
    if arguments.len() > HEADER + MAX_VIDEOS * PER_VIDEO { return; }
    let args: Option<Vec<String>> = arguments.iter().map(|value| {
        let value = value.as_ref()?;
        (value.is_string() == 1).then(|| CefString::from(&value.string_value()).to_string())
    }).collect();
    if let Some(mut message) = args.and_then(|args| outgoing(STATE, &args)) {
        frame.send_process_message(ProcessId::BROWSER, Some(&mut message));
    }
}

/// Install `media.js` in an http(s) top frame's new script context.
pub(crate) fn inject(frame: &Frame, context: &mut V8Context) {
    if frame.is_main() != 1 || crate::login::page_origin(&CefString::from(&frame.url()).to_string()).is_none() { return; }
    if context.enter() != 1 { return; }
    let report = v8_value_create_function(Some(&CefString::from("report")), Some(&mut ReportBuilder::new()));
    let (mut factory, mut exception): (Option<V8Value>, Option<V8Exception>) = (None, None);
    let command = (context.eval(Some(&CefString::from(SCRIPT)), Some(&CefString::from(SCRIPT_URL)), 1, Some(&mut factory), Some(&mut exception)) == 1)
        .then_some(factory).flatten()
        .filter(|factory| factory.is_function() == 1)
        .and_then(|factory| factory.execute_function_with_context(Some(context), None, Some(&[report])))
        .filter(|command| command.is_function() == 1);
    context.exit();
    if let Some(command) = command {
        let id = CefString::from(&frame.identifier()).to_string();
        COMMANDS.with_borrow_mut(|commands| commands.insert(id, (context.clone(), command)));
    }
}

pub(crate) fn release(frame: &Frame, context: &mut V8Context) {
    let id = CefString::from(&frame.identifier()).to_string();
    COMMANDS.with_borrow_mut(|commands| {
        if commands.get_mut(&id).is_some_and(|(stored, _)| stored.is_same(Some(&mut *context)) == 1) { commands.remove(&id); }
    });
}

/// A command from the browser, for the top frame it was sent to.
pub(crate) fn command(frame: &Frame, message: &ProcessMessage) -> bool {
    if CefString::from(&message.name()).to_string() != COMMAND { return false; }
    let Some((name, first, value)) = strings(message).as_deref().and_then(MediaCommand::decode) else { return true };
    let id = CefString::from(&frame.identifier()).to_string();
    COMMANDS.with_borrow_mut(|commands| {
        let Some((context, command)) = commands.get_mut(&id) else { return };
        if context.is_valid() != 1 || context.enter() != 1 { return; }
        let args = [v8_value_create_string(Some(&CefString::from(name))), v8_value_create_double(first), v8_value_create_double(value)];
        command.execute_function_with_context(Some(&mut *context), None, Some(&args));
        context.exit();
    });
    true
}

// ---------------------------------------------------------------------------
// Browser process
// ---------------------------------------------------------------------------

/// A [`STATE`] message from `frame`: the report, if it came from the top
/// frame and reads correctly. None for a report to drop.
pub(crate) fn receive(frame: &Frame, message: &ProcessMessage) -> Option<Option<MediaReport>> {
    if CefString::from(&message.name()).to_string() != STATE { return None; }
    if frame.is_main() != 1 { return Some(None); }
    Some(strings(message).as_deref().and_then(parse_report))
}

/// Chromium's own Picture in Picture can't open from a windowless browser, so
/// turn the web APIs off: pages then hide their Picture in Picture buttons
/// instead of offering one that fails. Crossbow's player replaces them.
pub(crate) const DISABLED_BLINK_FEATURES: &[&str] = &["PictureInPictureAPI", "DocumentPictureInPictureAPI"];
pub(crate) const DISABLE_BLINK_FEATURES: &str = "disable-blink-features";

/// `existing`, a comma-separated feature list, with `add` appended where
/// missing. Chromium reads only the last value of a repeated switch, so the
/// list is merged rather than replaced.
pub(crate) fn merge_features(existing: &str, add: &[&str]) -> String {
    let mut features: Vec<&str> = existing.split(',').map(str::trim).filter(|feature| !feature.is_empty()).collect();
    for feature in add {
        if !features.contains(feature) { features.push(feature); }
    }
    features.join(",")
}

#[cfg(test)]
#[path = "media_tests.rs"]
pub(crate) mod tests;
