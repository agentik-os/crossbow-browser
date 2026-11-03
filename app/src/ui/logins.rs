//! Website passwords, addresses and cards on pages: the offer a submitted
//! login or address shows at the top right of its pane, and the key button
//! and menus that fill a login, an address or a card on request. The
//! password and a card's full number are never drawn; the username, a
//! submitted address and a card's network and last four digits are.

use crate::app::{Action, CrossbowApp, MenuKind, MenuState};
use crate::passwords::{AddressOffer, KeyState, Offer, OfferKind, Passwords, Section, address_summary};
use crossbow_core::vault::AddressField;
use crossbow_engine::FillReport;
use crate::ui::icons::{self, Icon};
use crate::ui::overlays::MenuUi;
use crate::ui::style::{self, Button, Ink, Kind, Size};
use crate::ui::theme::Palette;
use egui::{Id, Key, Rect, Ui, pos2, vec2};

pub const OFFER_WIDTH: f32 = 320.0;
/// How long an offer ignores clicks and Enter after it appears or changes.
/// A page can replace its offer at any time, turning Save into Update
/// Password under the pointer.
pub const OFFER_INPUT_DELAY: f64 = 0.5;
const LOCKED: &str = "Passwords are locked. Unlock them to save this password.";
const MANAGE: &str = "Manage Passwords…";
const UNLOCK: &str = "Unlock Passwords";
const LOCKED_HEADING: &str = "Passwords are locked";
const NO_LOGINS: &str = "No passwords for this site";
const NO_USERNAME: &str = "No username";
/// The page navigated away from the site between the click and the fill.
const PAGE_MOVED: &str = "This page is no longer on that site, so nothing was filled.";
/// The page had no login field with focus and no password field the user can see.
pub const NOT_FILLED: &str = "Click the password field, then choose the login again.";
const ADDRESS_LOCKED: &str = "Passwords are locked. Unlock them to save this address.";
pub const FILL_ADDRESS: &str = "Fill Address";
const MANAGE_ADDRESSES: &str = "Manage Addresses…";
const NO_ADDRESSES: &str = "No addresses in this profile";
/// The page had no address field with focus and no address form the user can see.
pub const ADDRESS_NOT_FILLED: &str = "Click a field of the address form, then choose the address again.";
pub const FILL_CARD: &str = "Fill Card";
const MANAGE_CARDS: &str = "Manage Cards…";
const NO_CARDS: &str = "No cards in this profile";
/// The page had no card field with focus and no single card form the user can see.
pub const CARD_NOT_FILLED: &str = "Click the card number field, then choose the card again.";
/// Where the card menu was opened changed before the choice, so nothing was sent.
pub const CARD_MOVED: &str = "Nothing was filled: the page changed while the card menu was open.";
/// The same, for the address menu.
pub const ADDRESS_MOVED: &str = "Nothing was filled: the page changed while the address menu was open.";
/// An address or card fill filled nothing: the engine doesn't call the focused field clearly visible.
pub const FOCUS_HIDDEN: &str = "Nothing was filled: the field you clicked isn't clearly visible on the page.";
/// An address or card fill filled nothing: no answer came on which fields the user can see.
pub const VISIBILITY_UNCONFIRMED: &str = "Nothing was filled: Crossbow couldn't confirm the fields are visible on the page.";

/// What an address or card fill tells the user when the engine didn't call
/// every field clearly visible; None when every field took its value.
pub fn fill_report_text(report: FillReport) -> Option<String> {
    match report {
        FillReport::Done { hidden: 0, .. } => None,
        FillReport::Done { filled: 0, hidden: 1 } => Some("Nothing was filled. 1 field wasn't clearly visible, so it was left empty.".into()),
        FillReport::Done { filled: 0, hidden } => Some(format!("Nothing was filled. {hidden} fields weren't clearly visible, so they were left empty.")),
        FillReport::Done { filled, hidden: 1 } => Some(format!("Filled {filled} of {} fields. 1 wasn't clearly visible, so it was left empty.", u32::from(filled) + 1)),
        FillReport::Done { filled, hidden } => Some(format!("Filled {filled} of {} fields. {hidden} weren't clearly visible, so they were left empty.", u32::from(filled) + u32::from(hidden))),
        FillReport::FocusHidden => Some(FOCUS_HIDDEN.into()),
        FillReport::Unconfirmed => Some(VISIBILITY_UNCONFIRMED.into()),
    }
}

/// What a click or key on an offer asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfferRequest {
    Save,
    Never,
    NotNow,
    Unlock,
}

/// The site as the offer names it: the host, and the scheme only for http.
pub fn site(origin: &str) -> &str {
    origin.strip_prefix("https://").unwrap_or(origin)
}

fn title(offer: &Offer) -> String {
    match offer.kind {
        OfferKind::Save => format!("Save password for {}?", site(&offer.origin)),
        OfferKind::Update(_) => format!("Update password for {}?", site(&offer.origin)),
    }
}

fn field_id(tab: &str) -> Id {
    Id::unique(("login-offer-username", tab))
}

fn label_id(tab: &str) -> Id {
    Id::unique(("address-offer-label", tab))
}

/// What the card showed last, since when, whether the pointer has moved
/// since then, and whether a button went down once the delay was over.
#[derive(Clone)]
struct Armed {
    shown: (u64, Option<OfferKind>, bool),
    since: f64,
    moved: bool,
    pressed: bool,
}

/// Whether the card takes input now: Enter once [`OFFER_INPUT_DELAY`] has
/// passed since its content last changed, and a click only if the pointer
/// also moved since the change or the press began after the delay. Returns
/// (ready, a click counts).
/// `shown` is the offer's revision, kind and unlocking step; `id` is the card's.
fn arming(ctx: &egui::Context, id: Id, shown: (u64, Option<OfferKind>, bool)) -> (bool, bool) {
    let now = ctx.input(|input| input.time);
    let previous: Option<Armed> = ctx.data(|data| data.get_temp(id));
    let mut armed = previous.filter(|armed| armed.shown == shown).unwrap_or(Armed { shown, since: now, moved: false, pressed: false });
    let ready = now - armed.since >= OFFER_INPUT_DELAY;
    // Input in the frame that first shows the change was aimed at what came before.
    if now > armed.since {
        let (moved, pressed) = ctx.input(|input| (input.pointer.delta() != egui::Vec2::ZERO, input.pointer.any_pressed()));
        armed.moved |= moved;
        armed.pressed |= pressed && ready;
    }
    if !ready { ctx.request_repaint_after(std::time::Duration::from_secs_f64(armed.since + OFFER_INPUT_DELAY - now)); }
    let counts = ready && (armed.moved || armed.pressed);
    ctx.data_mut(|data| data.insert_temp(id, armed));
    (ready, counts)
}

/// Where the offer sits: the pane's top right, below the Find bar if it is
/// open on this pane, never wider than the pane allows, and down to 10
/// points above the pane's bottom at most.
pub fn offer_rect(pane: Rect, below_find: bool) -> Rect {
    let width = OFFER_WIDTH.min(pane.width() - 20.0).max(0.0);
    let top = pane.top() + if below_find { 10.0 + 40.0 + 6.0 } else { 10.0 };
    Rect::from_min_max(pos2(pane.right() - width - 10.0, top), pos2(pane.right() - 10.0, (pane.bottom() - 10.0).max(top)))
}

/// One offer's card. Non-modal: the page keeps working beneath it. It takes
/// no input for a moment after it appears or changes; see [`arming`].
pub fn offer_card(ctx: &egui::Context, at: Rect, offer: &mut Offer, pal: &Palette) -> Option<OfferRequest> {
    let ink = Ink::new(pal.panel, pal.accent);
    let id = field_id(&offer.tab);
    let (ready, counts) = arming(ctx, Id::unique(("login-offer-armed", &offer.tab)), (offer.revision, Some(offer.kind.clone()), offer.unlocking));
    // Escape is the app's: see dismiss_focused_offer.
    let enter = ready && ctx.memory(|memory| memory.has_focus(id)) && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Enter));
    let mut request = enter.then_some(OfferRequest::Save);
    egui::Area::new(Id::unique(("login-offer", &offer.tab))).order(egui::Order::Foreground).fixed_pos(at.min).show(ctx, |ui| {
        let frame = crate::ui::overlays::menu_frame(pal).inner_margin(egui::Margin::same(12));
        frame.show(ui, |ui| {
            ui.set_width(at.width() - frame.total_margin().sum().x);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.add(egui::Label::new(style::text(title(offer), 13.5, style::weight::SEMIBOLD).color(pal.panel_text)).wrap());
            if offer.unlocking {
                ui.add(egui::Label::new(style::text(LOCKED, 12.5, style::weight::REGULAR).color(ink.secondary)).wrap());
            } else {
                style::field_label(ui, &ink, "Username");
                ui.add(egui::TextEdit::singleline(&mut *offer.username)
                    .id(id)
                    .hint_text(style::text(NO_USERNAME, 13.5, style::weight::REGULAR).color(ink.secondary))
                    .font(egui::FontId::proportional(13.5))
                    .text_color(ink.text)
                    .char_limit(crossbow_core::vault::MAX_USERNAME_BYTES)
                    .desired_width(ui.available_width()));
                forget_undo(ui.ctx(), id);
            }
            if let Some(error) = &offer.error {
                ui.add(egui::Label::new(style::text(error.to_string(), 12.5, style::weight::REGULAR).color(ink.danger)).wrap());
            }
            ui.horizontal_wrapped(|ui| {
                let primary = match (&offer.kind, offer.unlocking) {
                    (_, true) => ("Unlock", OfferRequest::Unlock),
                    (OfferKind::Save, false) => ("Save", OfferRequest::Save),
                    (OfferKind::Update(_), false) => ("Update Password", OfferRequest::Save),
                };
                if Button::new(primary.0).kind(Kind::Primary).size(Size::Small).enabled(ready).show(ui, &ink).clicked() && counts { request = Some(primary.1); }
                if offer.kind == OfferKind::Save && !offer.unlocking && Button::new("Never for This Site").kind(Kind::Plain).size(Size::Small).enabled(ready).show(ui, &ink).clicked() && counts {
                    request = Some(OfferRequest::Never);
                }
                if Button::new("Not Now").kind(Kind::Plain).size(Size::Small).enabled(ready).show(ui, &ink).clicked() && counts { request = Some(OfferRequest::NotNow); }
            });
        });
    });
    // Enter on the unlock step unlocks.
    if offer.unlocking && request == Some(OfferRequest::Save) { request = Some(OfferRequest::Unlock); }
    request
}

/// The least height the address card's parts keep when the rest of the card
/// already fills its pane.
const MIN_PARTS_HEIGHT: f32 = 48.0;

/// A submitted address's card, as a login's: every part the page sent, a
/// label to save it under, and Save Address, Never for This Site and Not Now.
/// It ends within `at`: a page can send parts of up to 1,024 bytes, so the
/// parts scroll between the title and the label, and the buttons stay in
/// the window.
pub fn address_offer_card(ctx: &egui::Context, at: Rect, offer: &mut AddressOffer, pal: &Palette) -> Option<OfferRequest> {
    let ink = Ink::new(pal.panel, pal.accent);
    let id = label_id(&offer.tab);
    let (ready, counts) = arming(ctx, Id::unique(("address-offer-armed", &offer.tab)), (offer.revision, None, offer.unlocking));
    let enter = ready && ctx.memory(|memory| memory.has_focus(id)) && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Enter));
    let mut request = enter.then_some(OfferRequest::Save);
    egui::Area::new(Id::unique(("address-offer", &offer.tab))).order(egui::Order::Foreground).fixed_pos(at.min).show(ctx, |ui| {
        let frame = crate::ui::overlays::menu_frame(pal).inner_margin(egui::Margin::same(12));
        frame.show(ui, |ui| {
            ui.set_width(at.width() - frame.total_margin().sum().x);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.add(egui::Label::new(style::text(format!("Save address from {}?", site(&offer.origin)), 13.5, style::weight::SEMIBOLD).color(pal.panel_text)).wrap());
            // The parts get the height the rest of the card left last frame.
            let rest_id = Id::unique(("address-offer-rest", &offer.tab));
            let room = (at.height() - frame.total_margin().sum().y).max(0.0);
            let rest = ctx.data(|data| data.get_temp::<f32>(rest_id)).unwrap_or(room / 2.0);
            // Every part Save Address keeps, so nothing is saved unseen.
            let part = |field| offer.address.get(field).trim().to_owned();
            let parts = egui::ScrollArea::vertical().id_salt(("address-offer-parts", &offer.tab)).max_height((room - rest).max(MIN_PARTS_HEIGHT)).auto_shrink([false, true]).show(ui, |ui| {
                for line in [part(AddressField::Name), part(AddressField::Organization), address_summary(&offer.address), part(AddressField::Phone), part(AddressField::Email)] {
                    if !line.is_empty() { ui.add(egui::Label::new(style::text(line, 12.5, style::weight::REGULAR).color(ink.secondary)).wrap()); }
                }
            });
            if offer.unlocking {
                ui.add(egui::Label::new(style::text(ADDRESS_LOCKED, 12.5, style::weight::REGULAR).color(ink.secondary)).wrap());
            } else {
                style::field_label(ui, &ink, "Label");
                ui.add(egui::TextEdit::singleline(&mut **offer.address.part_mut(AddressField::Label))
                    .id(id)
                    .hint_text(style::text("For example, Home", 13.5, style::weight::REGULAR).color(ink.secondary))
                    .font(egui::FontId::proportional(13.5))
                    .text_color(ink.text)
                    .char_limit(AddressField::Label.limit())
                    .desired_width(ui.available_width()));
                forget_undo(ui.ctx(), id);
            }
            if let Some(error) = &offer.error {
                ui.add(egui::Label::new(style::text(error.to_string(), 12.5, style::weight::REGULAR).color(ink.danger)).wrap());
            }
            ui.horizontal_wrapped(|ui| {
                let primary = if offer.unlocking { ("Unlock", OfferRequest::Unlock) } else { ("Save Address", OfferRequest::Save) };
                if Button::new(primary.0).kind(Kind::Primary).size(Size::Small).enabled(ready).show(ui, &ink).clicked() && counts { request = Some(primary.1); }
                if !offer.unlocking && Button::new("Never for This Site").kind(Kind::Plain).size(Size::Small).enabled(ready).show(ui, &ink).clicked() && counts {
                    request = Some(OfferRequest::Never);
                }
                if Button::new("Not Now").kind(Kind::Plain).size(Size::Small).enabled(ready).show(ui, &ink).clicked() && counts { request = Some(OfferRequest::NotNow); }
            });
            let now = ui.min_rect().height() - parts.inner_rect.height();
            if (now - rest).abs() > 0.5 {
                ctx.data_mut(|data| data.insert_temp(rest_id, now));
                ctx.request_repaint();
            }
        });
    });
    if offer.unlocking && request == Some(OfferRequest::Save) { request = Some(OfferRequest::Unlock); }
    request
}

/// The offer's username keeps no undo history, like the vault's own fields.
fn forget_undo(ctx: &egui::Context, id: Id) {
    if let Some(mut state) = egui::text_edit::TextEditState::load(ctx, id) {
        state.clear_undoer();
        ctx.data_mut(|data| data.insert_temp(id, state));
    }
}

/// Drop egui's copy of a closed offer's username.
fn forget_field(ctx: &egui::Context, tab: &str) {
    let id = field_id(tab);
    ctx.data_mut(|data| data.remove::<egui::text_edit::TextEditState>(id));
    ctx.memory_mut(|memory| if memory.has_focus(id) { memory.surrender_focus(id) });
}

/// Apply what an offer asked for. Returns the notice to show, if any.
pub fn apply_offer(passwords: &mut Passwords, ctx: &egui::Context, tab: &str, request: OfferRequest) -> Option<&'static str> {
    let notice = match request {
        OfferRequest::Save => passwords.save_offer(tab),
        OfferRequest::Unlock => passwords.unlock_for_offer(tab),
        OfferRequest::Never => passwords.never_for_offer(tab),
        OfferRequest::NotNow => {
            passwords.dismiss_offer(tab);
            None
        }
    };
    if passwords.offer(tab).is_none() { forget_field(ctx, tab); }
    notice
}

/// Apply what an address offer asked for. Returns the notice to show, if any.
pub fn apply_address_offer(passwords: &mut Passwords, ctx: &egui::Context, tab: &str, request: OfferRequest) -> Option<&'static str> {
    let notice = match request {
        OfferRequest::Save => passwords.save_address_offer(tab),
        OfferRequest::Unlock => passwords.unlock_for_address_offer(tab),
        OfferRequest::Never => passwords.never_for_address_offer(tab),
        OfferRequest::NotNow => {
            passwords.dismiss_address_offer(tab);
            None
        }
    };
    if passwords.address_offer(tab).is_none() {
        let id = label_id(tab);
        ctx.data_mut(|data| data.remove::<egui::text_edit::TextEditState>(id));
        ctx.memory_mut(|memory| if memory.has_focus(id) { memory.surrender_focus(id) });
    }
    notice
}

/// Escape from an offer's username, which the app's shortcuts take before
/// the card is drawn and after egui has dropped the field's focus: Not Now.
pub fn dismiss_focused_offer(ctx: &egui::Context, passwords: &mut Passwords) -> bool {
    let had_focus = |tab: &str| ctx.memory(|memory| memory.has_focus(field_id(tab)) || memory.had_focus_last_frame(field_id(tab)));
    let login = passwords.offer_tabs().find(|tab| had_focus(tab)).map(str::to_owned);
    if let Some(tab) = login {
        apply_offer(passwords, ctx, &tab, OfferRequest::NotNow);
        return true;
    }
    let had_focus = |tab: &str| ctx.memory(|memory| memory.has_focus(label_id(tab)) || memory.had_focus_last_frame(label_id(tab)));
    let Some(tab) = passwords.address_offer_tabs().find(|tab| had_focus(tab)).map(str::to_owned) else { return false };
    apply_address_offer(passwords, ctx, &tab, OfferRequest::NotNow);
    true
}

/// Escape while `tab`, the focused pane's, shows an address offer, whatever
/// has focus: Not Now. Unlike a login's, the card can be long enough that
/// its Label is never clicked, so Escape doesn't wait for it. An offer
/// waiting behind a login's isn't shown, so it stays; so does one waiting
/// for a permission prompt, whose pane [`focused_offer_tab`] doesn't give.
pub fn dismiss_shown_address_offer(ctx: &egui::Context, tab: Option<&str>, passwords: &mut Passwords) -> bool {
    let Some(tab) = tab.filter(|tab| passwords.offer(tab).is_none() && passwords.address_offer(tab).is_some()).map(str::to_owned) else { return false };
    apply_address_offer(passwords, ctx, &tab, OfferRequest::NotNow);
    true
}

/// The focused pane's tab while its offers are drawn, for Escape: not while
/// a permission prompt or notice goes first ([`waits`]).
pub fn focused_offer_tab(app: &CrossbowApp, screen: Rect) -> Option<String> {
    let tab = app.focused_tab_id()?;
    let pane = app.ui.panes.iter().find(|(id, _)| *id == tab).map(|(_, pane)| *pane)?;
    let at = offer_rect(pane, app.ui.find.is_some());
    (!waits(&app.site_permissions, &tab, at, crate::ui::site_permissions::shown_column(app, screen))).then_some(tab)
}

/// Whether `tab`'s offer at `at` waits, as the Translate card does, so two
/// cards never share a place: a permission prompt or notice on that page,
/// or the focused tab's drawn over `at` (`prompt`, from
/// [`crate::ui::site_permissions::shown_column`]), goes first.
pub(crate) fn waits(perms: &crate::site_permissions::SitePermissions, tab: &str, at: Rect, prompt: Option<Rect>) -> bool {
    crate::ui::site_permissions::goes_first(perms, tab, at, prompt)
}

/// Every visible pane's offer, while nothing covers the window: a login's
/// first, and a pane's address offer once it has none.
pub fn offers(app: &mut CrossbowApp, ui: &mut Ui, pal: &Palette) {
    if app.ui.settings.is_some() || app.ui.command.is_some() { return; }
    let ctx = ui.ctx().clone();
    let find_tab = app.ui.find.is_some().then(|| app.focused_tab_id()).flatten();
    let prompt = crate::ui::site_permissions::shown_column(app, ctx.content_rect());
    let panes = app.ui.panes.clone();
    for (tab, pane) in panes {
        let at = offer_rect(pane, find_tab.as_deref() == Some(tab.as_str()));
        let Some(notice) = pane_offer(&ctx, &mut app.passwords, &app.site_permissions, &tab, at, prompt, pal) else { continue };
        if let Some(notice) = notice { app.actions.push(Action::Toast(notice.into())); }
        ctx.request_repaint();
    }
}

/// One pane's offer at `at`, as [`offers`] draws it: a login's first, and
/// the address offer once there is none, unless it [`waits`]. Back after
/// waiting, it takes no input for a moment, as after any change. Some when
/// a choice was made, with its notice.
pub(crate) fn pane_offer(ctx: &egui::Context, passwords: &mut Passwords, perms: &crate::site_permissions::SitePermissions, tab: &str, at: Rect, prompt: Option<Rect>, pal: &Palette) -> Option<Option<&'static str>> {
    if waits(perms, tab, at, prompt) {
        ctx.data_mut(|data| {
            data.remove::<Armed>(Id::unique(("login-offer-armed", tab)));
            data.remove::<Armed>(Id::unique(("address-offer-armed", tab)));
        });
        return None;
    }
    if let Some(offer) = passwords.offer_mut(tab) {
        let request = offer_card(ctx, at, offer, pal)?;
        Some(apply_offer(passwords, ctx, tab, request))
    } else if let Some(offer) = passwords.address_offer_mut(tab) {
        let request = address_offer_card(ctx, at, offer, pal)?;
        Some(apply_address_offer(passwords, ctx, tab, request))
    } else {
        None
    }
}

/// The page a key button acts on: its origin and the tab's profile, for a
/// live http(s) page whose context belongs to that profile and whose
/// document's own origin is its address's, never an opaque one.
pub fn page(app: &CrossbowApp, tab: &str) -> Option<(String, String, crossbow_engine::WebView)> {
    let t = app.tab(tab)?;
    let view = t.view.clone()?;
    let origin = view.login_origin()?;
    let profile = crate::app::tab_profile(&app.lib, t)?.to_owned();
    Some((origin, profile, view))
}

/// The frame whose address field last had focus on a tab's page: its
/// origin, which the engine checked is a live http(s) one, the tab's
/// profile, and the view. The frame is the page itself or a frame
/// same-origin with it and all its ancestors: only those get the login
/// script, so a frame from another site, such as an embedded checkout,
/// never reports focus and is never filled.
pub fn address_page(app: &CrossbowApp, tab: &str) -> Option<(String, String, crossbow_engine::WebView)> {
    let t = app.tab(tab)?;
    let view = t.view.clone()?;
    let origin = view.address_focus()?;
    let profile = crate::app::tab_profile(&app.lib, t)?.to_owned();
    Some((origin, profile, view))
}

/// Whether "Fill Address ›" applies to a tab: an address field had focus,
/// and its profile has addresses, or the locked vault holds any.
pub fn can_fill_address(app: &mut CrossbowApp, tab: &str) -> bool {
    let Some((_, profile, _)) = address_page(app, tab) else { return false };
    app.passwords.open_for_pages();
    app.passwords.address_key_state(&profile).is_some()
}

/// Where a card would go on a tab's page, once a card field there had
/// focus: the origin of the frame the engine would fill, that card field's
/// while its frame still has focus, else the page's own, only for an https
/// page or a frame inside one (or loopback, for a test fixture); the tab's
/// profile; and the view. The frame may be another site's, such as a
/// payment processor's card fields; a fill then reaches only its fields.
pub fn card_page(app: &CrossbowApp, tab: &str) -> Option<(String, String, crossbow_engine::WebView)> {
    let t = app.tab(tab)?;
    let view = t.view.clone()?;
    view.card_focus()?;
    let origin = view.card_origin()?;
    let profile = crate::app::tab_profile(&app.lib, t)?.to_owned();
    Some((origin, profile, view))
}

/// Whether "Fill Card ›" applies to a tab: a card field had focus on an
/// https page, and its profile has cards, or the locked vault holds any.
pub fn can_fill_card(app: &mut CrossbowApp, tab: &str) -> bool {
    let Some((_, profile, _)) = card_page(app, tab) else { return false };
    app.passwords.open_for_pages();
    app.passwords.card_key_state(&profile).is_some()
}

/// What the key button can do for the focused tab, if it shows: its page's
/// logins first, else the addresses for the address field that had focus,
/// else the cards for the card field that had focus.
pub fn key_state(app: &mut CrossbowApp) -> Option<(String, KeyState)> {
    let tab = app.focused_tab_id()?;
    app.passwords.open_for_pages();
    let logins = page(app, &tab).and_then(|(origin, profile, view)| {
        let login_field = view.login_focus().as_deref() == Some(origin.as_str());
        app.passwords.key_state(&origin, &profile, login_field)
    });
    let addresses = address_page(app, &tab).and_then(|(_, profile, _)| app.passwords.address_key_state(&profile));
    let cards = card_page(app, &tab).and_then(|(_, profile, _)| app.passwords.card_key_state(&profile));
    let state = match (logins, addresses.or(cards)) {
        (Some(KeyState::Logins(count)), _) => KeyState::Logins(count),
        (_, Some(state)) | (Some(state), None) => state,
        (None, None) => return None,
    };
    Some((tab, state))
}

/// The key button at `rect`, when the focused page has logins to fill.
/// Returns whether it is drawn.
pub fn key_button(app: &mut CrossbowApp, ui: &mut Ui, rect: Rect, color: egui::Color32, hover: egui::Color32) -> bool {
    let Some((tab, state)) = key_state(app) else { return false };
    let tip = match state {
        KeyState::Logins(1) => "Fill the saved password".to_owned(),
        KeyState::Logins(n) => format!("Fill one of {n} saved passwords"),
        KeyState::Locked => "Unlock passwords to fill this page".to_owned(),
        KeyState::Addresses(1) => "Fill the saved address".to_owned(),
        KeyState::Addresses(n) => format!("Fill one of {n} saved addresses"),
        KeyState::Cards(1) => "Fill the saved card".to_owned(),
        KeyState::Cards(n) => format!("Fill one of {n} saved cards"),
    };
    if icons::button(ui, rect, Icon::Key, color, hover, true).on_hover_text(tip).clicked() {
        app.ui.menu = Some(MenuState { pos: pos2(rect.left(), rect.bottom() + 4.0), kind: MenuKind::Logins { tab } });
    }
    true
}

/// What a click in the key button's menu asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuPick {
    Unlock,
    Fill(String),
    Manage,
    /// "Fill Address ›": the address menu replaces this one.
    Addresses,
    /// "Fill Card ›": the card menu replaces this one.
    Cards,
}

/// The key button's menu for a page of `origin` in `profile`: Unlock while
/// locked, then that page's own logins by username, "Fill Address ›" when
/// `address_field` had focus and the profile has addresses, and "Fill Card
/// ›" when `card_field` had focus and the profile has cards. A page with
/// only an address or card field skips the empty password list.
pub fn menu_entries(m: &MenuUi<'_>, ui: &mut Ui, passwords: &Passwords, origin: Option<&str>, profile: &str, address_field: bool, card_field: bool) -> Option<MenuPick> {
    let mut pick = None;
    if !passwords.unlocked() {
        m.heading(ui, LOCKED_HEADING);
        if m.item(ui, UNLOCK, "", true) { pick = Some(MenuPick::Unlock); }
    } else {
        let addresses = address_field && !passwords.addresses_for(profile).is_empty();
        let cards = card_field && !passwords.cards_for(profile).is_empty();
        let logins = origin.map(|origin| (origin, passwords.logins_for(origin, profile)));
        if let Some((origin, logins)) = logins.filter(|(_, logins)| !logins.is_empty() || !(addresses || cards)) {
            m.heading(ui, &format!("Fill a password for {}", site(origin)));
            if logins.is_empty() { m.item(ui, NO_LOGINS, "", false); }
            for (id, username) in logins {
                let label = if username.is_empty() { NO_USERNAME.to_owned() } else { username.to_string() };
                if m.item(ui, &label, "", true) { pick = Some(MenuPick::Fill(id)); }
            }
            if addresses || cards { m.sep(ui); }
        }
        if addresses && m.item(ui, FILL_ADDRESS, "›", true) { pick = Some(MenuPick::Addresses); }
        if cards && m.item(ui, FILL_CARD, "›", true) { pick = Some(MenuPick::Cards); }
    }
    m.sep(ui);
    if m.item(ui, MANAGE, "", true) { pick = Some(MenuPick::Manage); }
    pick
}

/// What applying a menu pick asks of the app.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MenuOutcome {
    pub close: bool,
    pub toast: Option<String>,
    /// Open Settings → Passwords on this list.
    pub manage: Option<Section>,
    /// Replace the menu with the address menu.
    pub addresses: bool,
    /// Replace the menu with the card menu.
    pub cards: bool,
}

/// Apply a pick. `fill` sends a login to the page and says whether a frame
/// of `origin` took it. Unlock keeps the menu open, to show the logins.
pub fn apply_pick(passwords: &mut Passwords, pick: MenuPick, origin: &str, profile: &str, fill: impl FnOnce(&str, &str) -> bool) -> MenuOutcome {
    match pick {
        MenuPick::Unlock => {
            passwords.unlock();
            let error = passwords.page.error.as_ref().map(ToString::to_string);
            MenuOutcome { close: error.is_some(), toast: error, ..MenuOutcome::default() }
        }
        MenuPick::Fill(id) => {
            let toast = match passwords.login_to_fill(&id, origin, profile) {
                Ok((username, password)) => (!fill(&username, &password)).then(|| PAGE_MOVED.to_owned()),
                Err(message) => Some(message.to_owned()),
            };
            MenuOutcome { close: true, toast, ..MenuOutcome::default() }
        }
        MenuPick::Manage => MenuOutcome { close: true, manage: Some(Section::Passwords), ..MenuOutcome::default() },
        MenuPick::Addresses => MenuOutcome { addresses: true, ..MenuOutcome::default() },
        MenuPick::Cards => MenuOutcome { cards: true, ..MenuOutcome::default() },
    }
}

/// What a click in the address menu asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddressPick {
    Unlock,
    Fill(String),
    Manage,
}

/// "Fill Address ›", from the key button or a field's context menu, for a
/// frame of `origin` in `profile`: Unlock while locked, then the profile's
/// addresses by label.
pub fn address_entries(m: &MenuUi<'_>, ui: &mut Ui, passwords: &Passwords, origin: &str, profile: &str) -> Option<AddressPick> {
    let mut pick = None;
    if !passwords.unlocked() {
        m.heading(ui, LOCKED_HEADING);
        if m.item(ui, UNLOCK, "", true) { pick = Some(AddressPick::Unlock); }
    } else {
        let addresses = passwords.addresses_for(profile);
        m.heading(ui, &format!("Fill an address on {}", site(origin)));
        if addresses.is_empty() { m.item(ui, NO_ADDRESSES, "", false); }
        for (id, title) in addresses {
            if m.item(ui, &title, "", true) { pick = Some(AddressPick::Fill(id)); }
        }
    }
    m.sep(ui);
    if m.item(ui, MANAGE_ADDRESSES, "", true) { pick = Some(AddressPick::Manage); }
    pick
}

/// Apply an address menu pick. `fill` sends the parts to the page and says
/// whether they went where the menu was opened; if not, nothing was sent.
/// Unlock keeps the menu open.
pub fn apply_address_pick(passwords: &mut Passwords, pick: AddressPick, profile: &str, fill: impl FnOnce(&[&str; crossbow_engine::ADDRESS_PARTS]) -> bool) -> MenuOutcome {
    match pick {
        AddressPick::Unlock => apply_pick(passwords, MenuPick::Unlock, "", profile, |_, _| false),
        AddressPick::Fill(id) => {
            let toast = match passwords.address_to_fill(&id, profile) {
                Ok(parts) => (!fill(&parts.each_ref().map(|part| part.as_str()))).then(|| ADDRESS_MOVED.to_owned()),
                Err(message) => Some(message.to_owned()),
            };
            MenuOutcome { close: true, toast, ..MenuOutcome::default() }
        }
        AddressPick::Manage => MenuOutcome { close: true, manage: Some(Section::Addresses), ..MenuOutcome::default() },
    }
}

/// What a click in the card menu asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardPick {
    Unlock,
    Fill(String),
    Manage,
}

/// "Fill Card ›", from the key button or a field's context menu, for a
/// frame of `origin` in `profile`: Unlock while locked, then the profile's
/// cards by network and last four digits, each with its expiry. Never a
/// full number, a name or a security code.
pub fn card_entries(m: &MenuUi<'_>, ui: &mut Ui, passwords: &Passwords, origin: &str, profile: &str) -> Option<CardPick> {
    let mut pick = None;
    if !passwords.unlocked() {
        m.heading(ui, LOCKED_HEADING);
        if m.item(ui, UNLOCK, "", true) { pick = Some(CardPick::Unlock); }
    } else {
        let cards = passwords.cards_for(profile);
        m.heading(ui, &format!("Fill a card on {}", site(origin)));
        if cards.is_empty() { m.item(ui, NO_CARDS, "", false); }
        for (id, label, expiry) in cards {
            if m.item(ui, &label, &expiry, true) { pick = Some(CardPick::Fill(id)); }
        }
    }
    m.sep(ui);
    if m.item(ui, MANAGE_CARDS, "", true) { pick = Some(CardPick::Manage); }
    pick
}

/// Apply a card menu pick. `fill` sends the card to the page, its number
/// opened for this fill only, and says whether it went where the menu was
/// opened; if not, nothing was sent. Unlock keeps the menu open.
pub fn apply_card_pick(passwords: &mut Passwords, pick: CardPick, profile: &str, fill: impl FnOnce(&[&str; crossbow_engine::CARD_PARTS]) -> bool) -> MenuOutcome {
    match pick {
        CardPick::Unlock => apply_pick(passwords, MenuPick::Unlock, "", profile, |_, _| false),
        CardPick::Fill(id) => {
            let toast = match passwords.card_to_fill(&id, profile) {
                Ok(parts) => (!fill(&parts.each_ref().map(|part| part.as_str()))).then(|| CARD_MOVED.to_owned()),
                Err(message) => Some(message.to_owned()),
            };
            MenuOutcome { close: true, toast, ..MenuOutcome::default() }
        }
        CardPick::Manage => MenuOutcome { close: true, manage: Some(Section::Cards), ..MenuOutcome::default() },
    }
}

/// Show a pick's toast and open Settings when it asks to.
fn follow(app: &mut CrossbowApp, outcome: &mut MenuOutcome) {
    if let Some(toast) = outcome.toast.take() { app.actions.push(Action::Toast(toast)); }
    if let Some(section) = outcome.manage {
        app.passwords.show_section(section);
        app.ui.settings_page_hint = Some(crate::ui::settings::Page::Passwords);
        app.actions.push(Action::OpenSettings);
    }
}

/// The key button's menu, for the tab it opened on. Returns true when it should close.
pub fn menu(app: &mut CrossbowApp, m: &MenuUi<'_>, ui: &mut Ui, tab: &str) -> bool {
    let login = page(app, tab);
    let address_field = can_fill_address(app, tab);
    let card_field = can_fill_card(app, tab);
    let profile = login.as_ref().map(|(_, profile, _)| profile.clone())
        .or_else(|| address_page(app, tab).map(|(_, profile, _)| profile))
        .or_else(|| card_page(app, tab).map(|(_, profile, _)| profile));
    let Some(profile) = profile else { return true };
    if login.is_none() && !address_field && !card_field { return true; }
    let origin = login.as_ref().map(|(origin, ..)| origin.as_str());
    let Some(pick) = menu_entries(m, ui, &app.passwords, origin, &profile, address_field, card_field) else { return false };
    let mut outcome = apply_pick(&mut app.passwords, pick, origin.unwrap_or_default(), &profile, |username, password| {
        login.as_ref().is_some_and(|(origin, _, view)| view.fill_login(origin, username, password))
    });
    follow(app, &mut outcome);
    if outcome.addresses {
        let keyboard = ui.input(|input| input.key_pressed(Key::Enter) || input.key_pressed(Key::Space));
        let pos = app.ui.menu.as_ref().map_or(ui.min_rect().min, |state| state.pos);
        let Some(menu) = freeze_address_menu(app, tab) else {
            app.actions.push(Action::Toast(ADDRESS_MOVED.into()));
            return true;
        };
        app.ui.menu = Some(crate::ui::folders::open_menu(ui.ctx(), pos, MenuKind::Addresses(menu), keyboard));
        return false;
    }
    if outcome.cards {
        let keyboard = ui.input(|input| input.key_pressed(Key::Enter) || input.key_pressed(Key::Space));
        let pos = app.ui.menu.as_ref().map_or(ui.min_rect().min, |state| state.pos);
        let Some(menu) = freeze_card_menu(app, tab) else {
            app.actions.push(Action::Toast(CARD_MOVED.into()));
            return true;
        };
        app.ui.menu = Some(crate::ui::folders::open_menu(ui.ctx(), pos, MenuKind::Cards(menu), keyboard));
        return false;
    }
    outcome.close
}

/// The address menu, as it was frozen when it opened. Returns true when it should close.
pub fn address_menu(app: &mut CrossbowApp, m: &MenuUi<'_>, ui: &mut Ui, menu: &FillMenu) -> bool {
    let Some(view) = frozen_view(app, menu, |view, target| view.address_target_holds(target)) else {
        app.actions.push(Action::Toast(ADDRESS_MOVED.into()));
        return true;
    };
    let Some(pick) = address_entries(m, ui, &app.passwords, menu.target.origin(), &menu.profile) else { return false };
    let mut outcome = apply_address_pick(&mut app.passwords, pick, &menu.profile, |parts| view.fill_address_at(&menu.target, parts));
    follow(app, &mut outcome);
    outcome.close
}

/// The address menu opening now on `tab`, frozen as [`FillMenu`]. None
/// when no frame of its page may take the address.
pub fn freeze_address_menu(app: &CrossbowApp, tab: &str) -> Option<FillMenu> {
    let t = app.tab(tab)?;
    let view = t.view.clone()?;
    let profile = crate::app::tab_profile(&app.lib, t)?.to_owned();
    let target = view.address_target()?;
    Some(FillMenu { tab: tab.to_owned(), view: view.serial(), profile, target })
}

/// Where a card or address menu fills, frozen when it opens (C10-1): its
/// tab, that tab's page view and profile, and the engine's frame, document,
/// origin, note and focus there. The menu fills only while all of it still
/// holds.
#[derive(Clone, Debug)]
pub struct FillMenu {
    pub tab: String,
    view: u64,
    profile: String,
    target: crossbow_engine::FillTarget,
}

/// The card menu opening now on `tab`, frozen as [`FillMenu`]. None when no
/// frame of its page may take a card.
pub fn freeze_card_menu(app: &CrossbowApp, tab: &str) -> Option<FillMenu> {
    let t = app.tab(tab)?;
    let view = t.view.clone()?;
    let profile = crate::app::tab_profile(&app.lib, t)?.to_owned();
    let target = view.card_target()?;
    Some(FillMenu { tab: tab.to_owned(), view: view.serial(), profile, target })
}

/// Whether a menu's frozen fill still holds: its tab is still the one in
/// front, so no tab switch and no closed window, with the profile and page
/// view it opened with, and the `engine` still sees the same frame,
/// document, origin, note and focus there.
pub fn menu_holds(menu: &FillMenu, front: Option<&str>, profile: Option<&str>, view: Option<u64>, engine: bool) -> bool {
    front == Some(menu.tab.as_str()) && profile == Some(menu.profile.as_str()) && view == Some(menu.view) && engine
}

/// The view a frozen menu fills through, while all it was frozen with still
/// holds (see [`menu_holds`]); `engine` asks the view about the target.
fn frozen_view(app: &CrossbowApp, menu: &FillMenu, engine: impl Fn(&crossbow_engine::WebView, &crossbow_engine::FillTarget) -> bool) -> Option<crossbow_engine::WebView> {
    let t = app.tab(&menu.tab);
    let view = t.and_then(|t| t.view.clone());
    let profile = t.and_then(|t| crate::app::tab_profile(&app.lib, t));
    let holds = view.as_ref().is_some_and(|view| engine(view, &menu.target));
    let serial = view.as_ref().map(|view| view.serial());
    view.filter(|_| menu_holds(menu, app.focused_tab_id().as_deref(), profile, serial, holds))
}

/// The card menu, as it was frozen when it opened. Returns true when it should close.
pub fn card_menu(app: &mut CrossbowApp, m: &MenuUi<'_>, ui: &mut Ui, menu: &FillMenu) -> bool {
    let Some(view) = frozen_view(app, menu, |view, target| view.card_target_holds(target)) else {
        app.actions.push(Action::Toast(CARD_MOVED.into()));
        return true;
    };
    let Some(pick) = card_entries(m, ui, &app.passwords, menu.target.origin(), &menu.profile) else { return false };
    let mut outcome = apply_card_pick(&mut app.passwords, pick, &menu.profile, |parts| view.fill_card_at(&menu.target, parts));
    follow(app, &mut outcome);
    outcome.close
}

#[cfg(test)]
#[path = "logins_tests.rs"]
mod tests;
