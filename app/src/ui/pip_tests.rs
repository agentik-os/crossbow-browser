use super::*;
use crate::app::pip::{DEFAULT_SIZE, MIN_SIZE};
use egui::{Event, PointerButton, Pos2, ViewportId, ViewportInfo};

const SIZES: [Vec2; 2] = [MIN_SIZE, DEFAULT_SIZE];
const TEXTURE: TextureId = TextureId::User(7);

/// The player's UI over its shared surface, as its viewport runs it.
struct Player {
    ctx: egui::Context,
    surface: PlayerSurface,
    size: Vec2,
    time: f64,
}

struct Shown {
    text: Vec<(String, Rect)>,
    /// Each textured mesh's bounds and its texture coordinates' extent.
    images: Vec<(Rect, Rect)>,
    commands: Vec<egui::ViewportCommand>,
}

impl Shown {
    /// The Lucide glyphs drawn: one per control shown.
    fn glyphs(&self) -> usize {
        self.text.iter().filter(|(text, _)| text.chars().all(|c| ('\u{e000}'..='\u{f8ff}').contains(&c)) && !text.is_empty()).count()
    }
}

impl Player {
    fn new(size: Vec2, frame: PlayerFrame) -> Self {
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        let mut surface = PlayerSurface::default();
        surface.publish(frame);
        let mut player = Player { ctx, surface, size, time: 0.0 };
        for _ in 0..3 { player.frame(vec![]); }
        player
    }

    fn frame_with(&mut self, events: Vec<Event>, close: bool) -> Shown {
        self.time += 1.0 / 60.0;
        let mut raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)), time: Some(self.time), focused: true, events, ..Default::default() };
        let info = ViewportInfo { native_pixels_per_point: Some(2.0), events: if close { vec![egui::ViewportEvent::Close] } else { vec![] }, ..Default::default() };
        raw.viewports.insert(ViewportId::ROOT, info);
        let surface = &mut self.surface;
        let mut output = self.ctx.run_ui(raw, |ui| show(ui, surface));
        output.textures_delta.clear();
        let text = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()))),
            _ => None,
        }).collect();
        let images = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Mesh(mesh) if mesh.texture_id == TEXTURE => {
                let uv = mesh.vertices.iter().fold(Rect::NOTHING, |extent, vertex| extent.union(Rect::from_min_size(vertex.uv, Vec2::ZERO)));
                Some((mesh.calc_bounds(), uv))
            }
            _ => None,
        }).collect();
        let commands = output.viewport_output.remove(&ViewportId::ROOT).map(|viewport| viewport.commands).unwrap_or_default();
        Shown { text, images, commands }
    }

    fn frame(&mut self, events: Vec<Event>) -> Shown {
        self.frame_with(events, false)
    }

    fn hover(&mut self, at: Pos2) -> Shown {
        self.frame(vec![Event::PointerMoved(at)]);
        self.frame(vec![])
    }

    fn click(&mut self, at: Pos2) -> Vec<PlayerInput> {
        self.hover(at);
        let press = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        self.frame(vec![press(true)]);
        self.frame(vec![press(false)]);
        self.frame(vec![]);
        self.surface.take()
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) -> Vec<PlayerInput> {
        self.frame(vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }]);
        self.frame(vec![Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers }]);
        self.surface.take()
    }

    fn window(&self) -> Rect {
        Rect::from_min_size(Pos2::ZERO, self.size)
    }
}

fn playing() -> PlayerFrame {
    PlayerFrame { texture: Some((TEXTURE, vec2(2560.0, 1600.0))), crop: Some(Rect::from_min_size(pos2(200.0, 100.0), vec2(1280.0, 720.0))), on_top: true, ..Default::default() }
}

#[test]
fn the_controls_fit_and_each_click_asks_for_exactly_one_thing_at_the_minimum_and_default_sizes() {
    for size in SIZES {
        let window = Rect::from_min_size(Pos2::ZERO, size);
        let at = layout(window);
        let rects = [at.on_top, at.back_to_tab, at.close, at.back, at.play, at.forward, at.mute];
        for (i, rect) in rects.iter().enumerate() {
            assert!(window.shrink(INSET - 0.5).contains_rect(*rect), "control {i} is inside the window at {size:?}");
            assert!(rect.width() >= 28.0 && rect.height() >= 28.0, "control {i} is large enough to hit");
            for other in &rects[i + 1..] { assert!(!rect.expand(1.0).intersects(*other), "controls {i} and another overlap at {size:?}"); }
        }
        assert_eq!(at.back.center().y, at.play.center().y);
        assert_eq!(at.on_top.top(), at.close.top(), "the top row shares one line");

        for (paused, muted, on_top) in [(false, false, true), (true, true, false)] {
            let mut player = Player::new(size, PlayerFrame { paused, muted, on_top, ..playing() });
            for (rect, expected) in [
                (at.play, PlayerInput::Control(Control::PlayPause)),
                (at.back, PlayerInput::Control(Control::Back)),
                (at.forward, PlayerInput::Control(Control::Forward)),
                (at.mute, PlayerInput::Control(Control::Mute)),
                (at.back_to_tab, PlayerInput::Control(Control::BackToTab)),
                (at.close, PlayerInput::Control(Control::Close)),
                (at.on_top, PlayerInput::KeepOnTop(!on_top)),
            ] {
                assert_eq!(player.click(rect.center()), [expected], "at {size:?}, paused {paused}");
            }
            let shown = player.hover(at.play.center());
            assert_eq!(shown.glyphs(), 7, "every control shows at {size:?}");
            assert!(shown.commands.is_empty(), "a click is not a drag");
            // Resting on a control names it.
            let name = if paused { "Play" } else { "Pause" };
            assert!((0..90).any(|_| player.frame(vec![]).text.iter().any(|(text, _)| text == name)), "{name} tooltip at {size:?}");
        }
    }
}

#[test]
fn controls_hide_while_the_video_plays_and_show_on_hover_focus_pause_or_end() {
    for size in SIZES {
        let mut player = Player::new(size, playing());
        assert_eq!(player.frame(vec![]).glyphs(), 0, "only the video while it plays");
        assert_eq!(player.hover(player.window().center()).glyphs(), 7);
        assert_eq!(player.frame(vec![Event::PointerGone]).glyphs(), 0);
        // Tab reaches the controls from the keyboard; Space presses the focused one.
        player.key(Key::Tab, Modifiers::NONE);
        assert_eq!(player.frame(vec![]).glyphs(), 7, "a focused control keeps them shown");
        assert_eq!(player.key(Key::Space, Modifiers::NONE).len(), 1, "one press, one input");

        let mut player = Player::new(size, PlayerFrame { paused: true, ..playing() });
        assert_eq!(player.frame(vec![]).glyphs(), 7, "paused");

        // Over: "Video ended" where the playback controls were; Back to Tab, Close and Keep on Top remain.
        let mut player = Player::new(size, PlayerFrame { status: Status::Ended, ..playing() });
        let shown = player.frame(vec![]);
        assert_eq!(shown.glyphs(), 3);
        assert!(shown.images.is_empty(), "no stale frame");
        let (_, bounds) = shown.text.iter().find(|(text, _)| text == ENDED).expect("says Video ended");
        let at = layout(player.window());
        assert!(player.window().shrink(INSET).contains_rect(*bounds), "{bounds:?} at {size:?}");
        for rect in [at.on_top, at.back_to_tab, at.close] { assert!(!rect.intersects(*bounds)); }
        assert_eq!(player.click(at.play.center()), [], "nothing to play");
        assert_eq!(player.key(Key::Space, Modifiers::NONE), []);
        assert_eq!(player.click(at.close.center()), [PlayerInput::Control(Control::Close)]);

        // Out of view: said beside Mute, with the controls still there.
        let mut player = Player::new(size, PlayerFrame { status: Status::OutOfView, crop: None, ..playing() });
        let shown = player.frame(vec![]);
        assert_eq!(shown.glyphs(), 7);
        let (_, bounds) = shown.text.iter().find(|(text, _)| text == OUT_OF_VIEW).expect("says Video out of view");
        assert!(player.window().contains_rect(*bounds) && !bounds.intersects(at.mute) && !bounds.intersects(at.back) && !bounds.intersects(at.play));

        // A Keep on Top that could not be saved says so, inside the window.
        let notice = "Couldn't save Keep on Top: disk full. Try again.";
        let mut player = Player::new(size, PlayerFrame { notice: Some(notice.into()), ..playing() });
        let shown = player.frame(vec![]);
        let words: Vec<_> = shown.text.iter().filter(|(text, _)| notice.contains(text.trim()) && !text.trim().is_empty()).collect();
        assert!(!words.is_empty());
        for (_, bounds) in words { assert!(player.window().contains_rect(*bounds) && !bounds.intersects(at.mute), "{bounds:?} at {size:?}"); }
    }
}

#[test]
fn the_video_is_cut_from_the_tabs_frame_and_letterboxed() {
    for size in SIZES {
        let mut player = Player::new(size, playing());
        let shown = player.frame(vec![]);
        let [(bounds, uv)] = shown.images[..] else { panic!("one image: {:?}", shown.images) };
        assert_eq!(uv, Rect::from_min_max(pos2(200.0 / 2560.0, 100.0 / 1600.0), pos2(1480.0 / 2560.0, 820.0 / 1600.0)), "only the video's pixels");
        assert!((bounds.width() - size.x).abs() < 0.01 && (bounds.height() - size.y).abs() < 0.01, "16:9 fills the window");
        // A 4:3 video is pillarboxed, centred.
        player.surface.publish(PlayerFrame { crop: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(960.0, 720.0))), ..playing() });
        let shown = player.frame(vec![]);
        let [(bounds, _)] = shown.images[..] else { panic!() };
        assert!((bounds.height() - size.y).abs() < 0.01 && (bounds.width() - size.y * 4.0 / 3.0).abs() < 0.01 && (bounds.center().x - size.x / 2.0).abs() < 0.01);
        // Nothing to cut: no image.
        player.surface.publish(PlayerFrame { crop: None, ..playing() });
        assert!(player.frame(vec![]).images.is_empty());
    }
}

#[test]
fn space_plays_or_pauses_escape_and_command_w_close_and_dragging_moves_the_window() {
    let mut player = Player::new(DEFAULT_SIZE, playing());
    assert_eq!(player.key(Key::Space, Modifiers::NONE), [PlayerInput::Control(Control::PlayPause)]);
    assert_eq!(player.key(Key::Escape, Modifiers::NONE), [PlayerInput::Control(Control::Close)]);
    assert_eq!(player.key(Key::W, Modifiers::COMMAND), [PlayerInput::Control(Control::Close)]);
    player.frame_with(vec![], true);
    assert_eq!(player.surface.take(), [PlayerInput::Control(Control::Close)], "the window's own close");
    // Dragging the video, away from the controls, moves the window.
    let from = pos2(60.0, 60.0);
    player.hover(from);
    let press = Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE };
    let mut commands = player.frame(vec![press]).commands;
    for step in 1..6 { commands.extend(player.frame(vec![Event::PointerMoved(from + vec2(step as f32 * 4.0, 0.0))]).commands); }
    assert!(commands.contains(&egui::ViewportCommand::StartDrag), "{commands:?}");
    assert_eq!(player.surface.take(), [], "a drag is not a control");
    // The window's place is read every pass for the root to keep.
    assert_eq!(player.surface.outer, None);
}
