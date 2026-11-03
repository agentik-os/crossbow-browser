//! Simulated egui journeys for the folder and pinned-item menus: open each menu
//! by pointer and by keyboard, choose each new entry, apply it to a real SQLite
//! library and read the tree back after reopening.

use super::*;
use crate::tabs::{FAVORITES_SPACE, Tab};
use crate::ui::folders::{self, Applied, FolderOp, FolderTarget, MenuCx, Pick};
use crate::ui::overlays::{MenuUi, menu_frame, menu_surface};
use crossbow_core::library::Library;
use crossbow_core::model::{ItemKind, Theme};
use crossbow_core::store::{Store, TabRow};
use egui::{Event, Key, Modifiers, PointerButton, Vec2};

fn sizes() -> [(Vec2, f32); 2] {
    [(vec2(640.0, 420.0), 200.0), (vec2(1360.0, 860.0), 252.0)]
}

fn key(key: Key, modifiers: Modifiers) -> Vec<Event> {
    [true, false].map(|pressed| Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }).into()
}

fn button(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }
}

/// The pinned tree as readable lines, depth-first in sidebar order.
fn tree_of(lib: &Library, space: &str) -> Vec<String> {
    fn walk(lib: &Library, space: &str, folder: Option<&str>, depth: usize, out: &mut Vec<String>) {
        for node in lib.pinned_children(space, folder) {
            match node {
                PinnedNode::Item(_, item) => out.push(format!("{}{}", "  ".repeat(depth), item.display_title())),
                PinnedNode::Folder(id, f) => {
                    out.push(format!("{}[{}{}]", "  ".repeat(depth), f.name, if f.collapsed { " collapsed" } else { "" }));
                    walk(lib, space, Some(&id), depth + 1, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(lib, space, None, 0, &mut out);
    out
}

struct Fixture {
    dir: std::path::PathBuf,
    lib: Library,
    space: String,
    tabs: Vec<Tab>,
    shortcuts: crate::shortcuts::Shortcuts,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("crossbow-folders-{}", crossbow_core::new_id()));
        std::fs::create_dir(&dir).unwrap();
        let mut lib = Library::open(Store::open(&dir.join("library.sqlite")).unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let space = lib.spaces_ordered()[0].0.clone();
        Self { dir, lib, space, tabs: Vec::new(), shortcuts: crate::shortcuts::Shortcuts::defaults() }
    }

    fn reopened_tree(&self) -> Vec<String> {
        tree_of(&Library::open(Store::open(&self.dir.join("library.sqlite")).unwrap()).unwrap(), &self.space)
    }

    fn tree(&self) -> Vec<String> {
        tree_of(&self.lib, &self.space)
    }

    fn cx(&self, focused: Option<&str>) -> MenuCx<'_> {
        MenuCx { lib: &self.lib, tabs: &self.tabs, space: &self.space, focused_tab: focused.map(String::from), shortcuts: &self.shortcuts, tidy_tabs: None }
    }

    fn pin(&mut self, folder: Option<&str>, title: &str) -> String {
        let space = self.space.clone();
        self.lib.add_pinned(&space, folder, &format!("https://{}.example/", title.to_lowercase()), title).unwrap()
    }

    fn folder(&mut self, parent: Option<&str>, name: &str) -> String {
        let space = self.space.clone();
        self.lib.create_folder(&space, parent, name).unwrap()
    }

    fn tab(&mut self, id: &str, item: Option<&str>) -> String {
        let (space, url) = match item.map(|item| &self.lib.items[item]) {
            Some(item) if item.kind == ItemKind::Favorite => (FAVORITES_SPACE.to_owned(), item.url.clone()),
            Some(item) => (self.space.clone(), item.url.clone()),
            None => (self.space.clone(), format!("https://{id}.example/")),
        };
        self.tabs.push(Tab::from_row(TabRow {
            id: id.into(), space_id: space, item_id: item.map(String::from), url, title: id.into(),
            order: "a0".into(), created_ms: 1, last_active_ms: 2,
        }));
        id.into()
    }

    /// Apply an action the way the app's action loop does for these menus.
    fn apply(&mut self, action: Action) -> Applied {
        match action {
            Action::Folder(op) => folders::run_op(&mut self.lib, &mut self.tabs, op).unwrap(),
            // app.rs Action::MovePinned: the destination is the visible Space.
            Action::MovePinned { node, folder, index } => {
                self.lib.move_pinned(&node, &self.space, folder.as_deref(), index).unwrap();
                Applied::Done
            }
            // app.rs Action::ToggleFolder / DeleteFolder / NewFolder.
            Action::ToggleFolder(id) => {
                self.lib.update_folder(&id, |f| f.collapsed = !f.collapsed).unwrap();
                Applied::Done
            }
            Action::DeleteFolder(id) => {
                self.lib.delete_folder(&id).unwrap();
                Applied::Done
            }
            Action::NewFolder => {
                let space = self.space.clone();
                Applied::Rename(self.lib.create_folder(&space, None, "New Folder").unwrap())
            }
            other => panic!("unexpected action {other:?}"),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Shown {
    pick: Option<Pick>,
    area: Rect,
    texts: Vec<(String, Rect, Rect)>,
    lit: Vec<String>,
}

impl Shown {
    fn has(&self, label: &str) -> bool {
        self.texts.iter().any(|(text, _, _)| text == label)
    }

    fn point(&self, label: &str) -> Pos2 {
        let (_, rect, clip) = self.texts.iter().find(|(text, _, _)| text == label)
            .unwrap_or_else(|| panic!("missing entry {label:?}; shown: {:?}", self.labels()));
        assert!(clip.intersects(*rect), "{label:?} is clipped");
        rect.intersect(*clip).center()
    }

    fn labels(&self) -> Vec<&str> {
        self.texts.iter().map(|(text, _, _)| text.as_str()).collect()
    }
}

/// Renders menus through the production surface, frame, entries and focus.
struct MenuProbe {
    ctx: egui::Context,
    screen: Rect,
    pal: Palette,
    frame: u32,
}

impl MenuProbe {
    fn new(size: Vec2) -> Self {
        let ctx = egui::Context::default();
        window::install_fonts(&ctx);
        Self { ctx, screen: Rect::from_min_size(Pos2::ZERO, size), pal: theme::palette(&Theme::preset(0), false), frame: 0 }
    }

    fn show(&mut self, cx: &MenuCx, kind: &MenuKind, pos: Pos2, focus_first: bool, events: Vec<Event>) -> Shown {
        let (mut pick, mut area) = (None, Rect::NOTHING);
        let pal = self.pal.clone();
        // A 100 ms frame step lets the menu's fade-in finish in a frame, so highlights compare exactly.
        let mut output = self.ctx.run_ui(egui::RawInput { screen_rect: Some(self.screen), events, time: Some(self.frame as f64 / 10.0), ..Default::default() }, |ui| {
            let m = MenuUi::new(&pal, focus_first);
            area = menu_surface(pos).show(ui.ctx(), |ui| {
                menu_frame(&pal).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    pick = match kind {
                        MenuKind::Tab(tab) => folders::tab_entries(&m, ui, cx, tab),
                        kind => folders::menu(&m, ui, cx, kind),
                    };
                });
            }).response.rect;
        });
        output.textures_delta.clear();
        self.frame += 1;
        let lit_fill = pal.accent.gamma_multiply(0.92);
        let lit: Vec<Rect> = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect) if rect.fill == lit_fill => Some(rect.rect),
            _ => None,
        }).collect();
        let texts: Vec<(String, Rect, Rect)> = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()), shape.clip_rect)),
            _ => None,
        }).collect();
        let lit = texts.iter().filter(|(_, rect, _)| lit.iter().any(|row| row.contains(rect.center()))).map(|(text, _, _)| text.clone()).collect();
        Shown { pick, area, texts, lit }
    }

    /// Open `kind` at `pos` like production does, then let the area size itself and finish fading in.
    fn open(&mut self, cx: &MenuCx, kind: &MenuKind, pos: Pos2, keyboard: bool) -> Shown {
        self.show(cx, kind, pos, keyboard, vec![]);
        self.show(cx, kind, pos, false, vec![]);
        self.show(cx, kind, pos, false, vec![])
    }

    /// Choose `label` by pointer, or by arrow keys and Return.
    fn choose(&mut self, cx: &MenuCx, kind: &MenuKind, pos: Pos2, label: &str, keyboard: bool) -> Option<Pick> {
        let mut shown = self.open(cx, kind, pos, keyboard);
        if keyboard {
            assert!(!shown.lit.is_empty(), "a keyboard-opened menu focuses its first entry: {:?}", shown.labels());
            for _ in 0..80 {
                if shown.lit.iter().any(|lit| lit == label) { break; }
                // egui moves focus at the end of the frame that saw the key, so the highlight shows a frame later.
                assert!(self.show(cx, kind, pos, false, key(Key::ArrowDown, Modifiers::NONE)).pick.is_none());
                shown = self.show(cx, kind, pos, false, vec![]);
            }
            assert!(shown.lit.iter().any(|lit| lit == label), "arrow keys never reached {label:?}; lit {:?}", shown.lit);
            return self.show(cx, kind, pos, false, key(Key::Enter, Modifiers::NONE)).pick;
        }
        let point = shown.point(label);
        self.show(cx, kind, pos, false, vec![Event::PointerMoved(point)]);
        assert!(self.show(cx, kind, pos, false, vec![button(point, PointerButton::Primary, true)]).pick.is_none());
        self.show(cx, kind, pos, false, vec![button(point, PointerButton::Primary, false)]).pick
    }
}

/// A sidebar column built from the production header, row, drop-zone and empty-space helpers.
struct SidebarProbe {
    ctx: egui::Context,
    screen: Rect,
    width: f32,
    pal: Palette,
    frame: u32,
}

struct SidebarFrame {
    opened: Option<MenuState>,
    add: egui::Response,
    header: Rect,
    row: Rect,
    row_focused: bool,
    empty: Rect,
    zone: Rect,
    texts: Vec<(String, Rect, Rect)>,
}

impl SidebarProbe {
    fn new(size: Vec2, width: f32) -> Self {
        let ctx = egui::Context::default();
        window::install_fonts(&ctx);
        Self { ctx, screen: Rect::from_min_size(Pos2::ZERO, size), width, pal: theme::palette(&Theme::preset(0), false), frame: 0 }
    }

    fn viewport(&self) -> Rect {
        Rect::from_min_max(pos2(0.0, 120.0), pos2(self.width, self.screen.bottom() - 46.0))
    }

    /// `kind` None draws the empty pinned zone instead of a row.
    fn frame(&mut self, kind: Option<MenuKind>, events: Vec<Event>) -> SidebarFrame {
        let (viewport, width, pal) = (self.viewport(), self.width, self.pal.clone());
        let mut frame = None;
        let mut output = self.ctx.run_ui(egui::RawInput { screen_rect: Some(self.screen), events, time: Some(self.frame as f64 / 60.0), ..Default::default() }, |ui| {
            let mut list = ui.new_child(egui::UiBuilder::new().max_rect(viewport).layer_id(ui.layer_id()));
            list.set_clip_rect(viewport.intersect(ui.clip_rect()));
            egui::ScrollArea::vertical().id_salt("folder-sidebar-probe").auto_shrink([false, false]).show(&mut list, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let (r, _) = ui.allocate_exact_size(vec2(width, 28.0), Sense::click());
                let header = Rect::from_min_max(pos2(r.left() + PAD, r.top()), pos2(r.left() + width - PAD, r.bottom()));
                let (add, mut opened) = add_button(ui, header_slots(header).close.unwrap(), &pal);
                let (mut row, mut row_focused, mut zone) = (Rect::NOTHING, false, Rect::NOTHING);
                match &kind {
                    Some(kind) => {
                        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click_and_drag());
                        row = Rect::from_min_max(pos2(r.left() + PAD, r.top()), pos2(r.left() + width - PAD, r.bottom()));
                        row_focused = resp.has_focus();
                        opened = opened.or(row_menu(ui, &resp, row, kind.clone()));
                    }
                    None => {
                        let (rect, menu) = empty_pinned_zone(ui, PAD, width - PAD * 2.0, &pal);
                        zone = rect;
                        opened = opened.or(menu);
                    }
                }
                let empty_top = ui.cursor().top();
                opened = opened.or(empty_area(ui, width));
                let empty = Rect::from_min_max(pos2(0.0, empty_top), pos2(width, ui.min_rect().bottom()));
                frame = Some(SidebarFrame { opened, add, header, row, row_focused, empty, zone, texts: Vec::new() });
            });
        });
        output.textures_delta.clear();
        self.frame += 1;
        let mut frame = frame.unwrap();
        frame.texts = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()), shape.clip_rect)),
            _ => None,
        }).collect();
        frame
    }

    fn settle(&mut self, kind: Option<MenuKind>) -> SidebarFrame {
        self.frame(kind.clone(), vec![]);
        self.frame(kind, vec![])
    }

    fn right_click(&mut self, kind: Option<MenuKind>, at: impl Fn(&SidebarFrame) -> Pos2) -> SidebarFrame {
        let first = self.settle(kind.clone());
        let pos = at(&first);
        self.frame(kind.clone(), vec![Event::PointerMoved(pos)]);
        self.frame(kind.clone(), vec![button(pos, PointerButton::Secondary, true)]);
        self.frame(kind, vec![button(pos, PointerButton::Secondary, false)])
    }

    /// Tab to the row, then Shift+F10.
    fn keyboard_row_menu(&mut self, kind: MenuKind) -> SidebarFrame {
        let mut shown = self.settle(Some(kind.clone()));
        for _ in 0..6 {
            if shown.row_focused { break; }
            shown = self.frame(Some(kind.clone()), key(Key::Tab, Modifiers::NONE));
            assert!(shown.opened.is_none());
        }
        assert!(shown.row_focused, "Tab must reach the row");
        self.frame(Some(kind), key(Key::F10, Modifiers::SHIFT))
    }
}

/// Open a row menu by right-click or by keyboard; returns its position and whether it came from the keyboard.
fn open_row_menu(size: Vec2, width: f32, kind: MenuKind, keyboard: bool) -> Pos2 {
    let mut probe = SidebarProbe::new(size, width);
    let shown = if keyboard { probe.keyboard_row_menu(kind.clone()) } else { probe.right_click(Some(kind.clone()), |f| f.row.center()) };
    let opened = shown.opened.unwrap_or_else(|| panic!("{kind:?} menu did not open (keyboard={keyboard})"));
    assert_eq!(format!("{:?}", opened.kind), format!("{kind:?}"));
    assert!(shown.row.expand(1.0).contains(opened.pos) || !keyboard, "keyboard menus open at their row");
    assert_eq!(folders::take_menu_focus(&probe.ctx), keyboard, "only keyboard-opened menus focus their first entry");
    opened.pos
}

fn run(pick: Option<Pick>) -> Action {
    match pick {
        Some(Pick::Run(action)) => action,
        other => panic!("expected an action, got {other:?}"),
    }
}

#[test]
fn add_menu_opens_from_the_plus_keyboard_and_empty_space_and_runs_each_entry() {
    for (size, width) in sizes() {
        // "+" by pointer.
        let mut probe = SidebarProbe::new(size, width);
        let first = probe.settle(Some(MenuKind::Tab("t".into())));
        let plus = first.add.rect.center();
        assert!(first.header.contains_rect(first.add.rect), "the + control stays inside the header row");
        probe.frame(None, vec![Event::PointerMoved(plus)]);
        probe.frame(None, vec![button(plus, PointerButton::Primary, true)]);
        let clicked = probe.frame(None, vec![button(plus, PointerButton::Primary, false)]);
        assert!(matches!(clicked.opened.map(|m| m.kind), Some(MenuKind::Add)));
        assert!(!folders::take_menu_focus(&probe.ctx));

        // "+" by keyboard: Tab to it, then Return.
        let mut probe = SidebarProbe::new(size, width);
        let mut shown = probe.settle(None);
        for _ in 0..6 {
            if shown.add.has_focus() { break; }
            shown = probe.frame(None, key(Key::Tab, Modifiers::NONE));
        }
        assert!(shown.add.has_focus(), "Tab must reach the + control");
        let opened = probe.frame(None, key(Key::Enter, Modifiers::NONE)).opened;
        assert!(matches!(opened.map(|m| m.kind), Some(MenuKind::Add)));
        assert!(folders::take_menu_focus(&probe.ctx));

        // Right-click below the lists.
        let mut probe = SidebarProbe::new(size, width);
        let shown = probe.right_click(Some(MenuKind::Tab("t".into())), |f| f.empty.center());
        assert!(shown.empty.height() >= 24.0 && shown.empty.bottom() <= probe.viewport().bottom() + 0.5, "empty space fills the list without scrolling");
        let menu = shown.opened.expect("right-clicking empty space opens the add menu");
        assert!(matches!(menu.kind, MenuKind::Add));

        for keyboard in [false, true] {
            let mut fx = Fixture::new();
            let tab = fx.tab("focused", None);
            let mut menu_probe = MenuProbe::new(size);
            let pos = menu.pos;
            assert!(matches!(run(menu_probe.choose(&fx.cx(Some(&tab)), &MenuKind::Add, pos, "New Tab", keyboard)), Action::OpenCommandBar { edit_current: false }));
            assert!(matches!(run(menu_probe.choose(&fx.cx(Some(&tab)), &MenuKind::Add, pos, "New Split View", keyboard)), Action::SplitWith(t) if t == tab));
            let action = run(menu_probe.choose(&fx.cx(Some(&tab)), &MenuKind::Add, pos, "New Folder", keyboard));
            let Applied::Rename(folder) = fx.apply(action) else { panic!("New Folder starts renaming") };
            assert_eq!(fx.lib.folders[&folder].parent_id, None);
            assert_eq!(fx.tree(), ["[New Folder]"]);
            assert_eq!(fx.reopened_tree(), fx.tree());
            // Without a tab there is nothing to split.
            let shown = menu_probe.open(&fx.cx(None), &MenuKind::Add, pos, false);
            assert!(shown.has("New Split View"));
            assert!(menu_probe.choose(&fx.cx(None), &MenuKind::Add, pos, "New Split View", false).is_none());
        }
    }
}

#[test]
fn item_menu_entries_work_by_pointer_and_keyboard_and_persist() {
    for (size, width) in sizes() {
        for keyboard in [false, true] {
            let mut probe = MenuProbe::new(size);

            // Duplicate.
            let mut fx = Fixture::new();
            let a = fx.pin(None, "A");
            fx.pin(None, "B");
            let pos = open_row_menu(size, width, MenuKind::Item(a.clone()), keyboard);
            let action = run(probe.choose(&fx.cx(None), &MenuKind::Item(a.clone()), pos, "Duplicate", keyboard));
            assert_eq!(fx.apply(action), Applied::Done);
            assert_eq!(fx.tree(), ["A", "A", "B"]);
            assert_eq!(fx.reopened_tree(), fx.tree());

            // Add to Favorites keeps an open tab attached.
            let mut fx = Fixture::new();
            let a = fx.pin(None, "A");
            fx.pin(None, "B");
            let tab = fx.tab("a-tab", Some(&a));
            let action = run(probe.choose(&fx.cx(None), &MenuKind::Item(a.clone()), pos, "Add to Favorites", keyboard));
            fx.apply(action);
            assert_eq!(fx.tree(), ["B"]);
            assert_eq!(fx.reopened_tree(), ["B"]);
            assert_eq!(fx.lib.items[&a].kind, ItemKind::Favorite);
            let tab = fx.tabs.iter().find(|t| t.id == tab).unwrap();
            assert_eq!((tab.space_id.as_str(), tab.item_id.as_deref(), tab.dirty), (FAVORITES_SPACE, Some(a.as_str()), true));
            assert_eq!(crate::app::tab_profile(&fx.lib, tab), Some(fx.lib.default_profile_id().as_str()));

            // Move to Folder › a nested folder, then back to Top Level.
            let mut fx = Fixture::new();
            let a = fx.pin(None, "A");
            fx.pin(None, "B");
            let work = fx.folder(None, "Work");
            let inner = fx.folder(Some(&work), "Inner");
            let item_menu = MenuKind::Item(a.clone());
            let Some(Pick::Open(picker)) = probe.choose(&fx.cx(None), &item_menu, pos, "Move to Folder", keyboard) else { panic!("Move to Folder opens the folder list") };
            assert!(matches!(&picker, MenuKind::Folders(FolderTarget::Item(id)) if *id == a));
            let shown = probe.open(&fx.cx(None), &picker, pos, keyboard);
            assert!(shown.has("Move to Folder") && shown.has("Work") && shown.has("Inner") && !shown.has("Top Level"));
            let (work_x, inner_x) = (shown.point("Work").x, shown.point("Inner").x);
            assert!(inner_x > work_x, "nested folders are indented");
            let action = run(probe.choose(&fx.cx(None), &picker, pos, "Inner", keyboard));
            assert!(matches!(&action, Action::MovePinned { node, folder: Some(f), .. } if *node == a && *f == inner));
            fx.apply(action);
            assert_eq!(fx.tree(), ["B", "[Work]", "  [Inner]", "    A"]);
            let Some(Pick::Open(picker)) = probe.choose(&fx.cx(None), &item_menu, pos, "Move to Folder", keyboard) else { panic!() };
            let action = run(probe.choose(&fx.cx(None), &picker, pos, "Top Level", keyboard));
            fx.apply(action);
            assert_eq!(fx.tree(), ["B", "[Work]", "  [Inner]", "A"]);
            assert_eq!(fx.reopened_tree(), fx.tree());

            // New Folder with This Tab.
            let mut fx = Fixture::new();
            fx.pin(None, "A");
            let b = fx.pin(None, "B");
            let action = run(probe.choose(&fx.cx(None), &MenuKind::Item(b.clone()), pos, "New Folder with This Tab", keyboard));
            let Applied::Rename(folder) = fx.apply(action) else { panic!("the new folder starts renaming") };
            assert_eq!(fx.lib.items[&b].folder_id.as_deref(), Some(folder.as_str()));
            assert_eq!(fx.tree(), ["A", "[New Folder]", "  B"]);
            assert_eq!(fx.reopened_tree(), fx.tree());

            // Favorites keep their own entries.
            let profile = fx.lib.default_profile_id();
            let favorite = fx.lib.add_favorite(&profile, "https://f.example/", "F").unwrap();
            let shown = probe.open(&fx.cx(None), &MenuKind::Item(favorite.clone()), pos, false);
            assert!(shown.has("Remove from Favorites") && shown.has("Duplicate"));
            assert!(!shown.has("Add to Favorites") && !shown.has("Move to Folder") && !shown.has("New Folder with This Tab"));
        }
    }
}

#[test]
fn folder_menu_entries_work_by_pointer_and_keyboard_and_persist() {
    for (size, width) in sizes() {
        for keyboard in [false, true] {
            let mut probe = MenuProbe::new(size);
            let fixture = || {
                let mut fx = Fixture::new();
                let work = fx.folder(None, "Work");
                let a = fx.pin(Some(&work), "A");
                let b = fx.pin(Some(&work), "B");
                let inner = fx.folder(Some(&work), "Inner");
                fx.pin(Some(&inner), "C");
                fx.tab("b-tab", Some(&b));
                (fx, work, a)
            };

            let (mut fx, work, _) = fixture();
            let pos = open_row_menu(size, width, MenuKind::Folder(work.clone()), keyboard);
            let menu = MenuKind::Folder(work.clone());
            let shown = probe.open(&fx.cx(None), &menu, pos, false);
            assert!(shown.has("Delete Folder") && shown.has("Contents move up one level"), "Delete Folder says what happens to its contents");

            // New Folder Inside expands a collapsed parent and starts renaming.
            fx.lib.update_folder(&work, |f| f.collapsed = true).unwrap();
            let action = run(probe.choose(&fx.cx(None), &menu, pos, "New Folder Inside", keyboard));
            let Applied::Rename(child) = fx.apply(action) else { panic!("New Folder Inside starts renaming") };
            assert_eq!(fx.lib.folders[&child].parent_id.as_deref(), Some(work.as_str()));
            assert_eq!(fx.tree(), ["[Work]", "  A", "  B", "  [Inner]", "    C", "  [New Folder]"]);
            assert_eq!(fx.reopened_tree(), fx.tree());

            // Collapse / Expand.
            let action = run(probe.choose(&fx.cx(None), &menu, pos, "Collapse", keyboard));
            fx.apply(action);
            assert!(fx.lib.folders[&work].collapsed);
            assert!(probe.open(&fx.cx(None), &menu, pos, false).has("Expand"));
            let action = run(probe.choose(&fx.cx(None), &menu, pos, "Expand", keyboard));
            fx.apply(action);
            assert_eq!(fx.reopened_tree()[0], "[Work]");

            // Open All loads only unopened items of the folder, including nested ones.
            let action = run(probe.choose(&fx.cx(None), &menu, pos, "Open All", keyboard));
            let Applied::Opened(opened) = fx.apply(action) else { panic!("Open All reports its tabs") };
            assert_eq!(opened.len(), 2, "A and C open; B was already open");
            let mut items: Vec<_> = fx.tabs.iter().filter_map(|t| t.item_id.clone()).collect();
            items.sort();
            items.dedup();
            assert_eq!(items.len(), fx.tabs.len(), "no duplicate tabs");
            assert!(probe.choose(&fx.cx(None), &menu, pos, "Open All", false).is_none(), "nothing left to open");

            // New Tab Inside opens search; its result is pinned in the folder.
            let action = run(probe.choose(&fx.cx(None), &menu, pos, "New Tab Inside", keyboard));
            assert_eq!(fx.apply(action), Applied::Search(work.clone()));
            let tab = fx.tab("searched", None);
            let index = fx.tabs.iter().position(|t| t.id == tab).unwrap();
            folders::pin_into_folder(&mut fx.lib, &mut fx.tabs[index], &work).unwrap();
            assert_eq!(fx.tree().last().unwrap(), "  searched");
            assert_eq!(fx.reopened_tree(), fx.tree());

            // Delete Folder moves its contents up a level.
            let action = run(probe.choose(&fx.cx(None), &menu, pos, "Delete Folder", keyboard));
            fx.apply(action);
            assert_eq!(fx.tree(), ["A", "B", "[Inner]", "  C", "[New Folder]", "searched"]);
            assert_eq!(fx.reopened_tree(), fx.tree());

            // A search finishing after its folder was deleted leaves the tab unpinned.
            let late = fx.tab("late", None);
            let index = fx.tabs.iter().position(|t| t.id == late).unwrap();
            assert!(folders::pin_into_folder(&mut fx.lib, &mut fx.tabs[index], &work).is_err());
            assert_eq!(fx.tabs[index].item_id, None);
        }
    }
}

#[test]
fn today_tab_pins_into_a_folder_by_pointer_and_keyboard() {
    for (size, width) in sizes() {
        for keyboard in [false, true] {
            let mut probe = MenuProbe::new(size);
            let mut fx = Fixture::new();
            let tab = fx.tab("today", None);
            let pos = open_row_menu(size, width, MenuKind::Tab(tab.clone()), keyboard);
            assert!(!probe.open(&fx.cx(None), &MenuKind::Tab(tab.clone()), pos, false).has("Pin to Folder"), "no folders, no picker");

            let work = fx.folder(None, "Work");
            let inner = fx.folder(Some(&work), "Inner");
            let Some(Pick::Open(picker)) = probe.choose(&fx.cx(None), &MenuKind::Tab(tab.clone()), pos, "Pin to Folder", keyboard) else { panic!("Pin to Folder opens the folder list") };
            let shown = probe.open(&fx.cx(None), &picker, pos, keyboard);
            assert!(shown.has("Pin to Folder") && !shown.has("Top Level"));
            let action = run(probe.choose(&fx.cx(None), &picker, pos, "Inner", keyboard));
            assert!(matches!(&action, Action::Folder(FolderOp::PinTab { tab: t, folder }) if *t == tab && *folder == inner));
            fx.apply(action);
            assert!(fx.tabs[0].item_id.is_some() && fx.tabs[0].dirty);
            assert_eq!(fx.tree(), ["[Work]", "  [Inner]", "    today"]);
            assert_eq!(fx.reopened_tree(), fx.tree());
        }
    }
}

#[test]
fn menus_fit_compact_and_normal_windows_and_long_folder_lists_scroll() {
    for (size, width) in sizes() {
        let mut fx = Fixture::new();
        let mut parent = None;
        let mut folders = Vec::new();
        for i in 0..40 {
            let id = fx.folder(parent.as_deref(), &format!("Folder {i}"));
            if i % 5 == 0 { parent = Some(id.clone()); }
            folders.push(id);
        }
        let pin = fx.pin(Some(&folders[3]), "Pinned");
        let tab = fx.tab("pinned-tab", Some(&pin));
        fx.tabs[0].url = "https://drifted.example/".into();
        let today = fx.tab("today", None);
        let profile = fx.lib.default_profile_id();
        let favorite = fx.lib.add_favorite(&profile, "https://f.example/", "F").unwrap();
        // Opened from a row near the bottom of the sidebar, as close to the corner as possible.
        let pos = pos2(width - 20.0, size.y - 60.0);
        let menus = [
            MenuKind::Add, MenuKind::Item(pin.clone()), MenuKind::Item(favorite), MenuKind::Folder(folders[0].clone()),
            MenuKind::Folders(FolderTarget::Item(pin.clone())), MenuKind::Folders(FolderTarget::Tab(today.clone())), MenuKind::Tab(today),
        ];
        let mut probe = MenuProbe::new(size);
        for menu in &menus {
            let shown = probe.open(&fx.cx(Some(&tab)), menu, pos, false);
            assert!(probe.screen.contains_rect(shown.area), "{menu:?} at {size:?}: {:?} leaves the window", shown.area);
            assert!(!shown.texts.is_empty());
            for (text, rect, clip) in &shown.texts {
                assert!(shown.area.contains(rect.center()) || !clip.contains(rect.center()), "{text:?} draws outside {menu:?}");
            }
        }
        // Every entry of the long list is reachable from the keyboard.
        let picker = MenuKind::Folders(FolderTarget::Item(pin.clone()));
        let action = run(probe.choose(&fx.cx(None), &picker, pos, "Folder 39", true));
        assert!(matches!(action, Action::MovePinned { folder: Some(f), .. } if f == folders[39]));
    }
}

#[test]
fn escape_leaves_a_keyboard_opened_menu_without_choosing() {
    let mut fx = Fixture::new();
    fx.folder(None, "Work");
    let tab = fx.tab("t", None);
    for kind in [MenuKind::Add, MenuKind::Folders(FolderTarget::Tab(tab.clone()))] {
        let mut probe = MenuProbe::new(vec2(640.0, 420.0));
        let pos = pos2(40.0, 120.0);
        assert!(!probe.open(&fx.cx(None), &kind, pos, true).lit.is_empty());
        assert!(probe.show(&fx.cx(None), &kind, pos, false, key(Key::Escape, Modifiers::NONE)).pick.is_none());
        // The menu is not an egui popup, so the app's Escape handler gets the key and closes it.
        assert!(!egui::Popup::is_any_open(&probe.ctx));
        assert!(probe.show(&fx.cx(None), &kind, pos, false, vec![]).lit.is_empty(), "Escape drops the highlight");
        assert!(probe.show(&fx.cx(None), &kind, pos, false, key(Key::Enter, Modifiers::NONE)).pick.is_none(), "Return after Escape chooses nothing");
    }
}

#[test]
fn empty_pinned_zone_accepts_tab_drops_and_opens_the_add_menu() {
    for (size, width) in sizes() {
        let mut probe = SidebarProbe::new(size, width);
        let shown = probe.settle(None);
        let viewport = probe.viewport();
        let (copy, text, clip) = shown.texts.iter().find(|(text, _, _)| text == EMPTY_PINNED || text == EMPTY_PINNED_SHORT).expect("the empty zone is labelled");
        // The full sentence at the normal width; the narrow sidebar falls back to the short copy instead of cutting it.
        assert_eq!(copy, if width >= 252.0 { EMPTY_PINNED } else { EMPTY_PINNED_SHORT }, "label at width {width}");
        assert!(viewport.contains_rect(shown.zone) && clip.contains_rect(*text), "the zone and its label are fully visible");
        assert_eq!(shown.zone.height(), ROW_H);
        assert!(text.right() <= shown.zone.right(), "the label fits the row");

        let rows = [DropRow { section: Section::Pinned(None), index: 0, rect: shown.zone, clip_rect: viewport, source: None, folder: None },
            DropRow { section: Section::Today, index: 0, rect: shown.zone.translate(vec2(0.0, 100.0)), clip_rect: viewport, source: Some(DragKind::Tab("t".into())), folder: None }];
        let sidebar = Rect::from_min_size(Pos2::ZERO, vec2(width, size.y));
        let drop = |kind: &DragKind| drop_target(kind, &rows, shown.zone.center(), sidebar).and_then(|target| drop_action(kind, &rows, &target));
        assert!(matches!(drop(&DragKind::Tab("t".into())), Some(Action::TabToPinned { tab, folder: None, index: 0 }) if tab == "t"));
        assert!(drop(&DragKind::Favorite("f".into())).is_none());

        let opened = probe.right_click(None, |f| f.zone.center()).opened.expect("right-click opens the add menu");
        assert!(matches!(opened.kind, MenuKind::Add));
    }
}

#[test]
fn header_controls_share_the_tab_rows_trailing_slots() {
    for width in [200.0, 252.0, 420.0] {
        for offset in [0.0, 24.0] {
            let header = Rect::from_min_size(pos2(PAD + offset, 140.0), vec2(width - PAD * 2.0, 28.0));
            let tab_row = Rect::from_min_size(pos2(PAD + offset, 200.0), vec2(width - PAD * 2.0, ROW_H));
            let (head, tab) = (header_slots(header), TabTrailingSlots::new(tab_row, true, true, false));
            let (add, more) = (head.close.unwrap(), head.mute.unwrap());
            assert_eq!(add.center().x, tab.close.unwrap().center().x, "+ aligns with the close column");
            assert_eq!(more.center().x, tab.mute.unwrap().center().x, "… aligns with the mute column");
            assert_eq!((add.size(), more.size()), (tab.close.unwrap().size(), tab.mute.unwrap().size()));
            assert!(header.contains_rect(add) && header.contains_rect(more) && more.right() < add.left());
            assert_eq!(add.center().y, header.center().y);
            assert!(head.title_right <= more.left());
        }
    }
}



#[test]
fn menu_hints_follow_rebound_and_unset_shortcuts() {
    use crate::shortcuts::{Chord, Command, Proposal, Slot, index_of};
    let label = |key: Key, modifiers: Modifiers| Chord::from_event(key, modifiers).label();
    let store = Store::open_in_memory().unwrap();
    for (size, _) in sizes() {
        let mut fx = Fixture::new();
        let tab = fx.tab("focused", None);
        let item = fx.pin(None, "Pinned");
        fx.tab("pinned", Some(&item));
        let folder = fx.folder(None, "Folder");
        let mut probe = MenuProbe::new(size);
        let pos = pos2(40.0, 40.0);
        let shown = probe.open(&fx.cx(Some(&tab)), &MenuKind::Add, pos, false);
        assert!(shown.has(&label(Key::T, Modifiers::COMMAND)) && shown.has(&label(Key::N, Modifiers::COMMAND | Modifiers::ALT)), "{:?}", shown.labels());

        for (command, chord) in [(Command::NewFolder, Some(Chord::from_event(Key::J, Modifiers::COMMAND))), (Command::NewTab, None), (Command::PinTab, Some(Chord::from_event(Key::B, Modifiers::COMMAND)))] {
            let Proposal::Ready(candidate) = fx.shortcuts.propose(index_of(command), Slot::Primary, chord) else { panic!("{command:?} is free to change") };
            fx.shortcuts.save(&store, candidate).unwrap();
        }
        let shown = probe.open(&fx.cx(Some(&tab)), &MenuKind::Add, pos, false);
        assert!(shown.has(&label(Key::J, Modifiers::COMMAND)), "New Folder shows its new chord: {:?}", shown.labels());
        assert!(!shown.has(&label(Key::N, Modifiers::COMMAND | Modifiers::ALT)) && !shown.has(&label(Key::T, Modifiers::COMMAND)), "{:?}", shown.labels());
        let shown = probe.open(&fx.cx(Some(&tab)), &MenuKind::Folder(folder.clone()), pos, false);
        assert!(shown.has(&label(Key::J, Modifiers::COMMAND)), "{:?}", shown.labels());
        let shown = probe.open(&fx.cx(Some(&tab)), &MenuKind::Item(item.clone()), pos, false);
        assert!(shown.has(&label(Key::B, Modifiers::COMMAND)) && !shown.has(&label(Key::D, Modifiers::COMMAND)), "Unpin follows Pin or Unpin Tab: {:?}", shown.labels());
    }
}

/// One pointer gesture over the sidebar: `kind` pressed at `path[0]` when
/// `press`, moved along the path, then let go, after `cancel` if given. The
/// action the release asks for, as `handle_drag` resolves it.
fn drag(kind: &DragKind, rows: &[DropRow], sidebar: Rect, press: bool, path: &[Pos2], cancel: Option<Event>) -> Option<Action> {
    let ctx = egui::Context::default();
    let mut state = None;
    let mut action = None;
    for (frame, pos) in std::iter::once(path[0]).chain(path.iter().copied()).enumerate() {
        let mut events = vec![Event::PointerMoved(pos)];
        if press && frame == 1 {
            state = Some(DragState { kind: kind.clone(), start: pos, active: false });
            events.push(button(pos, PointerButton::Primary, true));
        } else if press && frame == path.len() {
            events.extend(cancel.clone());
            events.push(button(pos, PointerButton::Primary, false));
        }
        ctx.run_ui(egui::RawInput { events, time: Some(frame as f64 / 60.0), ..Default::default() }, |ui| {
            if let Some((kind, pos, false)) = ui.input(|input| drag_pointer(&mut state, input)) {
                action = drop_target(&kind, rows, pos, sidebar).and_then(|target| drop_action(&kind, rows, &target));
            }
        }).textures_delta.clear();
    }
    action
}

fn pinned_rows(fx: &Fixture, sidebar: Rect) -> Vec<DropRow> {
    fx.lib.pinned_children(&fx.space, None).iter().enumerate().map(|(index, node)| {
        let rect = Rect::from_min_size(pos2(8.0, 120.0 + 32.0 * index as f32), vec2(sidebar.width() - 16.0, 30.0));
        let folder = matches!(node, PinnedNode::Folder(..)).then(|| node.id().to_string());
        DropRow { section: Section::Pinned(None), index, rect, clip_rect: sidebar, source: Some(DragKind::Pinned(node.id().into())), folder }
    }).collect()
}

fn favorite_rows(fx: &Fixture, profile: &str, sidebar: Rect) -> Vec<DropRow> {
    fx.lib.favorites(profile).iter().enumerate()
        .map(|(index, (id, _))| DropRow::favorite(index, id, Rect::from_min_size(pos2(8.0 + 60.0 * index as f32, 40.0), vec2(56.0, 56.0)), sidebar))
        .collect()
}

/// As the app's action loop: run the move, then tap if the sidebar changed.
fn land(fx: &mut Fixture, feel: &mut crate::feel::Feel, profile: &str, action: Option<Action>) {
    let Some(action) = action else { return };
    let before = crate::feel::SidebarOrder::of(&fx.lib, &fx.space, profile, []);
    match action {
        Action::MovePinned { node, folder, index } => fx.lib.move_pinned(&node, &fx.space, folder.as_deref(), index).unwrap(),
        Action::ReorderFavorite { item, index } => fx.lib.reorder_favorite(&item, index).unwrap(),
        other => panic!("unexpected {other:?}"),
    }
    feel.dropped(&before, &crate::feel::SidebarOrder::of(&fx.lib, &fx.space, profile, []));
}

#[test]
fn a_drop_taps_once_when_it_reorders_and_hover_rejection_or_cancel_never_tap() {
    for (size, width) in sizes() {
        let mut fx = Fixture::new();
        let store = Store::open(&fx.dir.join("library.sqlite")).unwrap();
        let (mut feel, taps) = crate::feel::tests::feel(&store);
        let profile = fx.lib.spaces[&fx.space].profile_id.clone();
        let pinned = [fx.pin(None, "A"), fx.pin(None, "B"), fx.folder(None, "F")];
        let favorites = ["X", "Y"].map(|title| fx.lib.add_favorite(&profile, &format!("https://{title}.example/"), title).unwrap());
        let sidebar = Rect::from_min_size(Pos2::ZERO, vec2(width, size.y));
        let rows = pinned_rows(&fx, sidebar);
        let (a, b, f) = (rows[0].rect.center(), rows[1].rect.center(), rows[2].rect.center());
        let kind = |index: usize| DragKind::Pinned(pinned[index].clone());

        // Hovering, or a click that never moves, is no drop.
        assert!(drag(&kind(0), &rows, sidebar, false, &[a, b, b + vec2(0.0, 12.0)], None).is_none());
        assert!(drag(&kind(0), &rows, sidebar, true, &[a, a], None).is_none());
        // Let go outside the sidebar, or Escape first: nothing moves.
        assert!(drag(&kind(0), &rows, sidebar, true, &[a, b, pos2(width + 40.0, b.y)], None).is_none());
        let escape = Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
        assert!(drag(&kind(0), &rows, sidebar, true, &[a, b, b + vec2(0.0, 12.0)], Some(escape)).is_none());
        assert_eq!(taps.taps(), 0, "hover, rejected and cancelled drops never tap at {size:?}");
        // Back on its own place: accepted, but nothing moved.
        let in_place = drag(&kind(1), &rows, sidebar, true, &[b, b - vec2(0.0, 10.0), b - vec2(0.0, 8.0)], None);
        assert!(in_place.is_some());
        land(&mut fx, &mut feel, &profile, in_place);
        assert_eq!(taps.taps(), 0, "a drop back in place does not tap");

        // A below B: one tap, and the order is saved.
        let moved = drag(&kind(0), &rows, sidebar, true, &[a, b, b + vec2(0.0, 12.0)], None);
        land(&mut fx, &mut feel, &profile, moved);
        assert_eq!(taps.taps(), 1, "one tap per accepted reorder at {size:?}");
        assert_eq!(fx.reopened_tree()[..2], ["B".to_string(), "A".to_string()]);
        // The folder to the top.
        let rows = pinned_rows(&fx, sidebar);
        let (top, folder_row) = (rows[0].rect.center(), rows[2].rect.center());
        assert_eq!(folder_row, f);
        let to_top = drag(&kind(2), &rows, sidebar, true, &[folder_row, top, top - vec2(0.0, 12.0)], None);
        land(&mut fx, &mut feel, &profile, to_top);
        assert_eq!(taps.taps(), 1, "reordering a folder taps");
        assert_eq!(fx.reopened_tree()[0], "[F]");
        // A favorite after the other.
        let rows = favorite_rows(&fx, &profile, sidebar);
        let (first, second) = (rows[0].rect, rows[1].rect);
        let favorite = drag(&DragKind::Favorite(favorites[0].clone()), &rows, sidebar, true, &[first.center(), second.center(), second.right_center() - vec2(4.0, 0.0)], None);
        land(&mut fx, &mut feel, &profile, favorite);
        assert_eq!(taps.taps(), 1, "reordering favorites taps");

        // Off: the same kind of reorder still happens, without a tap.
        feel.prefs.haptics.choose(&store, false);
        let rows = favorite_rows(&fx, &profile, sidebar);
        let back = drag(&DragKind::Favorite(favorites[0].clone()), &rows, sidebar, true, &[second.center(), first.center(), first.left_center() + vec2(4.0, 0.0)], None);
        assert!(back.is_some());
        land(&mut fx, &mut feel, &profile, back);
        assert_eq!(taps.taps(), 0, "off at {size:?}");
        assert_eq!(fx.lib.favorites(&profile)[0].0, favorites[0], "the move itself still happens");
    }
}

#[test]
fn tidy_title_shows_only_once_turned_on_explains_when_it_cant_run_and_restore_page_title_clears_a_custom_title() {
    for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
        let mut w = crate::tidy_tabs::tests::World::new();
        let space = w.space.clone();
        let item = w.lib.add_pinned(&space, None, "https://docs.example/a", "Docs").unwrap();
        let shortcuts = crate::shortcuts::Shortcuts::defaults();
        let kind = MenuKind::Item(item.clone());
        let pos = pos2(40.0, 60.0);
        fn cx<'a>(w: &'a crate::tidy_tabs::tests::World, shortcuts: &'a crate::shortcuts::Shortcuts) -> MenuCx<'a> {
            MenuCx { lib: &w.lib, tabs: &w.tabs, space: &w.space, focused_tab: None, shortcuts, tidy_tabs: Some(&w.tidy) }
        }

        let shown = MenuProbe::new(size).open(&cx(&w, &shortcuts), &kind, pos, false);
        assert!(!shown.has("Tidy Title") && !shown.has("Restore Page Title"), "off: no entries at {size:?}; shown {:?}", shown.labels());

        w.turn_on(crate::tidy_tabs::TITLES);
        for keyboard in [false, true] {
            let action = run(MenuProbe::new(size).choose(&cx(&w, &shortcuts), &kind, pos, "Tidy Title", keyboard));
            assert!(matches!(&action, Action::TidyTitle(id) if *id == item), "{action:?}");
        }

        w.lib.update_item(&item, |item| item.custom_title = Some("Mine".into())).unwrap();
        let why = "You renamed this tab, so its title stays.";
        let shown = MenuProbe::new(size).open(&cx(&w, &shortcuts), &kind, pos, false);
        let (_, note, clip) = shown.texts.iter().find(|(text, ..)| text == why).expect("the reason is shown");
        assert!(clip.contains_rect(*note) && shown.area.contains_rect(*note), "the reason fits the menu at {size:?}");
        let (_, entry, _) = shown.texts.iter().find(|(text, ..)| text == "Tidy Title").unwrap();
        assert!(entry.bottom() <= note.top(), "the reason sits under its entry");
        let mut probe = MenuProbe::new(size);
        let shown = probe.open(&cx(&w, &shortcuts), &kind, pos, false);
        let point = shown.point("Tidy Title");
        for pressed in [true, false] {
            assert!(probe.show(&cx(&w, &shortcuts), &kind, pos, false, vec![Event::PointerMoved(point), button(point, PointerButton::Primary, pressed)]).pick.is_none(), "a disabled Tidy Title does nothing");
        }
        let action = run(MenuProbe::new(size).choose(&cx(&w, &shortcuts), &kind, pos, "Restore Page Title", false));
        assert!(matches!(&action, Action::RestorePageTitle(id) if *id == item), "{action:?}");
    }
}
