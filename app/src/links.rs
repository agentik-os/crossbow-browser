//! Links from other apps and the command line. The macOS Apple Event handler
//! and launch arguments only queue what they receive; the UI thread opens
//! queued links once the library is loaded and first-run setup is closed.
//! Logs name a link's scheme and host, never its path or query.

use crossbow_core::link_routing::{self, Link};
use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

/// Links that arrive while this many wait are dropped.
const MAX_PENDING: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    AppleEvent,
    LaunchArgument,
}

impl Source {
    fn name(self) -> &'static str {
        match self {
            Source::AppleEvent => "another app",
            Source::LaunchArgument => "the command line",
        }
    }
}

type Waker = Box<dyn Fn() + Send>;

/// Links waiting for the UI thread, oldest first.
pub struct Inbox {
    queue: Mutex<VecDeque<Link>>,
    waker: Mutex<Option<Waker>>,
}

impl Inbox {
    pub const fn new() -> Self {
        Inbox { queue: Mutex::new(VecDeque::new()), waker: Mutex::new(None) }
    }

    /// Check and queue one link, then wake the UI. A refused link is logged
    /// and dropped.
    pub fn deliver(&self, raw: &str, source: Source) -> bool {
        let link = match link_routing::accept(raw) {
            Ok(link) => link,
            Err(refusal) => {
                log::warn!("Ignored a link from {} ({}): {}", source.name(), link_routing::redact(raw), refusal.reason());
                return false;
            }
        };
        {
            let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            if queue.len() >= MAX_PENDING {
                log::warn!("Ignored a link from {} ({}): {MAX_PENDING} links are already waiting", source.name(), link_routing::redact(raw));
                return false;
            }
            queue.push_back(link);
        }
        log::info!("Received a link from {}: {}", source.name(), link_routing::redact(raw));
        self.wake();
        true
    }

    /// Wake the UI after every delivery, and now if links are already waiting.
    pub fn set_waker(&self, waker: impl Fn() + Send + 'static) {
        *self.waker.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(waker));
        let waiting = !self.queue.lock().unwrap_or_else(PoisonError::into_inner).is_empty();
        if waiting {
            self.wake();
        }
    }

    fn wake(&self) {
        if let Some(waker) = &*self.waker.lock().unwrap_or_else(PoisonError::into_inner) {
            waker();
        }
    }

    /// Every waiting link, oldest first. Each is returned once.
    pub fn take(&self) -> Vec<Link> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner).drain(..).collect()
    }
}

static INBOX: Inbox = Inbox::new();

pub fn inbox() -> &'static Inbox {
    &INBOX
}

/// Queue links given as launch arguments. Flags and other arguments are left
/// alone; an argument with a scheme that Crossbow refuses is logged.
pub fn receive_arguments(inbox: &Inbox, args: impl IntoIterator<Item = String>) -> usize {
    args.into_iter()
        .filter(|arg| !arg.starts_with('-') && arg.contains(':'))
        .filter(|arg| inbox.deliver(arg, Source::LaunchArgument))
        .count()
}

/// Start receiving links from other apps. Call on the main thread before
/// the event loop starts, so a link that launches Crossbow is not lost.
pub fn install_native_receiver() {
    #[cfg(target_os = "macos")]
    macos::install();
}

/// Show the app if it is hidden, so a link from another app is visible.
pub fn unhide_app() {
    #[cfg(target_os = "macos")]
    if let Some(main) = objc2::MainThreadMarker::new() {
        let app = objc2_app_kit::NSApplication::sharedApplication(main);
        if app.isHidden() { app.unhide(None); }
    }
}

/// Whether the system opens web links in Crossbow. Read on request only;
/// Crossbow never changes the system default browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefaultBrowser {
    Yes,
    No,
    Unavailable(String),
}

impl DefaultBrowser {
    /// Compare the bundle that opens https links with this app's bundle.
    pub fn from_handlers(default: Option<&str>, own: Option<&str>) -> DefaultBrowser {
        match (own, default) {
            (None, _) => DefaultBrowser::Unavailable("Crossbow isn't running as an app bundle, so the system can't open links in it.".into()),
            (Some(own), Some(default)) if own.eq_ignore_ascii_case(default) => DefaultBrowser::Yes,
            _ => DefaultBrowser::No,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            DefaultBrowser::Yes => "Yes",
            DefaultBrowser::No => "No",
            DefaultBrowser::Unavailable(_) => "Unavailable",
        }
    }
}

/// Ask the system which app opens https links. This only reads.
pub fn default_browser() -> DefaultBrowser {
    #[cfg(target_os = "macos")]
    return macos::default_browser();
    #[cfg(not(target_os = "macos"))]
    DefaultBrowser::Unavailable("Checking the default browser is available on macOS only.".into())
}

/// Open the system's default-browser setting. Opening it changes nothing.
pub fn open_default_browser_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg("x-apple.systempreferences:com.apple.Desktop-Settings.extension").status();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd").args(["/C", "start", "", "ms-settings:defaultapps"]).status();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result: std::io::Result<std::process::ExitStatus> = Err(std::io::ErrorKind::Unsupported.into());
    match result {
        Ok(status) if status.success() => Ok(()),
        _ => Err("Couldn't open system settings. Choose Crossbow as the default web browser there yourself.".into()),
    }
}

/// The Apple Event shim. It cannot run in unit tests: it only reads the URL
/// out of the event and hands it to [`Inbox::deliver`].
#[cfg(target_os = "macos")]
mod macos {
    use super::{DefaultBrowser, INBOX, Source};
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, NSObject};
    use objc2::{MainThreadMarker, MainThreadOnly, class, define_class, msg_send, sel};
    use objc2_app_kit::{NSApplicationWillFinishLaunchingNotification, NSWorkspace};
    use objc2_foundation::{NSNotification, NSNotificationCenter, NSString, NSURL};
    use std::cell::OnceCell;
    use std::ptr::NonNull;

    /// kInternetEventClass and kAEGetURL are both 'GURL'.
    const GET_URL: u32 = u32::from_be_bytes(*b"GURL");
    /// keyDirectObject.
    const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "CrossbowLinkReceiver"]
        struct Receiver;

        impl Receiver {
            #[unsafe(method(handleGetURLEvent:withReplyEvent:))]
            fn handle_get_url(&self, event: Option<&AnyObject>, _reply: Option<&AnyObject>) {
                // SAFETY: AppKit passes an NSAppleEventDescriptor; both selectors
                // are public NSAppleEventDescriptor API and return objects or nil.
                let text: Option<Retained<NSString>> = event.and_then(|event| unsafe {
                    let descriptor: Option<Retained<AnyObject>> = msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT];
                    descriptor.and_then(|descriptor| msg_send![&*descriptor, stringValue])
                });
                match text {
                    Some(text) => {
                        INBOX.deliver(&text.to_string(), Source::AppleEvent);
                    }
                    None => log::warn!("Ignored a link from another app: the event carried no URL"),
                }
            }
        }
    );

    thread_local! {
        /// NSAppleEventManager does not retain its handler.
        static RECEIVER: OnceCell<Retained<Receiver>> = const { OnceCell::new() };
    }

    fn register(main: MainThreadMarker) {
        let receiver = RECEIVER.with(|cell| cell.get_or_init(|| {
            let this = Receiver::alloc(main).set_ivars(());
            // SAFETY: NSObject's designated initializer, on the main thread.
            unsafe { msg_send![super(this), init] }
        }).clone());
        let Some(manager) = AnyClass::get(c"NSAppleEventManager") else {
            log::warn!("Links from other apps are unavailable: NSAppleEventManager is missing");
            return;
        };
        // SAFETY: Public NSAppleEventManager API; the handler stays alive in RECEIVER.
        unsafe {
            let manager: Option<Retained<AnyObject>> = msg_send![manager, sharedAppleEventManager];
            let Some(manager) = manager else { return };
            let _: () = msg_send![&*manager, setEventHandler: &*receiver, andSelector: sel!(handleGetURLEvent:withReplyEvent:), forEventClass: GET_URL, andEventID: GET_URL];
        }
    }

    pub(super) fn install() {
        let Some(main) = MainThreadMarker::new() else {
            log::warn!("Links from other apps are unavailable: not on the main thread");
            return;
        };
        register(main);
        // Register again as AppKit finishes launching, before it handles the
        // event that launched Crossbow, in case it replaced the handler.
        let again = RcBlock::new(|_: NonNull<NSNotification>| {
            if let Some(main) = MainThreadMarker::new() {
                register(main);
            }
        });
        // SAFETY: A public notification name; the block captures nothing.
        let token = unsafe {
            NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(NSApplicationWillFinishLaunchingNotification),
                None,
                None,
                &again,
            )
        };
        // Observe for the life of the app.
        std::mem::forget(token);
    }

    pub(super) fn default_browser() -> DefaultBrowser {
        let Some(probe) = NSURL::URLWithString(&NSString::from_str("https://example.com/")) else {
            return DefaultBrowser::Unavailable("The system couldn't be asked about the default browser.".into());
        };
        let handler = NSWorkspace::sharedWorkspace().URLForApplicationToOpenURL(&probe);
        // SAFETY: Read-only NSBundle queries; each returns an object or nil.
        let (default, own): (Option<Retained<NSString>>, Option<Retained<NSString>>) = unsafe {
            let other: Option<Retained<AnyObject>> = match &handler {
                Some(handler) => msg_send![class!(NSBundle), bundleWithURL: &**handler],
                None => None,
            };
            let main: Option<Retained<AnyObject>> = msg_send![class!(NSBundle), mainBundle];
            (
                other.and_then(|bundle| msg_send![&*bundle, bundleIdentifier]),
                main.and_then(|bundle| msg_send![&*bundle, bundleIdentifier]),
            )
        };
        DefaultBrowser::from_handlers(default.map(|id| id.to_string()).as_deref(), own.map(|id| id.to_string()).as_deref())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn pages(links: Vec<Link>) -> Vec<String> {
        links.into_iter().map(|link| match link {
            Link::Page(url) => url,
            Link::App => "crossbow".into(),
        }).collect()
    }

    #[test]
    fn accepted_links_queue_in_order_and_drain_once() {
        let inbox = Inbox::new();
        assert!(inbox.deliver("https://example.com/a?q=1", Source::AppleEvent));
        assert!(!inbox.deliver("mailto:someone@example.com", Source::AppleEvent));
        assert!(!inbox.deliver("javascript:alert(1)", Source::AppleEvent));
        assert!(inbox.deliver("crossbow://", Source::AppleEvent));
        assert!(!inbox.deliver("file:///etc/hosts", Source::AppleEvent));
        assert!(inbox.deliver("file:///tmp/page.html", Source::AppleEvent));
        assert!(inbox.deliver("http://127.0.0.1:8080/b", Source::LaunchArgument));
        assert_eq!(pages(inbox.take()), ["https://example.com/a?q=1", "crossbow", "file:///tmp/page.html", "http://127.0.0.1:8080/b"]);
        assert!(inbox.take().is_empty(), "each link is handed over once");
    }

    #[test]
    fn links_that_arrive_before_the_app_exists_wake_it_once_it_does() {
        let inbox = Inbox::new();
        inbox.deliver("https://early.example/", Source::AppleEvent);
        inbox.deliver("https://second.example/", Source::AppleEvent);
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = wakes.clone();
        inbox.set_waker(move || { counter.fetch_add(1, Ordering::SeqCst); });
        assert_eq!(wakes.load(Ordering::SeqCst), 1, "waiting links wake the new UI");
        inbox.deliver("https://later.example/", Source::AppleEvent);
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        inbox.deliver("ftp://refused.example/", Source::AppleEvent);
        assert_eq!(wakes.load(Ordering::SeqCst), 2, "a refused link wakes nothing");
        assert_eq!(pages(inbox.take()), ["https://early.example/", "https://second.example/", "https://later.example/"]);
        let empty = Inbox::new();
        let counter = wakes.clone();
        empty.set_waker(move || { counter.fetch_add(1, Ordering::SeqCst); });
        assert_eq!(wakes.load(Ordering::SeqCst), 2, "nothing waiting, no wake");
    }

    #[test]
    fn launch_arguments_queue_only_links() {
        let inbox = Inbox::new();
        let args = ["--remote-debugging-port=9222", "-psn_0_12345", "https://example.com/x", "notes.txt", "mailto:a@example.com", "file:///tmp/Index.HTML", "C:\\Users\\someone"];
        assert_eq!(receive_arguments(&inbox, args.map(String::from)), 2);
        assert_eq!(pages(inbox.take()), ["https://example.com/x", "file:///tmp/Index.HTML"]);
    }

    #[test]
    fn a_flood_of_links_is_bounded() {
        let inbox = Inbox::new();
        let accepted = (0..MAX_PENDING + 10).filter(|n| inbox.deliver(&format!("https://example.com/{n}"), Source::AppleEvent)).count();
        assert_eq!(accepted, MAX_PENDING);
        let taken = pages(inbox.take());
        assert_eq!(taken.first().map(String::as_str), Some("https://example.com/0"), "the oldest are kept");
        assert!(inbox.deliver("https://example.com/after", Source::AppleEvent), "draining makes room");
    }

    #[test]
    fn default_browser_status_compares_bundle_identifiers() {
        assert_eq!(DefaultBrowser::from_handlers(Some("app.crossbow.Crossbow"), Some("app.crossbow.Crossbow")), DefaultBrowser::Yes);
        assert_eq!(DefaultBrowser::from_handlers(Some("APP.crossbow.crossbow"), Some("app.crossbow.Crossbow")), DefaultBrowser::Yes);
        assert_eq!(DefaultBrowser::from_handlers(Some("com.apple.Safari"), Some("app.crossbow.Crossbow")), DefaultBrowser::No);
        assert_eq!(DefaultBrowser::from_handlers(None, Some("app.crossbow.Crossbow")), DefaultBrowser::No);
        assert!(matches!(DefaultBrowser::from_handlers(Some("com.apple.Safari"), None), DefaultBrowser::Unavailable(reason) if !reason.is_empty()));
        assert_eq!([DefaultBrowser::Yes.label(), DefaultBrowser::No.label(), DefaultBrowser::Unavailable(String::new()).label()], ["Yes", "No", "Unavailable"]);
    }
}
