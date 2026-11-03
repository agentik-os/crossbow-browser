//! Little Windows: small windows that each hold one page, opened with New
//! Little Window or, when chosen in Settings → Links, by links from other
//! apps. Each belongs to a Space and is opened under its profile. Open in
//! Space moves its page into the main window; closing it archives the page.

use super::*;
use crate::window_host::{Frame, Host, Page, PageState, Phase, SpaceChoice, WindowCommand};
use crossbow_core::link_routing::{Routing, usable_space};
use crossbow_core::site_profiles::SiteProfiles;
use super::link_open::{Placed, add_first_today_tab, place_link};

pub(crate) const LINK_KEY: &str = "links_open_in_little_window";
pub const LINK_TITLE: &str = "Open links from other apps in a Little Window";
const NO_LITTLE_WINDOWS: &str = "No Little Windows are open.";

/// Settings → Links → Open links from other apps in a Little Window: a
/// device-local preference, off by default.
pub type LinkWindow = crate::link_prefs::DeviceSwitch<crate::link_prefs::OtherAppLinks>;

/// One Little Window.
pub(crate) struct Little<P = WebView> {
    /// Stable while the window is open; also its texture key.
    pub key: String,
    /// The Space its page belongs to.
    pub space: String,
    /// The profile its view opens in. It never changes.
    pub profile: String,
    pub url: String,
    pub title: String,
    pub host: Host<P>,
    /// Why the last close, move or load did not work.
    pub notice: Option<String>,
    /// When the window last had focus or page input; an unused window is
    /// archived after the delay chosen in Settings → Links.
    pub last_active_ms: u64,
}

impl<P: Page> Little<P> {
    /// A window for `url` in `space`, under that Space's profile. None when
    /// the Space can't open pages.
    pub fn open(lib: &Library, space: &str, url: &str) -> Option<Self> {
        if !usable_space(lib, space) { return None; }
        let key = format!("little:{}", new_id());
        Some(Little {
            host: Host::new(&key),
            key,
            space: space.into(),
            profile: lib.spaces[space].profile_id.clone(),
            url: url.into(),
            title: String::new(),
            notice: None,
            last_active_ms: now_ms(),
        })
    }
}

impl<P> Little<P> {
    fn display_title(&self) -> String {
        if !self.title.is_empty() { self.title.clone() }
        else if self.url.is_empty() || self.url == NEW_TAB_URL { "Little Window".into() }
        else { crossbow_core::search::display_url(&self.url) }
    }
}

/// The Space a new Little Window belongs to: the default destination for
/// links from other apps, which is the Space in use unless one is chosen.
pub(crate) fn new_window_space(lib: &Library, routing: &Routing, most_recent: &str) -> Option<String> {
    routing.default_space(lib, most_recent).map(|(space, _)| space)
}

/// Where a link from another app opened.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Delivered {
    Tab(Placed),
    Window { key: String, space: String },
}

/// Open a link from another app as a tab or, with `in_window`, in a new
/// Little Window. Routing and site rules pick the Space, and with it the
/// profile, either way. None when no Space can take it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn deliver_link<P: Page>(lib: &Library, routing: &Routing, sites: &SiteProfiles, in_window: bool, tabs: &mut Vec<Tab>, windows: &mut Vec<Little<P>>, most_recent: &str, url: &str) -> Option<Delivered> {
    if !in_window {
        return place_link(lib, routing, sites, tabs, most_recent, url).map(Delivered::Tab);
    }
    let route = super::site_open::route_link(lib, routing, sites, tabs, most_recent, url)?;
    let window = Little::open(lib, &route.space, url)?;
    let delivered = Delivered::Window { key: window.key.clone(), space: window.space.clone() };
    windows.push(window);
    Some(delivered)
}

/// How Open in Space moves a page.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Move {
    /// The Space uses the page's profile: the live page moves as it is.
    Live,
    /// The Space uses another profile: the page reopens there, after confirmation.
    Reopen,
}

pub(crate) fn plan_move(lib: &Library, profile: &str, space: &str, reopen: bool) -> Result<Move, &'static str> {
    if !usable_space(lib, space) {
        return Err("That Space is no longer available, so the page stayed here. Choose another Space.");
    }
    if lib.spaces[space].profile_id == profile { return Ok(Move::Live); }
    if reopen { Ok(Move::Reopen) } else { Err("That Space uses another profile. Confirm to reopen the page there.") }
}

/// The Space a closing window's page is archived in: its own while that
/// still uses its profile, otherwise the first Space that does.
fn archive_space<P>(lib: &Library, window: &Little<P>) -> Option<String> {
    let owned = |space: &str| lib.spaces.get(space).is_some_and(|owner| owner.profile_id == window.profile);
    if owned(&window.space) { return Some(window.space.clone()); }
    lib.spaces_ordered().into_iter().map(|(id, _)| id).find(|id| owned(id))
}

/// Put a closing window's page in the archive with its profile, marked as a
/// Little Window's, so Reopen Closed restores it like a closed tab.
/// Ok(false): nothing to archive (an empty page, or no Space uses its
/// profile any more).
pub(crate) fn archive_window<P>(lib: &Library, window: &Little<P>, now: u64) -> crossbow_core::store::Result<bool> {
    if window.url.is_empty() || window.url == NEW_TAB_URL { return Ok(false); }
    let Some(space) = archive_space(lib, window) else { return Ok(false) };
    lib.store.archive_page(&window.profile, &window.url, &window.display_title(), &space, now, crossbow_core::store::ArchiveSource::LittleWindow)?;
    Ok(true)
}

/// Close window `index`, archiving its page first, and return its key. If
/// archiving fails the window stays open and says why.
pub(crate) fn close_window<P: Page>(lib: &Library, windows: &mut Vec<Little<P>>, index: usize, now: u64) -> Option<String> {
    match archive_window(lib, &windows[index], now) {
        Ok(archived) => {
            let little = windows.remove(index);
            if !archived { log::info!("Closed a Little Window without an archive entry"); }
            let key = little.key.clone();
            little.host.close();
            Some(key)
        }
        Err(error) => {
            log::warn!("Could not archive a Little Window's page; it stays open: {error}");
            let little = &mut windows[index];
            little.notice = Some("Could not archive this page, so its window stays open. Close it again to retry.".into());
            little.host.keep_open();
            None
        }
    }
}

/// Close All Little Windows: each closes as a single close does, archiving
/// its page first, oldest first, so Reopen Closed brings back the newest.
/// A window whose page could not be archived stays open and says why.
/// Returns the closed windows' keys.
pub(crate) fn close_all<P: Page>(lib: &Library, windows: &mut Vec<Little<P>>, now: u64) -> Vec<String> {
    let mut closed = Vec::new();
    let mut index = 0;
    while index < windows.len() {
        match close_window(lib, windows, index, now) {
            Some(key) => closed.push(key),
            None => index += 1,
        }
    }
    closed
}

/// Where a link a page asked to open goes, by the keys held for the click.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Click {
    LittleWindow,
    Peek,
    /// As before: a tab, or Peek for popups and pinned tabs' outside links.
    Usual,
}

/// Chromium asks for a background tab on ⌥⌘-click, as on ⌘-click, and for
/// a new window on Shift-click; only the held keys tell them apart.
pub(crate) fn click_target(modifiers: crossbow_engine::Modifiers, background: bool, popup: bool, option_click: bool, shift_peek: bool) -> Click {
    let held = |shift: bool, alt: bool, command: bool| modifiers.shift == shift && modifiers.alt == alt && modifiers.command == command && !modifiers.ctrl;
    if option_click && background && held(false, true, true) {
        Click::LittleWindow
    } else if shift_peek && !background && !popup && held(true, false, false) {
        Click::Peek
    } else {
        Click::Usual
    }
}

/// The Space a link ⌥⌘-clicked in `tab` opens in: the tab's own. A favorite
/// has none, so the Space in use or else the first Space of its profile.
pub(crate) fn little_space_for_tab(lib: &Library, tab: &Tab, active_space: &str) -> Option<String> {
    let profile = tab_profile(lib, tab)?;
    let owned = |space: &str| usable_space(lib, space) && lib.spaces.get(space).is_some_and(|owner| owner.profile_id == profile);
    if tab.space_id != FAVORITES_SPACE {
        return owned(&tab.space_id).then(|| tab.space_id.clone());
    }
    if owned(active_space) { return Some(active_space.to_string()); }
    lib.spaces_ordered().into_iter().map(|(id, _)| id).find(|id| owned(id))
}

/// Count focus or page input since the last pass as use at `now`.
pub(crate) fn note_use<P: Page>(little: &mut Little<P>, now: u64) {
    if little.host.used() { little.last_active_ms = now; }
}

/// Windows unused for longer than `after_ms` at `now`. Never the focused
/// one, and only when its page allows an automatic close, as for tabs.
pub(crate) fn idle_windows<P: Page>(windows: &[Little<P>], after_ms: u64, now: u64, can_close: impl Fn(&P) -> bool) -> Vec<String> {
    windows.iter()
        .filter(|little| !little.host.focused() && little.last_active_ms < now.saturating_sub(after_ms) && little.host.view().is_none_or(&can_close))
        .map(|little| little.key.clone())
        .collect()
}

/// Archive and close unused windows as a close does. A window whose page
/// could not be archived stays open and says why. Returns the closed keys.
pub(crate) fn archive_idle<P: Page>(lib: &Library, windows: &mut Vec<Little<P>>, after_ms: u64, now: u64, can_close: impl Fn(&P) -> bool) -> Vec<String> {
    let mut closed = Vec::new();
    for key in idle_windows(windows, after_ms, now, can_close) {
        let Some(index) = windows.iter().position(|little| little.key == key) else { continue };
        closed.extend(close_window(lib, windows, index, now));
    }
    closed
}

/// Show All Little Windows: each comes forward, out of the Dock if
/// minimized. The main window stays where it is.
pub(crate) fn show_all<P: Page>(ctx: &egui::Context, windows: &[Little<P>]) {
    for little in windows {
        for command in [egui::ViewportCommand::Visible(true), egui::ViewportCommand::Minimized(false), egui::ViewportCommand::Focus] {
            ctx.send_viewport_cmd_to(little.host.id(), command);
        }
    }
}

/// A window's look: its Space's palette in the app's light or dark mode and
/// that Space's surface texture, as the main window paints the Space.
pub(crate) fn window_look(lib: &Library, space: &str, dark: bool) -> Option<(ui::theme::Palette, crossbow_core::model::Texture)> {
    lib.spaces.get(space).map(|space| (ui::theme::palette(&space.theme, dark), space.theme.texture))
}

/// A page Open in Space moved out of its Little Window.
pub(crate) struct Moved<P> {
    /// The closed window's key.
    pub key: String,
    /// The new Today tab.
    pub tab: String,
    /// The live page for the tab to keep; None when the page reopens there.
    pub view: Option<P>,
}

/// Open in Space: add the page as the first Today tab of `space` and close
/// its window. Under the same profile the live page moves; under another,
/// only with `reopen`, the page reopens there. Otherwise nothing changes and
/// the window says why.
pub(crate) fn move_window<P: Page>(lib: &Library, tabs: &mut Vec<Tab>, windows: &mut Vec<Little<P>>, index: usize, space: &str, reopen: bool) -> Option<Moved<P>> {
    let mode = match plan_move(lib, &windows[index].profile, space, reopen) {
        Ok(mode) => mode,
        Err(text) => {
            windows[index].notice = Some(text.into());
            return None;
        }
    };
    let little = windows.remove(index);
    let url = if little.url.is_empty() { NEW_TAB_URL } else { little.url.as_str() };
    let tab = add_first_today_tab(tabs, space, url, &little.title);
    let key = little.key.clone();
    let view = match mode {
        Move::Live => little.host.detach(),
        Move::Reopen => {
            little.host.close();
            None
        }
    };
    Some(Moved { key, tab, view })
}

/// Spaces the page can move to, in sidebar order.
fn space_choices(lib: &Library, profile: &str) -> Vec<SpaceChoice> {
    lib.spaces_ordered().into_iter().filter(|(id, _)| usable_space(lib, id)).map(|(id, space)| SpaceChoice {
        name: space.name.clone(),
        profile: lib.profiles[&space.profile_id].name.clone(),
        same_profile: space.profile_id == profile,
        id,
    }).collect()
}

/// The main window's chrome: no title bar strip, the toolbar beside the
/// traffic lights.
fn viewport(title: &str, on_top: bool) -> egui::ViewportBuilder {
    ui::window::window_chrome(egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size([640.0, 720.0])
        .with_min_inner_size([480.0, 320.0])
        .with_resizable(true)
        // Above other apps' windows; Spaces and full-screen apps as for any window.
        .with_window_level(if on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal }))
}

/// Keep Window on Top, for every Little Window on this device. On a failed
/// save the windows stay as they were, and this says why.
pub(crate) fn keep_on_top(switch: &mut crate::link_prefs::DeviceSwitch<crate::app::pip::LittleOnTop>, store: &crossbow_core::store::Store, wanted: bool) -> Option<String> {
    switch.choose(store, wanted);
    let failure = switch.failure.take()?;
    log::warn!("Could not save Keep Window on Top: {}", failure.error);
    Some("Couldn't save Keep Window on Top. Check disk space, then try again.".into())
}

impl CrossbowApp {
    /// New Little Window: an empty page whose address field is ready to type in.
    pub(super) fn new_little_window(&mut self, ctx: &egui::Context) {
        let Some(space) = new_window_space(&self.lib, self.link_routing.saved(), &self.active_space) else {
            let at = ctx.input(|input| input.time);
            self.ui.toasts.push(Toast { text: "No Space can open a Little Window. Create a Space first.".into(), at });
            return;
        };
        let Some(window) = Little::open(&self.lib, &space, NEW_TAB_URL) else { return };
        window.host.lock().chrome.address = Some(String::new());
        window.host.lock().chrome.focus_address = true;
        self.littles.push(window);
        self.show_new_windows(ctx);
    }

    /// New windows are created during the root's UI pass. When the main
    /// window is hidden, eframe runs only its logic, so ask for one pass.
    pub(super) fn show_new_windows(&self, ctx: &egui::Context) {
        if !ctx.input(|input| input.viewport().visible().unwrap_or(true)) {
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::RequestPaintWhileHidden);
        }
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }

    /// Run every Little Window for this pass: deliver what each collected to
    /// its view, act on its commands, publish its frame and keep its native
    /// window open. Called on every root UI pass; a window not shown in a
    /// pass is closed by eframe.
    pub(super) fn update_little_windows(&mut self, ctx: &egui::Context) {
        let mut index = 0;
        while index < self.littles.len() {
            let rs = self.render_state.clone();
            let (data_dir, screen) = (self.data_dir.clone(), self.screen_geometry);
            let little = &mut self.littles[index];
            let (url, profile) = (little.url.clone(), little.profile.clone());
            let commands = little.host.pump(|size, scale| WebView::new(WebViewConfig {
                url: if url.is_empty() { NEW_TAB_URL.into() } else { url },
                profile_id: profile,
                data_dir,
                size: (size.x, size.y),
                scale,
                screen,
                device: rs.device.clone(),
                queue: rs.queue.clone(),
                // Little Windows never offer to save, so their pages never capture.
                saves_logins: false,
            }));
            note_use(little, now_ms());
            if little.host.phase() == Phase::Failed && little.notice.is_none() {
                little.notice = Some("This page couldn't open. Close the window and try again.".into());
            }
            let mut removed = false;
            for command in commands {
                if self.little_command(ctx, index, command) {
                    removed = true;
                    break;
                }
            }
            if !removed {
                self.show_little(ctx, index);
                index += 1;
            }
        }
    }

    /// Act on one window command. True when the window is gone.
    fn little_command(&mut self, ctx: &egui::Context, index: usize, command: WindowCommand) -> bool {
        let little = &mut self.littles[index];
        let view = little.host.view().cloned();
        match command {
            WindowCommand::Back => if let Some(view) = view { view.go_back() },
            WindowCommand::Forward => if let Some(view) = view { view.go_forward() },
            WindowCommand::Reload => if let Some(view) = view { view.reload() },
            WindowCommand::Stop => if let Some(view) = view { view.stop() },
            WindowCommand::Navigate(text) => {
                let engine = crate::search_engines::profile_engine(&self.lib, &little.profile).unwrap_or_else(|| self.lib.settings.search_engine.into());
                let url = crossbow_core::search::resolve_input(&text, &engine);
                little.url = url.clone();
                little.notice = None;
                if let Some(view) = view { view.load_url(&url); }
            }
            WindowCommand::OpenInSpace { space, reopen } => return self.move_little(ctx, index, &space, reopen),
            WindowCommand::KeepOnTop(wanted) => little.notice = keep_on_top(self.pip.prefs.little_on_top(&self.lib.store), &self.lib.store, wanted),
            WindowCommand::Capture(region) => {
                let (key, scale) = (little.key.clone(), little.host.scale().unwrap_or(1.0));
                little.notice = None;
                let taken = match &view {
                    Some(view) => self.read_capture(ctx, view, Some(region).filter(|region| region.width > 0 && region.height > 0), scale, egui::Rect::NOTHING, Some(key)),
                    None => Err(crate::app::capture_host::NOTHING_TO_CAPTURE.to_string()),
                };
                if let Err(error) = taken { self.littles[index].notice = Some(error); }
            }
            WindowCommand::Card(action) => {
                if self.capture.finished.as_ref().is_some_and(|finished| finished.owner.as_deref() == Some(little.key.as_str())) {
                    self.capture.card_action(action);
                }
                if self.capture.list.show {
                    // Open in Captures: the main window comes forward on Captures.
                    self.ui.show_archive = true;
                    crate::links::unhide_app();
                    for command in [egui::ViewportCommand::Visible(true), egui::ViewportCommand::Minimized(false), egui::ViewportCommand::Focus] {
                        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, command);
                    }
                }
            }
            WindowCommand::Close => return self.close_little(index),
        }
        false
    }

    /// Close a window, archiving its page first. If that fails the window
    /// stays open and says so.
    fn close_little(&mut self, index: usize) -> bool {
        let Some(key) = close_window(&self.lib, &mut self.littles, index, now_ms()) else { return false };
        self.texture_retirement.retire_tab(&key, &mut self.textures, &mut self.popup_textures);
        true
    }

    /// Open in Space: the page becomes the first Today tab of `space` in the
    /// main window, which comes forward, and the Little Window closes.
    fn move_little(&mut self, ctx: &egui::Context, index: usize, space: &str, reopen: bool) -> bool {
        let Some(moved) = move_window(&self.lib, &mut self.tabs, &mut self.littles, index, space, reopen) else { return false };
        self.texture_retirement.retire_tab(&moved.key, &mut self.textures, &mut self.popup_textures);
        if let Some(tab) = self.tab_mut(&moved.tab) { tab.view = moved.view; }
        self.activate(&moved.tab);
        crate::links::unhide_app();
        for command in [egui::ViewportCommand::Visible(true), egui::ViewportCommand::Minimized(false), egui::ViewportCommand::Focus] {
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, command);
        }
        self.cue(crate::feel::Cue::MovedToSpace);
        true
    }

    /// Publish a window's frame and show its native window for this pass.
    fn show_little(&mut self, ctx: &egui::Context, index: usize) {
        let (key, view) = (self.littles[index].key.clone(), self.littles[index].host.view().cloned());
        let mut page = PageState { url: self.littles[index].url.clone(), ..Default::default() };
        let (mut texture, mut popup) = (None, None);
        if let Some(view) = &view {
            let nav = view.nav();
            let little = &mut self.littles[index];
            if !nav.url.is_empty() { little.url = nav.url.clone(); }
            if !nav.title.is_empty() { little.title = nav.title.clone(); }
            page = PageState {
                url: little.url.clone(),
                title: little.title.clone(),
                can_go_back: nav.can_go_back,
                can_go_forward: nav.can_go_forward,
                loading: nav.loading,
                progress: nav.progress as f32,
                cursor: nav.cursor,
                tooltip: nav.tooltip,
                editable: nav.editable_focus,
            };
            self.little_events(ctx, index, view);
            let painted = view.with_texture(|texture| texture.map_or(0, |texture| texture.frame));
            texture = self.view_texture(&key, view).map(|(id, size)| (id, size, painted));
            popup = self.view_popup_texture(&key, view);
        }
        let on_top = self.pip.prefs.little_on_top(&self.lib.store).enabled();
        let little = &self.littles[index];
        let (palette, surface) = window_look(&self.lib, &little.space, self.system_dark).unwrap_or_else(|| (self.palette(), Default::default()));
        let frame = Frame {
            texture,
            popup,
            page,
            palette: Some(palette),
            surface_texture: Some(self.texture_bank.get(surface).id()),
            on_top,
            spaces: space_choices(&self.lib, &little.profile),
            profile: (self.lib.profiles.len() > 1).then(|| self.lib.profiles.get(&little.profile).map(|profile| profile.name.clone())).flatten(),
            notice: little.notice.clone(),
            capture_drag: self.capture.prefs.drag(&self.lib.store),
            capture_card: self.capture.finished.as_ref().filter(|finished| finished.owner.as_deref() == Some(little.key.as_str())).map(|finished| crate::ui::capture::CardStatus { saved: finished.saved.is_some(), notice: finished.notice.clone() }),
        };
        let id = little.host.id();
        if little.host.publish(frame) { ctx.request_repaint_of(id); }
        let surface = little.host.surface();
        let builder = viewport(&little.display_title(), on_top);
        if self.littles[index].host.take_reveal() {
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
        }
        ctx.show_viewport_deferred(id, builder, move |ui, _| {
            let mut surface = surface.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            crate::ui::little::show(ui, &mut surface);
        });
    }

    /// A window's page events: its links open in the same window, as Peek's do.
    fn little_events(&mut self, ctx: &egui::Context, index: usize, view: &WebView) {
        for event in view.take_events() {
            match event {
                WebEvent::OpenUrl { url, .. } => view.load_url(&url),
                WebEvent::Favicon { page_url, width, height, rgba } => self.favicons.put(ctx, &page_url, width, height, &rgba),
                event @ (WebEvent::Download(_) | WebEvent::DownloadRequested(_)) => self.process_download_event(ctx, event),
                WebEvent::LoadError { text, .. } => self.littles[index].notice = Some(format!("This page couldn't load ({}). Reload to try again.", text.trim_start_matches("net::"))),
                WebEvent::Visited { url, title } => {
                    let little = &mut self.littles[index];
                    little.notice = None;
                    if url.starts_with("http://") || url.starts_with("https://") {
                        let _ = self.lib.store.record_visit(&little.profile, &url, &title, now_ms());
                    }
                }
                // Little Windows show no prompts: only what the profile allowed before.
                WebEvent::Permission(request) => {
                    view.answer_permission(request.id, self.site_permissions.remembered_only(&self.lib.store, view.profile_id(), &request));
                    if let Some(notice) = self.site_permissions.take_notices().pop() { self.littles[index].notice = Some(notice); }
                }
                WebEvent::ExternalLink(_) => self.littles[index].notice = Some(crate::site_permissions::FROM_TABS_ONLY.into()),
                // Saving passwords and addresses from Little Windows isn't offered
                // yet; dropping a login or an address wipes it.
                WebEvent::ContextMenu(_) | WebEvent::Credential(_) | WebEvent::LoginNotFilled | WebEvent::Address(_) | WebEvent::AddressNotFilled | WebEvent::CardNotFilled
                | WebEvent::AddressFillReport(_) | WebEvent::CardFillReport(_) | WebEvent::Closed => {}
                WebEvent::PermissionDismissed(_) => {}
            }
        }
    }

    /// Show All Little Windows, from the command bar or a chord the user set.
    pub(super) fn show_all_little_windows(&mut self, ctx: &egui::Context, now: f64) {
        if self.littles.is_empty() {
            self.ui.toasts.push(Toast { text: NO_LITTLE_WINDOWS.into(), at: now });
            return;
        }
        show_all(ctx, &self.littles);
        self.show_new_windows(ctx);
    }

    /// Close All Little Windows: each page goes to the archive, as when its
    /// window is closed. Windows that could not archive stay open.
    pub(super) fn close_all_little_windows(&mut self, now: f64) {
        if self.littles.is_empty() {
            self.ui.toasts.push(Toast { text: NO_LITTLE_WINDOWS.into(), at: now });
            return;
        }
        for key in close_all(&self.lib, &mut self.littles, now_ms()) {
            self.texture_retirement.retire_tab(&key, &mut self.textures, &mut self.popup_textures);
        }
        let text = match self.littles.len() {
            0 => return,
            1 => "1 Little Window stayed open because its page could not be archived.".to_string(),
            kept => format!("{kept} Little Windows stayed open because their pages could not be archived."),
        };
        self.ui.toasts.push(Toast { text, at: now });
    }

    /// ⌥⌘-click: open `url` in a Little Window in the Space and profile of
    /// the tab it was clicked in. False when no such Space can open it.
    pub(super) fn open_link_in_little_window(&mut self, ctx: &egui::Context, tab: &str, url: &str) -> bool {
        let Some(space) = self.tab(tab).and_then(|tab| little_space_for_tab(&self.lib, tab, &self.active_space)) else { return false };
        let Some(window) = Little::open(&self.lib, &space, url) else { return false };
        self.littles.push(window);
        self.show_new_windows(ctx);
        true
    }

    /// Archive and close the Little Windows nobody used for the delay
    /// chosen in Settings → Links.
    pub(super) fn archive_idle_little_windows(&mut self, after_ms: u64, now: u64) {
        for key in archive_idle(&self.lib, &mut self.littles, after_ms, now, WebView::can_auto_archive) {
            self.texture_retirement.retire_tab(&key, &mut self.textures, &mut self.popup_textures);
        }
    }

    /// Quitting: archive each window's page, then close it.
    pub(super) fn close_little_windows(&mut self) {
        for little in std::mem::take(&mut self.littles) {
            if let Err(error) = archive_window(&self.lib, &little, now_ms()) {
                log::warn!("Could not archive a Little Window's page while quitting: {error}");
            }
            little.host.close();
        }
    }

    /// A profile is being deleted: its windows close without an archive entry.
    pub(super) fn close_profile_little_windows(&mut self, profile: &str) {
        let closing: Vec<_> = self.littles.extract_if(.., |little| little.profile == profile).collect();
        for little in closing {
            self.texture_retirement.retire_tab(&little.key, &mut self.textures, &mut self.popup_textures);
            little.host.close();
        }
    }
}

/// New Little Window in the app menu.
#[cfg(target_os = "macos")]
pub(crate) mod menu {
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{MainThreadMarker, class, msg_send, sel};
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, Ordering};

    static REQUESTED: AtomicBool = AtomicBool::new(false);

    thread_local! {
        static WAKE: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
    }

    /// `-crossbowNewLittleWindow:` added to winit's application delegate.
    extern "C-unwind" fn new_little_window(_this: &AnyObject, _cmd: Sel, _sender: *mut AnyObject) {
        REQUESTED.store(true, Ordering::Release);
        if let Some(ctx) = WAKE.with(|wake| wake.borrow().clone()) {
            // AppKit may call this while egui holds its context lock.
            let _ = std::thread::Builder::new().name("crossbow-menu-wake".into()).spawn(move || ctx.request_repaint_of(egui::ViewportId::ROOT));
        }
    }

    /// Whether the menu item was chosen since the last call.
    pub(crate) fn take() -> bool {
        REQUESTED.swap(false, Ordering::AcqRel)
    }

    /// Add New Little Window under the app menu's first section. Returns why
    /// it could not be added.
    pub(crate) fn install(ctx: &egui::Context) -> Result<(), String> {
        let main = MainThreadMarker::new().ok_or("not on the main thread")?;
        let app = NSApplication::sharedApplication(main);
        let delegate = app.delegate().ok_or("the application has no delegate")?;
        let class: &AnyClass = AsRef::<AnyObject>::as_ref(&*delegate).class();
        // SAFETY: The function matches `-(void)crossbowNewLittleWindow:(id)sender`
        // ("v@:@"); class_addMethod refuses a class that already has the method.
        let added = unsafe {
            let imp = std::mem::transmute::<extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject), Imp>(new_little_window);
            objc2::ffi::class_addMethod(class as *const AnyClass as *mut AnyClass, sel!(crossbowNewLittleWindow:), imp, c"v@:@".as_ptr())
        };
        if !added.as_bool() {
            return Err(format!("{} already has a New Little Window action", class.name().to_string_lossy()));
        }
        // SAFETY: AppKit's menu API on the main thread, with objects it returned.
        unsafe {
            let menu: Option<Retained<AnyObject>> = msg_send![&*app, mainMenu];
            let menu = menu.ok_or("the app has no menu bar")?;
            let first: Option<Retained<AnyObject>> = msg_send![&*menu, itemAtIndex: 0isize];
            let submenu: Option<Retained<AnyObject>> = msg_send![&*first.ok_or("the menu bar is empty")?, submenu];
            let submenu = submenu.ok_or("the app menu has no items")?;
            let title = NSString::from_str("New Little Window");
            let none = NSString::from_str("");
            let item: Allocated<AnyObject> = msg_send![class!(NSMenuItem), alloc];
            let item: Retained<AnyObject> = msg_send![item, initWithTitle: &*title, action: sel!(crossbowNewLittleWindow:), keyEquivalent: &*none];
            let _: () = msg_send![&*item, setTarget: &*delegate];
            let count: isize = msg_send![&*submenu, numberOfItems];
            // After About and its separator.
            let at = count.min(2);
            let _: () = msg_send![&*submenu, insertItem: &*item, atIndex: at];
            let separator: Retained<AnyObject> = msg_send![class!(NSMenuItem), separatorItem];
            let _: () = msg_send![&*submenu, insertItem: &*separator, atIndex: at + 1];
        }
        WAKE.with(|wake| *wake.borrow_mut() = Some(ctx.clone()));
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_menu_item_asks_for_one_new_little_window() {
            let this = objc2::runtime::NSObject::new();
            take();
            assert!(!take());
            new_little_window(&this, sel!(crossbowNewLittleWindow:), std::ptr::null_mut());
            assert!(take(), "choosing the item asks for a window");
            assert!(!take(), "once");
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) mod menu {
    pub(crate) fn take() -> bool {
        false
    }

    pub(crate) fn install(_ctx: &egui::Context) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbow_core::store::ArchiveSource;
    use crate::shortcuts::tests::Fixture;
    use crate::window_host::tests::Recorder;
    use crossbow_core::link_routing::{Destination, Match, Rule};

    /// A library on disk: Personal and Side use profile A, Work uses profile B.
    struct World {
        fixture: Fixture,
        lib: Library,
        personal: String,
        side: String,
        work: String,
        profile_a: String,
        profile_b: String,
    }

    fn world() -> World {
        let fixture = Fixture::new();
        let mut lib = Library::open(fixture.store()).unwrap();
        lib.ensure_defaults().unwrap();
        let personal = lib.spaces_ordered()[0].0.clone();
        let profile_a = lib.spaces[&personal].profile_id.clone();
        let profile_b = lib.create_profile("B").unwrap();
        let side = lib.create_space("Side", "S", Theme::preset(2), &profile_a).unwrap();
        let work = lib.create_space("Work", "W", Theme::preset(1), &profile_b).unwrap();
        World { fixture, lib, personal, side, work, profile_a, profile_b }
    }

    impl World {
        /// Quit and reopen: a new library on the same file.
        fn reopened(&self) -> Library {
            Library::open(self.fixture.store()).unwrap()
        }

        /// A window on `url` in `space`, its view already open.
        fn window(&self, space: &str, url: &str, title: &str) -> (Little<Recorder>, Recorder) {
            let mut window: Little<Recorder> = Little::open(&self.lib, space, url).unwrap();
            window.title = title.into();
            let page = Recorder::default();
            window.host.lock().report_size(egui::vec2(624.0, 640.0), 2.0);
            window.host.pump(|_, _| Some(page.clone()));
            page.take();
            (window, page)
        }
    }

    fn rule(kind: Match, value: &str, space: &str) -> Rule {
        Rule::new(crossbow_core::new_id(), kind, value, space.into()).unwrap()
    }

    fn today(tabs: &[Tab], space: &str) -> Vec<String> {
        let mut today: Vec<&Tab> = tabs.iter().filter(|tab| tab.space_id == space && tab.item_id.is_none()).collect();
        today.sort_by(|a, b| a.order.cmp(&b.order));
        today.into_iter().map(|tab| tab.url.clone()).collect()
    }

    #[test]
    fn a_new_little_window_belongs_to_the_default_link_space_or_the_space_in_use() {
        let w = world();
        let most_recent = Routing::default();
        assert_eq!(new_window_space(&w.lib, &most_recent, &w.side), Some(w.side.clone()), "the Space in use");
        let chosen = Routing { default: Destination::Space(w.work.clone()), ..Routing::default() };
        let space = new_window_space(&w.lib, &chosen, &w.side).unwrap();
        assert_eq!(space, w.work, "the Space chosen for links");
        let window: Little<Recorder> = Little::open(&w.lib, &space, NEW_TAB_URL).unwrap();
        assert_eq!((window.space.as_str(), window.profile.as_str()), (w.work.as_str(), w.profile_b.as_str()), "and its profile");
        let gone = Routing { default: Destination::Space("deleted".into()), ..Routing::default() };
        assert_eq!(new_window_space(&w.lib, &gone, &w.personal), Some(w.personal.clone()), "a deleted default falls back");
        assert!(Little::<Recorder>::open(&w.lib, "deleted", NEW_TAB_URL).is_none(), "no window for a Space that can't open pages");
        let other: Little<Recorder> = Little::open(&w.lib, &w.side, NEW_TAB_URL).unwrap();
        assert_ne!(other.key, window.key, "each window has its own key");
        assert_ne!(other.host.id(), window.host.id(), "and its own native window");
    }

    #[test]
    fn links_from_other_apps_open_as_tabs_or_in_little_windows_in_the_routed_space_and_profile() {
        let w = world();
        let routing = Routing { rules: vec![rule(Match::HostIs, "github.com", &w.work)], default: Destination::MostRecent };
        let (mut tabs, mut windows): (Vec<Tab>, Vec<Little<Recorder>>) = (Vec::new(), Vec::new());

        let tab = deliver_link(&w.lib, &routing, &SiteProfiles::default(), false, &mut tabs, &mut windows, &w.side, "https://github.com/a").unwrap();
        let Delivered::Tab(placed) = tab else { panic!("off: a tab, got {tab:?}") };
        assert_eq!(placed.space, w.work);
        assert_eq!(today(&tabs, &w.work), ["https://github.com/a"]);
        assert!(windows.is_empty(), "no window while the choice is off");

        let window = deliver_link(&w.lib, &routing, &SiteProfiles::default(), true, &mut tabs, &mut windows, &w.side, "https://github.com/b").unwrap();
        assert_eq!(window, Delivered::Window { key: windows[0].key.clone(), space: w.work.clone() });
        assert_eq!((windows[0].url.as_str(), windows[0].profile.as_str()), ("https://github.com/b", w.profile_b.as_str()), "the rule picks Space and profile");
        assert_eq!(windows[0].host.phase(), Phase::Opening, "its view opens once its window has a size");
        let other = deliver_link(&w.lib, &routing, &SiteProfiles::default(), true, &mut tabs, &mut windows, &w.side, "https://example.com/").unwrap();
        assert!(matches!(other, Delivered::Window { ref space, .. } if *space == w.side), "other links use the default: {other:?}");
        assert_eq!(windows[1].profile, w.profile_a);
        assert_eq!(tabs.len(), 1, "on: no tab is added");

        // A site set to open in a profile opens in a window of its Space, after the rules.
        let mut sites = SiteProfiles::default();
        let site = crossbow_core::site_profiles::Site::parse("example.com", false, false).unwrap();
        sites.add(&w.lib.store, crossbow_core::site_profiles::SiteRule { site, profile: w.profile_b.clone() }, false).unwrap();
        let window = deliver_link(&w.lib, &routing, &sites, true, &mut tabs, &mut windows, &w.side, "https://example.com/").unwrap();
        assert_eq!(window, Delivered::Window { key: windows[2].key.clone(), space: w.work.clone() });
        assert_eq!(windows[2].profile, w.profile_b);
        let tab = deliver_link(&w.lib, &routing, &sites, false, &mut tabs, &mut windows, &w.side, "https://example.com/").unwrap();
        assert!(matches!(tab, Delivered::Tab(ref placed) if placed.space == w.work && placed.from == w.side), "{tab:?}");
    }

    #[test]
    fn open_in_space_moves_the_live_page_within_a_profile_and_reopens_it_elsewhere_only_once_confirmed() {
        let w = world();
        assert_eq!(plan_move(&w.lib, &w.profile_a, &w.side, false), Ok(Move::Live));
        assert_eq!(plan_move(&w.lib, &w.profile_a, &w.work, true), Ok(Move::Reopen));
        assert!(plan_move(&w.lib, &w.profile_a, &w.work, false).is_err(), "another profile needs confirmation");
        assert!(plan_move(&w.lib, &w.profile_a, "deleted", true).is_err());

        // Same profile: the live page becomes the first Today tab.
        let mut tabs = Vec::new();
        add_first_today_tab(&mut tabs, &w.side, "https://old.example/", "Old");
        let (window, page) = w.window(&w.personal, "https://example.com/a", "A");
        let mut windows = vec![window];
        let moved = move_window(&w.lib, &mut tabs, &mut windows, 0, &w.side, false).unwrap();
        assert!(windows.is_empty(), "the Little Window closes");
        assert_eq!(today(&tabs, &w.side), ["https://example.com/a", "https://old.example/"], "first in Today");
        assert_eq!(tabs.iter().find(|tab| tab.id == moved.tab).map(|tab| tab.title.as_str()), Some("A"));
        assert!(moved.view.is_some() && page.take().is_empty(), "the page keeps running, with its state");

        // Another profile, unconfirmed: nothing changes and the window says why.
        let (window, page) = w.window(&w.personal, "https://example.com/b", "B");
        let mut windows = vec![window];
        assert!(move_window(&w.lib, &mut tabs, &mut windows, 0, &w.work, false).is_none());
        assert_eq!(windows.len(), 1);
        assert!(windows[0].notice.as_deref().is_some_and(|notice| notice.contains("another profile")));
        assert!(today(&tabs, &w.work).is_empty() && page.take().is_empty(), "no tab, and the page stays open");

        // Confirmed: it reopens under the Space's profile; the old page closes.
        let moved = move_window(&w.lib, &mut tabs, &mut windows, 0, &w.work, true).unwrap();
        assert!(windows.is_empty() && moved.view.is_none(), "the tab opens a fresh view under profile B");
        assert_eq!(page.take(), ["close"]);
        assert_eq!(today(&tabs, &w.work), ["https://example.com/b"]);
        assert_eq!(tabs.iter().find(|tab| tab.id == moved.tab).and_then(|tab| tab_profile(&w.lib, tab)), Some(w.profile_b.as_str()));
    }

    #[test]
    fn closing_a_little_window_archives_its_page_with_its_profile_for_reopen_closed() {
        let mut w = world();
        let (window, page) = w.window(&w.side, "https://example.com/read", "Reading");
        let mut windows = vec![window];
        let key = windows[0].key.clone();
        assert_eq!(close_window(&w.lib, &mut windows, 0, 50), Some(key));
        assert!(windows.is_empty());
        assert_eq!(page.take(), ["close"], "closing frees the view");

        let lib = w.reopened();
        let archived = archive_for_space(&lib, &w.side, 5).unwrap();
        assert_eq!(archived.iter().map(|tab| (tab.url.as_str(), tab.title.as_str(), tab.space_id.as_str(), tab.profile_id.as_str(), tab.source)).collect::<Vec<_>>(),
            [("https://example.com/read", "Reading", w.side.as_str(), w.profile_a.as_str(), ArchiveSource::LittleWindow)], "kept after reopening, with where it was open");
        assert!(archive_for_space(&lib, &w.work, 5).unwrap().is_empty(), "another profile's archive is untouched");
        let mut tabs = Vec::new();
        let restored = restore_archived_tab(&lib, &mut tabs, &w.side, archived[0].rowid).unwrap().expect("Reopen Closed restores it");
        assert_eq!(tabs.iter().find(|tab| tab.id == restored).map(|tab| tab.url.as_str()), Some("https://example.com/read"));

        // A Space moved to another profile: another Space of the page's profile keeps it.
        let (mut window, _) = w.window(&w.side, "https://example.com/moved", "");
        window.space = w.work.clone();
        let mut windows = vec![window];
        assert!(close_window(&w.lib, &mut windows, 0, 60).is_some());
        let latest = &archive_for_space(&w.reopened(), &w.personal, 1).unwrap()[0];
        assert_eq!((latest.url.as_str(), latest.title.as_str(), latest.space_id.as_str()), ("https://example.com/moved", "example.com/moved", w.personal.as_str()));

        // An empty page closes without an entry.
        let before = archive_for_space(&w.lib, &w.side, 10).unwrap().len();
        let (window, _) = w.window(&w.side, NEW_TAB_URL, "");
        let mut windows = vec![window];
        assert!(close_window(&w.lib, &mut windows, 0, 70).is_some());
        assert_eq!(archive_for_space(&w.lib, &w.side, 10).unwrap().len(), before);

        // A failed archive keeps the window and its page open; closing again retries.
        let (window, page) = w.window(&w.side, "https://example.com/keep", "Keep");
        let mut windows = vec![window];
        w.lib.store = w.fixture.read_only();
        windows[0].host.lock().request_close();
        assert_eq!(windows[0].host.pump(|_, _| None), [WindowCommand::Close]);
        assert!(close_window(&w.lib, &mut windows, 0, 80).is_none());
        assert_eq!(windows[0].host.phase(), Phase::Open, "the window stays open");
        assert!(windows[0].notice.as_deref().is_some_and(|notice| notice.starts_with("Could not archive this page")));
        assert!(page.take().is_empty(), "its page stays open");
        w.lib.store = w.fixture.store();
        assert!(close_window(&w.lib, &mut windows, 0, 90).is_some());
        assert_eq!(page.take(), ["close"]);
        assert_eq!(archive_for_space(&w.reopened(), &w.side, 1).unwrap()[0].url, "https://example.com/keep");
    }

    #[test]
    fn keep_window_on_top_floats_every_little_window_is_remembered_and_a_failed_save_says_so() {
        let fixture = crate::shortcuts::tests::Fixture::new();
        let store = fixture.store();
        let mut prefs = crate::app::pip::PipPrefs::default();
        assert!(!prefs.little_on_top(&store).enabled(), "off by default");
        assert_eq!(viewport("Reading", false).window_level, Some(egui::WindowLevel::Normal));
        assert_eq!(keep_on_top(prefs.little_on_top(&store), &store, true), None);
        assert_eq!(viewport("Reading", prefs.little_on_top(&store).enabled()).window_level, Some(egui::WindowLevel::AlwaysOnTop), "above other apps' windows");
        drop(store);
        let mut reopened = crate::app::pip::PipPrefs::default();
        assert!(reopened.little_on_top(&fixture.store()).enabled(), "remembered on this device");

        let store = fixture.read_only();
        let mut prefs = crate::app::pip::PipPrefs::default();
        let notice = keep_on_top(prefs.little_on_top(&store), &store, false);
        assert_eq!(notice.as_deref(), Some("Couldn't save Keep Window on Top. Check disk space, then try again."));
        assert!(prefs.little_on_top(&store).enabled(), "the windows stay as they were");
        // The command reaches it, and every window's frame and level follow the choice.
        let source = include_str!("little.rs");
        assert!(source.contains("WindowCommand::KeepOnTop(wanted) => little.notice = keep_on_top(self.pip.prefs.little_on_top(&self.lib.store), &self.lib.store, wanted),"));
        assert!(source.contains("let builder = viewport(&little.display_title(), on_top);"));
    }

    #[test]
    fn a_little_window_has_the_main_windows_chrome_with_no_separate_title_bar() {
        let (little, main) = (viewport("Reading", false), crate::ui::window::main_viewport());
        let chrome = |builder: &egui::ViewportBuilder| (builder.fullsize_content_view, builder.titlebar_shown, builder.title_shown, builder.titlebar_buttons_shown, builder.transparent, builder.decorations);
        assert_eq!(chrome(&little), chrome(&main), "the same title bar settings as the main window");
        if cfg!(target_os = "macos") {
            assert_eq!(chrome(&little), (Some(true), Some(false), Some(false), None, Some(true), None), "full-size content under a transparent title bar");
        }
        assert_eq!((little.title.as_deref(), little.inner_size, little.resizable), (Some("Reading"), Some(egui::vec2(640.0, 720.0)), Some(true)), "still small and resizable");

        // At its smallest, the toolbar beside the traffic lights still shows the address.
        let smallest = little.min_inner_size.expect("a minimum size");
        let ctx = egui::Context::default();
        ui::window::install_fonts(&ctx);
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        let layout = ui::little::toolbar_layout(&ctx, egui::Rect::from_min_size(egui::Pos2::ZERO, smallest));
        assert!(layout.address.width() >= 80.0, "the address keeps room at {smallest:?}: {:?}", layout.address);
    }

    #[test]
    fn a_little_window_looks_like_its_space_in_the_main_window_in_light_and_dark() {
        let mut w = world();
        w.lib.update_space(&w.side, |space| space.theme.texture = crossbow_core::model::Texture::Denim).unwrap();
        for dark in [false, true] {
            let (palette, texture) = window_look(&w.lib, &w.side, dark).expect("a Space's look");
            assert_eq!(palette, ui::theme::palette(&w.lib.spaces[&w.side].theme, dark), "the main window's palette for that Space, dark {dark}");
            assert_eq!(texture, crossbow_core::model::Texture::Denim, "and its surface texture");
            assert_ne!(window_look(&w.lib, &w.work, dark).unwrap().0, palette, "each window follows its own Space");
        }
        assert_ne!(window_look(&w.lib, &w.side, true).unwrap().0, window_look(&w.lib, &w.side, false).unwrap().0, "light and dark differ");
        w.lib.update_space(&w.side, |space| space.theme = Theme::preset(3)).unwrap();
        assert_eq!(window_look(&w.lib, &w.side, false).unwrap().0, ui::theme::palette(&Theme::preset(3), false), "a theme change follows at once");
        assert!(window_look(&w.lib, "deleted", false).is_none());
    }

    #[test]
    fn close_all_archives_every_little_window_and_leaves_the_main_window_alone() {
        let mut w = world();
        let mut tabs = Vec::new();
        let main = add_first_today_tab(&mut tabs, &w.personal, "https://example.com/main", "Main");
        let (mut windows, mut pages) = (Vec::new(), Vec::new());
        for (space, url, title) in [(&w.side, "https://example.com/one", "One"), (&w.work, "https://example.com/two", "Two"), (&w.side, "https://example.com/three", "Three")] {
            let (window, page) = w.window(space, url, title);
            windows.push(window);
            pages.push(page);
        }
        let keys: Vec<String> = windows.iter().map(|window| window.key.clone()).collect();
        assert_eq!(close_all(&w.lib, &mut windows, 100), keys, "every window closes, oldest first");
        assert!(windows.is_empty());
        for page in &pages {
            assert_eq!(page.take(), ["close"], "each frees its view");
        }
        let lib = w.reopened();
        let side = archive_for_space(&lib, &w.side, 5).unwrap();
        assert_eq!(side.iter().map(|tab| (tab.url.as_str(), tab.profile_id.as_str())).collect::<Vec<_>>(),
            [("https://example.com/three", w.profile_a.as_str()), ("https://example.com/one", w.profile_a.as_str())], "archived with their profile, newest first");
        let work = archive_for_space(&lib, &w.work, 5).unwrap();
        assert_eq!(work.iter().map(|tab| (tab.url.as_str(), tab.profile_id.as_str())).collect::<Vec<_>>(), [("https://example.com/two", w.profile_b.as_str())]);
        assert_eq!(tabs.iter().map(|tab| (tab.id.as_str(), tab.url.as_str())).collect::<Vec<_>>(), [(main.as_str(), "https://example.com/main")], "the main window's tabs are untouched");
        let restored = restore_archived_tab(&lib, &mut tabs, &w.side, side[0].rowid).unwrap().expect("Reopen Closed restores the newest");
        assert_eq!(tabs.iter().find(|tab| tab.id == restored).map(|tab| tab.url.as_str()), Some("https://example.com/three"));

        // A failed archive keeps those windows open, saying why; Close All again retries.
        let (mut windows, mut pages) = (Vec::new(), Vec::new());
        for url in ["https://example.com/four", "https://example.com/five"] {
            let (window, page) = w.window(&w.side, url, "");
            windows.push(window);
            pages.push(page);
        }
        w.lib.store = w.fixture.read_only();
        assert!(close_all(&w.lib, &mut windows, 110).is_empty());
        assert_eq!(windows.len(), 2, "both stay open");
        assert!(windows.iter().all(|window| window.notice.as_deref().is_some_and(|notice| notice.starts_with("Could not archive this page"))));
        assert!(pages.iter().all(|page| page.take().is_empty()), "their pages stay open");
        w.lib.store = w.fixture.store();
        assert_eq!(close_all(&w.lib, &mut windows, 120).len(), 2);
        assert!(windows.is_empty() && pages.iter().all(|page| page.take() == ["close"]));
        assert_eq!(close_all::<Recorder>(&w.lib, &mut Vec::new(), 130), Vec::<String>::new(), "nothing to close");
    }

    #[test]
    fn show_all_brings_every_little_window_forward_and_leaves_the_main_window_alone() {
        let w = world();
        let windows: Vec<Little<Recorder>> = [&w.side, &w.work, &w.personal].into_iter().map(|space| w.window(space, "https://example.com/", "").0).collect();
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        // A root pass: actions run, then every window is shown.
        let pass = |show: bool| {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                if show { show_all(ui.ctx(), &windows); }
                for window in &windows {
                    ui.ctx().show_viewport_deferred(window.host.id(), egui::ViewportBuilder::default(), |_, _| {});
                }
            });
            output.textures_delta.clear();
            output
        };
        pass(false);
        let output = pass(true);
        for window in &windows {
            assert_eq!(output.viewport_output[&window.host.id()].commands, [egui::ViewportCommand::Visible(true), egui::ViewportCommand::Minimized(false), egui::ViewportCommand::Focus], "{} comes forward", window.key);
        }
        assert!(output.viewport_output[&egui::ViewportId::ROOT].commands.is_empty(), "the main window stays where it is: {:?}", output.viewport_output[&egui::ViewportId::ROOT].commands);
    }

    fn tab_in(space: &str, item: Option<String>) -> Tab {
        Tab::from_row(crossbow_core::store::TabRow {
            id: new_id(), space_id: space.into(), item_id: item, url: "https://example.com/source".into(),
            title: "Source".into(), order: "a0".into(), created_ms: 1, last_active_ms: 1,
        })
    }

    #[test]
    fn option_command_click_and_shift_click_are_told_apart_by_the_keys_held() {
        use crossbow_engine::Modifiers;
        let none = Modifiers::default();
        let option_command = Modifiers { alt: true, command: true, ..none };
        let command = Modifiers { command: true, ..none };
        let shift = Modifiers { shift: true, ..none };
        // ⌥⌘-click asks for a background tab, as ⌘-click does.
        assert_eq!(click_target(option_command, true, false, true, true), Click::LittleWindow);
        assert_eq!(click_target(option_command, true, false, false, true), Click::Usual, "off: the usual background tab");
        assert_eq!(click_target(command, true, false, true, true), Click::Usual, "⌘-click is unchanged");
        assert_eq!(click_target(Modifiers { shift: true, ..option_command }, false, false, true, true), Click::Usual, "⌥⇧⌘-click keeps its foreground tab");
        assert_eq!(click_target(Modifiers { ctrl: true, ..option_command }, true, false, true, true), Click::Usual);
        assert_eq!(click_target(option_command, false, true, true, true), Click::Usual, "a popup keeps its Peek");
        // Shift-click asks for a new window.
        assert_eq!(click_target(shift, false, false, true, true), Click::Peek);
        assert_eq!(click_target(shift, false, false, true, false), Click::Usual, "off: the usual new tab");
        assert_eq!(click_target(Modifiers { shift: true, ..command }, false, false, true, true), Click::Usual, "⇧⌘-click keeps its foreground tab");
        assert_eq!(click_target(Modifiers { alt: true, ..shift }, false, false, true, true), Click::Usual);
        assert_eq!(click_target(shift, false, true, true, true), Click::Usual, "a popup keeps its Peek");
        assert_eq!(click_target(none, false, false, true, true), Click::Usual, "a plain target=_blank link");
    }

    #[test]
    fn option_command_click_uses_the_clicked_tabs_space_and_profile() {
        let mut w = world();
        assert_eq!(little_space_for_tab(&w.lib, &tab_in(&w.work, None), &w.personal), Some(w.work.clone()), "the tab's own Space, not the one in use");
        assert_eq!(little_space_for_tab(&w.lib, &tab_in(&w.side, None), &w.work), Some(w.side.clone()));
        let window: Little<Recorder> = Little::open(&w.lib, &w.work, "https://example.com/next").unwrap();
        assert_eq!((window.space.as_str(), window.profile.as_str()), (w.work.as_str(), w.profile_b.as_str()));
        // A favorite has no Space: the one in use if it shares the favorite's profile.
        let item = w.lib.add_favorite(&w.profile_a, "https://example.com/favorite", "Favorite").unwrap();
        let favorite = tab_in(FAVORITES_SPACE, Some(item));
        assert_eq!(little_space_for_tab(&w.lib, &favorite, &w.side), Some(w.side.clone()));
        assert_eq!(little_space_for_tab(&w.lib, &favorite, &w.work), Some(w.personal.clone()), "never under another profile");
        assert_eq!(little_space_for_tab(&w.lib, &tab_in("deleted", None), &w.personal), None, "a tab whose Space is gone opens a tab instead");
    }

    #[test]
    fn focus_and_page_input_count_as_use() {
        let w = world();
        let (mut window, _) = w.window(&w.side, "https://example.com/a", "A");
        let pass = |window: &mut Little<Recorder>, now: u64| {
            window.host.pump(|_, _| None);
            note_use(window, now);
            window.last_active_ms
        };
        window.last_active_ms = 5;
        assert_eq!(pass(&mut window, 100), 5, "a pass without focus or input");
        window.host.lock().push_input(crate::window_host::ViewInput::Keyboard(crate::input::WebInput::Text("a".into())));
        assert_eq!(pass(&mut window, 200), 200, "page input");
        window.host.lock().report_focus(true);
        assert_eq!(pass(&mut window, 300), 300, "focus");
        assert!(window.host.focused());
        assert_eq!(pass(&mut window, 400), 400, "still focused");
        window.host.lock().report_focus(false);
        assert_eq!(pass(&mut window, 500), 400);
        assert!(!window.host.focused());
    }

    #[test]
    fn unused_little_windows_are_archived_after_the_delay_through_the_usual_close() {
        const HOUR: u64 = 3_600_000;
        let mut w = world();
        let (mut used, _) = w.window(&w.side, "https://example.com/used", "Used");
        let (mut idle, idle_page) = w.window(&w.side, "https://example.com/idle", "Idle");
        let (mut focused, _) = w.window(&w.side, "https://example.com/focused", "Focused");
        let (mut guarded, _) = w.window(&w.side, "https://example.com/form", "Form");
        let start = 10 * HOUR;
        for window in [&mut used, &mut idle, &mut focused, &mut guarded] { window.last_active_ms = start; }
        focused.host.lock().report_focus(true);
        focused.host.pump(|_, _| None);
        let mut windows = vec![used, idle, focused, guarded];
        // Like a page someone typed in: it refuses an automatic close, as a tab's does.
        const USED: &str = "used by the person";
        windows[3].host.view().unwrap().0.borrow_mut().push(USED.into());
        let allows = |page: &Recorder| !page.0.borrow().iter().any(|line| line == USED);

        assert!(archive_idle(&w.lib, &mut windows, 12 * HOUR, start + 12 * HOUR, allows).is_empty(), "not before the delay");
        windows[0].last_active_ms = start + 11 * HOUR;
        let closed = archive_idle(&w.lib, &mut windows, 12 * HOUR, start + 12 * HOUR + 1, allows);
        assert_eq!(closed.len(), 1, "only the unused window that allows it: {closed:?}");
        assert!(idle_page.take().contains(&"close".to_string()), "its view is freed");
        assert_eq!(windows.iter().map(|window| window.title.as_str()).collect::<Vec<_>>(), ["Used", "Focused", "Form"]);
        let archived = archive_for_space(&w.reopened(), &w.side, 5).unwrap();
        assert_eq!(archived.iter().map(|tab| (tab.url.as_str(), tab.source)).collect::<Vec<_>>(), [("https://example.com/idle", ArchiveSource::LittleWindow)], "archived as a close does");

        // Used an hour later, the first one goes once its own delay has passed.
        assert!(archive_idle(&w.lib, &mut windows, 12 * HOUR, start + 23 * HOUR, allows).is_empty());
        assert_eq!(archive_idle(&w.lib, &mut windows, 12 * HOUR, start + 23 * HOUR + 1, allows).len(), 1);
        assert_eq!(windows.iter().map(|window| window.title.as_str()).collect::<Vec<_>>(), ["Focused", "Form"]);
        // A shorter delay applies at once.
        assert!(archive_idle(&w.lib, &mut windows, HOUR, start + 24 * HOUR, allows).is_empty(), "the focused window and the used form stay");

        // A window whose page could not be archived stays open and says why.
        let (mut window, page) = w.window(&w.side, "https://example.com/keep", "Keep");
        window.last_active_ms = start;
        let mut windows = vec![window];
        w.lib.store = w.fixture.read_only();
        assert!(archive_idle(&w.lib, &mut windows, HOUR, start + 2 * HOUR, |_: &Recorder| true).is_empty());
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].notice.as_deref(), Some("Could not archive this page, so its window stays open. Close it again to retry."));
        assert!(!page.take().contains(&"close".to_string()), "its page is intact");
    }

    #[test]
    fn the_link_window_choice_is_off_by_default_read_back_and_kept_when_a_save_fails() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let mut choice = LinkWindow::load(&store);
        assert!(!choice.enabled() && choice.notice.is_none(), "off by default");
        choice.choose(&store, true);
        assert!(choice.enabled() && choice.failure.is_none());
        assert!(LinkWindow::load(&fixture.store()).enabled(), "read back after reopening");

        let read_only = fixture.read_only();
        choice.choose(&read_only, false);
        assert!(choice.enabled(), "a failed save keeps the previous choice");
        assert!(choice.failure.as_ref().is_some_and(|failure| !failure.wanted && !failure.error.is_empty()));
        assert!(LinkWindow::load(&fixture.store()).enabled());
        choice.choose(&store, false);
        assert!(!choice.enabled() && choice.failure.is_none() && !LinkWindow::load(&fixture.store()).enabled());

        store.kv_set(LINK_KEY, "maybe").unwrap();
        let unreadable = LinkWindow::load(&store);
        assert!(!unreadable.enabled() && unreadable.notice.is_some(), "an unreadable value opens tabs and says so");
    }
}
