//! The shield and its menu, driven by simulated pointer and keyboard input
//! at 640×420 and 1360×860 over the real content blocking host, on a
//! disposable library and data folder. Every site and list is a fixture.

use super::*;
use crate::content_blocking::tests::{ADS, Harness as Blocking};
use crate::content_blocking::NO_LISTS;
use crate::ui::overlays::{menu_frame, menu_surface};
use crate::ui::sidebar::{pill_control, pill_text_right};
use crossbow_core::content_blocking::exceptions::SAVE_FAILED;
use crossbow_core::content_blocking::lists::ListKind;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Vec2, pos2, vec2};

const SIZES: [Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
const TAB: &str = "fixture-tab";
const PAGE: &str = "https://www.news.example/today";

/// What one frame drew: every text with its bounds and clip.
struct Drawn {
    text: Vec<(String, Rect, Rect)>,
}

impl Drawn {
    fn shows(&self, text: &str) -> bool {
        self.text.iter().any(|(shown, ..)| shown == text)
    }

    fn bounds(&self, text: &str) -> Rect {
        self.text.iter().find(|(shown, ..)| shown == text).unwrap_or_else(|| panic!("{text:?} is not drawn: {:?}", self.labels())).1
    }

    fn labels(&self) -> Vec<&str> {
        self.text.iter().map(|(text, ..)| text.as_str()).collect()
    }
}

/// The sidebar's address pill with the shield after the key and blocked
/// icons, the page's blocked count, and the menu while open, with picks
/// applied as the app applies them.
struct Page {
    blocking: Blocking,
    size: Vec2,
    frame: u32,
    page: String,
    count: u32,
    menu: Option<Pos2>,
    outcome: Option<Outcome>,
    toasts: Vec<String>,
    settings: usize,
    reloads: usize,
}

impl Page {
    fn new(size: Vec2) -> Page {
        let blocking = Blocking::new();
        crate::ui::window::install_fonts(&blocking.ctx);
        Page { blocking, size, frame: 0, page: PAGE.into(), count: 0, menu: None, outcome: None, toasts: Vec::new(), settings: 0, reloads: 0 }
    }

    /// Blocking on, with the fixture ads list.
    fn with_ads(size: Vec2) -> Page {
        let mut page = Page::new(size);
        let ads = page.blocking.file("fixture-ads.txt", ADS);
        page.blocking.add(ListKind::Ads, &ads);
        page.blocking.switch(ListKind::Ads, true);
        page
    }

    fn pill(&self) -> Rect {
        Rect::from_min_size(pos2(8.0, 40.0), vec2(232.0, 34.0))
    }

    /// The shield's place: after the key and the blocked icon, both shown.
    fn at(&self) -> Rect {
        pill_control(self.pill(), 2)
    }

    fn frame(&mut self, events: Vec<Event>) -> Drawn {
        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
        self.frame += 1;
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)), time: Some(f64::from(self.frame) / 60.0), events, ..Default::default() };
        let at = self.at();
        let b = &mut self.blocking;
        b.host.poll(&b.ctx);
        let (host, store, profile, page, count) = (&mut b.host, &b.lib.store, b.profile.clone(), self.page.clone(), self.count);
        let (menu, outcome) = (&mut self.menu, &mut self.outcome);
        let mut output = b.ctx.run_ui(input, |ui| {
            let ctx = ui.ctx().clone();
            // The app's dismissal chain closes any open menu on Escape.
            if crate::app::take_shortcut(&ctx, Modifiers::NONE, Key::Escape) { *menu = None; }
            let shown = shield(host, store, &profile, &page, count);
            if let Some(shown) = &shown && let Some(state) = draw_button(ui, at, shown, TAB, pal.text_dim, pal.hover) {
                assert!(matches!(&state.kind, crate::app::MenuKind::ContentBlocking { tab } if tab == TAB));
                *menu = Some(state.pos);
            }
            let (Some(pos), Some(shown)) = (*menu, &shown) else { return };
            let m = MenuUi::new(&pal, crate::ui::folders::take_menu_focus(&ctx));
            let mut pick = None;
            let area = menu_surface(pos).show(&ctx, |ui| {
                menu_frame(&pal).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    pick = menu_entries(&m, ui, shown);
                });
            });
            if let Some(pick) = pick { *outcome = Some(apply_pick(host, store, &profile, shown, pick, &ctx)); }
            // As the app closes its menus: a press outside.
            if crate::ui::overlays::pressed_outside(&ctx, pos, area.response.rect) { *menu = None; }
        });
        output.textures_delta.clear();
        if let Some(outcome) = self.outcome.take() {
            if outcome.close { self.menu = None; }
            self.toasts.extend(outcome.toast);
            if outcome.settings { self.settings += 1; }
            // A reload starts the page's count again.
            if outcome.reload { self.reloads += 1; self.count = 0; }
        }
        let text = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()), shape.clip_rect)),
            _ => None,
        }).collect();
        Drawn { text }
    }

    fn settle(&mut self) -> Drawn {
        self.blocking.settle();
        for _ in 0..3 { self.frame(Vec::new()); }
        self.frame(Vec::new())
    }

    fn click_at(&mut self, pos: Pos2) -> Drawn {
        self.frame(vec![Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
        }
        self.settle()
    }

    fn click(&mut self, label: &str) -> Drawn {
        let pos = self.settle().bounds(label).center();
        self.click_at(pos)
    }

    /// Opens the menu with the pointer and checks it fits.
    fn open(&mut self) -> Drawn {
        self.settle();
        let drawn = self.click_at(self.at().center());
        assert!(self.menu.is_some(), "the shield opens its menu at {:?}", self.size);
        self.check_menu(&drawn);
        drawn
    }

    fn key(&mut self, key: Key) -> Drawn {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }]);
        }
        self.settle()
    }

    /// The tooltip after resting the pointer on the shield.
    fn tooltip(&mut self) -> Drawn {
        self.settle();
        self.frame(vec![Event::PointerMoved(self.at().center())]);
        for _ in 0..60 { self.frame(Vec::new()); }
        self.frame(Vec::new())
    }

    fn menu_rect(&self) -> Rect {
        self.blocking.ctx.memory(|memory| memory.area_rect(egui::Id::unique("context-menu"))).expect("the menu is shown")
    }

    /// Every label is whole inside its clip, the menu and the window, and
    /// clear of the others.
    fn check_menu(&self, drawn: &Drawn) {
        let menu = self.menu_rect();
        let window = Rect::from_min_size(Pos2::ZERO, self.size);
        assert!(window.contains_rect(menu), "the menu {menu:?} stays on screen at {:?}", self.size);
        let labels: Vec<_> = drawn.text.iter().filter(|(text, bounds, _)| !text.is_empty() && menu.intersects(*bounds)).collect();
        for (text, bounds, clip) in &labels {
            assert!(clip.expand(0.5).contains_rect(*bounds) && menu.expand(0.5).contains_rect(*bounds), "{text:?} is clipped at {:?}: {bounds:?} in {clip:?}, menu {menu:?}", self.size);
        }
        for (i, (text, bounds, _)) in labels.iter().enumerate() {
            for (other, other_bounds, _) in &labels[i + 1..] {
                assert!(!bounds.shrink(0.5).intersects(other_bounds.shrink(0.5)), "{text:?} overlaps {other:?} at {:?}", self.size);
            }
        }
    }

    fn site_blocking(&mut self) -> Option<(String, bool)> {
        let b = &mut self.blocking;
        b.host.site_blocking(&b.lib.store, &b.profile, &self.page)
    }
}

#[test]
fn the_shield_counts_this_page_s_blocked_requests_and_turns_blocking_off_and_on_for_the_site() {
    for size in SIZES {
        let mut page = Page::with_ads(size);
        page.count = 3;
        assert!(page.tooltip().shows("3 requests blocked on this page"), "the tooltip at {size:?}");
        let drawn = page.open();
        for label in ["news.example", "3 requests blocked on this page", TURN_OFF, SETTINGS] {
            assert!(drawn.shows(label), "{label:?} at {size:?}: {:?}", drawn.labels());
        }
        assert!(!drawn.shows(TURN_ON));

        page.click(TURN_OFF);
        assert!(page.menu.is_none() && page.toasts.is_empty(), "it closes at {size:?}");
        assert_eq!(page.reloads, 1, "the page reloads without blocking at {size:?}");
        assert_eq!(page.site_blocking(), Some(("news.example".into(), false)));
        assert!(!page.blocking.blocks("https://ads.example/a.js", PAGE));
        assert!(page.tooltip().shows(OFF_HERE), "at {size:?}");
        let drawn = page.open();
        assert!(drawn.shows(OFF_HERE) && drawn.shows(TURN_ON) && !drawn.shows(TURN_OFF), "{:?}", drawn.labels());
        page.key(Key::Escape);

        // Read back after reopening.
        page.blocking.reopen();
        let drawn = page.open();
        assert!(drawn.shows(OFF_HERE) && drawn.shows(TURN_ON), "after reopening at {size:?}: {:?}", drawn.labels());
        page.click(TURN_ON);
        assert_eq!(page.reloads, 2);
        assert_eq!(page.site_blocking(), Some(("news.example".into(), true)));
        assert!(page.blocking.blocks("https://ads.example/a.js", PAGE), "on again at {size:?}");

        for (count, note) in [(0, NONE_BLOCKED.to_owned()), (1, "1 request blocked on this page".into()), (12_345, "12,345 requests blocked on this page".into())] {
            page.count = count;
            let drawn = page.open();
            assert!(drawn.shows(&note), "{note:?} at {size:?}: {:?}", drawn.labels());
            page.key(Key::Escape);
        }

        assert!(!page.blocking.host.reveal);
        page.open();
        page.click(SETTINGS);
        assert!(page.settings == 1 && page.menu.is_none(), "at {size:?}");
        assert!(page.blocking.host.reveal, "Settings opens at Content Blocking at {size:?}");
    }
}

#[test]
fn the_shield_shows_on_web_pages_while_a_switch_is_on_or_the_site_has_blocking_off() {
    for size in SIZES {
        let mut page = Page::new(size);
        page.settle();
        page.click_at(page.at().center());
        assert!(page.menu.is_none(), "no shield while blocking was never turned on at {size:?}");

        page.blocking.switch(ListKind::CookieBanners, true);
        let drawn = page.open();
        assert!(drawn.shows(NO_LISTS) && drawn.shows(TURN_OFF), "a switch with no list says so at {size:?}: {:?}", drawn.labels());
        page.key(Key::Escape);

        for other in ["file:///fixture/notes.html", "about:blank", "chrome://settings"] {
            page.page = other.into();
            page.click_at(page.at().center());
            assert!(page.menu.is_none(), "no shield on {other:?} at {size:?}");
        }

        // Off on a site, with every switch off: still shown, to turn it back on.
        page.page = PAGE.into();
        page.blocking.switch(ListKind::CookieBanners, false);
        let b = &mut page.blocking;
        b.host.set_site_blocking(&b.lib.store, &b.profile.clone(), "news.example", false, &b.ctx).unwrap();
        let drawn = page.open();
        assert!(drawn.shows(OFF_HERE) && drawn.shows(TURN_ON), "at {size:?}: {:?}", drawn.labels());
    }
}

#[test]
fn a_long_site_name_wraps_inside_the_menu() {
    for size in SIZES {
        let mut page = Page::with_ads(size);
        page.page = "https://www.a-very-long-registrable-domain-name-used-only-as-a-fixture.example/".into();
        page.open();
    }
}

#[test]
fn the_shield_and_its_menu_work_from_the_keyboard_and_escape_closes_the_menu() {
    for size in SIZES {
        let mut page = Page::with_ads(size);
        page.settle();
        page.key(Key::Tab);
        page.key(Key::Enter);
        assert!(page.menu.is_some(), "Enter on the focused shield opens the menu at {size:?}");
        page.key(Key::Escape);
        assert!(page.menu.is_none(), "Escape closes it at {size:?}");
        assert_eq!(page.site_blocking(), Some(("news.example".into(), true)), "and changes nothing");

        // Opened from the keyboard, the menu focuses its first entry.
        page.key(Key::Tab);
        page.key(Key::Space);
        page.key(Key::Tab);
        page.key(Key::Enter);
        assert_eq!(page.settings, 1, "Tab then Enter on {SETTINGS:?} at {size:?}");
        assert!(page.blocking.host.reveal, "Settings opens at Content Blocking at {size:?}");
        assert!(page.menu.is_none());
        assert_eq!(page.site_blocking(), Some(("news.example".into(), true)));

        page.key(Key::Tab);
        page.key(Key::Enter);
        page.key(Key::Enter);
        assert_eq!(page.site_blocking(), Some(("news.example".into(), false)), "Enter on {TURN_OFF:?} at {size:?}");
    }
}

#[test]
fn a_failed_save_says_why_keeps_blocking_on_and_a_retry_works() {
    for size in SIZES {
        let mut page = Page::with_ads(size);
        let conn = rusqlite::Connection::open(page.blocking.fixture.path()).unwrap();
        conn.execute_batch("CREATE TRIGGER refuse_exceptions BEFORE INSERT ON kv WHEN NEW.k LIKE 'content_blocking.%' BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
        page.open();
        page.click(TURN_OFF);
        assert_eq!(page.toasts, [SAVE_FAILED], "says why at {size:?}");
        assert!(page.menu.is_none() && page.reloads == 0, "closes without reloading at {size:?}");
        assert_eq!(page.site_blocking(), Some(("news.example".into(), true)), "still on");
        assert!(page.blocking.blocks("https://ads.example/a.js", PAGE));

        conn.execute_batch("DROP TRIGGER refuse_exceptions").unwrap();
        page.open();
        page.click(TURN_OFF);
        assert_eq!(page.site_blocking(), Some(("news.example".into(), false)), "the retry works at {size:?}");
        assert_eq!((page.toasts.len(), page.reloads), (1, 1));
    }
}

#[test]
fn the_shield_follows_the_key_and_blocked_icons_in_the_pill_and_the_top_strip() {
    let pill = Rect::from_min_size(pos2(8.0, 40.0), vec2(232.0, 34.0));
    let slots: Vec<Rect> = (0..4).map(|slot| pill_control(pill, slot)).collect();
    for (i, slot) in slots.iter().enumerate() {
        assert!(pill.contains_rect(*slot), "slot {i} stays in the pill");
        for other in &slots[i + 1..] { assert!(!slot.intersects(*other), "slots {i} and the next don't overlap"); }
    }
    assert!(pill_text_right(pill, true, 3) <= slots[3].left(), "the address ends before Copy URL, past three controls");
    let strip = Rect::from_min_size(Pos2::ZERO, vec2(640.0, 38.0));
    let row = crate::ui::window::ControlRow::new(strip, true, strip.height());
    for index in 0..3 { assert!(!row.trailing_control(index).intersects(row.trailing_control(index + 1))); }

    // Wiring: both places draw the shield after the key and the blocked
    // icon, the shared menu draws its menu, and Escape's dismissal chain
    // closes any open menu.
    let sidebar = include_str!("sidebar.rs");
    assert!(sidebar.contains("crate::ui::content_blocking_shield::button(app, ui, pill_control(pill, usize::from(key) + usize::from(blocked)), pal.text_dim, pal.hover)"));
    assert!(sidebar.contains("let controls = usize::from(key) + usize::from(blocked) + usize::from(shield);"));
    let content = include_str!("content.rs");
    assert!(content.contains("crate::ui::content_blocking_shield::button(app, ui, row.trailing_control(usize::from(key) + usize::from(blocked)), pal.text_dim, pal.hover)"));
    assert!(include_str!("overlays.rs").contains("MenuKind::ContentBlocking { tab } => done = crate::ui::content_blocking_shield::menu(app, &m, ui, tab),"));
    assert!(include_str!("../app.rs").contains("if take_dismiss_shortcut(ctx) {\n            if self.ui.menu.is_some() {\n                self.ui.menu = None;"));
}

#[test]
fn a_click_whose_press_and_release_arrive_in_one_frame_opens_the_menu_and_keeps_it_open() {
    for size in SIZES {
        let mut page = Page::with_ads(size);
        page.settle();
        // A busy app (a resize, a page's dialog, a fill) runs no frame
        // between the press and the release, so both arrive together.
        let at = page.at().center();
        let click = |pos: Pos2| vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }, Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }];
        page.frame(click(at));
        assert!(page.menu.is_some(), "the first click opens the menu at {size:?}");
        let drawn = page.settle();
        assert!(page.menu.is_some() && drawn.shows(SETTINGS), "and it stays open at {size:?}");

        // A press outside still closes it, and the next click opens it again.
        page.frame(click(pos2(size.x - 20.0, size.y - 20.0)));
        assert!(page.menu.is_none(), "a click outside closes it at {size:?}");
        page.settle();
        page.frame(click(at));
        assert!(page.menu.is_some(), "at {size:?}");
        page.settle();
        page.click(SETTINGS);
        assert_eq!(page.settings, 1, "its entries work at {size:?}");
    }
}
