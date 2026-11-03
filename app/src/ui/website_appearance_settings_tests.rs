use super::super::Page;
use super::super::repair_tests::{PreferenceFrame, settings_test_frame};
use super::*;
use crate::shortcuts::tests::Fixture;
use crate::website_appearance::tests::Applied;
use crossbow_core::model::Theme;
use egui::vec2;

const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];

/// The row on a disposable library: Personal and Client are Spaces with
/// their own profiles.
struct Harness {
    fixture: Fixture,
    lib: Library,
    appearance: WebsiteAppearance,
    applied: Applied,
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
        let applied = Applied::default();
        let appearance = WebsiteAppearance::new(Box::new(applied.clone()));
        Harness { fixture, lib, appearance, applied, space: personal.clone(), personal, client, ctx, size, frame: 0 }
    }

    fn profile(&self, space: &str) -> String {
        self.lib.spaces[space].profile_id.clone()
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> PreferenceFrame {
        let space = self.space.clone();
        settings_test_frame(&self.ctx, Page::General, self.size, 1.0, &mut self.frame, events, |ui, pal| {
            show(&mut self.appearance, &self.lib, &space, ui, pal);
        })
    }

    /// Settled, with every label whole across and clear of the others.
    fn settle(&mut self) -> PreferenceFrame {
        for _ in 0..6 { self.frame(vec![]); }
        let shown = self.frame(vec![]);
        for (i, (text, bounds, clip)) in shown.text.iter().enumerate() {
            assert!(clip.min.x - 0.5 <= bounds.min.x && bounds.max.x <= clip.max.x + 0.5 && !text.ends_with('…'), "{text:?} is clipped at {:?}", self.size);
            for (other, other_bounds, _) in &shown.text[i + 1..] {
                assert!(!bounds.shrink(0.5).intersects(other_bounds.shrink(0.5)), "{text:?} overlaps {other:?} at {:?}", self.size);
            }
        }
        shown
    }

    fn click(&mut self, label: &str) {
        let shown = self.settle();
        let (_, bounds, _) = shown.text.iter().find(|(text, _, _)| text == label).unwrap_or_else(|| panic!("no {label:?}"));
        let pos = bounds.center();
        for pressed in [true, false] {
            self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }]);
        }
        self.settle();
    }

    fn saved(&self, space: &str) -> Appearance {
        crate::website_appearance::ProfileAppearance::load(&self.fixture.store(), &self.profile(space)).choice()
    }
}

#[test]
fn website_appearance_is_chosen_per_profile_applied_at_once_and_kept_after_reopening() {
    for size in SIZES {
        let mut h = Harness::new(size);
        let shown = h.settle();
        let bounds = |label: &str| shown.text.iter().find(|(text, ..)| text == label).map(|(_, bounds, _)| *bounds).unwrap_or_else(|| panic!("{label:?} at {size:?}"));
        let (title, _) = (bounds(TITLE), bounds(DESCRIPTION));
        let options = CHOICES.map(|choice| bounds(label(choice)));
        for pair in options.windows(2) {
            assert!(pair[0].max.x < pair[1].min.x && (pair[0].center().y - pair[1].center().y).abs() < 1.0, "three options in one row at {size:?}");
        }
        assert!(options[0].min.x > title.max.x, "beside the title at {size:?}");
        assert_eq!((h.saved(&h.personal.clone()), h.saved(&h.client.clone())), (Appearance::Automatic, Appearance::Automatic));

        h.click(label(Appearance::Dark));
        let personal = h.profile(&h.personal.clone());
        assert_eq!(h.saved(&h.personal.clone()), Appearance::Dark, "saved at {size:?}");
        assert_eq!(h.applied.take(), [(personal.clone(), Appearance::Dark)], "its pages are asked at once, and no other profile's");
        assert_eq!(h.saved(&h.client.clone()), Appearance::Automatic);

        // Another Space's profile has its own choice.
        h.space = h.client.clone();
        h.click(label(Appearance::Light));
        let client = h.profile(&h.client.clone());
        assert_eq!(h.applied.take(), [(client.clone(), Appearance::Light)]);
        h.click(label(Appearance::Light));
        assert!(h.applied.take().is_empty(), "choosing the same option again changes nothing");

        // Quit and reopen.
        h.lib = Library::open(h.fixture.store()).unwrap();
        h.appearance = WebsiteAppearance::new(Box::new(h.applied.clone()));
        h.appearance.launch(&h.lib);
        let mut expected = vec![(personal, Appearance::Dark), (client, Appearance::Light)];
        expected.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(h.applied.take(), expected, "read back after reopening at {size:?}");
    }
}

#[test]
fn a_failed_save_keeps_the_appearance_in_use_and_says_so_until_retry_or_cancel() {
    for size in SIZES {
        let mut h = Harness::new(size);
        h.lib = Library::open(h.fixture.read_only()).unwrap();
        h.click(label(Appearance::Dark));
        let shown = h.settle();
        let message = &shown.text.iter().find(|(text, ..)| text.starts_with(&format!("Could not save “{TITLE}”"))).unwrap_or_else(|| panic!("says why at {size:?}")).0;
        assert!(message.contains("Websites still follow the system"), "{message}");
        assert!(h.applied.take().is_empty(), "pages keep the saved choice");
        assert_eq!(h.saved(&h.personal.clone()), Appearance::Automatic, "nothing was written");

        h.click("Cancel");
        assert!(!h.settle().text.iter().any(|(text, ..)| text == "Retry Save"));
        h.click(label(Appearance::Dark));
        h.lib = Library::open(h.fixture.store()).unwrap();
        h.click("Retry Save");
        assert_eq!(h.saved(&h.personal.clone()), Appearance::Dark, "Retry Save writes the choice at {size:?}");
        assert_eq!(h.applied.take().len(), 1);
    }
}

impl Harness {
    fn key(&mut self, key: egui::Key) {
        for pressed in [true, false] {
            self.frame(vec![egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }]);
        }
        self.settle();
    }
}

#[test]
fn website_appearance_options_are_reached_with_tab_and_chosen_with_space_or_return() {
    for size in SIZES {
        let mut h = Harness::new(size);
        h.settle();
        // Automatic, the current option, is first; Tab moves to Light.
        h.key(egui::Key::Tab);
        h.key(egui::Key::Tab);
        h.key(egui::Key::Space);
        let personal = h.profile(&h.personal.clone());
        assert_eq!(h.saved(&h.personal.clone()), Appearance::Light, "Space chooses the focused option at {size:?}");
        h.key(egui::Key::Tab);
        h.key(egui::Key::Enter);
        assert_eq!(h.saved(&h.personal.clone()), Appearance::Dark, "Return chooses the focused option at {size:?}");
        assert_eq!(h.applied.take(), [(personal.clone(), Appearance::Light), (personal, Appearance::Dark)]);
    }
}
