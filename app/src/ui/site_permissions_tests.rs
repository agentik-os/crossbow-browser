//! The permission prompt, the quiet "tried to open" notice and Settings →
//! Privacy → Site Settings, driven by simulated pointer and keyboard input at
//! 640×420 and 1360×860. Disposable libraries and fake apps only.

use super::*;
use crate::site_permissions::Choice;
use crate::site_permissions::tests::{A, Fixture, MEET, request};
use crate::ui::settings::{compact_navigation_needed, set_visuals, sheet_rect, show_backdrop, show_sheet};
use crossbow_core::site_permissions::{self as rules, Decision, Permission};
use egui::{Event, Key, Modifiers, PointerButton, Rect, RichText, Vec2, pos2, vec2};

fn sizes() -> [Vec2; 2] {
    [vec2(640.0, 420.0), vec2(1360.0, 860.0)]
}

fn palette() -> Palette {
    crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false)
}

struct Shown {
    text: Vec<(String, Rect, Rect)>,
}

impl Shown {
    fn has(&self, label: &str) -> bool {
        self.text.iter().any(|(text, _, _)| text == label)
    }

    fn find(&self, label: &str) -> Option<Rect> {
        self.text.iter().find(|(text, _, _)| text == label).map(|(_, rect, _)| *rect)
    }

    fn visible(&self, label: &str) -> Option<Rect> {
        self.text.iter().find(|(text, rect, clip)| text == label && clip.expand(0.5).contains_rect(*rect)).map(|(_, rect, _)| *rect)
    }
}

/// egui frames at one window size, with the time under the test's control.
struct Screen {
    ctx: egui::Context,
    size: Vec2,
    time: f64,
    textures: std::collections::HashMap<egui::TextureId, egui::ColorImage>,
    shapes: Vec<egui::epaint::ClippedShape>,
}

impl Screen {
    fn new(size: Vec2) -> Screen {
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        Screen { ctx, size, time: 10.0, textures: Default::default(), shapes: Vec::new() }
    }

    fn rect(&self) -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), self.size)
    }

    fn run(&mut self, events: Vec<Event>, mut draw: impl FnMut(&mut egui::Ui, f64)) -> Shown {
        let mut input = egui::RawInput { screen_rect: Some(self.rect()), time: Some(self.time), events, focused: true, ..Default::default() };
        input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(1.0);
        let time = self.time;
        let mut output = self.ctx.run_ui(input, |ui| draw(ui, time));
        for (id, deltas) in std::mem::take(&mut output.textures_delta.set) {
            for delta in &deltas { self.update_texture(id, delta); }
        }
        self.time += 1.0 / 60.0;
        let text = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()), shape.clip_rect)),
            _ => None,
        }).collect();
        self.shapes = output.shapes;
        Shown { text }
    }

    fn click(pos: egui::Pos2) -> [Vec<Event>; 2] {
        [true, false].map(|pressed| vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::default() }])
    }

    fn key(key: Key, modifiers: Modifiers) -> [Vec<Event>; 2] {
        [true, false].map(|pressed| vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }])
    }

    fn update_texture(&mut self, id: egui::TextureId, delta: &egui::epaint::ImageDelta) {
        let egui::ImageData::Color(patch) = &delta.image;
        let Some([x, y]) = delta.pos else {
            self.textures.insert(id, (**patch).clone());
            return;
        };
        let Some(texture) = self.textures.get_mut(&id) else { return };
        for row in 0..patch.size[1] {
            let to = (y + row) * texture.size[0] + x;
            texture.pixels[to..to + patch.size[0]].copy_from_slice(&patch.pixels[row * patch.size[0]..(row + 1) * patch.size[0]]);
        }
    }

    /// The last frame, rasterized to `target/lane-evidence/screens` for review.
    fn snapshot(&mut self, name: &str) {
        let (width, height) = (self.size.x as usize, self.size.y as usize);
        let mut pixels = vec![[0.55, 0.55, 0.57, 1.0]; width * height];
        for clipped in self.ctx.tessellate(std::mem::take(&mut self.shapes), 1.0) {
            let egui::epaint::Primitive::Mesh(mesh) = clipped.primitive else { continue };
            let Some(texture) = self.textures.get(&mesh.texture_id) else { continue };
            let clip = clipped.clip_rect.intersect(self.rect());
            for triangle in mesh.indices.chunks_exact(3) {
                crate::ui::passwords::tests::fill_triangle(&mut pixels, width, clip, texture, [0, 1, 2].map(|i| mesh.vertices[triangle[i] as usize]));
            }
        }
        let bytes = pixels.iter().flat_map(|pixel| pixel.map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)).collect();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/lane-evidence/screens");
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::from_raw(width as u32, height as u32, bytes).unwrap().save(dir.join(format!("site-permissions-{name}-{width}x{height}.png"))).unwrap();
    }
}

/// Where the sidebar's URL pill sits in a normal window.
fn pill() -> Rect {
    Rect::from_min_size(pos2(8.0, 44.0), vec2(224.0, 34.0))
}

fn pane(size: Vec2) -> Rect {
    Rect::from_min_max(pos2(240.0, 8.0), pos2(size.x - 8.0, size.y - 8.0))
}

/// Frames until the prompt has been laid out: an egui Area draws nothing on its first.
fn prompt_settled(screen: &mut Screen, f: &mut Fixture) -> (Shown, Option<Choice>, Rect) {
    for _ in 0..2 { prompt_frame(screen, f, vec![]); }
    prompt_frame(screen, f, vec![])
}

/// One frame of the window with the prompt for "t1" showing, as the app draws it.
fn prompt_frame(screen: &mut Screen, f: &mut Fixture, events: Vec<Event>) -> (Shown, Option<Choice>, Rect) {
    let pal = palette();
    let at = card_rect(Some(pill()), pane(screen.size), screen.rect());
    let mut choice = None;
    let shown = screen.run(events, |ui, now| {
        ui.painter().rect_filled(ui.max_rect(), 0, pal.panel_hover);
        if let Some(waiting) = f.perms.question_mut("t1") { choice = prompt_card(ui.ctx(), at, waiting, now, &pal); }
    });
    (shown, choice, at)
}

fn click_prompt(screen: &mut Screen, f: &mut Fixture, label: &str) -> Option<Choice> {
    let (shown, ..) = prompt_settled(screen, f);
    let pos = shown.find(label).unwrap_or_else(|| panic!("no {label}: {:?}", shown.text.iter().map(|(t, _, _)| t).collect::<Vec<_>>())).center();
    let mut picked = None;
    for events in Screen::click(pos) { picked = picked.or(prompt_frame(screen, f, events).1); }
    picked
}

fn camera_prompt() -> Fixture {
    let mut f = Fixture::new();
    assert_eq!(f.ask("t1", 1, A, &request(1, Permission::Camera, "https://meet.example/", MEET, 1)), None);
    f
}

#[test]
fn the_prompt_names_the_site_sits_under_the_address_and_fits_the_window() {
    for size in sizes() {
        let mut screen = Screen::new(size);
        let mut f = camera_prompt();
        let (shown, choice, at) = prompt_settled(&mut screen, &mut f);
        assert_eq!(choice, None);
        for label in ["meet.example wants to use your camera", "Remember for this site", "Don't Allow", "Allow"] {
            let rect = shown.visible(label).unwrap_or_else(|| panic!("{label} at {size:?}"));
            assert!(screen.rect().contains_rect(rect) && rect.left() >= at.left() && rect.right() <= at.right() + 0.5, "{label} fits at {size:?}");
        }
        assert!((at.top() - (pill().bottom() + 6.0)).abs() < 0.5 && (at.left() - pill().left().max(10.0)).abs() < 0.5, "anchored under the address, 10 points from the window edge, at {size:?}");
        assert!(f.perms.question("t1").unwrap().remember, "Remember is on by default");
        // For review: after the fade-in and the input delay.
        screen.time += 1.0;
        prompt_settled(&mut screen, &mut f);
        screen.snapshot("prompt");
    }
}

#[test]
fn a_new_prompt_ignores_clicks_for_a_moment_then_allow_and_dont_allow_answer() {
    for size in sizes() {
        let mut screen = Screen::new(size);
        let mut f = camera_prompt();
        assert_eq!(click_prompt(&mut screen, &mut f, "Allow"), None, "too soon after it appeared");
        screen.time += crate::site_permissions::INPUT_DELAY;
        assert_eq!(click_prompt(&mut screen, &mut f, "Allow"), Some(Choice::Allow));
        let mut f = camera_prompt();
        prompt_frame(&mut screen, &mut f, vec![]);
        screen.time += crate::site_permissions::INPUT_DELAY;
        assert_eq!(click_prompt(&mut screen, &mut f, "Don't Allow"), Some(Choice::Deny));
    }
}

#[test]
fn remember_for_this_site_unticks_with_a_click_or_the_space_bar() {
    for size in sizes() {
        let mut screen = Screen::new(size);
        let mut f = camera_prompt();
        prompt_frame(&mut screen, &mut f, vec![]);
        screen.time += crate::site_permissions::INPUT_DELAY;
        click_prompt(&mut screen, &mut f, "Remember for this site");
        assert!(!f.perms.question("t1").unwrap().remember);
        // Keyboard: Tab reaches the box, Space ticks it again; Tab then Enter allows.
        for events in Screen::key(Key::Tab, Modifiers::NONE) { prompt_frame(&mut screen, &mut f, events); }
        for events in Screen::key(Key::Space, Modifiers::NONE) { prompt_frame(&mut screen, &mut f, events); }
        assert!(f.perms.question("t1").unwrap().remember, "Space toggles the focused box at {size:?}");
        let mut picked = None;
        for _ in 0..2 { for events in Screen::key(Key::Tab, Modifiers::NONE) { prompt_frame(&mut screen, &mut f, events); } }
        for events in Screen::key(Key::Enter, Modifiers::NONE) { picked = picked.or(prompt_frame(&mut screen, &mut f, events).1); }
        assert_eq!(picked, Some(Choice::Allow), "Tab, Tab, Enter reaches Allow at {size:?}");
    }
}

#[test]
fn escape_on_a_focused_prompt_is_dont_allow_unremembered() {
    let mut screen = Screen::new(sizes()[1]);
    let mut f = camera_prompt();
    prompt_frame(&mut screen, &mut f, vec![]);
    screen.time += crate::site_permissions::INPUT_DELAY;
    for events in Screen::key(Key::Tab, Modifiers::NONE) { prompt_frame(&mut screen, &mut f, events); }
    // The app's shortcuts take Escape after egui has dropped the focus, as with a password offer.
    let [down, _] = Screen::key(Key::Escape, Modifiers::NONE);
    let (_, choice, _) = prompt_frame(&mut screen, &mut f, down);
    assert_eq!(choice, None);
    let outcome = dismiss_focused_prompt(&screen.ctx, &mut f.perms, &f.store).expect("Escape dismissed the focused prompt");
    assert_eq!(outcome.reply.map(|reply| reply.allow), Some(false));
    assert!(f.perms.question("t1").is_none());
    assert!(rules::records(&f.store, A).unwrap().is_empty(), "nothing remembered");
    // Without focus on the prompt, Escape is not the prompt's.
    let mut f = camera_prompt();
    prompt_frame(&mut screen, &mut f, vec![]);
    assert_eq!(dismiss_focused_prompt(&screen.ctx, &mut f.perms, &f.store), None);
}

#[test]
fn an_embedded_frame_prompt_names_the_page_too() {
    let mut screen = Screen::new(sizes()[0]);
    let mut f = Fixture::new();
    f.ask("t1", 1, A, &request(1, Permission::Microphone, "https://widget.example/", "https://news.example/", 1));
    let (shown, ..) = prompt_settled(&mut screen, &mut f);
    assert!(shown.visible("widget.example wants to use your microphone").is_some());
    assert!(shown.visible("Embedded on news.example").is_some());
}

#[test]
fn a_link_prompt_offers_open_cancel_and_always_allow_off_by_default() {
    for size in sizes() {
        let mut screen = Screen::new(size);
        let mut f = Fixture::new();
        f.perms.link(&f.store, crate::site_permissions::Page { tab: "t1", serial: 1, load: 1 }, Some(A), &crossbow_engine::ExternalLink { url: "zoommtg://zoom.us/join?confno=1".into(), gesture: true, page: MEET.into(), frame: MEET.into(), load: 1 }, true, 0.0);
        let (shown, ..) = prompt_settled(&mut screen, &mut f);
        for label in ["Open Zoom?", "meet.example wants to open Zoom.", "Always allow meet.example to open links of this type", "Cancel", "Open"] {
            assert!(shown.visible(label).is_some(), "{label} at {size:?}");
        }
        assert!(!f.perms.question("t1").unwrap().remember);
        screen.time += 1.0;
        prompt_settled(&mut screen, &mut f);
        screen.snapshot("link-prompt");
        click_prompt(&mut screen, &mut f, "Always allow meet.example to open links of this type");
        assert!(f.perms.question("t1").unwrap().remember);
        assert_eq!(click_prompt(&mut screen, &mut f, "Open"), Some(Choice::Allow));
    }
}

#[test]
fn the_quiet_notice_opens_the_app_with_one_click_or_goes_away() {
    for size in sizes() {
        let mut screen = Screen::new(size);
        let mut f = Fixture::new();
        f.perms.link(&f.store, crate::site_permissions::Page { tab: "t1", serial: 1, load: 1 }, Some(A), &crossbow_engine::ExternalLink { url: "zoommtg://zoom.us/join?confno=1".into(), gesture: false, page: MEET.into(), frame: MEET.into(), load: 1 }, true, 0.0);
        let pal = palette();
        let at = card_rect(Some(pill()), pane(size), screen.rect());
        let frame = |screen: &mut Screen, f: &Fixture, events| {
            let mut picked = None;
            let shown = screen.run(events, |ui, _| {
                ui.painter().rect_filled(ui.max_rect(), 0, pal.panel_hover);
                if let Some(indicator) = f.perms.indicator("t1") { picked = indicator_card(ui.ctx(), at, "t1", indicator, &pal); }
            });
            (shown, picked)
        };
        for _ in 0..2 { frame(&mut screen, &f, vec![]); }
        let (shown, _) = frame(&mut screen, &f, vec![]);
        assert!(shown.visible("meet.example tried to open Zoom").is_some(), "{size:?}");
        let open = shown.visible("Open Zoom").unwrap();
        assert!(screen.rect().contains_rect(open));
        screen.time += 1.0;
        for _ in 0..3 { frame(&mut screen, &f, vec![]); }
        screen.snapshot("notice");
        let mut picked = None;
        for events in Screen::click(open.center()) { picked = picked.or(frame(&mut screen, &f, events).1); }
        assert_eq!(picked, Some(true), "one click opens it: it needs no input delay, nothing opened before the click");
        assert!(f.apps.opened().is_empty());
        let close = Rect::from_center_size(pos2(at.right() - 20.0, at.top() + 20.0), vec2(22.0, 22.0)).center();
        let mut picked = None;
        for events in Screen::click(close) { picked = picked.or(frame(&mut screen, &f, events).1); }
        assert_eq!(picked, Some(false), "the close button dismisses it");
    }
}

#[test]
fn the_card_stays_inside_the_window_wherever_the_address_is() {
    for size in sizes() {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), size);
        let pane = pane(size);
        // The sidebar's pill, the top strip's trailing controls on the right, and no address area.
        for anchor in [Some(pill()), Some(Rect::from_min_size(pos2(size.x - 40.0, 6.0), vec2(28.0, 28.0))), None] {
            let at = card_rect(anchor, pane, screen);
            assert!(at.left() >= 10.0 - 0.5 && at.right() <= size.x - 10.0 + 0.5, "{anchor:?} at {size:?}: {at:?}");
            assert!(at.width() <= CARD_WIDTH && at.width() >= 200.0);
            match anchor {
                Some(anchor) => assert!((at.top() - (anchor.bottom() + 6.0)).abs() < 0.5),
                None => assert!((at.top() - (pane.top() + 10.0)).abs() < 0.5),
            }
        }
        let right = card_rect(Some(Rect::from_min_size(pos2(size.x - 40.0, 6.0), vec2(28.0, 28.0))), pane, screen);
        assert!((right.right() - (size.x - 12.0)).abs() < 0.5, "a right-hand anchor right-aligns the card");
    }
}

// -- Settings → Privacy → Site Settings --------------------------------------------------------

/// Settings at one size, showing Privacy, as `settings::show` lays it out.
struct Sheet {
    screen: Screen,
    f: Fixture,
    editor: site_permission_settings::Editor,
    profile_name: String,
}

impl Sheet {
    fn new(size: Vec2) -> Sheet {
        Sheet { screen: Screen::new(size), f: Fixture::new(), editor: Default::default(), profile_name: "Fixture Work".into() }
    }

    fn frame(&mut self, events: Vec<Event>) -> Shown {
        let screen = self.screen.rect();
        let rect = sheet_rect(screen, 1.0);
        let pal = palette();
        let (editor, f, name) = (&mut self.editor, &mut self.f, self.profile_name.clone());
        self.screen.run(events, |ui, _| {
            show_backdrop(ui.ctx(), screen, 1.0);
            show_sheet(ui.ctx(), rect, |ui| {
                set_visuals(ui, &pal);
                ui.painter().rect_filled(rect, 14, pal.panel);
                let body = if compact_navigation_needed(rect.size()) {
                    Rect::from_min_max(rect.min + vec2(20.0, 60.0), rect.max - vec2(20.0, 16.0))
                } else {
                    let nav = Rect::from_min_size(rect.min, vec2(196.0, rect.height()));
                    ui.painter().rect_filled(nav, egui::CornerRadius { nw: 14, sw: 14, ne: 0, se: 0 }, pal.panel_hover.gamma_multiply(0.8));
                    Rect::from_min_max(rect.min + vec2(224.0, 18.0), rect.max - vec2(28.0, 16.0))
                };
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
                child.label(RichText::new("Privacy").size(20.0).strong().color(pal.panel_text));
                child.add_space(10.0);
                egui::ScrollArea::vertical().id_salt("site-settings-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
                    ui.set_width(body.width() - 8.0);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    site_permission_settings::show(editor, &mut f.perms, &f.store, A, &name, ui, &pal);
                    ui.add_space(16.0);
                });
            });
        })
    }

    fn settle(&mut self) -> Shown {
        for _ in 0..4 { self.frame(vec![]); }
        self.frame(vec![])
    }

    fn wheel(&mut self, delta: f32) {
        let point = sheet_rect(self.screen.rect(), 1.0).right_bottom() - vec2(60.0, 40.0);
        self.frame(vec![Event::PointerMoved(point), Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, delta), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() }]);
    }

    /// Scroll until `label` is fully in view, then return where it is.
    fn visible(&mut self, label: &str, nth: usize) -> Rect {
        for attempt in 0..40 {
            let shown = self.settle();
            let matches: Vec<_> = shown.text.iter().filter(|(text, _, _)| text == label).collect();
            match matches.get(nth) {
                Some((_, rect, clip)) if clip.expand(0.5).contains_rect(*rect) => return *rect,
                Some((_, rect, clip)) => self.wheel(if rect.center().y > clip.center().y { -80.0 } else { 80.0 }),
                None => self.wheel(if attempt < 20 { -80.0 } else { 80.0 }),
            }
        }
        let size = self.screen.size;
        let text: Vec<String> = self.settle().text.into_iter().map(|(t, _, _)| t).collect();
        panic!("not visible: {label} #{nth} at {size:?}: {text:?}");
    }

    fn appears(&mut self, label: &str) -> bool {
        for _ in 0..10 { self.wheel(1000.0); }
        for _ in 0..30 {
            if self.settle().has(label) { return true; }
            self.wheel(-100.0);
        }
        false
    }

    fn click(&mut self, label: &str, nth: usize) {
        let pos = self.visible(label, nth).center();
        for events in Screen::click(pos) { self.frame(events); }
        self.settle();
    }

    fn fits_width(&mut self) {
        let shown = self.settle();
        let body_right = sheet_rect(self.screen.rect(), 1.0).right() - 16.0;
        for (text, rect, clip) in &shown.text {
            if clip.contains_rect(*rect) { assert!(rect.right() <= body_right, "{text} overflows at {:?}", self.screen.size); }
        }
    }
}

fn remembered(f: &Fixture) {
    let set = |origin: &str, top: &str, permission, decision| rules::set_decision(&f.store, A, origin, top, permission, Some(decision), 1).unwrap();
    set("https://meet.example", "https://meet.example", Permission::Camera, Decision::Allow);
    set("https://meet.example", "https://meet.example", Permission::Microphone, Decision::Block);
    set("https://widget.example", "https://news.example", Permission::Location, Decision::Allow);
    rules::set_opens_links(&f.store, A, "https://meet.example", "zoommtg", true, 1).unwrap();
    rules::set_decision(&f.store, "fixture-profile-b", "https://meet.example", "https://meet.example", Permission::Camera, Some(Decision::Block), 1).unwrap();
}

#[test]
fn site_settings_lists_each_sites_choices_for_the_active_profile() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.visible("SITE SETTINGS", 0);
        page.visible("For the “Fixture Work” profile, used by this Space. Other profiles keep their own.", 0);
        page.visible("No site has a remembered choice yet.", 0);
        remembered(&page.f);
        for label in ["meet.example", "Camera", "Microphone", "Opens zoommtg: links without asking", "widget.example", "Location on news.example", "Reset All Site Permissions…"] {
            page.visible(label, 0);
        }
        assert!(!page.appears("No site has a remembered choice yet."));
        page.fits_width();
        // A title alone sits on its control's line, as in every Settings row.
        // Measured in one settled frame: the page scrolls with animation.
        page.visible("Camera", 0);
        for _ in 0..30 { page.frame(vec![]); }
        let shown = page.settle();
        let camera = shown.find("Camera").unwrap();
        let segment = shown.text.iter().filter(|(text, rect, _)| text == "Allow" && (rect.center().y - camera.center().y).abs() < 14.0).map(|(_, rect, _)| *rect).next().unwrap();
        assert!((segment.center().y - camera.center().y).abs() <= 1.0, "Camera at {camera:?}, its segments at {segment:?}, at {size:?}");
        let (site, reset) = (shown.find("meet.example").unwrap(), shown.find("Reset").unwrap());
        assert!((reset.center().y - site.center().y).abs() <= 1.0, "the site and its Reset share a line at {size:?}");
        page.screen.snapshot("settings");
    }
}

#[test]
fn site_settings_changes_a_choice_and_it_survives_a_reopen() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        remembered(&page.f);
        // Camera on meet.example: Allow → Block. Segments are Allow, Block, Ask in each row.
        let camera = page.visible("Camera", 0);
        let block = page.settle().text.iter().filter(|(text, rect, _)| text == "Block" && (rect.center().y - camera.center().y).abs() < 12.0).map(|(_, rect, _)| *rect).next().unwrap();
        for events in Screen::click(block.center()) { page.frame(events); }
        page.f.store = crossbow_core::store::Store::open(&page.f._dir.path().join("library.sqlite")).unwrap();
        assert_eq!(rules::decision(&page.f.store, A, "https://meet.example", "https://meet.example", Permission::Camera).unwrap(), Some(Decision::Block));
        // Ask forgets it, and the row stays until the page is left.
        let camera = page.visible("Camera", 0);
        let ask = page.settle().text.iter().filter(|(text, rect, _)| text == "Ask" && (rect.center().y - camera.center().y).abs() < 12.0).map(|(_, rect, _)| *rect).next().unwrap();
        for events in Screen::click(ask.center()) { page.frame(events); }
        assert_eq!(rules::decision(&page.f.store, A, "https://meet.example", "https://meet.example", Permission::Camera).unwrap(), None);
        page.visible("Camera", 0);
        page.editor.leave();
        assert!(!page.appears("Camera"));
        assert_eq!(rules::decision(&page.f.store, "fixture-profile-b", "https://meet.example", "https://meet.example", Permission::Camera).unwrap(), Some(Decision::Block), "another profile untouched");
    }
}

#[test]
fn site_settings_resets_a_site_removes_a_link_grant_and_resets_all_after_confirming() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        remembered(&page.f);
        page.click("Remove", 0);
        assert!(!rules::opens_links(&page.f.store, A, "https://meet.example", "zoommtg").unwrap());
        assert!(!page.appears("Opens zoommtg: links without asking"));
        page.click("Reset", 0);
        page.visible("Site permissions reset for meet.example.", 0);
        assert!(rules::records(&page.f.store, A).unwrap().iter().all(|record| record.origin != "https://meet.example"));
        page.visible("widget.example", 0);
        // Reset All asks first; Cancel keeps everything.
        page.click("Reset All Site Permissions…", 0);
        page.visible("Reset every site permission for “Fixture Work”? Sites will ask again, and links to other apps will ask before opening.", 0);
        page.fits_width();
        page.screen.snapshot("reset-all");
        page.click("Cancel", 0);
        assert_eq!(rules::records(&page.f.store, A).unwrap().len(), 1);
        page.click("Reset All Site Permissions…", 0);
        page.click("Reset All", 0);
        page.visible("All site permissions reset for “Fixture Work”.", 0);
        assert!(rules::records(&page.f.store, A).unwrap().is_empty());
        assert_eq!(rules::records(&page.f.store, "fixture-profile-b").unwrap().len(), 1, "only this profile's");
    }
}

#[test]
fn a_failed_save_in_site_settings_keeps_the_old_choice_and_says_so() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        remembered(&page.f);
        page.f.sql("CREATE TRIGGER reject_update BEFORE UPDATE ON site_permissions BEGIN SELECT RAISE(ABORT, 'fixture denial'); END; CREATE TRIGGER reject_delete BEFORE DELETE ON site_permissions BEGIN SELECT RAISE(ABORT, 'fixture denial'); END;").unwrap();
        let camera = page.visible("Camera", 0);
        let block = page.settle().text.iter().filter(|(text, rect, _)| text == "Block" && (rect.center().y - camera.center().y).abs() < 12.0).map(|(_, rect, _)| *rect).next().unwrap();
        for events in Screen::click(block.center()) { page.frame(events); }
        page.visible("Couldn't save. meet.example keeps its previous setting.", 0);
        assert_eq!(rules::decision(&page.f.store, A, "https://meet.example", "https://meet.example", Permission::Camera).unwrap(), Some(Decision::Allow));
        page.click("Reset", 0);
        page.visible("Couldn't reset meet.example. Nothing changed.", 0);
        page.click("Reset All Site Permissions…", 0);
        page.click("Reset All", 0);
        page.visible("Couldn't reset site permissions. Nothing changed.", 0);
        assert_eq!(rules::records(&page.f.store, A).unwrap().len(), 4);
        page.fits_width();
    }
}

#[test]
fn the_blocked_icon_brings_its_site_into_view() {
    let mut page = Sheet::new(sizes()[0]);
    for n in 0..12 {
        let origin = format!("https://site-{n:02}.example");
        rules::set_decision(&page.f.store, A, &origin, &origin, Permission::Notifications, Some(Decision::Block), 1).unwrap();
    }
    page.f.perms.reveal = Some("https://site-11.example".into());
    // Settings scrolls with animation.
    for _ in 0..40 { page.frame(vec![]); }
    let shown = page.settle();
    assert!(shown.visible("site-11.example").is_some(), "{:?}", shown.text.iter().map(|(t, _, _)| t).collect::<Vec<_>>());
    assert!(page.f.perms.reveal.is_none());
}


