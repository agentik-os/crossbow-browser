//! Local password vault (preview, not security reviewed).
//!
//! Entries live in their own SQLite file, `vault.sqlite`, which the library
//! store and the sync client never open. Every origin, username, password and
//! note is sealed separately with XChaCha20-Poly1305 under a subkey of the
//! vault's own random key, which stays in the OS keychain. The list opens only
//! origins and usernames; passwords and notes are opened on request. Saved
//! addresses live in the same file, each sealed whole with its profile tag,
//! and are opened at unlock. Saved payment cards too: each card's details
//! are opened at unlock, and its number, sealed on its own, only for a
//! fill. No card's security code is ever stored. Locking drops the cipher
//! and every opened value, which are wiped on drop. Nothing here is synced
//! or backed up.

mod address;
mod card;
mod crypto;
mod db;
pub mod generator;
mod key_store;
mod origin;

pub use address::{Address, AddressField, SavedAddress};
pub use card::{
    Card, CardHint, CardNetwork, FIRST_EXPIRY_YEAR, LAST_EXPIRY_YEAR, MAX_CARD_DIGITS, MAX_CARD_NAME_BYTES, MAX_NICKNAME_BYTES, MIN_CARD_DIGITS, SavedCard, expired,
    luhn_valid, year_month,
};
pub use crypto::VaultKey;
pub use key_store::{KEYCHAIN_SERVICE, KeyLocation, MemoryVaultKeyStore, OsVaultKeyStore, STORAGE_ERROR, VaultKeyStore, qa_key_file};
pub use origin::normalize_origin;
pub use zeroize::{Zeroize, Zeroizing};

use card::Details;
use crypto::{Binding, Field, VaultCipher};
use db::Db;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::path::Path;
use std::time::{Duration, Instant};

pub const FILE_NAME: &str = "vault.sqlite";
/// The vault locks after this long without vault activity.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub const MAX_ORIGIN_BYTES: usize = 1024;
pub const MAX_USERNAME_BYTES: usize = 256;
pub const MAX_PASSWORD_BYTES: usize = 1024;
pub const MAX_NOTES_BYTES: usize = 4096;
/// Profile ids are UUIDs; this only bounds a corrupt or hostile one.
pub const MAX_PROFILE_BYTES: usize = 128;
const CHECK_PLAINTEXT: &str = "crossbow-vault-v1";

/// Every message is safe to show: none contains a secret.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VaultError {
    #[error("{0}")]
    KeyStore(String),
    #[error("This vault's key wasn't found, so its passwords can't be opened.")]
    MissingKey,
    #[error("The stored key doesn't open any of this vault's passwords, so they can't be opened.")]
    KeyMismatch,
    #[error("The vault file is damaged, so its passwords can't be opened.")]
    Damaged,
    #[error("This vault was created by a newer version of Crossbow. Update Crossbow to open it.")]
    Newer,
    #[error("Couldn't read or write the vault file. Check the data folder's permissions and free disk space, then try again.")]
    Storage,
    #[error("The vault is locked. Unlock it to continue.")]
    Locked,
    #[error("The vault was reset in another window. Unlock again.")]
    ResetElsewhere,
    #[error("That password no longer exists.")]
    NotFound,
    #[error("This entry can't be decrypted. It may be damaged. You can delete it.")]
    Unreadable,
    #[error("{0}")]
    Invalid(&'static str),
    #[error("The system random number generator is unavailable.")]
    Random,
    /// A Recover Passwords stopped before it finished, and what it moved
    /// aside can't be put back beside the vault file it came from, or the
    /// vault file is neither that file nor the one the Recover made: every
    /// file it left, by name. They may hold what the vault file lacks, so
    /// nothing says they may be deleted. The vault file isn't opened, reset
    /// or recovered until they go back, so nothing is written over them, or
    /// until the user chooses Open Without Them (round 12, R10-1).
    #[error("Recover Passwords stopped before it finished, and the vault file couldn't be put back as it was. The vault stays closed so nothing is written over these files in Crossbow's data folder: {}. They may hold passwords the vault file doesn't, so don't delete them. Try Again puts them back if it can. If it can't, Open Without Them opens the vault file as it is.", .0.join(", "))]
    UndoIncomplete(Vec<String>),
    /// A Recover Passwords by a build before round 9 stopped after it moved
    /// the vault file's WAL or journal under a kept name, and left no record
    /// of the vault file as it was: every file it left, by name. That build's
    /// undo opened the vault even when it couldn't put a WAL back, so nothing
    /// tells whether the vault file was written since, and they never go
    /// back by themselves. The vault file isn't opened, reset or recovered,
    /// so nothing is written over them, until the user chooses Open Without
    /// Them; trying again can't change that.
    #[error("A Recover Passwords from an older version of Crossbow stopped before it finished and left no record of the vault file as it was. Crossbow can't tell whether these files in its data folder still belong with the vault file, so the vault stays closed and nothing is written over them: {}. They hold passwords the vault file may not have. Don't move or delete them, and copy the whole data folder somewhere safe before changing anything in it. Open Without Them opens the vault file as it is.", .0.join(", "))]
    UnrecordedStop(Vec<String>),
    /// A Recover Passwords stopped before it finished, after it moved the
    /// vault file's WAL or journal under a kept name, and they are gone from
    /// the data folder, such as deleted in Finder, while the vault file is
    /// still the one they came from: by name. The vault file lacks the
    /// commits they held, and Recover's working file, which has them, is gone
    /// or changed too (round 12, R10-2). Try Again puts them back once they
    /// are there again, or Open Without Them opens the vault file as it is.
    #[error("Recover Passwords stopped before it finished, and files it moved aside are gone from Crossbow's data folder: {}. They held the vault file's newest changes, so passwords, addresses or cards saved shortly before the Recover may be missing. If you still have them, such as in the Trash, put them back in the data folder and choose Try Again.", .0.join(", "))]
    KeptFilesGone(Vec<String>),
}

impl VaultError {
    /// Errors that only resetting the vault can resolve.
    pub fn needs_reset(&self) -> bool {
        matches!(self, VaultError::MissingKey | VaultError::KeyMismatch | VaultError::Damaged)
    }

    /// Errors that trying again can't change.
    pub fn is_final(&self) -> bool {
        matches!(self, VaultError::UnrecordedStop(_))
    }

    /// Errors a stopped Recover keeps the vault closed with, which Open
    /// Without Them ends (see `Vault::open_without_stopped_recover`).
    pub fn can_open_without(&self) -> bool {
        matches!(self, VaultError::UndoIncomplete(_) | VaultError::UnrecordedStop(_) | VaultError::KeptFilesGone(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VaultStatus {
    /// No key yet: the first unlock creates one.
    New,
    Locked,
    Unlocked,
}

/// A point in time on both clocks. The monotonic clock doesn't advance while
/// a Mac sleeps; the wall clock does, so idle time counts whichever is larger.
#[derive(Clone, Copy, Debug)]
pub struct Moment {
    pub mono: Instant,
    pub wall_ms: u64,
}

impl Moment {
    pub fn now() -> Moment {
        Moment { mono: Instant::now(), wall_ms: crate::now_ms() }
    }

    pub fn after(self, elapsed: Duration) -> Moment {
        Moment { mono: self.mono + elapsed, wall_ms: self.wall_ms + elapsed.as_millis() as u64 }
    }

    /// Time since `earlier` on whichever clock moved further. A wall clock
    /// that went backwards counts as unbounded, so it never extends a deadline.
    pub fn since(self, earlier: Moment) -> Duration {
        let Some(wall) = self.wall_ms.checked_sub(earlier.wall_ms) else { return Duration::MAX };
        self.mono.saturating_duration_since(earlier.mono).max(Duration::from_millis(wall))
    }
}

/// What Recover Passwords did with a damaged vault file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovery {
    /// Entries that opened and verified with the vault's key, now in the new file.
    pub recovered: usize,
    /// Distinct entry rows any read of the damaged file saw, readable or not,
    /// none left out but a "row" with no cell that reads at a rowid below 1,
    /// which the vault never writes. Damage can hide a row from every read,
    /// so the file may have held more; a damaged id can be counted as one
    /// more row.
    pub found: usize,
    /// Whether some read went to its end without an error having seen every
    /// row any read saw. Otherwise the reads disagree or stopped short, and
    /// `found` is not the file's total.
    pub complete: bool,
    /// The addresses, counted the same way.
    pub addresses: Counted,
    /// The cards, counted the same way.
    pub cards: Counted,
    /// The never-save list, counted the same way.
    pub never_save: Counted,
    /// The name the damaged file was moved aside as.
    pub moved: String,
    /// Files an earlier Recover that stopped left beside another vault file,
    /// named with this one's own until they are moved or deleted.
    pub also_kept: Vec<String>,
}

/// How much of one table Recover Passwords got back.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Counted {
    /// Rows written to the new file.
    pub recovered: usize,
    /// Distinct rows any read of the damaged file saw, readable or not, none left out.
    pub found: usize,
    /// Whether some read went to its end without an error having seen every
    /// row any read saw.
    pub complete: bool,
}

/// An entry as listed: its origin and username opened, its password and notes
/// still sealed. A damaged row keeps empty values and can only be deleted.
pub struct EntrySummary {
    pub id: String,
    pub origin: Zeroizing<String>,
    pub username: Zeroizing<String>,
    /// The Crossbow profile it belongs to. None for an entry from format 1,
    /// which is listed but never offered or filled on a page.
    pub profile: Option<String>,
    pub created_ms: u64,
    pub updated_ms: u64,
    /// When it was last filled into a page.
    pub used_ms: Option<u64>,
    pub state: EntryState,
}

/// Whether an entry opened. Unreadable entries keep empty values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryState {
    Readable,
    /// Damaged or substituted: it can only be deleted.
    Damaged,
}

impl EntrySummary {
    pub fn readable(&self) -> bool {
        self.state == EntryState::Readable
    }
}

/// How an unlock went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnlockOutcome {
    Opened,
    /// The check value was damaged or missing, but the stored key opens the
    /// entries, so the check was rewritten.
    CheckRepaired,
}

impl fmt::Debug for EntrySummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EntrySummary").field("id", &self.id).field("state", &self.state).finish_non_exhaustive()
    }
}

/// What the user typed for a new or edited entry.
pub struct EntryInput<'a> {
    pub origin: &'a str,
    pub username: &'a str,
    pub password: &'a str,
    pub notes: &'a str,
    /// The profile to offer and fill it in. None leaves it in no profile.
    pub profile: Option<&'a str>,
}

impl fmt::Debug for EntryInput<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EntryInput(..)")
    }
}

/// An entry's opened password and notes, for the editor.
pub struct Secret {
    pub password: Zeroizing<String>,
    pub notes: Zeroizing<String>,
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

struct Session {
    cipher: VaultCipher,
    entries: Vec<EntrySummary>,
    /// Rows whose id cell can't be read, by the id they are listed under.
    damaged_ids: HashMap<String, db::StoredId>,
    addresses: Vec<SavedAddress>,
    /// Address rows whose id cell can't be read, by the id they are listed under.
    damaged_addresses: HashMap<String, db::StoredId>,
    /// Every card's details; never a number.
    cards: Vec<SavedCard>,
    /// Card rows whose id cell can't be read, by the id they are listed under.
    damaged_cards: HashMap<String, db::StoredId>,
    last_activity: Moment,
}

pub struct Vault {
    db: Db,
    keys: Box<dyn VaultKeyStore>,
    open: Option<Session>,
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vault").field("unlocked", &self.open.is_some()).finish_non_exhaustive()
    }
}

impl Vault {
    /// Open or create `data_dir/vault.sqlite`. Creating the file creates no key.
    /// A Recover Passwords that stopped part way through is undone first;
    /// while that can't finish, the file is neither opened nor created.
    pub fn open(data_dir: &Path, keys: Box<dyn VaultKeyStore>) -> Result<Vault, VaultError> {
        undo_stopped_swap(data_dir)?;
        Ok(Vault { db: Db::open(&data_dir.join(FILE_NAME))?, keys, open: None })
    }

    /// Files a Recover Passwords left in `data_dir` when it finished but
    /// stopped before it could say where it kept the damaged file: the vault
    /// file is the one it made (see `Stand`). They hold the damaged file as
    /// it was, with anything Recover couldn't read, so they are named, by
    /// name, while they exist. The files of one that didn't finish are named
    /// by the error that keeps the vault closed. Reads only.
    pub fn kept_from_stopped_recover(data_dir: &Path) -> Vec<String> {
        stopped_swaps(data_dir).into_iter().filter(|name| matches!(stand(data_dir, name), Stand::Finished(_))).flat_map(|name| kept_files(data_dir, &name)).collect()
    }

    /// Files a stopped Recover Passwords left that Open Without Them set
    /// aside, by name, while they exist: they may hold passwords the vault
    /// doesn't, and are never put back. Reads only.
    pub fn set_aside_from_stopped_recover(data_dir: &Path) -> Vec<String> {
        stopped_swaps(data_dir).into_iter().filter(|name| matches!(stand(data_dir, name), Stand::SetAside)).flat_map(|name| kept_files(data_dir, &name)).collect()
    }

    /// Open Without Them, once the user confirmed it, for the errors a
    /// stopped Recover Passwords keeps the vault closed with (see
    /// `VaultError::can_open_without`): each stop that can't be undone or
    /// finished is set aside (see `set_aside`), its files left where they
    /// are, and the vault file then opens as it is. A stop whose moved
    /// files are gone leaves nothing to keep, so it goes. Recover's working
    /// file goes once nothing is left to undo.
    pub fn open_without_stopped_recover(data_dir: &Path) -> Result<(), VaultError> {
        for name in stopped_swaps(data_dir) {
            if !matches!(place(data_dir, &name)?, Placed::Done) { set_aside(data_dir, &name)?; }
        }
        undo_stopped_swap(data_dir)
    }

    pub fn status(&self) -> Result<VaultStatus, VaultError> {
        if self.open.is_some() { return Ok(VaultStatus::Unlocked); }
        // Entries, addresses or cards without a check value still mean an existing vault.
        Ok(if self.db.check()?.is_some() || self.db.check_and_count()?.1 > 0 { VaultStatus::Locked } else { VaultStatus::New })
    }

    pub fn key_location(&self) -> KeyLocation {
        self.keys.location()
    }

    pub fn is_unlocked(&self) -> bool {
        self.open.is_some()
    }

    /// The vault's id: its key-store account name. Not secret.
    pub fn id(&self) -> &str {
        &self.db.vault_id
    }

    fn binding<'a>(&'a self, entry_id: &'a str, field: Field, origin: &'a str) -> Binding<'a> {
        Binding { vault_id: &self.db.vault_id, entry_id, field, origin }
    }

    /// Read the key from secure storage, or create it on first use, and open
    /// every entry's origin and username. Never falls back to plaintext.
    pub fn unlock(&mut self, now: Moment) -> Result<UnlockOutcome, VaultError> {
        if self.open.is_some() { return Ok(UnlockOutcome::Opened); }
        // A pass that lost the first check write to another handle reads
        // again and finds that handle's check. Losing three times would need
        // that check removed twice more, by Reset or a refused key save.
        for _ in 0..3 {
            let Some((cipher, outcome)) = self.unlock_pass()? else { continue };
            let (mut entries, mut damaged_ids) = (Vec::new(), HashMap::new());
            for mut row in self.db.rows()? {
                if let Some(stored) = row.stored_id.take() { damaged_ids.insert(row.id.clone(), stored); }
                entries.push(self.summary(&cipher, row));
            }
            sort(&mut entries);
            let (mut addresses, mut damaged_addresses) = (Vec::new(), HashMap::new());
            for mut row in self.db.address_rows()? {
                if let Some(stored) = row.stored_id.take() { damaged_addresses.insert(row.id.clone(), stored); }
                addresses.push(self.saved_address_of(&cipher, row));
            }
            sort_addresses(&mut addresses);
            let (mut cards, mut damaged_cards) = (Vec::new(), HashMap::new());
            for mut row in self.db.card_rows()? {
                if let Some(stored) = row.stored_id.take() { damaged_cards.insert(row.id.clone(), stored); }
                cards.push(self.saved_card_of(&cipher, row));
            }
            sort_cards(&mut cards);
            self.open = Some(Session { cipher, entries, damaged_ids, addresses, damaged_addresses, cards, damaged_cards, last_activity: now });
            return Ok(outcome);
        }
        Err(VaultError::Storage)
    }

    /// One reading of the file and the key store. None means another handle
    /// wrote the first check after this pass read none.
    fn unlock_pass(&mut self) -> Result<Option<(VaultCipher, UnlockOutcome)>, VaultError> {
        // Another handle may have reset the file while this one was locked.
        self.follow_reset()?;
        // One read, so another handle's first unlock can't fall between them.
        let (check, count) = self.db.check_and_count()?;
        let stored = self.keys.load(&self.db.vault_id).map_err(VaultError::KeyStore)?;
        let mut outcome = UnlockOutcome::Opened;
        let cipher = match (stored, check) {
            (Some(key), Some(check)) => {
                let cipher = key.cipher();
                match crypto::open(&cipher, &self.binding("", Field::Check, ""), &check) {
                    Ok(opened) if opened.as_str() == CHECK_PLAINTEXT => {}
                    // Inside a current-format file, a header naming another
                    // version or epoch is damage like any other.
                    _ => outcome = self.repair_check(&cipher)?,
                }
                cipher
            }
            // A check value or entries mean a key was made, and it's gone. It
            // is also what another handle's first unlock looks like between
            // its check and its key: Try Again then finds the key.
            (None, Some(_)) => return Err(VaultError::MissingKey),
            (None, None) if count > 0 => return Err(VaultError::MissingKey),
            // Entries without a check value: rebuild it if the key opens them.
            (Some(key), None) if count > 0 => {
                let cipher = key.cipher();
                outcome = self.repair_check(&cipher)?;
                cipher
            }
            // A key with no check and no entries: a first unlock by an earlier
            // build saved its key and stopped, or the check row was lost. Adopt it.
            (Some(key), None) => {
                let cipher = key.cipher();
                if !self.db.insert_check(&self.seal_check(&cipher)?)? { return Ok(None); }
                cipher
            }
            // The first unlock. The check goes in first, and only if there is
            // none, so only the one handle that wrote it ever saves a key for
            // this vault id; a loser drops its key and reads the winner's.
            // Stopping before the key is saved leaves a check with no key and
            // no entries, which Try Again or Reset resolves with nothing lost.
            (None, None) => {
                let key = VaultKey::generate()?;
                let cipher = key.cipher();
                let check = self.seal_check(&cipher)?;
                if !self.db.insert_check(&check)? { return Ok(None); }
                if let Err(error) = self.keys.save(&self.db.vault_id, &key) {
                    // No key was stored, so take its check back out.
                    let _ = self.db.remove_check(&check);
                    return Err(VaultError::KeyStore(error));
                }
                cipher
            }
        };
        Ok(Some((cipher, outcome)))
    }

    fn seal_check(&self, cipher: &VaultCipher) -> Result<Vec<u8>, VaultError> {
        crypto::seal(cipher, &self.binding("", Field::Check, ""), CHECK_PLAINTEXT)
    }

    /// Replace a damaged or missing check, for a key that opens the entries.
    fn write_check(&self, cipher: &VaultCipher) -> Result<(), VaultError> {
        if self.db.set_check(&self.seal_check(cipher)?)? { Ok(()) } else { Err(VaultError::ResetElsewhere) }
    }

    /// Re-read the file's vault id. If another handle reset the vault, lock
    /// and take the new id, so nothing is opened or written under the old one.
    /// True when the id changed.
    pub fn follow_reset(&mut self) -> Result<bool, VaultError> {
        let current = self.db.file_vault_id()?;
        if current == self.db.vault_id { return Ok(false); }
        self.lock();
        self.db.vault_id = current;
        Ok(true)
    }

    /// A write that changed nothing: another handle reset the vault, or the
    /// entry is gone.
    fn unwritten(&mut self, id: &str) -> VaultError {
        match self.follow_reset() {
            Ok(true) => VaultError::ResetElsewhere,
            Ok(false) => {
                self.forget(id);
                VaultError::NotFound
            }
            Err(error) => error,
        }
    }

    /// The check value is damaged or missing. If the stored key opens any
    /// entry, address or card, the key is right and only the check row was
    /// damaged, so rewrite it. A key that opens none is a mismatch.
    fn repair_check(&self, cipher: &VaultCipher) -> Result<UnlockOutcome, VaultError> {
        let (rows, addresses, cards) = (self.db.rows()?, self.db.address_rows()?, self.db.card_rows()?);
        let opens_entry = rows.iter().any(|row| crypto::open(cipher, &self.binding(&row.id, Field::Origin, ""), &row.origin).is_ok());
        let opens_address = addresses.iter().any(|row| crypto::open(cipher, &self.binding(&row.id, Field::AddressProfile, ""), &row.profile).is_ok());
        let opens_card = cards.iter().any(|row| crypto::open(cipher, &self.binding(&row.id, Field::CardProfile, ""), &row.profile).is_ok());
        if opens_entry || opens_address || opens_card {
            self.write_check(cipher)?;
            return Ok(UnlockOutcome::CheckRepaired);
        }
        if !rows.is_empty() || !addresses.is_empty() || !cards.is_empty() { return Err(VaultError::KeyMismatch); }
        // No entries: nothing can be lost, so the stored key becomes the key.
        self.write_check(cipher)?;
        Ok(UnlockOutcome::CheckRepaired)
    }

    fn summary(&self, cipher: &VaultCipher, row: db::SealedRow) -> EntrySummary {
        if row.damaged {
            return EntrySummary { id: row.id, origin: Default::default(), username: Default::default(), profile: None, created_ms: 0, updated_ms: 0, used_ms: None, state: EntryState::Damaged };
        }
        let opened = crypto::open(cipher, &self.binding(&row.id, Field::Origin, ""), &row.origin).and_then(|origin| {
            let username = crypto::open(cipher, &self.binding(&row.id, Field::Username, &origin), &row.username)?;
            Ok((origin, username))
        });
        // A tag that doesn't open is damage, never "no profile": moving or
        // altering one must not make an entry fillable somewhere else.
        let opened = opened.and_then(|(origin, username)| {
            let profile = match &row.profile {
                Some(sealed) => Some(crypto::open(cipher, &self.binding(&row.id, Field::Profile, &origin), sealed)?.to_string()),
                None => None,
            };
            Ok((origin, username, profile))
        });
        let (state, (origin, username, profile)) = match opened {
            Ok(opened) => (EntryState::Readable, opened),
            Err(_) => (EntryState::Damaged, Default::default()),
        };
        EntrySummary { id: row.id, origin, username, profile, created_ms: row.created_ms, updated_ms: row.updated_ms, used_ms: row.used_ms, state }
    }

    /// A sealed address row opened, or listed damaged: a profile tag or an
    /// address that doesn't open, or doesn't decode, is damage, never "no
    /// profile", so a moved cell is filled nowhere.
    fn saved_address_of(&self, cipher: &VaultCipher, row: db::SealedAddress) -> SavedAddress {
        let opened = (!row.damaged).then(|| {
            let profile = crypto::open(cipher, &self.binding(&row.id, Field::AddressProfile, ""), &row.profile).ok()?;
            let address = Address::decode(&crypto::open(cipher, &self.binding(&row.id, Field::Address, ""), &row.address).ok()?)?;
            Some((profile.to_string(), address))
        }).flatten();
        match opened {
            Some((profile, address)) => SavedAddress { id: row.id, profile, address, created_ms: row.created_ms, updated_ms: row.updated_ms, used_ms: row.used_ms, state: EntryState::Readable },
            None => SavedAddress { id: row.id, profile: String::new(), address: Address::default(), created_ms: 0, updated_ms: 0, used_ms: None, state: EntryState::Damaged },
        }
    }

    /// A sealed card row opened, or listed damaged: a profile tag, details
    /// or number that doesn't open, or doesn't read as the vault writes it,
    /// is damage, so a moved cell is filled nowhere. The number is opened
    /// only to check it, and wiped.
    fn saved_card_of(&self, cipher: &VaultCipher, row: db::SealedCard) -> SavedCard {
        let opened = (!row.damaged).then(|| open_card(cipher, &self.db.vault_id, &row.id, &row.profile, &row.card, &row.number)).flatten();
        let Some((profile, details)) = opened else { return SavedCard::damaged(row.id) };
        let Details { name, nickname, exp_month, exp_year, network, last4 } = details;
        SavedCard {
            id: row.id, profile: profile.to_string(), name, nickname, exp_month, exp_year, network, last4,
            created_ms: row.created_ms, updated_ms: row.updated_ms, used_ms: row.used_ms, state: EntryState::Readable,
        }
    }

    /// Drop the cipher and every opened value; each wipes itself on drop.
    pub fn lock(&mut self) {
        self.open = None;
    }

    /// Record vault activity, postponing the idle lock.
    pub fn touch(&mut self, now: Moment) {
        if let Some(open) = &mut self.open { open.last_activity = now; }
    }

    /// Lock when idle for `IDLE_TIMEOUT`. True when this call locked it.
    pub fn lock_if_idle(&mut self, now: Moment) -> bool {
        let idle = self.open.as_ref().is_some_and(|open| now.since(open.last_activity) >= IDLE_TIMEOUT);
        if idle { self.lock(); }
        idle
    }

    /// The last activity, while unlocked.
    pub fn last_activity(&self) -> Option<Moment> {
        self.open.as_ref().map(|open| open.last_activity)
    }

    /// Time left before the idle lock, while unlocked.
    pub fn idle_remaining(&self, now: Moment) -> Option<Duration> {
        self.open.as_ref().map(|open| IDLE_TIMEOUT.saturating_sub(now.since(open.last_activity)))
    }

    /// Every entry, sorted by origin then username. Empty while locked.
    pub fn entries(&self) -> &[EntrySummary] {
        self.open.as_ref().map_or(&[], |open| &open.entries)
    }

    /// Readable entries whose origin or username contains `query`, ignoring
    /// case. Passwords and notes are never searched.
    pub fn search(&self, query: &str) -> Vec<&EntrySummary> {
        let query = Zeroizing::new(query.trim().to_lowercase());
        self.entries().iter().filter(|entry| {
            if query.is_empty() { return true; }
            entry.readable() && (Zeroizing::new(entry.origin.to_lowercase()).contains(query.as_str()) || Zeroizing::new(entry.username.to_lowercase()).contains(query.as_str()))
        }).collect()
    }

    pub fn entry(&self, id: &str) -> Option<&EntrySummary> {
        self.entries().iter().find(|entry| entry.id == id)
    }

    fn unlocked(&self) -> Result<&Session, VaultError> {
        self.open.as_ref().ok_or(VaultError::Locked)
    }

    /// Validate and seal every field of an entry bound to `id`.
    fn seal_entry(&self, id: &str, input: &EntryInput<'_>) -> Result<(Zeroizing<String>, [Vec<u8>; 4], Option<Vec<u8>>), VaultError> {
        let cipher = &self.unlocked()?.cipher;
        let origin = Zeroizing::new(normalize_origin(input.origin)?);
        if input.username.len() > MAX_USERNAME_BYTES { return Err(VaultError::Invalid("That username is too long.")); }
        if input.password.is_empty() { return Err(VaultError::Invalid("Enter a password, or generate one.")); }
        if input.password.len() > MAX_PASSWORD_BYTES { return Err(VaultError::Invalid("That password is too long.")); }
        if input.notes.len() > MAX_NOTES_BYTES { return Err(VaultError::Invalid("Those notes are too long.")); }
        let sealed = [
            crypto::seal(cipher, &self.binding(id, Field::Origin, ""), &origin)?,
            crypto::seal(cipher, &self.binding(id, Field::Username, &origin), input.username)?,
            crypto::seal(cipher, &self.binding(id, Field::Password, &origin), input.password)?,
            crypto::seal(cipher, &self.binding(id, Field::Notes, &origin), input.notes)?,
        ];
        let profile = input.profile.map(|profile| self.seal_profile(cipher, id, &origin, profile)).transpose()?;
        Ok((origin, sealed, profile))
    }

    fn seal_profile(&self, cipher: &VaultCipher, id: &str, origin: &str, profile: &str) -> Result<Vec<u8>, VaultError> {
        if profile.is_empty() || profile.len() > MAX_PROFILE_BYTES { return Err(VaultError::Invalid("That profile can't hold passwords.")); }
        crypto::seal(cipher, &self.binding(id, Field::Profile, origin), profile)
    }

    pub fn add(&mut self, input: &EntryInput<'_>) -> Result<String, VaultError> {
        let id = crate::new_id();
        let (origin, [o, u, p, n], profile) = self.seal_entry(&id, input)?;
        let now = crate::now_ms();
        if !self.db.insert(&id, now, &db::SealedEntry { origin: &o, username: &u, password: &p, notes: &n, profile: profile.as_deref() })? {
            return Err(self.unwritten(&id));
        }
        let open = self.open.as_mut().expect("sealed while unlocked");
        open.entries.push(EntrySummary {
            id: id.clone(), origin, username: Zeroizing::new(input.username.to_string()), profile: input.profile.map(str::to_owned),
            created_ms: now, updated_ms: now, used_ms: None, state: EntryState::Readable,
        });
        sort(&mut open.entries);
        Ok(id)
    }

    /// Replace every field, the profile tag included.
    pub fn update(&mut self, id: &str, input: &EntryInput<'_>) -> Result<(), VaultError> {
        let (origin, [o, u, p, n], profile) = self.seal_entry(id, input)?;
        let now = crate::now_ms();
        if !self.db.update(id, now, &db::SealedEntry { origin: &o, username: &u, password: &p, notes: &n, profile: profile.as_deref() })? {
            return Err(self.unwritten(id));
        }
        let open = self.open.as_mut().expect("sealed while unlocked");
        if let Some(entry) = open.entries.iter_mut().find(|entry| entry.id == id) {
            entry.origin = origin;
            entry.username = Zeroizing::new(input.username.to_string());
            entry.profile = input.profile.map(str::to_owned);
            entry.updated_ms = now;
            entry.state = EntryState::Readable;
        }
        sort(&mut open.entries);
        Ok(())
    }

    /// Put a readable entry, such as one from format 1, in `profile`.
    pub fn set_profile(&mut self, id: &str, profile: &str) -> Result<(), VaultError> {
        let open = self.unlocked()?;
        let entry = open.entries.iter().find(|entry| entry.id == id).ok_or(VaultError::NotFound)?;
        if !entry.readable() { return Err(VaultError::Unreadable); }
        let sealed = self.seal_profile(&open.cipher, id, &entry.origin, profile)?;
        if !self.db.set_profile(id, &sealed)? { return Err(self.unwritten(id)); }
        if let Some(entry) = self.open.as_mut().and_then(|open| open.entries.iter_mut().find(|entry| entry.id == id)) {
            entry.profile = Some(profile.to_owned());
        }
        Ok(())
    }

    /// Readable entries saved for exactly this origin in this profile, the
    /// only ones a page may be offered. Untagged entries never match.
    pub fn matching(&self, origin: &str, profile: &str) -> Vec<&EntrySummary> {
        self.entries().iter().filter(|entry| entry.readable() && entry.origin.as_str() == origin && entry.profile.as_deref() == Some(profile)).collect()
    }

    /// Record that an entry was filled into a page.
    pub fn mark_used(&mut self, id: &str, now_ms: u64) -> Result<(), VaultError> {
        self.unlocked()?;
        if !self.db.mark_used(id, now_ms)? { return Err(self.unwritten(id)); }
        if let Some(entry) = self.open.as_mut().and_then(|open| open.entries.iter_mut().find(|entry| entry.id == id)) {
            entry.used_ms = Some(now_ms);
        }
        Ok(())
    }

    /// The sites `profile` asked never to offer saving for. Readable while
    /// locked, since an offer is decided before any unlock.
    pub fn never_saved(&self, profile: &str) -> Result<Vec<String>, VaultError> {
        self.db.never_saved(profile)
    }

    pub fn is_never_saved(&self, profile: &str, origin: &str) -> Result<bool, VaultError> {
        self.db.is_never_saved(profile, origin)
    }

    /// Never offer saving for `origin` in `profile`. `origin` must already be
    /// a normalised web origin.
    pub fn never_save(&self, profile: &str, origin: &str) -> Result<(), VaultError> {
        if profile.is_empty() || profile.len() > MAX_PROFILE_BYTES || normalize_origin(origin).as_deref() != Ok(origin) {
            return Err(VaultError::Invalid("That site can't be added to the never-save list."));
        }
        self.db.never_save(profile, origin, crate::now_ms())
    }

    pub fn remove_never_save(&self, profile: &str, origin: &str) -> Result<(), VaultError> {
        self.db.remove_never_save(profile, origin)
    }

    pub fn delete(&mut self, id: &str) -> Result<(), VaultError> {
        let deleted = match self.unlocked()?.damaged_ids.get(id) {
            Some(row) => self.db.delete_row(row)?,
            None => self.db.delete(id)?,
        };
        if !deleted { return Err(self.unwritten(id)); }
        self.forget(id);
        Ok(())
    }

    fn forget(&mut self, id: &str) {
        if let Some(open) = &mut self.open {
            open.entries.retain(|entry| entry.id != id);
            open.damaged_ids.remove(id);
            open.addresses.retain(|saved| saved.id != id);
            open.damaged_addresses.remove(id);
            open.cards.retain(|saved| saved.id != id);
            open.damaged_cards.remove(id);
        }
    }

    /// Every address, readable or not, sorted by label. Empty while locked.
    pub fn addresses(&self) -> &[SavedAddress] {
        self.open.as_ref().map_or(&[], |open| &open.addresses)
    }

    /// Readable addresses saved in `profile`, the only ones its pages may be filled with.
    pub fn addresses_in(&self, profile: &str) -> Vec<&SavedAddress> {
        self.addresses().iter().filter(|saved| saved.readable() && saved.profile == profile).collect()
    }

    pub fn saved_address(&self, id: &str) -> Option<&SavedAddress> {
        self.addresses().iter().find(|saved| saved.id == id)
    }

    /// The number of saved addresses. Readable while locked; it isn't secret.
    pub fn address_count(&self) -> Result<usize, VaultError> {
        self.db.address_count()
    }

    fn seal_address(&self, id: &str, address: &Address) -> Result<Vec<u8>, VaultError> {
        let cipher = &self.unlocked()?.cipher;
        address.check()?;
        crypto::seal(cipher, &self.binding(id, Field::Address, ""), &address.encode())
    }

    /// Save a new address in `profile`.
    pub fn add_address(&mut self, profile: &str, address: &Address) -> Result<String, VaultError> {
        let id = crate::new_id();
        let sealed = self.seal_address(&id, address)?;
        if profile.is_empty() || profile.len() > MAX_PROFILE_BYTES { return Err(VaultError::Invalid("That profile can't hold addresses.")); }
        let tag = crypto::seal(&self.unlocked()?.cipher, &self.binding(&id, Field::AddressProfile, ""), profile)?;
        let now = crate::now_ms();
        if !self.db.insert_address(&id, now, &tag, &sealed)? { return Err(self.unwritten(&id)); }
        let open = self.open.as_mut().expect("sealed while unlocked");
        open.addresses.push(SavedAddress { id: id.clone(), profile: profile.to_owned(), address: address.clone(), created_ms: now, updated_ms: now, used_ms: None, state: EntryState::Readable });
        sort_addresses(&mut open.addresses);
        Ok(id)
    }

    /// Replace every part of a readable address. It stays in its profile.
    pub fn update_address(&mut self, id: &str, address: &Address) -> Result<(), VaultError> {
        let saved = self.unlocked()?.addresses.iter().find(|saved| saved.id == id).ok_or(VaultError::NotFound)?;
        if !saved.readable() { return Err(VaultError::Unreadable); }
        let sealed = self.seal_address(id, address)?;
        let now = crate::now_ms();
        if !self.db.update_address(id, now, &sealed)? { return Err(self.unwritten(id)); }
        let open = self.open.as_mut().expect("sealed while unlocked");
        if let Some(saved) = open.addresses.iter_mut().find(|saved| saved.id == id) {
            saved.address = address.clone();
            saved.updated_ms = now;
        }
        sort_addresses(&mut open.addresses);
        Ok(())
    }

    pub fn delete_address(&mut self, id: &str) -> Result<(), VaultError> {
        let deleted = match self.unlocked()?.damaged_addresses.get(id) {
            Some(row) => self.db.delete_address_row(row)?,
            None => self.db.delete_address(id)?,
        };
        if !deleted { return Err(self.unwritten(id)); }
        self.forget(id);
        Ok(())
    }

    /// Record that an address was filled into a page.
    pub fn mark_address_used(&mut self, id: &str, now_ms: u64) -> Result<(), VaultError> {
        self.unlocked()?;
        if !self.db.mark_address_used(id, now_ms)? { return Err(self.unwritten(id)); }
        if let Some(saved) = self.open.as_mut().and_then(|open| open.addresses.iter_mut().find(|saved| saved.id == id)) {
            saved.used_ms = Some(now_ms);
        }
        Ok(())
    }

    /// Every card, readable or not, sorted by nickname. Empty while locked.
    pub fn cards(&self) -> &[SavedCard] {
        self.open.as_ref().map_or(&[], |open| &open.cards)
    }

    /// Readable cards saved in `profile`, the only ones its pages may be filled with.
    pub fn cards_in(&self, profile: &str) -> Vec<&SavedCard> {
        self.cards().iter().filter(|saved| saved.readable() && saved.profile == profile).collect()
    }

    pub fn saved_card(&self, id: &str) -> Option<&SavedCard> {
        self.cards().iter().find(|saved| saved.id == id)
    }

    /// The number of saved cards. Readable while locked; it isn't secret.
    pub fn card_count(&self) -> Result<usize, VaultError> {
        self.db.card_count()
    }

    /// Save a new card in `profile`: its details, its number as digits and
    /// its profile, each sealed on its own. Only what the vault can't hold
    /// is refused; see `Card::hints` for what is only worth a second look.
    pub fn add_card(&mut self, profile: &str, card: &Card) -> Result<String, VaultError> {
        let cipher = &self.unlocked()?.cipher;
        card.check(false)?;
        if profile.is_empty() || profile.len() > MAX_PROFILE_BYTES { return Err(VaultError::Invalid("That profile can't hold cards.")); }
        let id = crate::new_id();
        let digits = card.digits();
        let details = Details::of(card, &digits);
        let sealed = crypto::seal(cipher, &self.binding(&id, Field::Card, ""), &details.encode())?;
        let number = crypto::seal(cipher, &self.binding(&id, Field::CardNumber, ""), &digits)?;
        let tag = crypto::seal(cipher, &self.binding(&id, Field::CardProfile, ""), profile)?;
        let now = crate::now_ms();
        if !self.db.insert_card(&id, now, &tag, &sealed, &number)? { return Err(self.unwritten(&id)); }
        let Details { name, nickname, exp_month, exp_year, network, last4 } = details;
        let open = self.open.as_mut().expect("sealed while unlocked");
        open.cards.push(SavedCard {
            id: id.clone(), profile: profile.to_owned(), name, nickname, exp_month, exp_year, network, last4, created_ms: now, updated_ms: now, used_ms: None, state: EntryState::Readable,
        });
        sort_cards(&mut open.cards);
        Ok(id)
    }

    /// Replace a readable card's name, expiry and nickname, and its number
    /// unless `card`'s is blank. It stays in its profile.
    pub fn update_card(&mut self, id: &str, card: &Card) -> Result<(), VaultError> {
        let open = self.unlocked()?;
        let saved = open.cards.iter().find(|saved| saved.id == id).ok_or(VaultError::NotFound)?;
        if !saved.readable() { return Err(VaultError::Unreadable); }
        card.check(true)?;
        let digits = card.digits();
        let mut details = Details::of(card, &digits);
        if digits.is_empty() { (details.network, details.last4) = (saved.network, saved.last4.clone()); }
        let sealed = crypto::seal(&open.cipher, &self.binding(id, Field::Card, ""), &details.encode())?;
        let number = (!digits.is_empty()).then(|| crypto::seal(&open.cipher, &self.binding(id, Field::CardNumber, ""), &digits)).transpose()?;
        let now = crate::now_ms();
        if !self.db.update_card(id, now, &sealed, number.as_deref())? { return Err(self.unwritten(id)); }
        let open = self.open.as_mut().expect("sealed while unlocked");
        if let Some(saved) = open.cards.iter_mut().find(|saved| saved.id == id) {
            let Details { name, nickname, exp_month, exp_year, network, last4 } = details;
            (saved.name, saved.nickname, saved.exp_month, saved.exp_year, saved.network, saved.last4) = (name, nickname, exp_month, exp_year, network, last4);
            saved.updated_ms = now;
        }
        sort_cards(&mut open.cards);
        Ok(())
    }

    pub fn delete_card(&mut self, id: &str) -> Result<(), VaultError> {
        let deleted = match self.unlocked()?.damaged_cards.get(id) {
            Some(row) => self.db.delete_card_row(row)?,
            None => self.db.delete_card(id)?,
        };
        if !deleted { return Err(self.unwritten(id)); }
        self.forget(id);
        Ok(())
    }

    /// Record that a card was filled into a page.
    pub fn mark_card_used(&mut self, id: &str, now_ms: u64) -> Result<(), VaultError> {
        self.unlocked()?;
        if !self.db.mark_card_used(id, now_ms)? { return Err(self.unwritten(id)); }
        if let Some(saved) = self.open.as_mut().and_then(|open| open.cards.iter_mut().find(|saved| saved.id == id)) {
            saved.used_ms = Some(now_ms);
        }
        Ok(())
    }

    /// Open one readable card's number, as digits, for a fill. The caller
    /// sends it once and drops it, which wipes it. A listed card whose row is
    /// gone may be a Reset in another window, reported as one.
    pub fn card_number(&self, id: &str) -> Result<Zeroizing<String>, VaultError> {
        let open = self.unlocked()?;
        let saved = open.cards.iter().find(|saved| saved.id == id).ok_or(VaultError::NotFound)?;
        if !saved.readable() { return Err(VaultError::Unreadable); }
        let Some(sealed) = self.db.card_number(id)? else {
            return Err(if self.db.file_vault_id()? == self.db.vault_id { VaultError::NotFound } else { VaultError::ResetElsewhere });
        };
        crypto::open(&open.cipher, &self.binding(id, Field::CardNumber, ""), &sealed).map_err(|_| VaultError::Unreadable)
    }

    /// A listed entry whose row is gone may be a Reset in another window,
    /// reported as one, as a write that finds it would be. The caller locks.
    fn open_secret(&self, id: &str, field: Field) -> Result<Zeroizing<String>, VaultError> {
        let open = self.unlocked()?;
        let entry = open.entries.iter().find(|entry| entry.id == id).ok_or(VaultError::NotFound)?;
        if !entry.readable() { return Err(VaultError::Unreadable); }
        let Some(sealed) = self.db.field(id, field)? else {
            return Err(if self.db.file_vault_id()? == self.db.vault_id { VaultError::NotFound } else { VaultError::ResetElsewhere });
        };
        crypto::open(&open.cipher, &self.binding(id, field, &entry.origin), &sealed).map_err(|_| VaultError::Unreadable)
    }

    /// Open one entry's password, for Reveal or Copy.
    pub fn password(&self, id: &str) -> Result<Zeroizing<String>, VaultError> {
        self.open_secret(id, Field::Password)
    }

    /// Open one entry's password and notes, for Edit.
    pub fn secret(&self, id: &str) -> Result<Secret, VaultError> {
        Ok(Secret { password: self.open_secret(id, Field::Password)?, notes: self.open_secret(id, Field::Notes)? })
    }

    /// The number of stored entries. Readable while locked; it isn't secret.
    pub fn count(&self) -> Result<usize, VaultError> {
        self.db.count()
    }

    /// Delete every entry, address and card, the never-save list and the check value and give
    /// this file a new vault id, leaving a new, empty vault whose first unlock
    /// makes a new key. No key is deleted: a byte copy of this file, such as a
    /// backup, shares the old vault id and still needs the old key. Ok only
    /// once the file was reopened and checked; Damaged means the reset left
    /// damage behind, and the file must be moved aside with
    /// `replace_damaged_file`.
    pub fn reset(&mut self) -> Result<(), VaultError> {
        self.lock();
        self.db.reset()
    }

    /// Move a vault file that can't be opened or reset aside, as
    /// `vault-damaged-<UTC time>.sqlite` in the same folder with its WAL and
    /// journal, readable only by this user, so the next open starts a new
    /// vault. Nothing is deleted: the old file can still be examined, with
    /// the key Reset keeps. Returns the name it was given. Close every handle
    /// on the file first.
    pub fn replace_damaged_file(data_dir: &Path) -> Result<String, VaultError> {
        undo_stopped_swap(data_dir)?;
        if std::fs::symlink_metadata(data_dir.join(FILE_NAME)).is_err() { return Err(VaultError::Storage); }
        let name = damaged_name(data_dir)?;
        // The WAL and journal go first and the shared memory is dropped, so a
        // new file never replays them; the shared memory only indexes the WAL.
        for suffix in ["-wal", "-journal", "-shm", ""] {
            let from = data_dir.join(format!("{FILE_NAME}{suffix}"));
            let moved = if suffix == "-shm" { std::fs::remove_file(&from) } else { std::fs::rename(&from, data_dir.join(format!("{name}{suffix}"))) };
            match moved {
                Ok(()) if suffix != "-shm" => private(&data_dir.join(format!("{name}{suffix}"))),
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && !suffix.is_empty() => {}
                Err(_) => return Err(VaultError::Storage),
            }
        }
        Ok(name)
    }

    /// Whether Recover Passwords can run on the damaged vault file in
    /// `data_dir`: its vault id can still be read and `keys` holds its key.
    pub fn can_recover(data_dir: &Path, keys: &dyn VaultKeyStore) -> bool {
        db::salvage_vault_id(&data_dir.join(FILE_NAME)).is_some_and(|id| keys.load(&id).is_ok_and(|key| key.is_some()))
    }

    /// Recover Passwords, for a vault file that is damaged while its key is
    /// available. The damaged file is opened read-only and read every way
    /// `db::salvage` reads it; every entry, address and card whose sealed cells all
    /// open with the key, bound to their vault, row, field and origin, is
    /// copied as stored, with every never-save row any read returned, into a
    /// new file with the same vault id and check, which is checked, flushed to
    /// the disk and read back. The new file then replaces the damaged one in one
    /// rename, and the damaged one, with its WAL and journal, is kept as
    /// `replace_damaged_file` names it. No key is created or deleted. Close
    /// every handle on the file first.
    pub fn recover_damaged_file(data_dir: &Path, keys: &dyn VaultKeyStore) -> Result<Recovery, VaultError> {
        undo_stopped_swap(data_dir)?;
        let also_kept = [Vault::kept_from_stopped_recover(data_dir), Vault::set_aside_from_stopped_recover(data_dir)].concat();
        let salvage = db::salvage(&data_dir.join(FILE_NAME))?;
        let key = keys.load(&salvage.vault_id).map_err(VaultError::KeyStore)?.ok_or(VaultError::MissingKey)?;
        let cipher = key.cipher();
        let vault_id = salvage.vault_id.as_str();
        let check_binding = Binding { vault_id, entry_id: "", field: Field::Check, origin: "" };
        let check_opens = |check: &[u8]| crypto::open(&cipher, &check_binding, check).is_ok_and(|opened| opened.as_str() == CHECK_PLAINTEXT);
        // The first version of each entry, address and card that verifies, in rowid order.
        let mut ids = HashSet::new();
        let verified: Vec<&db::SalvagedRow> = salvage.entries.rows.iter().filter_map(|seen| seen.cells.as_ref())
            .filter(|row| verifies(&cipher, vault_id, row) && ids.insert(row.id.as_str())).collect();
        let (found, complete) = rows_seen(&salvage.entries);
        let mut address_ids = HashSet::new();
        let addresses: Vec<&db::SalvagedAddress> = salvage.addresses.rows.iter().filter_map(|seen| seen.cells.as_ref())
            .filter(|row| address_verifies(&cipher, vault_id, row) && address_ids.insert(row.id.as_str())).collect();
        let (found_addresses, addresses_complete) = rows_seen(&salvage.addresses);
        let mut card_ids = HashSet::new();
        let cards: Vec<&db::SalvagedCard> = salvage.cards.rows.iter().filter_map(|seen| seen.cells.as_ref())
            .filter(|row| card_verifies(&cipher, vault_id, row) && card_ids.insert(row.id.as_str())).collect();
        let (found_cards, cards_complete) = rows_seen(&salvage.cards);
        let check = match &salvage.check {
            Some(check) if check_opens(check) => check.clone(),
            // A key that opens neither the check nor any row isn't this vault's.
            _ if verified.is_empty() && addresses.is_empty() && cards.is_empty() && found + found_addresses + found_cards > 0 => return Err(VaultError::KeyMismatch),
            _ => crypto::seal(&cipher, &check_binding, CHECK_PLAINTEXT)?,
        };
        let fresh = data_dir.join(RECOVERY_FILE);
        let built = db::write_recovered(&fresh, vault_id, &check, &verified, &addresses, &cards, &salvage.never_save.rows).and_then(|()| {
            // Read back as the vault will read it: every entry, address and
            // card opens, and every read sees exactly the rows written.
            let written = db::salvage(&fresh)?;
            let (mut opened, mut opened_addresses, mut opened_cards) = (HashSet::new(), HashSet::new(), HashSet::new());
            let whole = written.vault_id == vault_id && written.check.as_deref().is_some_and(check_opens)
                && written.entries.rows.iter().filter_map(|seen| seen.cells.as_ref()).all(|row| verifies(&cipher, vault_id, row) && { opened.insert(row.id.as_str()); true })
                && opened == ids && rows_seen(&written.entries) == (ids.len(), true)
                && written.addresses.rows.iter().filter_map(|seen| seen.cells.as_ref()).all(|row| address_verifies(&cipher, vault_id, row) && { opened_addresses.insert(row.id.as_str()); true })
                && opened_addresses == address_ids && rows_seen(&written.addresses) == (address_ids.len(), true)
                && written.cards.rows.iter().filter_map(|seen| seen.cells.as_ref()).all(|row| card_verifies(&cipher, vault_id, row) && { opened_cards.insert(row.id.as_str()); true })
                && opened_cards == card_ids && rows_seen(&written.cards) == (card_ids.len(), true)
                && written.never_save.complete && written.never_save.rows == salvage.never_save.rows;
            if whole { Ok(()) } else { Err(VaultError::Damaged) }
        });
        let moved = built.and_then(|()| swap_in(data_dir, &fresh));
        // An undo that couldn't finish names the working file with the files it kept.
        if moved.as_ref().is_err_and(|error| !matches!(error, VaultError::UndoIncomplete(_) | VaultError::UnrecordedStop(_))) { remove_working_file(&fresh); }
        let never_save = Counted { recovered: salvage.never_save.rows.len(), found: salvage.never_save.found, complete: salvage.never_save.complete };
        let addresses = Counted { recovered: addresses.len(), found: found_addresses, complete: addresses_complete || salvage.addresses.never_existed };
        let cards = Counted { recovered: cards.len(), found: found_cards, complete: cards_complete || salvage.cards.never_existed };
        Ok(Recovery { recovered: verified.len(), found, complete, addresses, cards, never_save, moved: moved?, also_kept })
    }

    #[cfg(test)]
    fn db(&self) -> &Db {
        &self.db
    }
}

/// Where Recover Passwords builds the new vault file before it replaces the damaged one.
pub const RECOVERY_FILE: &str = "vault-recovery.sqlite";

/// Beside a kept name while Recover Passwords replaces the vault file: its
/// `SwapRecord`, until the Recover finishes or is undone.
const RECORD: &str = "-swap";

/// The first `vault-damaged-<UTC time>[-n].sqlite` in `data_dir` that no
/// file, WAL, journal or record has.
fn damaged_name(data_dir: &Path) -> Result<String, VaultError> {
    let stamp = utc_stamp(crate::now_ms())?;
    let taken = |name: &str| ["", "-wal", "-journal", RECORD].iter().any(|suffix| std::fs::symlink_metadata(data_dir.join(format!("{name}{suffix}"))).is_ok());
    Ok((1..).map(|n| if n == 1 { format!("vault-damaged-{stamp}.sqlite") } else { format!("vault-damaged-{stamp}-{n}.sqlite") })
        .find(|name| !taken(name)).expect("a free name"))
}

/// How many distinct rows the reads of one table of a damaged file saw, and
/// whether one read went to its end having seen every one of them. Every row
/// a read returned is counted, none left out but those the reads skip as
/// no row (a rowid below 1 and no cell that reads): by its id if that
/// reads, else by an id another read gave its rowid, else by its rowid.
/// Two ids are two rows, even at one rowid, as damage to an index cell can
/// put one row's id at another's rowid; a damaged id can so be counted as
/// one more row, never one row fewer. A read that lacks a row another read
/// saw isn't complete, however far it went.
fn rows_seen<T>(table: &db::Table<T>) -> (usize, bool) {
    let mut named_at = HashMap::new();
    for seen in &table.rows {
        if let Some(id) = seen.id.as_deref() { named_at.entry(seen.rowid).or_insert(id); }
    }
    let row = |rowid: i64, id: Option<&str>| -> Result<String, i64> { id.or_else(|| named_at.get(&rowid).copied()).map(str::to_owned).ok_or(rowid) };
    let seen: HashSet<_> = table.rows.iter().map(|seen| row(seen.rowid, seen.id.as_deref())).collect();
    let complete = table.reads.iter().any(|(read, whole)| *whole && read.iter().map(|(rowid, id)| row(*rowid, id.as_deref())).collect::<HashSet<_>>() == seen);
    (seen.len(), complete)
}

/// Whether every sealed cell of a salvaged row opens with `cipher`, bound to
/// this vault, the row's id and its origin, as an unlock and a reveal open them.
fn verifies(cipher: &VaultCipher, vault_id: &str, row: &db::SalvagedRow) -> bool {
    let binding = |field, origin| Binding { vault_id, entry_id: &row.id, field, origin };
    let Ok(origin) = crypto::open(cipher, &binding(Field::Origin, ""), &row.origin) else { return false };
    let opens = |field, sealed: &[u8]| crypto::open(cipher, &binding(field, &origin), sealed).is_ok();
    opens(Field::Username, &row.username) && opens(Field::Password, &row.password) && opens(Field::Notes, &row.notes)
        && row.profile.as_deref().is_none_or(|profile| opens(Field::Profile, profile))
}

/// Whether a salvaged address's profile tag and address both open with
/// `cipher`, bound to this vault and the row's id, and the address reads as
/// an unlock reads it.
fn address_verifies(cipher: &VaultCipher, vault_id: &str, row: &db::SalvagedAddress) -> bool {
    let binding = |field| Binding { vault_id, entry_id: &row.id, field, origin: "" };
    crypto::open(cipher, &binding(Field::AddressProfile), &row.profile).is_ok()
        && crypto::open(cipher, &binding(Field::Address), &row.address).is_ok_and(|opened| Address::decode(&opened).is_some())
}

/// Whether a salvaged card opens as an unlock opens it.
fn card_verifies(cipher: &VaultCipher, vault_id: &str, row: &db::SalvagedCard) -> bool {
    open_card(cipher, vault_id, &row.id, &row.profile, &row.card, &row.number).is_some()
}

/// A card's profile and details, if its profile tag, details and number
/// all open with `cipher`, bound to this vault and the card's id, and read
/// as the vault writes them: the number 8 to 19 digits ending in the last
/// four its details name. The number is wiped before this returns.
fn open_card(cipher: &VaultCipher, vault_id: &str, id: &str, profile: &[u8], card: &[u8], number: &[u8]) -> Option<(Zeroizing<String>, Details)> {
    let binding = |field| Binding { vault_id, entry_id: id, field, origin: "" };
    let profile = crypto::open(cipher, &binding(Field::CardProfile), profile).ok().filter(|profile| !profile.is_empty() && profile.len() <= MAX_PROFILE_BYTES)?;
    let details = Details::decode(&crypto::open(cipher, &binding(Field::Card), card).ok()?)?;
    let number = crypto::open(cipher, &binding(Field::CardNumber), number).ok()?;
    let holds = (MIN_CARD_DIGITS..=MAX_CARD_DIGITS).contains(&number.len()) && number.bytes().all(|b| b.is_ascii_digit()) && number.ends_with(details.last4.as_str());
    holds.then_some((profile, details))
}

/// Put the finished file at `fresh` in the damaged vault file's place. A
/// record of the damaged file, of its WAL and journal and of `fresh` is
/// written first, beside the name it will be kept as; then the file is
/// linked under that name, its WAL and journal moved there and its shared
/// memory dropped, so neither is ever replayed into the new file; then one
/// rename replaces it, and the record goes. An error undoes those steps, so
/// a failed Recover leaves no second name of the damaged file. A stop
/// leaves them for the next open, Recover or Reset to undo, or to find
/// finished, by which file the vault file is then (see `undo_stopped_swap`).
/// The name it was kept as.
fn swap_in(data_dir: &Path, fresh: &Path) -> Result<String, VaultError> {
    let main = data_dir.join(FILE_NAME);
    let name = damaged_name(data_dir)?;
    let record = data_dir.join(format!("{name}{RECORD}"));
    if write_record(data_dir, &record, fresh).is_err() {
        let _ = std::fs::remove_file(&record);
        return Err(VaultError::Storage);
    }
    crash_after(1);
    let swapped = std::fs::hard_link(&main, data_dir.join(&name)).map_err(|_| VaultError::Storage).and_then(|()| {
        private(&data_dir.join(&name));
        crash_after(2);
        [(3, "-wal"), (4, "-journal"), (5, "-shm")].into_iter().try_for_each(|(step, suffix)| {
            let from = data_dir.join(format!("{FILE_NAME}{suffix}"));
            let moved = if suffix == "-shm" { std::fs::remove_file(&from) } else { std::fs::rename(&from, data_dir.join(format!("{name}{suffix}"))) };
            match moved {
                Ok(()) if suffix != "-shm" => private(&data_dir.join(format!("{name}{suffix}"))),
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(VaultError::Storage),
            }
            crash_after(step);
            Ok(())
        })
    }).and_then(|()| std::fs::rename(fresh, &main).map_err(|_| VaultError::Storage));
    if let Err(error) = swapped {
        undo_stopped_swap(data_dir)?;
        return Err(error);
    }
    crash_after(6);
    sync_folder(data_dir);
    let _ = std::fs::remove_file(&record);
    Ok(name)
}

/// What an undo needs to put a stopped Recover's moves back only beside the
/// vault file they came from, unchanged since, and to tell a Recover that
/// finished: that file, and its WAL and journal, as they were before
/// anything moved; the new file that replaces it, as written; and whether
/// the vault file has been found to be that new file. A round 9 record has
/// neither of the last two. Open Without Them marks a stop it ends set
/// aside, writing a record for one that had none (round 12).
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
struct SwapRecord {
    file: Identity,
    wal: Option<Identity>,
    journal: Option<Identity>,
    recovered: Option<Identity>,
    #[serde(default)]
    finished: bool,
    #[serde(default)]
    set_aside: bool,
}

/// A file as a record saw it: its device and inode, kept in the record only,
/// and its length and SHA-256, which decide (see `same_bytes`). The default,
/// for a set-aside record of no vault file, has no bytes any file has.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    len: u64,
    sha256: String,
}

impl Identity {
    fn of(path: &Path) -> std::io::Result<Identity> {
        use sha2::{Digest, Sha256};
        let meta = std::fs::symlink_metadata(path)?;
        #[cfg(unix)]
        let (dev, ino) = {
            use std::os::unix::fs::MetadataExt;
            (meta.dev(), meta.ino())
        };
        #[cfg(not(unix))]
        let (dev, ino) = (0, 0);
        let bytes = std::fs::read(path)?;
        Ok(Identity { dev, ino, len: meta.len(), sha256: data_encoding::HEXLOWER.encode(&Sha256::digest(&bytes)) })
    }

    /// Whether `other` holds the same bytes: a file changed since differs,
    /// and a copy doesn't. Two files with the same bytes are the same SQLite
    /// state, so a stop places the same in a data folder copied or restored
    /// file by file, which gives every file a new inode (round 12, R10-1).
    fn same_bytes(&self, other: &Identity) -> bool {
        self.len == other.len && self.sha256 == other.sha256
    }
}

/// Write the record of the vault file in `data_dir`, of its WAL and journal
/// and of the new file at `fresh`, at `path`, readable only by this user,
/// and flush it to the disk before anything moves.
fn write_record(data_dir: &Path, path: &Path, fresh: &Path) -> std::io::Result<()> {
    let beside = |suffix: &str| match Identity::of(&data_dir.join(format!("{FILE_NAME}{suffix}"))) {
        Ok(identity) => Ok(Some(identity)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    };
    let record = SwapRecord { file: Identity::of(&data_dir.join(FILE_NAME))?, wal: beside("-wal")?, journal: beside("-journal")?, recovered: Some(Identity::of(fresh)?), finished: false, set_aside: false };
    write_flushed(data_dir, path, &serde_json::to_vec(&record)?, true)
}

/// Mark a Recover's record finished (see `rewrite_record`).
fn mark_finished(data_dir: &Path, name: &str, mut record: SwapRecord) -> std::io::Result<()> {
    record.finished = true;
    rewrite_record(data_dir, name, &record)
}

/// Replace the record beside `name` with `record`: written beside it,
/// flushed, then renamed over it, so a stop leaves one record or the other
/// whole.
fn rewrite_record(data_dir: &Path, name: &str, record: &SwapRecord) -> std::io::Result<()> {
    let next = data_dir.join(format!("{name}{RECORD}{NEXT}"));
    write_flushed(data_dir, &next, &serde_json::to_vec(record)?, false)?;
    crash_after(20);
    std::fs::rename(&next, data_dir.join(format!("{name}{RECORD}")))?;
    sync_folder(data_dir);
    Ok(())
}

/// The record beside `name`, if one reads.
fn read_record(data_dir: &Path, name: &str) -> Option<SwapRecord> {
    std::fs::read(data_dir.join(format!("{name}{RECORD}"))).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// A stop's record goes, with what a replacement or a set aside left beside it.
fn drop_record(data_dir: &Path, name: &str) {
    for suffix in [RECORD.to_owned(), format!("{RECORD}{NEXT}"), COPY.to_owned()] { let _ = std::fs::remove_file(data_dir.join(format!("{name}{suffix}"))); }
}

/// Beside a record while `rewrite_record` replaces it.
const NEXT: &str = "-next";

/// Beside a set-aside kept name while `settle_set_aside` makes it a copy of its own.
const COPY: &str = "-copy";

/// Whether `name` exists in `data_dir`, as any kind of file.
fn exists(data_dir: &Path, name: &str) -> bool {
    std::fs::symlink_metadata(data_dir.join(name)).is_ok()
}

/// Whether the file at `path` holds the bytes `recorded` describes.
fn has_bytes(path: &Path, recorded: &Identity) -> bool {
    Identity::of(path).is_ok_and(|now| now.same_bytes(recorded))
}

/// Write `bytes` to a new file at `path` (or over one, unless `new`),
/// readable only by this user, flushed to the disk with its folder.
fn write_flushed(data_dir: &Path, path: &Path, bytes: &[u8], new: bool) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(new).create(!new).truncate(!new).open(path)?;
    private(path);
    file.write_all(bytes)?;
    file.sync_all()?;
    sync_folder(data_dir);
    Ok(())
}

/// Renames reach the disk with their folder. Best effort: not every file system syncs a folder.
fn sync_folder(data_dir: &Path) {
    if let Ok(folder) = std::fs::File::open(data_dir) { let _ = folder.sync_all(); }
}

/// Each Recover Passwords in `data_dir` that may have stopped part way, by
/// the name it kept or would have kept the vault file as: one with a record,
/// or a second name or a byte copy of the vault file itself (see
/// `copy_of_vault`).
fn stopped_swaps(data_dir: &Path) -> Vec<String> {
    let Ok(files) = std::fs::read_dir(data_dir) else { return Vec::new() };
    let present: BTreeSet<String> = files.flatten().filter_map(|file| file.file_name().into_string().ok()).collect();
    let kept_name = |name: &str| name.starts_with("vault-damaged-") && name.ends_with(".sqlite");
    present.iter().filter_map(|name| match name.strip_suffix(RECORD) {
        Some(name) => Some(name),
        None => (kept_name(name) && (linked(data_dir, name) || copy_of_vault(data_dir, name))).then_some(name.as_str()),
    }).filter(|name| kept_name(name)).collect::<BTreeSet<_>>().into_iter().map(str::to_owned).collect()
}

/// Whether `name` in `data_dir` is a second name of the vault file.
fn linked(data_dir: &Path, name: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(main) = std::fs::metadata(data_dir.join(FILE_NAME)) else { return false };
        std::fs::symlink_metadata(data_dir.join(name)).is_ok_and(|kept| kept.is_file() && (kept.dev(), kept.ino()) == (main.dev(), main.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = (data_dir, name);
        false
    }
}

/// Whether `name` in `data_dir`, a file of the vault file's length with a
/// WAL or journal kept beside it, holds the vault file's bytes: a stop with
/// no record, as a build before round 9 left one, in a data folder copied
/// file by file, where the kept name is no longer a second name of the
/// vault file (round 12). Only such a name is read.
fn copy_of_vault(data_dir: &Path, name: &str) -> bool {
    let length = |name: &str| std::fs::symlink_metadata(data_dir.join(name)).ok().filter(|meta| meta.is_file()).map(|meta| meta.len());
    ["-wal", "-journal"].iter().any(|suffix| exists(data_dir, &format!("{name}{suffix}")))
        && length(name).is_some() && length(name) == length(FILE_NAME)
        && Identity::of(&data_dir.join(FILE_NAME)).is_ok_and(|main| has_bytes(&data_dir.join(name), &main))
}

/// The files kept under `name`: the file, its WAL and its journal, those present.
fn kept_files(data_dir: &Path, name: &str) -> Vec<String> {
    ["", "-wal", "-journal"].map(|suffix| format!("{name}{suffix}")).into_iter().filter(|kept| exists(data_dir, kept)).collect()
}

/// The files a stop moved, as its record says, that are now neither beside
/// the vault file as recorded nor under the kept name, such as deleted in
/// Finder (R10-2).
fn lost_files(data_dir: &Path, name: &str, record: &SwapRecord) -> Vec<String> {
    [("-wal", &record.wal), ("-journal", &record.journal)].into_iter()
        .filter(|(suffix, recorded)| recorded.as_ref().is_some_and(|recorded| !exists(data_dir, &format!("{name}{suffix}")) && !has_bytes(&data_dir.join(format!("{FILE_NAME}{suffix}")), recorded)))
        .map(|(suffix, _)| format!("{name}{suffix}")).collect()
}

/// Where a Recover Passwords that may have stopped under a kept name stands,
/// judged by which file the vault file is now, by its bytes, never by which
/// names exist: a user may remove a kept name in Finder, or copy the folder.
enum Stand {
    /// Open Without Them ended it: its files are never put back.
    SetAside,
    /// The vault file holds the very bytes the record describes, unchanged, so
    /// the Recover didn't finish, whatever happened to its kept name; or, with
    /// no record that reads, the kept name is a second name or a byte copy
    /// of the vault file, as an older build's stop leaves it. To be undone,
    /// though without a record a moved WAL or journal never goes back.
    Unfinished(Option<SwapRecord>),
    /// The vault file holds the new file's bytes the record describes, or
    /// was found to: the Recover finished, and its kept files hold the
    /// damaged file as it was.
    Finished(SwapRecord),
    /// Anything else, such as a vault file replaced, written or removed
    /// since, or a record that doesn't read beside no second name or copy:
    /// the vault stays closed while the stop's files exist.
    Unknown(Option<SwapRecord>),
}

/// Only the vault file is read here, and only when a record must be
/// compared with it.
fn stand(data_dir: &Path, name: &str) -> Stand {
    let main = std::cell::OnceCell::new();
    let holds = |recorded: Option<&Identity>| recorded.is_some_and(|recorded| {
        main.get_or_init(|| Identity::of(&data_dir.join(FILE_NAME)).ok()).as_ref().is_some_and(|main: &Identity| main.same_bytes(recorded))
    });
    match read_record(data_dir, name) {
        Some(record) if record.set_aside => Stand::SetAside,
        Some(record) if record.finished => Stand::Finished(record),
        Some(record) if holds(Some(&record.file)) => Stand::Unfinished(Some(record)),
        Some(record) if holds(record.recovered.as_ref()) => Stand::Finished(record),
        Some(record) => Stand::Unknown(Some(record)),
        None if linked(data_dir, name) || copy_of_vault(data_dir, name) => Stand::Unfinished(None),
        None => Stand::Unknown(None),
    }
}

/// Where `place` left one stopped Recover.
enum Placed {
    /// Undone, found finished, finished from its working file, or set
    /// aside: nothing of it keeps the vault closed.
    Done,
    /// Its moves can't go back, or nothing shows they belong back: every
    /// file it left, by name, and whether it left no record at all.
    Stuck(Vec<String>, bool),
    /// It moved the vault file's WAL or journal, and they are gone: by name.
    Gone(Vec<String>),
}

/// Undo a Recover Passwords that stopped, or failed, part way, or find that
/// one finished, by which file the vault file is (see `Stand`), for each
/// one (see `place`). Recover's working file, which holds every entry it
/// recovered sealed under the vault key, is named with the files of one
/// that can't go back while the vault stays closed, and goes once nothing
/// is left to undo. The error is `UnrecordedStop` when an older build's
/// stop with no record moved a WAL or journal, which nothing can show
/// belongs back; `UndoIncomplete` for any other that can't go back; and
/// `KeptFilesGone` when the only stops left moved files that are gone.
fn undo_stopped_swap(data_dir: &Path) -> Result<(), VaultError> {
    let (mut stuck, mut gone, mut unrecorded) = (Vec::new(), Vec::new(), false);
    for name in stopped_swaps(data_dir) {
        match place(data_dir, &name)? {
            Placed::Done => {}
            Placed::Stuck(files, none) => {
                stuck.extend(files);
                unrecorded |= none;
            }
            Placed::Gone(files) => gone.extend(files),
        }
    }
    let working = data_dir.join(RECOVERY_FILE);
    if stuck.is_empty() && gone.is_empty() {
        remove_working_file(&working);
        return Ok(());
    }
    if stuck.is_empty() { return Err(VaultError::KeptFilesGone(gone)); }
    if exists(data_dir, RECOVERY_FILE) { stuck.push(RECOVERY_FILE.to_owned()); }
    Err(if unrecorded { VaultError::UnrecordedStop(stuck) } else { VaultError::UndoIncomplete(stuck) })
}

/// One stopped Recover, by where it stands. Undone, the WAL and journal
/// moved under the kept name go back beside the vault file only if it has
/// none of its own by now and they hold the recorded bytes; then the kept
/// name, if it is still a second name or a copy of it, and the record go,
/// and the vault file is as it was before the Recover. One whose moved WAL
/// or journal is gone is finished from its working file if it can be (see
/// `finish_swap`), else `Gone`: the vault file lacks what they held. A
/// finished one's record is marked so, and stays while its kept files do,
/// so they are named (see `Vault::kept_from_stopped_recover`). Anything
/// else, or a move that fails, leaves every file where it is, `Stuck`: the
/// vault file must not be opened, reset or recovered without its WAL, nor
/// that WAL replayed over newer writes. A record whose files are all gone
/// goes, unless it moved a WAL or journal that went with them. A set-aside
/// one is settled (see `settle_set_aside`).
fn place(data_dir: &Path, name: &str) -> Result<Placed, VaultError> {
    let exists = |name: &str| exists(data_dir, name);
    let kept = kept_files(data_dir, name);
    let unless_lost = |found: Option<&SwapRecord>| {
        let lost = found.map(|found| lost_files(data_dir, name, found)).unwrap_or_default();
        if lost.is_empty() { drop_record(data_dir, name); Placed::Done } else { Placed::Gone(lost) }
    };
    Ok(match stand(data_dir, name) {
        Stand::SetAside => {
            settle_set_aside(data_dir, name)?;
            Placed::Done
        }
        Stand::Finished(_) if kept.is_empty() => {
            drop_record(data_dir, name);
            Placed::Done
        }
        Stand::Finished(found) => {
            if !found.finished { mark_finished(data_dir, name, found).map_err(|_| VaultError::Storage)?; }
            Placed::Done
        }
        Stand::Unfinished(Some(found)) if !lost_files(data_dir, name, &found).is_empty() => {
            let lost = lost_files(data_dir, name, &found);
            if finish_swap(data_dir, name, &found) { Placed::Done } else { Placed::Gone(lost) }
        }
        Stand::Unfinished(found) => {
            let moved: Vec<&str> = ["-wal", "-journal"].into_iter().filter(|suffix| exists(&format!("{name}{suffix}"))).collect();
            // A kept name that is neither the vault file's own nor a copy
            // of the recorded file's bytes is no copy this Recover made.
            let ours = !exists(name) || linked(data_dir, name) || found.as_ref().is_some_and(|found| has_bytes(&data_dir.join(name), &found.file));
            let recorded = |suffix: &str| found.as_ref().and_then(|found| if suffix == "-wal" { found.wal.as_ref() } else { found.journal.as_ref() });
            let unchanged = ours && moved.iter().all(|suffix| !exists(&format!("{FILE_NAME}{suffix}"))
                && recorded(suffix).is_some_and(|recorded| has_bytes(&data_dir.join(format!("{name}{suffix}")), recorded)));
            let back = unchanged && moved.iter().all(|suffix| std::fs::rename(data_dir.join(format!("{name}{suffix}")), data_dir.join(format!("{FILE_NAME}{suffix}"))).is_ok());
            if back && (!exists(name) || std::fs::remove_file(data_dir.join(name)).is_ok()) {
                drop_record(data_dir, name);
                Placed::Done
            } else {
                // With no record at all, nothing will ever show the moved files belong back.
                let unrecorded = found.is_none() && !exists(&format!("{name}{RECORD}")) && !moved.is_empty();
                Placed::Stuck(kept_files(data_dir, name), unrecorded)
            }
        }
        Stand::Unknown(found) if kept.is_empty() => unless_lost(found.as_ref()),
        Stand::Unknown(_) => Placed::Stuck(kept, false),
    })
}

/// Finish a stopped Recover whose moved WAL or journal is gone from its
/// working file (R10-2): only while that file holds the bytes the record
/// took of it once it was checked and read back, read again just before it
/// goes in. The steps the stop left are done as `swap_in` does them: the
/// vault file linked under the kept name if that is gone, its WAL and
/// journal moved there if they are still beside it as recorded, its shared
/// memory dropped; then the working file replaces it, and the record is
/// marked finished. A stop at any point leaves what the next open places
/// the same way. False when it can't, with every file where those steps
/// left it.
fn finish_swap(data_dir: &Path, name: &str, record: &SwapRecord) -> bool {
    let (main, fresh, kept) = (data_dir.join(FILE_NAME), data_dir.join(RECOVERY_FILE), data_dir.join(name));
    let ready = || record.recovered.as_ref().is_some_and(|recorded| has_bytes(&fresh, recorded));
    if !ready() { return false; }
    if !exists(data_dir, name) {
        if std::fs::hard_link(&main, &kept).is_err() { return false; }
        private(&kept);
    }
    crash_after(11);
    for (suffix, recorded) in [("-wal", &record.wal), ("-journal", &record.journal)] {
        let from = data_dir.join(format!("{FILE_NAME}{suffix}"));
        if !exists(data_dir, &format!("{FILE_NAME}{suffix}")) { continue; }
        // Only the file recorded goes, and never over one kept there.
        let to = data_dir.join(format!("{name}{suffix}"));
        if !recorded.as_ref().is_some_and(|recorded| has_bytes(&from, recorded)) || to.exists() || std::fs::rename(&from, &to).is_err() { return false; }
        private(&to);
    }
    match std::fs::remove_file(data_dir.join(format!("{FILE_NAME}-shm"))) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return false,
        _ => {}
    }
    crash_after(12);
    if !ready() || std::fs::rename(&fresh, &main).is_err() { return false; }
    crash_after(13);
    sync_folder(data_dir);
    // Should this fail, the vault file now holds the new file's bytes, which says the same.
    let _ = mark_finished(data_dir, name, record.clone());
    true
}

/// Open Without Them for one stop: its record, or a new one where none
/// reads, says it is set aside, written beside the old one and renamed over
/// it (see `rewrite_record`), so a stop leaves the old record or the new
/// one whole, and the vault never opens while the record still says put
/// back; then its files are settled (see `settle_set_aside`).
fn set_aside(data_dir: &Path, name: &str) -> Result<(), VaultError> {
    let mut record = read_record(data_dir, name).unwrap_or_else(|| SwapRecord {
        file: Identity::of(&data_dir.join(FILE_NAME)).unwrap_or_default(), wal: None, journal: None, recovered: None, finished: false, set_aside: false,
    });
    record.set_aside = true;
    rewrite_record(data_dir, name, &record).map_err(|_| VaultError::Storage)?;
    crash_after(21);
    settle_set_aside(data_dir, name)
}

/// Make a set-aside stop's files safe beside an open vault, at Open Without
/// Them and at each open after it, so one stopped part way is settled by
/// the next. A second name of the vault file would change with every write
/// to the vault: while a WAL or journal is kept beside it, it becomes a
/// copy of its own, written beside it and renamed over it; with nothing
/// kept beside it, it holds nothing the vault file doesn't, and goes. The
/// record goes once none of its files is left.
fn settle_set_aside(data_dir: &Path, name: &str) -> Result<(), VaultError> {
    let copy = data_dir.join(format!("{name}{COPY}"));
    let _ = std::fs::remove_file(&copy);
    if linked(data_dir, name) {
        let kept = data_dir.join(name);
        if ["-wal", "-journal"].iter().any(|suffix| exists(data_dir, &format!("{name}{suffix}"))) {
            let bytes = std::fs::read(&kept).map_err(|_| VaultError::Storage)?;
            write_flushed(data_dir, &copy, &bytes, true).map_err(|_| VaultError::Storage)?;
            crash_after(22);
            std::fs::rename(&copy, &kept).map_err(|_| VaultError::Storage)?;
            sync_folder(data_dir);
        } else {
            std::fs::remove_file(&kept).map_err(|_| VaultError::Storage)?;
        }
    }
    if kept_files(data_dir, name).is_empty() { drop_record(data_dir, name); }
    Ok(())
}

/// In tests, end a Recover after `step` of its swap, as a crash would:
/// nothing is undone. Steps 1 to 6 are `swap_in`'s, 11 to 13
/// `finish_swap`'s, 20 `rewrite_record`'s, and 21 and 22 Open Without Them's.
#[cfg(test)]
fn crash_after(step: u8) {
    if test_hooks::SWAP_CRASH.with(std::cell::Cell::get) == Some(step) { panic!("crashed after step {step} of the swap"); }
}

#[cfg(not(test))]
fn crash_after(_: u8) {}

#[cfg(test)]
mod test_hooks {
    thread_local! {
        /// The step a test Recover, finish or set aside crashes after (see `crash_after`).
        pub(super) static SWAP_CRASH: std::cell::Cell<Option<u8>> = const { std::cell::Cell::new(None) };
    }
}

fn remove_working_file(fresh: &Path) {
    for suffix in ["", "-journal", "-wal", "-shm"] { let _ = std::fs::remove_file(format!("{}{suffix}", fresh.display())); }
}

/// A time as `20261029T101500Z`, in UTC, by SQLite's own calendar.
fn utc_stamp(ms: u64) -> Result<String, VaultError> {
    let conn = rusqlite::Connection::open_in_memory().map_err(|_| VaultError::Storage)?;
    conn.query_row("SELECT strftime('%Y%m%dT%H%M%SZ', ?1, 'unixepoch')", [(ms / 1000) as i64], |r| r.get(0)).map_err(|_| VaultError::Storage)
}

/// Readable and writable only by this user. The vault file and its WAL are
/// made that way already; a moved file keeps it even if something changed it.
fn private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn sort(entries: &mut [EntrySummary]) {
    entries.sort_by(|a, b| (a.state, a.origin.as_str(), a.username.as_str(), &a.id).cmp(&(b.state, b.origin.as_str(), b.username.as_str(), &b.id)));
}

/// Readable first, then by label ignoring case, then by when each was saved.
fn sort_addresses(addresses: &mut [SavedAddress]) {
    addresses.sort_by(|a, b| {
        let label = |saved: &SavedAddress| Zeroizing::new(saved.address.get(AddressField::Label).to_lowercase());
        (a.state, label(a).as_str(), a.created_ms, &a.id).cmp(&(b.state, label(b).as_str(), b.created_ms, &b.id))
    });
}

/// Readable first, then by nickname ignoring case, then by network and last
/// four, then by when each was saved.
fn sort_cards(cards: &mut [SavedCard]) {
    cards.sort_by(|a, b| {
        let nickname = |saved: &SavedCard| Zeroizing::new(saved.nickname.to_lowercase());
        (a.state, nickname(a).as_str(), a.network.name(), a.last4.as_str(), a.created_ms, &a.id).cmp(&(b.state, nickname(b).as_str(), b.network.name(), b.last4.as_str(), b.created_ms, &b.id))
    });
}

#[cfg(test)]
mod tests;
