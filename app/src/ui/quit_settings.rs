//! Settings → General → Quitting: Warn Before Quitting, saved on this device only.

use super::*;
use crate::quit::QuitGuard;
use crossbow_core::store::Store;

const TITLE: &str = "Warn before quitting";

fn description() -> &'static str {
    if cfg!(target_os = "macos") {
        "Ask before ⌘Q, Quit in the menu or closing the last window quits Crossbow. Logging out and shutting down never wait. This device only."
    } else {
        "Ask before closing the last window quits Crossbow. This device only."
    }
}

pub(super) fn show(guard: &mut QuitGuard, store: &Store, ui: &mut Ui, pal: &Palette) {
    let ink = Ink::new(pal.panel, pal.accent);
    section(ui, pal, "Quitting");
    let mut enabled = guard.enabled();
    row(ui, pal, TITLE, description(), |ui| {
        let response = toggle(ui, &mut enabled, ink.accent);
        response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Switch, ui.is_enabled(), enabled, TITLE));
        if response.has_focus() {
            ui.painter().rect_stroke(response.rect.expand(2.0), 13.0, Stroke::new(2.0, ink.accent), egui::StrokeKind::Outside);
        }
        if response.gained_focus() { response.scroll_to_me_animation(None, egui::style::ScrollAnimation::none()); }
    });
    if enabled != guard.enabled() {
        guard.choose(store, enabled);
        ui.ctx().request_repaint();
    }
    for message in [&guard.notice, &guard.unavailable].into_iter().flatten() {
        ui.label(style::text(message, 12.0, style::weight::REGULAR).color(ink.danger));
    }
    let Some(failure) = guard.failure.as_mut() else { return };
    let mut retry = false;
    let mut cancel = false;
    let kept = if failure.wanted { "quits without asking" } else { "asks before quitting" };
    let response = ui.push_id("quit-warning-failure", |ui| {
        ui.label(style::text(format!("Could not save “{TITLE}”: {}. Crossbow still {kept}. Check disk space and data-folder permissions, then Retry Save or Cancel.", failure.error), 12.0, style::weight::REGULAR).color(ink.danger));
        ui.horizontal_wrapped(|ui| {
            retry = Button::new("Retry Save").kind(Kind::Primary).show(ui, &ink).clicked();
            cancel = Button::new("Cancel").kind(Kind::Plain).show(ui, &ink).clicked();
        });
    }).response;
    if failure.reveal && !ui.is_sizing_pass() && ui.is_visible() {
        response.scroll_to_me_animation(None, egui::style::ScrollAnimation::none());
        failure.reveal = false;
    }
    if cancel {
        guard.failure = None;
    } else if retry {
        let wanted = failure.wanted;
        guard.choose(store, wanted);
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::super::repair_tests::{PreferenceFrame, settings_test_frame};
    use super::*;
    use crate::shortcuts::tests::Fixture;

    const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];

    struct Harness {
        fixture: Fixture,
        store: Store,
        guard: QuitGuard,
        ctx: egui::Context,
        size: egui::Vec2,
        frame: u32,
    }

    impl Harness {
        fn new(size: egui::Vec2) -> Self {
            let fixture = Fixture::new();
            let store = fixture.store();
            let guard = QuitGuard::load(&store);
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            Harness { fixture, store, guard, ctx, size, frame: 0 }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> PreferenceFrame {
            settings_test_frame(&self.ctx, Page::General, self.size, 1.0, &mut self.frame, events, |ui, pal| {
                show(&mut self.guard, &self.store, ui, pal);
            })
        }

        /// Settle, then check every label fits: fully inside its clip (the short
        /// section needs no scrolling at either size), clear of the switch and
        /// of every other label.
        fn settle(&mut self) -> PreferenceFrame {
            for _ in 0..6 { self.frame(vec![]); }
            let shown = self.frame(vec![]);
            for (i, (text, bounds, clip)) in shown.text.iter().enumerate() {
                assert!(clip.expand(0.5).contains_rect(*bounds) && !text.ends_with('…'), "{text:?} is clipped or overflows at {:?}: {bounds:?} in {clip:?}", self.size);
                assert!(shown.switches.iter().all(|switch| !switch.intersects(*bounds)), "{text:?} runs into the switch at {:?}", self.size);
                for (other, other_bounds, _) in &shown.text[i + 1..] {
                    assert!(!bounds.shrink(0.5).intersects(other_bounds.shrink(0.5)), "{text:?} overlaps {other:?} at {:?}", self.size);
                }
            }
            shown
        }

        fn shows(&mut self, prefix: &str) -> Option<String> {
            self.settle().text.into_iter().map(|(text, ..)| text).find(|text| text.starts_with(prefix))
        }

        fn click_at(&mut self, pos: egui::Pos2) {
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }]);
            }
            self.settle();
        }

        fn click_switch(&mut self) {
            let shown = self.settle();
            let switch = *shown.switches.first().expect("the switch is shown");
            let (_, title, clip) = shown.text.iter().find(|(text, ..)| text == TITLE).expect("the title is shown");
            assert!(clip.contains_rect(switch) && clip.contains_rect(*title), "row fits at {:?}", self.size);
            assert!(switch.min.x > title.max.x, "the switch sits right of its title at {:?}", self.size);
            self.click_at(switch.center());
        }

        fn key(&mut self, key: egui::Key) {
            for pressed in [true, false] {
                self.frame(vec![egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }]);
            }
            self.settle();
        }

        fn saved(&self) -> bool {
            QuitGuard::load(&self.fixture.store()).enabled()
        }
    }

    #[test]
    fn the_warning_is_off_and_turns_on_or_off_by_pointer_or_keyboard_at_both_sizes() {
        for size in SIZES {
            let mut h = Harness::new(size);
            assert!(h.shows("QUITTING").is_some());
            assert_eq!(h.shows(TITLE).as_deref(), Some(TITLE));
            assert!(h.shows(description()).is_some(), "the description says which routes ask");
            assert!(!h.guard.enabled() && !h.saved(), "off by default");
            h.click_switch();
            assert!(h.guard.enabled() && h.saved(), "on and saved at {size:?}");
            h.click_switch();
            assert!(!h.guard.enabled() && !h.saved());

            h.key(egui::Key::Tab);
            let focused = h.ctx.memory(|memory| memory.focused()).and_then(|id| h.ctx.read_response(id)).map(|response| response.rect);
            assert_eq!(focused.map(|rect| rect.size()), Some(vec2(38.0, 22.0)), "Tab reaches the switch at {size:?}");
            h.key(egui::Key::Space);
            assert!(h.guard.enabled() && h.saved(), "Space turns it on");
            h.key(egui::Key::Space);
            assert!(!h.guard.enabled() && !h.saved(), "and off");
        }
    }

    #[test]
    fn a_failed_save_keeps_the_previous_choice_and_retry_or_cancel_recovers() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.store = h.fixture.read_only();
            h.click_switch();
            let error = h.shows("Could not save “Warn before quitting”:").expect("the failure is explained");
            assert!(error.ends_with("Crossbow still quits without asking. Check disk space and data-folder permissions, then Retry Save or Cancel."), "{error}");
            assert!(!h.guard.enabled() && !h.saved(), "the previous choice stays in use");
            let retry = h.settle().text.iter().find(|(text, ..)| text == "Retry Save").map(|(_, bounds, _)| bounds.center()).unwrap();
            h.click_at(retry);
            assert!(h.shows("Could not save").is_some(), "still failing");
            let cancel = h.settle().text.iter().find(|(text, ..)| text == "Cancel").map(|(_, bounds, _)| bounds.center()).unwrap();
            h.click_at(cancel);
            assert!(h.shows("Could not save").is_none() && !h.saved());

            h.click_switch();
            h.store = h.fixture.store();
            let retry = h.settle().text.iter().find(|(text, ..)| text == "Retry Save").map(|(_, bounds, _)| bounds.center()).unwrap();
            h.click_at(retry);
            assert!(h.shows("Could not save").is_none());
            assert!(h.guard.enabled() && h.saved(), "Retry saves it at {size:?}");
        }
    }

    #[test]
    fn an_unreadable_value_or_a_missing_menu_hook_is_explained() {
        for size in SIZES {
            let mut h = Harness::new(size);
            h.store.kv_set("warn_before_quitting", "sometimes").unwrap();
            h.guard = QuitGuard::load(&h.store);
            assert!(h.shows("The saved quit warning could not be read").is_some(), "at {size:?}");
            h.click_switch();
            assert!(h.shows("The saved quit warning could not be read").is_none() && h.saved());
            h.guard.unavailable = Some("⌘Q and Quit in the menu can't ask first: test".into());
            assert!(h.shows("⌘Q and Quit in the menu can't ask first").is_some(), "at {size:?}");
        }
    }
}
