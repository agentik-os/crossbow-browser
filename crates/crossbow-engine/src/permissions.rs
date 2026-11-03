//! Permission requests and links to other apps, in the browser process.
//!
//! CEF's own handling, which Crossbow replaces: every view is windowless, so
//! Alloy style, where a media request (camera, microphone, screen) is denied
//! and any other permission prompt is ignored, leaving the page's promise
//! unresolved. A link to an unknown scheme is never handed to the system
//! unless `OnProtocolExecution` sets `allow_os_execution`, which Crossbow
//! never does: links to other apps are cancelled in `OnBeforeBrowse` and
//! `OnBeforePopup` and handed to the app, which asks first.
//!
//! Every request is answered exactly once: by the app, by a navigation or
//! close of its page, or by CEF dismissing it. Nothing here logs an origin
//! or a link.

use cef::*;

/// CEF's media bits (`cef_media_access_permission_types_t`), for a request
/// made with [`PermissionRequest::media`].
pub mod media {
    pub const MICROPHONE: u32 = 1 << 0;
    pub const CAMERA: u32 = 1 << 1;
    pub const SCREEN_AUDIO: u32 = 1 << 2;
    pub const SCREEN_VIDEO: u32 = 1 << 3;
}

/// CEF's prompt bits (`cef_permission_request_types_t`).
pub mod prompt {
    pub const AR_SESSION: u32 = 1 << 0;
    pub const CAMERA_PAN_TILT_ZOOM: u32 = 1 << 1;
    pub const CAMERA_STREAM: u32 = 1 << 2;
    pub const CAPTURED_SURFACE_CONTROL: u32 = 1 << 3;
    pub const CLIPBOARD: u32 = 1 << 4;
    pub const TOP_LEVEL_STORAGE_ACCESS: u32 = 1 << 5;
    pub const DISK_QUOTA: u32 = 1 << 6;
    pub const LOCAL_FONTS: u32 = 1 << 7;
    pub const GEOLOCATION: u32 = 1 << 8;
    pub const HAND_TRACKING: u32 = 1 << 9;
    pub const IDENTITY_PROVIDER: u32 = 1 << 10;
    pub const IDLE_DETECTION: u32 = 1 << 11;
    pub const MIC_STREAM: u32 = 1 << 12;
    pub const MIDI_SYSEX: u32 = 1 << 13;
    pub const MULTIPLE_DOWNLOADS: u32 = 1 << 14;
    pub const NOTIFICATIONS: u32 = 1 << 15;
    pub const KEYBOARD_LOCK: u32 = 1 << 16;
    pub const POINTER_LOCK: u32 = 1 << 17;
    pub const PROTECTED_MEDIA_IDENTIFIER: u32 = 1 << 18;
    pub const REGISTER_PROTOCOL_HANDLER: u32 = 1 << 19;
    pub const STORAGE_ACCESS: u32 = 1 << 20;
    pub const VR_SESSION: u32 = 1 << 21;
    pub const WEB_APP_INSTALLATION: u32 = 1 << 22;
    pub const WINDOW_MANAGEMENT: u32 = 1 << 23;
    pub const FILE_SYSTEM_ACCESS: u32 = 1 << 24;
    pub const LOCAL_NETWORK_ACCESS: u32 = 1 << 25;
    pub const LOCAL_NETWORK: u32 = 1 << 26;
    pub const LOOPBACK_NETWORK: u32 = 1 << 27;
    pub const SENSORS: u32 = 1 << 28;
}

/// A page asked for permissions. Answer it with
/// [`crate::WebView::answer_permission`]; until then the page waits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionRequest {
    /// Distinct within its view.
    pub id: u64,
    /// A camera, microphone or screen request (`media` bits), or any other
    /// prompt (`prompt` bits).
    pub media: bool,
    pub bits: u32,
    /// The origin that asked, as Chromium gives it.
    pub origin: String,
    /// The main frame's address when it asked.
    pub page: String,
    /// Which page load of the view asked; see [`crate::WebView::page_loads`].
    pub load: u64,
}

/// A link a page opened that only another app can open. Its `Debug` shows
/// the scheme only, since a link can carry tokens.
#[derive(Clone, PartialEq, Eq)]
pub struct ExternalLink {
    pub url: String,
    /// The page opened it from a click or key press, as Chromium reports.
    pub gesture: bool,
    /// The main frame's address when it was opened.
    pub page: String,
    /// The address of the frame that opened it: the page itself, or a frame
    /// embedded in it.
    pub frame: String,
    /// Which page load of the view it came from; see [`crate::WebView::page_loads`].
    pub load: u64,
}

impl std::fmt::Debug for ExternalLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let scheme = self.url.split_once(':').map_or("", |(scheme, _)| scheme);
        f.debug_struct("ExternalLink").field("scheme", &scheme).field("gesture", &self.gesture).field("load", &self.load).finish_non_exhaustive()
    }
}

/// Schemes Chromium loads itself. A navigation to any other scheme is a
/// link to another app.
const LOADED_HERE: [&str; 13] = ["http", "https", "file", "data", "blob", "about", "javascript", "filesystem", "chrome", "chrome-untrusted", "chrome-error", "devtools", "view-source"];

/// Whether `url` can only be opened by another app.
pub(crate) fn is_external(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else { return false };
    let mut chars = scheme.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid && !LOADED_HERE.iter().any(|loaded| scheme.eq_ignore_ascii_case(loaded))
}

/// How a held request is answered.
pub(crate) trait Respond {
    fn respond(&self, allow: bool);
}

/// A CEF callback, held until the app answers.
pub(crate) enum Callback {
    /// Allowing grants exactly the permissions asked, as CEF requires for
    /// getUserMedia; denying grants none.
    Media { callback: MediaAccessCallback, required: u32 },
    Prompt { callback: PermissionPromptCallback },
}

impl Respond for Callback {
    fn respond(&self, allow: bool) {
        match self {
            Callback::Media { callback, required } => callback.cont(if allow { *required } else { 0 }),
            Callback::Prompt { callback } => callback.cont(if allow { PermissionRequestResult::ACCEPT } else { PermissionRequestResult::DENY }),
        }
    }
}

struct Held<C> {
    id: u64,
    /// CEF's id for a prompt, which it may dismiss on its own.
    prompt: Option<u64>,
    callback: C,
}

/// One view's requests waiting for the app.
pub(crate) struct Pending<C> {
    next: u64,
    held: Vec<Held<C>>,
}

impl<C> Default for Pending<C> {
    fn default() -> Self {
        Pending { next: 1, held: Vec::new() }
    }
}

impl<C> Pending<C> {
    /// Hold a request; returns the id the app answers it by.
    pub(crate) fn hold(&mut self, prompt: Option<u64>, callback: C) -> u64 {
        let id = self.next;
        self.next += 1;
        self.held.push(Held { id, prompt, callback });
        id
    }

    /// CEF closed prompt `prompt`, after an answer or on its own; no answer
    /// is due. Returns the app's id if it was still waiting.
    pub(crate) fn dismissed(&mut self, prompt: u64) -> Option<u64> {
        let index = self.held.iter().position(|held| held.prompt == Some(prompt))?;
        Some(self.held.remove(index).id)
    }

    fn take(&mut self, id: u64) -> Option<C> {
        let index = self.held.iter().position(|held| held.id == id)?;
        Some(self.held.remove(index).callback)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.held.len()
    }
}

/// Answer request `id`. False if it was already answered or dismissed. The
/// callback runs once `pending` is released, since CEF calls back into the
/// view (`OnDismissPermissionPrompt` follows every answer).
pub(crate) fn answer<C: Respond>(pending: &std::cell::RefCell<Pending<C>>, id: u64, allow: bool) -> bool {
    let callback = pending.borrow_mut().take(id);
    callback.map(|callback| callback.respond(allow)).is_some()
}

/// Deny everything held, as when the page navigates or closes. Returns the
/// ids, for the app to drop.
pub(crate) fn deny_all<C: Respond>(pending: &std::cell::RefCell<Pending<C>>) -> Vec<u64> {
    let held = std::mem::take(&mut pending.borrow_mut().held);
    held.into_iter().map(|held| {
        held.callback.respond(false);
        held.id
    }).collect()
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
