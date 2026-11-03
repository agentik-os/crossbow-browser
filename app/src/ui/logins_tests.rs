//! The offer card and the key menu, driven by simulated pointer and keyboard
//! input at 640×420 and 1360×860 over the real controller, on a disposable
//! vault with the memory key store. Every login is a synthetic fixture.

use super::*;
use crate::passwords::tests::Harness;
use crate::ui::overlays::{menu_frame, menu_surface};
use crossbow_core::vault::{Address, EntryInput, Zeroizing};
use egui::{Event, Modifiers, PointerButton, Pos2, Vec2};

const SIZES: [Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];
const SITE: &str = "https://login.example";
const A: &str = "fixture-profile-a";
const B: &str = "fixture-profile-b";
const TAB: &str = "tab-1";
const USER: &str = "fixture-user";
const PASSWORD: &str = "fixture-page-password-1";

/// What one frame drew: every text with its bounds and clip.
struct Drawn {
    text: Vec<(String, Rect, Rect)>,
}

impl Drawn {
    fn shows(&self, text: &str) -> bool {
        self.text.iter().any(|(shown, ..)| shown == text)
    }

    fn bounds(&self, text: &str) -> Rect {
        self.text.iter().find(|(shown, ..)| shown == text).unwrap_or_else(|| panic!("{text:?} is not drawn: {:?}", self.labels())).1
    }

    fn labels(&self) -> Vec<&str> {
        self.text.iter().map(|(text, ..)| text.as_str()).collect()
    }
}

/// A window with one pane beside the sidebar, the controller, and the
/// requests and notices the card produced.
struct Page {
    ctx: egui::Context,
    size: Vec2,
    h: Harness,
    frame: u32,
    notices: Vec<&'static str>,
    filled: Vec<(String, String)>,
    menu_open: bool,
    outcome: Option<MenuOutcome>,
    /// Whether an address field on the page had focus.
    address_field: bool,
    /// Whether the address menu is open, as "Fill Address ›" opens it.
    address_menu_open: bool,
    filled_addresses: Vec<Vec<String>>,
    /// Whether a card field on the page had focus.
    card_field: bool,
    /// Whether the card menu is open, as "Fill Card ›" opens it.
    card_menu_open: bool,
    filled_cards: Vec<Vec<String>>,
    /// Pointer presses the page beneath took, as the content pane forwards them.
    page_presses: Vec<Pos2>,
    /// Whether the Find bar is open on the pane, which moves the card below it.
    below_find: bool,
    /// A text field of Crossbow's own beside the pane, as the sidebar has, when set.
    sidebar_field: Option<String>,
    /// Site permissions, whose prompt on this page goes before the card.
    perms: crate::site_permissions::tests::Fixture,
}

impl Page {
    fn new(size: Vec2, unlocked: bool) -> Page {
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        let mut h = Harness::new();
        h.passwords.set_profiles(A, [(A.to_owned(), "Personal".to_owned()), (B.to_owned(), "Work".to_owned())]);
        for profile in [A, B] { h.passwords.set_offer_to_save(profile, true); }
        h.passwords.status().unwrap();
        h.passwords.unlock();
        if !unlocked { h.passwords.lock(None); }
        Page {
            ctx, size, h, frame: 0, notices: Vec::new(), filled: Vec::new(), menu_open: false, outcome: None, page_presses: Vec::new(),
            address_field: false, address_menu_open: false, filled_addresses: Vec::new(), card_field: false, card_menu_open: false, filled_cards: Vec::new(),
            below_find: false, sidebar_field: None, perms: crate::site_permissions::tests::Fixture::new(),
        }
    }

    fn pane(&self) -> Rect {
        Rect::from_min_max(pos2(248.0, 8.0), pos2(self.size.x - 8.0, self.size.y - 8.0))
    }

    fn add(&mut self, origin: &str, username: &str, password: &str, profile: Option<&str>) -> String {
        self.h.passwords.vault_mut().unwrap().add(&EntryInput { origin, username, password, notes: "", profile }).unwrap()
    }

    /// One frame: the controller's tick, Escape as the app's shortcuts take it,
    /// the card as `offers` draws it (`pane_offer`), and the key or address menu while open,
    /// with picks applied as the app applies them.
    fn frame(&mut self, events: Vec<Event>) -> Drawn {
        self.h.passwords.tick_at(None, false);
        let pal = crate::ui::theme::palette(&crossbow_core::model::Theme::preset(0), false);
        let at = offer_rect(self.pane(), self.below_find);
        self.frame += 1;
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, self.size)), time: Some(f64::from(self.frame) / 60.0), events, ..Default::default() };
        let pane = self.pane();
        let page_presses = &mut self.page_presses;
        let (passwords, notices, filled, menu_open, outcome) = (&mut self.h.passwords, &mut self.notices, &mut self.filled, self.menu_open, &mut self.outcome);
        let (address_field, address_menu_open, filled_addresses) = (self.address_field, self.address_menu_open, &mut self.filled_addresses);
        let (card_field, card_menu_open, filled_cards) = (self.card_field, self.card_menu_open, &mut self.filled_cards);
        let sidebar_field = &mut self.sidebar_field;
        let perms = &self.perms.perms;
        let mut escaped = false;
        let mut output = self.ctx.run_ui(input, |ui| {
            let ctx = ui.ctx().clone();
            // The page, drawn first, takes only what lands on it.
            let page = ui.interact(pane, Id::unique("fixture-page"), egui::Sense::click_and_drag());
            let target = crate::ui::content::WebInput { page: &page, popup: None };
            for event in target.events(&ctx, false) {
                if let Event::PointerButton { pos, pressed: true, .. } = event { page_presses.push(pos); }
            }
            let ctx = ui.ctx().clone();
            // The app's shortcuts take Escape before overlays draw, in the app's
            // order: an open menu closes first, as the app's Close Overlay
            // closes any menu, then an offer.
            if (menu_open || address_menu_open || card_menu_open) && crate::app::take_dismiss_shortcut(&ctx) {
                escaped = true;
            } else if crate::app::take_shortcut(&ctx, Modifiers::NONE, egui::Key::Escape) && !dismiss_focused_offer(&ctx, passwords) {
                dismiss_shown_address_offer(&ctx, Some(TAB).filter(|tab| !waits(perms, tab, at, None)), passwords);
            }
            if let Some(text) = sidebar_field {
                ui.put(Rect::from_min_size(pos2(8.0, 44.0), vec2(232.0, 28.0)), egui::TextEdit::singleline(text).id(Id::unique("fixture-sidebar-field")));
            }
            if let Some(notice) = pane_offer(&ctx, passwords, perms, TAB, at, None, &pal) { notices.extend(notice); }
            if menu_open {
                let m = MenuUi::new(&pal, false);
                let mut pick = None;
                menu_surface(pos2(at.left(), at.top())).show(&ctx, |ui| {
                    menu_frame(&pal).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        pick = menu_entries(&m, ui, passwords, Some(SITE), A, address_field, card_field);
                    });
                });
                if let Some(pick) = pick {
                    *outcome = Some(apply_pick(passwords, pick, SITE, A, |username, password| {
                        filled.push((username.to_owned(), password.to_owned()));
                        true
                    }));
                }
            }
            if address_menu_open {
                let m = MenuUi::new(&pal, false);
                let mut pick = None;
                menu_surface(pos2(at.left(), at.top())).show(&ctx, |ui| {
                    menu_frame(&pal).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        pick = address_entries(&m, ui, passwords, SITE, A);
                    });
                });
                if let Some(pick) = pick {
                    *outcome = Some(apply_address_pick(passwords, pick, A, |parts| {
                        filled_addresses.push(parts.map(str::to_owned).to_vec());
                        true
                    }));
                }
            }
            if card_menu_open && !escaped {
                let m = MenuUi::new(&pal, false);
                let mut pick = None;
                menu_surface(pos2(at.left(), at.top())).show(&ctx, |ui| {
                    menu_frame(&pal).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        pick = card_entries(&m, ui, passwords, SITE, A);
                    });
                });
                if let Some(pick) = pick {
                    *outcome = Some(apply_card_pick(passwords, pick, A, |parts| {
                        filled_cards.push(parts.map(str::to_owned).to_vec());
                        true
                    }));
                }
            }
        });
        output.textures_delta.clear();
        if self.outcome.as_ref().is_some_and(|outcome| outcome.close) || escaped {
            self.menu_open = false;
            self.address_menu_open = false;
            self.card_menu_open = false;
        }
        // "Fill Card ›" replaces the key menu with the card menu.
        if self.outcome.as_ref().is_some_and(|outcome| outcome.cards) {
            self.outcome = None;
            self.menu_open = false;
            self.card_menu_open = true;
        }
        // "Fill Address ›" replaces the key menu with the address menu.
        if self.outcome.as_ref().is_some_and(|outcome| outcome.addresses) {
            self.outcome = None;
            self.menu_open = false;
            self.address_menu_open = true;
        }
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

    /// Let the card's input delay pass, as a person reading it would.
    fn wait(&mut self) -> Drawn {
        let frames = (OFFER_INPUT_DELAY * 60.0).ceil() as u32 + 2;
        for _ in 0..frames { self.frame(Vec::new()); }
        self.frame(Vec::new())
    }

    fn click(&mut self, label: &str) -> Drawn {
        let pos = self.wait().bounds(label).center();
        self.frame(vec![Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
        }
        self.settle()
    }

    fn key(&mut self, key: egui::Key) -> Drawn {
        self.wait();
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }]);
        }
        self.settle()
    }

    /// Click into the username field, as a user would.
    fn focus_username(&mut self) {
        let username = self.h.passwords.offer(TAB).unwrap().username.to_string();
        self.click(&username);
        assert!(self.ctx.memory(|memory| memory.has_focus(field_id(TAB))), "the username field has focus at {:?}", self.size);
    }

    fn card(&self) -> Option<Rect> {
        self.ctx.memory(|memory| memory.area_rect(Id::unique(("login-offer", TAB))))
    }

    fn address_card(&self) -> Option<Rect> {
        self.ctx.memory(|memory| memory.area_rect(Id::unique(("address-offer", TAB))))
    }

    /// Every label is whole inside its clip, the card and the pane, and clear
    /// of the others; the card keeps to the pane's top right.
    fn check_layout(&self, drawn: &Drawn) {
        let field = self.h.passwords.offer(TAB).map(|offer| offer.username.to_string()).unwrap_or_default();
        self.check_card(drawn, self.card().expect("the card is shown"), &field);
    }

    fn check_address_layout(&self, drawn: &Drawn) {
        let field = self.h.passwords.address_offer(TAB).map(|offer| offer.address.get(AddressField::Label).to_owned()).unwrap_or_default();
        self.check_card(drawn, self.address_card().expect("the address card is shown"), &field);
    }

    /// `field` is the text in the card's field, which scrolls inside it.
    fn check_card(&self, drawn: &Drawn, card: Rect, field: &str) {
        let pane = self.pane();
        assert!(pane.contains_rect(card), "the card {card:?} stays in the pane {pane:?} at {:?}", self.size);
        let at = offer_rect(pane, self.below_find);
        assert!((card.right() - at.right()).abs() < 0.5 && (card.top() - at.top()).abs() < 0.5 && (card.width() - at.width()).abs() < 0.5, "the card {card:?} keeps its place {at:?} at {:?}", self.size);
        let labels: Vec<_> = drawn.text.iter().filter(|(text, ..)| !text.is_empty() && *text != field).collect();
        for (text, bounds, clip) in &labels {
            assert!(clip.expand(0.5).contains_rect(*bounds) && card.expand(0.5).contains_rect(*bounds), "{text:?} is clipped at {:?}: {bounds:?} in {clip:?}, card {card:?}", self.size);
        }
        for (i, (text, bounds, _)) in labels.iter().enumerate() {
            for (other, other_bounds, _) in &labels[i + 1..] {
                assert!(!bounds.shrink(0.5).intersects(other_bounds.shrink(0.5)), "{text:?} overlaps {other:?} at {:?}", self.size);
            }
        }
    }
}

#[test]
fn the_offer_fits_in_every_state_at_both_sizes() {
    let long_site = format!("https://{}.login.example:8443", "very-long-subdomain-name".repeat(2));
    for size in SIZES {
        for (case, unlocked, origin, username, update, unlocking, error) in [
            ("save", true, SITE.to_owned(), USER.to_owned(), false, false, false),
            ("long site and username", true, long_site.clone(), "a-really-long-fixture-username@example-domain.test".to_owned(), false, false, false),
            ("update", true, SITE.to_owned(), USER.to_owned(), true, false, false),
            ("unlock", false, SITE.to_owned(), USER.to_owned(), false, true, false),
            ("unlock failed", false, SITE.to_owned(), USER.to_owned(), false, true, true),
        ] {
            let mut page = Page::new(size, unlocked);
            if update { page.add(&origin, &username, "fixture-old-password", Some(A)); }
            page.h.passwords.offer_login(TAB, A, &origin, &username, PASSWORD);
            if unlocking { page.h.passwords.save_offer(TAB); }
            if error { page.h.passwords.offer_mut(TAB).unwrap().error = Some(crossbow_core::vault::VaultError::Storage); }
            let drawn = page.settle();
            let title = format!("{} password for {}?", if update { "Update" } else { "Save" }, site(&origin));
            assert!(drawn.shows(&title), "{case}: {:?}", drawn.labels());
            for label in match (update, unlocking) {
                (_, true) => vec!["Unlock", "Not Now", LOCKED],
                (true, false) => vec!["Username", "Update Password", "Not Now"],
                (false, false) => vec!["Username", "Save", "Never for This Site", "Not Now"],
            } {
                assert!(drawn.shows(label), "{case} shows {label:?} at {size:?}: {:?}", drawn.labels());
            }
            assert!(!drawn.labels().iter().any(|text| text.contains(PASSWORD)), "the password is never drawn");
            assert_eq!(drawn.shows("Never for This Site"), !update && !unlocking, "{case}");
            page.check_layout(&drawn);
        }
    }
}

#[test]
fn save_with_a_locked_vault_asks_to_unlock_then_saves_the_edited_username() {
    for size in SIZES {
        let mut page = Page::new(size, false);
        page.h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
        page.focus_username();
        page.key(egui::Key::End);
        for _ in 0..USER.len() { page.key(egui::Key::Backspace); }
        page.frame(vec![Event::Text("edited-user".into())]);
        assert_eq!(page.h.passwords.offer(TAB).unwrap().username.as_str(), "edited-user");
        let drawn = page.click("Save");
        assert!(drawn.shows(LOCKED) && !page.h.passwords.unlocked(), "Save asks to unlock first at {size:?}");
        let drawn = page.click("Unlock");
        assert!(page.h.passwords.offer(TAB).is_none() && page.card().is_none_or(|_| !drawn.shows("Unlock")), "the card closes at {size:?}");
        assert_eq!(page.notices, [crate::passwords::SAVED]);
        page.h.reopen();
        page.h.passwords.status().unwrap();
        page.h.passwords.unlock();
        let saved = page.h.passwords.logins_for(SITE, A);
        assert_eq!(saved.iter().map(|(_, user)| user.as_str()).collect::<Vec<_>>(), ["edited-user"], "saved in the tab's profile, after a reopen");
        assert!(page.h.passwords.logins_for(SITE, B).is_empty());
    }
}

#[test]
fn never_and_not_now_close_the_card_and_escape_and_enter_work_from_the_username() {
    for size in SIZES {
        let mut page = Page::new(size, true);
        page.h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
        page.click("Never for This Site");
        assert!(page.h.passwords.offer(TAB).is_none());
        assert_eq!(page.notices, [crate::passwords::NEVER_SAVED]);
        assert_eq!(page.h.passwords.never_saved(A), [SITE]);

        let mut page = Page::new(size, true);
        page.h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
        page.click("Not Now");
        assert!(page.h.passwords.offer(TAB).is_none() && page.notices.is_empty());
        assert!(page.h.passwords.vault().unwrap().entries().is_empty(), "Not Now saves nothing");

        page.h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
        page.key(egui::Key::Escape);
        assert!(page.h.passwords.offer(TAB).is_some(), "Escape elsewhere is the page's at {size:?}");
        page.focus_username();
        page.key(egui::Key::Escape);
        assert!(page.h.passwords.offer(TAB).is_none(), "Escape in the username is Not Now at {size:?}");
        assert!(page.ctx.memory(|memory| memory.focused().is_none()), "and its field gives up focus");

        page.h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
        page.focus_username();
        page.key(egui::Key::Enter);
        assert_eq!(page.notices, [crate::passwords::SAVED], "Enter in the username is Save at {size:?}");
        assert_eq!(page.h.passwords.logins_for(SITE, A).len(), 1);
    }
}

#[test]
fn the_key_menu_unlocks_lists_only_this_page_logins_and_fills_the_chosen_one() {
    for size in SIZES {
        let mut page = Page::new(size, true);
        page.add(SITE, USER, PASSWORD, Some(A));
        page.add(SITE, "second-user", "fixture-second-password", Some(A));
        for (origin, username, profile) in [(SITE, "work-user", Some(B)), (SITE, "legacy-user", None), ("http://login.example", "http-user", Some(A)), ("https://login.example:8443", "port-user", Some(A))] {
            page.add(origin, username, "fixture-other-password", profile);
        }
        page.h.passwords.lock(None);
        page.menu_open = true;
        let drawn = page.settle();
        assert!(drawn.shows(LOCKED_HEADING) && drawn.shows(UNLOCK) && drawn.shows(MANAGE), "{:?}", drawn.labels());
        let drawn = page.click(UNLOCK);
        assert!(page.h.passwords.unlocked() && page.menu_open, "Unlock keeps the menu open at {size:?}");
        assert!(drawn.shows("Fill a password for login.example"));
        assert!(drawn.shows(USER) && drawn.shows("second-user"), "{:?}", drawn.labels());
        for other in ["work-user", "legacy-user", "http-user", "port-user"] {
            assert!(!drawn.shows(other), "{other} is never offered here at {size:?}");
        }
        page.click("second-user");
        assert_eq!(page.filled, [("second-user".to_owned(), "fixture-second-password".to_owned())]);
        assert!(!page.menu_open);
        assert!(page.outcome.take().is_some_and(|outcome| outcome.toast.is_none()));
    }
}

#[test]
fn a_fill_the_page_refuses_says_so_and_manage_opens_settings() {
    let mut h = Harness::new();
    h.passwords.status().unwrap();
    h.passwords.unlock();
    let id = h.passwords.vault_mut().unwrap().add(&EntryInput { origin: SITE, username: USER, password: PASSWORD, notes: "", profile: Some(A) }).unwrap();
    let outcome = apply_pick(&mut h.passwords, MenuPick::Fill(id.clone()), SITE, A, |_, _| false);
    assert_eq!(outcome, MenuOutcome { close: true, toast: Some(PAGE_MOVED.into()), manage: None, addresses: false, cards: false });
    let outcome = apply_pick(&mut h.passwords, MenuPick::Fill(id), "https://elsewhere.example", A, |_, _| panic!("never filled"));
    assert_eq!(outcome.toast.as_deref(), Some(crate::passwords::FILL_GONE));
    assert_eq!(apply_pick(&mut h.passwords, MenuPick::Manage, SITE, A, |_, _| false), MenuOutcome { close: true, toast: None, manage: Some(Section::Passwords), addresses: false, cards: false });
}

#[test]
fn the_key_and_copy_url_share_the_pill_row_without_overlapping_the_address() {
    for width in [200.0, 252.0] {
        let pill = Rect::from_min_size(pos2(8.0, 44.0), vec2(width - 16.0, 34.0));
        let (key, copy) = (crate::ui::sidebar::pill_control(pill, 0), crate::ui::sidebar::pill_control(pill, 1));
        assert!(pill.contains_rect(key) && pill.contains_rect(copy), "both stay in the pill at {width}");
        assert!(!key.intersects(copy) && (key.center().y - copy.center().y).abs() < 0.01 && key.size() == copy.size(), "one row, same size, apart at {width}");
        assert!(crate::ui::sidebar::pill_text_right(pill, true, 1) <= copy.left(), "the address ends before Copy URL at {width}");
        assert!(crate::ui::sidebar::pill_text_right(pill, true, 0) <= key.left(), "and before the lone Copy URL at {width}");
    }
}

#[test]
fn the_offer_moves_below_an_open_find_bar_and_never_exceeds_a_narrow_pane() {
    let pane = Rect::from_min_size(pos2(248.0, 8.0), vec2(300.0, 400.0));
    let (alone, below) = (offer_rect(pane, false), offer_rect(pane, true));
    assert!(below.top() >= pane.top() + 10.0 + 40.0 + 6.0 && alone.top() == pane.top() + 10.0);
    assert!(alone.width() <= pane.width() - 20.0 && alone.right() == pane.right() - 10.0);
}

#[test]
fn clicks_on_the_offer_never_reach_the_page_and_the_page_keeps_working_beside_it() {
    for size in SIZES {
        let mut page = Page::new(size, true);
        page.h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
        page.settle();
        let card = page.card().unwrap();
        let beside = pos2(page.pane().left() + 40.0, page.pane().bottom() - 40.0);
        assert!(!card.contains(beside));
        for pressed in [true, false] {
            page.frame(vec![Event::PointerMoved(beside), Event::PointerButton { pos: beside, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
        }
        assert_eq!(page.page_presses, [beside], "the page beside the card takes its click at {size:?}");
        assert!(page.h.passwords.offer(TAB).is_some(), "and the offer stays: it isn't modal");
        page.focus_username();
        page.click("Not Now");
        assert_eq!(page.page_presses, [beside], "nothing on the card reached the page at {size:?}");
    }
}

#[test]
fn the_top_strip_key_shares_the_traffic_light_row_and_clears_its_controls() {
    for (width, height) in [(640.0, 38.0), (1360.0, 38.0), (640.0, 52.0)] {
        let strip = Rect::from_min_size(egui::Pos2::ZERO, vec2(width, height));
        let key = crate::ui::window::ControlRow::new(strip, true, strip.height()).trailing_control(0);
        assert!(strip.contains_rect(key), "the key stays in the strip at {width}×{height}");
        for control in crate::ui::content::top_strip_controls(strip) {
            assert!(!key.intersects(control), "clear of the leading controls at {width}×{height}");
            assert!((key.center().y - control.center().y).abs() < 0.01 && key.size() == control.size(), "same row and size at {width}×{height}");
        }
    }
}

// -- round 5: a page can replace its offer at any time --------------------------------------

fn saved_passwords(page: &Page) -> Vec<String> {
    let vault = page.h.passwords.vault().unwrap();
    vault.matching(SITE, A).into_iter().map(|entry| vault.password(&entry.id).unwrap().to_string()).collect()
}

fn press_at(page: &mut Page, pos: Pos2) {
    for pressed in [true, false] {
        page.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
    }
}

/// WP4, ported from the review's probe: the user aims at a shown Save; the
/// page submits again before the press lands, turning it into Update
/// Password at the same spot. The press is ignored; only a click after the
/// delay, on what the card now says, updates.
#[test]
fn a_click_aimed_at_save_never_lands_on_a_swapped_update_password() {
    for size in SIZES {
        let mut page = Page::new(size, true);
        page.add(SITE, USER, PASSWORD, Some(A));
        page.h.passwords.offer_login(TAB, A, SITE, "fixture-new-user", "fixture-page-password-x");
        let aimed = page.wait().bounds("Save").center();
        page.frame(vec![Event::PointerMoved(aimed)]);
        // The page's second scripted submit arrives now.
        page.h.passwords.offer_login(TAB, A, SITE, USER, "fixture-forged-password");
        page.frame(Vec::new());
        press_at(&mut page, aimed);
        let drawn = page.settle();
        assert!(drawn.shows("Update password for login.example?"), "the card shows what the page sent at {size:?}");
        assert!(page.notices.is_empty(), "nothing applied at {size:?}: {:?}", page.notices);
        assert_eq!(saved_passwords(&page), vec![PASSWORD.to_owned()], "the saved password is unchanged at {size:?}");
        // Still resting there, a press that started before the delay ended is ignored too.
        page.frame(vec![Event::PointerButton { pos: aimed, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
        page.wait();
        page.frame(vec![Event::PointerButton { pos: aimed, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]);
        page.settle();
        assert!(page.notices.is_empty() && saved_passwords(&page) == vec![PASSWORD.to_owned()], "a press held across the change at {size:?}");
        // After the delay, a fresh press on what the card says is the user's choice.
        press_at(&mut page, aimed);
        page.settle();
        assert_eq!(page.notices, vec!["Password updated."], "at {size:?}");
        assert_eq!(saved_passwords(&page), vec!["fixture-forged-password".to_owned()]);
    }
}

/// WP4: a new offer, a replaced one, the Save-to-Update swap Save itself
/// makes and the unlock step each take no click or Enter for 500 ms, and a
/// click then needs the pointer to have moved or a fresh press.
#[test]
fn every_change_to_an_offer_ignores_input_for_half_a_second() {
    for size in SIZES {
        // Appearing: an immediate click and Enter do nothing.
        let mut page = Page::new(size, true);
        page.h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
        page.frame(Vec::new());
        let save = page.frame(Vec::new()).bounds("Save").center();
        page.frame(vec![Event::PointerMoved(save)]);
        press_at(&mut page, save);
        assert!(page.notices.is_empty() && saved_passwords(&page).is_empty(), "a click right after it appears at {size:?}");
        // After the delay, the same spot works: the pointer moved after the offer appeared.
        page.wait();
        press_at(&mut page, save);
        page.settle();
        assert_eq!(page.notices, vec![crate::passwords::SAVED], "at {size:?}");

        // Save turns into Update Password when the edited username is saved
        // already: the confirming click waits again.
        let mut page = Page::new(size, true);
        page.add(SITE, USER, "fixture-old-password", Some(A));
        page.h.passwords.offer_login(TAB, A, SITE, "fixture-other-user", PASSWORD);
        let name = &mut page.h.passwords.offer_mut(TAB).unwrap().username;
        name.clear();
        name.push_str(USER);
        let at = page.wait().bounds("Save").center();
        page.frame(vec![Event::PointerMoved(at)]);
        press_at(&mut page, at);
        let drawn = page.settle();
        assert!(drawn.shows("Update Password") && page.notices.is_empty(), "shown first at {size:?}");
        press_at(&mut page, at);
        page.settle();
        assert!(page.notices.is_empty(), "the swap's own confirming click waits at {size:?}");
        page.wait();
        press_at(&mut page, at);
        page.settle();
        assert_eq!(page.notices, vec!["Password updated."], "then it updates at {size:?}");

        // Enter in the username field waits too.
        let mut page = Page::new(size, true);
        page.h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
        page.settle();
        page.ctx.memory_mut(|memory| memory.request_focus(field_id(TAB)));
        page.frame(Vec::new());
        page.h.passwords.offer_login(TAB, A, SITE, USER, "fixture-page-password-2");
        page.frame(Vec::new());
        for pressed in [true, false] {
            page.frame(vec![Event::Key { key: egui::Key::Enter, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }]);
        }
        assert!(page.notices.is_empty(), "Enter right after the page replaced the offer at {size:?}");
    }
}

/// WP4 in the controller, ported from the review's probe: a page's second
/// submit replaces the shown offer with a new revision, which re-arms the card.
#[test]
fn a_replaced_offer_is_a_new_revision_that_the_card_arms_again() {
    let mut page = Page::new(SIZES[0], true);
    page.add(SITE, USER, PASSWORD, Some(A));
    page.h.passwords.offer_login(TAB, A, SITE, "fixture-new-user", "fixture-page-password-x");
    let first = page.h.passwords.offer(TAB).unwrap().revision;
    page.h.passwords.offer_login(TAB, A, SITE, USER, "fixture-forged-password");
    let swapped = page.h.passwords.offer(TAB).unwrap();
    assert!(matches!(swapped.kind, OfferKind::Update(_)));
    let revision = swapped.revision;
    assert_ne!(revision, first);
    page.h.passwords.offer_login(TAB, A, SITE, USER, "fixture-forged-password");
    assert_eq!(page.h.passwords.offer(TAB).unwrap().revision, revision, "the same login again changes nothing");
}

/// WP1, WP2 and WP5 in the app: the key button and fills use the document's
/// own origin, a fill that found no field says how to fill, and Peek and
/// Little Windows are created without capture. A WebView needs CEF, so this
/// is checked in the source.
#[test]
fn the_app_uses_the_documents_own_origin_says_when_nothing_was_filled_and_never_captures_outside_tabs() {
    let logins = include_str!("logins.rs");
    let page_fn = &logins[logins.find("pub fn page(app: &CrossbowApp").unwrap()..];
    let page_fn = &page_fn[..page_fn.find("\n}\n").unwrap()];
    assert!(page_fn.contains("let origin = view.login_origin()?;") && !page_fn.contains("page_origin"));
    let app = include_str!("../app.rs");
    assert!(app.contains("WebEvent::LoginNotFilled => self.ui.toasts.push(Toast { text: crate::ui::logins::NOT_FILLED.into(), at: ctx.input(|i| i.time) }),"));
    assert_eq!(NOT_FILLED, "Click the password field, then choose the login again.");
    let tab = &app[app.find("t.view = WebView::new(WebViewConfig {").unwrap()..];
    assert!(tab[..tab.find("});").unwrap()].contains("saves_logins: true,"));
    let peek = &app[app.find("Action::OpenInPeek(url) => {").unwrap()..];
    assert!(peek[..peek.find("}) {").unwrap()].contains("saves_logins: false,"));
    let little = include_str!("../little.rs");
    let little = &little[little.find("WebView::new(WebViewConfig {").unwrap()..];
    assert!(little[..little.find("}));").unwrap()].contains("saves_logins: false,"));
}

// -- addresses ------------------------------------------------------------------------------

/// A page's address parts, in the engine's order.
const PARTS: [&str; 9] = ["Ada Fixture", "", "1 Fixture Street", "Testville", "", "75001", "FR", "", ""];

impl Page {
    fn add_address(&mut self, profile: &str, label: &str) -> String {
        let mut address = Address::default();
        address.set(AddressField::Label, label);
        for (field, part) in crate::passwords::PAGE_PARTS.iter().zip(PARTS) { address.set(*field, part); }
        self.h.passwords.vault_mut().unwrap().add_address(profile, &address).unwrap()
    }

    /// Tab, as a keyboard user would, until the control showing `label` has focus.
    fn tab_to(&mut self, label: &str) {
        for _ in 0..20 {
            let drawn = self.key(egui::Key::Tab);
            // The page beneath takes focus too, and covers the menu.
            let focused = self.ctx.memory(|memory| memory.focused()).filter(|id| *id != Id::unique("fixture-page")).and_then(|id| self.ctx.read_response(id));
            if focused.is_some_and(|response| drawn.text.iter().any(|(text, bounds, _)| text == label && response.rect.contains(bounds.center()))) { return; }
        }
        panic!("not keyboard reachable: {label} at {:?}", self.size);
    }

    /// Every label of an open menu is whole, inside the window, and clear of the others.
    fn check_menu(&self, drawn: &Drawn) {
        let screen = Rect::from_min_size(Pos2::ZERO, self.size);
        for (i, (text, bounds, clip)) in drawn.text.iter().enumerate() {
            assert!(screen.contains_rect(*bounds) && clip.expand(0.5).contains_rect(*bounds), "{text:?} is clipped at {:?}: {bounds:?} in {clip:?}", self.size);
            for (other, other_bounds, _) in &drawn.text[i + 1..] {
                assert!(!bounds.shrink(0.5).intersects(other_bounds.shrink(0.5)), "{text:?} overlaps {other:?} at {:?}", self.size);
            }
        }
    }

    /// Click into the address card's empty label field, by its hint.
    fn focus_label(&mut self) {
        self.click("For example, Home");
        assert!(self.ctx.memory(|memory| memory.has_focus(label_id(TAB))), "the label field has focus at {:?}", self.size);
    }
}

#[test]
fn fill_address_opens_the_address_menu_and_fills_only_this_profile_address() {
    for size in SIZES {
        let mut page = Page::new(size, true);
        page.add(SITE, USER, PASSWORD, Some(A));
        page.add_address(A, "Fixture Home");
        let office = page.add_address(A, "Fixture Office");
        page.add_address(B, "Fixture Work Profile");
        page.menu_open = true;
        assert!(!page.settle().shows(FILL_ADDRESS), "no address field had focus at {size:?}");
        page.address_field = true;
        let drawn = page.settle();
        assert!(drawn.shows("Fill a password for login.example") && drawn.shows(USER), "{:?}", drawn.labels());
        assert!(drawn.shows(FILL_ADDRESS) && drawn.shows("›"), "{:?}", drawn.labels());
        page.check_menu(&drawn);
        let drawn = page.click(FILL_ADDRESS);
        assert!(page.address_menu_open && !page.menu_open, "the address menu replaces the key menu at {size:?}");
        assert!(drawn.shows("Fill an address on login.example") && drawn.shows("Fixture Home") && drawn.shows("Fixture Office") && drawn.shows(MANAGE_ADDRESSES), "{:?}", drawn.labels());
        assert!(!drawn.shows("Fixture Work Profile"), "never another profile's address at {size:?}");
        page.check_menu(&drawn);
        page.click("Fixture Office");
        assert_eq!(page.filled_addresses, [PARTS.map(str::to_owned).to_vec()], "the parts go to the page in the engine's order");
        assert!(!page.address_menu_open, "and the menu closes");
        assert!(page.outcome.take().is_some_and(|outcome| outcome.toast.is_none()));
        assert!(page.h.passwords.vault().unwrap().saved_address(&office).unwrap().used_ms.is_some(), "the fill is recorded");
    }
}

#[test]
fn the_address_menus_work_from_the_keyboard() {
    for size in SIZES {
        let mut page = Page::new(size, true);
        page.add_address(A, "Fixture Home");
        page.add_address(A, "Fixture Office");
        page.address_field = true;
        page.menu_open = true;
        let drawn = page.settle();
        assert!(!drawn.shows("No passwords for this site") && !drawn.labels().iter().any(|text| text.starts_with("Fill a password")), "a page with only an address field skips the password list: {:?}", drawn.labels());
        page.tab_to(FILL_ADDRESS);
        page.key(egui::Key::Enter);
        assert!(page.address_menu_open, "Enter opens the address menu at {size:?}");
        page.tab_to("Fixture Home");
        page.key(egui::Key::Space);
        assert_eq!(page.filled_addresses.len(), 1, "Space fills the focused address at {size:?}");
        assert!(!page.address_menu_open);
    }
}

#[test]
fn a_locked_vault_asks_to_unlock_in_both_menus_and_then_lists_the_addresses() {
    for size in SIZES {
        let mut page = Page::new(size, true);
        page.add_address(A, "Fixture Home");
        page.h.passwords.lock(None);
        page.address_field = true;
        page.menu_open = true;
        let drawn = page.settle();
        assert!(drawn.shows(LOCKED_HEADING) && drawn.shows(UNLOCK) && !drawn.shows(FILL_ADDRESS), "{:?}", drawn.labels());
        let drawn = page.click(UNLOCK);
        assert!(page.h.passwords.unlocked() && page.menu_open && drawn.shows(FILL_ADDRESS), "Unlock keeps the menu open at {size:?}");

        // A field's context menu opens the address menu directly.
        page.menu_open = false;
        page.h.passwords.lock(None);
        page.address_menu_open = true;
        let drawn = page.settle();
        assert!(drawn.shows(LOCKED_HEADING) && drawn.shows(UNLOCK) && !drawn.shows("Fixture Home"), "nothing is listed while locked: {:?}", drawn.labels());
        page.check_menu(&drawn);
        let drawn = page.click(UNLOCK);
        assert!(page.address_menu_open && drawn.shows("Fixture Home"), "{:?}", drawn.labels());
        let drawn = page.click(MANAGE_ADDRESSES);
        assert!(!page.address_menu_open && !drawn.shows("Fixture Home"));
        assert_eq!(page.outcome.take().map(|outcome| outcome.manage), Some(Some(Section::Addresses)), "Manage Addresses opens Settings on the list");
    }
}

#[test]
fn an_address_fill_the_page_refuses_or_another_profile_says_so() {
    let mut page = Page::new(SIZES[1], true);
    let home = page.add_address(A, "Fixture Home");
    let outcome = apply_address_pick(&mut page.h.passwords, AddressPick::Fill(home.clone()), A, |_| false);
    assert_eq!(outcome, MenuOutcome { close: true, toast: Some(ADDRESS_MOVED.into()), manage: None, addresses: false, cards: false });
    let outcome = apply_address_pick(&mut page.h.passwords, AddressPick::Fill(home), B, |_| panic!("never filled"));
    assert_eq!(outcome.toast.as_deref(), Some(crate::passwords::ADDRESS_GONE));
}

#[test]
fn the_address_offer_fits_in_every_state_at_both_sizes() {
    let long_site = format!("https://{}.shop.example:8443", "very-long-subdomain-name".repeat(2));
    let long_street = "42 Very Long Fixture Boulevard of the Synthetic Republic, Building C, Staircase 4";
    for size in SIZES {
        for (case, unlocked, origin, street, unlocking, error) in [
            ("save", true, SITE.to_owned(), PARTS[2], false, false),
            ("long site and street", true, long_site.clone(), long_street, false, false),
            ("unlock", false, SITE.to_owned(), PARTS[2], true, false),
            ("unlock failed", false, SITE.to_owned(), PARTS[2], true, true),
        ] {
            let mut page = Page::new(size, unlocked);
            let mut parts = PARTS;
            parts[2] = street;
            page.h.passwords.offer_address(TAB, A, &origin, parts);
            if unlocking { page.h.passwords.save_address_offer(TAB); }
            if error { page.h.passwords.address_offer_mut(TAB).unwrap().error = Some(crossbow_core::vault::VaultError::Storage); }
            let drawn = page.settle();
            assert!(drawn.shows(&format!("Save address from {}?", site(&origin))), "{case}: {:?}", drawn.labels());
            assert!(drawn.shows("Ada Fixture") && drawn.shows(&format!("{street}, Testville, 75001, FR")), "{case} shows what the page sent: {:?}", drawn.labels());
            let labels = if unlocking { vec!["Unlock", "Not Now", ADDRESS_LOCKED] } else { vec!["Label", "Save Address", "Never for This Site", "Not Now"] };
            for label in labels {
                assert!(drawn.shows(label), "{case} shows {label:?} at {size:?}: {:?}", drawn.labels());
            }
            page.check_address_layout(&drawn);
        }
    }
}

#[test]
fn an_address_offer_saves_with_its_label_or_goes_with_never_not_now_or_escape() {
    for size in SIZES {
        let mut page = Page::new(size, true);
        page.h.passwords.offer_address(TAB, A, SITE, PARTS);
        page.focus_label();
        page.frame(vec![Event::Text("Fixture Shop".into())]);
        page.click("Save Address");
        assert_eq!(page.notices, [crate::passwords::ADDRESS_SAVED]);
        assert!(page.h.passwords.address_offer(TAB).is_none());
        assert_eq!(page.h.passwords.addresses_for(A).iter().map(|(_, title)| title.as_str()).collect::<Vec<_>>(), ["Fixture Shop"], "saved in the tab's profile at {size:?}");

        let mut page = Page::new(size, true);
        page.h.passwords.offer_address(TAB, A, SITE, PARTS);
        page.click("Never for This Site");
        assert_eq!(page.notices, [crate::passwords::NEVER_SAVED]);
        assert_eq!(page.h.passwords.never_saved(A), [SITE]);

        let mut page = Page::new(size, true);
        page.h.passwords.offer_address(TAB, A, SITE, PARTS);
        page.click("Not Now");
        assert!(page.h.passwords.address_offer(TAB).is_none() && page.notices.is_empty());
        page.h.passwords.offer_address(TAB, A, SITE, PARTS);
        page.key(egui::Key::Escape);
        assert!(page.h.passwords.address_offer(TAB).is_none(), "Escape elsewhere is Not Now too (R9-5) at {size:?}");
        page.h.passwords.offer_address(TAB, A, SITE, PARTS);
        page.focus_label();
        page.key(egui::Key::Escape);
        assert!(page.h.passwords.address_offer(TAB).is_none(), "Escape in the label is Not Now at {size:?}");
        page.h.passwords.offer_address(TAB, A, SITE, PARTS);
        page.focus_label();
        page.key(egui::Key::Enter);
        assert_eq!(page.notices, [crate::passwords::ADDRESS_SAVED], "Enter in the label is Save Address at {size:?}");
        assert_eq!(page.h.passwords.vault().unwrap().addresses_in(A).len(), 1, "Not Now and Escape saved nothing");
    }
}

#[test]
fn a_locked_address_offer_unlocks_then_saves_and_waits_behind_a_login_offer() {
    for size in SIZES {
        let mut page = Page::new(size, false);
        page.h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
        page.h.passwords.offer_address(TAB, A, SITE, PARTS);
        let drawn = page.settle();
        assert!(drawn.shows(&format!("Save password for {}?", site(SITE))) && !drawn.labels().iter().any(|text| text.starts_with("Save address")), "a sign-up form's login comes first at {size:?}");
        page.click("Not Now");
        let drawn = page.settle();
        assert!(drawn.shows(&format!("Save address from {}?", site(SITE))), "then its address at {size:?}: {:?}", drawn.labels());
        let drawn = page.click("Save Address");
        assert!(drawn.shows(ADDRESS_LOCKED) && !page.h.passwords.unlocked(), "Save asks to unlock first at {size:?}");
        page.click("Unlock");
        assert_eq!(page.notices, [crate::passwords::ADDRESS_SAVED]);
        page.h.reopen();
        page.h.passwords.status().unwrap();
        page.h.passwords.unlock();
        assert_eq!(page.h.passwords.vault().unwrap().addresses_in(A).len(), 1, "kept after a reopen");
        assert!(page.h.passwords.logins_for(SITE, A).is_empty(), "Not Now on the login saved nothing");
    }
}

/// R8-1 on the card: an address offer shows every part Save Address keeps,
/// the organization, phone and email included, each on its own line beside
/// the name and the street line, inside the card at both sizes; Save
/// Address keeps exactly those parts and the label.
#[test]
fn ported_r8_1_the_address_offer_shows_every_part_it_saves() {
    const FULL: [&str; 9] = ["Ada Fixture", "Fixture Org", "1 Fixture Street", "Testville", "Fixture Region", "75001", "FR", "+1 555 0100", "ada@fixture.example"];
    for size in SIZES {
        let mut page = Page::new(size, true);
        page.h.passwords.offer_address(TAB, A, SITE, FULL);
        let drawn = page.settle();
        for line in ["Ada Fixture", "Fixture Org", "1 Fixture Street, Testville, Fixture Region, 75001, FR", "+1 555 0100", "ada@fixture.example"] {
            assert!(drawn.shows(line), "{line:?} is shown at {size:?}: {:?}", drawn.labels());
        }
        page.check_address_layout(&drawn);
        page.focus_label();
        page.frame(vec![Event::Text("Fixture Shop".into())]);
        page.click("Save Address");
        assert_eq!(page.notices, [crate::passwords::ADDRESS_SAVED]);
        let vault = page.h.passwords.vault().unwrap();
        let saved = vault.addresses_in(A);
        assert_eq!(saved.len(), 1);
        let parts: Vec<&str> = crate::passwords::PAGE_PARTS.iter().map(|field| saved[0].address.get(*field)).collect();
        assert_eq!(parts, FULL, "what the card showed, and only that, at {size:?}");
        assert_eq!(saved[0].address.get(AddressField::Label), "Fixture Shop");
    }
}

// -- round 10: the address card keeps its buttons in the window (R9-5) ----------------------

/// The end of each part, so its last line can be found on the card.
const PART_ENDS: [&str; 9] = ["name-end", "org-end", "street-end", "city-end", "region-end", "postal-end", "country-end", "phone-end", "email-end"];

/// Every part a page can send at its limit, each ending in its marker: on one
/// line, as an `<input>` sends it, or broken into short lines, as a
/// `<textarea>` can send it.
fn parts_at_their_limits(lines: bool) -> Vec<String> {
    let seed = if lines { "Fixture line\n" } else { "Fixture part " };
    crate::passwords::PAGE_PARTS.iter().zip(PART_ENDS).map(|(field, end)| {
        let mut part = String::new();
        while part.len() + seed.len() + end.len() <= field.limit() { part.push_str(seed); }
        part + end
    }).collect()
}

/// A site whose name wraps over several lines of the card's title.
fn long_site() -> String {
    format!("https://{label}.{label}.{label}.shop.example:8443", label = "fixture-long-label-".repeat(3))
}

impl Page {
    /// Offer every part at its limit from the long site, and let the delay pass.
    fn offer_at_limits(&mut self, lines: bool) -> Drawn {
        let parts = parts_at_their_limits(lines);
        let parts: [&str; 9] = std::array::from_fn(|i| parts[i].as_str());
        self.h.passwords.offer_address(TAB, A, &long_site(), parts);
        assert!(self.h.passwords.address_offer(TAB).is_some(), "offered at {:?}", self.size);
        self.wait()
    }

    /// Wheel down over the middle of the address card until nothing moves.
    fn scroll_card_down(&mut self) -> Drawn {
        let over = self.address_card().unwrap().center();
        self.frame(vec![Event::PointerMoved(over)]);
        let mut last = self.frame(Vec::new());
        for _ in 0..200 {
            let wheel = Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -400.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Modifiers::NONE };
            self.frame(vec![wheel]);
            let drawn = self.settle();
            let unchanged = drawn.text.len() == last.text.len() && drawn.text.iter().zip(&last.text).all(|(a, b)| a.0 == b.0 && a.1 == b.1);
            last = drawn;
            if unchanged { break; }
        }
        last
    }

    /// The label drawn with `needle` in it, its bounds and its clip.
    fn part<'a>(drawn: &'a Drawn, needle: &str) -> &'a (String, Rect, Rect) {
        drawn.text.iter().find(|(text, ..)| text.contains(needle)).unwrap_or_else(|| panic!("{needle:?} is not drawn: {:?}", drawn.labels()))
    }

    /// The title and the three buttons are drawn whole, unclipped, inside the
    /// card, and the card inside the pane, below the Find bar when it's open.
    fn check_card_frame(&self, drawn: &Drawn, case: &str) {
        let (pane, card) = (self.pane(), self.address_card().expect("the address card is shown"));
        let at = offer_rect(pane, self.below_find);
        assert!(pane.contains_rect(card) && card.bottom() <= pane.bottom() - 9.5, "{case}: the card {card:?} stays in the pane {pane:?}");
        assert!((card.top() - at.top()).abs() < 0.5 && (card.right() - at.right()).abs() < 0.5, "{case}: the card {card:?} keeps its place {at:?}");
        let title = format!("Save address from {}?", site(&long_site()));
        for label in [title.as_str(), "Save Address", "Never for This Site", "Not Now"] {
            let (_, bounds, clip) = drawn.text.iter().find(|(text, ..)| text == label).unwrap_or_else(|| panic!("{case}: {label:?} is not drawn: {:?}", drawn.labels()));
            assert!(clip.expand(0.5).contains_rect(*bounds) && card.expand(0.5).contains_rect(*bounds), "{case}: {label:?} is whole at {bounds:?} in {clip:?}, card {card:?}");
        }
    }
}

/// R9-5, ported from the ninth recheck's card probes: with every part at its
/// limit, on one line each or broken into lines, from a site that wraps, and
/// with or without the Find bar above it, the address card stays inside its
/// pane at both sizes. Its title and Save Address, Never for This Site and
/// Not Now are drawn whole and unclipped; the parts scroll, from the start
/// of the first to the end of the last, and the buttons stay put meanwhile.
#[test]
fn ported_r9_5_the_address_card_keeps_its_buttons_in_the_window_with_every_part_at_its_limit() {
    for size in SIZES {
        for below_find in [false, true] {
            for lines in [false, true] {
                let case = format!("{size:?}, below the Find bar {below_find}, line breaks {lines}");
                let mut page = Page::new(size, true);
                page.below_find = below_find;
                let drawn = page.offer_at_limits(lines);
                page.check_card_frame(&drawn, &case);
                let (_, first, clip) = Page::part(&drawn, "Fixture");
                assert!(clip.contains(first.left_top()), "{case}: the first part starts in view: {first:?} in {clip:?}");
                let drawn = page.scroll_card_down();
                page.check_card_frame(&drawn, &format!("{case}, scrolled"));
                let (_, last, clip) = Page::part(&drawn, "email-end");
                assert!(clip.expand(0.5).contains(last.left_bottom()), "{case}: scrolling reaches the last part's end: {last:?} in {clip:?}");
            }
        }
    }
}

/// R9-5: each button of a card at its limits does what it says. Save
/// Address keeps every part as sent, Never for This Site remembers the
/// site, and Not Now saves nothing.
#[test]
fn ported_r9_5_every_button_of_a_card_at_its_limits_works() {
    let parts = parts_at_their_limits(true);
    for size in SIZES {
        let mut page = Page::new(size, true);
        page.offer_at_limits(true);
        page.click("Save Address");
        assert_eq!(page.notices, [crate::passwords::ADDRESS_SAVED], "at {size:?}");
        let saved = page.h.passwords.vault().unwrap().addresses_in(A);
        let kept: Vec<&str> = crate::passwords::PAGE_PARTS.iter().map(|field| saved[0].address.get(*field)).collect();
        assert_eq!(kept, parts, "every part as sent at {size:?}");

        let mut page = Page::new(size, true);
        page.offer_at_limits(true);
        page.click("Never for This Site");
        assert_eq!(page.notices, [crate::passwords::NEVER_SAVED], "at {size:?}");
        assert_eq!(page.h.passwords.never_saved(A), [long_site()]);

        let mut page = Page::new(size, true);
        page.offer_at_limits(true);
        page.click("Not Now");
        assert!(page.h.passwords.address_offer(TAB).is_none() && page.notices.is_empty(), "at {size:?}");
        assert!(page.h.passwords.vault().unwrap().addresses_in(A).is_empty() && page.h.passwords.never_saved(A).is_empty());
    }
}

/// R9-5: Escape is Not Now for the address card on the pane, whatever has
/// focus: its Label, another field of Crossbow's own, or the page (no egui
/// focus). It saves and remembers nothing. A card waiting behind a login's
/// isn't drawn, so Escape leaves it, and the login card's own rule stands:
/// Escape elsewhere is the page's.
#[test]
fn ported_r9_5_escape_dismisses_the_address_card_whatever_has_focus() {
    for size in SIZES {
        for focus in ["the label", "another field", "the page"] {
            let mut page = Page::new(size, true);
            page.sidebar_field = Some(String::new());
            page.h.passwords.offer_address(TAB, A, SITE, PARTS);
            page.wait();
            match focus {
                "the label" => page.focus_label(),
                "another field" => {
                    let (field, at) = (Id::unique("fixture-sidebar-field"), pos2(124.0, 58.0));
                    page.frame(vec![Event::PointerMoved(at)]);
                    for pressed in [true, false] {
                        page.frame(vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
                    }
                    page.settle();
                    assert!(page.ctx.memory(|memory| memory.has_focus(field)), "the sidebar field has focus at {size:?}");
                }
                _ => assert!(page.ctx.memory(|memory| memory.focused()).is_none(), "nothing of Crossbow's has focus at {size:?}"),
            }
            page.key(egui::Key::Escape);
            assert!(page.h.passwords.address_offer(TAB).is_none(), "Escape with {focus} focused is Not Now at {size:?}");
            assert!(page.notices.is_empty() && page.h.passwords.vault().unwrap().addresses_in(A).is_empty() && page.h.passwords.never_saved(A).is_empty(), "{focus} at {size:?}");
        }
        let mut page = Page::new(size, true);
        page.offer_at_limits(true);
        page.key(egui::Key::Escape);
        assert!(page.h.passwords.address_offer(TAB).is_none(), "a card at its limits goes too at {size:?}");

        let mut page = Page::new(size, true);
        page.h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD);
        page.h.passwords.offer_address(TAB, A, SITE, PARTS);
        page.key(egui::Key::Escape);
        assert!(page.h.passwords.offer(TAB).is_some() && page.h.passwords.address_offer(TAB).is_some(), "the login card shows; Escape is the page's at {size:?}");
    }
    // The app's Escape chain: after what is drawn above the card, before Peek, Find and the page.
    let app = include_str!("../app.rs");
    let at = |line: &str| app.find(line).unwrap_or_else(|| panic!("{line:?} is in the chain"));
    let shown = at("} else if ui::logins::dismiss_shown_address_offer(ctx, ui::logins::focused_offer_tab(self, ctx.content_rect()).as_deref(), &mut self.passwords) {");
    assert!(at("} else if ui::logins::dismiss_focused_offer(ctx, &mut self.passwords) {") < shown && at("} else if self.dismiss_library(ctx, LibraryDismissal::RestoreFocus) {") < shown);
    assert!(shown < at("} else if let Some(peek) = &self.ui.peek {") && shown < at("} else if self.ui.find.is_some() {"));
}

// -- payment cards (ASP-P16) -------------------------------------------------------------------
// The card networks' published test numbers and invented names only.

const VISA: &str = "4242 4242 4242 4242";
const VISA_DIGITS: &str = "4242424242424242";
const MASTERCARD: &str = "5555 5555 5555 4444";

impl Page {
    fn add_card(&mut self, profile: &str, number: &str, nickname: &str) -> String {
        let card = crossbow_core::vault::Card {
            name: Zeroizing::new("Fixture Cardholder".into()), number: Zeroizing::new(number.into()), exp_month: 7, exp_year: 2034, nickname: Zeroizing::new(nickname.into()),
        };
        self.h.passwords.vault_mut().unwrap().add_card(profile, &card).unwrap()
    }

    /// No drawn text holds a full card number, in any spacing.
    fn no_full_number(&self, drawn: &Drawn) {
        for number in [VISA, VISA_DIGITS, MASTERCARD, "5555555555554444"] {
            assert!(!drawn.text.iter().any(|(text, ..)| text.contains(number)), "{number} is drawn at {:?}", self.size);
        }
    }
}

#[test]
fn fill_card_lists_the_profiles_cards_by_network_and_last_four_and_fills_only_the_chosen_one() {
    for size in SIZES {
        let mut page = Page::new(size, true);
        let visa = page.add_card(A, VISA, "Fixture Travel");
        page.add_card(A, MASTERCARD, "");
        page.add_card(B, VISA, "Fixture Work");
        page.menu_open = true;
        let drawn = page.settle();
        assert!(!drawn.shows(FILL_CARD), "no card field had focus: {:?}", drawn.labels());
        page.card_field = true;
        let drawn = page.settle();
        assert!(drawn.shows(FILL_CARD) && drawn.shows("›"), "{:?}", drawn.labels());
        page.check_menu(&drawn);
        let drawn = page.click(FILL_CARD);
        assert!(page.card_menu_open && !page.menu_open, "Fill Card › replaces the key menu at {size:?}");
        assert!(drawn.shows(&format!("Fill a card on {}", site(SITE))) && drawn.shows("Mastercard ••••4444") && drawn.shows("Visa ••••4242"), "{:?}", drawn.labels());
        assert_eq!(drawn.text.iter().filter(|(text, ..)| text.contains("••••")).count(), 2, "only this profile's cards: {:?}", drawn.labels());
        assert_eq!(drawn.text.iter().filter(|(text, ..)| text == "07/34").count(), 2, "each with its expiry");
        assert!(!drawn.labels().iter().any(|text| text.contains("Fixture Cardholder") || text.contains("CVC") || text.to_lowercase().contains("security code")));
        page.no_full_number(&drawn);
        page.check_menu(&drawn);
        page.click("Visa ••••4242");
        assert_eq!(page.filled_cards, [vec!["Fixture Cardholder".to_owned(), VISA_DIGITS.to_owned(), "07".to_owned(), "2034".to_owned()]], "the chosen card, once, at {size:?}");
        assert!(!page.card_menu_open, "the menu closes after a fill");
        assert!(page.outcome.take().is_some_and(|outcome| outcome.toast.is_none()), "a fill shows no toast");
        assert!(page.h.passwords.vault().unwrap().saved_card(&visa).unwrap().used_ms.is_some());

        // Escape closes the card menu and fills nothing.
        page.menu_open = true;
        page.click(FILL_CARD);
        assert!(page.card_menu_open);
        let drawn = page.key(egui::Key::Escape);
        assert!(!page.card_menu_open && !drawn.shows("Visa ••••4242"), "Escape closes the card menu at {size:?}");
        assert_eq!(page.filled_cards.len(), 1, "Escape filled nothing");

        // The keyboard reaches the card menu and its cards.
        page.menu_open = true;
        page.tab_to(FILL_CARD);
        page.key(egui::Key::Enter);
        assert!(page.card_menu_open, "Enter opens the card menu at {size:?}");
        page.tab_to("Mastercard ••••4444");
        page.key(egui::Key::Space);
        assert_eq!(page.filled_cards.len(), 2, "Space fills the focused card at {size:?}");
        assert_eq!(page.filled_cards[1][1], "5555555555554444");
    }
}

#[test]
fn a_locked_vault_asks_to_unlock_in_the_card_menu_and_manage_opens_the_cards_list() {
    for size in SIZES {
        let mut page = Page::new(size, true);
        page.add_card(A, VISA, "Fixture Travel");
        page.h.passwords.lock(None);
        page.card_menu_open = true;
        let drawn = page.settle();
        assert!(drawn.shows(LOCKED_HEADING) && drawn.shows(UNLOCK) && !drawn.shows("Visa ••••4242"), "nothing is listed while locked: {:?}", drawn.labels());
        page.check_menu(&drawn);
        let drawn = page.click(UNLOCK);
        assert!(page.card_menu_open && drawn.shows("Visa ••••4242"), "Unlock keeps the menu open at {size:?}: {:?}", drawn.labels());
        let drawn = page.click(MANAGE_CARDS);
        assert!(!page.card_menu_open && !drawn.shows("Visa ••••4242"));
        assert_eq!(page.outcome.take().map(|outcome| outcome.manage), Some(Some(Section::Cards)), "Manage Cards opens Settings on the list");
        assert!(page.filled_cards.is_empty());
    }
}

#[test]
fn a_card_fill_the_page_refuses_or_another_profile_says_so() {
    let mut page = Page::new(SIZES[1], true);
    let visa = page.add_card(A, VISA, "");
    let outcome = apply_card_pick(&mut page.h.passwords, CardPick::Fill(visa.clone()), A, |_| false);
    assert_eq!(outcome, MenuOutcome { close: true, toast: Some(CARD_MOVED.into()), manage: None, addresses: false, cards: false });
    let outcome = apply_card_pick(&mut page.h.passwords, CardPick::Fill(visa), B, |_| panic!("never filled"));
    assert_eq!(outcome.toast.as_deref(), Some(crate::passwords::CARD_GONE));
    assert_eq!(CARD_NOT_FILLED, "Click the card number field, then choose the card again.");
}

/// C10-1: the card menu is frozen when it opens, on its tab, that tab's
/// page view and profile, and the engine's frame, document, origin and
/// card note. While it is open, and at the choice, it fills only if all of
/// that still holds; otherwise it closes, nothing is sent, and a notice
/// says why: a tab switch, a closed window, a profile switch, a replaced
/// page, or any change the engine sees (navigation, a removed frame, focus
/// or a card note elsewhere). A fill the engine refuses at the choice says
/// the same.
#[test]
fn a_card_menu_fills_only_where_it_was_opened() {
    let menu = FillMenu { tab: TAB.into(), view: 7, profile: A.into(), target: crossbow_engine::FillTarget::default() };
    let wrong: Vec<&str> = [
        ("nothing changed", menu_holds(&menu, Some(TAB), Some(A), Some(7), true), true),
        ("another tab in front", menu_holds(&menu, Some("tab-2"), Some(A), Some(7), true), false),
        ("no tab in front, as when its window closed", menu_holds(&menu, None, Some(A), Some(7), true), false),
        ("the tab's profile changed", menu_holds(&menu, Some(TAB), Some(B), Some(7), true), false),
        ("the tab has no profile", menu_holds(&menu, Some(TAB), None, Some(7), true), false),
        ("the tab's page view was replaced", menu_holds(&menu, Some(TAB), Some(A), Some(8), true), false),
        ("the tab has no page", menu_holds(&menu, Some(TAB), Some(A), None, true), false),
        ("the engine's frame, document, origin, card note or focus changed", menu_holds(&menu, Some(TAB), Some(A), Some(7), false), false),
    ].into_iter().filter(|(_, got, want)| got != want).map(|(case, ..)| case).collect();
    assert!(wrong.is_empty(), "{wrong:?}");
    let mut page = Page::new(SIZES[0], true);
    let visa = page.add_card(A, VISA, "");
    let outcome = apply_card_pick(&mut page.h.passwords, CardPick::Fill(visa), A, |_| false);
    assert_eq!(outcome, MenuOutcome { close: true, toast: Some(CARD_MOVED.into()), manage: None, addresses: false, cards: false }, "the engine refused at the choice");
    assert_eq!(CARD_MOVED, "Nothing was filled: the page changed while the card menu was open.");
    // The real menu, read from the source: checked before it draws, every frame, and filled only through the frozen target.
    let logins = include_str!("logins.rs");
    let card_menu = &logins[logins.find("pub fn card_menu(").unwrap()..];
    let card_menu = &card_menu[..card_menu.find("\n}\n").unwrap()];
    let at = |needle: &str| card_menu.find(needle).unwrap_or_else(|| panic!("{needle}"));
    assert!(at("let Some(view) = frozen_view(app, menu, |view, target| view.card_target_holds(target)) else {\n        app.actions.push(Action::Toast(CARD_MOVED.into()));\n        return true;") < at("card_entries("));
    assert!(card_menu.contains("view.fill_card_at(&menu.target, parts)") && !card_menu.contains("card_page("), "never re-derived at the choice");
    let frozen = &logins[logins.find("fn frozen_view(").unwrap()..];
    assert!(frozen[..frozen.find("\n}\n").unwrap()].contains("view.filter(|_| menu_holds(menu, app.focused_tab_id().as_deref(), profile, serial, holds))"));
    // Both ways in freeze it as it opens.
    let overlays = include_str!("overlays.rs");
    assert!(overlays.contains("match crate::ui::logins::freeze_card_menu(app, tab) {\n                                Some(menu) => pick = Some(crate::ui::folders::Pick::Open(MenuKind::Cards(menu))),"));
    assert!(logins.contains("let Some(menu) = freeze_card_menu(app, tab) else {") && logins.contains("MenuKind::Cards(menu), keyboard)"));
}

/// C10-1 for addresses: the address menu is frozen as the card menu is,
/// checked every frame and at the choice through the same rule, and a
/// fill the engine refuses there says so.
#[test]
fn an_address_menu_fills_only_where_it_was_opened() {
    let mut page = Page::new(SIZES[0], true);
    let home = page.add_address(A, "Fixture Home");
    let outcome = apply_address_pick(&mut page.h.passwords, AddressPick::Fill(home), A, |_| false);
    assert_eq!(outcome, MenuOutcome { close: true, toast: Some(ADDRESS_MOVED.into()), manage: None, addresses: false, cards: false }, "the engine refused at the choice");
    assert_eq!(ADDRESS_MOVED, "Nothing was filled: the page changed while the address menu was open.");
    let logins = include_str!("logins.rs");
    let address_menu = &logins[logins.find("pub fn address_menu(").unwrap()..];
    let address_menu = &address_menu[..address_menu.find("\n}\n").unwrap()];
    let at = |needle: &str| address_menu.find(needle).unwrap_or_else(|| panic!("{needle}"));
    assert!(at("let Some(view) = frozen_view(app, menu, |view, target| view.address_target_holds(target)) else {\n        app.actions.push(Action::Toast(ADDRESS_MOVED.into()));\n        return true;") < at("address_entries("));
    assert!(address_menu.contains("view.fill_address_at(&menu.target, parts)") && !address_menu.contains("address_page("), "never re-derived at the choice");
    let overlays = include_str!("overlays.rs");
    assert!(overlays.contains("match crate::ui::logins::freeze_address_menu(app, tab) {\n                                Some(menu) => pick = Some(crate::ui::folders::Pick::Open(MenuKind::Addresses(menu))),"));
    assert!(logins.contains("let Some(menu) = freeze_address_menu(app, tab) else {") && logins.contains("MenuKind::Addresses(menu), keyboard)"));
}

/// Round 10: an address or card fill that waited for the engine's answer
/// says plainly what it left empty and why, and says nothing when every
/// field took its value. The app shows it as a toast for either kind.
#[test]
fn a_fill_that_left_fields_empty_says_how_many_and_why() {
    use crossbow_engine::FillReport;
    let text = |report: FillReport| fill_report_text(report);
    assert_eq!(text(FillReport::Done { filled: 4, hidden: 2 }).as_deref(), Some("Filled 4 of 6 fields. 2 weren't clearly visible, so they were left empty."));
    assert_eq!(text(FillReport::Done { filled: 2, hidden: 1 }).as_deref(), Some("Filled 2 of 3 fields. 1 wasn't clearly visible, so it was left empty."));
    assert_eq!(text(FillReport::Done { filled: 0, hidden: 5 }).as_deref(), Some("Nothing was filled. 5 fields weren't clearly visible, so they were left empty."));
    assert_eq!(text(FillReport::Done { filled: 0, hidden: 1 }).as_deref(), Some("Nothing was filled. 1 field wasn't clearly visible, so it was left empty."));
    assert_eq!(text(FillReport::Done { filled: 512, hidden: 512 }).as_deref(), Some("Filled 512 of 1024 fields. 512 weren't clearly visible, so they were left empty."));
    assert_eq!(text(FillReport::Done { filled: 6, hidden: 0 }), None, "every field filled: nothing to say");
    assert_eq!(text(FillReport::FocusHidden).as_deref(), Some("Nothing was filled: the field you clicked isn't clearly visible on the page."));
    assert_eq!(text(FillReport::Unconfirmed).as_deref(), Some("Nothing was filled: Crossbow couldn't confirm the fields are visible on the page."));
    let app = include_str!("../app.rs");
    assert!(app.contains("WebEvent::AddressFillReport(report) | WebEvent::CardFillReport(report) => {\n                        if let Some(text) = crate::ui::logins::fill_report_text(report) { self.ui.toasts.push(Toast { text, at: ctx.input(|i| i.time) }); }"));
}

/// AI-19 (third AI review): a permission prompt or notice on the offer's
/// page goes first, as it does for the Translate card, so the two never
/// share a place. The offer waits unseen and Escape leaves it; once the
/// prompt is answered it comes back taking no click for the input delay,
/// as after any change, then works.
#[test]
fn an_offer_waits_for_a_permission_prompt_on_its_page_and_arms_again_when_it_returns() {
    use crate::site_permissions::tests::{MEET, request};
    use crossbow_core::site_permissions::Permission;
    const NEVER: &str = "Never for This Site";
    for size in SIZES {
        for kind in ["login", "address"] {
            let mut page = Page::new(size, true);
            if kind == "login" { page.h.passwords.offer_login(TAB, A, SITE, USER, PASSWORD) } else { page.h.passwords.offer_address(TAB, A, SITE, PARTS) }
            let card = Id::unique((if kind == "login" { "login-offer" } else { "address-offer" }, TAB));
            let visible = |page: &Page| page.ctx.memory(|memory| memory.areas().visible_last_frame(&egui::LayerId::new(egui::Order::Foreground, card)));
            let at = page.wait().bounds(NEVER).center();
            assert!(visible(&page));
            assert_eq!(page.perms.ask(TAB, 1, A, &request(1, Permission::Camera, "https://meet.example/", MEET, 1)), None, "the page asks");
            let drawn = page.settle();
            assert!(!visible(&page) && !drawn.shows(NEVER), "the {kind} offer waits for the prompt at {size:?}");
            page.key(egui::Key::Escape);
            assert!(page.h.passwords.offer(TAB).is_some() || page.h.passwords.address_offer(TAB).is_some(), "Escape leaves the {kind} offer no one sees at {size:?}");

            // Answered: back, and a click at once on its place does nothing.
            page.perms.perms.choose(&page.perms.store, TAB, crate::site_permissions::Choice::Deny, crossbow_core::now_ms());
            page.frame(vec![Event::PointerMoved(at)]);
            assert!(visible(&page), "the {kind} offer comes back at {size:?}");
            press_at(&mut page, at);
            page.settle();
            assert!(page.notices.is_empty() && page.h.passwords.never_saved(A).is_empty(), "a click as the {kind} offer comes back at {size:?}");
            page.wait();
            press_at(&mut page, at);
            page.settle();
            assert_eq!(page.notices, [crate::passwords::NEVER_SAVED], "then the {kind} offer works at {size:?}");
        }
    }
}
