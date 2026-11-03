//! Crossbow's web engine layer.
//!
//! Chromium is embedded through CEF in windowless (off-screen) mode: every tab
//! renders into a GPU texture that the UI composites like any other image. That
//! is what lets Crossbow animate, round, scale and overlay web content freely
//! (split view, Peek, the command bar) on macOS, Windows and Linux alike.
//!
//! Threading: CEF runs with an external message pump on the main thread. The UI
//! calls [`pump`] every frame, and CEF asks for extra frames through the waker
//! installed with [`set_waker`].

mod appearance;
mod browsing_data;
mod content_rules;
mod downloads;
mod hiding;
mod input;
mod login;
mod media;
mod page_fetch;
pub mod permissions;
mod profile_path;
mod repaint;
mod screen;
mod texture;
mod translate;
mod view;

pub use appearance::Appearance;
pub use browsing_data::{ClearDataStatus, ClearDataStep, ClearDataTask, clear_browsing_data};
pub use content_rules::{ContentRules, HidingSheet, ResourceKind, ResourceRequest, has_content_rules, set_content_rules};
pub use downloads::{DownloadCommand, DownloadControl, DownloadControlStatus, DownloadInfo, DownloadPending, DownloadRequest};
pub use input::{KeyInput, Modifiers, MouseButton, NamedKey};
pub use page_fetch::{BODY_BYTES as PREVIEW_BODY_BYTES, FetchOutcome, PageFetch, fetch_page};
pub use media::{CssRect, MAX_VIDEOS, MediaCommand, MediaReport, PixelRect, Video, parse_report};
pub use login::{ADDRESS_PARTS, CARD_PARTS, FillTarget, FOLLOW_WINDOW, FillReport, LoginCapture, MAX_ADDRESS_PART_BYTES, MAX_PASSWORD_BYTES, MAX_USERNAME_BYTES, PageAddress, Secret, login_capture, page_origin, set_login_capture};
pub use permissions::{ExternalLink, PermissionRequest};
pub use profile_path::{DetachedProfileData, has_profile_data};
pub use screen::{ScreenGeometry, ScreenRect, ViewPosition};
pub use texture::WebTexture;
pub use translate::{MAX_CHARS as MAX_TRANSLATE_CHARS, MAX_SAMPLE as MAX_TRANSLATE_SAMPLE, MAX_SEGMENTS as MAX_TRANSLATE_SEGMENTS, Earlier, Segment, TranslateCommand, TranslateReport};
pub use view::{
    ContextMenuInfo, CursorKind, NavState, PageText, WebEvent, WebView, WebViewConfig,
};

use cef::{args::Args, *};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

static WAKER: OnceLock<repaint::RepaintWaker> = OnceLock::new();
static ROOT_CACHE_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Install the UI repaint sink. It runs on a dedicated dispatcher, never inside
/// CEF callbacks. Delays are handled before dispatch, so the argument is zero.
/// The main thread acknowledges each coalesced notification in [`pump`].
pub fn set_waker(waker: impl Fn(Duration) + Send + Sync + 'static) {
    WAKER.get_or_init(|| repaint::RepaintWaker::new(move || waker(Duration::ZERO)));
}

fn wake(after: Duration) {
    if let Some(w) = WAKER.get() {
        w.wake(after);
    }
}

fn stop_waker() {
    if let Some(w) = WAKER.get() {
        w.stop();
    }
}

/// Outcome of [`start`].
pub enum Startup {
    /// This is a CEF sub-process (renderer, GPU, …) that already ran; exit with the code.
    SubProcess(i32),
    /// This is the browser process; CEF is initialised.
    Browser(EngineGuard),
}

/// Keeps the CEF framework loaded (macOS) and shuts CEF down on drop.
pub struct EngineGuard {
    #[cfg(target_os = "macos")]
    _loader: library_loader::LibraryLoader,
}

impl Drop for EngineGuard {
    fn drop(&mut self) {
        stop_waker();
        cef::shutdown();
    }
}

pub struct EngineOptions {
    /// Root of all Chromium profile data (cookies, cache, storage).
    pub data_dir: PathBuf,
    pub locale: String,
    /// Opens Chromium's remote debugging port (development only).
    pub remote_debugging_port: Option<u16>,
}

/// Initialise CEF. Must run first in `main`, before any window or event loop.
pub fn start(opts: &EngineOptions) -> Startup {
    #[cfg(target_os = "macos")]
    let loader = {
        let loader = library_loader::LibraryLoader::new(&std::env::current_exe().unwrap(), false);
        assert!(loader.load(), "could not load the Chromium Embedded Framework");
        loader
    };

    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let args = Args::new();
    let cmd = args.as_cmd_line().expect("command line");
    let type_switch = CefString::from("type");
    let is_browser = cmd.has_switch(Some(&type_switch)) != 1;

    let mut app = AppBuilder::build(CrossbowApp);
    let code = execute_process(Some(args.as_main_args()), Some(&mut app), std::ptr::null_mut());
    if !is_browser {
        return Startup::SubProcess(code.max(0));
    }

    let root = profile_path::prepare_root(&opts.data_dir)
        .expect("could not prepare persistent Chromium storage");
    let settings = Settings {
        windowless_rendering_enabled: 1,
        external_message_pump: 1,
        root_cache_path: CefString::from(root.to_str().expect("validated Chromium root")),
        cache_path: CefString::from(root.join("Default").to_str().expect("validated Chromium root")),
        persist_session_cookies: 1,
        locale: CefString::from(opts.locale.as_str()),
        log_severity: LogSeverity::WARNING,
        background_color: 0xFFFF_FFFF,
        remote_debugging_port: opts.remote_debugging_port.map(i32::from).unwrap_or(0),
        ..Default::default()
    };
    let ok = initialize(Some(args.as_main_args()), Some(&settings), Some(&mut app), std::ptr::null_mut());
    assert_eq!(ok, 1, "Chromium failed to initialise");
    ROOT_CACHE_PATH.set(root).expect("Chromium was already initialised");
    Startup::Browser(EngineGuard {
        #[cfg(target_os = "macos")]
        _loader: loader,
    })
}

/// Entry point for the helper executable (macOS sub-processes).
pub fn run_helper() -> i32 {
    let args = Args::new();
    #[cfg(all(target_os = "macos", feature = "sandbox"))]
    let _sandbox = {
        let mut sandbox = cef::sandbox::Sandbox::new();
        sandbox.initialize(args.as_main_args());
        sandbox
    };
    #[cfg(target_os = "macos")]
    let _loader = {
        let loader = library_loader::LibraryLoader::new(&std::env::current_exe().unwrap(), true);
        assert!(loader.load());
        loader
    };
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let mut app = AppBuilder::build(CrossbowApp);
    execute_process(Some(args.as_main_args()), Some(&mut app), std::ptr::null_mut())
}

/// Run pending CEF work. Call once per UI frame on the main thread.
pub fn pump() {
    if let Some(w) = WAKER.get() {
        w.begin_pump();
    }
    do_message_loop_work();
}

/// Pump for a while after the UI exits so closing browsers can finish before shutdown.
pub fn drain(duration: Duration) {
    stop_waker();
    let end = std::time::Instant::now() + duration;
    while std::time::Instant::now() < end {
        do_message_loop_work();
        std::thread::sleep(Duration::from_millis(5));
    }
}

thread_local! {
    static CONTEXTS: RefCell<HashMap<PathBuf, RequestContext>> = RefCell::new(HashMap::new());
}

/// Browsing-data context for a Crossbow profile: separate cookies, storage and
/// cache under `<data_dir>/Chromium/Profile-<hex-id>`. Legacy stores are untouched.
/// Returns `None` on invalid/unusable storage; callers must not use a global fallback.
pub fn profile_context(data_dir: &Path, profile_id: &str) -> Option<RequestContext> {
    let root = ROOT_CACHE_PATH.get()?;
    let path = match profile_path::prepare_profile_path(data_dir, root, profile_id) {
        Ok(path) => path,
        Err(error) => {
            log::error!("Refusing profile context: {error}");
            return None;
        }
    };
    CONTEXTS.with_borrow_mut(|map| {
        if let Some(c) = map.get(&path) {
            return Some(c.clone());
        }
        let settings = RequestContextSettings {
            cache_path: CefString::from(path.to_str().expect("validated profile path")),
            persist_session_cookies: 1,
            ..Default::default()
        };
        let mut handler = appearance::ContextAppearance::new(profile_id.to_owned());
        let ctx = request_context_create_context(Some(&settings), Some(&mut handler))?;
        if Path::new(&CefString::from(&ctx.cache_path()).to_string()) != path {
            log::error!("Refusing profile context: CEF did not retain the requested storage path");
            return None;
        }
        map.insert(path, ctx.clone());
        Some(ctx)
    })
}

/// Ask `profile_id`'s pages for `appearance`: its open pages now, where
/// Chromium updates them, and every page it opens later.
pub fn set_website_appearance(profile_id: &str, appearance: Appearance) {
    appearance::remember(profile_id, appearance);
    let Some(root) = ROOT_CACHE_PATH.get() else { return };
    let Ok(name) = profile_path::profile_directory_name(profile_id) else { return };
    // A context still initializing applies the newest choice from its handler.
    if let Some(context) = CONTEXTS.with_borrow(|map| map.get(&root.join(name)).cloned()) {
        appearance::apply(&context, appearance);
    }
}

/// Move a deleted profile's Chromium folder aside once its browsers are closed,
/// and forget its cached context so no new browser reuses that storage.
/// Ok(None) when the profile never stored data on this device.
pub fn detach_profile_data(data_dir: &Path, profile_id: &str) -> std::io::Result<Option<DetachedProfileData>> {
    let detached = profile_path::detach_profile_data(data_dir, profile_id, ROOT_CACHE_PATH.get().map(PathBuf::as_path))?;
    if let Some(detached) = &detached {
        CONTEXTS.with_borrow_mut(|map| map.remove(detached.original()));
    }
    Ok(detached)
}

#[derive(Clone)]
struct CrossbowApp;

wrap_app! {
    struct AppBuilder {
        app: CrossbowApp,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(cl) = command_line else { return };
            cl.append_switch(Some(&"no-startup-window".into()));
            cl.append_switch(Some(&"noerrdialogs".into()));
            cl.append_switch(Some(&"hide-crash-restore-bubble".into()));
            cl.append_switch(Some(&"enable-smooth-scrolling".into()));
            // Merged into any list already given: Chromium reads only the last one.
            let name = CefString::from(media::DISABLE_BLINK_FEATURES);
            let existing = CefString::from(&cl.switch_value(Some(&name))).to_string();
            let merged = media::merge_features(&existing, media::DISABLED_BLINK_FEATURES);
            cl.append_switch_with_value(Some(&name), Some(&CefString::from(merged.as_str())));
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(ProcessHandlerBuilder::build(ProcessHandler))
        }

        /// Used only in renderers: the login listener and fills, and the
        /// video reporter.
        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(login::RendererBuilder::new())
        }
    }
}

impl AppBuilder {
    fn build(app: CrossbowApp) -> App {
        Self::new(app)
    }
}

#[derive(Clone)]
struct ProcessHandler;

wrap_browser_process_handler! {
    struct ProcessHandlerBuilder {
        handler: ProcessHandler,
    }

    impl BrowserProcessHandler {
        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            if let Some(w) = WAKER.get() {
                w.schedule_pump(Duration::from_millis(delay_ms.max(0) as u64));
            }
        }

        /// A renderer starts with the page capture setting as it is now, which
        /// a tab opened before a change can't carry in its `extra_info`.
        fn on_before_child_process_launch(&self, command_line: Option<&mut CommandLine>) {
            let Some(line) = command_line else { return };
            if CefString::from(&line.switch_value(Some(&"type".into()))).to_string() != "renderer" { return; }
            line.append_switch_with_value(Some(&login::SWITCH.into()), Some(&login::launch_switch().as_str().into()));
        }
    }
}

impl ProcessHandlerBuilder {
    fn build(h: ProcessHandler) -> BrowserProcessHandler {
        Self::new(h)
    }
}

/// CEF build string, e.g. `154.0.34+g14c5a08+chromium-154.0.8037.98`.
pub fn engine_version() -> &'static str {
    let v: &[u8] = cef::sys::CEF_VERSION;
    std::str::from_utf8(v.strip_suffix(&[0]).unwrap_or(v)).unwrap_or("")
}

/// Loads CEF's string utilities for tests, once per process; CEF itself is
/// never initialized and no browser opens.
#[cfg(test)]
pub(crate) fn load_cef_strings() {
    static LOAD_CEF: std::sync::Once = std::sync::Once::new();
    LOAD_CEF.call_once(|| {
        use std::os::unix::ffi::OsStrExt;
        let path = sys::get_cef_dir().expect("CEF_PATH must identify the pinned CEF distribution")
            .join(sys::FRAMEWORK_PATH).canonicalize().expect("CEF framework is required");
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { sys::cef_load_library(path.as_ptr()) }, 1);
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn cef_wake_returns_while_repaint_waits_for_ui_lock() {
        use cef::ImplBrowserProcessHandler;
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::Duration;

        let ui_lock = Arc::new(Mutex::new(()));
        let held = ui_lock.lock().unwrap();
        let (painted_tx, painted_rx) = mpsc::channel();
        let sink_lock = Arc::clone(&ui_lock);
        super::set_waker(move |_| {
            let _held = sink_lock.lock().unwrap();
            let _ = painted_tx.send(());
        });
        let (returned_tx, returned_rx) = mpsc::channel();
        let cef_callback = std::thread::spawn(move || {
            let handler = super::ProcessHandlerBuilder::new(super::ProcessHandler);
            handler.on_schedule_message_pump_work(0);
            returned_tx.send(()).unwrap();
        });
        let returned_without_ui_lock = returned_rx.recv_timeout(Duration::from_secs(1));
        drop(held);
        cef_callback.join().unwrap();
        painted_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(returned_without_ui_lock.is_ok(), "CEF callback waited for the UI lock");
    }

    #[test]
    fn profile_context_requires_an_initialized_engine() {
        assert!(super::ROOT_CACHE_PATH.get().is_none());
        assert!(super::profile_context(std::path::Path::new("unused"), "profile-a").is_none());
    }
}
