//! Saving and filling website passwords from pages: the offer a page's
//! submitted login waits in, and the entries a page may be filled with.
//!
//! An offer belongs to the tab and profile whose page submitted it, and is
//! saved only by a click. Pages are filled only on request, only with entries
//! saved in their own profile for exactly their origin. Nothing here logs,
//! toasts or syncs a username or password.
//!
//! Pages offer to save only in a profile whose "Offer to save passwords from
//! websites" is on: off by default, kept on this device only. The engine
//! leaves the submit listener out of pages while it is off.

use super::*;
use crossbow_core::library::Library;
use crossbow_core::vault::normalize_origin;

/// A page's login is kept this long for Save, then wiped.
pub const OFFER_TIMEOUT: Duration = Duration::from_secs(2 * 60);
pub const SAVED: &str = "Password saved.";
pub const UPDATED: &str = "Password updated.";
pub const ALREADY_SAVED: &str = "This password is already saved.";
pub const NEVER_SAVED: &str = "Crossbow won't offer to save passwords or addresses for this site.";
/// An entry that no longer matches the page, as after a reset or a delete elsewhere.
pub const FILL_GONE: &str = "That password is no longer saved for this site.";
pub const OFFER_TO_SAVE: &str = "Offer to save passwords from websites";
pub const OFFER_TO_SAVE_PREVIEW: &str = "Preview: use test sites and test passwords only until the vault passes its security review.";
pub const OFFER_TO_SAVE_APPLIES: &str = "For this profile on this device. Changes apply to pages loaded afterwards.";

/// Where a profile's switch is kept: the library's device-local, unsynced settings.
fn offer_to_save_key(profile: &str) -> String { format!("passwords.offer_to_save.v1.{profile}") }

/// Load each profile's "Offer to save passwords from websites" once, and
/// save the switch's changes. A value that can't be read is off. A change
/// that can't be saved leaves the switch off: turned on, it goes back off;
/// turned off, it stays off until Crossbow quits. Either way the page says why.
pub fn sync_offer_to_save(passwords: &mut Passwords, lib: &Library) {
    for profile in lib.profiles.keys() {
        if passwords.offer_to_save.contains_key(profile) { continue; }
        let on = match lib.store.kv_get(&offer_to_save_key(profile)) {
            Ok(value) => value.as_deref() == Some("1"),
            Err(error) => {
                passwords.offer_to_save_error = Some((profile.clone(), format!("Could not read this setting: {error}. It is off.")));
                false
            }
        };
        passwords.offer_to_save.insert(profile.clone(), on);
    }
    for (profile, on) in std::mem::take(&mut passwords.offer_to_save_writes) {
        let Err(error) = lib.store.kv_set(&offer_to_save_key(&profile), if on { "1" } else { "0" }) else { continue };
        let after = if on { "It is still off." } else { "It is off until Crossbow quits." };
        passwords.offer_to_save.insert(profile.clone(), false);
        passwords.offer_to_save_error = Some((profile, format!("Could not save this setting: {error}. {after}")));
    }
}

/// Whether Save adds an entry or replaces the password of an existing one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OfferKind {
    Save,
    /// The entry with the same origin, profile and username.
    Update(String),
}

/// A login a page submitted, waiting for Save, Never for This Site or Not Now.
pub struct Offer {
    pub tab: String,
    /// The page's origin, as the engine checked it.
    pub origin: String,
    pub profile: String,
    /// Editable before saving.
    pub username: Zeroizing<String>,
    password: Zeroizing<String>,
    pub kind: OfferKind,
    /// Save was chosen while the vault was locked: the card asks to unlock.
    pub unlocking: bool,
    pub error: Option<VaultError>,
    pub(super) at: Moment,
    /// Distinct for each offer a page submitted, so the card can tell a
    /// replaced offer from the one it showed.
    pub revision: u64,
}

impl std::fmt::Debug for Offer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Offer").field("tab", &self.tab).field("origin", &self.origin).field("kind", &self.kind).field("unlocking", &self.unlocking).finish_non_exhaustive()
    }
}

/// What the key button can do for a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyState {
    /// Unlocked, with this many entries for the page's origin and profile.
    Logins(usize),
    /// Locked, on a page with a login field, in a vault that has entries,
    /// or with an address or card field, in a vault that has addresses or cards.
    Locked,
    /// Unlocked, with no login for the page but this many addresses for the
    /// address field that has focus.
    Addresses(usize),
    /// Unlocked, with no login or address for the page but this many cards
    /// for the card field that had focus.
    Cards(usize),
}

impl Passwords {
    /// Whether pages of `profile` may offer to save a login.
    pub fn offer_to_save(&self, profile: &str) -> bool {
        self.offer_to_save.get(profile).copied().unwrap_or(false)
    }

    /// Every profile's switch, for the engine to mirror.
    pub fn offer_to_save_settings(&self) -> Vec<(String, bool)> {
        self.offer_to_save.iter().map(|(profile, on)| (profile.clone(), *on)).collect()
    }

    /// Why `profile`'s switch could not be read or saved.
    pub fn offer_to_save_error(&self, profile: &str) -> Option<&str> {
        self.offer_to_save_error.as_ref().filter(|(failed, _)| failed == profile).map(|(_, error)| error.as_str())
    }

    /// The switch. It applies here at once, and the app saves it. Turned off,
    /// the profile's waiting offers are wiped.
    pub fn set_offer_to_save(&mut self, profile: &str, on: bool) {
        if profile.is_empty() { return; }
        self.offer_to_save.insert(profile.to_owned(), on);
        self.offer_to_save_error = None;
        self.offer_to_save_writes.retain(|(changed, _)| changed != profile);
        self.offer_to_save_writes.push((profile.to_owned(), on));
        if !on {
            self.offers.retain(|offer| offer.profile != profile);
            self.address_offers.retain(|offer| offer.profile != profile);
        }
    }

    /// The active profile and every profile's name, from the app each frame.
    /// A change closes the page's editors and prompts as leaving the page
    /// does, wiping what was typed: an address or card form saves into the
    /// profile it opened in, and a Delete prompt names its entry there.
    pub fn set_profiles(&mut self, current: &str, names: impl IntoIterator<Item = (String, String)>) {
        if self.profile != current {
            self.profile = current.to_owned();
            self.hide_page();
        }
        self.profile_names.clear();
        self.profile_names.extend(names);
    }

    pub fn current_profile(&self) -> &str {
        &self.profile
    }

    pub fn profile_names(&self) -> &[(String, String)] {
        &self.profile_names
    }

    /// A page in `tab` submitted a login. Offer it if the profile offers to
    /// save, unless its site is on the profile's never-save list or, while
    /// unlocked, the same login is saved.
    /// One offer per tab: a new one replaces the last, unless it is the same login.
    pub fn offer_login(&mut self, tab: &str, profile: &str, origin: &str, username: &str, password: &str) {
        if !self.offer_to_save(profile) { return; }
        if profile.is_empty() || password.is_empty() || normalize_origin(origin).as_deref() != Ok(origin) { return; }
        if self.offers.iter().any(|offer| offer.tab == tab && offer.origin == origin && offer.username.as_str() == username && offer.password.as_str() == password) { return; }
        self.ensure_open();
        let Some(vault) = &self.vault else { return };
        if vault.is_never_saved(profile, origin) != Ok(false) { return; }
        let kind = if vault.is_unlocked() {
            match classify(vault, origin, profile, username, password) {
                Ok(Some(kind)) => kind,
                _ => return,
            }
        } else {
            OfferKind::Save
        };
        self.offers.retain(|offer| offer.tab != tab);
        self.offer_revision += 1;
        let mut name = buffer(MAX_USERNAME_BYTES);
        name.push_str(username);
        let mut secret = buffer(MAX_PASSWORD_BYTES);
        secret.push_str(password);
        self.offers.push(Offer {
            tab: tab.to_owned(), origin: origin.to_owned(), profile: profile.to_owned(), username: name, password: secret,
            kind, unlocking: false, error: None, at: self.now(), revision: self.offer_revision,
        });
    }

    pub fn offer(&self, tab: &str) -> Option<&Offer> {
        self.offers.iter().find(|offer| offer.tab == tab)
    }

    pub fn offer_mut(&mut self, tab: &str) -> Option<&mut Offer> {
        self.offers.iter_mut().find(|offer| offer.tab == tab)
    }

    /// The tabs with an offer.
    pub fn offer_tabs(&self) -> impl Iterator<Item = &str> {
        self.offers.iter().map(|offer| offer.tab.as_str())
    }

    /// Drop offers, of logins and addresses, whose tab closed.
    pub fn keep_offers(&mut self, live: impl Fn(&str) -> bool) {
        self.offers.retain(|offer| live(&offer.tab));
        self.address_offers.retain(|offer| live(&offer.tab));
    }

    /// Not Now: wipe the login and save nothing.
    pub fn dismiss_offer(&mut self, tab: &str) {
        self.offers.retain(|offer| offer.tab != tab);
    }

    /// Never for This Site: remember the site for this profile, on this device only.
    pub fn never_for_offer(&mut self, tab: &str) -> Option<&'static str> {
        let (Some(vault), Some(offer)) = (&self.vault, self.offers.iter_mut().find(|offer| offer.tab == tab)) else { return None };
        match vault.never_save(&offer.profile, &offer.origin) {
            Ok(()) => {
                self.dismiss_offer(tab);
                Some(NEVER_SAVED)
            }
            Err(error) => {
                offer.error = Some(error);
                None
            }
        }
    }

    /// Save or Update Password. Locked, it asks to unlock first. The login is
    /// checked against the vault again, so a username edited into an existing
    /// one shows Update Password before anything is replaced.
    pub fn save_offer(&mut self, tab: &str) -> Option<&'static str> {
        let unlocked = self.unlocked();
        let (Some(vault), Some(index)) = (&mut self.vault, self.offers.iter().position(|offer| offer.tab == tab)) else { return None };
        let last_input = vault.last_activity();
        let offer = &mut self.offers[index];
        if !unlocked {
            offer.unlocking = true;
            offer.error = None;
            return None;
        }
        let result = classify(vault, &offer.origin, &offer.profile, &offer.username, &offer.password).and_then(|kind| match kind {
            None => Ok(Some(ALREADY_SAVED)),
            Some(kind) if kind != offer.kind => {
                // Shown first; the next click confirms.
                offer.kind = kind;
                Ok(None)
            }
            Some(OfferKind::Save) => {
                let input = EntryInput { origin: &offer.origin, username: &offer.username, password: &offer.password, notes: "", profile: Some(&offer.profile) };
                vault.add(&input).map(|_| Some(SAVED))
            }
            Some(OfferKind::Update(id)) => {
                let notes = vault.secret(&id)?.notes;
                let input = EntryInput { origin: &offer.origin, username: &offer.username, password: &offer.password, notes: &notes, profile: Some(&offer.profile) };
                vault.update(&id, &input).map(|()| Some(UPDATED))
            }
        });
        let now = self.now();
        match result {
            Ok(Some(notice)) => {
                if let Some(vault) = &mut self.vault { vault.touch(now); }
                self.dismiss_offer(tab);
                Some(notice)
            }
            Ok(None) => None,
            Err(error) => {
                if error == VaultError::ResetElsewhere { self.reset_elsewhere(last_input); }
                self.offers[index].error = Some(error);
                None
            }
        }
    }

    /// Unlock, from an offer that asked to, then save as Save would.
    pub fn unlock_for_offer(&mut self, tab: &str) -> Option<&'static str> {
        if self.offer(tab).is_none() { return None; }
        self.unlock();
        let error = (!self.unlocked()).then(|| self.page.error.clone().unwrap_or(VaultError::Locked));
        let offer = self.offer_mut(tab)?;
        if let Some(error) = error {
            offer.error = Some(error);
            return None;
        }
        offer.unlocking = false;
        offer.error = None;
        self.save_offer(tab)
    }

    /// What the key button can do on a page of `origin` in `profile`.
    pub fn key_state(&mut self, origin: &str, profile: &str, login_field: bool) -> Option<KeyState> {
        let vault = self.vault.as_ref()?;
        if vault.is_unlocked() {
            let count = vault.matching(origin, profile).len();
            return (count > 0).then_some(KeyState::Logins(count));
        }
        (login_field && vault.count().is_ok_and(|count| count > 0)).then_some(KeyState::Locked)
    }

    /// Open the vault file, for a page that may need it.
    pub fn open_for_pages(&mut self) {
        self.ensure_open();
    }

    /// The entries a page of `origin` in `profile` may be filled with, by id
    /// and username. Empty while locked.
    pub fn logins_for(&self, origin: &str, profile: &str) -> Vec<(String, Zeroizing<String>)> {
        self.vault.as_ref().map(|vault| vault.matching(origin, profile).into_iter().map(|entry| (entry.id.clone(), entry.username.clone())).collect()).unwrap_or_default()
    }

    /// The username and password to fill into a page of `origin` in
    /// `profile`, if entry `id` is still saved for exactly that origin and
    /// profile. Records the use. A reset in another window, found by the read
    /// or the record, is handled as any write that finds one, and fills nothing.
    pub fn login_to_fill(&mut self, id: &str, origin: &str, profile: &str) -> Result<(Zeroizing<String>, Zeroizing<String>), &'static str> {
        let now = self.now();
        let vault = self.vault.as_mut().ok_or(FILL_GONE)?;
        let last_input = vault.last_activity();
        let entry = vault.matching(origin, profile).into_iter().find(|entry| entry.id == id).ok_or(FILL_GONE)?;
        let username = entry.username.clone();
        let found = vault.password(id).and_then(|password| match vault.mark_used(id, crossbow_core::now_ms()) {
            Err(VaultError::ResetElsewhere) => Err(VaultError::ResetElsewhere),
            // A fill is vault activity; failing to record it otherwise never blocks the fill.
            _ => Ok(password),
        });
        match found {
            Ok(password) => {
                if let Some(vault) = &mut self.vault { vault.touch(now); }
                Ok((username, password))
            }
            Err(VaultError::ResetElsewhere) => {
                self.reset_elsewhere(last_input);
                Err(FILL_GONE)
            }
            Err(_) => Err(FILL_GONE),
        }
    }

    /// The sites `profile` never offers saving for.
    pub fn never_saved(&mut self, profile: &str) -> Vec<String> {
        self.ensure_open();
        self.vault.as_ref().and_then(|vault| vault.never_saved(profile).ok()).unwrap_or_default()
    }

    pub fn remove_never_save(&mut self, profile: &str, origin: &str) {
        let Some(vault) = &self.vault else { return };
        if let Err(error) = vault.remove_never_save(profile, origin) { self.say(None, None, Some(error)); }
    }

    /// Use in Profile: offer and fill an entry from before profiles, in the active profile.
    pub fn use_in_current_profile(&mut self, id: &str) {
        let profile = self.profile.clone();
        let Some(vault) = &mut self.vault else { return };
        let last_input = vault.last_activity();
        if let Err(error) = vault.set_profile(id, &profile) { self.refused(id, last_input, error); }
    }
}

/// Save, Update or, for a login already saved, nothing.
fn classify(vault: &Vault, origin: &str, profile: &str, username: &str, password: &str) -> Result<Option<OfferKind>, VaultError> {
    let Some(entry) = vault.matching(origin, profile).into_iter().find(|entry| entry.username.as_str() == username) else { return Ok(Some(OfferKind::Save)) };
    let saved = vault.password(&entry.id)?;
    Ok((saved.as_str() != password).then(|| OfferKind::Update(entry.id.clone())))
}

#[cfg(test)]
#[path = "page_logins_tests.rs"]
pub(crate) mod tests;
