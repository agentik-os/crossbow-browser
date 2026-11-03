//! One Chromium browser rendered off-screen into a [`WebTexture`].

use crate::downloads::{DownloadControl, DownloadInfo, DownloadRequest};
use crate::input::{self, KeyInput, KeySignature, Modifiers, MouseButton, UnhandledKeys};
use crate::login::{self, LoginCapture, LoginState, PageAddress};
use crate::media::{self, MediaCommand, MediaReport};
use crate::permissions::{self, Callback, ExternalLink, Pending, PermissionRequest};
use crate::screen::{ScreenGeometry, ScreenRect, ViewPosition};
use crate::texture::{self, WebTexture};
use crate::translate::{self, TranslateCommand, TranslateReport};
use cef::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

pub struct WebViewConfig {
    pub url: String,
    pub profile_id: String,
    pub data_dir: PathBuf,
    /// Logical (DIP) size of the view.
    pub size: (f32, f32),
    pub scale: f32,
    pub screen: Option<ScreenGeometry>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// A tab, which may offer to save a page's login. Peek and Little Windows
    /// never do, so their pages never capture one.
    pub saves_logins: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorKind {
    #[default]
    Default,
    Pointer,
    Text,
    Crosshair,
    Wait,
    Progress,
    Help,
    Move,
    NotAllowed,
    Grab,
    Grabbing,
    ResizeEw,
    ResizeNs,
    ResizeNesw,
    ResizeNwse,
    ZoomIn,
    ZoomOut,
    None,
}

/// Navigation, display and content-free interaction state.
#[derive(Clone, Debug, Default)]
pub struct NavState {
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub progress: f64,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub favicon_url: Option<String>,
    /// Link under the pointer.
    pub status: Option<String>,
    pub tooltip: Option<String>,
    pub cursor: CursorKind,
    pub fullscreen: bool,
    /// `(matches, active ordinal)` of the last find request.
    pub find: Option<(i32, i32)>,
    /// The focused element accepts text (drives IME enablement).
    pub editable_focus: bool,
    pub crashed: bool,
    /// Content-free, lifetime latch for potentially state-changing page input.
    /// False for unloaded saved tabs; use the live view's `can_auto_archive` as authority.
    pub auto_archive_protected: bool,
}

impl NavState {
    fn for_live_view(url: String) -> Self {
        // Until CEF reports loading state, a new browser is not safe to archive.
        Self { url, loading: true, ..Default::default() }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ContextMenuInfo {
    /// View coordinates (logical).
    pub x: i32,
    pub y: i32,
    pub link_url: String,
    pub image_url: String,
    pub selection_text: String,
    pub page_url: String,
    /// The address of the frame the menu was opened in. CEF leaves it
    /// empty for the top page.
    pub frame_url: String,
    pub is_editable: bool,
}

/// The address whose origin a frame's document has, from the frame's own
/// address followed by its ancestors': its own, or for about:blank,
/// about:srcdoc or a frame not loaded yet, its nearest ancestor's.
fn document_address(chain: impl IntoIterator<Item = String>) -> String {
    let mut own = None;
    for address in chain {
        let inherits = address.is_empty() || address == "about:srcdoc" || address.split(['?', '#']).next() == Some("about:blank");
        if !inherits { return address; }
        own.get_or_insert(address);
    }
    own.unwrap_or_default()
}

#[derive(Clone, Debug)]
pub enum WebEvent {
    /// The page asked for a new tab or window (target=_blank, window.open, middle click).
    /// `modifiers` were held for the click or key press that asked; none without a user gesture.
    OpenUrl { url: String, background: bool, popup: bool, modifiers: Modifiers },
    Favicon { page_url: String, width: u32, height: u32, rgba: Vec<u8> },
    DownloadRequested(DownloadRequest),
    Download(DownloadInfo),
    ContextMenu(ContextMenuInfo),
    LoadError { url: String, code: i32, text: String },
    /// A main-frame load finished (for history).
    Visited { url: String, title: String },
    /// A login was submitted and a navigation or request followed it. Its
    /// username and password wipe themselves when dropped.
    Credential(LoginCapture),
    /// A page asked for permissions; answer with [`WebView::answer_permission`].
    Permission(PermissionRequest),
    /// A request needs no answer any more: CEF closed its prompt.
    PermissionDismissed(u64),
    /// A page opened a link that only another app can open. Crossbow's
    /// engine never opens it.
    ExternalLink(ExternalLink),
    /// A fill reached its frame, which had no visible, enabled password field
    /// to fill and no login field with focus.
    LoginNotFilled,
    /// An address-shaped form was submitted and a navigation or request
    /// followed it. Its parts wipe themselves when dropped.
    Address(PageAddress),
    /// An address fill reached its frame, which had no address form whose
    /// fields the user can see.
    AddressNotFilled,
    /// A card fill reached its frame, which had no card field the user can
    /// see, or no single card form to choose.
    CardNotFilled,
    /// An address fill waited for the engine's answer on which fields the
    /// user can see, and left some or all of them empty: see
    /// [`login::FillReport`]. One that filled every field sends nothing, and
    /// one that found nothing to fill after all sends [`WebEvent::AddressNotFilled`].
    AddressFillReport(login::FillReport),
    /// The same, for a card fill.
    CardFillReport(login::FillReport),
    Closed,
}

struct Shared {
    nav: RefCell<NavState>,
    events: RefCell<Vec<WebEvent>>,
    download_controls: RefCell<HashMap<u32, DownloadControl>>,
    /// Presses sent with `key_tracked`, waiting for CEF to report them unhandled.
    unhandled: RefCell<UnhandledKeys>,
    /// Modifiers of the last mouse button or key press sent to the page.
    gesture: Cell<Modifiers>,
    size: Cell<(f32, f32)>,
    scale: Cell<f32>,
    screen: Cell<Option<ScreenGeometry>>,
    root: Cell<Option<ScreenRect>>,
    position: Cell<Option<ViewPosition>>,
    texture: RefCell<Option<WebTexture>>,
    popup_texture: RefCell<Option<WebTexture>>,
    popup_rect: Cell<Option<(i32, i32, i32, i32)>>,
    browser: RefCell<Option<Browser>>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    profile_id: String,
    accelerated: bool,
    /// Distinct for every view this process creates.
    serial: u64,
    /// Main-frame address changes and load starts so far.
    navigations: Cell<u64>,
    /// Main-frame page loads so far, reloads included.
    loads: Cell<u64>,
    login: RefCell<LoginState>,
    permissions: RefCell<Pending<Callback>>,
    /// The top frame's latest video report and when it came. A new document
    /// clears it.
    media: RefCell<Option<(MediaReport, std::time::Instant)>>,
    /// Translate Page reports from the top frame, oldest first, until taken.
    /// A new document clears them.
    translate: RefCell<Vec<TranslateReport>>,
    /// The latest top-frame document response's `Content-Language`, from the
    /// IO thread.
    languages: Arc<Mutex<translate::Languages>>,
    /// Addresses the content rules cancelled on the current page, each once,
    /// from the IO thread.
    blocked: Arc<crate::content_rules::BlockedCount>,
    saves_logins: bool,
}

fn next_serial() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Shared {
    fn push(&self, e: WebEvent) {
        self.events.borrow_mut().push(e);
    }

    /// A load or address change in `frame` delivers the login and address it was submitting.
    fn login_navigated(&self, frame: &Frame) {
        let id = CefString::from(&frame.identifier()).to_string();
        let capture = self.login.borrow_mut().navigated(&id, frame.is_main() == 1, std::time::Instant::now());
        if let Some(capture) = capture { self.push(WebEvent::Credential(capture)); }
        self.followed_address();
    }

    /// Report a submitted address once a navigation or request followed it.
    fn followed_address(&self) {
        let address = self.login.borrow_mut().take_followed_address();
        if let Some(address) = address { self.push(WebEvent::Address(address)); }
    }

    /// A Translate Page report from a renderer, from the top frame only.
    fn translate_message(&self, frame: &Frame, message: &ProcessMessage) -> bool {
        let Some(report) = translate::receive(frame, message) else { return false };
        if let Some(report) = report {
            let mut reports = self.translate.borrow_mut();
            // The app takes them every frame; a page that floods is cut off.
            if reports.len() < 64 { reports.push(report); }
        }
        crate::wake(std::time::Duration::ZERO);
        true
    }

    /// A video report from a renderer, from the top frame only.
    fn media_message(&self, frame: &Frame, message: &ProcessMessage) -> bool {
        let Some(report) = media::receive(frame, message) else { return false };
        *self.media.borrow_mut() = report.map(|report| (report, std::time::Instant::now()));
        crate::wake(std::time::Duration::ZERO);
        true
    }

    /// A frame's renderer asks for its new document's hiding sheet.
    fn hiding_message(&self, browser: Option<&Browser>, frame: &Frame, message: &ProcessMessage) -> bool {
        if s(&message.name()) != crate::hiding::ASK { return false; }
        crate::hiding::answer(&self.profile_id, browser, frame, message);
        true
    }

    /// A new document starts loading in `frame`: what its renderer reported
    /// about the frame's last document no longer holds.
    fn login_document_started(&self, frame: &Frame) {
        let id = CefString::from(&frame.identifier()).to_string();
        self.login.borrow_mut().document_started(&id, frame.is_main() == 1);
    }

    /// Tell the renderer of a frame that starts loading this view's capture
    /// setting, so one that missed a change knows it before any submit.
    fn tell_login_setting(&self, frame: &Frame) {
        // A frame already gone has no renderer to tell.
        if frame.is_valid() != 1 { return; }
        if let Some(mut message) = login::setting_message(&self.profile_id) { frame.send_process_message(ProcessId::RENDERER, Some(&mut message)); }
    }

    /// A message from a renderer. Only `crossbow.login.*` messages are ours;
    /// each is checked against this process's own view of the sending frame.
    fn login_message(&self, frame: &Frame, message: &ProcessMessage) -> bool {
        let name = s(&message.name());
        if !name.starts_with("crossbow.login.") { return false; }
        // Dropped before any value is copied out of CEF's message; CEF's own copy isn't ours to wipe.
        if login::carries_login(&name) && !(self.saves_logins && login::login_capture(&self.profile_id)) { return true; }
        let Some(args) = login::string_args(message) else { return true };
        let now = std::time::Instant::now();
        if name == login::UNFILLED {
            if self.login.borrow_mut().unfilled(frame, args, now) { self.push(WebEvent::LoginNotFilled); }
        } else if name == login::ADDRESS_UNFILLED {
            if self.login.borrow_mut().address_unfilled(frame, args, now) { self.push(WebEvent::AddressNotFilled); }
        } else if name == login::CARD_UNFILLED {
            if self.login.borrow_mut().card_unfilled(frame, args, now) { self.push(WebEvent::CardNotFilled); }
        } else if name == login::ADDRESS_FILLED {
            let report = self.login.borrow_mut().address_filled(frame, args, now);
            if let Some(event) = fill_event(report, WebEvent::AddressNotFilled, WebEvent::AddressFillReport) { self.push(event); }
        } else if name == login::CARD_FILLED {
            let report = self.login.borrow_mut().card_filled(frame, args, now);
            if let Some(event) = fill_event(report, WebEvent::CardNotFilled, WebEvent::CardFillReport) { self.push(event); }
        } else if let Some(capture) = login::receive(&mut self.login.borrow_mut(), frame, &name, args, now, true) {
            self.push(WebEvent::Credential(capture));
        }
        self.followed_address();
        crate::wake(std::time::Duration::ZERO);
        true
    }

    /// The page asked for permissions: hold CEF's callback until the app answers.
    fn ask_permission(&self, prompt: Option<u64>, media: bool, bits: u32, origin: String, callback: Callback) {
        let id = self.permissions.borrow_mut().hold(prompt, callback);
        let page = self.nav.borrow().url.clone();
        self.push(WebEvent::Permission(PermissionRequest { id, media, bits, origin, page, load: self.loads.get() }));
        crate::wake(std::time::Duration::ZERO);
    }

    /// Deny what the page asked that is still unanswered, as when it navigates or closes.
    fn deny_permissions(&self) {
        for id in permissions::deny_all(&self.permissions) { self.push(WebEvent::PermissionDismissed(id)); }
    }

    /// A link only another app can open: never loaded here, handed to the app.
    fn external_link(&self, url: String, gesture: bool, frame: String) {
        let page = self.nav.borrow().url.clone();
        self.push(WebEvent::ExternalLink(ExternalLink { url, gesture, page, frame, load: self.loads.get() }));
        crate::wake(std::time::Duration::ZERO);
    }
}

fn s(u: &CefStringUserfree) -> String {
    CefString::from(u).to_string()
}

/// What a fill's report tells the app: nothing when every field took its
/// value, `not_filled` when none could, else the report.
pub(crate) fn fill_event(report: Option<login::FillReport>, not_filled: WebEvent, reported: fn(login::FillReport) -> WebEvent) -> Option<WebEvent> {
    match report? {
        login::FillReport::Done { filled: 0, hidden: 0 } => Some(not_filled),
        login::FillReport::Done { hidden: 0, .. } => None,
        report => Some(reported(report)),
    }
}

fn opt(v: Option<&CefString>) -> String {
    v.map(|s| s.to_string()).unwrap_or_default()
}

/// The main frame's rendered text, delivered once by the renderer.
#[derive(Clone)]
pub struct PageText(Rc<RefCell<PageTextSlot>>);

struct PageTextSlot {
    cap: usize,
    text: Option<(String, bool)>,
    delivered: bool,
}

impl PageText {
    pub fn new(cap: usize) -> Self {
        Self(Rc::new(RefCell::new(PageTextSlot { cap, text: None, delivered: false })))
    }

    /// Keeps at most `cap` bytes of the first delivery, cut on a character
    /// boundary, and whether anything was cut. Later deliveries are ignored.
    pub fn deliver(&self, text: Option<&str>) {
        let mut slot = self.0.borrow_mut();
        if slot.delivered { return; }
        slot.delivered = true;
        let text = text.unwrap_or_default();
        let mut end = text.len().min(slot.cap);
        while !text.is_char_boundary(end) { end -= 1; }
        slot.text = Some((text[..end].to_owned(), end < text.len()));
    }

    /// The kept text, once.
    pub fn take(&self) -> Option<(String, bool)> {
        self.0.borrow_mut().text.take()
    }
}

wrap_string_visitor! {
    struct PageTextBuilder {
        slot: PageText,
    }

    impl CefStringVisitor {
        fn visit(&self, string: Option<&CefString>) {
            self.slot.deliver(string.map(CefString::to_string).as_deref());
            crate::wake(std::time::Duration::ZERO);
        }
    }
}

/// A tab's web contents.
#[derive(Clone)]
pub struct WebView {
    shared: Rc<Shared>,
}

impl WebView {
    pub fn new(cfg: WebViewConfig) -> Option<WebView> {
        if !cfg.scale.is_finite() || cfg.scale <= 0.0 { return None; }
        let mut context = crate::profile_context(&cfg.data_dir, &cfg.profile_id)?;
        let accelerated = cfg!(any(target_os = "macos", target_os = "windows"))
            && std::env::var("CROSSBOW_SOFTWARE_PAINT").is_err();
        let shared = Rc::new(Shared {
            nav: RefCell::new(NavState::for_live_view(cfg.url.clone())),
            events: RefCell::new(Vec::new()),
            download_controls: RefCell::new(HashMap::new()),
            unhandled: RefCell::new(UnhandledKeys::default()),
            gesture: Cell::new(Modifiers::default()),
            size: Cell::new((cfg.size.0.max(1.0), cfg.size.1.max(1.0))),
            scale: Cell::new(cfg.scale),
            screen: Cell::new(cfg.screen.filter(|screen| screen.is_valid())),
            root: Cell::new(None),
            position: Cell::new(None),
            texture: RefCell::new(None),
            popup_texture: RefCell::new(None),
            popup_rect: Cell::new(None),
            browser: RefCell::new(None),
            device: cfg.device,
            queue: cfg.queue,
            profile_id: cfg.profile_id,
            accelerated,
            serial: next_serial(),
            navigations: Cell::new(0),
            loads: Cell::new(0),
            login: RefCell::new(LoginState::default()),
            permissions: RefCell::new(Pending::default()),
            media: RefCell::new(None),
            translate: RefCell::new(Vec::new()),
            languages: Arc::default(),
            blocked: Arc::default(),
            saves_logins: cfg.saves_logins,
        });
        let window_info = WindowInfo {
            windowless_rendering_enabled: 1,
            shared_texture_enabled: accelerated as i32,
            external_begin_frame_enabled: 0,
            ..Default::default()
        };
        let settings = BrowserSettings {
            windowless_frame_rate: 60,
            background_color: 0xFFFF_FFFF,
            ..Default::default()
        };
        let mut client = ClientBuilder::build(shared.clone());
        // Every renderer this browser uses learns the profile's capture setting from it.
        let mut info = login::creation_info(&shared.profile_id, shared.saves_logins);
        let browser = browser_host_create_browser_sync(
            Some(&window_info),
            Some(&mut client),
            Some(&CefString::from(cfg.url.as_str())),
            Some(&settings),
            info.as_mut(),
            Some(&mut context),
        )?;
        shared.browser.replace(Some(browser));
        Some(WebView { shared })
    }

    fn browser(&self) -> Option<Browser> {
        self.shared.browser.borrow().clone()
    }

    fn host(&self) -> Option<BrowserHost> {
        self.browser().and_then(|b| b.host())
    }

    fn frame(&self) -> Option<Frame> {
        self.browser().and_then(|b| b.focused_frame().or_else(|| b.main_frame()))
    }

    pub fn is_accelerated(&self) -> bool {
        self.shared.accelerated
    }

    /// Immutable owner of this view's Chromium request context.
    pub fn profile_id(&self) -> &str {
        &self.shared.profile_id
    }

    /// Distinct for every view this process creates; clones share it.
    pub fn serial(&self) -> u64 {
        self.shared.serial
    }

    /// Main-frame navigations so far: address changes (same-document ones
    /// included) and load starts (reloads included). Frames don't count.
    pub fn navigations(&self) -> u64 {
        self.shared.navigations.get()
    }

    pub fn nav(&self) -> NavState {
        self.shared.nav.borrow().clone()
    }

    /// Whether this view passes the live interaction/lifecycle auto-archive guard.
    /// Potentially state-changing input protects the view for its entire lifetime,
    /// including navigation/reload: history may still hold unsaved edits. This is
    /// deliberately conservative, not dirty-form detection; read-only interaction
    /// also protects a page, while scripted changes are not detected. No page
    /// content is retained, and manual close/restart recovery is not provided.
    /// Unknown initial loading state, loading, crashes and absent browsers refuse
    /// archiving. Call this immediately before an automatic close, not a snapshot.
    pub fn can_auto_archive(&self) -> bool {
        let nav = self.shared.nav.borrow();
        !nav.auto_archive_protected && !nav.loading && !nav.crashed && self.shared.browser.borrow().is_some()
    }

    /// Main-frame page loads so far, reloads included. An [`ExternalLink`]
    /// carries the one it came from.
    pub fn page_loads(&self) -> u64 {
        self.shared.loads.get()
    }

    /// Requests the profile's content rules cancelled on the current page.
    /// A reload starts again at 0.
    pub fn blocked(&self) -> u32 {
        self.shared.blocked.get(self.shared.loads.get())
    }

    /// Answer request `id` from a [`WebEvent::Permission`]: allow grants
    /// exactly what was asked. False if it was already answered, CEF
    /// dismissed it, or its page went away.
    pub fn answer_permission(&self, id: u64, allow: bool) -> bool {
        permissions::answer(&self.shared.permissions, id, allow)
    }

    pub fn take_events(&self) -> Vec<WebEvent> {
        self.shared.login.borrow_mut().expire(std::time::Instant::now());
        std::mem::take(&mut *self.shared.events.borrow_mut())
    }

    /// Drain only downloads during root logic, including hidden-window ticks.
    /// Leave navigation, menus and display events in order for the next UI pass.
    pub fn take_download_events(&self) -> Vec<WebEvent> {
        self.shared.events.borrow_mut().extract_if(.., |event| matches!(event, WebEvent::Download(_) | WebEvent::DownloadRequested(_))).collect()
    }

    /// Run `f` with the latest frame (if any has been painted yet).
    pub fn with_texture<R>(&self, f: impl FnOnce(Option<&WebTexture>) -> R) -> R {
        f(self.shared.texture.borrow().as_ref())
    }

    /// The open `<select>` popup and its rectangle in view coordinates.
    pub fn with_popup<R>(&self, f: impl FnOnce(Option<(&WebTexture, (i32, i32, i32, i32))>) -> R) -> R {
        let rect = self.shared.popup_rect.get();
        let tex = self.shared.popup_texture.borrow();
        match (tex.as_ref(), rect) {
            (Some(t), Some(r)) => f(Some((t, r))),
            _ => f(None),
        }
    }

    pub fn size(&self) -> (f32, f32) {
        self.shared.size.get()
    }

    /// Compatibility helper when only monitor bounds change. Call
    /// `set_display_geometry` with current root bounds and egui scale on display changes.
    pub fn set_screen_geometry(&self, screen: Option<ScreenGeometry>) {
        self.set_display_geometry(screen, self.shared.root.get(), self.shared.scale.get());
    }

    /// Publish monitor, root window and scale together before notifying CEF. Hidden
    /// views keep their viewport/texture; invalid updates leave the last state intact.
    pub fn set_display_geometry(&self, screen: Option<ScreenGeometry>, root: Option<ScreenRect>, scale: f32) {
        if !scale.is_finite() || scale <= 0.0 || screen.is_some_and(|screen| !screen.is_valid()) || root.is_some_and(|root| !root.is_valid()) { return; }
        if self.shared.screen.get() == screen && self.shared.root.get() == root && self.shared.scale.get() == scale { return; }
        self.shared.screen.set(screen);
        self.shared.root.set(root);
        self.shared.scale.set(scale);
        if let Some(host) = self.host() { host.notify_screen_info_changed(); }
    }

    /// Publish the actual pane position without resizing its content. None means
    /// no native position is available; CEF must use its screen-point fallback.
    pub fn set_view_position(&self, position: Option<ViewPosition>) {
        if position.is_some_and(|position| !position.is_valid()) { return; }
        if self.shared.position.replace(position) != position {
            if let Some(host) = self.host() { host.notify_screen_info_changed(); }
        }
    }

    /// Resize the view (logical points). Cheap when unchanged.
    pub fn resize(&self, width: f32, height: f32, scale: f32) {
        if !scale.is_finite() || scale <= 0.0 { return; }
        let new = (width.max(1.0).round(), height.max(1.0).round());
        let changed_scale = self.shared.scale.get() != scale;
        if self.shared.size.get() != new || changed_scale {
            self.shared.size.set(new);
            self.shared.scale.set(scale);
            if let Some(h) = self.host() {
                if changed_scale {
                    h.notify_screen_info_changed();
                }
                h.was_resized();
            }
        }
    }

    /// Hidden views stop painting and get throttled timers.
    pub fn set_hidden(&self, hidden: bool) {
        if let Some(h) = self.host() {
            h.was_hidden(hidden as i32);
        }
    }

    pub fn set_focus(&self, focus: bool) {
        if let Some(h) = self.host() {
            h.set_focus(focus as i32);
        }
    }

    pub fn load_url(&self, url: &str) {
        if let Some(f) = self.browser().and_then(|b| b.main_frame()) {
            f.load_url(Some(&CefString::from(url)));
        }
    }

    pub fn go_back(&self) {
        if let Some(b) = self.browser() {
            b.go_back();
        }
    }

    pub fn go_forward(&self) {
        if let Some(b) = self.browser() {
            b.go_forward();
        }
    }

    pub fn reload(&self) {
        if let Some(b) = self.browser() {
            b.reload();
        }
    }

    pub fn reload_ignore_cache(&self) {
        if let Some(b) = self.browser() {
            b.reload_ignore_cache();
        }
    }

    pub fn stop(&self) {
        if let Some(b) = self.browser() {
            b.stop_load();
        }
    }

    // -- input ---------------------------------------------------------------

    fn protect_from_auto_archive(&self) {
        // Do not depend on editable_focus: canvas, selection and other controls
        // can hold unsaved state too. Never clear this on navigation or reload.
        self.shared.nav.borrow_mut().auto_archive_protected = true;
    }

    fn mouse_event(x: f32, y: f32, mods: Modifiers, buttons: u32) -> MouseEvent {
        MouseEvent { x: x.round() as i32, y: y.round() as i32, modifiers: mods.cef_flags() | buttons }
    }

    fn button_flags(held: &[MouseButton]) -> u32 {
        held.iter().fold(0, |f, b| {
            f | match b {
                MouseButton::Left => input::FLAG_LEFT_MOUSE,
                MouseButton::Middle => input::FLAG_MIDDLE_MOUSE,
                MouseButton::Right => input::FLAG_RIGHT_MOUSE,
            }
        })
    }

    pub fn mouse_move(&self, x: f32, y: f32, mods: Modifiers, held: &[MouseButton]) {
        if !held.is_empty() {
            self.protect_from_auto_archive();
        }
        if let Some(h) = self.host() {
            h.send_mouse_move_event(Some(&Self::mouse_event(x, y, mods, Self::button_flags(held))), 0);
        }
    }

    pub fn capture_lost(&self) {
        if let Some(h) = self.host() {
            h.send_capture_lost_event();
        }
    }

    pub fn mouse_leave(&self) {
        if let Some(h) = self.host() {
            h.send_mouse_move_event(Some(&MouseEvent::default()), 1);
        }
        let mut nav = self.shared.nav.borrow_mut();
        // Both were for where the pointer was.
        (nav.status, nav.tooltip) = (None, None);
    }

    pub fn mouse_button(&self, x: f32, y: f32, button: MouseButton, up: bool, click_count: i32, mods: Modifiers) {
        self.shared.gesture.set(mods);
        if !up {
            self.protect_from_auto_archive();
        }
        let Some(h) = self.host() else { return };
        let t = match button {
            MouseButton::Left => MouseButtonType::LEFT,
            MouseButton::Middle => MouseButtonType::MIDDLE,
            MouseButton::Right => MouseButtonType::RIGHT,
        };
        let held = if up { 0 } else { Self::button_flags(&[button]) };
        h.send_mouse_click_event(Some(&Self::mouse_event(x, y, mods, held)), t, up as i32, click_count.max(1));
    }

    /// `dx`/`dy` in logical pixels; positive `dy` scrolls content up (wheel away from user).
    pub fn mouse_wheel(&self, x: f32, y: f32, dx: f32, dy: f32, mods: Modifiers) {
        self.protect_from_auto_archive();
        if let Some(h) = self.host() {
            h.send_mouse_wheel_event(Some(&Self::mouse_event(x, y, mods, 0)), dx.round() as i32, dy.round() as i32);
        }
    }

    pub fn key(&self, k: &KeyInput) {
        if k.pressed {
            self.shared.gesture.set(k.modifiers);
            self.protect_from_auto_archive();
        }
        let Some(h) = self.host() else { return };
        let editable = self.shared.nav.borrow().editable_focus as i32;
        for mut ev in input::key_events(k) {
            ev.focus_on_editable_field = editable;
            h.send_key_event(Some(&ev));
        }
    }

    /// Send a key press the page gets first. If CEF reports that the page left
    /// it unhandled, [`WebView::take_unhandled_keys`] yields the returned
    /// ticket once. None when there is no browser to send it to.
    pub fn key_tracked(&self, k: &KeyInput) -> Option<u64> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        if !k.pressed {
            self.key(k);
            return None;
        }
        self.host()?;
        let press = input::key_events(k).into_iter().next()?;
        let ticket = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Track before sending, so even an immediate answer finds the press.
        self.shared.unhandled.borrow_mut().track(KeySignature::of(&press), ticket, std::time::Instant::now());
        self.key(k);
        Some(ticket)
    }

    /// Tickets of tracked presses CEF reported unhandled since the last call.
    pub fn take_unhandled_keys(&self) -> Vec<u64> {
        self.shared.unhandled.borrow_mut().take()
    }

    pub fn ime_commit(&self, text: &str) {
        self.protect_from_auto_archive();
        if let Some(h) = self.host() {
            // CEF requires InvalidRange, not a null pointer, for no replacement.
            let replacement = Range { from: u32::MAX, to: u32::MAX };
            h.ime_commit_text(Some(&CefString::from(text)), Some(&replacement), 0);
        }
    }

    pub fn ime_set_composition(&self, text: &str) {
        self.protect_from_auto_archive();
        if let Some(h) = self.host() {
            let len = text.encode_utf16().count() as u32;
            let sel = Range { from: len, to: len };
            let replacement = Range { from: u32::MAX, to: u32::MAX };
            h.ime_set_composition(Some(&CefString::from(text)), None, Some(&replacement), Some(&sel));
        }
    }

    pub fn ime_cancel(&self) {
        if let Some(h) = self.host() {
            h.ime_cancel_composition();
        }
    }

    pub fn copy(&self) {
        if let Some(f) = self.frame() {
            f.copy();
        }
    }
    pub fn cut(&self) {
        self.protect_from_auto_archive();
        if let Some(f) = self.frame() {
            f.cut();
        }
    }
    pub fn paste(&self) {
        self.protect_from_auto_archive();
        if let Some(f) = self.frame() {
            f.paste();
        }
    }
    pub fn select_all(&self) {
        if let Some(f) = self.frame() {
            f.select_all();
        }
    }
    pub fn undo(&self) {
        self.protect_from_auto_archive();
        if let Some(f) = self.frame() {
            f.undo();
        }
    }
    pub fn redo(&self) {
        self.protect_from_auto_archive();
        if let Some(f) = self.frame() {
            f.redo();
        }
    }

    // -- page tools ----------------------------------------------------------

    pub fn find(&self, text: &str, forward: bool, find_next: bool) {
        if let Some(h) = self.host() {
            if text.is_empty() {
                h.stop_finding(1);
                self.shared.nav.borrow_mut().find = None;
            } else {
                h.find(Some(&CefString::from(text)), forward as i32, 0, find_next as i32);
            }
        }
    }

    pub fn stop_find(&self) {
        if let Some(h) = self.host() {
            h.stop_finding(1);
        }
        self.shared.nav.borrow_mut().find = None;
    }

    pub fn dismiss_find(&self) {
        if let Some(h) = self.host() {
            // Dismiss the session without clearing the page selection again.
            h.stop_finding(0);
            h.set_focus(1);
        }
        self.shared.nav.borrow_mut().find = None;
    }

    pub fn zoom_level(&self) -> f64 {
        self.host().map(|h| h.zoom_level()).unwrap_or(0.0)
    }

    /// Chromium zoom level: 0 = 100 %, each step is ×1.2.
    pub fn set_zoom_level(&self, level: f64) {
        if let Some(h) = self.host() {
            h.set_zoom_level(level.clamp(-7.6, 8.8));
        }
    }

    pub fn show_devtools(&self) {
        if let Some(h) = self.host() {
            let info = WindowInfo::default();
            let settings = BrowserSettings::default();
            h.show_dev_tools(Some(&info), None, Some(&settings), None);
        }
    }

    pub fn view_source(&self) {
        if let Some(f) = self.browser().and_then(|b| b.main_frame()) {
            f.view_source();
        }
    }

    pub fn print(&self) {
        if let Some(h) = self.host() {
            h.print();
        }
    }

    pub fn execute_js(&self, code: &str) {
        if let Some(f) = self.browser().and_then(|b| b.main_frame()) {
            f.execute_java_script(Some(&CefString::from(code)), Some(&CefString::from("crossbow://boost")), 0);
        }
    }

    /// Asks the renderer for the main frame's text: CEF dumps the document
    /// element's innerText, which leaves out script, style, hidden elements,
    /// form-control values and other frames. The renderer's dump has no size
    /// limit; only `cap` bytes are kept. None without a live page.
    pub fn read_text(&self, cap: usize) -> Option<PageText> {
        let frame = self.browser()?.main_frame()?;
        let slot = PageText::new(cap);
        frame.text(Some(&mut PageTextBuilder::new(slot.clone())));
        Some(slot)
    }

    /// The origin the key button fills for: the main frame's, unless its
    /// document's own origin is opaque or isn't its address's.
    pub fn login_origin(&self) -> Option<String> {
        let frame = self.browser()?.main_frame()?;
        self.shared.login.borrow().origin(&frame)
    }

    /// The origin of the login field that last had focus on this page, if its
    /// frame may be filled.
    pub fn login_focus(&self) -> Option<String> {
        self.shared.login.borrow().focus().map(|(_, origin)| origin.to_owned())
    }

    /// Fill a saved login into the frame whose login field last had focus,
    /// while that frame has focus, else the main frame, whichever first may
    /// hold logins of `origin`. The renderer checks the origin again and fills
    /// the focused field's login form, and nothing when that field has none;
    /// with no field focused, in the main frame only, the one form whose
    /// password field the user can see. One that fills nothing sends
    /// [`WebEvent::LoginNotFilled`]. False when no frame of this page has `origin`.
    pub fn fill_login(&self, origin: &str, username: &str, password: &str) -> bool {
        let Some(browser) = self.browser() else { return false };
        let mut state = self.shared.login.borrow_mut();
        let focused = state.focus().and_then(|(frame, _)| browser.frame_by_identifier(Some(&CefString::from(frame))));
        let Some(frame) = login::fill_target(&state, [focused, browser.focused_frame(), browser.main_frame()], origin) else { return false };
        let Some((_, document)) = state.current(&frame) else { return false };
        let Some(mut message) = login::outgoing(login::FILL, &[origin, &document, username, password]) else { return false };
        state.fill_sent(&frame, std::time::Instant::now());
        drop(state);
        frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
        true
    }

    /// The origin of the address field that last had focus on this page, if
    /// its frame may be filled.
    pub fn address_focus(&self) -> Option<String> {
        self.shared.login.borrow().address_focus().map(|(_, origin)| origin.to_owned())
    }

    /// Where an address fill would go now, frozen for the address menu that
    /// opens now: the frame with focus, while its document noted an address
    /// field, else the main frame, whichever first may hold logins of the
    /// latest noted field's origin, with its document and the address note
    /// that names its focused field's form or group. See
    /// [`login::FillTarget`]. None when no frame of this page qualifies.
    pub fn address_target(&self) -> Option<login::FillTarget> {
        let browser = self.browser()?;
        let state = self.shared.login.borrow();
        let focused = state.address_focus().and_then(|(frame, _)| browser.frame_by_identifier(Some(&CefString::from(frame))));
        login::address_menu_target(&state, browser.identifier(), [focused, browser.focused_frame(), browser.main_frame()])
    }

    /// Whether an address fill frozen as `target` would still go ahead now.
    pub fn address_target_holds(&self, target: &login::FillTarget) -> bool {
        self.address_target().as_ref() == Some(target)
    }

    /// Fill a saved address, its parts in [`login::ADDRESS_PARTS`] order,
    /// where the address menu froze it, `target`, only while nothing it was
    /// frozen on changed: the same browser, frame, document and origin, the
    /// same address note from that frame and the page, and the same frame
    /// with focus. The renderer fills only while that address note is still
    /// its document's latest, and the fields the user can see in the focused
    /// field's form, or outside any form in its nearest container that
    /// holds another address field, and nothing elsewhere; with no field
    /// focused, nothing, in any frame. It then waits up to a second for the
    /// engine's answer on which of those fields the user can see, fills only
    /// those, and reports any it left empty, or a focused field the engine
    /// doesn't call clearly visible, as [`WebEvent::AddressFillReport`]. One
    /// that fills nothing else sends [`WebEvent::AddressNotFilled`]. A frame
    /// of another origin, or below one, never gets an address: if it has the
    /// script, it has it for cards only. False, with nothing sent, when the
    /// target no longer holds.
    pub fn fill_address_at(&self, target: &login::FillTarget, parts: &[&str; login::ADDRESS_PARTS]) -> bool {
        let Some(browser) = self.browser() else { return false };
        let mut state = self.shared.login.borrow_mut();
        let focused = state.address_focus().and_then(|(frame, _)| browser.frame_by_identifier(Some(&CefString::from(frame))));
        let Some(frame) = login::address_frozen_frame(&state, browser.identifier(), [focused, browser.focused_frame(), browser.main_frame()], target) else { return false };
        let values = login::address_values(parts);
        let note = target.note().to_string();
        let mut args = vec![target.origin(), target.document(), note.as_str()];
        args.extend(values.iter().map(login::Secret::expose));
        let Some(mut message) = login::outgoing(login::FILL_ADDRESS, &args) else { return false };
        state.address_fill_sent(&frame, std::time::Instant::now());
        drop(state);
        frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
        true
    }

    /// The origin of the card field that last had focus on this page, which
    /// may be in another site's frame, if that frame may take a card. Until
    /// one had focus, the key button offers no card.
    pub fn card_focus(&self) -> Option<String> {
        self.shared.login.borrow().card_focus().map(|(_, origin)| origin.to_owned())
    }

    /// The origin a card fill would go to now: the frame with focus, while
    /// its document noted a card field, else the main frame, if it may take
    /// a card. None on an http page, file:, data: or an internal page, where
    /// the key button and the context menu offer no card.
    pub fn card_origin(&self) -> Option<String> {
        let browser = self.browser()?;
        let state = self.shared.login.borrow();
        let focused = state.card_focus().and_then(|(frame, _)| browser.frame_by_identifier(Some(&CefString::from(frame))));
        login::card_fill_origin(&state, [focused, browser.focused_frame(), browser.main_frame()])
    }

    /// Where a card fill would go now, frozen for the card menu that opens
    /// now: the frame with focus, while its document noted a card field,
    /// else the main frame, the first that may take a card, with its
    /// document, its origin as [`WebView::card_origin`] names it, and the
    /// card note that names its focused field's form or group. See
    /// [`login::FillTarget`]. None when no frame of this page qualifies.
    pub fn card_target(&self) -> Option<login::FillTarget> {
        let browser = self.browser()?;
        let state = self.shared.login.borrow();
        let focused = state.card_focus().and_then(|(frame, _)| browser.frame_by_identifier(Some(&CefString::from(frame))));
        login::card_menu_target(&state, browser.identifier(), [focused, browser.focused_frame(), browser.main_frame()])
    }

    /// Whether a card fill frozen as `target` would still go ahead now.
    pub fn card_target_holds(&self, target: &login::FillTarget) -> bool {
        self.card_target().as_ref() == Some(target)
    }

    /// Fill a saved card, its parts in [`login::CARD_PARTS`] order, where
    /// the card menu froze it, `target`, only while nothing it was frozen on
    /// changed: the same browser, frame, document and origin, the same card
    /// note from that frame and the page, and the same frame with focus.
    /// Unlike a login or an address, that frame may be another site's, as a
    /// payment processor's card fields are; it gets only the card fields it
    /// holds. The renderer fills only while that card note is still its
    /// document's latest, and only from a focused card field: the fields
    /// the user can see in its form or, outside any form, in its nearest
    /// container holding another card field, else that field alone, and
    /// nothing when no field has focus; never a security code, and it
    /// never submits. As for an address, it waits for the engine's answer
    /// and fills only the fields it calls clearly visible, reporting the
    /// rest as [`WebEvent::CardFillReport`]; in another site's frame that
    /// answer covers the frame's own place in the page. One that fills
    /// nothing else sends [`WebEvent::CardNotFilled`]. The number crosses
    /// once, in this message. False, with nothing sent, when the target no
    /// longer holds.
    pub fn fill_card_at(&self, target: &login::FillTarget, parts: &[&str; login::CARD_PARTS]) -> bool {
        let Some(browser) = self.browser() else { return false };
        let mut state = self.shared.login.borrow_mut();
        let focused = state.card_focus().and_then(|(frame, _)| browser.frame_by_identifier(Some(&CefString::from(frame))));
        let Some(frame) = login::card_frozen_frame(&state, browser.identifier(), [focused, browser.focused_frame(), browser.main_frame()], target) else { return false };
        let values = login::card_values(parts);
        let note = target.note().to_string();
        let mut args = vec![target.origin(), target.document(), note.as_str()];
        args.extend(values.iter().map(login::Secret::expose));
        let Some(mut message) = login::outgoing(login::FILL_CARD, &args) else { return false };
        state.card_fill_sent(&frame, std::time::Instant::now());
        drop(state);
        frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
        true
    }

    /// Tell the renderers of this page's frames the profile's capture setting
    /// after it changed. Turning it on applies to pages loaded afterwards;
    /// turning it off drops a login waiting for its follow, stops the open
    /// page's listener from sending, and leaves it out of the next load.
    pub fn refresh_login_capture(&self) {
        if !login::login_capture(&self.shared.profile_id) { self.shared.login.borrow_mut().forget_pending(); }
        let Some(browser) = self.browser() else { return };
        let mut ids = CefStringList::new();
        browser.frame_identifiers(Some(&mut ids));
        for id in ids {
            let (Some(frame), Some(mut message)) = (browser.frame_by_identifier(Some(&CefString::from(id.as_str()))), login::setting_message(&self.shared.profile_id)) else { continue };
            frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
        }
    }

    /// The address of every frame's document in the page, as
    /// `document_address` reads it. Empty without a page.
    pub fn documents(&self) -> Vec<String> {
        let Some(browser) = self.browser() else { return Vec::new() };
        let mut ids = CefStringList::new();
        browser.frame_identifiers(Some(&mut ids));
        ids.into_iter().filter_map(|id| browser.frame_by_identifier(Some(&CefString::from(id.as_str())))).map(|frame| {
            let chain = std::iter::successors(Some(frame), |frame| frame.parent()).take(16);
            document_address(chain.map(|frame| CefString::from(&frame.url()).to_string()))
        }).collect()
    }

    /// The top frame's latest video report, and when it came.
    pub fn media(&self) -> Option<(MediaReport, std::time::Instant)> {
        self.shared.media.borrow().clone()
    }

    /// Send one command to the top frame's videos. False without a page.
    pub fn media_command(&self, command: &MediaCommand) -> bool {
        let Some(frame) = self.browser().and_then(|browser| browser.main_frame()) else { return false };
        let Some(mut message) = media::outgoing(media::COMMAND, &command.encode()) else { return false };
        frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
        true
    }

    /// Send one command to the top frame's `translate.js`. False without a page.
    pub fn translate(&self, command: &TranslateCommand) -> bool {
        let Some(frame) = self.browser().and_then(|browser| browser.main_frame()) else { return false };
        let Some(mut message) = media::outgoing(translate::COMMAND, &command.encode()) else { return false };
        frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
        true
    }

    /// The top frame's Translate Page reports since the last call, oldest first.
    pub fn take_translate_reports(&self) -> Vec<TranslateReport> {
        std::mem::take(&mut *self.shared.translate.borrow_mut())
    }

    /// The `Content-Language` the current page's response named, if any.
    pub fn content_language(&self) -> Option<String> {
        let url = self.shared.nav.borrow().url.clone();
        self.shared.languages.lock().unwrap_or_else(std::sync::PoisonError::into_inner).of(&url)
    }

    /// Paint at most `fps` frames a second, from 1 to 60.
    pub fn set_frame_rate(&self, fps: i32) {
        if let Some(h) = self.host() {
            h.set_windowless_frame_rate(fps.clamp(1, 60));
        }
    }

    pub fn set_muted(&self, muted: bool) {
        if let Some(h) = self.host() {
            h.set_audio_muted(muted as i32);
        }
    }

    pub fn is_muted(&self) -> bool {
        self.host().is_some_and(|h| h.is_audio_muted() != 0)
    }

    pub fn exit_fullscreen(&self) {
        if let Some(h) = self.host() {
            h.exit_fullscreen(1);
        }
    }

    pub fn start_download(&self, url: &str) {
        if let Some(h) = self.host() {
            h.start_download(Some(&CefString::from(url)));
        }
    }

    /// Close the browser. The view stops painting; `WebEvent::Closed` follows.
    pub fn close(&self) {
        if let Some(h) = self.host() {
            h.close_browser(1);
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        for (_, control) in self.download_controls.get_mut().drain() { control.invalidate(); }
        if let Some(b) = self.browser.get_mut().take() {
            if let Some(h) = b.host() {
                h.close_browser(1);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// CEF handlers
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct H(Rc<Shared>);

wrap_client! {
    struct ClientBuilder {
        h: H,
        render: RenderHandler,
        display: DisplayHandler,
        load: LoadHandler,
        request: RequestHandler,
        life_span: LifeSpanHandler,
        download: DownloadHandler,
        find: FindHandler,
        menu: ContextMenuHandler,
        keyboard: KeyboardHandler,
        permission: PermissionHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> { Some(self.render.clone()) }
        fn display_handler(&self) -> Option<DisplayHandler> { Some(self.display.clone()) }
        fn load_handler(&self) -> Option<LoadHandler> { Some(self.load.clone()) }
        fn request_handler(&self) -> Option<RequestHandler> { Some(self.request.clone()) }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> { Some(self.life_span.clone()) }
        fn download_handler(&self) -> Option<DownloadHandler> { Some(self.download.clone()) }
        fn find_handler(&self) -> Option<FindHandler> { Some(self.find.clone()) }
        fn context_menu_handler(&self) -> Option<ContextMenuHandler> { Some(self.menu.clone()) }
        fn keyboard_handler(&self) -> Option<KeyboardHandler> { Some(self.keyboard.clone()) }
        fn permission_handler(&self) -> Option<PermissionHandler> { Some(self.permission.clone()) }

        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            let (Some(frame), Some(message)) = (frame, message) else { return 0 };
            (source_process == ProcessId::RENDERER && (self.h.0.login_message(frame, message) || self.h.0.media_message(frame, message)
                || self.h.0.hiding_message(browser.as_deref(), frame, message) || self.h.0.translate_message(frame, message))) as ::std::os::raw::c_int
        }
    }
}

impl ClientBuilder {
    fn build(shared: Rc<Shared>) -> Client {
        let documents = shared.login.borrow().documents();
        let languages = shared.languages.clone();
        let blocked = shared.blocked.clone();
        let profile: Arc<str> = shared.profile_id.as_str().into();
        let h = H(shared);
        Self::new(
            h.clone(),
            RenderBuilder::new(h.clone()),
            DisplayBuilder::new(h.clone()),
            LoadBuilder::new(h.clone()),
            RequestBuilder::new(h.clone(), documents, languages, profile, blocked),
            LifeSpanBuilder::new(h.clone()),
            DownloadBuilder::new(h.clone()),
            FindBuilder::new(h.clone()),
            MenuBuilder::new(h.clone()),
            KeyboardBuilder::new(h.clone()),
            PermissionBuilder::new(h),
        )
    }
}

wrap_render_handler! {
    struct RenderBuilder {
        h: H,
    }

    impl RenderHandler {
        fn root_screen_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) -> ::std::os::raw::c_int {
            let (Some(root), Some(rect)) = (self.h.0.root.get(), rect) else { return 0 };
            *rect = root.cef_rect();
            1
        }

        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                let (w, h) = self.h.0.size.get();
                // CEF's macOS OSR sample keeps this origin at zero for layout;
                // root_screen_rect and screen_point carry the native position.
                let (x, y) = if cfg!(target_os = "macos") { (0, 0) } else {
                    self.h.0.position.get().and_then(ViewPosition::cef_origin).unwrap_or((0, 0))
                };
                rect.x = x;
                rect.y = y;
                rect.width = (w as i32).max(1);
                rect.height = (h as i32).max(1);
            }
        }

        fn screen_point(&self, _browser: Option<&mut Browser>, view_x: ::std::os::raw::c_int, view_y: ::std::os::raw::c_int, screen_x: Option<&mut ::std::os::raw::c_int>, screen_y: Option<&mut ::std::os::raw::c_int>) -> ::std::os::raw::c_int {
            let (Some(screen_x), Some(screen_y)) = (screen_x, screen_y) else { return 0 };
            let Some((x, y)) = self.h.0.position.get().and_then(|position| position.screen_point(view_x, view_y)) else { return 0 };
            *screen_x = x;
            *screen_y = y;
            1
        }

        fn screen_info(&self, _browser: Option<&mut Browser>, info: Option<&mut ScreenInfo>) -> ::std::os::raw::c_int {
            let Some(info) = info else { return 0 };
            info.device_scale_factor = self.h.0.scale.get();
            if let Some(screen) = self.h.0.screen.get() {
                info.rect = screen.bounds.cef_rect();
                info.available_rect = screen.work_area.cef_rect();
            } else {
                info.rect = Rect::default();
                info.available_rect = Rect::default();
            }
            // Preserve the known backing scale even without native monitor data.
            // CEF documents GetViewRect as its fallback for empty screen bounds.
            1
        }

        fn on_popup_show(&self, _browser: Option<&mut Browser>, show: ::std::os::raw::c_int) {
            if show == 0 {
                self.h.0.popup_rect.set(None);
                self.h.0.popup_texture.replace(None);
            }
        }

        fn on_popup_size(&self, _browser: Option<&mut Browser>, rect: Option<&Rect>) {
            if let Some(r) = rect {
                self.h.0.popup_rect.set(Some((r.x, r.y, r.width, r.height)));
            }
        }

        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            info: Option<&AcceleratedPaintInfo>,
        ) {
            use cef::osr_texture_import::shared_texture_handle::SharedTextureHandle;
            let Some(info) = info else { return };
            let handle = SharedTextureHandle::new(info);
            if let SharedTextureHandle::Unsupported = handle {
                return;
            }
            let s = &self.h.0;
            // CEF keeps the shared texture valid only during this callback; the import
            // copies it into a texture we own before returning.
            match handle.import_texture(&s.device) {
                Ok(src) => {
                    let slot = if type_ == PaintElementType::POPUP { &s.popup_texture } else { &s.texture };
                    texture::copy_from_texture(&mut slot.borrow_mut(), &s.device, &s.queue, &src);
                }
                Err(e) => log::warn!("shared texture import failed: {e:?}"),
            }
            crate::wake(std::time::Duration::ZERO);
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            if buffer.is_null() || width <= 0 || height <= 0 {
                return;
            }
            let s = &self.h.0;
            // SAFETY: CEF guarantees `buffer` holds width*height BGRA pixels during the call.
            let bytes = unsafe { std::slice::from_raw_parts(buffer, (width * height * 4) as usize) };
            let slot = if type_ == PaintElementType::POPUP { &s.popup_texture } else { &s.texture };
            texture::upload_bgra(&mut slot.borrow_mut(), &s.device, &s.queue, bytes, width as u32, height as u32);
            crate::wake(std::time::Duration::ZERO);
        }

        fn on_virtual_keyboard_requested(&self, _browser: Option<&mut Browser>, input_mode: TextInputMode) {
            self.h.0.nav.borrow_mut().editable_focus = input_mode != TextInputMode::NONE;
        }
    }
}

wrap_display_handler! {
    struct DisplayBuilder {
        h: H,
    }

    impl DisplayHandler {
        fn on_address_change(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>) {
            if let Some(frame) = frame.filter(|f| f.is_main() == 1) {
                self.h.0.navigations.set(self.h.0.navigations.get() + 1);
                self.h.0.nav.borrow_mut().url = opt(url);
                self.h.0.login_navigated(frame);
            }
        }

        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            self.h.0.nav.borrow_mut().title = opt(title);
        }

        fn on_favicon_urlchange(&self, browser: Option<&mut Browser>, icon_urls: Option<&mut CefStringList>) {
            let Some(list) = icon_urls else { return };
            let urls: Vec<String> = std::mem::take(list).into_iter().map(|s| s.to_string()).collect();
            let Some(url) = urls.into_iter().next() else { return };
            let page_url = self.h.0.nav.borrow().url.clone();
            self.h.0.nav.borrow_mut().favicon_url = Some(url.clone());
            if let Some(host) = browser.and_then(|b| b.host()) {
                let mut cb = FaviconBuilder::new(FaviconJob { h: self.h.clone(), page_url });
                host.download_image(Some(&CefString::from(url.as_str())), 1, 64, 0, Some(&mut cb));
            }
        }

        fn on_fullscreen_mode_change(&self, _browser: Option<&mut Browser>, fullscreen: ::std::os::raw::c_int) {
            self.h.0.nav.borrow_mut().fullscreen = fullscreen != 0;
        }

        fn on_tooltip(&self, _browser: Option<&mut Browser>, text: Option<&mut CefString>) -> ::std::os::raw::c_int {
            let t = text.map(|t| t.to_string()).filter(|t| !t.is_empty());
            self.h.0.nav.borrow_mut().tooltip = t;
            1
        }

        fn on_status_message(&self, _browser: Option<&mut Browser>, value: Option<&CefString>) {
            let v = opt(value);
            self.h.0.nav.borrow_mut().status = if v.is_empty() { None } else { Some(v) };
        }

        fn on_loading_progress_change(&self, _browser: Option<&mut Browser>, progress: f64) {
            self.h.0.nav.borrow_mut().progress = progress;
        }

        fn on_cursor_change(
            &self,
            _browser: Option<&mut Browser>,
            _cursor: *mut u8,
            type_: CursorType,
            _custom_cursor_info: Option<&CursorInfo>,
        ) -> ::std::os::raw::c_int {
            self.h.0.nav.borrow_mut().cursor = cursor_kind(type_);
            1
        }
    }
}

fn cursor_kind(t: CursorType) -> CursorKind {
    match t {
        CursorType::HAND => CursorKind::Pointer,
        CursorType::IBEAM | CursorType::VERTICALTEXT => CursorKind::Text,
        CursorType::CROSS | CursorType::CELL => CursorKind::Crosshair,
        CursorType::WAIT => CursorKind::Wait,
        CursorType::PROGRESS => CursorKind::Progress,
        CursorType::HELP => CursorKind::Help,
        CursorType::MOVE => CursorKind::Move,
        CursorType::NOTALLOWED | CursorType::NODROP => CursorKind::NotAllowed,
        CursorType::GRAB => CursorKind::Grab,
        CursorType::GRABBING => CursorKind::Grabbing,
        CursorType::EASTRESIZE | CursorType::WESTRESIZE | CursorType::EASTWESTRESIZE | CursorType::COLUMNRESIZE => {
            CursorKind::ResizeEw
        }
        CursorType::NORTHRESIZE | CursorType::SOUTHRESIZE | CursorType::NORTHSOUTHRESIZE | CursorType::ROWRESIZE => {
            CursorKind::ResizeNs
        }
        CursorType::NORTHEASTRESIZE | CursorType::SOUTHWESTRESIZE | CursorType::NORTHEASTSOUTHWESTRESIZE => {
            CursorKind::ResizeNesw
        }
        CursorType::NORTHWESTRESIZE | CursorType::SOUTHEASTRESIZE | CursorType::NORTHWESTSOUTHEASTRESIZE => {
            CursorKind::ResizeNwse
        }
        CursorType::ZOOMIN => CursorKind::ZoomIn,
        CursorType::ZOOMOUT => CursorKind::ZoomOut,
        CursorType::NONE => CursorKind::None,
        _ => CursorKind::Default,
    }
}

#[derive(Clone)]
struct FaviconJob {
    h: H,
    page_url: String,
}

wrap_download_image_callback! {
    struct FaviconBuilder {
        job: FaviconJob,
    }

    impl DownloadImageCallback {
        fn on_download_image_finished(
            &self,
            _image_url: Option<&CefString>,
            _http_status_code: ::std::os::raw::c_int,
            image: Option<&mut Image>,
        ) {
            let Some(image) = image else { return };
            if image.is_empty() != 0 {
                return;
            }
            let (mut w, mut h) = (0, 0);
            let Some(bin) = image.as_bitmap(2.0, ColorType::RGBA_8888, AlphaType::POSTMULTIPLIED, Some(&mut w), Some(&mut h))
                .or_else(|| image.as_bitmap(1.0, ColorType::RGBA_8888, AlphaType::POSTMULTIPLIED, Some(&mut w), Some(&mut h)))
            else {
                return;
            };
            let mut rgba = vec![0u8; bin.size()];
            bin.data(Some(&mut rgba), 0);
            if w > 0 && h > 0 && rgba.len() >= (w * h * 4) as usize {
                self.job.h.0.push(WebEvent::Favicon {
                    page_url: self.job.page_url.clone(),
                    width: w as u32,
                    height: h as u32,
                    rgba,
                });
            }
        }
    }
}

wrap_load_handler! {
    struct LoadBuilder {
        h: H,
    }

    impl LoadHandler {
        fn on_load_start(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, _transition_type: TransitionType) {
            let Some(frame) = frame else { return };
            if frame.is_main() == 1 {
                self.h.0.navigations.set(self.h.0.navigations.get() + 1);
                // A new page: what the old one asked is denied, and its blocked
                // requests count from 0.
                self.h.0.loads.set(self.h.0.loads.get() + 1);
                self.h.0.blocked.start(self.h.0.loads.get());
                self.h.0.deny_permissions();
                // A new document: its script reports its own videos.
                self.h.0.media.borrow_mut().take();
                self.h.0.translate.borrow_mut().clear();
            }
            self.h.0.login_navigated(frame);
            // Before the setting, which the renderer answers by reporting its listener again.
            self.h.0.login_document_started(frame);
            self.h.0.tell_login_setting(frame);
        }

        fn on_loading_state_change(
            &self,
            _browser: Option<&mut Browser>,
            is_loading: ::std::os::raw::c_int,
            can_go_back: ::std::os::raw::c_int,
            can_go_forward: ::std::os::raw::c_int,
        ) {
            let mut nav = self.h.0.nav.borrow_mut();
            nav.loading = is_loading != 0;
            nav.can_go_back = can_go_back != 0;
            nav.can_go_forward = can_go_forward != 0;
            if is_loading != 0 {
                nav.crashed = false;
            }
        }

        fn on_load_end(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, _http_status_code: ::std::os::raw::c_int) {
            if frame.is_some_and(|f| f.is_main() == 1) {
                let nav = self.h.0.nav.borrow();
                let (url, title) = (nav.url.clone(), nav.title.clone());
                drop(nav);
                self.h.0.push(WebEvent::Visited { url, title });
            }
        }

        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            let code = error_code.get_raw();
            // ERR_ABORTED (-3) happens on every user-cancelled or replaced navigation.
            if code == -3 || !frame.is_some_and(|f| f.is_main() == 1) {
                return;
            }
            self.h.0.push(WebEvent::LoadError { url: opt(failed_url), code, text: opt(error_text) });
        }
    }
}

wrap_request_handler! {
    struct RequestBuilder {
        h: H,
        // For the IO thread, which must never touch `h`.
        documents: Arc<Mutex<login::Documents>>,
        languages: Arc<Mutex<translate::Languages>>,
        profile: Arc<str>,
        blocked: Arc<crate::content_rules::BlockedCount>,
    }

    impl RequestHandler {
        /// IO thread: navigations get a handler that reads their response's
        /// sandbox policy, and every request gets one while the profile has
        /// content rules. Touches only `documents`, `languages` and the rules.
        fn resource_request_handler(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _request: Option<&mut Request>,
            is_navigation: ::std::os::raw::c_int,
            _is_download: ::std::os::raw::c_int,
            _request_initiator: Option<&CefString>,
            _disable_default_handling: Option<&mut ::std::os::raw::c_int>,
        ) -> Option<ResourceRequestHandler> {
            let rules = crate::content_rules::rules_for(&self.profile);
            (is_navigation == 1 || rules.is_some()).then(|| ResourceBuilder::new(self.documents.clone(), self.languages.clone(), rules, self.blocked.clone()))
        }

        fn on_render_process_terminated(
            &self,
            _browser: Option<&mut Browser>,
            _status: TerminationStatus,
            _error_code: ::std::os::raw::c_int,
            _error_string: Option<&CefString>,
        ) {
            self.h.0.nav.borrow_mut().crashed = true;
        }

        fn on_before_browse(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            user_gesture: ::std::os::raw::c_int,
            _is_redirect: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            let Some(request) = request else { return 0 };
            let url = s(&request.url());
            if permissions::is_external(&url) {
                // Cancelled here, so CEF never offers it to the system
                // (OnProtocolExecution); the app asks first.
                let frame = frame.map(|frame| s(&frame.url())).unwrap_or_default();
                self.h.0.external_link(url, user_gesture != 0, frame);
                return 1;
            }
            0
        }
    }
}

wrap_resource_request_handler! {
    pub(crate) struct ResourceBuilder {
        documents: Arc<Mutex<login::Documents>>,
        languages: Arc<Mutex<translate::Languages>>,
        rules: Option<Arc<dyn crate::ContentRules>>,
        blocked: Arc<crate::content_rules::BlockedCount>,
    }

    impl ResourceRequestHandler {
        /// IO thread: the profile's content rules may cancel the request, which
        /// then counts for the view's current page.
        fn on_before_resource_load(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _callback: Option<&mut cef::Callback>,
        ) -> ReturnValue {
            let (Some(rules), Some(request)) = (&self.rules, request) else { return ReturnValue::CONTINUE };
            if !crate::content_rules::cancels(rules.as_ref(), browser.as_deref(), frame.as_deref(), request) { return ReturnValue::CONTINUE; }
            self.blocked.add(&s(&request.url()));
            ReturnValue::CANCEL
        }

        /// IO thread: a document whose `Content-Security-Policy` sandboxes it
        /// without allow-same-origin has an opaque origin, whatever its address.
        /// A top-frame document's `Content-Language` is kept for Translate Page.
        fn on_resource_response(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            response: Option<&mut Response>,
        ) -> ::std::os::raw::c_int {
            let (Some(request), Some(response)) = (request, response) else { return 0 };
            if request.resource_type() != ResourceType::MAIN_FRAME && request.resource_type() != ResourceType::SUB_FRAME { return 0; }
            let mut headers = CefStringMultimap::new();
            response.header_map(Some(&mut headers));
            let (mut policies, mut languages): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
            for (name, values) in headers {
                if name.eq_ignore_ascii_case("content-security-policy") { policies.extend(values); } else if name.eq_ignore_ascii_case("content-language") { languages.extend(values); }
            }
            let url = Some(s(&response.url())).filter(|url| !url.is_empty()).unwrap_or_else(|| s(&request.url()));
            self.documents.lock().unwrap_or_else(std::sync::PoisonError::into_inner).response(&url, &policies);
            if request.resource_type() == ResourceType::MAIN_FRAME {
                self.languages.lock().unwrap_or_else(std::sync::PoisonError::into_inner).response(&url, &languages);
            }
            0
        }
    }
}

wrap_life_span_handler! {
    struct LifeSpanBuilder {
        h: H,
    }

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            target_disposition: WindowOpenDisposition,
            user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            let url = opt(target_url);
            if permissions::is_external(&url) {
                let frame = frame.map(|frame| s(&frame.url())).unwrap_or_default();
                self.h.0.external_link(url, user_gesture != 0, frame);
            } else if !url.is_empty() {
                self.h.0.push(WebEvent::OpenUrl {
                    url,
                    background: target_disposition == WindowOpenDisposition::NEW_BACKGROUND_TAB,
                    popup: target_disposition == WindowOpenDisposition::NEW_POPUP,
                    modifiers: if user_gesture != 0 { self.h.0.gesture.get() } else { Modifiers::default() },
                });
                crate::wake(std::time::Duration::ZERO);
            }
            1 // never let CEF create its own window
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            for (_, control) in self.h.0.download_controls.borrow_mut().drain() { control.invalidate(); }
            self.h.0.deny_permissions();
            self.h.0.browser.replace(None);
            self.h.0.push(WebEvent::Closed);
        }
    }
}

wrap_download_handler! {
    struct DownloadBuilder {
        h: H,
    }

    impl DownloadHandler {
        fn can_download(&self, _browser: Option<&mut Browser>, _url: Option<&CefString>, _request_method: Option<&CefString>) -> ::std::os::raw::c_int {
            1
        }

        fn on_before_download(
            &self,
            _browser: Option<&mut Browser>,
            item: Option<&mut DownloadItem>,
            suggested_name: Option<&CefString>,
            callback: Option<&mut BeforeDownloadCallback>,
        ) -> ::std::os::raw::c_int {
            // Always handle this request; returning 0 permits Chrome-style
            // default handling, which could bypass profile preferences.
            if let (Some(item), Some(callback)) = (item, callback) {
                if item.is_valid() == 0 { return 1; }
                let mut info = download_info(item, &self.h.0.profile_id);
                let suggested = opt(suggested_name);
                if !suggested.is_empty() { info.file_name = suggested; }
                self.h.0.push(WebEvent::DownloadRequested(DownloadRequest::new(info, callback.clone())));
                crate::wake(std::time::Duration::ZERO);
            }
            1
        }

        fn on_download_updated(&self, _browser: Option<&mut Browser>, item: Option<&mut DownloadItem>, callback: Option<&mut DownloadItemCallback>) {
            let Some(item) = item else { return };
            if item.is_valid() == 0 { return; }
            let mut info = download_info(item, &self.h.0.profile_id);
            let control = self.h.0.download_controls.borrow_mut().entry(info.id).or_insert_with(|| DownloadControl::new(&info)).clone();
            control.update(&info, callback.cloned());
            if info.complete || info.canceled { self.h.0.download_controls.borrow_mut().remove(&info.id); }
            info.control = Some(control);
            crate::downloads::observe(&info);
            self.h.0.push(WebEvent::Download(info));
            crate::wake(std::time::Duration::ZERO);
        }
    }
}

fn download_info(item: &DownloadItem, profile: &str) -> DownloadInfo {
    let path = s(&item.full_path());
    let file_name = Path::new(&path).file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_else(|| s(&item.suggested_file_name()));
    let interrupted = item.is_interrupted() != 0;
    DownloadInfo {
        id: item.id(), profile_id: profile.into(), url: s(&item.url()), path, file_name,
        received: item.received_bytes(), total: item.total_bytes(), percent: item.percent_complete(),
        in_progress: item.is_in_progress() != 0, paused: item.is_paused() != 0, control: None,
        complete: item.is_complete() != 0,
        canceled: item.is_canceled() != 0, interrupted,
        error: interrupted.then(|| format!("Download interrupted ({:?}). Try Resume, or retry from the original page.", item.interrupt_reason())),
        destination_error: None,
        mime_type: s(&item.mime_type()),
    }
}

wrap_permission_handler! {
    struct PermissionBuilder {
        h: H,
    }

    impl PermissionHandler {
        fn on_request_media_access_permission(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            requesting_origin: Option<&CefString>,
            requested_permissions: u32,
            callback: Option<&mut MediaAccessCallback>,
        ) -> ::std::os::raw::c_int {
            let Some(callback) = callback else { return 0 };
            let callback = Callback::Media { callback: callback.clone(), required: requested_permissions };
            self.h.0.ask_permission(None, true, requested_permissions, opt(requesting_origin), callback);
            1
        }

        fn on_show_permission_prompt(
            &self,
            _browser: Option<&mut Browser>,
            prompt_id: u64,
            requesting_origin: Option<&CefString>,
            requested_permissions: u32,
            callback: Option<&mut PermissionPromptCallback>,
        ) -> ::std::os::raw::c_int {
            let Some(callback) = callback else { return 0 };
            let callback = Callback::Prompt { callback: callback.clone() };
            self.h.0.ask_permission(Some(prompt_id), false, requested_permissions, opt(requesting_origin), callback);
            1
        }

        fn on_dismiss_permission_prompt(&self, _browser: Option<&mut Browser>, prompt_id: u64, _result: PermissionRequestResult) {
            let dismissed = self.h.0.permissions.borrow_mut().dismissed(prompt_id);
            if let Some(id) = dismissed { self.h.0.push(WebEvent::PermissionDismissed(id)); }
        }
    }
}

wrap_find_handler! {
    struct FindBuilder {
        h: H,
    }

    impl FindHandler {
        fn on_find_result(
            &self,
            _browser: Option<&mut Browser>,
            _identifier: ::std::os::raw::c_int,
            count: ::std::os::raw::c_int,
            _selection_rect: Option<&Rect>,
            active_match_ordinal: ::std::os::raw::c_int,
            _final_update: ::std::os::raw::c_int,
        ) {
            self.h.0.nav.borrow_mut().find = Some((count, active_match_ordinal));
        }
    }
}

wrap_context_menu_handler! {
    struct MenuBuilder {
        h: H,
    }

    impl ContextMenuHandler {
        fn run_context_menu(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            params: Option<&mut ContextMenuParams>,
            _model: Option<&mut MenuModel>,
            callback: Option<&mut RunContextMenuCallback>,
        ) -> ::std::os::raw::c_int {
            // Crossbow draws its own menu; tell CEF we handled it.
            if let Some(p) = params {
                let image_url = if p.has_image_contents() != 0 { s(&p.source_url()) } else { String::new() };
                self.h.0.push(WebEvent::ContextMenu(ContextMenuInfo {
                    x: p.xcoord(),
                    y: p.ycoord(),
                    link_url: s(&p.link_url()),
                    image_url,
                    selection_text: s(&p.selection_text()),
                    page_url: s(&p.page_url()),
                    frame_url: s(&p.frame_url()),
                    is_editable: p.is_editable() != 0,
                }));
                crate::wake(std::time::Duration::ZERO);
            }
            if let Some(cb) = callback {
                cb.cancel();
            }
            1
        }
    }
}

wrap_keyboard_handler! {
    struct KeyboardBuilder {
        h: H,
    }

    impl KeyboardHandler {
        // CEF calls this after the renderer and the page had the event and did
        // not consume it. A tracked press is Crossbow's to handle; anything else
        // keeps CEF's default handling.
        fn on_key_event(&self, _browser: Option<&mut Browser>, event: Option<&KeyEvent>, _os_event: *mut u8) -> ::std::os::raw::c_int {
            let Some(event) = event else { return 0 };
            let tracked = self.h.0.unhandled.borrow_mut().report(event, std::time::Instant::now());
            if tracked {
                crate::wake(std::time::Duration::ZERO);
            }
            tracked as ::std::os::raw::c_int
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod host_call_tests {
    use super::*;
    use cef::rc::{ConvertReturnValue, RcImpl};
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    #[derive(Debug, PartialEq)]
    enum HostCall {
        StopFinding(i32),
        Focus(i32),
        Action(&'static str),
        MouseMove { leave: i32, buttons: u32 },
        MouseButton { up: i32 },
        Key { type_: sys::cef_key_event_type_t, editable: i32 },
        Commit { text: Option<Vec<u16>>, replacement: Option<(u32, u32)>, cursor: i32 },
        Composition { text: Option<Vec<u16>>, replacement: Option<(u32, u32)>, selection: Option<(u32, u32)>, underlines: usize, underlines_null: bool },
    }

    #[derive(Debug)]
    struct RecordedCall {
        call: HostCall,
        protected: bool,
        can_auto_archive: bool,
        display: (Option<ScreenGeometry>, Option<ScreenRect>, f32),
    }

    type Calls = Rc<RefCell<Vec<RecordedCall>>>;

    #[derive(Clone)]
    struct CallSpy {
        calls: Calls,
        shared: std::rc::Weak<Shared>,
    }

    impl CallSpy {
        fn record(&self, call: HostCall) {
            let view = WebView { shared: self.shared.upgrade().expect("spy view is alive") };
            let protected = view.shared.nav.borrow().auto_archive_protected;
            let display = (view.shared.screen.get(), view.shared.root.get(), view.shared.scale.get());
            self.calls.borrow_mut().push(RecordedCall { call, protected, can_auto_archive: view.can_auto_archive(), display });
        }
    }

    struct BrowserSpy {
        host: BrowserHost,
        frame: Frame,
        spy: CallSpy,
    }

    fn display_fixture() -> ScreenGeometry {
        use crate::ScreenRect;
        ScreenGeometry {
            bounds: ScreenRect { x: 0, y: 0, width: 1800, height: 1169 },
            work_area: ScreenRect { x: 0, y: 39, width: 1800, height: 1100 },
        }
    }

    fn screen_info_from_cef(handler: &RenderHandler) -> (i32, ScreenInfo) {
        // Exercise the incoming C ABI that Chromium uses. The pinned cef-rs
        // outgoing screen_info wrapper clones its argument without copying back.
        let raw = handler.get_raw();
        let mut info: sys::_cef_screen_info_t = ScreenInfo::default().into();
        let result = unsafe {
            ((*raw).get_screen_info.unwrap())(raw, std::ptr::null_mut(), &mut info)
        };
        (result, info.into())
    }

    fn view_rect_from_cef(handler: &RenderHandler) -> Rect {
        let raw = handler.get_raw();
        let mut rect: sys::_cef_rect_t = Rect::default().into();
        unsafe { ((*raw).get_view_rect.unwrap())(raw, std::ptr::null_mut(), &mut rect) };
        rect.into()
    }

    fn root_rect_from_cef(handler: &RenderHandler) -> (i32, Rect) {
        let raw = handler.get_raw();
        let mut rect: sys::_cef_rect_t = Rect::default().into();
        let result = unsafe { ((*raw).get_root_screen_rect.unwrap())(raw, std::ptr::null_mut(), &mut rect) };
        (result, rect.into())
    }

    fn screen_point_from_cef(handler: &RenderHandler, x: i32, y: i32) -> (i32, i32, i32) {
        let raw = handler.get_raw();
        let (mut screen_x, mut screen_y) = (123, 456);
        let result = unsafe {
            ((*raw).get_screen_point.unwrap())(raw, std::ptr::null_mut(), x, y, &mut screen_x, &mut screen_y)
        };
        (result, screen_x, screen_y)
    }

    #[test]
    fn pane_position_reaches_cef_root_rect_and_native_screen_point() {
        let (view, _) = view_with_host_spy();
        let root = ScreenRect { x: -1360, y: 40, width: 1120, height: 760 };
        view.set_display_geometry(Some(display_fixture()), Some(root), 2.5);
        // Native NSView (-1700, 80), pane (200, 48), backing 2, egui 2.5.
        view.set_view_position(Some(ViewPosition { x: -1160.0, y: 112.0, screen_scale: 1.25, macos_primary_top: Some(982.0) }));
        let handler = RenderBuilder::new(H(view.shared.clone()));
        let rect = view_rect_from_cef(&handler);
        // The pinned macOS CEF sample requires a zero view origin for layout.
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (0, 0, 640, 420));
        let (result, rect) = root_rect_from_cef(&handler);
        assert_eq!(result, 1);
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (-1360, 40, 1120, 760));
        assert_eq!(screen_point_from_cef(&handler, 10, 20), (1, -1438, 817));
    }

    #[test]
    fn background_display_update_exposes_new_scale_without_viewport_resize() {
        let (view, calls) = view_with_host_spy();
        let next = display_fixture();
        texture::upload_bgra(&mut view.shared.texture.borrow_mut(), &view.shared.device, &view.shared.queue, &[255; 16], 2, 2);
        let texture_before = view.with_texture(|texture| texture.map(|t| (t.generation, t.frame, t.width, t.height)));
        view.set_display_geometry(Some(next), None, 2.5);
        assert_eq!(calls.borrow().last().unwrap().display, (Some(next), None, 2.5), "all values are visible at the C ABI notification");
        let handler = RenderBuilder::new(H(view.shared.clone()));
        let (_, info) = screen_info_from_cef(&handler);
        assert_eq!(info.device_scale_factor, 2.5, "background display update must include the new egui scale");
        assert_eq!((info.rect.x, info.rect.width), (0, 1800));
        assert_eq!(view.size(), (640.0, 420.0));
        assert_eq!(view.with_texture(|texture| texture.map(|t| (t.generation, t.frame, t.width, t.height))), texture_before);
        assert_eq!(take_calls(&calls), vec![HostCall::Action("screen-changed")]);
        view.set_display_geometry(Some(next), None, 2.5);
        view.resize(640.0, 420.0, 2.5);
        assert!(take_calls(&calls).is_empty());
        view.set_display_geometry(Some(next), None, 2.0);
        assert_eq!(calls.borrow().last().unwrap().display, (Some(next), None, 2.0));
        assert_eq!(take_calls(&calls), vec![HostCall::Action("screen-changed")]);
        view.resize(380.0, 800.0, 2.0);
        assert_eq!(take_calls(&calls), vec![HostCall::Action("resize")]);
    }

    #[test]
    fn root_window_moves_reach_hidden_views_without_changing_pane_size() {
        let (view, calls) = view_with_host_spy();
        let handler = RenderBuilder::new(H(view.shared.clone()));
        assert_eq!(root_rect_from_cef(&handler).0, 0);
        let screen = display_fixture();
        let mut root = ScreenRect { x: 100, y: 40, width: 1200, height: 900 };
        for (x, y, scale) in [(100, 40, 2.0), (-1300, -40, 2.5), (-1300, -40, 1.0)] {
            root.x = x;
            root.y = y;
            view.set_display_geometry(Some(screen), Some(root), scale);
            assert_eq!(calls.borrow().last().unwrap().display, (Some(screen), Some(root), scale));
            assert_eq!(take_calls(&calls), vec![HostCall::Action("screen-changed")]);
            let (result, rect) = root_rect_from_cef(&handler);
            assert_eq!(result, 1);
            assert_eq!((rect.x, rect.y, rect.width, rect.height), (x, y, 1200, 900));
            assert_eq!(view.size(), (640.0, 420.0));
            view.set_display_geometry(Some(screen), Some(root), scale);
            assert!(take_calls(&calls).is_empty());
        }
        view.set_display_geometry(None, None, 1.0);
        assert_eq!(root_rect_from_cef(&handler).0, 0);
        assert_eq!(take_calls(&calls), vec![HostCall::Action("screen-changed")]);
    }

    #[test]
    fn invalid_display_updates_do_not_partially_change_screen_or_scale() {
        let (view, calls) = view_with_host_spy();
        let screen = display_fixture();
        view.set_display_geometry(Some(screen), None, 2.0);
        take_calls(&calls);
        let mut invalid = screen;
        invalid.work_area.width = 0;
        view.set_display_geometry(Some(invalid), None, 3.0);
        view.set_display_geometry(Some(screen), Some(ScreenRect { x: 0, y: 0, width: 0, height: 400 }), 3.0);
        for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            view.set_display_geometry(None, None, scale);
            view.resize(380.0, 800.0, scale);
        }
        assert!(take_calls(&calls).is_empty());
        assert_eq!(view.shared.screen.get(), Some(screen));
        assert_eq!(view.shared.scale.get(), 2.0);
        assert_eq!(view.size(), (640.0, 420.0));
        view.set_display_geometry(None, None, 1.5);
        assert_eq!(calls.borrow().last().unwrap().display, (None, None, 1.5));
        assert_eq!(take_calls(&calls), vec![HostCall::Action("screen-changed")]);
    }

    #[test]
    fn position_changes_are_deduplicated_and_missing_or_overflow_points_fail_closed() {
        let (view, calls) = view_with_host_spy();
        let handler = RenderBuilder::new(H(view.shared.clone()));
        assert_eq!(screen_point_from_cef(&handler, 10, 20), (0, 123, 456));
        let position = ViewPosition { x: -1160.25, y: 112.25, screen_scale: 1.25, macos_primary_top: Some(982.0) };
        view.set_view_position(Some(position));
        view.set_view_position(Some(position));
        assert_eq!(take_calls(&calls), vec![HostCall::Action("screen-changed")]);
        assert_eq!(screen_point_from_cef(&handler, 10, 20), (1, -1438, 817));
        assert_eq!(screen_point_from_cef(&handler, i32::MAX, 20), (0, 123, 456));
        view.set_view_position(Some(ViewPosition { x: f64::NAN, ..position }));
        assert!(take_calls(&calls).is_empty());
        assert_eq!(view.shared.position.get(), Some(position));
        view.set_view_position(None);
        assert_eq!(take_calls(&calls), vec![HostCall::Action("screen-changed")]);
        assert_eq!(screen_point_from_cef(&handler, 10, 20), (0, 123, 456));
        let rect = view_rect_from_cef(&handler);
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (0, 0, 640, 420));
    }

    #[test]
    fn screen_geometry_is_independent_of_sidebar_and_split_viewport_sizes() {
        let (view, calls) = view_with_host_spy();
        view.shared.screen.set(Some(display_fixture()));
        let handler = RenderBuilder::new(H(view.shared.clone()));
        for scale in [1.0, 2.0] {
            for width in [1100.0, 932.0, 546.0, 380.0, 1344.0] {
                view.resize(width, 800.0, scale);
                let (result, info) = screen_info_from_cef(&handler);
                assert_eq!(result, 1);
                assert_eq!((info.rect.width, info.rect.height), (1800, 1169));
                assert_eq!((info.available_rect.y, info.available_rect.height), (39, 1100));
                assert_eq!(info.device_scale_factor, scale);
                let rect = view_rect_from_cef(&handler);
                assert_eq!(rect.width, width as i32);
            }
        }
        assert!(!take_calls(&calls).is_empty());
    }

    #[test]
    fn screen_geometry_changes_notify_even_when_scale_and_viewport_match() {
        let (view, calls) = view_with_host_spy();
        let first = display_fixture();
        view.set_screen_geometry(Some(first));
        view.set_screen_geometry(Some(first));
        assert_eq!(take_calls(&calls), vec![HostCall::Action("screen-changed")]);
        let mut second = first;
        second.bounds.x = -1800;
        second.work_area.x = -1800;
        view.set_screen_geometry(Some(second));
        assert_eq!(take_calls(&calls), vec![HostCall::Action("screen-changed")]);
        assert_eq!(view.size(), (640.0, 420.0));
        assert_eq!(view.shared.scale.get(), 1.0);
        let handler = RenderBuilder::new(H(view.shared.clone()));
        let (result, info) = screen_info_from_cef(&handler);
        assert_eq!(result, 1);
        assert_eq!(info.rect.x, -1800);
        second.work_area.width = 0;
        view.set_screen_geometry(Some(second));
        assert_eq!(screen_info_from_cef(&handler).0, 1);
        assert!(take_calls(&calls).is_empty());
        view.set_screen_geometry(None);
        assert_eq!(screen_info_from_cef(&handler).1.rect.width, 0);
        assert_eq!(take_calls(&calls), vec![HostCall::Action("screen-changed")]);
    }

    #[test]
    fn missing_native_monitor_preserves_backing_scale_in_cef_fallback() {
        let (view, _) = view_with_host_spy();
        view.resize(640.0, 420.0, 2.0);
        let handler = RenderBuilder::new(H(view.shared.clone()));
        let (result, info) = screen_info_from_cef(&handler);
        assert_eq!(result, 1);
        assert_eq!(info.device_scale_factor, 2.0);
        assert_eq!((info.rect.width, info.rect.height), (0, 0));
        let raw = handler.get_raw();
        let mut seeded: sys::_cef_screen_info_t = ScreenInfo {
            rect: display_fixture().bounds.cef_rect(),
            available_rect: display_fixture().work_area.cef_rect(),
            ..Default::default()
        }.into();
        let result = unsafe { ((*raw).get_screen_info.unwrap())(raw, std::ptr::null_mut(), &mut seeded) };
        assert_eq!(result, 1);
        let cleared: ScreenInfo = seeded.into();
        assert_eq!((cleared.rect.width, cleared.available_rect.width), (0, 0), "missing native geometry must not retain a stale callback output");
        assert_eq!(cleared.device_scale_factor, 2.0);
    }

    #[test]
    fn capture_loss_notifies_the_existing_host_without_closing_or_reloading() {
        let (view, calls) = view_with_host_spy();
        view.capture_lost();
        assert_eq!(take_calls(&calls), [HostCall::Action("capture-lost")]);
        assert!(view.browser().is_some());
    }

    #[test]
    fn scale_only_resizes_notify_and_repeated_values_are_deduplicated() {
        let (view, calls) = view_with_host_spy();
        for scale in [2.0, 1.0] {
            view.resize(640.0, 420.0, scale);
            assert_eq!(take_calls(&calls), vec![HostCall::Action("screen-changed"), HostCall::Action("resize")]);
            view.resize(640.0, 420.0, scale);
            assert!(take_calls(&calls).is_empty());
        }
    }

    fn key_event_from_cef(handler: &KeyboardHandler, event: &KeyEvent) -> i32 {
        // Exercise the C ABI CEF calls after the renderer acknowledged the event.
        let raw = handler.get_raw();
        let event: sys::cef_key_event_t = event.clone().into();
        unsafe { ((*raw).on_key_event.unwrap())(raw, std::ptr::null_mut(), &event, std::ptr::null_mut()) }
    }

    #[test]
    fn cef_unhandled_reports_answer_tracked_presses_once() {
        let (view, calls) = view_with_host_spy();
        let handler = KeyboardBuilder::new(H(view.shared.clone()));
        let reload = KeyInput { key: crate::NamedKey::Letter('R'), pressed: true, repeat: false, modifiers: Modifiers { command: true, ..Default::default() }, text: None };
        let ticket = view.key_tracked(&reload).expect("a live browser takes the press");
        assert_eq!(take_calls(&calls), [HostCall::Key { type_: sys::cef_key_event_type_t::KEYEVENT_RAWKEYDOWN, editable: 0 }], "the page receives the press");
        assert!(view.take_unhandled_keys().is_empty(), "nothing runs before CEF answers");
        let sent = input::key_events(&reload).remove(0);
        // Key-up and an untracked chord stay with CEF's default handling.
        assert_eq!(key_event_from_cef(&handler, &KeyEvent { type_: KeyEventType::KEYUP, ..sent.clone() }), 0);
        let other = input::key_events(&KeyInput { key: crate::NamedKey::Letter('K'), ..reload.clone() }).remove(0);
        assert_eq!(key_event_from_cef(&handler, &other), 0);
        assert!(view.take_unhandled_keys().is_empty());
        assert_eq!(key_event_from_cef(&handler, &sent), 1, "Crossbow handles its own unhandled press");
        assert_eq!(view.take_unhandled_keys(), [ticket]);
        assert_eq!(key_event_from_cef(&handler, &sent), 0, "and only once");
        assert!(view.take_unhandled_keys().is_empty());
        assert!(take_calls(&calls).is_empty(), "reports send nothing back to the page");
    }

    unsafe extern "C" fn main_frame(_frame: *mut sys::_cef_frame_t) -> i32 { 1 }
    unsafe extern "C" fn sub_frame(_frame: *mut sys::_cef_frame_t) -> i32 { 0 }

    #[test]
    fn navigations_count_main_frame_address_changes_and_load_starts_only() {
        let (view, calls) = view_with_host_spy();
        let spy = CallSpy { calls, shared: Rc::downgrade(&view.shared) };
        let frame = |main: bool| -> Frame {
            let mut raw: sys::_cef_frame_t = unsafe { std::mem::zeroed() };
            raw.is_main = Some(if main { main_frame } else { sub_frame });
            RcImpl::new(raw, spy.clone()).cast::<sys::_cef_frame_t>().wrap_result()
        };
        let client = ClientBuilder::build(view.shared.clone());
        let (display, load) = (client.display_handler().unwrap(), client.load_handler().unwrap());
        let url = CefString::from("https://example.test/next");
        let start = view.navigations();
        // Frames loading, and the loading state they toggle, are no navigation.
        display.on_address_change(None, Some(&mut frame(false)), Some(&url));
        load.on_load_start(None, Some(&mut frame(false)), TransitionType::default());
        loading_state(&view, true);
        loading_state(&view, false);
        assert_eq!(view.navigations(), start);
        // A main-frame address change (same-document ones too), then a reload's load start.
        display.on_address_change(None, Some(&mut frame(true)), Some(&url));
        assert_eq!(view.navigations(), start + 1);
        load.on_load_start(None, Some(&mut frame(true)), TransitionType::default());
        assert_eq!(view.navigations(), start + 2);
        assert_eq!(view.clone().navigations(), start + 2, "clones share the count");
        let (other, _) = view_with_host_spy();
        assert_eq!(view.clone().serial(), view.serial());
        assert_ne!(other.serial(), view.serial(), "every view has its own serial");
    }

    #[test]
    fn a_views_blocked_count_is_its_current_pages_and_the_next_page_starts_at_zero() {
        use crate::content_rules::tests::{Everything, browser, frame, request};
        let (view, _calls) = view_with_profile_and_host_spy("fixture-blocked-count");
        crate::set_content_rules("fixture-blocked-count", Some(Arc::new(Everything::default())));
        let client = ClientBuilder::build(view.shared.clone());
        let (load, requests) = (client.load_handler().unwrap(), client.request_handler().unwrap());
        let top = frame("https://news.example/", None);
        let inner = frame("https://news.example/embed", Some(&top));
        let tab = browser(&top);
        // What CEF does for one script on the IO thread: ask for a handler, then decide.
        let script = |url: &str| {
            let handler = requests.resource_request_handler(None, None, None, 0, 0, None, None).expect("a handler while the profile has rules");
            handler.on_before_resource_load(Some(&mut tab.clone()), Some(&mut top.clone()), Some(&mut request(url, ResourceType::SCRIPT)), None)
        };
        load.on_load_start(None, Some(&mut top.clone()), TransitionType::default());
        assert_eq!(view.blocked(), 0);
        // Chromium's preloader asks first, then the parser asks again after the cancel.
        assert_eq!((script("https://ads.example/a.js"), script("https://ads.example/a.js")), (ReturnValue::CANCEL, ReturnValue::CANCEL));
        assert_eq!(view.blocked(), 1, "one address cancelled twice is one");
        script("https://ads.example/b.js");
        assert_eq!(view.blocked(), 2);
        load.on_load_start(None, Some(&mut inner.clone()), TransitionType::default());
        script("https://ads.example/b.js");
        assert_eq!(view.blocked(), 2, "a frame loading is part of the page");
        load.on_load_start(None, Some(&mut top.clone()), TransitionType::default());
        assert_eq!(view.blocked(), 0, "the next page, or a reload, starts at zero");
        script("https://ads.example/a.js");
        assert_eq!(view.clone().blocked(), 1, "counted again on the next page; clones share it");
        crate::set_content_rules("fixture-blocked-count", None);
    }

    #[test]
    fn a_mouse_leave_tells_the_page_and_forgets_its_tooltip_and_status() {
        let (view, calls) = view_with_host_spy();
        let client = ClientBuilder::build(view.shared.clone());
        let display = client.display_handler().unwrap();
        // A cross-origin frame's title, from where the page last saw the pointer.
        display.on_tooltip(None, Some(&mut CefString::from("Fixture payment frame")));
        display.on_status_message(None, Some(&CefString::from("https://example.test/pay")));
        assert_eq!(view.nav().tooltip.as_deref(), Some("Fixture payment frame"));
        take_calls(&calls);
        view.mouse_leave();
        assert!(matches!(take_calls(&calls).as_slice(), [HostCall::MouseMove { leave: 1, .. }]));
        assert_eq!((view.nav().tooltip, view.nav().status), (None, None), "nothing of the old place is shown");
    }

    #[test]
    fn page_text_reads_the_main_frame_once_and_keeps_only_the_cap() {
        TEXT_VISITORS.with(|visitors| visitors.borrow_mut().clear());
        let (view, calls) = view_with_host_spy();
        let short = view.read_text(64).expect("a live page can be read");
        assert_eq!(take_calls(&calls), [HostCall::Action("get-text")], "one main-frame text request");
        assert!(short.take().is_none(), "nothing before the renderer answers");
        let visitor = TEXT_VISITORS.with(|visitors| visitors.borrow_mut().remove(0));
        visitor.visit(Some(&CefString::from("Visible fixture text")));
        assert_eq!(short.take(), Some(("Visible fixture text".to_string(), false)));
        assert!(short.take().is_none(), "taken once");
        visitor.visit(Some(&CefString::from("A second delivery is ignored")));
        assert!(short.take().is_none());
        drop(visitor);

        // The cap counts bytes and never splits a character.
        let long = view.read_text(10).unwrap();
        let visitor = TEXT_VISITORS.with(|visitors| visitors.borrow_mut().remove(0));
        visitor.visit(Some(&CefString::from("abcdefghé€xyz")));
        assert_eq!(long.take(), Some(("abcdefghé".to_string(), true)));
        let empty = view.read_text(10).unwrap();
        TEXT_VISITORS.with(|visitors| visitors.borrow_mut().remove(0)).visit(None);
        assert_eq!(empty.take(), Some((String::new(), false)), "an absent string is an empty page");
        take_calls(&calls);

        view.shared.browser.replace(None);
        assert!(view.read_text(64).is_none(), "no page, no request");
        assert!(take_calls(&calls).is_empty());
    }

    fn take_calls(calls: &Calls) -> Vec<HostCall> {
        calls.borrow_mut().drain(..).map(|record| record.call).collect()
    }

    unsafe fn text_units(text: *const sys::cef_string_t) -> Option<Vec<u16>> {
        let text = unsafe { text.as_ref() }?;
        if text.length == 0 { return Some(Vec::new()); }
        if text.str_.is_null() { return None; }
        Some(unsafe { std::slice::from_raw_parts(text.str_, text.length) }.to_vec())
    }

    unsafe fn range_pair(range: *const sys::cef_range_t) -> Option<(u32, u32)> {
        unsafe { range.as_ref() }.map(|range| (range.from, range.to))
    }

    unsafe extern "C" fn record_commit(
        host: *mut sys::_cef_browser_host_t,
        text: *const sys::cef_string_t,
        replacement: *const sys::cef_range_t,
        cursor: i32,
    ) {
        let spy = &RcImpl::<sys::_cef_browser_host_t, CallSpy>::get(host).interface;
        spy.record(HostCall::Commit {
            text: unsafe { text_units(text) },
            replacement: unsafe { range_pair(replacement) },
            cursor,
        });
    }

    unsafe extern "C" fn record_composition(
        host: *mut sys::_cef_browser_host_t,
        text: *const sys::cef_string_t,
        underlines: usize,
        underline_data: *const sys::cef_composition_underline_t,
        replacement: *const sys::cef_range_t,
        selection: *const sys::cef_range_t,
    ) {
        let spy = &RcImpl::<sys::_cef_browser_host_t, CallSpy>::get(host).interface;
        spy.record(HostCall::Composition {
            text: unsafe { text_units(text) },
            replacement: unsafe { range_pair(replacement) },
            selection: unsafe { range_pair(selection) },
            underlines,
            underlines_null: underline_data.is_null(),
        });
    }

    unsafe extern "C" fn record_stop_finding(host: *mut sys::_cef_browser_host_t, clear_selection: i32) {
        let spy = &RcImpl::<sys::_cef_browser_host_t, CallSpy>::get(host).interface;
        spy.record(HostCall::StopFinding(clear_selection));
    }

    unsafe extern "C" fn record_focus(host: *mut sys::_cef_browser_host_t, focus: i32) {
        let spy = &RcImpl::<sys::_cef_browser_host_t, CallSpy>::get(host).interface;
        spy.record(HostCall::Focus(focus));
    }

    unsafe extern "C" fn record_mouse_move(host: *mut sys::_cef_browser_host_t, event: *const sys::cef_mouse_event_t, leave: i32) {
        let spy = &RcImpl::<sys::_cef_browser_host_t, CallSpy>::get(host).interface;
        spy.record(HostCall::MouseMove { leave, buttons: unsafe { (*event).modifiers } });
    }

    unsafe extern "C" fn record_mouse_button(
        host: *mut sys::_cef_browser_host_t,
        _event: *const sys::cef_mouse_event_t,
        _button: sys::cef_mouse_button_type_t,
        up: i32,
        _count: i32,
    ) {
        RcImpl::<sys::_cef_browser_host_t, CallSpy>::get(host).interface.record(HostCall::MouseButton { up });
    }

    unsafe extern "C" fn record_wheel(host: *mut sys::_cef_browser_host_t, _event: *const sys::cef_mouse_event_t, _dx: i32, _dy: i32) {
        RcImpl::<sys::_cef_browser_host_t, CallSpy>::get(host).interface.record(HostCall::Action("wheel"));
    }

    unsafe extern "C" fn record_key(host: *mut sys::_cef_browser_host_t, event: *const sys::cef_key_event_t) {
        let spy = &RcImpl::<sys::_cef_browser_host_t, CallSpy>::get(host).interface;
        let event = unsafe { &*event };
        spy.record(HostCall::Key { type_: event.type_, editable: event.focus_on_editable_field });
    }

    unsafe extern "C" fn record_find(host: *mut sys::_cef_browser_host_t, _text: *const sys::cef_string_t, _forward: i32, _match_case: i32, _next: i32) {
        RcImpl::<sys::_cef_browser_host_t, CallSpy>::get(host).interface.record(HostCall::Action("find"));
    }

    unsafe extern "C" fn record_hidden(host: *mut sys::_cef_browser_host_t, _hidden: i32) {
        RcImpl::<sys::_cef_browser_host_t, CallSpy>::get(host).interface.record(HostCall::Action("hide/show"));
    }

    unsafe extern "C" fn record_load_url(frame: *mut sys::_cef_frame_t, _url: *const sys::cef_string_t) {
        RcImpl::<sys::_cef_frame_t, CallSpy>::get(frame).interface.record(HostCall::Action("load-url"));
    }

    thread_local! {
        /// Visitors a page-text read handed to the fake frame, as the renderer would hold them.
        static TEXT_VISITORS: RefCell<Vec<CefStringVisitor>> = const { RefCell::new(Vec::new()) };
    }

    unsafe extern "C" fn record_get_text(frame: *mut sys::_cef_frame_t, visitor: *mut sys::_cef_string_visitor_t) {
        RcImpl::<sys::_cef_frame_t, CallSpy>::get(frame).interface.record(HostCall::Action("get-text"));
        if !visitor.is_null() { TEXT_VISITORS.with(|visitors| visitors.borrow_mut().push(visitor.wrap_result())); }
    }

    macro_rules! record_actions {
        ($raw:ty, $( $callback:ident => $action:literal ),+ $(,)?) => {
            $(unsafe extern "C" fn $callback(raw: *mut $raw) {
                RcImpl::<$raw, CallSpy>::get(raw).interface.record(HostCall::Action($action));
            })+
        };
    }

    record_actions!(sys::_cef_frame_t,
        record_cut => "cut", record_paste => "paste", record_undo => "undo",
        record_redo => "redo", record_copy => "copy", record_select_all => "select-all",
    );
    record_actions!(sys::_cef_browser_host_t,
        record_ime_cancel => "ime-cancel", record_screen_changed => "screen-changed", record_resize => "resize", record_capture_lost => "capture-lost",
    );

    unsafe extern "C" fn test_host(browser: *mut sys::_cef_browser_t) -> *mut sys::_cef_browser_host_t {
        RcImpl::<sys::_cef_browser_t, BrowserSpy>::get(browser).interface.host.clone().into()
    }

    unsafe extern "C" fn test_frame(browser: *mut sys::_cef_browser_t) -> *mut sys::_cef_frame_t {
        RcImpl::<sys::_cef_browser_t, BrowserSpy>::get(browser).interface.frame.clone().into()
    }

    macro_rules! record_navigation {
        ($( $callback:ident => $action:literal ),+ $(,)?) => {
            $(unsafe extern "C" fn $callback(browser: *mut sys::_cef_browser_t) {
                RcImpl::<sys::_cef_browser_t, BrowserSpy>::get(browser).interface.spy.record(HostCall::Action($action));
            })+
        };
    }

    record_navigation!(
        record_back => "back", record_forward => "forward", record_reload => "reload",
        record_reload_ignore_cache => "reload-ignore-cache", record_stop => "stop",
    );

    fn view_with_host_spy() -> (WebView, Calls) {
        view_with_profile_and_host_spy("profile-a")
    }

    fn view_with_profile_and_host_spy(profile_id: &str) -> (WebView, Calls) {
        crate::load_cef_strings();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let cx = &mut Context::from_waker(Waker::noop());
        let Poll::Ready(Ok(adapter)) = pin!(instance.request_adapter(&Default::default())).poll(cx) else {
            panic!("headless Metal adapter is required for this macOS regression");
        };
        let Poll::Ready(Ok((device, queue))) = pin!(adapter.request_device(&Default::default())).poll(cx) else {
            panic!("headless Metal device is required for this macOS regression");
        };
        let view = WebView {
            shared: Rc::new(Shared {
                nav: RefCell::new(NavState::for_live_view("about:blank".into())),
                events: RefCell::new(Vec::new()),
            download_controls: RefCell::new(HashMap::new()),
                unhandled: RefCell::new(UnhandledKeys::default()),
                gesture: Cell::new(Modifiers::default()),
                size: Cell::new((640.0, 420.0)),
                scale: Cell::new(1.0),
                screen: Cell::new(None),
                root: Cell::new(None),
                position: Cell::new(None),
                texture: RefCell::new(None),
                popup_texture: RefCell::new(None),
                popup_rect: Cell::new(None),
                browser: RefCell::new(None),
                device,
                queue,
                profile_id: profile_id.into(),
                accelerated: false,
                serial: next_serial(),
                navigations: Cell::new(0),
                loads: Cell::new(0),
                login: RefCell::new(LoginState::default()),
                permissions: RefCell::new(Pending::default()),
                media: RefCell::new(None),
                translate: RefCell::new(Vec::new()),
                languages: Arc::default(),
                blocked: Arc::default(),
                saves_logins: true,
            }),
        };
        let calls = Rc::new(RefCell::new(Vec::new()));
        let spy = CallSpy { calls: calls.clone(), shared: Rc::downgrade(&view.shared) };
        // CEF structs contain nullable function pointers. RcImpl owns the spies
        // and supplies reference counting; record guard state at C-ABI dispatch.
        let mut host: sys::_cef_browser_host_t = unsafe { std::mem::zeroed() };
        host.ime_commit_text = Some(record_commit);
        host.ime_set_composition = Some(record_composition);
        host.ime_cancel_composition = Some(record_ime_cancel);
        host.stop_finding = Some(record_stop_finding);
        host.find = Some(record_find);
        host.set_focus = Some(record_focus);
        host.was_hidden = Some(record_hidden);
        host.notify_screen_info_changed = Some(record_screen_changed);
        host.was_resized = Some(record_resize);
        host.send_mouse_move_event = Some(record_mouse_move);
        host.send_mouse_click_event = Some(record_mouse_button);
        host.send_mouse_wheel_event = Some(record_wheel);
        host.send_capture_lost_event = Some(record_capture_lost);
        host.send_key_event = Some(record_key);
        let host: BrowserHost = RcImpl::new(host, spy.clone()).cast::<sys::_cef_browser_host_t>().wrap_result();
        let mut frame: sys::_cef_frame_t = unsafe { std::mem::zeroed() };
        frame.cut = Some(record_cut);
        frame.paste = Some(record_paste);
        frame.undo = Some(record_undo);
        frame.redo = Some(record_redo);
        frame.copy = Some(record_copy);
        frame.select_all = Some(record_select_all);
        frame.load_url = Some(record_load_url);
        frame.get_text = Some(record_get_text);
        let frame: Frame = RcImpl::new(frame, spy.clone()).cast::<sys::_cef_frame_t>().wrap_result();
        let mut browser: sys::_cef_browser_t = unsafe { std::mem::zeroed() };
        browser.get_host = Some(test_host);
        browser.get_main_frame = Some(test_frame);
        browser.get_focused_frame = Some(test_frame);
        browser.go_back = Some(record_back);
        browser.go_forward = Some(record_forward);
        browser.reload = Some(record_reload);
        browser.reload_ignore_cache = Some(record_reload_ignore_cache);
        browser.stop_load = Some(record_stop);
        let browser: Browser = RcImpl::new(browser, BrowserSpy { host, frame, spy }).cast::<sys::_cef_browser_t>().wrap_result();
        view.shared.browser.replace(Some(browser));
        (view, calls)
    }

    #[test]
    fn profile_owner_stays_bound_across_clone_and_navigation() {
        let (view, calls) = view_with_profile_and_host_spy("immutable-context-a");
        let cloned = view.clone();
        assert_eq!(view.profile_id(), "immutable-context-a");
        cloned.load_url("https://example.test/next");
        assert_eq!(take_calls(&calls), vec![HostCall::Action("load-url")]);
        assert_eq!(cloned.profile_id(), "immutable-context-a");
        assert_eq!(view.profile_id(), "immutable-context-a");
        let (other, _) = view_with_profile_and_host_spy("immutable-context-b");
        assert_eq!(other.profile_id(), "immutable-context-b");
        assert_eq!(view.profile_id(), "immutable-context-a");
    }

    /// Ask for a popup as CEF does after a click and return the modifiers it reported.
    fn popup_modifiers(view: &WebView, disposition: WindowOpenDisposition, user_gesture: i32) -> Modifiers {
        let handler = ClientBuilder::build(view.shared.clone()).life_span_handler().unwrap();
        let url = CefString::from("https://example.test/next");
        assert_eq!(handler.on_before_popup(None, None, 0, Some(&url), None, disposition, user_gesture, None, None, None, None, None, None), 1);
        let events = view.take_events();
        let [WebEvent::OpenUrl { modifiers, .. }] = events.as_slice() else { panic!("{events:?}") };
        *modifiers
    }

    #[test]
    fn open_url_reports_the_modifiers_of_the_click_or_key_that_asked() {
        let (view, _) = view_with_host_spy();
        let option_command = Modifiers { alt: true, command: true, ..Modifiers::default() };
        let shift = Modifiers { shift: true, ..Modifiers::default() };
        view.mouse_button(2.0, 3.0, MouseButton::Left, false, 1, option_command);
        view.mouse_button(2.0, 3.0, MouseButton::Left, true, 1, option_command);
        assert_eq!(popup_modifiers(&view, WindowOpenDisposition::NEW_BACKGROUND_TAB, 1), option_command);

        // Script that opens a window without a user gesture reports none.
        assert_eq!(popup_modifiers(&view, WindowOpenDisposition::NEW_FOREGROUND_TAB, 0), Modifiers::default());

        // The release decides, as the page's click event does.
        view.mouse_button(2.0, 3.0, MouseButton::Left, false, 1, option_command);
        view.mouse_button(2.0, 3.0, MouseButton::Left, true, 1, shift);
        assert_eq!(popup_modifiers(&view, WindowOpenDisposition::NEW_WINDOW, 1), shift);

        // A later key press replaces the click's modifiers.
        view.key(&KeyInput { key: input::NamedKey::Enter, pressed: true, repeat: false, modifiers: option_command, text: None });
        assert_eq!(popup_modifiers(&view, WindowOpenDisposition::NEW_BACKGROUND_TAB, 1), option_command);
        view.key(&KeyInput { key: input::NamedKey::Enter, pressed: false, repeat: false, modifiers: Modifiers::default(), text: None });
        assert_eq!(popup_modifiers(&view, WindowOpenDisposition::NEW_BACKGROUND_TAB, 1), option_command, "a release keeps the press's modifiers");
        view.mouse_button(2.0, 3.0, MouseButton::Left, false, 1, Modifiers::default());
        view.mouse_button(2.0, 3.0, MouseButton::Left, true, 1, Modifiers::default());
        assert_eq!(popup_modifiers(&view, WindowOpenDisposition::NEW_FOREGROUND_TAB, 1), Modifiers::default());
    }

    mod download_tests {
        use super::*;
        use cef::rc::Rc as CefRc;

        struct ItemState {
            info: DownloadInfo,
            valid: bool,
            reason: DownloadInterruptReason,
            reads: Vec<&'static str>,
            drops: usize,
        }

        struct ItemSpy(Rc<RefCell<ItemState>>);

        impl Drop for ItemSpy {
            fn drop(&mut self) { self.0.borrow_mut().drops += 1; }
        }

        fn read_item<T>(raw: *mut sys::_cef_download_item_t, name: &'static str, read: impl FnOnce(&ItemState) -> T) -> T {
            let mut state = RcImpl::<sys::_cef_download_item_t, ItemSpy>::get(raw).interface.0.borrow_mut();
            state.reads.push(name);
            read(&state)
        }

        fn userfree(text: &str) -> sys::cef_string_userfree_t {
            let text: Vec<u16> = text.encode_utf16().collect();
            unsafe {
                let raw = sys::cef_string_userfree_utf16_alloc();
                if !raw.is_null() && sys::cef_string_utf16_set(text.as_ptr(), text.len(), raw, 1) == 0 {
                    sys::cef_string_userfree_utf16_free(raw);
                    return std::ptr::null_mut();
                }
                raw
            }
        }

        macro_rules! item_getters {
            ($( $name:ident -> $result:ty => $read:expr ),+ $(,)?) => {
                $(unsafe extern "C" fn $name(raw: *mut sys::_cef_download_item_t) -> $result {
                    read_item(raw, stringify!($name), $read)
                })+
            };
        }

        item_getters!(
            item_valid -> i32 => |s| i32::from(s.valid),
            item_progress -> i32 => |s| i32::from(s.info.in_progress),
            item_paused -> i32 => |s| i32::from(s.info.paused),
            item_complete -> i32 => |s| i32::from(s.info.complete),
            item_canceled -> i32 => |s| i32::from(s.info.canceled),
            item_interrupted -> i32 => |s| i32::from(s.info.interrupted),
            item_reason -> sys::cef_download_interrupt_reason_t => |s| s.reason.into(),
            item_percent -> i32 => |s| s.info.percent,
            item_total -> i64 => |s| s.info.total,
            item_received -> i64 => |s| s.info.received,
            item_path -> sys::cef_string_userfree_t => |s| userfree(&s.info.path),
            item_id -> u32 => |s| s.info.id,
            item_url -> sys::cef_string_userfree_t => |s| userfree(&s.info.url),
            item_name -> sys::cef_string_userfree_t => |s| userfree(&s.info.file_name),
            item_mime -> sys::cef_string_userfree_t => |s| userfree(&s.info.mime_type),
        );

        fn item(info: DownloadInfo) -> (DownloadItem, Rc<RefCell<ItemState>>) {
            let state = Rc::new(RefCell::new(ItemState {
                info, valid: true, reason: DownloadInterruptReason::NONE, reads: Vec::new(), drops: 0,
            }));
            let mut raw: sys::_cef_download_item_t = unsafe { std::mem::zeroed() };
            raw.is_valid = Some(item_valid);
            raw.is_in_progress = Some(item_progress);
            raw.is_paused = Some(item_paused);
            raw.is_complete = Some(item_complete);
            raw.is_canceled = Some(item_canceled);
            raw.is_interrupted = Some(item_interrupted);
            raw.get_interrupt_reason = Some(item_reason);
            raw.get_percent_complete = Some(item_percent);
            raw.get_total_bytes = Some(item_total);
            raw.get_received_bytes = Some(item_received);
            raw.get_full_path = Some(item_path);
            raw.get_id = Some(item_id);
            raw.get_url = Some(item_url);
            raw.get_suggested_file_name = Some(item_name);
            raw.get_mime_type = Some(item_mime);
            let item = RcImpl::new(raw, ItemSpy(state.clone())).cast::<sys::_cef_download_item_t>().wrap_result();
            (item, state)
        }

        #[derive(Default, Debug)]
        struct CallbackState {
            continues: Vec<(Option<String>, i32)>,
            actions: Vec<&'static str>,
            drops: usize,
        }

        struct CallbackSpy(Rc<RefCell<CallbackState>>);

        impl Drop for CallbackSpy {
            fn drop(&mut self) { self.0.borrow_mut().drops += 1; }
        }

        unsafe extern "C" fn record_continue(raw: *mut sys::_cef_before_download_callback_t, path: *const sys::cef_string_t, show_dialog: i32) {
            let spy = &RcImpl::<sys::_cef_before_download_callback_t, CallbackSpy>::get(raw).interface;
            let path = unsafe { text_units(path) }.map(|text| String::from_utf16_lossy(&text));
            spy.0.borrow_mut().continues.push((path, show_dialog));
        }

        unsafe extern "C" fn record_cancel(raw: *mut sys::_cef_download_item_callback_t) {
            RcImpl::<sys::_cef_download_item_callback_t, CallbackSpy>::get(raw).interface.0.borrow_mut().actions.push("cancel");
        }

        fn before_callback() -> (BeforeDownloadCallback, Rc<RefCell<CallbackState>>) {
            let state = Rc::new(RefCell::new(CallbackState::default()));
            let mut raw: sys::_cef_before_download_callback_t = unsafe { std::mem::zeroed() };
            raw.cont = Some(record_continue);
            let callback = RcImpl::new(raw, CallbackSpy(state.clone())).cast::<sys::_cef_before_download_callback_t>().wrap_result();
            (callback, state)
        }

        fn update_callback() -> (DownloadItemCallback, Rc<RefCell<CallbackState>>) {
            let state = Rc::new(RefCell::new(CallbackState::default()));
            let mut raw: sys::_cef_download_item_callback_t = unsafe { std::mem::zeroed() };
            raw.cancel = Some(record_cancel);
            let callback = RcImpl::new(raw, CallbackSpy(state.clone())).cast::<sys::_cef_download_item_callback_t>().wrap_result();
            (callback, state)
        }

        fn handler(view: &WebView) -> DownloadHandler {
            ClientBuilder::build(view.shared.clone()).download_handler().unwrap()
        }

        fn before(handler: &DownloadHandler, item: Option<&DownloadItem>, name: Option<&CefString>, callback: Option<&BeforeDownloadCallback>) -> i32 {
            let raw = handler.get_raw();
            // cef-rs 154.5.0 consumes one reference per incoming C-ABI argument.
            // Transfer clones, not borrowed raw pointers, just as its caller does.
            unsafe {
                ((*raw).on_before_download.unwrap())(
                    raw, std::ptr::null_mut(),
                    item.map(|item| item.clone().into()).unwrap_or(std::ptr::null_mut()),
                    name.map(Into::into).unwrap_or(std::ptr::null()),
                    callback.map(|callback| callback.clone().into()).unwrap_or(std::ptr::null_mut()),
                )
            }
        }

        fn update(handler: &DownloadHandler, item: Option<&DownloadItem>, callback: Option<&DownloadItemCallback>) {
            let raw = handler.get_raw();
            unsafe {
                ((*raw).on_download_updated.unwrap())(
                    raw, std::ptr::null_mut(),
                    item.map(|item| item.clone().into()).unwrap_or(std::ptr::null_mut()),
                    callback.map(|callback| callback.clone().into()).unwrap_or(std::ptr::null_mut()),
                );
            }
        }

        fn info() -> DownloadInfo {
            DownloadInfo {
                id: 47, url: "https://download.invalid/fixture".into(), file_name: "suggested.txt".into(),
                mime_type: "text/plain".into(),
                received: 123, total: -1, percent: -1, in_progress: true, ..Default::default()
            }
        }

        fn folder() -> PathBuf {
            let root = std::env::temp_dir().join(format!("crossbow-download-abi-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
            std::fs::create_dir(&root).unwrap();
            root.canonicalize().unwrap()
        }

        fn take_request(view: &WebView) -> DownloadRequest {
            let mut events = view.take_events();
            assert_eq!(events.len(), 1, "{events:?}");
            match events.pop().unwrap() {
                WebEvent::DownloadRequested(request) => request,
                event => panic!("expected a deferred request, got {event:?}"),
            }
        }

        #[test]
        fn logic_download_drain_preserves_other_events_and_releases_borrow_before_continue() {
            let root = folder();
            for profile in ["profile-a", "profile-b"] {
                let (view, _) = view_with_profile_and_host_spy(profile);
                let handler = handler(&view);
                let (item, state) = item(info());
                let (callback, calls) = before_callback();
                view.shared.push(WebEvent::Visited { url: "https://download.invalid/fixture".into(), title: "Fixture".into() });
                update(&handler, Some(&item), None);
                view.shared.push(WebEvent::OpenUrl { url: "about:blank".into(), background: true, popup: false, modifiers: Modifiers::default() });
                assert_eq!(before(&handler, Some(&item), None, Some(&callback)), 1);
                view.shared.push(WebEvent::ContextMenu(ContextMenuInfo::default()));
                update(&handler, Some(&item), None);
                drop(callback);
                let events = view.take_download_events();
                assert!(matches!(events.as_slice(), [WebEvent::Download(_), WebEvent::DownloadRequested(_), WebEvent::Download(_)]));
                for event in events {
                    match event {
                        WebEvent::DownloadRequested(request) => {
                            assert_eq!(request.info.profile_id, profile);
                            // Continue may synchronously enqueue another update.
                            // The shared event queue must not remain borrowed here.
                            view.shared.push(WebEvent::Download(request.info.clone()));
                            request.continue_in(&root, false).unwrap();
                        }
                        WebEvent::Download(info) => assert_eq!(info.profile_id, profile),
                        event => panic!("unexpected logic event: {event:?}"),
                    }
                }
                assert_eq!(calls.borrow().continues.len(), 1);
                assert_eq!(calls.borrow().drops, 1);
                assert!(matches!(view.take_download_events().as_slice(), [WebEvent::Download(_)]));
                assert!(view.take_download_events().is_empty());
                assert!(matches!(view.take_events().as_slice(), [WebEvent::Visited { .. }, WebEvent::OpenUrl { .. }, WebEvent::ContextMenu(_)]));
                state.borrow_mut().info.canceled = true;
                update(&handler, Some(&item), None);
                assert!(matches!(view.take_download_events().as_slice(), [WebEvent::Download(info)] if info.canceled));
                assert!(view.take_events().is_empty());
            }
            assert!(std::fs::read_dir(&root).unwrap().next().is_none(), "recording callbacks do not download bytes");
            std::fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn before_download_retains_only_callback_and_clones_execute_once() {
            let (view, _) = view_with_host_spy();
            let handler = handler(&view);
            let (item, item_state) = item(info());
            let (callback, callback_state) = before_callback();
            let name = CefString::from("résumé?.txt");
            assert_eq!(before(&handler, Some(&item), Some(&name), Some(&callback)), 1);
            assert!(callback_state.borrow().continues.is_empty());
            assert!(!callback.has_one_ref(), "the queued request must retain the callback");
            assert!(item.has_one_ref(), "the live DownloadItem must not escape the callback");
            drop(item);
            drop(callback);
            drop(name);
            assert_eq!(item_state.borrow().drops, 1);
            assert_eq!(callback_state.borrow().drops, 0);
            item_state.borrow_mut().info = DownloadInfo::default();
            let request = take_request(&view);
            assert_eq!(request.info, DownloadInfo { profile_id: "profile-a".into(), file_name: "résumé?.txt".into(), ..info() });
            let duplicate = request.clone();
            let root = folder();
            request.continue_in(&root, false).unwrap();
            assert_eq!(duplicate.continue_in(&root, true).unwrap_err(), "This download request was already handled.");
            assert_eq!(callback_state.borrow().continues, vec![(Some(root.join("résumé_.txt").to_str().unwrap().into()), 0)]);
            assert_eq!(callback_state.borrow().drops, 1);
            assert!(std::fs::read_dir(&root).unwrap().next().is_none());
            std::fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn deferred_requests_keep_original_profiles_and_forward_save_dialog_path() {
            let (first, _) = view_with_host_spy();
            let (second, _) = view_with_profile_and_host_spy("profile-b");
            let first_handler = handler(&first);
            let second_handler = handler(&second);
            let (item, _) = item(info());
            let (first_callback, first_calls) = before_callback();
            let (second_callback, second_calls) = before_callback();
            let name = CefString::from("résumé.txt");
            assert_eq!(before(&first_handler, Some(&item), Some(&name), Some(&first_callback)), 1);
            assert!(second.take_events().is_empty());
            assert_eq!(before(&second_handler, Some(&item), Some(&name), Some(&second_callback)), 1);
            let request_a = take_request(&first);
            let request_b = take_request(&second);
            assert_eq!((&*request_a.info.profile_id, &*request_b.info.profile_id), ("profile-a", "profile-b"));
            assert_eq!(request_a.info.id, request_b.info.id);
            drop((first_handler, first, first_callback, second_callback, item));
            let root = folder();
            let folder_a = root.join("profile-a");
            let folder_b = root.join("profile-b");
            std::fs::create_dir(&folder_a).unwrap();
            std::fs::create_dir(&folder_b).unwrap();
            std::fs::write(folder_a.join("résumé.txt"), "keep").unwrap();
            request_b.continue_in(&folder_b, false).unwrap();
            request_a.continue_in(&folder_a, true).unwrap();
            assert_eq!(first_calls.borrow().continues, vec![(Some(folder_a.join("résumé (1).txt").to_str().unwrap().into()), 1)]);
            assert_eq!(second_calls.borrow().continues, vec![(Some(folder_b.join("résumé.txt").to_str().unwrap().into()), 0)]);
            assert_eq!(std::fs::read_to_string(folder_a.join("résumé.txt")).unwrap(), "keep");
            assert_eq!((first_calls.borrow().drops, second_calls.borrow().drops), (1, 1));
            std::fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn concurrent_downloads_reserve_names_across_profiles_until_terminal_update() {
            let root = folder();
            let (a, _) = view_with_host_spy();
            let (b, _) = view_with_profile_and_host_spy("profile-b");
            let a_handler = handler(&a);
            let b_handler = handler(&b);
            let (download, state) = item(info());
            let name = CefString::from("same.txt");
            let (first, first_calls) = before_callback();
            let (second, second_calls) = before_callback();
            before(&a_handler, Some(&download), Some(&name), Some(&first));
            before(&b_handler, Some(&download), Some(&name), Some(&second));
            take_request(&a).continue_in(&root, false).unwrap();
            take_request(&b).continue_in(&root, false).unwrap();
            assert_eq!(first_calls.borrow().continues[0].0.as_deref(), root.join("same.txt").to_str());
            assert_eq!(second_calls.borrow().continues[0].0.as_deref(), root.join("same (1).txt").to_str(), "two pending transfers must not receive the same destination");
            state.borrow_mut().info.canceled = true;
            update(&a_handler, Some(&download), None);
            a.take_events();
            let (third, third_calls) = before_callback();
            state.borrow_mut().info = DownloadInfo { id: 48, ..info() };
            before(&a_handler, Some(&download), Some(&name), Some(&third));
            take_request(&a).continue_in(&root, false).unwrap();
            assert_eq!(third_calls.borrow().continues[0].0.as_deref(), root.join("same.txt").to_str(), "cancellation releases only its own profile/id reservation");
            state.borrow_mut().info.complete = true;
            update(&a_handler, Some(&download), None);
            state.borrow_mut().info.id = 47;
            update(&b_handler, Some(&download), None);
            assert!(std::fs::read_dir(&root).unwrap().next().is_none(), "path selection must not precreate output files");
            std::fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn missing_suggested_name_still_uses_explicit_canonical_destination() {
            let (view, _) = view_with_host_spy();
            let handler = handler(&view);
            let (item, _) = item(info());
            let (callback, state) = before_callback();
            assert_eq!(before(&handler, Some(&item), None, Some(&callback)), 1);
            drop(callback);
            let request = take_request(&view);
            assert_eq!(request.info.file_name, "suggested.txt");
            let root = folder();
            let selected = root.join("selected");
            let alias = root.join("alias");
            std::fs::create_dir(&selected).unwrap();
            std::os::unix::fs::symlink(&selected, &alias).unwrap();
            request.continue_in(&alias, true).unwrap();
            assert_eq!(state.borrow().continues, vec![(Some(selected.join("suggested.txt").to_str().unwrap().into()), 1)]);
            assert_eq!(state.borrow().drops, 1);
            assert!(std::fs::read_dir(&selected).unwrap().next().is_none());
            std::fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn last_request_clone_drop_releases_callback_without_continuing() {
            let (view, _) = view_with_host_spy();
            let handler = handler(&view);
            let (item, _) = item(info());
            let (callback, state) = before_callback();
            assert_eq!(before(&handler, Some(&item), None, Some(&callback)), 1);
            drop(callback);
            let request = take_request(&view);
            let duplicate = request.clone();
            drop(request);
            assert_eq!(state.borrow().drops, 0);
            drop(duplicate);
            // This proves release without Continue, not a live Chromium cancel.
            assert_eq!(state.borrow().drops, 1);
            assert!(state.borrow().continues.is_empty());
        }

        #[test]
        fn dropping_a_view_with_a_queued_request_releases_unexecuted_callback() {
            let (view, _) = view_with_host_spy();
            let handler = handler(&view);
            let (item, item_state) = item(info());
            let (callback, state) = before_callback();
            assert_eq!(before(&handler, Some(&item), None, Some(&callback)), 1);
            drop((item, callback, handler));
            assert_eq!(item_state.borrow().drops, 1);
            assert_eq!(state.borrow().drops, 0);
            drop(view);
            assert_eq!(state.borrow().drops, 1);
            assert!(state.borrow().continues.is_empty());
        }

        #[test]
        fn rejected_destinations_consume_callback_without_default_fallback() {
            use std::os::unix::ffi::OsStringExt;
            let (view, _) = view_with_host_spy();
            let handler = handler(&view);
            let root = folder();
            std::fs::write(root.join("file"), "keep").unwrap();
            let non_unicode = root.join(std::ffi::OsString::from_vec(vec![b'f', 0xff]));
            for path in [PathBuf::from("relative"), root.join("missing"), root.join("file"), non_unicode] {
                let (item, _) = item(info());
                let (callback, state) = before_callback();
                assert_eq!(before(&handler, Some(&item), None, Some(&callback)), 1);
                drop(callback);
                let request = take_request(&view);
                let duplicate = request.clone();
                let error = request.continue_in(&path, true).unwrap_err();
                assert!(error.contains("download folder") || error.contains("not valid Unicode"), "{error}");
                assert_eq!(duplicate.continue_in(&root, false).unwrap_err(), "This download request was already handled.");
                assert!(state.borrow().continues.is_empty());
                assert_eq!(state.borrow().drops, 1);
            }
            assert!(!root.join("missing").exists());
            assert_eq!(std::fs::read_to_string(root.join("file")).unwrap(), "keep");
            std::fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn missing_before_item_or_callback_is_handled_without_enqueuing() {
            let (view, _) = view_with_host_spy();
            let handler = handler(&view);
            for (has_item, has_callback) in [(false, true), (true, false), (false, false)] {
                let (item, item_state) = item(info());
                let (callback, state) = before_callback();
                assert_eq!(before(&handler, has_item.then_some(&item), None, has_callback.then_some(&callback)), 1);
                assert!(view.take_events().is_empty());
                assert!(item_state.borrow().reads.is_empty());
                assert!(item.has_one_ref());
                assert!(callback.has_one_ref());
                drop((item, callback));
                assert_eq!(item_state.borrow().drops, 1);
                assert_eq!(state.borrow().drops, 1);
                assert!(state.borrow().continues.is_empty());
            }
        }

        #[test]
        fn updates_before_and_after_request_copy_independent_snapshots() {
            let (view, _) = view_with_host_spy();
            let handler = handler(&view);
            let initial = DownloadInfo { in_progress: false, received: 0, ..info() };
            let (item, state) = item(initial.clone());
            let (callback, callback_state) = update_callback();
            update(&handler, Some(&item), Some(&callback));
            let (before_callback, before_state) = before_callback();
            let name = CefString::from("suggested.txt");
            assert_eq!(before(&handler, Some(&item), Some(&name), Some(&before_callback)), 1);
            state.borrow_mut().info = DownloadInfo { path: "/synthetic/final.txt".into(), ..info() };
            update(&handler, Some(&item), Some(&callback));
            assert!(item.has_one_ref());
            assert!(!callback.has_one_ref(), "only the asynchronous control callback is retained");
            drop((item, callback, before_callback));
            assert_eq!(state.borrow().drops, 1);
            assert_eq!(callback_state.borrow().drops, 0);
            assert!(callback_state.borrow().actions.is_empty());
            let events = view.take_events();
            let [WebEvent::Download(early), WebEvent::DownloadRequested(request), WebEvent::Download(later)] = events.as_slice() else { panic!("{events:?}") };
            assert_eq!(*early, DownloadInfo { profile_id: "profile-a".into(), control: early.control.clone(), ..initial.clone() });
            assert_eq!(request.info, DownloadInfo { profile_id: "profile-a".into(), ..initial });
            assert_eq!(*later, DownloadInfo { profile_id: "profile-a".into(), control: later.control.clone(), path: "/synthetic/final.txt".into(), file_name: "final.txt".into(), ..info() });
            assert_eq!(early.control, later.control);
            assert!(early.active());
            assert!(later.active());
            drop(events);
            assert_eq!(before_state.borrow().drops, 1);
            assert!(before_state.borrow().continues.is_empty());
            drop((handler, view));
            assert_eq!(callback_state.borrow().drops, 1);
        }

        #[test]
        fn updates_preserve_terminal_and_interruption_fields_without_callbacks() {
            let (view, _) = view_with_host_spy();
            let handler = handler(&view);
            for (complete, canceled, interrupted, reason) in [
                (true, false, false, DownloadInterruptReason::NONE),
                (false, true, false, DownloadInterruptReason::USER_CANCELED),
                (false, false, true, DownloadInterruptReason::NETWORK_DISCONNECTED),
            ] {
                let source = DownloadInfo { in_progress: false, complete, canceled, interrupted, total: 1000, received: 500, percent: 50, ..info() };
                let (item, state) = item(source.clone());
                state.borrow_mut().reason = reason;
                update(&handler, Some(&item), None);
                assert!(item.has_one_ref());
                drop(item);
                assert_eq!(state.borrow().drops, 1);
                let events = view.take_events();
                let [WebEvent::Download(actual)] = events.as_slice() else { panic!("{events:?}") };
                let error = interrupted.then(|| "Download interrupted (DownloadInterruptReason(CEF_DOWNLOAD_INTERRUPT_REASON_NETWORK_DISCONNECTED)). Try Resume, or retry from the original page.".to_string());
                assert_eq!(*actual, DownloadInfo { profile_id: "profile-a".into(), error, control: actual.control.clone(), ..source });
                assert!(!actual.active());
                assert_eq!(state.borrow().reads.contains(&"item_reason"), interrupted);
            }
        }

        #[test]
        fn update_controls_revoke_before_queued_terminal_events_are_drained() {
            let (view, _) = view_with_host_spy();
            let handler = handler(&view);
            let (item, state) = item(DownloadInfo { paused: true, ..info() });
            let (callback, calls) = update_callback();
            update(&handler, Some(&item), Some(&callback));
            assert!(item.has_one_ref());
            let events = view.take_events();
            let [WebEvent::Download(info)] = events.as_slice() else { panic!("{events:?}") };
            assert!(info.paused, "the CEF paused getter must reach the immutable snapshot");
            let control = info.control.as_ref().unwrap().clone();
            assert!(control.status().resume);
            assert!(control.request("profile-b", info.id, crate::DownloadCommand::Cancel).is_err());
            control.request("profile-a", info.id, crate::DownloadCommand::Cancel).unwrap();
            assert_eq!(calls.borrow().actions, ["cancel"]);
            state.borrow_mut().info.canceled = true;
            update(&handler, Some(&item), Some(&callback));
            assert!(control.request("profile-a", info.id, crate::DownloadCommand::Cancel).is_err());
            assert!(!control.status().available, "old UI snapshots must be revoked before event draining");
            assert!(!info.active(), "stale progress stops keeping the card alive before the terminal event drains");
            assert!(view.shared.download_controls.borrow().is_empty());
            drop(callback);
            assert_eq!(calls.borrow().drops, 1);
        }

        #[test]
        fn browser_close_revokes_download_controls_retained_by_old_snapshots() {
            let (view, _) = view_with_host_spy();
            let handler = handler(&view);
            let (item, _) = item(info());
            let (callback, calls) = update_callback();
            update(&handler, Some(&item), Some(&callback));
            drop(callback);
            let events = view.take_events();
            let [WebEvent::Download(info)] = events.as_slice() else { panic!("{events:?}") };
            let control = info.control.as_ref().unwrap();
            assert!(info.active());
            ClientBuilder::build(view.shared.clone()).life_span_handler().unwrap().on_before_close(None);
            assert!(!control.status().available);
            assert!(!info.active(), "revoked browser ownership must allow the card to expire");
            assert!(!info.canceled && !info.complete, "closure is not a transfer outcome");
            assert!(control.request("profile-a", info.id, crate::DownloadCommand::Cancel).is_err());
            assert_eq!(calls.borrow().drops, 1);
            assert!(calls.borrow().actions.is_empty());
        }

        #[test]
        fn missing_update_item_releases_callback_without_event_or_cancel() {
            let (view, _) = view_with_host_spy();
            let handler = handler(&view);
            let (callback, state) = update_callback();
            update(&handler, None, Some(&callback));
            update(&handler, None, None);
            assert!(view.take_events().is_empty());
            assert!(callback.has_one_ref());
            drop(callback);
            assert_eq!(state.borrow().drops, 1);
            assert!(state.borrow().actions.is_empty());
        }

        #[test]
        fn invalid_before_item_fails_closed_without_reading_other_fields() {
            let (view, _) = view_with_host_spy();
            let handler = handler(&view);
            let (item, item_state) = item(info());
            item_state.borrow_mut().valid = false;
            let (callback, state) = before_callback();
            assert_eq!(before(&handler, Some(&item), None, Some(&callback)), 1);
            assert_eq!(item_state.borrow().reads, vec!["item_valid"]);
            assert!(view.take_events().is_empty());
            assert!(callback.has_one_ref());
            drop(callback);
            assert_eq!(state.borrow().drops, 1);
            assert!(state.borrow().continues.is_empty());
        }

        #[test]
        fn invalid_update_item_fails_closed_without_reading_other_fields() {
            let (view, _) = view_with_host_spy();
            let handler = handler(&view);
            let (item, item_state) = item(info());
            item_state.borrow_mut().valid = false;
            let (callback, state) = update_callback();
            update(&handler, Some(&item), Some(&callback));
            assert_eq!(item_state.borrow().reads, vec!["item_valid"]);
            assert!(view.take_events().is_empty());
            assert!(callback.has_one_ref());
            drop(callback);
            assert_eq!(state.borrow().drops, 1);
            assert!(state.borrow().actions.is_empty());
        }
    }

    fn loading_state(view: &WebView, loading: bool) {
        ClientBuilder::build(view.shared.clone()).load_handler().unwrap()
            .on_loading_state_change(None, loading as i32, 1, 1);
    }

    fn editable_focus(view: &WebView, editable: bool) {
        ClientBuilder::build(view.shared.clone()).render_handler().unwrap()
            .on_virtual_keyboard_requested(None, if editable { TextInputMode::TEXT } else { TextInputMode::NONE });
    }

    fn loaded_view_with_host_spy() -> (WebView, Calls) {
        let (view, calls) = view_with_host_spy();
        loading_state(&view, false);
        assert!(view.can_auto_archive());
        (view, calls)
    }

    fn key_input(pressed: bool, text: Option<char>) -> KeyInput {
        KeyInput { key: input::NamedKey::Letter('A'), pressed, repeat: false, modifiers: Modifiers::default(), text }
    }

    fn assert_mutation_protects(dispatch: impl Fn(&WebView)) {
        for editable in [false, true] {
            let (view, calls) = loaded_view_with_host_spy();
            editable_focus(&view, editable);
            dispatch(&view);
            let calls = calls.borrow();
            assert!(!calls.is_empty(), "must exercise the real WebView-to-CEF dispatch");
            assert!(calls.iter().all(|call| call.protected && !call.can_auto_archive),
                "protection must latch before every CEF call, even without editable focus: {calls:?}");
            assert!(view.nav().auto_archive_protected);
            assert!(!view.can_auto_archive());
        }
    }

    #[test]
    fn archive_guard_protects_pointer_down() {
        for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
            assert_mutation_protects(|view| view.mouse_button(2.0, 3.0, button, false, 1, Modifiers::default()));
        }
    }

    #[test]
    fn archive_guard_protects_drag_without_a_prior_down() {
        for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
            assert_mutation_protects(|view| view.mouse_move(2.0, 3.0, Modifiers::default(), &[button]));
        }
    }

    #[test]
    fn archive_guard_protects_wheel() {
        for delta in [0.0, 1.0, -120.0] {
            assert_mutation_protects(|view| view.mouse_wheel(2.0, 3.0, 0.0, delta, Modifiers::default()));
        }
    }

    #[test]
    fn archive_guard_protects_key_down_without_text() {
        assert_mutation_protects(|view| view.key(&KeyInput { key: input::NamedKey::ArrowLeft, ..key_input(true, None) }));
    }

    #[test]
    fn archive_guard_protects_text_and_repeated_key_down() {
        for repeat in [false, true] {
            assert_mutation_protects(|view| view.key(&KeyInput { repeat, ..key_input(true, Some('é')) }));
        }
        let (view, calls) = loaded_view_with_host_spy();
        view.key(&key_input(true, Some('a')));
        assert_eq!(take_calls(&calls), vec![
            HostCall::Key { type_: sys::cef_key_event_type_t::KEYEVENT_RAWKEYDOWN, editable: 0 },
            HostCall::Key { type_: sys::cef_key_event_type_t::KEYEVENT_CHAR, editable: 0 },
        ]);
    }

    #[test]
    fn archive_guard_protects_ime_commit() {
        for text in ["é漢", ""] { assert_mutation_protects(|view| view.ime_commit(text)); }
    }

    #[test]
    fn archive_guard_protects_ime_preedit() {
        for text in ["é漢", ""] { assert_mutation_protects(|view| view.ime_set_composition(text)); }
    }

    #[test]
    fn archive_guard_protects_cut() { assert_mutation_protects(WebView::cut); }

    #[test]
    fn archive_guard_protects_paste() { assert_mutation_protects(WebView::paste); }

    #[test]
    fn archive_guard_protects_undo() { assert_mutation_protects(WebView::undo); }

    #[test]
    fn archive_guard_protects_redo() { assert_mutation_protects(WebView::redo); }

    #[test]
    fn archive_guard_leaves_harmless_actions_unprotected() {
        let actions: &[(&str, fn(&WebView))] = &[
            ("hover", |view| view.mouse_move(2.0, 3.0, Modifiers::default(), &[])),
            ("leave", WebView::mouse_leave),
            ("button-up", |view| view.mouse_button(2.0, 3.0, MouseButton::Left, true, 1, Modifiers::default())),
            ("key-up", |view| view.key(&key_input(false, None))),
            ("key-up-with-text", |view| view.key(&key_input(false, Some('a')))),
            ("focus", |view| view.set_focus(true)),
            ("blur", |view| view.set_focus(false)),
            ("hide", |view| view.set_hidden(true)),
            ("show", |view| view.set_hidden(false)),
            ("find", |view| view.find("fixture", true, false)),
            ("clear-find", |view| view.find("", true, false)),
            ("stop-find", WebView::stop_find),
            ("dismiss-find", WebView::dismiss_find),
            ("copy", WebView::copy),
            ("select-all", WebView::select_all),
            ("ime-cancel", WebView::ime_cancel),
        ];
        for (label, action) in actions {
            for editable in [false, true] {
                let (view, calls) = loaded_view_with_host_spy();
                editable_focus(&view, editable);
                action(&view);
                let calls = calls.borrow();
                assert!(!calls.is_empty(), "{label} must dispatch through CEF");
                assert!(calls.iter().all(|call| !call.protected && call.can_auto_archive), "{label}: {calls:?}");
                assert!(!view.nav().auto_archive_protected, "{label}");
                assert!(view.can_auto_archive(), "{label}");
            }
        }
    }

    #[test]
    fn archive_guard_is_sticky_across_focus_find_visibility_and_navigation() {
        let (view, calls) = loaded_view_with_host_spy();
        view.paste();
        let alias = view.clone();
        let actions: &[fn(&WebView)] = &[
            |view| view.set_focus(false), |view| view.set_focus(true),
            |view| view.set_hidden(true), |view| view.set_hidden(false),
            |view| view.find("fixture", true, false), |view| view.find("", true, false),
            WebView::stop_find, WebView::dismiss_find, WebView::ime_cancel,
            |view| view.load_url("https://fixture.invalid/next"),
            WebView::go_back, WebView::go_forward, WebView::reload,
            WebView::reload_ignore_cache, WebView::stop,
        ];
        for action in actions {
            calls.borrow_mut().clear();
            action(&alias);
            editable_focus(&view, false);
            loading_state(&view, true);
            loading_state(&view, false);
            assert!(view.nav().auto_archive_protected);
            assert!(!view.can_auto_archive());
            let calls = calls.borrow();
            assert!(!calls.is_empty());
            assert!(calls.iter().all(|call| call.protected && !call.can_auto_archive), "{calls:?}");
        }
        // A public snapshot cannot reset the live guard.
        let mut snapshot = view.nav();
        snapshot.auto_archive_protected = false;
        assert!(!snapshot.auto_archive_protected);
        assert!(!alias.can_auto_archive());
    }

    #[test]
    fn archive_guard_is_independent_for_two_live_views() {
        let (first, _) = loaded_view_with_host_spy();
        let (second, second_calls) = loaded_view_with_host_spy();
        first.cut();
        assert!(first.nav().auto_archive_protected);
        assert!(!first.can_auto_archive());
        assert!(!second.nav().auto_archive_protected);
        assert!(second.can_auto_archive());
        second.copy();
        assert!(second_calls.borrow().iter().all(|call| !call.protected && call.can_auto_archive));
        second.ime_set_composition("fixture");
        assert!(second.nav().auto_archive_protected);
        assert!(!second.can_auto_archive());
    }

    #[test]
    fn archive_guard_refuses_unknown_loading_and_absent_browser() {
        assert!(!NavState::default().auto_archive_protected, "unloaded saved tabs are not protected");
        let (view, _) = view_with_host_spy();
        assert!(!view.nav().auto_archive_protected);
        assert!(view.nav().loading);
        assert!(!view.can_auto_archive(), "initial live state is not known yet");
        loading_state(&view, false);
        assert!(view.can_auto_archive());
        loading_state(&view, true);
        assert!(!view.can_auto_archive());
        loading_state(&view, false);
        assert!(view.can_auto_archive());
        view.shared.browser.replace(None);
        assert!(!view.can_auto_archive());
    }

    #[test]
    fn archive_guard_refuses_closed_browser_after_lifecycle_callback() {
        let (view, _) = loaded_view_with_host_spy();
        ClientBuilder::build(view.shared.clone()).life_span_handler().unwrap().on_before_close(None);
        assert!(!view.can_auto_archive());
        assert!(view.browser().is_none());
        assert!(matches!(view.take_events().as_slice(), [WebEvent::Closed]));
    }

    #[test]
    fn archive_guard_observes_real_client_crash_callback() {
        let (view, _) = loaded_view_with_host_spy();
        let handler = ClientBuilder::build(view.shared.clone()).request_handler().expect("crashes must be observed");
        handler.on_render_process_terminated(None, TerminationStatus::PROCESS_CRASHED, 1, None);
        assert!(view.nav().crashed);
        assert!(!view.can_auto_archive());
        loading_state(&view, false);
        assert!(!view.can_auto_archive(), "a late load-end must not erase crash state");
        loading_state(&view, true);
        assert!(!view.nav().crashed);
        assert!(!view.can_auto_archive());
        loading_state(&view, false);
        assert!(view.can_auto_archive());
        view.undo();
        handler.on_render_process_terminated(None, TerminationStatus::PROCESS_CRASHED, 1, None);
        loading_state(&view, true);
        loading_state(&view, false);
        assert!(!view.can_auto_archive(), "crash recovery must not reset input protection");
    }

    #[test]
    fn find_dismissal_keeps_selection_then_hands_focus_back_once() {
        let (view, calls) = view_with_host_spy();
        for result in [None, Some((0, 0)), Some((3, 2))] {
            view.shared.nav.borrow_mut().find = result;
            view.dismiss_find();
            assert_eq!(take_calls(&calls), vec![HostCall::StopFinding(0), HostCall::Focus(1)]);
            assert_eq!(view.nav().find, None);
        }
    }

    #[test]
    fn find_query_clear_does_not_hand_focus_to_the_page() {
        let (view, calls) = view_with_host_spy();
        for empty_query in [false, true] {
            view.shared.nav.borrow_mut().find = Some((3, 2));
            if empty_query { view.find("", true, false); } else { view.stop_find(); }
            assert_eq!(take_calls(&calls), vec![HostCall::StopFinding(1)]);
            assert_eq!(view.nav().find, None);
        }
    }

    #[test]
    fn ime_commit_passes_invalid_replacement_range_to_cef() {
        let (view, calls) = view_with_host_spy();
        for text in ["IME_ONLY", "é漢", "A\u{1d11e}", ""] {
            view.ime_commit(text);
            assert_eq!(take_calls(&calls), vec![HostCall::Commit {
                text: Some(text.encode_utf16().collect()),
                replacement: Some((u32::MAX, u32::MAX)),
                cursor: 0,
            }]);
        }
    }

    #[test]
    fn ime_composition_passes_invalid_replacement_and_utf16_selection_to_cef() {
        let (view, calls) = view_with_host_spy();
        for text in ["PREEDIT", "é漢", "A\u{1d11e}", ""] {
            view.ime_set_composition(text);
            let len = text.encode_utf16().count() as u32;
            assert_eq!(take_calls(&calls), vec![HostCall::Composition {
                text: Some(text.encode_utf16().collect()),
                replacement: Some((u32::MAX, u32::MAX)),
                selection: Some((len, len)),
                underlines: 0,
                underlines_null: true,
            }]);
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    #[test]
    fn a_frame_on_about_blank_or_not_loaded_yet_has_its_nearest_ancestors_address() {
        let chain = |urls: &[&str]| document_address(urls.iter().map(|url| url.to_string()));
        assert_eq!(chain(&["https://a.example.test/"]), "https://a.example.test/");
        assert_eq!(chain(&["about:blank", "http://127.0.0.1:8123/"]), "http://127.0.0.1:8123/");
        assert_eq!(chain(&["about:srcdoc", "about:blank", "https://b.example.test/x"]), "https://b.example.test/x");
        assert_eq!(chain(&["", "https://c.example.test/"]), "https://c.example.test/");
        assert_eq!(chain(&["about:blank#top", "https://c.example.test/"]), "https://c.example.test/");
        assert_eq!(chain(&["https://d.example.test/", "http://127.0.0.1:8123/"]), "https://d.example.test/", "a frame's own address wins");
        assert_eq!(chain(&["about:blank"]), "about:blank", "nothing to inherit from");
        assert_eq!(chain(&["data:text/html,x", "https://e.example.test/"]), "data:text/html,x", "data: has its own opaque origin");
    }

    #[test]
    fn the_context_menu_reports_the_frame_its_selection_is_in() {
        let source = include_str!("view.rs");
        let menu = &source[source.find("fn run_context_menu(").unwrap()..];
        assert!(menu[..menu.find("cb.cancel()").unwrap()].contains("frame_url: s(&p.frame_url()),"));
    }

    #[test]
    fn missing_profile_context_stops_web_view_creation() {
        // A headless device satisfies the real constructor without a window,
        // CEF initialization, filesystem fixture or security permission prompt.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let cx = &mut Context::from_waker(Waker::noop());
        let Poll::Ready(Ok(adapter)) = pin!(instance.request_adapter(&Default::default())).poll(cx) else {
            panic!("headless Metal adapter is required for this macOS regression");
        };
        let Poll::Ready(Ok((device, queue))) = pin!(adapter.request_device(&Default::default())).poll(cx) else {
            panic!("headless Metal device is required for this macOS regression");
        };
        assert!(crate::ROOT_CACHE_PATH.get().is_none());
        let view = WebView::new(WebViewConfig {
            url: "about:blank".into(),
            profile_id: "profile-a".into(),
            data_dir: PathBuf::from("unused"),
            size: (640.0, 420.0),
            scale: 1.0,
            screen: None,
            device,
            queue,
            saves_logins: true,
        });
        assert!(view.is_none());
    }
}
