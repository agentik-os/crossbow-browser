//! What pages may do beyond the page: permission prompts and links to other
//! apps, per tab. Remembered decisions are the profile's, in the library
//! (`crossbow_core::site_permissions`); everything here lasts only as long as
//! its page.
//!
//! A prompt shows only on the focused tab, one at a time, oldest first; the
//! rest wait. Leaving the page, closing the tab or switching Spaces answers a
//! waiting request Don't Allow, without remembering it. Links are logged by
//! scheme only.

use crossbow_core::site_permissions::{self as rules, Decision, Permission};
use crossbow_core::store::Store;
use crossbow_engine::permissions::{media, prompt};
use crossbow_engine::{ExternalLink, PermissionRequest};
use std::collections::HashMap;

/// Seconds between two quiet "tried to open" notices on one tab, and between
/// two links a site opens without a click.
pub const LINK_INTERVAL: f64 = 10.0;
/// Seconds a new prompt ignores clicks, so a click meant for the page can't land on Allow.
pub const INPUT_DELAY: f64 = 0.5;
/// Peek and Little Windows never hand a link to another app.
pub const FROM_TABS_ONLY: &str = "Links to other apps open only from tabs. Open this page in a tab to use it.";
/// Requests one tab may keep waiting; more are refused.
const MAX_WAITING: usize = 8;

/// The app that opens a link, as macOS reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handler {
    pub name: String,
    pub bundle_id: String,
}

/// The system's apps and this app's bundle, faked in tests.
pub trait Apps {
    /// The app that would open `url`, if any.
    fn handler(&self, url: &str) -> Option<Handler>;
    /// Hand `url` to that app. False if the system refused.
    fn open(&mut self, url: &str) -> bool;
    /// Crossbow's own bundle id.
    fn own_bundle(&self) -> Option<String>;
    /// Whether Crossbow's Info.plist has `key`; systems without one need none.
    fn declares(&self, key: &str) -> bool;
}

/// The Info.plist keys macOS requires before an app uses a device, any one of
/// them. Without one, macOS ends an app that touches the camera or microphone
/// and never authorizes location, so the permission is refused instead.
pub fn usage_keys(permission: Permission) -> &'static [&'static str] {
    match permission {
        Permission::Camera => &["NSCameraUsageDescription"],
        Permission::Microphone => &["NSMicrophoneUsageDescription"],
        Permission::Location => &["NSLocationWhenInUseUsageDescription", "NSLocationUsageDescription"],
        _ => &[],
    }
}

/// What a request asks for, in Crossbow's terms. None for anything Crossbow
/// never offers, which is refused without a prompt.
pub fn requested(is_media: bool, bits: u32) -> Option<Vec<Permission>> {
    use Permission::*;
    let table: &[(u32, Option<Permission>)] = if is_media {
        &[(media::MICROPHONE, Some(Microphone)), (media::CAMERA, Some(Camera)), (media::SCREEN_AUDIO, Some(ScreenShare)), (media::SCREEN_VIDEO, Some(ScreenShare))]
    } else {
        &[
            (prompt::CAMERA_STREAM, Some(Camera)), (prompt::CAMERA_PAN_TILT_ZOOM, Some(Camera)), (prompt::MIC_STREAM, Some(Microphone)),
            (prompt::CAPTURED_SURFACE_CONTROL, Some(ScreenShare)), (prompt::GEOLOCATION, Some(Location)), (prompt::NOTIFICATIONS, Some(Notifications)),
            (prompt::CLIPBOARD, Some(Clipboard)), (prompt::MULTIPLE_DOWNLOADS, Some(MultipleDownloads)), (prompt::MIDI_SYSEX, Some(Midi)),
            (prompt::WINDOW_MANAGEMENT, Some(WindowManagement)), (prompt::LOCAL_FONTS, Some(LocalFonts)), (prompt::IDLE_DETECTION, Some(IdleDetection)),
            (prompt::STORAGE_ACCESS, Some(StorageAccess)), (prompt::TOP_LEVEL_STORAGE_ACCESS, Some(StorageAccess)), (prompt::SENSORS, Some(MotionSensors)),
            (prompt::LOCAL_NETWORK_ACCESS, Some(LocalNetwork)), (prompt::LOCAL_NETWORK, Some(LocalNetwork)), (prompt::LOOPBACK_NETWORK, Some(LocalNetwork)),
            (prompt::FILE_SYSTEM_ACCESS, Some(FileEditing)), (prompt::REGISTER_PROTOCOL_HANDLER, Some(ProtocolHandlers)),
            (prompt::KEYBOARD_LOCK, Some(KeyboardLock)), (prompt::POINTER_LOCK, Some(PointerLock)),
            // Crossbow has no UI for these.
            (prompt::AR_SESSION, None), (prompt::VR_SESSION, None), (prompt::HAND_TRACKING, None), (prompt::PROTECTED_MEDIA_IDENTIFIER, None),
            (prompt::WEB_APP_INSTALLATION, None), (prompt::IDENTITY_PROVIDER, None), (prompt::DISK_QUOTA, None),
        ]
    };
    let mut known = 0;
    let mut permissions = Vec::new();
    for (bit, permission) in table.iter().filter(|(bit, _)| bits & bit != 0) {
        known |= bit;
        permissions.push((*permission)?);
    }
    permissions.sort();
    permissions.dedup();
    (bits & !known == 0 && !permissions.is_empty()).then_some(permissions)
}

/// What becomes of a request before anyone is asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every permission is remembered as allowed.
    Allow,
    /// Refused without a prompt: nothing Crossbow offers, not a web page, or
    /// camera, microphone, screen or location from an insecure page.
    Refuse,
    /// The profile blocked these on this site.
    Blocked(Vec<Permission>),
    /// Ask about these; the others asked for are already allowed.
    Ask(Vec<Permission>),
}

/// The origins a request is decided under: the one that asked and the page's.
pub fn origins(request: &PermissionRequest) -> Option<(String, String)> {
    Some((rules::origin_of(&request.origin)?, rules::origin_of(&request.page)?))
}

pub fn verdict(store: &Store, profile: &str, request: &PermissionRequest) -> Verdict {
    let (Some(permissions), Some((origin, top))) = (requested(request.media, request.bits), origins(request)) else { return Verdict::Refuse };
    // A secure context needs every frame above it secure too.
    if permissions.iter().any(|permission| permission.needs_secure_origin()) && !(rules::is_secure_origin(&origin) && rules::is_secure_origin(&top)) {
        return Verdict::Refuse;
    }
    // A decision that can't be read is asked about again.
    let decided: Vec<Option<Decision>> = permissions.iter().map(|permission| rules::decision(store, profile, &origin, &top, *permission).ok().flatten()).collect();
    let blocked: Vec<Permission> = permissions.iter().zip(&decided).filter(|(_, decision)| **decision == Some(Decision::Block)).map(|(permission, _)| *permission).collect();
    if !blocked.is_empty() { return Verdict::Blocked(blocked); }
    let ask: Vec<Permission> = permissions.iter().zip(&decided).filter(|(_, decision)| decision.is_none()).map(|(permission, _)| *permission).collect();
    if ask.is_empty() { Verdict::Allow } else { Verdict::Ask(ask) }
}

/// The site as prompts name it: the host, and the scheme only for http.
pub fn site(origin: &str) -> &str {
    origin.strip_prefix("https://").unwrap_or(origin)
}

/// A question waiting for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Question {
    Permission {
        id: u64,
        profile: String,
        origin: String,
        top_origin: String,
        /// The permissions not decided yet; answering decides them.
        permissions: Vec<Permission>,
    },
    /// A clicked link to another app.
    Link {
        url: String,
        scheme: String,
        app: Handler,
        profile: String,
        top_origin: Option<String>,
        /// Whether "Always allow" can apply: the page itself opened the link.
        can_always: bool,
    },
    /// A clicked link that no app on this Mac opens.
    NoApp { top_origin: Option<String> },
}

impl Question {
    /// "meet.example wants to use your camera", "meet.example wants to open Zoom".
    pub fn title(&self) -> String {
        match self {
            Question::Permission { origin, permissions, .. } => format!("{} wants to {}", site(origin), rules::phrase(permissions)),
            Question::Link { app, .. } => format!("Open {}?", app.name),
            Question::NoApp { .. } => "No app on this Mac can open this link".into(),
        }
    }

    /// The line under the title, if any.
    pub fn detail(&self) -> Option<String> {
        match self {
            Question::Permission { origin, top_origin, .. } => (origin != top_origin).then(|| format!("Embedded on {}", site(top_origin))),
            Question::Link { app, top_origin, .. } => Some(format!("{} wants to open {}.", top_origin.as_deref().map_or("This page", site), app.name)),
            Question::NoApp { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Waiting {
    pub tab: String,
    /// The view the question came from, and its page load.
    pub serial: u64,
    pub load: u64,
    pub question: Question,
    /// "Remember for this site" (on by default), or "Always allow…" (off).
    pub remember: bool,
    /// When the prompt first showed, for [`INPUT_DELAY`].
    pub shown_at: Option<f64>,
}

impl Waiting {
    /// Whether clicks count yet.
    pub fn ready(&self, now: f64) -> bool {
        self.shown_at.is_some_and(|shown| now - shown >= INPUT_DELAY)
    }
}

/// "{site} tried to open {app}", for a link a page opened without a click.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Indicator {
    pub load: u64,
    pub url: String,
    pub scheme: String,
    pub app: Handler,
    pub top_origin: Option<String>,
}

impl Indicator {
    pub fn text(&self) -> String {
        format!("{} tried to open {}", self.top_origin.as_deref().map_or("This page", site), self.app.name)
    }
}

/// Permissions a remembered Block refused on a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blocked {
    pub load: u64,
    pub origin: String,
    pub permissions: Vec<Permission>,
}

#[derive(Default)]
struct TabState {
    blocked: Option<Blocked>,
    indicator: Option<Indicator>,
    indicator_at: Option<f64>,
    /// The page load and time of the last link opened without a click.
    unasked_load: Option<u64>,
    unasked_at: Option<f64>,
}

/// An answer for a view's request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reply {
    pub serial: u64,
    pub id: u64,
    pub allow: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Allow, or Open.
    Allow,
    /// Don't Allow, Cancel or OK.
    Deny,
    /// Escape: Don't Allow, never remembered.
    Dismiss,
}

/// What an answered question asks of the app.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub reply: Option<Reply>,
    pub notice: Option<String>,
}

/// One tab's page as the app last saw it.
#[derive(Clone, Copy, Debug)]
pub struct Page<'a> {
    pub tab: &'a str,
    pub serial: u64,
    pub load: u64,
}

pub struct SitePermissions {
    apps: Box<dyn Apps>,
    waiting: Vec<Waiting>,
    tabs: HashMap<String, TabState>,
    space: Option<String>,
    /// The site Site Settings brings into view next time it shows.
    pub reveal: Option<String>,
    /// What the app should say, and the undeclared permissions already said.
    notices: Vec<String>,
    undeclared_said: Vec<Permission>,
}

impl SitePermissions {
    pub fn new(apps: Box<dyn Apps>) -> SitePermissions {
        SitePermissions { apps, waiting: Vec::new(), tabs: HashMap::new(), space: None, reveal: None, notices: Vec::new(), undeclared_said: Vec::new() }
    }

    /// A tab's page asked for permissions. Some answer now, or None while it
    /// waits for the user.
    pub fn request(&mut self, store: &Store, page: Page<'_>, profile: Option<&str>, request: &PermissionRequest) -> Option<bool> {
        let Some(profile) = profile else { return Some(false) };
        if !self.declared(request) { return Some(false); }
        match verdict(store, profile, request) {
            Verdict::Allow => Some(true),
            Verdict::Refuse => Some(false),
            Verdict::Blocked(permissions) => {
                let origin = rules::origin_of(&request.origin).unwrap_or_default();
                let state = self.tabs.entry(page.tab.to_owned()).or_default();
                match &mut state.blocked {
                    Some(blocked) if blocked.load == request.load && blocked.origin == origin => {
                        blocked.permissions.extend(permissions);
                        blocked.permissions.sort();
                        blocked.permissions.dedup();
                    }
                    blocked => *blocked = Some(Blocked { load: request.load, origin, permissions }),
                }
                Some(false)
            }
            Verdict::Ask(permissions) => {
                if self.waiting.iter().filter(|waiting| waiting.tab == page.tab).count() >= MAX_WAITING { return Some(false); }
                let Some((origin, top_origin)) = origins(request) else { return Some(false) };
                let question = Question::Permission { id: request.id, profile: profile.to_owned(), origin, top_origin, permissions };
                self.waiting.push(Waiting { tab: page.tab.to_owned(), serial: page.serial, load: request.load, question, remember: true, shown_at: None });
                None
            }
        }
    }

    /// CEF dropped a request itself; its question goes.
    pub fn dismissed(&mut self, serial: u64, id: u64) {
        self.waiting.retain(|waiting| !(waiting.serial == serial && matches!(waiting.question, Question::Permission { id: asked, .. } if asked == id)));
    }

    /// A tab's page opened a link only another app can open. `focused` is
    /// whether the tab is the focused, visible one. Returns a notice to show.
    pub fn link(&mut self, store: &Store, page: Page<'_>, profile: Option<&str>, link: &ExternalLink, focused: bool, now: f64) -> Option<String> {
        let scheme = rules::link_scheme(&link.url)?;
        if let Some(why) = rules::refused_scheme(&scheme) {
            log::info!("Refused a {scheme}: link from a page: it {why}");
            return None;
        }
        let top_origin = rules::origin_of(&link.page);
        let Some(app) = self.apps.handler(&link.url) else {
            log::info!("No app opens {scheme}: links");
            if link.gesture { self.ask(page, link.load, Question::NoApp { top_origin }, false); }
            return None;
        };
        if let Some(why) = rules::refused_handler(&app.bundle_id) {
            log::info!("Refused a {scheme}: link from a page: its app {why}");
            return None;
        }
        if self.apps.own_bundle().is_some_and(|own| own.eq_ignore_ascii_case(&app.bundle_id)) { return None; }
        let profile = profile?;
        // Only the page itself can use "Always allow": not a frame from another site.
        let can_always = top_origin.is_some() && rules::origin_of(&link.frame) == top_origin;
        let always = can_always && top_origin.as_deref().is_some_and(|top| rules::opens_links(store, profile, top, &scheme).unwrap_or(false));
        if link.gesture {
            if always { return self.open(&link.url, &scheme, &app); }
            let question = Question::Link { url: link.url.clone(), scheme, app, profile: profile.to_owned(), top_origin, can_always };
            self.ask(page, link.load, question, false);
            return None;
        }
        let state = self.tabs.entry(page.tab.to_owned()).or_default();
        let since = |at: Option<f64>| at.is_none_or(|at| now - at >= LINK_INTERVAL);
        if always && focused && state.unasked_load != Some(link.load) && since(state.unasked_at) {
            state.unasked_load = Some(link.load);
            state.unasked_at = Some(now);
            return self.open(&link.url, &scheme, &app);
        }
        if !since(state.indicator_at) {
            log::info!("Ignored a {scheme}: link a page opened without a click: it opened one moments ago");
            return None;
        }
        state.indicator_at = Some(now);
        state.indicator = Some(Indicator { load: link.load, url: link.url.clone(), scheme, app, top_origin });
        None
    }

    /// Queue a question; a newer link question replaces the tab's older one.
    fn ask(&mut self, page: Page<'_>, load: u64, question: Question, remember: bool) {
        if !matches!(question, Question::Permission { .. }) {
            self.waiting.retain(|waiting| waiting.tab != page.tab || matches!(waiting.question, Question::Permission { .. }));
        }
        self.waiting.push(Waiting { tab: page.tab.to_owned(), serial: page.serial, load, question, remember, shown_at: None });
    }

    fn open(&mut self, url: &str, scheme: &str, app: &Handler) -> Option<String> {
        if self.apps.open(url) {
            log::info!("Opened a {scheme}: link in another app");
            None
        } else {
            log::warn!("The system didn't open a {scheme}: link");
            Some(format!("Couldn't open {}.", app.name))
        }
    }

    /// The question showing on `tab`: its oldest.
    pub fn question(&self, tab: &str) -> Option<&Waiting> {
        self.waiting.iter().find(|waiting| waiting.tab == tab)
    }

    pub fn question_mut(&mut self, tab: &str) -> Option<&mut Waiting> {
        self.waiting.iter_mut().find(|waiting| waiting.tab == tab)
    }

    /// Answer the question showing on `tab`.
    pub fn choose(&mut self, store: &Store, tab: &str, choice: Choice, now_ms: u64) -> Outcome {
        let Some(index) = self.waiting.iter().position(|waiting| waiting.tab == tab) else { return Outcome::default() };
        let waiting = self.waiting.remove(index);
        let allow = choice == Choice::Allow;
        let remember = waiting.remember && choice != Choice::Dismiss;
        match waiting.question {
            Question::Permission { id, profile, origin, top_origin, permissions } => {
                let reply = Some(Reply { serial: waiting.serial, id, allow });
                let decision = if allow { Decision::Allow } else { Decision::Block };
                let saved = !remember || rules::set_decisions(store, &profile, &origin, &top_origin, &permissions, Some(decision), now_ms).is_ok();
                let notice = (!saved).then(|| format!("Couldn't remember this for {}. It applies to this page only; nothing saved before changed.", site(&origin)));
                Outcome { reply, notice }
            }
            Question::Link { url, scheme, app, profile, top_origin, can_always } if allow => {
                let mut notice = None;
                if remember && can_always && let Some(top) = &top_origin && rules::set_opens_links(store, &profile, top, &scheme, true, now_ms).is_err() {
                    notice = Some(format!("Couldn't remember this for {}. It will ask again next time.", site(top)));
                }
                Outcome { reply: None, notice: self.open(&url, &scheme, &app).or(notice) }
            }
            Question::Link { .. } | Question::NoApp { .. } => Outcome::default(),
        }
    }

    pub fn indicator(&self, tab: &str) -> Option<&Indicator> {
        self.tabs.get(tab)?.indicator.as_ref()
    }

    /// Open the link the indicator names; the click is the user's consent.
    pub fn open_indicator(&mut self, tab: &str) -> Option<String> {
        let indicator = self.tabs.get_mut(tab)?.indicator.take()?;
        self.open(&indicator.url, &indicator.scheme, &indicator.app)
    }

    pub fn dismiss_indicator(&mut self, tab: &str) {
        if let Some(state) = self.tabs.get_mut(tab) { state.indicator = None; }
    }

    /// What a remembered Block refused on `tab`'s page.
    pub fn blocked(&self, tab: &str) -> Option<&Blocked> {
        self.tabs.get(tab)?.blocked.as_ref()
    }

    /// Once a frame, after the views' events. Questions and notices of tabs
    /// that closed or left their page go, and a Space switch answers every
    /// waiting request Don't Allow. Returns those answers.
    pub fn tick(&mut self, pages: &[Page<'_>], space: &str) -> Vec<Reply> {
        let switched = self.space.as_deref().is_some_and(|old| old != space);
        self.space = Some(space.to_owned());
        let current = |tab: &str| pages.iter().find(|page| page.tab == tab).copied();
        let mut replies = Vec::new();
        self.waiting.retain(|waiting| {
            let same_page = current(&waiting.tab).is_some_and(|page| page.serial == waiting.serial && page.load == waiting.load);
            if same_page && !switched { return true; }
            // A closed view answers its own requests; a live one is told.
            if let Question::Permission { id, .. } = waiting.question && current(&waiting.tab).is_some_and(|page| page.serial == waiting.serial) {
                replies.push(Reply { serial: waiting.serial, id, allow: false });
            }
            false
        });
        self.tabs.retain(|tab, state| {
            let Some(page) = current(tab) else { return false };
            if state.blocked.as_ref().is_some_and(|blocked| blocked.load != page.load) { state.blocked = None; }
            if switched || state.indicator.as_ref().is_some_and(|indicator| indicator.load != page.load) { state.indicator = None; }
            true
        });
        replies
    }

    /// A request from Peek or a Little Window, which show no prompts: only
    /// what the profile remembered as allowed is allowed.
    pub fn remembered_only(&mut self, store: &Store, profile: &str, request: &PermissionRequest) -> bool {
        self.declared(request) && verdict(store, profile, request) == Verdict::Allow
    }

    /// Whether this build may use every device `request` asks for. The first
    /// refusal of each says so.
    fn declared(&mut self, request: &PermissionRequest) -> bool {
        let missing: Vec<Permission> = requested(request.media, request.bits).unwrap_or_default().into_iter()
            .filter(|permission| { let keys = usage_keys(*permission); !keys.is_empty() && !keys.iter().any(|key| self.apps.declares(key)) })
            .collect();
        for permission in &missing {
            if self.undeclared_said.contains(permission) { continue; }
            self.undeclared_said.push(*permission);
            log::warn!("Refused {} access: Crossbow's Info.plist doesn't declare it", permission.key());
            self.notices.push(format!("{} access isn't available in this build of Crossbow.", permission.label()));
        }
        missing.is_empty()
    }

    /// Notices for the app to show, once each.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }
}

/// The system's apps through NSWorkspace.
pub struct Native;

impl Apps for Native {
    fn handler(&self, url: &str) -> Option<Handler> {
        #[cfg(target_os = "macos")]
        return macos::handler(url);
        #[cfg(not(target_os = "macos"))]
        { let _ = url; None }
    }

    fn open(&mut self, url: &str) -> bool {
        #[cfg(target_os = "macos")]
        return macos::open(url);
        #[cfg(not(target_os = "macos"))]
        { let _ = url; false }
    }

    fn own_bundle(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        return macos::own_bundle();
        #[cfg(not(target_os = "macos"))]
        None
    }

    fn declares(&self, key: &str) -> bool {
        #[cfg(target_os = "macos")]
        return macos::declares(key);
        #[cfg(not(target_os = "macos"))]
        { let _ = key; true }
    }
}

/// NSWorkspace and NSBundle calls. They cannot run in unit tests.
#[cfg(target_os = "macos")]
mod macos {
    use super::Handler;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};

    pub(super) fn handler(url: &str) -> Option<Handler> {
        let url = NSURL::URLWithString(&NSString::from_str(url))?;
        let app = NSWorkspace::sharedWorkspace().URLForApplicationToOpenURL(&url)?;
        let path = app.path()?;
        // SAFETY: Read-only NSBundle and NSFileManager queries; each returns an object or nil.
        let (bundle_id, name): (Option<Retained<NSString>>, Option<Retained<NSString>>) = unsafe {
            let bundle: Option<Retained<AnyObject>> = msg_send![class!(NSBundle), bundleWithURL: &*app];
            let files: Option<Retained<AnyObject>> = msg_send![class!(NSFileManager), defaultManager];
            (
                bundle.and_then(|bundle| msg_send![&*bundle, bundleIdentifier]),
                files.and_then(|files| msg_send![&*files, displayNameAtPath: &*path]),
            )
        };
        let name = name.map(|name| name.to_string()).unwrap_or_else(|| path.to_string());
        let name = name.rsplit('/').next().unwrap_or_default();
        Some(Handler { name: name.strip_suffix(".app").unwrap_or(name).to_owned(), bundle_id: bundle_id?.to_string() })
    }

    pub(super) fn open(url: &str) -> bool {
        NSURL::URLWithString(&NSString::from_str(url)).is_some_and(|url| NSWorkspace::sharedWorkspace().openURL(&url))
    }

    pub(super) fn own_bundle() -> Option<String> {
        // SAFETY: Read-only NSBundle query; returns an object or nil.
        let id: Option<Retained<NSString>> = unsafe {
            let main: Option<Retained<AnyObject>> = msg_send![class!(NSBundle), mainBundle];
            main.and_then(|bundle| msg_send![&*bundle, bundleIdentifier])
        };
        id.map(|id| id.to_string())
    }

    pub(super) fn declares(key: &str) -> bool {
        // SAFETY: Read-only NSBundle query; returns an object or nil.
        unsafe {
            let main: Option<Retained<AnyObject>> = msg_send![class!(NSBundle), mainBundle];
            let value: Option<Retained<AnyObject>> = main.and_then(|bundle| msg_send![&*bundle, objectForInfoDictionaryKey: &*NSString::from_str(key)]);
            value.is_some()
        }
    }
}

#[cfg(test)]
#[path = "site_permissions_tests.rs"]
pub(crate) mod tests;
