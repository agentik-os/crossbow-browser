//! The page menu's entries for selected text, drawn as the menu draws them
//! at 640×420 and 1360×860 and driven by simulated pointer and keyboard
//! input. A chosen Copy as Quote is applied as the app applies it, to a fake
//! pasteboard on a disposable store.

use super::*;
use crate::quote::{Quote, QuoteBoard, QuoteHost};
use crossbow_core::store::Store;
use crossbow_engine::ContextMenuInfo;
use egui::{Event, Modifiers, PointerButton, Vec2};
use std::cell::RefCell;
use std::rc::Rc;

const SIZES: [Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
const PAGE: &str = "https://www.example.test/article";

#[derive(Clone, Default)]
struct FakeBoard(Rc<RefCell<Vec<Quote>>>);

impl QuoteBoard for FakeBoard {
    fn copy(&mut self, quote: &Quote) -> Result<(), String> {
        self.0.borrow_mut().push(quote.clone());
        Ok(())
    }
}

struct Menu {
    ctx: egui::Context,
    size: Vec2,
    frame: u32,
    info: ContextMenuInfo,
    store: Store,
    quote: QuoteHost,
    board: FakeBoard,
    picks: Vec<SelectionPick>,
}

impl Menu {
    fn new(size: Vec2, selection: &str, on: bool) -> Menu {
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        let store = Store::open_in_memory().unwrap();
        let board = FakeBoard::default();
        let mut quote = QuoteHost::with_board(Box::new(board.clone()));
        if on { quote.switch(&store).choose(&store, true); }
        let info = ContextMenuInfo { selection_text: selection.into(), page_url: PAGE.into(), ..Default::default() };
        Menu { ctx, size, frame: 0, info, store, quote, board, picks: Vec::new() }
    }

    /// One frame of the menu's selected-text entries; a Copy as Quote pick
    /// is applied as the app applies it.
    fn frame(&mut self, events: Vec<Event>) -> Vec<(String, Rect)> {
        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
        self.frame += 1;
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)), time: Some(f64::from(self.frame) / 60.0), events, ..Default::default() };
        let on = self.quote.enabled(&self.store);
        let info = self.info.clone();
        let mut pick = None;
        let mut output = self.ctx.run_ui(input, |ui| {
            let ctx = ui.ctx().clone();
            let m = MenuUi::new(&pal, false);
            menu_surface(pos2(40.0, 40.0)).show(&ctx, |ui| {
                menu_frame(&pal).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    pick = selection_entries(&m, ui, &info, on, None);
                });
            });
        });
        output.textures_delta.clear();
        if let Some(pick) = pick {
            self.picks.push(pick);
            if pick == SelectionPick::Quote {
                let (title, page_url) = crate::quote::source(&info, PAGE, "Fixture Article");
                assert_eq!(self.quote.copy(&self.store, &info.selection_text, &title, &page_url), Some(Ok(())));
            }
        }
        output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => {
                let bounds = Rect::from_min_size(text.pos, text.galley.size());
                assert!(shape.clip_rect.expand(0.5).contains_rect(bounds), "{:?} is clipped at {:?}", text.galley.job.text, self.size);
                Some((text.galley.job.text.clone(), bounds))
            }
            _ => None,
        }).collect()
    }

    fn settle(&mut self) -> Vec<(String, Rect)> {
        for _ in 0..3 { self.frame(Vec::new()); }
        self.frame(Vec::new())
    }

    fn labels(&mut self) -> Vec<String> {
        self.settle().into_iter().map(|(text, _)| text).filter(|text| !text.is_empty()).collect()
    }

    fn click_at(&mut self, pos: Pos2) {
        self.frame(vec![Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
        }
        self.settle();
    }

    fn click(&mut self, label: &str) {
        let shown = self.settle();
        let pos = shown.iter().find(|(text, _)| text == label).unwrap_or_else(|| panic!("no {label:?}: {shown:?}")).1.center();
        self.click_at(pos);
    }

    fn key(&mut self, key: egui::Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }]);
        }
        self.settle();
    }
}

#[test]
fn copy_as_quote_is_offered_after_copy_only_while_on_with_text_selected() {
    let copy = crate::input::shortcut_label(true, false, false, false, "C");
    for size in SIZES {
        let mut off = Menu::new(size, "Quoted words", false);
        assert_eq!(off.labels(), ["Copy", copy.as_str(), "Search selected text"], "off by default at {size:?}");

        let mut on = Menu::new(size, "Quoted words", true);
        let shown = on.settle();
        assert_eq!(on.labels(), ["Copy", copy.as_str(), crate::quote::LABEL, "Search selected text"], "at {size:?}");
        let y = |label: &str| shown.iter().find(|(text, _)| text == label).unwrap().1.center().y;
        assert!(y("Copy") < y(crate::quote::LABEL) && y(crate::quote::LABEL) < y("Search selected text"));

        for nothing in ["\u{200B}", " \n\t "] {
            let mut empty = Menu::new(size, nothing, true);
            assert!(!empty.labels().iter().any(|label| label == crate::quote::LABEL), "{nothing:?} has nothing to quote at {size:?}");
        }
        assert!(off.board.0.borrow().is_empty() && on.board.0.borrow().is_empty(), "showing the menu writes nothing");
    }
}

#[test]
fn choosing_copy_as_quote_writes_once_and_dismissing_writes_nothing() {
    for size in SIZES {
        let mut menu = Menu::new(size, "Quoted\nwords", true);
        // Escape and a click outside dismiss the menu without a pick.
        menu.key(egui::Key::Escape);
        menu.click_at(pos2(size.x - 20.0, size.y - 20.0));
        assert!(menu.picks.is_empty() && menu.board.0.borrow().is_empty(), "dismissing writes nothing at {size:?}");

        menu.click(crate::quote::LABEL);
        assert_eq!(menu.picks, [SelectionPick::Quote]);
        assert_eq!(*menu.board.0.borrow(), [crate::quote::quote("Quoted words", "Fixture Article", PAGE).unwrap()], "at {size:?}");
        assert_eq!(menu.board.0.borrow()[0].plain, "“Quoted words” — Fixture Article (https://www.example.test/article)");

        menu.click("Copy");
        assert_eq!(menu.picks, [SelectionPick::Quote, SelectionPick::Copy]);
        assert_eq!(menu.board.0.borrow().len(), 1, "Copy leaves the quote alone");
    }
}

#[test]
fn copy_as_quote_works_from_the_keyboard() {
    for size in SIZES {
        let mut menu = Menu::new(size, "Quoted words", true);
        menu.settle();
        menu.key(egui::Key::Tab);
        menu.key(egui::Key::Tab);
        assert!(menu.picks.is_empty() && menu.board.0.borrow().is_empty(), "moving focus writes nothing");
        menu.key(egui::Key::Enter);
        assert_eq!(menu.picks, [SelectionPick::Quote], "Tab twice reaches Copy as Quote at {size:?}");
        assert_eq!(menu.board.0.borrow().len(), 1);
    }
}
