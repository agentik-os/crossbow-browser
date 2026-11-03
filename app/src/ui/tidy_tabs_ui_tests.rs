//! Simulated frames of the suggestion card and the review sheet at a compact
//! and a normal window: nothing overlaps, nothing is cut off, long text is
//! elided, and the buttons work by pointer and keyboard.

use super::*;
use crate::ai_features::tests::Script;
use crate::tidy_tabs::tests::World;
use egui::{Event, Modifiers, Pos2, Rect, pos2, vec2};

const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
/// The sidebar's narrowest, default and widest card widths.
const CARD_WIDTHS: [f32; 3] = [200.0 - 20.0, 252.0 - 20.0, 420.0 - 20.0];

struct Shown {
    text: Vec<(String, Rect, Rect, bool)>,
}

impl Shown {
    fn of(output: &egui::FullOutput) -> Self {
        let text = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()), shape.clip_rect, text.galley.elided)),
            _ => None,
        }).collect();
        Shown { text }
    }

    fn find(&self, label: &str) -> (Rect, Rect, bool) {
        self.text.iter().find(|(text, ..)| text == label).map(|(_, bounds, clip, elided)| (*bounds, *clip, *elided)).unwrap_or_else(|| panic!("{label:?} not drawn"))
    }

    /// Words, not icon glyphs, that are at least partly visible.
    fn visible(&self) -> impl Iterator<Item = &(String, Rect, Rect, bool)> {
        self.text.iter().filter(|(text, bounds, clip, _)| clip.intersects(*bounds) && !text.chars().all(|c| ('\u{e000}'..='\u{f8ff}').contains(&c)))
    }

    fn assert_fits(&self, within: Rect, context: &str) {
        let visible: Vec<_> = self.visible().collect();
        for (i, (text, bounds, clip, elided)) in visible.iter().enumerate() {
            let seen = bounds.intersect(*clip);
            assert!(within.expand(0.5).contains_rect(seen), "{text:?} leaves its surface {within:?}: {bounds:?} ({context})");
            assert!(bounds.left() >= clip.left() - 0.5 && bounds.right() <= clip.right() + 0.5, "{text:?} is cut off sideways: {bounds:?} in {clip:?} ({context})");
            assert!(!*elided || text.chars().count() > 20, "short text {text:?} was elided ({context})");
            for (other, other_bounds, other_clip, _) in &visible[i + 1..] {
                let theirs = other_bounds.intersect(*other_clip);
                assert!(!seen.shrink(0.5).intersects(theirs.shrink(0.5)), "{text:?} overlaps {other:?} ({context})");
            }
        }
    }
}

fn input(size: egui::Vec2, frame: u32, events: Vec<Event>) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), time: Some(f64::from(frame) / 60.0), events, ..Default::default() }
}

fn click(pos: Pos2) -> Vec<Vec<Event>> {
    [true, false].into_iter().map(|pressed| vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]).collect()
}

fn key(key: Key) -> Vec<Vec<Event>> {
    [true, false].into_iter().map(|pressed| vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }]).collect()
}

fn palette() -> Palette { crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false) }

/// Draws one card in a sidebar column; returns what it drew, the card's
/// area and what it answered.
fn card_frame(ctx: &egui::Context, size: egui::Vec2, width: f32, frame: u32, events: Vec<Event>, text: &str, buttons: &[(&str, CardAction)]) -> (Shown, Rect, Option<CardAction>) {
    let pal = palette();
    let mut answer = None;
    let mut area = Rect::NOTHING;
    let mut output = ctx.run_ui(input(size, frame, events), |ui| {
        let column = Rect::from_min_size(pos2(0.0, 40.0), vec2(width + 20.0, size.y - 40.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(column));
        child.set_clip_rect(column);
        let top = child.cursor().top();
        answer = card(&mut child, &pal, 10.0, width, text, false, buttons);
        area = Rect::from_min_max(pos2(10.0, top), pos2(10.0 + width, child.cursor().top()));
    });
    output.textures_delta.clear();
    (Shown::of(&output), area, answer)
}

#[test]
fn the_suggestion_card_fits_every_sidebar_width_elides_long_titles_and_its_buttons_work() {
    let buttons = [("Accept", CardAction::Accept), ("Edit", CardAction::Edit), ("Revert", CardAction::Revert)];
    let long = format!("Suggested: {}", "Quarterly Planning Notes ".repeat(4));
    for size in SIZES {
        for width in CARD_WIDTHS {
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let context = format!("{size:?}, card {width}px");
            for frame in 0..3 { card_frame(&ctx, size, width, frame, vec![], &long, &buttons); }
            let (shown, area, _) = card_frame(&ctx, size, width, 3, vec![], &long, &buttons);
            shown.assert_fits(area, &context);
            let (bounds, _, elided) = shown.find(&long);
            assert!(elided && bounds.width() <= width - 28.0, "the long title is elided inside the card ({context})");
            for (label, _) in buttons { assert!(!shown.find(label).2, "{label} is whole ({context})"); }
            let (short, ..) = card_frame(&ctx, size, width, 4, vec![], "Suggested: Docs", &buttons);
            assert!(!short.find("Suggested: Docs").2, "a short title is not elided ({context})");

            let accept = shown.find("Accept").0.center();
            let mut answers = Vec::new();
            for (frame, events) in (5..).zip(click(accept)) { answers.push(card_frame(&ctx, size, width, frame, events, &long, &buttons).2); }
            assert_eq!(answers.into_iter().flatten().collect::<Vec<_>>(), [CardAction::Accept], "a click answers once ({context})");
        }
    }

    // Keyboard: Tab reaches the buttons in order and Enter activates one.
    let ctx = egui::Context::default();
    crate::ui::window::install_fonts(&ctx);
    let mut answers = Vec::new();
    let mut frame = 0;
    for events in [key(Key::Tab), key(Key::Tab), key(Key::Enter)].concat() {
        answers.push(card_frame(&ctx, SIZES[0], CARD_WIDTHS[0], frame, events, "Suggested: Docs", &buttons).2);
        frame += 1;
    }
    assert_eq!(answers.into_iter().flatten().collect::<Vec<_>>(), [CardAction::Edit]);
}

fn sheet_frame(ctx: &egui::Context, size: egui::Vec2, frame: u32, events: Vec<Event>, world: &World, stale: bool) -> (Shown, Option<SheetAction>) {
    let pal = palette();
    let mut answer = None;
    let mut output = ctx.run_ui(input(size, frame, events), |ui| {
        answer = sheet(ui.ctx(), &pal, world.tidy.sheet.as_ref().unwrap(), stale);
    });
    output.textures_delta.clear();
    (Shown::of(&output), answer)
}

/// 40 Today tabs with long titles, asked about and (optionally) answered.
fn asked(answer: Option<&str>) -> World {
    let mut w = World::new();
    w.turn_on(crate::tidy_tabs::TODAY);
    w.fake.then(Script::Hold);
    let space = w.space.clone();
    for i in 0..40 { w.tab(&space, &format!("https://site{i}.test/"), &format!("Tab {i:02} — {}", "A very long page title that keeps going ".repeat(3))); }
    w.tidy_today();
    if let Some(answer) = answer {
        w.answer(Ok(answer));
        w.tick();
    }
    w
}

fn proposal() -> String {
    let folders: Vec<String> = (0..6).map(|f| {
        let tabs: Vec<String> = (0..6).map(|t| (f * 6 + t + 1).to_string()).collect();
        format!(r#"{{"name":"Folder {f} with a long name xx","tabs":[{}]}}"#, tabs.join(","))
    }).collect();
    format!(r#"{{"folders":[{}]}}"#, folders.join(","))
}

/// Settle the sheet and check it: inside the window, nothing overlapping or
/// cut off sideways, and the given buttons whole and on screen.
fn settled(world: &World, size: egui::Vec2, stale: bool, buttons: &[&str], context: &str) -> (egui::Context, Shown) {
    let ctx = egui::Context::default();
    crate::ui::window::install_fonts(&ctx);
    for frame in 0..4 { sheet_frame(&ctx, size, frame, vec![], world, stale); }
    let (shown, _) = sheet_frame(&ctx, size, 4, vec![], world, stale);
    let screen = Rect::from_min_size(Pos2::ZERO, size);
    shown.assert_fits(screen, context);
    for label in ["Tidy Today"].iter().chain(buttons) {
        let (bounds, clip, elided) = shown.find(label);
        assert!(clip.contains_rect(bounds) && screen.contains_rect(bounds) && !elided, "{label} is whole and visible ({context})");
    }
    (ctx, shown)
}

#[test]
fn the_review_sheet_fits_both_windows_in_every_state_and_elides_long_titles() {
    for size in SIZES {
        let review = asked(Some(&proposal()));
        let (ctx, shown) = settled(&review, size, false, &["Accept", "Cancel"], &format!("review {size:?}"));
        let status = shown.text.iter().find(|(text, ..)| text.starts_with("6 folders for 36 of 40 tabs.")).expect("the status says what changes");
        assert!(status.2.contains_rect(status.1), "the status is whole ({size:?})");
        let (note, clip, elided) = shown.find("Folders are added to Pinned; their tabs no longer archive automatically.");
        assert!(clip.contains_rect(note) && !elided && note.bottom() <= shown.find("Accept").0.top(), "the Pinned note is whole, before Accept ({size:?})");
        assert!(shown.text.iter().any(|(text, _, _, elided)| text.starts_with("Tab 00 — A very long") && *elided), "long titles are elided ({size:?})");
        assert!(shown.text.iter().any(|(text, ..)| text == "Folder 0 with a long name xx"), "folder names are shown");
        let small = asked(Some(r#"{"folders":[{"name":"Reading","tabs":[2,1]}]}"#));
        let (_, few) = settled(&small, size, false, &["Accept", "Cancel"], &format!("small review {size:?}"));
        assert!(few.text.iter().any(|(text, bounds, clip, _)| text == "38 tabs stay in Today." && clip.contains_rect(*bounds)), "the tabs left out are counted ({size:?})");
        assert!(!shown.text.iter().any(|(text, ..)| text == "Try Again"));

        // Accept by pointer and Cancel by keyboard.
        let accept = shown.find("Accept").0.center();
        let answers: Vec<_> = click(accept).into_iter().zip(5..).filter_map(|(events, frame)| sheet_frame(&ctx, size, frame, events, &review, false).1).collect();
        assert_eq!(answers, [SheetAction::Accept], "{size:?}");
        let mut escape = false;
        ctx.run_ui(input(size, 9, key(Key::Escape).remove(0)), |ui| escape = take_escape(ui.ctx())).textures_delta.clear();
        assert!(escape, "Escape answers Cancel");

        let (_, shown) = settled(&review, size, true, &["Try Again", "Cancel"], &format!("stale {size:?}"));
        assert!(shown.text.iter().any(|(text, ..)| text.starts_with("Some tabs changed since this proposal")));
        assert!(!shown.text.iter().any(|(text, ..)| text.starts_with("Folders are added to Pinned")), "only a proposal that can be accepted says it");
        assert!(!shown.text.iter().any(|(text, ..)| text == "Accept"), "a stale proposal can't be accepted");

        let asking = asked(None);
        let (_, shown) = settled(&asking, size, false, &["Cancel"], &format!("asking {size:?}"));
        assert!(shown.text.iter().any(|(text, ..)| text == "Asking Ollama (fixture-model) at http://127.0.0.1:11434 to sort 40 Today tabs into folders…"));
        assert!(!shown.text.iter().any(|(text, ..)| text == "Accept" || text == "Try Again"));

        let failed = asked(Some("Here are your folders!"));
        let (_, shown) = settled(&failed, size, false, &["Try Again", "Cancel"], &format!("failed {size:?}"));
        assert!(shown.text.iter().any(|(text, ..)| text == "The model's answer wasn't a valid list of folders. Nothing was changed."));
    }
}
