//! The password vault preview's app-level state: the opened vault, the
//! clipboard timer and the Settings page draft. It outlives the Settings
//! sheet so the idle lock and clipboard clearing still run after it closes.
//! Nothing secret is logged, toasted or synced from here.

use crossbow_core::vault::generator::{self, PasswordRules};
use crossbow_core::vault::{EntryInput, IDLE_TIMEOUT, KeyLocation, MAX_NOTES_BYTES, MAX_ORIGIN_BYTES, MAX_PASSWORD_BYTES, MAX_USERNAME_BYTES, Moment, OsVaultKeyStore, RECOVERY_FILE, Recovery, UnlockOutcome, Vault, VaultError, VaultKeyStore, VaultStatus, Zeroize, Zeroizing};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

#[path = "page_logins.rs"]
mod page_logins;
pub use page_logins::{KeyState, OFFER_TIMEOUT, OFFER_TO_SAVE, OFFER_TO_SAVE_APPLIES, OFFER_TO_SAVE_PREVIEW, Offer, OfferKind, sync_offer_to_save};
#[cfg(test)]
pub use page_logins::{FILL_GONE, NEVER_SAVED, SAVED};
#[path = "page_addresses.rs"]
mod page_addresses;
pub use page_addresses::{
    ADDRESS_FIELD_IDS, AddressEditor, AddressOffer, NO_PROFILE, Section, address_field_id, address_hints, address_summary, address_title,
};
#[cfg(test)]
pub use page_addresses::{ADDRESS_ADDED, ADDRESS_DELETED, ADDRESS_GONE, ADDRESS_SAVED, PAGE_PARTS};
#[path = "page_cards.rs"]
mod page_cards;
pub use page_cards::{CARD_FIELD_IDS, CARD_FIELD_LIMITS, CardEditor, NO_CARD_PROFILE, card_field_id, card_title, this_month};
pub use page_cards::{MONTH, NAME, NICKNAME, NUMBER, YEAR};
#[cfg(test)]
pub use page_cards::{CARD_ADDED, CARD_DELETED, CARD_GONE, CARD_SAVED};

/// A copied password is cleared after this long, if nothing replaced it.
pub const CLIPBOARD_CLEAR: Duration = Duration::from_secs(30);
pub const SEARCH_CHARS: usize = 128;

/// Where a copied password goes. The production one is the macOS general
/// pasteboard; tests use a fake and never touch the real clipboard.
pub trait Pasteboard {
    /// Replace the contents with `text`, for this device only and marked as
    /// concealed. Returns the change count that identifies this copy.
    fn copy_secret(&mut self, text: &str) -> Result<isize, String>;
    fn change_count(&self) -> Option<isize>;
    fn clear(&mut self);
}

/// macOS Secure Event Input, which keeps other processes from reading
/// keystrokes. The system counts enables per process, so every enable must be
/// matched by exactly one disable.
pub trait SecureInput {
    fn enable(&mut self);
    fn disable(&mut self);
}

/// Secure Event Input where the platform has it; elsewhere nothing.
pub fn system_secure_input() -> Box<dyn SecureInput> {
    #[cfg(target_os = "macos")]
    { Box::new(mac::SystemSecureInput) }
    #[cfg(not(target_os = "macos"))]
    { Box::new(NoSecureInput) }
}

/// One balanced Secure Event Input hold, shared with the AppKit observers that
/// release it the moment Crossbow resigns active or hides, or a window
/// miniaturizes or resigns key.
struct SecureGate {
    input: Box<dyn SecureInput>,
    /// Whether this controller holds one enable.
    on: bool,
}

impl SecureGate {
    /// Only a change calls the system, so enables and disables stay balanced.
    fn set(&mut self, on: bool) {
        if on == self.on { return; }
        self.on = on;
        if on { self.input.enable(); } else { self.input.disable(); }
    }
}

#[cfg(not(target_os = "macos"))]
struct NoSecureInput;

#[cfg(not(target_os = "macos"))]
impl SecureInput for NoSecureInput {
    fn enable(&mut self) {}
    fn disable(&mut self) {}
}

/// What Reset would delete, for its confirmation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetScope {
    /// The saved passwords, addresses and cards in this vault file.
    Entries { passwords: usize, addresses: usize, cards: usize },
    /// A vault file that can't be opened, moved aside with whatever it holds.
    DamagedFile,
}

/// The system pasteboard, where Copy is supported.
pub fn system_pasteboard() -> Option<Box<dyn Pasteboard>> {
    #[cfg(target_os = "macos")]
    { mac::GeneralPasteboard::new().map(|board| Box::new(board) as Box<dyn Pasteboard>) }
    #[cfg(not(target_os = "macos"))]
    { None }
}

struct CopiedPassword {
    change_count: isize,
    at: Moment,
}

/// The Add or Edit form. Buffers reserve their whole limit up front, and the
/// fields cap what they accept, so typing never reallocates and leaves an
/// unwiped copy behind.
pub struct Editor {
    /// None for a new entry.
    pub id: Option<String>,
    pub origin: Zeroizing<String>,
    pub username: Zeroizing<String>,
    pub password: Zeroizing<String>,
    pub notes: Zeroizing<String>,
    /// The profile whose pages are offered this entry. Add uses the active
    /// profile; Edit keeps the entry's, including none.
    pub profile: Option<String>,
    pub show_password: bool,
    pub rules: PasswordRules,
    pub error: Option<String>,
    /// Move keyboard focus to the first field on the next frame.
    pub focus: bool,
}

/// A field for `limit` bytes accepts that many characters, and its buffer
/// reserves four bytes for each, so no character can make it reallocate.
/// Saving still enforces the byte limit.
fn buffer(limit: usize) -> Zeroizing<String> {
    Zeroizing::new(String::with_capacity(limit * 4))
}

fn fill(text: &str, limit: usize) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity((limit * 4).max(text.len())));
    out.push_str(text);
    out
}

impl Editor {
    fn new() -> Editor {
        Editor {
            id: None,
            origin: buffer(MAX_ORIGIN_BYTES),
            username: buffer(MAX_USERNAME_BYTES),
            password: buffer(MAX_PASSWORD_BYTES),
            notes: buffer(MAX_NOTES_BYTES),
            profile: None,
            show_password: false,
            rules: PasswordRules::default(),
            error: None,
            focus: true,
        }
    }
}

impl std::fmt::Debug for Editor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Editor").field("id", &self.id).finish_non_exhaustive()
    }
}

/// What the Passwords page shows. Cleared on lock and when the page is left.
#[derive(Default)]
pub struct PageState {
    pub search: Zeroizing<String>,
    pub editor: Option<Editor>,
    /// Opened passwords, by entry id, while Reveal is on.
    pub revealed: HashMap<String, Zeroizing<String>>,
    pub confirm_delete: Option<String>,
    pub confirm_reset: bool,
    pub confirm_recover: bool,
    pub confirm_open_without: bool,
    /// Whether Open Without Them's Cancel takes focus when it next draws.
    pub focus_cancel: bool,
    /// Whether Key Details is open. Not secret.
    pub key_details: bool,
    pub notice: Option<String>,
    pub error: Option<VaultError>,
    /// The entry or address the notice or error is about, shown beneath it;
    /// None shows it at the top of the page.
    pub anchor: Option<String>,
    /// Which list shows while unlocked. Not secret.
    pub section: Section,
    pub address_editor: Option<AddressEditor>,
    pub confirm_delete_address: Option<String>,
    pub card_editor: Option<CardEditor>,
    pub confirm_delete_card: Option<String>,
}

/// egui's own state for every vault text field, so it can be dropped with the
/// draft: undo history keeps copies of typed text.
pub const FIELD_IDS: [&str; 5] = ["passwords-search", "passwords-origin", "passwords-username", "passwords-password", "passwords-notes"];

pub struct Passwords {
    data_dir: PathBuf,
    keys: Box<dyn Fn() -> Box<dyn VaultKeyStore>>,
    vault: Option<Vault>,
    open_error: Option<VaultError>,
    /// Whether Recover Passwords can run on the damaged file, once asked;
    /// forgotten whenever what the page says changes.
    recoverable: Option<bool>,
    /// The files a Recover Passwords that stopped left in the data folder, by
    /// name, as of the last open and the last time the page appeared.
    left_by_recover: Vec<String>,
    /// The files of stopped Recovers that Open Without Them set aside, by
    /// name, as of the same times.
    set_aside: Vec<String>,
    /// Whether the page was drawn at the last tick.
    page_was_open: bool,
    pasteboard: Option<Box<dyn Pasteboard>>,
    copied: Option<CopiedPassword>,
    secure_input: Rc<RefCell<SecureGate>>,
    /// Releases Secure Event Input when the app resigns active or hides, or a
    /// window miniaturizes or resigns key.
    deactivation: Option<deactivation::Observers>,
    clock: Box<dyn Fn() -> Moment>,
    /// The signed-in account at the last tick, once known.
    account: Option<Option<String>>,
    /// The last input while a new entry's draft is kept through a lock caused
    /// by a reset in another window, starting with the last input before it.
    /// The draft is discarded 5 minutes after it.
    kept: Option<Moment>,
    ctx: Option<egui::Context>,
    /// The active profile and every profile's name, set by the app each frame.
    profile: String,
    profile_names: Vec<(String, String)>,
    /// Logins pages submitted, waiting for Save, Never for This Site or Not Now.
    offers: Vec<Offer>,
    /// Addresses pages submitted, waiting the same way.
    address_offers: Vec<AddressOffer>,
    /// The last offer's revision; every new offer, of a login or an address, takes the next.
    offer_revision: u64,
    /// "Offer to save passwords from websites", by profile, as loaded from this
    /// device's settings or switched since; a profile not listed is off.
    offer_to_save: HashMap<String, bool>,
    /// Switch changes the app has yet to save.
    offer_to_save_writes: Vec<(String, bool)>,
    /// Why a profile's switch could not be read or saved.
    offer_to_save_error: Option<(String, String)>,
    pub page: PageState,
}

impl Passwords {
    pub fn new(data_dir: &Path) -> Passwords {
        let dir = data_dir.to_path_buf();
        let mut passwords = Passwords::with_parts(data_dir, Box::new(move || Box::new(OsVaultKeyStore::new(&dir)) as Box<dyn VaultKeyStore>), system_pasteboard(), Box::new(Moment::now));
        passwords.observe_app_deactivation();
        passwords
    }

    pub fn with_parts(data_dir: &Path, keys: Box<dyn Fn() -> Box<dyn VaultKeyStore>>, pasteboard: Option<Box<dyn Pasteboard>>, clock: Box<dyn Fn() -> Moment>) -> Passwords {
        Passwords {
            data_dir: data_dir.to_path_buf(), keys, vault: None, open_error: None, recoverable: None, left_by_recover: Vec::new(), set_aside: Vec::new(), page_was_open: false, pasteboard, copied: None, clock,
            secure_input: Rc::new(RefCell::new(SecureGate { input: system_secure_input(), on: false })), deactivation: None,
            account: None, kept: None, ctx: None, profile: String::new(), profile_names: Vec::new(), offers: Vec::new(), address_offers: Vec::new(), offer_revision: 0,
            offer_to_save: HashMap::new(), offer_to_save_writes: Vec::new(), offer_to_save_error: None,
            page: PageState { search: buffer(SEARCH_CHARS), ..PageState::default() },
        }
    }

    fn now(&self) -> Moment {
        (self.clock)()
    }

    /// Once per frame, before the UI and while hidden: lock on sign-out, an
    /// account change or idle time, clear an expired copy, and hide what the
    /// page showed once Settings leaves the Passwords page. A hidden or
    /// minimised window with the page open keeps the draft until lock.
    pub fn tick(&mut self, ctx: &egui::Context, account: Option<&str>, page_open: bool) {
        if let Some(wait) = self.tick_at(account, page_open) {
            ctx.request_repaint_after(wait);
        }
    }

    /// The same as `tick`, returning when it next needs to run.
    pub fn tick_at(&mut self, account: Option<&str>, page_open: bool) -> Option<Duration> {
        let now = self.now();
        let account = account.map(str::to_owned);
        if let Some(previous) = self.account.replace(account.clone()) {
            let signed_out = account.is_none();
            // A page's login or address never outlives the account it was typed under.
            if previous != account {
                self.offers.clear();
                self.address_offers.clear();
            }
            if previous != account && self.unlocked() {
                self.lock(Some(if signed_out { "Locked because you signed out." } else { "Locked because the signed-in account changed." }));
            } else if previous != account && self.kept.is_some() {
                // A draft never outlives the account it was typed under.
                self.discard_kept(if signed_out { "You signed out, so the unsaved entry was discarded." } else { "The signed-in account changed, so the unsaved entry was discarded." });
            }
        }
        if self.vault.as_mut().is_some_and(|vault| vault.lock_if_idle(now)) {
            self.wipe_page();
            self.page.notice = Some("Locked after 5 minutes without activity.".into());
        }
        // The same limit as a draft in an unlocked vault, which the idle lock wipes.
        if self.kept.is_some_and(|since| now.since(since) >= IDLE_TIMEOUT) {
            self.discard_kept("The unsaved entry was discarded after 5 minutes without activity.");
        }
        if !page_open {
            self.hide_page();
        } else if !self.page_was_open {
            self.find_left_by_recover();
        }
        self.page_was_open = page_open;
        self.offers.retain(|offer| now.since(offer.at) < OFFER_TIMEOUT);
        self.address_offers.retain(|offer| now.since(offer.at) < OFFER_TIMEOUT);
        if let Some(copied) = &self.copied {
            if now.since(copied.at) >= CLIPBOARD_CLEAR {
                self.clear_copy();
                if self.page.notice.as_deref() == Some(COPIED) { self.page.notice = Some("Clipboard cleared.".into()); }
            }
        }
        let idle = self.vault.as_ref().and_then(|vault| vault.idle_remaining(now));
        let copy = self.copied.as_ref().map(|copied| CLIPBOARD_CLEAR.saturating_sub(now.since(copied.at)));
        // While the page holds an opened password or a draft, tick every second,
        // so they are hidden soon after Settings closes even if nothing repaints.
        let holding = (!self.page.revealed.is_empty() || self.page.editor.is_some() || self.page.address_editor.is_some() || self.page.card_editor.is_some()).then_some(Duration::from_secs(1));
        let ats = self.offers.iter().map(|offer| offer.at).chain(self.address_offers.iter().map(|offer| offer.at));
        let offer = ats.map(|at| OFFER_TIMEOUT.saturating_sub(now.since(at))).min();
        // A small margin, so the deadline has passed when the next tick runs.
        idle.into_iter().chain(copy).chain(holding).chain(offer).min().map(|wait| wait + Duration::from_millis(50))
    }

    /// Clear a copied password that is still on the clipboard, and lock.
    pub fn shutdown(&mut self) {
        self.clear_copy();
        self.lock(None);
    }

    fn clear_copy(&mut self) {
        let Some(copied) = self.copied.take() else { return };
        if let Some(board) = &mut self.pasteboard {
            // Only our own copy: anything copied since is left alone.
            if board.change_count() == Some(copied.change_count) { board.clear(); }
        }
    }

    // -- for the page --------------------------------------------------------------------

    /// Called by the page each time it is drawn.
    pub fn page_shown(&mut self, ctx: &egui::Context) {
        if self.ctx.is_none() { self.ctx = Some(ctx.clone()); }
    }

    /// User input on the page counts as vault activity.
    pub fn touch(&mut self) {
        let now = self.now();
        if let Some(vault) = &mut self.vault { vault.touch(now); }
        if self.kept.is_some() { self.kept = Some(now); }
    }

    /// Open the vault file once. A failure stays until Try Again.
    fn ensure_open(&mut self) {
        if self.vault.is_some() || self.open_error.is_some() { return; }
        match Vault::open(&self.data_dir, (self.keys)()) {
            Ok(vault) => self.vault = Some(vault),
            Err(error) => self.open_error = Some(error),
        }
        self.find_left_by_recover();
    }

    fn find_left_by_recover(&mut self) {
        self.left_by_recover = Vault::kept_from_stopped_recover(&self.data_dir);
        self.set_aside = Vault::set_aside_from_stopped_recover(&self.data_dir);
    }

    /// What the page says, whatever its state, while a Recover Passwords
    /// that finished but stopped before it could say so has left the damaged
    /// file under its kept name. Those files may hold passwords the vault
    /// doesn't, so nothing here says they may be moved or deleted.
    pub fn left_by_recover(&self) -> Option<String> {
        let hold = match self.left_by_recover.as_slice() {
            [] => return None,
            [_] => "It holds",
            _ => "They hold",
        };
        Some(format!("A Recover Passwords that stopped before saying where it kept the damaged vault file left {} in Crossbow's data folder. {hold} that file as it was, with any passwords Recover couldn't read.", and_list(&self.left_by_recover)))
    }

    /// What the page says, whatever its state, while files of a stopped
    /// Recover Passwords that Open Without Them set aside are in the data
    /// folder. They may hold passwords the vault doesn't, and are never put
    /// back or written over, so nothing here says they may be moved or deleted.
    pub fn set_aside_by_recover(&self) -> Option<String> {
        let hold = match self.set_aside.as_slice() {
            [] => return None,
            [_] => "It may hold",
            _ => "They may hold",
        };
        Some(format!("A Recover Passwords that stopped before it finished left {} in Crossbow's data folder. {hold} passwords the vault doesn't.", and_list(&self.set_aside)))
    }

    /// Open Without Them's question, for the error a stopped Recover
    /// Passwords keeps the vault closed with: what won't be in the vault,
    /// that kept files stay, and that Recover's working file, if it is
    /// there, is deleted. None for any other state.
    pub fn open_without_question(&self) -> Option<String> {
        let question = match self.open_error.as_ref()? {
            VaultError::KeptFilesGone(files) => format!("Open the vault file without {}? Anything saved only in them won't be in the vault.", and_list(files)),
            VaultError::UndoIncomplete(files) | VaultError::UnrecordedStop(files) => {
                let kept: Vec<String> = files.iter().filter(|name| *name != RECOVERY_FILE).cloned().collect();
                format!("Open the vault file as it is now? Passwords, addresses and cards that are only in {} won't be in the vault. The files stay in Crossbow's data folder, and Crossbow won't put them back or write over them.", and_list(&kept))
            }
            _ => return None,
        };
        let working = std::fs::symlink_metadata(self.data_dir.join(RECOVERY_FILE)).is_ok();
        Some(if working { format!("{question} Recover's working file, {RECOVERY_FILE}, which was made from them, is deleted.") } else { question })
    }

    /// The vault's state, opening its file on first use.
    pub fn status(&mut self) -> Result<VaultStatus, VaultError> {
        self.ensure_open();
        match &self.vault {
            Some(vault) => vault.status(),
            None => Err(self.open_error.clone().unwrap_or(VaultError::Storage)),
        }
    }

    pub fn vault(&self) -> Option<&Vault> {
        self.vault.as_ref()
    }

    /// The vault and the page draft at once, for drawing the list.
    pub fn parts(&mut self) -> (Option<&Vault>, &mut PageState) {
        (self.vault.as_ref(), &mut self.page)
    }

    #[cfg(test)]
    pub fn vault_mut(&mut self) -> Option<&mut Vault> {
        self.vault.as_mut()
    }

    pub fn unlocked(&self) -> bool {
        self.vault.as_ref().is_some_and(Vault::is_unlocked)
    }

    /// Where the vault key is kept: the keychain, or the QA key file.
    pub fn key_location(&self) -> KeyLocation {
        self.vault.as_ref().map_or_else(|| (self.keys)().location(), Vault::key_location)
    }

    /// What Reset would delete now.
    pub fn reset_scope(&self) -> ResetScope {
        match self.vault.as_ref().map(|vault| Ok::<_, VaultError>((vault.count()?, vault.address_count()?, vault.card_count()?))) {
            Some(Ok((passwords, addresses, cards))) => ResetScope::Entries { passwords, addresses, cards },
            _ => ResetScope::DamagedFile,
        }
    }

    /// Hold Secure Event Input while a vault password field has focus. Called
    /// by the page every frame; only a change calls the system, so enables and
    /// disables stay balanced.
    pub fn set_secure_input(&mut self, on: bool) {
        self.secure_input.borrow_mut().set(on);
    }

    /// Release Secure Event Input as soon as Crossbow resigns active or hides,
    /// or a window miniaturizes or resigns key, without waiting for a frame.
    pub fn observe_app_deactivation(&mut self) {
        self.deactivation = Some(deactivation::observe(&self.secure_input));
    }

    #[cfg(test)]
    pub fn use_secure_input(&mut self, input: Box<dyn SecureInput>) {
        self.secure_input.borrow_mut().input = input;
    }

    pub fn can_copy(&self) -> bool {
        self.pasteboard.is_some()
    }

    /// Retry opening the vault file after an error.
    pub fn retry_open(&mut self) {
        self.vault = None;
        self.open_error = None;
        self.page.error = None;
        self.page.confirm_reset = false;
        self.page.confirm_recover = false;
        self.page.confirm_open_without = false;
        self.recoverable = None;
        self.ensure_open();
    }

    fn say(&mut self, anchor: Option<&str>, notice: Option<&str>, error: Option<VaultError>) {
        self.recoverable = None;
        self.page.anchor = anchor.map(str::to_owned);
        self.page.notice = notice.map(str::to_owned);
        self.page.error = error;
    }

    pub fn unlock(&mut self) {
        let now = self.now();
        self.page.confirm_reset = false;
        self.page.confirm_recover = false;
        self.page.confirm_open_without = false;
        let Some(vault) = &mut self.vault else { return };
        match vault.unlock(now) {
            Ok(outcome) => {
                // A kept draft is now an ordinary one, which the idle lock wipes.
                self.kept = None;
                self.say(None, (outcome == UnlockOutcome::CheckRepaired).then_some(CHECK_REPAIRED), None);
            }
            Err(error) => self.say(None, None, Some(error)),
        }
    }

    /// Lock and wipe everything the page opened.
    pub fn lock(&mut self, notice: Option<&str>) {
        self.set_secure_input(false);
        if let Some(vault) = &mut self.vault { vault.lock(); }
        self.wipe_page();
        self.say(None, notice, None);
    }

    /// Drop revealed passwords, the drafts, an address or a card opened for
    /// editing, confirmations and messages, and egui's copies of the draft
    /// fields. The search query stays while unlocked.
    fn hide_page(&mut self) {
        self.set_secure_input(false);
        let page = &mut self.page;
        let address = page.address_editor.is_some() || page.confirm_delete_address.is_some();
        let card = page.card_editor.is_some() || page.confirm_delete_card.is_some();
        if !address && !card && page.revealed.is_empty() && page.editor.is_none() && page.confirm_delete.is_none() && !page.confirm_reset && !page.confirm_recover && !page.confirm_open_without && page.notice.is_none() && page.error.is_none() { return; }
        page.revealed.clear();
        page.editor = None;
        page.address_editor = None;
        page.card_editor = None;
        self.kept = None;
        page.confirm_delete = None;
        page.confirm_delete_address = None;
        page.confirm_delete_card = None;
        page.confirm_reset = false;
        page.confirm_recover = false;
        page.confirm_open_without = false;
        self.say(None, None, None);
        self.forget_fields(&FIELD_IDS[1..]);
        self.forget_fields(&ADDRESS_FIELD_IDS);
        self.forget_fields(&CARD_FIELD_IDS);
    }

    fn wipe_page(&mut self) {
        self.hide_page();
        self.page.search.zeroize();
        self.forget_fields(&FIELD_IDS);
    }

    fn forget_fields(&self, names: &[&str]) {
        let Some(ctx) = &self.ctx else { return };
        ctx.data_mut(|data| {
            for name in names { data.remove::<egui::text_edit::TextEditState>(field_id(name)); }
        });
        ctx.memory_mut(|memory| {
            if let Some(focused) = memory.focused().filter(|id| names.iter().any(|name| field_id(name) == *id)) {
                memory.surrender_focus(focused);
            }
        });
    }

    /// Show a vault error on entry `id`; a reset in another window is handled
    /// as every write that finds one handles it.
    fn refused(&mut self, id: &str, last_input: Option<Moment>, error: VaultError) {
        if error == VaultError::ResetElsewhere { self.reset_elsewhere(last_input) } else { self.say(Some(id), None, Some(error)) }
    }

    pub fn toggle_reveal(&mut self, id: &str) {
        if self.page.revealed.remove(id).is_some() { return; }
        let Some(vault) = &self.vault else { return };
        let last_input = vault.last_activity();
        match vault.password(id) {
            Ok(password) => { self.page.revealed.insert(id.to_owned(), password); }
            Err(error) => self.refused(id, last_input, error),
        }
    }

    pub fn copy(&mut self, id: &str) {
        let Some(vault) = &self.vault else { return };
        let last_input = vault.last_activity();
        let password = match vault.password(id) {
            Ok(password) => password,
            Err(error) => return self.refused(id, last_input, error),
        };
        let Some(board) = &mut self.pasteboard else { return self.say(Some(id), Some(COPY_UNAVAILABLE), None) };
        match board.copy_secret(&password) {
            Ok(change_count) => {
                self.copied = Some(CopiedPassword { change_count, at: self.now() });
                self.say(Some(id), Some(COPIED), None);
            }
            Err(message) => self.say(Some(id), Some(&message), None),
        }
    }

    pub fn start_add(&mut self) {
        self.close_editor();
        self.page.confirm_delete = None;
        self.say(None, None, None);
        let mut editor = Editor::new();
        editor.profile = Some(self.profile.clone()).filter(|profile| !profile.is_empty());
        self.page.editor = Some(editor);
    }

    pub fn start_edit(&mut self, id: &str) {
        self.close_editor();
        self.page.confirm_delete = None;
        self.say(None, None, None);
        let Some(vault) = &self.vault else { return };
        let last_input = vault.last_activity();
        let (Some(entry), secret) = (vault.entry(id), vault.secret(id)) else { return };
        match secret {
            Ok(secret) => {
                let mut editor = Editor::new();
                editor.id = Some(id.to_owned());
                editor.origin = fill(&entry.origin, MAX_ORIGIN_BYTES);
                editor.username = fill(&entry.username, MAX_USERNAME_BYTES);
                editor.password = fill(&secret.password, MAX_PASSWORD_BYTES);
                editor.notes = fill(&secret.notes, MAX_NOTES_BYTES);
                editor.profile = entry.profile.clone();
                self.page.editor = Some(editor);
            }
            Err(error) => self.refused(id, last_input, error),
        }
    }

    pub fn close_editor(&mut self) {
        self.kept = None;
        if self.page.editor.take().is_some() { self.forget_fields(&FIELD_IDS[1..]); }
    }

    /// Whether the page holds a draft through a lock, from a reset in another
    /// window. Locked, it is never shown; Unlock brings it back to save.
    pub fn kept_draft(&self) -> bool {
        self.kept.is_some()
    }

    /// Discard Entry on the locked page.
    pub fn discard_draft(&mut self) {
        self.discard_kept(DISCARDED);
    }

    fn discard_kept(&mut self, why: &str) {
        self.close_editor();
        self.say(None, Some(why), None);
    }

    pub fn generate(&mut self) {
        let Some(editor) = &mut self.page.editor else { return };
        match generator::generate(&editor.rules) {
            Ok(password) => {
                editor.password.zeroize();
                editor.password.push_str(&password);
                editor.error = None;
            }
            Err(error) => editor.error = Some(error.to_string()),
        }
    }

    pub fn save_editor(&mut self) {
        let (Some(vault), Some(editor)) = (&mut self.vault, &mut self.page.editor) else { return };
        let input = EntryInput { origin: &editor.origin, username: &editor.username, password: &editor.password, notes: &editor.notes, profile: editor.profile.as_deref() };
        // Read first: a write that finds a reset locks the vault.
        let last_input = vault.last_activity();
        let result = match &editor.id {
            Some(id) => vault.update(id, &input).map(|_| id.clone()),
            None => vault.add(&input),
        };
        match result {
            Ok(id) => {
                let edited = editor.id.is_some();
                self.page.revealed.remove(&id);
                self.close_editor();
                self.say(Some(&id), Some(if edited { "Password saved." } else { "Password added." }), None);
            }
            Err(VaultError::ResetElsewhere) => self.reset_elsewhere(last_input),
            // The draft stays for a correction or another try.
            Err(error) => editor.error = Some(error.to_string()),
        }
    }

    /// A write, a read of an entry whose row is gone, or Key Details found the
    /// vault reset in another window: every path that can goes through here.
    /// Lock, as the core does for a write, wipe what the page opened and say why.
    /// A new entry's draft is the user's own typing, so it is kept, hidden, to
    /// save into the new vault after Unlock, until 5 minutes after the last
    /// input, as the idle lock would have wiped it. An Edit draft holds the
    /// entry's opened password and notes, which the reset deleted, so it goes
    /// with every other value opened from the old vault, a copy included.
    /// `last_input` is the vault's last activity, read before the call that
    /// found the reset locked it.
    fn reset_elsewhere(&mut self, last_input: Option<Moment>) {
        let last_input = last_input.unwrap_or_else(|| self.now());
        let draft = self.page.editor.take();
        self.clear_copy();
        self.lock(None);
        match draft {
            Some(mut draft) if draft.id.is_none() => {
                draft.error = None;
                self.page.editor = Some(draft);
                self.page.error = Some(VaultError::ResetElsewhere);
                self.kept = Some(last_input);
            }
            Some(edit) => {
                // Dropping it wipes its buffers; egui's copies of the fields go too.
                drop(edit);
                self.forget_fields(&FIELD_IDS[1..]);
                self.say(None, Some(RESET_ELSEWHERE_EDIT_DISCARDED), None);
            }
            None => self.page.error = Some(VaultError::ResetElsewhere),
        }
    }

    /// The vault id behind the key in use, for Key Details. Re-read each time,
    /// so a reset in another window never leaves an old id on screen; an
    /// unlocked session it catches out locks, as a write would.
    pub fn live_key(&mut self) -> Option<(KeyLocation, String)> {
        let unlocked = self.unlocked();
        let vault = self.vault.as_mut()?;
        let last_input = vault.last_activity();
        let changed = vault.follow_reset().ok()?;
        let key = (vault.key_location(), vault.id().to_owned());
        if changed && unlocked { self.reset_elsewhere(last_input); }
        Some(key)
    }

    pub fn request_delete(&mut self, id: &str) {
        self.close_editor();
        self.say(None, None, None);
        self.page.confirm_delete = Some(id.to_owned());
    }

    pub fn confirm_delete(&mut self) {
        let (Some(vault), Some(id)) = (&mut self.vault, self.page.confirm_delete.clone()) else { return };
        let last_input = vault.last_activity();
        match vault.delete(&id) {
            Ok(()) => {
                self.page.confirm_delete = None;
                self.page.revealed.remove(&id);
                self.say(None, Some("Password deleted."), None);
            }
            Err(VaultError::NotFound) => {
                self.page.confirm_delete = None;
                self.say(None, Some("That password was already deleted."), None);
            }
            Err(VaultError::ResetElsewhere) => self.reset_elsewhere(last_input),
            // The confirmation stays so Delete can be retried.
            Err(error) => self.say(Some(&id), None, Some(error)),
        }
    }

    /// Empty the vault under a new vault id, or move a file too damaged to open,
    /// or that Reset left damaged, aside for a new vault. Reset is reported only
    /// once the file was checked; see `Vault::reset`. No key is deleted.
    pub fn reset(&mut self) {
        let result = match self.vault.as_mut().map(Vault::reset) {
            Some(Err(VaultError::Damaged)) => self.replace_damaged_file().map(Some),
            None if self.open_error == Some(VaultError::Damaged) => self.replace_damaged_file().map(Some),
            None => return,
            Some(result) => result.map(|()| None),
        };
        self.page.confirm_reset = false;
        self.recoverable = None;
        match result {
            Ok(moved) => {
                self.wipe_page();
                let notice = moved.map_or_else(|| RESET_DONE.to_owned(), |name| moved_aside(&name));
                self.say(None, Some(&notice), None);
            }
            Err(error) => self.page.error = Some(error),
        }
    }

    /// Whether Recover Passwords is offered: the vault file is damaged, at
    /// open or at unlock, and its vault id still names a key the key store
    /// holds. Read from the file and the key store once per message the page shows.
    pub fn can_recover(&mut self) -> bool {
        let damaged = match &self.vault {
            None => self.open_error == Some(VaultError::Damaged),
            Some(_) => self.page.error == Some(VaultError::Damaged),
        };
        if !damaged { return false; }
        if self.recoverable.is_none() { self.recoverable = Some(Vault::can_recover(&self.data_dir, (self.keys)().as_ref())); }
        self.recoverable == Some(true)
    }

    /// Recover Passwords: close the file, copy every entry that still opens
    /// with the vault's key into a new file under the same key, put it in
    /// place, move the damaged one aside and open the new one, locked. See
    /// `Vault::recover_damaged_file`. A failure leaves the damaged file where
    /// it was and says why, for Try Again or another Recover.
    pub fn recover(&mut self) {
        self.page.confirm_recover = false;
        self.vault = None;
        let result = Vault::recover_damaged_file(&self.data_dir, (self.keys)().as_ref());
        self.open_error = None;
        self.ensure_open();
        self.recoverable = None;
        match result {
            Ok(recovery) => {
                self.wipe_page();
                self.say(None, Some(&recovered(&recovery)), None);
            }
            Err(error) => self.page.error = Some(error),
        }
    }

    /// Open Without Them, once confirmed, while a stopped Recover Passwords
    /// keeps the vault closed: its files are set aside, never put back or
    /// written over, and the vault file opens as it is, as Try Again opens
    /// it. See `Vault::open_without_stopped_recover`. A failure says why
    /// beneath the error that still keeps the vault closed.
    pub fn open_without(&mut self) {
        self.page.confirm_open_without = false;
        if !self.open_error.as_ref().is_some_and(VaultError::can_open_without) { return; }
        let result = Vault::open_without_stopped_recover(&self.data_dir);
        self.retry_open();
        if let Err(error) = result {
            if self.open_error.as_ref() != Some(&error) { self.page.error = Some(error); }
        }
    }

    /// Close the file, move it aside and open a new one. The new name.
    fn replace_damaged_file(&mut self) -> Result<String, VaultError> {
        self.vault = None;
        let name = Vault::replace_damaged_file(&self.data_dir)?;
        self.open_error = None;
        self.ensure_open();
        self.open_error.clone().map_or(Ok(name), Err)
    }
}

/// A controller dropped without `shutdown` still releases Secure Event Input.
impl Drop for Passwords {
    fn drop(&mut self) {
        self.set_secure_input(false);
    }
}

/// The egui id of one of `FIELD_IDS`.
pub fn field_id(name: &str) -> egui::Id {
    egui::Id::unique(name)
}

/// After Reset emptied the vault file and checked it.
pub const RESET_DONE: &str = "The vault was reset. Unlock to start a new one.";

/// After a damaged vault file was moved aside as `name` for a new vault.
pub fn moved_aside(name: &str) -> String {
    format!("The damaged vault file was moved aside as {name} in Crossbow's data folder, readable only by your user account. Unlock to start a new vault.")
}

/// After Recover Passwords put a new vault file in place of a damaged one:
/// what it got back of the rows it saw, whether damage hid any from every
/// read, what it couldn't read, where the damaged file is kept, and every
/// file an earlier Recover that stopped left.
pub fn recovered(recovery: &Recovery) -> String {
    const PASSWORD: (&str, &str) = ("password", "passwords");
    const ADDRESS: (&str, &str) = ("address", "addresses");
    const CARD: (&str, &str) = ("card", "cards");
    const SITE: (&str, &str) = ("never-save site", "never-save sites");
    let count = |n: usize, (one, many): (&str, &str)| format!("{n} {}", if n == 1 { one } else { many });
    let list = and_list;
    let (addresses, cards, never_save) = (&recovery.addresses, &recovery.cards, &recovery.never_save);
    let mut read = vec![format!("{} of {}", recovery.recovered, count(recovery.found, PASSWORD))];
    if addresses.found > 0 { read.push(format!("{} of {}", addresses.recovered, count(addresses.found, ADDRESS))); }
    if cards.found > 0 { read.push(format!("{} of {}", cards.recovered, count(cards.found, CARD))); }
    if never_save.found > 0 { read.push(format!("{} of {}", never_save.recovered, count(never_save.found, SITE))); }
    let mut text = format!("Recovered {}.", list(&read));
    if !recovery.complete { text += " Part of the file couldn't be read, so it may have held more passwords."; }
    if !addresses.complete { text += " The addresses couldn't be read in full."; }
    if !cards.complete { text += " The cards couldn't be read in full."; }
    if !never_save.complete { text += " The never-save list couldn't be read in full."; }
    let kept = format!("kept as {} in Crossbow's data folder, readable only by your user account.", recovery.moved);
    // What an earlier Recover that stopped left, which no notice may have named.
    let also = match recovery.also_kept.as_slice() {
        [] => String::new(),
        files => format!(" A Recover Passwords that stopped earlier also left {} there.", list(files)),
    };
    let lost = [
        (recovery.found.saturating_sub(recovery.recovered), PASSWORD),
        (addresses.found.saturating_sub(addresses.recovered), ADDRESS),
        (cards.found.saturating_sub(cards.recovered), CARD),
        (never_save.found.saturating_sub(never_save.recovered), SITE),
    ];
    let unread = match lost {
        [(0, _), (0, _), (0, _), (0, _)] => return format!("{text} The damaged vault file is {kept}{also}"),
        // Passwords alone read as they always have: "3 couldn't be read".
        [(passwords, _), (0, _), (0, _), (0, _)] => passwords.to_string(),
        lost => list(&lost.iter().filter(|(n, _)| *n > 0).map(|(n, name)| count(*n, *name)).collect::<Vec<_>>()),
    };
    format!("{text} {unread} couldn't be read; the damaged file is {kept}{also}")
}

/// "a", "a and b", "a, b and c".
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
    }
}

/// A quiet line after an unlock that rewrote a damaged check value.
pub const CHECK_REPAIRED: &str = "The vault's check value was damaged, so it was rewritten. Your passwords are unchanged.";
pub const COPIED: &str = "Password copied. It will be cleared from the clipboard in 30 seconds unless something else is copied first.";
pub const COPY_UNAVAILABLE: &str = "Copying passwords is only available on macOS in this preview.";
/// In place of `VaultError::ResetElsewhere` while a draft is kept.
pub const RESET_ELSEWHERE_KEPT: &str = "The vault was reset in another window. Unlock again to save this entry.";
/// After a reset in another window removed the entry an Edit draft held.
pub const RESET_ELSEWHERE_EDIT_DISCARDED: &str = "The vault was reset in another window. The entry you were editing was removed, so your changes were discarded.";
pub const DISCARDED: &str = "The unsaved entry was discarded.";

#[cfg(target_os = "macos")]
mod mac {
    use super::Pasteboard;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    const PLAIN_TEXT: &str = "public.utf8-plain-text";
    /// nspasteboard.org: clipboard managers that honour it don't record this item.
    const CONCEALED: &str = "org.nspasteboard.ConcealedType";
    /// NSPasteboardContentsCurrentHostOnly: no Universal Clipboard hand-off.
    const CURRENT_HOST_ONLY: usize = 1;

    pub struct GeneralPasteboard(Retained<AnyObject>);

    impl GeneralPasteboard {
        pub fn new() -> Option<GeneralPasteboard> {
            let board: Option<Retained<AnyObject>> = unsafe { msg_send![class!(NSPasteboard), generalPasteboard] };
            board.map(GeneralPasteboard)
        }

        /// A private, uniquely named pasteboard, so tests never touch the user's clipboard.
        #[cfg(test)]
        pub fn unique() -> Option<GeneralPasteboard> {
            let board: Option<Retained<AnyObject>> = unsafe { msg_send![class!(NSPasteboard), pasteboardWithUniqueName] };
            board.map(GeneralPasteboard)
        }

        #[cfg(test)]
        pub fn text(&self) -> Option<String> {
            let kind = NSString::from_str(PLAIN_TEXT);
            let text: Option<Retained<NSString>> = unsafe { msg_send![&*self.0, stringForType: &*kind] };
            text.map(|text| text.to_string())
        }

        /// Another app's copy, for tests.
        #[cfg(test)]
        pub fn copy_plain(&self, text: &str) {
            let _: isize = unsafe { msg_send![&*self.0, clearContents] };
            let (text, kind) = (NSString::from_str(text), NSString::from_str(PLAIN_TEXT));
            let _: bool = unsafe { msg_send![&*self.0, setString: &*text, forType: &*kind] };
        }

        /// Each pasteboard item's types, in the order they were declared.
        #[cfg(test)]
        pub fn item_types(&self) -> Vec<Vec<String>> {
            let items: Option<Retained<AnyObject>> = unsafe { msg_send![&*self.0, pasteboardItems] };
            let Some(items) = items else { return Vec::new() };
            let count: usize = unsafe { msg_send![&*items, count] };
            (0..count).map(|index| {
                let item: Retained<AnyObject> = unsafe { msg_send![&*items, objectAtIndex: index] };
                let types: Retained<AnyObject> = unsafe { msg_send![&*item, types] };
                let count: usize = unsafe { msg_send![&*types, count] };
                (0..count).map(|index| {
                    let kind: Retained<NSString> = unsafe { msg_send![&*types, objectAtIndex: index] };
                    kind.to_string()
                }).collect()
            }).collect()
        }

        #[cfg(test)]
        pub fn release(self) {
            let _: () = unsafe { msg_send![&*self.0, releaseGlobally] };
        }
    }

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn EnableSecureEventInput() -> i32;
        fn DisableSecureEventInput() -> i32;
    }

    pub struct SystemSecureInput;

    impl super::SecureInput for SystemSecureInput {
        fn enable(&mut self) {
            unsafe { EnableSecureEventInput() };
        }

        fn disable(&mut self) {
            unsafe { DisableSecureEventInput() };
        }
    }

    impl Pasteboard for GeneralPasteboard {
        fn copy_secret(&mut self, text: &str) -> Result<isize, String> {
            // AppKit keeps its own copy of the string; it can't be wiped from here.
            let (text, plain, concealed, empty) = (NSString::from_str(text), NSString::from_str(PLAIN_TEXT), NSString::from_str(CONCEALED), NSString::from_str(""));
            // One item that declares the concealed marker before the text, written
            // in one call, so no reader sees the text without the marker.
            let item: Retained<AnyObject> = unsafe { msg_send![class!(NSPasteboardItem), new] };
            let marked: bool = unsafe { msg_send![&*item, setString: &*empty, forType: &*concealed] };
            let filled: bool = unsafe { msg_send![&*item, setString: &*text, forType: &*plain] };
            if !(marked && filled) { return Err("Couldn't copy the password. Try again.".into()); }
            let items: Retained<AnyObject> = unsafe { msg_send![class!(NSArray), arrayWithObject: &*item] };
            let _: isize = unsafe { msg_send![&*self.0, prepareForNewContentsWithOptions: CURRENT_HOST_ONLY] };
            let copied: bool = unsafe { msg_send![&*self.0, writeObjects: &*items] };
            if !copied { return Err("Couldn't copy the password. Try again.".into()); }
            Ok(unsafe { msg_send![&*self.0, changeCount] })
        }

        fn change_count(&self) -> Option<isize> {
            Some(unsafe { msg_send![&*self.0, changeCount] })
        }

        fn clear(&mut self) {
            let _: isize = unsafe { msg_send![&*self.0, clearContents] };
        }
    }
}

/// Releases Secure Event Input from AppKit's own notifications, so a hidden or
/// inactive Crossbow never waits for a frame, which eframe doesn't draw for a
/// hidden window. The page's per-frame release stays as a second path.
#[cfg(target_os = "macos")]
mod deactivation {
    use super::SecureGate;
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSApplicationDidHideNotification, NSApplicationDidResignActiveNotification, NSWindowDidMiniaturizeNotification, NSWindowDidResignKeyNotification};
    use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::ptr::NonNull;
    use std::rc::{Rc, Weak};

    thread_local! {
        /// The gates to release, by observer. Per thread, so a notification
        /// posted on another thread finds nothing to touch.
        static GATES: RefCell<HashMap<u64, Weak<RefCell<SecureGate>>>> = RefCell::default();
        static NEXT: Cell<u64> = const { Cell::new(0) };
    }

    pub struct Observers {
        id: u64,
        center: Retained<NSNotificationCenter>,
        tokens: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    }

    pub fn observe(gate: &Rc<RefCell<SecureGate>>) -> Observers {
        let id = NEXT.with(|next| next.replace(next.get() + 1));
        GATES.with(|gates| gates.borrow_mut().insert(id, Rc::downgrade(gate)));
        let center = NSNotificationCenter::defaultCenter();
        let release = RcBlock::new(move |_: NonNull<NSNotification>| release(id));
        // The app resigning active or hiding, and any window miniaturizing or
        // resigning key: ⌘M and another Crossbow window taking the keyboard
        // post no app notification. Only a drawn frame with the field focused
        // and the window focused holds it again.
        // SAFETY: Public AppKit notification names. The block captures only a
        // number and doesn't dereference the notification.
        let names = unsafe { [NSApplicationDidResignActiveNotification, NSApplicationDidHideNotification, NSWindowDidMiniaturizeNotification, NSWindowDidResignKeyNotification] };
        let tokens = names.into_iter()
            .map(|name| unsafe { center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &release) })
            .collect();
        Observers { id, center, tokens }
    }

    fn release(id: u64) {
        // try_with: a notification during thread teardown finds nothing.
        let gate = GATES.try_with(|gates| gates.borrow().get(&id).and_then(Weak::upgrade)).ok().flatten();
        // The gate is borrowed only inside its own enable or disable call. A
        // notification posted from inside one would be skipped; none is known
        // to be, and a hidden window draws no frame to release it later.
        if let Some(Ok(mut gate)) = gate.as_ref().map(|gate| gate.try_borrow_mut()) { gate.set(false); }
    }

    impl Drop for Observers {
        fn drop(&mut self) {
            for token in &self.tokens {
                // SAFETY: Tokens returned by this exact center.
                unsafe { self.center.removeObserver((**token).as_ref()) };
            }
            let _ = GATES.try_with(|gates| gates.borrow_mut().remove(&self.id));
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod deactivation {
    pub struct Observers;

    pub fn observe(_: &std::rc::Rc<std::cell::RefCell<super::SecureGate>>) -> Observers {
        Observers
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crossbow_core::vault::MemoryVaultKeyStore;

    /// Records what was copied and lets a test copy something else on top.
    #[derive(Clone, Default)]
    pub struct FakePasteboard(pub Rc<RefCell<(isize, Option<String>)>>);

    impl FakePasteboard {
        pub fn text(&self) -> Option<String> {
            self.0.borrow().1.clone()
        }

        pub fn other_app_copies(&self, text: &str) {
            let mut board = self.0.borrow_mut();
            board.0 += 1;
            board.1 = Some(text.into());
        }
    }

    impl Pasteboard for FakePasteboard {
        fn copy_secret(&mut self, text: &str) -> Result<isize, String> {
            self.other_app_copies(text);
            Ok(self.0.borrow().0)
        }

        fn change_count(&self) -> Option<isize> {
            Some(self.0.borrow().0)
        }

        fn clear(&mut self) {
            let mut board = self.0.borrow_mut();
            board.0 += 1;
            board.1 = None;
        }
    }

    /// Records each Secure Event Input call: true for enable, false for disable.
    #[derive(Clone, Default)]
    pub struct FakeSecureInput(pub Rc<RefCell<Vec<bool>>>);

    impl FakeSecureInput {
        pub fn calls(&self) -> Vec<bool> {
            self.0.borrow().clone()
        }

        /// Whether an enable is outstanding.
        pub fn held(&self) -> bool {
            let calls = self.0.borrow();
            calls.iter().filter(|on| **on).count() > calls.iter().filter(|on| !**on).count()
        }
    }

    impl SecureInput for FakeSecureInput {
        fn enable(&mut self) {
            self.0.borrow_mut().push(true);
        }

        fn disable(&mut self) {
            self.0.borrow_mut().push(false);
        }
    }

    /// A controller on a disposable data directory, a memory key store, a fake
    /// pasteboard and a clock the test moves by hand.
    pub struct Harness {
        pub dir: tempfile::TempDir,
        pub keys: MemoryVaultKeyStore,
        pub board: FakePasteboard,
        pub secure: FakeSecureInput,
        pub clock: Rc<RefCell<Moment>>,
        pub passwords: Passwords,
    }

    impl Harness {
        pub fn new() -> Harness {
            Harness::with_copy(true)
        }

        pub fn with_copy(copy: bool) -> Harness {
            let dir = tempfile::tempdir().unwrap();
            let keys = MemoryVaultKeyStore::default();
            let board = FakePasteboard::default();
            let clock = Rc::new(RefCell::new(Moment::now()));
            let (store, time) = (keys.clone(), clock.clone());
            let pasteboard = copy.then(|| Box::new(board.clone()) as Box<dyn Pasteboard>);
            let mut passwords = Passwords::with_parts(dir.path(), Box::new(move || Box::new(store.clone()) as Box<dyn VaultKeyStore>), pasteboard, Box::new(move || *time.borrow()));
            let secure = FakeSecureInput::default();
            passwords.use_secure_input(Box::new(secure.clone()));
            Harness { dir, keys, board, secure, clock, passwords }
        }

        /// Quit and relaunch: a new controller on the same files, key store,
        /// pasteboard and clock.
        pub fn reopen(&mut self) {
            self.passwords.shutdown();
            let (store, time) = (self.keys.clone(), self.clock.clone());
            let pasteboard = self.passwords.pasteboard.is_some().then(|| Box::new(self.board.clone()) as Box<dyn Pasteboard>);
            self.passwords = Passwords::with_parts(self.dir.path(), Box::new(move || Box::new(store.clone()) as Box<dyn VaultKeyStore>), pasteboard, Box::new(move || *time.borrow()));
            self.passwords.use_secure_input(Box::new(self.secure.clone()));
        }

        pub fn advance(&self, by: Duration) {
            let next = self.clock.borrow().after(by);
            *self.clock.borrow_mut() = next;
        }

        /// One frame with the page drawn, as the app's logic pass then the UI.
        pub fn shown_tick(&mut self, account: Option<&str>) -> Option<Duration> {
            self.passwords.tick_at(account, true)
        }

        pub fn unlock_with(&mut self, entries: &[(&str, &str, &str)]) -> Vec<String> {
            assert_ne!(self.passwords.status().unwrap(), VaultStatus::Unlocked);
            self.passwords.unlock();
            assert!(self.passwords.unlocked(), "{:?}", self.passwords.page.error);
            entries.iter().map(|(origin, username, password)| {
                self.passwords.vault.as_mut().unwrap().add(&EntryInput { origin, username, password, notes: "", profile: None }).unwrap()
            }).collect()
        }
    }

    #[test]
    fn copy_clears_after_30_seconds_only_if_the_clipboard_still_holds_it() {
        let mut h = Harness::new();
        let ids = h.unlock_with(&[("example.com", "fixture", "fixture-password-copy")]);
        h.shown_tick(None);
        h.passwords.copy(&ids[0]);
        assert_eq!(h.board.text().as_deref(), Some("fixture-password-copy"));
        assert_eq!(h.passwords.page.notice.as_deref(), Some(COPIED));
        let wait = h.shown_tick(None).unwrap();
        assert!(wait <= CLIPBOARD_CLEAR + Duration::from_millis(50) && wait > CLIPBOARD_CLEAR - Duration::from_secs(1), "a repaint is scheduled for the deadline: {wait:?}");
        h.advance(Duration::from_secs(29));
        h.shown_tick(None);
        assert_eq!(h.board.text().as_deref(), Some("fixture-password-copy"), "not before 30 seconds");
        h.advance(Duration::from_secs(1));
        h.shown_tick(None);
        assert_eq!(h.board.text(), None, "cleared at 30 seconds");
        assert_eq!(h.passwords.page.notice.as_deref(), Some("Clipboard cleared."));

        // Something copied since is never deleted.
        h.passwords.copy(&ids[0]);
        h.board.other_app_copies("fixture unrelated text");
        h.advance(CLIPBOARD_CLEAR);
        h.shown_tick(None);
        assert_eq!(h.board.text().as_deref(), Some("fixture unrelated text"));

        // Quitting clears our own copy early, and only ours.
        h.passwords.copy(&ids[0]);
        h.passwords.shutdown();
        assert_eq!(h.board.text(), None);
        assert!(!h.passwords.unlocked());
    }

    #[test]
    fn copy_is_unavailable_without_a_system_pasteboard() {
        let mut h = Harness::with_copy(false);
        let ids = h.unlock_with(&[("example.com", "fixture", "fixture-password-copy")]);
        assert!(!h.passwords.can_copy());
        h.passwords.copy(&ids[0]);
        assert_eq!(h.passwords.page.notice.as_deref(), Some(COPY_UNAVAILABLE));
        assert!(h.passwords.copied.is_none());
    }

    #[test]
    fn sign_out_account_change_and_idle_time_lock_and_wipe_the_page() {
        let mut h = Harness::new();
        h.shown_tick(Some("fixture-account-a"));
        let ids = h.unlock_with(&[("example.com", "fixture", "fixture-password-lock")]);
        h.passwords.toggle_reveal(&ids[0]);
        h.passwords.start_add();
        h.passwords.page.editor.as_mut().unwrap().password.push_str("fixture-draft");
        h.shown_tick(Some("fixture-account-a"));
        assert!(h.passwords.unlocked(), "the same account keeps it unlocked");
        h.shown_tick(None);
        assert!(!h.passwords.unlocked());
        assert_eq!(h.passwords.page.notice.as_deref(), Some("Locked because you signed out."));
        assert!(h.passwords.page.revealed.is_empty() && h.passwords.page.editor.is_none());

        h.passwords.unlock();
        h.shown_tick(Some("fixture-account-b"));
        assert!(!h.passwords.unlocked(), "signing in locks too");
        assert_eq!(h.passwords.page.notice.as_deref(), Some("Locked because the signed-in account changed."));
        h.passwords.unlock();
        h.shown_tick(Some("fixture-account-c"));
        assert!(!h.passwords.unlocked(), "switching accounts locks");
        assert_eq!(h.passwords.page.notice.as_deref(), Some("Locked because the signed-in account changed."));
        h.shown_tick(Some("fixture-account-b"));

        h.passwords.unlock();
        h.passwords.page.search.push_str("fixture");
        let wait = h.shown_tick(Some("fixture-account-b")).unwrap();
        assert!(wait > Duration::from_secs(290), "the idle lock schedules its own repaint: {wait:?}");
        h.advance(Duration::from_secs(200));
        h.passwords.touch();
        h.advance(Duration::from_secs(200));
        h.shown_tick(Some("fixture-account-b"));
        assert!(h.passwords.unlocked(), "activity postponed the lock");
        h.advance(Duration::from_secs(100));
        h.shown_tick(Some("fixture-account-b"));
        assert!(!h.passwords.unlocked());
        assert_eq!(h.passwords.page.notice.as_deref(), Some("Locked after 5 minutes without activity."));
        assert!(h.passwords.page.search.is_empty(), "the search query is wiped on lock");
    }

    #[test]
    fn leaving_the_page_hides_revealed_passwords_and_discards_drafts_but_stays_unlocked() {
        let mut h = Harness::new();
        let ids = h.unlock_with(&[("example.com", "fixture", "fixture-password-leave")]);
        h.shown_tick(None);
        h.passwords.toggle_reveal(&ids[0]);
        assert_eq!(h.passwords.page.revealed[&ids[0]].as_str(), "fixture-password-leave");
        h.passwords.request_delete(&ids[0]);
        h.passwords.start_edit(&ids[0]);
        assert!(h.passwords.page.confirm_delete.is_none(), "editing replaces a pending delete");
        h.shown_tick(None);
        assert!(h.passwords.page.editor.is_some(), "still on the page");
        // Settings left the page: the first tick that says so hides it.
        h.passwords.tick_at(None, false);
        assert!(h.passwords.page.revealed.is_empty() && h.passwords.page.editor.is_none());
        assert!(h.passwords.unlocked());
        assert_eq!(h.passwords.vault().unwrap().entries().len(), 1, "a discarded edit changes nothing");
    }

    #[test]
    fn failed_saves_keep_the_draft_and_retry() {
        let mut h = Harness::new();
        h.unlock_with(&[]);
        h.passwords.start_add();
        {
            let editor = h.passwords.page.editor.as_mut().unwrap();
            editor.origin.push_str("ftp://example.com");
            editor.username.push_str("fixture");
        }
        h.passwords.generate();
        let generated = h.passwords.page.editor.as_ref().unwrap().password.to_string();
        assert_eq!(generated.len(), 20);
        let capacity = h.passwords.page.editor.as_ref().unwrap().password.capacity();
        h.passwords.generate();
        assert_ne!(h.passwords.page.editor.as_ref().unwrap().password.as_str(), generated);
        assert_eq!(h.passwords.page.editor.as_ref().unwrap().password.capacity(), capacity, "regenerating reuses the reserved buffer");
        h.passwords.save_editor();
        let editor = h.passwords.page.editor.as_mut().unwrap();
        assert!(editor.error.as_deref().unwrap().contains("https://"), "{:?}", editor.error);
        editor.origin.zeroize();
        editor.origin.push_str("example.com");
        h.passwords.save_editor();
        assert!(h.passwords.page.editor.is_none());
        assert_eq!(h.passwords.page.notice.as_deref(), Some("Password added."));
        assert_eq!(h.passwords.vault().unwrap().entries()[0].origin.as_str(), "https://example.com");

        // A failed delete keeps its confirmation for Retry.
        let id = h.passwords.vault().unwrap().entries()[0].id.clone();
        h.passwords.request_delete(&id);
        h.passwords.lock(None);
        h.passwords.page.confirm_delete = Some(id.clone());
        h.passwords.confirm_delete();
        assert_eq!(h.passwords.page.error, Some(VaultError::Locked));
        assert_eq!(h.passwords.page.confirm_delete.as_deref(), Some(id.as_str()));
    }

    #[test]
    fn page_state_never_debug_prints_secrets() {
        let mut h = Harness::new();
        let ids = h.unlock_with(&[("https://debug.example", "debug-user", "fixture-password-debug")]);
        h.passwords.toggle_reveal(&ids[0]);
        h.passwords.start_edit(&ids[0]);
        let printed = format!("{:?} {:?} {:?}", h.passwords.page.editor, h.passwords.page.revealed, h.passwords.vault());
        for secret in ["debug.example", "debug-user", "fixture-password-debug"] {
            assert!(!printed.contains(secret), "{printed}");
        }
    }

    /// The real AppKit calls, on a private uniquely named pasteboard: never
    /// the user's clipboard.
    #[cfg(target_os = "macos")]
    #[test]
    fn appkit_pasteboard_clears_only_its_own_unchanged_copy() {
        let Some(mut board) = mac::GeneralPasteboard::unique() else { panic!("AppKit pasteboard unavailable") };
        let count = board.copy_secret("fixture-password-appkit").unwrap();
        assert_eq!(board.change_count(), Some(count));
        assert_eq!(board.text().as_deref(), Some("fixture-password-appkit"));
        board.clear();
        assert_eq!(board.text(), None);

        let count = board.copy_secret("fixture-password-appkit").unwrap();
        board.copy_plain("fixture unrelated text");
        assert_ne!(board.change_count(), Some(count), "another copy changes the count, so it is never cleared");
        assert_eq!(board.text().as_deref(), Some("fixture unrelated text"));
        board.release();
    }

    /// One item carries both types, the concealed marker declared first and
    /// written in the same call, so no reader sees the text without it.
    #[cfg(target_os = "macos")]
    #[test]
    fn appkit_copy_writes_one_item_with_the_concealed_marker_declared_first() {
        let Some(mut board) = mac::GeneralPasteboard::unique() else { panic!("AppKit pasteboard unavailable") };
        let copied = board.copy_secret("fixture-password-concealed");
        let (items, text) = (board.item_types(), board.text());
        board.release();
        copied.unwrap();
        assert_eq!(text.as_deref(), Some("fixture-password-concealed"));
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].first().map(String::as_str), Some("org.nspasteboard.ConcealedType"), "{items:?}");
        assert!(items[0].iter().any(|kind| kind == "public.utf8-plain-text"), "{items:?}");
    }

    #[test]
    fn secure_input_is_held_once_while_a_password_field_is_focused_and_always_released() {
        let mut h = Harness::new();
        h.unlock_with(&[]);
        h.shown_tick(None);
        h.passwords.set_secure_input(true);
        h.passwords.set_secure_input(true);
        assert_eq!(h.secure.calls(), [true], "one enable, however many frames");
        h.passwords.set_secure_input(false);
        assert_eq!(h.secure.calls(), [true, false], "blur releases it");

        h.passwords.set_secure_input(true);
        h.passwords.lock(None);
        assert!(!h.secure.held(), "locking releases it");
        h.passwords.unlock();
        h.shown_tick(None);
        h.passwords.set_secure_input(true);
        h.passwords.tick_at(None, false);
        h.passwords.tick_at(None, false);
        assert!(!h.secure.held(), "leaving the page releases it");
        h.passwords.set_secure_input(true);
        h.passwords.shutdown();
        assert!(!h.secure.held(), "quitting releases it");
        h.passwords.set_secure_input(true);
        let secure = h.secure.clone();
        drop(h);
        assert!(!secure.held(), "dropping the controller releases it");
        let calls = secure.calls();
        assert_eq!(calls.len(), 10, "{calls:?}");
        assert!(calls.chunks(2).all(|pair| pair == [true, false]), "every enable is matched by one disable: {calls:?}");
    }

    /// AppKit posts these on the main thread when Crossbow resigns active or
    /// hides. Posted here by hand, so no real app changes state.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_app_releases_secure_input_the_moment_it_resigns_active_or_hides() {
        use objc2_app_kit::{NSApplicationDidHideNotification, NSApplicationDidResignActiveNotification};
        use objc2_foundation::NSNotificationCenter;
        let post = |name| unsafe { NSNotificationCenter::defaultCenter().postNotificationName_object(name, None) };
        let mut h = Harness::new();
        h.passwords.observe_app_deactivation();
        h.unlock_with(&[]);
        for name in [unsafe { NSApplicationDidResignActiveNotification }, unsafe { NSApplicationDidHideNotification }] {
            h.passwords.set_secure_input(true);
            assert!(h.secure.held());
            post(name);
            assert!(!h.secure.held(), "released by the notification itself, with no frame or tick");
            // The page's own release on its next pass finds nothing left to release.
            h.passwords.set_secure_input(false);
        }
        assert_eq!(h.secure.calls(), [true, false, true, false], "one disable per enable");
        // Coming back to the focused field holds it again.
        h.passwords.set_secure_input(true);
        assert!(h.secure.held());
        let secure = h.secure.clone();
        drop(h);
        assert!(!secure.held());
        // The observer went with the controller.
        post(unsafe { NSApplicationDidResignActiveNotification });
        assert_eq!(secure.calls().len(), 6);
    }

    /// ⌘M, or another Crossbow window taking the keyboard, posts no app-level
    /// notification. Any window that miniaturizes or resigns key releases it.
    #[cfg(target_os = "macos")]
    #[test]
    fn any_window_minimising_or_losing_the_keyboard_releases_secure_input_at_once() {
        use objc2_app_kit::{NSWindowDidMiniaturizeNotification, NSWindowDidResignKeyNotification};
        use objc2_foundation::NSNotificationCenter;
        let post = |name| unsafe { NSNotificationCenter::defaultCenter().postNotificationName_object(name, None) };
        let mut h = Harness::new();
        h.passwords.observe_app_deactivation();
        h.unlock_with(&[]);
        for name in [unsafe { NSWindowDidMiniaturizeNotification }, unsafe { NSWindowDidResignKeyNotification }] {
            h.passwords.set_secure_input(true);
            post(name);
            assert!(!h.secure.held(), "released by the window notification itself");
            // A logic pass while minimised changes nothing; the page isn't drawn.
            h.passwords.tick_at(None, true);
            assert!(!h.secure.held());
            h.passwords.set_secure_input(false);
        }
        assert_eq!(h.secure.calls(), [true, false, true, false], "one disable per enable");
    }

    #[test]
    fn a_hidden_window_keeps_the_draft_and_revealed_passwords_until_the_page_is_left() {
        let mut h = Harness::new();
        let ids = h.unlock_with(&[("example.com", "fixture", "fixture-password-hidden")]);
        h.shown_tick(None);
        h.passwords.toggle_reveal(&ids[0]);
        h.passwords.start_add();
        h.passwords.page.editor.as_mut().unwrap().origin.push_str("draft.example");
        // ⌘H or minimise with Passwords open: logic passes only, the page isn't drawn.
        for _ in 0..3 { h.passwords.tick_at(None, true); }
        assert_eq!(h.passwords.page.editor.as_ref().map(|editor| editor.origin.as_str()), Some("draft.example"), "the draft is kept while hidden");
        assert!(h.passwords.page.revealed.contains_key(&ids[0]));
        assert!(h.passwords.unlocked());
        // Leaving the page still hides both.
        h.passwords.tick_at(None, false);
        assert!(h.passwords.page.editor.is_none());
        assert!(h.passwords.page.revealed.is_empty());
    }

    #[test]
    fn opened_secrets_schedule_a_tick_so_leaving_the_page_hides_them_promptly() {
        let mut h = Harness::new();
        let ids = h.unlock_with(&[("example.com", "fixture", "fixture-password-hide")]);
        h.shown_tick(None);
        h.passwords.toggle_reveal(&ids[0]);
        let wait = h.shown_tick(None).unwrap();
        assert!(wait <= Duration::from_millis(1050), "an idle app still hides a revealed password soon after Settings closes: {wait:?}");
        h.passwords.toggle_reveal(&ids[0]);
        h.passwords.start_add();
        assert!(h.shown_tick(None).unwrap() <= Duration::from_millis(1050), "and a draft");
        h.passwords.close_editor();
        assert!(h.shown_tick(None).unwrap() > Duration::from_secs(290), "nothing open: no extra repaints");
    }
}
