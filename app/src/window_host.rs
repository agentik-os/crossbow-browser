//! A second native window showing one web view, as an eframe deferred
//! viewport. eframe runs a deferred viewport's UI on its own, so that callback
//! must be `Send + Sync` and cannot hold a `WebView`, which stays with the
//! root. Window and root share a [`Surface`]: the root publishes the latest
//! frame and page state, the window collects input and commands, and the root
//! delivers them to the view it owns on its next pass.

use crate::input::WebInput;
use crate::ui::content::PointerInput;
use crate::ui::theme::Palette;
use crossbow_engine::CursorKind;
use egui::{Rect, TextureId, Vec2, ViewportId};
use std::sync::{Arc, Mutex, MutexGuard};

/// Input for the hosted page, in its view coordinates.
#[derive(Clone, Debug)]
pub enum ViewInput {
    Pointer(PointerInput),
    Keyboard(WebInput),
    /// The page held the pointer and lost it without a release.
    CaptureLost,
}

/// What a host needs from its page: the `WebView` in the app, a recorder in tests.
pub trait Page {
    fn deliver(&self, input: &ViewInput);
    fn resize(&self, size: Vec2, scale: f32);
    fn set_focus(&self, focused: bool);
    fn close(&self);
}

impl Page for crossbow_engine::WebView {
    fn deliver(&self, input: &ViewInput) {
        match input {
            ViewInput::Pointer(pointer) => pointer.send(self),
            ViewInput::Keyboard(key) => crate::input::send(self, key),
            ViewInput::CaptureLost => self.capture_lost(),
        }
    }

    fn resize(&self, size: Vec2, scale: f32) {
        crossbow_engine::WebView::resize(self, size.x, size.y, scale);
    }

    fn set_focus(&self, focused: bool) {
        crossbow_engine::WebView::set_focus(self, focused);
    }

    fn close(&self) {
        crossbow_engine::WebView::close(self);
    }
}

/// The page as the window shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageState {
    pub url: String,
    pub title: String,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub loading: bool,
    pub progress: f32,
    pub cursor: CursorKind,
    pub tooltip: Option<String>,
    /// A text field in the page has focus, so the window turns on IME.
    pub editable: bool,
}

/// A Space the page can move to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceChoice {
    pub id: String,
    pub name: String,
    pub profile: String,
    /// It uses the page's profile, so the live page moves as it is.
    pub same_profile: bool,
}

/// What the root publishes for the window to draw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    /// The page's texture, its size in pixels and the engine's paint counter.
    pub texture: Option<(TextureId, Vec2, u64)>,
    /// An open `<select>` popup: texture, pixel size and rectangle in the page.
    pub popup: Option<(TextureId, Vec2, Rect)>,
    pub page: PageState,
    pub palette: Option<Palette>,
    /// The Space's surface texture, painted at the palette's strength.
    pub surface_texture: Option<TextureId>,
    /// Keep Window on Top is on.
    pub on_top: bool,
    pub spaces: Vec<SpaceChoice>,
    /// The page's profile, named when there are several.
    pub profile: Option<String>,
    /// Why the last close, move or capture did not happen.
    pub notice: Option<String>,
    /// Command-Shift drag on the page captures.
    pub capture_drag: bool,
    /// The card of the capture this window took last.
    pub capture_card: Option<crate::ui::capture::CardStatus>,
}

/// What the window's controls ask the root to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowCommand {
    Back,
    Forward,
    Reload,
    Stop,
    /// Text typed into the address field: a URL or a search.
    Navigate(String),
    /// Move the page into a Space. `reopen` confirms reopening it under that
    /// Space's profile when it differs from the page's.
    OpenInSpace { space: String, reopen: bool },
    /// Keep Window on Top, for every Little Window on this device.
    KeepOnTop(bool),
    /// Capture these pixels of the page.
    Capture(crate::capture::PixelRect),
    /// One of the capture card's buttons, or its timeout.
    Card(crate::ui::capture::CardAction),
    Close,
}

/// The Space chooser behind Open in Space….
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chooser {
    /// A Space under another profile, waiting for the reopen to be confirmed.
    pub confirm: Option<String>,
    /// Its first control takes keyboard focus on the next pass.
    pub focus: bool,
}

/// State the window's own controls keep between passes.
#[derive(Clone, Debug, Default)]
pub struct Chrome {
    /// The address being typed, while the field is open.
    pub address: Option<String>,
    /// The address field takes keyboard focus on the next pass.
    pub focus_address: bool,
    pub chooser: Option<Chooser>,
    /// The page holds the pointer between a press and its release.
    pub captured: bool,
    /// The page heard the pointer over it and not yet that it left.
    pub pointer_over: bool,
    /// Keys the page saw pressed, so it gets their releases and no others.
    pub keys_down: Vec<crossbow_engine::NamedKey>,
    pub last_press: Option<(f64, egui::Pos2, egui::PointerButton, i32)>,
    /// A capture being selected over the page.
    pub capture: Option<crate::ui::capture::Selection>,
    /// Where the last capture was selected, for its card.
    pub capture_anchor: Option<Rect>,
    /// When the pointer was last over the card, or when it appeared.
    pub card_seen_at: Option<f64>,
}

/// State shared by the window's callback and the root.
#[derive(Debug, Default)]
pub struct Surface {
    /// Published by the root.
    pub frame: Frame,
    /// Kept by the window.
    pub chrome: Chrome,
    size: Option<(Vec2, f32)>,
    focused: bool,
    input: Vec<ViewInput>,
    commands: Vec<WindowCommand>,
    close_requested: bool,
}

impl Surface {
    /// The page area in points and the window's pixels per point. True when
    /// it changed, so the root has something to deliver.
    pub fn report_size(&mut self, size: Vec2, scale: f32) -> bool {
        let valid = size.x.is_finite() && size.y.is_finite() && size.x >= 1.0 && size.y >= 1.0 && scale.is_finite() && scale > 0.0;
        valid && self.size.replace((size, scale)) != Some((size, scale))
    }

    /// Whether the window has keyboard focus. True when it changed.
    pub fn report_focus(&mut self, focused: bool) -> bool {
        std::mem::replace(&mut self.focused, focused) != focused
    }

    pub fn push_input(&mut self, input: ViewInput) {
        self.input.push(input);
    }

    pub fn command(&mut self, command: WindowCommand) {
        self.commands.push(command);
    }

    /// The window's close button.
    pub fn request_close(&mut self) {
        self.close_requested = true;
    }

    /// Something waits for the root's next pass.
    pub fn pending(&self) -> bool {
        !self.input.is_empty() || !self.commands.is_empty() || self.close_requested
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The window was asked for. Its view opens once the window reports a size.
    Opening,
    Open,
    /// The view could not be created; the window says so and can only close.
    Failed,
    /// The window asked to close and waits for the root to finish it.
    Closing,
}

/// The root's side of one second window.
pub struct Host<P> {
    id: ViewportId,
    surface: Arc<Mutex<Surface>>,
    view: Option<P>,
    phase: Phase,
    size: Option<(Vec2, f32)>,
    /// The focus last given to the view.
    focused: Option<bool>,
    /// The last pass had focus or page input.
    used: bool,
    /// Bring the window forward once it exists.
    reveal: bool,
}

impl<P: Page> Host<P> {
    pub fn new(key: &str) -> Self {
        Host {
            id: ViewportId::from_hash_of(("second-window", key)),
            surface: Arc::default(),
            view: None,
            phase: Phase::Opening,
            size: None,
            focused: None,
            used: false,
            reveal: true,
        }
    }

    pub fn id(&self) -> ViewportId {
        self.id
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn view(&self) -> Option<&P> {
        self.view.as_ref()
    }

    /// The window's pixels per point, once it has reported a size.
    pub fn scale(&self) -> Option<f32> {
        self.size.map(|(_, scale)| scale)
    }

    /// The window's handle on the shared state, for its viewport callback.
    pub fn surface(&self) -> Arc<Mutex<Surface>> {
        self.surface.clone()
    }

    pub fn lock(&self) -> MutexGuard<'_, Surface> {
        // A panic in one pass must not leave the window unusable.
        self.surface.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Take what the window reported since the last pass. Its page input goes
    /// to this host's view only. The view opens with `open` once the window
    /// has a size, and follows its size, scale and focus after that.
    pub fn pump(&mut self, open: impl FnOnce(Vec2, f32) -> Option<P>) -> Vec<WindowCommand> {
        let (size, focused, input, mut commands, close) = {
            let mut surface = self.lock();
            let close = std::mem::take(&mut surface.close_requested);
            (surface.size, surface.focused, std::mem::take(&mut surface.input), std::mem::take(&mut surface.commands), close)
        };
        self.used = focused || !input.is_empty();
        if let Some((points, scale)) = size {
            match &self.view {
                None if self.phase == Phase::Opening => {
                    self.view = open(points, scale);
                    self.phase = if self.view.is_some() { Phase::Open } else { Phase::Failed };
                    self.size = size;
                }
                Some(view) if self.size != size => {
                    view.resize(points, scale);
                    self.size = size;
                }
                _ => {}
            }
        }
        if let Some(view) = &self.view {
            if self.focused != Some(focused) {
                view.set_focus(focused);
                self.focused = Some(focused);
            }
            for input in &input { view.deliver(input); }
        }
        if close { commands.push(WindowCommand::Close); }
        if commands.contains(&WindowCommand::Close) {
            if self.phase == Phase::Closing {
                commands.retain(|command| *command != WindowCommand::Close);
            } else {
                self.phase = Phase::Closing;
            }
        }
        commands
    }

    /// Publish what the window draws. True when it changed, so the window
    /// must repaint.
    pub fn publish(&self, frame: Frame) -> bool {
        let mut surface = self.lock();
        if surface.frame == frame { return false; }
        surface.frame = frame;
        true
    }

    /// A close that could not finish: the window stays open.
    pub fn keep_open(&mut self) {
        if self.phase == Phase::Closing {
            self.phase = if self.view.is_some() { Phase::Open } else { Phase::Failed };
        }
    }

    /// Whether the window had focus or page input in the last pass.
    pub fn used(&self) -> bool {
        self.used
    }

    /// Whether the view has focus.
    pub fn focused(&self) -> bool {
        self.focused == Some(true)
    }

    /// Whether to bring the window forward now; true once, after it exists.
    pub fn take_reveal(&mut self) -> bool {
        self.size.is_some() && std::mem::take(&mut self.reveal)
    }

    /// Close the window and its view.
    pub fn close(mut self) {
        if let Some(view) = self.view.take() { view.close(); }
    }

    /// Close the window but keep its view running, for a tab to take over.
    pub fn detach(mut self) -> Option<P> {
        self.view.take()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crossbow_engine::{KeyInput, Modifiers, MouseButton, NamedKey};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A page that records what reached it.
    #[derive(Clone, Default)]
    pub(crate) struct Recorder(pub(crate) Rc<RefCell<Vec<String>>>);

    impl Recorder {
        pub(crate) fn take(&self) -> Vec<String> {
            std::mem::take(&mut self.0.borrow_mut())
        }
    }

    impl Page for Recorder {
        fn deliver(&self, input: &ViewInput) {
            let line = match input {
                ViewInput::Pointer(PointerInput::Button { x, y, pressed, mods, .. }) if *mods != Modifiers::default() => format!("button {x},{y} {pressed} {mods:?}"),
                ViewInput::Pointer(PointerInput::Button { x, y, pressed, .. }) => format!("button {x},{y} {pressed}"),
                ViewInput::Pointer(other) => format!("{other:?}"),
                ViewInput::Keyboard(WebInput::Key(key)) => format!("key {:?}{}", key.key, if key.pressed { "" } else { " up" }),
                ViewInput::Keyboard(WebInput::Text(text)) => format!("text {text}"),
                ViewInput::Keyboard(WebInput::Composition(text)) => format!("compose {text}"),
                ViewInput::Keyboard(other) => format!("{other:?}"),
                ViewInput::CaptureLost => "capture lost".into(),
            };
            self.0.borrow_mut().push(line);
        }

        fn resize(&self, size: Vec2, scale: f32) {
            self.0.borrow_mut().push(format!("resize {}x{} @{scale}", size.x, size.y));
        }

        fn set_focus(&self, focused: bool) {
            self.0.borrow_mut().push(format!("focus {focused}"));
        }

        fn close(&self) {
            self.0.borrow_mut().push("close".into());
        }
    }

    fn click(x: f32, y: f32) -> ViewInput {
        ViewInput::Pointer(PointerInput::Button { x, y, button: MouseButton::Left, pressed: true, count: 1, mods: Modifiers::default() })
    }

    fn key(key: NamedKey) -> ViewInput {
        ViewInput::Keyboard(WebInput::Key(KeyInput { key, pressed: true, repeat: false, modifiers: Modifiers::default(), text: None }))
    }

    /// Open `host` with `page` once its window reports a size.
    fn open(host: &mut Host<Recorder>, page: &Recorder, size: Vec2, scale: f32) -> Vec<WindowCommand> {
        host.lock().report_size(size, scale);
        let mut opened = None;
        let commands = host.pump(|points, s| {
            opened = Some((points, s));
            Some(page.clone())
        });
        assert_eq!(opened, Some((size, scale)), "the view opens at the window's page size and scale");
        commands
    }

    #[test]
    fn a_view_opens_only_once_its_window_reports_a_size_then_follows_resize_and_scale() {
        let page = Recorder::default();
        let mut host: Host<Recorder> = Host::new("fixture");
        assert_eq!(host.phase(), Phase::Opening);
        host.lock().push_input(key(NamedKey::Letter('A')));
        assert!(host.pump(|_, _| panic!("no view before the window exists")).is_empty());
        assert!(host.view().is_none() && host.phase() == Phase::Opening);
        assert!(!host.take_reveal(), "nothing to bring forward yet");

        open(&mut host, &page, Vec2::new(624.0, 640.0), 2.0);
        assert_eq!(host.phase(), Phase::Open);
        assert!(host.take_reveal() && !host.take_reveal(), "the new window comes forward once");
        assert_eq!(page.take(), ["focus false"], "input sent before the view existed is dropped");

        host.pump(|_, _| panic!("opens once"));
        assert!(page.take().is_empty(), "an unchanged size sends nothing");
        host.lock().report_size(Vec2::new(1344.0, 780.0), 2.0);
        host.pump(|_, _| panic!("opens once"));
        host.lock().report_size(Vec2::new(1344.0, 780.0), 1.0);
        host.pump(|_, _| panic!("opens once"));
        assert_eq!(page.take(), ["resize 1344x780 @2", "resize 1344x780 @1"], "resize and backing scale follow the window");
        host.lock().report_size(Vec2::new(0.0, 780.0), 1.0);
        host.lock().report_size(Vec2::new(10.0, 10.0), f32::NAN);
        host.pump(|_, _| panic!("opens once"));
        assert!(page.take().is_empty(), "an empty or invalid size is ignored");
    }

    #[test]
    fn input_and_focus_reach_only_the_view_of_the_window_that_received_them() {
        let (a, b) = (Recorder::default(), Recorder::default());
        let mut first: Host<Recorder> = Host::new("a");
        let mut second: Host<Recorder> = Host::new("b");
        assert_ne!(first.id(), second.id());
        assert_ne!(first.id(), ViewportId::ROOT);
        open(&mut first, &a, Vec2::new(640.0, 680.0), 2.0);
        open(&mut second, &b, Vec2::new(640.0, 680.0), 2.0);
        a.take();
        b.take();

        {
            let mut surface = first.lock();
            surface.report_focus(true);
            surface.push_input(click(12.0, 30.0));
            surface.push_input(key(NamedKey::Letter('K')));
            surface.push_input(ViewInput::Keyboard(WebInput::Composition("に".into())));
            surface.push_input(ViewInput::Keyboard(WebInput::Text("日本".into())));
            surface.push_input(ViewInput::Keyboard(WebInput::Paste));
            assert!(surface.pending());
        }
        first.pump(|_, _| None);
        second.pump(|_, _| None);
        assert_eq!(a.take(), ["focus true", "button 12,30 true", "key Letter('K')", "compose に", "text 日本", "Paste"]);
        assert!(b.take().is_empty(), "the other window's view receives nothing");
        assert!(!first.lock().pending(), "each input is delivered once");

        // Focus moves to the second window.
        first.lock().report_focus(false);
        {
            let mut surface = second.lock();
            surface.report_focus(true);
            surface.push_input(ViewInput::CaptureLost);
        }
        first.pump(|_, _| None);
        second.pump(|_, _| None);
        assert_eq!(a.take(), ["focus false"]);
        assert_eq!(b.take(), ["focus true", "capture lost"]);
    }

    #[test]
    fn close_is_asked_once_then_frees_the_view_or_keeps_the_window_open() {
        let page = Recorder::default();
        let mut host: Host<Recorder> = Host::new("fixture");
        open(&mut host, &page, Vec2::new(640.0, 680.0), 1.0);
        host.lock().command(WindowCommand::Reload);
        host.lock().request_close();
        assert_eq!(host.pump(|_, _| None), [WindowCommand::Reload, WindowCommand::Close]);
        assert_eq!(host.phase(), Phase::Closing);
        host.lock().command(WindowCommand::Close);
        assert!(host.pump(|_, _| None).is_empty(), "a second close while closing is ignored");

        // The root could not archive the page: the window stays and can retry.
        host.keep_open();
        assert_eq!(host.phase(), Phase::Open);
        host.lock().command(WindowCommand::Close);
        assert_eq!(host.pump(|_, _| None), [WindowCommand::Close]);
        page.take();
        host.close();
        assert_eq!(page.take(), ["close"], "closing frees the view once");

        // Open in Space keeps the view running for its new tab.
        let moved = Recorder::default();
        let mut host: Host<Recorder> = Host::new("moved");
        open(&mut host, &moved, Vec2::new(640.0, 680.0), 1.0);
        moved.take();
        let view = host.detach().expect("the live view is handed over");
        assert!(moved.take().is_empty(), "a detached view is not closed");
        drop(view);
    }

    #[test]
    fn a_view_that_cannot_open_fails_once_and_the_window_can_still_close() {
        let mut host: Host<Recorder> = Host::new("fixture");
        host.lock().report_size(Vec2::new(640.0, 680.0), 1.0);
        let mut attempts = 0;
        host.pump(|_, _| { attempts += 1; None });
        host.pump(|_, _| { attempts += 1; None });
        assert_eq!((attempts, host.phase()), (1, Phase::Failed), "no retry on every pass");
        host.lock().request_close();
        assert_eq!(host.pump(|_, _| None), [WindowCommand::Close]);
        host.keep_open();
        assert_eq!(host.phase(), Phase::Failed);
        host.close();
    }

    #[test]
    fn a_frame_is_published_only_when_it_changes() {
        let host: Host<Recorder> = Host::new("fixture");
        let frame = Frame { page: PageState { url: "https://example.com/".into(), ..Default::default() }, ..Default::default() };
        assert!(host.publish(frame.clone()));
        assert!(!host.publish(frame.clone()), "the window repaints only for a change");
        let painted = Frame { texture: Some((TextureId::User(4), Vec2::new(1280.0, 1360.0), 2)), ..frame };
        assert!(host.publish(painted.clone()), "a new paint repaints the window");
        assert_eq!(host.lock().frame, painted);
    }
}
