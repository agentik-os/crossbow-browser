//! Settings → General → Picture in Picture: whether a video keeps playing in
//! a floating window on leaving its tab, chosen for each profile and kept on
//! this device only.

use super::section;
use crate::app::pip::PipPrefs;
use crate::ui::style::{self, Ink};
use crate::ui::theme::Palette;
use crossbow_core::library::Library;
use egui::Ui;

pub(super) fn show(prefs: &mut PipPrefs, lib: &Library, space: &str, ui: &mut Ui, pal: &Palette) {
    let Some(profile) = crate::downloads::active_profile(lib, space) else { return };
    let ink = Ink::new(pal.panel, pal.accent);
    section(ui, pal, "Picture in Picture");
    ui.label(style::text(format!("For {} · this device only", lib.profiles[profile].name), 12.0, style::weight::REGULAR).color(ink.secondary));
    super::link_page::switch(prefs.automatic_switch(&lib.store, profile), &lib.store, ui, &ink, pal);
}

#[cfg(test)]
mod tests {
    use super::super::Page;
    use super::super::repair_tests::{PreferenceFrame, settings_test_frame};
    use super::*;
    use crate::app::pip::AutomaticPip;
    use crate::link_prefs::SwitchSpec;
    use crate::shortcuts::tests::Fixture;
    use crossbow_core::model::Theme;
    use egui::vec2;

    const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
    const TITLE: &str = AutomaticPip::TITLE;

    /// The section on a disposable library: Personal and Client are Spaces
    /// with their own profiles.
    struct Harness {
        fixture: Fixture,
        lib: Library,
        prefs: PipPrefs,
        space: String,
        personal: String,
        client: String,
        ctx: egui::Context,
        size: egui::Vec2,
        frame: u32,
    }

    impl Harness {
        fn new(size: egui::Vec2) -> Self {
            let fixture = Fixture::new();
            let mut lib = Library::open(fixture.store()).unwrap();
            lib.ensure_defaults().unwrap();
            let personal = lib.spaces_ordered()[0].0.clone();
            let client_profile = lib.create_profile("Client").unwrap();
            let client = lib.create_space("Client", "C", Theme::preset(2), &client_profile).unwrap();
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            Harness { fixture, lib, prefs: PipPrefs::default(), space: personal.clone(), personal, client, ctx, size, frame: 0 }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> PreferenceFrame {
            let space = self.space.clone();
            settings_test_frame(&self.ctx, Page::General, self.size, 1.0, &mut self.frame, events, |ui, pal| {
                show(&mut self.prefs, &self.lib, &space, ui, pal);
            })
        }

        /// Settled, with every label whole and clear of the switch and the others.
        fn settle(&mut self) -> PreferenceFrame {
            for _ in 0..6 { self.frame(vec![]); }
            let shown = self.frame(vec![]);
            for (i, (text, bounds, clip)) in shown.text.iter().enumerate() {
                assert!(clip.expand(0.5).contains_rect(*bounds) && !text.ends_with('…'), "{text:?} is clipped at {:?}", self.size);
                assert!(shown.switches.iter().all(|switch| !switch.intersects(*bounds)), "{text:?} runs into the switch at {:?}", self.size);
                for (other, other_bounds, _) in &shown.text[i + 1..] {
                    assert!(!bounds.shrink(0.5).intersects(other_bounds.shrink(0.5)), "{text:?} overlaps {other:?} at {:?}", self.size);
                }
            }
            shown
        }

        fn click_at(&mut self, pos: egui::Pos2) {
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }]);
            }
            self.settle();
        }

        fn click(&mut self, label: &str) {
            let shown = self.settle();
            let (_, bounds, _) = shown.text.iter().find(|(text, _, _)| text == label).unwrap_or_else(|| panic!("no {label:?}"));
            self.click_at(bounds.center());
        }

        fn toggle(&mut self) {
            let shown = self.settle();
            let [switch] = shown.switches[..] else { panic!("one switch: {:?}", shown.switches) };
            self.click_at(switch.center());
        }

        fn saved(&self, space: &str) -> bool {
            let profile = self.lib.spaces[space].profile_id.clone();
            PipPrefs::default().automatic(&self.fixture.store(), &profile)
        }
    }

    #[test]
    fn automatic_picture_in_picture_starts_on_and_is_kept_per_profile_after_reopening() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let shown = h.settle();
            for label in ["PICTURE IN PICTURE", "For Personal · this device only", TITLE, AutomaticPip::DESCRIPTION] {
                assert!(shown.text.iter().any(|(text, ..)| text == label), "{label:?} at {size:?}");
            }
            assert!(h.saved(&h.personal.clone()) && h.saved(&h.client.clone()), "on by default, as in Arc");
            h.toggle();
            assert!(!h.saved(&h.personal.clone()));
            assert!(h.saved(&h.client.clone()), "another profile keeps its own choice");

            h.space = h.client.clone();
            assert!(h.settle().text.iter().any(|(text, ..)| text == "For Client · this device only"));
            h.toggle();
            assert!(!h.saved(&h.client.clone()));

            // Quit and reopen: a new library and new choices on the same file.
            h.lib = Library::open(h.fixture.store()).unwrap();
            h.prefs = PipPrefs::default();
            let personal = h.lib.spaces[&h.personal].profile_id.clone();
            assert!(!h.prefs.automatic(&h.lib.store, &personal), "read back after reopening at {size:?}");
            h.space = h.personal.clone();
            h.toggle();
            assert!(h.saved(&h.personal.clone()));
        }
    }

    #[test]
    fn a_failed_save_keeps_the_choice_and_says_so_until_retry_or_cancel() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let profile = h.lib.spaces[&h.personal].profile_id.clone();
            h.lib = Library::open(h.fixture.read_only()).unwrap();
            h.toggle();
            assert!(h.prefs.automatic(&h.lib.store, &profile), "the failed choice is not used at {size:?}");
            let shown = h.settle();
            let message = &shown.text.iter().find(|(text, ..)| text.starts_with("Could not save “Automatic Picture-in-Picture”")).expect("says why").0;
            assert!(message.contains(AutomaticPip::STILL_ON), "{message}");
            assert!(h.saved(&h.personal.clone()), "nothing was written");

            h.click("Cancel");
            assert!(h.prefs.automatic_switch(&h.lib.store, &profile).failure.is_none());
            h.toggle();
            h.lib = Library::open(h.fixture.store()).unwrap();
            h.click("Retry Save");
            assert!(!h.prefs.automatic(&h.lib.store, &profile));
            assert!(!h.saved(&h.personal.clone()), "Retry Save writes the choice at {size:?}");
        }
    }
}
