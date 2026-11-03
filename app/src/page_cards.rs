//! Saved cards: Settings → Passwords → Cards, and the card a page is filled
//! with.
//!
//! Cards are kept in the vault, sealed with its key and tagged with a
//! profile, and are read only while it is unlocked. A page is filled only
//! when the user picks a card, only with one saved in the page's own
//! profile, and never with a security code: the vault has no place for one
//! and the editor no field. The full number is never shown or copied: the
//! list and the menus name a card by its network and last four digits, and
//! the number is opened only for the fill the user chose. Pages never offer
//! to save a card. Nothing here logs, toasts or syncs a card's number, name,
//! expiry or nickname, or the page's origin.

use super::*;
use crossbow_core::vault::{Card, CardHint, CardNetwork, FIRST_EXPIRY_YEAR, MAX_CARD_DIGITS, MAX_CARD_NAME_BYTES, MAX_NICKNAME_BYTES, SavedCard, year_month};

pub const CARD_ADDED: &str = "Card added.";
pub const CARD_SAVED: &str = "Card saved.";
pub const CARD_DELETED: &str = "Card deleted.";
pub const CARD_ALREADY_DELETED: &str = "That card was already deleted.";
/// A card that no longer matches the page's profile, as after a delete elsewhere.
pub const CARD_GONE: &str = "That card is no longer saved in this profile.";
pub const NO_CARD_PROFILE: &str = "Cards are kept per profile. Choose a profile to see its cards.";

/// egui's own state for every card field, so it can be dropped with the
/// draft: the number, the name on the card, the expiry month and year, and
/// the nickname. There is no field for a security code.
pub const CARD_FIELD_IDS: [&str; 5] = ["card-number", "card-name", "card-month", "card-year", "card-nickname"];
pub const NUMBER: usize = 0;
pub const NAME: usize = 1;
pub const MONTH: usize = 2;
pub const YEAR: usize = 3;
pub const NICKNAME: usize = 4;
/// What each field accepts: a number's digits with a space or hyphen
/// between each group of four, a month as 1 or 2 digits, a year as 2 or 4.
pub const CARD_FIELD_LIMITS: [usize; 5] = [MAX_CARD_DIGITS + 4, MAX_CARD_NAME_BYTES, 2, 4, MAX_NICKNAME_BYTES];

pub fn card_field_id(field: usize) -> egui::Id {
    field_id(CARD_FIELD_IDS[field])
}

/// How a card is named in the list and the menus: its network and last four digits.
pub fn card_label(network: CardNetwork, last4: &str) -> String {
    format!("{} ••••{last4}", network.name())
}

pub fn card_title(saved: &SavedCard) -> String {
    card_label(saved.network, &saved.last4)
}

/// The Add or Edit form for a card. Each buffer reserves its whole limit,
/// as the password editor's do, so typing never reallocates. For an edit,
/// the number starts blank, and a blank number keeps the saved one.
pub struct CardEditor {
    /// None for a new card.
    pub id: Option<String>,
    /// The profile it is saved in: the active one for Add, its own for Edit.
    pub profile: String,
    pub number: Zeroizing<String>,
    pub name: Zeroizing<String>,
    pub month: Zeroizing<String>,
    pub year: Zeroizing<String>,
    pub nickname: Zeroizing<String>,
    /// The saved number, by its network and last four, for an edit.
    pub saved: Option<String>,
    pub error: Option<String>,
    /// Move keyboard focus to the number on the next frame.
    pub focus: bool,
}

impl std::fmt::Debug for CardEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CardEditor").field("id", &self.id).finish_non_exhaustive()
    }
}

/// A month of 1 to 12, typed as 1 or 2 digits; 0, which the vault refuses, otherwise.
fn month_of(text: &str) -> u8 {
    let text = text.trim();
    if text.is_empty() || text.len() > 2 || !text.bytes().all(|b| b.is_ascii_digit()) { return 0; }
    text.parse().unwrap_or(0)
}

/// A year typed as 4 digits, or 2 for this century; 0, which the vault refuses, otherwise.
fn year_of(text: &str) -> u16 {
    let text = text.trim();
    if !text.bytes().all(|b| b.is_ascii_digit()) { return 0; }
    match text.len() {
        2 => FIRST_EXPIRY_YEAR + text.parse::<u16>().unwrap_or(0),
        4 => text.parse().unwrap_or(0),
        _ => 0,
    }
}

impl CardEditor {
    fn new(id: Option<String>, profile: String, saved: Option<&SavedCard>) -> CardEditor {
        let field = |index: usize, text: &str| fill(text, CARD_FIELD_LIMITS[index]);
        let (name, month, year, nickname) = match saved {
            Some(saved) => (saved.name.as_str(), format!("{:02}", saved.exp_month), format!("{:04}", saved.exp_year), saved.nickname.as_str()),
            None => ("", String::new(), String::new(), ""),
        };
        CardEditor {
            id, profile, number: field(NUMBER, ""), name: field(NAME, name), month: field(MONTH, &month), year: field(YEAR, &year), nickname: field(NICKNAME, nickname),
            saved: saved.map(card_title), error: None, focus: true,
        }
    }

    /// The card as typed, for the vault.
    pub fn card(&self) -> Card {
        let copy = |text: &str| Zeroizing::new(text.to_owned());
        Card { name: copy(&self.name), number: copy(&self.number), exp_month: month_of(&self.month), exp_year: year_of(&self.year), nickname: copy(&self.nickname) }
    }

    /// Hints under the number or the year, for `today`, a year and month.
    /// None of them stops a save: an unusual card is saved as typed.
    pub fn hints(&self, today: (u16, u8)) -> Vec<(usize, CardHint)> {
        self.card().hints(today).into_iter().map(|hint| (if hint == CardHint::Expired { YEAR } else { NUMBER }, hint)).collect()
    }
}

impl Passwords {
    // -- the page ---------------------------------------------------------------------------

    /// Every card the page lists for the active profile: its own, then
    /// damaged rows, whose profile can't be read, so they can be deleted.
    pub fn listed_cards(&self) -> Vec<&SavedCard> {
        let Some(vault) = &self.vault else { return Vec::new() };
        let mut listed: Vec<_> = vault.cards().iter().filter(|saved| !saved.readable() || saved.profile == self.profile).collect();
        listed.sort_by_key(|saved| !saved.readable());
        listed
    }

    pub fn start_add_card(&mut self) {
        self.close_card_editor();
        self.page.confirm_delete_card = None;
        self.say(None, None, None);
        if self.profile.is_empty() { return self.say(None, Some(NO_CARD_PROFILE), None); }
        self.page.card_editor = Some(CardEditor::new(None, self.profile.clone(), None));
    }

    pub fn start_edit_card(&mut self, id: &str) {
        self.close_card_editor();
        self.page.confirm_delete_card = None;
        self.say(None, None, None);
        let Some(saved) = self.vault.as_ref().and_then(|vault| vault.saved_card(id)).filter(|saved| saved.readable()) else { return };
        self.page.card_editor = Some(CardEditor::new(Some(id.to_owned()), saved.profile.clone(), Some(saved)));
    }

    pub fn close_card_editor(&mut self) {
        if self.page.card_editor.take().is_some() { self.forget_fields(&CARD_FIELD_IDS); }
    }

    /// Save the form. Only what the vault can't hold is refused, and the
    /// draft stays for a correction or another try.
    pub fn save_card_editor(&mut self) {
        let (Some(vault), Some(editor)) = (&mut self.vault, &mut self.page.card_editor) else { return };
        let last_input = vault.last_activity();
        let card = editor.card();
        let result = match &editor.id {
            Some(id) => vault.update_card(id, &card).map(|()| id.clone()),
            None => vault.add_card(&editor.profile, &card),
        };
        match result {
            Ok(id) => {
                let edited = editor.id.is_some();
                self.close_card_editor();
                self.say(Some(&id), Some(if edited { CARD_SAVED } else { CARD_ADDED }), None);
            }
            Err(VaultError::ResetElsewhere) => self.reset_elsewhere(last_input),
            Err(error) => editor.error = Some(error.to_string()),
        }
    }

    pub fn request_delete_card(&mut self, id: &str) {
        self.close_card_editor();
        self.say(None, None, None);
        self.page.confirm_delete_card = Some(id.to_owned());
    }

    pub fn confirm_delete_card(&mut self) {
        let (Some(vault), Some(id)) = (&mut self.vault, self.page.confirm_delete_card.clone()) else { return };
        let last_input = vault.last_activity();
        match vault.delete_card(&id) {
            Ok(()) => {
                self.page.confirm_delete_card = None;
                self.say(None, Some(CARD_DELETED), None);
            }
            Err(VaultError::NotFound) => {
                self.page.confirm_delete_card = None;
                self.say(None, Some(CARD_ALREADY_DELETED), None);
            }
            Err(VaultError::ResetElsewhere) => self.reset_elsewhere(last_input),
            // The confirmation stays so Delete can be retried.
            Err(error) => self.say(Some(&id), None, Some(error)),
        }
    }

    // -- pages ------------------------------------------------------------------------------

    /// The cards a page in `profile` may be filled with: each id, the label
    /// its menu shows and its expiry, by label. Empty while locked.
    pub fn cards_for(&self, profile: &str) -> Vec<(String, String, String)> {
        let Some(vault) = &self.vault else { return Vec::new() };
        let mut listed: Vec<_> = vault.cards_in(profile).into_iter().map(|saved| (saved.id.clone(), card_title(saved), saved.expiry())).collect();
        listed.sort_by(|(_, a, _), (_, b, _)| Zeroizing::new(a.to_lowercase()).as_str().cmp(Zeroizing::new(b.to_lowercase()).as_str()));
        listed
    }

    /// What a page in `profile` is filled with, if card `id` is still saved
    /// in that profile: the name on the card, the number's digits, opened
    /// now for this fill only, the month as two digits and the year as four.
    /// Records the use; failing to record it never blocks the fill, but a
    /// reset found by it fills nothing.
    pub fn card_to_fill(&mut self, id: &str, profile: &str) -> Result<[Zeroizing<String>; 4], &'static str> {
        let now = self.now();
        let vault = self.vault.as_mut().ok_or(CARD_GONE)?;
        let last_input = vault.last_activity();
        let saved = vault.cards_in(profile).into_iter().find(|saved| saved.id == id).ok_or(CARD_GONE)?;
        let (name, month, year) = (Zeroizing::new(saved.name.to_string()), Zeroizing::new(format!("{:02}", saved.exp_month)), Zeroizing::new(format!("{:04}", saved.exp_year)));
        let number = match vault.card_number(id) {
            Ok(number) => number,
            Err(VaultError::ResetElsewhere) => {
                self.reset_elsewhere(last_input);
                return Err(CARD_GONE);
            }
            Err(_) => return Err(CARD_GONE),
        };
        if vault.mark_card_used(id, crossbow_core::now_ms()) == Err(VaultError::ResetElsewhere) {
            self.reset_elsewhere(last_input);
            return Err(CARD_GONE);
        }
        if let Some(vault) = &mut self.vault { vault.touch(now); }
        Ok([name, number, month, year])
    }

    /// What the key button can do for a page in `profile` whose card field
    /// had focus: fill one of its cards, or unlock when the vault, locked,
    /// holds any card. A locked vault can't tell profiles apart.
    pub fn card_key_state(&mut self, profile: &str) -> Option<KeyState> {
        self.ensure_open();
        let vault = self.vault.as_ref()?;
        if vault.is_unlocked() {
            let count = vault.cards_in(profile).len();
            return (count > 0).then_some(KeyState::Cards(count));
        }
        vault.card_count().is_ok_and(|count| count > 0).then_some(KeyState::Locked)
    }
}

/// This month, for the editor's hints.
pub fn this_month() -> (u16, u8) {
    year_month(crossbow_core::now_ms())
}

#[cfg(test)]
#[path = "page_cards_tests.rs"]
mod tests;
