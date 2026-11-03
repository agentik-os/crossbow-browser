//! The page side of saving and filling website passwords.
//!
//! In each renderer, `login.js` is evaluated when a frame's script context is
//! created, only for an http(s) frame that is the main frame or same-origin
//! with every ancestor. It holds three native functions in its closure; they
//! send the username and password of a trusted, user-activated submit, and
//! content-free "a request followed" and "a login field has focus" notes, to
//! the browser process. The browser checks each message against its own view
//! of the sending frame, holds a submitted login for at most
//! [`FOLLOW_WINDOW`], and reports it as [`WebEvent::Credential`] only once the
//! page navigates or a request follows. Fills go the other way, to one frame
//! whose origin both processes check. Nothing here logs a message's contents.
//!
//! The browser takes logins from, fills and shows the key for only a frame
//! whose renderer reported, with [`INSTALLED`], that it installed `login.js`
//! in the frame's current document for the origin the browser sees. Each
//! report names the frame and an id the renderer gave that document, and
//! every later message and fill carries that id. A new document in the frame
//! ends the report, so a frame that never reports, as an opaque document
//! never does, gets nothing whatever its address later says.
//!
//! A document whose own origin is opaque, as in a sandboxed frame or a page
//! served with `Content-Security-Policy: sandbox`, or isn't the origin its
//! address names, holds no logins: the renderer reads the origin natively
//! when the script context is created, before any page script, and installs
//! nothing. As a second layer, the browser also treats a frame as opaque when
//! its response carried such a policy or its renderer said so. See
//! [`realm_allows`].
//!
//! Capture is behind the per-profile setting "Offer to save passwords from
//! websites", off by default. While it is off for a tab's profile, the
//! renderer installs `login.js` without its capture functions, so the page
//! gets no submit listener and no password leaves the renderer; the focus
//! note and the fill, for the key button, stay. The renderer learns the
//! setting from the browser's creation `extra_info`, its own launch command
//! line and [`SETTING`] messages, keeping the newest; see [`Gate`].
//!
//! Saved addresses take the same paths: `login.js` notes, content-free, that
//! a field of an address-shaped form has focus, and with capture on sends the
//! parts of a submitted one, which wait for their follow as a login does and
//! arrive as [`WebEvent::Address`]. A fill of an address goes, like a login,
//! to one frame that reported [`INSTALLED`] for the origin and document.
//!
//! Saved cards are only filled, never captured. A card may go to a frame of
//! another site than the page's, as a payment processor's card fields need,
//! but only to a secure https document, or http on this computer's loopback
//! address for a test fixture, inside pages that are each one too. The
//! browser decides that from its own frame tree, [`card_origin`]: under site
//! isolation such a frame's renderer can't see its parent, so it leaves
//! those ancestors to the browser, [`renderer_card_origin`]. Such a frame
//! gets `login.js` with the card note alone, reports [`INSTALLED`] like any
//! other, and counts for cards only. A card's number crosses once, in the
//! one [`FILL_CARD`] message the user's choice sends, and only to where the
//! card menu was opened: see [`FillTarget`].

use cef::*;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

pub(crate) const CAPTURE: &str = "crossbow.login.capture";
pub(crate) const FOLLOWED: &str = "crossbow.login.followed";
pub(crate) const FOCUSED: &str = "crossbow.login.focused";
pub(crate) const FILL: &str = "crossbow.login.fill";
/// The browser telling a renderer a profile's setting changed: profile,
/// generation, "1" or "0".
pub(crate) const SETTING: &str = "crossbow.login.setting";
/// A renderer telling the browser a frame's document has an opaque origin, or
/// one other than its address's. It can only make the browser stricter.
pub(crate) const OPAQUE: &str = "crossbow.login.opaque";
/// A renderer telling the browser a fill found no field it may fill.
pub(crate) const UNFILLED: &str = "crossbow.login.unfilled";
/// A submitted address: origin, document, then its [`ADDRESS_PARTS`] parts.
pub(crate) const ADDRESS_CAPTURE: &str = "crossbow.login.address";
/// Focus in a document moved to a field of another address-shaped form or
/// group than the last, or, once one had focus, to anything outside one:
/// origin, document, and the note's number in that document, from 1.
/// Content-free.
pub(crate) const ADDRESS_FOCUSED: &str = "crossbow.login.address_focused";
/// An address to fill: origin, document, the number of the address note the
/// menu was opened on, then [`ADDRESS_VALUES`] values.
pub(crate) const FILL_ADDRESS: &str = "crossbow.login.fill_address";
/// A renderer telling the browser an address fill found no field it may fill.
pub(crate) const ADDRESS_UNFILLED: &str = "crossbow.login.address_unfilled";
/// Focus in a document moved to a field of another card form or group than
/// the last, or, once one had focus, to anything outside one: origin,
/// document, and the note's number in that document, from 1. Content-free.
pub(crate) const CARD_FOCUSED: &str = "crossbow.login.card_focused";
/// A card to fill: origin, document, the number of the card note the menu
/// was opened on, then [`CARD_VALUES`] values.
pub(crate) const FILL_CARD: &str = "crossbow.login.fill_card";
/// A renderer telling the browser a card fill found no field it may fill.
pub(crate) const CARD_UNFILLED: &str = "crossbow.login.card_unfilled";
/// A renderer telling the browser what an address fill did once the engine
/// said which fields the user can see: origin, document, then a
/// [`FillReport`] as an outcome, the fields filled and the fields left empty.
pub(crate) const ADDRESS_FILLED: &str = "crossbow.login.address_filled";
/// The same, for a card fill.
pub(crate) const CARD_FILLED: &str = "crossbow.login.card_filled";
/// A report's outcomes, as login.js names them.
const FILL_DONE: &str = "done";
const FILL_FOCUS_HIDDEN: &str = "focus-hidden";
const FILL_UNCONFIRMED: &str = "unconfirmed";
/// The most fields login.js reads in one form or group (`MAX_FIELDS`).
const MAX_FILL_FIELDS: u16 = 512;
/// A renderer telling the browser it installed `login.js` in a frame's new
/// document: the origin the document had when its script context was
/// created, the id the renderer gave the document, and how many card and
/// address notes it sent from there. Every other message between the two
/// carries the origin and the id.
pub(crate) const INSTALLED: &str = "crossbow.login.installed";
/// A new browser's `extra_info` keys, and the renderer launch switch, whose
/// value is the generation, a colon and the profiles that have it on.
const INFO_PROFILE: &str = "crossbow.login.profile";
const INFO_GENERATION: &str = "crossbow.login.generation";
const INFO_CAPTURE: &str = "crossbow.login.capture";
/// "1" for a tab, which may offer to save; Peek and Little Windows never do.
const INFO_SAVES: &str = "crossbow.login.saves";
pub(crate) const SWITCH: &str = "crossbow-login-capture";
const SCRIPT: &str = include_str!("login.js");
const SCRIPT_URL: &str = "crossbow://login";

/// How long a submitted login waits for the navigation or request that
/// shows it was sent. Without one it is wiped and nothing is offered.
pub const FOLLOW_WINDOW: Duration = Duration::from_secs(30);
/// The vault's own limits. A longer value is never offered, since it couldn't be saved.
pub const MAX_USERNAME_BYTES: usize = 256;
pub const MAX_PASSWORD_BYTES: usize = 1024;
/// The parts of an address a page holds, in this order: name, organization,
/// street (one line per street line), city, region, postal code, country,
/// phone and email.
pub const ADDRESS_PARTS: usize = 9;
/// The vault's limit for each part, in the same order. A submitted address
/// with a longer part is never offered, since it couldn't be saved.
pub const MAX_ADDRESS_PART_BYTES: [usize; ADDRESS_PARTS] = [256, 256, 1024, 256, 256, 256, 256, 256, 256];
const STREET: usize = 2;
const CITY: usize = 3;
const POSTAL_CODE: usize = 5;
/// How many values an address fill sends; see [`address_values`].
const ADDRESS_VALUES: usize = 16;
/// The parts of a saved card a fill takes, in this order: the cardholder's
/// name, the number's digits, the expiry month as two digits and the year
/// as four. Never a security code, which the vault never holds.
pub const CARD_PARTS: usize = 4;
/// How many values a card fill sends; see [`card_values`].
const CARD_VALUES: usize = 6;
/// Hosts on this computer's loopback address, where a test fixture may take a card over http.
const LOOPBACK: [&str; 3] = ["http://localhost", "http://127.0.0.1", "http://[::1]"];
/// How long after a fill was sent its frame may say it filled nothing.
const UNFILLED_WINDOW: Duration = Duration::from_secs(5);
/// How many opaque documents a view remembers.
const DOCUMENTS_KEPT: usize = 128;
/// How many frames' reports a view keeps. A page with more same-origin
/// frames loses the oldest, which then hold no logins.
const FRAMES_KEPT: usize = 128;
/// The longest document id a renderer gives; a longer report is dropped.
const MAX_DOCUMENT_BYTES: usize = 64;

/// A username or password from a page. Its bytes, spare capacity included,
/// are zeroed when it is dropped, and Debug never shows it.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(text: String) -> Secret {
        Secret(text)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// Zero every byte `text` allocated, spare capacity included, and empty it.
fn wipe(text: &mut String) {
    // SAFETY: only zero bytes are written, and the length is then zero, so the string stays valid UTF-8.
    let bytes = unsafe { text.as_mut_vec() };
    for byte in bytes.iter_mut() {
        // SAFETY: a valid, aligned pointer to a byte this string owns.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    for byte in bytes.spare_capacity_mut() {
        // SAFETY: as above, for the allocated but unused tail.
        unsafe { std::ptr::write_volatile(byte.as_mut_ptr(), 0) };
    }
    bytes.clear();
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// A login a page submitted, after a navigation or request followed it.
/// `origin` is the frame's origin as the browser process saw it.
#[derive(Clone)]
pub struct LoginCapture {
    pub origin: String,
    pub username: Secret,
    pub password: Secret,
}

impl std::fmt::Debug for LoginCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginCapture").field("origin", &self.origin).finish_non_exhaustive()
    }
}

/// An address a page submitted, after a navigation or request followed it.
/// `origin` is the frame's origin as the browser process saw it; `parts` are
/// in [`ADDRESS_PARTS`] order. Debug shows neither.
#[derive(Clone)]
pub struct PageAddress {
    pub origin: String,
    pub parts: [Secret; ADDRESS_PARTS],
}

impl std::fmt::Debug for PageAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PageAddress(..)")
    }
}

/// Whether submitted parts make an address worth offering: a street and a
/// city or postal code, every part within the vault's limit.
fn address_shaped(parts: &[Secret]) -> bool {
    let filled = |i: usize| parts.get(i).is_some_and(|part| !part.expose().trim().is_empty());
    parts.len() == ADDRESS_PARTS && parts.iter().zip(MAX_ADDRESS_PART_BYTES).all(|(part, limit)| part.len() <= limit)
        && filled(STREET) && (filled(CITY) || filled(POSTAL_CODE))
}

/// The values an address fill sends, from parts in [`ADDRESS_PARTS`] order,
/// so `login.js` never splits a saved value: the name, its given and family
/// parts (all words but the last, and the last), the organization, the street
/// on its lines and on one line, its first line, its second, its second line
/// on, its third line on, then the city, region, postal code, country, phone
/// and email. Blank street lines are dropped.
pub(crate) fn address_values(parts: &[&str; ADDRESS_PARTS]) -> Vec<Secret> {
    let text = |value: &str| Secret::new(value.to_owned());
    let (given, family) = split_name(parts[0]);
    let lines: Vec<&str> = parts[STREET].lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let joined = |from: usize, separator: &str| Secret::new(lines.get(from..).unwrap_or_default().join(separator));
    let mut values = vec![text(parts[0]), text(given), text(family), text(parts[1]), joined(0, "\n"), joined(0, ", ")];
    values.extend([text(lines.first().copied().unwrap_or("")), text(lines.get(1).copied().unwrap_or("")), joined(1, ", "), joined(2, ", ")]);
    values.extend(parts[CITY..].iter().map(|part| text(part)));
    debug_assert_eq!(values.len(), ADDRESS_VALUES);
    values
}

/// A name's given and family parts: all words but the last, and the last.
fn split_name(name: &str) -> (&str, &str) {
    let name = name.trim();
    match name.rsplit_once(char::is_whitespace) {
        Some((given, family)) => (given.trim_end(), family),
        None => (name, ""),
    }
}

/// The values a card fill sends, from parts in [`CARD_PARTS`] order, so
/// `login.js` never splits a saved value: the name, its given and family
/// parts, split as an address's are, the digits, the month and the year.
pub(crate) fn card_values(parts: &[&str; CARD_PARTS]) -> Vec<Secret> {
    let text = |value: &str| Secret::new(value.to_owned());
    let (given, family) = split_name(parts[0]);
    let values = vec![text(parts[0]), text(given), text(family), text(parts[1]), text(parts[2]), text(parts[3])];
    debug_assert_eq!(values.len(), CARD_VALUES);
    values
}

/// The origin of an http or https address, as `scheme://host[:port]`. CEF
/// reports canonical addresses: a lowercase scheme and host, the host
/// IDNA-encoded and the default port dropped, the form the vault stores.
/// Anything else, `file:`, `data:`, `blob:`, internal pages and
/// addresses without a host included, has no origin here.
pub fn page_origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "http" && scheme != "https" { return None; }
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    // User info, which an address may still carry, is not part of the origin.
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let (name, port) = if host.starts_with('[') {
        let end = host.find(']')?;
        (&host[1..end], match &host[end + 1..] { "" => None, after => Some(after.strip_prefix(':')?) })
    } else {
        host.split_once(':').map_or((host, None), |(name, port)| (name, Some(port)))
    };
    let port_ok = port.is_none_or(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()));
    // Not canonical means not from CEF: fail closed rather than guess.
    let canonical = !host.bytes().any(|b| b.is_ascii_uppercase() || b.is_ascii_whitespace());
    (!name.is_empty() && port_ok && canonical).then(|| format!("{scheme}://{host}"))
}

/// What the origin rules need from a frame, so tests can use plain values.
pub(crate) trait FrameLike: Sized {
    fn id(&self) -> String;
    fn address(&self) -> String;
    fn main(&self) -> bool;
    fn parent_frame(&self) -> Option<Self>;
}

impl FrameLike for Frame {
    fn id(&self) -> String {
        CefString::from(&self.identifier()).to_string()
    }

    fn address(&self) -> String {
        CefString::from(&self.url()).to_string()
    }

    fn main(&self) -> bool {
        self.is_main() == 1
    }

    fn parent_frame(&self) -> Option<Frame> {
        self.parent()
    }
}

/// The frame's origin, when logins may be saved from and filled into it: an
/// http(s) frame that is the main frame, or same-origin with every ancestor
/// up to the main frame. A missing ancestor fails closed, as the renderer
/// sees a cross-process parent.
pub(crate) fn eligible_origin<F: FrameLike>(frame: &F) -> Option<String> {
    let origin = page_origin(&frame.address())?;
    let mut current = frame.parent_frame();
    let mut is_main = frame.main();
    // A frame tree is finite, but a broken one must not loop forever.
    for _ in 0..64 {
        if is_main { return Some(origin); }
        let parent = current?;
        if page_origin(&parent.address()).as_deref() != Some(origin.as_str()) { return None; }
        is_main = parent.main();
        current = parent.parent_frame();
    }
    None
}

/// Whether an origin is http on this computer's loopback address, any port.
fn loopback(origin: &str) -> bool {
    LOOPBACK.iter().any(|host| origin.strip_prefix(host).is_some_and(|rest| rest.is_empty() || rest.starts_with(':')))
}

/// The frame's origin, when a card may be filled into it: an https frame,
/// or an http one on this computer's loopback address, whose ancestors up
/// to the main frame are each one too. Unlike [`eligible_origin`], a frame
/// of another site than the page's qualifies, as a payment processor's card
/// fields do. A missing ancestor fails closed. This is the browser's rule,
/// over its own frame tree and the addresses each frame committed.
pub(crate) fn card_origin<F: FrameLike>(frame: &F) -> Option<String> {
    card_chain(frame, false)
}

/// The renderer's half of [`card_origin`], over the ancestors this process
/// can see. Under site isolation a frame of another site, such as a payment
/// processor's, runs in another process than its parent, which reads here
/// as missing. That ends the walk without refusing: the browser decides,
/// keeping the frame's report and taking its card note only for its own
/// [`card_origin`], and sending a card only to a frame it chose by that
/// rule. An ancestor this process can see still refuses here if it isn't
/// https or loopback; see also [`takes_cards`].
pub(crate) fn renderer_card_origin<F: FrameLike>(frame: &F) -> Option<String> {
    card_chain(frame, true)
}

/// The frame's https or loopback origin when each ancestor up to the main
/// frame is one too. A subframe's missing parent fails closed, unless
/// `defer`, when what lies beyond it is left to the browser.
fn card_chain<F: FrameLike>(frame: &F, defer: bool) -> Option<String> {
    let takes = |frame: &F| page_origin(&frame.address()).filter(|origin| origin.starts_with("https://") || loopback(origin));
    let origin = takes(frame)?;
    let mut current = frame.parent_frame();
    let mut is_main = frame.main();
    // A frame tree is finite, but a broken one must not loop forever.
    for _ in 0..64 {
        if is_main { return Some(origin); }
        let Some(parent) = current else { return defer.then_some(origin) };
        takes(&parent)?;
        is_main = parent.main();
        current = parent.parent_frame();
    }
    None
}

/// The origin a frame's listener is installed for: [`eligible_origin`], or,
/// in a frame of another site, [`card_origin`], where it serves cards only.
fn listener_origin<F: FrameLike>(frame: &F) -> Option<String> {
    eligible_origin(frame).or_else(|| card_origin(frame))
}

/// The renderer's [`listener_origin`]: logins and addresses still only for
/// a frame whose every ancestor it sees has its origin, and cards by
/// [`renderer_card_origin`].
pub(crate) fn renderer_listener_origin<F: FrameLike>(frame: &F) -> Option<String> {
    eligible_origin(frame).or_else(|| renderer_card_origin(frame))
}

/// What a document says about itself when its script context is created,
/// before any page script can replace a global: its serialized origin and
/// whether it is a secure context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Realm {
    pub origin: String,
    pub secure: bool,
}

/// Whether a document at an address with `url_origin` may hold logins: only
/// when its own origin is that origin, never an opaque one ("null"), and,
/// for an https address, only in a secure context.
pub(crate) fn realm_allows(url_origin: &str, document: Option<&Realm>) -> bool {
    document.is_some_and(|document| document.origin == url_origin && (document.secure || !url_origin.starts_with("https://")))
}

/// Whether a document in `frame`, in its renderer, may take a card: the
/// frame has a [`renderer_card_origin`], and the document, as it said when
/// its script context was created, has that origin and is a secure context,
/// as https and loopback documents are. The browser checks the frame again.
pub(crate) fn takes_cards<F: FrameLike>(frame: &F, document: Option<&Realm>) -> bool {
    renderer_card_origin(frame).is_some_and(|origin| document.is_some_and(|document| document.origin == origin && document.secure))
}

/// Whether response `policies`, the values of every
/// `Content-Security-Policy` header, sandbox the document without
/// allow-same-origin, which gives it an opaque origin. Each policy applies,
/// so one such sandbox is enough. Report-only policies are never enforced
/// and aren't passed here.
pub(crate) fn sandboxes_origin(policies: &[String]) -> bool {
    policies.iter().flat_map(|header| header.split(',')).flat_map(|policy| policy.split(';')).any(|directive| {
        let mut tokens = directive.split_ascii_whitespace();
        tokens.next().is_some_and(|name| name.eq_ignore_ascii_case("sandbox")) && !tokens.any(|token| token.eq_ignore_ascii_case("allow-same-origin"))
    })
}

/// An http(s) document's address without its fragment, which names the same document.
fn document_address(url: &str) -> Option<&str> {
    page_origin(url)?;
    Some(url.split_once('#').map_or(url, |(address, _)| address))
}

/// Documents whose address doesn't name their origin, in one view: those
/// whose latest response sandboxed them, by address, and frames whose
/// renderer said their document is opaque, by frame and address. The IO
/// thread records responses; the UI thread reads. A few hundred at most.
#[derive(Default)]
pub(crate) struct Documents {
    sandboxed: VecDeque<String>,
    noted: VecDeque<(String, String)>,
}

impl Documents {
    /// A document's response, with its `Content-Security-Policy` values.
    pub(crate) fn response(&mut self, url: &str, policies: &[String]) {
        let Some(address) = document_address(url) else { return };
        self.sandboxed.retain(|kept| kept != address);
        if !sandboxes_origin(policies) { return; }
        self.sandboxed.push_back(address.to_owned());
        if self.sandboxed.len() > DOCUMENTS_KEPT { self.sandboxed.pop_front(); }
    }

    fn noted(&mut self, frame: String, url: &str) {
        let Some(address) = document_address(url) else { return };
        let entry = (frame, address.to_owned());
        if self.noted.contains(&entry) { return; }
        self.noted.push_back(entry);
        if self.noted.len() > DOCUMENTS_KEPT { self.noted.pop_front(); }
    }

    /// Whether `frame`'s document, or an ancestor's, whose sandbox it
    /// inherits, is opaque. A broken frame tree counts as opaque.
    fn covers<F: FrameLike>(&self, frame: &F) -> bool {
        let opaque = |frame: &F| {
            let address = frame.address();
            let Some(address) = document_address(&address) else { return false };
            let id = frame.id();
            self.sandboxed.iter().any(|kept| kept == address) || self.noted.iter().any(|(noted, kept)| *noted == id && kept == address)
        };
        if opaque(frame) { return true; }
        let mut current = frame.parent_frame();
        for _ in 0..64 {
            let Some(parent) = current else { return false };
            if opaque(&parent) { return true; }
            current = parent.parent_frame();
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.sandboxed.len() + self.noted.len()
    }
}

/// What an address or card fill did, as its frame's renderer reported it,
/// once the engine said which fields the user can see (IntersectionObserver
/// v2: see login.js). Counts only, never a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillReport {
    /// `filled` fields took their value; `hidden` more would have, but the
    /// engine didn't call them clearly visible, so they were left empty.
    /// Both zero: nothing could be filled after all.
    Done { filled: u16, hidden: u16 },
    /// The focused field isn't clearly visible, so nothing was filled.
    FocusHidden,
    /// No answer came within login.js's wait, or the engine can't give one,
    /// so nothing was filled.
    Unconfirmed,
}

/// A report's outcome and counts, as login.js sends them; None for anything else.
fn fill_report(args: &[Secret]) -> Option<FillReport> {
    // Digits only, so no sign or space; at most the fields login.js reads.
    let count = |text: &Secret| text.expose().bytes().all(|b| b.is_ascii_digit()).then(|| text.expose().parse::<u16>().ok()).flatten().filter(|count| *count <= MAX_FILL_FIELDS);
    let [outcome, filled, hidden] = args else { return None };
    let (filled, hidden) = (count(filled)?, count(hidden)?);
    match outcome.expose() {
        FILL_DONE => Some(FillReport::Done { filled, hidden }),
        FILL_FOCUS_HIDDEN if filled == 0 && hidden == 0 => Some(FillReport::FocusHidden),
        FILL_UNCONFIRMED if filled == 0 && hidden == 0 => Some(FillReport::Unconfirmed),
        _ => None,
    }
}

/// Whether a message carries a login, an address or their follow, which
/// only a tab whose profile has the setting on may send.
pub(crate) fn carries_login(name: &str) -> bool {
    name == CAPTURE || name == FOLLOWED || name == ADDRESS_CAPTURE
}

/// One renderer message, after its sending frame passed [`LoginState::current`]
/// in the browser process.
pub(crate) enum Message {
    Capture { username: Secret, password: Secret },
    Followed,
    Focused,
    Address(Box<[Secret; ADDRESS_PARTS]>),
    /// An address note, with its number in its document.
    AddressFocused(u64),
    /// A card note, with its number in its document.
    CardFocused(u64),
}

/// Accept a message only if it is ours, has the right string arguments and
/// claims the origin the browser sees for its frame and the document its
/// renderer last reported; otherwise drop it, so a navigation race or a
/// forged origin never attributes a login to the wrong site or document.
pub(crate) fn accept(name: &str, mut args: Vec<Secret>, origin: &str, document: &str) -> Option<Message> {
    if args.first().map(Secret::expose) != Some(origin) || args.get(1).map(Secret::expose) != Some(document) { return None; }
    match (name, args.len()) {
        (CAPTURE, 4) => {
            let password = args.pop()?;
            let username = args.pop()?;
            let fits = !password.is_empty() && password.len() <= MAX_PASSWORD_BYTES && username.len() <= MAX_USERNAME_BYTES;
            fits.then_some(Message::Capture { username, password })
        }
        (FOLLOWED, 2) => Some(Message::Followed),
        (FOCUSED, 2) => Some(Message::Focused),
        (ADDRESS_CAPTURE, 11) => {
            let parts = args.split_off(2);
            if !address_shaped(&parts) { return None; }
            Some(Message::Address(Box::new(parts.try_into().ok()?)))
        }
        (ADDRESS_FOCUSED, 3) => note_number(&args[2]).map(Message::AddressFocused),
        (CARD_FOCUSED, 3) => note_number(&args[2]).map(Message::CardFocused),
        _ => None,
    }
}

/// A card or address note's number, as the renderer counts them in a
/// document: digits only, from 1.
fn note_number(text: &Secret) -> Option<u64> {
    note_count(text).filter(|number| *number > 0)
}

/// How many notes of a kind a document sent: digits only, from 0.
fn note_count(text: &Secret) -> Option<u64> {
    let text = text.expose();
    (!text.is_empty() && text.len() <= 20 && text.bytes().all(|b| b.is_ascii_digit())).then(|| text.parse().ok()).flatten()
}

/// A frame's report: its renderer installed `login.js` in the document it
/// calls `document`, which had `origin`; and the number of the last card
/// and address notes the browser took from that document, as the report
/// counted them and each note since, 0 before one.
struct Installed {
    frame: String,
    document: String,
    origin: String,
    card_note: u64,
    address_note: u64,
}

struct Pending {
    frame: String,
    capture: LoginCapture,
    at: Instant,
}

struct PendingAddress {
    frame: String,
    address: PageAddress,
    at: Instant,
}

/// One view's login traffic in the browser process.
#[derive(Default)]
pub(crate) struct LoginState {
    /// The last submitted login, until a navigation or request follows it.
    pending: Option<Pending>,
    /// The frame and origin of the last focused login field on this page.
    focus: Option<(String, String)>,
    /// Shared with the IO thread, which records responses.
    documents: Arc<Mutex<Documents>>,
    /// The frame the last fill was sent to, and when.
    fill_sent: Option<(String, Instant)>,
    /// What each frame's renderer last reported, oldest first.
    installed: VecDeque<Installed>,
    /// The last submitted address, until a navigation or request follows it.
    pending_address: Option<PendingAddress>,
    /// A submitted address that a navigation or request followed, until the view takes it.
    followed_address: Option<PageAddress>,
    /// The frame and origin of the last focused address field on this page.
    address_focus: Option<(String, String)>,
    /// The frame the last address fill was sent to, and when.
    address_fill_sent: Option<(String, Instant)>,
    /// The frame and origin of the last focused card field on this page.
    card_focus: Option<(String, String)>,
    /// The frame the last card fill was sent to, and when.
    card_fill_sent: Option<(String, Instant)>,
}

impl LoginState {
    pub(crate) fn documents(&self) -> Arc<Mutex<Documents>> {
        self.documents.clone()
    }

    /// The frame's origin, when logins may be saved from and filled into it.
    /// See [`LoginState::current`].
    pub(crate) fn origin<F: FrameLike>(&self, frame: &F) -> Option<String> {
        self.current(frame).map(|(origin, _)| origin)
    }

    /// The frame's origin and its current document's id, when logins may be
    /// saved from and filled into it: its renderer reported installing
    /// `login.js` in the frame's current document for [`eligible_origin`],
    /// and neither that document nor an ancestor's is known to be opaque.
    pub(crate) fn current<F: FrameLike>(&self, frame: &F) -> Option<(String, String)> {
        self.reported(frame, eligible_origin(frame))
    }

    /// The same, when a card may be filled into the frame: its
    /// [`card_origin`], which a frame of another site may have, and its
    /// current document's id.
    pub(crate) fn card_current<F: FrameLike>(&self, frame: &F) -> Option<(String, String)> {
        self.reported(frame, card_origin(frame))
    }

    /// `origin` and the frame's current document's id, when its renderer
    /// reported installing `login.js` there for that origin and neither that
    /// document nor an ancestor's is known to be opaque.
    fn reported<F: FrameLike>(&self, frame: &F, origin: Option<String>) -> Option<(String, String)> {
        let origin = origin?;
        let id = frame.id();
        let installed = self.installed.iter().find(|kept| kept.frame == id && kept.origin == origin)?;
        (!self.covered(frame)).then(|| (origin, installed.document.clone()))
    }

    fn covered<F: FrameLike>(&self, frame: &F) -> bool {
        self.documents.lock().unwrap_or_else(PoisonError::into_inner).covers(frame)
    }

    /// An [`INSTALLED`] report from `frame`'s renderer: kept, replacing the
    /// frame's last, only when it names the origin the browser sees for the
    /// frame, [`listener_origin`], and neither its document nor an
    /// ancestor's is known to be opaque. Any other report only ends the last
    /// one. A frame of another site counts for cards only: see
    /// [`LoginState::card_current`].
    pub(crate) fn installed<F: FrameLike>(&mut self, frame: &F, args: Vec<Secret>) {
        let id = frame.id();
        self.installed.retain(|kept| kept.frame != id);
        let [origin, document, cards, addresses] = args.as_slice() else { return };
        if document.is_empty() || document.len() > MAX_DOCUMENT_BYTES { return; }
        let Some((card_note, address_note)) = note_count(cards).zip(note_count(addresses)) else { return };
        if listener_origin(frame).as_deref() != Some(origin.expose()) || self.covered(frame) { return; }
        // A document reported again, as each setting message makes its renderer do, keeps its count (C11-2).
        self.installed.push_back(Installed { frame: id, document: document.expose().to_owned(), origin: origin.expose().to_owned(), card_note, address_note });
        if self.installed.len() > FRAMES_KEPT { self.installed.pop_front(); }
    }

    /// A new document started loading in `frame`, as CEF's `OnLoadStart`
    /// says, never for a same-document change such as `pushState`: what its
    /// renderer reported about the last document, and its focus, no longer
    /// hold. A new page ends every frame's report.
    pub(crate) fn document_started(&mut self, frame: &str, main: bool) {
        self.installed.retain(|kept| !main && kept.frame != frame);
        if self.focus.as_ref().is_some_and(|(focused, _)| focused == frame) { self.focus = None; }
        if self.address_focus.as_ref().is_some_and(|(focused, _)| focused == frame) { self.address_focus = None; }
        if self.card_focus.as_ref().is_some_and(|(focused, _)| focused == frame) { self.card_focus = None; }
    }

    /// The renderer of `frame` said its document is opaque: forget its
    /// report, its focus and any login it was submitting.
    pub(crate) fn noted_opaque<F: FrameLike>(&mut self, frame: &F) {
        let id = frame.id();
        self.documents.lock().unwrap_or_else(PoisonError::into_inner).noted(id.clone(), &frame.address());
        self.installed.retain(|kept| kept.frame != id);
        if self.focus.as_ref().is_some_and(|(focused, _)| *focused == id) { self.focus = None; }
        if self.pending.as_ref().is_some_and(|pending| pending.frame == id) { self.pending = None; }
        if self.address_focus.as_ref().is_some_and(|(focused, _)| *focused == id) { self.address_focus = None; }
        if self.pending_address.as_ref().is_some_and(|pending| pending.frame == id) { self.pending_address = None; }
        if self.card_focus.as_ref().is_some_and(|(focused, _)| *focused == id) { self.card_focus = None; }
    }

    pub(crate) fn fill_sent<F: FrameLike>(&mut self, frame: &F, now: Instant) {
        self.fill_sent = Some((frame.id(), now));
    }

    pub(crate) fn address_fill_sent<F: FrameLike>(&mut self, frame: &F, now: Instant) {
        self.address_fill_sent = Some((frame.id(), now));
    }

    pub(crate) fn card_fill_sent<F: FrameLike>(&mut self, frame: &F, now: Instant) {
        self.card_fill_sent = Some((frame.id(), now));
    }

    /// A frame said the fill sent to it found no field it may fill. True, once,
    /// only for the frame, origin and document of a fill sent moments ago.
    pub(crate) fn unfilled<F: FrameLike>(&mut self, frame: &F, args: Vec<Secret>, now: Instant) -> bool {
        let current = self.current(frame);
        took(&mut self.fill_sent, frame, &args, now, current)
    }

    /// The same, for an address fill.
    pub(crate) fn address_unfilled<F: FrameLike>(&mut self, frame: &F, args: Vec<Secret>, now: Instant) -> bool {
        let current = self.current(frame);
        took(&mut self.address_fill_sent, frame, &args, now, current)
    }

    /// The same, for a card fill, whose frame may be another site's.
    pub(crate) fn card_unfilled<F: FrameLike>(&mut self, frame: &F, args: Vec<Secret>, now: Instant) -> bool {
        let current = self.card_current(frame);
        took(&mut self.card_fill_sent, frame, &args, now, current)
    }

    /// A frame said what the address fill sent to it did. Some, once, only
    /// for the frame, origin and document of a fill sent moments ago.
    pub(crate) fn address_filled<F: FrameLike>(&mut self, frame: &F, args: Vec<Secret>, now: Instant) -> Option<FillReport> {
        let current = self.current(frame);
        took_report(&mut self.address_fill_sent, frame, args, now, current)
    }

    /// The same, for a card fill, whose frame may be another site's.
    pub(crate) fn card_filled<F: FrameLike>(&mut self, frame: &F, args: Vec<Secret>, now: Instant) -> Option<FillReport> {
        let current = self.card_current(frame);
        took_report(&mut self.card_fill_sent, frame, args, now, current)
    }

    /// Handle an accepted message from `frame`, whose origin is `origin`.
    /// Returns a login once a request followed its submit.
    pub(crate) fn receive(&mut self, frame: &str, origin: &str, message: Message, now: Instant) -> Option<LoginCapture> {
        self.expire(now);
        match message {
            Message::Capture { username, password } => {
                // A second submit replaces the first, which is wiped.
                self.pending = Some(Pending { frame: frame.to_owned(), capture: LoginCapture { origin: origin.to_owned(), username, password }, at: now });
                None
            }
            Message::Followed => {
                if self.pending_address.as_ref().is_some_and(|p| p.frame == frame && p.address.origin == origin) { self.follow_address(); }
                self.pending.as_ref().is_some_and(|p| p.frame == frame && p.capture.origin == origin).then(|| self.take()).flatten()
            }
            Message::Focused => {
                self.focus = Some((frame.to_owned(), origin.to_owned()));
                None
            }
            Message::Address(parts) => {
                // A second submit replaces the first, which is wiped.
                self.pending_address = Some(PendingAddress { frame: frame.to_owned(), address: PageAddress { origin: origin.to_owned(), parts: *parts }, at: now });
                None
            }
            Message::AddressFocused(number) => {
                self.address_focus = Some((frame.to_owned(), origin.to_owned()));
                if let Some(kept) = self.installed.iter_mut().find(|kept| kept.frame == frame) { kept.address_note = number; }
                None
            }
            Message::CardFocused(number) => {
                self.card_focus = Some((frame.to_owned(), origin.to_owned()));
                if let Some(kept) = self.installed.iter_mut().find(|kept| kept.frame == frame) { kept.card_note = number; }
                None
            }
        }
    }

    /// The submitted address now followed, for the view to report.
    fn follow_address(&mut self) {
        self.followed_address = self.pending_address.take().map(|pending| pending.address);
    }

    /// A submitted address a navigation or request followed, once.
    pub(crate) fn take_followed_address(&mut self) -> Option<PageAddress> {
        self.followed_address.take()
    }

    /// A load started in `frame`, or the main frame changed address.
    /// Returns a login this frame, or the page, was submitting.
    pub(crate) fn navigated(&mut self, frame: &str, main: bool, now: Instant) -> Option<LoginCapture> {
        self.expire(now);
        if main {
            self.focus = None;
            self.address_focus = None;
            self.card_focus = None;
        }
        if self.pending_address.as_ref().is_some_and(|p| main || p.frame == frame) { self.follow_address(); }
        self.pending.as_ref().is_some_and(|p| main || p.frame == frame).then(|| self.take()).flatten()
    }

    fn take(&mut self) -> Option<LoginCapture> {
        self.pending.take().map(|pending| pending.capture)
    }

    /// Wipe a login or address waiting for its follow, or for the view, as
    /// when capture is turned off.
    pub(crate) fn forget_pending(&mut self) {
        self.pending = None;
        self.pending_address = None;
        self.followed_address = None;
    }

    /// Wipe a login or address that nothing followed in time.
    pub(crate) fn expire(&mut self, now: Instant) {
        if self.pending.as_ref().is_some_and(|p| now.saturating_duration_since(p.at) >= FOLLOW_WINDOW) { self.pending = None; }
        if self.pending_address.as_ref().is_some_and(|p| now.saturating_duration_since(p.at) >= FOLLOW_WINDOW) { self.pending_address = None; }
    }

    pub(crate) fn focus(&self) -> Option<(&str, &str)> {
        self.focus.as_ref().map(|(frame, origin)| (frame.as_str(), origin.as_str()))
    }

    pub(crate) fn address_focus(&self) -> Option<(&str, &str)> {
        self.address_focus.as_ref().map(|(frame, origin)| (frame.as_str(), origin.as_str()))
    }

    pub(crate) fn card_focus(&self) -> Option<(&str, &str)> {
        self.card_focus.as_ref().map(|(frame, origin)| (frame.as_str(), origin.as_str()))
    }

    /// The number of the last card note taken from `frame`'s current
    /// document, 0 before one.
    fn card_note(&self, frame: &str) -> u64 {
        self.installed.iter().find(|kept| kept.frame == frame).map_or(0, |kept| kept.card_note)
    }

    /// The same, for address notes.
    fn address_note(&self, frame: &str) -> u64 {
        self.installed.iter().find(|kept| kept.frame == frame).map_or(0, |kept| kept.address_note)
    }

    #[cfg(test)]
    pub(crate) fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
}

/// Whether `args` name the frame, origin and document, `current`, that
/// `sent` was sent to moments ago; if so, `sent` is used up.
fn took<F: FrameLike>(sent: &mut Option<(String, Instant)>, frame: &F, args: &[Secret], now: Instant, current: Option<(String, String)>) -> bool {
    let recent = sent.as_ref().is_some_and(|(to, at)| *to == frame.id() && now.saturating_duration_since(*at) < UNFILLED_WINDOW);
    let names = |(origin, document): (String, String)| matches!(args, [said, of] if said.expose() == origin && of.expose() == document);
    let taken = recent && current.is_some_and(names);
    if taken { *sent = None; }
    taken
}

/// A fill's report, when `args` are the frame, origin and document `sent`
/// went to moments ago, then a [`FillReport`]; if so, `sent` is used up. A
/// malformed report uses up nothing.
fn took_report<F: FrameLike>(sent: &mut Option<(String, Instant)>, frame: &F, mut args: Vec<Secret>, now: Instant, current: Option<(String, String)>) -> Option<FillReport> {
    if args.len() != 5 { return None; }
    let report = fill_report(&args.split_off(2))?;
    took(sent, frame, &args, now, current).then_some(report)
}

/// A renderer message from `frame`, in the browser process: dropped unless the
/// frame may hold logins and the message passes [`accept`] for its origin and
/// document. Without `capture`, the setting for the view's profile, a
/// submitted login or a follow is dropped too, and its values wiped: only a
/// renderer that missed the change still sends one. A note that the frame's
/// document is opaque is always taken, since it only takes logins away, and
/// so is a report, which [`LoginState::installed`] checks. A card note may
/// come from a frame of another site; nothing else from there is taken.
pub(crate) fn receive<F: FrameLike>(state: &mut LoginState, frame: &F, name: &str, args: Vec<Secret>, now: Instant, capture: bool) -> Option<LoginCapture> {
    if !capture && carries_login(name) { return None; }
    if name == OPAQUE {
        if args.len() == 1 && args.first().map(Secret::expose) == listener_origin(frame).as_deref() { state.noted_opaque(frame); }
        return None;
    }
    if name == INSTALLED {
        state.installed(frame, args);
        return None;
    }
    let (origin, document) = if name == CARD_FOCUSED { state.card_current(frame)? } else { state.current(frame)? };
    let message = accept(name, args, &origin, &document)?;
    state.receive(&frame.id(), &origin, message, now)
}

/// Where a fill goes, given the frame whose login field last had focus, the
/// frame that has focus and the main frame: the first, while it is also the
/// second, else the main frame, whichever may hold logins of `origin`.
/// Outside the main frame, login.js fills only the login form that has focus.
pub(crate) fn fill_target<F: FrameLike>(state: &LoginState, frames: [Option<F>; 3], origin: &str) -> Option<F> {
    let [login, focused, main] = frames;
    let login = login.filter(|login| focused.as_ref().is_some_and(|focused| focused.id() == login.id()));
    [login, main].into_iter().flatten().find(|frame| state.origin(frame).as_deref() == Some(origin))
}

/// The frames a card fill may go to, given the frame whose card field last
/// had focus, the frame that has focus and the main frame: the frame with
/// focus, while its current document noted a card field, then the main
/// frame. The frame with the latest note always has; another may too, as
/// when the user comes back to a processor's field after another frame's,
/// which sends no new note (C11-2).
fn card_frames<F: FrameLike>(state: &LoginState, frames: [Option<F>; 3]) -> impl Iterator<Item = F> {
    let [_, focused, main] = frames;
    let focused = focused.filter(|focused| state.card_note(&focused.id()) > 0);
    [focused, main].into_iter().flatten()
}

/// The origin a card fill would go to now: that of the first of
/// [`card_frames`] that may take a card. None on an http page, file:, data:
/// or an internal page.
pub(crate) fn card_fill_origin<F: FrameLike>(state: &LoginState, frames: [Option<F>; 3]) -> Option<String> {
    card_frames(state, frames).find_map(|frame| state.card_current(&frame).map(|(origin, _)| origin))
}

/// Where a card fill goes, and its document: the first of [`card_frames`]
/// that may take a card and has `origin`, the one the menu named. A
/// processor's frame that lost the user's focus gets nothing, and the page,
/// another origin, never gets the card in its place. Outside the main frame,
/// login.js fills only the card fields that have focus.
pub(crate) fn card_fill_target<F: FrameLike>(state: &LoginState, frames: [Option<F>; 3], origin: &str) -> Option<(F, String)> {
    card_frames(state, frames).find_map(|frame| {
        let (of, document) = state.card_current(&frame)?;
        (of == origin).then_some((frame, document))
    })
}

/// Where a card or address fill goes, frozen when its menu opens: the
/// browser, the frame and its document, the origin the browser computed for
/// that frame, the frame of the page's latest card or address note, and the
/// frame that had the user's focus. Also the number of the card or address
/// note the frame last sent, which names the form or group its focused
/// field is in, and the latest note's number. A default one names no
/// frame, so it never holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FillTarget {
    browser: i32,
    frame: String,
    document: String,
    origin: String,
    note: u64,
    noted: Option<(String, u64)>,
    focused: Option<String>,
}

impl FillTarget {
    /// The origin of the frame the fill goes to, which the menu names.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub(crate) fn document(&self) -> &str {
        &self.document
    }

    pub(crate) fn note(&self) -> u64 {
        self.note
    }
}

/// The frame a card fill would go to now in `browser`, given the frame of
/// the latest card note, the frame that has focus and the main frame, with
/// what it is frozen with: [`card_fill_target`] for [`card_fill_origin`].
fn card_aim<F: FrameLike + Clone>(state: &LoginState, browser: i32, frames: [Option<F>; 3]) -> Option<(F, FillTarget)> {
    let origin = card_fill_origin(state, frames.clone())?;
    let [noted, focused, _] = &frames;
    let (noted, focused) = (noted.as_ref().map(|frame| frame.id()), focused.as_ref().map(|frame| frame.id()));
    let (frame, document) = card_fill_target(state, frames, &origin)?;
    let id = frame.id();
    let target = FillTarget { browser, note: state.card_note(&id), frame: id, document, origin, noted: noted.map(|noted| { let number = state.card_note(&noted); (noted, number) }), focused };
    Some((frame, target))
}

/// The card fill the card menu opening now freezes; see [`FillTarget`].
pub(crate) fn card_menu_target<F: FrameLike + Clone>(state: &LoginState, browser: i32, frames: [Option<F>; 3]) -> Option<FillTarget> {
    card_aim(state, browser, frames).map(|(_, target)| target)
}

/// The frame a card fill frozen as `target` goes to at the choice: the one
/// it was frozen on, only while all it was frozen with still holds. Page
/// script that moved focus to the top page's field or another frame, a
/// card note from another frame, or from the same one as focus moves to
/// another form or group or to anything that reaches no card field (C11-1),
/// a navigation, a reload, a removed frame or another browser each void
/// the fill, and nothing is sent.
pub(crate) fn card_frozen_frame<F: FrameLike + Clone>(state: &LoginState, browser: i32, frames: [Option<F>; 3], target: &FillTarget) -> Option<F> {
    card_aim(state, browser, frames).filter(|(_, now)| now == target).map(|(frame, _)| frame)
}

/// The frame an address fill would go to now in `browser`, given the frame
/// of the latest address note, the frame that has focus and the main frame,
/// with what it is frozen with: the frame with focus, while its current
/// document noted an address field, else the main frame, the first that
/// may hold logins of that note's origin, as [`card_frames`] says for a card.
fn address_aim<F: FrameLike + Clone>(state: &LoginState, browser: i32, frames: [Option<F>; 3]) -> Option<(F, FillTarget)> {
    let origin = state.address_focus()?.1.to_owned();
    let [noted, focused, main] = frames;
    let noted = noted.map(|frame| (frame.id(), state.address_note(&frame.id())));
    let focused_id = focused.as_ref().map(|frame| frame.id());
    let here = focused.filter(|focused| state.address_note(&focused.id()) > 0);
    let frame = [here, main].into_iter().flatten().find(|frame| state.origin(frame).as_deref() == Some(origin.as_str()))?;
    let (_, document) = state.current(&frame)?;
    let target = FillTarget { browser, note: state.address_note(&frame.id()), frame: frame.id(), document, origin, noted, focused: focused_id };
    Some((frame, target))
}

/// The address fill the address menu opening now freezes; see [`FillTarget`].
pub(crate) fn address_menu_target<F: FrameLike + Clone>(state: &LoginState, browser: i32, frames: [Option<F>; 3]) -> Option<FillTarget> {
    address_aim(state, browser, frames).map(|(_, target)| target)
}

/// The frame an address fill frozen as `target` goes to at the choice, as
/// [`card_frozen_frame`] says for a card: only while all it was frozen
/// with still holds, so never a page that navigated to another site since.
pub(crate) fn address_frozen_frame<F: FrameLike + Clone>(state: &LoginState, browser: i32, frames: [Option<F>; 3], target: &FillTarget) -> Option<F> {
    address_aim(state, browser, frames).filter(|(_, now)| now == target).map(|(frame, _)| frame)
}

/// A message's arguments, if every one is a string. Each is copied into a
/// [`Secret`]; CEF's own copies are not wiped.
pub(crate) fn string_args(message: &ProcessMessage) -> Option<Vec<Secret>> {
    let list = message.argument_list()?;
    (0..list.size()).map(|i| {
        (list.get_type(i) == ValueType::STRING).then(|| Secret::new(CefString::from(&list.string(i)).to_string()))
    }).collect()
}

pub(crate) fn outgoing(name: &str, args: &[&str]) -> Option<ProcessMessage> {
    let message = process_message_create(Some(&CefString::from(name)))?;
    let list = message.argument_list()?;
    list.set_size(args.len());
    for (i, arg) in args.iter().enumerate() {
        list.set_string(i, Some(&CefString::from(*arg)));
    }
    Some(message)
}

// ---------------------------------------------------------------------------
// The setting, in the browser process
// ---------------------------------------------------------------------------

/// The profiles that have "Offer to save passwords from websites" on, as the
/// app last set them; every other profile is off. Each change takes the next
/// generation.
struct Settings {
    generation: u64,
    on: BTreeSet<String>,
}

static SETTINGS: Mutex<Settings> = Mutex::new(Settings { generation: 0, on: BTreeSet::new() });

fn settings() -> std::sync::MutexGuard<'static, Settings> {
    SETTINGS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Turn page capture on or off for `profile`. Browsers created afterwards
/// carry the new value; open ones need [`crate::WebView::refresh_login_capture`].
/// True when it changed.
pub fn set_login_capture(profile: &str, on: bool) -> bool {
    let mut settings = settings();
    if settings.on.contains(profile) == on { return false; }
    settings.generation += 1;
    if on { settings.on.insert(profile.to_owned()); } else { settings.on.remove(profile); }
    true
}

/// Whether page capture is on for `profile`. Off unless the app turned it on.
pub fn login_capture(profile: &str) -> bool {
    settings().on.contains(profile)
}

/// `profile`'s setting now, with its generation.
pub(crate) fn capture_setting(profile: &str) -> (u64, bool) {
    let settings = settings();
    (settings.generation, settings.on.contains(profile))
}

/// What a new browser carries to every renderer that will host it. `saves`
/// is false for Peek and Little Windows, which never offer to save, so their
/// pages never get the capture functions.
pub(crate) fn creation_info(profile: &str, saves: bool) -> Option<DictionaryValue> {
    let (generation, on) = capture_setting(profile);
    let info = dictionary_value_create()?;
    for (key, value) in [(INFO_PROFILE, profile.to_owned()), (INFO_GENERATION, generation.to_string()), (INFO_CAPTURE, (on as u8).to_string()), (INFO_SAVES, (saves as u8).to_string())] {
        info.set_string(Some(&CefString::from(key)), Some(&CefString::from(value.as_str())));
    }
    Some(info)
}

/// The value of [`SWITCH`] for a renderer launching now. A profile id that
/// can't be listed is left out, which reads as off.
pub(crate) fn launch_switch() -> String {
    let settings = settings();
    let listed: Vec<&str> = settings.on.iter().map(String::as_str).filter(|id| !id.is_empty() && !id.contains([',', ':'])).collect();
    format!("{}:{}", settings.generation, listed.join(","))
}

/// The message that tells a renderer `profile`'s setting now.
pub(crate) fn setting_message(profile: &str) -> Option<ProcessMessage> {
    let (generation, on) = capture_setting(profile);
    outgoing(SETTING, &[profile, &generation.to_string(), if on { "1" } else { "0" }])
}

// ---------------------------------------------------------------------------
// Renderer process
// ---------------------------------------------------------------------------

/// What a renderer knows of the setting: the value this process was launched
/// with, values from each browser's creation and later messages. For each
/// profile the newest generation wins, and a tie is off. A browser whose
/// profile has no known value is off, so capture never starts by default.
/// A renderer that Chromium launched before the setting was turned off, and
/// that first hosts an older tab after it, reads on until the browser's
/// message at the document's load start arrives; a submit checks again, and
/// the browser process drops what it sends. A view that never offers to save
/// is always off.
///
/// One browser can have several renderer-side browsers here at once, all
/// with its identifier: CEF creates the next before it destroys the last
/// (`OnBrowserCreated` in CEF's cef_render_process_handler.h). A browser
/// counts until its last one is destroyed; ending it at the first left a
/// tab's later documents without capture (N1103-V6).
#[derive(Default)]
pub(crate) struct Gate {
    launch: Option<(u64, BTreeSet<String>)>,
    learned: HashMap<String, (u64, bool)>,
    browsers: HashMap<i32, Hosted>,
}

/// A browser as this renderer knows it: its profile, whether it may offer
/// to save, and how many of its renderer-side browsers are alive here.
struct Hosted {
    profile: String,
    saves: bool,
    live: usize,
}

impl Gate {
    /// This process's launch value, from [`SWITCH`]; unparsable reads as off.
    pub(crate) fn launched(&mut self, switch: &str) {
        let parsed = switch.split_once(':').and_then(|(generation, ids)| Some((generation.parse().ok()?, ids.split(',').filter(|id| !id.is_empty()).map(str::to_owned).collect())));
        self.launch = Some(parsed.unwrap_or_default());
    }

    pub(crate) fn has_launch(&self) -> bool {
        self.launch.is_some()
    }

    pub(crate) fn learn(&mut self, profile: &str, generation: u64, on: bool) {
        let known = self.learned.entry(profile.to_owned()).or_insert((generation, on));
        if (generation, !on) > (known.0, !known.1) { *known = (generation, on); }
    }

    pub(crate) fn browser_created(&mut self, browser: i32, profile: &str, generation: u64, on: bool, saves: bool) {
        let hosted = self.browsers.entry(browser).or_insert_with(|| Hosted { profile: String::new(), saves: false, live: 0 });
        hosted.live += 1;
        // A later one without creation values keeps the browser's, which never change.
        if !profile.is_empty() || hosted.live == 1 { (hosted.profile, hosted.saves) = (profile.to_owned(), saves); }
        if !profile.is_empty() { self.learn(profile, generation, on); }
    }

    pub(crate) fn browser_destroyed(&mut self, browser: i32) {
        let Some(hosted) = self.browsers.get_mut(&browser) else { return };
        hosted.live = hosted.live.saturating_sub(1);
        if hosted.live == 0 { self.browsers.remove(&browser); }
    }

    /// Whether `browser`'s pages may capture a login.
    pub(crate) fn allows(&self, browser: i32) -> bool {
        let Some(Hosted { profile, .. }) = self.browsers.get(&browser).filter(|hosted| hosted.saves && !hosted.profile.is_empty()) else { return false };
        let launch = self.launch.as_ref().map(|(generation, on)| (*generation, on.contains(profile)));
        [launch, self.learned.get(profile).copied()].into_iter().flatten().max_by_key(|(generation, on)| (*generation, !*on)).is_some_and(|(_, on)| on)
    }
}

/// A browser's creation values, from its `extra_info`. Missing ones read as off.
fn created(gate: &mut Gate, browser: i32, info: Option<&DictionaryValue>) {
    let text = |key: &str| info.map(|info| CefString::from(&info.string(Some(&CefString::from(key)))).to_string()).unwrap_or_default();
    let generation = text(INFO_GENERATION).parse().unwrap_or(0);
    gate.browser_created(browser, &text(INFO_PROFILE), generation, text(INFO_CAPTURE) == "1", text(INFO_SAVES) == "1");
}

/// A [`SETTING`] message from the browser.
fn setting(message: &ProcessMessage) -> bool {
    if CefString::from(&message.name()).to_string() != SETTING { return false; }
    let Some(args) = string_args(message) else { return true };
    let [profile, generation, on] = args.as_slice() else { return true };
    let Ok(generation) = generation.expose().parse() else { return true };
    GATE.with_borrow_mut(|gate| gate.learn(profile.expose(), generation, on.expose() == "1"));
    true
}

fn allowed(browser: Option<Browser>) -> bool {
    browser.is_some_and(|browser| GATE.with_borrow(|gate| gate.allows(browser.identifier())))
}

/// A frame's listener: its script context, the fill functions `login.js`
/// returned, the origin the document had when the context was created, the
/// id this renderer gave the document, and how many card and address notes
/// it sent.
struct Filler {
    context: V8Context,
    fill: V8Value,
    address: V8Value,
    card: V8Value,
    origin: String,
    document: String,
    card_notes: u64,
    address_notes: u64,
}

thread_local! {
    /// Each injected frame's listener, by frame id. Renderer main thread only.
    static FILLERS: RefCell<HashMap<String, Filler>> = RefCell::new(HashMap::new());
    /// The number of the next document this renderer installs `login.js` in.
    static DOCUMENTS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// The setting, as this renderer knows it. Renderer main thread only.
    static GATE: RefCell<Gate> = RefCell::new(Gate::default());
}

wrap_v8_handler! {
    struct NativeBuilder;

    impl V8Handler {
        fn execute(
            &self,
            name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            arguments: Option<&[Option<V8Value>]>,
            _retval: Option<&mut Option<V8Value>>,
            _exception: Option<&mut CefString>,
        ) -> ::std::os::raw::c_int {
            native(&name.map(|name| name.to_string()).unwrap_or_default(), arguments.unwrap_or_default());
            1
        }
    }
}

/// A call from `login.js`. The frame and its origin come from the calling
/// context, never from the page; the browser checks them again.
fn native(name: &str, arguments: &[Option<V8Value>]) {
    let Some(context) = v8_context_get_current_context() else { return };
    let Some(frame) = context.frame() else { return };
    // Another site's frame, where only a card is ever filled, sends only the card note and the card fill's report.
    let Some(origin) = (if name == "cardFocused" || name == "cardFilled" { renderer_card_origin(&frame) } else { eligible_origin(&frame) }) else { return };
    // Only a listener installed in this very context for this origin.
    let Some(document) = installed_for(&frame, &context, &origin) else { return };
    // Turned off since this page loaded: its listener stays until a reload, and
    // sends nothing. Checked at each call, with the newest value this renderer
    // holds. A note or a fill's report carries no value, and fills don't depend on the setting.
    if name != "focused" && name != "addressFocused" && name != "cardFocused" && name != "addressFilled" && name != "cardFilled" && !allowed(context.browser()) { return; }
    let text = |i: usize| -> Option<Secret> {
        let value = arguments.get(i)?.as_ref()?;
        (value.is_string() == 1).then(|| Secret::new(CefString::from(&value.string_value()).to_string()))
    };
    let message = match name {
        "capture" => {
            let (Some(username), Some(password)) = (text(0), text(1)) else { return };
            // A value the vault couldn't hold never crosses.
            if username.len() > MAX_USERNAME_BYTES || password.len() > MAX_PASSWORD_BYTES { return; }
            outgoing(CAPTURE, &[&origin, &document, username.expose(), password.expose()])
        }
        "followed" => outgoing(FOLLOWED, &[&origin, &document]),
        "focused" => outgoing(FOCUSED, &[&origin, &document]),
        "addressCapture" => {
            let parts: Option<Vec<Secret>> = (0..ADDRESS_PARTS).map(text).collect();
            let Some(parts) = parts.filter(|parts| address_shaped(parts)) else { return };
            let mut args = vec![origin.as_str(), document.as_str()];
            args.extend(parts.iter().map(Secret::expose));
            outgoing(ADDRESS_CAPTURE, &args)
        }
        "addressFocused" | "cardFocused" => {
            // Numbered in this document, so a fill names the note its menu was opened on.
            let id = CefString::from(&frame.identifier()).to_string();
            let card = name == "cardFocused";
            let Some(number) = FILLERS.with_borrow_mut(|fillers| fillers.get_mut(&id).map(|filler| {
                let notes = if card { &mut filler.card_notes } else { &mut filler.address_notes };
                *notes += 1;
                *notes
            })) else { return };
            outgoing(if card { CARD_FOCUSED } else { ADDRESS_FOCUSED }, &[&origin, &document, &number.to_string()])
        }
        "addressFilled" | "cardFilled" => {
            let report: Option<Vec<Secret>> = (0..3).map(text).collect();
            // Only a well-formed report crosses: an outcome and two counts.
            let Some(report) = report.filter(|report| fill_report(report).is_some()) else { return };
            let mut args = vec![origin.as_str(), document.as_str()];
            args.extend(report.iter().map(Secret::expose));
            outgoing(if name == "cardFilled" { CARD_FILLED } else { ADDRESS_FILLED }, &args)
        }
        _ => None,
    };
    if let Some(mut message) = message {
        frame.send_process_message(ProcessId::BROWSER, Some(&mut message));
    }
}

fn v8_string(text: &str) -> Option<V8Value> {
    v8_value_create_string(Some(&CefString::from(text)))
}

/// The document's own origin and whether it is a secure context, read
/// natively from the global's getters. Called once, when the context is
/// created, before any page script can replace them.
fn realm(context: &V8Context) -> Option<Realm> {
    let global = context.global()?;
    let origin = global.value_bykey(Some(&CefString::from("origin")))?;
    let secure = global.value_bykey(Some(&CefString::from("isSecureContext")))?;
    (origin.is_string() == 1 && secure.is_bool() == 1).then(|| Realm { origin: CefString::from(&origin.string_value()).to_string(), secure: secure.bool_value() == 1 })
}

/// Install the listener in a new script context, before any page script:
/// only when the document's own origin is its address's, with capture only
/// if the setting is on for the tab's profile, and with the card note only
/// if the document may take a card. A frame of another site than the
/// page's, which may hold a payment processor's card fields, gets it for
/// cards only, and nothing if it can't take one. Then report it to the
/// browser, which otherwise treats the frame as holding no logins.
fn inject(browser: Option<Browser>, frame: &Frame, context: &mut V8Context) {
    let id = CefString::from(&frame.identifier()).to_string();
    // A new context is a new document: the frame's last listener is gone.
    FILLERS.with_borrow_mut(|fillers| fillers.remove(&id));
    let Some(origin) = renderer_listener_origin(frame) else { return };
    let cards_only = eligible_origin(frame).is_none();
    if context.enter() != 1 { return; }
    // Read natively, before login.js and any page script, and never again.
    let snapshot = realm(context);
    if !realm_allows(&origin, snapshot.as_ref()) {
        context.exit();
        // It can only take logins away: the browser then treats this frame as holding none.
        if let Some(mut message) = outgoing(OPAQUE, &[&origin]) { frame.send_process_message(ProcessId::BROWSER, Some(&mut message)); }
        return;
    }
    let cards = takes_cards(frame, snapshot.as_ref());
    if cards_only && !cards {
        context.exit();
        return;
    }
    let fills = install(context, allowed(browser), cards_only, cards);
    context.exit();
    // A script that didn't load installed nothing, and isn't reported.
    let Some((fill, address, card)) = fills else { return };
    let document = DOCUMENTS.with(|next| {
        next.set(next.get() + 1);
        format!("{}.{}", std::process::id(), next.get())
    });
    report(frame, &origin, &document);
    FILLERS.with_borrow_mut(|fillers| fillers.insert(id, Filler { context: context.clone(), fill, address, card, origin, document, card_notes: 0, address_notes: 0 }));
}

/// Report `document` in `frame`, with how many card and address notes its
/// listener sent: none for a new one, and its own count when it is
/// reported again, so the browser's count stays the renderer's.
fn report(frame: &Frame, origin: &str, document: &str) {
    let id = CefString::from(&frame.identifier()).to_string();
    let (cards, addresses) = FILLERS.with_borrow(|fillers| fillers.get(&id).filter(|filler| filler.document == document).map_or((0, 0), |filler| (filler.card_notes, filler.address_notes)));
    if let Some(mut message) = outgoing(INSTALLED, &[origin, document, &cards.to_string(), &addresses.to_string()]) { frame.send_process_message(ProcessId::BROWSER, Some(&mut message)); }
}

/// The browser forgets a frame's report when a new document starts loading
/// in it, then sends the frame the setting. Reporting the frame's live
/// listener again in reply keeps it whichever of that start and the first
/// report reaches the browser first.
fn report_again(frame: &Frame) {
    let id = CefString::from(&frame.identifier()).to_string();
    let live = FILLERS.with_borrow(|fillers| {
        let filler = fillers.get(&id)?;
        (filler.context.is_valid() == 1).then(|| (filler.origin.clone(), filler.document.clone()))
    });
    let Some((origin, document)) = live else { return };
    if renderer_listener_origin(frame).as_deref() == Some(origin.as_str()) { report(frame, &origin, &document); }
}

/// The id of the document whose context, `context`, this renderer installed
/// the listener in for `frame`, when the document had `origin`.
fn installed_for(frame: &Frame, context: &V8Context, origin: &str) -> Option<String> {
    let id = CefString::from(&frame.identifier()).to_string();
    FILLERS.with_borrow(|fillers| {
        let filler = fillers.get(&id)?;
        (filler.origin == origin && filler.context.is_same(Some(&mut context.clone())) == 1).then(|| filler.document.clone())
    })
}

/// Evaluate `login.js` and call it with the native functions, which only its
/// closure keeps. Without `capture`, capture(), followed() and
/// addressCapture() are undefined, so it adds no submit listener. Without
/// `cards`, cardFocused() and cardFilled() are undefined, so it never fills
/// a card; in another site's frame, `cards_only`, they are the only ones.
/// Returns the login, address and card fill functions it gives back, read
/// from the null-prototype object it returns, whose own properties run no
/// page code.
fn install(context: &mut V8Context, capture: bool, cards_only: bool, cards: bool) -> Option<(V8Value, V8Value, V8Value)> {
    let natives: Vec<Option<V8Value>> = ["capture", "followed", "focused", "addressCapture", "addressFocused", "cardFocused", "addressFilled", "cardFilled"].iter()
        .map(|name| {
            let always = *name == "focused" || *name == "addressFocused" || *name == "addressFilled";
            let wanted = if *name == "cardFocused" || *name == "cardFilled" { cards } else { !cards_only && (capture || always) };
            if wanted { v8_value_create_function(Some(&CefString::from(*name)), Some(&mut NativeBuilder::new())) } else { v8_value_create_undefined() }
        })
        .collect();
    let (mut factory, mut exception): (Option<V8Value>, Option<V8Exception>) = (None, None);
    if context.eval(Some(&CefString::from(SCRIPT)), Some(&CefString::from(SCRIPT_URL)), 1, Some(&mut factory), Some(&mut exception)) != 1 { return None; }
    let factory = factory.filter(|factory| factory.is_function() == 1)?;
    let fills = factory.execute_function_with_context(Some(context), None, Some(&natives)).filter(|fills| fills.is_object() == 1)?;
    let function = |key: &str| fills.value_bykey(Some(&CefString::from(key))).filter(|fill| fill.is_function() == 1);
    Some((function("login")?, function("address")?, function("card")?))
}

fn release(frame: &Frame, context: &mut V8Context) {
    let id = CefString::from(&frame.identifier()).to_string();
    FILLERS.with_borrow_mut(|fillers| {
        if fillers.get(&id).is_some_and(|filler| filler.context.is_same(Some(&mut *context)) == 1) { fillers.remove(&id); }
    });
}

/// What a fill message fills.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fill {
    Login,
    Address,
    Card,
}

/// A fill from the browser, of a login, an address or a card: only into the
/// frame it was sent to, and only if that frame still has the origin the
/// browser chose the entry for and the document the browser chose the frame
/// for, as its listener recorded them; a card, only while the card note its
/// menu was opened on is that document's latest. A card may go to a frame of another
/// site, never to an http page; a login or an address only to the page's
/// own origin. login.js learns whether this is the main frame, the only one
/// where it fills a form without a field of its kind focused. Tells the
/// browser when no field could be filled; an address or card fill that
/// waits for the engine's answer reports later, through addressFilled() or
/// cardFilled().
fn fill(frame: &Frame, message: &ProcessMessage) -> bool {
    let name = CefString::from(&message.name()).to_string();
    let (kind, unfilled, values) = match name.as_str() {
        FILL => (Fill::Login, UNFILLED, 2),
        FILL_ADDRESS => (Fill::Address, ADDRESS_UNFILLED, ADDRESS_VALUES),
        FILL_CARD => (Fill::Card, CARD_UNFILLED, CARD_VALUES),
        _ => return false,
    };
    let Some(args) = string_args(message) else { return true };
    let [origin, document, rest @ ..] = args.as_slice() else { return true };
    // A card or address fill names the note its menu was opened on before its values.
    let (note, rest) = match (kind, rest) {
        (Fill::Card | Fill::Address, [note, rest @ ..]) => (Some(note), rest),
        (Fill::Card | Fill::Address, []) => return true,
        _ => (None, rest),
    };
    let frame_origin = if kind == Fill::Card { renderer_card_origin(frame) } else { eligible_origin(frame) };
    if rest.len() != values || frame_origin.as_deref() != Some(origin.expose()) { return true; }
    let id = CefString::from(&frame.identifier()).to_string();
    // Taken out first: page listeners run during the fill and may release this context.
    let listener = FILLERS.with_borrow(|fillers| {
        let filler = fillers.get(&id)?;
        if filler.origin != origin.expose() || filler.document != document.expose() { return None; }
        // A card or an address, only while that note is still this
        // document's latest of its kind: focus moved to another form or
        // group since, or to a field that reaches none, and the note saying
        // so may not have reached the browser yet.
        let notes = if kind == Fill::Card { filler.card_notes } else { filler.address_notes };
        if note.is_some_and(|note| note.expose() != notes.to_string()) { return None; }
        let function = match kind {
            Fill::Login => &filler.fill,
            Fill::Address => &filler.address,
            Fill::Card => &filler.card,
        };
        Some((filler.context.clone(), function.clone()))
    });
    let filled = listener.is_some_and(|(mut context, filler)| {
        if context.is_valid() != 1 || context.enter() != 1 { return false; }
        let main = v8_value_create_bool(frame.is_main());
        let texts = rest.iter().map(|value| v8_string(value.expose()));
        // A login is (username, password, main); an address or a card is (main, ...values).
        let args: Vec<Option<V8Value>> = if kind == Fill::Login { texts.chain(std::iter::once(main)).collect() } else { std::iter::once(main).chain(texts).collect() };
        let filled = filler.execute_function_with_context(Some(&mut context), None, Some(&args)).is_some_and(|done| done.is_bool() == 1 && done.bool_value() == 1);
        context.exit();
        filled
    });
    if !filled && let Some(mut message) = outgoing(unfilled, &[origin.expose(), document.expose()]) { frame.send_process_message(ProcessId::BROWSER, Some(&mut message)); }
    true
}

wrap_render_process_handler! {
    pub(crate) struct RendererBuilder;

    impl RenderProcessHandler {
        fn on_browser_created(&self, browser: Option<&mut Browser>, extra_info: Option<&mut DictionaryValue>) {
            let Some(browser) = browser else { return };
            GATE.with_borrow_mut(|gate| {
                if !gate.has_launch() {
                    let switch = command_line_get_global().map(|line| CefString::from(&line.switch_value(Some(&CefString::from(SWITCH)))).to_string());
                    gate.launched(&switch.unwrap_or_default());
                }
                created(gate, browser.identifier(), extra_info.as_deref());
            });
        }

        fn on_browser_destroyed(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser { GATE.with_borrow_mut(|gate| gate.browser_destroyed(browser.identifier())); }
        }

        fn on_context_created(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, context: Option<&mut V8Context>) {
            if let (Some(frame), Some(context)) = (frame, context) {
                // Before any page script, so it keeps the original style sheet API.
                crate::hiding::inject(frame, context);
                inject(browser.map(|browser| browser.clone()), frame, context);
                crate::media::inject(frame, context);
                crate::translate::inject(frame, context);
            }
        }

        fn on_context_released(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, context: Option<&mut V8Context>) {
            if let (Some(frame), Some(context)) = (frame, context) {
                release(frame, context);
                crate::media::release(frame, context);
                crate::translate::release(frame, context);
                crate::hiding::release(frame, context);
            }
        }

        fn on_process_message_received(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            let (Some(frame), Some(message)) = (frame, message) else { return 0 };
            // Player commands for the video reporter come only from the browser process too.
            if source_process == ProcessId::BROWSER && crate::media::command(frame, message) { return 1; }
            if source_process == ProcessId::BROWSER && crate::translate::command(frame, message) { return 1; }
            if source_process == ProcessId::BROWSER && crate::hiding::sheet(frame, message) { return 1; }
            if source_process != ProcessId::BROWSER { return 0; }
            if setting(message) {
                report_again(frame);
                return 1;
            }
            fill(frame, message) as ::std::os::raw::c_int
        }
    }
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "login_js_tests.rs"]
pub(crate) mod js_tests;
