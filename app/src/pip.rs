//! Crossbow's own Picture in Picture: a small floating window showing one
//! tab's video, cut from the tab's own frames, with controls that go to the
//! page. Chromium's own can't open from a windowless browser.
//!
//! [`Pip`] decides from each pass's [`TabState`]s when the player opens and
//! closes; the app applies its [`Effect`]s to the tabs' views and keeps the
//! player's window. Only one player is open at a time.

use super::CrossbowApp;
use crate::link_prefs::{DeviceSwitch, SwitchSpec};
use crate::ui::pip::{PlayerFrame, PlayerInput, PlayerSurface, Status};
use crossbow_core::store::Store;
use crossbow_engine::{MediaCommand, MediaReport, Video};
use egui::{Rect, Vec2, ViewportBuilder, ViewportId, ViewportIdMap, ViewportInfo, pos2, vec2};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The tab menu's and command bar's name for it.
pub const PICTURE_IN_PICTURE: &str = "Picture in Picture";
/// The player window's title, which also finds its native window.
pub const PLAYER_TITLE: &str = "Crossbow Picture in Picture";
/// How long the player says "Video ended" before it closes.
pub const ENDED_FOR: Duration = Duration::from_secs(3);
/// A page with a video reports every second. After this long without one,
/// the video is gone.
pub const STALE_AFTER: Duration = Duration::from_secs(3);
/// Frames a second for a tab that only the player shows, and otherwise.
pub const PLAYER_FPS: i32 = 30;
pub const FULL_FPS: i32 = 60;
pub const SEEK_SECONDS: f64 = 10.0;

/// One tab, as the player needs it this pass.
#[derive(Clone, Copy, Debug)]
pub struct TabState<'a> {
    pub id: &'a str,
    /// Shown in the main window.
    pub visible: bool,
    /// Has a live page, not unloaded or discarded.
    pub loaded: bool,
    pub fullscreen: bool,
    /// Muted by Crossbow.
    pub muted: bool,
    /// Its profile opens the player on leaving the tab.
    pub automatic: bool,
    /// The view's count of top-frame loads.
    pub navigations: u64,
    pub media: Option<(&'a MediaReport, Instant)>,
}

impl TabState<'_> {
    /// Whether the tab menu and command bar offer Picture in Picture.
    pub fn has_video(&self) -> bool {
        self.loaded && self.media.is_some_and(|(report, _)| !report.videos.is_empty())
    }
}

/// What the app does to a tab's view or window.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Command { tab: String, command: MediaCommand },
    FrameRate { tab: String, fps: i32 },
    /// Back to Tab: show the tab in the main window, in its Space.
    ShowTab(String),
}

/// The player's controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    PlayPause,
    Back,
    Forward,
    Mute,
    BackToTab,
    Close,
}

/// Where the page was before the player scrolled its video into view.
#[derive(Clone, Debug, PartialEq)]
struct Restore {
    scroll: (f64, f64),
    page: u64,
    navigations: u64,
    video: u32,
}

impl Restore {
    /// The command that puts the page back, if it is still the same page
    /// with the same video. Otherwise nothing is said or done.
    fn command(&self, state: &TabState) -> Option<MediaCommand> {
        let (report, _) = state.media?;
        let same = state.loaded && state.navigations == self.navigations && report.page == self.page && report.video(self.video).is_some();
        same.then_some(MediaCommand::Restore { x: self.scroll.0, y: self.scroll.1 })
    }
}

#[derive(Clone, Debug)]
pub struct Player {
    pub tab: String,
    pub video: u32,
    page: u64,
    opened: Instant,
    /// The tab left the main window while the player was open. Until then
    /// the player was asked for in view, and coming back closes nothing.
    left: bool,
    /// When the video was first seen ended or gone.
    ended: Option<Instant>,
    restore: Option<Restore>,
    /// The tab's view was told to paint at [`PLAYER_FPS`].
    throttled: bool,
}

/// The player's video as the page last reported it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Playing<'a> {
    Video(&'a MediaReport, &'a Video),
    /// Ended, removed or navigated away: "Video ended".
    Ended,
}

impl Player {
    /// The video, from a report no older than [`STALE_AFTER`] from the same
    /// page. A stale report counts from when the player opened, since a
    /// hidden tab reports less often.
    pub fn playing<'a>(&self, state: &TabState<'a>, now: Instant) -> Playing<'a> {
        let Some((report, at)) = state.media else { return Playing::Ended };
        if now.saturating_duration_since(at.max(self.opened)) > STALE_AFTER || report.page != self.page { return Playing::Ended; }
        match report.video(self.video) {
            Some(video) if !video.ended => Playing::Video(report, video),
            _ => Playing::Ended,
        }
    }
}

#[derive(Debug, Default)]
pub struct Pip {
    pub player: Option<Player>,
    /// Tabs in the main window on the last pass.
    shown: HashSet<String>,
    /// Scroll positions to put back when each tab is next in view.
    restores: HashMap<String, Restore>,
}

fn fresh<'a>(state: &TabState<'a>, now: Instant) -> Option<&'a MediaReport> {
    let (report, at) = state.media?;
    (now.saturating_duration_since(at) <= STALE_AFTER).then_some(report)
}

impl Pip {
    /// The tab whose page keeps painting for the player.
    pub fn painting(&self) -> Option<&str> {
        self.player.as_ref().map(|player| player.tab.as_str())
    }

    /// One pass: close the player when its tab is back in view, closed,
    /// unloaded or full screen, or its video is over; open one for a tab
    /// that just left the main window with a video playing audibly; put
    /// back scroll positions for tabs back in view.
    pub fn update(&mut self, tabs: &[TabState], now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        if let Some(player) = &mut self.player {
            let state = tabs.iter().find(|state| state.id == player.tab);
            let close = match state {
                None => Some(false),
                Some(state) if !state.loaded => Some(false),
                Some(state) if state.fullscreen => Some(true),
                Some(state) => {
                    if !state.visible { player.left = true; }
                    if state.visible && player.left { Some(true) }
                    else if player.playing(state, now) == Playing::Ended {
                        let ended = *player.ended.get_or_insert(now);
                        (now.saturating_duration_since(ended) >= ENDED_FOR).then_some(true)
                    } else {
                        player.ended = None;
                        if player.throttled == state.visible {
                            player.throttled = !state.visible;
                            effects.push(Effect::FrameRate { tab: player.tab.clone(), fps: if player.throttled { PLAYER_FPS } else { FULL_FPS } });
                        }
                        None
                    }
                }
            };
            if let Some(restore) = close { self.close(restore, &mut effects); }
        }
        let leaving = tabs.iter()
            .filter(|state| self.shown.contains(state.id) && !state.visible && state.automatic && state.loaded && !state.fullscreen)
            .filter(|state| self.painting() != Some(state.id))
            .filter_map(|state| { let report = fresh(state, now)?; Some((state, report, report.automatic(state.muted)?)) })
            .max_by(|a, b| (a.2.rect.width * a.2.rect.height).total_cmp(&(b.2.rect.width * b.2.rect.height)));
        if let Some((state, report, video)) = leaving {
            let video = video.id;
            self.open(state, report, video, now, &mut effects);
        }
        self.restores.retain(|tab, restore| {
            let Some(state) = tabs.iter().find(|state| state.id == tab) else { return false };
            if !state.visible { return true; }
            if let Some(command) = restore.command(state) { effects.push(Effect::Command { tab: tab.clone(), command }); }
            false
        });
        self.shown = tabs.iter().filter(|state| state.visible).map(|state| state.id.to_owned()).collect();
        effects
    }

    /// Picture in Picture asked for by name. Nothing when the tab has no
    /// video, no page or is full screen.
    pub fn open_manual(&mut self, state: &TabState, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        if !state.loaded || state.fullscreen { return effects; }
        let Some((report, _)) = state.media else { return effects };
        let Some(video) = report.manual(state.muted) else { return effects };
        let video = video.id;
        self.open(state, report, video, now, &mut effects);
        effects
    }

    /// A player for `video`, replacing any other. A video not wholly in view
    /// is scrolled there, centred, and the page's position kept to put back.
    fn open(&mut self, state: &TabState, report: &MediaReport, video: u32, now: Instant, effects: &mut Vec<Effect>) {
        // The old player's tab keeps its video playing, as on leaving any tab.
        self.close(true, effects);
        let restore = report.video(video).filter(|shown| !report.in_view(shown)).map(|_| {
            effects.push(Effect::Command { tab: state.id.to_owned(), command: MediaCommand::Reveal(video) });
            Restore { scroll: report.scroll, page: report.page, navigations: state.navigations, video }
        });
        let throttled = !state.visible;
        if throttled { effects.push(Effect::FrameRate { tab: state.id.to_owned(), fps: PLAYER_FPS }); }
        self.player = Some(Player { tab: state.id.to_owned(), video, page: report.page, opened: now, left: !state.visible, ended: None, restore, throttled });
    }

    /// Close the player. `restore`: put the page back when its tab is next in
    /// view, if the player had moved it and the tab was left.
    fn close(&mut self, restore: bool, effects: &mut Vec<Effect>) {
        let Some(player) = self.player.take() else { return };
        if player.throttled { effects.push(Effect::FrameRate { tab: player.tab.clone(), fps: FULL_FPS }); }
        if let Some(kept) = player.restore.filter(|_| restore && player.left) { self.restores.insert(player.tab, kept); }
    }

    /// A control in the player. Each sends at most one command to the page.
    pub fn control(&mut self, control: Control, tabs: &[TabState], now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        let Some(player) = &mut self.player else { return effects };
        let (tab, id) = (player.tab.clone(), player.video);
        let video = match tabs.iter().find(|state| state.id == tab).map(|state| player.playing(state, now)) {
            Some(Playing::Video(_, video)) => Some(video.clone()),
            _ => None,
        };
        let command = match control {
            Control::PlayPause => video.map(|video| if video.paused { MediaCommand::Play(id) } else { MediaCommand::Pause(id) }),
            Control::Back => video.map(|_| MediaCommand::Seek { video: id, seconds: -SEEK_SECONDS }),
            Control::Forward => video.map(|_| MediaCommand::Seek { video: id, seconds: SEEK_SECONDS }),
            Control::Mute => video.map(|video| MediaCommand::SetMuted { video: id, muted: !video.muted }),
            Control::Close => video.map(|_| MediaCommand::Pause(id)),
            Control::BackToTab => {
                // Put the page back as soon as it is in view.
                player.left = true;
                effects.push(Effect::ShowTab(tab.clone()));
                None
            }
        };
        if let Some(command) = command { effects.push(Effect::Command { tab, command }); }
        if matches!(control, Control::Close | Control::BackToTab) { self.close(true, &mut effects); }
        effects
    }
}

// ---------------------------------------------------------------------------
// Geometry, in points with the origin at the primary screen's top left
// ---------------------------------------------------------------------------

pub const DEFAULT_SIZE: Vec2 = vec2(320.0, 180.0);
pub const MIN_SIZE: Vec2 = vec2(240.0, 135.0);
/// From the screen's edges, past the Dock and menu bar.
const MARGIN: f32 = 16.0;
const FRAME_KEY: &str = "pip_player_frame";

fn usable(rect: Rect) -> bool {
    [rect.min.x, rect.min.y, rect.max.x, rect.max.y].iter().all(|value| value.is_finite()) && rect.width() >= 1.0 && rect.height() >= 1.0
}

/// A 16:9 size about `width` wide, no smaller than the minimum and no larger
/// than `room`.
pub fn fit(width: f32, room: Vec2) -> Vec2 {
    let most = room.x.min(room.y * 16.0 / 9.0).max(1.0);
    let width = if width.is_finite() { width } else { DEFAULT_SIZE.x };
    let width = width.max(MIN_SIZE.x).min(most);
    vec2(width, width * 9.0 / 16.0)
}

/// The default place: the bottom-right corner of the main window's screen.
pub fn default_frame(screen: Rect) -> Rect {
    let size = fit(DEFAULT_SIZE.x, screen.size() - vec2(2.0 * MARGIN, 2.0 * MARGIN));
    let min = (screen.max - size - vec2(MARGIN, MARGIN)).max(screen.min);
    Rect::from_min_size(min, size)
}

/// Where the player goes: `saved`, kept wholly on the screen it overlaps
/// most, or else the default place on `home`. `screens` are the work areas,
/// without the menu bar and Dock.
pub fn place(saved: Option<Rect>, home: Rect, screens: &[Rect]) -> Rect {
    let Some(saved) = saved.filter(|saved| usable(*saved)) else { return default_frame(home) };
    let overlap = |screen: &Rect| { let shared = screen.intersect(saved); if shared.is_positive() { shared.area() } else { 0.0 } };
    let Some(screen) = screens.iter().copied().filter(|screen| usable(*screen)).filter(|screen| overlap(screen) > 0.0).max_by(|a, b| overlap(a).total_cmp(&overlap(b))) else {
        return default_frame(home);
    };
    let size = fit(saved.width(), screen.size());
    let min = saved.min.clamp(screen.min, (screen.max - size).max(screen.min));
    Rect::from_min_size(min, size)
}

/// Whether `frame` is wholly on one of `screens`.
pub fn on_screen(frame: Rect, screens: &[Rect]) -> bool {
    screens.iter().any(|screen| screen.expand(0.5).contains_rect(frame))
}

/// The player's last place on this device, if it reads correctly.
pub fn load_frame(store: &Store) -> Option<Rect> {
    let raw = store.kv_get(FRAME_KEY).ok()??;
    let values: Vec<f32> = raw.split(' ').map(str::parse).collect::<Result<_, _>>().ok()?;
    let [x, y, width, height] = values[..] else { return None };
    let frame = Rect::from_min_size(pos2(x, y), vec2(width, height));
    usable(frame).then_some(frame)
}

pub fn save_frame(store: &Store, frame: Rect) -> crossbow_core::store::Result<()> {
    store.kv_set(FRAME_KEY, &format!("{} {} {} {}", frame.min.x, frame.min.y, frame.width(), frame.height()))
}

// ---------------------------------------------------------------------------
// Choices
// ---------------------------------------------------------------------------

/// Arc's Automatic Picture-in-Picture. On by default, as in Arc.
#[derive(Debug)]
pub struct AutomaticPip;

impl SwitchSpec for AutomaticPip {
    const KEY: &'static str = "pip_automatic";
    const DEFAULT: bool = true;
    const TITLE: &'static str = "Automatic Picture-in-Picture";
    const DESCRIPTION: &'static str = "When you leave a tab whose video plays with sound, it keeps playing in a small floating window. Going back to the tab closes the window.";
    const STILL_OFF: &'static str = "Videos still stay in their tabs";
    const STILL_ON: &'static str = "Videos still float when you leave their tab";
    const UNREADABLE: &'static str = "The saved choice for Picture-in-Picture could not be read, so videos float. Changing it saves over it.";
}

/// The player floats above other apps' windows.
#[derive(Debug)]
pub struct PlayerOnTop;

impl SwitchSpec for PlayerOnTop {
    const KEY: &'static str = "pip_keep_on_top";
    const DEFAULT: bool = true;
    const TITLE: &'static str = "Keep on Top";
    const DESCRIPTION: &'static str = "The Picture in Picture window stays above other apps' windows.";
    const STILL_OFF: &'static str = "The player still stays behind other apps";
    const STILL_ON: &'static str = "The player still stays on top";
    const UNREADABLE: &'static str = "The saved choice for Keep on Top could not be read, so the player stays on top. Changing it saves over it.";
}

/// Little Windows float above other apps' windows.
#[derive(Debug)]
pub struct LittleOnTop;

impl SwitchSpec for LittleOnTop {
    const KEY: &'static str = "little_windows_keep_on_top";
    const DEFAULT: bool = false;
    const TITLE: &'static str = "Keep Window on Top";
    const DESCRIPTION: &'static str = "Little Windows stay above other apps' windows.";
    const STILL_OFF: &'static str = "Little Windows still stay behind other apps";
    const STILL_ON: &'static str = "Little Windows still stay on top";
    const UNREADABLE: &'static str = "The saved choice for Keep Window on Top could not be read. Changing it saves over it.";
}

/// Picture in Picture choices kept on this device only, read once each.
#[derive(Debug, Default)]
pub struct PipPrefs {
    automatic: HashMap<String, DeviceSwitch<AutomaticPip>>,
    player_on_top: Option<DeviceSwitch<PlayerOnTop>>,
    little_on_top: Option<DeviceSwitch<LittleOnTop>>,
}

impl PipPrefs {
    /// `profile`'s own choice, apart from other profiles' on this device.
    pub fn automatic_switch(&mut self, store: &Store, profile: &str) -> &mut DeviceSwitch<AutomaticPip> {
        if !self.automatic.contains_key(profile) { self.automatic.insert(profile.to_owned(), DeviceSwitch::load_for(store, profile)); }
        self.automatic.get_mut(profile).expect("inserted above")
    }

    pub fn automatic(&mut self, store: &Store, profile: &str) -> bool {
        self.automatic_switch(store, profile).enabled()
    }

    pub fn player_on_top(&mut self, store: &Store) -> &mut DeviceSwitch<PlayerOnTop> {
        self.player_on_top.get_or_insert_with(|| DeviceSwitch::load(store))
    }

    pub fn little_on_top(&mut self, store: &Store) -> &mut DeviceSwitch<LittleOnTop> {
        self.little_on_top.get_or_insert_with(|| DeviceSwitch::load(store))
    }
}

// ---------------------------------------------------------------------------
// The app's side: the tabs' views and the player's window
// ---------------------------------------------------------------------------

/// The one player window: a new player replaces what it shows.
pub const PLAYER_VIEWPORT: &str = "picture-in-picture";
/// Where a screen's size is unknown.
const FALLBACK_SCREEN: Rect = Rect { min: pos2(0.0, 0.0), max: pos2(1440.0, 900.0) };

pub fn player_id() -> ViewportId {
    ViewportId::from_hash_of(PLAYER_VIEWPORT)
}

/// The player's native window, while a player is open.
struct Window {
    surface: Arc<Mutex<PlayerSurface>>,
    /// Where it opened. Moving it back on screen uses commands, so the
    /// person's own moves are never undone.
    opened: Rect,
    screens: Vec<Rect>,
    /// Its Spaces behaviour and aspect ratio are set.
    native: bool,
}

impl Window {
    fn lock(&self) -> MutexGuard<'_, PlayerSurface> {
        self.surface.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Picture in Picture as the app keeps it.
#[derive(Default)]
pub struct PipHost {
    pub pip: Pip,
    pub prefs: PipPrefs,
    window: Option<Window>,
    /// The player's viewport while it is open, for the engine's waker: frames
    /// a hidden main window would wait for still reach the player.
    pub wake: Arc<Mutex<Option<ViewportId>>>,
    /// A tab asked for by name since the last pass.
    pub request: Option<String>,
    /// The player's last place, read once from the store.
    saved: Option<Option<Rect>>,
}

impl PipHost {
    fn set_wake(&self, id: Option<ViewportId>) {
        *self.wake.lock().unwrap_or_else(PoisonError::into_inner) = id;
    }

    /// Keep the window's last place for the next player and the next launch.
    fn remember(&mut self, store: &Store) {
        let Some(window) = self.window.take() else { return };
        let Some(frame) = window.lock().outer.filter(|frame| usable(*frame)) else { return };
        self.saved = Some(Some(frame));
        if let Err(error) = save_frame(store, frame) { log::warn!("Could not save where Picture in Picture was: {error}"); }
    }
}

/// eframe paints a hidden main window only every 100 ms, and the engine is
/// pumped from its passes. While the player shows a video, its own passes
/// pump the engine then.
pub fn root_hidden(viewports: &ViewportIdMap<ViewportInfo>) -> bool {
    viewports.get(&ViewportId::ROOT).and_then(ViewportInfo::visible) == Some(false)
}

fn player_viewport(opened: Rect, on_top: bool) -> ViewportBuilder {
    crate::ui::window::window_chrome(ViewportBuilder::default()
        .with_title(PLAYER_TITLE)
        .with_position(opened.min)
        .with_inner_size(opened.size())
        .with_min_inner_size(MIN_SIZE)
        .with_resizable(true)
        .with_titlebar_buttons_shown(false)
        // Never takes focus from what the person is doing.
        .with_active(false)
        .with_window_level(if on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal }))
}

fn to_rect(rect: crate::native_window::DipRect) -> Rect {
    Rect::from_min_size(pos2(rect.x as f32, rect.y as f32), vec2(rect.width as f32, rect.height as f32))
}

impl CrossbowApp {
    /// Whether the tab menu and command bar offer Picture in Picture.
    pub(crate) fn tab_has_video(&self, tab: &str) -> bool {
        let Some(tab) = self.tab(tab) else { return false };
        let media = tab.view.as_ref().and_then(crossbow_engine::WebView::media);
        let state = TabState {
            id: &tab.id, visible: false, loaded: tab.view.is_some(), fullscreen: tab.nav.fullscreen, muted: tab.muted,
            automatic: false, navigations: 0, media: media.as_ref().map(|(report, at)| (report, *at)),
        };
        state.has_video()
    }

    /// One pass of Picture in Picture, after the panes are laid out and
    /// actions applied: the player's controls, opening and closing it, and
    /// its window.
    pub(super) fn update_pip(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let visible: HashSet<&str> = self.ui.panes.iter().map(|(id, _)| id.as_str()).collect();
        let reports: Vec<_> = self.tabs.iter().map(|tab| tab.view.as_ref().and_then(crossbow_engine::WebView::media)).collect();
        let (lib, prefs) = (&self.lib, &mut self.pip.prefs);
        let automatic: Vec<bool> = self.tabs.iter().map(|tab| super::tab_profile(lib, tab).is_some_and(|profile| prefs.automatic(&lib.store, profile))).collect();
        let states: Vec<TabState> = self.tabs.iter().zip(&reports).zip(&automatic).map(|((tab, media), automatic)| TabState {
            id: &tab.id,
            visible: visible.contains(tab.id.as_str()),
            loaded: tab.view.is_some(),
            fullscreen: tab.nav.fullscreen,
            muted: tab.muted,
            automatic: *automatic,
            navigations: tab.view.as_ref().map_or(0, crossbow_engine::WebView::navigations),
            media: media.as_ref().map(|(report, at)| (report, *at)),
        }).collect();

        let mut effects = Vec::new();
        let mut on_top = None;
        let inputs = self.pip.window.as_ref().map(|window| window.lock().take()).unwrap_or_default();
        for input in inputs {
            match input {
                PlayerInput::Control(control) => effects.extend(self.pip.pip.control(control, &states, now)),
                PlayerInput::KeepOnTop(wanted) => on_top = Some(wanted),
            }
        }
        if let Some(tab) = self.pip.request.take() && let Some(state) = states.iter().find(|state| state.id == tab) {
            effects.extend(self.pip.pip.open_manual(state, now));
        }
        effects.extend(self.pip.pip.update(&states, now));
        let playing = self.pip.pip.player.as_ref().and_then(|player| {
            let state = states.iter().find(|state| state.id == player.tab)?;
            match player.playing(state, now) {
                Playing::Video(report, video) => Some((report.clone(), video.clone())),
                Playing::Ended => None,
            }
        });
        drop(states);

        for effect in effects {
            match effect {
                Effect::Command { tab, command } => if let Some(view) = self.tab(&tab).and_then(|tab| tab.view.as_ref()) { view.media_command(&command); },
                Effect::FrameRate { tab, fps } => if let Some(view) = self.tab(&tab).and_then(|tab| tab.view.as_ref()) { view.set_frame_rate(fps); },
                Effect::ShowTab(tab) => {
                    self.activate(&tab);
                    crate::links::unhide_app();
                    for command in [egui::ViewportCommand::Visible(true), egui::ViewportCommand::Minimized(false), egui::ViewportCommand::Focus] {
                        ctx.send_viewport_cmd_to(ViewportId::ROOT, command);
                    }
                }
            }
        }
        let store = &self.lib.store;
        if let Some(wanted) = on_top {
            let switch = self.pip.prefs.player_on_top(store);
            switch.choose(store, wanted);
            if let Some(failure) = &switch.failure { log::warn!("Could not save Keep on Top: {}", failure.error); }
        }
        self.show_player(ctx, playing);
    }

    /// Publish the player's frame and keep its window for this pass; a pass
    /// without a player closes it.
    fn show_player(&mut self, ctx: &egui::Context, playing: Option<(MediaReport, Video)>) {
        let Some(tab) = self.pip.pip.player.as_ref().map(|player| player.tab.clone()) else {
            self.pip.remember(&self.lib.store);
            self.pip.set_wake(None);
            return;
        };
        let screens: Vec<Rect> = crate::native_window::work_areas().into_iter().map(to_rect).collect();
        let home = self.display_snapshot.map(|display| to_rect(display.work_area)).or_else(|| screens.first().copied()).unwrap_or(FALLBACK_SCREEN);
        if self.pip.window.is_none() {
            let saved = *self.pip.saved.get_or_insert_with(|| load_frame(&self.lib.store));
            let opened = place(saved, home, &screens);
            self.pip.window = Some(Window { surface: Arc::default(), opened, screens: screens.clone(), native: false });
        }
        let texture = self.tab(&tab).and_then(|tab| tab.view.clone()).and_then(|view| self.view_texture(&tab, &view));
        let (status, crop, paused, muted) = match &playing {
            Some((report, video)) => {
                let crop = texture.and_then(|(_, size)| report.crop(video, (size.x as u32, size.y as u32)))
                    .map(|crop| Rect::from_min_size(pos2(crop.x, crop.y), vec2(crop.width, crop.height)));
                (if crop.is_some() { Status::Playing } else { Status::OutOfView }, crop, video.paused, video.muted)
            }
            None => (Status::Ended, None, false, false),
        };
        let switch = self.pip.prefs.player_on_top(&self.lib.store);
        let on_top = switch.enabled();
        let notice = switch.failure.as_ref().map(|_| "Couldn't save Keep on Top. Check disk space, then try again.".to_string());
        let id = player_id();
        let window = self.pip.window.as_mut().expect("opened above");
        // A display came or went: back wholly onto a screen if it is now off one.
        if screens != window.screens {
            window.screens = screens.clone();
            if let Some(outer) = window.lock().outer.filter(|outer| !on_screen(*outer, &screens)) {
                let moved = place(Some(outer), home, &screens);
                ctx.send_viewport_cmd_to(id, egui::ViewportCommand::OuterPosition(moved.min));
                ctx.send_viewport_cmd_to(id, egui::ViewportCommand::InnerSize(moved.size()));
            }
        }
        if !window.native { window.native = crate::native_window::float_everywhere(PLAYER_TITLE); }
        let frame = PlayerFrame { texture, crop, status, paused, muted, on_top, notice };
        if window.lock().publish(frame) { ctx.request_repaint_of(id); }
        let surface = window.surface.clone();
        let builder = player_viewport(window.opened, on_top);
        self.pip.set_wake(Some(id));
        ctx.show_viewport_deferred(id, builder, move |ui, _| {
            if ui.ctx().input(|input| root_hidden(&input.raw.viewports)) { crossbow_engine::pump(); }
            let mut surface = surface.lock().unwrap_or_else(PoisonError::into_inner);
            crate::ui::pip::show(ui, &mut surface);
        });
    }

    /// On quit: keep where the player was.
    pub(super) fn close_pip(&mut self) {
        self.pip.remember(&self.lib.store);
        self.pip.set_wake(None);
    }
}

#[cfg(test)]
#[path = "pip_tests.rs"]
mod tests;
