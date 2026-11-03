use super::super::repair_tests::{PreferenceFrame, settings_test_frame};
use super::*;
use crate::shortcuts::tests::Fixture;
use crate::link_prefs::{DeviceSwitch, SwitchSpec};
use crate::updates::AutoUpdate;
use crate::updates::tests::{Code, TEAM, fake_app};
use crossbow_core::update::archive::Bundle;
use crossbow_core::update::codesign::CodeError;
use crossbow_core::update::feed::{Archive, Release};
use crossbow_core::update::net::{Progress, Ready};
use crossbow_core::update::{MacVersion, Version};
use std::path::PathBuf;
use std::sync::Arc;

const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
const KEY: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";

fn release() -> Release {
    Release {
        channel: "stable".into(),
        version: "0.2.0".into(),
        build: 7,
        minimum_macos: "11.0".into(),
        published: "2026-11-01T11:23:42Z".into(),
        expires: "2026-11-15T11:23:42Z".into(),
        notes: "Faster tabs.\nFewer crashes.".into(),
        archive: Archive { url: "https://updates.example.com/a.tar.gz".into(), size: 84_200_000, sha256: "0".repeat(64), signature: String::new() },
    }
}

fn ready(app: PathBuf, install: Result<(), CodeError>) -> Ready {
    Ready { release: release(), app, bundle: Bundle { version: Version(0, 2, 0), build: 7, minimum_macos: MacVersion(11, 0, 0), executable: "crossbow".into() }, team: TEAM.into(), install }
}

struct Harness {
    tmp: tempfile::TempDir,
    fixture: Fixture,
    store: Store,
    updates: Updates,
    ctx: egui::Context,
    size: egui::Vec2,
    frame: u32,
    browser_downloads: usize,
    quit: bool,
    helpers: Arc<std::sync::Mutex<usize>>,
}

impl Harness {
    fn new(size: egui::Vec2, updater: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let fixture = Fixture::new();
        let store = fixture.store();
        let build = if updater { update::build(Some("https://updates.example.com/stable.json"), Some(KEY)) } else { None };
        let mut updates = Updates::load(&store, &tmp.path().join("data"), build, Arc::new(Code(Some(TEAM))), || {});
        let installed = tmp.path().join("Applications/Crossbow.app");
        fake_app(&installed, "0.1.0", 1);
        updates.app = Some(installed);
        let helpers = Arc::new(std::sync::Mutex::new(0));
        let counted = helpers.clone();
        updates.spawn_helper = Box::new(move |_| {
            *counted.lock().unwrap() += 1;
            Ok(())
        });
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        Harness { tmp, fixture, store, updates, ctx, size, frame: 0, browser_downloads: 0, quit: false, helpers }
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> PreferenceFrame {
        let (updates, store, downloads, quit) = (&mut self.updates, &self.store, self.browser_downloads, &mut self.quit);
        settings_test_frame(&self.ctx, Page::General, self.size, 1.0, &mut self.frame, events, |ui, pal| {
            if show(updates, store, downloads, ui, pal).is_some() {
                *quit = true;
            }
        })
    }

    /// Settle, then check every label: never wider than its column, never
    /// over another label or the switch; below the fold it scrolls.
    fn settle(&mut self) -> PreferenceFrame {
        for _ in 0..6 {
            self.frame(vec![]);
        }
        let shown = self.frame(vec![]);
        for (i, (text, bounds, clip)) in shown.text.iter().enumerate() {
            assert!(bounds.min.x >= clip.min.x - 0.5 && bounds.max.x <= clip.max.x + 0.5 && !text.ends_with('…') || text.ends_with("updates…"), "{text:?} overflows at {:?}: {bounds:?} in {clip:?}", self.size);
            assert!(shown.switches.iter().all(|switch| !switch.intersects(*bounds)), "{text:?} runs into the switch at {:?}", self.size);
            for (other, other_bounds, _) in &shown.text[i + 1..] {
                assert!(!bounds.shrink(0.5).intersects(other_bounds.shrink(0.5)), "{text:?} overlaps {other:?} at {:?}", self.size);
            }
        }
        shown
    }

    /// Every label, scrolling down until `label` is in view (if it is shown).
    fn texts(&mut self) -> Vec<String> {
        self.settle().text.into_iter().map(|(text, ..)| text).collect()
    }

    /// Where `label` is, scrolled fully into view.
    fn find(&mut self, label: &str) -> Option<egui::Pos2> {
        for _ in 0..12 {
            let shown = self.settle();
            let (_, bounds, clip) = shown.text.iter().find(|(text, ..)| text == label)?;
            if clip.expand(0.5).contains_rect(*bounds) {
                return Some(bounds.center());
            }
            let pos = clip.center();
            self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -120.0), modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown }]);
        }
        None
    }

    fn shows(&mut self, label: &str) -> bool {
        self.find(label).is_some()
    }

    fn click(&mut self, label: &str) {
        let pos = self.find(label).unwrap_or_else(|| panic!("{label:?} is shown at {:?}", self.size));
        for pressed in [true, false] {
            self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }]);
        }
        self.settle();
    }
}

#[test]
fn a_build_without_an_updater_says_so_and_offers_nothing_else_at_both_sizes() {
    for size in SIZES {
        let mut h = Harness::new(size, false);
        let texts = h.texts();
        assert!(texts.contains(&"UPDATES".to_string()), "{texts:?}");
        assert!(h.shows(&format!("Crossbow {}", update::VERSION)));
        assert!(h.shows("This build has no updater."), "at {size:?}");
        for absent in ["Check Now", AutoUpdate::TITLE, "Download", "Restart to Update"] {
            assert!(!h.texts().iter().any(|text| text == absent), "{absent} is not offered at {size:?}");
        }
        assert!(h.settle().switches.is_empty());
    }
}

#[test]
fn each_state_has_its_own_line_and_control_and_fits_at_both_sizes() {
    for size in SIZES {
        let downloading = Arc::new(Progress::default());
        let cases: Vec<(State, &str, Option<&str>)> = vec![
            (State::Idle, "Crossbow hasn't checked for updates yet.", None),
            (State::Checking, "Checking for updates…", None),
            (State::Checked(format!("Crossbow {} is up to date.", update::VERSION)), "is up to date.", None),
            (State::Failed("Crossbow couldn't reach the update server. Check your connection and try again.".into()), "Crossbow couldn't reach the update server. Check your connection and try again.", None),
            (State::Checked("The update's signature didn't match this build's keys, so nothing was downloaded or installed.".into()), "The update's signature didn't match this build's keys, so nothing was downloaded or installed.", None),
            (State::Checked("Crossbow 0.3.0 needs macOS 26.0 or later; this Mac has macOS 15.0.".into()), "Crossbow 0.3.0 needs macOS 26.0 or later; this Mac has macOS 15.0.", None),
            (State::Available(release()), "Crossbow 0.2.0 is available (84.2 MB).", Some("Download")),
            (State::Downloading { release: release(), progress: downloading }, "Downloading Crossbow 0.2.0… 0% of 84.2 MB.", Some("Cancel")),
            (State::Ready { ready: ready(PathBuf::from("/nowhere"), Ok(())), refusal: None }, "Crossbow 0.2.0 is downloaded and verified. It installs when you quit, or now with Restart to Update.", Some("Restart to Update")),
            (
                State::Ready { ready: ready(PathBuf::from("/nowhere"), Err(CodeError::AdHoc)), refusal: Some(CodeError::AdHoc.to_string()) },
                "Crossbow 0.2.0 is downloaded. Its update signature and hash match, and the app is validly signed by Developer ID team ABCDE12345, but this ad-hoc signed copy has no team of its own to match, so it won't install it. Show Download to install it yourself.",
                Some("Show Download"),
            ),
            (
                State::Ready { ready: ready(PathBuf::from("/nowhere"), Ok(())), refusal: Some("you can't write to the folder that holds Crossbow".into()) },
                "Crossbow 0.2.0 is downloaded and verified, but it can't install itself: you can't write to the folder that holds Crossbow. Show Download to install it yourself.",
                Some("Show Download"),
            ),
            (State::Waiting(ready(PathBuf::from("/nowhere"), Ok(()))), "Crossbow restarts to install 0.2.0 when the downloads in progress finish.", Some("Cancel")),
        ];
        for (state, line, control) in cases {
            let mut h = Harness::new(size, true);
            h.updates.state = state;
            let texts = h.texts();
            assert!(texts.iter().any(|text| text.ends_with(line)), "{line:?} at {size:?}: {texts:?}");
            assert!(h.shows("Check Now") && h.shows(AutoUpdate::TITLE), "at {size:?}");
            if let Some(control) = control {
                assert!(h.shows(control), "{control} for {line:?} at {size:?}");
            }
        }
    }
}

#[test]
fn check_now_starts_one_check_and_the_last_check_is_shown() {
    for size in SIZES {
        let mut h = Harness::new(size, true);
        h.updates.last = Some(crate::updates::LastCheck { at: 1_793_532_222, result: format!("Crossbow {} is up to date.", update::VERSION) });
        assert!(h.texts().iter().any(|text| text.starts_with("Last checked ")), "at {size:?}");
        h.click("Check Now");
        assert!(matches!(h.updates.state, State::Checking), "at {size:?}");
        assert!(h.shows("Checking for updates…"));
    }
}

#[test]
fn release_notes_open_and_close_for_an_available_update() {
    for size in SIZES {
        let mut h = Harness::new(size, true);
        h.updates.state = State::Available(release());
        assert!(!h.texts().iter().any(|text| text.contains("Fewer crashes.")));
        h.click("Release Notes");
        assert!(h.shows("Faster tabs.\nFewer crashes."), "the notes, as plain text, at {size:?}");
        h.click("Hide Release Notes");
        assert!(!h.texts().iter().any(|text| text.contains("Fewer crashes.")));
    }
}

#[test]
fn the_switch_is_saved_on_this_device_and_read_back_after_reopening() {
    for size in SIZES {
        let mut h = Harness::new(size, true);
        assert!(h.updates.auto.enabled());
        let pos = h.settle().switches.first().expect("the switch").center();
        for pressed in [true, false] {
            h.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }]);
        }
        h.settle();
        assert!(!h.updates.auto.enabled());
        assert!(!DeviceSwitch::<AutoUpdate>::load(&h.fixture.store()).enabled(), "saved off at {size:?}");
        let reopened = Updates::load(&h.fixture.store(), &h.tmp.path().join("data"), None, Arc::new(Code(Some(TEAM))), || {});
        assert!(!reopened.auto.enabled(), "still off after reopening");
    }
}

#[test]
fn restart_to_update_asks_while_browser_downloads_run_then_waits_or_restarts() {
    for size in SIZES {
        let mut h = Harness::new(size, true);
        let new_app = h.tmp.path().join("download/Crossbow.app");
        fake_app(&new_app, "0.2.0", 7);
        h.updates.state = State::Ready { ready: ready(new_app, Ok(())), refusal: None };
        h.browser_downloads = 1;
        h.click("Restart to Update");
        assert!(h.shows("1 download is in progress. Restarting now cancels them."), "at {size:?}");
        assert!(!h.quit && *h.helpers.lock().unwrap() == 0, "nothing happens before an answer");
        h.click("Wait for Downloads");
        assert!(h.shows("Crossbow restarts to install 0.2.0 when the downloads in progress finish."));
        h.click("Cancel");
        assert!(h.shows("Restart to Update"));
        h.click("Restart to Update");
        h.click("Restart Now");
        assert!(h.quit, "Crossbow quits for the helper at {size:?}: {}", h.updates.status());
        assert_eq!(*h.helpers.lock().unwrap(), 1);
    }
}
