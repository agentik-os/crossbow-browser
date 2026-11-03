use super::super::Page;
use super::super::repair_tests::{PreferenceFrame, settings_test_frame};
use super::*;
use crate::link_prefs::SwitchSpec;
use crate::quote::CopyAsQuote;
use crate::shortcuts::tests::Fixture;
use egui::vec2;

const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];

/// The section on a disposable store.
struct Harness {
    fixture: Fixture,
    store: Store,
    quote: QuoteHost,
    ctx: egui::Context,
    size: egui::Vec2,
    frame: u32,
}

impl Harness {
    fn new(size: egui::Vec2) -> Self {
        let fixture = Fixture::new();
        let store = fixture.store();
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        Harness { fixture, store, quote: QuoteHost::default(), ctx, size, frame: 0 }
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> PreferenceFrame {
        settings_test_frame(&self.ctx, Page::General, self.size, 1.0, &mut self.frame, events, |ui, pal| {
            show(&mut self.quote, &self.store, ui, pal);
        })
    }

    /// Settled, with every label whole across and clear of the switch and
    /// the others.
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

    fn toggle(&mut self) {
        let shown = self.settle();
        assert_eq!(shown.switches.len(), 1, "one switch at {:?}", self.size);
        self.click_at(shown.switches[0].center());
    }

    /// Read again, as after quitting and reopening.
    fn reopened(&self) -> bool {
        QuoteHost::default().enabled(&self.fixture.store())
    }
}

#[test]
fn copy_as_quote_starts_off_and_its_switch_is_read_back_after_reopening() {
    for size in SIZES {
        let mut h = Harness::new(size);
        let shown = h.settle();
        for label in ["SELECTED TEXT", CopyAsQuote::TITLE, CopyAsQuote::DESCRIPTION] {
            assert!(shown.text.iter().any(|(text, ..)| text == label), "{label:?} at {size:?}");
        }
        assert!(!h.quote.enabled(&h.store) && !h.reopened(), "off by default");

        h.toggle();
        assert!(h.quote.enabled(&h.store));
        h.quote = QuoteHost::default();
        h.store = h.fixture.store();
        assert!(h.quote.enabled(&h.store), "on after reopening at {size:?}");
        h.settle();

        h.toggle();
        assert!(!h.quote.enabled(&h.store) && !h.reopened(), "off again after reopening at {size:?}");
    }
}

#[test]
fn a_failed_save_keeps_copy_as_quote_off_and_says_so_until_retry_or_cancel() {
    for size in SIZES {
        let mut h = Harness::new(size);
        h.store = h.fixture.read_only();
        h.toggle();
        assert!(!h.quote.enabled(&h.store), "the failed choice is not used at {size:?}");
        let shown = h.settle();
        let message = &shown.text.iter().find(|(text, ..)| text.starts_with("Could not save “Offer Copy as Quote for selected text”")).expect("says why").0;
        assert!(message.contains(CopyAsQuote::STILL_OFF), "{message}");
        assert!(!h.reopened(), "nothing was written");

        h.click("Cancel");
        assert!(h.quote.switch(&h.store).failure.is_none());
        h.toggle();
        h.store = h.fixture.store();
        h.click("Retry Save");
        assert!(h.quote.enabled(&h.store) && h.reopened(), "Retry Save writes the choice at {size:?}");
    }
}

#[test]
fn an_unreadable_saved_choice_reads_as_off_and_says_so() {
    for size in SIZES {
        let mut h = Harness::new(size);
        h.store.kv_set(CopyAsQuote::KEY, "maybe").unwrap();
        let shown = h.settle();
        assert!(shown.text.iter().any(|(text, ..)| text == CopyAsQuote::UNREADABLE), "at {size:?}");
        assert!(!h.quote.enabled(&h.store));
        h.toggle();
        assert!(h.reopened());
        let shown = h.settle();
        assert!(!shown.text.iter().any(|(text, ..)| text == CopyAsQuote::UNREADABLE), "the next save clears it");
    }
}

#[test]
fn copy_as_quote_is_in_general_after_capture() {
    let settings = include_str!("settings.rs");
    let capture = settings.find("    capture_page::show(&mut app.capture.prefs, &app.lib, &app.active_space, ui, pal);\n").unwrap();
    let quote = settings.find("    quote_page::show(&mut app.quote, &app.lib.store, ui, pal);\n").unwrap();
    let quit = settings.find("    quit_page::show(&mut app.quit.borrow_mut(), &app.lib.store, ui, pal);\n").unwrap();
    let general = settings.find("fn general(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette) {").unwrap();
    assert!(general < capture && capture < quote && quote < quit);
}
