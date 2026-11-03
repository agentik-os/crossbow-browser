use super::super::Page;
use super::super::repair_tests::{PreferenceFrame, settings_test_frame};
use super::*;
use crate::content_blocking::tests::{ADS, FIXTURE_BUNDLED, Harness as Blocking};
use crate::content_blocking::{ADD_LIST, BlockAds, BlockCookieBanners, INCLUDED, LOADING, NO_LISTS, TURNED_OFF};
use crate::link_prefs::SwitchSpec;
use crossbow_core::content_blocking::bundled::Bundled;
use crossbow_core::content_blocking::lists::{AddError, ListKind};
use egui::{Key, Modifiers, vec2};

const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
const PAGE: &str = "https://news.example/";

/// The section on a disposable library and data folder, for the default
/// Space and its profile, with a fake file panel.
struct Harness {
    blocking: Blocking,
    space: String,
    size: egui::Vec2,
    frame: u32,
    /// While set, frames take no finished work, as while lists compile.
    hold: bool,
}

impl Harness {
    fn new(size: egui::Vec2) -> Self {
        Self::with_bundled(size, &[])
    }

    /// With `bundled` as the lists that come with the app.
    fn with_bundled(size: egui::Vec2, bundled: &'static [&'static Bundled]) -> Self {
        let blocking = Blocking::with_bundled(bundled);
        crate::ui::window::install_fonts(&blocking.ctx);
        let space = blocking.lib.spaces_ordered()[0].0.clone();
        Harness { blocking, space, size, frame: 0, hold: false }
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> PreferenceFrame {
        let b = &mut self.blocking;
        if !self.hold { b.host.poll(&b.ctx); }
        settings_test_frame(&b.ctx, Page::Privacy, self.size, 1.0, &mut self.frame, events, |ui, pal| {
            show(&mut b.host, &b.lib, &self.space, ui, pal);
        })
    }

    /// Settled, with every label whole across and its shown part clear of
    /// the switches and the others. The page scrolls, so a label may be cut
    /// at the top or bottom.
    fn settle(&mut self) -> PreferenceFrame {
        if !self.hold { self.blocking.settle(); }
        for _ in 0..6 { self.frame(vec![]); }
        let shown = self.frame(vec![]);
        for (i, (text, bounds, clip)) in shown.text.iter().enumerate() {
            assert!(clip.min.x - 0.5 <= bounds.min.x && bounds.max.x <= clip.max.x + 0.5 && (text == ADD_LIST || !text.ends_with('…')), "{text:?} is clipped at {:?}", self.size);
            let visible = bounds.intersect(*clip).shrink(0.5);
            if !visible.is_positive() { continue; }
            assert!(shown.switches.iter().all(|switch| !switch.intersects(visible)), "{text:?} runs into a switch at {:?}", self.size);
            for (other, other_bounds, other_clip) in &shown.text[i + 1..] {
                let other_visible = other_bounds.intersect(*other_clip).shrink(0.5);
                assert!(!other_visible.is_positive() || !visible.intersects(other_visible), "{text:?} overlaps {other:?} at {:?}", self.size);
            }
        }
        shown
    }

    /// One wheel step inside the page; positive scrolls up.
    fn wheel(&mut self, delta: f32) {
        let rect = super::super::sheet_rect(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size), 1.0);
        let at = rect.right_bottom() - vec2(60.0, 40.0);
        self.frame(vec![
            egui::Event::PointerMoved(at),
            egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, delta), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Modifiers::NONE },
        ]);
        // Scrolling is animated.
        self.settle();
    }

    /// Every label wholly shown somewhere on the page, top to bottom, with
    /// its place on the page rather than on screen.
    fn scan(&mut self) -> Vec<(String, egui::Rect)> {
        for _ in 0..3 { self.wheel(4000.0); }
        let mut seen: Vec<(String, egui::Rect)> = Vec::new();
        for _ in 0..60 {
            let shown = self.settle();
            for (text, bounds, clip) in &shown.text {
                if !clip.expand(0.5).contains_rect(*bounds) { continue; }
                let on_page = bounds.translate(vec2(0.0, shown.scroll_offset.y));
                if !seen.iter().any(|(seen, at)| seen == text && (at.top() - on_page.top()).abs() < 1.0) { seen.push((text.clone(), on_page)); }
            }
            self.wheel(-120.0);
            if self.settle().scroll_offset.y == shown.scroll_offset.y { break; }
        }
        seen.sort_by(|a, b| a.1.top().total_cmp(&b.1.top()));
        seen
    }

    fn shows(&mut self, label: &str) -> bool {
        self.scan().iter().any(|(text, _)| text == label)
    }

    /// How many times `label` is on the page.
    fn count(&mut self, label: &str) -> usize {
        self.scan().iter().filter(|(text, _)| text == label).count()
    }

    /// Scrolls until the `index`th `label` from the top is wholly shown;
    /// its place on screen, and the frame.
    fn reveal(&mut self, label: &str, index: usize) -> (egui::Rect, PreferenceFrame) {
        let (_, target) = self.scan().into_iter().filter(|(text, _)| text == label).nth(index).unwrap_or_else(|| panic!("no {label:?} #{index} at {:?}", self.size));
        for _ in 0..3 { self.wheel(4000.0); }
        for _ in 0..60 {
            let shown = self.settle();
            let found = shown.text.iter().find(|(text, bounds, clip)| text == label && clip.expand(0.5).contains_rect(*bounds) && (bounds.top() + shown.scroll_offset.y - target.top()).abs() < 1.0).map(|(_, bounds, _)| *bounds);
            if let Some(bounds) = found { return (bounds, shown); }
            self.wheel(-120.0);
        }
        panic!("{label:?} #{index} never came into view at {:?}", self.size)
    }

    fn click_at(&mut self, pos: egui::Pos2) {
        for pressed in [true, false] {
            self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
        }
        self.settle();
    }

    /// Click the `index`th `label`, from the top.
    fn click_nth(&mut self, label: &str, index: usize) {
        let (bounds, _) = self.reveal(label, index);
        self.click_at(bounds.center());
    }

    fn click(&mut self, label: &str) {
        self.click_nth(label, 0);
    }

    /// The switch beside `title`, centred on its row below the title's top,
    /// scrolling on a little when the switch is still out of view.
    fn switch_beside(&mut self, title: &str) -> Option<egui::Rect> {
        self.reveal(title, 0);
        for _ in 0..6 {
            let shown = self.settle();
            let label = shown.text.iter().find(|(text, bounds, clip)| text == title && clip.expand(0.5).contains_rect(*bounds)).map(|(_, bounds, _)| *bounds);
            let switch = label.and_then(|label| shown.switches.iter().find(|switch| switch.center().y >= label.top() - 2.0 && switch.center().y <= label.top() + 80.0).copied());
            if switch.is_some() { return switch; }
            self.wheel(-40.0);
        }
        None
    }

    /// Flip the switch beside `title`.
    fn toggle(&mut self, title: &str) {
        let switch = self.switch_beside(title).unwrap_or_else(|| panic!("no switch beside {title:?} at {:?}", self.size));
        self.click_at(switch.center());
    }

    fn key(&mut self, key: Key) {
        self.key_with(key, Modifiers::NONE);
    }

    fn key_with(&mut self, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }]);
        }
        self.settle();
    }

    /// The panel answers with `path`, then the list is added off the UI thread.
    fn choose(&mut self, path: &std::path::Path) {
        self.blocking.panel.answer(Ok(Some(path.to_owned())));
        self.settle();
    }
}

#[test]
fn the_section_shows_both_switches_off_with_their_own_add_buttons() {
    for size in SIZES {
        let mut h = Harness::new(size);
        let shown = h.settle();
        for label in ["CONTENT BLOCKING", "For Personal · this device only", BlockAds::TITLE, BlockAds::DESCRIPTION, BlockCookieBanners::TITLE, BlockCookieBanners::DESCRIPTION] {
            assert!(shown.text.iter().any(|(text, ..)| text == label), "{label:?} at {size:?}");
        }
        assert!(BlockAds::DESCRIPTION.contains("simplified site rule"), "the third-party heuristic is disclosed");
        // At 640×420 the page scrolls to the second switch.
        for title in [BlockAds::TITLE, BlockCookieBanners::TITLE] {
            assert!(h.switch_beside(title).is_some(), "a switch beside {title:?} at {size:?}");
        }
        assert_eq!(h.count(ADD_LIST), 2, "one per switch at {size:?}");
        assert!(!h.shows(NO_LISTS), "said only while a switch is on");
        h.toggle(BlockCookieBanners::TITLE);
        assert_eq!(h.count(NO_LISTS), 1, "under the switch that is on at {size:?}");
        let (profile, other) = (h.blocking.profile.clone(), h.blocking.other.clone());
        assert!(h.blocking.host.profile(&h.blocking.lib.store, &profile).enabled(ListKind::CookieBanners));
        assert!(!h.blocking.host.profile(&h.blocking.lib.store, &other).enabled(ListKind::CookieBanners), "another profile's own choice");
    }
}

#[test]
fn a_list_added_with_the_pointer_shows_its_counts_and_remove_takes_it_away() {
    for size in SIZES {
        let mut h = Harness::new(size);
        h.toggle(BlockAds::TITLE);
        assert!(h.shows(NO_LISTS));
        h.click_nth(ADD_LIST, 0);
        assert_eq!(h.blocking.panel.opened(), 1, "the file panel opens at {size:?}");
        let ads = h.blocking.file("ads.txt", ADS);
        h.choose(&ads);
        for label in ["ads.txt", "3 rules · 1 unsupported, skipped", "Added “ads.txt”."] {
            assert!(h.shows(label), "{label:?} at {size:?}");
        }
        assert!(!h.shows(NO_LISTS));
        assert!(h.blocking.blocks("https://ads.example/a.js", PAGE), "applied at once at {size:?}");
        assert_eq!(h.blocking.lists(ListKind::CookieBanners).len(), 0, "added under the button's own switch");

        h.toggle(BlockAds::TITLE);
        assert!(h.shows("ads.txt") && !h.blocking.blocks("https://ads.example/a.js", PAGE), "kept while off, not applied");
        h.toggle(BlockAds::TITLE);
        assert!(h.blocking.blocks("https://ads.example/a.js", PAGE));

        h.click("Remove");
        assert!(!h.shows("ads.txt") && h.shows("Removed “ads.txt”.") && h.shows(NO_LISTS), "at {size:?}");
        assert!(!h.blocking.blocks("https://ads.example/a.js", PAGE));
        h.blocking.reopen();
        assert!(!h.shows("ads.txt"), "still removed after reopening at {size:?}");
    }
}

#[test]
fn add_and_remove_work_from_the_keyboard() {
    for size in SIZES {
        let mut h = Harness::new(size);
        h.settle();
        // The ads switch, then its Add List from File….
        h.key(Key::Tab);
        h.key(Key::Space);
        assert!(h.blocking.host.profile(&h.blocking.lib.store, &h.blocking.profile.clone()).enabled(ListKind::Ads), "Space flips the focused switch at {size:?}");
        h.key(Key::Tab);
        h.key(Key::Enter);
        assert_eq!(h.blocking.panel.opened(), 1, "Enter opens the panel at {size:?}");
        let ads = h.blocking.file("ads.txt", ADS);
        h.choose(&ads);
        assert_eq!(h.blocking.lists(ListKind::Ads).len(), 1);
        assert!(h.blocking.blocks("https://ads.example/a.js", PAGE));
        // Focus stays on Add List from File…; the new row's Remove is just before it.
        h.key_with(Key::Tab, Modifiers::SHIFT);
        h.key(Key::Enter);
        assert!(h.blocking.lists(ListKind::Ads).is_empty(), "Enter on Remove at {size:?}");
    }
}

#[test]
fn a_refused_file_says_why_and_the_lists_stay() {
    for size in SIZES {
        let mut h = Harness::new(size);
        h.toggle(BlockAds::TITLE);
        h.click_nth(ADD_LIST, 0);
        let ads = h.blocking.file("ads.txt", ADS);
        h.choose(&ads);
        h.click_nth(ADD_LIST, 0);
        let empty = h.blocking.file("empty.txt", "! nothing to use\n");
        h.choose(&empty);
        assert!(h.shows(&AddError::NoRules.to_string()), "says why at {size:?}");
        assert!(h.shows("ads.txt") && !h.shows("empty.txt"));
        assert!(h.blocking.blocks("https://ads.example/a.js", PAGE));
        // Try again with another file.
        h.click_nth(ADD_LIST, 0);
        let more = h.blocking.file("more.txt", crate::content_blocking::tests::MORE_ADS);
        h.choose(&more);
        assert!(h.shows("more.txt") && h.shows("Added “more.txt”.") && !h.shows(&AddError::NoRules.to_string()), "at {size:?}");
    }
}

#[test]
fn a_failed_switch_save_keeps_the_choice_until_retry_or_cancel() {
    for size in SIZES {
        let mut h = Harness::new(size);
        h.blocking.lib = crossbow_core::library::Library::open(h.blocking.fixture.read_only()).unwrap();
        h.toggle(BlockAds::TITLE);
        let profile = h.blocking.profile.clone();
        assert!(!h.blocking.host.profile(&h.blocking.lib.store, &profile).enabled(ListKind::Ads), "the failed choice is not used at {size:?}");
        let shown = h.settle();
        let message = &shown.text.iter().find(|(text, ..)| text.starts_with("Could not save “Block ads and trackers”")).expect("says why").0;
        assert!(message.contains(BlockAds::STILL_OFF), "{message}");
        h.click("Cancel");
        h.blocking.lib = crossbow_core::library::Library::open(h.blocking.fixture.store()).unwrap();
        h.toggle(BlockAds::TITLE);
        assert!(h.blocking.host.profile(&h.blocking.lib.store, &profile).enabled(ListKind::Ads), "saved at {size:?}");
    }
}

/// Turns blocking off on `sites` for the harness's profile.
fn turn_off(h: &mut Harness, sites: &[&str]) {
    let b = &mut h.blocking;
    let profile = b.profile.clone();
    for site in sites { b.host.set_site_blocking(&b.lib.store, &profile, site, false, &b.ctx).unwrap(); }
}

#[test]
fn sites_with_blocking_off_are_listed_and_remove_turns_blocking_back_on() {
    for size in SIZES {
        let mut h = Harness::new(size);
        assert!(!h.shows(crate::content_blocking::SITES_OFF), "only once a site has blocking off at {size:?}");
        let ads = h.blocking.file("ads.txt", ADS);
        h.blocking.add(ListKind::Ads, &ads);
        h.blocking.switch(ListKind::Ads, true);
        turn_off(&mut h, &["news.example", "shop.example"]);
        let other = h.blocking.other.clone();
        let b = &mut h.blocking;
        b.host.set_site_blocking(&b.lib.store, &other, "other.example", false, &b.ctx).unwrap();
        for label in [crate::content_blocking::SITES_OFF, "news.example", "shop.example"] {
            assert!(h.shows(label), "{label:?} at {size:?}");
        }
        assert!(!h.shows("other.example"), "another profile's sites stay out at {size:?}");
        assert!(!h.blocking.blocks("https://ads.example/a.js", "https://news.example/"));

        // The list's own Remove comes first; then one per site.
        assert_eq!(h.count("Remove"), 3, "at {size:?}");
        h.click_nth("Remove", 1);
        assert!(!h.shows("news.example") && h.shows("shop.example"), "at {size:?}");
        assert!(h.blocking.blocks("https://ads.example/a.js", "https://news.example/"), "blocking is back on there at {size:?}");
        assert_eq!(h.blocking.lists(ListKind::Ads).len(), 1, "the list stays");
        h.blocking.reopen();
        assert!(!h.shows("news.example") && h.shows("shop.example"), "read back after reopening at {size:?}");
        h.click_nth("Remove", 1);
        assert!(!h.shows(crate::content_blocking::SITES_OFF), "gone with the last site at {size:?}");
    }
}

#[test]
fn a_site_s_remove_works_from_the_keyboard() {
    for size in SIZES {
        let mut h = Harness::new(size);
        turn_off(&mut h, &["news.example"]);
        h.settle();
        // Each switch and its Add List from File…, then the site's Remove.
        for _ in 0..5 { h.key(Key::Tab); }
        h.key(Key::Enter);
        assert!(!h.shows("news.example"), "Enter on the site's Remove at {size:?}");
        let b = &mut h.blocking;
        assert_eq!(b.host.site_blocking(&b.lib.store, &b.profile.clone(), "https://news.example/"), Some(("news.example".to_owned(), true)));
    }
}

#[test]
fn a_failed_remove_says_why_keeps_the_site_and_a_retry_works() {
    for size in SIZES {
        let mut h = Harness::new(size);
        turn_off(&mut h, &["news.example"]);
        let conn = rusqlite::Connection::open(h.blocking.fixture.path()).unwrap();
        conn.execute_batch("CREATE TRIGGER refuse_exceptions BEFORE DELETE ON kv WHEN OLD.k LIKE 'content_blocking.%' BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
        h.click("Remove");
        assert!(h.shows(crossbow_core::content_blocking::exceptions::SAVE_FAILED) && h.shows("news.example"), "at {size:?}");
        conn.execute_batch("DROP TRIGGER refuse_exceptions").unwrap();
        h.click("Remove");
        assert!(!h.shows("news.example") && !h.shows(crossbow_core::content_blocking::exceptions::SAVE_FAILED), "the retry works at {size:?}");
    }
}

#[test]
fn unreadable_sites_with_blocking_off_say_so() {
    for size in SIZES {
        let mut h = Harness::new(size);
        let profile = h.blocking.profile.clone();
        h.blocking.lib.store.kv_set(&crossbow_core::content_blocking::exceptions::key(&profile), "{not json").unwrap();
        h.blocking.reopen();
        assert!(h.shows(crossbow_core::content_blocking::exceptions::UNREADABLE), "at {size:?}");
    }
}

#[test]
fn hiding_rules_past_the_bounds_are_counted_in_the_section() {
    let many: String = (0..crossbow_core::content_blocking::hiding::MAX_SELECTORS + 3).map(|i| format!("##.g{i}\n")).collect();
    for size in SIZES {
        let mut h = Harness::new(size);
        let file = h.blocking.file("many.txt", &many);
        h.blocking.add(ListKind::Ads, &file);
        assert!(!h.shows("3 cosmetic rules skipped: too many"), "not while the switch is off at {size:?}");
        h.toggle(BlockAds::TITLE);
        assert!(h.shows("3 cosmetic rules skipped: too many"), "at {size:?}");
    }
}

#[test]
fn bundled_lists_show_their_credit_and_turn_off_and_on_with_the_pointer() {
    for size in SIZES {
        let mut h = Harness::with_bundled(size, &FIXTURE_BUNDLED);
        for label in ["Fixture Ads", "Fixture Trackers", INCLUDED, "Fixture lists by the Crossbow tests."] {
            assert!(h.shows(label), "{label:?} at {size:?}");
        }
        assert_eq!(h.count("Turn Off"), 2, "one per bundled list at {size:?}");
        assert_eq!(h.count("Fixture lists by the Crossbow tests."), 1, "one credit for the lists that share it");
        assert!(!h.shows(NO_LISTS));
        h.toggle(BlockAds::TITLE);
        for label in ["2 rules", "1 rule · 1 unsupported, skipped"] {
            assert!(h.shows(label), "{label:?} at {size:?}");
        }
        assert!(h.blocking.blocks("https://bundled-tracker.example/t.js", PAGE));
        h.click_nth("Turn Off", 1);
        for label in ["Turned off “Fixture Trackers”.", TURNED_OFF, "Turn On"] {
            assert!(h.shows(label), "{label:?} at {size:?}");
        }
        assert!(!h.blocking.blocks("https://bundled-tracker.example/t.js", PAGE));
        h.blocking.reopen();
        assert!(h.shows(TURNED_OFF), "after reopening at {size:?}");
        h.click("Turn On");
        assert!(h.shows("Turned on “Fixture Trackers”.") && h.blocking.blocks("https://bundled-tracker.example/t.js", PAGE), "at {size:?}");
    }
}

#[test]
fn a_bundled_list_turns_off_from_the_keyboard() {
    for size in SIZES {
        let mut h = Harness::with_bundled(size, &FIXTURE_BUNDLED);
        h.settle();
        // The ads switch, then the first bundled list's Turn Off.
        h.key(Key::Tab);
        h.key(Key::Space);
        h.key(Key::Tab);
        h.key(Key::Enter);
        assert_eq!(h.blocking.bundled(ListKind::Ads)[0].1, false, "Enter on Turn Off at {size:?}");
        assert!(!h.blocking.blocks("https://bundled-ads.example/a.js", PAGE));
        h.key(Key::Enter);
        assert_eq!(h.blocking.bundled(ListKind::Ads)[0].1, true, "focus stays on the button, now Turn On, at {size:?}");
    }
}

#[test]
fn while_bundled_lists_load_the_section_says_requests_still_load() {
    for size in SIZES {
        let mut h = Harness::with_bundled(size, &FIXTURE_BUNDLED);
        let profile = h.blocking.profile.clone();
        h.blocking.host.profile(&h.blocking.lib.store, &profile).ads.choose(&h.blocking.lib.store, true);
        h.blocking.host.apply(&profile, &h.blocking.ctx);
        // Frames that don't take the compiled lists, as while they compile.
        h.hold = true;
        assert!(h.shows(LOADING), "at {size:?}");
        h.hold = false;
        h.settle();
        assert!(!h.shows(LOADING) && h.shows("2 rules"), "at {size:?}");
    }
}

#[test]
fn a_failed_turn_off_says_why_beside_the_lists_and_a_retry_works() {
    let failed = "Couldn't save the change. Check disk space and data-folder permissions, then try again.";
    for size in SIZES {
        let mut h = Harness::with_bundled(size, &FIXTURE_BUNDLED);
        h.toggle(BlockAds::TITLE);
        // The lists folder's place is taken by a file, so nothing can be saved there.
        let folder = h.blocking.data.path().join(crossbow_core::content_blocking::lists::FOLDER);
        std::fs::write(&folder, "").unwrap();
        h.click_nth("Turn Off", 1);
        assert!(h.shows(failed) && h.count("Turn Off") == 2 && !h.shows(TURNED_OFF), "still on at {size:?}");
        assert!(h.blocking.blocks("https://bundled-tracker.example/t.js", PAGE));
        std::fs::remove_file(&folder).unwrap();
        h.click_nth("Turn Off", 1);
        assert!(h.shows("Turned off “Fixture Trackers”.") && h.shows(TURNED_OFF) && !h.shows(failed), "the retry works at {size:?}");
        assert!(!h.blocking.blocks("https://bundled-tracker.example/t.js", PAGE));
    }
}

#[test]
fn content_blocking_settings_from_the_shield_shows_the_section_from_its_top() {
    use crate::ui::content_blocking_shield::{Pick, apply_pick, shield};
    for size in SIZES {
        let mut h = Harness::with_bundled(size, &FIXTURE_BUNDLED);
        // A page that scrolls at both sizes: added lists and sites with blocking off.
        for n in 0..8 {
            let list = h.blocking.file(&format!("ads-{n}.txt"), &format!("{ADS}! {n}\n"));
            h.blocking.add(ListKind::Ads, &list);
        }
        h.blocking.switch(ListKind::Ads, true);
        turn_off(&mut h, &["one.example", "two.example", "three.example", "four.example", "five.example", "six.example"]);
        // Left in the middle of the ads lists, as natively at 640×420.
        h.reveal("ads-6.txt", 0);
        let away = h.settle().scroll_offset.y;
        assert!(away > 40.0, "the fixture scrolls at {size:?}: {away}");

        // Settings closes; later the shield's Content Blocking Settings… opens it.
        let b = &mut h.blocking;
        let profile = b.profile.clone();
        let shown = shield(&mut b.host, &b.lib.store, &profile, PAGE, 3).expect("the shield shows while blocking is on");
        assert!(apply_pick(&mut b.host, &b.lib.store, &profile, &shown, Pick::Settings, &b.ctx).settings);
        let shown = h.settle();
        let heading = shown.text.iter().any(|(text, bounds, clip)| text == "CONTENT BLOCKING" && clip.expand(0.5).contains_rect(*bounds));
        assert!(heading, "the heading is in view at {size:?}, scrolled {}", shown.scroll_offset.y);
        assert_eq!(shown.scroll_offset.y, 0.0, "the section from its top at {size:?}");
        assert!(shown.text.iter().any(|(text, bounds, clip)| text == BlockAds::TITLE && clip.contains_rect(*bounds)), "its first switch at {size:?}");

        // Once: the page scrolls on from there, and opening Settings another
        // way keeps where it was left.
        h.wheel(-120.0);
        let scrolled = h.settle().scroll_offset.y;
        assert!(scrolled > 0.0, "at {size:?}");
        for _ in 0..3 { h.settle(); }
        assert_eq!(h.settle().scroll_offset.y, scrolled, "not pulled back at {size:?}");
    }
}
