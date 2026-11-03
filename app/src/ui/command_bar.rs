//! The command bar (⌘T / ⌘L): one field for URLs, searches, open tabs, saved
//! items, history, app commands and, when a profile allows it, its search
//! engine's suggestions.
//!
//! Tab scopes the field: to an engine whose name or keyword the text begins,
//! or, with Ask AI in the Command Bar on, to the saved local AI. Backspace in
//! the empty field or Escape leaves the scope.

use crate::app::{Action, CrossbowApp, NEW_TAB_URL};
use crate::search_engines::{BarSearch, SearchPreferences};
use crate::shortcuts::{Command, Shortcuts};
use crate::suggestions::Suggestions;
use crate::tabs::FAVORITES_SPACE;
use crate::ui::icons::{self, Icon};
use crate::ui::sidebar;
use crossbow_core::library::Library;
use crossbow_core::search::{self, Candidate, Engine, Suggestion, SuggestionKind};
use egui::{Color32, FontId, Id, Key, Rect, Sense, Ui, pos2, vec2};
use std::sync::Arc;

/// App commands reachable from the bar: (id, title, keywords, shortcut action).
const COMMANDS: &[(&str, &str, &str, Option<Command>)] = &[
    ("new-space", "New Space", "create space", None),
    ("new-folder", "New Folder", "pinned folder", Some(Command::NewFolder)),
    ("toggle-sidebar", "Toggle Sidebar", "hide show", Some(Command::ToggleSidebar)),
    ("settings", "Settings", "preferences options", Some(Command::Settings)),
    ("ask-ai", "Ask AI", "chat assistant conversation", None),
    ("tidy-today", "Tidy Today", "tidy tabs organize group folders ai", None),
    ("clear-today", "Clear Unpinned Tabs", "archive close all", Some(Command::ClearToday)),
    ("reopen", "Reopen Closed Tab", "undo close restore", Some(Command::ReopenClosed)),
    ("new-little-window", "New Little Window", "small popup mini", Some(Command::NewLittleWindow)),
    ("show-little-windows", "Show All Little Windows", "small popup mini bring front", Some(Command::ShowLittleWindows)),
    ("close-little-windows", "Close All Little Windows", "small popup mini archive", Some(Command::CloseLittleWindows)),
    // New Document once the profile has a document app, Choose a Document App until then.
    (NEW_DOCUMENT, "New Document", "doc docs sheet slides word notion blank write create", Some(Command::NewDocument)),
    (CHOOSE_DOCUMENT_APP, crate::new_document::CHOOSE, "new document docs sheet slides word notion default app", None),
    ("copy-url", "Copy URL", "link share", Some(Command::CopyUrl)),
    ("split", "Add Split View", "side by side", Some(Command::SplitView)),
    ("close-split", "Close Split View", "unsplit", None),
    ("pin", "Pin / Unpin Tab", "keep save", Some(Command::PinTab)),
    ("favorite", "Add to Favorites", "favourite star", Some(Command::Favorite)),
    ("find", "Find on Page", "search text", Some(Command::Find)),
    ("hard-reload", "Hard Reload", "refresh cache", Some(Command::HardReload)),
    ("devtools", "Developer Tools", "inspect console", Some(Command::DevTools)),
    ("view-source", "View Page Source", "html", Some(Command::ViewSource)),
    ("print", "Print Page", "pdf", Some(Command::Print)),
    ("capture", crate::app::capture_host::CAPTURE, "screenshot snip image picture area region", Some(Command::Capture)),
    ("captures", crate::app::capture_host::SHOW_CAPTURES, "screenshots images pictures library saved", None),
    ("archive", "Show Archive", "history closed tabs library", None),
    ("mute", "Mute / Unmute Tab", "sound audio", None),
    // Only while the current tab has a video.
    (PIP, crate::app::pip::PICTURE_IN_PICTURE, "pip video float player window", None),
    // Translate Page until the tab shows a translation, then Show Original.
    (TRANSLATE, crate::ui::translate::TRANSLATE_PAGE, "translate language foreign", None),
    (ORIGINAL, crate::ui::translate::SHOW_ORIGINAL, "translate page untranslate original language", None),
    ("sync", "Account & Sync", "sign in login cloud", None),
    ("welcome", "Welcome Tour", "onboarding setup import", None),
];

/// First words of command titles. An engine keyword can't be one, so typing a
/// command never turns into a keyword search.
pub(crate) fn command_words() -> Vec<&'static str> {
    COMMANDS.iter().filter_map(|(_, title, _, _)| title.split_whitespace().next()).collect()
}

/// The shortcut shown beside an app command.
fn command_badge(shortcuts: &Shortcuts, action_id: &str) -> Option<String> {
    COMMANDS.iter().find(|c| c.0 == action_id).and_then(|c| c.3).map(|command| shortcuts.hint(command)).filter(|hint| !hint.is_empty())
}

const PIP: &str = "picture-in-picture";
const TRANSLATE: &str = "translate-page";
const ORIGINAL: &str = "show-original";
const NEW_DOCUMENT: &str = "new-document";
const CHOOSE_DOCUMENT_APP: &str = "choose-document-app";

/// The app commands the bar offers: Picture in Picture only when the
/// current tab has a video, Show Original instead of Translate Page while
/// it shows a translation, and Choose a Document App instead of New
/// Document until the profile has one.
fn offered_commands(video: bool, translated: bool, document: bool) -> impl Iterator<Item = &'static (&'static str, &'static str, &'static str, Option<Command>)> {
    COMMANDS.iter().filter(move |(id, ..)| (*id != PIP || video) && (*id != TRANSLATE || !translated) && (*id != ORIGINAL || translated)
        && (*id != NEW_DOCUMENT || document) && (*id != CHOOSE_DOCUMENT_APP || !document))
}

/// The Ask row's action. It is not in COMMANDS: it needs typed text.
pub(crate) const ASK_ROW: &str = "ask-typed";

/// Where Tab sends what is typed: one engine, or the saved local AI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Site(Engine),
    /// Names the provider this profile consented to.
    Ask(&'static str),
}

impl Target {
    /// The chip's text.
    fn name(&self) -> &str {
        match self {
            Target::Site(engine) => engine.name(),
            Target::Ask(provider) => provider,
        }
    }

    /// The field's placeholder, and what Return does.
    fn action(&self) -> String {
        match self {
            Target::Site(engine) => format!("Search {}", engine.name()),
            Target::Ask(provider) => format!("Ask {provider}"),
        }
    }
}

/// What Tab entered. It lasts while the bar's profile and Space are the ones
/// it was entered in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub target: Target,
    profile: String,
    space: String,
    /// What was typed before Tab, put back when an empty site search is left.
    before: String,
}

const FIELD_HEIGHT: f32 = 56.0;
const RESULT_HEIGHT: f32 = 44.0;
const SURFACE_INSET: f32 = 8.0;
const ICON_COLUMN: f32 = 28.0;
const TEXT_COLUMN: f32 = 48.0;
const TRAILING_INSET: f32 = 20.0;
const CHIP_HEIGHT: f32 = 26.0;
const CHIP_GAP: f32 = 8.0;

struct BarLayout {
    rect: Rect,
    field: Rect,
    results: Rect,
}

#[derive(Default)]
struct BarKeys {
    step: isize,
    /// Plain Tab presses: they enter an offered scope, or else move down.
    tab: isize,
    submit: Option<bool>,
    /// ⇧ held with Return: open here, whatever the site's rule.
    here: bool,
}

impl BarKeys {
    fn take(ctx: &egui::Context) -> Self {
        let mut keys = Self::default();
        ctx.input_mut(|input| input.events.retain(|event| {
            let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { return true };
            let plain = modifiers.matches_exact(egui::Modifiers::NONE);
            match key {
                Key::ArrowDown if plain => keys.step += 1,
                Key::Tab if plain => keys.tab += 1,
                Key::ArrowUp if plain => keys.step -= 1,
                Key::Tab if modifiers.matches_exact(egui::Modifiers::SHIFT) => keys.step -= 1,
                Key::Enter if plain => keys.submit = Some(false),
                Key::Enter if modifiers.matches_exact(egui::Modifiers::COMMAND) || modifiers.matches_exact(egui::Modifiers::ALT) => keys.submit = Some(true),
                Key::Enter if modifiers.matches_exact(egui::Modifiers::SHIFT) => (keys.submit, keys.here) = (Some(false), true),
                Key::Enter if modifiers.matches_exact(egui::Modifiers::SHIFT | egui::Modifiers::COMMAND) || modifiers.matches_exact(egui::Modifiers::SHIFT | egui::Modifiers::ALT) => (keys.submit, keys.here) = (Some(true), true),
                _ => return true,
            }
            false
        }));
        keys
    }
}

impl BarLayout {
    fn new(screen: Rect, rows: usize) -> Self {
        let width = (screen.width() - 48.0).clamp(320.0, 680.0);
        let top = screen.top() + (screen.height() * 0.2).min(180.0);
        let available = (screen.bottom() - 24.0 - top - FIELD_HEIGHT - SURFACE_INSET * 2.0).max(0.0);
        let visible_rows = rows.min((available / RESULT_HEIGHT).floor() as usize);
        let height = FIELD_HEIGHT + if rows > 0 { visible_rows as f32 * RESULT_HEIGHT + SURFACE_INSET * 2.0 } else { 0.0 };
        let rect = Rect::from_min_size(pos2(screen.center().x - width / 2.0, top), vec2(width, height));
        let field = Rect::from_min_size(rect.min, vec2(width, FIELD_HEIGHT));
        let results = Rect::from_min_max(pos2(rect.left() + SURFACE_INSET, field.bottom() + SURFACE_INSET), rect.max - vec2(SURFACE_INSET, SURFACE_INSET));
        Self { rect, field, results }
    }

    fn edit_rect(&self) -> Rect {
        Rect::from_min_max(self.field.min + vec2(TEXT_COLUMN, 0.0), self.field.max - vec2(TRAILING_INSET, 0.0))
    }
}

fn search_field(ui: &mut Ui, rect: Rect, query: &mut String, hint: &str, pal: &crate::ui::theme::Palette) -> egui::text_edit::TextEditOutput {
    egui::TextEdit::singleline(query)
        .id(Id::unique("cmd-field"))
        .font(FontId::proportional(18.0))
        .lock_focus(true)
        .text_color(pal.panel_text)
        .hint_text(egui::RichText::new(hint).font(FontId::proportional(18.0)).color(pal.panel_dim))
        .frame(egui::Frame::NONE)
        .margin(egui::Margin::ZERO)
        .desired_width(rect.width())
        .min_size(rect.size())
        .vertical_align(egui::Align::Center)
        .show(&mut ui.new_child(egui::UiBuilder::new().max_rect(rect)))
}

fn focused_search_field(ui: &mut Ui, layout: &BarLayout, bar: &mut crate::app::CommandBar, refocus: bool, pal: &crate::ui::theme::Palette) -> egui::text_edit::TextEditOutput {
    let ctx = ui.ctx();
    // The invisible first sizing pass can release focus again at frame end.
    if bar.needs_focus || refocus || !ctx.memory(|memory| memory.has_focus(Id::unique("cmd-field"))) {
        ctx.memory_mut(|memory| {
            let field = Id::unique("cmd-field");
            if !memory.has_focus(field) { memory.request_focus(field); }
        });
        if bar.needs_focus && bar.edit_current {
            let mut state = egui::TextEdit::load_state(ctx, Id::unique("cmd-field")).unwrap_or_default();
            let n = bar.query.chars().count();
            state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(n))));
            state.store(ctx, Id::unique("cmd-field"));
        }
        bar.needs_focus = false;
    }
    let mut rect = layout.edit_rect();
    let hint = match &bar.scope {
        Some(scope) => {
            rect.min.x = chip(ui, rect, scope.target.name(), pal).right() + CHIP_GAP;
            scope.target.action()
        }
        None if bar.edit_current => "Enter URL or search".into(),
        None if bar.origin.as_ref().is_some_and(|origin| origin.pin_folder().is_some()) => "Search or enter URL for this folder…".into(),
        None => "Search or enter URL…".into(),
    };
    search_field(ui, rect, &mut bar.query, &hint, pal)
}

/// The scope's name at the start of the field, elided to 40% of it.
fn chip(ui: &Ui, field: Rect, name: &str, pal: &crate::ui::theme::Palette) -> Rect {
    let ink = crate::ui::style::Ink::new(pal.panel, pal.accent);
    let mut job = egui::text::LayoutJob::single_section(name.to_owned(), egui::TextFormat::simple(FontId::proportional(14.0), ink.on_accent));
    job.wrap = egui::text::TextWrapping::truncate_at_width((field.width() * 0.4).min(240.0) - 20.0);
    let galley = ui.painter().layout_job(job);
    let rect = Rect::from_min_size(pos2(field.left(), field.center().y - CHIP_HEIGHT / 2.0), vec2(galley.size().x + 20.0, CHIP_HEIGHT));
    ui.painter().rect_filled(rect, 6, ink.accent);
    ui.painter().galley(pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0), galley, ink.on_accent);
    rect
}

/// A row's trailing text: a badge such as "Open", or the Tab key with what
/// Tab does. Laid out first, so the title takes what is left.
struct Trailing {
    label: Option<Arc<egui::Galley>>,
    key: Option<Arc<egui::Galley>>,
}

const KEY_PADDING: egui::Vec2 = vec2(6.0, 2.0);

impl Trailing {
    fn new(p: &egui::Painter, label: Option<String>, tab: bool, max_width: f32, color: Color32) -> Self {
        let key = tab.then(|| p.layout_no_wrap("Tab".into(), FontId::proportional(11.0), color));
        let room = max_width - key.as_ref().map_or(0.0, |key| key.size().x + KEY_PADDING.x * 2.0 + 6.0);
        let label = label.map(|text| {
            let mut job = egui::text::LayoutJob::single_section(text, egui::TextFormat::simple(FontId::proportional(12.0), color));
            job.wrap = egui::text::TextWrapping::truncate_at_width(room.max(0.0));
            p.layout_job(job)
        });
        Trailing { label, key }
    }

    fn width(&self) -> f32 {
        let key = self.key.as_ref().map_or(0.0, |key| key.size().x + KEY_PADDING.x * 2.0);
        let label = self.label.as_ref().map_or(0.0, |label| label.size().x);
        key + label + if key > 0.0 && label > 0.0 { 6.0 } else { 0.0 }
    }

    /// Paint ending at `right`; the label's and the key's rectangles.
    fn paint(self, p: &egui::Painter, right: f32, center_y: f32, color: Color32) -> (Option<Rect>, Option<Rect>) {
        let mut right = right;
        let key = self.key.map(|galley| {
            let rect = Rect::from_min_max(pos2(right - galley.size().x - KEY_PADDING.x * 2.0, center_y - galley.size().y / 2.0 - KEY_PADDING.y), pos2(right, center_y + galley.size().y / 2.0 + KEY_PADDING.y));
            p.rect_stroke(rect, 4, egui::Stroke::new(1.0, color), egui::StrokeKind::Inside);
            p.galley(rect.min + KEY_PADDING, galley, color);
            right = rect.left() - 6.0;
            rect
        });
        let label = self.label.map(|galley| {
            let rect = Rect::from_min_size(pos2(right - galley.size().x, center_y - galley.size().y / 2.0), galley.size());
            p.galley(rect.min, galley, color);
            rect
        });
        (label, key)
    }
}

/// A row's trailing text and whether it shows the Tab key: Tab's hint on the
/// row Tab acts on, otherwise the row's badge.
fn trailing(offer: Option<&(usize, Target)>, row: usize, s: &Suggestion, shortcuts: &Shortcuts) -> (Option<String>, bool) {
    match offer.filter(|(at, _)| *at == row) {
        Some((_, target)) => (matches!(target, Target::Site(_)).then(|| target.action()), true),
        None => (match &s.kind {
            SuggestionKind::Tab { .. } => Some("Switch to Tab".into()),
            SuggestionKind::Item { .. } => Some("Open".into()),
            SuggestionKind::Action { action_id } => command_badge(shortcuts, action_id),
            _ => None,
        }, false),
    }
}

/// Where a row's text went: its title (with subtitle) and trailing parts.
#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))] // The layout tests read it.
struct RowText {
    title: Rect,
    label: Option<Rect>,
    key: Option<Rect>,
}

/// A row's title and subtitle from `left`, and its trailing text at the right
/// end, which is laid out first and takes at most half the row.
#[allow(clippy::too_many_arguments)]
fn row_text(p: &egui::Painter, row: Rect, left: f32, s: &Suggestion, label: Option<String>, tab: bool, title_col: Color32, sub_col: Color32) -> RowText {
    let right = row.right() - (TRAILING_INSET - SURFACE_INSET);
    let trailing = Trailing::new(p, label, tab, row.width() * 0.5, sub_col);
    let width = trailing.width();
    let text_right = right - if width > 0.0 { width + 12.0 } else { 0.0 };
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = (text_right - left).max(0.0);
    job.wrap.max_rows = 1;
    job.append(&s.title, 0.0, egui::TextFormat::simple(FontId::proportional(14.0), title_col));
    if !s.subtitle.is_empty() && s.subtitle != s.title {
        job.append(&format!("  — {}", s.subtitle), 0.0, egui::TextFormat::simple(FontId::proportional(13.0), sub_col));
    }
    let galley = p.layout_job(job);
    let title = Rect::from_min_size(pos2(left, row.center().y - galley.size().y / 2.0), galley.size());
    p.galley(title.min, galley, title_col);
    let (label, key) = trailing.paint(p, right, row.center().y, sub_col);
    RowText { title, label, key }
}

/// Claim keyboard input before the sidebar and Find render their text fields.
pub fn prepare_input(ctx: &egui::Context) {
    let layer = egui::LayerId::new(egui::Order::Foreground, Id::unique("cmd-backdrop"));
    ctx.move_to_top(layer);
    ctx.memory_mut(|memory| {
        memory.set_modal_layer(layer);
        let field = Id::unique("cmd-field");
        if !memory.has_focus(field) { memory.request_focus(field); }
    });
}

fn show_backdrop(ctx: &egui::Context, screen: Rect) -> egui::Response {
    let area = egui::Area::new(Id::unique("cmd-backdrop")).order(egui::Order::Foreground).fixed_pos(screen.min).fade_in(false).sense(Sense::hover());
    ctx.move_to_top(area.layer());
    ctx.memory_mut(|memory| memory.set_modal_layer(area.layer()));
    area.show(ctx, |ui| {
        let response = ui.allocate_rect(screen, Sense::CLICK);
        ui.painter().rect_filled(screen, 0, Color32::from_black_alpha(60));
        response
    }).inner
}

fn surface_area(ctx: &egui::Context, position: egui::Pos2) -> egui::Area {
    let area = egui::Area::new(Id::unique("cmd-bar")).order(egui::Order::Foreground).fixed_pos(position).fade_in(false).sense(Sense::hover());
    ctx.set_sublayer(egui::LayerId::new(egui::Order::Foreground, Id::unique("cmd-backdrop")), area.layer());
    area
}

pub fn show(app: &mut CrossbowApp, ui: &mut Ui) {
    let now = ui.ctx().input(|i| i.time);
    let Some(bar) = &app.ui.command else {
        // A closed bar wants nothing, so a late reply is dropped.
        app.suggest.want(now, None);
        return;
    };
    let ctx = ui.ctx().clone();
    let screen = ctx.content_rect();
    let pal = app.palette();
    let ink = crate::ui::style::Ink::new(pal.panel, pal.accent);
    let first_frame = bar.needs_focus;

    // Floating chrome is foreground too; both modal layers must stay above it.
    let mut close = show_backdrop(&ctx, screen).clicked();

    let profile = bar_profile(app);
    let space = app.active_space.clone();
    let bar = app.ui.command.as_mut().unwrap();
    if take_backspace(&ctx, bar) { leave_scope(&ctx, bar); }
    let keys = BarKeys::take(&ctx);
    let field_layout = BarLayout::new(screen, 0);
    let mut chosen: Option<(Suggestion, bool, bool)> = None;

    surface_area(&ctx, field_layout.rect.min).show(&ctx, |ui| {
        let p = ui.painter().clone();
        let background = p.add(egui::Shape::Noop);
        crate::ui::style::apply_visuals(ui, &ink);
        let field = field_layout.field;
        let bar = app.ui.command.as_mut().unwrap();
        let icon = if matches!(bar.scope.as_ref().map(|scope| &scope.target), Some(Target::Ask(_))) { Icon::Sparkle } else { Icon::Search };
        icons::draw(&p, Rect::from_center_size(pos2(field.left() + ICON_COLUMN, field.center().y), vec2(16.0, 16.0)), icon, pal.panel_dim);
        let out = focused_search_field(ui, &field_layout, bar, keys.step != 0 || keys.tab != 0, &pal);
        if out.response.changed() {
            bar.selected = 0;
        }
        // Apply text/paste before ranking or Return can submit the previous URL.
        app.suggest.receive();
        refresh_results(app, now);
        let bar = app.ui.command.as_mut().unwrap();
        let entered = take_tab(&ctx, bar, keys.tab, &profile, &space);
        // Rows for the scope Tab just entered, before Return can choose one.
        if entered { refresh_results(app, now); }
        let step = keys.step + if entered { 0 } else { keys.tab };
        if let Some(wait) = app.suggest.send_due(now) {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
        }
        let bar = app.ui.command.as_mut().unwrap();
        if !bar.results.is_empty() {
            bar.selected = (bar.selected as isize + step).rem_euclid(bar.results.len() as isize) as usize;
        }
        if let Some(new_tab) = keys.submit {
            chosen = bar.results.get(bar.selected).cloned().map(|suggestion| (suggestion, new_tab, keys.here));
        }
        let rows = bar.results.len().min(9);
        let layout = BarLayout::new(screen, rows);
        let rect = layout.rect;
        ui.set_min_size(rect.size());
        p.set(background, egui::Shape::Vec(vec![
            egui::epaint::Shadow { offset: [0, 18], blur: 48, spread: 0, color: Color32::from_black_alpha(70) }.as_shape(rect, egui::CornerRadius::same(16)).into(),
            egui::Shape::rect_filled(rect, 16, pal.panel),
        ]));

        if rows > 0 {
            p.line_segment([pos2(rect.left() + SURFACE_INSET, field.bottom()), pos2(rect.right() - SURFACE_INSET, field.bottom())], egui::Stroke::new(1.0, pal.panel_border));
            let results: Vec<Suggestion> = bar.results.iter().take(rows).cloned().collect();
            let selected = bar.selected;
            let offer = offered(bar).cloned();
            let scroll_to_selection = first_frame || step != 0 || out.response.changed();
            let opened_at = bar.opened_at.to_bits();
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(layout.results));
            egui::ScrollArea::vertical().id_salt(("cmd-results", opened_at)).auto_shrink([false, false]).show(&mut child, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (i, s) in results.iter().enumerate() {
                    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), RESULT_HEIGHT), Sense::hover());
                    let resp = ui.interact(r, Id::unique(("cmd-row", i)), Sense::click());
                    if resp.hovered() && ctx.input(|i| i.pointer.delta().length_sq() > 0.0) {
                        app.ui.command.as_mut().unwrap().selected = i;
                    }
                    let sel = i == selected;
                    if sel && scroll_to_selection {
                        ui.scroll_to_rect_animation(r, None, egui::style::ScrollAnimation::none());
                    }
                    if !ui.is_rect_visible(r) { continue; }
                    let p = ui.painter().clone();
                    if sel {
                        p.rect_filled(r, 10, ink.accent);
                    }
                    let (title_col, sub_col) = if sel { (ink.on_accent, ink.on_accent) } else { (pal.panel_text, pal.panel_dim) };
                    let icon_r = Rect::from_center_size(pos2(rect.left() + ICON_COLUMN, r.center().y), vec2(18.0, 18.0));
                    match &s.kind {
                        SuggestionKind::Action { .. } => icons::draw(&p, icon_r.shrink(1.0), Icon::Sparkle, title_col),
                        SuggestionKind::Navigate if s.subtitle.starts_with("Search") => icons::draw(&p, icon_r.shrink(1.0), Icon::Search, title_col),
                        SuggestionKind::EngineSuggestion => icons::draw(&p, icon_r.shrink(1.0), Icon::Search, title_col),
                        _ => sidebar::favicon(app, ui, s.url.as_deref().unwrap_or_default(), icon_r, &pal),
                    }
                    let (label, tab) = trailing(offer.as_ref(), i, s, &app.shortcuts);
                    row_text(&p, r, rect.left() + TEXT_COLUMN, s, label, tab, title_col, sub_col);
                    if resp.clicked() {
                        chosen = Some((s.clone(), ctx.input(|i| i.modifiers.command), ctx.input(|i| i.modifiers.shift)));
                    }
                }
            });
        }
    });

    if let Some((s, new_tab, here)) = chosen {
        close |= run(app, &ctx, s, new_tab, here);
    }
    if close {
        crate::app::finish_command(&ctx, &mut app.ui.command, &mut app.ui.focus, app.ui.find.as_mut());
    }
}

fn library_candidates(lib: &crossbow_core::library::Library, tabs: &[crate::tabs::Tab], active: Option<&str>, profile: &str) -> Vec<Candidate> {
    let open_items: std::collections::HashSet<&str> = tabs.iter().filter_map(|tab| tab.item_id.as_deref()).collect();
    let mut candidates = Vec::new();
    for tab in tabs {
        if Some(tab.id.as_str()) == active || tab.url.is_empty() || tab.url == NEW_TAB_URL || crate::app::tab_profile(lib, tab) != Some(profile) { continue; }
        candidates.push(Candidate {
            kind: SuggestionKind::Tab { tab_id: tab.id.clone(), space_id: tab.space_id.clone() },
            title: tab.display_title(), url: Some(tab.url.clone()), boost: 400,
        });
    }
    for (id, item) in &lib.items {
        if open_items.contains(id.as_str()) || !crate::app::item_in_profile(lib, item, profile) { continue; }
        candidates.push(Candidate { kind: SuggestionKind::Item { item_id: id.clone() }, title: item.display_title().to_owned(), url: Some(item.url.clone()), boost: 300 });
    }
    candidates
}

fn history_candidates(lib: &crossbow_core::library::Library, profile: &str) -> Vec<Candidate> {
    if !lib.profiles.contains_key(profile) { return Vec::new(); }
    lib.store.recent_history(profile, 400).unwrap_or_default().into_iter().map(|h| Candidate {
        kind: SuggestionKind::History,
        title: if h.title.is_empty() { search::display_url(&h.url) } else { h.title },
        url: Some(h.url), boost: h.visit_count.min(50) * 2,
    }).collect()
}

fn candidate_snapshot(lib: &crossbow_core::library::Library, tabs: &[crate::tabs::Tab], active: Option<&str>, profile: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    (lib.revision, lib.store.history_revision(), active, profile).hash(&mut hash);
    for tab in tabs { (&tab.id, &tab.space_id, &tab.item_id, &tab.url, &tab.title).hash(&mut hash); }
    hash.finish()
}

#[cfg(test)]
fn profile_results(lib: &crossbow_core::library::Library, profile: &str, query: &str, candidates: &[Candidate]) -> Vec<Suggestion> {
    profile_rows(lib, profile, query, candidates, &[])
}

/// Rows for `query` in `profile`. While the profile's suggestions are on,
/// `suggested` joins them; see [`search::blend`] for the order.
fn profile_rows(lib: &crossbow_core::library::Library, profile: &str, query: &str, candidates: &[Candidate], suggested: &[String]) -> Vec<Suggestion> {
    if let Some(bar) = crate::search_engines::bar_search(lib, profile, query) {
        let mut results = search::rank(query, candidates, &bar.engine, 9);
        if bar.keyword && let Some(first) = results.first_mut() {
            // `keyword text` searches that engine for the text alone.
            *first = Suggestion { kind: SuggestionKind::Navigate, title: bar.text.clone(), subtitle: format!("Search {}", bar.engine.name()), url: Some(bar.engine.search_url(&bar.text)), score: u32::MAX };
        }
        if let Some((id, title, _, _)) = COMMANDS.iter().find(|(_, title, _, _)| query.trim().eq_ignore_ascii_case(title)) {
            let kind = SuggestionKind::Action { action_id: id };
            if candidates.iter().any(|candidate| candidate.kind == kind) {
                // Exact commands stay reachable behind Search, regardless of tab boosts.
                results.retain(|result| result.kind != kind);
                results.insert(1, Suggestion { kind, title: (*title).into(), subtitle: String::new(), url: None, score: u32::MAX - 1 });
                results.truncate(9);
            }
        }
        if bar.suggestions {
            results = search::blend(results, &bar.text, suggested, &bar.engine, 9);
        }
        return results;
    }
    // Keep only recovery commands that cannot create or navigate profile data.
    let recovery: Vec<_> = candidates.iter().filter(|candidate| matches!(candidate.kind,
        SuggestionKind::Action { action_id: "settings" | "toggle-sidebar" | "sync" | "ask-ai" }
    )).map(|candidate| Candidate { kind: candidate.kind.clone(), title: candidate.title.clone(), url: None, boost: candidate.boost }).collect();
    search::rank(query, &recovery, &lib.settings.search_engine.into(), 10).into_iter()
        .filter(|result| matches!(result.kind, SuggestionKind::Action { .. })).take(9).collect()
}

/// Want suggestions for what is typed, when the profile allows sending it,
/// and rank rows with any that answer exactly that.
fn suggested_rows(suggest: &mut Suggestions, lib: &crossbow_core::library::Library, profile: &str, query: &str, candidates: &[Candidate], now: f64) -> Vec<Suggestion> {
    let request = crate::search_engines::bar_search(lib, profile, query).and_then(|bar| bar.request(profile));
    suggest.want(now, request.clone());
    let suggested = request.as_ref().map_or(&[][..], |request| suggest.shown(request));
    profile_rows(lib, profile, query, candidates, suggested)
}

/// The profile the open bar searches and asks in, or "" when none is safe.
fn bar_profile(app: &CrossbowApp) -> String {
    let focused = app.focused_tab_id();
    let tab = focused.as_deref().and_then(|id| app.tab(id));
    let view_profile = tab.and_then(|tab| tab.view.as_ref().map(crossbow_engine::WebView::profile_id));
    app.ui.command.as_ref().and_then(|bar| bar.origin.as_ref())
        .and_then(|origin| origin.profile(&app.lib, &app.active_space, tab, view_profile)).unwrap_or("").to_owned()
}

/// Rank the bar's rows and what Tab offers from them. A scope whose profile,
/// Space or engine is gone is left first; while one lasts only its rows show.
fn rank(bar: &mut crate::app::CommandBar, suggest: &mut Suggestions, lib: &Library, profile: &str, space: &str, candidates: impl FnOnce(&str) -> Vec<Candidate>, now: f64) {
    if bar.scope.as_ref().is_some_and(|scope| !scope_current(lib, profile, space, scope)) { bar.scope = None; }
    let query = bar.query.clone();
    let (results, offers) = match bar.scope.as_ref().map(|scope| scope.target.clone()) {
        Some(Target::Site(engine)) => (site_rows(suggest, lib, profile, &engine, &query, now), Vec::new()),
        Some(Target::Ask(provider)) => {
            suggest.want(now, None);
            (ask_rows(lib, profile, provider, &query), Vec::new())
        }
        None => offered_rows(suggest, lib, profile, &query, &candidates(&query), now),
    };
    // Rows that change under the same text keep the chosen one; new text
    // selects the typed-text row.
    bar.selected = if bar.last_query.as_deref() == Some(query.as_str()) { kept_selection(&bar.results, bar.selected, &results) } else { 0 };
    bar.results = results;
    bar.offers = offers;
    bar.last_query = Some(query);
}

/// The ordinary rows, with what Tab offers: the engine the text names on the
/// top row, and the Ask row while Ask AI in the Command Bar is on.
fn offered_rows(suggest: &mut Suggestions, lib: &Library, profile: &str, query: &str, candidates: &[Candidate], now: f64) -> (Vec<Suggestion>, Vec<(usize, Target)>) {
    let mut rows = suggested_rows(suggest, lib, profile, query, candidates, now);
    let mut offers = Vec::new();
    // Rows without the typed-text row are recovery commands: nothing to offer.
    if rows.first().is_none_or(|row| row.kind != SuggestionKind::Navigate) { return (rows, offers); }
    if let Some(engine) = crate::search_engines::tab_engine(lib, profile, query) { offers.push((0, Target::Site(engine))); }
    if let Some(provider) = crate::ask_bar::offered(lib, profile) {
        // After the typed-text row and an exact command.
        let at = rows.iter().take_while(|row| row.score >= u32::MAX - 1).count();
        let reason = crate::ask_bar::check(lib, profile, query).err().unwrap_or_default();
        rows.insert(at, ask_row(format!("Ask {provider}"), reason));
        rows.truncate(9);
        offers.push((at, Target::Ask(provider)));
    }
    (rows, offers)
}

fn ask_row(title: String, subtitle: String) -> Suggestion {
    Suggestion { kind: SuggestionKind::Action { action_id: ASK_ROW }, title, subtitle, url: None, score: u32::MAX - 1 }
}

/// A site scope's rows: its search for the text, and suggestions only from its
/// own suggestion address, under the profile's consent and never-send rules.
fn site_rows(suggest: &mut Suggestions, lib: &Library, profile: &str, engine: &Engine, query: &str, now: f64) -> Vec<Suggestion> {
    let text = query.trim();
    let suggestions = SearchPreferences::load(lib, profile).is_ok_and(|saved| saved.suggestions);
    let request = BarSearch { engine: engine.clone(), text: text.into(), keyword: true, suggestions }.request(profile);
    suggest.want(now, request.clone());
    if text.is_empty() { return Vec::new(); }
    let row = Suggestion { kind: SuggestionKind::Navigate, title: text.into(), subtitle: format!("Search {}", engine.name()), url: Some(engine.search_url(text)), score: u32::MAX };
    let suggested = request.as_ref().map_or(&[][..], |request| suggest.shown(request));
    search::blend(vec![row], text, suggested, engine, 9)
}

/// Ask mode's row: the question and where Return sends it, or why it won't.
fn ask_rows(lib: &Library, profile: &str, provider: &str, query: &str) -> Vec<Suggestion> {
    let text = query.trim();
    if text.is_empty() { return Vec::new(); }
    let subtitle = crate::ask_bar::check(lib, profile, text).err().unwrap_or_else(|| format!("Ask {provider}"));
    vec![ask_row(text.into(), subtitle)]
}

/// A scope lasts while the bar's profile and Space are its own, and a site
/// scope while that profile can search and still has its engine.
fn scope_current(lib: &Library, profile: &str, space: &str, scope: &Scope) -> bool {
    scope.profile == profile && scope.space == space && match &scope.target {
        Target::Site(Engine::BuiltIn(_)) => crate::search_engines::bar_search(lib, profile, "").is_some(),
        Target::Site(Engine::Custom(engine)) => crate::search_engines::bar_search(lib, profile, "").is_some()
            && SearchPreferences::load(lib, profile).is_ok_and(|saved| saved.engines.contains(engine)),
        // Ask checks consent and the provider again on Return.
        Target::Ask(_) => true,
    }
}

/// The row Tab acts on and what it enters: the selected row's offer, or the
/// first one while the top row is selected.
fn offered(bar: &crate::app::CommandBar) -> Option<&(usize, Target)> {
    bar.offers.iter().find(|(row, _)| *row == bar.selected).or_else(|| bar.offers.first().filter(|_| bar.selected == 0))
}

/// Plain Tab enters what the bar offers. False when nothing is offered, so Tab
/// moves down instead.
fn take_tab(ctx: &egui::Context, bar: &mut crate::app::CommandBar, tabs: isize, profile: &str, space: &str) -> bool {
    if tabs <= 0 { return false; }
    let Some((_, target)) = offered(bar).cloned() else { return false };
    enter(ctx, bar, target, profile, space);
    true
}

/// A site scope starts with an empty field; Ask keeps the text to send.
fn enter(ctx: &egui::Context, bar: &mut crate::app::CommandBar, target: Target, profile: &str, space: &str) {
    let before = match target {
        Target::Site(_) => std::mem::take(&mut bar.query),
        Target::Ask(_) => String::new(),
    };
    bar.scope = Some(Scope { target, profile: profile.into(), space: space.into(), before });
    bar.results.clear();
    bar.offers.clear();
    bar.selected = 0;
    bar.last_query = None;
    cursor_to_end(ctx, &bar.query);
}

/// Backspace in an empty scoped field leaves the scope instead of editing.
fn take_backspace(ctx: &egui::Context, bar: &crate::app::CommandBar) -> bool {
    bar.scope.is_some() && bar.query.is_empty() && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Backspace))
}

/// Leave the scope Tab entered, if any; an empty site search gets back what
/// was typed before Tab. Escape does this before it closes the bar.
pub(crate) fn leave_scope(ctx: &egui::Context, bar: &mut crate::app::CommandBar) -> bool {
    let Some(scope) = bar.scope.take() else { return false };
    if bar.query.is_empty() { bar.query = scope.before; }
    bar.selected = 0;
    bar.last_query = None;
    cursor_to_end(ctx, &bar.query);
    true
}

fn cursor_to_end(ctx: &egui::Context, text: &str) {
    let id = Id::unique("cmd-field");
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(text.chars().count()))));
    state.store(ctx, id);
}

/// Return on the Ask row. Outside Ask mode it enters it, like Tab; in it, the
/// text goes to Ask AI as a new chat's first message, or stays in the field
/// while the row says why not. The action to run, which closes the bar.
fn ask_return(ctx: &egui::Context, bar: &mut crate::app::CommandBar, lib: &Library, profile: &str, space: &str) -> Option<Action> {
    if !matches!(bar.scope.as_ref().map(|scope| &scope.target), Some(Target::Ask(_))) {
        let target = bar.offers.iter().map(|(_, target)| target).find(|target| matches!(target, Target::Ask(_)))?.clone();
        enter(ctx, bar, target, profile, space);
        return None;
    }
    let question = bar.query.trim().to_string();
    if crate::ask_bar::check(lib, profile, &question).is_err() {
        // The row shows why; the text stays.
        bar.last_query = None;
        return None;
    }
    Some(match &bar.chat_entry {
        Some(entry) => Action::AskChat(entry.clone(), question),
        None => bar.ask_ai_action(),
    })
}

fn refresh_results(app: &mut CrossbowApp, now: f64) {
    use std::hash::{Hash, Hasher};
    let profile = bar_profile(app);
    let focused = app.focused_tab_id();
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    let video = focused.as_deref().is_some_and(|tab| app.tab_has_video(tab));
    let translated = focused.as_deref().is_some_and(|tab| app.translator.translated(tab));
    let document = crate::new_document::has_app(&app.lib, &app.active_space);
    (candidate_snapshot(&app.lib, &app.tabs, focused.as_deref(), &profile), app.suggest.revision(), &app.active_space, video, translated, document).hash(&mut hash);
    let snapshot = hash.finish();
    let Some(bar) = app.ui.command.as_mut() else { return };
    if bar.last_query.as_deref() == Some(bar.query.as_str()) && bar.last_snapshot == Some(snapshot) {
        return;
    }
    let (lib, tabs) = (&app.lib, &app.tabs);
    rank(bar, &mut app.suggest, lib, &profile, &app.active_space, |query| {
        let mut cands = library_candidates(lib, tabs, focused.as_deref(), &profile);
        if !query.trim().is_empty() {
            cands.extend(history_candidates(lib, &profile));
            for (id, title, keywords, _) in offered_commands(video, translated, document) {
                cands.push(Candidate { kind: SuggestionKind::Action { action_id: id }, title: format!("{title} ({keywords})"), url: None, boost: 0 });
            }
        }
        cands
    }, now);
    bar.last_snapshot = Some(snapshot);
    for r in &mut bar.results {
        if let SuggestionKind::Action { action_id } = r.kind {
            if let Some(c) = COMMANDS.iter().find(|c| c.0 == action_id) {
                r.title = c.1.to_string();
            }
        }
        if let SuggestionKind::Tab { space_id, .. } = &r.kind {
            let space = if space_id == FAVORITES_SPACE { Some("Favorites".to_string()) } else { app.lib.spaces.get(space_id).map(|s| s.name.clone()) };
            if let Some(s) = space {
                r.subtitle = format!("{} · {s}", r.subtitle);
            }
        }
    }
}

/// The chosen row's place among new rows, so suggestions arriving above it
/// don't move the selection to another row.
fn kept_selection(old: &[Suggestion], selected: usize, new: &[Suggestion]) -> usize {
    let chosen = old.get(selected).map(|row| (&row.kind, &row.url));
    let kept = chosen.and_then(|(kind, url)| new.iter().position(|row| row.kind == *kind && row.url == *url));
    kept.unwrap_or(selected).min(new.len().saturating_sub(1))
}

/// An address a site's rule may open in another profile: a typed link or
/// a page from history, not a search.
fn is_address(s: &Suggestion) -> bool {
    match s.kind {
        SuggestionKind::Navigate => crossbow_core::search::input_to_url(&s.title).is_some(),
        SuggestionKind::History => true,
        _ => false,
    }
}

/// Do what the chosen row says. False keeps the bar open. An address for a
/// new tab may open in its site's profile; with `here` it opens in this Space,
/// and editing the current tab's address loads it in place.
fn run(app: &mut CrossbowApp, ctx: &egui::Context, s: Suggestion, new_tab: bool, here: bool) -> bool {
    let edit_current = app.ui.command.as_ref().is_some_and(|b| b.edit_current);
    if s.kind == (SuggestionKind::Action { action_id: ASK_ROW }) {
        let profile = bar_profile(app);
        let Some(bar) = app.ui.command.as_mut() else { return true };
        let action = ask_return(ctx, bar, &app.lib, &profile, &app.active_space);
        let close = action.is_some();
        app.actions.extend(action);
        return close;
    }
    match s.kind {
        SuggestionKind::Navigate | SuggestionKind::History | SuggestionKind::EngineSuggestion => {
            let new_tab = !edit_current || new_tab;
            let route = new_tab && !here && is_address(&s);
            let Some(url) = s.url else { return true };
            let Some(origin) = app.ui.command.as_ref().and_then(|bar| bar.origin.clone()) else { return true };
            app.actions.push(Action::OpenCommandUrl { url, origin, new_tab, route });
        }
        SuggestionKind::Tab { tab_id, .. } => app.actions.push(Action::ActivateTab(tab_id)),
        SuggestionKind::Item { item_id } => app.actions.push(Action::OpenItem(item_id)),
        SuggestionKind::Action { action_id } => {
            let focused = app.focused_tab_id();
            let a = match action_id {
                "new-space" => Some(Action::NewSpace),
                "new-folder" => Some(Action::NewFolder),
                "toggle-sidebar" => Some(Action::ToggleSidebar),
                "settings" => Some(Action::OpenSettings),
                "ask-ai" => app.ui.command.as_ref().map(crate::app::CommandBar::ask_ai_action),
                "tidy-today" => Some(Action::TidyToday(app.active_space.clone())),
                "clear-today" => Some(Action::ClearToday),
                "reopen" => Some(Action::ReopenClosed),
                "new-little-window" | "show-little-windows" | "close-little-windows" => little_window_action(action_id),
                NEW_DOCUMENT => Some(Action::NewDocument),
                CHOOSE_DOCUMENT_APP => Some(Action::ChooseDocumentApp),
                "copy-url" => Some(Action::CopyUrl),
                "split" => focused.clone().map(Action::SplitWith),
                "close-split" => Some(Action::CloseSplit),
                "pin" => focused.clone().map(|t| if app.tab(&t).is_some_and(|x| x.item_id.is_some()) { Action::UnpinTab(t) } else { Action::PinTab(t) }),
                "favorite" => focused.clone().map(Action::FavoriteTab),
                "find" => Some(Action::OpenFind),
                "hard-reload" => Some(Action::HardReload),
                "devtools" => Some(Action::DevTools),
                "view-source" => Some(Action::ViewSource),
                "print" => Some(Action::Print),
                "capture" => Some(Action::Capture(None)),
                "captures" => Some(Action::ShowCaptures),
                "mute" => focused.clone().map(Action::ToggleMute),
                PIP => focused.clone().map(Action::PictureInPicture),
                TRANSLATE => focused.clone().map(|tab| Action::Translate(tab, crate::translate::Choice::Translate)),
                ORIGINAL => focused.clone().map(|tab| Action::Translate(tab, crate::translate::Choice::ShowOriginal)),
                "archive" => {
                    app.ui.show_archive = true;
                    None
                }
                "sync" => {
                    app.actions.push(Action::OpenSettings);
                    app.ui.settings_page_hint = Some(crate::ui::settings::Page::Account);
                    None
                }
                "welcome" => Some(Action::StartOnboarding),
                _ => None,
            };
            if let Some(a) = a {
                app.actions.push(a);
            }
        }
    }
    true
}

/// The Little Window commands; they act on those windows, not on a tab.
fn little_window_action(action_id: &str) -> Option<Action> {
    match action_id {
        "new-little-window" => Some(Action::NewLittleWindow),
        "show-little-windows" => Some(Action::ShowLittleWindows),
        "close-little-windows" => Some(Action::CloseLittleWindows),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_and_close_all_little_windows_are_commands_with_no_default_badge() {
        use crate::shortcuts::{Proposal, Slot, index_of};
        let store = crossbow_core::store::Store::open_in_memory().unwrap();
        let mut shortcuts = Shortcuts::defaults();
        for (id, title, command, key) in [("show-little-windows", "Show All Little Windows", Command::ShowLittleWindows, Key::F6), ("close-little-windows", "Close All Little Windows", Command::CloseLittleWindows, Key::F7)] {
            let entry = COMMANDS.iter().find(|entry| entry.0 == id).unwrap_or_else(|| panic!("{id} is in the command bar"));
            assert_eq!((entry.1, entry.3), (title, Some(command)));
            assert_eq!(command_badge(&shortcuts, id), None, "{id} has no default chord");
            let chord = crate::shortcuts::Chord::from_event(key, egui::Modifiers::NONE);
            let Proposal::Ready(candidate) = shortcuts.propose(index_of(command), Slot::Primary, Some(chord)) else { panic!("{id} can take a chord") };
            shortcuts.save(&store, candidate).unwrap();
            assert_eq!(command_badge(&shortcuts, id), Some(chord.label()), "a chord the user sets is shown");
        }
        assert!(matches!(little_window_action("show-little-windows"), Some(Action::ShowLittleWindows)));
        assert!(matches!(little_window_action("close-little-windows"), Some(Action::CloseLittleWindows)));
        assert!(matches!(little_window_action("new-little-window"), Some(Action::NewLittleWindow)));
    }

    #[test]
    fn command_badges_follow_the_registry() {
        use crate::shortcuts::{Chord, Command, Proposal, Shortcuts, Slot, index_of};
        let label = |key: Key, modifiers: egui::Modifiers| Chord::from_event(key, modifiers).label();
        let mut shortcuts = Shortcuts::defaults();
        assert_eq!(command_badge(&shortcuts, "toggle-sidebar"), Some(label(Key::S, egui::Modifiers::COMMAND)));
        assert_eq!(command_badge(&shortcuts, "new-folder"), Some(label(Key::N, egui::Modifiers::COMMAND | egui::Modifiers::ALT)));
        assert_eq!(command_badge(&shortcuts, "split"), Some(label(Key::Equals, egui::Modifiers::CTRL | egui::Modifiers::SHIFT)));
        assert_eq!(command_badge(&shortcuts, "ask-ai"), None);
        let store = crossbow_core::store::Store::open_in_memory().unwrap();
        for (command, chord) in [(Command::ToggleSidebar, Some(Chord::from_event(Key::J, egui::Modifiers::COMMAND))), (Command::Print, None)] {
            let Proposal::Ready(candidate) = shortcuts.propose(index_of(command), Slot::Primary, chord) else { panic!("{command:?} is free to change") };
            shortcuts.save(&store, candidate).unwrap();
        }
        assert_eq!(command_badge(&shortcuts, "toggle-sidebar"), Some(label(Key::J, egui::Modifiers::COMMAND)));
        assert_eq!(command_badge(&shortcuts, "print"), None, "an unset shortcut shows no badge");
    }

    #[test]
    fn capture_and_show_captures_are_found_by_name_and_by_what_they_do() {
        let (lib, space, _) = crate::chat::tests::configured(crossbow_ai::local::LocalEndpoint::default());
        let profile = &lib.spaces[&space].profile_id;
        let candidates: Vec<_> = offered_commands(false, false, false).map(|(id, title, keywords, _)| Candidate {
            kind: SuggestionKind::Action { action_id: id }, title: format!("{title} ({keywords})"), url: None, boost: 0,
        }).collect();
        for (query, id) in [("Capture", "capture"), ("screenshot", "capture"), ("Show Captures", "captures"), ("captures", "captures")] {
            let results = profile_results(&lib, profile, query, &candidates);
            assert!(results.iter().any(|result| result.kind == SuggestionKind::Action { action_id: id }), "{query:?} finds {id}");
        }
        let label = crate::shortcuts::Chord::from_event(Key::Num2, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT).label();
        assert_eq!(command_badge(&Shortcuts::defaults(), "capture"), Some(label), "its shortcut shows beside it");
        assert_eq!(command_badge(&Shortcuts::defaults(), "captures"), None);
    }

    fn key(key: Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    #[test]
    fn picture_in_picture_is_offered_only_for_a_tab_with_a_video() {
        let ids = |video| offered_commands(video, false, false).map(|(id, ..)| *id).collect::<Vec<_>>();
        assert!(!ids(false).contains(&PIP) && ids(true).contains(&PIP));
        assert_eq!(ids(true).len(), COMMANDS.len() - 2, "nothing else depends on it; Show Original waits for a translation, New Document for an app");
        let (lib, space, _) = crate::chat::tests::configured(crossbow_ai::local::LocalEndpoint::default());
        let profile = &lib.spaces[&space].profile_id;
        for video in [false, true] {
            let candidates: Vec<_> = offered_commands(video, false, false).map(|(id, title, keywords, _)| Candidate {
                kind: SuggestionKind::Action { action_id: id }, title: format!("{title} ({keywords})"), url: None, boost: 0,
            }).collect();
            for query in ["Picture in Picture", "pip", "float video"] {
                let results = profile_results(&lib, profile, query, &candidates);
                assert_eq!(results.iter().any(|result| result.kind == SuggestionKind::Action { action_id: PIP }), video, "{query:?}, video {video}");
            }
        }
        // Running it, and the tab menu's item, need the whole app: checked in source.
        assert!(include_str!("command_bar.rs").contains("PIP => focused.clone().map(Action::PictureInPicture),"));
        assert!(include_str!("overlays.rs").contains("if app.tab_has_video(tab) && m.item(ui, PICTURE_IN_PICTURE, \"\", true) {\n                        app.actions.push(Action::PictureInPicture(tab.clone()));"));
        assert!(include_str!("../app.rs").contains("Action::PictureInPicture(tab) => self.pip.request = Some(tab),"));
    }

    #[test]
    fn translate_page_and_show_original_take_turns_and_run_on_the_focused_tab() {
        let ids = |translated| offered_commands(false, translated, false).map(|(id, ..)| *id).collect::<Vec<_>>();
        assert!(ids(false).contains(&TRANSLATE) && !ids(false).contains(&ORIGINAL), "Translate Page until the tab is translated");
        assert!(ids(true).contains(&ORIGINAL) && !ids(true).contains(&TRANSLATE), "then Show Original");
        let (lib, space, _) = crate::chat::tests::configured(crossbow_ai::local::LocalEndpoint::default());
        let profile = &lib.spaces[&space].profile_id;
        for translated in [false, true] {
            let candidates: Vec<_> = offered_commands(false, translated, false).map(|(id, title, keywords, _)| Candidate {
                kind: SuggestionKind::Action { action_id: id }, title: format!("{title} ({keywords})"), url: None, boost: 0,
            }).collect();
            let (wanted, queries) = if translated { (ORIGINAL, ["Translate Page", "translate", "Show Original"]) } else { (TRANSLATE, ["Translate Page", "translate", "foreign language"]) };
            for query in queries {
                let results = profile_results(&lib, profile, query, &candidates);
                assert!(results.iter().any(|result| result.kind == SuggestionKind::Action { action_id: wanted }), "{query:?} finds {wanted}");
            }
        }
        // Running them, and the tab menu's item, need the whole app: checked in source.
        let source = include_str!("command_bar.rs");
        assert!(source.contains("TRANSLATE => focused.clone().map(|tab| Action::Translate(tab, crate::translate::Choice::Translate)),"));
        assert!(source.contains("ORIGINAL => focused.clone().map(|tab| Action::Translate(tab, crate::translate::Choice::ShowOriginal)),"));
        assert!(source.contains("let translated = focused.as_deref().is_some_and(|tab| app.translator.translated(tab));"));
        assert!(include_str!("overlays.rs").contains("if translate && m.item(ui, if translated { SHOW_ORIGINAL } else { TRANSLATE_PAGE }, \"\", true) {\n                        app.actions.push(Action::Translate(tab.clone(), if translated { Choice::ShowOriginal } else { Choice::Translate }));"));
    }

    #[test]
    fn new_document_is_offered_once_the_profile_has_an_app_and_choose_a_document_app_until_then() {
        let ids = |document| offered_commands(false, false, document).map(|(id, ..)| *id).collect::<Vec<_>>();
        assert!(ids(true).contains(&NEW_DOCUMENT) && !ids(true).contains(&CHOOSE_DOCUMENT_APP));
        assert!(ids(false).contains(&CHOOSE_DOCUMENT_APP) && !ids(false).contains(&NEW_DOCUMENT));
        let (lib, space, _) = crate::chat::tests::configured(crossbow_ai::local::LocalEndpoint::default());
        let profile = &lib.spaces[&space].profile_id;
        for document in [false, true] {
            let candidates: Vec<_> = offered_commands(false, false, document).map(|(id, title, keywords, _)| Candidate {
                kind: SuggestionKind::Action { action_id: id }, title: format!("{title} ({keywords})"), url: None, boost: 0,
            }).collect();
            let wanted = if document { NEW_DOCUMENT } else { CHOOSE_DOCUMENT_APP };
            for query in ["New Document", "docs", "word"] {
                let results = profile_results(&lib, profile, query, &candidates);
                assert!(results.iter().any(|result| result.kind == SuggestionKind::Action { action_id: wanted }), "{query:?} finds {wanted}");
            }
        }
        assert_eq!(COMMANDS.iter().find(|command| command.0 == CHOOSE_DOCUMENT_APP).map(|command| command.1), Some(crate::new_document::CHOOSE));
        // A shortcut, once set, shows beside it; there is none by default.
        assert_eq!(command_badge(&Shortcuts::defaults(), NEW_DOCUMENT), None);
        assert!(include_str!("command_bar.rs").contains("let document = crate::new_document::has_app(&app.lib, &app.active_space);"));
    }

    #[test]
    fn ask_ai_is_an_explicit_command_and_does_not_replace_search() {
        let (lib, space, _) = crate::chat::tests::configured(crossbow_ai::local::LocalEndpoint::default());
        let profile = &lib.spaces[&space].profile_id;
        let candidates: Vec<_> = COMMANDS.iter().map(|(id, title, keywords, _)| Candidate {
            kind: SuggestionKind::Action { action_id: id }, title: format!("{title} ({keywords})"), url: None, boost: 0,
        }).collect();
        for query in ["Ask AI", "chat", "assistant", "conversation"] {
            let results = profile_results(&lib, profile, query, &candidates);
            assert_eq!(results[0].kind, SuggestionKind::Navigate, "typing must retain normal search");
            let chat = results.iter().find(|result| result.kind == SuggestionKind::Action { action_id: "ask-ai" }).unwrap();
            assert!(chat.url.is_none());
        }
        let results = profile_results(&lib, profile, "explain photosynthesis", &candidates);
        assert_eq!(results[0].kind, SuggestionKind::Navigate);
        assert_eq!(results[0].url, Some(lib.settings.search_engine.search_url("explain photosynthesis")));
    }

    #[test]
    fn ask_ai_action_retains_the_captured_owner_not_the_query() {
        let (mut lib, space, _) = crate::chat::tests::configured(crossbow_ai::local::LocalEndpoint::default());
        let account = Some(("https://sync.example.test".into(), "synthetic-account".into()));
        for edit_current in [false, true] {
            let mut bar = crate::app::CommandBar {
                query: "https://example.test/private-url?query=not-a-prompt".into(), edit_current, origin: None,
                chat_entry: crate::chat::Entry::capture(&lib, &space, &account), needs_focus: true,
                selected: 0, opened_at: 0.0, results: Vec::new(), last_query: None, last_snapshot: None, scope: None, offers: Vec::new(),
            };
            let Action::OpenChat(entry) = bar.ask_ai_action() else { panic!("Ask AI did not retain its entry") };
            assert!(entry.open(&mut lib, &space, &None).is_err());
            let chat = entry.open(&mut lib, &space, &account).unwrap();
            assert!(chat.draft.is_empty() && chat.messages.is_empty() && !chat.busy());
            assert_eq!(chat.profile, lib.spaces[&space].profile_id);
            bar.chat_entry = None;
            assert!(matches!(bar.ask_ai_action(), Action::Toast(_)), "no fallback owner is allowed");
        }
    }

    #[test]
    fn review_command_chat_submit_batch_cannot_reach_the_web_page() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for stale in [false, true] {
                let (mut lib, space, _) = crate::chat::tests::configured(crossbow_ai::local::LocalEndpoint::default());
                let ctx = egui::Context::default();
                let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
                let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
                let mut bar = Some(crate::app::CommandBar {
                    query: String::new(), edit_current: false, origin: None,
                    chat_entry: crate::chat::Entry::capture(&lib, &space, &None), needs_focus: true,
                    selected: 0, opened_at: 0.0, results: Vec::new(), last_query: None, last_snapshot: None, scope: None, offers: Vec::new(),
                });
                let candidates: Vec<_> = COMMANDS.iter().map(|(id, title, keywords, _)| Candidate {
                    kind: SuggestionKind::Action { action_id: id }, title: format!("{title} ({keywords})"), url: None, boost: 0,
                }).collect();
                let mut focus = crate::app::Focus::Chrome;
                let mut settings = None;
                let mut dispatched = 0;
                let mut forwarded = Vec::new();
                for frame in 0..6 {
                    let events = match frame {
                        3 => vec![egui::Event::Paste("Ask AI".into()), key(Key::ArrowDown, egui::Modifiers::NONE), key(Key::Enter, egui::Modifiers::NONE)],
                        5 => vec![egui::Event::Text("NEXT_FRAME".into())],
                        _ => Vec::new(),
                    };
                    ctx.run_ui(egui::RawInput { screen_rect: Some(screen), events, ..Default::default() }, |ui| {
                        let mut action = None;
                        if let Some(current) = &mut bar {
                            prepare_input(ui.ctx());
                            show_backdrop(ui.ctx(), screen);
                            let keys = BarKeys::take(ui.ctx());
                            let layout = BarLayout::new(screen, 2);
                            surface_area(ui.ctx(), layout.rect.min).show(ui.ctx(), |ui| {
                                ui.set_min_size(layout.rect.size());
                                focused_search_field(ui, &layout, current, keys.step != 0, &pal);
                                let results = profile_results(&lib, &lib.spaces[&space].profile_id, &current.query, &candidates);
                                if keys.submit.is_some() {
                                    assert_eq!(results[keys.step as usize].kind, SuggestionKind::Action { action_id: "ask-ai" });
                                    action = Some(current.ask_ai_action());
                                }
                            });
                        }
                        if action.is_some() { crate::app::finish_command(ui.ctx(), &mut bar, &mut focus, None); }
                        // Production orders command closure, forward_keys, then apply_actions.
                        if crate::app::web_keyboard_blocker(ui.ctx(), focus, bar.is_some() || settings.is_some()).is_none() {
                            forwarded.extend(crate::input::web_key_inputs(&ui.input(|input| input.events.clone())));
                        }
                        if let Some(Action::OpenChat(entry)) = action {
                            dispatched += 1;
                            let account = stale.then(|| ("https://sync.example.test".into(), "changed-account".into()));
                            match crate::ui::settings::SettingsState::from_chat_entry(&entry, &mut lib, &space, &account, 1.0) {
                                Ok(state) => { settings = Some(state); focus = crate::app::Focus::Chrome; }
                                Err(_) => assert!(stale),
                            }
                            assert!(forwarded.is_empty(), "command input escaped to the website before dispatch: {forwarded:?}");
                        }
                    }).textures_delta.clear();
                }
                assert_eq!(dispatched, 1);
                if stale {
                    assert!(settings.is_none());
                    assert!(matches!(forwarded.as_slice(), [crate::input::WebInput::Text(text)] if text == "NEXT_FRAME"), "rejection must restore later page input");
                } else {
                    assert_eq!(settings.as_ref().unwrap().page, crate::ui::settings::Page::AiChat);
                    assert!(forwarded.is_empty());
                }
            }
        }
    }

    #[test]
    fn review_command_exact_actions_survive_matching_tabs() {
        use crossbow_core::store::TabRow;
        let (lib, space, _) = crate::chat::tests::configured(crossbow_ai::local::LocalEndpoint::default());
        let profile = &lib.spaces[&space].profile_id;
        for (action_id, title, _, _) in COMMANDS.iter().filter(|command| command.0 == "ask-ai").chain(COMMANDS.iter().filter(|command| command.0 != "ask-ai")) {
            let tabs: Vec<_> = (0..8).map(|index| crate::tabs::Tab::from_row(TabRow {
                id: format!("matching-tab-{index}"), item_id: None, space_id: space.clone(),
                url: format!("https://example.test/{index}"), title: (*title).into(), order: "a0".into(), created_ms: 1, last_active_ms: 1,
            })).collect();
            let mut candidates = library_candidates(&lib, &tabs, None, profile);
            candidates.extend(COMMANDS.iter().map(|(id, title, keywords, _)| Candidate {
                kind: SuggestionKind::Action { action_id: id }, title: format!("{title} ({keywords})"), url: None, boost: 0,
            }));
            for query in [(*title).to_string(), format!("  {}  ", title.to_lowercase())] {
                let results = profile_results(&lib, profile, &query, &candidates);
                assert_eq!(results.len(), 9);
                assert_eq!(results[0].kind, SuggestionKind::Navigate, "exact command entry still requires explicit selection");
                assert_eq!(results[1].kind, SuggestionKind::Action { action_id }, "exact {title} command was crowded out");
                assert_eq!(results.iter().filter(|result| result.kind == SuggestionKind::Action { action_id }).count(), 1);
                assert!(results[1].url.is_none());
            }
        }
    }

    #[test]
    fn stable_search_focus_does_not_interrupt_ime_composition() {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, vec2(640.0, 420.0));
        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
        let mut bar = crate::app::CommandBar { query: String::new(), edit_current: false, origin: None, chat_entry: None, needs_focus: true, selected: 0, opened_at: 0.0, results: Vec::new(), last_query: None, last_snapshot: None, scope: None, offers: Vec::new() };
        for frame in 0..6 {
            let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), time: Some(frame as f64 / 60.0), ..Default::default() }, |ui| {
                prepare_input(ui.ctx());
                show_backdrop(ui.ctx(), screen);
                let layout = BarLayout::new(screen, 0);
                surface_area(ui.ctx(), layout.rect.min).show(ui.ctx(), |ui| {
                    focused_search_field(ui, &layout, &mut bar, false, &pal);
                });
            });
            if frame >= 3 {
                let ime = output.platform_output.ime.expect("focused search supplies the native IME rectangle");
                assert!(!ime.should_interrupt_composition, "stable search focus interrupted IME on frame {frame}");
            }
            output.textures_delta.clear();
        }
    }

    #[test]
    fn search_candidates_are_scoped_to_the_current_profile() {
        use crossbow_core::{library::Library, store::{Store, TabRow}};
        let mut lib = Library::open(Store::open(std::path::Path::new(":memory:")).unwrap()).unwrap();
        let a = lib.create_profile("A").unwrap();
        let b = lib.create_profile("B").unwrap();
        let opened_a = lib.add_favorite(&a, "http://127.0.0.1/a-open", "A open").unwrap();
        let saved_a = lib.add_favorite(&a, "http://127.0.0.1/a-saved", "A saved").unwrap();
        let opened_b = lib.add_favorite(&b, "http://127.0.0.1/b-open", "B open").unwrap();
        lib.add_favorite(&b, "http://127.0.0.1/b-saved", "B saved").unwrap();
        let mut tabs: Vec<_> = [(&opened_a, "A open"), (&opened_b, "B open")].into_iter().map(|(id, title)| crate::tabs::Tab::from_row(TabRow {
            id: id.clone(), item_id: Some(id.clone()), space_id: FAVORITES_SPACE.into(), url: format!("http://127.0.0.1/{title}"),
            title: title.into(), order: "a0".into(), created_ms: 1, last_active_ms: 2,
        })).collect();
        let candidates = library_candidates(&lib, &tabs, None, &a);
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|candidate| candidate.title.starts_with('A')));
        assert!(candidates.iter().any(|candidate| matches!(&candidate.kind, SuggestionKind::Tab { tab_id, .. } if tab_id == &opened_a)));
        assert!(candidates.iter().any(|candidate| matches!(&candidate.kind, SuggestionKind::Item { item_id } if item_id == &saved_a)));
        let before = candidate_snapshot(&lib, &tabs, None, &a);
        lib.delete_item(&saved_a).unwrap();
        assert_ne!(before, candidate_snapshot(&lib, &tabs, None, &a));
        assert_eq!(library_candidates(&lib, &tabs, None, &a).len(), 1);
        let before = candidate_snapshot(&lib, &tabs, None, &a);
        tabs[0].title = "Updated navigation".into();
        assert_ne!(before, candidate_snapshot(&lib, &tabs, None, &a));
        assert_ne!(candidate_snapshot(&lib, &tabs, None, &a), candidate_snapshot(&lib, &tabs, None, &b));
    }

    #[test]
    fn profile_search_preferences_update_provider_without_cross_profile_fallback() {
        use crossbow_core::{library::Library, model::{ProfilePreferences, SearchEngine}, store::Store};
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        let a = lib.create_profile("Search A").unwrap();
        let b = lib.create_profile("Search B").unwrap();
        let query = "synthetic search & café";
        let before = candidate_snapshot(&lib, &[], None, &a);
        assert_eq!(profile_results(&lib, &a, query, &[])[0].url, Some(SearchEngine::Google.search_url(query)));
        lib.set_profile_preferences(&a, Some(ProfilePreferences { search_engine: Some(SearchEngine::DuckDuckGo), auto_archive_hours: None })).unwrap();
        assert_ne!(before, candidate_snapshot(&lib, &[], None, &a));
        assert_eq!(profile_results(&lib, &a, query, &[])[0].url, Some(SearchEngine::DuckDuckGo.search_url(query)));
        assert_eq!(profile_results(&lib, &b, query, &[])[0].url, Some(SearchEngine::Google.search_url(query)));
        assert_eq!(profile_results(&lib, &a, "https://example.test/path", &[])[0].url.as_deref(), Some("https://example.test/path"));
        lib.profiles.remove(&a);
        assert!(profile_results(&lib, &a, query, &[]).is_empty());
        assert!(profile_results(&lib, "", query, &[]).is_empty());
        assert_eq!(profile_results(&lib, &b, query, &[])[0].url, Some(SearchEngine::Google.search_url(query)));
    }

    #[test]
    fn profile_unavailable_keeps_recovery_actions_without_navigation() {
        use crossbow_core::{library::Library, store::Store};
        let lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        let mut candidates: Vec<_> = COMMANDS.iter().map(|(id, title, keywords, _)| Candidate {
            kind: SuggestionKind::Action { action_id: id }, title: format!("{title} ({keywords})"), url: None, boost: 0,
        }).collect();
        candidates.push(Candidate { kind: SuggestionKind::History, title: "Settings private history".into(), url: Some("https://example.test/settings".into()), boost: 900 });
        for profile in ["", "removed-profile"] {
            for (query, action) in [("settings", "settings"), ("toggle sidebar", "toggle-sidebar"), ("account", "sync"), ("ask ai", "ask-ai")] {
                let results = profile_results(&lib, profile, query, &candidates);
                assert!(results.iter().any(|result| result.kind == SuggestionKind::Action { action_id: action }), "missing recovery action {action}");
                assert!(results.iter().all(|result| matches!(result.kind, SuggestionKind::Action { action_id: "settings" | "toggle-sidebar" | "sync" | "ask-ai" })));
                assert!(results.iter().all(|result| result.url.is_none()));
            }
            for query in ["https://example.test/", "clear unpinned tabs", "new space", "new folder", "welcome tour", "unrelated text"] {
                assert!(profile_results(&lib, profile, query, &candidates).is_empty(), "unsafe fallback for {query}");
            }
        }
    }

    #[test]
    fn rr01_edit_current_results_reject_context_drift_and_keep_recovery() {
        use crate::app::CommandOrigin;
        use crossbow_core::{library::Library, model::{ProfilePreferences, SearchEngine, Theme}, store::{Store, TabRow}};
        for drift_before_open in [false, true] {
            let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
            let a = lib.create_profile("Original context").unwrap();
            let b = lib.create_profile("Other context").unwrap();
            let space = lib.create_space("Search", "S", Theme::preset(0), &a).unwrap();
            lib.set_profile_preferences(&a, Some(ProfilePreferences { search_engine: Some(SearchEngine::Kagi), auto_archive_hours: None })).unwrap();
            lib.set_profile_preferences(&b, Some(ProfilePreferences { search_engine: Some(SearchEngine::Bing), auto_archive_hours: None })).unwrap();
            let tab = crate::tabs::Tab::from_row(TabRow { id: "original-tab".into(), space_id: space.clone(), item_id: None, url: "https://example.test/".into(), title: String::new(), order: "V".into(), created_ms: 1, last_active_ms: 1 });
            if drift_before_open { lib.update_space(&space, |space| space.profile_id = b.clone()).unwrap(); }
            let origin = CommandOrigin::capture(&lib, &space, Some(&tab), Some(&a));
            if !drift_before_open {
                let profile = origin.as_ref().unwrap().profile(&lib, &space, Some(&tab), Some(&a)).unwrap();
                assert_eq!(profile_results(&lib, profile, "synthetic query", &[])[0].url, Some(SearchEngine::Kagi.search_url("synthetic query")));
                lib.update_space(&space, |space| space.profile_id = b.clone()).unwrap();
            }
            let profile = origin.as_ref().and_then(|origin| origin.profile(&lib, &space, Some(&tab), Some(&a))).unwrap_or("");
            assert!(profile_results(&lib, profile, "synthetic query", &[]).is_empty(), "A-context search adopted B's provider");
            let recovery: Vec<_> = COMMANDS.iter().map(|(id, title, _, _)| Candidate { kind: SuggestionKind::Action { action_id: id }, title: title.to_string(), url: None, boost: 0 }).collect();
            for (query, id) in [("Settings", "settings"), ("Account", "sync"), ("Toggle Sidebar", "toggle-sidebar")] {
                let results = profile_results(&lib, profile, query, &recovery);
                assert!(results.iter().any(|result| result.kind == SuggestionKind::Action { action_id: id }));
                assert!(results.iter().all(|result| result.url.is_none()));
            }
        }
    }

    #[test]
    fn history_search_is_profile_scoped_and_invalidated_after_writes() {
        use crossbow_core::{library::Library, store::Store};
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        let a = lib.create_profile("A").unwrap();
        let b = lib.create_profile("B").unwrap();
        let url = "http://127.0.0.1/shared";
        let before = candidate_snapshot(&lib, &[], None, &a);
        lib.store.record_visit(&a, url, "A private title", 1).unwrap();
        assert_ne!(before, candidate_snapshot(&lib, &[], None, &a));
        assert_eq!(history_candidates(&lib, &a)[0].title, "A private title");
        assert!(history_candidates(&lib, &b).is_empty());
        lib.store.record_visit(&b, url, "B private title", 2).unwrap();
        let before = candidate_snapshot(&lib, &[], None, &a);
        lib.store.update_history_title(&a, url, "A updated").unwrap();
        assert_ne!(before, candidate_snapshot(&lib, &[], None, &a));
        assert_eq!(history_candidates(&lib, &a)[0].title, "A updated");
        assert_eq!(history_candidates(&lib, &b)[0].title, "B private title");
        let before = candidate_snapshot(&lib, &[], None, &a);
        lib.store.clear_history(&a).unwrap();
        assert_ne!(before, candidate_snapshot(&lib, &[], None, &a));
        assert!(history_candidates(&lib, &a).is_empty());
        assert_eq!(history_candidates(&lib, &b)[0].title, "B private title");
        lib.profiles.remove(&b);
        assert!(history_candidates(&lib, &b).is_empty(), "stale profile references fail closed");
    }

    #[test]
    fn modal_blocks_floating_chrome_but_keeps_its_own_controls_interactive() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for button in [egui::PointerButton::Primary, egui::PointerButton::Secondary] {
                let ctx = egui::Context::default();
                ctx.all_styles_mut(|style| style.animation_time = 0.0);
                let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
                let layout = BarLayout::new(screen, 1);
                let floating = egui::LayerId::new(egui::Order::Foreground, Id::unique("sidebar-float"));
                let tile = Rect::from_min_size(pos2(18.0, 96.0), vec2(54.0, 44.0));
                let result = Rect::from_min_size(layout.results.min, vec2(layout.results.width(), RESULT_HEIGHT));
                let mut background_clicks = 0;
                let mut dismissals = 0;
                let mut selections = 0;
                // Persisted area order can place the surface before its backdrop.
                for _ in 0..3 {
                    ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| {
                        surface_area(ui.ctx(), layout.rect.min).show(ui.ctx(), |ui| { ui.set_min_size(layout.rect.size()); });
                    }).textures_delta.clear();
                }
                for frame in 0..12 {
                    let position = if frame < 7 { pos2(20.0, 118.0) } else { result.center() };
                    let mut events = vec![egui::Event::PointerMoved(position)];
                    if [4, 5, 8, 9].contains(&frame) {
                        events.push(egui::Event::PointerButton { pos: position, button, pressed: frame % 2 == 0, modifiers: Default::default() });
                    }
                    let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), events, ..Default::default() }, |ui| {
                        crate::ui::content::show_floating_sidebar(ui, screen, |ui| {
                            ui.painter().rect_filled(tile, 0, Color32::RED);
                            let response = ui.interact(tile, Id::unique("floating-favorite-probe"), Sense::click_and_drag());
                            if response.clicked() || response.secondary_clicked() { background_clicks += 1; }
                        });
                        if show_backdrop(ui.ctx(), screen).clicked() { dismissals += 1; }
                        surface_area(ui.ctx(), layout.rect.min).show(ui.ctx(), |ui| {
                            ui.set_min_size(layout.rect.size());
                            ui.painter().rect_filled(result, 0, Color32::BLUE);
                            if ui.interact(result, Id::unique("modal-result-probe"), Sense::click()).clicked() { selections += 1; }
                        });
                        if frame > 2 {
                            assert!(!ui.memory(|memory| memory.allows_interaction(floating)), "floating chrome must be behind the modal");
                        }
                    });
                    if frame > 2 {
                        let painted_at = |color| output.shapes.iter().position(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.fill == color)).expect("painted marker");
                        assert!(painted_at(Color32::RED) < painted_at(Color32::BLUE), "floating chrome must paint below search, not merely reject input");
                    }
                    output.textures_delta.clear();
                }
                assert_eq!(background_clicks, 0, "neither click nor context menu may reach a favorite behind search");
                assert_eq!(dismissals, usize::from(button == egui::PointerButton::Primary));
                assert_eq!(selections, usize::from(button == egui::PointerButton::Primary));
            }
        }
    }

    #[test]
    fn opening_search_keeps_first_frame_text_and_paste() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for edit_current in [false, true] {
                for event in [egui::Event::Text("favorite 17".into()), egui::Event::Paste("favorite 17".into())] {
                    let ctx = egui::Context::default();
                    let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
                    let layout = BarLayout::new(screen, 1);
                    let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
                    let mut bar = crate::app::CommandBar { query: if edit_current { "https://example.test/old".into() } else { String::new() }, edit_current, origin: None, chat_entry: None, needs_focus: true, selected: 0, opened_at: 0.0, results: Vec::new(), last_query: None, last_snapshot: None, scope: None, offers: Vec::new() };
                    for events in [vec![event], vec![egui::Event::Text(" x".into())]] {
                        ctx.run_ui(egui::RawInput { screen_rect: Some(screen), events, ..Default::default() }, |ui| {
                            show_backdrop(ui.ctx(), screen);
                            surface_area(ui.ctx(), layout.rect.min).show(ui.ctx(), |ui| {
                                ui.set_min_size(layout.rect.size());
                                focused_search_field(ui, &layout, &mut bar, false, &pal);
                            });
                        }).textures_delta.clear();
                    }
                    assert_eq!(bar.query, "favorite 17 x", "first-open text must replace the URL for Command-L and keep focus after sizing");
                }
            }
        }
    }

    #[test]
    fn opening_search_takes_input_before_existing_chrome_fields() {
        for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
            for previous in ["find-field", "rename-field"] {
                for edit_current in [false, true] {
                    for event in [egui::Event::Text("favorite 17".into()), egui::Event::Paste("favorite 17".into())] {
                        let ctx = egui::Context::default();
                        let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
                        let layout = BarLayout::new(screen, 1);
                        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
                        let previous_id = Id::unique(previous);
                        let mut underlying = "Keep this text".to_string();
                        for _ in 0..3 {
                            ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| {
                                ui.memory_mut(|memory| memory.request_focus(previous_id));
                                ui.add(egui::TextEdit::singleline(&mut underlying).id(previous_id));
                            }).textures_delta.clear();
                        }
                        let mut bar = crate::app::CommandBar { query: if edit_current { "https://example.test/old".into() } else { String::new() }, edit_current, origin: None, chat_entry: None, needs_focus: true, selected: 0, opened_at: 0.0, results: Vec::new(), last_query: None, last_snapshot: None, scope: None, offers: Vec::new() };
                        for events in [vec![event, key(Key::Enter, egui::Modifiers::NONE)], vec![egui::Event::Text(" x".into())]] {
                            let submit = events.iter().any(|event| matches!(event, egui::Event::Key { key: Key::Enter, .. }));
                            ctx.run_ui(egui::RawInput { screen_rect: Some(screen), events, ..Default::default() }, |ui| {
                                prepare_input(ui.ctx());
                                // Find and inline rename render before the command surface.
                                if ui.memory(|memory| memory.has_focus(previous_id)) {
                                    ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Enter));
                                }
                                ui.add(egui::TextEdit::singleline(&mut underlying).id(previous_id));
                                show_backdrop(ui.ctx(), screen);
                                let keys = BarKeys::take(ui.ctx());
                                surface_area(ui.ctx(), layout.rect.min).show(ui.ctx(), |ui| {
                                    ui.set_min_size(layout.rect.size());
                                    focused_search_field(ui, &layout, &mut bar, false, &pal);
                                });
                                assert_eq!(underlying, "Keep this text", "search must not edit {previous}");
                                assert_eq!(keys.submit, submit.then_some(false), "search owns Return");
                            }).textures_delta.clear();
                        }
                        assert_eq!(bar.query, "favorite 17 x");
                    }
                }
            }
        }
    }

    #[test]
    fn keyboard_search_surfaces_do_not_fade_in() {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, vec2(640.0, 420.0));
        let rect = BarLayout::new(screen, 1).rect;
        for frame in 0..3 {
            let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), time: Some(1.0 + frame as f64 / 60.0), ..Default::default() }, |ui| {
                show_backdrop(ui.ctx(), screen);
                surface_area(ui.ctx(), rect.min).show(ui.ctx(), |ui| {
                    ui.set_min_size(rect.size());
                    ui.painter().rect_filled(rect, 16, Color32::BLUE);
                });
            });
            if frame > 0 {
                let fill = |bounds| output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) if rect.rect == bounds => Some(rect.fill),
                    _ => None,
                });
                assert_eq!(fill(screen), Some(Color32::from_black_alpha(60)), "backdrop must be immediate");
                assert_eq!(fill(rect), Some(Color32::BLUE), "search must reach full contrast without an entrance fade");
            }
            output.textures_delta.clear();
        }
    }

    #[test]
    fn selected_search_text_keeps_contrast_for_every_space_preset() {
        use crate::ui::style::{contrast, over};
        for preset in 0..8 {
            for dark in [false, true] {
                let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(preset), dark);
                let ink = crate::ui::style::Ink::new(pal.panel, pal.accent);
                assert_eq!(ink.accent.a(), 255, "selection fill must not depend on the panel beneath it");
                let fill = ink.accent;
                let ratio = contrast(over(ink.on_accent, fill), fill);
                assert!(ratio >= 4.5, "preset {preset}, dark={dark}, selected text contrast={ratio}");
            }
        }
    }

    #[test]
    fn only_addresses_and_history_from_the_bar_follow_site_rules() {
        let row = |kind, title: &str| Suggestion { kind, title: title.into(), subtitle: String::new(), url: Some("https://github.com/".into()), score: 0 };
        assert!(is_address(&row(SuggestionKind::Navigate, "github.com/rust-lang")));
        assert!(is_address(&row(SuggestionKind::Navigate, "https://github.com")));
        assert!(is_address(&row(SuggestionKind::History, "Rust")));
        assert!(!is_address(&row(SuggestionKind::Navigate, "rust traits")), "a search opens here");
        assert!(!is_address(&row(SuggestionKind::EngineSuggestion, "github")));
    }

    #[test]
    fn return_modifiers_and_reverse_tab_are_exact() {
        for (modifiers, expected) in [
            (egui::Modifiers::NONE, Some(false)),
            (egui::Modifiers::ALT, Some(true)),
            (egui::Modifiers::COMMAND, Some(true)),
            (egui::Modifiers::MAC_CMD | egui::Modifiers::COMMAND, Some(true)),
            (egui::Modifiers::SHIFT, Some(false)),
            (egui::Modifiers::SHIFT | egui::Modifiers::COMMAND, Some(true)),
            (egui::Modifiers::SHIFT | egui::Modifiers::ALT, Some(true)),
            (egui::Modifiers::SHIFT | egui::Modifiers::CTRL, None),
        ] {
            let ctx = egui::Context::default();
            ctx.run_ui(egui::RawInput { events: vec![key(Key::Enter, modifiers)], ..Default::default() }, |ui| {
                let keys = BarKeys::take(ui.ctx());
                assert_eq!((keys.submit, keys.here), (expected, expected.is_some() && modifiers.shift), "{modifiers:?}");
            }).textures_delta.clear();
        }
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput { events: vec![key(Key::Tab, egui::Modifiers::SHIFT), key(Key::ArrowUp, egui::Modifiers::NONE), egui::Event::Text("kept".into())], ..Default::default() }, |ui| {
            assert_eq!(BarKeys::take(ui.ctx()).step, -2);
            assert!(ui.input(|input| input.events.iter().any(|event| matches!(event, egui::Event::Text(text) if text == "kept"))));
        }).textures_delta.clear();
    }

    #[test]
    fn tab_navigation_keeps_search_text_focus_and_does_not_insert_tabs() {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(640.0, 420.0));
        let layout = BarLayout::new(screen, 9);
        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
        let mut query = String::new();
        for events in [vec![], vec![], vec![key(Key::Tab, egui::Modifiers::NONE)], vec![egui::Event::Text("x".into())], vec![key(Key::Tab, egui::Modifiers::SHIFT)], vec![egui::Event::Text("y".into())]] {
            ctx.run_ui(egui::RawInput { screen_rect: Some(screen), events, ..Default::default() }, |ui| {
                if query.is_empty() { ui.memory_mut(|memory| memory.request_focus(Id::unique("cmd-field"))); }
                BarKeys::take(ui.ctx());
                let out = search_field(ui, layout.edit_rect(), &mut query, "Search", &pal);
                ui.button("Another focusable control");
                assert!(out.response.has_focus());
            }).textures_delta.clear();
        }
        assert_eq!(query, "xy");
    }

    #[test]
    fn search_surface_stays_inside_normal_compact_and_intermediate_windows() {
        for size in [vec2(640.0, 420.0), vec2(900.0, 600.0), vec2(1360.0, 860.0)] {
            let screen = Rect::from_min_size(pos2(0.0, 0.0), size);
            for rows in 0..=9 {
                let layout = BarLayout::new(screen, rows);
                assert!(screen.shrink(24.0).contains_rect(layout.rect));
                assert_eq!(layout.field.center().y, layout.edit_rect().center().y);
                assert_eq!(layout.edit_rect().left(), layout.rect.left() + TEXT_COLUMN);
                if rows > 0 {
                    assert!(layout.rect.contains_rect(layout.results));
                    assert_eq!(layout.results.height() % RESULT_HEIGHT, 0.0);
                }
            }
        }
    }

    #[test]
    fn search_text_is_centered_from_real_font_metrics_at_each_scale() {
        for scale in [1.0, 1.5, 2.0] {
            for size in [vec2(640.0, 420.0), vec2(1360.0, 860.0)] {
                let ctx = egui::Context::default();
                crate::ui::window::install_fonts(&ctx);
                ctx.set_pixels_per_point(scale);
                let screen = Rect::from_min_size(pos2(0.0, 0.0), size);
                let layout = BarLayout::new(screen, 9);
                let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
                for content in ["", "Search", "https://example.com/a-very-long-address/".repeat(20).as_str()] {
                    let mut query = content.to_owned();
                    for _ in 0..3 {
                        ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| {
                            let out = search_field(ui, layout.edit_rect(), &mut query, "Search or enter URL…", &pal);
                            let text_center = out.galley_pos.y + out.galley.size().y / 2.0;
                            assert!((text_center - layout.field.center().y).abs() <= 1.0 / scale, "text center {text_center} differs from field {} at {scale}x; response={:?}, clip={:?}, galley={:?}, position={:?}", layout.field.center().y, out.response.rect, out.text_clip_rect, out.galley.rect, out.galley_pos);
                            assert!((out.text_clip_rect.left() - layout.edit_rect().left()).abs() <= 1.0 / scale);
                            assert_eq!(out.galley.rows.len(), 1, "the search field must never wrap");
                        }).textures_delta.clear();
                    }
                }
            }
        }
    }

    /// The bar for one profile, with a fake engine transport and 60 fps frames.
    struct SuggestBar {
        lib: crossbow_core::library::Library,
        profile: String,
        suggest: Suggestions,
        fake: crate::suggestions::tests::Fake,
        now: f64,
    }

    impl SuggestBar {
        fn new() -> Self {
            let mut lib = crossbow_core::library::Library::open(crossbow_core::store::Store::open_in_memory().unwrap()).unwrap();
            let profile = lib.create_profile("Synthetic search").unwrap();
            let (suggest, fake) = crate::suggestions::tests::fake();
            SuggestBar { lib, profile, suggest, fake, now: 0.0 }
        }

        fn settings(&self, change: impl FnOnce(&mut crate::search_engines::SearchPreferences)) {
            let mut preferences = crate::search_engines::SearchPreferences::load(&self.lib, &self.profile).unwrap();
            change(&mut preferences);
            preferences.save(&self.lib, &self.profile).unwrap();
        }

        fn candidates() -> Vec<Candidate> {
            vec![
                Candidate { kind: SuggestionKind::History, title: "Trusty tools".into(), url: Some("https://trusty.example.test/".into()), boost: 400 },
                Candidate { kind: SuggestionKind::Tab { tab_id: "t".into(), space_id: "s".into() }, title: "The Rust Book".into(), url: Some("https://book.example.test/".into()), boost: 0 },
            ]
        }

        /// Type `query` and let `seconds` pass; the rows of the last frame.
        fn type_for(&mut self, query: &str, seconds: f64) -> Vec<Suggestion> {
            let end = self.now + seconds;
            loop {
                self.suggest.receive();
                let rows = suggested_rows(&mut self.suggest, &self.lib, &self.profile, query, &Self::candidates(), self.now);
                self.suggest.send_due(self.now);
                if self.now >= end { return rows; }
                self.now += 1.0 / 60.0;
            }
        }

        fn close(&mut self) {
            self.suggest.want(self.now, None);
        }
    }

    fn titles(rows: &[Suggestion]) -> Vec<(&str, &SuggestionKind)> {
        rows.iter().map(|row| (row.title.as_str(), &row.kind)).collect()
    }

    #[test]
    fn nothing_typed_leaves_the_mac_unless_the_profile_allows_it() {
        use crate::search_engines::tests::engine;
        use crossbow_core::model::{ProfilePreferences, SearchEngine};
        let mut bar = SuggestBar::new();
        bar.type_for("rust lang", 1.0);
        bar.close();
        assert!(bar.fake.sent().is_empty(), "off by default");

        bar.settings(|search| search.suggestions = true);
        for text in ["github.com/rust-lang", "https://example.test/a b", "/Users/me/notes", "~/notes", "my password", "me@example.test", "key=value", "4111 1111 1111 1111", "sk-live-0123456789abcdef"] {
            bar.type_for(text, 1.0);
            bar.close();
        }
        assert!(bar.fake.sent().is_empty(), "addresses, paths and secrets are never sent: {:?}", bar.fake.sent());

        bar.lib.set_profile_preferences(&bar.profile, Some(ProfilePreferences { search_engine: Some(SearchEngine::Perplexity), auto_archive_hours: None })).unwrap();
        bar.type_for("rust lang", 1.0);
        bar.close();
        bar.settings(|search| {
            search.engines = vec![
                engine("plain", "Plain", Some("pl"), "https://plain.example.test/?q=%s", None),
                engine("docs", "Docs", Some("docs"), "https://docs.example.test/?q=%s", Some("https://docs.example.test/suggest?q=%s")),
            ];
            search.default = Some("plain".into());
        });
        bar.type_for("rust lang", 1.0);
        bar.close();
        bar.type_for("pl rust", 1.0);
        bar.close();
        assert!(bar.fake.sent().is_empty(), "no suggestion URL, no request: {:?}", bar.fake.sent());

        // An unreadable profile or unreadable search settings send nothing.
        let profile = std::mem::replace(&mut bar.profile, "missing-profile".into());
        bar.type_for("rust lang", 1.0);
        bar.close();
        bar.profile = profile;
        bar.lib.store.kv_set(&format!("search.v1.{}", bar.profile), "{").unwrap();
        bar.type_for("rust lang", 1.0);
        bar.close();
        assert!(bar.fake.sent().is_empty());

        bar.lib.store.kv_delete(&format!("search.v1.{}", bar.profile)).unwrap();
        bar.lib.set_profile_preferences(&bar.profile, None).unwrap();
        bar.settings(|search| {
            search.suggestions = true;
            search.engines = vec![engine("docs", "Docs", Some("docs"), "https://docs.example.test/?q=%s", Some("https://docs.example.test/suggest?q=%s"))];
        });
        bar.type_for("rust lang", 1.0);
        bar.close();
        // The worker always answers, if only with a timeout; until then nothing else is sent.
        bar.type_for("docs serde json", 1.0);
        assert_eq!(bar.fake.sent().len(), 1, "one request in flight");
        bar.fake.reply(None);
        bar.close();
        bar.type_for("docs serde json", 1.0);
        bar.close();
        assert_eq!(bar.fake.sent(), [
            SearchEngine::Google.suggest_url("rust lang").unwrap(),
            "https://docs.example.test/suggest?q=serde%20json".to_string(),
        ]);
    }

    #[test]
    fn suggestions_rank_after_word_matches_and_failures_keep_local_rows() {
        let mut bar = SuggestBar::new();
        bar.settings(|search| search.suggestions = true);
        let waiting = bar.type_for("rust", 0.1);
        assert!(bar.fake.sent().is_empty(), "typing has not paused");
        let tab = SuggestionKind::Tab { tab_id: "t".into(), space_id: "s".into() };
        assert_eq!(titles(&waiting), [("rust", &SuggestionKind::Navigate), ("The Rust Book", &tab), ("Trusty tools", &SuggestionKind::History)]);
        bar.type_for("rust", 0.2);
        assert_eq!(bar.fake.sent().len(), 1);
        bar.fake.reply(Some(&["rust lang", "rustup"]));
        let rows = bar.type_for("rust", 0.05);
        assert_eq!(titles(&rows), [
            ("rust", &SuggestionKind::Navigate),
            ("The Rust Book", &tab),
            ("rust lang", &SuggestionKind::EngineSuggestion),
            ("rustup", &SuggestionKind::EngineSuggestion),
            ("Trusty tools", &SuggestionKind::History),
        ]);
        assert_eq!(rows[2].url, Some(crossbow_core::model::SearchEngine::Google.search_url("rust lang")));
        // The arrow-key choice stays on its row when suggestions arrive above it.
        assert_eq!(kept_selection(&waiting, 2, &rows), 4);
        assert_eq!(kept_selection(&waiting, 0, &rows), 0);
        assert_eq!(kept_selection(&rows, 4, &waiting), 2);

        // More typing drops the shown suggestions at once; a failure or an
        // offline engine leaves the ordinary rows.
        let typed = bar.type_for("rusty", 0.05);
        assert!(typed.iter().all(|row| row.kind != SuggestionKind::EngineSuggestion));
        bar.type_for("rusty", 0.3);
        bar.fake.reply(None);
        let failed = bar.type_for("rusty", 1.0);
        assert_eq!(failed, typed);
        assert_eq!(bar.fake.sent().len(), 2, "a failure is not retried");
    }

    #[test]
    fn keywords_and_added_defaults_choose_the_engine() {
        use crate::search_engines::tests::engine;
        use crossbow_core::model::SearchEngine;
        let mut bar = SuggestBar::new();
        let github = engine("gh", "GitHub", Some("gh"), "https://github.com/search?q=%s", None);
        let intranet = engine("in", "Intranet", None, "https://search.example.test/find/%s", None);
        bar.settings(|search| search.engines = vec![github.clone(), intranet.clone()]);
        let rows = bar.type_for("GH serde json", 0.0);
        assert_eq!((rows[0].title.as_str(), rows[0].subtitle.as_str(), rows[0].url.as_deref()), ("serde json", "Search GitHub", Some("https://github.com/search?q=serde%20json")));
        // A keyword alone, or an unknown one, searches the default engine for all of it.
        for text in ["gh", "ghx serde"] {
            assert_eq!(bar.type_for(text, 0.0)[0].url, Some(SearchEngine::Google.search_url(text)));
        }
        bar.settings(|search| search.default = Some("in".into()));
        assert_eq!(bar.type_for("rust lang", 0.0)[0].url.as_deref(), Some("https://search.example.test/find/rust%20lang"));
        assert_eq!(bar.type_for("rust lang", 0.0)[0].subtitle, "Search Intranet");
        assert_eq!(crate::search_engines::profile_engine(&bar.lib, &bar.profile), Some(crossbow_core::search::Engine::Custom(intranet)));
        assert_eq!(bar.type_for("example.test/page", 0.0)[0].url.as_deref(), Some("https://example.test/page"));
        // Without the added default, the profile's built-in engine is back.
        bar.settings(|search| { search.engines.retain(|engine| engine.id != "in"); search.default = None; });
        assert_eq!(bar.type_for("rust lang", 0.0)[0].url, Some(SearchEngine::Google.search_url("rust lang")));
        // Command words can't be keywords, so commands keep their meaning.
        let words = command_words();
        assert!(["New", "Find", "Settings", "Ask", "Copy"].iter().all(|word| words.contains(word)));
    }

    const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];

    /// Every text painted in a frame, with where it was painted.
    fn painted_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, Rect)> {
        fn walk(shape: &egui::Shape, out: &mut Vec<(String, Rect)>) {
            match shape {
                egui::Shape::Text(text) => out.push((text.galley.text().to_owned(), text.galley.rect.translate(text.pos.to_vec2()))),
                egui::Shape::Vec(shapes) => for shape in shapes { walk(shape, out) },
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in shapes { walk(&clipped.shape, &mut out); }
        out
    }

    /// Filled rectangles painted in a frame, with their fill.
    fn painted_rects(shapes: &[egui::epaint::ClippedShape]) -> Vec<(Rect, Color32)> {
        shapes.iter().filter_map(|clipped| match &clipped.shape {
            egui::Shape::Rect(rect) => Some((rect.rect, rect.fill)),
            _ => None,
        }).collect()
    }

    /// The open bar over a disposable library, run in the order `show` runs
    /// it: Escape through the app's `cancel_command` first, then Backspace,
    /// keys, the field, ranking, Tab, Return and the rows' text.
    struct TabBar {
        lib: crossbow_core::library::Library,
        space: String,
        profile: String,
        bar: Option<crate::app::CommandBar>,
        suggest: Suggestions,
        fake: crate::suggestions::tests::Fake,
        ctx: egui::Context,
        screen: Rect,
        now: f64,
        focus: crate::app::Focus,
        chosen: Vec<(Suggestion, bool)>,
        actions: Vec<Action>,
        rows: Vec<RowText>,
        field: Option<Rect>,
        surface: Option<Rect>,
        texts: Vec<(String, Rect)>,
        rects: Vec<(Rect, Color32)>,
    }

    impl TabBar {
        fn new(size: egui::Vec2, endpoint: crossbow_ai::local::LocalEndpoint) -> Self {
            let (lib, space, profile) = crate::ai_features::tests::configured(crossbow_core::store::Store::open_in_memory().unwrap(), endpoint);
            let (suggest, fake) = crate::suggestions::tests::fake();
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let mut h = TabBar {
                lib, space, profile, bar: None, suggest, fake, ctx, screen: Rect::from_min_size(pos2(0.0, 0.0), size), now: 0.0,
                focus: crate::app::Focus::Chrome, chosen: Vec::new(), actions: Vec::new(), rows: Vec::new(), field: None, surface: None, texts: Vec::new(), rects: Vec::new(),
            };
            h.reopen();
            h
        }

        /// A newly opened bar, as ⌘T opens it.
        fn reopen(&mut self) {
            self.bar = Some(crate::app::CommandBar {
                query: String::new(), edit_current: false, origin: crate::app::CommandOrigin::capture(&self.lib, &self.space, None, None),
                chat_entry: crate::chat::Entry::capture(&self.lib, &self.space, &None), needs_focus: true,
                selected: 0, opened_at: self.now, results: Vec::new(), last_query: None, last_snapshot: None, scope: None, offers: Vec::new(),
            });
            self.settle();
        }

        fn bar(&self) -> &crate::app::CommandBar { self.bar.as_ref().expect("the bar is open") }

        fn settings(&self, change: impl FnOnce(&mut crate::search_engines::SearchPreferences)) {
            let mut preferences = crate::search_engines::SearchPreferences::load(&self.lib, &self.profile).unwrap();
            change(&mut preferences);
            preferences.save(&self.lib, &self.profile).unwrap();
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            let screen = self.screen;
            let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
            let shortcuts = Shortcuts::defaults();
            self.now += 1.0 / 60.0;
            let mut output = self.ctx.run_ui(egui::RawInput { screen_rect: Some(screen), time: Some(self.now), events, ..Default::default() }, |ui| {
                let ctx = ui.ctx().clone();
                if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Escape)) {
                    crate::app::cancel_command(&ctx, &mut self.bar, &mut self.focus, None);
                }
                let Some(bar) = self.bar.as_mut() else { return };
                prepare_input(&ctx);
                show_backdrop(&ctx, screen);
                if take_backspace(&ctx, bar) { leave_scope(&ctx, bar); }
                let keys = BarKeys::take(&ctx);
                let field_layout = BarLayout::new(screen, 0);
                let mut chosen = None;
                surface_area(&ctx, field_layout.rect.min).show(&ctx, |ui| {
                    let out = focused_search_field(ui, &field_layout, bar, keys.step != 0 || keys.tab != 0, &pal);
                    self.field = Some(out.text_clip_rect);
                    if out.response.changed() { bar.selected = 0; }
                    self.suggest.receive();
                    rank(bar, &mut self.suggest, &self.lib, &self.profile, &self.space, |_| SuggestBar::candidates(), self.now);
                    let entered = take_tab(&ctx, bar, keys.tab, &self.profile, &self.space);
                    if entered { rank(bar, &mut self.suggest, &self.lib, &self.profile, &self.space, |_| SuggestBar::candidates(), self.now); }
                    self.suggest.send_due(self.now);
                    let step = keys.step + if entered { 0 } else { keys.tab };
                    if !bar.results.is_empty() {
                        bar.selected = (bar.selected as isize + step).rem_euclid(bar.results.len() as isize) as usize;
                    }
                    if let Some(new_tab) = keys.submit { chosen = bar.results.get(bar.selected).cloned().map(|row| (row, new_tab)); }
                    let rows = bar.results.len().min(9);
                    let layout = BarLayout::new(screen, rows);
                    ui.set_min_size(layout.rect.size());
                    self.surface = Some(layout.rect);
                    let offer = offered(bar).cloned();
                    self.rows = bar.results.iter().take(rows).enumerate().map(|(i, s)| {
                        let r = Rect::from_min_size(layout.results.min + vec2(0.0, i as f32 * RESULT_HEIGHT), vec2(layout.results.width(), RESULT_HEIGHT));
                        let (label, tab) = trailing(offer.as_ref(), i, s, &shortcuts);
                        row_text(ui.painter(), r, layout.rect.left() + TEXT_COLUMN, s, label, tab, pal.panel_text, pal.panel_dim)
                    }).collect();
                });
                // As `run`: the Ask row can keep the bar open; other rows close it.
                if let Some((row, new_tab)) = chosen {
                    self.chosen.push((row.clone(), new_tab));
                    let close = if row.kind == (SuggestionKind::Action { action_id: ASK_ROW }) {
                        let action = ask_return(&ctx, bar, &self.lib, &self.profile, &self.space);
                        let close = action.is_some();
                        self.actions.extend(action);
                        close
                    } else { true };
                    if close { crate::app::finish_command(&ctx, &mut self.bar, &mut self.focus, None); }
                }
            });
            self.texts = painted_texts(&output.shapes);
            self.rects = painted_rects(&output.shapes);
            output.textures_delta.clear();
        }

        fn settle(&mut self) { for _ in 0..3 { self.frame(Vec::new()); } }

        fn wait(&mut self, seconds: f64) {
            let end = self.now + seconds;
            while self.now < end { self.frame(Vec::new()); }
        }

        fn type_text(&mut self, text: &str) {
            self.frame(vec![egui::Event::Text(text.into())]);
            self.settle();
        }

        fn press(&mut self, key_pressed: Key, modifiers: egui::Modifiers) {
            self.frame(vec![key(key_pressed, modifiers)]);
            self.settle();
        }

        fn focused(&self) -> bool { self.ctx.memory(|memory| memory.has_focus(Id::unique("cmd-field"))) }

        fn painted(&self, text: &str) -> Option<Rect> {
            self.texts.iter().find(|(painted, _)| painted == text).map(|(_, rect)| *rect)
        }

        /// The chip: the accent fill holding `name`.
        fn chip(&self, name: &str) -> (Rect, Rect) {
            let text = self.painted(name).unwrap_or_else(|| panic!("{name} is painted"));
            let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
            let fill = crate::ui::style::Ink::new(pal.panel, pal.accent).accent;
            let chip = self.rects.iter().find(|(rect, color)| *color == fill && rect.contains_rect(text)).map(|(rect, _)| *rect).expect("the name sits on the chip");
            (chip, text)
        }

        fn site(&self, id: &str) -> Target {
            let saved = crate::search_engines::SearchPreferences::load(&self.lib, &self.profile).unwrap();
            Target::Site(Engine::Custom(saved.engines.into_iter().find(|engine| engine.id == id).unwrap()))
        }
    }

    #[test]
    fn tab_scopes_the_search_to_the_engine_the_text_names_and_backspace_or_escape_leave_it() {
        for size in SIZES {
            let mut h = TabBar::new(size, Default::default());
            h.type_text("gi");
            let github = h.site("site-github");
            assert_eq!(h.bar().offers, [(0, github.clone())], "at {size:?}");
            let top = &h.bar().results[0];
            assert_eq!((top.title.as_str(), top.subtitle.as_str()), ("gi", "Search Google"), "Return still searches the profile's engine");
            assert!(h.rows[0].label.is_some() && h.rows[0].key.is_some(), "the top row offers Tab");
            assert!(h.painted("Search GitHub").is_some() && h.painted("Tab").is_some());

            // Tab: a GitHub chip, the text cleared and a Search GitHub placeholder.
            h.press(Key::Tab, egui::Modifiers::NONE);
            assert_eq!(h.bar().scope.as_ref().map(|scope| &scope.target), Some(&github));
            assert_eq!(h.bar().query, "");
            assert!(h.bar().results.is_empty() && h.bar().offers.is_empty());
            let (chip, _) = h.chip("GitHub");
            let placeholder = h.painted("Search GitHub").expect("the placeholder names the engine");
            assert!(placeholder.left() >= chip.right() + CHIP_GAP - 0.5, "the placeholder follows the chip at {size:?}");
            assert!(h.focused());

            // What is typed searches GitHub only.
            h.type_text("serde json");
            let top = &h.bar().results[0];
            assert_eq!((top.subtitle.as_str(), top.url.as_deref()), ("Search GitHub", Some("https://github.com/search?q=serde%20json")));
            assert!(h.bar().offers.is_empty(), "no scope inside a scope");

            // Backspace edits; on the empty field it leaves and gives the typed text back.
            for _ in 0.."serde json".len() { h.press(Key::Backspace, egui::Modifiers::NONE); }
            assert_eq!(h.bar().query, "");
            assert!(h.bar().scope.is_some(), "emptying the field keeps the scope");
            h.press(Key::Backspace, egui::Modifiers::NONE);
            assert!(h.bar().scope.is_none());
            assert_eq!(h.bar().query, "gi");
            h.type_text("t");
            assert_eq!(h.bar().query, "git", "the cursor is after the restored text");

            // Escape leaves the scope and keeps the bar and its focus.
            h.press(Key::Tab, egui::Modifiers::NONE);
            assert!(h.bar().scope.is_some());
            h.press(Key::Escape, egui::Modifiers::NONE);
            assert!(h.bar().scope.is_none() && h.bar().query == "git" && h.focused(), "at {size:?}");
            h.type_text("h");
            assert_eq!(h.bar().query, "gith");

            // Return in the scope opens GitHub's search, ⌘Return in a new tab.
            h.press(Key::Tab, egui::Modifiers::NONE);
            h.type_text("café #1 & 100% a+b");
            h.press(Key::Enter, egui::Modifiers::COMMAND);
            let (row, new_tab) = h.chosen.last().unwrap();
            assert_eq!(row.url.as_deref(), Some("https://github.com/search?q=caf%C3%A9%20%231%20%26%20100%25%20a%2Bb"));
            assert!(*new_tab && h.bar.is_none());

            // Without a scope, Escape closes the bar.
            h.reopen();
            h.press(Key::Escape, egui::Modifiers::NONE);
            assert!(h.bar.is_none());
        }
    }

    #[test]
    fn typing_into_a_new_bar_selects_the_typed_text_row() {
        let mut h = TabBar::new(SIZES[0], Default::default());
        let tab = SuggestionKind::Tab { tab_id: "t".into(), space_id: "s".into() };
        assert_eq!(titles(&h.bar().results), [("Trusty tools", &SuggestionKind::History), ("The Rust Book", &tab)], "an empty bar lists what is open and visited");
        h.type_text("tr");
        assert_eq!(h.bar().results[0].kind, SuggestionKind::Navigate);
        assert!(h.bar().results.iter().any(|row| row.title == "Trusty tools"));
        assert_eq!(h.bar().selected, 0, "the typed text is selected, not the row that was first before typing");
        h.press(Key::Enter, egui::Modifiers::NONE);
        assert_eq!(h.chosen.last().unwrap().0.url, Some(crossbow_core::model::SearchEngine::Google.search_url("tr")));
    }

    #[test]
    fn tab_moves_down_where_nothing_is_offered_and_other_keys_keep_their_meaning() {
        use crate::search_engines::tests::engine;
        let mut h = TabBar::new(SIZES[0], Default::default());
        for typed in ["r", "ru", "rust b"] {
            h.reopen();
            h.type_text(typed);
            assert!(h.bar().offers.is_empty(), "{typed:?} names no engine");
            assert!(h.bar().results.len() > 1);
            h.press(Key::Tab, egui::Modifiers::NONE);
            assert_eq!((h.bar().selected, h.bar().scope.is_some()), (1, false), "Tab moves down for {typed:?}");
            h.press(Key::Tab, egui::Modifiers::SHIFT);
            assert_eq!(h.bar().selected, 0);
        }

        // With an offer, Tab acts only from the top row, where its hint is.
        h.settings(|search| search.engines.push(engine("docs", "The Docs", None, "https://docs.example.test/?q=%s", None)));
        h.reopen();
        h.type_text("th");
        assert_eq!(h.bar().offers, [(0, h.site("docs"))]);
        assert!(h.rows[0].key.is_some() && h.rows[1..].iter().all(|row| row.key.is_none()));
        h.press(Key::ArrowDown, egui::Modifiers::NONE);
        assert!(h.rows.iter().all(|row| row.key.is_none()), "no hint off the row Tab acts on");
        h.press(Key::Tab, egui::Modifiers::NONE);
        assert_eq!((h.bar().selected, h.bar().scope.is_some()), (0, false), "Tab moved on");
        h.press(Key::Tab, egui::Modifiers::NONE);
        assert_eq!(h.bar().scope.as_ref().map(|scope| &scope.target), Some(&h.site("docs")));
        // Arrows and Shift-Tab never enter a scope, and ⌘L stays for the app.
        h.reopen();
        h.type_text("th");
        for (pressed, modifiers) in [(Key::ArrowDown, egui::Modifiers::NONE), (Key::ArrowUp, egui::Modifiers::NONE), (Key::Tab, egui::Modifiers::SHIFT)] {
            h.press(pressed, modifiers);
            assert!(h.bar().scope.is_none());
        }
        let ctx = egui::Context::default();
        let mut bar = h.bar.take().unwrap();
        bar.query.clear();
        bar.scope = Some(Scope { target: h.site("docs"), profile: h.profile.clone(), space: h.space.clone(), before: String::new() });
        ctx.run_ui(egui::RawInput { events: vec![key(Key::L, egui::Modifiers::COMMAND), key(Key::Backspace, egui::Modifiers::NONE)], ..Default::default() }, |ui| {
            assert!(take_backspace(ui.ctx(), &bar), "Backspace in the empty scoped field is the bar's");
            BarKeys::take(ui.ctx());
            assert!(ui.input(|input| input.events.iter().any(|event| matches!(event, egui::Event::Key { key: Key::L, .. }))), "⌘L is left for the app");
        }).textures_delta.clear();
    }

    #[test]
    fn scoped_searches_escape_what_is_typed_into_the_template() {
        let mut h = TabBar::new(SIZES[0], Default::default());
        h.type_text("md");
        h.press(Key::Tab, egui::Modifiers::NONE);
        let long = format!("{} end", "ü".repeat(2_000));
        for (typed, query) in [
            ("a b", "a%20b"), ("R&D", "R%26D"), ("C# 12", "C%23%2012"), ("100%", "100%25"), ("c++ a+b", "c%2B%2B%20a%2Bb"), ("café 東京", "caf%C3%A9%20%E6%9D%B1%E4%BA%AC"),
        ] {
            h.bar.as_mut().unwrap().query = typed.into();
            h.settle();
            assert_eq!(h.bar().results[0].url, Some(format!("https://developer.mozilla.org/en-US/search?q={query}")), "{typed:?}");
        }
        h.bar.as_mut().unwrap().query = long;
        h.settle();
        assert_eq!(h.bar().results[0].url, Some(format!("https://developer.mozilla.org/en-US/search?q={}%20end", "%C3%BC".repeat(2_000))), "long text reaches the site whole");
        // A built-in engine encodes the same way it does without Tab.
        h.reopen();
        h.type_text("du");
        h.press(Key::Tab, egui::Modifiers::NONE);
        h.type_text("R&D #1");
        assert_eq!(h.bar().results[0].url, Some(crossbow_core::model::SearchEngine::DuckDuckGo.search_url("R&D #1")));
    }

    #[test]
    fn scoped_suggestions_come_only_from_that_engine_under_the_profile_rules() {
        use crate::search_engines::tests::engine;
        let mut h = TabBar::new(SIZES[0], Default::default());
        h.settings(|search| {
            search.suggestions = true;
            search.engines.push(engine("docs", "Docs", Some("dd"), "https://docs.example.test/?q=%s", Some("https://docs.example.test/suggest?q=%s")));
        });
        h.reopen();
        h.type_text("do");
        h.press(Key::Tab, egui::Modifiers::NONE);
        let before = h.fake.sent().len();
        h.type_text("serde json");
        h.wait(0.3);
        assert_eq!(h.fake.sent()[before..], ["https://docs.example.test/suggest?q=serde%20json".to_string()]);
        h.fake.reply(Some(&["serde json derive"]));
        h.wait(0.05);
        assert_eq!(titles(&h.bar().results), [("serde json", &SuggestionKind::Navigate), ("serde json derive", &SuggestionKind::EngineSuggestion)]);
        assert_eq!(h.bar().results[1].url.as_deref(), Some("https://docs.example.test/?q=serde%20json%20derive"));

        // Refused text, an engine without a suggestion address, or suggestions
        // turned off send nothing.
        let sent = h.fake.sent().len();
        h.bar.as_mut().unwrap().query = "~/notes/secret plan".into();
        h.wait(0.3);
        h.reopen();
        h.type_text("wi");
        h.press(Key::Tab, egui::Modifiers::NONE);
        h.type_text("rust");
        h.wait(0.3);
        h.settings(|search| search.suggestions = false);
        h.reopen();
        h.type_text("do");
        h.press(Key::Tab, egui::Modifiers::NONE);
        h.type_text("serde");
        h.wait(0.3);
        assert_eq!(h.fake.sent().len(), sent, "nothing more was sent: {:?}", h.fake.sent());
    }

    #[test]
    fn engines_are_offered_per_profile_and_a_scope_ends_when_its_profile_space_or_engine_changes() {
        use crate::search_engines::tests::engine;
        let mut h = TabBar::new(SIZES[0], Default::default());
        let other = h.lib.create_profile("Other").unwrap();
        h.settings(|search| search.engines.push(engine("in", "Intranet", None, "https://search.example.test/?q=%s", None)));
        h.type_text("in");
        assert_eq!(h.bar().offers, [(0, h.site("in"))]);
        let first = std::mem::replace(&mut h.profile, other.clone());
        h.reopen();
        h.type_text("in");
        assert!(h.bar().offers.is_empty(), "another profile's engines are not offered");
        h.profile = first;

        // The engine is deleted: the scope ends and the text stays.
        h.reopen();
        h.type_text("in");
        h.press(Key::Tab, egui::Modifiers::NONE);
        h.type_text("x");
        h.settings(|search| search.engines.retain(|engine| engine.id != "in"));
        h.settle();
        assert!(h.bar().scope.is_none() && h.bar().query == "x");
        assert_eq!(h.bar().results[0].url, Some(crossbow_core::model::SearchEngine::Google.search_url("x")));

        // Another Space, or a profile change, ends a site or Ask scope.
        for change in ["space", "profile"] {
            h.reopen();
            h.type_text("gi");
            h.press(Key::Tab, egui::Modifiers::NONE);
            assert!(h.bar().scope.is_some());
            let (space, profile) = (h.space.clone(), h.profile.clone());
            match change {
                "space" => h.space = h.lib.create_space("Elsewhere", "E", crossbow_core::model::Theme::preset(1), &profile).unwrap(),
                _ => h.profile = other.clone(),
            }
            h.type_text("y");
            assert!(h.bar().scope.is_none(), "a {change} change ends the scope");
            (h.space, h.profile) = (space, profile);
        }
        let mut bar = h.bar.take().unwrap();
        bar.scope = Some(Scope { target: Target::Ask("Ollama"), profile: h.profile.clone(), space: h.space.clone(), before: String::new() });
        assert!(scope_current(&h.lib, &h.profile, &h.space, bar.scope.as_ref().unwrap()));
        assert!(!scope_current(&h.lib, &h.profile, "another-space", bar.scope.as_ref().unwrap()));
        assert!(!scope_current(&h.lib, &other, &h.space, bar.scope.as_ref().unwrap()));
    }

    #[test]
    fn the_chip_and_tab_hint_fit_long_names_at_compact_and_normal_sizes() {
        use crate::search_engines::tests::engine;
        for size in SIZES {
            let mut h = TabBar::new(size, Default::default());
            let long = "Long Name ".repeat(6).trim().to_string();
            h.settings(|search| search.engines.push(engine("long", &long, None, "https://long.example.test/?q=%s", None)));
            h.reopen();
            h.type_text("long n");
            let surface = h.surface.unwrap();
            let row = &h.rows[0];
            let (title, label, key) = (row.title, row.label.expect("Search Long Name…"), row.key.expect("Tab"));
            let row_rect = Rect::from_x_y_ranges(surface.left() + SURFACE_INSET..=surface.right() - SURFACE_INSET, title.y_range());
            assert!(title.right() <= label.left() - 12.0 + 0.5, "the title stops before the hint at {size:?}: {row:?}");
            assert!(label.right() <= key.left() - 6.0 + 0.5 && key.right() <= row_rect.right() + 0.5, "{row:?} at {size:?}");
            assert!(label.width() <= row_rect.width() * 0.5, "a long name is elided at {size:?}");
            assert!(h.painted("Tab").is_some_and(|text| key.contains_rect(text)));

            h.press(Key::Tab, egui::Modifiers::NONE);
            let field = BarLayout::new(h.screen, 0);
            let (chip, text) = h.chip(&long);
            assert!(field.field.contains_rect(chip), "the chip is in the field at {size:?}");
            assert!(chip.width() <= field.edit_rect().width() * 0.4 + 0.5 && text.right() <= chip.right(), "the chip elides a long name at {size:?}: {chip:?}");
            let typing = h.field.unwrap();
            assert!(typing.left() >= chip.right() + CHIP_GAP - 0.5 && typing.right() <= field.edit_rect().right() + 0.5, "the text follows the chip at {size:?}");
            assert!(typing.width() >= field.edit_rect().width() * 0.55, "room is left to type at {size:?}");
            h.type_text(&"query ".repeat(60));
            assert!(h.field.unwrap().left() >= chip.right(), "long text scrolls, never over the chip");
            for (one, other) in [(title, label), (label, key)] {
                assert!(!one.shrink(0.5).intersects(other.shrink(0.5)));
            }

            // A short name is shown whole.
            h.reopen();
            h.type_text("yo");
            h.press(Key::Tab, egui::Modifiers::NONE);
            let (chip, text) = h.chip("YouTube");
            assert!((chip.width() - text.width() - 20.0).abs() < 0.5, "YouTube is not elided at {size:?}");
        }
    }

    #[test]
    fn ask_needs_the_feature_then_tab_and_return_and_refuses_what_must_stay_local() {
        use crate::ask_bar::FEATURE;
        for size in SIZES {
            let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let mut h = TabBar::new(size, crossbow_ai::local::LocalEndpoint { port, ..Default::default() });
            let ask = SuggestionKind::Action { action_id: ASK_ROW };
            let question = "why is the sky blue";

            // Off by default: no Ask row, and Return searches the web.
            h.type_text(question);
            assert!(h.bar().results.iter().all(|row| row.kind != ask) && h.bar().offers.is_empty());
            h.press(Key::Tab, egui::Modifiers::NONE);
            h.press(Key::Enter, egui::Modifiers::NONE);
            assert_eq!(h.chosen.last().unwrap().0.url, Some(crossbow_core::model::SearchEngine::Google.search_url(question)));
            assert!(h.actions.is_empty());

            // On: an Ask Ollama row with Tab's hint; plain Return still searches.
            crate::ai_features::enable(&h.lib, &h.profile, FEATURE, &crate::ai_features::tests::fixture_ready(port)).unwrap();
            h.reopen();
            h.type_text(question);
            let at = h.bar().results.iter().position(|row| row.kind == ask).expect("the Ask row");
            assert_eq!((at, h.bar().results[at].title.as_str()), (1, "Ask Ollama"), "after the typed-text row at {size:?}");
            assert_eq!(h.bar().offers, [(at, Target::Ask("Ollama"))]);
            assert!(h.rows[at].key.is_some() && h.rows[at].label.is_none() && h.rows[0].key.is_none());
            h.press(Key::Enter, egui::Modifiers::NONE);
            assert_eq!(h.chosen.last().unwrap().0.kind, SuggestionKind::Navigate);
            assert!(h.actions.is_empty(), "Return without Tab asks nothing");

            // Tab enters Ask mode with the text kept; Return asks once.
            h.reopen();
            h.type_text(question);
            h.press(Key::Tab, egui::Modifiers::NONE);
            assert_eq!(h.bar().scope.as_ref().map(|scope| &scope.target), Some(&Target::Ask("Ollama")));
            assert_eq!(h.bar().query, question);
            h.chip("Ollama");
            assert_eq!((h.bar().results[0].title.as_str(), h.bar().results[0].subtitle.as_str()), (question, "Ask Ollama"));
            h.press(Key::Enter, egui::Modifiers::NONE);
            assert!(h.bar.is_none());
            assert!(matches!(h.actions.as_slice(), [Action::AskChat(_, text)] if text == question), "one question was handed to Ask AI");

            // Choosing the Ask row with the arrow keys enters Ask mode too, and sends nothing yet.
            h.reopen();
            h.type_text(question);
            h.press(Key::ArrowDown, egui::Modifiers::NONE);
            h.press(Key::Enter, egui::Modifiers::NONE);
            assert_eq!(h.bar().scope.as_ref().map(|scope| &scope.target), Some(&Target::Ask("Ollama")));
            assert_eq!((h.bar().query.as_str(), h.actions.len()), (question, 1));

            // Text the bar never sends stays, with the reason, and nothing is asked.
            for (text, reason) in [
                ("github.com/rust-lang", "Not sent: it looks like a web address."),
                ("~/notes/plan.txt", "Not sent: it looks like a file path."),
                ("my password is hunter2", "Not sent: it looks like a password, key or personal detail."),
            ] {
                h.reopen();
                h.type_text("ask this");
                h.press(Key::Tab, egui::Modifiers::NONE);
                h.press(Key::A, egui::Modifiers::COMMAND);
                h.type_text(text);
                assert_eq!(h.bar().results[0].subtitle, reason);
                h.press(Key::Enter, egui::Modifiers::NONE);
                assert!(h.bar.is_some() && h.bar().query == text && h.bar().scope.is_some(), "{text:?} stays at {size:?}");
                h.press(Key::Escape, egui::Modifiers::NONE);
                assert!(h.bar().scope.is_none() && h.bar().query == text && h.focused(), "Escape leaves Ask mode and keeps the text");
            }
            assert_eq!(h.actions.len(), 1);

            // Without a provider the row says why, and Return keeps the text.
            h.lib.store.kv_delete(&format!("ai.providers.v1.{}", h.profile)).unwrap();
            h.reopen();
            h.type_text(question);
            h.press(Key::Tab, egui::Modifiers::NONE);
            assert_eq!(h.bar().results[0].subtitle, "Choose a provider and model in AI Providers, then Save Selection.");
            h.press(Key::Enter, egui::Modifiers::NONE);
            assert!(h.bar.is_some() && h.bar().query == question && h.actions.len() == 1);

            // The Ask AI command still opens a blank conversation.
            assert!(matches!(h.bar().ask_ai_action(), Action::OpenChat(_)));
            assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock, "nothing reached the loopback server at {size:?}");
        }
    }
}
