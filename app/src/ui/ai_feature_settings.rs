//! Settings → AI Providers → Browser features. Each opt-in feature has a
//! switch: turning one on first shows exactly what it sends and where, and
//! only Turn On saves. Turning one off saves at once. Below them, Reset
//! Translation Choices clears Translate Page's Never and Always choices
//! behind a confirmation.

use super::{row, section, toggle};
use crate::ai_features::{self, Ready};
use crate::ui::style::{self, Button, Ink, Kind, Size};
use crate::ui::theme::Palette;
use crossbow_core::library::Library;
use egui::{RichText, Ui};

struct Feature {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    sends: fn(&Ready) -> String,
}

const FEATURES: [Feature; 7] = [
    Feature { id: crate::ask_page::FEATURE, name: crate::ask_page::NAME, description: "Ask about the current page from the Find bar: ⌘F, then Tab.", sends: ask_sends },
    Feature { id: crate::tidy_downloads::FEATURE, name: crate::tidy_downloads::NAME, description: "Suggest a clearer name when a download finishes.", sends: tidy_sends },
    Feature { id: crate::tidy_tabs::TITLES, name: crate::tidy_tabs::TITLES_NAME, description: "Suggest a shorter title when you pin a tab, or from a pinned tab's menu.", sends: titles_sends },
    Feature { id: crate::tidy_tabs::TODAY, name: crate::tidy_tabs::TODAY_NAME, description: "Sort a Space's Today tabs into folders, from Today's menu or the command bar.", sends: today_sends },
    Feature { id: crate::ask_bar::FEATURE, name: crate::ask_bar::NAME, description: "Type a question in the command bar, then Tab and Return, to ask the saved AI in a new chat.", sends: ask_bar_sends },
    Feature { id: crate::translate::FEATURE, name: crate::translate::NAME, description: "Translate pages in other languages, from an offer, the command bar or a tab's menu.", sends: translate_sends },
    Feature { id: crate::link_preview::FEATURE, name: crate::link_preview::NAME, description: "Hold Shift over a link for half a second to see a short summary of the linked page.", sends: previews_sends },
];

// The consent text states the cap in words.
const _: () = assert!(crate::ask_page::PAGE_BYTES == 12_000);
const _: () = assert!(crate::translate::SAMPLE_BYTES == 600);
const _: () = assert!(crate::link_preview::TEXT_BYTES == 6_000 && crossbow_core::link_preview::MAX_REDIRECTS == 3);

pub(crate) const RESET_TITLE: &str = "Translation choices";
pub(crate) const RESET: &str = "Reset…";
const RESET_CONFIRM: &str = "Reset";

fn ask_sends(ready: &Ready) -> String {
    format!("When you press Return in Ask mode, Crossbow sends your question and up to 12,000 bytes of the page's text to {}. That can include text you can't see, such as text scrolled out of view, in a scrolling area or covered by something else, and text you're writing in a page's editor, like an email or a document. It never sends what you type into form fields, the page address, cookies or passwords, and never reads file: or internal pages. Answers are not saved.", ready.destination())
}

fn tidy_sends(ready: &Ready) -> String {
    format!("When a download finishes, Crossbow sends its file name, the site's host name and the file type to {}, never the file's contents. Nothing is renamed until you choose Accept.", ready.destination())
}

fn titles_sends(ready: &Ready) -> String {
    format!("When you pin a tab or choose Tidy Title, Crossbow sends that tab's title and the site's host name to {}, never the full address, page contents, cookies or form data. A title you set yourself is never replaced, and nothing changes until you choose Accept.", ready.destination())
}

fn today_sends(ready: &Ready) -> String {
    format!("When you choose Tidy Today, Crossbow sends the titles and host names of up to {} of the Space's Today tabs to {}, never their full addresses, page contents, cookies or form data. Pinned tabs and favorites are never sent or moved, and nothing moves until you choose Accept.", crate::tidy_tabs::MAX_TABS, ready.destination())
}

fn ask_bar_sends(ready: &Ready) -> String {
    format!("When you press Return in Ask mode in the command bar, Crossbow opens Ask AI and sends what you typed, and nothing else, to {} as the first message of a new chat. It never sends text that looks like a web address, a file path, a password or a key. The chat is not saved.", ready.destination())
}

fn translate_sends(ready: &Ready) -> String {
    format!("When a web page finishes loading without naming its language, Crossbow sends up to 600 bytes of the page's text to {} to tell which language it is. Only when you choose Translate, or Always for that language, does it send the rest, in parts. Either can include text you can't see at the moment: scrolled out of view, in a scrolling area or covered by something else. It never sends the page address, cookies, form fields, passwords or code, and never reads file: or internal pages. Translations are not saved.", ready.destination())
}

fn previews_sends(ready: &Ready) -> String {
    format!("When you hold Shift over a link for half a second, Crossbow loads the linked page in the background, so its site sees a visit from this Mac. The visit is made without your cookies, logins, cache or Referer, through this profile's network and proxy settings. It follows up to 3 redirects. It skips local network names and addresses, unless the page you're on and all its frames have that same scheme, host and port. A site that changes its address right after that check, or a proxy or DNS setting that looks names up differently, can still make it reach a device on your network. If looking up a site's address fails or takes over a second, there's no preview. Up to 6,000 bytes of the page's text, with its title, description and site name, go to {}, never the full address.", ready.destination())
}

/// Saved consent per feature, read when the page opens and after each save.
#[derive(Default)]
pub struct Draft {
    profile: Option<String>,
    saved: Vec<Option<Ready>>,
    /// The feature waiting for Turn On or Cancel, with the destination shown.
    confirming: Option<(usize, Ready)>,
    error: Option<String>,
    /// How many translation choices this profile has, and why they couldn't
    /// be read, if so.
    choices: usize,
    choices_error: Option<String>,
    /// Reset Translation Choices is asking to confirm.
    resetting: bool,
    /// What the last reset did, and whether it failed.
    reset_note: Option<(String, bool)>,
}

impl Draft {
    /// Reset Translation Choices is asking: Escape cancels it, not Settings.
    pub(super) fn owns_keyboard(&self) -> bool { self.resetting }

    fn load(lib: &Library, profile: &str) -> Self {
        let mut error = None;
        let saved = FEATURES.iter().map(|feature| ai_features::consented(lib, profile, feature.id).unwrap_or_else(|text| {
            error = Some(text);
            None
        })).collect();
        let (choices, choices_error) = match crate::translate::choices(lib, profile) {
            Ok(choices) => (choices.count(), None),
            Err(error) => (0, Some(error)),
        };
        Self { profile: Some(profile.into()), saved, confirming: None, error, choices, choices_error, resetting: false, reset_note: None }
    }
}

/// `ready` is the saved provider selection, or why there is none.
pub(super) fn show(draft: &mut Draft, lib: &Library, profile: &str, ready: Result<Ready, String>, ui: &mut Ui, pal: &Palette) {
    if draft.profile.as_deref() != Some(profile) { *draft = Draft::load(lib, profile); }
    let ink = Ink::new(pal.panel, pal.accent);
    section(ui, pal, "Browser features");
    ui.label(style::text("Off by default. Each sends data only after you turn it on and confirm, and only to the saved provider.", 12.0, style::weight::REGULAR).color(ink.secondary));
    for (index, feature) in FEATURES.iter().enumerate() {
        let consent = draft.saved[index].clone();
        let on = consent.is_some();
        ui.push_id(feature.id, |ui| row(ui, pal, feature.name, feature.description, |ui| {
            let mut shown = on;
            let switch = ui.add_enabled_ui(on || ready.is_ok(), |ui| toggle(ui, &mut shown, pal.accent)).inner;
            if !switch.changed() { return; }
            draft.error = None;
            if on {
                draft.confirming = None;
                match ai_features::disable(lib, profile, feature.id) {
                    Ok(()) => draft.saved[index] = None,
                    Err(error) => draft.error = Some(error),
                }
            } else if let Ok(ready) = &ready {
                draft.confirming = Some((index, ready.clone()));
            }
        }));
        if consent.is_some_and(|consent| ready.as_ref().ok() != Some(&consent)) {
            ui.label(style::text(format!("{} is paused: the saved provider changed since you turned it on. Turn it off and on again to confirm the new destination.", feature.name), 12.0, style::weight::REGULAR).color(ink.secondary));
        }
        if draft.confirming.as_ref().is_some_and(|(confirming, _)| *confirming == index) {
            confirm(draft, lib, profile, index, ui, pal, &ink);
        }
    }
    translation_choices(draft, lib, profile, ui, pal, &ink);
    if let Err(reason) = &ready {
        ui.label(style::text(format!("To turn these on: {reason}"), 12.0, style::weight::REGULAR).color(ink.secondary));
    }
    if let Some(error) = &draft.error {
        ui.label(style::text(error, 12.0, style::weight::REGULAR).color(ink.danger));
    }
}

/// Reset Translation Choices (ASP-A08): forgets every Never and Always this
/// profile chose for Translate Page, on this device, after a confirmation.
/// Escape and Cancel change nothing; a failed save keeps every choice.
fn translation_choices(draft: &mut Draft, lib: &Library, profile: &str, ui: &mut Ui, pal: &Palette, ink: &Ink) {
    if draft.resetting && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) { draft.resetting = false; }
    let description = match (&draft.choices_error, draft.choices) {
        (Some(_), _) => "Translate Page's Never and Always choices couldn't be read. Reset clears them.".to_string(),
        (None, 0) => "No Never or Always choices for Translate Page in this profile.".to_string(),
        (None, 1) => "1 Never or Always choice for Translate Page in this profile, on this Mac.".to_string(),
        (None, count) => format!("{count} Never and Always choices for Translate Page in this profile, on this Mac."),
    };
    let any = draft.choices > 0 || draft.choices_error.is_some();
    ui.push_id("translation-choices", |ui| row(ui, pal, RESET_TITLE, &description, |ui| {
        if Button::new(RESET).kind(Kind::Secondary).size(Size::Small).enabled(any && !draft.resetting).show(ui, ink).clicked() {
            draft.resetting = true;
            draft.reset_note = None;
        }
    }));
    if draft.resetting {
        egui::Frame::new().fill(pal.panel_hover).stroke(egui::Stroke::new(1.0, pal.panel_border)).corner_radius(8).inner_margin(12).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Reset translation choices?").size(13.5).strong().color(pal.panel_text));
            ui.label(style::text("Crossbow forgets every site and language you chose Never or Always for in this profile, and offers to translate them again.", 12.0, style::weight::REGULAR).color(pal.panel_text));
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if Button::new(RESET_CONFIRM).kind(Kind::Primary).show(ui, ink).clicked() {
                    draft.resetting = false;
                    draft.reset_note = Some(match crate::translate::reset_choices(lib, profile) {
                        Ok(()) => ("Translation choices were reset.".into(), false),
                        Err(error) => (error, true),
                    });
                    if let Ok(choices) = crate::translate::choices(lib, profile) {
                        draft.choices = choices.count();
                        draft.choices_error = None;
                    }
                }
                if Button::new("Cancel").kind(Kind::Plain).show(ui, ink).clicked() { draft.resetting = false; }
            });
        });
    }
    if let Some((note, failed)) = &draft.reset_note {
        ui.label(style::text(note, 12.0, style::weight::REGULAR).color(if *failed { ink.danger } else { ink.secondary }));
    }
}

fn confirm(draft: &mut Draft, lib: &Library, profile: &str, index: usize, ui: &mut Ui, pal: &Palette, ink: &Ink) {
    let Some((_, shown)) = draft.confirming.clone() else { return };
    let feature = &FEATURES[index];
    egui::Frame::new().fill(pal.panel_hover).stroke(egui::Stroke::new(1.0, pal.panel_border)).corner_radius(8).inner_margin(12).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(format!("Turn on {}?", feature.name)).size(13.5).strong().color(pal.panel_text));
        ui.label(style::text((feature.sends)(&shown), 12.0, style::weight::REGULAR).color(pal.panel_text));
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if Button::new("Turn On").kind(Kind::Primary).show(ui, ink).clicked() {
                match ai_features::enable(lib, profile, feature.id, &shown) {
                    Ok(()) => {
                        draft.saved[index] = Some(shown.clone());
                        draft.confirming = None;
                        draft.error = None;
                    }
                    Err(error) => {
                        draft.confirming = None;
                        draft.error = Some(error);
                    }
                }
            }
            if Button::new("Cancel").kind(Kind::Plain).show(ui, ink).clicked() {
                draft.confirming = None;
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::super::repair_tests::{PreferenceFrame, settings_test_frame};
    use super::super::{Page, sheet_rect};
    use super::*;
    use crate::shortcuts::tests::Fixture;
    use crossbow_ai::local::LocalEndpoint;
    use egui::{Rect, pos2, vec2};

    const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
    const ASK: &str = "Ask on Page";
    const TIDY: &str = "Tidy Downloads";

    struct Harness {
        fixture: Fixture,
        lib: Library,
        profile: String,
        draft: Draft,
        ctx: egui::Context,
        size: egui::Vec2,
        frame: u32,
    }

    impl Harness {
        fn new(size: egui::Vec2, provider: bool) -> Self {
            let fixture = Fixture::new();
            let (lib, _, profile) = crate::ai_features::tests::configured(fixture.store(), LocalEndpoint { port: 41_300, ..Default::default() });
            if !provider { lib.store.kv_delete(&format!("ai.providers.v1.{profile}")).unwrap(); }
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            Harness { fixture, lib, profile, draft: Draft::default(), ctx, size, frame: 0 }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> PreferenceFrame {
            let ready = crate::ai_features::ready(&self.lib, &self.profile);
            settings_test_frame(&self.ctx, Page::AiProviders, self.size, 1.0, &mut self.frame, events, |ui, pal| {
                show(&mut self.draft, &self.lib, &self.profile, ready.clone(), ui, pal);
            })
        }

        /// Settle, then check every fully shown label: inside the sheet's
        /// width, clear of the switches and of every other label.
        fn settle(&mut self) -> PreferenceFrame {
            for _ in 0..6 { self.frame(vec![]); }
            let shown = self.frame(vec![]);
            let visible: Vec<_> = shown.text.iter().filter(|(_, bounds, clip)| clip.expand(0.5).contains_rect(*bounds)).collect();
            for (text, bounds, clip) in &shown.text {
                // Reset… ends in an ellipsis because it asks first; any other one was cut.
                assert!(bounds.left() >= clip.left() - 0.5 && bounds.right() <= clip.right() + 0.5 && (!text.ends_with('…') || text == RESET), "{text:?} is clipped at {:?}", self.size);
            }
            for (i, (text, bounds, _)) in visible.iter().enumerate() {
                assert!(shown.switches.iter().all(|switch| !switch.intersects(*bounds)), "{text:?} runs into a switch at {:?}", self.size);
                for (other, other_bounds, _) in &visible[i + 1..] {
                    assert!(!bounds.shrink(0.5).intersects(other_bounds.shrink(0.5)), "{text:?} overlaps {other:?} at {:?}", self.size);
                }
            }
            shown
        }

        /// Scrolls until a label starting with `prefix` is fully inside the
        /// sheet, then returns that frame.
        fn find(&mut self, prefix: &str) -> Option<PreferenceFrame> {
            self.find_by(|text| text.starts_with(prefix))
        }

        fn find_by(&mut self, wanted: impl Fn(&str) -> bool) -> Option<PreferenceFrame> {
            let scroll = sheet_rect(Rect::from_min_size(pos2(0.0, 0.0), self.size), 1.0).right_bottom() - vec2(60.0, 40.0);
            for attempt in 0..24 {
                let shown = self.settle();
                let delta = match shown.text.iter().find(|(text, _, _)| wanted(text)) {
                    Some((_, bounds, clip)) if clip.expand(0.5).contains_rect(*bounds) => return Some(shown),
                    Some((_, bounds, clip)) => if bounds.center().y > clip.center().y { -100.0 } else { 100.0 },
                    None => if attempt < 12 { -100.0 } else { 100.0 },
                };
                self.frame(vec![
                    egui::Event::PointerMoved(scroll),
                    egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, delta), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() },
                ]);
            }
            None
        }

        fn visible(&mut self, prefix: &str) -> PreferenceFrame {
            self.find(prefix).unwrap_or_else(|| panic!("{prefix:?} never scrolled into view at {:?}", self.size))
        }

        fn shows(&mut self, prefix: &str) -> bool { self.find(prefix).is_some() }

        fn click_at(&mut self, pos: egui::Pos2) {
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }]);
            }
            self.settle();
        }

        /// Clicks the label that reads exactly `label`, once it is fully in view.
        fn click(&mut self, label: &str) {
            let shown = self.find_by(|text| text == label).unwrap_or_else(|| panic!("{label:?} never scrolled into view at {:?}", self.size));
            let (_, bounds, _) = shown.text.iter().find(|(text, _, _)| text == label).unwrap();
            self.click_at(bounds.center());
        }

        /// The switch on the row titled `title`.
        fn switch(&mut self, title: &str) -> Rect {
            let shown = self.visible(title);
            let (_, row, clip) = shown.text.iter().find(|(text, _, _)| text == title).unwrap();
            let switch = *shown.switches.iter().find(|switch| switch.left() > row.right() && (switch.center().y - row.bottom()).abs() < 24.0)
                .unwrap_or_else(|| panic!("no switch for {title} at {:?}", self.size));
            assert!(clip.contains_rect(switch), "the {title} switch is visible at {:?}", self.size);
            switch
        }

        fn toggle(&mut self, title: &str) {
            let switch = self.switch(title);
            self.click_at(switch.center());
        }

        fn saved(&self, feature: &str) -> bool {
            crate::ai_features::consented(&self.lib, &self.profile, feature).unwrap().is_some()
        }
    }

    #[test]
    fn features_start_off_and_turning_one_on_shows_what_is_sent_and_needs_confirmation() {
        for size in SIZES {
            let mut h = Harness::new(size, true);
            assert!(h.shows("BROWSER FEATURES"));
            assert!(h.shows(ASK) && h.shows(TIDY));
            assert!(!h.saved(crate::ask_page::FEATURE) && !h.saved(crate::tidy_downloads::FEATURE), "off by default");

            // Cancel leaves it off and saves nothing.
            h.toggle(ASK);
            assert!(h.shows("Turn on Ask on Page?"));
            let shown = h.visible("When you press Return in Ask mode");
            let consent = &shown.text.iter().find(|(text, ..)| text.starts_with("When you press Return")).unwrap().0;
            assert!(consent.contains("up to 12,000 bytes of the page's text to Ollama (fixture-model) at http://127.0.0.1:41300."), "{consent}");
            // Ask reads the page's text as the renderer lays it out, wherever it is, editors included (round 14).
            assert!(consent.contains("That can include text you can't see, such as text scrolled out of view, in a scrolling area or covered by something else, and text you're writing in a page's editor, like an email or a document.") && !consent.contains("visible"), "{consent}");
            assert!(consent.contains("It never sends what you type into form fields, the page address, cookies or passwords, and never reads file: or internal pages."), "{consent}");
            assert!(!h.saved(crate::ask_page::FEATURE), "showing the consent saves nothing");
            h.click("Cancel");
            assert!(!h.settle().text.iter().any(|(text, ..)| text.starts_with("Turn on")), "Cancel closes the confirmation at {size:?}");
            assert!(!h.saved(crate::ask_page::FEATURE));

            // Turn On saves; switching off saves at once.
            h.toggle(ASK);
            h.click("Turn On");
            assert!(h.saved(crate::ask_page::FEATURE) && !h.saved(crate::tidy_downloads::FEATURE), "only the confirmed feature at {size:?}");
            h.toggle(TIDY);
            assert!(h.shows("When a download finishes, Crossbow sends its file name, the site's host name and the file type"));
            h.click("Turn On");
            assert!(h.saved(crate::tidy_downloads::FEATURE));
            h.toggle(ASK);
            assert!(!h.saved(crate::ask_page::FEATURE) && h.saved(crate::tidy_downloads::FEATURE), "off saves at once at {size:?}");

            // Reopening the page reads the saved state.
            let mut reopened = Harness { lib: Library::open(h.fixture.store()).unwrap(), draft: Draft::default(), frame: 0, ctx: egui::Context::default(), ..h };
            crate::ui::window::install_fonts(&reopened.ctx);
            reopened.settle();
            assert_eq!(reopened.draft.saved, [None, Some(crate::ai_features::tests::fixture_ready(41_300)), None, None, None, None, None]);
        }
    }

    #[test]
    fn switches_are_disabled_without_a_provider_and_a_changed_provider_pauses_a_feature() {
        for size in SIZES {
            let mut h = Harness::new(size, false);
            assert!(h.shows("To turn these on: Choose a provider and model in AI Providers, then Save Selection."));
            h.toggle(ASK);
            assert!(!h.settle().text.iter().any(|(text, ..)| text.starts_with("Turn on")), "no provider, no confirmation at {size:?}");
            assert!(!h.saved(crate::ask_page::FEATURE));

            let mut h = Harness::new(size, true);
            h.toggle(TIDY);
            h.click("Turn On");
            let mut settings = crate::ai::SettingsDraft::load(&h.lib, &h.profile);
            settings.set_model(Some("other-model".into()));
            let profile = h.profile.clone();
            settings.save(&mut h.lib, Some(profile.as_str()));
            assert!(h.shows("Tidy Downloads is paused: the saved provider changed"));
            h.toggle(TIDY);
            assert!(!h.saved(crate::tidy_downloads::FEATURE), "a paused feature can still be turned off");
            h.toggle(TIDY);
            assert!(h.shows("When a download finishes") && h.settle().text.iter().any(|(text, ..)| text.contains("Ollama (other-model)")), "the new destination is shown");
        }
    }

    #[test]
    fn failed_saves_keep_the_saved_state_and_say_so_and_the_keyboard_reaches_the_switches() {
        for size in SIZES {
            let mut h = Harness::new(size, true);
            let conn = rusqlite::Connection::open(h.fixture.path()).unwrap();
            conn.execute_batch("CREATE TRIGGER reject_on BEFORE INSERT ON kv WHEN NEW.k LIKE 'ai.feature.%' BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
            h.toggle(ASK);
            h.click("Turn On");
            assert!(h.shows("Could not save this setting. It is still off."), "the failure is explained at {size:?}");
            assert!(!h.saved(crate::ask_page::FEATURE));
            conn.execute_batch("DROP TRIGGER reject_on; CREATE TRIGGER reject_off BEFORE DELETE ON kv WHEN OLD.k LIKE 'ai.feature.%' BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
            h.toggle(ASK);
            h.click("Turn On");
            assert!(h.saved(crate::ask_page::FEATURE));
            h.toggle(ASK);
            assert!(h.shows("Could not save this setting. It is still on."));
            assert!(h.saved(crate::ask_page::FEATURE));
            conn.execute_batch("DROP TRIGGER reject_off").unwrap();

            // Tab reaches the first switch; Space opens the confirmation.
            let mut h = Harness::new(size, true);
            let switch = h.switch(ASK);
            for _ in 0..12 {
                for pressed in [true, false] {
                    h.frame(vec![egui::Event::Key { key: egui::Key::Tab, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }]);
                }
                let focused = h.ctx.memory(|memory| memory.focused()).and_then(|id| h.ctx.read_response(id)).map(|response| response.rect);
                if focused == Some(switch) { break; }
            }
            let focused = h.ctx.memory(|memory| memory.focused()).and_then(|id| h.ctx.read_response(id)).map(|response| response.rect);
            assert_eq!(focused, Some(switch), "Tab reaches the switch at {size:?}");
            for pressed in [true, false] {
                h.frame(vec![egui::Event::Key { key: egui::Key::Space, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }]);
            }
            assert!(h.shows("Turn on Ask on Page?"), "Space asks for confirmation at {size:?}");
            assert!(!h.saved(crate::ask_page::FEATURE));
        }
    }

    #[test]
    fn tidy_tab_titles_and_tidy_today_share_the_consent_flow_and_say_what_they_send() {
        let titles = crate::tidy_tabs::TITLES_NAME;
        let today = crate::tidy_tabs::TODAY_NAME;
        for size in SIZES {
            let mut h = Harness::new(size, true);
            assert!(h.shows(titles) && h.shows(today), "both rows are in Browser features at {size:?}");
            assert!(!h.saved(crate::tidy_tabs::TITLES) && !h.saved(crate::tidy_tabs::TODAY), "off by default");

            h.toggle(titles);
            assert!(h.shows("Turn on Tidy Tab Titles?"));
            let shown = h.visible("When you pin a tab or choose Tidy Title");
            let consent = &shown.text.iter().find(|(text, ..)| text.starts_with("When you pin a tab")).unwrap().0;
            assert!(consent.contains("that tab's title and the site's host name to Ollama (fixture-model) at http://127.0.0.1:41300") && consent.contains("never the full address, page contents, cookies or form data"), "{consent}");
            h.click("Cancel");
            assert!(!h.saved(crate::tidy_tabs::TITLES), "Cancel leaves it off at {size:?}");

            h.toggle(today);
            let shown = h.visible("When you choose Tidy Today");
            let consent = &shown.text.iter().find(|(text, ..)| text.starts_with("When you choose Tidy Today")).unwrap().0;
            assert!(consent.contains("titles and host names of up to 40 of the Space's Today tabs to Ollama (fixture-model) at http://127.0.0.1:41300") && consent.contains("Pinned tabs and favorites are never sent or moved"), "{consent}");
            h.click("Turn On");
            assert!(h.saved(crate::tidy_tabs::TODAY) && !h.saved(crate::tidy_tabs::TITLES), "only the confirmed feature at {size:?}");
            let raw = h.lib.store.kv_get(&format!("ai.feature.v1.tidy_today.{}", h.profile)).unwrap().unwrap();
            assert_eq!(raw, format!(r#"{{"version":{},"endpoint":{{"provider":"ollama","port":41300}},"model":"fixture-model"}}"#, crate::ai_features::CONSENT_VERSION), "only the consent and its destination are stored");
            assert!(crate::tidy_tabs::allowed(&h.lib, &h.profile, crate::tidy_tabs::TODAY, today).is_ok(), "the feature reads the shared consent");

            let mut reopened = Harness { lib: Library::open(h.fixture.store()).unwrap(), draft: Draft::default(), frame: 0, ctx: egui::Context::default(), ..h };
            crate::ui::window::install_fonts(&reopened.ctx);
            reopened.settle();
            assert_eq!(reopened.draft.saved, [None, None, None, Some(crate::ai_features::tests::fixture_ready(41_300)), None, None, None], "read back after reopening at {size:?}");
            reopened.toggle(today);
            assert!(!reopened.saved(crate::tidy_tabs::TODAY), "turning off saves at once at {size:?}");
        }
    }

    #[test]
    fn ask_in_the_command_bar_shares_the_consent_flow_and_names_the_loopback_destination() {
        let name = crate::ask_bar::NAME;
        for size in SIZES {
            let mut h = Harness::new(size, true);
            assert!(h.shows(name), "the row is in Browser features at {size:?}");
            h.visible("Type a question in the command bar, then Tab and Return, to ask the saved AI in a new chat.");
            assert!(!h.saved(crate::ask_bar::FEATURE), "off by default");

            h.toggle(name);
            assert!(h.shows("Turn on Ask AI in the Command Bar?"));
            let shown = h.visible("When you press Return in Ask mode in the command bar");
            let consent = &shown.text.iter().find(|(text, ..)| text.starts_with("When you press Return in Ask mode in the command bar")).unwrap().0;
            assert!(consent.contains("sends what you typed, and nothing else, to Ollama (fixture-model) at http://127.0.0.1:41300") && consent.contains("never sends text that looks like a web address, a file path, a password or a key"), "{consent}");
            h.click("Cancel");
            assert!(!h.saved(crate::ask_bar::FEATURE), "Cancel leaves it off at {size:?}");
            assert_eq!(crate::ask_bar::offered(&h.lib, &h.profile), None, "the command bar offers nothing");

            h.toggle(name);
            h.click("Turn On");
            assert!(h.saved(crate::ask_bar::FEATURE));
            assert_eq!(crate::ask_bar::offered(&h.lib, &h.profile), Some("Ollama"));
            let mut reopened = Harness { lib: Library::open(h.fixture.store()).unwrap(), draft: Draft::default(), frame: 0, ctx: egui::Context::default(), ..h };
            crate::ui::window::install_fonts(&reopened.ctx);
            reopened.settle();
            assert_eq!(reopened.draft.saved, [None, None, None, None, Some(crate::ai_features::tests::fixture_ready(41_300)), None, None], "read back after reopening at {size:?}");
            reopened.visible("Type a question in the command bar, then Tab and Return, to ask the saved AI in a new chat.");
            reopened.toggle(name);
            assert!(!reopened.saved(crate::ask_bar::FEATURE), "turning off saves at once at {size:?}");
        }
    }

    #[test]
    fn translate_pages_shares_the_consent_flow_and_states_the_sample_limit() {
        let name = crate::translate::NAME;
        for size in SIZES {
            let mut h = Harness::new(size, true);
            assert!(h.shows(name), "the row is in Browser features at {size:?}");
            h.visible("Translate pages in other languages, from an offer, the command bar or a tab's menu.");
            assert!(!h.saved(crate::translate::FEATURE), "off by default");

            h.toggle(name);
            assert!(h.shows("Turn on Translate Pages?"));
            let shown = h.visible("When a web page finishes loading without naming its language");
            let consent = &shown.text.iter().find(|(text, ..)| text.starts_with("When a web page finishes loading")).unwrap().0;
            assert!(consent.contains("up to 600 bytes of the page's text to Ollama (fixture-model) at http://127.0.0.1:41300") && consent.contains("Only when you choose Translate, or Always for that language, does it send the rest, in parts."), "{consent}");
            // What is read is what the page shows, wherever it is on the page (round 14).
            assert!(consent.contains("Either can include text you can't see at the moment: scrolled out of view, in a scrolling area or covered by something else.") && !consent.contains("visible"), "{consent}");
            assert!(consent.contains("never sends the page address, cookies, form fields, passwords or code, and never reads file: or internal pages"), "{consent}");
            h.click("Cancel");
            assert!(!h.saved(crate::translate::FEATURE), "Cancel leaves it off at {size:?}");

            h.toggle(name);
            h.click("Turn On");
            assert!(h.saved(crate::translate::FEATURE) && crate::translate::enabled(&h.lib, &h.profile));
            let mut reopened = Harness { lib: Library::open(h.fixture.store()).unwrap(), draft: Draft::default(), frame: 0, ctx: egui::Context::default(), ..h };
            crate::ui::window::install_fonts(&reopened.ctx);
            reopened.settle();
            assert_eq!(reopened.draft.saved, [None, None, None, None, None, Some(crate::ai_features::tests::fixture_ready(41_300)), None], "read back after reopening at {size:?}");
            reopened.toggle(name);
            assert!(!reopened.saved(crate::translate::FEATURE), "turning off saves at once at {size:?}");

            // Turned on under the text before (consent version 1): off, and
            // turning it on shows the text and asks again.
            let mut h = Harness::new(size, true);
            let ready = crate::ai_features::ready(&h.lib, &h.profile).unwrap();
            h.lib.store.kv_set(&format!("ai.feature.v1.{}.{}", crate::translate::FEATURE, h.profile), &serde_json::json!({ "version": 1, "endpoint": ready.endpoint, "model": ready.model }).to_string()).unwrap();
            h.settle();
            assert_eq!(h.draft.saved[5], None, "a version 1 grant shows as off at {size:?}");
            h.toggle(name);
            assert!(h.shows("Turn on Translate Pages?") && h.shows("When a web page finishes loading without naming its language"), "asked again at {size:?}");
            h.click("Turn On");
            assert!(h.saved(crate::translate::FEATURE), "on again, under the current text, at {size:?}");
        }
    }


    #[test]
    fn five_second_previews_share_the_consent_flow_and_say_what_the_site_and_the_model_get() {
        let name = crate::link_preview::NAME;
        for size in SIZES {
            let mut h = Harness::new(size, true);
            assert!(h.shows(name), "the row is in Browser features at {size:?}");
            h.visible("Hold Shift over a link for half a second to see a short summary of the linked page.");
            assert!(!h.saved(crate::link_preview::FEATURE), "off by default");

            h.toggle(name);
            assert!(h.shows("Turn on 5-Second Previews?"));
            let shown = h.visible("When you hold Shift over a link");
            let consent = &shown.text.iter().find(|(text, ..)| text.starts_with("When you hold Shift over a link")).unwrap().0;
            // The visit the site sees, what it leaves out and uses, the local
            // check with its one exception and what it can't stop (AI-1,
            // second AI review), the lookup limit, and what the model gets.
            assert_eq!(consent, "When you hold Shift over a link for half a second, Crossbow loads the linked page in the background, so its site sees \
                a visit from this Mac. The visit is made without your cookies, logins, cache or Referer, through this profile's network and proxy \
                settings. It follows up to 3 redirects. It skips local network names \
                and addresses, unless the page you're on and all its frames have that same scheme, host and port. A site that changes its address \
                right after that check, or a proxy or DNS setting that looks names up differently, can still make it reach a device on your \
                network. If looking up a site's address \
                fails or takes over a second, there's no preview. Up to 6,000 bytes of the page's text, with its title, description and site name, \
                go to Ollama (fixture-model) at http://127.0.0.1:41300, never the full address.", "at {size:?}");
            assert!(!h.saved(crate::link_preview::FEATURE), "showing the consent saves nothing");
            h.click("Cancel");
            assert!(!h.saved(crate::link_preview::FEATURE), "Cancel leaves it off at {size:?}");

            h.toggle(name);
            h.click("Turn On");
            assert!(h.saved(crate::link_preview::FEATURE));
            assert!(crate::ai_features::allowed(&h.lib, &h.profile, crate::link_preview::FEATURE, name).is_ok(), "previews read the shared consent");
            let mut reopened = Harness { lib: Library::open(h.fixture.store()).unwrap(), draft: Draft::default(), frame: 0, ctx: egui::Context::default(), ..h };
            crate::ui::window::install_fonts(&reopened.ctx);
            reopened.settle();
            assert_eq!(reopened.draft.saved, [None, None, None, None, None, None, Some(crate::ai_features::tests::fixture_ready(41_300))], "read back after reopening at {size:?}");
            // The last row: bring its description into view so its switch is too.
            reopened.visible("Hold Shift over a link for half a second to see a short summary of the linked page.");
            reopened.toggle(name);
            assert!(!reopened.saved(crate::link_preview::FEATURE), "turning off saves at once at {size:?}");
        }
    }

    #[test]
    fn reset_translation_choices_asks_first_and_escape_cancel_and_a_failed_save_change_nothing() {
        const NONE: &str = "No Never or Always choices for Translate Page in this profile.";
        const ASK: &str = "Reset translation choices?";
        let three = r#"{"never_sites":["journal.example"],"never_languages":["fr"],"always_languages":["de"]}"#;
        for size in SIZES {
            let mut h = Harness::new(size, true);
            assert!(h.shows(RESET_TITLE) && h.shows(NONE), "at {size:?}");
            h.click(RESET);
            assert!(!h.shows(ASK), "nothing to reset: the button is off at {size:?}");

            let key = format!("translate.choices.v1.{}", h.profile);
            h.lib.store.kv_set(&key, three).unwrap();
            let mut h = Harness { draft: Draft::default(), ..h };
            let count = |h: &Harness| crate::translate::choices(&h.lib, &h.profile).unwrap().count();
            assert!(h.shows("3 Never and Always choices for Translate Page in this profile, on this Mac."), "read when the page opens at {size:?}");

            // Escape and Cancel close the question and change nothing.
            h.click(RESET);
            assert!(h.shows(ASK) && h.shows("Crossbow forgets every site and language you chose Never or Always for in this profile, and offers to translate them again."));
            assert!(h.draft.owns_keyboard(), "Escape belongs to the question, not to Settings");
            for pressed in [true, false] {
                h.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }]);
            }
            assert!(!h.shows(ASK) && !h.draft.owns_keyboard(), "Escape cancels at {size:?}");
            assert_eq!(count(&h), 3);
            h.click(RESET);
            h.click("Cancel");
            assert!(!h.shows(ASK), "Cancel closes it at {size:?}");
            assert_eq!(count(&h), 3);

            // A failed save keeps every choice and says so.
            let conn = rusqlite::Connection::open(h.fixture.path()).unwrap();
            conn.execute_batch("CREATE TRIGGER reject_reset BEFORE DELETE ON kv WHEN OLD.k LIKE 'translate.choices.%' BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
            h.click(RESET);
            h.click("Reset");
            assert!(h.shows("Could not reset translation choices. They are unchanged. Check disk space and permissions, then try again."), "at {size:?}");
            assert!(h.shows("3 Never and Always choices"), "the count stays at {size:?}");
            assert_eq!(count(&h), 3);
            conn.execute_batch("DROP TRIGGER reject_reset").unwrap();

            // Reset clears them, and reopening reads none.
            h.click(RESET);
            h.click("Reset");
            assert!(h.shows("Translation choices were reset.") && h.shows(NONE), "at {size:?}");
            assert_eq!(count(&h), 0);
            assert_eq!(h.lib.store.kv_get(&key).unwrap(), None, "the record is gone");
            let mut reopened = Harness { lib: Library::open(h.fixture.store()).unwrap(), draft: Draft::default(), frame: 0, ctx: egui::Context::default(), ..h };
            crate::ui::window::install_fonts(&reopened.ctx);
            assert!(reopened.shows(NONE), "read back after reopening at {size:?}");

            // Unreadable choices can still be reset.
            reopened.lib.store.kv_set(&key, "{\"never_sites\":7}").unwrap();
            let mut h = Harness { draft: Draft::default(), ..reopened };
            assert!(h.shows("Translate Page's Never and Always choices couldn't be read. Reset clears them."), "at {size:?}");
            h.click(RESET);
            h.click("Reset");
            assert_eq!(count(&h), 0);
        }
    }
}
