//! Saved addresses: the parts of one postal address, as typed, sealed
//! together in one envelope and opened only while the vault is unlocked.

use super::{EntryState, VaultError};
use std::fmt;
use zeroize::Zeroizing;

/// One part of an address, in the order the parts are sealed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AddressField {
    /// The user's own name for the address, such as "Home".
    Label,
    Name,
    Organization,
    /// Every street line, one per line.
    Street,
    City,
    /// A state, province, county or region.
    Region,
    PostalCode,
    Country,
    Phone,
    Email,
}

impl AddressField {
    pub const ALL: [AddressField; 10] = [
        AddressField::Label, AddressField::Name, AddressField::Organization, AddressField::Street, AddressField::City,
        AddressField::Region, AddressField::PostalCode, AddressField::Country, AddressField::Phone, AddressField::Email,
    ];

    /// The most bytes this part holds.
    pub const fn limit(self) -> usize {
        match self {
            AddressField::Label => 128,
            AddressField::Street => 1024,
            _ => 256,
        }
    }

    /// The part's name on screen.
    pub const fn name(self) -> &'static str {
        match self {
            AddressField::Label => "Label",
            AddressField::Name => "Name",
            AddressField::Organization => "Organization",
            AddressField::Street => "Street address",
            AddressField::City => "City",
            AddressField::Region => "State or region",
            AddressField::PostalCode => "Postal code",
            AddressField::Country => "Country",
            AddressField::Phone => "Phone",
            AddressField::Email => "Email",
        }
    }

    const fn too_long(self) -> &'static str {
        match self {
            AddressField::Label => "That label is too long.",
            AddressField::Name => "That name is too long.",
            AddressField::Organization => "That organization is too long.",
            AddressField::Street => "That street address is too long.",
            AddressField::City => "That city is too long.",
            AddressField::Region => "That state or region is too long.",
            AddressField::PostalCode => "That postal code is too long.",
            AddressField::Country => "That country is too long.",
            AddressField::Phone => "That phone number is too long.",
            AddressField::Email => "That email address is too long.",
        }
    }
}

/// An address's parts as typed. Each wipes itself on drop; Debug never shows one.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Address([Zeroizing<String>; 10]);

impl Address {
    pub fn get(&self, field: AddressField) -> &str {
        &self.0[field as usize]
    }

    pub fn set(&mut self, field: AddressField, value: &str) {
        let part = &mut self.0[field as usize];
        part.clear();
        part.push_str(value);
    }

    /// The part's own buffer, for an editor that reserves its capacity.
    pub fn part_mut(&mut self, field: AddressField) -> &mut Zeroizing<String> {
        &mut self.0[field as usize]
    }

    /// The street lines, without blank ones.
    pub fn street_lines(&self) -> impl Iterator<Item = &str> {
        self.get(AddressField::Street).lines().map(str::trim).filter(|line| !line.is_empty())
    }

    /// Whether every part but the label is empty.
    pub fn is_blank(&self) -> bool {
        AddressField::ALL[1..].iter().all(|field| self.get(*field).trim().is_empty())
    }

    /// Only what the vault can hold: some part besides the label, and none
    /// over its limit. Nothing else about an address is checked, so an
    /// unusual one always saves.
    pub fn check(&self) -> Result<(), VaultError> {
        if let Some(field) = AddressField::ALL.iter().find(|field| self.get(**field).len() > field.limit()) {
            return Err(VaultError::Invalid(field.too_long()));
        }
        if self.is_blank() { return Err(VaultError::Invalid("Enter at least one part of the address.")); }
        Ok(())
    }

    /// Every part, each as its length, a colon and its text, in one buffer
    /// sized up front so it never reallocates and leaves an unwiped copy.
    pub(super) fn encode(&self) -> Zeroizing<String> {
        let size = self.0.iter().map(|part| part.len().to_string().len() + 1 + part.len()).sum();
        let mut out = Zeroizing::new(String::with_capacity(size));
        for part in &self.0 {
            out.push_str(&part.len().to_string());
            out.push(':');
            out.push_str(part);
        }
        out
    }

    /// The parts `encode` wrote, or None for anything else.
    pub(super) fn decode(text: &str) -> Option<Address> {
        let mut address = Address::default();
        let mut rest = text;
        for part in &mut address.0 {
            let (len, after) = rest.split_once(':')?;
            if len.is_empty() || len.len() > 5 || !len.bytes().all(|b| b.is_ascii_digit()) { return None; }
            let len: usize = len.parse().ok()?;
            part.push_str(after.get(..len)?);
            rest = &after[len..];
        }
        rest.is_empty().then_some(address)
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Address(..)")
    }
}

/// A saved address as listed while unlocked, every part opened. A damaged
/// row keeps no profile and empty parts, and can only be deleted.
pub struct SavedAddress {
    pub id: String,
    /// The Crossbow profile whose pages it may be filled into.
    pub profile: String,
    pub address: Address,
    pub created_ms: u64,
    pub updated_ms: u64,
    /// When it was last filled into a page.
    pub used_ms: Option<u64>,
    pub state: EntryState,
}

impl SavedAddress {
    pub fn readable(&self) -> bool {
        self.state == EntryState::Readable
    }
}

impl fmt::Debug for SavedAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SavedAddress").field("id", &self.id).field("state", &self.state).finish_non_exhaustive()
    }
}
