//! Capture in the app: a selection over one page, its pixels read from the
//! page's own frame and encoded off the UI thread, then the copy, the save
//! to Captures and the card. Peek, Little Windows and tabs in any profile
//! capture into their own page's profile.

use super::CrossbowApp;
use crate::capture::{self, Board, SavePanel};
use crate::link_prefs::{DeviceSwitch, SwitchSpec};
use crate::ui::capture::{self as capture_ui, CardAction, CardView, ListAction, Outcome, Row, Selection};
use crossbow_core::captures::{Capture, Captures, CapturesError, NewCapture};
use crossbow_core::store::Store;
use egui::{Key, Rect, TextureHandle, Vec2};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

pub const CAPTURE: &str = "Capture";
pub const SHOW_CAPTURES: &str = "Show Captures";
pub use crate::ui::capture::CARD_FOR;
pub const NOTHING_TO_CAPTURE: &str = "There's nothing on this page to capture yet. Try again once it has loaded.";
pub const EMPTY: &str = "Captures you take in this profile appear here.";
/// Thumbnails decoded per pass, so a long list never stalls a frame.
const THUMBNAILS_PER_PASS: usize = 2;

/// Command-Shift drag on a page starts a capture.
#[derive(Debug)]
pub struct CaptureDrag;

impl SwitchSpec for CaptureDrag {
    const KEY: &'static str = "capture_command_shift_drag";
    const DEFAULT: bool = true;
    const TITLE: &'static str = "Command-Shift drag to capture";
    const DESCRIPTION: &'static str = "Hold Command and Shift and drag over a page to capture that area, in every profile on this device. When off, the page gets the drag.";
    const STILL_OFF: &'static str = "Command-Shift drag still reaches the page";
    const STILL_ON: &'static str = "Command-Shift drag still captures";
    const UNREADABLE: &'static str = "The saved choice for Command-Shift drag could not be read, so it captures. Changing it saves over it.";
}

/// Each capture is kept in Captures as well as copied. On by default, as in Arc.
#[derive(Debug)]
pub struct SaveCaptures;

impl SwitchSpec for SaveCaptures {
    const KEY: &'static str = "captures_save_to_library";
    const DEFAULT: bool = true;
    const TITLE: &'static str = "Save captures to Library";
    const DESCRIPTION: &'static str = "Each capture is kept in Captures as well as copied. When off, a capture is only copied, and only Save As… writes a file.";
    const STILL_OFF: &'static str = "Captures are still only copied";
    const STILL_ON: &'static str = "Captures are still saved";
    const UNREADABLE: &'static str = "The saved choice for saving captures could not be read, so captures are saved. Changing it saves over it.";
}

/// Capture choices kept on this device only, read once each.
#[derive(Debug, Default)]
pub struct CapturePrefs {
    drag: Option<DeviceSwitch<CaptureDrag>>,
    save: HashMap<String, DeviceSwitch<SaveCaptures>>,
}

impl CapturePrefs {
    pub fn drag_switch(&mut self, store: &Store) -> &mut DeviceSwitch<CaptureDrag> {
        self.drag.get_or_insert_with(|| DeviceSwitch::load(store))
    }

    pub fn drag(&mut self, store: &Store) -> bool {
        self.drag_switch(store).enabled()
    }

    /// `profile`'s own choice, apart from other profiles' on this device.
    pub fn save_switch(&mut self, store: &Store, profile: &str) -> &mut DeviceSwitch<SaveCaptures> {
        if !self.save.contains_key(profile) { self.save.insert(profile.to_owned(), DeviceSwitch::load_for(store, profile)); }
        self.save.get_mut(profile).expect("inserted above")
    }

    pub fn save(&mut self, store: &Store, profile: &str) -> bool {
        self.save_switch(store, profile).enabled()
    }
}

/// The page a capture is taken from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Tab(String),
    Peek,
}

/// A capture's pixels and where they came from, ready to finish.
#[derive(Clone, Debug)]
pub struct Encoded {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub profile: String,
    pub url: String,
    pub title: String,
    pub created_ms: u64,
    /// The selection, in the window's points, for the card.
    pub anchor: Rect,
    /// The Little Window it came from, by key; None for the main window.
    pub owner: Option<String>,
}

/// A finished capture, as its card offers it.
#[derive(Clone, Debug)]
pub struct Finished {
    pub png: Arc<Vec<u8>>,
    pub saved: Option<Capture>,
    pub anchor: Rect,
    pub owner: Option<String>,
    pub notice: Option<String>,
    /// When the pointer last was over the card, or when it appeared.
    pub seen_at: f64,
}

impl Finished {
    /// Hidden after [`CARD_FOR`] seconds without the pointer over it.
    pub fn expired(&self, now: f64) -> bool {
        now - self.seen_at >= CARD_FOR
    }
}

/// Save `encoded` to Captures when the profile keeps them, then copy it,
/// with the saved file when there is one. A failed save or copy is said on
/// the card; a failed save leaves no file and no record.
pub fn finish(encoded: Encoded, save: bool, captures: Option<Result<&Captures, CapturesError>>, board: &mut dyn Board, now: f64) -> Finished {
    let mut notice = None;
    let saved = match (save, captures) {
        (false, _) => None,
        (true, Some(Ok(captures))) => {
            let new = NewCapture {
                profile_id: &encoded.profile,
                url: &encoded.url,
                title: &encoded.title,
                created_ms: encoded.created_ms,
                offset_secs: crate::backups::local_offset(encoded.created_ms),
                width: encoded.width,
                height: encoded.height,
                png: &encoded.png,
            };
            match captures.save(&new) {
                Ok(capture) => Some((capture.clone(), captures.path(&capture))),
                Err(error) => { notice = Some(error); None }
            }
        }
        (true, Some(Err(CapturesError::Damaged))) => { notice = Some(capture_ui::NOT_SAVED_DAMAGED.to_string()); None }
        (true, Some(Err(CapturesError::Unavailable)) | None) => { notice = Some(crossbow_core::captures::UNAVAILABLE.to_string()); None }
    };
    if let Err(error) = board.copy(&encoded.png, saved.as_ref().and_then(|(_, path)| path.as_deref())) { notice = Some(notice.map_or(error.clone(), |saving| format!("{saving} {error}"))); }
    Finished {
        png: Arc::new(encoded.png),
        saved: saved.map(|(capture, _)| capture),
        anchor: encoded.anchor,
        owner: encoded.owner,
        notice,
        seen_at: now,
    }
}

/// The Captures list's state: decoded thumbnails, the open viewer and a
/// request to show the list.
#[derive(Default)]
pub struct ListState {
    thumbnails: HashMap<i64, Option<(TextureHandle, Vec2)>>,
    /// The capture shown in the viewer, and its decoded image.
    pub viewer: Option<(i64, Option<(TextureHandle, Vec2)>)>,
    /// The Library panel opens on Captures.
    pub show: bool,
}

pub struct CaptureHost {
    pub prefs: CapturePrefs,
    pub selecting: Option<(Target, Selection)>,
    /// Peek's page this pass, for a capture started by command.
    pub peek_page: Option<Rect>,
    encoding: Option<(Encoded, Receiver<Result<Vec<u8>, String>>)>,
    pub finished: Option<Finished>,
    pub list: ListState,
    store: Option<Result<Captures, CapturesError>>,
    pub board: Box<dyn Board>,
    pub panel: Box<dyn SavePanel>,
    /// A Save As… waiting for the panel: the bytes to write.
    saving: Option<(Arc<Vec<u8>>, Receiver<Result<Option<PathBuf>, String>>)>,
    /// Said once each, as toasts.
    pub notices: Vec<String>,
    /// A tab the Capture command was chosen for, until it's shown.
    pub after_show: Option<String>,
}

impl Default for CaptureHost {
    fn default() -> Self {
        CaptureHost {
            prefs: CapturePrefs::default(),
            selecting: None,
            peek_page: None,
            encoding: None,
            finished: None,
            list: ListState::default(),
            store: None,
            board: capture::system_board(),
            panel: capture::system_save_panel(),
            saving: None,
            notices: Vec::new(),
            after_show: None,
        }
    }
}

impl CaptureHost {
    /// Open the Captures library on first use, and again after a storage
    /// error. A damaged file stays damaged until it's moved aside.
    pub fn open(&mut self, data_dir: &Path) -> Result<(), CapturesError> {
        if !matches!(self.store, Some(Ok(_) | Err(CapturesError::Damaged))) { self.store = Some(Captures::open(data_dir)); }
        match &self.store {
            Some(Err(error)) => Err(*error),
            _ => Ok(()),
        }
    }

    /// Move a damaged Captures file aside and start a new one, which lists
    /// the capture files on disk again. Says how it went.
    pub fn move_aside(&mut self, data_dir: &Path) {
        // Its connection closes first, so the file moves whole.
        self.store = None;
        self.list.thumbnails.clear();
        self.list.viewer = None;
        let notice = match crossbow_core::captures::move_aside(data_dir) {
            Ok((_, 0)) => "Moved the damaged file aside and started Captures again.".to_string(),
            Ok((_, 1)) => "Moved the damaged file aside. 1 capture is listed again, without its page.".to_string(),
            Ok((_, listed)) => format!("Moved the damaged file aside. {listed} captures are listed again, without their pages."),
            Err(error) => error,
        };
        self.notices.push(notice);
    }

    /// Start a Save As… for `png`, offering `name`.
    pub fn save_as(&mut self, png: Arc<Vec<u8>>, name: &str) {
        let chosen = self.panel.choose(name);
        self.saving = Some((png, chosen));
    }

    /// Finish a Save As… whose panel has answered: write the copy there.
    pub fn poll_save_as(&mut self) -> bool {
        let Some((png, chosen)) = &self.saving else { return false };
        let answer = match chosen.try_recv() {
            Ok(answer) => answer,
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => Err(capture::SAVE_AS_FAILED.to_string()),
        };
        if let Err(error) = answer.and_then(|path| path.map_or(Ok(()), |path| capture::write_copy(&path, png))) { self.notices.push(error); }
        self.saving = None;
        false
    }

    /// Run one of the card's actions. Open in Captures asks the app to show the list.
    pub fn card_action(&mut self, action: CardAction) {
        let Some(finished) = self.finished.clone() else { return };
        match action {
            CardAction::Copy => {
                let file = finished.saved.as_ref().and_then(|capture| opened(&self.store)?.path(capture));
                let copied = self.board.copy(&finished.png, file.as_deref());
                if let Some(card) = &mut self.finished { card.notice = copied.err(); }
            }
            CardAction::SaveAs => self.save_as(finished.png.clone(), &file_name(&finished)),
            CardAction::OpenInCaptures => {
                self.list.show = true;
                self.list.viewer = finished.saved.map(|capture| (capture.id, None));
                self.finished = None;
            }
            CardAction::Close => self.finished = None,
        }
    }

    /// Run one of the Captures list's actions on `profile`'s capture `id`.
    /// Returns a file to reveal in Finder.
    pub fn list_action(&mut self, data_dir: &Path, profile: &str, action: ListAction) -> Option<PathBuf> {
        let id = match action { ListAction::Open(id) | ListAction::Copy(id) | ListAction::Reveal(id) | ListAction::SaveAs(id) | ListAction::Delete(id) | ListAction::Remove(id) => id };
        if let Err(error) = self.open(data_dir) { self.notices.push(error.message().to_string()); return None; }
        let store = opened(&self.store)?;
        // Only this profile's captures, whatever the id says.
        let capture = store.get(id).filter(|capture| capture.profile_id == profile)?;
        let path = store.path(&capture)?;
        match action {
            ListAction::Open(_) => self.list.viewer = Some((id, None)),
            ListAction::Reveal(_) => return Some(path),
            ListAction::Copy(_) => match std::fs::read(&path) {
                Ok(png) => if let Err(error) = self.board.copy(&png, Some(&path)) { self.notices.push(error) },
                Err(_) => self.notices.push(capture_ui::FILE_MISSING.to_string()),
            },
            ListAction::SaveAs(_) => match std::fs::read(&path) {
                Ok(png) => self.save_as(Arc::new(png), &capture.file),
                Err(_) => self.notices.push(capture_ui::FILE_MISSING.to_string()),
            },
            ListAction::Delete(_) | ListAction::Remove(_) => {
                let done = if matches!(action, ListAction::Delete(_)) { store.delete(&capture) } else { store.remove_record(&capture) };
                match done {
                    Ok(()) => {
                        self.list.thumbnails.remove(&id);
                        if self.list.viewer.as_ref().is_some_and(|(open, _)| *open == id) { self.list.viewer = None; }
                    }
                    Err(error) => self.notices.push(error),
                }
            }
        }
        None
    }

    /// `profile`'s captures as the list shows them, decoding a few more
    /// thumbnails each pass.
    pub fn rows(&mut self, ctx: &egui::Context, data_dir: &Path, profile: &str) -> Result<Vec<Row>, CapturesError> {
        self.open(data_dir)?;
        let (captures, missing): (Vec<Capture>, Vec<bool>) = {
            let store = opened(&self.store).ok_or(CapturesError::Unavailable)?;
            let captures = match store.list(profile) {
                Ok(captures) => captures,
                Err(error) => {
                    // Damage found on reading is kept, as at open.
                    if error == CapturesError::Damaged { self.store = Some(Err(error)); }
                    return Err(error);
                }
            };
            let missing = captures.iter().map(|capture| !store.file_exists(capture)).collect();
            (captures, missing)
        };
        let mut decoded = 0;
        let mut rows = Vec::with_capacity(captures.len());
        for (capture, missing) in captures.into_iter().zip(missing) {
            if !missing && !self.list.thumbnails.contains_key(&capture.id) && decoded < THUMBNAILS_PER_PASS {
                decoded += 1;
                let path = opened(&self.store).and_then(|store| store.path(&capture));
                let thumbnail = path.and_then(|path| load_image(ctx, &path, Some(128), ("capture-thumbnail", capture.id)));
                self.list.thumbnails.insert(capture.id, thumbnail);
            }
            if decoded >= THUMBNAILS_PER_PASS { ctx.request_repaint(); }
            let thumbnail = self.list.thumbnails.get(&capture.id).cloned().flatten().map(|(texture, size)| (texture.id(), size));
            rows.push(Row { id: capture.id, title: title_of(&capture), detail: detail_of(&capture), missing, thumbnail });
        }
        Ok(rows)
    }
}

/// The library, once open.
fn opened(store: &Option<Result<Captures, CapturesError>>) -> Option<&Captures> {
    store.as_ref()?.as_ref().ok()
}

/// What a capture is called: its page's title, else its host.
pub fn title_of(capture: &Capture) -> String {
    if !capture.title.trim().is_empty() { return capture.title.clone(); }
    crossbow_core::search::host_of(&capture.url).unwrap_or_else(|| CAPTURE.to_string())
}

/// Its page's host and when it was taken.
pub fn detail_of(capture: &Capture) -> String {
    let when = crate::backups::date(capture.created_ms);
    match crossbow_core::search::host_of(&capture.url) {
        Some(host) => format!("{host} · {when}"),
        None => when,
    }
}

/// The name Save As… offers.
fn file_name(finished: &Finished) -> String {
    match &finished.saved {
        Some(capture) => capture.file.clone(),
        None => format!("{}.png", crossbow_core::captures::file_stem(crossbow_core::now_ms(), crate::backups::local_offset(crossbow_core::now_ms()))),
    }
}

/// A PNG file as a texture, at most `longest` pixels on its longer side.
fn load_image(ctx: &egui::Context, path: &Path, longest: Option<u32>, name: (&str, i64)) -> Option<(TextureHandle, Vec2)> {
    let bytes = std::fs::read(path).ok()?;
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).ok()?.to_rgba8();
    let (width, height) = image.dimensions();
    let image = match longest {
        Some(longest) if width.max(height) > longest => {
            let scale = longest as f32 / width.max(height) as f32;
            image::imageops::thumbnail(&image, ((width as f32 * scale).round() as u32).max(1), ((height as f32 * scale).round() as u32).max(1))
        }
        _ => image,
    };
    let size = [image.width() as usize, image.height() as usize];
    let texture = ctx.load_texture(format!("{}-{}", name.0, name.1), egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()), egui::TextureOptions::LINEAR);
    Some((texture, Vec2::new(width as f32, height as f32)))
}

impl CrossbowApp {
    /// The page a capture command starts on: Peek while it's open, else
    /// `tab` or the focused pane.
    fn capture_page(&self, tab: Option<&str>) -> Option<(Target, Rect)> {
        if tab.is_none() && self.ui.peek.is_some() { return self.capture.peek_page.map(|page| (Target::Peek, page)); }
        let tab = tab.map(str::to_owned).or_else(|| self.focused_tab_id())?;
        let page = self.ui.panes.iter().find(|(id, _)| *id == tab)?.1;
        Some((Target::Tab(tab), page))
    }

    /// The Capture command: a selection over the focused page, or over
    /// `tab` once it's shown.
    pub(crate) fn start_capture(&mut self, tab: Option<String>) {
        let Some((target, page)) = self.capture_page(tab.as_deref()) else {
            if let Some(tab) = tab { self.actions.push(super::Action::ActivateTab(tab.clone())); self.capture.after_show = Some(tab); }
            return;
        };
        self.begin_capture(target, page);
    }

    fn begin_capture(&mut self, target: Target, page: Rect) {
        self.capture.finished = None;
        self.capture.selecting = Some((target, Selection::new(page)));
        self.ui.menu = None;
    }

    /// A Command-Shift press on a page, not on something over it, starts a
    /// capture there; true when it did, and the page gets none of this pass's
    /// pointer input.
    pub(crate) fn capture_press(&mut self, ctx: &egui::Context, key: &str, page: Rect, hovered: bool) -> bool {
        let events = ctx.input(|input| input.events.clone());
        if !hovered || capture_ui::starts(&events, page, true).is_none() || !self.capture.prefs.drag(&self.lib.store) { return false; }
        self.begin_capture(if key == "peek" { Target::Peek } else { Target::Tab(key.to_owned()) }, page);
        true
    }

    fn capture_view(&self, target: &Target) -> Option<crossbow_engine::WebView> {
        match target {
            Target::Tab(tab) => self.tab(tab).and_then(|tab| tab.view.clone()),
            Target::Peek => self.ui.peek.as_ref().filter(|peek| peek.closing_at.is_none()).map(|peek| peek.view.clone()),
        }
    }

    /// Say why a capture failed, in the window it was taken in.
    pub(crate) fn capture_notice(&mut self, owner: Option<&str>, error: String) {
        match owner.and_then(|key| self.littles.iter_mut().find(|little| little.key == key)) {
            Some(little) => little.notice = Some(error),
            None => self.capture.notices.push(error),
        }
    }

    /// The selection, the card and the viewer, after the pages are drawn.
    pub(crate) fn show_capture(&mut self, ctx: &egui::Context) {
        if let Some(tab) = self.capture.after_show.take() {
            match self.capture_page(Some(&tab)) {
                Some((target, page)) => self.begin_capture(target, page),
                None if self.tab(&tab).is_some() && self.active_tab_id().as_deref() == Some(tab.as_str()) => self.capture.after_show = Some(tab),
                None => {}
            }
        }
        let ppp = ctx.pixels_per_point();
        if let Some((target, mut selection)) = self.capture.selecting.take() {
            let view = self.capture_view(&target);
            // The page must still be where the selection began.
            let shown = match &target {
                Target::Tab(tab) => self.ui.panes.iter().any(|(id, page)| id == tab && *page == selection.page),
                Target::Peek => self.capture.peek_page == Some(selection.page),
            };
            if let (Some(view), true) = (view, shown) {
                let (events, space) = ctx.input(|input| (input.events.clone(), input.key_down(Key::Space)));
                match selection.input(&events, space) {
                    Outcome::Selecting => {
                        capture_ui::paint(ctx, &selection, view.with_texture(|texture| texture.map(|texture| (texture.width, texture.height))), ppp);
                        self.capture.selecting = Some((target, selection));
                    }
                    Outcome::Cancelled => {}
                    Outcome::Chosen(rect) => self.take_capture(ctx, &view, selection.page, rect),
                }
            }
        }
        self.poll_capture(ctx);
        let pal = self.palette();
        let now = ctx.input(|input| input.time);
        if let Some(finished) = self.capture.finished.as_ref().filter(|finished| finished.owner.is_none()) {
            let view = CardView { anchor: finished.anchor, bounds: ctx.content_rect(), saved: finished.saved.is_some(), notice: finished.notice.clone() };
            let (action, hovered) = capture_ui::card(ctx, &view, &pal);
            if hovered && let Some(card) = &mut self.capture.finished { card.seen_at = now; }
            if let Some(action) = action {
                self.capture.card_action(action);
                if self.capture.list.show { self.ui.show_archive = true; }
            }
            match &self.capture.finished {
                Some(card) if card.expired(now) => self.capture.finished = None,
                Some(card) => ctx.request_repaint_after(std::time::Duration::from_secs_f64((CARD_FOR - (now - card.seen_at)).max(0.05))),
                None => {}
            }
        }
        if let Some((id, image)) = self.capture.list.viewer.clone() {
            let profile = self.current_profile();
            let data_dir = self.data_dir.clone();
            let capture = self.capture.open(&data_dir).ok().and_then(|()| opened(&self.capture.store)?.get(id)).filter(|capture| capture.profile_id == profile);
            match capture {
                None => self.capture.list.viewer = None,
                Some(capture) => {
                    let image = image.or_else(|| opened(&self.capture.store).and_then(|store| store.path(&capture)).and_then(|path| load_image(ctx, &path, None, ("capture-viewer", id))));
                    self.capture.list.viewer = Some((id, image.clone()));
                    if capture_ui::viewer(ctx, &title_of(&capture), image.map(|(texture, size)| (texture.id(), size)), &pal) { self.capture.list.viewer = None; }
                }
            }
        }
        let at = now;
        for text in self.capture.notices.drain(..) { self.ui.toasts.push(super::Toast { text, at }); }
    }

    /// Capture the part of a main-window page the selection chose.
    fn take_capture(&mut self, ctx: &egui::Context, view: &crossbow_engine::WebView, page: Rect, selection: Rect) {
        let ppp = ctx.pixels_per_point();
        let region = view.with_texture(|texture| texture.and_then(|texture| capture::pixels(selection, page, (texture.width, texture.height), ppp)));
        if let Err(error) = self.read_capture(ctx, view, region, ppp, selection.intersect(page), None) { self.capture.notices.push(error); }
    }

    /// Read `region` of the page's frame now, and encode it off the UI thread.
    pub(crate) fn read_capture(&mut self, ctx: &egui::Context, view: &crossbow_engine::WebView, region: Option<capture::PixelRect>, ppp: f32, anchor: Rect, owner: Option<String>) -> Result<(), String> {
        let region = region.ok_or(NOTHING_TO_CAPTURE)?;
        let (device, queue) = (self.render_state.device.clone(), self.render_state.queue.clone());
        let image = view.with_texture(|texture| texture.map(|texture| capture::read(&device, &queue, &texture.texture, region))).ok_or(NOTHING_TO_CAPTURE)??;
        let nav = view.nav();
        let encoded = Encoded {
            png: Vec::new(),
            width: image.width,
            height: image.height,
            profile: view.profile_id().to_owned(),
            url: nav.url,
            title: nav.title,
            created_ms: crossbow_core::now_ms(),
            anchor,
            owner,
        };
        let (send, receive) = mpsc::channel();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let _ = send.send(capture::png(&image, ppp));
            wake.request_repaint();
        });
        self.capture.encoding = Some((encoded, receive));
        Ok(())
    }

    /// A finished encoding is saved, copied and shown on the card.
    fn poll_capture(&mut self, ctx: &egui::Context) {
        if self.capture.poll_save_as() { ctx.request_repaint_after(std::time::Duration::from_millis(100)); }
        let Some((_, receive)) = &self.capture.encoding else { return };
        let encoded = match receive.try_recv() {
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("The capture couldn't be encoded. Try again.".to_string()),
            Ok(result) => result,
        };
        let (meta, _) = self.capture.encoding.take().expect("checked above");
        let png = match encoded {
            Ok(png) => png,
            Err(error) => return self.capture_notice(meta.owner.as_deref(), error),
        };
        let save = self.capture.prefs.save(&self.lib.store, &meta.profile);
        let data_dir = self.data_dir.clone();
        let host = &mut self.capture;
        let open = save.then(|| host.open(&data_dir));
        let store = open.map(|open| open.and_then(|()| opened(&host.store).ok_or(CapturesError::Unavailable)));
        let finished = finish(Encoded { png, ..meta }, save, store, host.board.as_mut(), ctx.input(|input| input.time));
        host.finished = Some(finished);
        ctx.request_repaint();
    }
}

#[cfg(test)]
#[path = "capture_host_tests.rs"]
mod tests;
