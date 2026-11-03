//! Hover previews, as Arc's Show Hover Preview. Resting the pointer on a
//! collapsed folder for 600 ms, or focusing it with the keyboard, lists its first
//! pinned tabs; resting it on a pinned tab or favorite that isn't loaded
//! shows its title, site and when it was last opened. Nothing is loaded to
//! make a preview, and Crossbow keeps no page thumbnails to show.

use crate::app::Action;
use crate::favicons::Favicons;
use crate::tabs::Tab;
use crate::ui::icons::{self, Icon};
use crate::ui::theme::Palette;
use crossbow_core::library::{Library, PinnedNode};
use crossbow_core::search::{display_url, host_of};
use egui::{Color32, FontId, Id, Pos2, Rect, Sense, Ui, Vec2, pos2, vec2};
use std::time::Duration;

pub const DELAY: f64 = 0.6;
/// Time to move the pointer from the row into the popover.
pub const GRACE: f64 = 0.3;
pub const MAX_ITEMS: usize = 8;
/// Between the sidebar and the popover, and the popover and the window's edge.
const GAP: f32 = 6.0;
const MARGIN: f32 = 8.0;
const WIDTH: f32 = 280.0;
/// Narrower than this beside the sidebar, no popover is shown. The smallest
/// window and widest sidebar leave about 198 points.
const MIN_WIDTH: f32 = 160.0;
const PADDING: f32 = 6.0;
const HEADER: f32 = 26.0;
const ENTRY: f32 = 28.0;
const MORE: f32 = 22.0;
const LINE: f32 = 19.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subject {
    Folder(String),
    /// A pinned tab or favorite that isn't loaded.
    Item(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub item: String,
    pub url: String,
    pub title: String,
    pub host: String,
}

/// What a preview shows, read once when it opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Folder { name: String, entries: Vec<Entry>, more: usize },
    Item { title: String, host: String, opened: Option<u64> },
}

/// What `subject` shows for `profile`: a collapsed folder's first pinned tabs
/// at any depth, or an item's title, site and last opening, from its tab or this
/// profile's history. None when it is gone or another profile's.
pub fn content(lib: &Library, tabs: &[Tab], profile: &str, subject: &Subject) -> Option<Content> {
    match subject {
        Subject::Folder(id) => {
            let folder = lib.folders.get(id).filter(|folder| folder.collapsed)?;
            if !lib.profiles.contains_key(profile) || lib.spaces.get(&folder.space_id)?.profile_id != profile { return None; }
            let mut entries = Vec::new();
            collect(lib, &folder.space_id, id, &mut entries);
            let more = entries.len().saturating_sub(MAX_ITEMS);
            entries.truncate(MAX_ITEMS);
            Some(Content::Folder { name: folder.name.clone(), entries, more })
        }
        Subject::Item(id) => {
            let item = lib.items.get(id).filter(|item| crate::app::item_in_profile(lib, item, profile))?;
            let tab = tabs.iter().filter(|tab| tab.item_id.as_deref() == Some(id.as_str())).map(|tab| tab.last_active_ms).filter(|ms| *ms > 0).max();
            let visit = lib.store.last_visit(profile, &item.url).ok().flatten();
            Some(Content::Item { title: title_of(item.display_title(), &item.url), host: host_of(&item.url).unwrap_or_default(), opened: tab.max(visit) })
        }
    }
}

/// A folder's pinned tabs, depth first, in sidebar order.
fn collect(lib: &Library, space: &str, folder: &str, out: &mut Vec<Entry>) {
    for node in lib.pinned_children(space, Some(folder)) {
        match node {
            PinnedNode::Item(id, item) => out.push(Entry { title: title_of(item.display_title(), &item.url), host: host_of(&item.url).unwrap_or_default(), url: item.url, item: id }),
            PinnedNode::Folder(id, _) => collect(lib, space, &id, out),
        }
    }
}

fn title_of(title: &str, url: &str) -> String {
    if title.is_empty() { display_url(url) } else { title.to_owned() }
}

/// "Last opened 3 days ago", from the newest of the item's tab and history.
pub fn opened_label(opened: Option<u64>, now_ms: u64) -> String {
    let Some(ms) = opened else { return "No visit on this Mac yet".into() };
    let secs = now_ms.saturating_sub(ms) / 1000;
    match secs {
        0..=59 => "Last opened just now".into(),
        60..=3599 => format!("Last opened {} min ago", secs / 60),
        3600..=86_399 => format!("Last opened {} h ago", secs / 3600),
        86_400..=172_799 => "Last opened yesterday".into(),
        _ => format!("Last opened {} days ago", secs / 86_400),
    }
}

/// The popover beside the sidebar, level with the row and inside the window.
/// None when the window leaves too little room.
pub fn place(row: Rect, sidebar: Rect, window: Rect, size: Vec2, on_right: bool) -> Option<Rect> {
    let room = if on_right { sidebar.left() - GAP - (window.left() + MARGIN) } else { window.right() - MARGIN - (sidebar.right() + GAP) };
    let width = size.x.min(room);
    if width < MIN_WIDTH { return None; }
    let height = size.y.min(window.height() - MARGIN * 2.0);
    let x = if on_right { sidebar.left() - GAP - width } else { sidebar.right() + GAP };
    let y = row.top().min(window.bottom() - MARGIN - height).max(window.top() + MARGIN);
    Some(Rect::from_min_size(pos2(x, y), vec2(width, height)))
}

fn size(content: &Content) -> Vec2 {
    let height = match content {
        Content::Folder { entries, more, .. } => PADDING * 2.0 + HEADER + ENTRY * entries.len().max(1) as f32 + if *more > 0 { MORE } else { 0.0 },
        Content::Item { .. } => PADDING * 2.0 + 8.0 + LINE * 3.0,
    };
    vec2(WIDTH, height)
}

/// Where a preview is in its life.
struct Open {
    subject: Subject,
    /// The visible part of its row.
    row: Rect,
    /// The Space it was opened in: a Space switch closes it.
    space: String,
    since: f64,
    keyboard: bool,
    /// The pass its row last offered it, by hover or focus.
    offered: u64,
    /// The last pass the sidebar drew; a hidden sidebar draws none.
    drawn: Option<u64>,
    /// When neither the row nor the popover had the pointer.
    left: Option<f64>,
    content: Option<Content>,
    popover: Option<Rect>,
}

/// One frame's surroundings for the preview the sidebar's rows offered.
pub struct Cx<'a> {
    pub lib: &'a Library,
    pub tabs: &'a [Tab],
    pub favicons: &'a mut Favicons,
    pub profile: &'a str,
    pub space: &'a str,
    pub sidebar: Rect,
    pub on_right: bool,
    /// A drag started, or a menu is open.
    pub busy: bool,
    pub now: f64,
    pub now_ms: u64,
    pub pal: &'a Palette,
}

#[derive(Default)]
pub struct HoverPreviews {
    open: Option<Open>,
}

impl HoverPreviews {
    /// A row offers its preview each frame it is hovered, or focused when
    /// `keyboard` allows it. Rows scrolled out of view offer nothing.
    pub fn offer(&mut self, ui: &Ui, subject: Subject, row: Rect, response: &egui::Response, space: &str, keyboard: bool) {
        let pointer = response.hovered();
        if !pointer && !(keyboard && response.has_focus()) { return; }
        let row = row.intersect(ui.clip_rect());
        if !row.is_positive() { return; }
        let pass = ui.ctx().cumulative_frame_nr();
        if let Some(open) = self.open.as_mut().filter(|open| open.subject == subject) {
            open.row = row;
            open.offered = pass;
            return;
        }
        // Focus opens a preview only as it arrives, so a dismissed one stays
        // closed, and never takes over the pointer's.
        if !pointer && (!response.gained_focus() || self.open.as_ref().is_some_and(|open| !open.keyboard)) { return; }
        let since = ui.input(|input| input.time);
        self.open = Some(Open { subject, row, space: space.to_owned(), since, keyboard: !pointer, offered: pass, drawn: None, left: None, content: None, popover: None });
    }

    /// Show the offered preview once it is due; dismiss it on pointer exit,
    /// scroll, a drag, a menu or a Space switch. A click on a listed tab
    /// returns the action that opens it.
    pub fn finish(&mut self, ui: &Ui, mut cx: Cx<'_>) -> Option<Action> {
        let ctx = ui.ctx().clone();
        let pass = ctx.cumulative_frame_nr();
        let open = self.open.as_mut()?;
        let (pointer, scrolled) = ctx.input(|input| (input.pointer.hover_pos(), input.smooth_scroll_delta != Vec2::ZERO || input.events.iter().any(|event| matches!(event, egui::Event::MouseWheel { .. }))));
        let hidden = open.drawn.is_some_and(|drawn| drawn + 1 < pass);
        if hidden || scrolled || cx.busy || open.space != cx.space {
            self.open = None;
            return None;
        }
        open.drawn = Some(pass);
        let over_popover = pointer.zip(open.popover).is_some_and(|(pos, popover)| popover.contains(pos));
        if open.offered == pass || over_popover {
            open.left = None;
        } else {
            let left = *open.left.get_or_insert(cx.now);
            if cx.now - left >= GRACE {
                self.open = None;
                return None;
            }
            ctx.request_repaint_after(Duration::from_secs_f64(GRACE - (cx.now - left)));
        }
        if !open.keyboard && cx.now - open.since < DELAY {
            ctx.request_repaint_after(Duration::from_secs_f64(DELAY - (cx.now - open.since)));
            return None;
        }
        if open.content.is_none() {
            open.content = content(cx.lib, cx.tabs, cx.profile, &open.subject);
        }
        let placed = open.content.as_ref().and_then(|content| place(open.row, cx.sidebar, ctx.content_rect(), size(content), cx.on_right));
        let (Some(content), Some(rect)) = (open.content.as_ref(), placed) else {
            self.open = None;
            return None;
        };
        open.popover = Some(rect);
        let clicked = egui::Area::new(Id::unique("hover-preview")).order(egui::Order::Tooltip).fixed_pos(rect.min)
            .constrain(false).fade_in(false).sense(Sense::hover())
            .show(&ctx, |ui| paint(ui, rect, content, &mut cx)).inner;
        let item = clicked?;
        self.open = None;
        Some(Action::OpenItem(item))
    }

    /// Escape: true when a shown popover closed, so nothing else takes it.
    pub fn dismiss(&mut self) -> bool {
        let shown = self.shown().is_some();
        if shown { self.close(); }
        shown
    }

    pub fn close(&mut self) {
        self.open = None;
    }

    /// Whether the shown popover has `pos`, which keeps a floating sidebar open.
    pub fn contains(&self, pos: Pos2) -> bool {
        self.shown().is_some_and(|popover| popover.contains(pos))
    }

    pub fn shown(&self) -> Option<Rect> {
        self.open.as_ref().and_then(|open| open.popover)
    }
}

/// One line of text from `left`, elided before `right`.
fn line(p: &egui::Painter, text: &str, left: f32, right: f32, cy: f32, size: f32, color: Color32) -> Rect {
    let mut job = egui::text::LayoutJob::single_section(text.to_owned(), egui::TextFormat::simple(FontId::proportional(size), color));
    job.wrap = egui::text::TextWrapping::truncate_at_width((right - left).max(0.0));
    let galley = p.layout_job(job);
    let rect = Rect::from_min_size(pos2(left, cy - galley.size().y / 2.0), galley.size());
    p.galley(rect.min, galley, color);
    rect
}

/// The popover's card and content, like the context menus. Returns the
/// listed tab that was clicked.
fn paint(ui: &mut Ui, rect: Rect, content: &Content, cx: &mut Cx<'_>) -> Option<String> {
    ui.set_min_size(rect.size());
    let pal = cx.pal;
    let frame = crate::ui::overlays::menu_frame(pal);
    let p = ui.painter().clone();
    p.add(frame.shadow.as_shape(rect, frame.corner_radius));
    p.rect(rect, frame.corner_radius, frame.fill, frame.stroke, egui::StrokeKind::Inside);
    let inner = rect.shrink(PADDING);
    match content {
        Content::Folder { name, entries, more } => {
            let header = Rect::from_min_size(inner.min, vec2(inner.width(), HEADER));
            icons::draw(&p, Rect::from_center_size(pos2(header.left() + 12.0, header.center().y), vec2(12.0, 12.0)), Icon::Folder, pal.panel_dim);
            line(&p, name, header.left() + 26.0, header.right() - 8.0, header.center().y, 12.5, pal.panel_dim);
            let mut clicked = None;
            for (index, entry) in entries.iter().enumerate() {
                let row = Rect::from_min_size(pos2(inner.left(), header.bottom() + index as f32 * ENTRY), vec2(inner.width(), ENTRY));
                let response = ui.interact(row, Id::unique(("hover-preview-entry", &entry.item)), Sense::click());
                let lit = response.hovered();
                if lit { p.rect_filled(row, 6, pal.accent.gamma_multiply(0.92)); }
                let (text, dim) = if lit { (Color32::WHITE, Color32::from_white_alpha(200)) } else { (pal.panel_text, pal.panel_dim) };
                crate::ui::sidebar::paint_favicon(cx.favicons, ui, &entry.url, Rect::from_center_size(pos2(row.left() + 14.0, row.center().y), vec2(16.0, 16.0)), pal);
                let mut job = egui::text::LayoutJob::single_section(entry.host.clone(), egui::TextFormat::simple(FontId::proportional(11.5), dim));
                job.wrap = egui::text::TextWrapping::truncate_at_width(row.width() * 0.4);
                let host = p.layout_job(job);
                let host_left = row.right() - 8.0 - host.size().x;
                p.galley(pos2(host_left, row.center().y - host.size().y / 2.0), host, dim);
                line(&p, &entry.title, row.left() + 30.0, host_left - 8.0, row.center().y, 13.0, text);
                if response.clicked() { clicked = Some(entry.item.clone()); }
            }
            if entries.is_empty() {
                line(&p, "No pinned tabs", inner.left() + 10.0, inner.right() - 8.0, header.bottom() + ENTRY / 2.0, 12.5, pal.panel_dim);
            }
            if *more > 0 {
                let top = header.bottom() + ENTRY * entries.len() as f32;
                line(&p, &format!("+{more} more"), inner.left() + 10.0, inner.right() - 8.0, top + MORE / 2.0, 12.0, pal.panel_dim);
            }
            clicked
        }
        Content::Item { title, host, opened } => {
            let (left, right) = (inner.left() + 8.0, inner.right() - 8.0);
            let top = inner.top() + 4.0;
            line(&p, title, left, right, top + LINE / 2.0, 13.5, pal.panel_text);
            line(&p, host, left, right, top + LINE * 1.5, 12.0, pal.panel_dim);
            line(&p, &opened_label(*opened, cx.now_ms), left, right, top + LINE * 2.5, 12.0, pal.panel_dim);
            None
        }
    }
}

#[cfg(test)]
#[path = "hover_preview_tests.rs"]
mod tests;
