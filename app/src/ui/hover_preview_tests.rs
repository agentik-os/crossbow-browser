//! Simulated frames of hover previews: rows offer them as the sidebar's do,
//! at a compact and a normal window, the sidebar's narrowest, default and
//! widest widths, docked left or right and floating.

use super::*;
use crossbow_core::model::Theme;
use crossbow_core::store::{Store, TabRow};
use egui::{Event, Key, Modifiers, PointerButton};

const SIZES: [Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
const WIDTHS: [f32; 3] = [200.0, 252.0, 420.0];
const DAY_MS: u64 = 86_400_000;
const NOW_MS: u64 = 1_800_000_000_000;
const STEP: f64 = 1.0 / 32.0;

/// Personal: collapsed folder Reading list (Article 1–9, then Later › Later 1–2), the
/// pinned Docs (no tab, visited three days ago), Notes (never opened), and the
/// favorite Chat (unloaded, last active two hours ago). Client: folder Client
/// work and a favorite.
struct World {
    dir: std::path::PathBuf,
    lib: Library,
    space: String,
    other_space: String,
    profile: String,
    reading: String,
    articles: Vec<String>,
    docs: String,
    notes: String,
    chat: String,
    client_folder: String,
    client_favorite: String,
    tabs: Vec<Tab>,
}

impl World {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("crossbow-previews-{}", crossbow_core::new_id()));
        std::fs::create_dir(&dir).unwrap();
        let mut lib = Library::open(Store::open(&dir.join("library.sqlite")).unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let space = lib.spaces_ordered()[0].0.clone();
        let profile = lib.spaces[&space].profile_id.clone();
        let reading = lib.create_folder(&space, None, "Reading list").unwrap();
        lib.update_folder(&reading, |folder| folder.collapsed = true).unwrap();
        let mut articles: Vec<String> = (1..=9).map(|n| lib.add_pinned(&space, Some(&reading), &format!("https://news{n}.example/a"), &format!("Article {n}")).unwrap()).collect();
        let later = lib.create_folder(&space, Some(&reading), "Later").unwrap();
        articles.extend((1..=2).map(|n| lib.add_pinned(&space, Some(&later), &format!("https://later{n}.example/"), &format!("Later {n}")).unwrap()));
        let docs = lib.add_pinned(&space, None, "https://docs.example/guide", "Docs").unwrap();
        let notes = lib.add_pinned(&space, None, "https://notes.example/", "Notes").unwrap();
        let chat = lib.add_favorite(&profile, "https://chat.example/", "Chat").unwrap();
        lib.store.record_visit(&profile, "https://docs.example/guide", "Docs", NOW_MS - 3 * DAY_MS).unwrap();
        let client = lib.create_profile("Client").unwrap();
        let other_space = lib.create_space("Client", "C", Theme::preset(2), &client).unwrap();
        let client_folder = lib.create_folder(&other_space, None, "Client work").unwrap();
        lib.update_folder(&client_folder, |folder| folder.collapsed = true).unwrap();
        lib.add_pinned(&other_space, Some(&client_folder), "https://client.example/", "Client plan").unwrap();
        let client_favorite = lib.add_favorite(&client, "https://client.example/", "Client chat").unwrap();
        lib.store.record_visit(&client, "https://docs.example/guide", "Docs", NOW_MS - 60_000).unwrap();
        let tabs = vec![Tab::from_row(TabRow {
            id: "chat-tab".into(), space_id: crate::tabs::FAVORITES_SPACE.into(), item_id: Some(chat.clone()), url: "https://chat.example/".into(),
            title: "Chat".into(), order: "a0".into(), created_ms: 1, last_active_ms: NOW_MS - 2 * 3_600_000,
        })];
        World { dir, lib, space, other_space, profile, reading, articles, docs, notes, chat, client_folder, client_favorite, tabs }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Shown {
    popover: Option<Rect>,
    texts: Vec<(String, Rect, Rect)>,
    action: Option<Action>,
    escaped: bool,
}

impl Shown {
    fn has(&self, text: &str) -> bool {
        self.texts.iter().any(|(shown, ..)| shown == text)
    }

    fn at(&self, text: &str) -> Pos2 {
        self.texts.iter().find(|(shown, ..)| shown == text).unwrap_or_else(|| panic!("{text:?} not drawn")).1.center()
    }
}

/// The sidebar's rows, each offering a preview as sidebar::show does.
struct Probe {
    world: World,
    ctx: egui::Context,
    previews: HoverPreviews,
    favicons: Favicons,
    size: Vec2,
    sidebar: Rect,
    on_right: bool,
    space: String,
    busy: bool,
    rows: Vec<(Subject, Rect, bool)>,
    time: f64,
}

impl Probe {
    /// A docked left sidebar of `width` with the Reading list folder, Docs and
    /// Chat rows near the top.
    fn new(size: Vec2, width: f32) -> Self {
        let world = World::new();
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        let favicons = Favicons::new(&world.dir);
        let sidebar = Rect::from_min_size(Pos2::ZERO, vec2(width, size.y));
        let space = world.space.clone();
        let mut probe = Probe { world, ctx, previews: HoverPreviews::default(), favicons, size, sidebar, on_right: false, space, busy: false, rows: Vec::new(), time: 0.0 };
        probe.rows_at(140.0);
        probe
    }

    /// Rows of 32 points from `top`: the folder, Docs, Notes and Chat.
    fn rows_at(&mut self, top: f32) {
        let row = |index: usize| Rect::from_min_size(pos2(self.sidebar.left() + 10.0, top + index as f32 * 34.0), vec2(self.sidebar.width() - 20.0, 32.0));
        self.rows = vec![
            (Subject::Folder(self.world.reading.clone()), row(0), true),
            (Subject::Item(self.world.docs.clone()), row(1), false),
            (Subject::Item(self.world.notes.clone()), row(2), false),
            (Subject::Item(self.world.chat.clone()), row(3), false),
        ];
    }

    fn row(&self, index: usize) -> Rect {
        self.rows[index].1
    }

    /// One frame, 1/32 s after the last: exact in binary, so delays end on a known frame.
    fn frame(&mut self, events: Vec<Event>) -> Shown {
        self.time += STEP;
        let (mut action, mut escaped) = (None, false);
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)), time: Some(self.time), events, ..Default::default() };
        let mut output = self.ctx.run_ui(input, |ui| {
            // As handle_shortcuts: a shown preview takes Escape first.
            if ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) { escaped = self.previews.dismiss(); }
            let mut list = ui.new_child(egui::UiBuilder::new().max_rect(self.sidebar));
            list.set_clip_rect(self.sidebar.intersect(ui.clip_rect()));
            for (index, (subject, row, keyboard)) in self.rows.iter().enumerate() {
                let response = list.interact(*row, Id::unique(("preview-probe-row", index)), Sense::click_and_drag());
                self.previews.offer(&list, subject.clone(), *row, &response, &self.space, *keyboard);
            }
            let cx = Cx {
                lib: &self.world.lib, tabs: &self.world.tabs, favicons: &mut self.favicons, profile: &self.world.lib.spaces[&self.space].profile_id,
                space: &self.space, sidebar: self.sidebar, on_right: self.on_right, busy: self.busy, now: self.time, now_ms: NOW_MS, pal: &crate::ui::theme::palette(&Theme::preset(0), false),
            };
            action = self.previews.finish(&list, cx);
        });
        output.textures_delta.clear();
        let texts = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()), shape.clip_rect)),
            _ => None,
        }).collect();
        Shown { popover: self.previews.shown(), texts, action, escaped }
    }

    /// Frames with nothing new for `seconds`.
    fn wait(&mut self, seconds: f64) -> Shown {
        let end = self.time + seconds;
        let mut shown = self.frame(vec![]);
        while self.time < end - 1e-9 { shown = self.frame(vec![]); }
        shown
    }

    fn point(&mut self, pos: Pos2) -> Shown {
        self.frame(vec![Event::PointerMoved(pos)])
    }

    /// Rest the pointer on a row until its preview shows. A new egui area is
    /// laid out unseen for one frame, so this waits a frame past the delay.
    #[track_caller]
    fn rest_on(&mut self, index: usize) -> Shown {
        self.point(self.row(index).center());
        let shown = self.wait(DELAY + 2.0 * STEP);
        assert!(shown.popover.is_some(), "the preview shows after resting at {:?}", self.size);
        shown
    }

    fn click(&mut self, pos: Pos2) -> Shown {
        self.point(pos);
        self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
        self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }])
    }

    fn key(&mut self, key: Key) -> Shown {
        self.key_with(key, Modifiers::NONE)
    }

    fn key_with(&mut self, key: Key, modifiers: Modifiers) -> Shown {
        self.frame([true, false].map(|pressed| Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }).into())
    }

    /// Frames until the preview shows, for at most `seconds`.
    fn until_shown(&mut self, seconds: f64) -> Shown {
        let end = self.time + seconds;
        let mut shown = self.frame(vec![]);
        while shown.popover.is_none() && self.time < end { shown = self.frame(vec![]); }
        shown
    }
}

fn articles(n: std::ops::RangeInclusive<usize>) -> Vec<String> {
    n.map(|n| format!("Article {n}")).collect()
}

#[test]
fn a_folder_previews_its_first_tabs_after_resting_or_at_once_with_the_keyboard() {
    for size in SIZES {
        let mut probe = Probe::new(size, 252.0);
        let start = probe.time + STEP;
        probe.point(probe.row(0).center());
        let mut shown = probe.frame(vec![]);
        while shown.popover.is_none() && probe.time < start + 2.0 { shown = probe.frame(vec![]); }
        // egui sees the hover a frame late; the popover is laid out unseen on its first frame.
        let after = probe.time - start;
        assert!((DELAY..=DELAY + 3.0 * STEP).contains(&after), "shows 600 ms after the pointer rests, not {after} s, at {size:?}");
        let shown = probe.frame(vec![]);
        assert!(shown.has("Reading list") && shown.has("+3 more"));
        for title in articles(1..=8) { assert!(shown.has(&title), "{title} is listed at {size:?}"); }
        assert!(!shown.has("Article 9") && !shown.has("Later 1"), "only the first 8 at {size:?}");
        for host in ["news1.example", "news8.example"] { assert!(shown.has(host)); }
        let order: Vec<f32> = articles(1..=8).iter().map(|title| shown.at(title).y).collect();
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "in sidebar order");

        // Tab to the folder row: its preview shows at once.
        let mut probe = Probe::new(size, 252.0);
        probe.point(pos2(size.x - 20.0, size.y - 20.0));
        probe.key(Key::Tab);
        assert!(probe.until_shown(0.2).popover.is_some(), "focus shows it without the 600 ms wait at {size:?}");
        assert!(probe.frame(vec![]).has("Article 1"));
        // The pointer's preview still shows on a row below the focused folder.
        probe.rest_on(1);
        let shown = probe.wait(0.5);
        assert!(shown.has("docs.example") && !shown.has("Article 1"), "the focused folder does not take over at {size:?}");
        probe.point(pos2(size.x - 20.0, size.y - 20.0));
        assert!(probe.wait(GRACE + DELAY).popover.is_none(), "focus alone does not bring it back at {size:?}");
        // Focus on an unloaded pin does not: those preview on hover only.
        probe.key(Key::Tab);
        assert!(probe.wait(GRACE + DELAY).popover.is_none(), "keyboard focus on a pin shows nothing at {size:?}");
        probe.key_with(Key::Tab, Modifiers::SHIFT);
        assert!(probe.until_shown(0.2).popover.is_some(), "focus coming back shows it again at {size:?}");
        // A menu closes it, and it stays closed while the folder keeps focus.
        probe.busy = true;
        assert!(probe.frame(vec![]).popover.is_none());
        probe.busy = false;
        assert!(probe.wait(GRACE + DELAY).popover.is_none(), "a closed keyboard preview stays closed at {size:?}");
    }
}

#[test]
fn moving_into_the_preview_keeps_it_and_leaving_closes_it_after_a_grace() {
    for size in SIZES {
        let mut probe = Probe::new(size, 252.0);
        let popover = probe.rest_on(0).popover.unwrap();
        // Through the gap between the sidebar and the popover, into it.
        probe.point(pos2(probe.sidebar.right() + 3.0, probe.row(0).center().y));
        let shown = probe.point(popover.center());
        assert!(shown.popover.is_some(), "crossing the gap keeps it at {size:?}");
        assert!(probe.wait(1.0).popover.is_some(), "it stays while the pointer is on it at {size:?}");
        // Out to the page: it waits a moment, then closes.
        let away = pos2(size.x - 10.0, size.y - 10.0);
        assert!(probe.point(away).popover.is_some(), "a short grace at {size:?}");
        assert!(probe.wait(GRACE + 2.0 * STEP).popover.is_none(), "closed after the grace at {size:?}");
        // Moving to another row starts that row's own wait.
        probe.rest_on(0);
        probe.point(probe.row(1).center());
        assert!(probe.frame(vec![]).popover.is_none(), "the next row waits its own 600 ms at {size:?}");
    }
}

#[test]
fn escape_scroll_a_drag_a_menu_a_space_switch_and_hiding_the_sidebar_close_it() {
    for size in SIZES {
        let mut probe = Probe::new(size, 252.0);
        probe.point(probe.row(0).center());
        assert!(!probe.key(Key::Escape).escaped, "Escape before it shows is left to the page");
        let shown = { probe.rest_on(0); probe.key(Key::Escape) };
        assert!(shown.escaped && shown.popover.is_none(), "Escape closes it and nothing else takes Escape at {size:?}");
        assert!(probe.wait(0.2).popover.is_none(), "it stays closed while the pointer rests at {size:?}");

        probe.rest_on(1);
        let wheel = Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -40.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Modifiers::NONE };
        assert!(probe.frame(vec![wheel]).popover.is_none(), "scrolling closes it at {size:?}");
        assert!(probe.wait(0.3).popover.is_none(), "it stays closed while the scroll runs on at {size:?}");

        probe.rest_on(0);
        probe.busy = true;
        assert!(probe.frame(vec![]).popover.is_none(), "a drag or a menu closes it at {size:?}");
        probe.busy = false;

        probe.rest_on(0);
        probe.space = probe.world.other_space.clone();
        assert!(probe.frame(vec![]).popover.is_none(), "a Space switch closes it at {size:?}");
        probe.space = probe.world.space.clone();

        // A hidden sidebar draws no rows and no preview for a while.
        probe.rest_on(0);
        for _ in 0..3 {
            probe.time += STEP;
            probe.ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), time: Some(probe.time), ..Default::default() }, |_| {}).textures_delta.clear();
        }
        assert!(probe.frame(vec![]).popover.is_none(), "an old preview does not come back with the sidebar at {size:?}");
        assert!(probe.wait(DELAY + 2.0 * STEP).popover.is_some(), "resting again shows it again at {size:?}");
    }
}

#[test]
fn clicking_a_listed_tab_opens_it_and_hovering_loads_nothing() {
    for size in SIZES {
        let mut probe = Probe::new(size, 252.0);
        let tabs_before: Vec<String> = probe.world.tabs.iter().map(|tab| tab.id.clone()).collect();
        let mut shown = probe.rest_on(0);
        for _ in 0..20 {
            assert!(shown.action.is_none(), "hovering and showing start no navigation at {size:?}");
            shown = probe.frame(vec![]);
        }
        for index in [1, 3] {
            probe.point(pos2(size.x - 10.0, size.y - 10.0));
            probe.wait(GRACE + 2.0 * STEP);
            let shown = probe.rest_on(index);
            assert!(shown.action.is_none() && probe.wait(0.5).action.is_none(), "an unloaded tab's preview loads nothing at {size:?}");
        }
        assert_eq!(probe.world.tabs.iter().map(|tab| tab.id.clone()).collect::<Vec<_>>(), tabs_before, "no tab was opened");
        assert!(probe.world.tabs.iter().all(|tab| tab.view.is_none()), "no page was loaded");

        probe.point(pos2(size.x - 10.0, size.y - 10.0));
        probe.wait(GRACE + 2.0 * STEP);
        let target = probe.rest_on(0).at("Article 3");
        let shown = probe.click(target);
        assert!(matches!(&shown.action, Some(Action::OpenItem(id)) if *id == probe.world.articles[2]), "the click opens that tab, as in the folder, at {size:?}");
        assert!(shown.popover.is_none(), "and the preview closes");
    }
}

#[test]
fn unloaded_pins_and_favorites_show_their_site_and_last_opening() {
    let world = World::new();
    let p = &world.profile;
    assert_eq!(content(&world.lib, &world.tabs, p, &Subject::Item(world.docs.clone())),
        Some(Content::Item { title: "Docs".into(), host: "docs.example".into(), opened: Some(NOW_MS - 3 * DAY_MS) }), "from this profile's history, not Client's newer visit");
    assert_eq!(content(&world.lib, &world.tabs, p, &Subject::Item(world.chat.clone())),
        Some(Content::Item { title: "Chat".into(), host: "chat.example".into(), opened: Some(NOW_MS - 2 * 3_600_000) }), "from its unloaded tab");
    assert_eq!(content(&world.lib, &world.tabs, p, &Subject::Item(world.notes.clone())).map(|content| match content { Content::Item { opened, .. } => opened, _ => unreachable!() }), Some(None));
    for (opened, label) in [
        (None, "No visit on this Mac yet"),
        (Some(NOW_MS - 5_000), "Last opened just now"),
        (Some(NOW_MS + 5_000), "Last opened just now"),
        (Some(NOW_MS - 120_000), "Last opened 2 min ago"),
        (Some(NOW_MS - 2 * 3_600_000), "Last opened 2 h ago"),
        (Some(NOW_MS - DAY_MS - 1_000), "Last opened yesterday"),
        (Some(NOW_MS - 3 * DAY_MS), "Last opened 3 days ago"),
    ] {
        assert_eq!(opened_label(opened, NOW_MS), label);
    }
    for size in SIZES {
        let mut probe = Probe::new(size, 252.0);
        let shown = probe.rest_on(1);
        for text in ["Docs", "docs.example", "Last opened 3 days ago"] { assert!(shown.has(text), "{text} at {size:?}"); }
        probe.point(pos2(size.x - 10.0, size.y - 10.0));
        probe.wait(GRACE + 2.0 * STEP);
        let shown = probe.rest_on(3);
        for text in ["Chat", "chat.example", "Last opened 2 h ago"] { assert!(shown.has(text), "{text} at {size:?}"); }
    }
}

#[test]
fn an_expanded_folder_shows_no_preview_by_pointer_or_keyboard() {
    for size in SIZES {
        let mut probe = Probe::new(size, 252.0);
        let reading = probe.world.reading.clone();
        probe.world.lib.update_folder(&reading, |folder| folder.collapsed = false).unwrap();
        assert_eq!(content(&probe.world.lib, &probe.world.tabs, &probe.world.profile, &Subject::Folder(reading.clone())), None, "its tabs already show below it");
        probe.point(probe.row(0).center());
        assert!(probe.wait(DELAY + 0.2).popover.is_none(), "resting shows nothing at {size:?}");
        probe.point(pos2(size.x - 20.0, size.y - 20.0));
        probe.key(Key::Tab);
        assert!(probe.until_shown(0.3).popover.is_none(), "focus shows nothing at {size:?}");
        // Collapsed again, it previews as before.
        probe.world.lib.update_folder(&reading, |folder| folder.collapsed = true).unwrap();
        probe.point(probe.row(0).center());
        probe.rest_on(0);
    }
}

#[test]
fn another_profiles_folders_and_items_are_never_previewed() {
    let world = World::new();
    let personal = &world.profile;
    let client = world.lib.spaces[&world.other_space].profile_id.clone();
    for subject in [Subject::Folder(world.client_folder.clone()), Subject::Item(world.client_favorite.clone())] {
        assert_eq!(content(&world.lib, &world.tabs, personal, &subject), None, "{subject:?} is Client's");
    }
    for subject in [Subject::Folder(world.reading.clone()), Subject::Item(world.docs.clone()), Subject::Item(world.chat.clone())] {
        assert_eq!(content(&world.lib, &world.tabs, &client, &subject), None, "{subject:?} is Personal's");
    }
    assert_eq!(content(&world.lib, &world.tabs, personal, &Subject::Folder("missing".into())), None);
    // Each profile still previews its own.
    assert!(matches!(content(&world.lib, &world.tabs, personal, &Subject::Folder(world.reading.clone())), Some(Content::Folder { more: 3, .. })));
    assert!(matches!(content(&world.lib, &world.tabs, &client, &Subject::Folder(world.client_folder.clone())), Some(Content::Folder { ref entries, .. }) if entries.len() == 1 && entries[0].title == "Client plan"));
    assert!(content(&world.lib, &world.tabs, &client, &Subject::Item(world.client_favorite.clone())).is_some());
    // A row offering another profile's folder shows nothing.
    let mut probe = Probe::new(SIZES[0], 252.0);
    probe.rows[0].0 = Subject::Folder(probe.world.client_folder.clone());
    probe.point(probe.row(0).center());
    assert!(probe.wait(DELAY + 0.2).popover.is_none());
}

#[test]
fn the_preview_stays_in_the_window_beside_the_sidebar_at_every_size() {
    for size in SIZES {
        for width in WIDTHS {
            for layout in ["left", "right", "floating"] {
                let mut probe = Probe::new(size, width);
                probe.sidebar = match layout {
                    "left" => Rect::from_min_size(Pos2::ZERO, vec2(width, size.y)),
                    "right" => Rect::from_min_size(pos2(size.x - width, 0.0), vec2(width, size.y)),
                    _ => Rect::from_min_max(pos2(8.0, 8.0), pos2(8.0 + width, size.y - 8.0)),
                };
                probe.on_right = layout == "right";
                for top in [100.0, size.y / 2.0, size.y - 46.0 - 4.0 * 34.0] {
                    probe.rows_at(top);
                    for index in [0, 1] {
                        probe.point(pos2(size.x / 2.0, 4.0));
                        probe.wait(GRACE + 2.0 * STEP);
                        let shown = probe.rest_on(index);
                        let popover = shown.popover.unwrap();
                        let context = format!("{layout} sidebar {width} wide, row {index} at {top}, window {size:?}");
                        let window = Rect::from_min_size(Pos2::ZERO, size);
                        assert!(window.shrink(MARGIN - 0.5).contains_rect(popover), "inside the window: {context}: {popover:?}");
                        assert!(!popover.intersects(probe.sidebar), "beside the sidebar, clear of the row and its controls: {context}");
                        assert!(popover.top() <= probe.row(index).top() + 0.5, "level with the row or moved up to fit: {context}");
                        assert!(popover.width() >= MIN_WIDTH, "{context}");
                        let inside: Vec<_> = shown.texts.iter().filter(|(_, bounds, _)| bounds.intersects(popover)).collect();
                        assert!(inside.len() >= 3, "{context}");
                        for (i, (text, bounds, clip)) in inside.iter().enumerate() {
                            assert!(popover.shrink(PADDING - 0.5).contains_rect(*bounds), "{text:?} stays in the card: {context}");
                            assert!(clip.contains_rect(*bounds), "{text:?} is not clipped: {context}");
                            for (other, theirs, _) in &inside[i + 1..] {
                                assert!(!bounds.shrink(0.5).intersects(theirs.shrink(0.5)), "{text:?} overlaps {other:?}: {context}");
                            }
                        }
                    }
                }
            }
        }
    }
}
