//! Saved addresses: Settings → Passwords → Addresses, the address a page is
//! filled with, and the offer a page's submitted address form waits in.
//!
//! Addresses are kept in the vault, sealed with its key and tagged with a
//! profile, and are read only while it is unlocked. A page is filled only
//! when the user picks an address, and only with one saved in the page's own
//! profile. An offer to save one follows "Offer to save passwords from
//! websites" and the never-save list, as a login's does. Nothing here logs,
//! toasts or syncs a part of an address, its label or the page's origin.

use super::*;
use super::page_logins::NEVER_SAVED;
use crossbow_core::vault::{Address, AddressField, normalize_origin};

pub const ADDRESS_ADDED: &str = "Address added.";
pub const ADDRESS_SAVED: &str = "Address saved.";
pub const ADDRESS_DELETED: &str = "Address deleted.";
pub const ADDRESS_ALREADY_DELETED: &str = "That address was already deleted.";
pub const ADDRESS_ALREADY_SAVED: &str = "This address is already saved.";
/// An address that no longer matches the page's profile, as after a delete elsewhere.
pub const ADDRESS_GONE: &str = "That address is no longer saved in this profile.";
pub const NO_PROFILE: &str = "Addresses are kept per profile. Choose a profile to see its addresses.";

/// egui's own state for every address field, so it can be dropped with the
/// draft, in `AddressField` order.
pub const ADDRESS_FIELD_IDS: [&str; 10] = [
    "address-label", "address-name", "address-organization", "address-street", "address-city",
    "address-region", "address-postal-code", "address-country", "address-phone", "address-email",
];

pub fn address_field_id(field: AddressField) -> egui::Id {
    field_id(ADDRESS_FIELD_IDS[field as usize])
}

/// The parts a page holds, in the engine's order: every part but the label.
pub const PAGE_PARTS: [AddressField; 9] = [
    AddressField::Name, AddressField::Organization, AddressField::Street, AddressField::City, AddressField::Region,
    AddressField::PostalCode, AddressField::Country, AddressField::Phone, AddressField::Email,
];

/// Which list the unlocked Passwords page shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    Passwords,
    Addresses,
    Cards,
}

/// The Add or Edit form for an address. Each part's buffer reserves its
/// whole limit, as the password editor's do, so typing never reallocates.
pub struct AddressEditor {
    /// None for a new address.
    pub id: Option<String>,
    /// The profile it is saved in: the active one for Add, its own for Edit.
    pub profile: String,
    pub address: Address,
    pub error: Option<String>,
    /// Move keyboard focus to the first field on the next frame.
    pub focus: bool,
}

impl std::fmt::Debug for AddressEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddressEditor").field("id", &self.id).finish_non_exhaustive()
    }
}

/// A copy of `address` whose parts reserve their whole limit.
fn reserved(address: &Address) -> Address {
    let mut out = Address::default();
    for field in AddressField::ALL {
        *out.part_mut(field) = fill(address.get(field), field.limit());
    }
    out
}

/// What an address is called in a list or menu: its label, else its name,
/// else its first street line, else its city.
pub fn address_title(address: &Address) -> String {
    let first = |field| Some(address.get(field).trim()).filter(|text| !text.is_empty()).map(str::to_owned);
    first(AddressField::Label).or_else(|| first(AddressField::Name)).or_else(|| address.street_lines().next().map(str::to_owned))
        .or_else(|| first(AddressField::City)).unwrap_or_else(|| "Address".to_owned())
}

/// One line under the title: the street, city, region, postal code and country.
pub fn address_summary(address: &Address) -> String {
    let mut parts: Vec<&str> = address.street_lines().collect();
    parts.extend([AddressField::City, AddressField::Region, AddressField::PostalCode, AddressField::Country].map(|field| address.get(field).trim()).into_iter().filter(|part| !part.is_empty()));
    parts.join(", ")
}

/// Hints under a part that looks mistyped. None of them stops a save: an
/// unusual address is saved as typed.
pub fn address_hints(address: &Address) -> Vec<(AddressField, &'static str)> {
    let mut hints = Vec::new();
    let email = address.get(AddressField::Email).trim();
    if !email.is_empty() && !email.contains('@') { hints.push((AddressField::Email, "This email address has no @. It will be saved as typed.")); }
    let phone = address.get(AddressField::Phone).trim();
    if !phone.is_empty() && !phone.bytes().any(|b| b.is_ascii_digit()) { hints.push((AddressField::Phone, "This phone number has no digits. It will be saved as typed.")); }
    hints
}

/// Whether two addresses hold the same parts, the label aside.
fn same_parts(a: &Address, b: &Address) -> bool {
    PAGE_PARTS.iter().all(|field| a.get(*field).trim() == b.get(*field).trim())
}

/// An address a page submitted, waiting for Save, Never for This Site or Not Now.
pub struct AddressOffer {
    pub tab: String,
    /// The page's origin, as the engine checked it.
    pub origin: String,
    pub profile: String,
    /// Its label is editable before saving.
    pub address: Address,
    /// Save was chosen while the vault was locked: the card asks to unlock.
    pub unlocking: bool,
    pub error: Option<VaultError>,
    pub(super) at: Moment,
    /// Distinct for each offer, shared with logins' offers.
    pub revision: u64,
}

impl std::fmt::Debug for AddressOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddressOffer").field("tab", &self.tab).field("unlocking", &self.unlocking).finish_non_exhaustive()
    }
}

impl Passwords {
    // -- the page ---------------------------------------------------------------------------

    /// Show the Passwords, Addresses or Cards list. Switching closes every editor.
    pub fn show_section(&mut self, section: Section) {
        if self.page.section == section { return; }
        self.close_editor();
        self.close_address_editor();
        self.close_card_editor();
        self.page.confirm_delete = None;
        self.page.confirm_delete_address = None;
        self.page.confirm_delete_card = None;
        self.say(None, None, None);
        self.page.section = section;
    }

    /// Every address the page lists for the active profile, by title: its
    /// own, then damaged rows, whose profile can't be read, so they can be deleted.
    pub fn listed_addresses(&self) -> Vec<&crossbow_core::vault::SavedAddress> {
        let Some(vault) = &self.vault else { return Vec::new() };
        let mut listed: Vec<_> = vault.addresses().iter().filter(|saved| !saved.readable() || saved.profile == self.profile).collect();
        let key = |saved: &crossbow_core::vault::SavedAddress| Zeroizing::new(address_title(&saved.address).to_lowercase());
        listed.sort_by(|a, b| (!a.readable()).cmp(&!b.readable()).then_with(|| key(a).as_str().cmp(key(b).as_str())));
        listed
    }

    pub fn start_add_address(&mut self) {
        self.close_address_editor();
        self.page.confirm_delete_address = None;
        self.say(None, None, None);
        if self.profile.is_empty() { return self.say(None, Some(NO_PROFILE), None); }
        self.page.address_editor = Some(AddressEditor { id: None, profile: self.profile.clone(), address: reserved(&Address::default()), error: None, focus: true });
    }

    pub fn start_edit_address(&mut self, id: &str) {
        self.close_address_editor();
        self.page.confirm_delete_address = None;
        self.say(None, None, None);
        let Some(saved) = self.vault.as_ref().and_then(|vault| vault.saved_address(id)).filter(|saved| saved.readable()) else { return };
        let editor = AddressEditor { id: Some(id.to_owned()), profile: saved.profile.clone(), address: reserved(&saved.address), error: None, focus: true };
        self.page.address_editor = Some(editor);
    }

    pub fn close_address_editor(&mut self) {
        if self.page.address_editor.take().is_some() { self.forget_fields(&ADDRESS_FIELD_IDS); }
    }

    /// Save the form. Only what the vault can't hold is refused, and the
    /// draft stays for a correction or another try.
    pub fn save_address_editor(&mut self) {
        let (Some(vault), Some(editor)) = (&mut self.vault, &mut self.page.address_editor) else { return };
        let last_input = vault.last_activity();
        let result = match &editor.id {
            Some(id) => vault.update_address(id, &editor.address).map(|()| id.clone()),
            None => vault.add_address(&editor.profile, &editor.address),
        };
        match result {
            Ok(id) => {
                let edited = editor.id.is_some();
                self.close_address_editor();
                self.say(Some(&id), Some(if edited { ADDRESS_SAVED } else { ADDRESS_ADDED }), None);
            }
            Err(VaultError::ResetElsewhere) => self.reset_elsewhere(last_input),
            Err(error) => editor.error = Some(error.to_string()),
        }
    }

    pub fn request_delete_address(&mut self, id: &str) {
        self.close_address_editor();
        self.say(None, None, None);
        self.page.confirm_delete_address = Some(id.to_owned());
    }

    pub fn confirm_delete_address(&mut self) {
        let (Some(vault), Some(id)) = (&mut self.vault, self.page.confirm_delete_address.clone()) else { return };
        let last_input = vault.last_activity();
        match vault.delete_address(&id) {
            Ok(()) => {
                self.page.confirm_delete_address = None;
                self.say(None, Some(ADDRESS_DELETED), None);
            }
            Err(VaultError::NotFound) => {
                self.page.confirm_delete_address = None;
                self.say(None, Some(ADDRESS_ALREADY_DELETED), None);
            }
            Err(VaultError::ResetElsewhere) => self.reset_elsewhere(last_input),
            // The confirmation stays so Delete can be retried.
            Err(error) => self.say(Some(&id), None, Some(error)),
        }
    }

    // -- pages ------------------------------------------------------------------------------

    /// The addresses a page in `profile` may be filled with, by id and the
    /// title its menu shows. Empty while locked.
    pub fn addresses_for(&self, profile: &str) -> Vec<(String, String)> {
        let Some(vault) = &self.vault else { return Vec::new() };
        let mut listed: Vec<_> = vault.addresses_in(profile).into_iter().map(|saved| (saved.id.clone(), address_title(&saved.address))).collect();
        listed.sort_by(|(_, a), (_, b)| Zeroizing::new(a.to_lowercase()).as_str().cmp(Zeroizing::new(b.to_lowercase()).as_str()));
        listed
    }

    /// The parts to fill into a page in `profile`, in the engine's order, if
    /// address `id` is still saved in that profile. Records the use; failing
    /// to record it never blocks the fill, but a reset found by it fills nothing.
    pub fn address_to_fill(&mut self, id: &str, profile: &str) -> Result<[Zeroizing<String>; 9], &'static str> {
        let now = self.now();
        let vault = self.vault.as_mut().ok_or(ADDRESS_GONE)?;
        let last_input = vault.last_activity();
        let saved = vault.addresses_in(profile).into_iter().find(|saved| saved.id == id).ok_or(ADDRESS_GONE)?;
        let parts = PAGE_PARTS.map(|field| Zeroizing::new(saved.address.get(field).to_owned()));
        if vault.mark_address_used(id, crossbow_core::now_ms()) == Err(VaultError::ResetElsewhere) {
            self.reset_elsewhere(last_input);
            return Err(ADDRESS_GONE);
        }
        if let Some(vault) = &mut self.vault { vault.touch(now); }
        Ok(parts)
    }

    // -- offers -----------------------------------------------------------------------------

    /// A page in `tab` submitted an address, its parts in the engine's order.
    /// Offer it if the profile offers to save, unless its site is on the
    /// profile's never-save list or, while unlocked, the same address is
    /// saved in the profile. One per tab: a new one replaces the last.
    pub fn offer_address(&mut self, tab: &str, profile: &str, origin: &str, parts: [&str; 9]) {
        if !self.offer_to_save(profile) { return; }
        if profile.is_empty() || normalize_origin(origin).as_deref() != Ok(origin) { return; }
        let mut address = reserved(&Address::default());
        for (field, part) in PAGE_PARTS.iter().zip(parts) {
            if part.len() > field.limit() { return; }
            address.part_mut(*field).push_str(part);
        }
        // Worth saving: a street, and a city or postal code to go with it.
        let has = |field| !address.get(field).trim().is_empty();
        if address.street_lines().next().is_none() || !(has(AddressField::City) || has(AddressField::PostalCode)) || address.check().is_err() { return; }
        if self.address_offers.iter().any(|offer| offer.tab == tab && offer.origin == origin && same_parts(&offer.address, &address)) { return; }
        self.ensure_open();
        let Some(vault) = &self.vault else { return };
        if vault.is_never_saved(profile, origin) != Ok(false) { return; }
        if vault.addresses_in(profile).iter().any(|saved| same_parts(&saved.address, &address)) { return; }
        self.address_offers.retain(|offer| offer.tab != tab);
        self.offer_revision += 1;
        self.address_offers.push(AddressOffer {
            tab: tab.to_owned(), origin: origin.to_owned(), profile: profile.to_owned(), address, unlocking: false, error: None, at: self.now(), revision: self.offer_revision,
        });
    }

    pub fn address_offer(&self, tab: &str) -> Option<&AddressOffer> {
        self.address_offers.iter().find(|offer| offer.tab == tab)
    }

    pub fn address_offer_mut(&mut self, tab: &str) -> Option<&mut AddressOffer> {
        self.address_offers.iter_mut().find(|offer| offer.tab == tab)
    }

    /// What the key button can do for a page in `profile` whose address
    /// field has focus: fill one of its addresses, or unlock when the vault,
    /// locked, holds any address. A locked vault can't tell profiles apart.
    pub fn address_key_state(&mut self, profile: &str) -> Option<KeyState> {
        self.ensure_open();
        let vault = self.vault.as_ref()?;
        if vault.is_unlocked() {
            let count = vault.addresses_in(profile).len();
            return (count > 0).then_some(KeyState::Addresses(count));
        }
        vault.address_count().is_ok_and(|count| count > 0).then_some(KeyState::Locked)
    }

    /// The tabs with an address offer.
    pub fn address_offer_tabs(&self) -> impl Iterator<Item = &str> {
        self.address_offers.iter().map(|offer| offer.tab.as_str())
    }

    /// Not Now: wipe the address and save nothing.
    pub fn dismiss_address_offer(&mut self, tab: &str) {
        self.address_offers.retain(|offer| offer.tab != tab);
    }

    /// Never for This Site: the profile's never-save list, shared with passwords.
    pub fn never_for_address_offer(&mut self, tab: &str) -> Option<&'static str> {
        let (Some(vault), Some(offer)) = (&self.vault, self.address_offers.iter_mut().find(|offer| offer.tab == tab)) else { return None };
        match vault.never_save(&offer.profile, &offer.origin) {
            Ok(()) => {
                self.dismiss_address_offer(tab);
                Some(NEVER_SAVED)
            }
            Err(error) => {
                offer.error = Some(error);
                None
            }
        }
    }

    /// Save Address. Locked, it asks to unlock first.
    pub fn save_address_offer(&mut self, tab: &str) -> Option<&'static str> {
        let unlocked = self.unlocked();
        let (Some(vault), Some(index)) = (&mut self.vault, self.address_offers.iter().position(|offer| offer.tab == tab)) else { return None };
        let last_input = vault.last_activity();
        let offer = &mut self.address_offers[index];
        if !unlocked {
            offer.unlocking = true;
            offer.error = None;
            return None;
        }
        let result = if vault.addresses_in(&offer.profile).iter().any(|saved| same_parts(&saved.address, &offer.address)) {
            Ok(ADDRESS_ALREADY_SAVED)
        } else {
            vault.add_address(&offer.profile, &offer.address).map(|_| ADDRESS_SAVED)
        };
        let now = self.now();
        match result {
            Ok(notice) => {
                if let Some(vault) = &mut self.vault { vault.touch(now); }
                self.dismiss_address_offer(tab);
                Some(notice)
            }
            Err(error) => {
                if error == VaultError::ResetElsewhere { self.reset_elsewhere(last_input); }
                self.address_offers[index].error = Some(error);
                None
            }
        }
    }

    /// Unlock, from an offer that asked to, then save as Save would.
    pub fn unlock_for_address_offer(&mut self, tab: &str) -> Option<&'static str> {
        self.address_offer(tab)?;
        self.unlock();
        let error = (!self.unlocked()).then(|| self.page.error.clone().unwrap_or(VaultError::Locked));
        let offer = self.address_offer_mut(tab)?;
        if let Some(error) = error {
            offer.error = Some(error);
            return None;
        }
        offer.unlocking = false;
        offer.error = None;
        self.save_address_offer(tab)
    }
}

#[cfg(test)]
#[path = "page_addresses_tests.rs"]
mod tests;
