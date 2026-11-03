//! Saved payment cards: the cardholder's name, the number, the expiry and a
//! nickname, sealed in the vault and opened only while it is unlocked. A
//! card's security code is never stored, asked for or filled.
//!
//! Each card is sealed in two envelopes: its details (the name, nickname,
//! expiry, network and last four digits), which an unlock opens for the
//! list, and its number, which is opened only to check it at unlock and
//! for a fill, and wiped straight after.

use super::{EntryState, VaultError};
use std::fmt;
use zeroize::{Zeroize, Zeroizing};

pub const MAX_CARD_NAME_BYTES: usize = 256;
pub const MAX_NICKNAME_BYTES: usize = 128;
/// A card number has 8 to 19 digits (ISO/IEC 7812).
pub const MIN_CARD_DIGITS: usize = 8;
pub const MAX_CARD_DIGITS: usize = 19;
/// The expiry years the vault holds.
pub const FIRST_EXPIRY_YEAR: u16 = 2000;
pub const LAST_EXPIRY_YEAR: u16 = 2099;

/// The network a card belongs to, by its number's leading digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CardNetwork {
    Visa,
    Mastercard,
    Amex,
    Discover,
    DinersClub,
    Jcb,
    UnionPay,
    Unknown,
}

impl CardNetwork {
    const ALL: [CardNetwork; 8] = [
        CardNetwork::Visa, CardNetwork::Mastercard, CardNetwork::Amex, CardNetwork::Discover,
        CardNetwork::DinersClub, CardNetwork::Jcb, CardNetwork::UnionPay, CardNetwork::Unknown,
    ];

    /// The network the leading digits of `digits` name, by the ranges the
    /// networks publish. Anything else, a short number included, is Unknown.
    pub fn of(digits: &str) -> CardNetwork {
        let within = |len: usize, low: u32, high: u32| {
            digits.get(..len).filter(|lead| lead.bytes().all(|b| b.is_ascii_digit())).and_then(|lead| lead.parse::<u32>().ok()).is_some_and(|lead| (low..=high).contains(&lead))
        };
        if within(1, 4, 4) {
            CardNetwork::Visa
        } else if within(2, 51, 55) || within(4, 2221, 2720) {
            CardNetwork::Mastercard
        } else if within(2, 34, 34) || within(2, 37, 37) {
            CardNetwork::Amex
        } else if within(4, 6011, 6011) || within(3, 644, 649) || within(2, 65, 65) {
            CardNetwork::Discover
        } else if within(3, 300, 305) || within(2, 36, 36) || within(2, 38, 39) {
            CardNetwork::DinersClub
        } else if within(4, 3528, 3589) {
            CardNetwork::Jcb
        } else if within(2, 62, 62) {
            CardNetwork::UnionPay
        } else {
            CardNetwork::Unknown
        }
    }

    /// The network's name on screen. An unknown one is just a card.
    pub const fn name(self) -> &'static str {
        match self {
            CardNetwork::Visa => "Visa",
            CardNetwork::Mastercard => "Mastercard",
            CardNetwork::Amex => "American Express",
            CardNetwork::Discover => "Discover",
            CardNetwork::DinersClub => "Diners Club",
            CardNetwork::Jcb => "JCB",
            CardNetwork::UnionPay => "UnionPay",
            CardNetwork::Unknown => "Card",
        }
    }

    /// How a card's details envelope names it.
    const fn code(self) -> &'static str {
        match self {
            CardNetwork::Visa => "visa",
            CardNetwork::Mastercard => "mastercard",
            CardNetwork::Amex => "amex",
            CardNetwork::Discover => "discover",
            CardNetwork::DinersClub => "diners",
            CardNetwork::Jcb => "jcb",
            CardNetwork::UnionPay => "unionpay",
            CardNetwork::Unknown => "unknown",
        }
    }
}

/// Whether `digits`, digits only, pass the Luhn check every network's numbers pass.
pub fn luhn_valid(digits: &str) -> bool {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) { return false; }
    let sum: u32 = digits.bytes().rev().enumerate().map(|(i, b)| {
        let digit = u32::from(b - b'0');
        if i % 2 == 1 { if digit * 2 > 9 { digit * 2 - 9 } else { digit * 2 } } else { digit }
    }).sum();
    sum % 10 == 0
}

/// The UTC year and month of a time in milliseconds since 1970.
pub fn year_month(ms: u64) -> (u16, u8) {
    // Howard Hinnant's days-to-civil algorithm, for days since 1970-01-01.
    let days = (ms / 86_400_000) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let month = if shifted < 10 { shifted + 3 } else { shifted - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year.clamp(0, i64::from(u16::MAX)) as u16, month as u8)
}

/// Whether a card that expires at the end of `exp_month` of `exp_year` has
/// expired by `today`, a year and month.
pub fn expired(exp_month: u8, exp_year: u16, today: (u16, u8)) -> bool {
    (exp_year, exp_month) < today
}

/// Something about a card worth a second look. None of them stops a save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardHint {
    /// The number fails the Luhn check: likely a typo.
    FailsCheck,
    Expired,
    UnknownNetwork,
}

impl CardHint {
    pub const fn message(self) -> &'static str {
        match self {
            CardHint::FailsCheck => "This number doesn't look like a card number. Check it for a typo; it will be saved as typed.",
            CardHint::Expired => "This card has expired. It will be saved as typed.",
            CardHint::UnknownNetwork => "Crossbow doesn't recognize this card's network. It will be saved as typed.",
        }
    }
}

/// A card as typed. For an edit, a blank number keeps the saved one. Every
/// text wipes itself on drop, and Debug shows only the network and the last
/// four digits.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Card {
    /// The cardholder's name, as on the card. May be blank.
    pub name: Zeroizing<String>,
    /// Digits, with any spaces or hyphens between them.
    pub number: Zeroizing<String>,
    pub exp_month: u8,
    pub exp_year: u16,
    /// The user's own name for the card. May be blank.
    pub nickname: Zeroizing<String>,
}

impl Card {
    /// The number's digits, without the spaces and hyphens typed between them.
    pub fn digits(&self) -> Zeroizing<String> {
        let mut digits = Zeroizing::new(String::with_capacity(self.number.len()));
        digits.extend(self.number.chars().filter(|c| *c != ' ' && *c != '-'));
        digits
    }

    pub fn network(&self) -> CardNetwork {
        CardNetwork::of(&self.digits())
    }

    /// Only what the vault can hold: a name and nickname within their
    /// limits, a month and a year it keeps, and, unless `keep_number` and
    /// it is blank, a number of 8 to 19 digits with nothing but spaces and
    /// hyphens between them. Nothing else about a card is checked.
    pub(super) fn check(&self, keep_number: bool) -> Result<(), VaultError> {
        if self.name.len() > MAX_CARD_NAME_BYTES { return Err(VaultError::Invalid("That name is too long.")); }
        if self.nickname.len() > MAX_NICKNAME_BYTES { return Err(VaultError::Invalid("That nickname is too long.")); }
        let digits = self.digits();
        if !(keep_number && digits.is_empty()) {
            if digits.is_empty() { return Err(VaultError::Invalid("Enter the card number.")); }
            if !digits.bytes().all(|b| b.is_ascii_digit()) { return Err(VaultError::Invalid("A card number has only digits.")); }
            if !(MIN_CARD_DIGITS..=MAX_CARD_DIGITS).contains(&digits.len()) { return Err(VaultError::Invalid("A card number has 8 to 19 digits.")); }
        }
        if !(1..=12).contains(&self.exp_month) { return Err(VaultError::Invalid("Enter the month the card expires, 1 to 12.")); }
        if !(FIRST_EXPIRY_YEAR..=LAST_EXPIRY_YEAR).contains(&self.exp_year) { return Err(VaultError::Invalid("Enter the year the card expires, such as 2031.")); }
        Ok(())
    }

    /// The hints for this card on `today`, a year and month: its number
    /// fails the check, it has expired, or its network is unknown. A blank
    /// number gets none of a number's hints.
    pub fn hints(&self, today: (u16, u8)) -> Vec<CardHint> {
        let digits = self.digits();
        let mut hints = Vec::new();
        if !digits.is_empty() && !luhn_valid(&digits) { hints.push(CardHint::FailsCheck); }
        if expired(self.exp_month, self.exp_year, today) { hints.push(CardHint::Expired); }
        if !digits.is_empty() && CardNetwork::of(&digits) == CardNetwork::Unknown { hints.push(CardHint::UnknownNetwork); }
        hints
    }
}

/// The last four digits of `digits`, or nothing for a shorter or non-digit number.
fn last_four(digits: &str) -> Zeroizing<String> {
    let four = digits.len().checked_sub(4).and_then(|start| digits.get(start..)).filter(|four| four.bytes().all(|b| b.is_ascii_digit()));
    Zeroizing::new(four.unwrap_or_default().to_owned())
}

impl fmt::Debug for Card {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digits = self.digits();
        f.debug_struct("Card").field("network", &CardNetwork::of(&digits)).field("last4", &last_four(&digits).as_str()).finish_non_exhaustive()
    }
}

/// A saved card as listed while unlocked: its details opened, its number
/// still sealed. A damaged row keeps no profile and empty details, and can
/// only be deleted. Every detail wipes itself on drop.
pub struct SavedCard {
    pub id: String,
    /// The Crossbow profile whose pages it may be filled into.
    pub profile: String,
    pub name: Zeroizing<String>,
    pub nickname: Zeroizing<String>,
    pub exp_month: u8,
    pub exp_year: u16,
    pub network: CardNetwork,
    pub last4: Zeroizing<String>,
    pub created_ms: u64,
    pub updated_ms: u64,
    /// When it was last filled into a page.
    pub used_ms: Option<u64>,
    pub state: EntryState,
}

impl SavedCard {
    pub fn readable(&self) -> bool {
        self.state == EntryState::Readable
    }

    /// The expiry as a card prints it, MM/YY.
    pub fn expiry(&self) -> String {
        format!("{:02}/{:02}", self.exp_month, self.exp_year % 100)
    }

    /// A damaged row, listed under `id`.
    pub(super) fn damaged(id: String) -> SavedCard {
        SavedCard {
            id, profile: String::new(), name: Zeroizing::default(), nickname: Zeroizing::default(), exp_month: 0, exp_year: 0,
            network: CardNetwork::Unknown, last4: Zeroizing::default(), created_ms: 0, updated_ms: 0, used_ms: None, state: EntryState::Damaged,
        }
    }
}

impl Drop for SavedCard {
    fn drop(&mut self) {
        self.exp_month.zeroize();
        self.exp_year.zeroize();
    }
}

impl fmt::Debug for SavedCard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SavedCard").field("network", &self.network).field("last4", &self.last4.as_str()).finish_non_exhaustive()
    }
}

/// A card's details as its envelope holds them: the name, nickname,
/// expiry month and year, network and last four digits.
pub(super) struct Details {
    pub name: Zeroizing<String>,
    pub nickname: Zeroizing<String>,
    pub exp_month: u8,
    pub exp_year: u16,
    pub network: CardNetwork,
    pub last4: Zeroizing<String>,
}

impl Details {
    /// The details of `card`, whose number has `digits`.
    pub fn of(card: &Card, digits: &str) -> Details {
        Details {
            name: card.name.clone(), nickname: card.nickname.clone(), exp_month: card.exp_month, exp_year: card.exp_year,
            network: CardNetwork::of(digits), last4: last_four(digits),
        }
    }

    /// Every detail, each as its length, a colon and its text, in one
    /// buffer sized up front so it never reallocates and leaves an unwiped copy.
    pub fn encode(&self) -> Zeroizing<String> {
        let (month, year) = (Zeroizing::new(self.exp_month.to_string()), Zeroizing::new(self.exp_year.to_string()));
        let parts: [&str; 6] = [&self.name, &self.nickname, &month, &year, self.network.code(), &self.last4];
        let size = parts.iter().map(|part| part.len().to_string().len() + 1 + part.len()).sum();
        let mut out = Zeroizing::new(String::with_capacity(size));
        for part in parts {
            out.push_str(&part.len().to_string());
            out.push(':');
            out.push_str(part);
        }
        out
    }

    /// The details `encode` wrote, or None for anything else, including a
    /// month, year, network or last four the vault never writes.
    pub fn decode(text: &str) -> Option<Details> {
        let mut parts: [Zeroizing<String>; 6] = Default::default();
        let mut rest = text;
        for part in &mut parts {
            let (len, after) = rest.split_once(':')?;
            if len.is_empty() || len.len() > 5 || !len.bytes().all(|b| b.is_ascii_digit()) { return None; }
            let len: usize = len.parse().ok()?;
            part.push_str(after.get(..len)?);
            rest = &after[len..];
        }
        if !rest.is_empty() { return None; }
        let [name, nickname, month, year, network, last4] = parts;
        let number = |text: &str, max: usize| (!text.is_empty() && text.len() <= max && text.bytes().all(|b| b.is_ascii_digit())).then(|| text.parse::<u16>().ok()).flatten();
        let exp_month = u8::try_from(number(&month, 2)?).ok().filter(|month| (1..=12).contains(month))?;
        let exp_year = number(&year, 4).filter(|year| (FIRST_EXPIRY_YEAR..=LAST_EXPIRY_YEAR).contains(year))?;
        let network = CardNetwork::ALL.into_iter().find(|known| known.code() == network.as_str())?;
        if last4.len() != 4 || !last4.bytes().all(|b| b.is_ascii_digit()) || name.len() > MAX_CARD_NAME_BYTES || nickname.len() > MAX_NICKNAME_BYTES { return None; }
        Some(Details { name, nickname, exp_month, exp_year, network, last4 })
    }
}
