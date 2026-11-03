use super::super::Page;
use super::super::repair_tests::{PreferenceFrame, settings_test_frame};
use super::*;
use crate::app::capture_host::{CaptureDrag, SaveCaptures};
use crate::link_prefs::SwitchSpec;
use crate::shortcuts::tests::Fixture;
use crossbow_core::model::Theme;
use egui::vec2;

const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];

/// The section on a disposable library: Personal and Client are Spaces with
/// their own profiles.
struct Harness {
    fixture: Fixture,
    lib: Library,
    prefs: CapturePrefs,
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
        Harness { fixture, lib, prefs: CapturePrefs::default(), space: personal.clone(), personal, client, ctx, size, frame: 0 }
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> PreferenceFrame {
        let space = self.space.clone();
        settings_test_frame(&self.ctx, Page::General, self.size, 1.0, &mut self.frame, events, |ui, pal| {
            show(&mut self.prefs, &self.lib, &space, ui, pal);
        })
    }

    /// Settled, with every label whole across and clear of the switches and
    /// the others. The page scrolls, so a label may be cut at the top or bottom.
    fn settle(&mut self) -> PreferenceFrame {
        for _ in 0..6 { self.frame(vec![]); }
        let shown = self.frame(vec![]);
        for (i, (text, bounds, clip)) in shown.text.iter().enumerate() {
            assert!(clip.min.x - 0.5 <= bounds.min.x && bounds.max.x <= clip.max.x + 0.5 && !text.ends_with('…'), "{text:?} is clipped at {:?}", self.size);
            assert!(shown.switches.iter().all(|switch| !switch.intersects(*bounds)), "{text:?} runs into a switch at {:?}", self.size);
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

    /// Flip the switch beside `title`.
    fn toggle(&mut self, title: &str) {
        let shown = self.settle();
        let (_, label, _) = shown.text.iter().find(|(text, ..)| text == title).unwrap_or_else(|| panic!("no {title:?}"));
        let switch = *shown.switches.iter().find(|switch| switch.center().y >= label.top() - 2.0 && switch.center().y <= label.bottom() + 24.0).unwrap_or_else(|| panic!("no switch beside {title:?}: {:?}", shown.switches));
        self.click_at(switch.center());
    }

    fn saved(&self, space: &str) -> bool {
        let profile = self.lib.spaces[space].profile_id.clone();
        CapturePrefs::default().save(&self.fixture.store(), &profile)
    }

    fn drags(&self) -> bool {
        CapturePrefs::default().drag(&self.fixture.store())
    }
}

#[test]
fn both_capture_switches_start_on_and_keep_their_scope_after_reopening() {
    for size in SIZES {
        let mut h = Harness::new(size);
        let shown = h.settle();
        for label in ["CAPTURE", "For Personal · this device only", SaveCaptures::TITLE, SaveCaptures::DESCRIPTION, CaptureDrag::TITLE, CaptureDrag::DESCRIPTION] {
            assert!(shown.text.iter().any(|(text, ..)| text == label), "{label:?} at {size:?}");
        }
        assert_eq!(shown.switches.len(), 2);
        assert!(h.saved(&h.personal.clone()) && h.saved(&h.client.clone()) && h.drags(), "both on by default");

        h.toggle(SaveCaptures::TITLE);
        assert!(!h.saved(&h.personal.clone()));
        assert!(h.saved(&h.client.clone()), "another profile keeps its own choice");
        assert!(h.drags(), "the other switch is untouched");

        h.toggle(CaptureDrag::TITLE);
        assert!(!h.drags());
        h.space = h.client.clone();
        let shown = h.settle();
        assert!(shown.text.iter().any(|(text, ..)| text == "For Client · this device only"));
        assert!(!h.prefs.drag(&h.lib.store), "Command-Shift drag is one choice for every profile");

        // Quit and reopen: a new library and new choices on the same file.
        h.lib = Library::open(h.fixture.store()).unwrap();
        h.prefs = CapturePrefs::default();
        let (personal, client) = (h.lib.spaces[&h.personal].profile_id.clone(), h.lib.spaces[&h.client].profile_id.clone());
        assert!(!h.prefs.save(&h.lib.store, &personal) && h.prefs.save(&h.lib.store, &client), "read back after reopening at {size:?}");
        assert!(!h.prefs.drag(&h.lib.store));
    }
}

#[test]
fn a_failed_save_keeps_the_choice_and_says_so_until_retry_or_cancel() {
    for size in SIZES {
        let mut h = Harness::new(size);
        let profile = h.lib.spaces[&h.personal].profile_id.clone();
        h.lib = Library::open(h.fixture.read_only()).unwrap();
        h.toggle(SaveCaptures::TITLE);
        assert!(h.prefs.save(&h.lib.store, &profile), "the failed choice is not used at {size:?}");
        let shown = h.settle();
        let message = &shown.text.iter().find(|(text, ..)| text.starts_with("Could not save “Save captures to Library”")).expect("says why").0;
        assert!(message.contains(SaveCaptures::STILL_ON), "{message}");
        assert!(h.saved(&h.personal.clone()), "nothing was written");

        h.click("Cancel");
        assert!(h.prefs.save_switch(&h.lib.store, &profile).failure.is_none());
        h.toggle(SaveCaptures::TITLE);
        h.lib = Library::open(h.fixture.store()).unwrap();
        h.click("Retry Save");
        assert!(!h.prefs.save(&h.lib.store, &profile));
        assert!(!h.saved(&h.personal.clone()), "Retry Save writes the choice at {size:?}");
    }
}
