//! The one-line notice after a link opens in its site's profile: "Opened in
//! Work", with Undo. It sits at the bottom centre like toasts, which stack
//! above it, and stays six seconds, or longer while the pointer rests on it.

use crate::app::site_open::{NOTICE_SECONDS, SiteNotice};
use crate::app::{Action, CrossbowApp};
use crate::ui::theme::Palette;
use egui::epaint::Shadow;
use egui::{Color32, CornerRadius, FontId, Id, Rect, Sense, pos2, vec2};

const HEIGHT: f32 = 32.0;
/// Above the window's bottom edge, where toasts sit.
const BOTTOM: f32 = 28.0;
/// Between the notice and the window's sides.
const MARGIN: f32 = 16.0;
const PAD: f32 = 14.0;
const UNDO: &str = "Undo";

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Shown(Rect),
    Undo,
    Gone,
}

/// Draw `notice` for this frame.
pub fn show(ctx: &egui::Context, notice: &mut SiteNotice, pal: &Palette) -> Outcome {
    let now = ctx.input(|i| i.time);
    let since = *notice.shown.get_or_insert(now);
    if now - since >= NOTICE_SECONDS { return Outcome::Gone; }
    let screen = ctx.content_rect();
    let font = FontId::proportional(13.0);
    let undo = notice.undo.is_some().then(|| ctx.fonts_mut(|fonts| fonts.layout_no_wrap(UNDO.into(), font.clone(), Color32::WHITE)));
    let undo_width = undo.as_ref().map_or(0.0, |galley| galley.size().x + PAD * 2.0);
    let room = (screen.width() - MARGIN * 2.0 - PAD * 2.0 - undo_width).max(40.0);
    let mut job = egui::text::LayoutJob::single_section(notice.text.clone(), egui::TextFormat::simple(font, Color32::WHITE));
    job.wrap = egui::text::TextWrapping::truncate_at_width(room);
    let text = ctx.fonts_mut(|fonts| fonts.layout_job(job));
    let size = vec2(text.size().x + PAD * 2.0 + undo_width, HEIGHT);
    let rect = Rect::from_center_size(pos2(screen.center().x, screen.bottom() - BOTTOM - HEIGHT / 2.0), size);
    let bg = if pal.dark { Color32::from_rgb(0x3a, 0x3a, 0x44) } else { Color32::from_rgb(0x22, 0x22, 0x28) };
    let area = egui::Area::new(Id::unique("site-notice")).order(egui::Order::Tooltip).fixed_pos(rect.min)
        .constrain(false).fade_in(false).sense(Sense::hover())
        .show(ctx, |ui| {
            ui.set_min_size(size);
            let p = ui.painter();
            p.add(Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(60) }.as_shape(rect, CornerRadius::same(10)));
            p.rect_filled(rect, 10, bg);
            p.galley(pos2(rect.left() + PAD, rect.center().y - text.size().y / 2.0), text, Color32::WHITE);
            let galley = undo?;
            let button = Rect::from_min_max(pos2(rect.right() - undo_width, rect.top()), rect.max).shrink2(vec2(4.0, 4.0));
            let response = ui.interact(button, Id::unique("site-notice-undo"), Sense::click());
            if response.hovered() { p.rect_filled(button, 7, Color32::from_white_alpha(28)); }
            p.galley(button.center() - galley.size() / 2.0, galley, Color32::WHITE);
            Some(response.clicked())
        });
    if area.inner == Some(true) { return Outcome::Undo; }
    // Resting the pointer on it keeps it.
    if area.response.contains_pointer() { notice.shown = Some(now); }
    ctx.request_repaint_after(std::time::Duration::from_secs_f64((NOTICE_SECONDS - (now - since)).max(0.05)));
    Outcome::Shown(rect)
}

/// The app's notice this frame; Undo becomes an action. The notice's
/// rectangle, while shown.
pub fn show_app(app: &mut CrossbowApp, ctx: &egui::Context, pal: &Palette) -> Option<Rect> {
    let notice = app.site_notice.as_mut()?;
    match show(ctx, notice, pal) {
        Outcome::Shown(rect) => Some(rect),
        Outcome::Undo => {
            app.actions.push(Action::UndoSiteOpen);
            None
        }
        Outcome::Gone => {
            app.site_notice = None;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::site_open::Undo;
    use crossbow_core::model::Theme;
    use egui::{Event, Modifiers, PointerButton, Pos2, Vec2};

    const SIZES: [Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
    const STEP: f64 = 1.0 / 32.0;

    fn notice(text: &str, undo: bool) -> SiteNotice {
        SiteNotice { text: text.into(), undo: undo.then(|| Undo { tab: "t".into(), url: "https://github.com/".into(), space: "s".into() }), shown: None }
    }

    struct Frame {
        outcome: Outcome,
        texts: Vec<(String, Rect, Rect)>,
    }

    struct Probe {
        ctx: egui::Context,
        size: Vec2,
        time: f64,
    }

    impl Probe {
        fn new(size: Vec2) -> Self {
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            Probe { ctx, size, time: 0.0 }
        }

        fn frame(&mut self, notice: &mut SiteNotice, events: Vec<Event>) -> Frame {
            self.time += STEP;
            let mut outcome = Outcome::Gone;
            let pal = crate::ui::theme::palette(&Theme::preset(0), false);
            let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)), time: Some(self.time), events, ..Default::default() };
            let mut output = self.ctx.run_ui(input, |ui| outcome = show(ui.ctx(), notice, &pal));
            output.textures_delta.clear();
            let texts = output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()), shape.clip_rect)),
                _ => None,
            }).collect();
            Frame { outcome, texts }
        }

        fn click(&mut self, notice: &mut SiteNotice, pos: Pos2) -> Frame {
            self.frame(notice, vec![Event::PointerMoved(pos)]);
            self.frame(notice, vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
            self.frame(notice, vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }])
        }
    }

    #[test]
    fn the_notice_fits_the_window_and_undo_is_one_click() {
        for size in SIZES {
            for text in ["Opened in Work", "github.com opens in A Profile With A Rather Long Name, which has no Space, so it opened here."] {
                let mut probe = Probe::new(size);
                let mut shown = notice(text, true);
                probe.frame(&mut shown, vec![]);
                let frame = probe.frame(&mut shown, vec![]);
                let Outcome::Shown(rect) = frame.outcome else { panic!("shown at {size:?}") };
                let window = Rect::from_min_size(Pos2::ZERO, size);
                assert!(window.shrink(MARGIN - 0.5).contains_rect(rect), "{text:?} fits at {size:?}: {rect:?}");
                assert!((rect.center().x - size.x / 2.0).abs() < 0.5 && rect.bottom() <= size.y - BOTTOM + 0.5);
                let (_, undo, _) = frame.texts.iter().find(|(shown, ..)| shown == UNDO).expect("Undo is drawn").clone();
                for (shown, bounds, clip) in &frame.texts {
                    assert!(rect.contains_rect(*bounds) && clip.contains_rect(*bounds), "{shown:?} stays in the notice at {size:?}");
                    if shown != UNDO { assert!(bounds.right() <= undo.left() - PAD, "the text stops before Undo at {size:?}"); }
                }
                assert_eq!(probe.click(&mut shown, undo.center()).outcome, Outcome::Undo, "at {size:?}");
            }
        }
    }

    #[test]
    fn the_notice_leaves_after_six_seconds_unless_the_pointer_rests_on_it() {
        let mut probe = Probe::new(SIZES[0]);
        let mut shown = notice("Opened in Work", true);
        let Outcome::Shown(rect) = probe.frame(&mut shown, vec![]).outcome else { panic!() };
        // The pointer resting on it keeps it.
        let mut frame = probe.frame(&mut shown, vec![Event::PointerMoved(rect.left_center() + vec2(4.0, 0.0))]);
        while probe.time < NOTICE_SECONDS + 1.0 { frame = probe.frame(&mut shown, vec![]); }
        assert!(matches!(frame.outcome, Outcome::Shown(_)), "kept while the pointer rests on it");
        probe.frame(&mut shown, vec![Event::PointerMoved(pos2(5.0, 5.0))]);
        let left = probe.time;
        while probe.time < left + NOTICE_SECONDS - 0.2 { frame = probe.frame(&mut shown, vec![]); }
        assert!(matches!(frame.outcome, Outcome::Shown(_)));
        while probe.time < left + NOTICE_SECONDS + 0.1 { frame = probe.frame(&mut shown, vec![]); }
        assert_eq!(frame.outcome, Outcome::Gone);

        // Without Undo only the text shows.
        let mut probe = Probe::new(SIZES[1]);
        let mut plain = notice("github.com opens in Work, which has no Space, so it opened here.", false);
        probe.frame(&mut plain, vec![]);
        let frame = probe.frame(&mut plain, vec![]);
        assert!(!frame.texts.iter().any(|(text, ..)| text == UNDO));
    }
}
