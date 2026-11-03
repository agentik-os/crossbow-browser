//! Application state and the per-frame loop.
//!
//! Drawing code never mutates the model directly: it pushes [`Action`]s, which
//! are applied once per frame after drawing. That keeps the UI code simple and
//! makes every user operation go through one place.

use crate::favicons::Favicons;
use crate::feel::Cue;
use crate::input::{self, WebInput};
use crate::sync_glue::SyncGlue;
use crate::tabs::{FAVORITES_SPACE, Split, Tab};
use crate::texture_retirement::TextureRetirement;
use crate::ui;
use crossbow_core::library::{Library, PinnedNode};
use crossbow_core::model::{Item, ItemKind, Theme};
use crossbow_core::order::key_between;
use crossbow_core::store::Store;
use crossbow_core::{new_id, now_ms};
use crossbow_engine::{ContextMenuInfo, DownloadInfo, WebEvent, WebView, WebViewConfig};
use egui::{Key, Pos2, Rect};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[path = "engine_pump.rs"]
mod engine_pump;
use engine_pump::EnginePump;

#[path = "navigation.rs"]
mod navigation;
use navigation::{NavigationBatch, NavigationOwner, select_navigation_owner};

#[path = "profile_switch.rs"]
pub(crate) mod profile_switch;
pub(crate) use profile_switch::ProfileDeletion;

#[path = "little.rs"]
pub(crate) mod little;

#[path = "link_open.rs"]
mod link_open;
#[path = "site_open.rs"]
pub(crate) mod site_open;
#[path = "pip.rs"]
pub(crate) mod pip;
#[path = "capture_host.rs"]
pub(crate) mod capture_host;

pub const NEW_TAB_URL: &str = "about:blank";

#[derive(Clone, Debug)]
pub enum Action {
    OpenCommandBar { edit_current: bool },
    NewTab { url: String, background: bool },
    SearchSelection { tab: String, profile: String, text: String },
    /// Copy as Quote from a page menu: its selection, the tab's title while
    /// it showed the menu's page, and that page's address.
    CopyAsQuote { text: String, title: String, page_url: String },
    /// `route`: an address its site's rule may open in another profile.
    OpenCommandUrl { url: String, origin: CommandOrigin, new_tab: bool, route: bool },
    /// A link from a page's menu, in its site's profile unless `here`.
    OpenLinkTab { url: String, background: bool, here: bool },
    /// The site notice's Undo.
    UndoSiteOpen,
    /// "Always Open {site} in {profile}…": Settings with the site filled in.
    AlwaysOpenSite { host: String, profile: String },
    StopSiteProfile(String),
    OpenInPeek(String),
    /// ⌥⌘-click in `tab`: a Little Window in its Space, or a background tab.
    OpenInLittleWindow { tab: String, url: String },
    ActivateTab(String),
    CloseTab(String),
    OpenItem(String),
    PinTab(String),
    UnpinTab(String),
    /// Ask for a tidy title for this pin.
    TidyTitle(String),
    /// Clear a pin's custom title so it shows its page title.
    RestorePageTitle(String),
    /// Open Tidy Today for this Space.
    TidyToday(String),
    UndoTidyToday,
    FavoriteTab(String),
    DeleteItem(String),
    MoveTab { tab: String, index: usize },
    MovePinned { node: String, folder: Option<String>, index: usize },
    TabToPinned { tab: String, folder: Option<String>, index: usize },
    ItemToToday { item: String, index: usize },
    ReorderFavorite { item: String, index: usize },
    /// A sidebar drag let go on a place that takes it: the move it asks for.
    Drop(Box<Action>),
    SwitchSpace(String),
    SwitchSpaceBy(i32),
    NewSpace,
    RenameSpace(String, String),
    SetSpaceTheme(String, Theme),
    SetSpaceIcon(String, String),
    DeleteSpace(String),
    MoveSpaceToProfile { space: String, profile: String },
    DeleteProfile(String),
    NewFolder,
    RenameFolder(String, String),
    ToggleFolder(String),
    DeleteFolder(String),
    RenameItem(String, String),
    Back,
    Forward,
    Reload,
    HardReload,
    Stop,
    ToggleSidebar,
    CopyUrl,
    ClearToday,
    RestoreArchived(i64),
    ReopenClosed,
    NewLittleWindow,
    ShowLittleWindows,
    CloseLittleWindows,
    /// A new tab in the active Space at its profile's document app, or
    /// Choose a Document App when it has none.
    NewDocument,
    /// Settings → General, at New Document opens in.
    ChooseDocumentApp,
    SplitWith(String),
    CloseSplit,
    Zoom(i32),
    DevTools,
    ViewSource,
    Print,
    OpenFind,
    OpenSettings,
    OpenChat(crate::chat::Entry),
    /// Ask AI with this text as the first message, sent once.
    AskChat(crate::chat::Entry, String),
    ToggleMute(String),
    /// Show this tab's video in the Picture in Picture player.
    PictureInPicture(String),
    /// Translate Page, Show Original, or a button on this tab's translation card.
    Translate(String, crate::translate::Choice),
    /// Select part of this tab, or of the focused page, to capture.
    Capture(Option<String>),
    /// The Library panel, open on Captures.
    ShowCaptures,
    PeekToTab,
    ClosePeek,
    Toast(String),
    DownloadControl { profile: String, id: u32, command: crossbow_engine::DownloadCommand },
    TidyDownload { profile: String, id: u32, command: crate::tidy_downloads::Command },
    /// ⌘1–⌘9: the number pressed, 1 to 9.
    SelectNumbered(u8),
    NextTab(i32),
    StartOnboarding,
    OpenInSplit(String),
    Duplicate(String),
    MoveTabToSpace { tab: String, space: String },
    ResetItem(String),
    Folder(ui::folders::FolderOp),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Web,
    Chrome,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LibraryDismissal {
    RestoreFocus,
    OutsidePointer,
}

// Search retains its opening target even if synced metadata or focus changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOrigin {
    space: String,
    profile: String,
    tab: Option<String>,
    folder: Option<String>,
}

impl CommandOrigin {
    pub(crate) fn capture(lib: &Library, space: &str, tab: Option<&Tab>, view_profile: Option<&str>) -> Option<Self> {
        let profile = crate::downloads::active_profile(lib, space)?;
        let origin = Self { space: space.into(), profile: profile.into(), tab: tab.map(|tab| tab.id.clone()), folder: None };
        origin.profile(lib, space, tab, view_profile)?;
        Some(origin)
    }

    pub(crate) fn profile<'a>(&'a self, lib: &'a Library, space: &str, tab: Option<&Tab>, view_profile: Option<&str>) -> Option<&'a str> {
        if space != self.space || crate::downloads::active_profile(lib, space)? != self.profile { return None; }
        if let Some(id) = &self.tab {
            let tab = tab.filter(|tab| &tab.id == id)?;
            if (tab.space_id != space && tab.space_id != FAVORITES_SPACE)
                || tab_profile_with_view_owner(lib, tab, view_profile)? != self.profile { return None; }
        }
        lib.effective_profile_preferences(&self.profile)?;
        Some(&self.profile)
    }

    /// Pin a new tab opened from this search inside `folder` (New Tab Inside).
    pub(crate) fn pin_into(&mut self, folder: &str) {
        self.folder = Some(folder.into());
    }

    /// The folder a new tab joins, if one was requested.
    pub(crate) fn pin_folder(&self) -> Option<&str> {
        self.folder.as_deref()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CommandUrlTarget {
    Tab(String),
    NewTab,
}

fn command_url_target(lib: &Library, origin: &CommandOrigin, space: &str, tab: Option<&Tab>, view_profile: Option<&str>, new_tab: bool) -> Option<CommandUrlTarget> {
    origin.profile(lib, space, tab, view_profile)?;
    match tab.filter(|_| !new_tab && origin.tab.is_some()).filter(|tab| tab.url == NEW_TAB_URL || tab.url.is_empty() || view_profile.is_some()) {
        Some(tab) => Some(CommandUrlTarget::Tab(tab.id.clone())),
        None => Some(CommandUrlTarget::NewTab),
    }
}

pub struct CommandBar {
    pub query: String,
    pub edit_current: bool,
    pub origin: Option<CommandOrigin>,
    pub chat_entry: Option<crate::chat::Entry>,
    pub needs_focus: bool,
    pub selected: usize,
    pub opened_at: f64,
    pub results: Vec<crossbow_core::search::Suggestion>,
    pub last_query: Option<String>,
    pub last_snapshot: Option<u64>,
    /// What Tab entered, if anything.
    pub scope: Option<ui::command_bar::Scope>,
    /// Rows Tab can enter a scope from, for the rows last ranked.
    pub offers: Vec<(usize, ui::command_bar::Target)>,
}

impl CommandBar {
    pub(crate) fn ask_ai_action(&self) -> Action {
        match &self.chat_entry {
            Some(entry) => Action::OpenChat(entry.clone()),
            None => Action::Toast("Select a space with a profile, then open Ask AI again.".into()),
        }
    }
}

pub struct Peek {
    pub view: WebView,
    pub opened_at: f64,
    pub closing_at: Option<f64>,
    pub texture: Option<(egui::TextureId, u64)>,
    pub popup_texture: Option<(egui::TextureId, u64)>,
}

fn take_peek_for_tab<T>(lib: &Library, space: &str, peek: &mut Option<T>, owner: impl FnOnce(&T) -> Option<&str>) -> Result<Option<T>, &'static str> {
    let Some(current) = peek.as_ref() else { return Ok(None) };
    let profile = owner(current).ok_or("Peek is closing. It cannot be opened as a tab.")?;
    if profile.is_empty() || crate::downloads::active_profile(lib, space) != Some(profile) {
        return Err("Peek is unchanged. Select a space in its original profile, then retry.");
    }
    Ok(peek.take())
}

pub(crate) fn retire_peek_textures(retirement: &TextureRetirement, main: Option<(egui::TextureId, u64)>, popup: Option<(egui::TextureId, u64)>) {
    for (id, _) in [main, popup].into_iter().flatten() { retirement.retire(id); }
}

fn send_web_escape(view: &WebView) {
    for pressed in [true, false] {
        view.key(&crossbow_engine::KeyInput {
            key: crossbow_engine::NamedKey::Escape, pressed, repeat: false,
            modifiers: Default::default(), text: None,
        });
    }
}

pub struct FindBar {
    pub query: String,
    pub last_sent: String,
    pub just_opened: bool,
}

#[derive(Clone, Debug)]
pub enum MenuKind {
    Web { tab: String, profile: String, info: ContextMenuInfo },
    Tab(String),
    Item(String),
    Folder(String),
    Space(String),
    /// New Tab, New Folder and New Split View, from the sidebar's "+".
    Add,
    /// A Space's folders, as a destination for a pinned item or tab.
    Folders(ui::folders::FolderTarget),
    /// The Today divider's menu for a Space.
    Today(String),
    /// The key button's saved passwords for a tab's page.
    Logins { tab: String },
    /// "Fill Address ›": the profile's saved addresses for a tab's address
    /// form, filled only where the menu was opened.
    Addresses(ui::logins::FillMenu),
    /// "Fill Card ›": the profile's saved cards for a tab's card fields,
    /// filled only where the menu was opened.
    Cards(ui::logins::FillMenu),
    /// The shield's menu for a tab's page: its site's blocking and count.
    ContentBlocking { tab: String },
}

pub struct MenuState {
    pub pos: Pos2,
    pub kind: MenuKind,
}

pub struct Toast {
    pub text: String,
    pub at: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RenameTarget {
    Space(String),
    Folder(String),
    Item(String),
}

pub struct SpaceAnim {
    pub from: String,
    pub to: String,
    pub start: f64,
    pub dir: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DragKind {
    Tab(String),
    Pinned(String),
    Favorite(String),
}

pub struct DragState {
    pub kind: DragKind,
    pub start: Pos2,
    pub active: bool,
}

pub struct UiState {
    pub sidebar_visible: bool,
    pub sidebar_width: f32,
    pub command: Option<CommandBar>,
    pub peek: Option<Peek>,
    pub find: Option<FindBar>,
    pub settings: Option<ui::settings::SettingsState>,
    pub onboarding: Option<ui::onboarding::Onboarding>,
    pub menu: Option<MenuState>,
    pub toasts: Vec<Toast>,
    pub renaming: Option<(RenameTarget, String)>,
    pub drag: Option<DragState>,
    pub space_anim: Option<SpaceAnim>,
    pub show_archive: bool,
    pub show_downloads: bool,
    pub focus: Focus,
    /// Content rects of the visible panes this frame (tab id → rect).
    pub panes: Vec<(String, Rect)>,
    /// The focused tab's address area this frame, which permission prompts hang from.
    pub address: Option<Rect>,
    pub focused_pane: Option<String>,
    /// Web view that owns the pointer while a button is held.
    pub pointer_capture: Option<String>,
    /// Web views that heard the pointer over them and not yet that it left.
    pub pointer_over: HashSet<String>,
    /// (time, position, button, count) of the last press, for double clicks.
    pub last_press: Option<(f64, Pos2, egui::PointerButton, i32)>,
    /// Floating sidebar shown while hovering the window edge with the sidebar hidden.
    pub sidebar_peek: bool,
    /// Page the next settings window opens on.
    pub settings_page_hint: Option<ui::settings::Page>,
    /// Profile for new Spaces chosen in Settings → Spaces; None follows the active Space.
    pub new_space_profile: Option<String>,
}

impl UiState {
    fn open_settings(&mut self, settings: ui::settings::SettingsState) {
        self.settings = Some(settings);
        self.settings_page_hint = None;
        self.command = None;
        self.menu = None;
        self.show_archive = false;
        self.focus = Focus::Chrome;
    }
}

pub struct CrossbowApp {
    pub data_dir: PathBuf,
    pub downloads_dir: PathBuf,
    pub lib: Library,
    pub tabs: Vec<Tab>,
    pub active_space: String,
    pub active_tab: HashMap<String, String>,
    pub splits: HashMap<String, Split>,
    pub favicons: Favicons,
    pub app_icon: crate::app_icon::AppIcon,
    pub window_chrome: crate::window_appearance::WindowChrome,
    pub shortcuts: crate::shortcuts::Shortcuts,
    /// This device's rules for links from other apps.
    pub link_routing: crossbow_core::link_routing::LinkRouting,
    /// Sites that open in a chosen profile, on this device.
    pub site_profiles: crossbow_core::site_profiles::SiteProfiles,
    /// "Opened in Work", with Undo.
    pub site_notice: Option<site_open::SiteNotice>,
    /// A site and profile for Settings → Spaces to fill in.
    pub site_prefill: Option<(String, String)>,
    /// Open links from other apps in a Little Window instead of a tab.
    pub link_window: little::LinkWindow,
    /// ⌥⌘-click, Shift-click and archiving Little Windows.
    pub link_prefs: crate::link_prefs::LinkPreferences,
    /// Open a Little Window from anywhere.
    pub hotkey: crate::hotkey::GlobalHotkey,
    /// Window & Feedback: page-top dragging, sound effects and reorder haptics.
    pub feel: crate::feel::Feel,
    /// Search-engine suggestions for the command bar.
    pub suggest: crate::suggestions::Suggestions,
    /// This pass has a key's auto-repeat, so its actions play no sound.
    key_repeat: bool,
    /// Open Little Windows, oldest first.
    pub(crate) littles: Vec<little::Little>,
    /// Warn Before Quitting, shared with the macOS quit hook.
    pub quit: std::rc::Rc<std::cell::RefCell<crate::quit::QuitGuard>>,
    /// Signed updates: Settings → General → Updates and Check for Updates….
    pub updates: crate::updates::Updates,
    pub browsing_data: ui::settings::BrowsingDataState,
    pub passwords: crate::passwords::Passwords,
    /// Permission prompts and links to other apps, per tab.
    pub site_permissions: crate::site_permissions::SitePermissions,
    pub material_state: crate::native_window::MaterialState,
    native_window: Option<crate::native_window::NativeWindow>,
    display_snapshot: Option<crate::native_window::DisplaySnapshot>,
    screen_geometry: Option<crossbow_engine::ScreenGeometry>,
    pub textures: HashMap<String, (egui::TextureId, u64)>,
    pub popup_textures: HashMap<String, (egui::TextureId, u64)>,
    pub texture_retirement: TextureRetirement,
    pub render_state: egui_wgpu::RenderState,
    pub ui: UiState,
    pub sync: SyncGlue,
    pub actions: Vec<Action>,
    pub grain: egui::TextureHandle,
    pub texture_bank: ui::theme::TextureBank,
    pub downloads: Vec<DownloadInfo>,
    /// Tidy Downloads suggestions for this session's finished downloads.
    pub tidy: crate::tidy_downloads::Tidy,
    /// Ask on Page in the Find bar, while in Ask mode. Session-only.
    pub ask: Option<crate::ask_page::Ask>,
    /// Whether Ask on Page is on for a profile, read when Find opens or Settings stops covering it.
    ask_available: Option<(String, bool)>,
    pub errors: HashMap<String, (String, i32, String)>,
    pub system_dark: bool,
    performance: Option<crate::performance::FrameTrace>,
    engine_pump: EnginePump,
    hidden_views: HashSet<String>,
    last_save: f64,
    last_archive_check: f64,
    last_little_archive_check: f64,
    last_ui_kv: String,
    /// Shortcut presses this frame sends to the page first.
    deferred_keys: Vec<crate::shortcuts::Deferred>,
    /// Shortcut presses a page has, until it reports them unhandled.
    waiting_keys: crate::shortcuts::Waiting,
    /// Press once for the website, twice for Crossbow: the first press, in memory only.
    first_press: crate::shortcuts::FirstPress,
    /// Tidy Tab Titles and Tidy Today: requests, suggestions and one Undo.
    pub tidy_tabs: crate::tidy_tabs::Tidy,
    /// Translate Page: each tab's detection, offer and translation. Session-only.
    pub translator: crate::translate::Translator,
    /// Show unread badges and Show hover previews, per profile.
    pub sidebar: crate::sidebar_prefs::SidebarPrefs,
    /// Pinned tabs and favorites that changed in the background. Session-only.
    pub unread: crate::unread::Unread,
    /// The sidebar's hover preview, if one is offered or shown.
    pub previews: ui::hover_preview::HoverPreviews,
    /// 5-Second Previews: Shift held over a page's link. Session-only.
    pub link_previews: crate::link_preview::Previews,
    /// Picture in Picture: the player, its window and its choices.
    pub(crate) pip: pip::PipHost,
    /// Capture: the selection, its card, the Captures list and its library.
    pub(crate) capture: capture_host::CaptureHost,
    /// ⌘1–⌘9: whether Favorites count first, and where ⌘9 goes.
    pub(crate) numbered: crate::numbered_tabs::NumberedTabs,
    /// Copy as Quote: whether the page menu offers it, and the pasteboard.
    pub(crate) quote: crate::quote::QuoteHost,
    /// Each profile's website appearance, applied by the engine.
    pub(crate) website_appearance: crate::website_appearance::WebsiteAppearance,
    /// Each profile's content blocking switches and lists, applied by the engine.
    pub(crate) content_blocking: crate::content_blocking::ContentBlocking,
    /// The daily backup and why a waiting restore didn't finish at launch.
    pub backups: crate::backups::Backups,
}

impl CrossbowApp {
    pub fn new(cc: &eframe::CreationContext<'_>, data_dir: PathBuf) -> CrossbowApp {
        ui::window::install_fonts(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        let pip = pip::PipHost::default();
        let player = pip.wake.clone();
        crossbow_engine::set_waker(move |_| {
            // The engine dispatches off the CEF callback stack once its timer is due.
            ctx.request_repaint_of(egui::ViewportId::ROOT);
            if let Some(player) = *player.lock().unwrap_or_else(std::sync::PoisonError::into_inner) { ctx.request_repaint_of(player); }
        });
        let render_state = cc.wgpu_render_state.clone().expect("Crossbow needs the wgpu renderer");
        let library = data_dir.join("crossbow.sqlite");
        // A restore chosen in Settings finishes before the library opens, and a
        // library this version upgrades is backed up first.
        let backups = crate::backups::Backups::launch(&library);
        let store = Store::open(&library).expect("open local store");
        let mut lib = Library::open(store).expect("open library");
        let first_run = lib.spaces.is_empty();
        lib.ensure_defaults().expect("create default space");
        // A profile deletion that a stop cut short finishes with its captures.
        crossbow_core::captures::finish_deletions(&data_dir, |profile| lib.profiles.contains_key(profile));
        // Each profile's first page already opens in its chosen appearance.
        let mut website_appearance = crate::website_appearance::WebsiteAppearance::default();
        website_appearance.launch(&lib);
        // Lists open off the UI thread; pages load unfiltered until they're ready.
        let mut content_blocking = crate::content_blocking::ContentBlocking::system(data_dir.clone());
        content_blocking.launch(&lib, &cc.egui_ctx);
        let tabs: Vec<Tab> = lib.store.tabs().unwrap_or_default().into_iter().map(Tab::from_row).collect();

        let ui_state: serde_json::Value = lib
            .store
            .kv_get("ui_state")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let (active_tab, splits) = restore_tab_layout(&lib, &tabs, &ui_state);
        let active_space = ui_state["space"]
            .as_str()
            .filter(|s| lib.spaces.contains_key(*s))
            .map(String::from)
            .unwrap_or_else(|| lib.spaces_ordered()[0].0.clone());
        let focused_pane = crate::split_session::focused_pane(&lib, &tabs, &active_space,
            active_tab.get(&active_space).map(String::as_str), &splits, ui_state["focused_pane"].as_str()).map(str::to_owned);
        let downloads_dir = directories::UserDirs::new()
            .and_then(|u| u.download_dir().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| data_dir.join("Downloads"));
        let wake_ctx = cc.egui_ctx.clone();
        let sync = SyncGlue::new(&data_dir, &lib, move || wake_ctx.request_repaint());
        let system_dark = cc.egui_ctx.theme() == egui::Theme::Dark;
        let onboarded = lib.store.kv_get("onboarded").ok().flatten().is_some();

        let mut app_icon = crate::app_icon::AppIcon::new(&data_dir);
        ui::window::restore_app_icon(&mut app_icon, &cc.egui_ctx);
        let texture_bank = ui::theme::TextureBank::new(&cc.egui_ctx);
        let window_chrome = crate::window_appearance::WindowChrome::load(&lib.store);
        let shortcuts = crate::shortcuts::Shortcuts::load(&lib.store);
        let link_routing = crossbow_core::link_routing::LinkRouting::load(&lib.store);
        let site_profiles = crossbow_core::site_profiles::SiteProfiles::load(&lib.store);
        let link_window = little::LinkWindow::load(&lib.store);
        let link_prefs = crate::link_prefs::LinkPreferences::load(&lib.store);
        let hotkey = crate::hotkey::GlobalHotkey::load(&lib.store, crate::hotkey::system(&cc.egui_ctx));
        let (sounds, haptics) = crate::feel::systems();
        let feel = crate::feel::Feel::load(&lib.store, sounds, haptics);
        let suggest_ctx = cc.egui_ctx.clone();
        let suggest = crate::suggestions::Suggestions::network(move || suggest_ctx.request_repaint());
        if let Some(notice) = &hotkey.notice { log::warn!("{notice}"); }
        if let Err(error) = little::menu::install(&cc.egui_ctx) {
            log::warn!("Could not add New Little Window to the app menu: {error}");
        }
        let link_ctx = cc.egui_ctx.clone();
        // Links can arrive while AppKit holds the main thread; wake from another thread.
        crate::links::inbox().set_waker(move || {
            let ctx = link_ctx.clone();
            let _ = std::thread::Builder::new().name("crossbow-link-wake".into()).spawn(move || ctx.request_repaint_of(egui::ViewportId::ROOT));
        });
        let quit = std::rc::Rc::new(std::cell::RefCell::new(crate::quit::QuitGuard::load(&lib.store)));
        if let Err(error) = crate::quit::install(&quit, &cc.egui_ctx) {
            log::warn!("Quit confirmation for ⌘Q: {error}");
            quit.borrow_mut().unavailable = Some(format!("⌘Q and Quit in the menu can't ask first on this Mac ({error}). Closing the last window still asks."));
        }
        let update_ctx = cc.egui_ctx.clone();
        let updates = crate::updates::Updates::load(&lib.store, &data_dir, crossbow_core::update::BUILD, std::sync::Arc::new(crossbow_core::update::codesign::System), move || update_ctx.request_repaint());
        // After New Little Window, so this one lands right after About.
        if let Err(error) = crate::updates::menu::install(&cc.egui_ctx) {
            log::warn!("Could not add Check for Updates to the app menu: {error}");
        }
        let native_window = crate::native_window::NativeWindow::new(cc, &cc.egui_ctx)
            .map_err(|error| log::warn!("Native window setup: {error}")).ok();
        let display_snapshot = native_window.as_ref().and_then(|window| window.display_snapshot());
        let screen_geometry = display_snapshot.and_then(|display| crate::display_geometry::screen_geometry(display, cc.egui_ctx.pixels_per_point()));
        let mut app = CrossbowApp {
            performance: crate::performance::FrameTrace::from_env(&data_dir),
            engine_pump: EnginePump::default(),
            favicons: Favicons::new(&data_dir),
            app_icon,
            window_chrome,
            shortcuts,
            link_routing,
            site_profiles,
            site_notice: None,
            site_prefill: None,
            link_window,
            link_prefs,
            hotkey,
            feel,
            suggest,
            key_repeat: false,
            littles: Vec::new(),
            quit,
            updates,
            browsing_data: ui::settings::BrowsingDataState::default(),
            passwords: crate::passwords::Passwords::new(&data_dir),
            site_permissions: crate::site_permissions::SitePermissions::new(Box::new(crate::site_permissions::Native)),
            material_state: crate::native_window::MaterialState {
                transparent: false,
                opaque_reason: Some(if cfg!(target_os = "macos") { crate::native_window::OpaqueReason::InstallationFailed } else { crate::native_window::OpaqueReason::UnsupportedPlatform }),
                accessibility: crate::native_window::AccessibilityDisplay::default(),
            },
            native_window,
            display_snapshot,
            screen_geometry,
            grain: texture_bank.get(crossbow_core::model::Texture::Grain).clone(),
            texture_bank,
            data_dir,
            downloads_dir,
            lib,
            tabs,
            active_space,
            active_tab,
            splits,
            textures: HashMap::new(),
            popup_textures: HashMap::new(),
            texture_retirement: TextureRetirement::new(&cc.egui_ctx),
            render_state,
            ui: UiState {
                sidebar_visible: ui_state["sidebar"].as_bool().unwrap_or(true),
                sidebar_width: ui_state["sidebar_width"].as_f64().unwrap_or(252.0) as f32,
                command: None,
                peek: None,
                find: None,
                settings: None,
                onboarding: None,
                menu: None,
                toasts: Vec::new(),
                renaming: None,
                drag: None,
                space_anim: None,
                show_archive: false,
                show_downloads: false,
                focus: Focus::Web,
                panes: Vec::new(),
                address: None,
                focused_pane,
                pointer_capture: None,
                pointer_over: HashSet::new(),
                last_press: None,
                sidebar_peek: false,
                settings_page_hint: None,
                new_space_profile: None,
            },
            sync,
            actions: Vec::new(),
            downloads: Vec::new(),
            tidy: Default::default(),
            ask: None,
            ask_available: None,
            errors: HashMap::new(),
            system_dark,
            hidden_views: HashSet::new(),
            last_save: 0.0,
            last_archive_check: 0.0,
            last_little_archive_check: 0.0,
            last_ui_kv: String::new(),
            deferred_keys: Vec::new(),
            waiting_keys: crate::shortcuts::Waiting::default(),
            first_press: crate::shortcuts::FirstPress::default(),
            tidy_tabs: crate::tidy_tabs::Tidy::default(),
            translator: crate::translate::Translator::new(crate::translate::preferred_language()),
            sidebar: crate::sidebar_prefs::SidebarPrefs::default(),
            unread: crate::unread::Unread::default(),
            previews: ui::hover_preview::HoverPreviews::default(),
            link_previews: Default::default(),
            pip,
            capture: capture_host::CaptureHost::default(),
            numbered: Default::default(),
            quote: Default::default(),
            website_appearance,
            content_blocking,
            backups,
        };
        if let Some(text) = crate::backups::restored_notice(&app.lib, &app.data_dir) {
            app.ui.toasts.push(Toast { text, at: cc.egui_ctx.input(|i| i.time) });
        }
        if app.backups.restore_error.is_some() {
            app.ui.toasts.push(Toast { text: "The restore didn't finish; your library is unchanged. See Settings → Advanced.".into(), at: cc.egui_ctx.input(|i| i.time) });
        }
        if first_run || !onboarded || std::env::var("CROSSBOW_ONBOARDING").is_ok() {
            app.ui.onboarding = Some(ui::onboarding::Onboarding::new(&cc.egui_ctx));
        } else if app.active_tab_id().is_none() {
            app.actions.push(Action::OpenCommandBar { edit_current: false });
        }
        app
    }

    // -- lookups ---------------------------------------------------------------

    pub fn tab(&self, id: &str) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn tab_mut(&mut self, id: &str) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn active_tab_id(&self) -> Option<String> {
        self.active_tab.get(&self.active_space).filter(|id| self.tab(id).is_some()).cloned()
    }

    /// The tab that keyboard commands act on (focused split pane or active tab).
    pub fn focused_tab_id(&self) -> Option<String> {
        if let Some(p) = &self.ui.focused_pane {
            if self.ui.panes.iter().any(|(id, _)| id == p) {
                return Some(p.clone());
            }
        }
        self.active_tab_id()
    }

    pub fn focused_view(&self) -> Option<WebView> {
        self.focused_tab_id().and_then(|id| self.tab(&id).and_then(|t| t.view.clone()))
    }

    /// Whether `profile` turned Ask on Page on. Reads the store only for a
    /// new profile or when `refresh` is set, never every frame.
    pub(crate) fn ask_available(&mut self, profile: &str, refresh: bool) -> bool {
        if refresh || self.ask_available.as_ref().is_none_or(|(cached, _)| cached != profile) {
            self.ask_available = Some((profile.to_owned(), crate::ask_page::enabled(&self.lib, profile)));
        }
        self.ask_available.as_ref().is_some_and(|(_, on)| *on)
    }

    fn navigation_owner(&self) -> Option<NavigationOwner<WebView>> {
        select_navigation_owner(self.ui.peek.as_ref().map(|peek| peek.view.clone()), || {
            self.focused_tab_id().map(|id| {
                let view = self.tab(&id).and_then(|tab| tab.view.clone());
                (id, view)
            })
        })
    }

    /// Unpinned tabs of a space, in sidebar order.
    pub fn today_tabs(&self, space: &str) -> Vec<&Tab> {
        crate::tabs::today(&self.tabs, space)
    }

    pub fn tab_for_item(&self, item: &str) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.item_id.as_deref() == Some(item))
    }

    pub fn current_profile(&self) -> String {
        self.lib.spaces.get(&self.active_space).map(|s| s.profile_id.clone()).unwrap_or_else(|| self.lib.default_profile_id())
    }

    pub fn palette(&self) -> ui::theme::Palette {
        let theme = self.lib.spaces.get(&self.active_space).map(|s| s.theme.clone()).unwrap_or_else(|| Theme::preset(0));
        ui::theme::palette(&theme, self.system_dark)
    }

    pub fn chrome_tint_opacity(&self) -> f32 {
        self.window_chrome.tint_opacity(self.material_state.transparent)
    }

    fn update_native_window(&mut self, ctx: &egui::Context) {
        let pal = self.palette();
        let background = pal.stops.first().copied().unwrap_or(pal.panel);
        if let Some(window) = &mut self.native_window {
            self.material_state = window.reconcile(crate::native_window::WindowAppearance {
                transparent: self.window_chrome.material_requested(self.ui.onboarding.is_some()),
                dark: pal.dark,
                opaque_background: [background.r(), background.g(), background.b()],
            });
            self.display_snapshot = window.display_snapshot();
        }
        let scale = ctx.pixels_per_point();
        self.screen_geometry = self.display_snapshot.and_then(|display| crate::display_geometry::screen_geometry(display, scale));
        let root = self.display_snapshot.and_then(|display| crate::display_geometry::root_geometry(display, scale));
        // Scale can change while monitor bounds remain equal. Keep hidden tabs in
        // sync too; WebView deduplicates notifications without resizing them.
        for view in self.tabs.iter().filter_map(|tab| tab.view.as_ref()) {
            view.set_display_geometry(self.screen_geometry, root, scale);
        }
        if let Some(peek) = &self.ui.peek { peek.view.set_display_geometry(self.screen_geometry, root, scale); }
    }

    pub fn update_view_position(&self, view: &WebView, origin: Pos2, scale: f32) {
        let root = self.display_snapshot.and_then(|display| crate::display_geometry::root_geometry(display, scale));
        // Also initialize geometry for a WebView created after this frame's native update.
        view.set_display_geometry(self.screen_geometry, root, scale);
        view.set_view_position(self.display_snapshot.and_then(|display| crate::display_geometry::view_position(display, (origin.x, origin.y), scale)));
    }

    /// Every tab reachable from the sidebar, in order (⌘1–9 and ⌃Tab).
    /// Every favorite, pinned item, folder and Today tab of the active Space,
    /// in sidebar order, open or not.
    fn sidebar_snapshot(&self) -> crate::feel::SidebarOrder {
        let space = self.active_space.clone();
        crate::feel::SidebarOrder::of(&self.lib, &space, &self.current_profile(), self.today_tabs(&space).iter().map(|tab| tab.id.clone()))
    }

    /// Play `cue` for an action that just finished.
    pub fn cue(&mut self, cue: Cue) {
        self.feel.play(cue, self.key_repeat);
    }

    pub fn sidebar_order(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (id, _) in self.lib.favorites(&self.current_profile()) {
            if let Some(t) = self.tab_for_item(&id) {
                out.push(t.id.clone());
            }
        }
        fn walk(app: &CrossbowApp, space: &str, folder: Option<&str>, out: &mut Vec<String>) {
            for node in app.lib.pinned_children(space, folder) {
                match node {
                    PinnedNode::Item(id, _) => {
                        if let Some(t) = app.tab_for_item(&id) {
                            out.push(t.id.clone());
                        }
                    }
                    PinnedNode::Folder(fid, f) => {
                        if !f.collapsed {
                            walk(app, space, Some(&fid), out);
                        }
                    }
                }
            }
        }
        walk(self, &self.active_space, None, &mut out);
        out.extend(self.today_tabs(&self.active_space).iter().map(|t| t.id.clone()));
        out
    }

    // -- tab lifecycle ---------------------------------------------------------

    fn new_tab_record(&mut self, space: &str, url: &str, item_id: Option<String>, after: Option<&str>) -> String {
        let id = new_id();
        let siblings: Vec<String> = self.today_tabs(space).iter().map(|t| t.order.clone()).collect();
        let order = match after.and_then(|a| siblings.iter().position(|o| o == a)) {
            Some(i) => key_between(Some(&siblings[i]), siblings.get(i + 1).map(String::as_str)),
            None => key_between(None, siblings.first().map(String::as_str)),
        };
        let now = now_ms();
        let tab = Tab {
            nav: Default::default(),
            id: id.clone(),
            space_id: space.into(),
            item_id,
            url: url.into(),
            title: String::new(),
            order,
            created_ms: now,
            last_active_ms: now,
            view: None,
            muted: false,
            dirty: true,
        };
        self.tabs.push(tab);
        id
    }

    pub fn ensure_view(&mut self, tab_id: &str, size: (f32, f32), scale: f32) {
        let Some(profile) = self.tab(tab_id).and_then(|tab| tab_profile(&self.lib, tab)).map(str::to_owned) else { return };
        let rs = self.render_state.clone();
        let data_dir = self.data_dir.clone();
        let screen = self.screen_geometry;
        if let Some(t) = self.tab_mut(tab_id) {
            if t.view.is_none() {
                let url = if t.url.is_empty() { NEW_TAB_URL.to_string() } else { t.url.clone() };
                t.view = WebView::new(WebViewConfig {
                    url,
                    profile_id: profile,
                    data_dir,
                    size,
                    scale,
                    screen,
                    device: rs.device.clone(),
                    queue: rs.queue.clone(),
                    saves_logins: true,
                });
                if let Some(v) = &t.view {
                    v.set_muted(t.muted);
                }
            }
        }
    }

    fn activate(&mut self, tab_id: &str) {
        let Some(tab) = self.tab(tab_id) else { return };
        let Some(profile) = tab_profile(&self.lib, tab) else { return };
        if tab.space_id == FAVORITES_SPACE && profile != self.current_profile() { return; }
        let Some(t) = self.tab_mut(tab_id) else { return };
        t.last_active_ms = now_ms();
        t.dirty = true;
        let space = if t.space_id == FAVORITES_SPACE { None } else { Some(t.space_id.clone()) };
        if let Some(space) = space {
            if space != self.active_space {
                self.switch_space(&space);
            }
        }
        self.active_tab.insert(self.active_space.clone(), tab_id.to_string());
        // Leaving a split for a tab outside it closes nothing; the split stays for later.
        self.ui.focused_pane = Some(tab_id.to_string());
        self.ui.focus = Focus::Web;
        if let Some(v) = self.tab(tab_id).and_then(|t| t.view.clone()) {
            v.set_focus(true);
        }
    }

    pub(crate) fn note_activity(&mut self, tab_id: &str) {
        if let Some(tab) = self.tab_mut(tab_id) {
            tab.last_active_ms = now_ms();
            tab.dirty = true;
        }
    }

    fn open_new_tab(&mut self, url: &str, background: bool) -> String {
        let after = self.active_tab_id().and_then(|id| self.tab(&id).filter(|t| t.item_id.is_none()).map(|t| t.order.clone()));
        let space = self.active_space.clone();
        let id = self.new_tab_record(&space, url, None, if background { after.as_deref() } else { None });
        if !background {
            self.activate(&id);
        }
        id
    }

    fn close_tab(&mut self, tab_id: &str) -> crossbow_core::store::Result<Option<bool>> {
        if self.tab(tab_id).is_none() { return Ok(None); }
        let selection = self.tab_selection();
        let Some(tab) = take_closed_tab(&self.lib, &mut self.tabs, tab_id)? else { return Ok(None) };
        let archived = should_archive_tab(&tab);
        self.finish_closed_tabs(vec![tab], selection);
        Ok(Some(archived))
    }

    fn tab_selection(&self) -> RemovalSelection {
        RemovalSelection {
            space: self.active_space.clone(), profile: self.current_profile(),
            active: self.active_tab.clone(), focused: self.ui.focused_pane.clone(),
            panes: self.ui.panes.iter().map(|(id, _)| id.clone()).collect(),
            splits: self.splits.clone(), order: self.sidebar_order(),
        }
    }

    fn finish_closed_tabs(&mut self, tabs: Vec<Tab>, selection: RemovalSelection) {
        for tab in tabs { self.finish_closed_tab(tab); }
        self.reconcile_selection(selection);
    }

    /// Select surviving tabs after removals or a profile change.
    fn reconcile_selection(&mut self, mut selection: RemovalSelection) {
        selection.reconcile(&self.lib, &self.tabs);
        let changed = selection.space != self.active_space || selection.focused != self.ui.focused_pane
            || selection.active.get(&selection.space) != self.active_tab.get(&self.active_space);
        self.active_space = selection.space;
        self.active_tab = selection.active;
        self.ui.focused_pane = selection.focused;
        self.splits.retain(|space, _| self.lib.spaces.contains_key(space));
        if self.ui.space_anim.as_ref().is_some_and(|anim| !self.lib.spaces.contains_key(&anim.from) || !self.lib.spaces.contains_key(&anim.to)) {
            self.ui.space_anim = None;
        }
        if changed {
            self.ui.focus = Focus::Web;
            if let Some(id) = self.ui.focused_pane.clone() {
                self.note_activity(&id);
                if let Some(view) = self.tab(&id).and_then(|tab| tab.view.as_ref()) { view.set_focus(true); }
            }
        }
    }

    fn finish_closed_tab(&mut self, mut tab: Tab) {
        tab.unload();
        let id = tab.id.as_str();
        self.texture_retirement.retire_tab(id, &mut self.textures, &mut self.popup_textures);
        for split in self.splits.values_mut() { split.remove(id); }
        self.splits.retain(|_, split| split.tabs.len() >= 2);
        self.active_tab.retain(|_, active| active != id);
        self.errors.remove(id);
        self.hidden_views.remove(id);
        self.ui.pointer_over.remove(id);
        self.ui.panes.retain(|(pane, _)| pane != id);
        if self.ui.focused_pane.as_deref() == Some(id) { self.ui.focused_pane = None; }
        if self.ui.pointer_capture.as_deref() == Some(id) { self.ui.pointer_capture = None; }
    }

    fn remove_parent(&mut self, parent: ParentRemoval<'_>) -> crossbow_core::library::Result<()> {
        let selection = self.tab_selection();
        let removed = take_removed_parent_tabs(&mut self.lib, &mut self.tabs, parent, now_ms())?;
        self.finish_closed_tabs(removed, selection);
        Ok(())
    }

    fn close_tabs(&mut self, ids: &[String]) -> CloseSummary {
        let mut summary = CloseSummary::default();
        for id in ids {
            summary.record(self.close_tab(id));
        }
        summary
    }

    fn restore_archived(&mut self, rowid: i64, now: f64) {
        match restore_archived_tab(&self.lib, &mut self.tabs, &self.active_space, rowid) {
            Ok(Some(id)) => self.activate(&id),
            Ok(None) => self.ui.toasts.push(Toast { text: "This archived tab is no longer available.".into(), at: now }),
            Err(error) => {
                log::warn!("Could not restore an archived tab; it remains available for retry: {error}");
                self.ui.toasts.push(Toast { text: "Could not restore tab. It is still in the Archive. Try again.".into(), at: now });
            }
        }
    }

    /// A switch the person asked for: it sounds when the Space changes.
    fn switch_space_by_hand(&mut self, space: &str) {
        let from = self.active_space.clone();
        self.switch_space(space);
        if self.active_space != from { self.cue(Cue::SpaceSwitched); }
    }

    pub fn switch_space(&mut self, space: &str) {
        if space == self.active_space || !self.lib.spaces.contains_key(space) {
            return;
        }
        let order: Vec<String> = self.lib.spaces_ordered().into_iter().map(|(id, _)| id).collect();
        let from_i = order.iter().position(|s| *s == self.active_space).unwrap_or(0);
        let to_i = order.iter().position(|s| s == space).unwrap_or(0);
        self.ui.space_anim = Some(SpaceAnim {
            from: self.active_space.clone(),
            to: space.to_string(),
            start: -1.0,
            dir: if to_i >= from_i { 1.0 } else { -1.0 },
        });
        self.active_space = space.to_string();
        self.ui.focused_pane = self.active_tab_id();
    }

    fn pin_tab(&mut self, tab_id: &str, folder: Option<&str>, index: Option<usize>) {
        let Some(t) = self.tab(tab_id) else { return };
        if t.item_id.is_some() {
            return;
        }
        let (url, title, space) = (t.url.clone(), t.display_title(), t.space_id.clone());
        let Ok(item) = self.lib.add_pinned(&space, folder, &url, &title) else { return };
        if let Some(i) = index {
            let _ = self.lib.move_pinned(&item, &space, folder, i);
        }
        self.tidy_tabs.pinned(&self.lib, &item);
        if let Some(t) = self.tab_mut(tab_id) {
            t.item_id = Some(item);
            t.dirty = true;
        }
    }

    /// Tidy's notices become toasts.
    fn tidy_notices(&mut self, now: f64) {
        for text in self.tidy_tabs.notices.drain(..) { self.ui.toasts.push(Toast { text, at: now }); }
    }

    fn unpin(&mut self, item_id: &str, index: Option<usize>) {
        let space = self.active_space.clone();
        let _ = self.lib.delete_item(item_id);
        let tabs_today: Vec<String> = self.today_tabs(&space).iter().map(|t| t.order.clone()).collect();
        if let Some(t) = self.tabs.iter_mut().find(|t| t.item_id.as_deref() == Some(item_id)) {
            t.item_id = None;
            t.space_id = space;
            let i = index.unwrap_or(0).min(tabs_today.len());
            t.order = key_between(i.checked_sub(1).map(|j| tabs_today[j].as_str()), tabs_today.get(i).map(String::as_str));
            t.dirty = true;
        }
    }

    fn open_item(&mut self, item_id: &str) {
        if !self.lib.items.get(item_id).is_some_and(|item| item_in_profile(&self.lib, item, &self.current_profile())) { return; }
        if let Some(t) = self.tab_for_item(item_id) {
            let id = t.id.clone();
            self.activate(&id);
            return;
        }
        let Some(item) = self.lib.items.get(item_id).cloned() else { return };
        let space = match item.kind {
            ItemKind::Favorite => FAVORITES_SPACE.to_string(),
            ItemKind::Pinned => item.space_id.clone().unwrap_or_else(|| self.active_space.clone()),
        };
        let id = self.new_tab_record(&space, &item.url, Some(item_id.to_string()), None);
        if let Some(t) = self.tab_mut(&id) {
            t.title = item.display_title().to_string();
        }
        self.activate(&id);
    }

    // -- per-frame -------------------------------------------------------------

    /// Translate Page for every tab with a page: its reports, then its
    /// detection and requests. Peek and Little Windows' popups aren't tabs.
    fn update_translations(&mut self, ctx: &egui::Context) {
        let now = std::time::Instant::now();
        let tabs = &self.tabs;
        self.translator.retain(|id| tabs.iter().any(|tab| tab.id == id && tab.view.is_some()));
        for tab in &self.tabs {
            let Some(view) = tab.view.as_ref() else { continue };
            let tab_id = tab.id.clone();
            let page = crate::translate::TabPage { tab: &tab_id, view };
            for report in view.take_translate_reports() {
                self.translator.report(&tab_id, report, &page, &self.lib, &crate::translate::PROVIDER, ctx, now);
            }
            self.translator.tick(&tab_id, &page, &self.lib, &crate::translate::PROVIDER, ctx, now);
        }
    }

    /// A Translate Page command or card button for `tab`.
    fn translate(&mut self, tab: &str, choice: crate::translate::Choice, ctx: &egui::Context) {
        let Some(view) = self.tabs.iter().find(|t| t.id == tab).and_then(|t| t.view.as_ref()) else {
            self.actions.push(Action::Toast("Load this tab first, then translate it.".into()));
            return;
        };
        let page = crate::translate::TabPage { tab, view };
        let notice = self.translator.choose(tab, choice, &page, &self.lib, &crate::translate::PROVIDER, ctx, std::time::Instant::now());
        if let Some(notice) = notice { self.actions.push(Action::Toast(notice)); }
    }

    fn process_view_events(&mut self, ctx: &egui::Context) {
        let mut opens = Vec::new();
        let mut changed_items: Vec<(String, String)> = Vec::new();
        let mut download_events = Vec::new();
        let mut notices = Vec::new();
        // A page may open another app by itself only from the tab in front of the user.
        let window_focused = ctx.input(|input| input.focused);
        let front = self.focused_tab_id().filter(|tab| window_focused && self.ui.panes.iter().any(|(id, _)| id == tab));
        let now = ctx.input(|input| input.time);
        for i in 0..self.tabs.len() {
            if self.tabs[i].view.is_none() {
                continue;
            }
            self.tabs[i].sync_from_view();
            let events = self.tabs[i].view.as_ref().unwrap().take_events();
            let tab_id = self.tabs[i].id.clone();
            for e in events {
                match e {
                    WebEvent::OpenUrl { url, background, popup, modifiers } => opens.push((tab_id.clone(), url, background, popup, modifiers)),
                    WebEvent::Favicon { page_url, width, height, rgba } => self.favicons.put(ctx, &page_url, width, height, &rgba),
                    event @ (WebEvent::Download(_) | WebEvent::DownloadRequested(_)) => download_events.push(event),
                    WebEvent::ContextMenu(info) => {
                        let pane = self.ui.panes.iter().find(|(id, _)| *id == tab_id).map(|(_, r)| r.min).unwrap_or(Pos2::ZERO);
                        self.ui.menu = Some(MenuState {
                            pos: pane + egui::vec2(info.x as f32, info.y as f32),
                            kind: MenuKind::Web { tab: tab_id.clone(), profile: self.tabs[i].view.as_ref().unwrap().profile_id().to_owned(), info },
                        });
                    }
                    WebEvent::LoadError { url, code, text } => {
                        self.errors.insert(tab_id.clone(), (url, code, text));
                    }
                    WebEvent::Visited { url, title } => {
                        self.errors.remove(&tab_id);
                        if (url.starts_with("http://") || url.starts_with("https://"))
                            && let Some(profile) = self.tabs.iter().find(|tab| tab.id == tab_id).and_then(|tab| tab_profile(&self.lib, tab))
                        {
                            let _ = self.lib.store.record_visit(profile, &url, &title, now_ms());
                        }
                        self.apply_boosts(&tab_id, &url);
                        if let Some(item) = self.tabs.iter().find(|t| t.id == tab_id).and_then(|t| t.item_id.clone()) {
                            if self.lib.items.get(&item).is_some_and(|it| it.title.is_empty() && !title.is_empty()) {
                                changed_items.push((item, title.clone()));
                            }
                        }
                    }
                    WebEvent::Credential(login) => {
                        // The profile whose context the page runs in; none, as for a
                        // tab whose owner changed, offers nothing.
                        if let Some(profile) = tab_profile(&self.lib, &self.tabs[i]).map(str::to_owned) {
                            self.passwords.offer_login(&tab_id, &profile, &login.origin, login.username.expose(), login.password.expose());
                            ctx.request_repaint();
                        }
                    }
                    WebEvent::Address(address) => {
                        if let Some(profile) = tab_profile(&self.lib, &self.tabs[i]).map(str::to_owned) {
                            self.passwords.offer_address(&tab_id, &profile, &address.origin, address.parts.each_ref().map(|part| part.expose()));
                            ctx.request_repaint();
                        }
                    }
                    WebEvent::Permission(request) => {
                        let view = self.tabs[i].view.as_ref().unwrap();
                        let page = crate::site_permissions::Page { tab: &tab_id, serial: view.serial(), load: request.load };
                        let profile = tab_profile(&self.lib, &self.tabs[i]);
                        if let Some(allow) = self.site_permissions.request(&self.lib.store, page, profile, &request) { view.answer_permission(request.id, allow); }
                        ctx.request_repaint();
                    }
                    WebEvent::PermissionDismissed(id) => self.site_permissions.dismissed(self.tabs[i].view.as_ref().unwrap().serial(), id),
                    WebEvent::ExternalLink(link) => {
                        let page = crate::site_permissions::Page { tab: &tab_id, serial: self.tabs[i].view.as_ref().unwrap().serial(), load: link.load };
                        let profile = tab_profile(&self.lib, &self.tabs[i]);
                        let focused = front.as_deref() == Some(tab_id.as_str());
                        notices.extend(self.site_permissions.link(&self.lib.store, page, profile, &link, focused, now));
                        ctx.request_repaint();
                    }
                    WebEvent::LoginNotFilled => self.ui.toasts.push(Toast { text: crate::ui::logins::NOT_FILLED.into(), at: ctx.input(|i| i.time) }),
                    WebEvent::AddressNotFilled => self.ui.toasts.push(Toast { text: crate::ui::logins::ADDRESS_NOT_FILLED.into(), at: ctx.input(|i| i.time) }),
                    WebEvent::CardNotFilled => self.ui.toasts.push(Toast { text: crate::ui::logins::CARD_NOT_FILLED.into(), at: ctx.input(|i| i.time) }),
                    WebEvent::AddressFillReport(report) | WebEvent::CardFillReport(report) => {
                        if let Some(text) = crate::ui::logins::fill_report_text(report) { self.ui.toasts.push(Toast { text, at: ctx.input(|i| i.time) }); }
                    }
                    WebEvent::Closed => {}
                }
            }
        }
        let pages: Vec<(String, u64, u64)> = self.tabs.iter().filter_map(|tab| tab.view.as_ref().map(|view| (tab.id.clone(), view.serial(), view.page_loads()))).collect();
        let pages: Vec<crate::site_permissions::Page<'_>> = pages.iter().map(|(tab, serial, load)| crate::site_permissions::Page { tab, serial: *serial, load: *load }).collect();
        for reply in self.site_permissions.tick(&pages, &self.active_space) {
            if let Some(view) = self.tabs.iter().filter_map(|tab| tab.view.as_ref()).find(|view| view.serial() == reply.serial) { view.answer_permission(reply.id, reply.allow); }
        }
        let live: std::collections::HashSet<&str> = self.tabs.iter().map(|tab| tab.id.as_str()).collect();
        self.passwords.keep_offers(|tab| live.contains(tab));
        for (item, title) in changed_items {
            let _ = self.lib.update_item(&item, |i| i.title = title);
        }
        let peek_links = self.lib.settings.peek_external_links;
        let (option_click, shift_peek) = (self.link_prefs.option_click.enabled(), self.link_prefs.shift_peek.enabled());
        for (from, url, background, popup, modifiers) in opens {
            match little::click_target(modifiers, background, popup, option_click, shift_peek) {
                little::Click::LittleWindow => {
                    self.actions.push(Action::OpenInLittleWindow { tab: from, url });
                    continue;
                }
                little::Click::Peek => {
                    self.actions.push(Action::OpenInPeek(url));
                    continue;
                }
                little::Click::Usual => {}
            }
            // A new tab or window for a site with a rule opens in its profile,
            // even one headed for Peek. ⌥ held for the click keeps it here.
            if !modifiers.alt && self.open_site_tab(&url, background) { continue; }
            let from_pinned = self.tab(&from).is_some_and(|t| t.item_id.is_some());
            let host_differs = self.tab(&from).and_then(|t| crossbow_core::search::host_of(&t.url)) != crossbow_core::search::host_of(&url);
            if popup || (peek_links && from_pinned && host_differs && !background) {
                self.actions.push(Action::OpenInPeek(url));
            } else {
                self.actions.push(Action::NewTab { url, background });
            }
        }
        if let Some(peek) = &self.ui.peek {
            for e in peek.view.take_events() {
                match e {
                    WebEvent::OpenUrl { url, .. } => peek.view.load_url(&url),
                    event @ (WebEvent::Download(_) | WebEvent::DownloadRequested(_)) => download_events.push(event),
                    // Peek shows no prompts: only what the profile allowed before.
                    WebEvent::Permission(request) => { peek.view.answer_permission(request.id, self.site_permissions.remembered_only(&self.lib.store, peek.view.profile_id(), &request)); }
                    WebEvent::ExternalLink(_) => notices.push(crate::site_permissions::FROM_TABS_ONLY.to_owned()),
                    _ => {}
                }
            }
        }
        for event in download_events { self.process_download_event(ctx, event); }
        notices.extend(self.site_permissions.take_notices());
        for text in notices { self.ui.toasts.push(Toast { text, at: now }); }
    }

    /// "Offer to save passwords from websites": load and save each profile's
    /// switch, and mirror it into the engine before any view of the frame is
    /// created. Open pages of a profile whose switch changed are told, for
    /// their next load.
    fn sync_login_capture(&mut self) {
        crate::passwords::sync_offer_to_save(&mut self.passwords, &self.lib);
        for (profile, on) in self.passwords.offer_to_save_settings() {
            if !crossbow_engine::set_login_capture(&profile, on) { continue; }
            let views = self.tabs.iter().filter_map(|tab| tab.view.as_ref())
                .chain(self.ui.peek.as_ref().map(|peek| &peek.view))
                .chain(self.littles.iter().filter_map(|little| little.host.view()));
            for view in views.filter(|view| view.profile_id() == profile) { view.refresh_login_capture(); }
        }
    }

    fn process_download_events(&mut self, ctx: &egui::Context) {
        let events: Vec<_> = self.tabs.iter().filter_map(|tab| tab.view.as_ref())
            .chain(self.ui.peek.as_ref().map(|peek| &peek.view))
            .chain(self.littles.iter().filter_map(|little| little.host.view()))
            .flat_map(WebView::take_download_events).collect();
        for event in events { self.process_download_event(ctx, event); }
    }

    fn process_download_event(&mut self, ctx: &egui::Context, event: WebEvent) {
        let info = match event {
            WebEvent::DownloadRequested(request) => {
                let mut info = request.info.clone();
                let result = crate::downloads::Preferences::load(&self.lib, &info.profile_id).and_then(|preferences| {
                    let directory = preferences.destination(&self.downloads_dir)?;
                    request.continue_in(&directory, preferences.ask_each_time)
                });
                if let Err(error) = result { info.destination_error = Some(error); info.in_progress = false; }
                info
            }
            WebEvent::Download(info) => info,
            _ => return,
        };
        let visible = crate::downloads::active_profile(&self.lib, &self.active_space) == Some(info.profile_id.as_str());
        let finished = info.complete.then(|| info.clone());
        let completed = crate::downloads::update(&mut self.downloads, info);
        if let Some(info) = finished.filter(|_| completed) {
            let private = crate::ai_features::private_profile(&info.profile_id);
            self.tidy.completed(&info, &self.lib, private, &crate::ai_features::Local, ctx);
        }
        if visible {
            self.ui.show_downloads = true;
            if completed {
                // This toast may outlive a profile switch; do not expose the
                // originating profile's filename on another profile's screen.
                self.ui.toasts.push(Toast { text: "Download completed.".into(), at: ctx.input(|i| i.time) });
            }
        }
    }

    fn apply_boosts(&self, tab_id: &str, url: &str) {
        let Some(host) = crossbow_core::search::host_of(url) else { return };
        let Some(view) = self.tab(tab_id).and_then(|t| t.view.clone()) else { return };
        for b in self.lib.boosts.values().filter(|b| b.enabled && (b.host == host || host.ends_with(&format!(".{}", b.host)))) {
            if !b.css.is_empty() {
                let css = serde_json::to_string(&b.css).unwrap_or_default();
                view.execute_js(&format!(
                    "(()=>{{const s=document.createElement('style');s.dataset.crossbowBoost='1';s.textContent={css};document.documentElement.appendChild(s);}})()"
                ));
            }
            if !b.js.is_empty() {
                view.execute_js(&b.js);
            }
        }
    }

    /// Register or refresh the egui textures of every visible web view.
    pub fn texture_for(&mut self, tab_id: &str) -> Option<(egui::TextureId, egui::Vec2)> {
        let view = self.tab(tab_id)?.view.clone()?;
        self.view_texture(tab_id, &view)
    }

    /// Register or refresh `view`'s latest frame as the egui texture kept under `key`.
    pub(crate) fn view_texture(&mut self, key: &str, view: &WebView) -> Option<(egui::TextureId, egui::Vec2)> {
        let rs = self.render_state.clone();
        let entry = self.textures.get(key).copied();
        view.with_texture(|tex| {
            let tex = tex?;
            let size = egui::vec2(tex.width as f32, tex.height as f32);
            let id = native_texture(&rs, entry, tex);
            if entry != Some((id, tex.generation)) { self.textures.insert(key.to_string(), (id, tex.generation)); }
            Some((id, size))
        })
    }

    pub fn popup_texture_for(&mut self, tab_id: &str) -> Option<(egui::TextureId, egui::Vec2, Rect)> {
        let view = self.tab(tab_id)?.view.clone()?;
        self.view_popup_texture(tab_id, &view)
    }

    pub(crate) fn view_popup_texture(&mut self, key: &str, view: &WebView) -> Option<(egui::TextureId, egui::Vec2, Rect)> {
        let rs = self.render_state.clone();
        let entry = self.popup_textures.get(key).copied();
        view.with_popup(|p| {
            let (tex, (x, y, w, h)) = p?;
            let id = native_texture(&rs, entry, tex);
            if entry != Some((id, tex.generation)) { self.popup_textures.insert(key.to_string(), (id, tex.generation)); }
            Some((
                id,
                egui::vec2(tex.width as f32, tex.height as f32),
                Rect::from_min_size(egui::pos2(x as f32, y as f32), egui::vec2(w as f32, h as f32)),
            ))
        })
    }

    /// Hide views that are not on screen so Chromium throttles them. The
    /// Picture in Picture player's tab keeps painting.
    fn update_visibility(&mut self) {
        let visible: HashSet<String> = self.ui.panes.iter().map(|(id, _)| id.clone()).chain(self.pip.pip.painting().map(str::to_owned)).collect();
        for t in &self.tabs {
            let Some(v) = &t.view else { continue };
            let hidden = !visible.contains(&t.id);
            let was_hidden = self.hidden_views.contains(&t.id);
            if hidden != was_hidden {
                v.set_hidden(hidden);
                if hidden {
                    self.hidden_views.insert(t.id.clone());
                } else {
                    self.hidden_views.remove(&t.id);
                }
            }
        }
    }

    /// Unread badges: watch loaded pinned tabs and favorites whose profile
    /// shows them. Other tabs are left out, which clears theirs.
    fn update_unread(&mut self) {
        let (lib, prefs, panes) = (&self.lib, &mut self.sidebar, &self.ui.panes);
        self.unread.update(self.tabs.iter().filter(|tab| tab.item_id.is_some() && tab.view.is_some()).filter_map(|tab| {
            let profile = tab_profile(lib, tab)?;
            prefs.unread(&lib.store, profile).then(|| crate::unread::Watched {
                tab: &tab.id, title: &tab.title, loaded: true, loading: tab.nav.loading,
                visible: panes.iter().any(|(id, _)| *id == tab.id),
            })
        }));
    }

    pub(crate) fn chat_account_scope(&self) -> crate::chat::AccountScope {
        self.sync.session.as_ref().map(|session| (session.base_url.clone(), session.account_id.clone()))
    }

    pub(crate) fn open_command_bar(&mut self, ctx: &egui::Context, edit_current: bool) {
        let query = if edit_current { self.focused_tab_id().and_then(|t| self.tab(&t).map(|t| t.url.clone())).filter(|u| u != NEW_TAB_URL).unwrap_or_default() } else { String::new() };
        let focused = self.focused_tab_id();
        let tab = focused.as_deref().and_then(|id| self.tab(id)).filter(|_| edit_current);
        let origin = CommandOrigin::capture(&self.lib, &self.active_space, tab, tab.and_then(|tab| tab.view.as_ref().map(WebView::profile_id)));
        let chat_entry = crate::chat::Entry::capture(&self.lib, &self.active_space, &self.chat_account_scope());
        self.ui.command = Some(CommandBar { query, edit_current, origin, chat_entry, needs_focus: true, selected: 0, opened_at: ctx.input(|i| i.time), results: Vec::new(), last_query: None, last_snapshot: None, scope: None, offers: Vec::new() });
        self.ui.settings = None;
        self.ui.show_archive = false;
        self.ui.focus = Focus::Chrome;
        self.ui.menu = None;
    }

    pub(crate) fn dismiss_library(&mut self, ctx: &egui::Context, reason: LibraryDismissal) -> bool {
        if !dismiss_library(ctx, &mut self.ui.show_archive, &mut self.ui.focus, self.ui.find.as_mut(), reason) { return false; }
        if reason == LibraryDismissal::RestoreFocus { self.focus_restored_web_view(); }
        true
    }

    pub(crate) fn dismiss_settings(&mut self, ctx: &egui::Context) -> bool {
        if !dismiss_settings(ctx, &mut self.ui.settings, &mut self.ui.focus, self.ui.find.as_mut()) { return false; }
        self.window_chrome.cancel();
        self.focus_restored_web_view();
        true
    }

    fn focus_restored_web_view(&self) {
        if self.ui.focus == Focus::Web {
            let view = self.ui.peek.as_ref().map(|peek| peek.view.clone()).or_else(|| self.focused_view());
            if let Some(view) = view { view.set_focus(true); }
        }
    }

    fn run_shortcuts(&mut self, ctx: &egui::Context, commands: Vec<crate::shortcuts::Command>, out: &mut Vec<Action>) {
        use crate::shortcuts::Command;
        let mut push = |a: Action| out.push(a);

        for command in commands {
            match command {
                Command::ReopenClosed => push(Action::ReopenClosed),
                Command::NewLittleWindow => push(Action::NewLittleWindow),
                Command::ShowLittleWindows => push(Action::ShowLittleWindows),
                Command::CloseLittleWindows => push(Action::CloseLittleWindows),
                Command::NewDocument => push(Action::NewDocument),
                // Open before drawing so text in the same input batch belongs to search, not the page.
                Command::NewTab => self.open_command_bar(ctx, false),
                Command::EditUrl => self.open_command_bar(ctx, true),
                Command::CloseTab => {
                    if self.ui.peek.is_some() {
                        push(Action::ClosePeek);
                    } else if let Some(t) = self.focused_tab_id() {
                        push(Action::CloseTab(t));
                    }
                }
                Command::HardReload => push(Action::HardReload),
                Command::Reload => push(Action::Reload),
                Command::Back => push(Action::Back),
                Command::Forward => push(Action::Forward),
                Command::PreviousSpace => push(Action::SwitchSpaceBy(-1)),
                Command::NextSpace => push(Action::SwitchSpaceBy(1)),
                Command::ToggleSidebar => push(Action::ToggleSidebar),
                Command::Favorite => {
                    if let Some(t) = self.focused_tab_id() {
                        push(Action::FavoriteTab(t));
                    }
                }
                Command::PinTab => {
                    if let Some(t) = self.focused_tab_id() {
                        let pinned = self.tab(&t).and_then(|x| x.item_id.clone());
                        push(if pinned.is_some() { Action::UnpinTab(t) } else { Action::PinTab(t) });
                    }
                }
                Command::NewFolder => push(Action::NewFolder),
                Command::CopyUrl => push(Action::CopyUrl),
                Command::Find => push(Action::OpenFind),
                Command::ZoomIn => push(Action::Zoom(1)),
                Command::ZoomOut => push(Action::Zoom(-1)),
                Command::ActualSize => push(Action::Zoom(0)),
                Command::Settings => push(Action::OpenSettings),
                Command::DevTools => push(Action::DevTools),
                Command::ViewSource => push(Action::ViewSource),
                Command::Print => push(Action::Print),
                Command::Capture => push(Action::Capture(None)),
                Command::ClearToday => push(Action::ClearToday),
                Command::SplitView => {
                    if let Some(t) = self.focused_tab_id() {
                        push(Action::SplitWith(t));
                    }
                }
                Command::NextTab => push(Action::NextTab(1)),
                Command::PreviousTab => push(Action::NextTab(-1)),
                Command::Tab(n) => push(Action::SelectNumbered(n)),
                // Last, or the ninth, as chosen in Settings → Shortcuts.
                Command::LastTab => push(Action::SelectNumbered(9)),
                Command::Space(n) => {
                    let spaces = self.lib.spaces_ordered();
                    if let Some((id, _)) = spaces.get(usize::from(n) - 1) {
                        push(Action::SwitchSpace(id.clone()));
                    }
                }
                // Escape runs the dismissal chain in handle_shortcuts.
                Command::CloseOverlay => {}
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let target = self.web_keyboard_target(ctx);
        let priority = self.shortcuts.priority();
        // Settings, the quit question and other overlays have no page target, so they end a sequence.
        self.first_press.follow(ctx, priority == crate::shortcuts::Priority::Twice, target.as_ref().map(page_mark));
        // A shortcut being recorded or confirmed in Settings, or the quit question, runs nothing.
        if self.ui.settings.as_ref().is_some_and(ui::settings::SettingsState::owns_keyboard) || self.quit.borrow().asking() {
            return;
        }
        // A capture being selected takes every key, Escape included.
        if self.capture.selecting.is_some() { return; }
        let editing_text = ctx.memory(|m| m.focused().is_some()) || self.web_editable();
        let mut out: Vec<Action> = Vec::new();
        // Presses a page has reported unhandled since the last frame run first.
        let mut commands = self.unhandled_shortcuts(ctx);
        let page_first = priority != crate::shortcuts::Priority::Browser && target.is_some();
        let routed = self.shortcuts.route(ctx, editing_text, page_first, &mut self.first_press);
        self.deferred_keys = routed.deferred;
        for ticket in routed.withdrawn {
            self.waiting_keys.withdraw(ticket);
        }
        commands.extend(routed.run);
        self.run_shortcuts(ctx, commands, &mut out);
        if take_dismiss_shortcut(ctx) {
            if self.ui.menu.is_some() {
                self.ui.menu = None;
            } else if self.capture.list.viewer.take().is_some() {
                // The capture viewer closes before the Library under it.
            } else if self.previews.dismiss() {
                // A shown hover preview closes before anything under it.
            } else if self.link_previews.escape() {
                // A shown 5-Second Preview closes; otherwise Escape goes on to the page.
            } else if self.ui.drag.take().is_some() {
                // Global shortcuts run before the sidebar sees this Escape.
            } else if cancel_command(ctx, &mut self.ui.command, &mut self.ui.focus, self.ui.find.as_mut()) {
                // Search owns cancellation before the underlying Find bar.
            } else if dismiss_rename(ctx, &mut self.ui.renaming, &mut self.ui.focus) {
                // Discard the draft before a lost-focus frame can commit it.
            } else if ui::logins::dismiss_focused_offer(ctx, &mut self.passwords) {
                // Not Now, from a password offer's username; the page gets the keyboard back.
                restore_web_focus(ctx, &mut self.ui.focus);
            } else if let Some(outcome) = ui::site_permissions::dismiss_focused_prompt(ctx, &mut self.site_permissions, &self.lib.store) {
                // Don't Allow, unremembered, from a focused permission prompt.
                ui::site_permissions::apply(self, outcome);
                restore_web_focus(ctx, &mut self.ui.focus);
            } else if let Some(tab) = ui::translate::dismiss_focused_card(ctx) {
                // Close, from a focused translation card.
                self.actions.push(Action::Translate(tab, crate::translate::Choice::Close));
                restore_web_focus(ctx, &mut self.ui.focus);
            } else if ui::settings::cancel_account_step(self.ui.settings.as_ref(), &mut self.sync) {
                // A browser sign-in in progress on Account ends before the sheet closes.
            } else if self.dismiss_settings(ctx) {
                // The visible sheet owns Escape before Library, Peek or Find.
            } else if self.dismiss_library(ctx, LibraryDismissal::RestoreFocus) {
                // The Library popover owns Escape before the page or a lower Find bar.
            } else if ui::logins::dismiss_shown_address_offer(ctx, ui::logins::focused_offer_tab(self, ctx.content_rect()).as_deref(), &mut self.passwords) {
                // Not Now, from the focused pane's address offer, whatever has focus; focus stays where it is.
            } else if let Some(peek) = &self.ui.peek {
                if peek.view.with_popup(|popup| popup.is_some()) {
                    send_web_escape(&peek.view);
                } else {
                    self.actions.push(Action::ClosePeek);
                }
            } else if leave_ask(&mut self.ask, self.ui.find.as_mut()) {
                // Find stays open with its query.
            } else if self.ui.find.is_some() {
                let view = self.focused_view();
                dismiss_find(ctx, &mut self.ui.find, &mut self.ui.focus, || {
                    if let Some(view) = view { view.dismiss_find(); }
                });
            } else if let Some(v) = self.focused_view() {
                // Let the page see Escape too (closing its own dialogs, leaving fullscreen).
                if v.nav().fullscreen {
                    v.exit_fullscreen();
                }
                send_web_escape(&v);
            }
        }
        self.actions.extend(out);
    }

    /// Overlays that keep keyboard input from every web page.
    pub(crate) fn web_overlay(&self) -> bool {
        self.ui.command.is_some() || self.ui.settings.is_some() || self.ui.onboarding.is_some() || self.ui.show_archive || self.quit.borrow().asking() || self.tidy_tabs.sheet.is_some() || self.capture.selecting.is_some()
    }

    /// The page that has the keyboard, unless Crossbow's own UI has it.
    fn web_keyboard_target(&self, ctx: &egui::Context) -> Option<WebView> {
        if web_keyboard_blocker(ctx, self.ui.focus, self.web_overlay()).is_some() { return None; }
        if let Some(p) = &self.ui.peek { Some(p.view.clone()) } else { self.focused_view() }
    }

    /// Actions for shortcut presses a page left unhandled. Only the page that
    /// still has the keyboard runs them: after a switch to another tab, Peek
    /// or Crossbow's own controls, a late report runs nothing.
    fn unhandled_shortcuts(&mut self, ctx: &egui::Context) -> Vec<crate::shortcuts::Command> {
        let mut run = Vec::new();
        if let Some(view) = self.web_keyboard_target(ctx) {
            run.extend(view.take_unhandled_keys().into_iter().filter_map(|ticket| {
                self.first_press.reported(ticket);
                self.waiting_keys.unhandled(ticket, true)
            }));
        }
        let views = self.tabs.iter().filter_map(|tab| tab.view.as_ref()).chain(self.ui.peek.as_ref().map(|peek| &peek.view));
        for ticket in views.flat_map(WebView::take_unhandled_keys) {
            self.first_press.reported(ticket);
            self.waiting_keys.unhandled(ticket, false);
        }
        run
    }

    /// Shortcut presses no page received this frame belong to the browser.
    fn run_unsent_shortcuts(&mut self, ctx: &egui::Context) {
        let commands = std::mem::take(&mut self.deferred_keys).into_iter().map(|press| press.command).collect();
        let mut out = Vec::new();
        self.run_shortcuts(ctx, commands, &mut out);
        self.actions.extend(out);
    }

    fn web_editable(&self) -> bool {
        self.navigation_owner().is_some_and(|owner| owner.editable_focus())
    }

    #[cfg(all(debug_assertions, target_os = "macos"))]
    fn trace_key_focus(&self, ctx: &egui::Context, phase: &str, events: &[egui::Event], window_focused: bool) {
        static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        if !focus_trace_enabled() { return; }
        let counts = keyboard_event_counts(events);
        if counts == [0; 4] || COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 2000 { return; }
        let (widget, modal) = ctx.memory(|memory| {
            let widget = match memory.focused() {
                None => "none",
                Some(id) if id == egui::Id::unique("cmd-field") => "search",
                Some(id) if id == egui::Id::unique("find-field") => "find",
                Some(id) if id == egui::Id::unique("rename-field") => "rename",
                Some(_) => "other",
            };
            (widget, memory.top_modal_layer().is_some())
        });
        let view = if let Some(peek) = &self.ui.peek { Some(peek.view.clone()) } else { self.focused_view() };
        log::info!("focus-diagnostic phase={phase} pass={} window={window_focused} owner={:?} widget={widget} modal={modal} search={} find={} settings={} rename={} peek={} view={} editable={} counts={counts:?}",
            ctx.cumulative_pass_nr(), self.ui.focus, self.ui.command.is_some(), self.ui.find.is_some(), self.ui.settings.is_some(), self.ui.renaming.is_some(), self.ui.peek.is_some(), view.is_some(), view.is_some_and(|view| view.nav().editable_focus));
    }

    /// Forward the remaining keyboard input to the focused web view.
    fn forward_keys(&mut self, ctx: &egui::Context) {
        let blocked = web_keyboard_blocker(ctx, self.ui.focus, self.web_overlay());
        let target = if let Some(p) = &self.ui.peek { Some(p.view.clone()) } else { self.focused_view() };
        #[cfg(all(debug_assertions, target_os = "macos"))]
        if focus_trace_enabled() {
            let (events, focused) = ctx.input(|input| (input.events.clone(), input.focused));
            self.trace_key_focus(ctx, blocked.unwrap_or(if target.is_some() { "dispatch" } else { "no-view" }), &events, focused);
        }
        if blocked.is_some() { return; }
        let Some(view) = target else { return };
        let events = ctx.input(|i| i.events.clone());
        let inputs = input::web_key_inputs(&events);
        if self.ui.peek.is_none() && !inputs.is_empty() {
            if let Some(tab) = self.focused_tab_id() { self.note_activity(&tab); }
        }
        let mut unsent = Vec::new();
        for inp in inputs {
            // A browser shortcut the page has first: its action waits for an unhandled report.
            if let WebInput::Key(k) = &inp && let Some(press) = crate::shortcuts::claim(&mut self.deferred_keys, k) {
                match view.key_tracked(k) {
                    Some(ticket) => {
                        self.waiting_keys.wait(ticket, press.command);
                        self.first_press.arm(&press, page_mark(&view), ticket);
                    }
                    None => unsent.push(press),
                }
                continue;
            }
            input::send(&view, &inp);
        }
        // With no browser to send them to, these presses stay with Crossbow.
        self.deferred_keys.extend(unsent);
        // Let the platform IME follow the page's caret while a field is focused.
        if view.nav().editable_focus {
            if let Some((_, rect)) = self.ui.panes.iter().find(|(id, _)| Some(id.clone()) == self.focused_tab_id()) {
                let r = Rect::from_min_size(rect.center(), egui::vec2(1.0, 18.0));
                ctx.output_mut(|o| o.ime = Some(egui::output::IMEOutput { purpose: Default::default(), rect: *rect, cursor_rect: r, should_interrupt_composition: false }));
            }
        }
    }

    fn periodic(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        self.backups.tick(now);
        if now - self.last_save > 2.0 {
            self.last_save = now;
            self.save_session();
        }
        if auto_archive_enabled(&self.lib) {
            if now - self.last_archive_check >= 60.0 {
                self.last_archive_check = now;
                let stale = archive_candidates(&self.lib, &self.tabs, &self.active_tab, &self.ui.panes, now_ms());
                let mut summary = CloseSummary::default();
                for id in stale {
                    if self.tab(&id).is_some_and(Tab::can_auto_archive) {
                        summary.record(self.close_tab(&id));
                    }
                }
                if summary.failed > 0 {
                    if let Some(text) = summary.message() {
                        self.ui.toasts.push(Toast { text: format!("Auto-archive: {text}"), at: now });
                    }
                }
            }
            ctx.request_repaint_after(archive_check_delay(now, self.last_archive_check));
        }
        if let Some(after) = self.link_prefs.archive_after.after_ms().filter(|_| !self.littles.is_empty()) {
            if now - self.last_little_archive_check >= 60.0 {
                self.last_little_archive_check = now;
                self.archive_idle_little_windows(after, now_ms());
            }
            ctx.request_repaint_after(archive_check_delay(now, self.last_little_archive_check));
        }
        self.ui.toasts.retain(|t| now - t.at < 3.0);
        if self.tabs.iter().any(|t| t.is_loading()) || !self.ui.toasts.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }

    fn save_session(&mut self) {
        let kv = crate::split_session::SessionLayout {
            space: &self.active_space,
            active_tabs: &self.active_tab,
            splits: &self.splits,
            focused_pane: self.ui.focused_pane.as_deref(),
            sidebar: self.ui.sidebar_visible,
            sidebar_width: self.ui.sidebar_width,
        }.encode(&self.lib, &self.tabs);
        save_session_checkpoint(&self.lib.store, &mut self.tabs, &kv, &mut self.last_ui_kv);
    }

    // -- actions ---------------------------------------------------------------

    fn apply_actions(&mut self, ctx: &egui::Context, mut navigation: NavigationBatch) {
        let now = ctx.input(|i| i.time);
        let actions = std::mem::take(&mut self.actions);
        for a in actions {
            // A drop runs its move, then taps if the sidebar changed.
            let (a, dropped) = match a {
                Action::Drop(action) => (*action, Some(self.sidebar_snapshot())),
                a => (a, None),
            };
            navigation.observe(&a);
            match a {
                Action::OpenCommandBar { edit_current } => self.open_command_bar(ctx, edit_current),
                Action::NewTab { url, background } => {
                    self.open_new_tab(&url, background);
                }
                Action::SearchSelection { tab, profile, text } => {
                    let engine = self.tab(&tab).and_then(|tab| ui::overlays::selection_search_engine(&self.lib, tab, &self.active_space, &profile));
                    if let Some(engine) = engine {
                        self.open_new_tab(&engine.search_url(&text), false);
                    }
                }
                Action::CopyAsQuote { text, title, page_url } => match self.quote.copy(&self.lib.store, &text, &title, &page_url) {
                    Some(Ok(())) => {
                        self.ui.toasts.push(Toast { text: crate::quote::COPIED.into(), at: now });
                        self.cue(Cue::LinkCopied);
                    }
                    Some(Err(text)) => self.ui.toasts.push(Toast { text, at: now }),
                    None => {}
                },
                Action::OpenLinkTab { url, background, here } => {
                    if here || !self.open_site_tab(&url, background) { self.open_new_tab(&url, background); }
                }
                Action::UndoSiteOpen => self.undo_site_open(now),
                Action::AlwaysOpenSite { host, profile } => {
                    self.site_prefill = Some((profile, host));
                    self.ui.settings_page_hint = Some(ui::settings::Page::Spaces);
                    self.actions.push(Action::OpenSettings);
                }
                Action::StopSiteProfile(host) => self.stop_site_profile(&host, now),
                Action::OpenCommandUrl { url, origin, new_tab, route } => {
                    // Not New Tab Inside a folder: that folder is in this Space.
                    if route && origin.pin_folder().is_none() && self.open_site_tab(&url, false) { continue; }
                    let focused = self.focused_tab_id();
                    let tab = focused.as_deref().and_then(|id| self.tab(id));
                    let target = command_url_target(&self.lib, &origin, &self.active_space, tab, tab.and_then(|tab| tab.view.as_ref().map(WebView::profile_id)), new_tab);
                    match target {
                        Some(CommandUrlTarget::Tab(t)) => {
                            let view = self.tab(&t).and_then(|tab| tab.view.clone());
                            if let Some(tab) = self.tab_mut(&t) {
                                tab.url = url.clone();
                                tab.dirty = true;
                            }
                            self.errors.remove(&t);
                            match view {
                                Some(view) => {
                                    view.load_url(&url);
                                    view.set_focus(true);
                                    self.ui.focus = Focus::Web;
                                }
                                None => self.activate(&t),
                            }
                        }
                        Some(CommandUrlTarget::NewTab) => {
                            let tab = self.open_new_tab(&url, false);
                            if let Some(folder) = origin.pin_folder() { self.pin_new_tab(&tab, folder, now); }
                        }
                        None => self.ui.toasts.push(Toast { text: "Search target changed or is unavailable. Reopen search to try again.".into(), at: now }),
                    }
                }
                Action::OpenInPeek(url) => {
                    let rs = self.render_state.clone();
                    let size = ctx.content_rect().size() * 0.8;
                    if let Some(view) = WebView::new(WebViewConfig {
                        url,
                        profile_id: self.current_profile(),
                        data_dir: self.data_dir.clone(),
                        size: (size.x, size.y),
                        scale: ctx.pixels_per_point(),
                        screen: self.screen_geometry,
                        device: rs.device.clone(),
                        queue: rs.queue.clone(),
                        // Peek never offers to save, so its pages never capture.
                        saves_logins: false,
                    }) {
                        if let Some(old) = self.ui.peek.take() {
                            retire_peek_textures(&self.texture_retirement, old.texture, old.popup_texture);
                            old.view.close();
                        }
                        view.set_focus(true);
                        self.ui.peek = Some(Peek { view, opened_at: now, closing_at: None, texture: None, popup_texture: None });
                    }
                }
                Action::ClosePeek => {
                    if let Some(p) = &mut self.ui.peek {
                        if p.closing_at.is_none() {
                            p.closing_at = Some(now);
                        }
                    }
                }
                Action::PeekToTab => {
                    let peek = take_peek_for_tab(&self.lib, &self.active_space, &mut self.ui.peek, |peek| peek.closing_at.is_none().then(|| peek.view.profile_id()));
                    match peek {
                        Ok(Some(p)) => {
                            let url = p.view.nav().url;
                            let space = self.active_space.clone();
                            let id = self.new_tab_record(&space, &url, None, None);
                            if let Some(t) = self.tab_mut(&id) {
                                t.title = p.view.nav().title;
                                t.view = Some(p.view.clone());
                            }
                            retire_peek_textures(&self.texture_retirement, p.texture, p.popup_texture);
                            self.activate(&id);
                        }
                        Ok(None) => {}
                        Err(error) => self.ui.toasts.push(Toast { text: error.into(), at: now }),
                    }
                }
                Action::ActivateTab(t) => self.activate(&t),
                Action::CloseTab(t) => match self.close_tab(&t) {
                    Ok(Some(_)) => self.cue(Cue::TabClosed),
                    Ok(None) => {}
                    Err(error) => {
                        log::warn!("Could not close a tab; it remains open for retry: {error}");
                        self.ui.toasts.push(Toast { text: "Could not close tab. It is still open. Try again.".into(), at: now });
                    }
                },
                Action::OpenItem(i) => self.open_item(&i),
                Action::PinTab(t) => self.pin_tab(&t, None, None),
                Action::UnpinTab(t) => {
                    if let Some(item) = self.tab(&t).and_then(|x| x.item_id.clone()) {
                        self.unpin(&item, Some(0));
                    }
                }
                Action::FavoriteTab(t) => {
                    if let Some(tab) = self.tab(&t) {
                        if tab.item_id.as_ref().and_then(|i| self.lib.items.get(i)).is_some_and(|i| i.kind == ItemKind::Favorite) {
                            continue;
                        }
                        let (url, title) = (tab.url.clone(), tab.display_title());
                        let old_item = tab.item_id.clone();
                        if let Ok(item) = self.lib.add_favorite(&self.current_profile(), &url, &title) {
                            if let Some(old) = old_item {
                                let _ = self.lib.delete_item(&old);
                            }
                            if let Some(tab) = self.tab_mut(&t) {
                                tab.item_id = Some(item);
                                tab.space_id = FAVORITES_SPACE.into();
                                tab.dirty = true;
                            }
                        }
                    }
                }
                Action::DeleteItem(i) => {
                    if let Err(error) = self.remove_parent(ParentRemoval::Item(&i)) {
                        log::warn!("Could not remove an item: {error}");
                        self.ui.toasts.push(Toast { text: "Could not remove item. Nothing was changed. Try again.".into(), at: now });
                    }
                }
                Action::MoveTab { tab, index } => {
                    let space = self.active_space.clone();
                    let others: Vec<String> = self.today_tabs(&space).iter().filter(|t| t.id != tab).map(|t| t.order.clone()).collect();
                    let i = index.min(others.len());
                    let order = key_between(i.checked_sub(1).map(|j| others[j].as_str()), others.get(i).map(String::as_str));
                    if let Some(t) = self.tab_mut(&tab) {
                        t.order = order;
                        t.dirty = true;
                    }
                }
                Action::MovePinned { node, folder, index } => {
                    let space = self.active_space.clone();
                    if let Err(error) = self.lib.move_pinned(&node, &space, folder.as_deref(), index) {
                        self.ui.toasts.push(Toast { text: format!("Could not move this item: {error}"), at: now });
                    }
                }
                Action::TabToPinned { tab, folder, index } => self.pin_tab(&tab, folder.as_deref(), Some(index)),
                Action::ItemToToday { item, index } => {
                    if self.tab_for_item(&item).is_none() {
                        self.open_item(&item);
                    }
                    self.unpin(&item, Some(index));
                }
                Action::ReorderFavorite { item, index } => {
                    if let Err(error) = self.lib.reorder_favorite(&item, index) {
                        self.ui.toasts.push(Toast { text: format!("Could not save favorite order: {error}"), at: now });
                    }
                }
                Action::SwitchSpace(s) => self.switch_space_by_hand(&s),
                Action::SwitchSpaceBy(d) => {
                    let order: Vec<String> = self.lib.spaces_ordered().into_iter().map(|(id, _)| id).collect();
                    if let Some(i) = order.iter().position(|s| *s == self.active_space) {
                        let j = (i as i32 + d).rem_euclid(order.len() as i32) as usize;
                        let target = order[j].clone();
                        self.switch_space_by_hand(&target);
                    }
                }
                Action::NewSpace => {
                    let n = self.lib.spaces.len();
                    let profile = profile_switch::new_space_profile(&self.lib, self.ui.new_space_profile.as_deref(), &self.active_space);
                    let icons = ["🏠", "💼", "🎨", "📚", "🎧", "🧪", "🌿", "🚀"];
                    if let Ok(id) = self.lib.create_space(&format!("Space {}", n + 1), icons[n % icons.len()], Theme::preset(n), &profile) {
                        self.switch_space(&id);
                        self.ui.renaming = Some((RenameTarget::Space(id.clone()), format!("Space {}", n + 1)));
                    }
                }
                Action::RenameSpace(id, name) => {
                    let _ = self.lib.update_space(&id, |s| s.name = name);
                }
                Action::SetSpaceTheme(id, theme) => {
                    let _ = self.lib.update_space(&id, |s| s.theme = theme);
                }
                Action::SetSpaceIcon(id, icon) => {
                    if let Err(error) = self.lib.update_space(&id, |s| s.icon = icon) {
                        log::warn!("Could not save a space icon: {error}");
                        self.ui.toasts.push(Toast { text: "Could not save the icon. Nothing was changed. Try again.".into(), at: now });
                    }
                }
                Action::DeleteSpace(id) => {
                    if self.lib.spaces.len() > 1 {
                        if let Err(error) = self.remove_parent(ParentRemoval::Space(&id)) {
                            log::warn!("Could not remove a space: {error}");
                            self.ui.toasts.push(Toast { text: "Could not remove space. Nothing was changed. Try again.".into(), at: now });
                        }
                    }
                }
                Action::MoveSpaceToProfile { space, profile } => {
                    let result = self.move_space_to_profile(&space, &profile);
                    match self.ui.settings.as_mut() {
                        Some(settings) => settings.space_profile_moved(&space, &profile, result),
                        None => if let Err(text) = result { self.ui.toasts.push(Toast { text, at: now }); },
                    }
                }
                Action::DeleteProfile(profile) => {
                    let result = self.delete_profile(&profile);
                    match self.ui.settings.as_mut() {
                        Some(settings) => settings.profile_deleted(&profile, result),
                        None => if let Err(text) = result { self.ui.toasts.push(Toast { text, at: now }); },
                    }
                }
                Action::NewFolder => {
                    let space = self.active_space.clone();
                    if let Ok(id) = self.lib.create_folder(&space, None, "New Folder") {
                        self.ui.renaming = Some((RenameTarget::Folder(id), "New Folder".into()));
                    }
                }
                Action::RenameFolder(id, name) => {
                    let _ = self.lib.update_folder(&id, |f| f.name = name);
                }
                Action::ToggleFolder(id) => {
                    let _ = self.lib.update_folder(&id, |f| f.collapsed = !f.collapsed);
                }
                Action::DeleteFolder(id) => {
                    if let Err(error) = self.lib.delete_folder(&id) {
                        log::warn!("Could not remove a folder: {error}");
                        self.ui.toasts.push(Toast { text: "Could not remove folder. Nothing was changed. Try again.".into(), at: now });
                    }
                }
                Action::RenameItem(id, name) => {
                    let _ = self.lib.update_item(&id, |i| i.custom_title = if name.trim().is_empty() { None } else { Some(name) });
                }
                Action::Back | Action::Forward | Action::Reload | Action::HardReload | Action::Stop => {
                    if let Some(owner) = navigation.owner(self.ui.peek.as_ref().map(|peek| peek.closing_at.is_some()), || self.navigation_owner()) {
                        owner.apply(&a, &mut self.errors);
                    }
                }
                Action::ToggleSidebar => self.ui.sidebar_visible = !self.ui.sidebar_visible,
                Action::CopyUrl => {
                    if let Some(url) = self.focused_tab_id().and_then(|t| self.tab(&t).map(|t| t.url.clone())) {
                        ctx.copy_text(url);
                        self.ui.toasts.push(Toast { text: "Copied URL".into(), at: now });
                        self.cue(Cue::LinkCopied);
                    }
                }
                Action::ClearToday => {
                    let space = self.active_space.clone();
                    let ids: Vec<String> = self.today_tabs(&space).iter().map(|t| t.id.clone()).collect();
                    if let Some(text) = self.close_tabs(&ids).message() {
                        self.ui.toasts.push(Toast { text, at: now });
                    }
                }
                Action::RestoreArchived(rowid) => self.restore_archived(rowid, now),
                Action::NewLittleWindow => self.new_little_window(ctx),
                Action::OpenInLittleWindow { tab, url } => if !self.open_link_in_little_window(ctx, &tab, &url) {
                    // No Space can take it: as ⌘-click.
                    self.open_new_tab(&url, true);
                },
                Action::ShowLittleWindows => self.show_all_little_windows(ctx, now),
                Action::CloseLittleWindows => self.close_all_little_windows(now),
                Action::NewDocument => match crate::new_document::resolve(&self.lib, &self.active_space) {
                    crate::new_document::NewDocument::Open { url, .. } => { self.open_new_tab(&url, false); }
                    crate::new_document::NewDocument::Choose => self.actions.push(Action::ChooseDocumentApp),
                },
                Action::ChooseDocumentApp => {
                    let mut settings = ui::settings::SettingsState::new(ui::settings::Page::General, now);
                    settings.reveal_document_app();
                    self.ui.open_settings(settings);
                }
                Action::ReopenClosed => match archive_for_space(&self.lib, &self.active_space, 1) {
                    Ok(rows) => {
                        if let Some(archived) = rows.first() { self.restore_archived(archived.rowid, now); }
                    }
                    Err(error) => {
                        log::warn!("Could not read the Archive: {error}");
                        self.ui.toasts.push(Toast { text: "Could not read the Archive. Try again.".into(), at: now });
                    }
                },
                Action::SplitWith(t) => {
                    let space = self.active_space.clone();
                    if let Some(split) = self.splits.get_mut(&space) {
                        if split.tabs.contains(&t) {
                            continue;
                        }
                        split.add(t.clone());
                    } else {
                        // Pair the tab with the previously used tab of the space, or a new one.
                        let other = self
                            .tabs
                            .iter()
                            .filter(|x| x.space_id == space && x.id != t)
                            .max_by_key(|x| x.last_active_ms)
                            .map(|x| x.id.clone())
                            .unwrap_or_else(|| self.new_tab_record(&space, NEW_TAB_URL, None, None));
                        self.splits.insert(space.clone(), Split::new(t.clone(), other));
                    }
                    self.activate(&t);
                }
                Action::CloseSplit => {
                    self.splits.remove(&self.active_space);
                }
                Action::Zoom(d) => {
                    if let Some(v) = self.focused_view() {
                        let level = if d == 0 { 0.0 } else { v.zoom_level() + d as f64 * 0.5 };
                        v.set_zoom_level(level);
                        let pct = (1.2f64.powf(level) * 100.0).round();
                        self.ui.toasts.push(Toast { text: format!("Zoom {pct}%"), at: now });
                    }
                }
                Action::DevTools => {
                    if let Some(v) = self.focused_view() {
                        v.show_devtools();
                    }
                }
                Action::ViewSource => {
                    if let Some(url) = self.focused_tab_id().and_then(|t| self.tab(&t).map(|t| t.url.clone())) {
                        self.open_new_tab(&format!("view-source:{url}"), false);
                    }
                }
                Action::Print => {
                    if let Some(v) = self.focused_view() {
                        v.print();
                    }
                }
                Action::OpenFind => {
                    self.ui.find = Some(FindBar { query: String::new(), last_sent: String::new(), just_opened: true });
                    // Find always opens in Find mode.
                    self.ask = None;
                    self.ui.focus = Focus::Chrome;
                }
                Action::OpenSettings => {
                    let page = self.ui.settings_page_hint.take().unwrap_or_default();
                    self.ui.open_settings(ui::settings::SettingsState::new(page, now));
                }
                Action::OpenChat(entry) => {
                    let account = self.chat_account_scope();
                    match ui::settings::SettingsState::from_chat_entry(&entry, &mut self.lib, &self.active_space, &account, now) {
                        Ok(settings) => self.ui.open_settings(settings),
                        Err(text) => self.ui.toasts.push(Toast { text: text.into(), at: now }),
                    }
                }
                Action::AskChat(entry, question) => {
                    let account = self.chat_account_scope();
                    match ui::settings::SettingsState::from_chat_question(&entry, question, &mut self.lib, &self.active_space, &account, ctx, now) {
                        Ok(settings) => self.ui.open_settings(settings),
                        Err(text) => self.ui.toasts.push(Toast { text: text.into(), at: now }),
                    }
                }
                Action::ToggleMute(t) => {
                    if let Some(tab) = self.tab_mut(&t) {
                        tab.muted = !tab.muted;
                        if let Some(v) = &tab.view {
                            v.set_muted(tab.muted);
                        }
                    }
                }
                Action::PictureInPicture(tab) => self.pip.request = Some(tab),
                Action::Translate(tab, choice) => self.translate(&tab, choice, ctx),
                Action::Capture(tab) => self.start_capture(tab),
                Action::ShowCaptures => {
                    self.capture.list.show = true;
                    self.ui.show_archive = true;
                }
                Action::Toast(text) => self.ui.toasts.push(Toast { text, at: now }),
                Action::DownloadControl { profile, id, command } => {
                    let current = crate::downloads::active_profile(&self.lib, &self.active_space);
                    if let Err(text) = crate::downloads::control(&self.downloads, current, &profile, id, command) {
                        self.ui.toasts.push(Toast { text, at: now });
                    }
                    ctx.request_repaint();
                }
                Action::TidyDownload { profile, id, command } => {
                    // Only the visible profile's rows offer these buttons.
                    if crate::downloads::active_profile(&self.lib, &self.active_space) == Some(profile.as_str()) {
                        self.tidy.command(&profile, id, command, &self.lib, &crate::ai_features::Local, ctx);
                    }
                    ctx.request_repaint();
                }
                Action::SelectNumbered(n) => {
                    let rows = crate::numbered_tabs::rows(&self.lib, &self.tabs, &self.active_space);
                    match self.numbered.target(&self.lib.store, &rows, n) {
                        Some(crate::numbered_tabs::Row::Item(id)) => self.open_item(&id),
                        Some(crate::numbered_tabs::Row::Tab(id)) => self.activate(&id),
                        None => {}
                    }
                }
                Action::NextTab(d) => {
                    let order = self.sidebar_order();
                    if !order.is_empty() {
                        let cur = self.active_tab_id().and_then(|a| order.iter().position(|x| *x == a)).unwrap_or(0);
                        let j = (cur as i32 + d).rem_euclid(order.len() as i32) as usize;
                        let id = order[j].clone();
                        self.activate(&id);
                    }
                }
                Action::StartOnboarding => {
                    self.ui.onboarding = Some(ui::onboarding::Onboarding::new(ctx));
                }
                Action::OpenInSplit(url) => {
                    let Some(cur) = self.focused_tab_id() else {
                        self.open_new_tab(&url, false);
                        continue;
                    };
                    let id = self.open_new_tab(&url, true);
                    let space = self.active_space.clone();
                    match self.splits.get_mut(&space) {
                        Some(s) if s.tabs.contains(&cur) => s.add(id.clone()),
                        _ => {
                            self.splits.insert(space, Split::new(cur, id.clone()));
                        }
                    }
                    self.activate(&id);
                }
                Action::Duplicate(t) => {
                    if let Some(url) = self.tab(&t).map(|x| x.url.clone()) {
                        let order = self.tab(&t).filter(|x| x.item_id.is_none()).map(|x| x.order.clone());
                        let space = self.active_space.clone();
                        let id = self.new_tab_record(&space, &url, None, order.as_deref());
                        self.activate(&id);
                    }
                }
                Action::MoveTabToSpace { tab, space } => {
                    let Some(source) = self.tab(&tab) else { continue };
                    if source.space_id == space { continue; }
                    if !can_move_tab(&self.lib, source, &space) {
                        self.ui.toasts.push(Toast { text: "Tabs cannot move between browser profiles.".into(), at: now });
                        continue;
                    }
                    let last = self.today_tabs(&space).last().map(|t| t.order.clone());
                    let was_active = self.active_tab_id().as_deref() == Some(tab.as_str());
                    if let Some(item) = self.tab(&tab).and_then(|t| t.item_id.clone()) {
                        if self.lib.delete_item(&item).is_err() {
                            self.ui.toasts.push(Toast { text: "Could not save this move. Try again.".into(), at: now });
                            continue;
                        }
                    }
                    if let Some(t) = self.tab_mut(&tab) {
                        t.item_id = None;
                        t.space_id = space.clone();
                        t.order = key_between(last.as_deref(), None);
                        t.dirty = true;
                    }
                    for split in self.splits.values_mut() {
                        split.remove(&tab);
                    }
                    self.splits.retain(|_, s| s.tabs.len() >= 2);
                    if was_active {
                        self.active_tab.remove(&self.active_space);
                    }
                    self.active_tab.insert(space.clone(), tab.clone());
                    self.ui.toasts.push(Toast { text: format!("Moved to {}", self.lib.spaces.get(&space).map(|s| s.name.as_str()).unwrap_or("space")), at: now });
                }
                Action::Folder(op) => self.apply_folder_op(ctx, op, now),
                Action::TidyTitle(item) => self.tidy_tabs.tidy_title(&self.lib, &item, &crate::tidy_tabs::TITLE_PROVIDER, ctx),
                Action::RestorePageTitle(item) => {
                    if self.lib.update_item(&item, |item| item.custom_title = None).is_err() {
                        self.ui.toasts.push(Toast { text: "Could not restore the page title. Try again.".into(), at: now });
                    }
                }
                Action::TidyToday(space) => self.tidy_tabs.tidy_today(&self.lib, &self.tabs, &space, &crate::tidy_tabs::TODAY_PROVIDER, ctx),
                Action::UndoTidyToday => self.tidy_tabs.undo(&mut self.lib, &mut self.tabs),
                Action::ResetItem(item) => {
                    let url = self.lib.items.get(&item).map(|i| i.url.clone());
                    if let (Some(url), Some(t)) = (url, self.tab_for_item(&item).map(|t| t.id.clone())) {
                        if let Some(v) = self.tab(&t).and_then(|x| x.view.clone()) {
                            v.load_url(&url);
                        }
                        if let Some(tab) = self.tab_mut(&t) {
                            tab.url = url;
                            tab.dirty = true;
                        }
                        self.activate(&t);
                    }
                }
                Action::Drop(_) => {}
            }
            if let Some(before) = dropped {
                let after = self.sidebar_snapshot();
                self.feel.dropped(&before, &after);
            }
        }
        complete_find_handoff(ctx, self.ui.focus, self.ui.find.as_mut());
    }

    /// Items currently pinned in the active space, used by the command bar.
    pub fn all_items(&self) -> Vec<(String, Item)> {
        self.lib.items.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
}

#[derive(Default)]
struct RemovalSelection {
    space: String,
    profile: String,
    active: HashMap<String, String>,
    focused: Option<String>,
    panes: Vec<String>,
    splits: HashMap<String, Split>,
    order: Vec<String>,
}

impl RemovalSelection {
    fn reconcile(&mut self, lib: &Library, tabs: &[Tab]) {
        let previous_space = self.space.clone();
        if !lib.spaces.contains_key(&self.space) {
            let spaces = lib.spaces_ordered();
            if let Some((id, _)) = spaces.iter().find(|(_, space)| space.profile_id == self.profile).or_else(|| spaces.first()) {
                self.space = id.clone();
            }
        }
        let valid = |space: &str, id: &str| {
            let Some(owner) = lib.spaces.get(space) else { return false };
            tabs.iter().any(|tab| tab.id == id && (tab.space_id == space || tab.space_id == FAVORITES_SPACE) && tab_profile(lib, tab) == Some(owner.profile_id.as_str()))
        };
        let fallback = |space: &str| {
            tabs.iter().filter(|tab| valid(space, &tab.id))
                .min_by_key(|tab| (tab.space_id != space, tab.item_id.is_some(), &tab.order, tab.created_ms, &tab.id))
                .map(|tab| tab.id.clone())
        };
        self.active.retain(|space, active| {
            if !lib.spaces.contains_key(space) { return false; }
            if valid(space, active) { return true; }
            // Keep a displayed split visible before considering an unrelated sidebar neighbor.
            let split = self.splits.get(space).filter(|split| split.tabs.contains(active));
            let next = split.and_then(|split| split.tabs.iter().find(|id| valid(space, id))).cloned()
                .or_else(|| (space == &previous_space).then(|| self.panes.iter().find(|id| valid(space, id)).cloned()).flatten())
                .or_else(|| (space == &previous_space).then(|| surviving_neighbor(&self.order, active, tabs).filter(|id| valid(space, id))).flatten())
                .or_else(|| fallback(space));
            if let Some(next) = next { *active = next; true } else { false }
        });
        if !self.active.contains_key(&self.space) {
            if let Some(id) = fallback(&self.space) { self.active.insert(self.space.clone(), id); }
        }
        if self.space != previous_space || !self.focused.as_ref().is_some_and(|id| valid(&self.space, id) && self.panes.contains(id)) {
            self.focused = self.active.get(&self.space).cloned();
        }
    }
}

#[derive(Clone, Copy)]
enum ParentRemoval<'a> {
    Item(&'a str),
    Space(&'a str),
}

impl ParentRemoval<'_> {
    fn contains(self, lib: &Library, tab: &Tab) -> bool {
        match self {
            Self::Item(id) => tab.item_id.as_deref() == Some(id),
            Self::Space(id) => tab.space_id == id || tab.item_id.as_ref().and_then(|item| lib.items.get(item)).is_some_and(|item| item.space_id.as_deref() == Some(id)),
        }
    }
}

fn take_removed_parent_tabs(lib: &mut Library, tabs: &mut Vec<Tab>, parent: ParentRemoval<'_>, at: u64) -> crossbow_core::library::Result<Vec<Tab>> {
    let rows: Vec<_> = tabs.iter().filter(|tab| parent.contains(lib, tab)).map(|tab| {
        let mut row = tab.row();
        row.title = tab.display_title();
        row
    }).collect();
    match parent {
        ParentRemoval::Item(id) => lib.delete_item_and_tabs(id, &rows, at)?,
        ParentRemoval::Space(id) => lib.delete_space_and_tabs(id, &rows, at)?,
    }
    // A failed transaction must leave the live views, dirty state and selection intact.
    let ids: HashSet<_> = rows.iter().map(|row| row.id.as_str()).collect();
    Ok(tabs.extract_if(.., |tab| ids.contains(tab.id.as_str())).collect())
}

fn surviving_neighbor(order: &[String], active: &str, tabs: &[Tab]) -> Option<String> {
    let index = order.iter().position(|id| id == active)?;
    order[index + 1..].iter().chain(order[..index].iter().rev()).find(|id| tabs.iter().any(|tab| &tab.id == *id)).cloned()
}

fn should_archive_tab(tab: &Tab) -> bool {
    tab.item_id.is_none() && !tab.url.is_empty() && tab.url != NEW_TAB_URL
}

fn take_closed_tab(lib: &Library, tabs: &mut Vec<Tab>, id: &str) -> crossbow_core::store::Result<Option<Tab>> {
    let Some(index) = tabs.iter().position(|tab| tab.id == id) else { return Ok(None) };
    let tab = &tabs[index];
    let mut row = tab.row();
    row.title = tab.display_title();
    // An unresolved owner must fail the archive write, not skip it and close the page.
    let profile = should_archive_tab(tab).then(|| tab_profile(lib, tab).unwrap_or(""));
    lib.store.close_tab(&row, profile, now_ms())?;
    Ok(Some(tabs.remove(index)))
}

fn archive_profile<'a>(lib: &'a Library, active_space: &str) -> Option<&'a str> {
    let profile = &lib.spaces.get(active_space)?.profile_id;
    lib.profiles.contains_key(profile).then_some(profile.as_str())
}

pub(crate) fn archive_for_space(lib: &Library, active_space: &str, limit: usize) -> crossbow_core::store::Result<Vec<crossbow_core::store::ArchivedTab>> {
    let Some(profile) = archive_profile(lib, active_space) else { return Ok(Vec::new()) };
    if !lib.store.archive_space_matches(profile, active_space)? { return Ok(Vec::new()); }
    lib.store.archive(profile, limit)
}

fn restore_archived_tab(lib: &Library, tabs: &mut Vec<Tab>, active_space: &str, rowid: i64) -> crossbow_core::store::Result<Option<String>> {
    let Some(profile) = archive_profile(lib, active_space) else { return Ok(None) };
    let Some(archived) = lib.store.archived_tab(profile, rowid)? else { return Ok(None) };
    let space = match lib.spaces.get(&archived.space_id) {
        Some(space) if space.profile_id == profile => archived.space_id.as_str(),
        Some(_) => return Ok(None),
        None => active_space,
    };
    let first = tabs.iter().filter(|tab| tab.space_id == space && tab.item_id.is_none()).map(|tab| tab.order.as_str()).min();
    let order = key_between(None, first);
    let id = new_id();
    let Some(row) = lib.store.restore_archived(profile, rowid, &id, active_space, &order, now_ms())? else { return Ok(None) };
    tabs.push(Tab::from_row(row));
    Ok(Some(id))
}

#[derive(Default, Debug, PartialEq, Eq)]
struct CloseSummary {
    archived: usize,
    closed: usize,
    failed: usize,
}

impl CloseSummary {
    fn record(&mut self, result: crossbow_core::store::Result<Option<bool>>) {
        match result {
            Ok(Some(true)) => self.archived += 1,
            Ok(Some(false)) => self.closed += 1,
            Ok(None) => {}
            Err(error) => {
                self.failed += 1;
                log::warn!("Could not close a tab; it remains open for retry: {error}");
            }
        }
    }

    fn message(&self) -> Option<String> {
        let mut parts = Vec::new();
        for (verb, count) in [("Archived", self.archived), ("Closed", self.closed)] {
            if count > 0 { parts.push(format!("{verb} {count} tab{}.", if count == 1 { "" } else { "s" })); }
        }
        if self.failed > 0 {
            parts.push(format!("{} tab{} could not be closed and {} still open. Try again.", self.failed, if self.failed == 1 { "" } else { "s" }, if self.failed == 1 { "is" } else { "are" }));
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }
}

/// The egui texture showing `tex`: `entry` itself, rebound when the engine
/// replaced its texture, or a new registration.
fn native_texture(rs: &egui_wgpu::RenderState, entry: Option<(egui::TextureId, u64)>, tex: &crossbow_engine::WebTexture) -> egui::TextureId {
    match entry {
        Some((id, generation)) if generation == tex.generation => id,
        Some((id, _)) => {
            rs.renderer.write().update_egui_texture_from_wgpu_texture(&rs.device, &tex.view, wgpu::FilterMode::Linear, id);
            id
        }
        None => rs.renderer.write().register_native_texture(&rs.device, &tex.view, wgpu::FilterMode::Linear),
    }
}

fn flush_tabs(store: &Store, tabs: &mut [Tab]) {
    for tab in tabs.iter_mut().filter(|tab| tab.dirty) {
        match store.put_tab(&tab.row()) {
            Ok(()) => tab.dirty = false,
            Err(error) => log::warn!("Could not save a tab; will retry: {error}"),
        }
    }
}

fn save_session_checkpoint(store: &Store, tabs: &mut [Tab], state: &str, saved: &mut String) {
    flush_tabs(store, tabs);
    // Do not advance the layout to tabs whose latest records failed to persist.
    // Both dirty records and failed layout writes remain retryable on the next save.
    if !tabs.iter().any(|tab| tab.dirty) { save_ui_state(store, state, saved); }
}

fn save_ui_state(store: &Store, state: &str, saved: &mut String) {
    if state != saved {
        match store.kv_set("ui_state", state) {
            Ok(()) => *saved = state.to_owned(),
            Err(error) => log::warn!("Could not save window state; will retry: {error}"),
        }
    }
}

fn restore_tab_layout(lib: &Library, tabs: &[Tab], state: &serde_json::Value) -> (HashMap<String, String>, HashMap<String, Split>) {
    let mut by_space: HashMap<String, (u64, String)> = HashMap::new();
    for tab in tabs {
        let entry = by_space.entry(tab.space_id.clone()).or_insert((0, String::new()));
        if tab.last_active_ms >= entry.0 { *entry = (tab.last_active_ms, tab.id.clone()); }
    }
    let mut active = by_space.into_iter().map(|(space, (_, id))| (space, id)).collect();
    restore_active_tabs(lib, tabs, state, &mut active);
    let splits = crate::split_session::restore(lib, tabs, state, &mut active);
    (active, splits)
}

fn restore_active_tabs(lib: &Library, tabs: &[Tab], state: &serde_json::Value, active: &mut HashMap<String, String>) {
    let valid = |space: &str, id: &str| {
        let Some(owner) = lib.spaces.get(space) else { return false };
        tabs.iter().any(|tab| tab.id == id && (tab.space_id == space || tab.space_id == FAVORITES_SPACE) && tab_profile(lib, tab) == Some(owner.profile_id.as_str()))
    };
    active.retain(|space, id| valid(space, id));
    if let Some(saved) = state["active_tabs"].as_object() {
        for (space, id) in saved {
            if let Some(id) = id.as_str().filter(|id| valid(space, id)) { active.insert(space.clone(), id.to_owned()); }
        }
    }
}

fn archive_check_delay(now: f64, last_check: f64) -> std::time::Duration {
    std::time::Duration::from_secs_f64((60.0 - (now - last_check)).clamp(0.001, 60.0))
}

fn auto_archive_age_ms(hours: u32) -> u64 {
    #[cfg(debug_assertions)]
    {
        static QA_AGE: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
        if let Some(age) = *QA_AGE.get_or_init(|| {
            let isolated = std::env::var_os("CROSSBOW_DATA_DIR").is_some_and(|dir| !dir.is_empty());
            let age = qa_archive_age_ms(std::env::var("CROSSBOW_QA_ARCHIVE_AFTER_SECS").ok().as_deref(), isolated);
            if age.is_some() { log::warn!("QA auto-archive age override enabled; the check interval remains 60 seconds"); }
            age
        }) { return age; }
    }
    u64::from(hours) * 3_600_000
}

#[cfg(any(debug_assertions, test))]
fn qa_archive_age_ms(value: Option<&str>, isolated: bool) -> Option<u64> {
    if !isolated { return None; }
    value?.parse::<u64>().ok().filter(|seconds| (1..=3600).contains(seconds)).map(|seconds| seconds * 1000)
}

fn auto_archive_enabled(lib: &Library) -> bool {
    lib.profiles.keys().any(|profile| lib.effective_profile_preferences(profile).is_some_and(|preferences| preferences.auto_archive_hours > 0))
}

fn archive_candidates(lib: &Library, tabs: &[Tab], active: &HashMap<String, String>, panes: &[(String, Rect)], now: u64) -> Vec<String> {
    let protected: HashSet<&str> = active.values().map(String::as_str).chain(panes.iter().map(|(id, _)| id.as_str())).collect();
    tabs.iter().filter(|tab| tab.item_id.is_none() && tab.space_id != FAVORITES_SPACE && !protected.contains(tab.id.as_str()) && tab.can_auto_archive()).filter_map(|tab| {
        let preferences = lib.effective_profile_preferences(tab_profile(lib, tab)?)?;
        let hours = preferences.auto_archive_hours;
        (hours > 0 && tab.last_active_ms < now.saturating_sub(auto_archive_age_ms(hours))).then(|| tab.id.clone())
    }).collect()
}

pub(crate) fn web_keyboard_blocker(ctx: &egui::Context, focus: Focus, overlay: bool) -> Option<&'static str> {
    if overlay { Some("overlay") }
    else if focus != Focus::Web { Some("chrome") }
    else if ctx.memory(|memory| memory.focused().is_some()) { Some("egui") }
    else { None }
}

#[cfg(all(debug_assertions, target_os = "macos"))]
fn focus_trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("CROSSBOW_FOCUS_TRACE").as_deref() == Ok("1"))
}

#[cfg(any(test, all(debug_assertions, target_os = "macos")))]
fn keyboard_event_counts(events: &[egui::Event]) -> [usize; 4] {
    let mut counts = [0; 4];
    for event in events {
        match event {
            egui::Event::Key { .. } => counts[0] += 1,
            egui::Event::Text(_) => counts[1] += 1,
            egui::Event::Ime(_) => counts[2] += 1,
            egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_) => counts[3] += 1,
            _ => {}
        }
    }
    counts
}

/// Escape leaves a Tab scope first, keeping the bar open, then closes it.
pub(crate) fn cancel_command(ctx: &egui::Context, command: &mut Option<CommandBar>, focus: &mut Focus, find: Option<&mut FindBar>) -> bool {
    let Some(bar) = command.as_mut() else { return false };
    if !ui::command_bar::leave_scope(ctx, bar) { finish_command(ctx, command, focus, find); }
    true
}

fn complete_find_handoff(ctx: &egui::Context, focus: Focus, find: Option<&mut FindBar>) {
    // A successful deferred navigation overrides only the queued restoration,
    // not a Find field the user already focused themselves.
    if focus == Focus::Web {
        if let Some(find) = find.filter(|find| find.just_opened) {
            find.just_opened = false;
            ctx.memory_mut(|memory| memory.surrender_focus(egui::Id::unique("find-field")));
        }
    }
}

pub(crate) fn finish_command(ctx: &egui::Context, command: &mut Option<CommandBar>, focus: &mut Focus, find: Option<&mut FindBar>) {
    *command = None;
    // TextEdit retains text/clipboard events. This batch belongs to the closing
    // command surface, never the page or a deferred action's destination.
    ctx.input_mut(|input| input.events.retain(|event| !matches!(event,
        egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::Ime(_)
        | egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_)
    )));
    restore_after_search(ctx, focus, find);
}

pub(crate) fn restore_web_focus(ctx: &egui::Context, focus: &mut Focus) {
    ctx.memory_mut(|memory| {
        if let Some(id) = memory.focused() { memory.surrender_focus(id); }
    });
    *focus = Focus::Web;
}

fn restore_after_search(ctx: &egui::Context, focus: &mut Focus, find: Option<&mut FindBar>) {
    if let Some(find) = find {
        *focus = Focus::Chrome;
        find.just_opened = true;
        ctx.memory_mut(|memory| memory.request_focus(egui::Id::unique("find-field")));
    } else {
        restore_web_focus(ctx, focus);
    }
}

fn dismiss_library(ctx: &egui::Context, open: &mut bool, focus: &mut Focus, find: Option<&mut FindBar>, reason: LibraryDismissal) -> bool {
    if !std::mem::take(open) { return false; }
    // An outside press has already selected its page/widget target this frame.
    // Only an explicit dismissal should restore the previous input owner.
    if reason == LibraryDismissal::RestoreFocus { restore_after_search(ctx, focus, find); }
    true
}

fn dismiss_settings(ctx: &egui::Context, settings: &mut Option<ui::settings::SettingsState>, focus: &mut Focus, find: Option<&mut FindBar>) -> bool {
    if settings.take().is_none() { return false; }
    restore_after_search(ctx, focus, find);
    true
}

pub(crate) fn dismiss_find(ctx: &egui::Context, find: &mut Option<FindBar>, focus: &mut Focus, finish: impl FnOnce()) -> bool {
    if find.take().is_none() { return false; }
    restore_web_focus(ctx, focus);
    finish();
    true
}

/// Escape leaves Ask mode before it closes Find. Typing goes back to the
/// Find field, whose query was kept.
pub(crate) fn leave_ask(ask: &mut Option<crate::ask_page::Ask>, find: Option<&mut FindBar>) -> bool {
    if ask.take().is_none() { return false; }
    if let Some(find) = find { find.just_opened = true; }
    true
}

fn dismiss_rename(ctx: &egui::Context, renaming: &mut Option<(RenameTarget, String)>, focus: &mut Focus) -> bool {
    if renaming.take().is_none() { return false; }
    restore_web_focus(ctx, focus);
    true
}

pub(crate) fn item_in_profile(lib: &Library, item: &Item, profile: &str) -> bool {
    item.profile_id == profile && lib.profiles.contains_key(profile) && match item.kind {
        ItemKind::Favorite => item.space_id.is_none(),
        ItemKind::Pinned => item.space_id.as_ref().and_then(|id| lib.spaces.get(id)).is_some_and(|space| space.profile_id == profile),
    }
}

pub(crate) fn tab_profile<'a>(lib: &'a Library, tab: &Tab) -> Option<&'a str> {
    tab_profile_with_view_owner(lib, tab, tab.view.as_ref().map(WebView::profile_id))
}

fn tab_profile_with_view_owner<'a>(lib: &'a Library, tab: &Tab, view_profile: Option<&str>) -> Option<&'a str> {
    let owner = tab_metadata_profile(lib, tab)?;
    // Synced metadata cannot change an existing Chromium request context.
    view_profile.is_none_or(|profile| profile == owner).then_some(owner)
}

fn tab_metadata_profile<'a>(lib: &'a Library, tab: &Tab) -> Option<&'a str> {
    if tab.space_id == FAVORITES_SPACE {
        let item = lib.items.get(tab.item_id.as_ref()?)?;
        return (item.kind == ItemKind::Favorite && item_in_profile(lib, item, &item.profile_id)).then_some(item.profile_id.as_str());
    }
    let space = lib.spaces.get(&tab.space_id)?;
    if !lib.profiles.contains_key(&space.profile_id) { return None; }
    if let Some(id) = &tab.item_id {
        let item = lib.items.get(id)?;
        if item.kind != ItemKind::Pinned || item.space_id.as_deref() != Some(tab.space_id.as_str()) || !item_in_profile(lib, item, &space.profile_id) { return None; }
    }
    Some(&space.profile_id)
}

fn can_move_tab(lib: &Library, tab: &Tab, destination: &str) -> bool {
    let Some(source_profile) = tab_profile(lib, tab) else { return false };
    lib.spaces.get(destination).is_some_and(|space| space.profile_id == source_profile)
}

/// Escape belongs to an open popup before it dismisses the containing surface.
pub(crate) fn take_dismiss_shortcut(ctx: &egui::Context) -> bool {
    !egui::Popup::is_any_open(ctx) && take_shortcut(ctx, egui::Modifiers::NONE, Key::Escape)
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod session_tests;

#[cfg(test)]
#[path = "split_session_tests.rs"]
mod split_session_tests;

#[cfg(test)]
mod texture_retirement_tests {
    use super::*;

    #[test]
    fn peek_cleanup_retires_main_and_popup_once_after_queued_shapes() {
        for popup in [None, Some((egui::TextureId::User(71), 2))] {
            let ctx = egui::Context::default();
            let retirement = TextureRetirement::new(&ctx);
            let main = Some((egui::TextureId::User(70), 1));
            let mut output = ctx.run_ui(Default::default(), |ui| {
                for (id, _) in [main, popup].into_iter().flatten() {
                    ui.painter().image(id, Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(30.0)), Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                }
                // The same helper serves dismissal, replacement and promotion.
                retire_peek_textures(&retirement, main, popup);
                retire_peek_textures(&retirement, main, popup);
            });
            let free = output.textures_delta.free.clone();
            output.textures_delta.clear();
            assert_eq!(free, [main, popup].into_iter().flatten().map(|(id, _)| id).collect());
            let mut next = ctx.run_ui(Default::default(), |_| {});
            let empty = next.textures_delta.free.is_empty();
            next.textures_delta.clear();
            assert!(empty);
        }
    }

    #[test]
    fn failed_parent_removal_retains_registrations_until_successful_retry() {
        for kind in ["item", "space"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("fixture.sqlite");
            let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
            lib.ensure_defaults().unwrap();
            let space = lib.spaces_ordered()[0].0.clone();
            let profile = lib.spaces[&space].profile_id.clone();
            lib.create_space("Keep", "K", Theme::preset(0), &profile).unwrap();
            let pin = lib.add_pinned(&space, None, "https://fixture.example/pin", "Pin").unwrap();
            let mut tabs = vec![Tab::from_row(crossbow_core::store::TabRow {
                id: "removed".into(), space_id: space.clone(), item_id: Some(pin.clone()),
                url: "https://fixture.example/pin".into(), title: "Pin".into(), order: "a0".into(),
                created_ms: 1, last_active_ms: 2,
            })];
            lib.store.put_tab(&tabs[0].row()).unwrap();
            let ctx = egui::Context::default();
            let retirement = TextureRetirement::new(&ctx);
            let main = egui::TextureId::User(50);
            let popup = egui::TextureId::User(51);
            let keep = egui::TextureId::User(52);
            let mut textures = HashMap::from([("removed".into(), (main, 1)), ("keep".into(), (keep, 2))]);
            let mut popups = HashMap::from([("removed".into(), (popup, 3))]);
            let parent = if kind == "item" { ParentRemoval::Item(&pin) } else { ParentRemoval::Space(&space) };
            let id = if kind == "item" { &pin } else { &space };
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection.execute_batch(&format!("CREATE TRIGGER reject_texture_removal BEFORE UPDATE ON entities WHEN NEW.id = '{id}' AND NEW.deleted = 1 BEGIN SELECT RAISE(ABORT, 'fixture texture failure'); END;")).unwrap();
            // Exercise the production take/retire helpers at the caller's post-commit boundary.
            let mut remove = |lib: &mut Library, tabs: &mut Vec<Tab>| -> crossbow_core::library::Result<()> {
                let removed = take_removed_parent_tabs(lib, tabs, parent, 100)?;
                for tab in removed { retirement.retire_tab(&tab.id, &mut textures, &mut popups); }
                Ok(())
            };
            assert!(remove(&mut lib, &mut tabs).is_err());
            let mut failed = ctx.run_ui(Default::default(), |_| {});
            assert!(failed.textures_delta.free.is_empty(), "{kind}: rollback must not retire textures");
            failed.textures_delta.clear();
            assert_eq!(tabs.len(), 1);
            connection.execute_batch("DROP TRIGGER reject_texture_removal;").unwrap();
            remove(&mut lib, &mut tabs).unwrap();
            drop(remove);
            assert!(tabs.is_empty());
            let mut success = ctx.run_ui(Default::default(), |_| {});
            assert_eq!(success.textures_delta.free, [main, popup].into_iter().collect());
            success.textures_delta.clear();
            assert_eq!(textures, HashMap::from([("keep".into(), (keep, 2))]));
            assert!(popups.is_empty());
            retirement.retire_tab("removed", &mut textures, &mut popups);
            let mut repeated = ctx.run_ui(Default::default(), |_| {});
            assert!(repeated.textures_delta.free.is_empty(), "retry cleanup must not retire twice");
            repeated.textures_delta.clear();
        }
    }
}

#[cfg(test)]
mod shortcut_tests {
    use super::*;

    #[test]
    fn command_shortcut_does_not_consume_following_text() {
        for key in [Key::T, Key::L] {
            let ctx = egui::Context::default();
            let input = egui::RawInput {
                events: vec![
                    egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND },
                    egui::Event::Text("favorite 17".into()),
                ],
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                assert!(take_shortcut(ui.ctx(), egui::Modifiers::COMMAND, key));
                assert!(ui.input(|input| input.events.iter().any(|event| matches!(event, egui::Event::Text(text) if text == "favorite 17"))));
            }).textures_delta.clear();
        }
    }

    #[test]
    fn new_folder_shortcut_needs_option_and_command() {
        let press = |modifiers: egui::Modifiers| {
            let ctx = egui::Context::default();
            let input = egui::RawInput {
                events: vec![egui::Event::Key { key: Key::N, physical_key: None, pressed: true, repeat: false, modifiers }],
                ..Default::default()
            };
            let mut taken = false;
            ctx.run_ui(input, |ui| taken = take_shortcut(ui.ctx(), egui::Modifiers::COMMAND | egui::Modifiers::ALT, Key::N)).textures_delta.clear();
            taken
        };
        assert!(press(egui::Modifiers::COMMAND | egui::Modifiers::ALT));
        assert!(!press(egui::Modifiers::COMMAND));
        assert!(!press(egui::Modifiers::COMMAND | egui::Modifiers::ALT | egui::Modifiers::SHIFT));
    }

    #[test]
    fn popup_owns_escape_before_dismissing_settings() {
        let ctx = egui::Context::default();
        let popup = egui::Id::unique("settings-navigation-fixture");
        egui::Popup::open_id(&ctx, popup);
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: Key::Escape, physical_key: None, pressed: true,
                repeat: false, modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        ctx.run_ui(input, |ui| {
            let ctx = ui.ctx();
            assert!(!take_dismiss_shortcut(ctx));
            assert!(ctx.input(|i| i.key_pressed(Key::Escape)));
            egui::Popup::close_id(ctx, popup);
            assert!(take_dismiss_shortcut(ctx));
            assert!(!take_dismiss_shortcut(ctx));
        }).textures_delta.clear();
    }
}

/// Which page, and which of its navigations, has the keyboard.
fn page_mark(view: &WebView) -> crate::shortcuts::PageMark {
    crate::shortcuts::PageMark { view: view.serial(), navigations: view.navigations() }
}

pub(crate) fn take_shortcut(ctx: &egui::Context, mods: egui::Modifiers, key: Key) -> bool {
    ctx.input_mut(|i| {
        let pos = i.events.iter().position(|e| match e {
            egui::Event::Key { key: k, pressed: true, modifiers: m, .. } => {
                *k == key
                    && m.shift == mods.shift
                    && m.alt == mods.alt
                    && m.command == mods.command
                    && (if cfg!(target_os = "macos") { m.ctrl == mods.ctrl } else { mods.command || m.ctrl == mods.ctrl })
            }
            _ => false,
        });
        match pos {
            Some(p) => {
                i.events.remove(p);
                // egui-winit already suppresses text generated by command keys.
                // A following Text event belongs to subsequent typing or an IME.
                true
            }
            None => false,
        }
    })
}

impl CrossbowApp {
    /// Crossbow → Check for Updates…: Settings → General, showing Updates, and a check.
    fn check_for_updates(&mut self, ctx: &egui::Context) {
        self.ui.open_settings(ui::settings::SettingsState::new(ui::settings::Page::General, ctx.input(|i| i.time)));
        self.updates.reveal = true;
        self.updates.check_now();
        ctx.request_repaint();
    }

    /// The helper is waiting to install: quit without asking again.
    pub(crate) fn quit_for_update(&mut self, ctx: &egui::Context) {
        self.quit.borrow_mut().allow_next();
        crate::quit::terminate(ctx);
    }
}

impl eframe::App for CrossbowApp {
    #[cfg(all(debug_assertions, target_os = "macos"))]
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.trace_key_focus(ctx, "raw", &raw.events, raw.focused);
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        if !*ENABLED.get_or_init(|| std::env::var("CROSSBOW_INPUT_TRACE").as_deref() == Ok("1")) { return; }
        let events: Vec<String> = raw.events.iter().filter_map(|event| match event {
            egui::Event::PointerButton { pos, pressed, button, .. } => Some(format!("button:{button:?}:{pressed}@{pos:?}")),
            egui::Event::MouseWheel { delta, .. } => Some(format!("wheel:{delta:?}")),
            egui::Event::WindowFocused(focused) => Some(format!("focus:{focused}")),
            egui::Event::PointerGone => Some("pointer-gone".into()),
            _ => None,
        }).collect();
        if events.is_empty() || COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 2000 { return; }
        if let Some(main) = objc2::MainThreadMarker::new() {
            let app = objc2_app_kit::NSApplication::sharedApplication(main);
            log::info!("input-diagnostic focused={} key={:?} main={:?} settings={} command={} events={events:?}", raw.focused, app.keyWindow().map(|w| w.windowNumber()), app.mainWindow().map(|w| w.windowNumber()), self.ui.settings.is_some(), self.ui.command.is_some());
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.engine_pump.on_logic(self.performance.is_some());
        self.process_download_events(ctx);
        self.tidy.poll(&self.lib, &crate::ai_features::Local, ctx, std::time::Instant::now());
        // Runs while hidden too, so the vault still locks and a copied password is cleared.
        let passwords_open = self.ui.settings.as_ref().is_some_and(|settings| settings.page == ui::settings::Page::Passwords);
        let profile = self.current_profile();
        self.passwords.set_profiles(&profile, self.lib.profiles.iter().map(|(id, profile)| (id.clone(), profile.name.clone())));
        self.passwords.tick(ctx, self.sync.session.as_ref().map(|s| s.account_id.as_str()), passwords_open);
        self.sync_login_capture();
        self.receive_links(ctx);
        if little::menu::take() { self.new_little_window(ctx); }
        if self.hotkey.take_pressed() {
            // From another app: bring Crossbow back if hidden; the new window comes forward.
            crate::links::unhide_app();
            self.new_little_window(ctx);
        }
        if crate::updates::menu::take() { self.check_for_updates(ctx); }
        let browser_downloads = self.downloads.iter().filter(|download| download.active()).count();
        let now_secs = crossbow_core::now_ms() / 1000;
        if self.updates.tick(&self.lib.store, now_secs, std::time::Instant::now(), browser_downloads) == Some(crate::updates::Restart::Quit) {
            self.quit_for_update(ctx);
        }
        crate::quit::on_close_request(&mut self.quit.borrow_mut(), ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        use crate::performance::Surface;
        let timing = self.engine_pump.on_ui();
        let started = timing.map(|(started, _)| started);
        let cef_pump_ms = timing.map(|(_, elapsed)| elapsed).unwrap_or(0.0);
        let mut surface = Surface::Onboarding;
        let ctx = ui.ctx().clone();
        let mut settings_ms = 0.0;
        self.system_dark = ctx.theme() == egui::Theme::Dark;
        ui::anim::set_reduced_motion(!self.lib.settings.smooth_animations);
        self.sync.poll(&mut self.lib, &ctx);
        self.update_native_window(&ctx);
        self.app_icon.poll(&ctx);
        // The file panel's answer arrives without waking the UI.
        if self.content_blocking.poll(&ctx) { ctx.request_repaint_after(std::time::Duration::from_millis(100)); }
        self.process_view_events(&ctx);
        self.update_translations(&ctx);
        self.tidy_tabs.tick(&self.lib, &crate::tidy_tabs::TITLE_PROVIDER, &ctx);
        self.tidy_notices(ctx.input(|i| i.time));
        for text in self.updates.notices.drain(..) { self.ui.toasts.push(Toast { text, at: ctx.input(|i| i.time) }); }
        // Drawing may finish closing Peek after its keyboard action was queued.
        let navigation = NavigationBatch::new(self.ui.peek.as_ref().map(|peek| peek.closing_at.is_some()));
        // The quit question answers Enter and Escape before anything else sees them.
        let asking = self.quit.borrow().asking();
        if let Some(quit) = ui::quit::take_keys(&ctx, asking) { crate::quit::answer(&mut self.quit.borrow_mut(), &ctx, quit); }
        let tidy_escape = self.tidy_tabs.sheet.is_some() && !asking && ui::tidy_tabs::take_escape(&ctx);

        if self.ui.onboarding.is_some() {
            ui::onboarding::show(self, ui);
        } else {
            // Before shortcuts take their presses: a held key's repeats play no sound.
            self.key_repeat = ctx.input(|i| i.events.iter().any(|event| matches!(event, egui::Event::Key { pressed: true, repeat: true, .. })));
            self.handle_shortcuts(&ctx);
            if self.ui.command.is_some() {
                ui::command_bar::prepare_input(&ctx);
            }
            ui::content::layout_and_draw(self, ui);
            ui::overlays::show(self, ui);
            let command_drawn = self.ui.command.is_some();
            ui::command_bar::show(self, ui);
            let settings_started = started.map(|_| std::time::Instant::now());
            ui::settings::show(self, ui);
            ui::tidy_tabs::show_sheet(self, &ctx, tidy_escape);
            settings_ms = settings_started.map(|s| s.elapsed().as_secs_f64() * 1000.0).unwrap_or(0.0);
            surface = if self.ui.settings.is_some() { Surface::Settings }
                else if command_drawn { Surface::CommandBar }
                else { Surface::Browser };
            self.forward_keys(&ctx);
            self.run_unsent_shortcuts(&ctx);
        }
        if self.quit.borrow().asking() {
            if let Some(quit) = ui::quit::show(&ctx, &self.palette(), &self.downloads) { crate::quit::answer(&mut self.quit.borrow_mut(), &ctx, quit); }
        }
        self.apply_actions(&ctx, navigation);
        self.tidy_notices(ctx.input(|i| i.time));
        self.update_little_windows(&ctx);
        self.update_pip(&ctx);
        self.update_visibility();
        self.update_unread();
        self.periodic(&ctx);
        if let (Some(trace), Some(started)) = (&mut self.performance, started) {
            trace.record(&ctx, started, cef_pump_ms, settings_ms, frame.info().cpu_usage, surface);
        }
        // A new version confirms itself only once it has drawn and run a while.
        self.updates.drawn(std::time::Instant::now());
    }

    fn on_exit(&mut self) {
        self.save_session();
        self.passwords.shutdown();
        self.hotkey.release();
        self.close_little_windows();
        self.close_pip();
        for t in self.tabs.iter_mut() {
            t.unload();
        }
        if let Some(p) = self.ui.peek.take() {
            p.view.close();
        }
        // A clean quit after a frame confirms a new version, before a
        // verified update installs, with automatic updates on, unless
        // browser downloads were in progress.
        self.updates.confirm_at_quit();
        let browser_downloads = self.downloads.iter().filter(|download| download.active()).count();
        self.updates.install_at_quit(browser_downloads);
        self.sync.shutdown();
        if let Some(trace) = &self.performance {
            if let Err(error) = trace.save() {
                log::warn!("Could not save local frame timing: {error}");
            }
        }
    }
}
