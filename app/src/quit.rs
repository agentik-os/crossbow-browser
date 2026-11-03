//! Warn Before Quitting: a device-local preference, off by default. When on,
//! ⌘Q, Quit in the app menu (or the Dock) and closing the last window ask
//! "Quit Crossbow?" first. Logging out, restarting and shutting down are
//! never held up. Stored in the store's key/value table, so it never syncs.

use crossbow_core::store::Store;

const KEY: &str = "warn_before_quitting";

/// How a quit was asked for; confirming replays the same route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// ⌘Q, Quit in the app menu or the Dock: AppKit's `terminate:`.
    Quit,
    /// Closing the last (root) window.
    CloseWindow,
}

/// A change to a preference that could not be saved.
#[derive(Debug)]
pub struct SaveFailure<T = bool> {
    pub wanted: T,
    pub error: String,
    pub reveal: bool,
}

#[derive(Debug, Default)]
pub struct QuitGuard {
    enabled: bool,
    /// The route waiting on the confirmation.
    asking: Option<Route>,
    /// The confirmation said Quit: the next request goes through.
    confirmed: bool,
    /// Bring the window forward so the confirmation can be seen.
    reveal: bool,
    /// Why the saved preference was not read; cleared by the next save.
    pub notice: Option<String>,
    pub failure: Option<SaveFailure>,
    /// Why ⌘Q and the app menu's Quit cannot ask first on this system.
    pub unavailable: Option<String>,
}

impl QuitGuard {
    pub fn load(store: &Store) -> Self {
        const UNREADABLE: &str = "The saved quit warning could not be read, so Crossbow quits without asking. Changing it saves over it.";
        let (enabled, notice) = match store.kv_get(KEY) {
            Ok(None) => (false, None),
            Ok(Some(raw)) if raw == "on" => (true, None),
            Ok(Some(raw)) if raw == "off" => (false, None),
            Ok(Some(_)) | Err(_) => (false, Some(UNREADABLE.to_string())),
        };
        QuitGuard { enabled, notice, ..QuitGuard::default() }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Save this device's choice. On failure the previous one stays in use
    /// and `failure` offers it again.
    pub fn choose(&mut self, store: &Store, enabled: bool) {
        match store.kv_set(KEY, if enabled { "on" } else { "off" }) {
            Ok(()) => {
                self.enabled = enabled;
                self.notice = None;
                self.failure = None;
            }
            Err(error) => self.failure = Some(SaveFailure { wanted: enabled, error: error.to_string(), reveal: true }),
        }
    }

    /// Whether a quit by `route` may go ahead now; otherwise the confirmation
    /// is showing. A second Quit while it shows goes ahead; closing the window
    /// again keeps it.
    pub fn request(&mut self, route: Route) -> bool {
        if !self.enabled || std::mem::take(&mut self.confirmed) {
            self.asking = None;
            return true;
        }
        match self.asking {
            Some(_) if route == Route::Quit => {
                self.asking = None;
                true
            }
            Some(_) => false,
            None => {
                self.asking = Some(route);
                self.reveal = route == Route::Quit;
                false
            }
        }
    }

    pub fn asking(&self) -> bool {
        self.asking.is_some()
    }

    /// Quit (or Enter): the route to replay, which then goes through once.
    pub fn confirm(&mut self) -> Option<Route> {
        let route = self.asking.take()?;
        self.confirmed = true;
        Some(route)
    }

    /// The user already chose to quit, as Restore and Quit does: the next quit
    /// goes through without asking again.
    pub fn allow_next(&mut self) {
        self.asking = None;
        self.confirmed = true;
    }

    /// Cancel (or Escape): everything stays open.
    pub fn cancel(&mut self) {
        self.asking = None;
        self.reveal = false;
    }

    /// Whether the window should come forward for a confirmation it can't show hidden.
    pub fn take_reveal(&mut self) -> bool {
        std::mem::take(&mut self.reveal)
    }
}

/// Before the frame is drawn: closing the last window waits for the answer,
/// and a quit from the app menu or Dock brings the window forward to ask.
pub fn on_close_request(guard: &mut QuitGuard, ctx: &egui::Context) {
    if ctx.input(|input| input.viewport().close_requested()) && !guard.request(Route::CloseWindow) {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
    }
    if guard.take_reveal() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }
}

/// The confirmation's answer: Cancel keeps everything open, Quit replays the
/// route that asked.
pub fn answer(guard: &mut QuitGuard, ctx: &egui::Context, quit: bool) {
    if !quit {
        guard.cancel();
        return;
    }
    match guard.confirm() {
        Some(Route::Quit) => terminate(ctx),
        Some(Route::CloseWindow) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        None => {}
    }
}

/// A quit Apple Event carries a quit reason only when the session is ending
/// (log out, restart, shut down); those are never held up.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
fn session_ending(quit_reason: Option<u32>) -> bool {
    quit_reason.is_some()
}

/// Ask the guard before AppKit's `terminate:` quits: ⌘Q, Quit in the app menu
/// and the Dock all arrive there. Returns why it could not be installed.
#[cfg(target_os = "macos")]
pub fn install(guard: &std::rc::Rc<std::cell::RefCell<QuitGuard>>, ctx: &egui::Context) -> Result<(), String> {
    macos::install(guard, ctx)
}

#[cfg(not(target_os = "macos"))]
pub fn install(_guard: &std::rc::Rc<std::cell::RefCell<QuitGuard>>, _ctx: &egui::Context) -> Result<(), String> {
    Ok(())
}

/// Quit the way ⌘Q does, after the current frame.
#[cfg(target_os = "macos")]
pub fn terminate(_ctx: &egui::Context) {
    macos::terminate();
}

#[cfg(not(target_os = "macos"))]
pub fn terminate(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{QuitGuard, Route, session_ending};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{MainThreadMarker, class, msg_send, sel};
    use objc2_app_kit::NSApplication;
    use std::cell::RefCell;
    use std::rc::{Rc, Weak};

    const TERMINATE_CANCEL: usize = 0;
    const TERMINATE_NOW: usize = 1;
    /// keyAEQuitReason, 'why?'.
    const QUIT_REASON: u32 = u32::from_be_bytes(*b"why?");

    thread_local! {
        static GUARD: RefCell<Option<(Weak<RefCell<QuitGuard>>, egui::Context)>> = const { RefCell::new(None) };
    }

    /// `-applicationShouldTerminate:` added to winit's application delegate.
    extern "C-unwind" fn should_terminate(_this: &AnyObject, _cmd: Sel, _sender: *mut AnyObject) -> usize {
        if session_ending(current_quit_reason()) {
            return TERMINATE_NOW;
        }
        let asked = GUARD.with(|slot| {
            let slot = slot.borrow();
            let (guard, ctx) = slot.as_ref()?;
            let guard = guard.upgrade()?;
            // Never hold up a quit the guard is too busy to answer.
            let mut guard = guard.try_borrow_mut().ok()?;
            let allowed = guard.request(Route::Quit);
            Some((allowed, ctx.clone()))
        });
        match asked {
            Some((false, ctx)) => {
                // AppKit may call this while egui holds its context lock.
                let _ = std::thread::Builder::new().name("crossbow-quit-repaint".into()).spawn(move || ctx.request_repaint());
                TERMINATE_CANCEL
            }
            _ => TERMINATE_NOW,
        }
    }

    fn current_quit_reason() -> Option<u32> {
        // SAFETY: Foundation's shared Apple Event manager, read on the main thread.
        unsafe {
            let manager: Option<Retained<AnyObject>> = msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
            let event: Option<Retained<AnyObject>> = msg_send![&*manager?, currentAppleEvent];
            let reason: Option<Retained<AnyObject>> = msg_send![&*event?, attributeDescriptorForKeyword: QUIT_REASON];
            let code: u32 = msg_send![&*reason?, typeCodeValue];
            Some(code)
        }
    }

    pub(super) fn install(guard: &Rc<RefCell<QuitGuard>>, ctx: &egui::Context) -> Result<(), String> {
        let main = MainThreadMarker::new().ok_or("not on the main thread")?;
        let app = NSApplication::sharedApplication(main);
        let delegate = app.delegate().ok_or("the application has no delegate")?;
        let class: &AnyClass = AsRef::<AnyObject>::as_ref(&*delegate).class();
        // SAFETY: The function matches `-(NSUInteger)applicationShouldTerminate:(id)sender`
        // ("Q@:@"); class_addMethod refuses a class that already has the method.
        let added = unsafe {
            let imp = std::mem::transmute::<extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> usize, Imp>(should_terminate);
            objc2::ffi::class_addMethod(class as *const AnyClass as *mut AnyClass, sel!(applicationShouldTerminate:), imp, c"Q@:@".as_ptr())
        };
        if !added.as_bool() {
            return Err(format!("{} already answers quit requests", class.name().to_string_lossy()));
        }
        // NSApplication may cache which optional methods its delegate implements.
        app.setDelegate(Some(&*delegate));
        attach(guard, ctx);
        Ok(())
    }

    fn attach(guard: &Rc<RefCell<QuitGuard>>, ctx: &egui::Context) {
        GUARD.with(|slot| *slot.borrow_mut() = Some((Rc::downgrade(guard), ctx.clone())));
    }

    pub(super) fn terminate() {
        let Some(main) = MainThreadMarker::new() else { return };
        let app = NSApplication::sharedApplication(main);
        // Outside this frame, so eframe is not inside its own update when AppKit quits.
        // SAFETY: terminate: takes an optional sender; nil is allowed.
        unsafe {
            let _: () = msg_send![&*app, performSelector: sel!(terminate:), withObject: std::ptr::null::<AnyObject>(), afterDelay: 0.0f64];
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_quit_hook_asks_the_guard_and_never_holds_up_a_quit_it_cannot_answer() {
            let this = objc2::runtime::NSObject::new();
            let ask = || should_terminate(&this, sel!(applicationShouldTerminate:), std::ptr::null_mut());
            assert_eq!(ask(), TERMINATE_NOW, "nothing to ask yet");
            let guard = Rc::new(RefCell::new(QuitGuard { enabled: true, ..QuitGuard::default() }));
            attach(&guard, &egui::Context::default());
            assert_eq!(ask(), TERMINATE_CANCEL, "⌘Q asks first");
            assert!(guard.borrow().asking());
            assert_eq!(ask(), TERMINATE_NOW, "⌘Q again quits");
            assert_eq!(ask(), TERMINATE_CANCEL);
            assert_eq!(guard.borrow_mut().confirm(), Some(Route::Quit));
            assert_eq!(ask(), TERMINATE_NOW, "Quit replays terminate, which then goes through");
            let busy = guard.borrow_mut();
            assert_eq!(ask(), TERMINATE_NOW, "a guard too busy to answer never holds up the quit");
            drop(busy);
            guard.borrow_mut().enabled = false;
            assert_eq!(ask(), TERMINATE_NOW, "off quits at once");
            drop(guard);
            assert_eq!(ask(), TERMINATE_NOW, "after the app is gone");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcuts::tests::Fixture;

    fn on() -> QuitGuard {
        QuitGuard { enabled: true, ..QuitGuard::default() }
    }

    #[test]
    fn the_warning_is_off_by_default_saved_on_this_device_and_a_failed_save_keeps_the_previous_one() {
        let fixture = Fixture::new();
        let mut lib = crossbow_core::library::Library::open(fixture.store()).unwrap();
        lib.ensure_defaults().unwrap();
        let dirty = lib.store.dirty_count().unwrap();
        let mut guard = QuitGuard::load(&lib.store);
        assert!(!guard.enabled(), "off by default");
        guard.choose(&lib.store, true);
        assert!(guard.enabled() && guard.failure.is_none());
        assert!(QuitGuard::load(&fixture.store()).enabled(), "read back after reopening");
        assert_eq!(lib.store.dirty_count().unwrap(), dirty, "no sync change is queued");

        guard.choose(&fixture.read_only(), false);
        assert!(guard.enabled(), "a failed save keeps the previous choice in use");
        let failure = guard.failure.as_ref().expect("the failure is kept for Retry");
        assert!(!failure.wanted && !failure.error.is_empty());
        assert!(QuitGuard::load(&fixture.store()).enabled());
        guard.choose(&lib.store, false);
        assert!(!guard.enabled() && guard.failure.is_none());
        assert!(!QuitGuard::load(&fixture.store()).enabled());

        lib.store.kv_set(KEY, "sometimes").unwrap();
        let unreadable = QuitGuard::load(&lib.store);
        assert!(!unreadable.enabled() && unreadable.notice.is_some(), "an unreadable value quits without asking and says so");
        assert_eq!(lib.store.kv_get(KEY).unwrap().as_deref(), Some("sometimes"), "kept until the next save");
    }

    #[test]
    fn with_the_warning_off_every_route_quits_without_asking() {
        let mut guard = QuitGuard::default();
        for route in [Route::Quit, Route::CloseWindow, Route::Quit] {
            assert!(guard.request(route), "{route:?}");
            assert!(!guard.asking() && !guard.take_reveal());
        }
    }

    #[test]
    fn every_route_asks_and_cancel_leaves_everything_open() {
        for route in [Route::Quit, Route::CloseWindow] {
            let mut guard = on();
            assert!(!guard.request(route), "{route:?} waits for the answer");
            assert!(guard.asking());
            assert_eq!(guard.take_reveal(), route == Route::Quit, "a menu or Dock quit brings the window forward");
            guard.cancel();
            assert!(!guard.asking());
            assert!(!guard.request(route), "the next {route:?} asks again");
        }
    }

    #[test]
    fn quit_replays_the_route_once_and_a_second_command_q_quits() {
        for route in [Route::Quit, Route::CloseWindow] {
            let mut guard = on();
            assert!(!guard.request(route));
            assert_eq!(guard.confirm(), Some(route), "Quit replays {route:?}");
            assert!(guard.confirm().is_none(), "answered once");
            assert!(guard.request(route), "the replayed {route:?} goes through");
            assert!(!guard.request(route), "only once");
        }
        for first in [Route::Quit, Route::CloseWindow] {
            let mut guard = on();
            assert!(!guard.request(first));
            assert!(!guard.request(Route::CloseWindow), "closing again keeps the question");
            assert!(guard.asking());
            assert!(guard.request(Route::Quit), "⌘Q while it shows quits");
            assert!(!guard.asking());
        }
    }

    #[test]
    fn a_quit_the_user_already_chose_goes_through_once_without_asking() {
        let mut guard = on();
        assert!(!guard.request(Route::CloseWindow));
        guard.allow_next();
        assert!(!guard.asking(), "the question it replaced is gone");
        assert!(guard.request(Route::Quit), "Restore and Quit quits");
        assert!(!guard.request(Route::Quit), "only once");
    }

    #[test]
    fn logging_out_restarting_and_shutting_down_are_never_held_up() {
        for reason in [*b"logo", *b"rlgo", *b"rrst", *b"rsdn", *b"rest", *b"shut"] {
            assert!(session_ending(Some(u32::from_be_bytes(reason))), "{}", String::from_utf8_lossy(&reason));
        }
        assert!(!session_ending(None), "⌘Q, the app menu and the Dock carry no quit reason");
    }
}
