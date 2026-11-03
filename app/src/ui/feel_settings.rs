//! Settings → Advanced → Window & Feedback: dragging the window from the top
//! of webpages, sound effects and haptic feedback when reordering. Each is a
//! switch kept on this device only.

use super::*;
use super::link_page::switch;
use crate::feel::FeelPreferences;
use crossbow_core::store::Store;

pub(super) fn show(prefs: &mut FeelPreferences, store: &Store, ui: &mut Ui, pal: &Palette) {
    let ink = Ink::new(pal.panel, pal.accent);
    section(ui, pal, "Window & Feedback");
    switch(&mut prefs.page_drag, store, ui, &ink, pal);
    switch(&mut prefs.sounds, store, ui, &ink, pal);
    switch(&mut prefs.haptics, store, ui, &ink, pal);
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::repair_tests::{PreferenceFrame, settings_test_frame};
    use crate::feel::{PageTopDrag, ReorderHaptics, UiSounds};
    use crate::link_prefs::SwitchSpec;
    use crate::shortcuts::tests::Fixture;
    use egui::{Event, Modifiers};

    const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
    const ROWS: [(&str, &str); 3] = [
        (PageTopDrag::TITLE, PageTopDrag::DESCRIPTION),
        (UiSounds::TITLE, UiSounds::DESCRIPTION),
        (ReorderHaptics::TITLE, ReorderHaptics::DESCRIPTION),
    ];

    /// Settings → Advanced's Window & Feedback section on a disposable store.
    struct Harness {
        fixture: Fixture,
        store: Store,
        prefs: FeelPreferences,
        ctx: egui::Context,
        size: egui::Vec2,
        frame: u32,
    }

    impl Harness {
        fn new(size: egui::Vec2) -> Self {
            Self::open(Fixture::new(), size)
        }

        fn open(fixture: Fixture, size: egui::Vec2) -> Self {
            let store = fixture.store();
            let prefs = FeelPreferences::load(&store);
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            Self { fixture, store, prefs, ctx, size, frame: 0 }
        }

        /// Quit and reopen on the same file.
        fn reopen(self) -> Self {
            Self::open(self.fixture, self.size)
        }

        fn reopened(&self) -> FeelPreferences {
            FeelPreferences::load(&self.fixture.store())
        }

        fn frame(&mut self, events: Vec<Event>) -> PreferenceFrame {
            settings_test_frame(&self.ctx, Page::Advanced, self.size, 1.0, &mut self.frame, events, |ui, pal| {
                show(&mut self.prefs, &self.store, ui, pal);
            })
        }

        /// Settle animations and check every visible text fits the sheet.
        fn settle(&mut self) -> PreferenceFrame {
            for _ in 0..6 { self.frame(vec![]); }
            let shown = self.frame(vec![]);
            for (text, bounds, clip) in &shown.text {
                if bounds.intersects(*clip) {
                    assert!(bounds.min.x >= clip.min.x - 0.5 && bounds.max.x <= clip.max.x + 0.5, "{text:?} overflows the sheet at {:?}: {bounds:?} in {clip:?}", self.size);
                }
            }
            shown
        }

        /// Scroll until `pick` is wholly in view; its centre.
        fn reveal(&mut self, what: &str, pick: impl Fn(&PreferenceFrame) -> Option<(Rect, Rect)>) -> egui::Pos2 {
            let sheet = sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), self.size), 1.0);
            let wheel = sheet.right_bottom() - vec2(60.0, 40.0);
            for attempt in 0..60 {
                let shown = self.settle();
                let delta = match pick(&shown) {
                    Some((bounds, clip)) if clip.expand(0.5).contains_rect(bounds) => return bounds.center(),
                    Some((bounds, clip)) => if bounds.center().y > clip.center().y { -100.0 } else { 100.0 },
                    None => if attempt < 12 { 200.0 } else { -200.0 },
                };
                self.frame(vec![
                    Event::PointerMoved(wheel),
                    Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, delta), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Modifiers::NONE },
                ]);
                for _ in 0..30 { self.frame(vec![]); }
            }
            panic!("{what} never became visible at {:?}", self.size);
        }

        fn visible(&mut self, label: &str) -> egui::Pos2 {
            self.reveal(label, |shown| shown.text.iter().find(|(text, ..)| text == label).map(|(_, bounds, clip)| (*bounds, *clip)))
        }

        /// The switch of the row titled `label`, in view with its title.
        fn switch_for(&mut self, label: &str) -> Rect {
            let beside = |shown: &PreferenceFrame| -> Option<(Rect, Rect)> {
                let (_, title, _) = shown.text.iter().find(|(text, ..)| text == label)?;
                shown.switches.iter().find(|switch| switch.min.x > title.max.x && switch.top() > title.top() - 4.0 && switch.top() < title.top() + 60.0).map(|switch| (*switch, *title))
            };
            self.reveal(label, |shown| {
                let (_, title, clip) = shown.text.iter().find(|(text, ..)| text == label)?;
                let row = beside(shown).map_or(title.union(title.translate(vec2(0.0, 50.0))), |(switch, title)| switch.union(title));
                Some((row, *clip))
            });
            beside(&self.settle()).expect("the switch is beside its title").0
        }

        fn shown_starting(&mut self, prefix: &str) -> Option<String> {
            self.settle().text.into_iter().map(|(text, ..)| text).find(|text| text.starts_with(prefix))
        }

        fn click_at(&mut self, pos: egui::Pos2) {
            for pressed in [true, false] {
                self.frame(vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
            }
            self.settle();
        }

        fn click(&mut self, label: &str) {
            let pos = self.visible(label);
            self.click_at(pos);
        }

        fn toggle(&mut self, title: &str) {
            let switch = self.switch_for(title).center();
            self.click_at(switch);
        }
    }

    fn enabled(prefs: &FeelPreferences, title: &str) -> bool {
        match title {
            PageTopDrag::TITLE => prefs.page_drag.enabled(),
            UiSounds::TITLE => prefs.sounds.enabled(),
            _ => prefs.haptics.enabled(),
        }
    }

    #[test]
    fn window_and_feedback_rows_fit_both_sizes_without_overlap_or_clipping() {
        for size in SIZES {
            let mut h = Harness::new(size);
            for (title, description) in ROWS {
                h.visible(description);
                h.visible(title);
                let shown = h.settle();
                let find = |label: &str| shown.text.iter().find(|(text, ..)| text == label).map(|(_, bounds, clip)| (*bounds, *clip)).unwrap();
                for label in [title, description] {
                    let (bounds, clip) = find(label);
                    assert!(clip.expand(0.5).contains_rect(bounds), "{label} is clipped at {size:?}");
                    for (other, rect, _) in &shown.text {
                        assert!(other == label || !rect.intersects(bounds) || rect.height() < 1.0, "{label} overlaps {other:?} at {size:?}");
                    }
                    assert!(shown.switches.iter().all(|switch| !switch.intersects(bounds)), "{label} overlaps a switch at {size:?}");
                }
                let switch = h.switch_for(title);
                assert!(switch.left() > find(title).0.right(), "the switch sits right of {title} at {size:?}");
            }
        }
    }

    #[test]
    fn each_choice_is_saved_read_back_and_kept_when_a_save_fails() {
        for size in SIZES {
            // What stays in use when going back to the default fails.
            for (title, kept) in [
                (PageTopDrag::TITLE, PageTopDrag::STILL_ON),
                (UiSounds::TITLE, UiSounds::STILL_ON),
                (ReorderHaptics::TITLE, ReorderHaptics::STILL_OFF),
            ] {
                let mut h = Harness::new(size);
                let default = title == ReorderHaptics::TITLE;
                assert!(enabled(&h.prefs, title) == default && enabled(&h.reopened(), title) == default, "{title}'s default");
                h.toggle(title);
                assert!(enabled(&h.prefs, title) != default && enabled(&h.reopened(), title) != default, "{title} changed and saved at {size:?}");
                let mut h = h.reopen();
                assert!(enabled(&h.prefs, title) != default, "{title} read back after reopening");

                // Back to the default, on a store that refuses writes.
                h.store = h.fixture.read_only();
                h.toggle(title);
                let error = h.shown_starting(&format!("Could not save “{title}”:")).expect("the failure is explained");
                assert!(error.ends_with(&format!("{kept}. Check disk space and data-folder permissions, then Retry Save or Cancel.")), "{error}");
                assert!(enabled(&h.prefs, title) != default, "the previous choice stays in use");
                h.click("Cancel");
                assert!(h.shown_starting("Could not save").is_none() && enabled(&h.prefs, title) != default);
                h.toggle(title);
                h.store = h.fixture.store();
                h.click("Retry Save");
                assert!(enabled(&h.prefs, title) == default && enabled(&h.reopened(), title) == default, "Retry saves {title} at {size:?}");
            }
        }
    }
}
