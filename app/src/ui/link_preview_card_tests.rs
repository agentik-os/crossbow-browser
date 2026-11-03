//! The card drawn in each state at 640×420 and 1360×860: placed by the link
//! inside its page, every line whole, and never focused.

use super::*;
use crate::link_preview::{BUSY, READING};
use egui::{Event, Modifiers, PointerButton};

const SIZES: [Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];

/// The page beside a 240-point sidebar.
fn area(size: Vec2) -> Rect {
    Rect::from_min_max(pos2(248.0, 44.0), pos2(size.x - 8.0, size.y - 8.0))
}

struct Drawn {
    card: Rect,
    text: Vec<(String, Rect, Rect)>,
}

fn draw(ctx: &egui::Context, size: Vec2, frame: &mut u32, shown: &Shown, events: Vec<Event>) -> Drawn {
    let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
    *frame += 1;
    let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), time: Some(f64::from(*frame) / 60.0), events, ..Default::default() };
    let mut card_rect = Rect::NOTHING;
    let mut output = ctx.run_ui(input, |ui| card_rect = card(ui.ctx(), shown, &pal));
    output.textures_delta.clear();
    let text = output.shapes.iter().filter_map(|shape| match &shape.shape {
        egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()), shape.clip_rect)),
        _ => None,
    }).collect();
    Drawn { card: card_rect, text }
}

fn settle(ctx: &egui::Context, size: Vec2, frame: &mut u32, shown: &Shown) -> Drawn {
    for _ in 0..4 { draw(ctx, size, frame, shown, Vec::new()); }
    draw(ctx, size, frame, shown, Vec::new())
}

fn shown(card: Card, anchor: Pos2, size: Vec2) -> Shown {
    Shown { card, host: "example.test".into(), anchor, area: area(size), generation: 1 }
}

#[test]
fn every_state_fits_by_the_link_inside_the_page() {
    let summary = format!("{}.", "A fixture summary that runs long enough to wrap ".repeat(8).trim_end().chars().take(399).collect::<String>());
    for size in SIZES {
        for (state, card, lines) in [
            ("reading", Card::Working(READING.into()), vec![READING.to_owned()]),
            ("summarizing", Card::Working("Summarizing with Ollama…".into()), vec!["Summarizing with Ollama…".to_owned()]),
            ("summary", Card::Summary { text: summary.clone(), provider: "Ollama".into() }, vec![summary.clone(), "Summary by Ollama · may be wrong".to_owned()]),
            ("notice", Card::Notice(BUSY.into()), vec![BUSY.to_owned()]),
        ] {
            // Near the top left, and near the bottom right where it flips above.
            for anchor in [area(size).min + vec2(20.0, 20.0), area(size).max - vec2(20.0, 20.0)] {
                let ctx = egui::Context::default();
                crate::ui::window::install_fonts(&ctx);
                let mut frame = 0;
                let drawn = settle(&ctx, size, &mut frame, &shown(card.clone(), anchor, size));
                assert!(area(size).contains_rect(drawn.card), "{state}: the card {:?} stays in the page at {size:?}", drawn.card);
                assert!(!drawn.card.contains(anchor), "{state}: the card leaves the link visible at {size:?}");
                for line in lines.iter().chain([&"example.test".to_owned()]) {
                    let (_, bounds, clip) = drawn.text.iter().find(|(text, ..)| text == line).unwrap_or_else(|| panic!("{state}: {line:?} is not drawn at {size:?}"));
                    assert!(clip.expand(0.5).contains_rect(*bounds) && drawn.card.contains_rect(*bounds), "{state}: {line:?} is clipped at {size:?}");
                }
                for (i, (text, bounds, _)) in drawn.text.iter().enumerate() {
                    for (other, other_bounds, _) in &drawn.text[i + 1..] {
                        assert!(!bounds.shrink(0.5).intersects(other_bounds.shrink(0.5)), "{state}: {text:?} overlaps {other:?} at {size:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn the_card_never_takes_focus_from_the_page() {
    for size in SIZES {
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        let mut frame = 0;
        let shown = shown(Card::Summary { text: "A fixture summary.".into(), provider: "Ollama".into() }, area(size).min + vec2(40.0, 40.0), size);
        let drawn = settle(&ctx, size, &mut frame, &shown);
        let inside = drawn.card.center();
        draw(&ctx, size, &mut frame, &shown, vec![Event::PointerMoved(inside)]);
        for pressed in [true, false] {
            draw(&ctx, size, &mut frame, &shown, vec![Event::PointerButton { pos: inside, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
        }
        for key in [egui::Key::Tab, egui::Key::Tab] {
            for pressed in [true, false] {
                draw(&ctx, size, &mut frame, &shown, vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }]);
            }
        }
        settle(&ctx, size, &mut frame, &shown);
        assert_eq!(ctx.memory(|memory| memory.focused()), None, "nothing in the card takes focus at {size:?}");
    }
}

#[test]
fn the_card_sits_below_the_pointer_or_above_it_when_there_is_no_room() {
    let page = Rect::from_min_max(pos2(100.0, 50.0), pos2(700.0, 450.0));
    let size = vec2(320.0, 120.0);
    assert_eq!(placement(pos2(200.0, 100.0), page, size), pos2(188.0, 118.0), "below and a little left of the pointer");
    assert_eq!(placement(pos2(200.0, 400.0), page, size), pos2(188.0, 262.0), "above when it would run off the bottom");
    assert_eq!(placement(pos2(690.0, 100.0), page, size).x, 372.0, "kept inside on the right");
    assert_eq!(placement(pos2(102.0, 100.0), page, size).x, 108.0, "and on the left");
    assert_eq!(placement(pos2(400.0, 120.0), Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 200.0)), size).y, 8.0, "a short page keeps it at the top");
}

#[test]
fn escape_reaches_the_card_before_the_page_and_the_card_is_ticked_every_frame() {
    // The dismissal chain and the frame need the whole app: checked in source.
    let app = include_str!("../app.rs");
    let chain = app.find("} else if self.link_previews.escape() {").expect("Escape asks the preview first");
    let page = app.find("send_web_escape(&v);").unwrap();
    assert!(app.find("} else if self.previews.dismiss() {").unwrap() < chain && chain < page, "after the sidebar preview, before the page");
    let overlays = include_str!("overlays.rs");
    assert!(overlays.contains("    menu(app, ui, &pal);\n    crate::ui::link_preview_card::show(app, ui.ctx(), &pal);\n"));
    assert!(overlays.contains("app.link_previews.seen = Some(crate::link_preview::hover(&view, &nav, pointer, shown));"), "Peek previews in its own profile");
    let content = include_str!("content.rs");
    assert!(content.contains("let view = tab.view.clone().filter(|view| crate::app::tab_profile(&app.lib, tab) == Some(view.profile_id()));"), "a tab previews only in its own profile");
}
