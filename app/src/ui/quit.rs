//! The "Quit Crossbow?" confirmation shown by Warn Before Quitting.

use crate::ui::style::{self, Button, Ink, Kind};
use crate::ui::theme::Palette;
use crossbow_engine::DownloadInfo;
use egui::{Align2, Color32, Event, Id, Key, Sense, Stroke};

pub const TITLE: &str = "Quit Crossbow?";
const WIDTH: f32 = 340.0;

fn answered_with() -> Id {
    Id::unique("quit-answer-key")
}

/// Run every frame, before anything else sees its keys. While `asking`,
/// Enter answers Quit and Escape answers Cancel, typing reaches nothing
/// underneath, and Tab and Space stay for the two buttons. The release of the
/// key that answered is taken too, so no page sees a key-up without its press.
pub fn take_keys(ctx: &egui::Context, asking: bool) -> Option<bool> {
    let mut release: Option<Key> = ctx.data(|data| data.get_temp(answered_with()));
    let mut answer = None;
    ctx.input_mut(|input| {
        input.events.retain(|event| match event {
            Event::Key { key, pressed: false, .. } if release == Some(*key) => {
                release = None;
                false
            }
            _ if !asking || answer.is_some() => true,
            Event::Key { key: key @ (Key::Enter | Key::Escape), pressed, modifiers, .. } if modifiers.is_none() => {
                if *pressed {
                    answer = Some(*key == Key::Enter);
                    release = Some(*key);
                }
                false
            }
            Event::Key { key: Key::Tab | Key::Space, .. } => true,
            Event::Key { .. } | Event::Text(_) | Event::Ime(_) | Event::Copy | Event::Cut | Event::Paste(_) => false,
            _ => true,
        });
    });
    ctx.data_mut(|data| match release {
        Some(key) => { data.insert_temp(answered_with(), key); }
        None => data.remove::<Key>(answered_with()),
    });
    answer
}

/// Quitting stops every profile's unfinished downloads, paused ones included.
fn downloads_line(downloads: &[DownloadInfo]) -> Option<String> {
    match downloads.iter().filter(|download| download.active()).count() {
        0 => None,
        1 => Some("1 download is in progress and will be cancelled.".into()),
        n => Some(format!("{n} downloads are in progress and will be cancelled.")),
    }
}

/// Draw the confirmation above everything else. Some(true) is Quit, Some(false) Cancel.
/// `downloads` is the app's download list, as the Downloads card shows it.
pub fn show(ctx: &egui::Context, pal: &Palette, downloads: &[DownloadInfo]) -> Option<bool> {
    let screen = ctx.content_rect();
    let backdrop = egui::Area::new(Id::unique("quit-backdrop")).order(egui::Order::Foreground).fixed_pos(screen.min).sense(Sense::hover());
    let layer = backdrop.layer();
    ctx.move_to_top(layer);
    ctx.memory_mut(|memory| memory.set_modal_layer(layer));
    backdrop.show(ctx, |ui| {
        ui.allocate_rect(screen, Sense::CLICK);
        ui.painter().rect_filled(screen, 0, Color32::from_black_alpha(80));
    });
    let card = egui::Area::new(Id::unique("quit-card")).order(egui::Order::Foreground).pivot(Align2::CENTER_CENTER).fixed_pos(screen.center()).sense(Sense::hover());
    ctx.set_sublayer(layer, card.layer());
    let ink = Ink::new(pal.panel, pal.accent);
    let width = (screen.width() - 32.0).min(WIDTH);
    let answer = card.show(ctx, |ui| {
        let frame = egui::Frame::new().fill(pal.panel).stroke(Stroke::new(1.0, pal.panel_border)).corner_radius(14).inner_margin(20)
            .shadow(egui::epaint::Shadow { offset: [0, 20], blur: 56, spread: 0, color: Color32::from_black_alpha(90) });
        frame.show(ui, |ui| {
            ui.set_width(width - 40.0);
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(style::text(TITLE, 15.0, style::weight::SEMIBOLD).color(ink.text));
            if let Some(line) = downloads_line(downloads) {
                ui.label(style::text(line, 12.5, style::weight::REGULAR).color(ink.secondary));
            }
            if cfg!(target_os = "macos") {
                ui.label(style::text("Press ⌘Q again to quit.", 12.5, style::weight::REGULAR).color(ink.secondary));
            }
            ui.add_space(4.0);
            // A fixed-height row keeps the buttons out of the card's spare height.
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 32.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if Button::new("Quit").kind(Kind::Primary).show(ui, &ink).clicked() {
                    return Some(true);
                }
                Button::new("Cancel").kind(Kind::Secondary).show(ui, &ink).clicked().then_some(false)
            }).inner
        }).inner
    }).inner;
    // A button pressed with Space: its key-up belongs to the dialog too.
    if answer.is_some() && ctx.input(|input| input.key_pressed(Key::Space)) {
        ctx.data_mut(|data| data.insert_temp(answered_with(), Key::Space));
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quit::{QuitGuard, Route};
    use crate::shortcuts::tests::Fixture;
    use egui::{Modifiers, Rect, ViewportCommand, ViewportId, pos2, vec2};

    const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];

    /// The window as `CrossbowApp` runs it: the close request is checked before
    /// drawing, the confirmation takes its keys first and is drawn last.
    struct Window {
        ctx: egui::Context,
        size: egui::Vec2,
        guard: QuitGuard,
        frame: u32,
        /// The app's download list.
        downloads: Vec<DownloadInfo>,
        /// Keys that reached browser dispatch underneath.
        underneath: Vec<Event>,
        /// (text, bounds, clip, elided) of every label drawn.
        text: Vec<(String, Rect, Rect, bool)>,
        /// The card's panel rectangle, when shown.
        card: Option<Rect>,
        commands: Vec<ViewportCommand>,
    }

    impl Window {
        fn new(size: egui::Vec2) -> Self {
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            let fixture = Fixture::new();
            let mut guard = QuitGuard::load(&fixture.store());
            guard.choose(&fixture.store(), true);
            Window { ctx, size, guard, frame: 0, downloads: Vec::new(), underneath: Vec::new(), text: Vec::new(), card: None, commands: Vec::new() }
        }

        fn frame(&mut self, events: Vec<Event>, close_requested: bool) {
            let mut input = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), self.size)), time: Some(f64::from(self.frame) / 60.0), events, ..Default::default() };
            if close_requested {
                input.viewports.get_mut(&ViewportId::ROOT).unwrap().events.push(egui::ViewportEvent::Close);
            }
            self.frame += 1;
            let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
            let mut output = self.ctx.clone().run_ui(input, |ui| {
                let ctx = ui.ctx().clone();
                crate::quit::on_close_request(&mut self.guard, &ctx);
                if let Some(quit) = take_keys(&ctx, self.guard.asking()) { crate::quit::answer(&mut self.guard, &ctx, quit); }
                self.underneath.extend(ctx.input(|input| input.events.clone()));
                if self.guard.asking() {
                    if let Some(quit) = show(&ctx, &pal, &self.downloads) { crate::quit::answer(&mut self.guard, &ctx, quit); }
                }
            });
            self.text = output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()), shape.clip_rect, text.galley.elided)),
                _ => None,
            }).collect();
            // The card is the only frame with a shadow, painted as a Vec of the
            // shadow, then its rectangle (colours fade in, so match the shape).
            self.card = output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::Vec(shapes) => match shapes.last() {
                    Some(egui::Shape::Rect(rect)) => Some(rect.rect),
                    _ => None,
                },
                _ => None,
            }).max_by(|a, b| a.area().total_cmp(&b.area()));
            self.commands = output.viewport_output.get(&ViewportId::ROOT).map(|viewport| viewport.commands.clone()).unwrap_or_default();
            output.textures_delta.clear();
        }

        /// Settle, then check every label of a shown question fits: inside its
        /// clip, inside the card's padding, not elided and not overlapping another.
        fn settle(&mut self) {
            for _ in 0..4 { self.frame(vec![], false); }
            let Some(card) = self.card else { return };
            // Inside the 20-point margin and the 1-point border.
            let inner = card.shrink(21.0 - 0.5);
            assert!(Rect::from_min_size(pos2(0.0, 0.0), self.size).shrink(16.0).contains_rect(card), "the card fits the window at {:?}: {card:?}", self.size);
            for (i, (text, bounds, clip, elided)) in self.text.iter().enumerate() {
                assert!(clip.expand(0.5).contains_rect(*bounds) && inner.contains_rect(*bounds) && !elided, "{text:?} is clipped or overflows at {:?}: {bounds:?} in {inner:?}", self.size);
                for (other, other_bounds, ..) in &self.text[i + 1..] {
                    assert!(!bounds.shrink(0.5).intersects(other_bounds.shrink(0.5)), "{text:?} overlaps {other:?} at {:?}", self.size);
                }
            }
        }

        fn shown(&self, label: &str) -> Option<Rect> {
            self.text.iter().find(|(text, ..)| text == label).map(|(_, bounds, ..)| *bounds)
        }

        fn click(&mut self, label: &str) {
            let pos = self.shown(label).unwrap_or_else(|| panic!("{label} is not shown at {:?}", self.size)).center();
            for pressed in [true, false] {
                self.frame(vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE }], false);
            }
        }

        fn key(&mut self, key: Key, modifiers: Modifiers) {
            for pressed in [true, false] {
                self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }], false);
            }
        }

        /// Click the window's close button.
        fn close(&mut self) {
            self.frame(vec![], true);
        }
    }

    #[test]
    fn closing_the_last_window_asks_and_cancel_or_escape_leaves_it_open() {
        for size in SIZES {
            let mut w = Window::new(size);
            w.close();
            assert!(w.commands.contains(&ViewportCommand::CancelClose), "the window stays open while asking at {size:?}");
            w.settle();
            let title = w.shown(TITLE).expect("the question is shown");
            assert!(w.card.is_some(), "the fit check sees the card");
            let screen = Rect::from_min_size(pos2(0.0, 0.0), size);
            for label in [TITLE, "Quit", "Cancel"] {
                let bounds = w.shown(label).unwrap_or_else(|| panic!("{label} at {size:?}"));
                assert!(screen.shrink(16.0).contains_rect(bounds), "{label} fits at {size:?}: {bounds:?}");
            }
            let (quit, cancel) = (w.shown("Quit").unwrap(), w.shown("Cancel").unwrap());
            assert!((quit.center().y - cancel.center().y).abs() < 1.0 && cancel.max.x < quit.min.x, "Cancel, then Quit, on one row");
            assert!(title.max.y < quit.min.y && quit.max.y - title.min.y < 110.0, "a compact card: {title:?} {quit:?}");
            assert!((title.center().x - size.x / 2.0).abs() < 170.0 && (quit.center().y - size.y / 2.0).abs() < 80.0, "centred in the window");
            if cfg!(target_os = "macos") { assert!(w.shown("Press ⌘Q again to quit.").is_some()); }

            w.close();
            assert!(w.commands.contains(&ViewportCommand::CancelClose), "closing again keeps the question");
            w.click("Cancel");
            w.settle();
            assert!(!w.guard.asking() && w.shown(TITLE).is_none(), "Cancel closes the question");
            assert!(!w.commands.contains(&ViewportCommand::Close));

            w.close();
            w.settle();
            w.key(Key::Escape, Modifiers::NONE);
            w.settle();
            assert!(!w.guard.asking(), "Escape cancels");
            assert!(!w.underneath.iter().any(|event| matches!(event, Event::Key { key: Key::Escape, .. })), "and never reaches the page or browser shortcuts");
            w.frame(vec![], false);
            assert!(!w.commands.iter().any(|command| matches!(command, ViewportCommand::Close | ViewportCommand::CancelClose)), "nothing closes");
        }
    }

    #[test]
    fn quit_or_enter_closes_the_window_and_typing_reaches_nothing_underneath() {
        for size in SIZES {
            let mut w = Window::new(size);
            w.close();
            w.settle();
            w.frame(vec![Event::Text("q".into()), Event::Key { key: Key::W, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND }], false);
            assert!(w.underneath.is_empty(), "typing and shortcuts reach nothing while asking: {:?}", w.underneath);
            w.click("Quit");
            assert!(w.commands.contains(&ViewportCommand::Close), "Quit closes the window at {size:?}");
            w.close();
            assert!(!w.commands.contains(&ViewportCommand::CancelClose), "and that close goes through");

            let mut w = Window::new(size);
            w.close();
            w.settle();
            w.frame(vec![Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }], false);
            assert!(w.commands.contains(&ViewportCommand::Close), "Enter answers Quit at {size:?}");
            w.frame(vec![Event::Key { key: Key::Enter, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE }], false);
            w.close();
            assert!(!w.commands.contains(&ViewportCommand::CancelClose), "Enter's close goes through at {size:?}");
            assert!(w.underneath.iter().all(|event| !matches!(event, Event::Key { key: Key::Enter, .. })));
        }
    }

    #[test]
    fn tab_and_space_press_the_focused_button_and_its_key_up_stays_with_the_dialog() {
        for size in SIZES {
            let mut w = Window::new(size);
            w.close();
            w.settle();
            let cancel = w.shown("Cancel").unwrap();
            let mut reached = false;
            for _ in 0..4 {
                w.key(Key::Tab, Modifiers::NONE);
                let focused = w.ctx.memory(|memory| memory.focused()).and_then(|id| w.ctx.read_response(id)).map(|response| response.rect);
                if focused.is_some_and(|rect| rect.contains(cancel.center())) { reached = true; break; }
            }
            assert!(reached, "Tab reaches Cancel at {size:?}");
            w.underneath.clear();
            w.key(Key::Space, Modifiers::NONE);
            w.settle();
            assert!(!w.guard.asking(), "Space presses Cancel");
            assert!(!w.commands.contains(&ViewportCommand::Close));
            assert!(!w.underneath.iter().any(|event| matches!(event, Event::Key { key: Key::Space, pressed: false, .. })), "its key-up stays with the dialog: {:?}", w.underneath);
        }
    }

    fn download(id: u32, profile: &str) -> DownloadInfo {
        DownloadInfo { id, profile_id: profile.into(), file_name: format!("fixture-{id}.bin"), in_progress: true, received: 10, total: 100, percent: 10, ..Default::default() }
    }

    fn downloads_line(w: &Window) -> Option<String> {
        w.text.iter().map(|(text, ..)| text.clone()).find(|text| text.contains("in progress and will be cancelled"))
    }

    #[test]
    fn the_question_counts_downloads_in_progress_and_follows_them_while_it_shows() {
        for size in SIZES {
            let mut w = Window::new(size);
            w.close();
            w.settle();
            assert_eq!(downloads_line(&w), None, "no downloads, no line at {size:?}");
            w.click("Cancel");

            // Finished, cancelled, interrupted and failed downloads are not in progress.
            w.downloads = vec![
                download(1, "A"),
                DownloadInfo { complete: true, in_progress: false, ..download(2, "A") },
                DownloadInfo { canceled: true, in_progress: false, ..download(3, "A") },
                DownloadInfo { interrupted: true, in_progress: false, ..download(4, "B") },
                DownloadInfo { error: Some("Network interruption".into()), ..download(5, "B") },
            ];
            w.close();
            w.settle();
            assert_eq!(downloads_line(&w).as_deref(), Some("1 download is in progress and will be cancelled."), "at {size:?}");
            w.click("Cancel");

            // Every profile's downloads stop when Crossbow quits; a paused one too.
            w.downloads = vec![download(1, "A"), download(2, "B"), DownloadInfo { paused: true, ..download(3, "A") }];
            w.close();
            w.settle();
            assert_eq!(downloads_line(&w).as_deref(), Some("3 downloads are in progress and will be cancelled."), "at {size:?}");

            // Downloads finish while the question shows: it follows the app's list.
            crate::downloads::update(&mut w.downloads, DownloadInfo { complete: true, in_progress: false, percent: 100, ..download(2, "B") });
            w.settle();
            assert_eq!(downloads_line(&w).as_deref(), Some("2 downloads are in progress and will be cancelled."));
            crate::downloads::update(&mut w.downloads, DownloadInfo { canceled: true, in_progress: false, ..download(3, "A") });
            w.settle();
            assert_eq!(downloads_line(&w).as_deref(), Some("1 download is in progress and will be cancelled."));
            crate::downloads::update(&mut w.downloads, DownloadInfo { complete: true, in_progress: false, percent: 100, ..download(1, "A") });
            w.settle();
            assert_eq!(downloads_line(&w), None, "the line goes once none are left");
            assert!(w.guard.asking() && w.shown(TITLE).is_some(), "and the question stays");
            w.click("Quit");
            assert!(w.commands.contains(&ViewportCommand::Close), "Quit still answers at {size:?}");
        }
    }

    #[test]
    fn many_downloads_still_fit_the_question() {
        for size in SIZES {
            let mut w = Window::new(size);
            w.downloads = (1..=1234).map(|id| download(id, "A")).collect();
            w.close();
            w.settle();
            assert_eq!(downloads_line(&w).as_deref(), Some("1234 downloads are in progress and will be cancelled."), "at {size:?}");
        }
    }

    #[test]
    fn a_menu_quit_brings_the_window_forward_and_a_second_command_q_quits() {
        let mut w = Window::new(SIZES[0]);
        assert!(!w.guard.request(Route::Quit), "⌘Q asks");
        w.frame(vec![], false);
        for command in [ViewportCommand::Visible(true), ViewportCommand::Minimized(false), ViewportCommand::Focus] {
            assert!(w.commands.contains(&command), "{command:?}");
        }
        w.settle();
        assert!(w.shown(TITLE).is_some());
        assert!(w.guard.request(Route::Quit), "⌘Q again quits");
        w.settle();
        assert!(w.shown(TITLE).is_none());
    }

    #[test]
    fn with_the_warning_off_closing_the_window_quits_without_asking() {
        for size in SIZES {
            let mut w = Window::new(size);
            let fixture = Fixture::new();
            w.guard.choose(&fixture.store(), false);
            w.close();
            assert!(!w.commands.contains(&ViewportCommand::CancelClose), "the window closes at {size:?}");
            w.settle();
            assert!(w.shown(TITLE).is_none(), "nothing asks");
        }
    }
}
