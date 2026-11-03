use super::*;
use egui::{Modifiers, ViewportId, ViewportInfo};

const SIZES: [Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];

/// The page area of a window `size` points wide, right of a sidebar.
fn page_of(size: Vec2) -> Rect {
    Rect::from_min_max(pos2(240.0, 52.0), pos2(size.x - 8.0, size.y - 8.0))
}

fn press(pos: Pos2, modifiers: Modifiers) -> Event {
    Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers }
}

fn release(pos: Pos2) -> Event {
    Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }
}

fn key(key: Key) -> Event {
    Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }
}

fn palette() -> Palette {
    crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false)
}

/// What one pass drew: each text with its bounds and clip, and every shape's clip.
struct Drawn {
    text: Vec<(String, Rect, Rect)>,
    clips: Vec<Rect>,
    /// Filled rectangles: bounds and fill.
    fills: Vec<(Rect, Color32)>,
    /// Texts cut short with an ellipsis.
    elided: Vec<String>,
    cursor: CursorIcon,
}

impl Drawn {
    fn find(&self, label: &str) -> Option<Rect> {
        self.text.iter().find(|(text, ..)| text == label).map(|(_, bounds, _)| *bounds)
    }

    fn at(&self, label: &str) -> Pos2 {
        self.find(label).unwrap_or_else(|| panic!("{label:?} is not shown: {:?}", self.text.iter().map(|(text, ..)| text).collect::<Vec<_>>())).center()
    }
}

/// A window of its own for these tests.
struct Window {
    ctx: egui::Context,
    size: Vec2,
    ppp: f32,
    time: f64,
}

impl Window {
    fn new(size: Vec2, ppp: f32) -> Window {
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        Window { ctx, size, ppp, time: 0.0 }
    }

    fn frame(&mut self, events: Vec<Event>, mut draw: impl FnMut(&egui::Context)) -> Drawn {
        self.time += 1.0 / 60.0;
        let mut raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)), time: Some(self.time), focused: true, events, ..Default::default() };
        raw.viewports.insert(ViewportId::ROOT, ViewportInfo { native_pixels_per_point: Some(self.ppp), ..Default::default() });
        let mut output = self.ctx.run_ui(raw, |ui| draw(ui.ctx()));
        output.textures_delta.clear();
        let text = output.shapes.iter().filter_map(|shape| match &shape.shape {
            // A centered galley lies left of its position.
            egui::Shape::Text(text) => Some((text.galley.job.text.clone(), text.galley.rect.translate(text.pos.to_vec2()), shape.clip_rect)),
            _ => None,
        }).collect();
        let fills = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect) if rect.fill != Color32::TRANSPARENT => Some((rect.rect, rect.fill)),
            _ => None,
        }).collect();
        let elided = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.elided => Some(text.galley.job.text.clone()),
            _ => None,
        }).collect();
        Drawn { text, clips: output.shapes.iter().map(|shape| shape.clip_rect).collect(), fills, elided, cursor: output.platform_output.cursor_icon }
    }
}

#[test]
fn a_drag_selects_space_moves_it_and_release_or_return_captures_it() {
    for size in SIZES {
        let page = page_of(size);
        let from = page.min + vec2(40.0, 30.0);
        let mut selection = Selection::new(page);
        assert_eq!(selection.input(&[Event::PointerMoved(from), press(from, Modifiers::COMMAND | Modifiers::SHIFT)], false), Outcome::Selecting);
        assert_eq!(selection.input(&[Event::PointerMoved(from + vec2(120.0, 80.0))], false), Outcome::Selecting);
        assert_eq!(selection.rect, Some(Rect::from_min_size(from, vec2(120.0, 80.0))));
        // Space held: the same size moves, and stops at the page's edge.
        assert_eq!(selection.input(&[Event::PointerMoved(from + vec2(150.0, 90.0))], true), Outcome::Selecting);
        assert_eq!(selection.rect, Some(Rect::from_min_size(from + vec2(30.0, 10.0), vec2(120.0, 80.0))));
        selection.input(&[Event::PointerMoved(from + vec2(5000.0, 90.0))], true);
        let moved = selection.rect.unwrap();
        assert_eq!((moved.max.x, moved.size()), (page.max.x, vec2(120.0, 80.0)), "kept whole inside the page at {size:?}");
        // Space let go: the drag resizes again from where the selection now starts.
        let corner = moved.min + vec2(60.0, 40.0);
        selection.input(&[Event::PointerMoved(corner)], false);
        assert_eq!(selection.rect, Some(Rect::from_min_size(moved.min, vec2(60.0, 40.0))));
        assert_eq!(selection.input(&[release(corner)], false), Outcome::Chosen(Rect::from_min_size(moved.min, vec2(60.0, 40.0))));

        // Return captures the selection being dragged, and the page before any drag.
        let mut selection = Selection::new(page);
        selection.input(&[press(from, Modifiers::NONE), Event::PointerMoved(from + vec2(50.0, 20.0))], false);
        assert_eq!(selection.input(&[key(Key::Enter)], false), Outcome::Chosen(Rect::from_min_size(from, vec2(50.0, 20.0))));
        assert_eq!(Selection::new(page).input(&[key(Key::Enter)], false), Outcome::Chosen(page));

        // A drag past the page stops at its edge.
        let mut selection = Selection::new(page);
        selection.input(&[press(from, Modifiers::NONE), Event::PointerMoved(pos2(0.0, 0.0))], false);
        assert_eq!(selection.rect, Some(Rect::from_min_max(page.min, from)));
    }
}

#[test]
fn a_plain_click_captures_the_visible_page_and_escape_or_a_press_outside_cancels() {
    let page = page_of(SIZES[0]);
    let at = page.center();
    let mut selection = Selection::new(page);
    selection.input(&[press(at, Modifiers::NONE), Event::PointerMoved(at + vec2(3.0, 2.0))], false);
    assert_eq!(selection.input(&[release(at + vec2(3.0, 2.0))], false), Outcome::Chosen(page), "a few points is a click");

    let mut selection = Selection::new(page);
    selection.input(&[press(at, Modifiers::NONE), Event::PointerMoved(at + vec2(80.0, 60.0))], false);
    assert_eq!(selection.input(&[key(Key::Escape)], false), Outcome::Cancelled, "Escape mid-drag");
    assert_eq!(Selection::new(page).input(&[key(Key::Escape)], false), Outcome::Cancelled);
    assert_eq!(Selection::new(page).input(&[press(pos2(20.0, 200.0), Modifiers::NONE)], false), Outcome::Cancelled, "a press on the sidebar");
    // A release with no press of its own, from the click that chose Capture, is ignored.
    assert_eq!(Selection::new(page).input(&[release(at)], false), Outcome::Selecting);
}

#[test]
fn command_shift_press_on_the_page_starts_a_capture_only_when_on() {
    let page = page_of(SIZES[1]);
    let at = page.center();
    let both = Modifiers::COMMAND | Modifiers::SHIFT;
    assert_eq!(starts(&[Event::PointerMoved(at), press(at, both)], page, true), Some(at));
    assert_eq!(starts(&[press(at, both)], page, false), None, "off: the page gets it");
    assert_eq!(starts(&[press(at, Modifiers::COMMAND)], page, true), None);
    assert_eq!(starts(&[press(at, Modifiers::SHIFT)], page, true), None);
    assert_eq!(starts(&[press(at, both | Modifiers::ALT)], page, true), None);
    assert_eq!(starts(&[press(pos2(10.0, 10.0), both)], page, true), None, "outside the page");
    assert_eq!(starts(&[Event::PointerButton { pos: at, button: PointerButton::Secondary, pressed: true, modifiers: both }], page, true), None);
}

#[test]
fn the_overlay_dims_only_the_page_and_labels_the_selection_in_pixels() {
    for size in SIZES {
        for ppp in [1.0, 2.0] {
            let page = page_of(size);
            let texture = ((page.width() * ppp) as u32, (page.height() * ppp) as u32);
            let mut window = Window::new(size, ppp);
            let mut selection = Selection::new(page);
            let hover = page.center();
            let drawn = window.frame(vec![Event::PointerMoved(hover)], |ctx| paint(ctx, &selection, Some(texture), ppp));
            assert!(drawn.find(HINT).is_some(), "the hint before a drag at {size:?}");
            assert_eq!(drawn.cursor, CursorIcon::Crosshair);
            assert!(drawn.clips.iter().all(|clip| page.contains_rect(*clip)), "nothing outside the page at {size:?}");
            let (_, hint, clip) = drawn.text.iter().find(|(text, ..)| text == HINT).unwrap();
            assert!(clip.contains_rect(*hint), "the hint is whole at {size:?}");

            let from = page.min + vec2(20.0, 20.0);
            selection.input(&[press(from, Modifiers::NONE), Event::PointerMoved(from + vec2(150.5, 90.25))], false);
            let drawn = window.frame(vec![], |ctx| paint(ctx, &selection, Some(texture), ppp));
            let label = format!("{} × {}", (150.5 * ppp).round() as u32, (90.25 * ppp).round() as u32);
            assert!(drawn.find(&label).is_some(), "{label} at {size:?} @{ppp}");
            assert!(drawn.clips.iter().all(|clip| page.contains_rect(*clip)), "nothing outside the page at {size:?}");
            let rect = selection.rect.unwrap();
            let dims: Vec<Rect> = drawn.fills.iter().filter(|(_, fill)| *fill == DIM).map(|(rect, _)| *rect).collect();
            assert!(!dims.is_empty() && dims.iter().all(|dim| !dim.intersects(rect.shrink(0.5))), "the selection is clear at {size:?}");
            let dimmed: f32 = dims.iter().map(|dim| dim.area()).sum();
            assert!((dimmed + rect.area() - page.area()).abs() < 1.0, "the rest of the page is dimmed at {size:?}");
        }
    }
}

#[test]
fn the_card_sits_by_the_selection_inside_the_window_at_both_sizes() {
    for size in SIZES {
        let bounds = Rect::from_min_size(Pos2::ZERO, size);
        let page = page_of(size);
        for anchor in [Rect::from_min_size(page.min + vec2(20.0, 20.0), vec2(200.0, 100.0)), Rect::from_min_max(page.min + vec2(0.0, 200.0), page.max), page, Rect::from_min_size(page.max - vec2(30.0, 30.0), vec2(30.0, 30.0))] {
            let card = card_rect(anchor, bounds);
            assert!(bounds.shrink(GAP - 0.01).contains_rect(card), "{card:?} inside {bounds:?}");
            if anchor.max.y + GAP + card.height() <= bounds.max.y - GAP {
                assert_eq!(card.min.y, anchor.max.y + GAP, "under the selection");
            } else if anchor.min.y - GAP - card.height() >= bounds.min.y + GAP {
                assert_eq!(card.max.y, anchor.min.y - GAP, "above it");
            }
        }
    }
}

#[test]
fn the_card_offers_its_actions_and_never_takes_keyboard_focus() {
    for size in SIZES {
        let pal = palette();
        let mut window = Window::new(size, 2.0);
        let page = page_of(size);
        let mut view = CardView { anchor: Rect::from_min_size(page.min + vec2(30.0, 30.0), vec2(220.0, 120.0)), bounds: Rect::from_min_size(Pos2::ZERO, size), saved: true, notice: None };
        let mut chosen = Vec::new();
        let run = |window: &mut Window, view: &CardView, events: Vec<Event>, chosen: &mut Vec<CardAction>| {
            window.frame(events, |ctx| if let (Some(action), _) = card(ctx, view, &pal) { chosen.push(action) })
        };
        // An area shows from its second pass.
        run(&mut window, &view, vec![], &mut chosen);
        let drawn = run(&mut window, &view, vec![], &mut chosen);
        for label in [COPIED_AND_SAVED, COPY, SAVE_AS, OPEN_IN_CAPTURES] {
            let bounds = drawn.find(label).unwrap_or_else(|| panic!("{label:?} at {size:?}"));
            assert!(card_rect(view.anchor, view.bounds).contains_rect(bounds), "{label:?} inside the card at {size:?}");
        }
        for (label, action) in [(COPY, CardAction::Copy), (SAVE_AS, CardAction::SaveAs), (OPEN_IN_CAPTURES, CardAction::OpenInCaptures)] {
            let at = run(&mut window, &view, vec![], &mut chosen).at(label);
            for pressed in [true, false] {
                run(&mut window, &view, vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }], &mut chosen);
            }
            assert_eq!(chosen.pop(), Some(action), "{label:?} at {size:?}");
            assert_eq!(window.ctx.memory(|memory| memory.focused()), None, "{label:?} took focus");
        }
        let close = card_rect(view.anchor, view.bounds);
        let close = pos2(close.max.x - 22.0, close.min.y + 16.0);
        for pressed in [true, false] {
            run(&mut window, &view, vec![Event::PointerMoved(close), Event::PointerButton { pos: close, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }], &mut chosen);
        }
        assert_eq!(chosen.pop(), Some(CardAction::Close));
        for _ in 0..4 { run(&mut window, &view, vec![key(Key::Tab)], &mut chosen); }
        assert_eq!(window.ctx.memory(|memory| memory.focused()), None, "Tab never reaches the card at {size:?}");

        // Not saved: Open in Captures does nothing; a notice replaces the status.
        view.saved = false;
        view.notice = Some(crossbow_core::captures::SAVE_FAILED.into());
        let drawn = run(&mut window, &view, vec![], &mut chosen);
        assert!(drawn.find(COPIED).is_none() && drawn.text.iter().any(|(text, ..)| text.starts_with("Couldn't save the capture")), "the notice shows at {size:?}");
        let at = drawn.at(OPEN_IN_CAPTURES);
        for pressed in [true, false] {
            run(&mut window, &view, vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }], &mut chosen);
        }
        assert!(chosen.is_empty(), "nothing to open at {size:?}");

        // A damaged Captures file: the card's line fits whole.
        view.notice = Some(NOT_SAVED_DAMAGED.into());
        let drawn = run(&mut window, &view, vec![], &mut chosen);
        let bounds = drawn.find(NOT_SAVED_DAMAGED).unwrap_or_else(|| panic!("shown at {size:?}"));
        assert!(card_rect(view.anchor, view.bounds).contains_rect(bounds) && drawn.elided.is_empty(), "whole, inside the card at {size:?}");
    }
}

#[test]
fn a_captures_row_shows_its_page_and_time_and_offers_its_actions_on_hover() {
    for size in SIZES {
        let pal = palette();
        let mut window = Window::new(size, 1.0);
        // The Library panel's list width.
        let list = Rect::from_min_size(pos2(12.0, 80.0), vec2(320.0, size.y - 160.0));
        let rows = [
            Row { id: 1, title: "A story with a long title that never fits in one row of the list".into(), detail: "news.example · Oct 29, 15:04".into(), missing: false, thumbnail: None },
            Row { id: 2, title: "Gone".into(), detail: "site.example · Oct 28, 09:00".into(), missing: true, thumbnail: None },
        ];
        let mut chosen = Vec::new();
        let run = |window: &mut Window, events: Vec<Event>, chosen: &mut Vec<ListAction>| {
            window.frame(events, |ctx| {
                egui::Area::new(Id::unique("list")).fixed_pos(list.min).show(ctx, |ui| {
                    ui.set_width(list.width());
                    ui.spacing_mut().item_spacing.y = 1.0;
                    for one in &rows { if let Some(action) = row(ui, one, &pal) { chosen.push(action); } }
                });
            })
        };
        run(&mut window, vec![], &mut chosen);
        let drawn = run(&mut window, vec![], &mut chosen);
        for label in ["news.example · Oct 29, 15:04", "Gone", FILE_MISSING, REMOVE] {
            let bounds = drawn.find(label).unwrap_or_else(|| panic!("{label:?} at {size:?}"));
            assert!(list.contains_rect(bounds), "{label:?} inside the list at {size:?}");
        }
        assert!(drawn.find("site.example · Oct 28, 09:00").is_none(), "a missing file says so instead");
        assert!(drawn.elided.iter().any(|text| text.starts_with("A story")), "a long title is cut, not wrapped");

        // Hovered, the first row offers Copy, Reveal, Save As… and Delete at its right.
        let first = Rect::from_min_size(list.min, vec2(list.width(), ROW_HEIGHT));
        run(&mut window, vec![Event::PointerMoved(first.center())], &mut chosen);
        let buttons = [ListAction::Delete(1), ListAction::SaveAs(1), ListAction::Reveal(1), ListAction::Copy(1)];
        for (i, action) in buttons.into_iter().enumerate() {
            let at = pos2(first.max.x - 6.0 - 13.0 - i as f32 * 28.0, first.center().y);
            for pressed in [true, false] {
                run(&mut window, vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }], &mut chosen);
            }
            assert_eq!(chosen.pop(), Some(action), "at {size:?}");
        }
        let at = pos2(first.min.x + 120.0, first.center().y);
        for pressed in [true, false] {
            run(&mut window, vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }], &mut chosen);
        }
        assert_eq!(chosen.pop(), Some(ListAction::Open(1)), "the row opens the viewer");
        let at = run(&mut window, vec![], &mut chosen).at(REMOVE);
        for pressed in [true, false] {
            run(&mut window, vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }], &mut chosen);
        }
        assert_eq!(chosen, [ListAction::Remove(2)]);
    }
}

#[test]
fn the_viewer_shows_the_capture_and_closes() {
    for size in SIZES {
        let pal = palette();
        let mut window = Window::new(size, 2.0);
        let mut closed = false;
        window.frame(vec![], |ctx| closed |= viewer(ctx, "A story", None, &pal));
        let drawn = window.frame(vec![], |ctx| closed |= viewer(ctx, "A story", None, &pal));
        assert!(!closed && drawn.find("A story").is_some() && drawn.find(FILE_MISSING).is_some());
        let close = pos2(size.x - 26.0, 22.0);
        for pressed in [true, false] {
            window.frame(vec![Event::PointerMoved(close), Event::PointerButton { pos: close, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }], |ctx| closed |= viewer(ctx, "A story", None, &pal));
        }
        assert!(closed, "Close at {size:?}");
    }
}

#[test]
fn captures_that_cant_be_read_say_why_and_a_damaged_file_offers_move_aside() {
    for size in SIZES {
        let pal = palette();
        let mut window = Window::new(size, 1.0);
        // The Library panel's list width.
        let list = Rect::from_min_size(pos2(12.0, 80.0), vec2(320.0, size.y - 160.0));
        for (message, damaged) in [(crossbow_core::captures::DAMAGED, true), (crossbow_core::captures::UNAVAILABLE, false)] {
            let mut clicked = 0;
            let mut run = |window: &mut Window, events: Vec<Event>| window.frame(events, |ctx| {
                egui::Area::new(Id::unique("list")).fixed_pos(list.min).show(ctx, |ui| {
                    ui.set_width(list.width());
                    if unreadable(ui, message, damaged, &pal) { clicked += 1; }
                });
            });
            run(&mut window, vec![]);
            let drawn = run(&mut window, vec![]);
            let bounds = drawn.find(message).unwrap_or_else(|| panic!("{message:?} at {size:?}"));
            assert!(list.contains_rect(bounds) && bounds.height() > 20.0 && drawn.elided.is_empty(), "wrapped whole inside the list at {size:?}: {bounds:?}");
            let button = drawn.find(MOVE_ASIDE);
            assert_eq!(button.is_some(), damaged, "Move Aside only for damage at {size:?}");
            let Some(button) = button else { continue };
            assert!(button.min.y > bounds.max.y && (button.center().x - bounds.center().x).abs() < 1.0 && list.contains_rect(button), "centered below the reason at {size:?}");
            for pressed in [true, false] {
                run(&mut window, vec![Event::PointerMoved(button.center()), Event::PointerButton { pos: button.center(), button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
            }
            drop(run);
            assert_eq!(clicked, 1, "at {size:?}");
        }
    }
}
