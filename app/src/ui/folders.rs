//! Folder and pinned-item menus: the sidebar's "+" menu, the item, folder and
//! folder-picker menus, and the library work behind their new entries.

use crate::app::{Action, CrossbowApp, MenuKind, MenuState, RenameTarget, Toast};
use crate::shortcuts::Command;
use crate::tabs::{FAVORITES_SPACE, Tab};
use crate::ui::icons::Icon;
use crate::ui::overlays::MenuUi;
use crossbow_core::library::{self, Library, LibraryError, PinnedNode};
use crossbow_core::model::ItemKind;
use crossbow_core::now_ms;
use crossbow_core::order::key_between;
use egui::{Id, Key, Pos2, Ui};
use std::collections::BTreeSet;

const NEW_FOLDER: &str = "New Folder";
/// Deeper folders keep this indent so their names stay readable.
const MAX_INDENT_DEPTH: usize = 6;

/// What a picked folder receives.
#[derive(Clone, Debug, PartialEq)]
pub enum FolderTarget {
    /// Move a pinned item.
    Item(String),
    /// Pin a Today tab.
    Tab(String),
}

/// Library work behind the folder menus, applied with the other actions.
#[derive(Clone, Debug, PartialEq)]
pub enum FolderOp {
    NewFolderIn(String),
    NewFolderWith(String),
    Duplicate(String),
    Favorite(String),
    OpenAll(String),
    NewTabIn(String),
    SetUrl { item: String, url: String },
    PinTab { tab: String, folder: String },
}

/// What a chosen menu entry asks for.
#[derive(Debug)]
pub(crate) enum Pick {
    Run(Action),
    Rename(RenameTarget, String),
    Copy(String),
    /// Replace the menu, e.g. "Move to Folder ›".
    Open(MenuKind),
    Close,
}

/// What a menu reads: the library, live tabs and the visible Space.
pub(crate) struct MenuCx<'a> {
    pub lib: &'a Library,
    pub tabs: &'a [Tab],
    pub space: &'a str,
    pub focused_tab: Option<String>,
    pub shortcuts: &'a crate::shortcuts::Shortcuts,
    /// Tidy Tab Titles, when the app has it; tests of other menus leave it out.
    pub tidy_tabs: Option<&'a crate::tidy_tabs::Tidy>,
}

impl<'a> MenuCx<'a> {
    pub(crate) fn of(app: &'a CrossbowApp) -> Self {
        Self { lib: &app.lib, tabs: &app.tabs, space: &app.active_space, focused_tab: app.focused_tab_id(), shortcuts: &app.shortcuts, tidy_tabs: Some(&app.tidy_tabs) }
    }

    fn tab_for_item(&self, item: &str) -> Option<&'a Tab> {
        self.tabs.iter().find(|t| t.item_id.as_deref() == Some(item))
    }
}

/// The library side of an op; the app adds renaming, loading and search.
#[derive(Debug, PartialEq)]
pub(crate) enum Applied {
    Done,
    Rename(String),
    Opened(Vec<String>),
    Search(String),
}

fn menu_focus_id() -> Id {
    Id::unique("context-menu-keyboard")
}

/// The next menu shown was opened from the keyboard: focus its first entry.
pub(crate) fn request_menu_focus(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(menu_focus_id(), true));
}

pub(crate) fn take_menu_focus(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| d.remove_temp::<bool>(menu_focus_id())).unwrap_or(false)
}

/// Open a menu, focusing its first entry when the keyboard opened it.
pub(crate) fn open_menu(ctx: &egui::Context, pos: Pos2, kind: MenuKind, keyboard: bool) -> MenuState {
    if keyboard {
        request_menu_focus(ctx);
    }
    MenuState { pos, kind }
}

/// One entry: records `make()` when it is chosen.
fn entry(m: &MenuUi, ui: &mut Ui, label: &str, shortcut: &str, enabled: bool, pick: &mut Option<Pick>, make: impl FnOnce() -> Pick) {
    if m.item(ui, label, shortcut, enabled) {
        *pick = Some(make());
    }
}

/// Tidy Title appears once Tidy Tab Titles was turned on for the pin's
/// profile, disabled with the reason when it can't run. Restore Page Title
/// clears any custom title, hand-made or accepted.
fn tidy_entries(m: &MenuUi, ui: &mut Ui, cx: &MenuCx, id: &str, pick: &mut Option<Pick>) {
    let Some(item) = cx.lib.items.get(id) else { return };
    if let Some(tidy) = cx.tidy_tabs.filter(|_| item.kind == ItemKind::Pinned) {
        if crate::tidy_tabs::turned_on(cx.lib, &item.profile_id, crate::tidy_tabs::TITLES) {
            let why = tidy.can_tidy_title(cx.lib, id).err();
            entry(m, ui, "Tidy Title", "", why.is_none(), pick, || Pick::Run(Action::TidyTitle(id.into())));
            if let Some(why) = why { m.note(ui, &why); }
        }
    }
    if item.custom_title.as_deref().is_some_and(|title| !title.is_empty()) {
        entry(m, ui, "Restore Page Title", "", true, pick, || Pick::Run(Action::RestorePageTitle(id.into())));
    }
}

fn op(op: FolderOp) -> Pick {
    Pick::Run(Action::Folder(op))
}

/// The sidebar menus this module owns; None for other kinds.
pub(crate) fn menu(m: &MenuUi, ui: &mut Ui, cx: &MenuCx, kind: &MenuKind) -> Option<Pick> {
    match kind {
        MenuKind::Add => add_menu(m, ui, cx),
        MenuKind::Item(item) => item_menu(m, ui, cx, item),
        MenuKind::Folder(folder) => folder_menu(m, ui, cx, folder),
        MenuKind::Folders(target) => folder_picker(m, ui, cx, target),
        MenuKind::Web { .. } | MenuKind::Tab(_) | MenuKind::Space(_) | MenuKind::Today(_) | MenuKind::Logins { .. } | MenuKind::Addresses(_) | MenuKind::Cards(_) | MenuKind::ContentBlocking { .. } => None,
    }
}

fn add_menu(m: &MenuUi, ui: &mut Ui, cx: &MenuCx) -> Option<Pick> {
    let mut pick = None;
    entry(m, ui, "New Tab", &cx.shortcuts.hint(Command::NewTab), true, &mut pick, || Pick::Run(Action::OpenCommandBar { edit_current: false }));
    entry(m, ui, NEW_FOLDER, &cx.shortcuts.hint(Command::NewFolder), true, &mut pick, || Pick::Run(Action::NewFolder));
    let split = &cx.shortcuts.hint(Command::SplitView);
    entry(m, ui, "New Split View", split, cx.focused_tab.is_some(), &mut pick, || Pick::Run(Action::SplitWith(cx.focused_tab.clone().unwrap_or_default())));
    pick
}

fn item_menu(m: &MenuUi, ui: &mut Ui, cx: &MenuCx, id: &str) -> Option<Pick> {
    let Some(item) = cx.lib.items.get(id) else { return Some(Pick::Close) };
    let tab = cx.tab_for_item(id);
    let pinned = item.kind == ItemKind::Pinned;
    let mut pick = None;
    entry(m, ui, "Open", "", true, &mut pick, || Pick::Run(Action::OpenItem(id.into())));
    entry(m, ui, "Rename…", "", true, &mut pick, || Pick::Rename(RenameTarget::Item(id.into()), item.display_title().into()));
    tidy_entries(m, ui, cx, id, &mut pick);
    entry(m, ui, "Copy URL", "", true, &mut pick, || Pick::Copy(tab.map_or(&item.url, |t| &t.url).clone()));
    entry(m, ui, "Duplicate", "", true, &mut pick, || op(FolderOp::Duplicate(id.into())));
    let drifted = tab.filter(|t| t.url != item.url);
    entry(m, ui, "Go Back to Pinned URL", "", drifted.is_some(), &mut pick, || Pick::Run(Action::ResetItem(id.into())));
    entry(m, ui, "Update Pinned URL to Current", "", drifted.is_some(), &mut pick, || {
        op(FolderOp::SetUrl { item: id.into(), url: drifted.map(|t| t.url.clone()).unwrap_or_default() })
    });
    if let Some(t) = tab {
        entry(m, ui, "Unload Tab", "", true, &mut pick, || Pick::Run(Action::CloseTab(t.id.clone())));
    }
    m.sep(ui);
    if pinned {
        entry(m, ui, "Add to Favorites", "", true, &mut pick, || op(FolderOp::Favorite(id.into())));
        let space = item.space_id.as_deref().unwrap_or_default();
        let destinations = item.folder_id.is_some() || folder_tree(cx.lib, space).iter().any(|(f, _, _)| Some(f) != item.folder_id.as_ref());
        if destinations {
            entry(m, ui, "Move to Folder", "›", true, &mut pick, || Pick::Open(MenuKind::Folders(FolderTarget::Item(id.into()))));
        }
        entry(m, ui, "New Folder with This Tab", "", true, &mut pick, || op(FolderOp::NewFolderWith(id.into())));
        m.sep(ui);
        if let Some(t) = tab {
            entry(m, ui, "Unpin", &cx.shortcuts.hint(Command::PinTab), true, &mut pick, || Pick::Run(Action::UnpinTab(t.id.clone())));
        }
    }
    entry(m, ui, if pinned { "Delete" } else { "Remove from Favorites" }, "", true, &mut pick, || Pick::Run(Action::DeleteItem(id.into())));
    pick
}

fn folder_menu(m: &MenuUi, ui: &mut Ui, cx: &MenuCx, id: &str) -> Option<Pick> {
    let Some(folder) = cx.lib.folders.get(id) else { return Some(Pick::Close) };
    let mut pick = None;
    entry(m, ui, "Rename…", "", true, &mut pick, || Pick::Rename(RenameTarget::Folder(id.into()), folder.name.clone()));
    entry(m, ui, NEW_FOLDER, &cx.shortcuts.hint(Command::NewFolder), true, &mut pick, || Pick::Run(Action::NewFolder));
    entry(m, ui, "New Folder Inside", "", true, &mut pick, || op(FolderOp::NewFolderIn(id.into())));
    entry(m, ui, "New Tab Inside", "", true, &mut pick, || op(FolderOp::NewTabIn(id.into())));
    entry(m, ui, if folder.collapsed { "Expand" } else { "Collapse" }, "", true, &mut pick, || Pick::Run(Action::ToggleFolder(id.into())));
    let unopened = folder_items(cx.lib, id).iter().any(|item| cx.tab_for_item(item).is_none());
    entry(m, ui, "Open All", "", unopened, &mut pick, || op(FolderOp::OpenAll(id.into())));
    m.sep(ui);
    entry(m, ui, "Delete Folder", "", true, &mut pick, || Pick::Run(Action::DeleteFolder(id.into())));
    m.heading(ui, "Contents move up one level");
    pick
}

/// A Space's folders as destinations; the current parent is shown disabled.
fn folder_picker(m: &MenuUi, ui: &mut Ui, cx: &MenuCx, target: &FolderTarget) -> Option<Pick> {
    let (space, current) = match target {
        FolderTarget::Item(id) => match cx.lib.items.get(id) {
            Some(item) if item.kind == ItemKind::Pinned && item.space_id.as_deref() == Some(cx.space) => (cx.space, item.folder_id.as_deref()),
            _ => return Some(Pick::Close),
        },
        FolderTarget::Tab(id) => match cx.tabs.iter().find(|t| t.id == *id) {
            Some(tab) if tab.item_id.is_none() && tab.space_id == cx.space => (cx.space, None),
            _ => return Some(Pick::Close),
        },
    };
    let destination = |folder: Option<String>| match target {
        FolderTarget::Item(id) => Pick::Run(Action::MovePinned { node: id.clone(), folder, index: usize::MAX }),
        FolderTarget::Tab(id) => op(FolderOp::PinTab { tab: id.clone(), folder: folder.unwrap_or_default() }),
    };
    let mut pick = None;
    m.heading(ui, if matches!(target, FolderTarget::Item(_)) { "Move to Folder" } else { "Pin to Folder" });
    if current.is_some() {
        entry(m, ui, "Top Level", "", true, &mut pick, || destination(None));
    }
    let limit = (ui.ctx().content_rect().height() - 120.0).max(26.0 * 3.0);
    egui::ScrollArea::vertical().id_salt("folder-picker").max_height(limit).auto_shrink([true, true]).show(ui, |ui| {
        for (id, name, depth) in folder_tree(cx.lib, space) {
            let indent = depth.min(MAX_INDENT_DEPTH) as f32 * 14.0;
            if m.item_at(ui, &name, "", Some(id.as_str()) != current, Some((Icon::Folder, indent))) {
                pick = Some(destination(Some(id)));
            }
        }
    });
    pick
}

/// "Pin to Folder ›" in a Today tab's menu, when the Space has folders.
pub(crate) fn tab_entries(m: &MenuUi, ui: &mut Ui, cx: &MenuCx, tab: &str) -> Option<Pick> {
    let t = cx.tabs.iter().find(|t| t.id == tab)?;
    if t.item_id.is_some() || t.space_id == FAVORITES_SPACE || folder_tree(cx.lib, &t.space_id).is_empty() {
        return None;
    }
    let mut pick = None;
    entry(m, ui, "Pin to Folder", "›", true, &mut pick, || Pick::Open(MenuKind::Folders(FolderTarget::Tab(tab.into()))));
    pick
}

/// Apply a chosen entry; returns whether the menu closes.
pub(crate) fn apply_pick(app: &mut CrossbowApp, ctx: &egui::Context, pos: Pos2, pick: Pick) -> bool {
    match pick {
        Pick::Run(action) => app.actions.push(action),
        Pick::Rename(target, text) => app.ui.renaming = Some((target, text)),
        Pick::Copy(text) => {
            ctx.copy_text(text);
            app.actions.push(Action::Toast("Copied URL".into()));
            app.cue(crate::feel::Cue::LinkCopied);
        }
        Pick::Open(kind) => {
            let keyboard = ctx.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Space));
            app.ui.menu = Some(open_menu(ctx, pos, kind, keyboard));
            return false;
        }
        Pick::Close => {}
    }
    true
}

/// A Space's folders depth-first in sidebar order, with their depth.
pub(crate) fn folder_tree(lib: &Library, space: &str) -> Vec<(String, String, usize)> {
    fn walk(lib: &Library, space: &str, parent: Option<&str>, depth: usize, seen: &mut BTreeSet<String>, out: &mut Vec<(String, String, usize)>) {
        for node in lib.pinned_children(space, parent) {
            if let PinnedNode::Folder(id, folder) = node {
                // A malformed synced tree must not loop.
                if seen.insert(id.clone()) {
                    out.push((id.clone(), folder.name, depth));
                    walk(lib, space, Some(&id), depth + 1, seen, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(lib, space, None, 0, &mut BTreeSet::new(), &mut out);
    out
}

/// Pinned items anywhere inside `folder`, in sidebar order.
fn folder_items(lib: &Library, folder: &str) -> Vec<String> {
    let Some(space) = lib.folders.get(folder).map(|f| f.space_id.clone()) else { return Vec::new() };
    let mut stack = vec![folder.to_owned()];
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    while let Some(parent) = stack.pop() {
        if !seen.insert(parent.clone()) { continue; }
        let mut folders = Vec::new();
        for node in lib.pinned_children(&space, Some(&parent)) {
            match node {
                PinnedNode::Item(id, _) => items.push(id),
                PinnedNode::Folder(id, _) => folders.push(id),
            }
        }
        stack.extend(folders.into_iter().rev());
    }
    items
}

/// Tab records for the folder's items that have none yet; returns their ids.
fn open_all(lib: &Library, tabs: &mut Vec<Tab>, folder: &str) -> Vec<String> {
    let Some(profile) = lib.folders.get(folder).and_then(|f| lib.spaces.get(&f.space_id)).map(|s| s.profile_id.clone()) else { return Vec::new() };
    let mut opened = Vec::new();
    for id in folder_items(lib, folder) {
        let item = &lib.items[&id];
        if tabs.iter().any(|t| t.item_id.as_deref() == Some(id.as_str())) || !crate::app::item_in_profile(lib, item, &profile) {
            continue;
        }
        let now = now_ms();
        let tab = crossbow_core::new_id();
        tabs.push(Tab {
            id: tab.clone(),
            space_id: item.space_id.clone().unwrap_or_default(),
            item_id: Some(id.clone()),
            url: item.url.clone(),
            title: item.display_title().into(),
            order: key_between(None, None),
            created_ms: now,
            last_active_ms: now,
            view: None,
            nav: Default::default(),
            muted: false,
            dirty: true,
        });
        opened.push(tab);
    }
    opened
}

/// Pin a Today tab at the end of `folder`, in the tab's own Space.
pub(crate) fn pin_into_folder(lib: &mut Library, tab: &mut Tab, folder: &str) -> library::Result<String> {
    if tab.item_id.is_some() {
        return Err(LibraryError::InvalidMove("the tab is already saved"));
    }
    if lib.folders.get(folder).is_none_or(|f| f.space_id != tab.space_id) {
        return Err(LibraryError::NotFound(folder.into()));
    }
    let item = lib.add_pinned(&tab.space_id, Some(folder), &tab.url, &tab.display_title())?;
    tab.item_id = Some(item.clone());
    tab.dirty = true;
    Ok(item)
}

/// The library side of an op, shared by the app and its tests.
pub(crate) fn run_op(lib: &mut Library, tabs: &mut Vec<Tab>, op: FolderOp) -> library::Result<Applied> {
    Ok(match op {
        FolderOp::NewFolderIn(parent) => Applied::Rename(lib.create_folder_in(&parent, NEW_FOLDER)?),
        FolderOp::NewFolderWith(item) => Applied::Rename(lib.create_folder_with_item(&item, NEW_FOLDER)?),
        FolderOp::Duplicate(item) => {
            lib.duplicate_item(&item)?;
            Applied::Done
        }
        FolderOp::Favorite(item) => {
            lib.favorite_pinned(&item)?;
            for tab in tabs.iter_mut().filter(|t| t.item_id.as_deref() == Some(item.as_str())) {
                tab.space_id = FAVORITES_SPACE.into();
                tab.dirty = true;
            }
            Applied::Done
        }
        FolderOp::OpenAll(folder) => Applied::Opened(open_all(lib, tabs, &folder)),
        FolderOp::NewTabIn(folder) => {
            if !lib.folders.contains_key(&folder) {
                return Err(LibraryError::NotFound(folder));
            }
            Applied::Search(folder)
        }
        FolderOp::SetUrl { item, url } => {
            lib.update_item(&item, |i| i.url = url)?;
            Applied::Done
        }
        FolderOp::PinTab { tab, folder } => {
            let tab = tabs.iter_mut().find(|t| t.id == tab).ok_or(LibraryError::NotFound(tab))?;
            pin_into_folder(lib, tab, &folder)?;
            Applied::Done
        }
    })
}

impl CrossbowApp {
    pub(crate) fn apply_folder_op(&mut self, ctx: &egui::Context, op: FolderOp, now: f64) {
        let pinning = if let FolderOp::PinTab { tab, .. } = &op { Some(tab.clone()) } else { None };
        match run_op(&mut self.lib, &mut self.tabs, op) {
            Ok(Applied::Done) => {
                if let Some(item) = pinning.and_then(|tab| self.tab(&tab)).and_then(|tab| tab.item_id.clone()) {
                    self.tidy_tabs.pinned(&self.lib, &item);
                }
            }
            Ok(Applied::Rename(folder)) => self.ui.renaming = Some((RenameTarget::Folder(folder), NEW_FOLDER.into())),
            Ok(Applied::Opened(tabs)) => {
                // Load in the background at the current page size; nothing is activated.
                let size = self.ui.panes.first().map(|(_, rect)| rect.size()).unwrap_or_else(|| ctx.content_rect().size());
                for tab in &tabs {
                    self.ensure_view(tab, (size.x, size.y), ctx.pixels_per_point());
                }
                let text = match tabs.len() {
                    0 => "Every tab in this folder is already open.".into(),
                    1 => "Opened 1 tab.".into(),
                    n => format!("Opened {n} tabs."),
                };
                self.ui.toasts.push(Toast { text, at: now });
            }
            Ok(Applied::Search(folder)) => {
                self.open_command_bar(ctx, false);
                if let Some(origin) = self.ui.command.as_mut().and_then(|bar| bar.origin.as_mut()) {
                    origin.pin_into(&folder);
                }
            }
            Err(error) => {
                log::warn!("Could not apply a folder action: {error}");
                self.ui.toasts.push(Toast { text: "Could not save this change. Nothing was changed. Try again.".into(), at: now });
            }
        }
    }

    /// Pin a tab opened by New Tab Inside; a folder deleted meanwhile leaves it in Today.
    pub(crate) fn pin_new_tab(&mut self, tab: &str, folder: &str, now: f64) {
        let Some(index) = self.tabs.iter().position(|t| t.id == tab) else { return };
        if let Err(error) = pin_into_folder(&mut self.lib, &mut self.tabs[index], folder) {
            log::warn!("Could not pin a new tab in its folder: {error}");
            self.ui.toasts.push(Toast { text: "The folder is no longer available. The tab stays in Today.".into(), at: now });
        }
    }
}
