//! Public AppKit window material and CEF-compatible display geometry.
//! A successful install describes the view tree, not verified wgpu compositing.

use raw_window_handle::HasWindowHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowAppearance {
    pub transparent: bool,
    pub dark: bool,
    /// Opaque fallback only. The shared translucent tint is painted by egui.
    pub opaque_background: [u8; 3],
}

impl Default for WindowAppearance {
    fn default() -> Self {
        Self {
            transparent: false,
            dark: false,
            opaque_background: [243, 242, 244],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccessibilityDisplay {
    pub reduce_transparency: bool,
    pub increase_contrast: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpaqueReason {
    Disabled,
    ReduceTransparency,
    IncreasedContrast,
    UnsupportedPlatform,
    UnsupportedHierarchy,
    AppearanceUnavailable,
    InstallationFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaterialState {
    pub transparent: bool,
    pub opaque_reason: Option<OpaqueReason>,
    pub accessibility: AccessibilityDisplay,
}

fn material_policy(
    requested: bool,
    accessibility: AccessibilityDisplay,
    installed: bool,
) -> MaterialState {
    let opaque_reason = if !requested {
        Some(OpaqueReason::Disabled)
    } else if accessibility.reduce_transparency {
        Some(OpaqueReason::ReduceTransparency)
    } else if accessibility.increase_contrast {
        Some(OpaqueReason::IncreasedContrast)
    } else if !installed {
        Some(OpaqueReason::UnsupportedHierarchy)
    } else {
        None
    };
    MaterialState {
        transparent: opaque_reason.is_none(),
        opaque_reason,
        accessibility,
    }
}

fn material_state_is_current(
    state: MaterialState,
    hierarchy_supported: bool,
    material_intact: bool,
) -> bool {
    if state.transparent {
        hierarchy_supported && material_intact
    } else {
        // An ancestor can rejoin the original window without changing this
        // view's direct parent or frame, and without a window notification.
        state.opaque_reason != Some(OpaqueReason::UnsupportedHierarchy) || !hierarchy_supported
    }
}

fn rollback_material_install(
    make_opaque: impl FnOnce() -> MaterialState,
    restore_responder: impl FnOnce() -> bool,
) -> MaterialState {
    let state = make_opaque();
    if !restore_responder() {
        log::warn!("Native material was removed, but restoring the prior first responder failed");
    }
    state
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DipRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl DipRect {
    fn valid(self) -> bool {
        [
            self.x,
            self.y,
            self.width,
            self.height,
            self.x + self.width,
            self.y + self.height,
        ]
        .into_iter()
        .all(f64::is_finite)
            && self.width > 0.0
            && self.height > 0.0
    }
}

/// Global DIPs, with origin at the primary display's top-left. Do not multiply
/// these rectangles by backing_scale when passing CEF's screen information.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplaySnapshot {
    pub screen: DipRect,
    pub work_area: DipRect,
    /// The exact NSWindow frame, including window chrome, in top-left DIPs.
    pub window: DipRect,
    /// The full egui rendering NSView, not an individual CEF content pane.
    /// Add that pane's local DIP offset when converting CEF's view coordinates.
    pub view: DipRect,
    /// Primary display max Y in Cocoa global DIPs. Convert a top-left point
    /// back to Cocoa with primary_top - y; subtract height too for a rectangle.
    pub primary_top: f64,
    pub backing_scale: f64,
}

fn cocoa_rect_to_top_left(rect: DipRect, primary_top: f64) -> Option<DipRect> {
    if !rect.valid() || !primary_top.is_finite() {
        return None;
    }
    let rect = DipRect {
        y: primary_top - rect.y - rect.height,
        ..rect
    };
    rect.valid().then_some(rect)
}

fn display_from_cocoa(
    primary: DipRect,
    screen: DipRect,
    work_area: DipRect,
    window: DipRect,
    view: DipRect,
    backing_scale: f64,
) -> Option<DisplaySnapshot> {
    if !primary.valid() || !backing_scale.is_finite() || backing_scale <= 0.0 {
        return None;
    }
    let top = primary.y + primary.height;
    Some(DisplaySnapshot {
        screen: cocoa_rect_to_top_left(screen, top)?,
        work_area: cocoa_rect_to_top_left(work_area, top)?,
        window: cocoa_rect_to_top_left(window, top)?,
        view: cocoa_rect_to_top_left(view, top)?,
        primary_top: top,
        backing_scale,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeWindowError {
    UnsupportedPlatform,
    NotMainThread,
    WindowHandleUnavailable,
    UnsupportedHandle,
    MissingWindow,
    RepaintWorkerUnavailable,
}

impl std::fmt::Display for NativeWindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedPlatform => "Native window material is unavailable on this platform.",
            Self::NotMainThread => "Native window setup requires the main thread.",
            Self::WindowHandleUnavailable => "The application window handle is unavailable.",
            Self::UnsupportedHandle => "The application window handle is not an AppKit view.",
            Self::MissingWindow => "The rendering view is not attached to a window.",
            Self::RepaintWorkerUnavailable => {
                "The native display notification worker could not start."
            }
        })
    }
}

impl std::error::Error for NativeWindowError {}

#[cfg(target_os = "macos")]
pub use macos::NativeWindow;

/// Every display's work area, without the menu bar and Dock, in top-left
/// DIPs. Empty off macOS.
pub fn work_areas() -> Vec<DipRect> {
    #[cfg(target_os = "macos")]
    return macos::work_areas();
    #[cfg(not(target_os = "macos"))]
    Vec::new()
}

/// Let the window titled `title` follow the person to every Space and float
/// over full-screen apps, and keep 16:9 while it is resized. False while no
/// such window exists, or off macOS.
pub fn float_everywhere(title: &str) -> bool {
    #[cfg(target_os = "macos")]
    return macos::float_everywhere(title);
    #[cfg(not(target_os = "macos"))]
    { let _ = title; false }
}

#[cfg(not(target_os = "macos"))]
pub struct NativeWindow;

#[cfg(not(target_os = "macos"))]
impl NativeWindow {
    pub fn new(
        _source: &impl HasWindowHandle,
        _ctx: &egui::Context,
    ) -> Result<Self, NativeWindowError> {
        Err(NativeWindowError::UnsupportedPlatform)
    }

    pub fn reconcile(&mut self, _appearance: WindowAppearance) -> MaterialState {
        MaterialState {
            transparent: false,
            opaque_reason: Some(OpaqueReason::UnsupportedPlatform),
            accessibility: AccessibilityDisplay::default(),
        }
    }

    pub fn display_snapshot(&self) -> Option<DisplaySnapshot> {
        None
    }
    pub fn remove_material(&mut self) {}
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, ProtocolObject};
    use objc2::{MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
    use objc2_app_kit::{
        NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
        NSApplication, NSApplicationDidChangeScreenParametersNotification, NSColor, NSScreen, NSView,
        NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
        NSVisualEffectView, NSWindow, NSWindowCollectionBehavior, NSWindowDidChangeBackingPropertiesNotification,
        NSWindowDidChangeScreenNotification, NSWindowDidEnterFullScreenNotification,
        NSWindowDidExitFullScreenNotification, NSWindowDidMoveNotification,
        NSWindowDidResizeNotification, NSWindowOrderingMode, NSWorkspace,
        NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification,
    };
    use objc2_foundation::{
        NSNotification, NSNotificationCenter, NSNotificationName, NSObjectProtocol, NSPoint, NSRect,
        NSSize,
    };
    use raw_window_handle::RawWindowHandle;
    use std::ptr::NonNull;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{SyncSender, sync_channel},
    };

    define_class!(
        #[unsafe(super(NSVisualEffectView))]
        #[name = "CrossbowWindowMaterial"]
        struct Backdrop;

        impl Backdrop {
            #[unsafe(method(hitTest:))]
            fn hit_test(&self, _point: NSPoint) -> *mut NSView { std::ptr::null_mut() }

            #[unsafe(method(acceptsFirstResponder))]
            fn accepts_first_responder(&self) -> bool { false }
        }
    );

    impl Backdrop {
        fn new(frame: NSRect, main: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(main).set_ivars(());
            // SAFETY: AppKit initializes this subclass on the main thread.
            let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
            view.setMaterial(NSVisualEffectMaterial::UnderWindowBackground);
            view.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            view.setState(NSVisualEffectState::FollowsWindowActiveState);
            view.setHidden(true);
            view
        }
    }

    struct Observer {
        center: Retained<NSNotificationCenter>,
        token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    }

    impl Drop for Observer {
        fn drop(&mut self) {
            // SAFETY: This is the retained token returned by this exact center.
            unsafe {
                self.center.removeObserver((*self.token).as_ref());
            }
        }
    }

    /// AppKit can post synchronously while egui holds its context lock. Never
    /// call request_repaint on that stack or join its worker during UI teardown.
    struct RepaintBridge {
        sender: SyncSender<()>,
        alive: Arc<AtomicBool>,
        dirty: Arc<AtomicBool>,
    }

    impl RepaintBridge {
        fn new(ctx: &egui::Context) -> Result<Self, NativeWindowError> {
            let (sender, receiver) = sync_channel(1);
            let alive = Arc::new(AtomicBool::new(true));
            let worker_alive = alive.clone();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name("crossbow-native-repaint".into())
                .spawn(move || {
                    while receiver.recv().is_ok() {
                        if !worker_alive.load(Ordering::Acquire) {
                            break;
                        }
                        ctx.request_repaint_of(egui::ViewportId::ROOT);
                    }
                })
                .map_err(|_| NativeWindowError::RepaintWorkerUnavailable)?;
            Ok(Self {
                sender,
                alive,
                dirty: Arc::new(AtomicBool::new(true)),
            })
        }

        fn observe(
            &self,
            center: &NSNotificationCenter,
            name: &NSNotificationName,
            object: Option<&AnyObject>,
        ) -> Observer {
            let sender = self.sender.clone();
            let alive = self.alive.clone();
            let dirty = self.dirty.clone();
            let callback = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                if alive.load(Ordering::Acquire) {
                    dirty.store(true, Ordering::Release);
                    let _ = sender.try_send(());
                }
            });
            // SAFETY: Callback owns only Send/Sync state, never captures native
            // objects or self, and does not dereference the notification.
            let token = unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    object,
                    None,
                    &callback,
                )
            };
            Observer {
                center: center.retain(),
                token,
            }
        }
    }

    impl Drop for RepaintBridge {
        fn drop(&mut self) {
            self.alive.store(false, Ordering::Release);
            let _ = self.sender.try_send(());
        }
    }

    pub struct NativeWindow {
        view: Retained<NSView>,
        window: Retained<NSWindow>,
        material: Option<Retained<Backdrop>>,
        parent: Option<Retained<NSView>>,
        observers: Vec<Observer>,
        repaint: RepaintBridge,
        light: Option<Retained<NSAppearance>>,
        dark: Option<Retained<NSAppearance>>,
        applied: Option<WindowAppearance>,
        state: MaterialState,
        view_frame: NSRect,
    }

    impl NativeWindow {
        /// Use the exact eframe CreationContext/Frame, never a global key window.
        /// The constructor and every failed material setup leave this window opaque.
        pub fn new(
            source: &impl HasWindowHandle,
            ctx: &egui::Context,
        ) -> Result<Self, NativeWindowError> {
            MainThreadMarker::new().ok_or(NativeWindowError::NotMainThread)?;
            let handle = source
                .window_handle()
                .map_err(|_| NativeWindowError::WindowHandleUnavailable)?;
            let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
                return Err(NativeWindowError::UnsupportedHandle);
            };
            // SAFETY: The borrowed window handle guarantees a live AppKit view.
            // Retain it before that borrow ends, and use it only on the main thread.
            let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() }.retain();
            let window = view.window().ok_or(NativeWindowError::MissingWindow)?;
            let appearance = WindowAppearance::default();
            set_opaque(&window, appearance.opaque_background);
            let repaint = RepaintBridge::new(ctx)?;
            let workspace = NSWorkspace::sharedWorkspace();
            let accessibility = read_accessibility(&workspace);
            let center = NSNotificationCenter::defaultCenter();
            let mut observers = Vec::with_capacity(8);
            // SAFETY: AppKit exports these public notification-name constants.
            unsafe {
                for name in [
                    NSWindowDidMoveNotification,
                    NSWindowDidResizeNotification,
                    NSWindowDidChangeScreenNotification,
                    NSWindowDidChangeBackingPropertiesNotification,
                    NSWindowDidEnterFullScreenNotification,
                    NSWindowDidExitFullScreenNotification,
                ] {
                    observers.push(repaint.observe(&center, name, Some(&window)));
                }
                observers.push(repaint.observe(
                    &center,
                    NSApplicationDidChangeScreenParametersNotification,
                    None,
                ));
                observers.push(repaint.observe(
                    &workspace.notificationCenter(),
                    NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification,
                    None,
                ));
            }
            let view_frame = view.frame();
            // SAFETY: These are public appearance-name constants.
            let (light, dark) = unsafe {
                (
                    NSAppearance::appearanceNamed(NSAppearanceNameAqua),
                    NSAppearance::appearanceNamed(NSAppearanceNameDarkAqua),
                )
            };
            Ok(Self {
                view,
                window,
                material: None,
                parent: None,
                observers,
                repaint,
                light,
                dark,
                applied: None,
                state: material_policy(false, accessibility, false),
                view_frame,
            })
        }

        /// Reconcile before drawing. Only a true result permits translucent shell
        /// fills; it does not qualify native pixels, blur strength or contrast.
        pub fn reconcile(&mut self, appearance: WindowAppearance) -> MaterialState {
            // SAFETY: Retained views are main-thread-only; no parent is modified.
            let parent = unsafe { self.view.superview() };
            let frame = self.view.frame();
            let parent_changed = !same_view(parent.as_deref(), self.parent.as_deref());
            let dirty = self.repaint.dirty.swap(false, Ordering::AcqRel);
            let material_intact = material_state_is_current(
                self.state,
                (self.state.transparent
                    || self.state.opaque_reason == Some(OpaqueReason::UnsupportedHierarchy))
                    && self.supported_hierarchy(),
                self.state.transparent
                    && self.material_is_below_render_view()
                    && self
                        .material
                        .as_ref()
                        .is_some_and(|material| !material.isHidden() && material.frame() == frame),
            );
            let opacity_matches =
                self.window.alphaValue() == 1.0 && self.window.isOpaque() != self.state.transparent;
            if !dirty
                && !parent_changed
                && material_intact
                && opacity_matches
                && frame == self.view_frame
                && self.applied == Some(appearance)
            {
                return self.state;
            }
            self.view_frame = frame;
            let accessibility = read_accessibility(&NSWorkspace::sharedWorkspace());
            self.applied = Some(appearance);
            let policy = material_policy(appearance.transparent, accessibility, true);
            if !policy.transparent {
                self.detach(appearance.opaque_background);
                self.parent = parent;
                self.state = policy;
                return self.state;
            }

            if parent_changed || (self.material.is_some() && !self.material_is_below_render_view())
            {
                self.detach(appearance.opaque_background);
            }
            self.parent = parent;
            if !self.supported_hierarchy() {
                return self.fail_opaque(
                    appearance,
                    accessibility,
                    OpaqueReason::UnsupportedHierarchy,
                );
            }
            let Some(native_appearance) = (if appearance.dark {
                &self.dark
            } else {
                &self.light
            })
            .as_ref() else {
                return self.fail_opaque(
                    appearance,
                    accessibility,
                    OpaqueReason::AppearanceUnavailable,
                );
            };
            let parent = self.parent.as_ref().expect("checked hierarchy");
            if self.material.is_none() {
                set_opaque(&self.window, appearance.opaque_background);
                let material = Backdrop::new(frame, self.view.mtm());
                material.setAutoresizingMask(self.view.autoresizingMask());
                material.setAppearance(Some(native_appearance));
                let responder = self.window.firstResponder();
                parent.addSubview_positioned_relativeTo(
                    &material,
                    NSWindowOrderingMode::Below,
                    Some(&self.view),
                );
                self.material = Some(material);
                if !same_responder(
                    responder.as_deref(),
                    self.window.firstResponder().as_deref(),
                ) {
                    let window = self.window.clone();
                    return rollback_material_install(
                        || {
                            self.fail_opaque(
                                appearance,
                                accessibility,
                                OpaqueReason::InstallationFailed,
                            )
                        },
                        || restore_first_responder(&window, responder.as_deref()),
                    );
                }
            }
            let material = self.material.as_ref().expect("created material");
            material.setFrame(frame);
            material.setAppearance(Some(native_appearance));
            if !self.material_is_below_render_view() {
                return self.fail_opaque(
                    appearance,
                    accessibility,
                    OpaqueReason::InstallationFailed,
                );
            }
            material.setHidden(false);
            self.window.setAlphaValue(1.0);
            self.window.setBackgroundColor(Some(&NSColor::clearColor()));
            self.window.setOpaque(false);
            self.state = policy;
            self.state
        }

        /// Reads current native geometry on every call. AppKit says not to cache
        /// visibleFrame: Dock/menu-bar reservations can change between calls.
        pub fn display_snapshot(&self) -> Option<DisplaySnapshot> {
            snapshot(&self.view, &self.window, self.view.mtm())
        }

        pub fn remove_material(&mut self) {
            let appearance = self.applied.unwrap_or_default();
            self.detach(appearance.opaque_background);
            self.applied = None;
            self.state = material_policy(false, self.state.accessibility, false);
        }

        fn supported_hierarchy(&self) -> bool {
            let Some(parent) = &self.parent else {
                return false;
            };
            let Some(current_window) = self.view.window() else {
                return false;
            };
            let Some(parent_window) = parent.window() else {
                return false;
            };
            let Some(content) = self.window.contentView() else {
                return false;
            };
            std::ptr::eq(&*current_window, &*self.window)
                && std::ptr::eq(&*parent_window, &*self.window)
                && self.view.isDescendantOf(&content)
                && !self.view.isOpaque()
                && rect(self.view.frame()).valid()
                && rect(self.view.bounds()).valid()
        }

        fn material_is_below_render_view(&self) -> bool {
            let Some(parent) = &self.parent else {
                return false;
            };
            let Some(material) = &self.material else {
                return false;
            };
            // Inspect only public view ordering, never backing-layer internals.
            let siblings = parent.subviews();
            let render_index = siblings
                .iter()
                .position(|view| std::ptr::eq(&*view, &*self.view));
            let material_index = siblings
                .iter()
                .position(|view| std::ptr::eq(&*view, &**material as &NSView));
            matches!((material_index, render_index), (Some(a), Some(b)) if a < b)
        }

        fn fail_opaque(
            &mut self,
            appearance: WindowAppearance,
            accessibility: AccessibilityDisplay,
            reason: OpaqueReason,
        ) -> MaterialState {
            self.detach(appearance.opaque_background);
            self.state = MaterialState {
                transparent: false,
                opaque_reason: Some(reason),
                accessibility,
            };
            self.state
        }

        fn detach(&mut self, background: [u8; 3]) {
            set_opaque(&self.window, background);
            if let Some(material) = self.material.take() {
                material.removeFromSuperview();
            }
        }
    }

    impl Drop for NativeWindow {
        fn drop(&mut self) {
            // In-flight notifications retain only the bridge, never this window.
            self.repaint.alive.store(false, Ordering::Release);
            self.observers.clear();
            self.remove_material();
        }
    }

    fn same_view(a: Option<&NSView>, b: Option<&NSView>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => std::ptr::eq(a, b),
            _ => false,
        }
    }

    fn same_responder(
        a: Option<&objc2_app_kit::NSResponder>,
        b: Option<&objc2_app_kit::NSResponder>,
    ) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => std::ptr::eq(a, b),
            _ => false,
        }
    }

    fn restore_first_responder(
        window: &NSWindow,
        responder: Option<&objc2_app_kit::NSResponder>,
    ) -> bool {
        if same_responder(responder, window.firstResponder().as_deref()) {
            return true;
        }
        if responder.is_some_and(|responder| !responder.acceptsFirstResponder()) {
            return false;
        }
        let accepted = window.makeFirstResponder(responder);
        // AppKit can return true after substituting the window for a rejected
        // responder. Only identity readback proves restoration of the old one.
        let restored = same_responder(responder, window.firstResponder().as_deref());
        accepted && restored
    }

    fn set_opaque(window: &NSWindow, [r, g, b]: [u8; 3]) {
        window.setAlphaValue(1.0);
        window.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            f64::from(r) / 255.0,
            f64::from(g) / 255.0,
            f64::from(b) / 255.0,
            1.0,
        )));
        window.setOpaque(true);
    }

    fn read_accessibility(workspace: &NSWorkspace) -> AccessibilityDisplay {
        AccessibilityDisplay {
            reduce_transparency: workspace.accessibilityDisplayShouldReduceTransparency(),
            increase_contrast: workspace.accessibilityDisplayShouldIncreaseContrast(),
        }
    }

    fn rect(value: NSRect) -> DipRect {
        DipRect {
            x: value.origin.x,
            y: value.origin.y,
            width: value.size.width,
            height: value.size.height,
        }
    }

    fn snapshot(
        view: &NSView,
        window: &NSWindow,
        main: MainThreadMarker,
    ) -> Option<DisplaySnapshot> {
        if !std::ptr::eq(&*view.window()?, window) {
            return None;
        }
        // screens[0] anchors Cocoa's global coordinate system. mainScreen() is
        // the keyboard-focus display and would jump when the window moves.
        let primary = NSScreen::screens(main).firstObject()?;
        let screen = window.screen()?;
        let window_rect = view.convertRect_toView(view.bounds(), None);
        display_from_cocoa(
            rect(primary.frame()),
            rect(screen.frame()),
            rect(screen.visibleFrame()),
            rect(window.frame()),
            rect(window.convertRectToScreen(window_rect)),
            window.backingScaleFactor(),
        )
    }

    pub(super) fn work_areas() -> Vec<DipRect> {
        let Some(main) = MainThreadMarker::new() else { return Vec::new() };
        let screens = NSScreen::screens(main);
        let Some(primary) = screens.firstObject() else { return Vec::new() };
        let primary = rect(primary.frame());
        if !primary.valid() {
            return Vec::new();
        }
        let top = primary.y + primary.height;
        screens.iter().filter_map(|screen| cocoa_rect_to_top_left(rect(screen.visibleFrame()), top)).collect()
    }

    pub(super) fn float_everywhere(title: &str) -> bool {
        let Some(main) = MainThreadMarker::new() else { return false };
        let app = NSApplication::sharedApplication(main);
        let Some(window) = app.windows().iter().find(|window| window.title().to_string() == title) else {
            return false;
        };
        // Every Space, so it stays with the person as they switch; over
        // full-screen apps too, as a floating player should.
        window.setCollectionBehavior(NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary);
        window.setContentAspectRatio(NSSize::new(16.0, 9.0));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f64, y: f64, width: f64, height: f64) -> DipRect {
        DipRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn failed_hierarchy_retries_after_ancestor_reattachment_without_other_changes() {
        let failed = material_policy(true, AccessibilityDisplay::default(), false);
        assert_eq!(
            failed.opaque_reason,
            Some(OpaqueReason::UnsupportedHierarchy)
        );
        assert!(material_state_is_current(failed, false, false));
        // The direct parent, frame, appearance and dirty flag may all be unchanged.
        assert!(!material_state_is_current(failed, true, false));
        let recovered = material_policy(true, AccessibilityDisplay::default(), true);
        assert!(material_state_is_current(recovered, true, true));
    }

    #[test]
    fn stable_opaque_policy_and_failed_install_do_not_retry_each_frame() {
        for reason in [
            OpaqueReason::Disabled,
            OpaqueReason::ReduceTransparency,
            OpaqueReason::IncreasedContrast,
            OpaqueReason::InstallationFailed,
            OpaqueReason::AppearanceUnavailable,
            OpaqueReason::UnsupportedPlatform,
        ] {
            let state = MaterialState {
                transparent: false,
                opaque_reason: Some(reason),
                accessibility: AccessibilityDisplay::default(),
            };
            for supported in [false, true] {
                assert!(material_state_is_current(state, supported, false));
            }
        }
    }

    #[test]
    fn transparent_fast_path_rejects_detached_or_missing_material() {
        let state = material_policy(true, AccessibilityDisplay::default(), true);
        assert!(!material_state_is_current(state, false, true));
        assert!(!material_state_is_current(state, true, false));
        assert!(material_state_is_current(state, true, true));
    }

    fn exercise_install_rollback(restored: bool) {
        let calls = std::cell::RefCell::new(Vec::new());
        let failed = MaterialState {
            transparent: false,
            opaque_reason: Some(OpaqueReason::InstallationFailed),
            accessibility: AccessibilityDisplay::default(),
        };
        let result = rollback_material_install(
            || {
                calls.borrow_mut().push("opaque_and_detached");
                failed
            },
            || {
                assert_eq!(*calls.borrow(), vec!["opaque_and_detached"]);
                calls.borrow_mut().push("restore_and_read_back");
                restored
            },
        );
        assert_eq!(
            *calls.borrow(),
            vec!["opaque_and_detached", "restore_and_read_back"]
        );
        assert_eq!(result, failed);
    }

    #[test]
    fn failed_install_detaches_then_restores_responder_once() {
        exercise_install_rollback(true);
    }

    #[test]
    fn rejected_responder_restore_stays_opaque_without_retry() {
        exercise_install_rollback(false);
    }

    #[test]
    fn transparency_requires_request_install_and_accessibility_permission() {
        for requested in [false, true] {
            for installed in [false, true] {
                for reduce_transparency in [false, true] {
                    for increase_contrast in [false, true] {
                        let state = material_policy(
                            requested,
                            AccessibilityDisplay {
                                reduce_transparency,
                                increase_contrast,
                            },
                            installed,
                        );
                        assert_eq!(
                            state.transparent,
                            requested && installed && !reduce_transparency && !increase_contrast
                        );
                        assert_eq!(state.opaque_reason.is_none(), state.transparent);
                    }
                }
            }
        }
    }

    #[test]
    fn accessibility_override_does_not_replace_user_intent() {
        let request = WindowAppearance {
            transparent: true,
            ..Default::default()
        };
        assert_eq!(
            material_policy(
                request.transparent,
                AccessibilityDisplay {
                    reduce_transparency: true,
                    increase_contrast: false
                },
                true
            )
            .opaque_reason,
            Some(OpaqueReason::ReduceTransparency)
        );
        assert!(
            material_policy(request.transparent, AccessibilityDisplay::default(), true).transparent
        );
        assert_eq!(
            material_policy(
                true,
                AccessibilityDisplay {
                    reduce_transparency: false,
                    increase_contrast: true
                },
                true
            )
            .opaque_reason,
            Some(OpaqueReason::IncreasedContrast)
        );
    }

    #[test]
    fn primary_screen_work_area_and_view_are_top_left_dips_not_pixels() {
        let primary = r(0.0, 0.0, 1512.0, 982.0);
        let value = display_from_cocoa(
            primary,
            primary,
            r(0.0, 64.0, 1512.0, 894.0),
            r(100.0, 182.0, 800.0, 628.0),
            r(100.0, 182.0, 800.0, 600.0),
            2.0,
        )
        .unwrap();
        assert_eq!(value.screen, primary);
        assert_eq!(value.work_area, r(0.0, 24.0, 1512.0, 894.0));
        assert_eq!(value.window, r(100.0, 172.0, 800.0, 628.0));
        assert_eq!(value.view, r(100.0, 200.0, 800.0, 600.0));
        assert_eq!(value.primary_top, 982.0);
        assert_eq!(value.backing_scale, 2.0);
    }

    #[test]
    fn exact_window_and_primary_reference_round_trip_to_cocoa() {
        let primary = r(0.0, -120.0, 1512.0, 982.0);
        let screen = r(-1920.0, 862.0, 1920.0, 1080.0);
        let window = r(-1500.0, 1000.0, 800.0, 628.0);
        let view = r(-1500.0, 1000.0, 800.0, 600.0);
        for scale in [1.0, 2.0] {
            let value = display_from_cocoa(primary, screen, screen, window, view, scale).unwrap();
            assert_eq!(value.primary_top, 862.0);
            assert_eq!(value.window, r(-1500.0, -766.0, 800.0, 628.0));
            assert_eq!(value.view, r(-1500.0, -738.0, 800.0, 600.0));
            assert_ne!(value.window, value.view);
            assert_eq!(
                value.primary_top - value.window.y - value.window.height,
                window.y,
            );
            let cocoa_view_top = value.primary_top - value.view.y;
            assert_eq!(cocoa_view_top, view.y + view.height);
            assert_eq!(value.primary_top - (value.view.y + 40.0), 1560.0);
        }
    }

    #[test]
    fn window_frame_changes_are_independent_of_render_view_geometry() {
        let primary = r(0.0, 0.0, 1512.0, 982.0);
        let view = r(100.0, 182.0, 800.0, 600.0);
        let first = display_from_cocoa(
            primary,
            primary,
            primary,
            r(100.0, 182.0, 800.0, 628.0),
            view,
            2.0,
        )
        .unwrap();
        let changed = display_from_cocoa(
            primary,
            primary,
            primary,
            r(100.0, 182.0, 800.0, 640.0),
            view,
            2.0,
        )
        .unwrap();
        assert_eq!(first.screen, changed.screen);
        assert_eq!(first.view, changed.view);
        assert_eq!(first.primary_top, changed.primary_top);
        assert_eq!(first.backing_scale, changed.backing_scale);
        assert_ne!(first.window, changed.window);
        assert_ne!(first, changed);
    }

    #[test]
    fn displays_above_left_and_below_keep_signed_global_coordinates() {
        let top = 982.0;
        assert_eq!(
            cocoa_rect_to_top_left(r(-1920.0, 0.0, 1920.0, 1080.0), top),
            Some(r(-1920.0, -98.0, 1920.0, 1080.0))
        );
        assert_eq!(
            cocoa_rect_to_top_left(r(0.0, 982.0, 1920.0, 1080.0), top),
            Some(r(0.0, -1080.0, 1920.0, 1080.0))
        );
        assert_eq!(
            cocoa_rect_to_top_left(r(0.0, -1080.0, 1920.0, 1080.0), top),
            Some(r(0.0, 982.0, 1920.0, 1080.0))
        );
    }

    #[test]
    fn equal_scale_display_and_work_area_changes_are_observable() {
        let primary = r(0.0, 0.0, 1512.0, 982.0);
        let view = r(100.0, 182.0, 800.0, 600.0);
        let window = r(100.0, 182.0, 800.0, 628.0);
        let first = display_from_cocoa(primary, primary, primary, window, view, 2.0).unwrap();
        let right = r(1512.0, 0.0, 1512.0, 982.0);
        let moved = display_from_cocoa(
            primary,
            right,
            right,
            r(1612.0, 182.0, 800.0, 628.0),
            r(1612.0, 182.0, 800.0, 600.0),
            2.0,
        )
        .unwrap();
        assert_eq!(first.backing_scale, moved.backing_scale);
        assert_eq!(first.primary_top, moved.primary_top);
        assert_ne!(first.window, moved.window);
        assert_ne!(first, moved);
        let dock_changed = display_from_cocoa(
            primary,
            primary,
            r(60.0, 0.0, 1452.0, 958.0),
            window,
            view,
            2.0,
        )
        .unwrap();
        assert_eq!(first.screen, dock_changed.screen);
        assert_eq!(first.window, dock_changed.window);
        assert_eq!(first.view, dock_changed.view);
        assert_eq!(first.backing_scale, dock_changed.backing_scale);
        assert_ne!(first.work_area, dock_changed.work_area);
        assert_ne!(first, dock_changed);
    }

    #[test]
    fn invalid_geometry_or_scale_never_fabricates_a_display() {
        let valid = r(0.0, 0.0, 100.0, 100.0);
        for invalid in [
            r(f64::NAN, 0.0, 100.0, 100.0),
            r(0.0, f64::INFINITY, 100.0, 100.0),
            r(0.0, 0.0, 0.0, 100.0),
            r(0.0, 0.0, 100.0, -1.0),
            r(0.0, f64::MAX, 100.0, f64::MAX),
        ] {
            assert!(display_from_cocoa(invalid, valid, valid, valid, valid, 1.0).is_none());
            assert!(display_from_cocoa(valid, invalid, valid, valid, valid, 1.0).is_none());
            assert!(display_from_cocoa(valid, valid, invalid, valid, valid, 1.0).is_none());
            assert!(display_from_cocoa(valid, valid, valid, invalid, valid, 1.0).is_none());
            assert!(display_from_cocoa(valid, valid, valid, valid, invalid, 1.0).is_none());
        }
        for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(display_from_cocoa(valid, valid, valid, valid, valid, scale).is_none());
        }
    }
}
