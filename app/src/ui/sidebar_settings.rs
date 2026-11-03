//! Settings → General → Appearance: Show unread badges and Show hover
//! previews, for the active Space's profile on this device. A failed save
//! keeps the previous choice in use and offers it again.

use crate::sidebar_prefs::SidebarPrefs;
use crate::ui::style::{self, Ink};
use crate::ui::theme::Palette;
use crossbow_core::library::Library;
use egui::Ui;

pub(super) fn show(prefs: &mut SidebarPrefs, lib: &Library, space: &str, ui: &mut Ui, pal: &Palette) {
    let Some(profile) = crate::downloads::active_profile(lib, space) else { return };
    let ink = Ink::new(pal.panel, pal.accent);
    // These rows end Appearance; unlike the rows above, each profile has its own.
    ui.label(style::text(format!("For {} · this device only", lib.profiles[profile].name), 12.0, style::weight::REGULAR).color(ink.secondary));
    let switches = prefs.get(&lib.store, profile);
    super::link_page::switch(&mut switches.unread, &lib.store, ui, &ink, pal);
    super::link_page::switch(&mut switches.previews, &lib.store, ui, &ink, pal);
}

#[cfg(test)]
mod tests {
    use super::super::repair_tests::{PreferenceFrame, settings_test_frame};
    use super::super::{Page, sheet_rect};
    use super::*;
    use crate::shortcuts::tests::Fixture;
    use crate::sidebar_prefs::{HoverPreviews, UnreadBadges};
    use crate::link_prefs::SwitchSpec;
    use crossbow_core::model::Theme;
    use egui::{Rect, pos2, vec2};

    const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
    const UNREAD: &str = UnreadBadges::TITLE;
    const PREVIEWS: &str = HoverPreviews::TITLE;

    /// The Sidebar rows on a disposable library: Personal and Client are
    /// Spaces with their own profiles.
    struct Harness {
        fixture: Fixture,
        lib: Library,
        prefs: SidebarPrefs,
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
            Harness { fixture, lib, prefs: SidebarPrefs::default(), space: personal.clone(), personal, client, ctx, size, frame: 0 }
        }

        fn profile(&self, space: &str) -> String {
            self.lib.spaces[space].profile_id.clone()
        }

        /// Quit and reopen: a new library and new switches on the same file.
        fn reopen(&mut self) {
            self.lib = Library::open(self.fixture.store()).unwrap();
            self.prefs = SidebarPrefs::default();
            self.ctx = egui::Context::default();
            crate::ui::window::install_fonts(&self.ctx);
        }

        fn read_only(&mut self) {
            self.lib = Library::open(self.fixture.read_only()).unwrap();
        }

        fn writable(&mut self) {
            self.lib = Library::open(self.fixture.store()).unwrap();
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> PreferenceFrame {
            let space = self.space.clone();
            settings_test_frame(&self.ctx, Page::General, self.size, 1.0, &mut self.frame, events, |ui, pal| {
                show(&mut self.prefs, &self.lib, &space, ui, pal);
            })
        }

        /// Settle, then check every label: inside the sheet's width, clear of
        /// the switches and of every other label.
        fn settle(&mut self) -> PreferenceFrame {
            for _ in 0..6 { self.frame(vec![]); }
            let shown = self.frame(vec![]);
            let visible: Vec<_> = shown.text.iter().filter(|(_, bounds, clip)| clip.expand(0.5).contains_rect(*bounds)).collect();
            for (text, bounds, clip) in &shown.text {
                assert!(bounds.left() >= clip.left() - 0.5 && bounds.right() <= clip.right() + 0.5 && !text.ends_with('…'), "{text:?} is clipped at {:?}", self.size);
            }
            for (i, (text, bounds, _)) in visible.iter().enumerate() {
                assert!(shown.switches.iter().all(|switch| !switch.intersects(*bounds)), "{text:?} runs into a switch at {:?}", self.size);
                for (other, other_bounds, _) in &visible[i + 1..] {
                    assert!(!bounds.shrink(0.5).intersects(other_bounds.shrink(0.5)), "{text:?} overlaps {other:?} at {:?}", self.size);
                }
            }
            shown
        }

        /// Scrolls until a label starting with `prefix` is fully in view.
        fn visible(&mut self, prefix: &str) -> PreferenceFrame {
            let scroll = sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), self.size), 1.0).right_bottom() - vec2(60.0, 40.0);
            for attempt in 0..24 {
                let shown = self.settle();
                let delta = match shown.text.iter().find(|(text, _, _)| text.starts_with(prefix)) {
                    Some((_, bounds, clip)) if clip.expand(0.5).contains_rect(*bounds) => return shown,
                    Some((_, bounds, clip)) => if bounds.center().y > clip.center().y { -100.0 } else { 100.0 },
                    None => if attempt < 12 { -100.0 } else { 100.0 },
                };
                self.frame(vec![
                    egui::Event::PointerMoved(scroll),
                    egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, delta), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() },
                ]);
            }
            panic!("{prefix:?} never scrolled into view at {:?}", self.size);
        }

        fn click_at(&mut self, pos: egui::Pos2) {
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }]);
            }
            self.settle();
        }

        fn click(&mut self, label: &str) {
            let shown = self.visible(label);
            let (_, bounds, _) = shown.text.iter().find(|(text, _, _)| text == label).unwrap();
            self.click_at(bounds.center());
        }

        /// Click the switch on the row titled `title`, once it is wholly in
        /// view: it sits a little below the title.
        fn toggle(&mut self, title: &str) {
            let scroll = sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), self.size), 1.0).right_bottom() - vec2(60.0, 40.0);
            self.visible(title);
            for _ in 0..12 {
                let shown = self.settle();
                let (_, row, clip) = shown.text.iter().find(|(text, _, _)| text == title).expect("the title stays drawn").clone();
                // The row's only switch: right of the title, centred on title and description.
                let beside = shown.switches.iter().find(|switch| switch.left() > row.right() && switch.top() > row.top() - 4.0 && switch.top() < row.top() + 60.0).copied();
                let wanted = beside.map_or(row.union(row.translate(vec2(0.0, 50.0))), |switch| switch.union(row));
                if let Some(switch) = beside.filter(|_| clip.contains_rect(wanted)) {
                    return self.click_at(switch.center());
                }
                let delta = if wanted.bottom() > clip.bottom() { -40.0 } else { 40.0 };
                self.frame(vec![
                    egui::Event::PointerMoved(scroll),
                    egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, delta), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() },
                ]);
            }
            panic!("no switch for {title} in view at {:?}", self.size);
        }

        /// What this profile's choices are after reopening the store.
        fn saved(&self, space: &str) -> (bool, bool) {
            let profile = self.profile(space);
            let mut prefs = SidebarPrefs::default();
            let store = self.fixture.store();
            (prefs.unread(&store, &profile), prefs.previews(&store, &profile))
        }
    }

    #[test]
    fn both_switches_start_on_and_are_kept_per_profile_after_reopening() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let shown = h.visible("For Personal");
            assert!(shown.text.iter().any(|(text, ..)| text == "For Personal · this device only"), "the rows name the profile at {size:?}");
            h.visible(UNREAD);
            h.visible(PREVIEWS);
            assert_eq!(h.saved(&h.personal.clone()), (true, true), "on by default, as in Arc");

            h.toggle(UNREAD);
            assert_eq!(h.saved(&h.personal.clone()), (false, true));
            h.toggle(PREVIEWS);
            assert_eq!(h.saved(&h.personal.clone()), (false, false));
            assert_eq!(h.saved(&h.client.clone()), (true, true), "another profile keeps its own choices");

            // The Client Space shows and changes Client's choices only.
            h.space = h.client.clone();
            assert!(h.visible("For Client").text.iter().any(|(text, ..)| text == "For Client · this device only"));
            h.toggle(PREVIEWS);
            assert_eq!((h.saved(&h.personal.clone()), h.saved(&h.client.clone())), ((false, false), (true, false)));

            h.reopen();
            let personal = h.profile(&h.personal.clone());
            assert_eq!((h.prefs.unread(&h.lib.store, &personal), h.prefs.previews(&h.lib.store, &personal)), (false, false), "read back after reopening at {size:?}");
            h.space = h.personal.clone();
            h.toggle(UNREAD);
            assert_eq!(h.saved(&h.personal.clone()), (true, false));
        }
    }

    #[test]
    fn a_failed_save_keeps_the_switch_and_says_so_until_retry_or_cancel() {
        for size in SIZES {
            let mut h = Harness::new(size);
            let profile = h.profile(&h.personal.clone());
            h.visible(UNREAD);
            h.read_only();
            h.toggle(UNREAD);
            assert!(h.prefs.unread(&h.lib.store, &profile), "the failed choice is not used at {size:?}");
            let shown = h.visible("Could not save “Show unread badges”");
            let message = &shown.text.iter().find(|(text, ..)| text.starts_with("Could not save “Show unread badges”")).unwrap().0;
            assert!(message.contains("Unread badges still show"), "{message}");
            assert_eq!(h.saved(&h.personal.clone()), (true, true), "nothing was written");

            h.click("Cancel");
            assert!(h.prefs.unread(&h.lib.store, &profile) && h.prefs.get(&h.lib.store, &profile).unread.failure.is_none());

            h.toggle(UNREAD);
            h.writable();
            h.click("Retry Save");
            assert!(!h.prefs.unread(&h.lib.store, &profile));
            assert_eq!(h.saved(&h.personal.clone()), (false, true), "Retry Save writes the choice at {size:?}");
        }
    }

    #[test]
    fn an_unreadable_choice_shows_and_says_why() {
        let mut h = Harness::new(SIZES[0]);
        let profile = h.profile(&h.personal.clone());
        h.lib.store.kv_set(&format!("{}.{profile}", UnreadBadges::KEY), "garbled").unwrap();
        h.reopen();
        assert!(h.prefs.unread(&h.lib.store, &profile));
        h.visible(UnreadBadges::UNREADABLE);
    }
}
