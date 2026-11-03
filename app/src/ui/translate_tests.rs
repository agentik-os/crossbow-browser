//! The translation card driven by simulated pointer and keyboard input at
//! 640×420 and 1360×860, over the real translator with a fake page and the
//! fake provider. Every page and answer is a synthetic fixture.

use super::*;
use crate::translate::tests::{FakePage, TAB, URL, World};
use crate::ai_features::tests::Script;
use crossbow_engine::TranslateCommand;
use egui::{Event, Modifiers, PointerButton, Pos2, Vec2, pos2};

const SIZES: [Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];

struct Drawn {
    text: Vec<(String, Rect, Rect)>,
}

impl Drawn {
    fn shows(&self, text: &str) -> bool { self.text.iter().any(|(shown, ..)| shown == text) }

    fn bounds(&self, text: &str) -> Rect {
        self.text.iter().find(|(shown, ..)| shown == text).unwrap_or_else(|| panic!("{text:?} is not drawn: {:?}", self.text.iter().map(|(t, ..)| t).collect::<Vec<_>>())).1
    }
}

/// A window with one pane beside the sidebar, the translator and its page.
struct Screen {
    ctx: egui::Context,
    size: Vec2,
    w: World,
    page: FakePage,
    frame: u32,
    page_presses: Vec<Pos2>,
    /// Saved passwords and addresses, whose offers draw where the card does.
    vault: Option<crate::passwords::tests::Harness>,
    /// Site permissions for `offered`, made with the vault.
    perms: Option<crate::site_permissions::tests::Fixture>,
}

impl Screen {
    fn new(size: Vec2, w: World, page: FakePage) -> Screen {
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        Screen { ctx, size, w, page, frame: 0, page_presses: Vec::new(), vault: None, perms: None }
    }

    /// A French page offering translation.
    fn offering(size: Vec2, url: &str) -> Screen {
        let mut w = World::new();
        w.turn_on();
        let page = w.page(url);
        w.tick(&page);
        w.probe(&page, "fr", "");
        page.take();
        Screen::new(size, w, page)
    }

    fn pane(&self) -> Rect {
        Rect::from_min_max(pos2(248.0, 8.0), pos2(self.size.x - 8.0, self.size.y - 8.0))
    }

    /// One frame: Escape as the app's shortcuts take it, an address offer as
    /// `logins::offers` draws it, then the card as `cards` draws it, with a
    /// choice applied as the app applies it.
    fn frame(&mut self, events: Vec<Event>) -> Drawn {
        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
        let at = crate::ui::logins::offer_rect(self.pane(), false);
        self.frame += 1;
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)), time: Some(f64::from(self.frame) / 60.0), events, ..Default::default() };
        let pane = self.pane();
        let mut chosen = None;
        let shown = self.w.translator.card(TAB);
        let page_presses = &mut self.page_presses;
        let vault = &mut self.vault;
        let perms = vault.as_ref().map(|_| &self.perms.get_or_insert_with(crate::site_permissions::tests::Fixture::new).perms);
        let mut output = self.ctx.run_ui(input, |ui| {
            let ctx = ui.ctx().clone();
            let page = ui.interact(pane, Id::unique("fixture-page"), egui::Sense::click_and_drag());
            let target = crate::ui::content::WebInput { page: &page, popup: None };
            for event in target.events(&ctx, false) {
                if let Event::PointerButton { pos, pressed: true, .. } = event { page_presses.push(pos); }
            }
            if crate::app::take_shortcut(&ctx, Modifiers::NONE, egui::Key::Escape) && dismiss_focused_card(&ctx).as_deref() == Some(TAB) {
                chosen = Some(Choice::Close);
            }
            let mut waits = false;
            if let Some(h) = vault {
                if let Some(offer) = h.passwords.address_offer_mut(TAB) {
                    if let Some(request) = crate::ui::logins::address_offer_card(&ctx, at, offer, &pal) {
                        crate::ui::logins::apply_address_offer(&mut h.passwords, &ctx, TAB, request);
                    }
                }
                waits = offered(&h.passwords, perms.unwrap(), TAB, at, None);
            }
            if let Some(shown) = shown.as_ref().filter(|_| !waits) {
                if let Some(choice) = card(&ctx, at, TAB, shown, &pal) { chosen = Some(choice); }
            }
        });
        output.textures_delta.clear();
        if let Some(choice) = chosen { self.w.choose(&self.page, choice); }
        let text = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()), shape.clip_rect)),
            _ => None,
        }).collect();
        Drawn { text }
    }

    fn settle(&mut self) -> Drawn {
        for _ in 0..3 { self.frame(Vec::new()); }
        self.frame(Vec::new())
    }

    /// Let the input delay pass, as a person reading the card would.
    fn wait(&mut self) -> Drawn {
        for _ in 0..(INPUT_DELAY * 60.0).ceil() as u32 + 2 { self.frame(Vec::new()); }
        self.frame(Vec::new())
    }

    fn press(&mut self, pos: Pos2) -> Drawn {
        self.frame(vec![Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
        }
        self.settle()
    }

    fn click(&mut self, label: &str) -> Drawn {
        let pos = self.wait().bounds(label).center();
        self.press(pos)
    }

    fn key(&mut self, key: egui::Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }]);
        }
    }

    /// Press Tab until a button holding `at` has focus, not the page under
    /// the card; false if none does.
    fn tab_to(&mut self, at: Pos2) -> bool {
        for _ in 0..12 {
            self.key(egui::Key::Tab);
            let focused = self.ctx.memory(|memory| memory.focused()).and_then(|id| self.ctx.read_response(id)).map(|response| response.rect);
            if focused.is_some_and(|rect| rect.contains(at) && rect != self.pane()) { return true; }
        }
        false
    }

    fn card_rect(&self) -> Option<Rect> {
        self.ctx.memory(|memory| memory.area_rect(Id::unique(("translate-card", TAB))))
    }

    /// Every label is whole inside its clip and the card and clear of the
    /// others; the card keeps to the pane's top right, where the password
    /// offer sits.
    fn check_layout(&self, drawn: &Drawn) {
        let card = self.card_rect().expect("the card is shown");
        let pane = self.pane();
        assert!(pane.contains_rect(card), "the card {card:?} stays in the pane {pane:?} at {:?}", self.size);
        let at = crate::ui::logins::offer_rect(pane, false);
        assert!((card.right() - at.right()).abs() < 0.5 && (card.top() - at.top()).abs() < 0.5 && (card.width() - at.width()).abs() < 0.5, "the card {card:?} keeps its place {at:?} at {:?}", self.size);
        let labels: Vec<_> = drawn.text.iter().filter(|(text, ..)| !text.is_empty()).collect();
        for (text, bounds, clip) in &labels {
            // Progress titles end in an ellipsis of their own; any other was cut.
            let whole = !text.ends_with('…') || text.starts_with("Translating") || text == "Showing the original…";
            assert!(clip.expand(0.5).contains_rect(*bounds) && card.expand(0.5).contains_rect(*bounds) && whole, "{text:?} is clipped at {:?}: {bounds:?} in {clip:?}, card {card:?}", self.size);
        }
        for (i, (text, bounds, _)) in labels.iter().enumerate() {
            for (other, other_bounds, _) in &labels[i + 1..] {
                assert!(!bounds.shrink(0.5).intersects(other_bounds.shrink(0.5)), "{text:?} overlaps {other:?} at {:?}", self.size);
            }
        }
    }
}

#[test]
fn the_card_fits_and_keeps_its_place_in_every_state_at_both_sizes() {
    let long = "https://a-very-long-subdomain-for-layout.journal.example/";
    let texts: Vec<String> = (1..=45).map(|i| format!("Ligne {i}")).collect();
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    for size in SIZES {
        // The offer, with a short and a long site.
        let mut s = Screen::offering(size, URL);
        let drawn = s.settle();
        for label in ["Translate this page from French?", "Translate", "Never for journal.example", "Never for French", "Close"] {
            assert!(drawn.shows(label), "{label:?} at {size:?}");
        }
        s.check_layout(&drawn);
        let mut s = Screen::offering(size, long);
        let drawn = s.settle();
        assert!(drawn.shows("Never for This Site"), "a long host is named by its place at {size:?}");
        s.check_layout(&drawn);

        // A failed save shows its error in the card.
        let mut s = Screen::offering(size, URL);
        let conn = s.w.reject_choices("INSERT");
        let drawn = s.click("Never for French");
        assert!(drawn.shows("Could not save this choice. Nothing changed. Check disk space and permissions, then try again."), "at {size:?}");
        s.check_layout(&drawn);
        drop(conn);

        // Progress, the result with its notes, and a stopped translation.
        let mut w = World::new();
        w.fake.then(Script::Hold);
        let page = w.translating(&texts);
        let mut s = Screen::new(size, w, page);
        let drawn = s.settle();
        assert!(drawn.shows("Translating from French…") && drawn.shows("0 of 2 parts") && drawn.shows("Show Original"), "at {size:?}");
        s.check_layout(&drawn);
        s.w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":41,"text":"Line 41"}]}"#.into())));
        s.w.fake.task().finish(Ok(r#"{"translations":[{"id":1,"text":"Line 1"}]}"#.into()));
        s.w.tick(&s.page);
        s.w.confirm(&s.page);
        let drawn = s.settle();
        assert!(drawn.shows("Translated from French") && drawn.shows("43 passages were left untranslated.") && drawn.shows("Always Translate French"), "at {size:?}");
        s.check_layout(&drawn);
        // Asked again: what the page added is read while the translation,
        // and Show Original, stay.
        s.w.translator.translate(TAB, &s.page, &s.w.lib, s.w.now).unwrap();
        assert_eq!(s.page.take(), [TranslateCommand::Collect], "at {size:?}");
        let drawn = s.settle();
        assert!(drawn.shows("Translating from French…") && drawn.shows("Show Original") && !drawn.shows("0 of 2 parts"), "at {size:?}");
        s.check_layout(&drawn);
        s.click("Show Original");
        assert_eq!(s.page.take(), [TranslateCommand::Restore { pass: 4 }], "Show Original ends the whole translation at {size:?}");

        let mut w = World::new();
        w.fake.then(Script::Answer(Err(crossbow_ai::local::LocalError::Unavailable)));
        let page = w.translating(&texts);
        let mut s = Screen::new(size, w, page);
        let drawn = s.settle();
        assert!(drawn.shows("Translation stopped") && drawn.shows("Retry"), "at {size:?}");
        s.check_layout(&drawn);

        // Translate on a page that opts out says why, and Close dismisses it.
        let mut s = Screen::offering(size, URL);
        s.click("Translate");
        assert_eq!(s.page.take(), [TranslateCommand::Collect], "at {size:?}");
        s.w.report(&s.page, crossbow_engine::TranslateReport::Declined { page: 9 });
        let drawn = s.settle();
        assert!(drawn.shows("This page asks not to be translated.") && drawn.shows("Close") && !drawn.shows("Translate"), "at {size:?}");
        s.check_layout(&drawn);
        let drawn = s.click("Close");
        assert!(!drawn.shows("This page asks not to be translated.") && s.page.take().is_empty(), "at {size:?}");
    }
}

#[test]
fn an_address_offer_on_the_page_goes_before_the_card_at_both_sizes() {
    const PROFILE: &str = "fixture-profile-a";
    const PARTS: [&str; 9] = ["Ada Fixture", "", "1 Fixture Street", "Testville", "", "75001", "FR", "", ""];
    for size in SIZES {
        // Both pending on one page: the Translate offer and an address offer.
        let mut s = Screen::offering(size, URL);
        let mut h = crate::passwords::tests::Harness::new();
        h.passwords.set_profiles(PROFILE, [(PROFILE.to_owned(), "Personal".to_owned())]);
        h.passwords.set_offer_to_save(PROFILE, true);
        h.passwords.status().unwrap();
        h.passwords.unlock();
        h.passwords.offer_address(TAB, PROFILE, "https://journal.example", PARTS);
        assert!(h.passwords.address_offer(TAB).is_some() && s.w.translator.card(TAB).is_some(), "both pending at {size:?}");
        s.vault = Some(h);
        let drawn = s.settle();
        assert!(drawn.shows("Save address from journal.example?") && drawn.shows("Not Now"), "the address offer draws at {size:?}");
        assert!(!drawn.shows("Translate this page from French?") && !drawn.shows("Never for French") && s.card_rect().is_none(), "the card doesn't draw over it at {size:?}");
        // Not Now: the address offer goes and the card takes its place.
        let drawn = s.click("Not Now");
        assert!(s.vault.as_ref().unwrap().passwords.address_offer(TAB).is_none(), "at {size:?}");
        assert!(drawn.shows("Translate this page from French?") && !drawn.shows("Save address from journal.example?"), "at {size:?}");
        s.check_layout(&drawn);
    }
}

#[test]
fn buttons_wait_for_the_input_delay_then_choose_through_simulated_clicks() {
    for size in SIZES {
        // A click as the card appears belongs to the page, not to Translate.
        let mut s = Screen::offering(size, URL);
        // An area is measured on its first frame and shown on the next.
        s.frame(Vec::new());
        let first = s.frame(Vec::new());
        let at = first.bounds("Translate").center();
        s.press(at);
        assert!(s.page.take().is_empty(), "too soon: nothing is collected at {size:?}");
        assert!(s.w.title().is_some(), "the offer stays");
        s.click("Translate");
        assert_eq!(s.page.take(), [TranslateCommand::Collect], "Translate asks the page for its text at {size:?}");
        assert!(s.page_presses.iter().all(|press| !s.card_rect().unwrap().contains(*press)), "presses on the card never reach the page at {size:?}");

        // Never for this site and Close.
        let mut s = Screen::offering(size, URL);
        s.click("Never for journal.example");
        assert!(s.card_rect().is_none() || !s.settle().shows("Translate this page from French?"), "the offer goes at {size:?}");
        assert!(crate::translate::choices(&s.w.lib, &s.w.profile).unwrap().never_sites.contains("journal.example"));
        let mut s = Screen::offering(size, URL);
        let drawn = s.click("Close");
        assert!(!drawn.shows("Translate this page from French?") && s.page.take().is_empty(), "Close sends nothing at {size:?}");
        assert_eq!(crate::translate::choices(&s.w.lib, &s.w.profile).unwrap().count(), 0, "Close saves nothing at {size:?}");

        // Always and Show Original on the result; Retry on a stop.
        let mut w = World::new();
        w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Hello"}]}"#.into())));
        let page = w.translating(&["Bonjour"]);
        let mut s = Screen::new(size, w, page);
        s.w.confirm(&s.page);
        let drawn = s.click("Always Translate French");
        assert!(!drawn.shows("Always Translate French") && drawn.shows("Translated from French"), "said once at {size:?}");
        assert!(crate::translate::choices(&s.w.lib, &s.w.profile).unwrap().always_languages.contains("fr"));
        let drawn = s.click("Show Original");
        assert!(drawn.shows("Showing the original…") && !drawn.shows("Translated from French"), "until the page confirms at {size:?}");
        s.check_layout(&drawn);
        assert_eq!(s.w.confirm(&s.page), [TranslateCommand::Restore { pass: 4 }]);
        let drawn = s.settle();
        assert!(!drawn.shows("Showing the original…") && s.card_rect().is_none_or(|_| drawn.text.is_empty()), "the card goes with the translation at {size:?}");

        let mut w = World::new();
        w.fake.then(Script::Answer(Err(crossbow_ai::local::LocalError::Unavailable)));
        let page = w.translating(&["Bonjour"]);
        let mut s = Screen::new(size, w, page);
        s.w.fake.then(Script::Answer(Ok(r#"{"translations":[{"id":1,"text":"Hello"}]}"#.into())));
        s.click("Retry");
        assert!(matches!(s.w.confirm(&s.page).as_slice(), [TranslateCommand::Apply { texts, .. }] if texts == &[(1, "Hello".to_string())]));
        assert!(s.settle().shows("Translated from French"), "Retry finishes once the page confirms at {size:?}");
    }
}

#[test]
fn the_keyboard_reaches_the_buttons_and_escape_closes_the_card() {
    for size in SIZES {
        let mut s = Screen::offering(size, URL);
        s.wait();
        let translate = s.frame(Vec::new()).bounds("Translate").center();
        assert!(s.tab_to(translate), "Tab reaches Translate at {size:?}");
        // Space on it translates; the next card's Escape closes it.
        s.key(egui::Key::Space);
        s.settle();
        assert_eq!(s.page.take(), [TranslateCommand::Collect], "Space chooses Translate at {size:?}");

        let mut s = Screen::offering(size, URL);
        let close = s.wait().bounds("Close").center();
        assert!(s.tab_to(close), "Tab reaches Close at {size:?}");
        s.key(egui::Key::Escape);
        let drawn = s.settle();
        assert!(!drawn.shows("Translate this page from French?"), "Escape closes the card at {size:?}");
        assert!(s.page.take().is_empty() && crate::translate::choices(&s.w.lib, &s.w.profile).unwrap().count() == 0, "and changes nothing at {size:?}");
        // The page's own Escape is untouched without focus on the card.
        let mut s = Screen::offering(size, URL);
        s.wait();
        s.key(egui::Key::Escape);
        assert!(s.settle().shows("Translate this page from French?"), "an Escape meant for the page leaves the card at {size:?}");
    }
}

/// Two panes side by side below a top strip, as `content` lays out split
/// view: with the sidebar hidden, the strip's trailing control anchors the
/// prompt; with it shown, the sidebar's pill does.
fn split(size: Vec2, sidebar: bool) -> (Rect, Rect, Rect) {
    let strip = Rect::from_min_max(pos2(0.0, 0.0), pos2(size.x, crate::ui::window::NATIVE_CONTROL_ROW));
    let (left_edge, anchor) = if sidebar {
        (248.0, Rect::from_min_size(pos2(8.0, 44.0), vec2(224.0, 34.0)))
    } else {
        (8.0, crate::ui::window::ControlRow::new(strip, true, strip.height()).trailing_control(0))
    };
    let content = Rect::from_min_max(pos2(left_edge, if sidebar { 8.0 } else { strip.bottom() }), pos2(size.x - 8.0, size.y - 8.0));
    let half = (content.width() - 8.0) / 2.0;
    let left = Rect::from_min_size(content.min, vec2(half, content.height()));
    let right = Rect::from_min_size(pos2(left.right() + 8.0, content.top()), vec2(half, content.height()));
    (anchor, left, right)
}

/// AI-16 (second AI review): in split view with the sidebar hidden, the
/// focused left page's camera prompt hangs from the top strip's right end,
/// over the right pane, where that pane's Translate card draws. The card
/// waits while the prompt shows and comes back once it is answered; with
/// the sidebar shown the prompt sits under the pill and the card stays.
/// Both drawn with the real `prompt_card` and `card`, at 640×420 and
/// 1360×860; the panes are laid out as `content` lays them out, not taken
/// from a running window.
#[test]
fn another_panes_permission_prompt_over_the_card_holds_it_back_until_answered() {
    use crate::site_permissions::tests::{A, Fixture, MEET, request};
    use crossbow_core::site_permissions::Permission;
    const LEFT: &str = "fixture-tab-left";
    for sidebar in [false, true] {
        for size in SIZES {
            let screen = Rect::from_min_size(Pos2::ZERO, size);
            let (anchor, left, right) = split(size, sidebar);
            let prompt_at = crate::ui::site_permissions::card_rect(Some(anchor), left, screen);
            let translate_at = crate::ui::logins::offer_rect(right, false);
            let s = Screen::offering(size, URL);
            let shown = s.w.translator.card(TAB).expect("the right page offers translation");
            let h = crate::passwords::tests::Harness::new();
            let mut f = Fixture::new();
            assert_eq!(f.ask(LEFT, 1, A, &request(1, Permission::Camera, "https://meet.example/", MEET, 1)), None, "the left page asks");
            let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
            let ctx = egui::Context::default();
            crate::ui::window::install_fonts(&ctx);
            // Frames as `overlays` draws them: prompts, then cards.
            let mut frames = 0;
            let mut run = |f: &mut Fixture| {
                let mut drawn_card = false;
                for _ in 0..4 {
                    frames += 1;
                    let now = f64::from(frames) / 60.0;
                    let input = egui::RawInput { screen_rect: Some(screen), time: Some(now), ..Default::default() };
                    let mut output = ctx.run_ui(input, |ui| {
                        let ctx = ui.ctx().clone();
                        if let Some(waiting) = f.perms.question_mut(LEFT) { crate::ui::site_permissions::prompt_card(&ctx, prompt_at, waiting, now, &pal); }
                        let prompt = f.perms.question(LEFT).map(|_| crate::ui::site_permissions::prompt_column(prompt_at, screen));
                        drawn_card = !offered(&h.passwords, &f.perms, TAB, translate_at, prompt);
                        if drawn_card { card(&ctx, translate_at, TAB, &shown, &pal); }
                    });
                    output.textures_delta.clear();
                }
                drawn_card
            };
            let drawn = run(&mut f);
            let prompt = ctx.memory(|memory| memory.area_rect(Id::unique(("site-permission-prompt", LEFT)))).expect("the prompt draws");
            let card_rect = ctx.memory(|memory| memory.area_rect(Id::unique(("translate-card", TAB))));
            if sidebar {
                let card_rect = card_rect.expect("the card draws beside the prompt");
                assert!(drawn && !prompt.intersects(card_rect), "sidebar shown at {size:?}: prompt {prompt:?}, card {card_rect:?}");
            } else {
                assert!(!drawn && card_rect.is_none(), "sidebar hidden at {size:?}: the card {card_rect:?} waits under the prompt {prompt:?}");
                assert!(prompt.intersects(translate_at), "the prompt {prompt:?} covers the card's place {translate_at:?} at {size:?}");
                // Don't Allow: the prompt goes and the card comes back.
                f.perms.choose(&f.store, LEFT, crate::site_permissions::Choice::Deny, crossbow_core::now_ms());
                let before = ctx.input(|input| input.time);
                assert!(run(&mut f), "the card returns once the prompt is answered at {size:?}");
                // Back, it takes no clicks for a moment, as after any change.
                let armed: Armed = ctx.data(|data| data.get_temp(Id::unique(("translate-card-armed", TAB)))).expect("armed");
                assert!(armed.since > before, "the returning card arms again at {size:?}");
                let card_rect = ctx.memory(|memory| memory.area_rect(Id::unique(("translate-card", TAB)))).expect("the card draws");
                assert!(right.contains_rect(card_rect), "the card {card_rect:?} keeps to its pane {right:?} at {size:?}");
            }
        }
    }
}

#[test]
fn the_app_draws_cards_for_tabs_and_routes_their_choices() {
    let overlays = include_str!("overlays.rs");
    assert!(overlays.contains("    crate::ui::site_permissions::prompts(app, ui, &pal);\n    crate::ui::translate::cards(app, ui, &pal);"));
    // The rule the both-pending and split-view tests draw by is the one
    // `cards` yields by, and the prompt's place is the one `prompts` draws at.
    let translate = include_str!("translate.rs");
    assert!(translate.contains("    let prompt = crate::ui::site_permissions::shown_column(app, ctx.content_rect());"));
    assert!(translate.contains("        if offered(&app.passwords, &app.site_permissions, &tab, at, prompt) {\n"));
    // Offers yield by the same rule, at the same column (AI-19).
    let logins = include_str!("logins.rs");
    assert!(logins.contains("    let prompt = crate::ui::site_permissions::shown_column(app, ctx.content_rect());"));
    assert!(logins.contains("        let Some(notice) = pane_offer(&ctx, &mut app.passwords, &app.site_permissions, &tab, at, prompt, pal) else { continue };"));
    assert!(include_str!("site_permissions.rs").contains("    let Some((tab, at)) = prompt_place(app, ctx.content_rect()) else { return };"));
    let app = include_str!("../app.rs");
    for wired in [
        "Action::Translate(tab, choice) => self.translate(&tab, choice, ctx),",
        "} else if let Some(tab) = ui::translate::dismiss_focused_card(ctx) {",
        "self.update_translations(&ctx);",
    ] {
        assert!(app.contains(wired), "{wired}");
    }
}

/// AI-19 (third AI review), its probe kept as a test: split view at both
/// sizes, the sidebar shown and hidden, either pane focused (24
/// placements). The focused tab's camera prompt, and a Translate card, a
/// login offer or an address offer on the other pane or on the prompt's
/// own page, appearing before or after the prompt. Each is drawn by its
/// real function in `overlays` order (offers, prompts, cards): no card
/// ever shares a pixel with the prompt, and one waits only when the
/// prompt is on its page or over its place.
#[test]
fn no_card_or_offer_shares_a_place_with_a_permission_prompt() {
    use crate::site_permissions::tests::{A, Fixture, MEET, request};
    use crossbow_core::site_permissions::Permission;
    const PROFILE: &str = "fixture-profile-a";
    const PARTS: [&str; 9] = ["Ada Fixture", "", "1 Fixture Street", "Testville", "", "75001", "FR", "", ""];
    const PROMPT_TAB: &str = "fixture-tab-prompt";
    let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
    let (mut placements, mut cases, mut waited) = (0, 0, 0);
    for sidebar in [false, true] {
        for size in SIZES {
            let screen = Rect::from_min_size(Pos2::ZERO, size);
            let (anchor, left, right) = split(size, sidebar);
            for (focused_name, focused, other) in [("left", left, right), ("right", right, left)] {
                let prompt_at = crate::ui::site_permissions::card_rect(Some(anchor), focused, screen);
                for kind in ["translate", "login", "address"] {
                    placements += 1;
                    for (where_, pane, same) in [("other pane", other, false), ("same page", focused, true)] {
                        for prompt_first in [false, true] {
                            cases += 1;
                            let case = format!("sidebar {}, {size:?}, {focused_name} focused, {kind} on the {where_}, {} first", if sidebar { "shown" } else { "hidden" }, if prompt_first { "prompt" } else { kind });
                            let card_tab = if same { PROMPT_TAB } else { TAB };
                            let at = crate::ui::logins::offer_rect(pane, false);
                            let s = Screen::offering(size, URL);
                            let shown = s.w.translator.card(TAB).expect("an offer to translate");
                            let mut h = crate::passwords::tests::Harness::new();
                            h.passwords.set_profiles(PROFILE, [(PROFILE.to_owned(), "Personal".to_owned())]);
                            h.passwords.set_offer_to_save(PROFILE, true);
                            h.passwords.status().unwrap();
                            h.passwords.unlock();
                            match kind {
                                "login" => h.passwords.offer_login(card_tab, PROFILE, "https://meet.example", "ada@example.com", "fixture-password"),
                                "address" => h.passwords.offer_address(card_tab, PROFILE, "https://meet.example", PARTS),
                                _ => {}
                            }
                            let mut f = Fixture::new();
                            let ctx = egui::Context::default();
                            crate::ui::window::install_fonts(&ctx);
                            let card_id = match kind { "login" => Id::unique(("login-offer", card_tab)), "address" => Id::unique(("address-offer", card_tab)), _ => Id::unique(("translate-card", card_tab)) };
                            let prompt_id = Id::unique(("site-permission-prompt", PROMPT_TAB));
                            let ask = |f: &mut Fixture| assert_eq!(f.ask(PROMPT_TAB, 1, A, &request(1, Permission::Camera, "https://meet.example/", MEET, 1)), None);
                            if prompt_first { ask(&mut f); }
                            let mut compared = 0;
                            for frame in 0..10u32 {
                                if frame == 5 && !prompt_first { ask(&mut f); }
                                let card_now = !prompt_first || frame >= 5;
                                let now = f64::from(frame + 1) / 60.0;
                                let input = egui::RawInput { screen_rect: Some(screen), time: Some(now), ..Default::default() };
                                // An area's first frame is egui's sizing pass: laid out at a
                                // guessed size, kept inside the screen, and painted nowhere.
                                let new = |id: Id| ctx.memory(|memory| memory.area_rect(id)).is_none();
                                let (prompt_new, card_new) = (new(prompt_id), new(card_id));
                                let mut output = ctx.run_ui(input, |ui| {
                                    let ctx = ui.ctx().clone();
                                    // The focused tab's prompt column, as `shown_column` gives it.
                                    let column = f.perms.question(PROMPT_TAB).map(|_| crate::ui::site_permissions::prompt_column(prompt_at, screen));
                                    if card_now && kind != "translate" { crate::ui::logins::pane_offer(&ctx, &mut h.passwords, &f.perms, card_tab, at, column, &pal); }
                                    if let Some(waiting) = f.perms.question_mut(PROMPT_TAB) { crate::ui::site_permissions::prompt_card(&ctx, prompt_at, waiting, now, &pal); }
                                    if card_now && kind == "translate" && !offered(&h.passwords, &f.perms, card_tab, at, column) { card(&ctx, at, card_tab, &shown, &pal); }
                                });
                                output.textures_delta.clear();
                                let visible = |id: Id| ctx.memory(|memory| memory.areas().visible_last_frame(&egui::LayerId::new(egui::Order::Foreground, id)));
                                let (prompt_drawn, card_drawn) = (visible(prompt_id), visible(card_id));
                                if prompt_drawn && card_drawn && !prompt_new && !card_new {
                                    let prompt = ctx.memory(|memory| memory.area_rect(prompt_id)).expect("the prompt's place");
                                    let card = ctx.memory(|memory| memory.area_rect(card_id)).expect("the card's place");
                                    let shared = if prompt.intersects(card) { prompt.intersect(card).area() } else { 0.0 };
                                    assert_eq!(shared, 0.0, "{case}, frame {frame}: prompt {prompt:?}, card {card:?}");
                                    compared += 1;
                                }
                                if frame == 9 {
                                    assert!(prompt_drawn, "{case}");
                                    let covered = same || crate::ui::site_permissions::prompt_column(prompt_at, screen).intersects(at);
                                    assert_eq!(card_drawn, !covered, "{case}: a card waits only for a prompt on its page or over its place");
                                    if covered { waited += 1; } else { assert!(compared >= 4, "{case}: {compared} frames painted both"); }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!((placements, cases), (24, 96));
    assert!(waited > 48 && waited < 96, "{waited} cases waited");
}
