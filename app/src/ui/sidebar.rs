//! The sidebar: navigation row, URL pill, favorites, the space's pinned tree,
//! today's tabs and the space switcher. Rows can be dragged to reorder, to pin
//! (drop above the divider) or to unpin (drop below it).

use crate::app::{Action, CrossbowApp, DragKind, DragState, MenuKind, MenuState, RenameTarget};
use crate::tabs::Tab;
use crate::ui::hover_preview::Subject;
use crate::ui::icons::{self, Icon};
use crate::ui::style::Ink;
use crate::ui::theme::{self, Palette};
use crate::ui::{anim, window};
use crate::unread::{Badge, Unread};
use crossbow_core::library::{Library, PinnedNode};
use egui::{Align2, Color32, FontId, Id, Pos2, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};
use std::sync::Arc;

const ROW_H: f32 = 32.0;
const PAD: f32 = 10.0;
const ITEM_ICON_X: f32 = 14.0;
const ITEM_TEXT_X: f32 = 30.0;
const FAVORITE_COLUMNS: usize = 4;
const FAVORITE_HEIGHT: f32 = 44.0;
const FAVORITE_GAP: f32 = 6.0;
const SECTION_GAP: f32 = 14.0;
const TRAILING_CONTROL_SIZE: f32 = 22.0;
const TRAILING_CONTROL_GAP: f32 = 2.0;
const TRAILING_INSET: f32 = 5.0;

struct TabTrailingSlots {
    close: Option<Rect>,
    mute: Option<Rect>,
    split: Option<Rect>,
    title_right: f32,
}

impl TabTrailingSlots {
    fn new(row: Rect, show_close: bool, muted: bool, in_split: bool) -> Self {
        let mut right = row.right() - TRAILING_INSET;
        let mut slot = |visible: bool| {
            visible.then(|| {
                let rect = Rect::from_center_size(pos2(right - TRAILING_CONTROL_SIZE / 2.0, row.center().y), vec2(TRAILING_CONTROL_SIZE, TRAILING_CONTROL_SIZE));
                right = rect.left() - TRAILING_CONTROL_GAP;
                rect
            })
        };
        let close = slot(show_close);
        let mute = slot(muted);
        let split = slot(in_split);
        Self { close, mute, split, title_right: right }
    }

    fn show(&self, ui: &mut Ui, tab: &str, pinned: bool, text: Color32, pal: &Palette) -> Option<Action> {
        let mut action = None;
        if let Some(rect) = self.split {
            icons::draw(ui.painter(), Rect::from_center_size(rect.center(), vec2(12.0, 12.0)), Icon::Split, pal.text_faint);
        }
        if let Some(rect) = self.mute {
            if icons::button(ui, rect, Icon::SpeakerMuted, text, pal.hover, true).clicked() {
                action = Some(Action::ToggleMute(tab.to_string()));
            }
        }
        if let Some(rect) = self.close {
            if icons::button(ui, rect, Icon::Close, text.gamma_multiply(0.8), pal.hover, true)
                .on_hover_text(if pinned { "Unload" } else { "Close tab" }).clicked() {
                return Some(Action::CloseTab(tab.to_string()));
            }
        }
        action
    }
}

struct TabRowInteraction {
    rect: Rect,
    response: egui::Response,
}

impl TabRowInteraction {
    fn new(ui: &mut Ui, pad: f32, width: f32) -> Self {
        let (r, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click_and_drag());
        let rect = Rect::from_min_max(pos2(r.left() + pad, r.top()), pos2(r.left() + pad + width, r.bottom()));
        Self { rect, response }
    }

    fn trailing(&self, active: bool, muted: bool, in_split: bool) -> TabTrailingSlots {
        // A child click unhovers the row on its release frame.
        TabTrailingSlots::new(self.rect, self.response.container_hovered() || active, muted, in_split)
    }

    fn dispatch_row_actions(&self, tab: &str, actions: &mut Vec<Action>) {
        if self.response.clicked() {
            actions.push(Action::ActivateTab(tab.to_string()));
        }
        if self.response.middle_clicked() {
            actions.push(Action::CloseTab(tab.to_string()));
        }
    }
}

pub(super) struct SidebarHeader {
    pub(super) rect: Rect,
    pub(super) row: window::ControlRow,
}

impl SidebarHeader {
    pub(super) fn new(rect: Rect, native_controls: bool) -> Self {
        let top = Rect::from_min_size(rect.min, vec2(rect.width(), window::SIDEBAR_HEADER));
        Self { rect: top, row: window::ControlRow::new(rect, native_controls, window::SIDEBAR_HEADER) }
    }
}

struct SidebarRegions {
    favorites: Option<Rect>,
    tabs: Rect,
}

impl SidebarRegions {
    fn new(body: Rect, favorites: usize) -> Self {
        if favorites == 0 {
            return Self { favorites: None, tabs: body };
        }
        // Favorites keep their place without pushing the space and tabs off-screen.
        let budget = (body.height() * 0.35).max(FAVORITE_HEIGHT).min(body.height());
        let rows = ((budget + FAVORITE_GAP) / (FAVORITE_HEIGHT + FAVORITE_GAP)).floor() as usize;
        let rows = favorites.div_ceil(FAVORITE_COLUMNS).min(rows.max(1)).min(2);
        let height = favorite_grid_height(rows).min(body.height());
        let favorites = Rect::from_min_size(body.min + vec2(PAD, 0.0), vec2(body.width() - PAD * 2.0, height));
        let tabs = body.with_min_y((favorites.bottom() + SECTION_GAP).min(body.bottom()));
        Self { favorites: Some(favorites), tabs }
    }
}

fn favorite_grid_height(rows: usize) -> f32 {
    rows as f32 * FAVORITE_HEIGHT + rows.saturating_sub(1) as f32 * FAVORITE_GAP
}

fn favorite_grid(ui: &mut Ui, rect: Rect, id: Id, count: usize, mut tile: impl FnMut(&mut Ui, usize, Rect)) -> egui::scroll_area::ScrollAreaOutput<()> {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::vertical().id_salt(id).min_scrolled_height(0.0).content_margin(egui::Margin::ZERO).auto_shrink([false, false]).show(&mut child, |ui| {
        let (grid, _) = ui.allocate_exact_size(vec2(ui.available_width(), favorite_grid_height(count.div_ceil(FAVORITE_COLUMNS))), Sense::hover());
        let width = (grid.width() - FAVORITE_GAP * (FAVORITE_COLUMNS - 1) as f32) / FAVORITE_COLUMNS as f32;
        for index in 0..count {
            let origin = grid.min + vec2((index % FAVORITE_COLUMNS) as f32 * (width + FAVORITE_GAP), (index / FAVORITE_COLUMNS) as f32 * (FAVORITE_HEIGHT + FAVORITE_GAP));
            tile(ui, index, Rect::from_min_size(origin, vec2(width, FAVORITE_HEIGHT)));
        }
    })
}

fn favorite_response(ui: &mut Ui, rect: Rect, id: Id, title: &str) -> egui::Response {
    let response = ui.interact(rect, id, Sense::click_and_drag());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, title));
    if response.gained_focus() {
        ui.scroll_to_rect_animation(rect, None, egui::style::ScrollAnimation::none());
    }
    response
}

#[derive(Clone, Debug, PartialEq)]
enum Section {
    Favorites,
    Pinned(Option<String>),
    Today,
}

struct DropRow {
    section: Section,
    index: usize,
    rect: Rect,
    /// The scroll viewport's clip, not just the unscrolled row geometry.
    clip_rect: Rect,
    /// Keep even clipped source rows so scrolling during a drag preserves its index.
    source: Option<DragKind>,
    /// Folder rows accept drops *into* them.
    folder: Option<String>,
}

impl DropRow {
    fn favorite(index: usize, id: &str, rect: Rect, clip_rect: Rect) -> Self {
        Self { section: Section::Favorites, index, rect, clip_rect, source: Some(DragKind::Favorite(id.to_string())), folder: None }
    }
}

struct DropTarget<'a> {
    row: &'a DropRow,
    visible: Rect,
    after: bool,
    into_folder: bool,
}

fn drop_target<'a>(kind: &DragKind, drops: &'a [DropRow], pos: Pos2, sidebar: Rect) -> Option<DropTarget<'a>> {
    drops.iter().find_map(|row| {
        // Favorites reorder only inside their own grid; other rows cannot be
        // implicitly converted to favorites by dropping on a tile.
        if matches!(kind, DragKind::Favorite(_)) != (row.section == Section::Favorites) {
            return None;
        }
        let visible = row.rect.intersect(row.clip_rect).intersect(sidebar);
        if !visible.is_positive() || !visible.contains(pos) {
            return None;
        }
        Some(DropTarget {
            row,
            visible,
            after: if row.section == Section::Favorites { pos.x >= row.rect.center().x } else { pos.y >= row.rect.center().y },
            into_folder: row.folder.is_some() && (pos.y - row.rect.center().y).abs() < row.rect.height() * 0.25,
        })
    })
}

/// Actions/core accept an index after removing the source, but painted slots
/// are in the list before removal. Cross-list insertions and append stay intact.
fn insertion_index(slot: usize, source: Option<usize>) -> usize {
    if slot == usize::MAX { slot } else { slot - usize::from(source.is_some_and(|index| index < slot)) }
}

fn drop_action(kind: &DragKind, drops: &[DropRow], target: &DropTarget<'_>) -> Option<Action> {
    // A space/profile change or a removed source cancels the old gesture.
    let source = drops.iter().find(|row| row.source.as_ref() == Some(kind))?;
    let row = target.row;
    let slot = if row.source.is_some() { row.index.saturating_add(usize::from(target.after)) } else { row.index };
    let index = insertion_index(slot, (source.section == row.section).then_some(source.index));
    if target.into_folder {
        let folder = row.folder.clone()?;
        return match kind {
            DragKind::Tab(tab) => Some(Action::TabToPinned { tab: tab.clone(), folder: Some(folder), index: usize::MAX }),
            DragKind::Pinned(node) if *node != folder => Some(Action::MovePinned { node: node.clone(), folder: Some(folder), index: usize::MAX }),
            _ => None,
        };
    }
    match (kind, &row.section) {
        (DragKind::Favorite(item), Section::Favorites) => Some(Action::ReorderFavorite { item: item.clone(), index }),
        (DragKind::Tab(tab), Section::Today) => Some(Action::MoveTab { tab: tab.clone(), index }),
        (DragKind::Tab(tab), Section::Pinned(folder)) => Some(Action::TabToPinned { tab: tab.clone(), folder: folder.clone(), index }),
        (DragKind::Pinned(node), Section::Pinned(folder)) => Some(Action::MovePinned { node: node.clone(), folder: folder.clone(), index }),
        (DragKind::Pinned(item), Section::Today) if source.folder.is_none() => Some(Action::ItemToToday { item: item.clone(), index }),
        _ => None,
    }
}

const EMPTY_PINNED: &str = "Drag tabs here to keep them";
const EMPTY_PINNED_SHORT: &str = "Drag tabs here";

/// The Space header's "+" and "…" take the tab rows' close and mute slots.
fn header_slots(row: Rect) -> TabTrailingSlots {
    TabTrailingSlots::new(row, true, true, false)
}

/// The "+" control; opens the add menu.
fn add_button(ui: &mut Ui, rect: Rect, pal: &Palette) -> (egui::Response, Option<MenuState>) {
    let resp = icons::button(ui, rect, Icon::Plus, pal.text_dim, pal.hover, true).on_hover_text("New tab, folder or split view");
    let opened = resp.clicked().then(|| {
        let keyboard = resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space));
        crate::ui::folders::open_menu(ui.ctx(), rect.left_bottom() + vec2(0.0, 2.0), MenuKind::Add, keyboard)
    });
    (resp, opened)
}

/// A row's context menu: right-click, or Shift+F10 / the Menu key while the row has focus.
fn row_menu(ui: &Ui, resp: &egui::Response, row: Rect, kind: MenuKind) -> Option<MenuState> {
    if resp.secondary_clicked() {
        return Some(MenuState { pos: resp.interact_pointer_pos().unwrap_or(row.center()), kind });
    }
    let keyboard = resp.has_focus()
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::F10) || i.consume_key(egui::Modifiers::NONE, egui::Key::Menu));
    keyboard.then(|| crate::ui::folders::open_menu(ui.ctx(), row.left_bottom(), kind, true))
}

/// Right-click on a hover-only area, which is not a keyboard focus stop.
fn secondary_click(ui: &Ui, resp: &egui::Response) -> Option<Pos2> {
    let clicked = resp.hovered() && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary));
    clicked.then(|| resp.hover_pos().unwrap_or(resp.rect.center()))
}

/// Empty space below the lists; right-click opens the add menu.
fn empty_area(ui: &mut Ui, width: f32) -> Option<MenuState> {
    // Fill the rest of the viewport without making the list scroll.
    let rest = ui.clip_rect().bottom() - ui.cursor().top();
    let (_, resp) = ui.allocate_exact_size(vec2(width, rest.max(24.0)), Sense::hover());
    secondary_click(ui, &resp).map(|pos| MenuState { pos, kind: MenuKind::Add })
}

/// A quiet drop zone shown while a Space has no pinned items.
fn empty_pinned_zone(ui: &mut Ui, pad: f32, width: f32, pal: &Palette) -> (Rect, Option<MenuState>) {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
    let row = Rect::from_min_max(pos2(r.left() + pad, r.top()), pos2(r.left() + pad + width, r.bottom()));
    let p = ui.painter();
    p.rect_stroke(row.shrink(0.5), 8, Stroke::new(1.0, pal.divider), egui::StrokeKind::Inside);
    icons::draw(p, Rect::from_center_size(pos2(row.left() + ITEM_ICON_X, row.center().y), vec2(12.0, 12.0)), Icon::Pin, pal.text_faint);
    let room = row.right() - 8.0 - (row.left() + ITEM_TEXT_X);
    let fits = p.layout_no_wrap(EMPTY_PINNED.into(), FontId::proportional(13.0), pal.text_faint).size().x <= room;
    label(ui, row.left() + ITEM_TEXT_X, row.right() - 8.0, row.center().y, if fits { EMPTY_PINNED } else { EMPTY_PINNED_SHORT }, pal.text_faint);
    (row, secondary_click(ui, &resp).map(|pos| MenuState { pos, kind: MenuKind::Add }))
}

/// Keyboard focus on a sidebar row, matching the favorite tiles.
fn focus_ring(ui: &Ui, resp: &egui::Response, row: Rect, pal: &Palette) {
    if resp.has_focus() {
        ui.painter().rect_stroke(row.shrink(1.0), 7, Stroke::new(1.5, pal.text), egui::StrokeKind::Inside);
    }
}

/// The URL pill's trailing controls, counted from its right end: the key
/// button when a page has logins, the blocked icon, the content blocking
/// shield, then Copy URL on hover.
pub(crate) fn pill_control(pill: Rect, slot: usize) -> Rect {
    Rect::from_center_size(pos2(pill.right() - 16.0 - slot as f32 * 24.0, pill.center().y), vec2(22.0, 22.0))
}

/// Where the pill's address ends, clear of Copy URL and the `controls`
/// (the key, the blocked icon, the shield) that show before it.
pub(crate) fn pill_text_right(pill: Rect, url: bool, controls: usize) -> f32 {
    pill.right() - if url { 32.0 } else { 10.0 } - controls as f32 * 24.0
}

/// What the rows draw this frame, from the sidebar switches of the
/// sidebar's profile.
#[derive(Clone, Copy, Debug, Default)]
struct Marks {
    unread: bool,
    previews: bool,
}

const BADGE_HEIGHT: f32 = 16.0;
const BADGE_DOT: f32 = 3.5;
/// Between a badge and the title or controls beside it.
const BADGE_GAP: f32 = 6.0;

/// A badge's size, and its count's text: a dot, or a pill wide enough for
/// the count.
fn badge_text(p: &egui::Painter, badge: Badge, pal: &Palette) -> (Vec2, Option<Arc<egui::Galley>>) {
    match badge.label() {
        Some(text) => {
            let galley = p.layout_no_wrap(text, FontId::proportional(10.5), Ink::new(pal.panel, pal.accent).on_accent);
            (vec2((galley.size().x + 10.0).max(BADGE_HEIGHT), BADGE_HEIGHT), Some(galley))
        }
        None => (Vec2::splat(BADGE_DOT * 2.0), None),
    }
}

fn paint_badge(p: &egui::Painter, rect: Rect, text: Option<Arc<egui::Galley>>, pal: &Palette) {
    let ink = Ink::new(pal.panel, pal.accent);
    match text {
        Some(galley) => {
            p.rect_filled(rect, BADGE_HEIGHT / 2.0, ink.accent);
            p.galley(rect.center() - galley.size() / 2.0, galley, ink.on_accent);
        }
        None => { p.circle_filled(rect.center(), BADGE_DOT, ink.accent); }
    }
}

/// A row's badge, ending at `right` before the row's trailing controls.
/// Returns where it went, so the title stops short of it.
fn row_badge(p: &egui::Painter, right: f32, cy: f32, badge: Badge, pal: &Palette) -> Rect {
    let (size, text) = badge_text(p, badge, pal);
    let rect = Rect::from_min_size(pos2(right - size.x, cy - size.y / 2.0), size);
    paint_badge(p, rect, text, pal);
    rect
}

/// A favorite's badge on its icon's top-right corner, inside the tile and
/// above the icon's middle.
fn tile_badge(p: &egui::Painter, tile: Rect, icon: Rect, badge: Badge, pal: &Palette) -> Rect {
    let (size, text) = badge_text(p, badge, pal);
    let center = pos2(icon.right() + 2.0, icon.top());
    let min = pos2((center.x - size.x / 2.0).min(tile.right() - 3.0 - size.x).max(tile.left() + 3.0), (center.y - size.y / 2.0).max(tile.top() + 3.0));
    let rect = Rect::from_min_size(min, size);
    paint_badge(p, rect, text, pal);
    rect
}

/// The badge of the tab an item has open, if its profile shows them.
fn item_badge(tabs: &[Tab], unread: &Unread, item: &str, marks: Marks) -> Option<Badge> {
    if !marks.unread { return None; }
    tabs.iter().find(|tab| tab.item_id.as_deref() == Some(item)).and_then(|tab| unread.badge(&tab.id))
}

/// Whether anything in a folder, at any depth, has a badge: a collapsed
/// folder shows a dot.
fn folder_unread(lib: &Library, tabs: &[Tab], unread: &Unread, space: &str, folder: &str, marks: Marks) -> bool {
    if !marks.unread || unread.is_empty() { return false; }
    lib.pinned_children(space, Some(folder)).iter().any(|node| match node {
        PinnedNode::Item(id, _) => item_badge(tabs, unread, id, marks).is_some(),
        PinnedNode::Folder(id, _) => folder_unread(lib, tabs, unread, space, id, marks),
    })
}

pub fn show(app: &mut CrossbowApp, ui: &mut Ui, rect: Rect, pal: &Palette, floating: bool) {
    let ctx = ui.ctx().clone();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layer_id(ui.layer_id()));
    let ui = &mut child;
    ui.set_clip_rect(rect.intersect(ui.clip_rect()));
    let painter = ui.painter().clone();

    // -- top row: window drag area + navigation ----------------------------
    let native_controls = cfg!(target_os = "macos") && !app.lib.settings.sidebar_on_right
        && !ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
    let header = SidebarHeader::new(rect, native_controls);
    let top = header.rect;
    let drag = ui.interact(top, Id::unique(("sidebar-top", floating)), Sense::click_and_drag());
    if drag.drag_started() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }
    let nav = app.focused_view().map(|v| v.nav()).unwrap_or_default();
    let loading = app.focused_tab_id().and_then(|t| app.tab(&t)).is_some_and(|t| t.is_loading());
    for (index, (icon, action, enabled)) in [
        (if loading { Icon::Stop } else { Icon::Reload }, if loading { Action::Stop } else { Action::Reload }, true),
        (Icon::Forward, Action::Forward, nav.can_go_forward),
        (Icon::Back, Action::Back, nav.can_go_back),
    ].into_iter().enumerate() {
        if icons::button(ui, header.row.trailing_control(index), icon, pal.text_dim, pal.hover, enabled).clicked() {
            app.actions.push(action);
        }
    }
    let r = header.row.leading_control(0);
    if icons::button(ui, r, Icon::Sidebar, pal.text_dim, pal.hover, true).on_hover_text("Toggle sidebar").clicked() {
        app.actions.push(Action::ToggleSidebar);
        app.ui.sidebar_peek = false;
    }

    // -- URL pill -----------------------------------------------------------
    let pill = Rect::from_min_size(pos2(rect.left() + PAD, top.bottom() + 2.0), vec2(rect.width() - 2.0 * PAD, 34.0));
    let pill_resp = ui.interact(pill, Id::unique(("url-pill", floating)), Sense::click());
    painter.rect_filled(pill, 9, if pill_resp.hovered() { pal.selected.gamma_multiply(0.9) } else { pal.hover.gamma_multiply(1.6) });
    let url = app.focused_tab_id().and_then(|t| app.tab(&t).map(|t| t.url.clone())).filter(|u| u != crate::app::NEW_TAB_URL);
    let shown = match &url {
        Some(u) if app.lib.settings.show_url_in_sidebar => crossbow_core::search::display_url(u),
        Some(u) => crossbow_core::search::host_of(u).unwrap_or_default(),
        None => "Search or enter URL".into(),
    };
    let secure = url.as_deref().is_some_and(|u| u.starts_with("https://"));
    let tx = pill.left() + ITEM_TEXT_X;
    let icon = if secure { Icon::Lock } else if url.is_some() { Icon::Globe } else { Icon::Search };
    icons::draw(&painter, Rect::from_center_size(pos2(pill.left() + ITEM_ICON_X, pill.center().y), vec2(12.0, 12.0)), icon, pal.text_dim);
    let text_color = if url.is_some() { pal.text } else { pal.text_dim };
    // The key stays at the pill's end; Copy URL appears beside it on hover.
    let key = url.is_some() && crate::ui::logins::key_button(app, ui, pill_control(pill, 0), pal.text_dim, pal.hover);
    let blocked = url.is_some() && crate::ui::site_permissions::blocked_button(app, ui, pill_control(pill, usize::from(key)), pal.text_dim, pal.hover);
    let shield = url.is_some() && crate::ui::content_blocking_shield::button(app, ui, pill_control(pill, usize::from(key) + usize::from(blocked)), pal.text_dim, pal.hover);
    let controls = usize::from(key) + usize::from(blocked) + usize::from(shield);
    let text_right = pill_text_right(pill, url.is_some(), controls);
    // Permission prompts hang from the docked pill; the floating one comes and goes.
    if !floating { app.ui.address = Some(pill); }
    let mut job = egui::text::LayoutJob::simple(shown, FontId::proportional(13.0), text_color, (text_right - tx).max(0.0));
    job.wrap.max_rows = 1;
    let galley = painter.layout_job(job);
    let text_y = pill.center().y - galley.size().y / 2.0;
    painter.with_clip_rect(pill.intersect(ui.clip_rect())).galley(pos2(tx, text_y), galley, text_color);
    if pill_resp.hovered() && url.is_some() {
        let cr = pill_control(pill, controls);
        if icons::button(ui, cr, Icon::Copy, pal.text_dim, pal.hover, true).on_hover_text("Copy URL").clicked() {
            app.actions.push(Action::CopyUrl);
        }
    }
    if pill_resp.clicked() {
        app.actions.push(Action::OpenCommandBar { edit_current: true });
    }

    // -- favorites ----------------------------------------------------------
    let profile = app.current_profile();
    let marks = Marks { unread: app.sidebar.unread(&app.lib.store, &profile), previews: app.sidebar.previews(&app.lib.store, &profile) };
    let favorites = app.lib.favorites(&profile);
    let mut drops: Vec<DropRow> = Vec::new();
    let body = Rect::from_min_max(pos2(rect.left(), pill.bottom() + 12.0), pos2(rect.right(), rect.bottom() - 46.0));
    let regions = SidebarRegions::new(body, favorites.len());
    if let Some(favorites_rect) = regions.favorites {
        let active = app.active_tab_id();
        favorite_grid(ui, favorites_rect, Id::unique(("favorite-scroll", &profile, floating)), favorites.len(), |ui, i, r| {
            let (id, item) = &favorites[i];
            drops.push(DropRow::favorite(i, id, r, ui.clip_rect()));
            let resp = favorite_response(ui, r, Id::unique(("fav", id)), item.display_title());
            if ui.is_rect_visible(r) {
                let open = app.tab_for_item(id).map(|t| (t.id.clone(), t.view.is_some()));
                let is_active = open.as_ref().is_some_and(|t| Some(&t.0) == active.as_ref());
                let bg = if is_active { pal.selected } else if resp.hovered() { pal.hover.gamma_multiply(2.2) } else { pal.hover.gamma_multiply(1.3) };
                ui.painter().rect_filled(r, 10, bg);
                let icon = Rect::from_center_size(r.center(), vec2(18.0, 18.0));
                favicon(app, ui, &item.url, icon, pal);
                if open.as_ref().is_some_and(|t| t.1) && !is_active {
                    ui.painter().circle_filled(pos2(r.center().x, r.bottom() - 5.0), 1.8, pal.text_dim);
                }
                if let Some(badge) = item_badge(&app.tabs, &app.unread, id, marks) {
                    tile_badge(ui.painter(), r, icon, badge, pal);
                }
                if resp.has_focus() {
                    ui.painter().rect_stroke(r.shrink(1.0), 9, Stroke::new(1.5, pal.text), egui::StrokeKind::Inside);
                }
            }
            // A favorite that isn't loaded previews instead of showing its title.
            let unloaded = !app.tab_for_item(id).is_some_and(|tab| tab.view.is_some());
            let resp = if marks.previews && unloaded {
                app.previews.offer(ui, Subject::Item(id.clone()), r, &resp, &app.active_space, false);
                resp
            } else {
                resp.on_hover_text(item.display_title())
            };
            if resp.clicked() {
                app.actions.push(Action::OpenItem(id.clone()));
            }
            if let Some(menu) = row_menu(ui, &resp, r, MenuKind::Item(id.clone())) {
                app.ui.menu = Some(menu);
            }
            if resp.drag_started() {
                app.ui.drag = Some(DragState { kind: DragKind::Favorite(id.clone()), start: r.center(), active: false });
            }
        });
    }

    // -- space content (slides during space switches) -----------------------
    let space_id = app.active_space.clone();
    let mut offset = 0.0;
    let mut alpha = 1.0;
    if let Some(a) = &app.ui.space_anim {
        if a.start >= 0.0 {
            let t = anim::ease_out_cubic(anim::progress(&ctx, a.start, 0.32));
            offset = (1.0 - t) * 48.0 * a.dir;
            alpha = t;
        }
    }
    let list_rect = regions.tabs;
    let mut list = ui.new_child(egui::UiBuilder::new().max_rect(list_rect).layer_id(ui.layer_id()));
    list.set_clip_rect(list_rect.intersect(ui.clip_rect()));
    list.multiply_opacity(alpha);

    egui::ScrollArea::vertical().id_salt(("sidebar-scroll", &space_id, floating)).auto_shrink([false, false]).show(&mut list, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        let width = rect.width() - 2.0 * PAD;
        ui.add_space(0.0);
        let indent = PAD + offset;

        // Space header.
        if let Some(space) = app.lib.spaces.get(&space_id).cloned() {
            let (r, resp) = ui.allocate_exact_size(vec2(rect.width(), 28.0), Sense::click());
            let p = ui.painter();
            let renaming = matches!(&app.ui.renaming, Some((RenameTarget::Space(id), _)) if *id == space_id);
            crate::ui::space_icon::paint(p, Rect::from_center_size(pos2(r.left() + indent + 10.0, r.center().y), vec2(18.0, 18.0)), &space.icon, 14.0, pal.text);
            if renaming {
                rename_field(app, ui, Rect::from_min_max(pos2(r.left() + indent + 24.0, r.top() + 2.0), pos2(r.right() - PAD, r.bottom() - 2.0)), pal);
            } else {
                let slots = header_slots(Rect::from_min_max(pos2(r.left() + indent, r.top()), pos2(r.left() + indent + width, r.bottom())));
                p.with_clip_rect(r.with_max_x(slots.title_right).intersect(ui.clip_rect()))
                    .text(pos2(r.left() + indent + 24.0, r.center().y), Align2::LEFT_CENTER, &space.name, FontId::proportional(13.5), pal.text_dim);
                if resp.double_clicked() {
                    app.ui.renaming = Some((RenameTarget::Space(space_id.clone()), space.name.clone()));
                }
                let (Some(add), Some(more)) = (slots.close, slots.mute) else { unreachable!("header slots are always shown") };
                // The pointer stays "inside" while it is over the header's own controls.
                if resp.contains_pointer() || app.ui.menu.as_ref().is_some_and(|m| matches!(&m.kind, MenuKind::Space(s) if *s == space_id)) {
                    let mresp = icons::button(ui, more, Icon::More, pal.text_dim, pal.hover, true);
                    if mresp.clicked() {
                        app.ui.menu = Some(MenuState { pos: more.left_bottom(), kind: MenuKind::Space(space_id.clone()) });
                    }
                }
                if let (_, Some(menu)) = add_button(ui, add, pal) {
                    app.ui.menu = Some(menu);
                }
                if let Some(menu) = row_menu(ui, &resp, r, MenuKind::Space(space_id.clone())) {
                    app.ui.menu = Some(menu);
                }
            }
        }

        // Pinned tree.
        pinned_tree(app, ui, &space_id, None, 0, indent, width, pal, marks, &mut drops);

        // Divider with "Clear".
        let today = app.today_tabs(&space_id).iter().map(|t| t.id.clone()).collect::<Vec<_>>();
        let (dr, divider) = ui.allocate_exact_size(vec2(rect.width(), 22.0), Sense::hover());
        if let Some(pos) = secondary_click(ui, &divider) {
            app.ui.menu = Some(MenuState { pos, kind: MenuKind::Today(space_id.clone()) });
        }
        drops.push(DropRow { section: Section::Pinned(None), index: usize::MAX, rect: dr.with_max_y(dr.center().y), clip_rect: ui.clip_rect(), source: None, folder: None });
        drops.push(DropRow { section: Section::Today, index: 0, rect: dr.with_min_y(dr.center().y), clip_rect: ui.clip_rect(), source: None, folder: None });
        let line_y = dr.center().y;
        let clear_w = if today.is_empty() { 0.0 } else { 52.0 };
        ui.painter().line_segment([pos2(dr.left() + indent + 4.0, line_y), pos2(dr.right() - PAD - clear_w - 4.0, line_y)], Stroke::new(1.0, pal.divider));
        if !today.is_empty() {
            let cr = Rect::from_min_max(pos2(dr.right() - PAD - clear_w, dr.top() + 2.0), pos2(dr.right() - PAD, dr.bottom() - 2.0));
            let cresp = ui.interact(cr, Id::unique("clear-today"), Sense::click());
            if cresp.hovered() {
                ui.painter().rect_filled(cr, 5, pal.hover);
            }
            ui.painter().text(cr.center(), Align2::CENTER_CENTER, "↓ Clear", FontId::proportional(11.5), pal.text_faint);
            if cresp.on_hover_text("Archive today's tabs").clicked() {
                app.actions.push(Action::ClearToday);
            }
        }
        crate::ui::tidy_tabs::undo_card(app, ui, &space_id, indent, width, pal);

        // New tab row.
        let (nr, nresp) = ui.allocate_exact_size(vec2(rect.width(), ROW_H), Sense::click());
        let row = Rect::from_min_max(pos2(nr.left() + indent, nr.top()), pos2(nr.right() - PAD, nr.bottom()));
        if nresp.hovered() {
            ui.painter().rect_filled(row, 8, pal.hover);
        }
        icons::draw(ui.painter(), Rect::from_center_size(pos2(row.left() + ITEM_ICON_X, row.center().y), vec2(12.0, 12.0)), Icon::Plus, pal.text_dim);
        ui.painter().text(pos2(row.left() + ITEM_TEXT_X, row.center().y), Align2::LEFT_CENTER, "New Tab", FontId::proportional(13.0), pal.text_dim);
        if nresp.clicked() {
            app.actions.push(Action::OpenCommandBar { edit_current: false });
        }

        // Today's tabs.
        for (i, tab_id) in today.iter().enumerate() {
            let r = tab_row(app, ui, tab_id, None, indent, width, pal, marks);
            drops.push(DropRow { section: Section::Today, index: i, rect: r, clip_rect: ui.clip_rect(), source: Some(DragKind::Tab(tab_id.clone())), folder: None });
        }
        if let Some(menu) = empty_area(ui, rect.width()) {
            app.ui.menu = Some(menu);
        }
    });

    handle_drag(app, ui, &drops, pal, rect);
    if marks.previews {
        let cx = crate::ui::hover_preview::Cx {
            lib: &app.lib, tabs: &app.tabs, favicons: &mut app.favicons, profile: &profile, space: &app.active_space,
            sidebar: rect, on_right: app.lib.settings.sidebar_on_right, busy: app.ui.drag.is_some() || app.ui.menu.is_some(),
            now: ctx.input(|i| i.time), now_ms: crossbow_core::now_ms(), pal,
        };
        app.actions.extend(app.previews.finish(ui, cx));
    } else {
        app.previews.close();
    }

    // Horizontal two-finger swipe over the sidebar switches spaces.
    if ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| list_rect.contains(p)) {
        let dx = ctx.input(|i| i.smooth_scroll_delta.x);
        let acc_id = Id::unique("space-swipe");
        let mut acc: f32 = ctx.data(|d| d.get_temp(acc_id)).unwrap_or(0.0);
        acc += dx;
        if acc.abs() > 120.0 {
            app.actions.push(Action::SwitchSpaceBy(if acc > 0.0 { -1 } else { 1 }));
            acc = 0.0;
        }
        if dx == 0.0 {
            acc *= 0.9;
        }
        ctx.data_mut(|d| d.insert_temp(acc_id, acc));
    }

    // -- bottom bar: library, space switcher, new space ----------------------
    let bar = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 44.0), rect.right_bottom());
    let lib_r = Rect::from_center_size(pos2(bar.left() + PAD + 13.0, bar.center().y), vec2(28.0, 28.0));
    if icons::button(ui, lib_r, Icon::Archive, pal.text_dim, pal.hover, true).on_hover_text("Archive, downloads & captures").clicked() {
        if !app.dismiss_library(&ctx, crate::app::LibraryDismissal::RestoreFocus) { app.ui.show_archive = true; }
    }
    let plus_r = Rect::from_center_size(pos2(bar.right() - PAD - 13.0, bar.center().y), vec2(28.0, 28.0));
    if icons::button(ui, plus_r, Icon::Plus, pal.text_dim, pal.hover, true).on_hover_text("New space").clicked() {
        app.actions.push(Action::NewSpace);
    }
    let spaces = app.lib.spaces_ordered();
    let slot = 26.0;
    let total = slot * spaces.len() as f32;
    let avail = plus_r.left() - lib_r.right() - 8.0;
    let slot = if total > avail { avail / spaces.len() as f32 } else { slot };
    let mut sx = bar.center().x - slot * spaces.len() as f32 / 2.0;
    for (id, space) in &spaces {
        let r = Rect::from_min_size(pos2(sx, bar.center().y - 13.0), vec2(slot, 26.0));
        let resp = ui.interact(r, Id::unique(("space-dot", id)), Sense::click());
        let active = *id == app.active_space;
        if active || resp.hovered() {
            painter.rect_filled(r.shrink(1.0), 7, if active { pal.selected.gamma_multiply(0.85) } else { pal.hover });
        }
        crate::ui::space_icon::paint(&painter, r, &space.icon, if active { 14.0 } else { 13.0 }, if active { pal.selected_text } else { pal.text_dim });
        let resp = resp.on_hover_text(&space.name);
        if resp.clicked() {
            app.actions.push(Action::SwitchSpace(id.clone()));
        }
        if resp.secondary_clicked() {
            app.ui.menu = Some(MenuState { pos: resp.interact_pointer_pos().unwrap_or(r.center()), kind: MenuKind::Space(id.clone()) });
        }
        sx += slot;
    }
}

#[allow(clippy::too_many_arguments)]
fn pinned_tree(
    app: &mut CrossbowApp,
    ui: &mut Ui,
    space: &str,
    folder: Option<&str>,
    depth: usize,
    indent: f32,
    width: f32,
    pal: &Palette,
    marks: Marks,
    drops: &mut Vec<DropRow>,
) {
    let nodes = app.lib.pinned_children(space, folder);
    if nodes.is_empty() && folder.is_none() {
        let (zone, menu) = empty_pinned_zone(ui, indent, width, pal);
        drops.push(DropRow { section: Section::Pinned(None), index: 0, rect: zone, clip_rect: ui.clip_rect(), source: None, folder: None });
        if menu.is_some() {
            app.ui.menu = menu;
        }
        return;
    }
    let pad = indent + depth as f32 * 14.0;
    for (i, node) in nodes.iter().enumerate() {
        match node {
            PinnedNode::Item(id, item) => {
                let tab_id = app.tab_for_item(id).map(|t| t.id.clone());
                let r = match tab_id {
                    Some(t) => tab_row(app, ui, &t, Some(id), pad, width - depth as f32 * 14.0, pal, marks),
                    None => item_row(app, ui, id, &item.url, item.display_title(), pad, width - depth as f32 * 14.0, pal, marks),
                };
                drops.push(DropRow { section: Section::Pinned(folder.map(String::from)), index: i, rect: r, clip_rect: ui.clip_rect(), source: Some(DragKind::Pinned(id.clone())), folder: None });
                crate::ui::tidy_tabs::title_card(app, ui, id, pad, width - depth as f32 * 14.0, pal);
            }
            PinnedNode::Folder(fid, f) => {
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click_and_drag());
                let row = Rect::from_min_max(pos2(r.left() + pad, r.top()), pos2(r.left() + pad + width - depth as f32 * 14.0, r.bottom()));
                if resp.hovered() {
                    ui.painter().rect_filled(row, 8, pal.hover);
                }
                let chev = Rect::from_center_size(pos2(row.left() + ITEM_ICON_X, row.center().y), vec2(10.0, 10.0));
                let p = ui.painter().clone();
                if f.collapsed {
                    icons::draw(&p, chev, Icon::Chevron, pal.text_dim);
                } else {
                    let c = chev.center();
                    p.add(egui::Shape::line(vec![c + vec2(-3.5, -1.5), c + vec2(0.0, 2.0), c + vec2(3.5, -1.5)], Stroke::new(1.4, pal.text_dim)));
                }
                icons::draw(&p, Rect::from_center_size(pos2(row.left() + 32.0, row.center().y), vec2(14.0, 14.0)), if f.collapsed { Icon::Folder } else { Icon::FolderOpen }, pal.text_dim);
                let renaming = matches!(&app.ui.renaming, Some((RenameTarget::Folder(x), _)) if x == fid);
                if renaming {
                    rename_field(app, ui, Rect::from_min_max(pos2(row.left() + 46.0, row.top() + 3.0), pos2(row.right() - 4.0, row.bottom() - 3.0)), pal);
                } else {
                    let mut right = row.right() - 8.0;
                    if f.collapsed && folder_unread(&app.lib, &app.tabs, &app.unread, space, fid, marks) {
                        right = row_badge(&p, row.right() - 8.0, row.center().y, Badge::Dot, pal).left() - BADGE_GAP;
                    }
                    label(ui, row.left() + 46.0, right, row.center().y, &f.name, pal.text);
                    // An expanded folder already shows its tabs.
                    if marks.previews && f.collapsed {
                        app.previews.offer(ui, Subject::Folder(fid.clone()), row, &resp, space, true);
                    }
                }
                if resp.clicked() {
                    app.actions.push(Action::ToggleFolder(fid.clone()));
                }
                if resp.double_clicked() {
                    app.ui.renaming = Some((RenameTarget::Folder(fid.clone()), f.name.clone()));
                }
                focus_ring(ui, &resp, row, pal);
                if let Some(menu) = row_menu(ui, &resp, row, MenuKind::Folder(fid.clone())) {
                    app.ui.menu = Some(menu);
                }
                if resp.drag_started() {
                    app.ui.drag = Some(DragState { kind: DragKind::Pinned(fid.clone()), start: row.center(), active: false });
                }
                drops.push(DropRow { section: Section::Pinned(folder.map(String::from)), index: i, rect: row, clip_rect: ui.clip_rect(), source: Some(DragKind::Pinned(fid.clone())), folder: Some(fid.clone()) });
                if !f.collapsed {
                    pinned_tree(app, ui, space, Some(fid), depth + 1, indent, width, pal, marks, drops);
                }
            }
        }
    }
}

/// A pinned item that has no live tab yet.
#[allow(clippy::too_many_arguments)]
fn item_row(app: &mut CrossbowApp, ui: &mut Ui, item_id: &str, url: &str, title: &str, pad: f32, width: f32, pal: &Palette, marks: Marks) -> Rect {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click_and_drag());
    let row = Rect::from_min_max(pos2(r.left() + pad, r.top()), pos2(r.left() + pad + width, r.bottom()));
    if resp.hovered() {
        ui.painter().rect_filled(row, 8, pal.hover);
    }
    favicon(app, ui, url, Rect::from_center_size(pos2(row.left() + ITEM_ICON_X, row.center().y), vec2(16.0, 16.0)), pal);
    let renaming = matches!(&app.ui.renaming, Some((RenameTarget::Item(x), _)) if x == item_id);
    if renaming {
        rename_field(app, ui, Rect::from_min_max(pos2(row.left() + ITEM_TEXT_X, row.top() + 3.0), pos2(row.right() - 4.0, row.bottom() - 3.0)), pal);
    } else {
        label(ui, row.left() + ITEM_TEXT_X, row.right() - 8.0, row.center().y, title, pal.text_dim);
        if marks.previews {
            app.previews.offer(ui, Subject::Item(item_id.to_string()), row, &resp, &app.active_space, false);
        }
    }
    if resp.clicked() {
        app.actions.push(Action::OpenItem(item_id.to_string()));
    }
    if resp.double_clicked() {
        app.ui.renaming = Some((RenameTarget::Item(item_id.to_string()), title.to_string()));
    }
    focus_ring(ui, &resp, row, pal);
    if let Some(menu) = row_menu(ui, &resp, row, MenuKind::Item(item_id.to_string())) {
        app.ui.menu = Some(menu);
    }
    if resp.drag_started() {
        app.ui.drag = Some(DragState { kind: DragKind::Pinned(item_id.to_string()), start: row.center(), active: false });
    }
    row
}

/// A row for a live tab (today's tab, or a loaded pinned item).
#[allow(clippy::too_many_arguments)]
fn tab_row(app: &mut CrossbowApp, ui: &mut Ui, tab_id: &str, item_id: Option<&str>, pad: f32, width: f32, pal: &Palette, marks: Marks) -> Rect {
    let interaction = TabRowInteraction::new(ui, pad, width);
    let row = interaction.rect;
    let resp = &interaction.response;
    let Some(tab) = app.tab(tab_id) else { return row };
    let active = app.active_tab_id().as_deref() == Some(tab_id);
    let in_split = app.splits.get(&app.active_space).is_some_and(|s| s.tabs.iter().any(|t| t == tab_id));
    let (url, title, loaded, loading, muted) = (tab.url.clone(), tab.display_title(), tab.view.is_some(), tab.is_loading(), tab.muted);
    let p = ui.painter().clone();
    if active {
        p.add(egui::epaint::Shadow { offset: [0, 1], blur: 4, spread: 0, color: Color32::from_black_alpha(18) }.as_shape(row, egui::CornerRadius::same(8)));
        p.rect_filled(row, 8, pal.selected);
    } else if resp.hovered() {
        p.rect_filled(row, 8, pal.hover);
    }
    let text_col = if active { pal.selected_text } else if loaded || item_id.is_none() { pal.text } else { pal.text_dim };
    let icon_rect = Rect::from_center_size(pos2(row.left() + ITEM_ICON_X, row.center().y), vec2(16.0, 16.0));
    if loading {
        let t = ui.ctx().input(|i| i.time) as f32;
        let c = icon_rect.center();
        let pts: Vec<Pos2> = (0..=16).map(|k| {
            let a = t * 6.0 + k as f32 / 16.0 * 4.4;
            c + vec2(a.cos(), a.sin()) * 6.5
        }).collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.6, pal.accent)));
    } else {
        favicon(app, ui, &url, icon_rect, pal);
    }
    let trailing = interaction.trailing(active, muted, in_split);
    if let Some(action) = trailing.show(ui, tab_id, item_id.is_some(), text_col, pal) {
        let closing = matches!(action, Action::CloseTab(_));
        app.actions.push(action);
        if closing {
            return row;
        }
    }
    let mut right = trailing.title_right;
    // Pinned tabs only: Today's tabs have no badge.
    if let Some(badge) = item_id.and_then(|item| item_badge(&app.tabs, &app.unread, item, marks)) {
        right = row_badge(&p, right - 4.0, row.center().y, badge, pal).left() - BADGE_GAP;
    }
    let renaming = item_id.is_some_and(|i| matches!(&app.ui.renaming, Some((RenameTarget::Item(x), _)) if x == i));
    if renaming {
        rename_field(app, ui, Rect::from_min_max(pos2(row.left() + ITEM_TEXT_X, row.top() + 3.0), pos2(right, row.bottom() - 3.0)), pal);
    } else {
        let shown = item_id.and_then(|i| app.lib.items.get(i)).and_then(|i| i.custom_title.clone()).unwrap_or(title);
        label(ui, row.left() + ITEM_TEXT_X, right, row.center().y, &shown, text_col);
        // A pinned tab kept without its page previews like an unloaded pin.
        if let Some(item) = item_id.filter(|_| marks.previews && !loaded) {
            app.previews.offer(ui, Subject::Item(item.to_string()), row, resp, &app.active_space, false);
        }
    }
    interaction.dispatch_row_actions(tab_id, &mut app.actions);
    if resp.double_clicked() {
        if let Some(i) = item_id {
            let t = app.lib.items.get(i).map(|x| x.display_title().to_string()).unwrap_or_default();
            app.ui.renaming = Some((RenameTarget::Item(i.to_string()), t));
        }
    }
    focus_ring(ui, resp, row, pal);
    let kind = match item_id {
        Some(i) => MenuKind::Item(i.to_string()),
        None => MenuKind::Tab(tab_id.to_string()),
    };
    if let Some(menu) = row_menu(ui, resp, row, kind) {
        app.ui.menu = Some(menu);
    }
    if resp.drag_started() {
        let kind = match item_id {
            Some(i) => DragKind::Pinned(i.to_string()),
            None => DragKind::Tab(tab_id.to_string()),
        };
        app.ui.drag = Some(DragState { kind, start: row.center(), active: false });
    }
    row
}

fn label(ui: &Ui, left: f32, right: f32, cy: f32, text: &str, color: Color32) {
    let p = ui.painter().with_clip_rect(Rect::from_min_max(pos2(left, cy - 10.0), pos2(right, cy + 10.0)).intersect(ui.clip_rect()));
    let galley = p.layout_no_wrap(text.to_string(), FontId::proportional(13.0), color);
    let y = cy - galley.size().y / 2.0;
    let fade = galley.size().x > right - left;
    p.galley(pos2(left, y), galley, color);
    if fade {
        // Soft fade instead of a hard cut for long titles.
        let fr = Rect::from_min_max(pos2(right - 18.0, cy - 9.0), pos2(right, cy + 9.0));
        let mut mesh = egui::Mesh::default();
        let bg = Color32::TRANSPARENT;
        mesh.colored_vertex(fr.left_top(), bg);
        mesh.colored_vertex(fr.right_top(), bg);
        mesh.colored_vertex(fr.left_bottom(), bg);
        mesh.colored_vertex(fr.right_bottom(), bg);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(1, 3, 2);
        p.add(mesh);
    }
}

pub fn favicon(app: &mut CrossbowApp, ui: &Ui, url: &str, rect: Rect, pal: &Palette) {
    paint_favicon(&mut app.favicons, ui, url, rect, pal);
}

/// A site's saved favicon, or its first letter. Never fetches one.
pub(crate) fn paint_favicon(favicons: &mut crate::favicons::Favicons, ui: &Ui, url: &str, rect: Rect, pal: &Palette) {
    let p = ui.painter();
    if url.is_empty() || url == crate::app::NEW_TAB_URL {
        icons::draw(p, rect.shrink(1.0), Icon::Globe, pal.text_dim);
        return;
    }
    match favicons.get(ui.ctx(), url) {
        Some(tex) => {
            p.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            let host = crossbow_core::search::host_of(url).unwrap_or_default();
            let letter = host.chars().next().map(|c| c.to_ascii_uppercase().to_string()).unwrap_or_else(|| "•".into());
            p.rect_filled(rect, 4, theme::letter_color(&host));
            p.text(rect.center(), Align2::CENTER_CENTER, letter, FontId::proportional(rect.height() * 0.62), Color32::WHITE);
        }
    }
}

fn rename_field(app: &mut CrossbowApp, ui: &mut Ui, rect: Rect, pal: &Palette) {
    let Some((target, mut text)) = app.ui.renaming.clone() else { return };
    let id = Id::unique("rename-field");
    let resp = ui.put(
        rect,
        egui::TextEdit::singleline(&mut text).id(id).font(FontId::proportional(13.0)).text_color(pal.text).frame(egui::Frame::NONE),
    );
    if !resp.has_focus() && !resp.lost_focus() {
        resp.request_focus();
    }
    app.ui.renaming = Some((target.clone(), text.clone()));
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
    if resp.lost_focus() || enter {
        if !ui.input(|i| i.key_pressed(egui::Key::Escape)) && !text.trim().is_empty() {
            app.actions.push(match target {
                RenameTarget::Space(id) => Action::RenameSpace(id, text.trim().to_string()),
                RenameTarget::Folder(id) => Action::RenameFolder(id, text.trim().to_string()),
                RenameTarget::Item(id) => Action::RenameItem(id, text.trim().to_string()),
            });
        }
        app.ui.renaming = None;
    }
}

fn drag_pointer(drag: &mut Option<DragState>, input: &egui::InputState) -> Option<(DragKind, Pos2, bool)> {
    if !input.focused || input.key_pressed(egui::Key::Escape) {
        *drag = None;
        return None;
    }
    let Some(pos) = input.pointer.hover_pos() else {
        // Losing the pointer must not release onto its last in-sidebar position.
        *drag = None;
        return None;
    };
    let down = input.pointer.primary_down();
    let state = drag.as_mut()?;
    if !state.active && pos.distance(state.start) > 5.0 {
        state.active = true;
    }
    let active = state.active;
    let kind = state.kind.clone();
    if !down {
        *drag = None;
    }
    (active && (down || input.pointer.primary_released())).then_some((kind, pos, down))
}

fn handle_drag(app: &mut CrossbowApp, ui: &mut Ui, drops: &[DropRow], pal: &Palette, sidebar: Rect) {
    let ctx = ui.ctx().clone();
    let Some((kind, pos, down)) = ctx.input(|input| drag_pointer(&mut app.ui.drag, input)) else { return };
    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    // Use the same resolved action for the preview and the eventual release.
    let target = drop_target(&kind, drops, pos, sidebar.intersect(ui.clip_rect()))
        .and_then(|target| drop_action(&kind, drops, &target).map(|action| (target, action)));
    let layer = ui.painter().clone().with_layer_id(egui::LayerId::new(egui::Order::Tooltip, Id::unique("drag-ghost")));
    if let Some((t, _)) = &target {
        let marker = layer.with_clip_rect(t.visible);
        if t.into_folder {
            marker.rect_stroke(t.visible, 8, Stroke::new(1.5, pal.accent), egui::StrokeKind::Inside);
        } else if t.row.section == Section::Favorites {
            let x = if t.after { t.visible.right() } else { t.visible.left() };
            marker.line_segment([pos2(x, t.visible.top()), pos2(x, t.visible.bottom())], Stroke::new(2.0, pal.accent));
        } else {
            let y = if t.after { t.visible.bottom() } else { t.visible.top() };
            marker.line_segment([pos2(t.visible.left(), y), pos2(t.visible.right(), y)], Stroke::new(2.0, pal.accent));
        }
    }
    let title = match &kind {
        DragKind::Tab(id) => app.tab(id).map(|t| t.display_title()).unwrap_or_default(),
        DragKind::Pinned(id) | DragKind::Favorite(id) => app
            .lib
            .items
            .get(id)
            .map(|i| i.display_title().to_string())
            .or_else(|| app.lib.folders.get(id).map(|f| f.name.clone()))
            .unwrap_or_default(),
    };
    let ghost = Rect::from_min_size(pos + vec2(8.0, -14.0), vec2((sidebar.width() - 40.0).min(220.0), 28.0));
    layer.rect_filled(ghost, 8, pal.selected);
    layer.text(ghost.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, title, FontId::proportional(12.5), pal.selected_text);

    if down {
        ctx.request_repaint();
    } else if let Some((_, action)) = target {
        app.actions.push(Action::Drop(Box::new(action)));
    }
}

#[cfg(test)]
#[path = "sidebar_folder_tests.rs"]
mod folder_tests;

#[cfg(test)]
#[path = "sidebar_badge_tests.rs"]
mod badge_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn list_rows(section: Section, ids: &[&str], clip: Rect) -> Vec<DropRow> {
        ids.iter().enumerate().map(|(index, id)| DropRow {
            section: section.clone(), index,
            rect: Rect::from_min_size(clip.min + vec2(PAD, index as f32 * (ROW_H + 2.0)), vec2(clip.width() - PAD * 2.0, ROW_H)),
            clip_rect: clip,
            source: Some(match section {
                Section::Today => DragKind::Tab((*id).into()),
                Section::Pinned(_) => DragKind::Pinned((*id).into()),
                Section::Favorites => DragKind::Favorite((*id).into()),
            }),
            folder: None,
        }).collect()
    }

    fn action_at(kind: &DragKind, rows: &[DropRow], pos: Pos2, sidebar: Rect) -> Option<Action> {
        drop_target(kind, rows, pos, sidebar).and_then(|target| drop_action(kind, rows, &target))
    }

    fn button_event(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }
    }

    #[test]
    fn drop_targets_require_both_axes_and_visible_scroll_bounds() {
        for (left, height) in [(0.0, 420.0), (8.0, 404.0), (0.0, 860.0)] {
            let sidebar = Rect::from_min_size(pos2(left, 0.0), vec2(252.0, height));
            let clip = Rect::from_min_max(pos2(left, 200.0), pos2(sidebar.right(), height - 46.0));
            for section in [Section::Today, Section::Pinned(None)] {
                let mut rows = list_rows(section, &["a", "b", "c"], clip);
                let kind = rows[0].source.clone().unwrap();
                rows[1].rect = Rect::from_min_size(pos2(left + PAD, clip.top() - 20.0), vec2(232.0, ROW_H));
                rows[2].rect = Rect::from_min_size(pos2(left + PAD, clip.bottom() + 1.0), vec2(232.0, ROW_H));
                let y = rows[0].rect.center().y;
                for pos in [pos2(sidebar.right() + 80.0, y), pos2(sidebar.left() - 8.0, y),
                    pos2(sidebar.center().x, clip.top() - 4.0), rows[2].rect.center()] {
                    assert!(action_at(&kind, &rows, pos, sidebar).is_none(), "must cancel at {pos:?}");
                }
                // Only the exposed fragment is a target; clipping must not move its midpoint.
                let exposed = pos2(sidebar.center().x, clip.top() + 4.0);
                let target = drop_target(&kind, &rows[1..], exposed, sidebar).unwrap();
                assert_eq!(target.visible, rows[1].rect.intersect(clip));
                assert!(target.after);
                assert!(action_at(&kind, &rows, pos2(sidebar.center().x, y), sidebar).is_some());
                // Also respect a narrower parent clip, e.g. a partially exposed floating sidebar.
                let narrow = sidebar.with_max_x(sidebar.center().x);
                assert!(action_at(&kind, &rows, pos2(sidebar.right() - 15.0, y), narrow).is_none());
            }
        }
    }

    #[test]
    fn same_list_slots_normalize_downward_upward_and_self_moves() {
        let clip = Rect::from_min_size(pos2(0.0, 100.0), vec2(252.0, 300.0));
        for section in [Section::Today, Section::Pinned(None), Section::Pinned(Some("folder".into()))] {
            let rows = list_rows(section, &["a", "b", "c", "d"], clip);
            for (from, to, after, expected) in [
                (0, 1, false, ["a", "b", "c", "d"]),
                (0, 1, true, ["b", "a", "c", "d"]),
                (0, 2, false, ["b", "a", "c", "d"]),
                (3, 1, false, ["a", "d", "b", "c"]),
                (3, 1, true, ["a", "b", "d", "c"]),
                (0, 3, true, ["b", "c", "d", "a"]),
                (3, 0, false, ["d", "a", "b", "c"]),
                (1, 1, false, ["a", "b", "c", "d"]),
                (1, 1, true, ["a", "b", "c", "d"]),
            ] {
                let kind = rows[from].source.as_ref().unwrap();
                let pos = rows[to].rect.center() + vec2(0.0, if after { 10.0 } else { -10.0 });
                let index = match action_at(kind, &rows, pos, clip).unwrap() {
                    Action::MoveTab { index, .. } | Action::MovePinned { index, .. } => index,
                    action => panic!("unexpected action: {action:?}"),
                };
                let mut ordered = vec!["a", "b", "c", "d"];
                let moved = ordered.remove(from);
                ordered.insert(index, moved);
                assert_eq!(ordered, expected, "from {from} to {to}, after={after}");
            }
        }
        assert_eq!(insertion_index(usize::MAX, Some(0)), usize::MAX);
        assert_eq!(insertion_index(2, None), 2);
    }

    #[test]
    fn cross_list_drops_and_dividers_preserve_insertion_slots() {
        let sidebar = Rect::from_min_size(Pos2::ZERO, vec2(252.0, 600.0));
        let mut rows = list_rows(Section::Pinned(None), &["pin", "folder"], sidebar.with_min_y(100.0));
        rows[1].folder = Some("folder".into());
        rows.extend(list_rows(Section::Pinned(Some("folder".into())), &["child"], sidebar.with_min_y(200.0)));
        rows.extend(list_rows(Section::Today, &["tab"], sidebar.with_min_y(300.0)));
        let pin = DragKind::Pinned("pin".into());
        let tab = DragKind::Tab("tab".into());
        assert!(matches!(action_at(&pin, &rows, rows[2].rect.center() + vec2(0.0, 10.0), sidebar),
            Some(Action::MovePinned { index: 1, folder: Some(ref f), .. }) if f == "folder"));
        assert!(matches!(action_at(&tab, &rows, rows[0].rect.center() + vec2(0.0, 10.0), sidebar), Some(Action::TabToPinned { index: 1, .. })));
        assert!(matches!(action_at(&pin, &rows, rows[3].rect.center() + vec2(0.0, 10.0), sidebar), Some(Action::ItemToToday { index: 1, .. })));
        assert!(matches!(action_at(&pin, &rows, rows[1].rect.center(), sidebar), Some(Action::MovePinned { index: usize::MAX, .. })));
        let folder = DragKind::Pinned("folder".into());
        assert!(action_at(&folder, &rows, rows[1].rect.center(), sidebar).is_none());
        assert!(action_at(&folder, &rows, rows[3].rect.center(), sidebar).is_none());
        assert!(action_at(&DragKind::Pinned("other-space".into()), &rows, rows[0].rect.center(), sidebar).is_none());
        for (section, index) in [(Section::Pinned(None), usize::MAX), (Section::Today, 0)] {
            rows.push(DropRow { section, index, rect: Rect::from_min_size(pos2(0.0, 400.0), vec2(252.0, 11.0)), clip_rect: sidebar, source: None, folder: None });
            let target = rows.last().unwrap().rect;
            for y in [target.top() + 1.0, target.bottom() - 1.0] {
                let action = action_at(&tab, &rows, pos2(100.0, y), sidebar).unwrap();
                assert!(matches!(action, Action::MoveTab { index: 0, .. } | Action::TabToPinned { index: usize::MAX, .. }));
            }
            rows.pop();
        }
    }

    #[test]
    fn drag_release_outside_cancels_and_a_new_gesture_can_retry() {
        let sidebar = Rect::from_min_size(Pos2::ZERO, vec2(252.0, 420.0));
        for section in [Section::Today, Section::Pinned(None), Section::Favorites] {
            for outside_x in [-8.0, 400.0] {
                let ctx = egui::Context::default();
                let rows = list_rows(section.clone(), &["a", "b"], sidebar.with_min_y(200.0));
                let kind = rows[0].source.clone().unwrap();
                let start = rows[0].rect.center();
                let valid = rows[1].rect.right_bottom() - vec2(8.0, 8.0);
                let outside = pos2(outside_x, valid.y);
                let mut drag = None;
                let mut actions = Vec::new();
                for (frame, pos) in [start, start, outside, outside, start, start, valid, valid].into_iter().enumerate() {
                    let mut events = vec![egui::Event::PointerMoved(pos)];
                    if frame == 1 || frame == 5 {
                        drag = Some(DragState { kind: kind.clone(), start, active: false });
                        events.push(button_event(pos, true));
                    } else if frame == 3 || frame == 7 {
                        events.push(button_event(pos, false));
                    }
                    ctx.run_ui(egui::RawInput { events, time: Some(frame as f64 / 60.0), ..Default::default() }, |ui| {
                        if let Some((kind, pos, false)) = ui.input(|input| drag_pointer(&mut drag, input)) {
                            if let Some(action) = action_at(&kind, &rows, pos, sidebar) { actions.push(action); }
                        }
                    }).textures_delta.clear();
                    if frame == 3 {
                        assert!(drag.is_none());
                        assert!(actions.is_empty());
                    }
                }
                assert!(drag.is_none());
                assert_eq!(actions.len(), 1, "only the retried in-sidebar release may mutate");
            }
        }
    }

    #[test]
    fn cancelled_drag_never_uses_a_stale_pointer_position() {
        for cancel in [
            egui::Event::PointerGone,
            egui::Event::WindowFocused(false),
            egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() },
        ] {
            let ctx = egui::Context::default();
            let pos = pos2(100.0, 200.0);
            let mut drag = Some(DragState { kind: DragKind::Tab("tab".into()), start: pos, active: true });
            ctx.run_ui(egui::RawInput { events: vec![egui::Event::PointerMoved(pos), button_event(pos, true)], ..Default::default() }, |_| {}).textures_delta.clear();
            let focused = !matches!(cancel, egui::Event::WindowFocused(false));
            ctx.run_ui(egui::RawInput { events: vec![button_event(pos, false), cancel], focused, ..Default::default() }, |ui| {
                assert!(ui.input(|input| drag_pointer(&mut drag, input)).is_none());
            }).textures_delta.clear();
            assert!(drag.is_none());
        }
    }

    #[test]
    fn pinned_drop_actions_preserve_core_order_after_reopening() {
        use crossbow_core::{library::Library, model::Theme, store::Store};
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        let profile = lib.create_profile("Fixture").unwrap();
        let space = lib.create_space("Fixture", "", Theme::preset(0), &profile).unwrap();
        let a = lib.add_pinned(&space, None, "https://a.example", "A").unwrap();
        let b = lib.create_folder(&space, None, "B").unwrap();
        let c = lib.add_pinned(&space, None, "https://c.example", "C").unwrap();
        let d = lib.create_folder(&space, None, "D").unwrap();
        let clip = Rect::from_min_size(Pos2::ZERO, vec2(252.0, 400.0));
        for (source, target, after) in [(&a, &b, true), (&d, &a, false)] {
            let nodes = lib.pinned_children(&space, None);
            let ids: Vec<_> = nodes.iter().map(PinnedNode::id).collect();
            let mut rows = list_rows(Section::Pinned(None), &ids, clip);
            for (row, node) in rows.iter_mut().zip(&nodes) {
                if let PinnedNode::Folder(id, _) = node { row.folder = Some(id.clone()); }
            }
            let row = &rows[ids.iter().position(|id| *id == target).unwrap()];
            let pos = row.rect.center() + vec2(0.0, if after { 12.0 } else { -12.0 });
            let Some(Action::MovePinned { node, folder, index }) = action_at(&DragKind::Pinned(source.clone()), &rows, pos, clip) else { panic!("expected pinned move") };
            lib.move_pinned(&node, &space, folder.as_deref(), index).unwrap();
        }
        let lib = Library::open(lib.store).unwrap();
        let actual: Vec<_> = lib.pinned_children(&space, None).into_iter().map(|node| node.id().to_string()).collect();
        assert_eq!(actual, [b, d, a, c]);
    }

    fn favorite_rows(ctx: &egui::Context, viewport: Rect, ids: &[String], frame: usize, scroll: bool) -> Vec<DropRow> {
        let mut rows = Vec::new();
        let mut events = vec![egui::Event::PointerMoved(viewport.center())];
        if scroll {
            events.push(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -1000.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() });
        }
        ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 860.0))), events, time: Some(frame as f64 / 60.0), ..Default::default() }, |ui| {
            rows.clear();
            favorite_grid(ui, viewport, Id::unique("favorite-drop-probe"), ids.len(), |ui, index, rect| {
                favorite_response(ui, rect, Id::unique(("favorite-drop", &ids[index])), "Synthetic favorite");
                rows.push(DropRow::favorite(index, &ids[index], rect, ui.clip_rect()));
            });
        }).textures_delta.clear();
        rows
    }

    #[test]
    fn favorite_grid_drops_are_clipped_and_first_last_moves_persist() {
        use crossbow_core::{library::Library, store::Store};
        for height in [404.0, 420.0, 860.0] {
            let sidebar = Rect::from_min_size(Pos2::ZERO, vec2(252.0, height));
            let body = sidebar.with_min_y(96.0).with_max_y(height - 46.0);
            let viewport = SidebarRegions::new(body, 12).favorites.unwrap();
            let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
            let profile = lib.create_profile("Fixture").unwrap();
            let other_profile = lib.create_profile("Other").unwrap();
            let other = lib.add_favorite(&other_profile, "https://other.example", "Other").unwrap();
            let ids: Vec<_> = (0..12).map(|i| lib.add_favorite(&profile, &format!("https://favorite{i}.example"), "Favorite").unwrap()).collect();
            let kind = DragKind::Favorite(ids[0].clone());
            let ctx = egui::Context::default();
            let mut rows = favorite_rows(&ctx, viewport, &ids, 0, false);
            assert!(action_at(&kind, &rows, rows[11].rect.center(), sidebar).is_none(), "hidden tiles cannot receive drops");
            let third = rows[3].rect.right_center() - vec2(2.0, 0.0);
            assert!(matches!(action_at(&kind, &rows, third, sidebar), Some(Action::ReorderFavorite { index: 3, .. })), "column hit testing must use x, not just y");
            assert!(action_at(&DragKind::Tab("tab".into()), &rows, third, sidebar).is_none());
            assert!(action_at(&DragKind::Favorite(other.clone()), &rows, third, sidebar).is_none(), "a source from another profile must cancel");
            for frame in 1..40 { rows = favorite_rows(&ctx, viewport, &ids, frame, frame == 2); }
            assert!(viewport.contains_rect(rows[11].rect), "scroll must expose the last tile");
            assert!(!viewport.intersects(rows[0].rect));
            let pos = rows[11].rect.right_center() - vec2(2.0, 0.0);
            for invalid in [pos2(sidebar.right() + 20.0, pos.y), pos2(pos.x, viewport.bottom() + 1.0), rows[0].rect.center()] {
                assert!(action_at(&kind, &rows, invalid, sidebar).is_none());
            }
            let Some(Action::ReorderFavorite { item, index }) = action_at(&kind, &rows, pos, sidebar) else { panic!("expected favorite move") };
            assert_eq!(index, 11);
            lib.reorder_favorite(&item, index).unwrap();
            lib = Library::open(lib.store).unwrap();
            let ordered: Vec<_> = lib.favorites(&profile).into_iter().map(|(id, _)| id).collect();
            let mut expected = ids[1..].to_vec();
            expected.push(ids[0].clone());
            assert_eq!(ordered, expected);
            let ctx = egui::Context::default();
            let rows = favorite_rows(&ctx, viewport, &ordered, 0, false);
            let pos = rows[0].rect.left_center() + vec2(2.0, 0.0);
            let Some(Action::ReorderFavorite { item, index }) = action_at(&kind, &rows, pos, sidebar) else { panic!("expected favorite move") };
            assert_eq!(index, 0);
            lib.reorder_favorite(&item, index).unwrap();
            let lib = Library::open(lib.store).unwrap();
            assert_eq!(lib.favorites(&profile).into_iter().map(|(id, _)| id).collect::<Vec<_>>(), ids);
            assert_eq!(lib.favorites(&other_profile).into_iter().map(|(id, _)| id).collect::<Vec<_>>(), [other]);
        }
    }

    #[test]
    fn favorite_pointer_gesture_dispatches_reorder_from_explicit_grid_slots() {
        for height in [420.0, 860.0] {
            for to_end in [false, true] {
                let ctx = egui::Context::default();
                let sidebar = Rect::from_min_size(Pos2::ZERO, vec2(252.0, height));
                let viewport = SidebarRegions::new(sidebar.with_min_y(96.0).with_max_y(height - 46.0), 4).favorites.unwrap();
                let ids = ["a", "b", "c", "d"];
                let mut rows: Vec<DropRow> = Vec::new();
                let mut drag = None;
                let mut actions = Vec::new();
                for frame in 0..6 {
                    let pos = if frame == 0 { Pos2::ZERO } else if frame <= 2 {
                        rows[if to_end { 0 } else { 3 }].rect.center()
                    } else if to_end {
                        rows[3].rect.right_center() - vec2(2.0, 0.0)
                    } else {
                        rows[0].rect.left_center() + vec2(2.0, 0.0)
                    };
                    let mut events = vec![egui::Event::PointerMoved(pos)];
                    if frame == 2 || frame == 4 { events.push(button_event(pos, frame == 2)); }
                    ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, height))), events, time: Some(frame as f64 / 60.0), ..Default::default() }, |ui| {
                        rows.clear();
                        favorite_grid(ui, viewport, Id::unique("favorite-gesture"), ids.len(), |ui, index, rect| {
                            let response = favorite_response(ui, rect, Id::unique(("gesture-tile", ids[index])), ids[index]);
                            rows.push(DropRow::favorite(index, ids[index], rect, ui.clip_rect()));
                            if response.drag_started() {
                                drag = Some(DragState { kind: DragKind::Favorite(ids[index].into()), start: rect.center(), active: false });
                            }
                        });
                        if let Some((kind, pos, false)) = ui.input(|input| drag_pointer(&mut drag, input)) {
                            if let Some(action) = action_at(&kind, &rows, pos, sidebar) { actions.push(action); }
                        }
                    }).textures_delta.clear();
                }
                assert!(drag.is_none());
                assert_eq!(actions.len(), 1);
                let Action::ReorderFavorite { item, index } = &actions[0] else { panic!("expected favorite move") };
                assert_eq!(item, if to_end { "a" } else { "d" });
                assert_eq!(*index, if to_end { 3 } else { 0 });
            }
        }
    }

    #[test]
    fn trailing_slots_and_title_clip_do_not_overlap_at_normal_or_compact_sizes() {
        for (width, height) in [(200.0, 420.0), (252.0, 420.0), (252.0, 860.0), (420.0, 860.0)] {
            for indent in [0.0, 14.0, 42.0] {
                let row = Rect::from_min_size(pos2(PAD + indent, height - 100.0), vec2(width - PAD * 2.0 - indent, ROW_H));
                for close in [false, true] {
                    for muted in [false, true] {
                        for split in [false, true] {
                            let slots = TabTrailingSlots::new(row, close, muted, split);
                            assert!(slots.title_right > row.left() + ITEM_TEXT_X);
                            let controls: Vec<_> = [slots.close, slots.mute, slots.split].into_iter().flatten().collect();
                            for (i, control) in controls.iter().enumerate() {
                                assert!(row.contains_rect(*control));
                                assert_eq!(control.center().y, row.center().y);
                                assert!(control.left() >= slots.title_right + TRAILING_CONTROL_GAP);
                                for other in &controls[i + 1..] { assert!(!control.intersects(*other)); }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn muted_active_tab_buttons_dispatch_distinct_actions_in_egui() {
        let pal = theme::palette(&crossbow_core::model::Theme::preset(0), false);
        for (width, height) in [(200.0, 420.0), (252.0, 860.0)] {
            for in_split in [false, true] {
                for click_mute in [false, true] {
                    let ctx = egui::Context::default();
                    window::install_fonts(&ctx);
                    let row = Rect::from_min_size(pos2(PAD, height - 100.0), vec2(width - PAD * 2.0, ROW_H));
                    let slots = TabTrailingSlots::new(row, true, true, in_split);
                    let pos = if click_mute { slots.mute } else { slots.close }.unwrap().center();
                    let mut actions = Vec::new();
                    for frame in 0..4 {
                        let mut events = vec![egui::Event::PointerMoved(pos)];
                        if frame == 2 || frame == 3 { events.push(button_event(pos, frame == 2)); }
                        ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, height))), events, ..Default::default() }, |ui| {
                            ui.interact(row, Id::unique("tab-control-probe"), Sense::click_and_drag());
                            if let Some(action) = slots.show(ui, "fixture", false, pal.text, &pal) { actions.push(action); }
                        }).textures_delta.clear();
                    }
                    assert_eq!(actions.len(), 1);
                    assert!(matches!((&actions[0], click_mute), (Action::ToggleMute(_), true) | (Action::CloseTab(_), false)));
                }
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum TabRowHit { Body, Close, Mute }

    struct TabRowFrame {
        interaction: TabRowInteraction,
        trailing: TabTrailingSlots,
        actions: Vec<Action>,
    }

    struct TabRowProbe {
        ctx: egui::Context,
        screen: Rect,
        viewport: Rect,
        state: (bool, bool, bool),
        frame: usize,
    }

    impl TabRowProbe {
        fn new(size: egui::Vec2, width: f32, state: (bool, bool, bool)) -> Self {
            let ctx = egui::Context::default();
            window::install_fonts(&ctx);
            Self { ctx, screen: Rect::from_min_size(Pos2::ZERO, size),
                viewport: Rect::from_min_max(pos2(0.0, 120.0), pos2(width, size.y - 46.0)), state, frame: 0 }
        }

        fn frame(&mut self, pos: Pos2, buttons: &[(egui::PointerButton, bool)]) -> TabRowFrame {
            let pal = theme::palette(&crossbow_core::model::Theme::preset(0), false);
            let mut events = vec![egui::Event::PointerMoved(pos)];
            events.extend(buttons.iter().map(|&(button, pressed)| egui::Event::PointerButton { pos, button, pressed, modifiers: Default::default() }));
            let mut target = None;
            let mut actions = Vec::new();
            self.ctx.run_ui(egui::RawInput { screen_rect: Some(self.screen), events, time: Some(self.frame as f64 / 60.0), ..Default::default() }, |ui| {
                let mut list = ui.new_child(egui::UiBuilder::new().max_rect(self.viewport).layer_id(ui.layer_id()));
                list.set_clip_rect(self.viewport.intersect(ui.clip_rect()));
                egui::ScrollArea::vertical().id_salt("tab-row-composition").auto_shrink([false, false]).show(&mut list, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for (tab, active) in [("survivor", true), ("target", false)] {
                        let (muted, split, pinned) = if active { (false, false, false) } else { self.state };
                        let interaction = TabRowInteraction::new(ui, PAD, self.viewport.width() - PAD * 2.0);
                        let trailing = interaction.trailing(active, muted, split);
                        let control = trailing.show(ui, tab, pinned, pal.text, &pal);
                        let closing = matches!(control, Some(Action::CloseTab(_)));
                        actions.extend(control);
                        if !closing {
                            label(ui, interaction.rect.left() + ITEM_TEXT_X, trailing.title_right, interaction.rect.center().y, tab, pal.text);
                            interaction.dispatch_row_actions(tab, &mut actions);
                        }
                        if !active { target = Some((interaction, trailing)); }
                    }
                });
            }).textures_delta.clear();
            self.frame += 1;
            let (interaction, trailing) = target.unwrap();
            TabRowFrame { interaction, trailing, actions }
        }

        fn click(&mut self, hit: TabRowHit, batched: bool) -> Vec<TabRowFrame> {
            let first = self.frame(self.screen.right_center() - vec2(10.0, 0.0), &[]);
            let body = first.interaction.rect.left_center() + vec2(ITEM_TEXT_X + 15.0, 0.0);
            let hovered = self.frame(body, &[]);
            let pos = match hit {
                TabRowHit::Body => body,
                TabRowHit::Close => hovered.trailing.close.unwrap().center(),
                TabRowHit::Mute => hovered.trailing.mute.unwrap().center(),
            };
            let mut frames = vec![first, hovered, self.frame(pos, &[])];
            if batched {
                frames.push(self.frame(pos, &[(egui::PointerButton::Primary, true), (egui::PointerButton::Primary, false)]));
            } else {
                frames.push(self.frame(pos, &[(egui::PointerButton::Primary, true)]));
                frames.push(self.frame(pos, &[]));
                frames.push(self.frame(pos, &[(egui::PointerButton::Primary, false)]));
            }
            frames.push(self.frame(pos, &[]));
            frames
        }
    }

    fn check_inactive_tab_control(hit: TabRowHit) {
        let mut failures = Vec::new();
        let mut cases = 0;
        for (size, width) in [(vec2(1360.0, 860.0), 252.0), (vec2(640.0, 420.0), 200.0)] {
            for muted in [false, true] {
                if matches!(hit, TabRowHit::Mute) && !muted { continue; }
                for split in [false, true] {
                    for pinned in [false, true] {
                        for batched in [false, true] {
                            cases += 1;
                            let mut probe = TabRowProbe::new(size, width, (muted, split, pinned));
                            let frames = probe.click(hit, batched);
                            let release = &frames[frames.len() - 2];
                            let actions: Vec<_> = frames.iter().flat_map(|frame| &frame.actions).collect();
                            let action_matches = actions.len() == 1 && match hit {
                                TabRowHit::Close => matches!(actions[0], Action::CloseTab(tab) if tab == "target"),
                                TabRowHit::Mute => matches!(actions[0], Action::ToggleMute(tab) if tab == "target"),
                                TabRowHit::Body => unreachable!(),
                            };
                            let stable = release.trailing.close == frames[2].trailing.close && release.trailing.mute == frames[2].trailing.mute;
                            let only_release = frames.iter().enumerate().all(|(index, frame)| index == frames.len() - 2 || frame.actions.is_empty());
                            if !action_matches || !stable || !only_release || release.interaction.response.hovered()
                                || !release.interaction.response.container_hovered() || frames.iter().any(|frame| frame.interaction.response.dragged()) {
                                failures.push(format!("{hit:?} {size:?} muted={muted} split={split} pinned={pinned} batched={batched}: actions={actions:?}, stable={stable}, row_hovered={}", release.interaction.response.hovered()));
                            }
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{} of {cases} control cases failed:\n{}", failures.len(), failures.join("\n"));
    }

    #[test]
    fn tab_row_inactive_close_survives_release_frame_composition() {
        check_inactive_tab_control(TabRowHit::Close);
    }

    #[test]
    fn tab_row_inactive_mute_survives_release_frame_composition() {
        check_inactive_tab_control(TabRowHit::Mute);
    }

    #[test]
    fn tab_row_body_click_and_middle_close_keep_their_actions() {
        for (size, width) in [(vec2(1360.0, 860.0), 252.0), (vec2(640.0, 420.0), 200.0)] {
            for muted in [false, true] {
                for split in [false, true] {
                    for pinned in [false, true] {
                        for batched in [false, true] {
                            let mut probe = TabRowProbe::new(size, width, (muted, split, pinned));
                            let frames = probe.click(TabRowHit::Body, batched);
                            let actions: Vec<_> = frames.iter().flat_map(|frame| &frame.actions).collect();
                            assert_eq!(actions.len(), 1);
                            assert!(matches!(actions[0], Action::ActivateTab(tab) if tab == "target"));
                            let pos = frames[0].interaction.rect.left_center() + vec2(ITEM_TEXT_X + 15.0, 0.0);
                            assert!(probe.frame(pos, &[(egui::PointerButton::Middle, true)]).actions.is_empty());
                            let release = probe.frame(pos, &[(egui::PointerButton::Middle, false)]);
                            assert_eq!(release.actions.len(), 1);
                            assert!(matches!(&release.actions[0], Action::CloseTab(tab) if tab == "target"));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn tab_row_controls_do_not_reveal_or_dispatch_through_the_scroll_clip() {
        for (size, width) in [(vec2(1360.0, 860.0), 252.0), (vec2(640.0, 420.0), 200.0)] {
            for muted in [false, true] {
                for split in [false, true] {
                    for pinned in [false, true] {
                        let mut probe = TabRowProbe::new(size, width, (muted, split, pinned));
                        probe.viewport.max.y = probe.viewport.top() + ROW_H + 4.0;
                        let first = probe.frame(size.to_pos2() - vec2(10.0, 10.0), &[]);
                        let slots = TabTrailingSlots::new(first.interaction.rect, true, muted, split);
                        for control in [slots.close, slots.mute].into_iter().flatten() {
                            let pos = control.center();
                            assert!(!probe.viewport.contains(pos));
                            for buttons in [vec![], vec![(egui::PointerButton::Primary, true)], vec![], vec![(egui::PointerButton::Primary, false)], vec![],
                                vec![(egui::PointerButton::Primary, true), (egui::PointerButton::Primary, false)], vec![]] {
                                let frame = probe.frame(pos, &buttons);
                                assert!(!frame.interaction.response.container_hovered());
                                assert!(frame.trailing.close.is_none());
                                assert!(frame.actions.is_empty());
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn tab_row_drag_and_cancel_do_not_dispatch_controls_and_allow_retry() {
        for (size, width) in [(vec2(1360.0, 860.0), 252.0), (vec2(640.0, 420.0), 200.0)] {
            for state in [(false, false, false), (true, true, true)] {
                let mut probe = TabRowProbe::new(size, width, state);
                let first = probe.frame(size.to_pos2() - vec2(10.0, 10.0), &[]);
                let body = first.interaction.rect.left_center() + vec2(ITEM_TEXT_X + 15.0, 0.0);
                let hovered = probe.frame(body, &[]);
                let close = hovered.trailing.close.unwrap().center();
                assert!(probe.frame(body, &[(egui::PointerButton::Primary, true)]).actions.is_empty());
                let dragged = probe.frame(body + vec2(25.0, 0.0), &[]);
                assert!(dragged.interaction.response.drag_started());
                assert!(dragged.trailing.close.is_none());
                assert!(dragged.actions.is_empty());
                let over_close = probe.frame(close, &[]);
                assert!(over_close.interaction.response.dragged());
                assert!(over_close.trailing.close.is_none());
                assert!(over_close.actions.is_empty());
                assert!(probe.frame(close, &[(egui::PointerButton::Primary, false)]).actions.is_empty());

                let survivor = body - vec2(0.0, ROW_H + 2.0);
                probe.frame(survivor, &[]);
                assert!(probe.frame(survivor, &[(egui::PointerButton::Primary, true)]).actions.is_empty());
                let other_drag = probe.frame(close, &[]);
                assert!(probe.ctx.dragged_id().is_some());
                assert!(!other_drag.interaction.response.dragged());
                assert!(other_drag.trailing.close.is_none());
                assert!(other_drag.actions.is_empty());
                assert!(probe.frame(close, &[(egui::PointerButton::Primary, false)]).actions.is_empty());

                // Cancel a Close press outside the row, then retry through the same controls.
                probe.frame(body, &[]);
                probe.frame(close, &[]);
                assert!(probe.frame(close, &[(egui::PointerButton::Primary, true)]).actions.is_empty());
                let outside = pos2(width + 80.0, close.y);
                assert!(probe.frame(outside, &[]).actions.is_empty());
                assert!(probe.frame(outside, &[(egui::PointerButton::Primary, false)]).actions.is_empty());
                let retry = probe.click(TabRowHit::Close, false);
                let actions: Vec<_> = retry.iter().flat_map(|frame| &frame.actions).collect();
                assert_eq!(actions.len(), 1);
                assert!(matches!(actions[0], Action::CloseTab(tab) if tab == "target"));
            }
        }
    }

    #[test]
    fn floating_and_docked_headers_share_the_native_control_row() {
        for height in [420.0, 860.0] {
            let docked = SidebarHeader::new(Rect::from_min_size(Pos2::ZERO, vec2(252.0, height)), true);
            let floating_rect = Rect::from_min_size(pos2(super::super::content::GAP, 0.0), vec2(252.0, height - super::super::content::GAP));
            let floating = SidebarHeader::new(floating_rect, true);
            assert_eq!(docked.row.center_y, window::NATIVE_CONTROL_ROW / 2.0);
            assert_eq!(floating.row.center_y, docked.row.center_y);
            if cfg!(target_os = "macos") {
                assert_eq!(floating.row.leading_control(0), docked.row.leading_control(0));
            }
            assert!(floating.rect.contains_rect(floating.row.leading_control(0)));
            let separate = SidebarHeader::new(floating_rect, false);
            assert_eq!(separate.row.leading_control(0).center().x, floating_rect.left() + PAD + 13.0);
            assert_eq!(separate.row.center_y, separate.rect.center().y);
        }
    }

    #[test]
    fn the_hidden_sidebars_strip_keeps_the_header_controls_in_place() {
        for (width, height) in [(640.0, 420.0), (1360.0, 860.0)] {
            let header = SidebarHeader::new(Rect::from_min_size(Pos2::ZERO, vec2(252.0, height)), true);
            let strip = Rect::from_min_size(Pos2::ZERO, vec2(width, window::NATIVE_CONTROL_ROW));
            let controls = super::super::content::top_strip_controls(strip);
            assert_eq!(controls[0], header.row.leading_control(0), "the sidebar toggle does not move as the sidebar hides at {width}");
            for (index, control) in controls.iter().enumerate() {
                assert_eq!(control.size(), vec2(window::ROW_CONTROL, window::ROW_CONTROL));
                assert_eq!(control.center().y, header.row.center_y);
                assert_eq!(control.left(), controls[0].left() + index as f32 * window::ROW_PITCH);
                assert!(control.left() >= window::traffic_light_inset(), "clear of the traffic lights");
            }
        }
    }

    #[test]
    fn favorites_never_displace_the_space_tabs_or_bottom_bar() {
        for height in [420.0, 600.0, 860.0] {
            let body = Rect::from_min_max(pos2(0.0, 96.0), pos2(256.0, height - 46.0));
            for count in [0, 1, 4, 5, 8, 9, 32, 100] {
                let regions = SidebarRegions::new(body, count);
                assert!(body.contains_rect(regions.tabs));
                assert_eq!(regions.tabs.bottom(), height - 46.0);
                assert!(regions.tabs.height() >= ROW_H * 3.0 + 50.0);
                if let Some(favorites) = regions.favorites {
                    assert!(body.contains_rect(favorites));
                    assert!(favorites.height() <= favorite_grid_height(2));
                    assert_eq!((favorites.height() + FAVORITE_GAP) % (FAVORITE_HEIGHT + FAVORITE_GAP), 0.0);
                    assert_eq!(regions.tabs.top() - favorites.bottom(), SECTION_GAP);
                } else {
                    assert_eq!(count, 0);
                    assert_eq!(regions.tabs, body);
                }
            }
        }
    }

    #[test]
    fn favorite_grid_scrolls_and_focus_reveals_hidden_tiles() {
        // A compact floating sidebar loses 16 pt to its outer insets and fits one row.
        for height in [404.0, 420.0, 860.0] {
            let ctx = egui::Context::default();
            let screen = Rect::from_min_size(Pos2::ZERO, vec2(640.0, height));
            let body = Rect::from_min_max(pos2(0.0, 96.0), pos2(256.0, height - 46.0));
            let viewport = SidebarRegions::new(body, 32).favorites.unwrap();
            let mut offset = 0.0;
            let mut last_tile = Rect::NOTHING;
            let mut clicked = None;
            for frame in 0..108 {
                let mut events = vec![egui::Event::PointerMoved(viewport.center())];
                if frame == 3 {
                    events.push(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -100.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() });
                }
                if (38..=104).contains(&frame) {
                    events.push(egui::Event::Key { key: egui::Key::Tab, physical_key: None, pressed: frame % 2 == 0, repeat: false, modifiers: egui::Modifiers { shift: frame == 102 || frame == 103, ..Default::default() } });
                }
                if frame == 106 {
                    events.push(egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
                }
                ctx.run_ui(egui::RawInput { screen_rect: Some(screen), time: Some(frame as f64 / 60.0), events, ..Default::default() }, |ui| {
                    let result = favorite_grid(ui, viewport, Id::unique("favorite-grid-probe"), 32, |ui, index, rect| {
                        let response = favorite_response(ui, rect, Id::unique(("favorite-probe", index)), "Synthetic favorite");
                        if index == 31 { last_tile = rect; }
                        if response.clicked() { clicked = Some(index); }
                        assert!(viewport.contains_rect(ui.clip_rect()));
                        assert_eq!(rect.height(), FAVORITE_HEIGHT);
                    });
                    offset = result.state.clamped_offset().y;
                }).textures_delta.clear();
                if frame == 37 { assert!(offset > 90.0, "wheel must move favorites, offset={offset}"); }
                if frame == 101 || frame == 105 {
                    assert!(viewport.contains_rect(last_tile), "Tab must reveal the last tile: {last_tile:?} in {viewport:?}");
                    assert_eq!(ctx.memory(|memory| memory.focused()), Some(Id::unique(("favorite-probe", 31usize))));
                }
                if frame == 103 {
                    assert_eq!(ctx.memory(|memory| memory.focused()), Some(Id::unique(("favorite-probe", 30usize))), "Shift+Tab must move focus backward");
                }
            }
            assert!(viewport.contains_rect(last_tile), "keyboard focus must reveal the last tile: {last_tile:?} in {viewport:?}");
            assert_eq!(clicked, Some(31), "the focused tile must activate with Return");
        }
    }
}
