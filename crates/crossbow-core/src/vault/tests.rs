//! Only disposable vaults under the process TMPDIR and the in-memory key
//! store are used. Every secret here is a synthetic fixture.

use super::crypto::{self, OpenError};
use super::generator::{self, GeneratorError, PasswordRules};
use super::*;
use rusqlite::{OptionalExtension, params};
use std::sync::{Arc, Mutex};

const PASSWORD_A: &str = "fixture-password-alpha-7Q";
const PASSWORD_B: &str = "fixture-password-bravo-3Z";
const NOTES_A: &str = "fixture recovery note";

struct Fixture {
    dir: tempfile::TempDir,
    keys: MemoryVaultKeyStore,
}

impl Fixture {
    fn new() -> Fixture {
        Fixture { dir: tempfile::tempdir().unwrap(), keys: MemoryVaultKeyStore::default() }
    }

    fn open(&self) -> Vault {
        Vault::open(self.dir.path(), Box::new(self.keys.clone())).unwrap()
    }

    fn unlocked(&self) -> Vault {
        let mut vault = self.open();
        vault.unlock(Moment::now()).unwrap();
        vault
    }

    /// Every byte the vault wrote: the database plus any WAL or journal.
    fn file_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for entry in std::fs::read_dir(self.dir.path()).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().unwrap().to_string_lossy().starts_with("vault.sqlite") {
                bytes.extend(std::fs::read(path).unwrap());
            }
        }
        bytes
    }
}

fn input<'a>(origin: &'a str, username: &'a str, password: &'a str, notes: &'a str) -> EntryInput<'a> {
    EntryInput { origin, username, password, notes, profile: None }
}

fn tagged<'a>(origin: &'a str, username: &'a str, password: &'a str, profile: &'a str) -> EntryInput<'a> {
    EntryInput { origin, username, password, notes: "", profile: Some(profile) }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle.as_bytes())
}

/// Ids for a script: shaped like the vault's UUIDs but with no digit, so a
/// search for a card's or an address's digits can't find them in an id.
const VAULT_ID: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const ROW_ID: &str = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
const OTHER_ROW_ID: &str = "cccccccc-cccc-cccc-cccc-cccccccccccc";

/// Fix this thread's ids, clock (2026-01-01T00:00:00Z) and the vault's
/// random bytes until the script drops. The vault's ids, times and sealed
/// cells are in its file, and fresh ones spell any four given digits now and
/// then; scripted, the same steps write the same file on every run, so a
/// search for a short secret can read every byte of it.
fn script(ids: &[&str]) -> crate::test_script::Script {
    crate::test_script::Script::start(ids, 1_767_225_600_000, 0x5eed)
}

fn listed(vault: &Vault) -> Vec<(String, String)> {
    vault.entries().iter().map(|e| (e.origin.to_string(), e.username.to_string())).collect()
}

// -- envelope ----------------------------------------------------------------------------

#[test]
fn fields_round_trip_and_reject_tamper_wrong_key_entry_field_vault_and_origin() {
    let key = VaultKey::from_slice(&[4; 32]).unwrap();
    let cipher = key.cipher();
    let binding = Binding { vault_id: "vault-a", entry_id: "entry-1", field: Field::Password, origin: "https://example.com" };
    let sealed = crypto::seal(&cipher, &binding, PASSWORD_A).unwrap();
    assert_eq!(crypto::open(&cipher, &binding, &sealed).unwrap().as_str(), PASSWORD_A);
    assert!(!contains(&sealed, PASSWORD_A), "the envelope holds no plaintext");
    assert_ne!(sealed, crypto::seal(&cipher, &binding, PASSWORD_A).unwrap(), "every seal uses a fresh nonce");

    // Flipping any byte of the nonce, ciphertext or tag fails closed.
    for at in 5..sealed.len() {
        let mut tampered = sealed.clone();
        tampered[at] ^= 0x01;
        assert_eq!(crypto::open(&cipher, &binding, &tampered).unwrap_err(), OpenError::Integrity, "byte {at}");
    }
    let other_key = VaultKey::from_slice(&[5; 32]).unwrap().cipher();
    assert_eq!(crypto::open(&other_key, &binding, &sealed).unwrap_err(), OpenError::Integrity, "wrong key");
    let moved = [
        Binding { origin: "https://example.org", ..binding },
        Binding { origin: "https://example.com:8443", ..binding },
        Binding { origin: "http://example.com", ..binding },
        Binding { entry_id: "entry-2", ..binding },
        Binding { field: Field::Notes, ..binding },
        Binding { field: Field::Username, ..binding },
        Binding { vault_id: "vault-b", ..binding },
    ];
    for other in moved {
        assert_eq!(crypto::open(&cipher, &other, &sealed).unwrap_err(), OpenError::Integrity, "{other:?}");
    }
    let mut version = sealed.clone();
    version[0] = 2;
    assert_eq!(crypto::open(&cipher, &binding, &version).unwrap_err(), OpenError::BadHeader);
    let mut epoch = sealed.clone();
    epoch[4] = 9;
    assert_eq!(crypto::open(&cipher, &binding, &epoch).unwrap_err(), OpenError::BadHeader);
    assert_eq!(crypto::open(&cipher, &binding, &sealed[..20]).unwrap_err(), OpenError::Truncated);
}

#[test]
fn inside_a_current_format_file_any_other_envelope_version_or_epoch_is_damage() {
    let cipher = VaultKey::from_slice(&[4; 32]).unwrap().cipher();
    let binding = Binding { vault_id: "vault-a", entry_id: "entry-1", field: Field::Password, origin: "https://example.com" };
    let sealed = crypto::seal(&cipher, &binding, PASSWORD_A).unwrap();
    let open_with = |version: u8, epoch: u32| {
        let mut envelope = sealed.clone();
        envelope[0] = version;
        envelope[1..5].copy_from_slice(&epoch.to_be_bytes());
        crypto::open(&cipher, &binding, &envelope).unwrap_err()
    };
    // A newer Crossbow that changes envelopes raises the file's format row, so
    // within this format nothing but version 1 and epoch 1 is ever written.
    for version in [0, 2, 3, 15, 16, 200, 0xff] { assert_eq!(open_with(version, 1), OpenError::BadHeader, "version {version}"); }
    for epoch in [0, 2, 9, 257, 65_535, 65_536, 1 << 31, u32::MAX] { assert_eq!(open_with(1, epoch), OpenError::BadHeader, "epoch {epoch}"); }
}

/// Every single-bit flip of the 5-byte header (version and epoch), as the
/// second recheck's probe made them: none may read as "made by a newer Crossbow".
#[test]
fn every_single_bit_flip_of_an_envelope_header_is_recoverable_damage() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let entry = vault.add(&input("example.com", "fixture", PASSWORD_A, "")).unwrap();
    let other = vault.add(&input("other.example", "fixture", PASSWORD_B, "")).unwrap();
    vault.lock();
    let check = vault.db().check().unwrap().unwrap();
    let origin: Vec<u8> = vault.db().connection().query_row("SELECT origin FROM entries WHERE id = ?1", params![entry], |r| r.get(0)).unwrap();
    let (mut repaired, mut damaged) = (0, 0);
    for byte in 0..5 {
        for bit in 0..8 {
            // The check: the key still opens the entries, so it is rewritten.
            let mut flipped = check.clone();
            flipped[byte] ^= 1 << bit;
            vault.db().connection().execute("UPDATE meta SET v = ?1 WHERE k = 'check'", params![flipped]).unwrap();
            if vault.unlock(Moment::now()) == Ok(UnlockOutcome::CheckRepaired) { repaired += 1; }
            vault.lock();
            vault.db().connection().execute("UPDATE meta SET v = ?1 WHERE k = 'check'", params![check]).unwrap();
            // An entry: listed as damaged, which can be deleted.
            let mut flipped = origin.clone();
            flipped[byte] ^= 1 << bit;
            vault.db().connection().execute("UPDATE entries SET origin = ?2 WHERE id = ?1", params![entry, flipped]).unwrap();
            assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::Opened));
            if vault.entry(&entry).unwrap().state == EntryState::Damaged { damaged += 1; }
            vault.lock();
        }
    }
    assert_eq!((repaired, damaged), (40, 40), "no flip is left reading as newer");
    vault.unlock(Moment::now()).unwrap();
    vault.delete(&entry).unwrap();
    assert_eq!(vault.password(&other).unwrap().as_str(), PASSWORD_B);
}

#[test]
fn the_vault_subkey_is_independent_of_the_same_bytes_used_as_a_library_key() {
    // Same 32 bytes, different HKDF salt and purpose: a library ciphertext
    // never opens as a vault field and vice versa.
    let bytes = [6u8; 32];
    let vault = VaultKey::from_slice(&bytes).unwrap().cipher();
    let binding = Binding { vault_id: "v", entry_id: "e", field: Field::Password, origin: "https://example.com" };
    let nonce = [1u8; 24];
    let sealed = crypto::seal_fixed(&vault, &binding, PASSWORD_A, &nonce).unwrap();
    let library = crate::sync::e2ee::LibraryKey::from_slice(&bytes).unwrap().library_cipher();
    let hlc = crate::proto::Hlc::new(1, 0, "d");
    let library_binding = crate::sync::e2ee::Binding { account_id: "v", kind: "e", id: "e", hlc: &hlc };
    let library_sealed = crate::sync::e2ee::seal(&library, &library_binding, PASSWORD_A).unwrap();
    let decoded = data_encoding::BASE64URL_NOPAD.decode(library_sealed.as_bytes()).unwrap();
    assert!(crypto::open(&vault, &binding, &decoded).is_err(), "a library envelope never opens as a vault field");
    // Both formats share a header layout, so these fail authentication, not parsing.
    assert_eq!(crypto::open(&vault, &binding, &sealed).unwrap().as_str(), PASSWORD_A);
    let as_library = data_encoding::BASE64URL_NOPAD.encode(&sealed);
    assert_eq!(crate::sync::e2ee::open(&library, &library_binding, &as_library).unwrap_err(), crate::sync::e2ee::E2eeError::Integrity, "a vault field never opens as a library row");
}

// -- unlock, persistence, key store --------------------------------------------------------

#[test]
fn first_unlock_creates_one_key_and_entries_survive_reopening() {
    let fixture = Fixture::new();
    let mut vault = fixture.open();
    assert_eq!(vault.status().unwrap(), VaultStatus::New);
    let vault_id = vault.db().vault_id.clone();
    assert!(!fixture.keys.contains(&vault_id), "opening the file creates no key");
    vault.unlock(Moment::now()).unwrap();
    assert_eq!(vault.status().unwrap(), VaultStatus::Unlocked);
    assert!(fixture.keys.contains(&vault_id));
    let a = vault.add(&input("https://Example.COM/login?next=1", "fixture-a@example.com", PASSWORD_A, NOTES_A)).unwrap();
    let b = vault.add(&input("accounts.example.org", "fixture-b", PASSWORD_B, "")).unwrap();
    drop(vault);

    // A relaunch: a new connection on the same file and key store.
    let mut vault = fixture.open();
    assert_eq!(vault.status().unwrap(), VaultStatus::Locked);
    assert!(vault.entries().is_empty(), "nothing is opened while locked");
    assert_eq!(vault.password(&a).unwrap_err(), VaultError::Locked);
    vault.unlock(Moment::now()).unwrap();
    assert_eq!(listed(&vault), vec![
        ("https://accounts.example.org".into(), "fixture-b".into()),
        ("https://example.com".into(), "fixture-a@example.com".into()),
    ]);
    assert_eq!(vault.password(&a).unwrap().as_str(), PASSWORD_A);
    let secret = vault.secret(&b).unwrap();
    assert_eq!((secret.password.as_str(), secret.notes.as_str()), (PASSWORD_B, ""));
    assert_eq!(vault.secret(&a).unwrap().notes.as_str(), NOTES_A);
    assert!(fixture.keys.contains(&vault_id));
    // The same key is reused, never replaced, on later unlocks.
    let key = fixture.keys.load(&vault_id).unwrap().unwrap();
    vault.lock();
    vault.unlock(Moment::now()).unwrap();
    assert_eq!(fixture.keys.load(&vault_id).unwrap().unwrap().as_bytes(), key.as_bytes());
}

#[test]
fn key_store_failures_stop_unlock_and_never_fall_back_to_plaintext() {
    let fixture = Fixture::new();
    let mut vault = fixture.open();
    let vault_id = vault.db().vault_id.clone();

    // A declined read on first use creates nothing.
    fixture.keys.fail_reads(true);
    let error = vault.unlock(Moment::now()).unwrap_err();
    assert!(matches!(error, VaultError::KeyStore(_)), "{error:?}");
    assert_eq!(vault.status().unwrap(), VaultStatus::New);
    assert!(vault.db().check().unwrap().is_none());
    fixture.keys.fail_reads(false);

    // A failed save leaves no check value, so the vault stays new.
    fixture.keys.fail_writes(true);
    assert!(matches!(vault.unlock(Moment::now()).unwrap_err(), VaultError::KeyStore(_)));
    assert!(!vault.is_unlocked());
    assert!(vault.db().check().unwrap().is_none());
    assert!(!fixture.keys.contains(&vault_id));
    fixture.keys.fail_writes(false);

    vault.unlock(Moment::now()).unwrap();
    vault.add(&input("example.com", "fixture", PASSWORD_A, "")).unwrap();
    vault.lock();

    // Later read failures keep it locked, with the store's own message.
    fixture.keys.fail_reads(true);
    assert_eq!(vault.unlock(Moment::now()).unwrap_err(), VaultError::KeyStore(key_store::STORAGE_ERROR.into()));
    assert!(!vault.is_unlocked() && vault.entries().is_empty());
    fixture.keys.fail_reads(false);
    vault.unlock(Moment::now()).unwrap();
    assert_eq!(vault.entries().len(), 1, "retrying after the store recovers works");
    assert!(!contains(&fixture.file_bytes(), PASSWORD_A));
}

#[test]
fn a_missing_or_foreign_key_is_refused_until_an_explicit_reset() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let vault_id = vault.db().vault_id.clone();
    vault.add(&input("example.com", "fixture", PASSWORD_A, "")).unwrap();
    vault.lock();

    fixture.keys.forget(&vault_id);
    assert_eq!(vault.unlock(Moment::now()).unwrap_err(), VaultError::MissingKey);
    assert!(VaultError::MissingKey.needs_reset());
    assert!(!fixture.keys.contains(&vault_id), "a missing key is never silently recreated");

    fixture.keys.replace(&vault_id, VaultKey::from_slice(&[9; 32]).unwrap());
    assert_eq!(vault.unlock(Moment::now()).unwrap_err(), VaultError::KeyMismatch);
    assert_eq!(vault.count().unwrap(), 1, "refusal deletes nothing");

    // Reset deletes the entries and starts a new vault id. It never touches a
    // key, so a denied key store doesn't stop it.
    fixture.keys.fail_writes(true);
    vault.reset().unwrap();
    fixture.keys.fail_writes(false);
    assert_eq!(vault.count().unwrap(), 0);
    assert_ne!(vault.id(), vault_id);
    assert!(fixture.keys.contains(&vault_id), "the old key stays for any copy that needs it");
    assert_eq!(vault.status().unwrap(), VaultStatus::New);
    vault.unlock(Moment::now()).unwrap();
    assert!(vault.entries().is_empty());
    assert!(fixture.keys.contains(vault.id()));
}

#[test]
fn reset_starts_a_new_vault_id_and_keeps_the_key_a_copy_still_needs() {
    let original = Fixture::new();
    let mut vault = original.unlocked();
    let entry = vault.add(&input("example.com", "fixture", PASSWORD_A, "")).unwrap();
    let old_id = vault.id().to_owned();
    drop(vault);
    // A byte copy of the data folder, as a restored backup or a cloned profile.
    let copy = tempfile::tempdir().unwrap();
    for file in std::fs::read_dir(original.dir.path()).unwrap() {
        let path = file.unwrap().path();
        std::fs::copy(&path, copy.path().join(path.file_name().unwrap())).unwrap();
    }
    let mut copied = Vault::open(copy.path(), Box::new(original.keys.clone())).unwrap();
    assert_eq!(copied.id(), old_id, "a byte copy shares the vault id and so the key");
    copied.unlock(Moment::now()).unwrap();
    copied.reset().unwrap();
    assert_ne!(copied.id(), old_id, "Reset gives the copy a vault id of its own");
    assert!(original.keys.contains(&old_id), "Reset never deletes a key another copy may need");
    assert_eq!(copied.status().unwrap(), VaultStatus::New);
    copied.unlock(Moment::now()).unwrap();
    let new_id = copied.id().to_owned();
    drop(copied);
    assert_eq!(Vault::open(copy.path(), Box::new(original.keys.clone())).unwrap().id(), new_id, "the new id survives reopening");

    let mut vault = original.open();
    assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::Opened), "the original still opens with its own key");
    assert_eq!(vault.password(&entry).unwrap().as_str(), PASSWORD_A);
}

#[test]
fn a_damaged_check_is_rewritten_when_the_stored_key_still_opens_entries() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let entry = vault.add(&input("example.com", "fixture", PASSWORD_A, "")).unwrap();
    vault.lock();
    let flip_check = |vault: &Vault| {
        let mut check = vault.db().check().unwrap().unwrap();
        let last = check.len() - 1;
        check[last] ^= 0x01;
        vault.db().set_check(&check).unwrap();
    };

    flip_check(&vault);
    assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::CheckRepaired), "the key opens the entry, so only the check was damaged");
    assert_eq!(vault.password(&entry).unwrap().as_str(), PASSWORD_A);
    vault.lock();
    assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::Opened), "the check was rewritten");
    vault.lock();

    // A lost check row, with the key and the entries intact.
    vault.db().connection().execute("DELETE FROM meta WHERE k = 'check'", []).unwrap();
    assert_eq!(vault.status().unwrap(), VaultStatus::Locked, "entries mean an existing vault, not a new one");
    assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::CheckRepaired));
    vault.lock();

    // Only a key that opens no entry is a mismatch, which offers Reset.
    let id = vault.id().to_owned();
    let right = fixture.keys.load(&id).unwrap().unwrap();
    fixture.keys.replace(&id, VaultKey::from_slice(&[9; 32]).unwrap());
    flip_check(&vault);
    let error = vault.unlock(Moment::now()).unwrap_err();
    assert_eq!(error, VaultError::KeyMismatch);
    assert!(error.needs_reset());
    fixture.keys.replace(&id, right);

    // A check naming another version or epoch is damage too: the format row
    // is current, so no newer Crossbow wrote it. The key opens the entry.
    let mut check = vault.db().check().unwrap().unwrap();
    check[1..5].copy_from_slice(&2u32.to_be_bytes());
    vault.db().set_check(&check).unwrap();
    assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::CheckRepaired));
    vault.lock();

    // A damaged check over an entry that doesn't open either: nothing shows
    // the key is right, so it is a mismatch, with Try Again and Reset.
    flip_check(&vault);
    set_header(&vault, &entry, "origin", 1, 2);
    let error = vault.unlock(Moment::now()).unwrap_err();
    assert_eq!(error, VaultError::KeyMismatch);
    assert!(error.needs_reset());
    set_header(&vault, &entry, "origin", 1, 1);

    // Entries with neither a key nor a check: the key is missing.
    fixture.keys.forget(&id);
    vault.db().connection().execute("DELETE FROM meta WHERE k = 'check'", []).unwrap();
    assert_eq!(vault.unlock(Moment::now()).unwrap_err(), VaultError::MissingKey);
    assert_eq!(vault.count().unwrap(), 1, "nothing was deleted");
}

/// Rewrite one sealed column's envelope header, as a newer Crossbow would.
fn set_header(vault: &Vault, id: &str, column: &str, version: u8, epoch: u32) {
    let conn = vault.db().connection();
    let mut sealed: Vec<u8> = conn.query_row(&format!("SELECT {column} FROM entries WHERE id = ?1"), params![id], |r| r.get(0)).unwrap();
    sealed[0] = version;
    sealed[1..5].copy_from_slice(&epoch.to_be_bytes());
    conn.execute(&format!("UPDATE entries SET {column} = ?2 WHERE id = ?1"), params![id, sealed]).unwrap();
}

#[test]
fn a_damaged_check_header_is_repaired_and_a_damaged_entry_header_can_be_deleted() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let entry = vault.add(&input("example.com", "fixture", PASSWORD_A, "")).unwrap();
    let other = vault.add(&input("other.example", "fixture", PASSWORD_B, "")).unwrap();
    vault.lock();
    let set_check_header = |vault: &Vault, version: u8, epoch: u32| {
        let mut check = vault.db().check().unwrap().unwrap();
        check[0] = version;
        check[1..5].copy_from_slice(&epoch.to_be_bytes());
        vault.db().set_check(&check).unwrap();
    };
    for (version, epoch) in [(0, 1), (1, 0), (16, 1), (200, 1), (0xff, 1), (1, 1 << 31)] {
        set_check_header(&vault, version, epoch);
        assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::CheckRepaired), "version {version}, epoch {epoch}: the key opens the entries");
        assert_eq!(vault.password(&entry).unwrap().as_str(), PASSWORD_A);
        vault.lock();
        assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::Opened), "the check was rewritten");
        vault.lock();
    }

    // With a key that opens nothing, a damaged header is a key mismatch: Try Again or Reset.
    let id = vault.id().to_owned();
    let right = fixture.keys.load(&id).unwrap().unwrap();
    fixture.keys.replace(&id, VaultKey::from_slice(&[9; 32]).unwrap());
    set_check_header(&vault, 0, 1);
    let error = vault.unlock(Moment::now()).unwrap_err();
    assert_eq!(error, VaultError::KeyMismatch);
    assert!(error.needs_reset());
    fixture.keys.replace(&id, right);
    // Version 2 inside a format-1 file is damage as well, and is repaired.
    set_check_header(&vault, 2, 1);
    assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::CheckRepaired));
    vault.lock();

    // An entry header no release writes is damage, so the entry can be deleted.
    set_header(&vault, &entry, "origin", 0, 1);
    set_header(&vault, &other, "password", 1, 0);
    assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::Opened));
    assert_eq!(vault.entry(&entry).unwrap().state, EntryState::Damaged);
    assert_eq!(vault.password(&entry).unwrap_err(), VaultError::Unreadable);
    assert_eq!(vault.entry(&other).unwrap().state, EntryState::Readable, "its origin and username still open");
    assert_eq!(vault.password(&other).unwrap_err(), VaultError::Unreadable, "a damaged password is not a newer one");
    vault.delete(&entry).unwrap();
    assert_eq!(vault.count().unwrap(), 1);
}

/// A key store that runs a hook once: inside `load` after reading, or inside
/// `save` before writing, so a second handle can act at exactly that point.
#[derive(Clone)]
struct HookStore {
    inner: MemoryVaultKeyStore,
    on_load: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>,
    on_save: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>,
}

impl HookStore {
    fn new(inner: MemoryVaultKeyStore) -> HookStore {
        HookStore { inner, on_load: Arc::default(), on_save: Arc::default() }
    }

    fn on_load(&self, hook: impl FnOnce() + Send + 'static) {
        *self.on_load.lock().unwrap() = Some(Box::new(hook));
    }

    fn on_save(&self, hook: impl FnOnce() + Send + 'static) {
        *self.on_save.lock().unwrap() = Some(Box::new(hook));
    }
}

impl VaultKeyStore for HookStore {
    fn load(&self, vault_id: &str) -> Result<Option<VaultKey>, String> {
        let loaded = self.inner.load(vault_id);
        let hook = self.on_load.lock().unwrap().take();
        if let Some(hook) = hook { hook(); }
        loaded
    }

    fn save(&self, vault_id: &str, key: &VaultKey) -> Result<(), String> {
        let hook = self.on_save.lock().unwrap().take();
        if let Some(hook) = hook { hook(); }
        self.inner.save(vault_id, key)
    }

    fn clear(&self, vault_id: &str) -> Result<(), String> {
        self.inner.clear(vault_id)
    }
}

#[test]
fn two_first_unlocks_share_one_key_and_neither_loses_entries() {
    // B's whole first unlock, with or without a save, runs inside A's key read.
    for b_saves in [false, true] {
        let fixture = Fixture::new();
        let store = HookStore::new(fixture.keys.clone());
        let mut a = Vault::open(fixture.dir.path(), Box::new(store.clone())).unwrap();
        let b = Arc::new(Mutex::new(fixture.open()));
        let b_first: Arc<Mutex<Option<String>>> = Arc::default();
        {
            let (b, b_first) = (b.clone(), b_first.clone());
            store.on_load(move || {
                let mut b = b.lock().unwrap();
                assert_eq!(b.unlock(Moment::now()), Ok(UnlockOutcome::Opened));
                if b_saves { *b_first.lock().unwrap() = Some(b.add(&input("b-first.example", "fixture-b", PASSWORD_B, "")).unwrap()); }
            });
        }
        assert_eq!(a.unlock(Moment::now()), Ok(UnlockOutcome::Opened), "A takes B's key (B saved first: {b_saves})");
        let a_entry = a.add(&input("a-side.example", "fixture-a", PASSWORD_A, "")).unwrap();
        let b_entry = b.lock().unwrap().add(&input("b-side.example", "fixture-b", PASSWORD_B, "")).unwrap();
        drop((a, b));
        let mut fresh = fixture.open();
        assert_eq!(fresh.unlock(Moment::now()), Ok(UnlockOutcome::Opened), "one key and one check survive");
        assert_eq!(fresh.password(&a_entry).unwrap().as_str(), PASSWORD_A);
        assert_eq!(fresh.password(&b_entry).unwrap().as_str(), PASSWORD_B, "nothing B saved became damaged");
        if let Some(id) = b_first.lock().unwrap().clone() { assert_eq!(fresh.password(&id).unwrap().as_str(), PASSWORD_B); }
        assert!(fresh.entries().iter().all(EntrySummary::readable));
    }
}

#[test]
fn a_first_unlock_that_meets_one_in_progress_never_replaces_its_key_or_check() {
    let fixture = Fixture::new();
    let store = HookStore::new(fixture.keys.clone());
    let mut a = Vault::open(fixture.dir.path(), Box::new(store.clone())).unwrap();
    let b = Arc::new(Mutex::new(fixture.open()));
    let b_result: Arc<Mutex<Option<Result<UnlockOutcome, VaultError>>>> = Arc::default();
    {
        // B's first unlock runs after A wrote its check and before A saves its key.
        let (b, b_result) = (b.clone(), b_result.clone());
        store.on_save(move || *b_result.lock().unwrap() = Some(b.lock().unwrap().unlock(Moment::now())));
    }
    assert_eq!(a.unlock(Moment::now()), Ok(UnlockOutcome::Opened));
    let check = a.db().check().unwrap();
    let met = b_result.lock().unwrap().take().unwrap();
    assert_eq!(met, Err(VaultError::MissingKey), "B finds A's check without a key yet, and writes nothing");
    assert_eq!(a.db().check().unwrap(), check, "A's check is untouched");
    // B's Try Again, once A's key is saved, opens with A's key.
    assert_eq!(b.lock().unwrap().unlock(Moment::now()), Ok(UnlockOutcome::Opened));
    let a_entry = a.add(&input("a-side.example", "fixture-a", PASSWORD_A, "")).unwrap();
    let b_entry = b.lock().unwrap().add(&input("b-side.example", "fixture-b", PASSWORD_B, "")).unwrap();
    drop((a, b));
    let mut fresh = fixture.open();
    assert_eq!(fresh.unlock(Moment::now()), Ok(UnlockOutcome::Opened));
    assert_eq!(fresh.password(&a_entry).unwrap().as_str(), PASSWORD_A);
    assert_eq!(fresh.password(&b_entry).unwrap().as_str(), PASSWORD_B);
}

/// A first unlock inserts the check, then saves the key. Stopping between
/// them leaves a check, no key and no entries: "key not found", where Reset
/// deletes nothing but that check. A refused save removes the check again.
#[test]
fn a_first_unlock_that_stops_halfway_loses_nothing_and_can_be_recovered() {
    let fixture = Fixture::new();
    let store = HookStore::new(fixture.keys.clone());
    let mut vault = Vault::open(fixture.dir.path(), Box::new(store.clone())).unwrap();
    store.on_save(|| panic!("stopped before the key was saved"));
    let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| vault.unlock(Moment::now())));
    assert!(stopped.is_err());
    drop(vault);
    let mut vault = fixture.open();
    assert!(vault.db().check().unwrap().is_some(), "the check went first");
    assert!(fixture.keys.load(vault.id()).unwrap().is_none(), "the key never arrived");
    let error = vault.unlock(Moment::now()).unwrap_err();
    assert_eq!(error, VaultError::MissingKey);
    assert!(error.needs_reset());
    assert_eq!(vault.count().unwrap(), 0, "no entry was ever saved under it");
    vault.reset().unwrap();
    assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::Opened));

    let fixture = Fixture::new();
    let mut vault = fixture.open();
    fixture.keys.fail_writes(true);
    assert!(matches!(vault.unlock(Moment::now()), Err(VaultError::KeyStore(_))));
    assert_eq!(vault.db().check().unwrap(), None, "the check written for a refused key is removed");
    assert_eq!(vault.status().unwrap(), VaultStatus::New);
    fixture.keys.fail_writes(false);
    assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::Opened));
}

#[test]
fn reset_restores_a_lost_or_retyped_vault_id_and_format_row_in_the_session() {
    for (case, damage) in [
        ("lost id", "DELETE FROM meta WHERE k = 'vault_id'"),
        ("retyped id", "UPDATE meta SET v = x'00' WHERE k = 'vault_id'"),
        ("lost id and format", "DELETE FROM meta WHERE k IN ('vault_id', 'format')"),
    ] {
        let fixture = Fixture::new();
        let mut vault = fixture.unlocked();
        vault.add(&input("example.com", "fixture", PASSWORD_A, "")).unwrap();
        vault.lock();
        vault.db().connection().execute(damage, []).unwrap();
        let error = vault.unlock(Moment::now()).unwrap_err();
        assert_eq!(error, VaultError::Damaged, "{case}");
        assert!(error.needs_reset());
        vault.reset().unwrap();
        let meta = |k: &str| vault.db().connection().query_row("SELECT typeof(v) || ':' || v FROM meta WHERE k = ?1", [k], |r| r.get::<_, String>(0)).optional().unwrap();
        assert_eq!(meta("format").as_deref(), Some("integer:8"), "{case}");
        assert_eq!(meta("vault_id"), Some(format!("text:{}", vault.id())), "{case}");
        assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::Opened), "{case}: no Reset loop, no relaunch");
        let entry = vault.add(&input("after.example", "fixture", PASSWORD_B, "")).unwrap();
        drop(vault);
        let mut reopened = fixture.open();
        assert_eq!(reopened.unlock(Moment::now()), Ok(UnlockOutcome::Opened), "{case}");
        assert_eq!(reopened.password(&entry).unwrap().as_str(), PASSWORD_B);
    }
}

#[test]
fn a_handle_never_writes_under_a_vault_id_another_handle_reset() {
    let fixture = Fixture::new();
    let mut a = fixture.unlocked();
    a.add(&input("before.example", "fixture", PASSWORD_A, "")).unwrap();
    let mut b = fixture.open();
    b.reset().unwrap();
    let new_id = b.id().to_owned();

    let error = a.add(&input("after.example", "fixture", PASSWORD_B, "")).unwrap_err();
    assert_eq!(error, VaultError::ResetElsewhere);
    assert_eq!(error.to_string(), "The vault was reset in another window. Unlock again.");
    assert!(!error.needs_reset());
    assert!(!a.is_unlocked(), "the stale handle locks");
    assert_eq!(a.id(), new_id, "and follows the file's new id");
    assert_eq!(a.count().unwrap(), 0, "nothing was written");

    // Unlocking again gives both handles the one new key.
    a.unlock(Moment::now()).unwrap();
    assert_eq!(b.unlock(Moment::now()), Ok(UnlockOutcome::Opened));
    let shared = b.add(&input("shared.example", "fixture", PASSWORD_B, "")).unwrap();
    assert_eq!(a.id(), b.id());

    // Update and delete are refused the same way.
    fixture.open().reset().unwrap();
    assert_eq!(a.update(&shared, &input("shared.example", "fixture", PASSWORD_A, "")).unwrap_err(), VaultError::ResetElsewhere);
    assert!(!a.is_unlocked());
    assert_eq!(b.delete(&shared).unwrap_err(), VaultError::ResetElsewhere);
    assert!(!b.is_unlocked());

    // A handle that was locked during the reset unlocks under the new id
    // rather than adopting its old key for the reset file.
    let mut c = fixture.unlocked();
    c.lock();
    let mut d = fixture.open();
    d.reset().unwrap();
    c.unlock(Moment::now()).unwrap();
    assert_eq!(c.id(), d.id());
    let written = c.add(&input("later.example", "fixture", PASSWORD_A, "")).unwrap();
    drop((a, b, c, d));
    let mut fresh = fixture.open();
    assert_eq!(fresh.unlock(Moment::now()), Ok(UnlockOutcome::Opened));
    assert_eq!(fresh.password(&written).unwrap().as_str(), PASSWORD_A, "what the handle wrote opens after a relaunch");
}

#[test]
fn entries_with_another_envelope_version_or_epoch_are_damaged_and_deletable() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let current = vault.add(&input("https://a.example.com", "fixture-a", PASSWORD_A, "")).unwrap();
    let epoch = vault.add(&input("https://b.example.com", "fixture-b", PASSWORD_B, "")).unwrap();
    let password = vault.add(&input("https://c.example.com", "fixture-c", "fixture-password-c", "")).unwrap();
    let version = vault.add(&input("https://d.example.com", "fixture-d", "fixture-password-d", "")).unwrap();
    vault.lock();
    set_header(&vault, &epoch, "origin", 1, 2);
    set_header(&vault, &epoch, "username", 1, 2);
    set_header(&vault, &password, "password", 1, 2);
    set_header(&vault, &version, "origin", 2, 1);
    vault.unlock(Moment::now()).unwrap();

    let state = |id: &str| vault.entry(id).unwrap().state;
    assert_eq!(state(&current), EntryState::Readable);
    assert_eq!(state(&epoch), EntryState::Damaged, "inside a format-1 file, epoch 2 is damage");
    assert_eq!(state(&version), EntryState::Damaged, "and so is envelope version 2");
    assert_eq!(state(&password), EntryState::Readable, "its origin and username still open");
    assert!(vault.entry(&epoch).unwrap().origin.is_empty(), "nothing from an unopened row is shown");
    for id in [&epoch, &password, &version] {
        assert_eq!(vault.password(id).unwrap_err(), VaultError::Unreadable, "{id}");
    }
    assert_eq!(vault.password(&current).unwrap().as_str(), PASSWORD_A);
    let order: Vec<EntryState> = vault.entries().iter().map(|entry| entry.state).collect();
    assert_eq!(order, [EntryState::Readable, EntryState::Readable, EntryState::Damaged, EntryState::Damaged], "unreadable entries sort last");
    assert_eq!(vault.search("example").len(), 2, "unopened entries never match a search");
    for id in [&epoch, &version] { vault.delete(id).unwrap(); }
    assert_eq!(vault.count().unwrap(), 2);
}

#[test]
fn a_vault_file_damaged_at_open_can_be_moved_aside_and_a_storage_error_cannot() {
    let fixture = Fixture::new();
    let path = fixture.dir.path().join(FILE_NAME);
    // Not an SQLite file at all.
    std::fs::write(&path, vec![0x5a; 8192]).unwrap();
    let error = Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).unwrap_err();
    assert_eq!(error, VaultError::Damaged, "a corrupt file is damage, not a permissions problem");
    assert!(error.needs_reset());
    Vault::replace_damaged_file(fixture.dir.path()).unwrap();
    let mut vault = fixture.unlocked();

    // An SQLite file that lost its vault id.
    vault.add(&input("example.com", "fixture", PASSWORD_A, "")).unwrap();
    let old_id = vault.id().to_owned();
    vault.db().connection().execute("DELETE FROM meta WHERE k = 'vault_id'", []).unwrap();
    drop(vault);
    assert_eq!(Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).unwrap_err(), VaultError::Damaged);
    let name = Vault::replace_damaged_file(fixture.dir.path()).unwrap();
    let left: Vec<String> = std::fs::read_dir(fixture.dir.path()).unwrap().map(|file| file.unwrap().file_name().to_string_lossy().into_owned()).filter(|name| name.starts_with(FILE_NAME)).collect();
    assert!(left.is_empty(), "nothing is left under the vault's name: {left:?}");
    assert_eq!(moved_aside(fixture.dir.path()).len(), 2, "both damaged files are kept, under their own names");
    assert!(moved_aside(fixture.dir.path()).contains(&name));
    assert!(fixture.keys.contains(&old_id), "moving the file aside leaves keys alone");
    let vault = fixture.open();
    assert_ne!(vault.id(), old_id);
    assert_eq!(vault.count().unwrap(), 0);
    drop(vault);

    // A folder that can't be written is a storage problem, which Reset must not touch.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let locked = fixture.dir.path().join("read-only");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
        let error = Vault::open(&locked, Box::new(fixture.keys.clone())).unwrap_err();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(error, VaultError::Storage);
        assert!(!error.needs_reset());
    }
}

#[test]
fn replaced_and_deleted_values_leave_no_ciphertext_in_the_vault_files() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let id = vault.add(&input("example.com", "fixture", PASSWORD_A, NOTES_A)).unwrap();
    let sealed = |vault: &Vault, id: &str, column: &str| -> Vec<u8> {
        vault.db().connection().query_row(&format!("SELECT {column} FROM entries WHERE id = ?1"), params![id], |r| r.get(0)).unwrap()
    };
    let (old_password, old_notes) = (sealed(&vault, &id, "password"), sealed(&vault, &id, "notes"));
    // Every file, read while the vault is still open, as a crash would leave them.
    let holding = |needle: &[u8]| -> Vec<String> {
        std::fs::read_dir(fixture.dir.path()).unwrap().map(|file| file.unwrap().path())
            .filter(|path| path.file_name().unwrap().to_string_lossy().starts_with(FILE_NAME))
            .filter(|path| std::fs::read(path).unwrap().windows(needle.len()).any(|window| window == needle))
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned()).collect()
    };
    assert!(!holding(&old_password).is_empty(), "the probe finds live ciphertext");

    vault.update(&id, &input("example.com", "fixture", PASSWORD_B, "")).unwrap();
    assert_eq!(holding(&old_password), Vec::<String>::new(), "a replaced password");
    assert_eq!(holding(&old_notes), Vec::<String>::new(), "replaced notes");

    let replaced = sealed(&vault, &id, "password");
    vault.delete(&id).unwrap();
    assert_eq!(holding(&replaced), Vec::<String>::new(), "a deleted password");

    let id = vault.add(&input("example.com", "fixture", "fixture-password-reset", "")).unwrap();
    let before_reset = sealed(&vault, &id, "password");
    vault.reset().unwrap();
    assert_eq!(holding(&before_reset), Vec::<String>::new(), "a password removed by Reset");
}

#[test]
fn an_interrupted_first_unlock_adopts_its_key_and_a_lost_check_is_rebuilt_from_entries() {
    let fixture = Fixture::new();
    let vault = fixture.open();
    let vault_id = vault.db().vault_id.clone();
    // The key was saved, then the app quit before writing the check value.
    let key = VaultKey::generate().unwrap();
    fixture.keys.save(&vault_id, &key).unwrap();
    drop(vault);
    let mut vault = fixture.open();
    assert_eq!(vault.status().unwrap(), VaultStatus::New);
    vault.unlock(Moment::now()).unwrap();
    assert_eq!(fixture.keys.load(&vault_id).unwrap().unwrap().as_bytes(), key.as_bytes(), "the saved key is adopted, not replaced");
    vault.add(&input("example.com", "fixture", PASSWORD_A, "")).unwrap();
    vault.lock();
    vault.db().connection().execute("DELETE FROM meta WHERE k = 'check'", []).unwrap();
    assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::CheckRepaired), "the adopted key opens the entry, so the check is rebuilt");
    assert_eq!(vault.count().unwrap(), 1);
}

#[test]
fn a_vault_file_from_a_newer_format_is_refused() {
    let fixture = Fixture::new();
    let vault = fixture.open();
    // 11 is the first format past 8 that no flipped bit of a known one reaches.
    vault.db().connection().execute("UPDATE meta SET v = 11 WHERE k = 'format'", []).unwrap();
    drop(vault);
    assert_eq!(Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).unwrap_err(), VaultError::Newer);
}

/// A store this build writes, read by the format-5 build's check of known
/// formats (the same check with its own format 5 and formats 1, 2 and 5,
/// not a real old build), is Newer, never damage: that build asks for an
/// update and offers no Reset. The rule that keeps it so is that every new
/// format is at least two bits from every format a build reads.
#[test]
fn a_format_8_store_is_newer_to_the_format_5_check_and_never_damage() {
    let fixture = Fixture::new();
    let vault = fixture.open();
    let stored: Option<i64> = vault.db().connection().query_row("SELECT v FROM meta WHERE k = 'format'", [], |r| r.get(0)).unwrap();
    drop(vault);
    let format = stored.unwrap();
    assert_eq!(db::check_format(stored, 5, &[1, 2, 5]), Err(VaultError::Newer), "the format-5 check");
    assert_eq!(db::check_format(stored, format, &[1, 2, 5, format]), Ok(()), "this build's own");
    for known in [1, 2, 5] {
        assert!((format ^ known).count_ones() >= 2, "format {format} is one bit from {known}");
    }
    assert_eq!(stored, Some(8));
}

#[test]
fn a_format_value_below_or_far_beyond_the_current_one_is_damage() {
    for (value, expected) in [
        ("11", VaultError::Newer), ("14", VaultError::Newer), ("15", VaultError::Newer),
        // One flipped bit of format 8's, 5's, 2's or 1's payload byte: damage, never "update".
        ("9", VaultError::Damaged), ("10", VaultError::Damaged), ("12", VaultError::Damaged),
        ("4", VaultError::Damaged), ("7", VaultError::Damaged), ("13", VaultError::Damaged),
        ("3", VaultError::Damaged), ("6", VaultError::Damaged),
        ("0", VaultError::Damaged), ("-1", VaultError::Damaged), ("16", VaultError::Damaged), ("200", VaultError::Damaged),
        ("'one'", VaultError::Damaged), ("x'01'", VaultError::Damaged), ("1.5", VaultError::Damaged),
    ] {
        let fixture = Fixture::new();
        let vault = fixture.open();
        vault.db().connection().execute(&format!("UPDATE meta SET v = {value} WHERE k = 'format'"), []).unwrap();
        drop(vault);
        let error = Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).unwrap_err();
        assert_eq!(error, expected, "format {value}");
        assert_eq!(error.needs_reset(), expected == VaultError::Damaged, "only damage offers Reset: {value}");
    }
}

// -- lock and idle timeout --------------------------------------------------------------

#[test]
fn lock_drops_everything_opened_and_idle_time_locks_on_either_clock() {
    let fixture = Fixture::new();
    let mut vault = fixture.open();
    let start = Moment::now();
    vault.unlock(start).unwrap();
    let id = vault.add(&input("example.com", "fixture", PASSWORD_A, "")).unwrap();
    assert_eq!(vault.idle_remaining(start), Some(IDLE_TIMEOUT));
    assert!(!vault.lock_if_idle(start.after(IDLE_TIMEOUT - Duration::from_secs(1))));
    // Activity postpones the lock.
    vault.touch(start.after(Duration::from_secs(120)));
    assert!(!vault.lock_if_idle(start.after(IDLE_TIMEOUT + Duration::from_secs(60))));
    assert!(vault.lock_if_idle(start.after(IDLE_TIMEOUT + Duration::from_secs(120))));
    assert_eq!(vault.status().unwrap(), VaultStatus::Locked);
    assert!(vault.entries().is_empty() && vault.idle_remaining(start).is_none());
    assert_eq!(vault.password(&id).unwrap_err(), VaultError::Locked);
    assert_eq!(vault.add(&input("example.com", "x", "x", "")).unwrap_err(), VaultError::Locked);
    assert_eq!(vault.delete(&id).unwrap_err(), VaultError::Locked);
    assert!(!vault.lock_if_idle(start.after(IDLE_TIMEOUT * 3)), "already locked");

    // Sleep: the monotonic clock barely moved, the wall clock passed the limit.
    vault.unlock(start).unwrap();
    let asleep = Moment { mono: start.mono + Duration::from_secs(1), wall_ms: start.wall_ms + IDLE_TIMEOUT.as_millis() as u64 };
    assert!(vault.lock_if_idle(asleep));

    // A wall clock set backwards locks rather than extending the deadline.
    vault.unlock(start).unwrap();
    let rewound = Moment { mono: start.mono + Duration::from_secs(1), wall_ms: start.wall_ms - 60_000 };
    assert!(vault.lock_if_idle(rewound));

    // Explicit lock.
    vault.unlock(start).unwrap();
    assert_eq!(vault.entries().len(), 1);
    vault.lock();
    assert!(!vault.is_unlocked() && vault.entries().is_empty());
}

// -- entries -----------------------------------------------------------------------------

#[test]
fn add_edit_delete_and_search_by_origin_or_username() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let a = vault.add(&input("https://mail.example.com", "Fixture.Alice", PASSWORD_A, "")).unwrap();
    let b = vault.add(&input("http://localhost:8080", "bob", PASSWORD_B, NOTES_A)).unwrap();
    let c = vault.add(&input("shop.example.net", "", "fixture-password-c", "")).unwrap();

    let found = |vault: &Vault, query: &str| vault.search(query).iter().map(|e| e.id.clone()).collect::<Vec<_>>();
    assert_eq!(found(&vault, "EXAMPLE.COM"), vec![a.clone()]);
    assert_eq!(found(&vault, "alice"), vec![a.clone()]);
    assert_eq!(found(&vault, "8080"), vec![b.clone()]);
    assert_eq!(found(&vault, "  "), found(&vault, ""), "blank queries list everything");
    assert_eq!(found(&vault, "").len(), 3);
    assert!(found(&vault, "fixture-password").is_empty(), "passwords are never searched");
    assert!(found(&vault, "recovery").is_empty(), "notes are never searched");

    let created = vault.entry(&a).unwrap().created_ms;
    std::thread::sleep(Duration::from_millis(3));
    vault.update(&a, &input("https://login.example.com:8443/path", "alice", "fixture-password-new", "new note")).unwrap();
    let edited = vault.entry(&a).unwrap();
    assert_eq!((edited.origin.as_str(), edited.username.as_str()), ("https://login.example.com:8443", "alice"));
    assert_eq!(edited.created_ms, created);
    assert!(edited.updated_ms > created);
    assert_eq!(vault.password(&a).unwrap().as_str(), "fixture-password-new");
    assert_eq!(vault.secret(&a).unwrap().notes.as_str(), "new note");

    vault.delete(&c).unwrap();
    assert!(vault.entry(&c).is_none());
    assert_eq!(vault.delete(&c).unwrap_err(), VaultError::NotFound);
    assert_eq!(vault.update(&c, &input("example.com", "x", "x", "")).unwrap_err(), VaultError::NotFound);
    assert_eq!(vault.password(&c).unwrap_err(), VaultError::NotFound);

    vault.lock();
    vault.unlock(Moment::now()).unwrap();
    assert_eq!(listed(&vault), vec![("http://localhost:8080".into(), "bob".into()), ("https://login.example.com:8443".into(), "alice".into())]);
}

#[test]
fn invalid_input_is_refused_before_anything_is_written() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let long = |n: usize| "x".repeat(n);
    let cases = [
        input("", "fixture", PASSWORD_A, ""),
        input("ftp://example.com", "fixture", PASSWORD_A, ""),
        input("https://user:secret@example.com", "fixture", PASSWORD_A, ""),
        input("example.com", "fixture", "", ""),
    ];
    for case in &cases {
        assert!(matches!(vault.add(case).unwrap_err(), VaultError::Invalid(_)), "{case:?}");
    }
    let username = long(MAX_USERNAME_BYTES + 1);
    let password = long(MAX_PASSWORD_BYTES + 1);
    let notes = long(MAX_NOTES_BYTES + 1);
    assert!(matches!(vault.add(&input("example.com", &username, PASSWORD_A, "")).unwrap_err(), VaultError::Invalid(_)));
    assert!(matches!(vault.add(&input("example.com", "fixture", &password, "")).unwrap_err(), VaultError::Invalid(_)));
    assert!(matches!(vault.add(&input("example.com", "fixture", PASSWORD_A, &notes)).unwrap_err(), VaultError::Invalid(_)));
    assert_eq!(vault.count().unwrap(), 0);
    // Exactly at the limits is accepted.
    vault.add(&input("example.com", &long(MAX_USERNAME_BYTES), &long(MAX_PASSWORD_BYTES), &long(MAX_NOTES_BYTES))).unwrap();
}

#[test]
fn origins_normalise_to_scheme_host_and_port() {
    for (typed, origin) in [
        ("example.com", "https://example.com"),
        ("  https://Example.COM/login?next=/a#top  ", "https://example.com"),
        ("https://example.com:443/", "https://example.com"),
        ("http://example.com:80", "http://example.com"),
        ("https://example.com:8443", "https://example.com:8443"),
        ("http://localhost:3000/x", "http://localhost:3000"),
        ("http://127.0.0.1:8080", "http://127.0.0.1:8080"),
        ("https://[::1]:9000", "https://[::1]:9000"),
        ("https://exämple.com", "https://xn--exmple-cua.com"),
        ("HTTPS://SUB.Example.com.", "https://sub.example.com."),
        // Without a scheme, a port is a port, not a scheme.
        ("example.com:8443", "https://example.com:8443"),
        ("localhost:3000/login", "https://localhost:3000"),
        ("[::1]:9000", "https://[::1]:9000"),
        // WHATWG parsing of a full address is kept, extra slashes included.
        ("https:///example.com", "https://example.com"),
    ] {
        assert_eq!(normalize_origin(typed).unwrap(), origin, "{typed}");
    }
    for typed in ["", "   ", "ftp://example.com", "file:///etc/passwd", "javascript:alert(1)", "data:text/html,x", "https://user:pass@example.com", "https://user@example.com", "https://", "chrome://settings", "user:pass@example.com"] {
        assert!(matches!(normalize_origin(typed), Err(VaultError::Invalid(_))), "{typed}");
    }
    // A scheme with too few slashes is refused, not saved as a host named "https".
    for typed in ["https:/example.com", "http:/example.com", "https:example.com", "HTTPS:/Example.com", "https:\\example.com", "ftp:/example.com", "localhost:"] {
        assert_eq!(normalize_origin(typed), Err(VaultError::Invalid(MISTYPED_ORIGIN)), "{typed}");
    }
    assert!(normalize_origin(&format!("https://{}.com", "a".repeat(MAX_ORIGIN_BYTES))).is_err());
}

const MISTYPED_ORIGIN: &str = "Check the Website field. Enter an address like https://example.com.";

#[test]
fn web_schemes_without_a_proper_slash_slash_and_hosts_named_like_a_scheme_are_refused() {
    for typed in [
        "https//bank.example", "http//bank.example", "HTTPS//Bank.example", "Http//bank.example",
        "https:/bank.example", "http:/bank.example", "hTTps:/bank.example",
        "http:bank.example", "https:bank.example", "HTTP:bank.example", "Https:bank.example",
        "http:", "https:", "https;//bank.example", "https:://bank.example", "https\\bank.example", "https,//bank.example",
        // A host equal to a scheme name.
        "https", "http", "HTTPS", "https://https", "http://http/login", "https://HTTP:8443", "http://https.",
    ] {
        assert_eq!(normalize_origin(typed), Err(VaultError::Invalid(MISTYPED_ORIGIN)), "{typed}");
    }
    // Hosts that merely start with the letters of a scheme are fine.
    for (typed, origin) in [("httpbin.org", "https://httpbin.org"), ("http.example.com", "https://http.example.com"), ("https-portal.example", "https://https-portal.example"), ("http_proxy.example", "https://http_proxy.example"), ("https://https.example", "https://https.example"), ("HTTP://Bank.example", "http://bank.example")] {
        assert_eq!(normalize_origin(typed).as_deref(), Ok(origin), "{typed}");
    }
}

/// The second recheck's edges: `http` or `https` followed only by punctuation
/// and then a slash, a host that is a scheme name plus punctuation, and a
/// mistyped scheme before `//`.
#[test]
fn a_scheme_followed_by_punctuation_and_a_slash_is_refused() {
    for typed in [
        "https_//bank.example", "https-//bank.example", "HTTPS_//Bank.example", "http-//bank.example", "https.//bank.example",
        "https //bank.example", "https：//bank.example", "http:\\\\bank.example", "https-_:/bank.example", "Https;\\bank.example",
        // A host that is a scheme name followed only by punctuation.
        "https_", "https-", "http-.", "https.", "http.", "https://https_", "http://https-/login", "https://http_.",
        // `//` with no `://` before it: a mistyped scheme, whatever its letters.
        "ttps//bank.example", "htps//bank.example", "www.https//bank.example", "bank.example//login",
        // A proper scheme in front doesn't hide a scheme-named host.
        "http://https//bank.example",
    ] {
        assert_eq!(normalize_origin(typed), Err(VaultError::Invalid(MISTYPED_ORIGIN)), "{typed}");
    }
    for (typed, origin) in [
        ("https-portal.example", "https://https-portal.example"), ("http_proxy.example", "https://http_proxy.example"),
        ("httpbin.org", "https://httpbin.org"), ("http.example.com", "https://http.example.com"), ("HTTPBIN.org", "https://httpbin.org"),
        ("http2.example", "https://http2.example"), ("https:///bank.example", "https://bank.example"), ("https://bank.example/a//b", "https://bank.example"),
    ] {
        assert_eq!(normalize_origin(typed).as_deref(), Ok(origin), "{typed}");
    }
}

// -- damaged rows and substitution -------------------------------------------------------------

#[test]
fn damaged_or_substituted_rows_fail_closed_and_can_be_deleted() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let a = vault.add(&input("https://a.example.com", "fixture-a", PASSWORD_A, "")).unwrap();
    let b = vault.add(&input("https://b.example.com", "fixture-b", PASSWORD_B, "")).unwrap();
    let c = vault.add(&input("https://c.example.com", "fixture-c", "fixture-password-c", "")).unwrap();
    vault.lock();
    let conn = vault.db().connection();
    // Swap A's and B's passwords: each is bound to its own entry.
    conn.execute("UPDATE entries SET password = (SELECT password FROM entries WHERE id = ?2) WHERE id = ?1", params![a, b]).unwrap();
    // Give C B's origin: bound to B's id, so C's row no longer opens.
    conn.execute("UPDATE entries SET origin = (SELECT origin FROM entries WHERE id = ?2) WHERE id = ?1", params![c, b]).unwrap();
    // Flip one byte of B's username.
    let mut username: Vec<u8> = conn.query_row("SELECT username FROM entries WHERE id = ?1", params![b], |r| r.get(0)).unwrap();
    let last = username.len() - 1;
    username[last] ^= 0x80;
    conn.execute("UPDATE entries SET username = ?2 WHERE id = ?1", params![b, username]).unwrap();

    vault.unlock(Moment::now()).unwrap();
    let damaged: Vec<_> = vault.entries().iter().filter(|e| e.state == EntryState::Damaged).map(|e| e.id.clone()).collect();
    assert_eq!(damaged.len(), 2);
    assert!(damaged.contains(&b) && damaged.contains(&c));
    for entry in vault.entries().iter().filter(|e| e.state == EntryState::Damaged) {
        assert!(entry.origin.is_empty() && entry.username.is_empty(), "nothing from a damaged row is shown");
    }
    assert_eq!(vault.password(&a).unwrap_err(), VaultError::Unreadable, "A's row holds B's password ciphertext");
    assert_eq!(vault.password(&b).unwrap_err(), VaultError::Unreadable);
    assert_eq!(vault.search("example").len(), 1, "damaged rows never match a search");
    vault.delete(&b).unwrap();
    vault.delete(&c).unwrap();
    assert_eq!(vault.entries().len(), 1);
    assert_eq!(vault.count().unwrap(), 1);
}

// -- storage boundaries ------------------------------------------------------------------------

#[test]
fn no_plaintext_reaches_the_vault_file_or_the_library_store() {
    let fixture = Fixture::new();
    let store = crate::store::Store::open(&fixture.dir.path().join("crossbow.sqlite")).unwrap();
    let mut lib = crate::library::Library::open(store).unwrap();
    lib.ensure_defaults().unwrap();
    let dirty = lib.store.dirty_count().unwrap();
    let entities = lib.store.all_entities().unwrap().len();

    let mut vault = fixture.unlocked();
    let id = vault.add(&input("https://secret-origin.example", "secret-username", PASSWORD_A, NOTES_A)).unwrap();
    vault.update(&id, &input("https://secret-origin.example", "secret-username", PASSWORD_B, NOTES_A)).unwrap();
    let gone = vault.add(&input("https://deleted-origin.example", "deleted-username", "fixture-deleted-password", "")).unwrap();
    vault.delete(&gone).unwrap();
    drop(vault);

    let bytes = fixture.file_bytes();
    assert!(!bytes.is_empty());
    for secret in ["secret-origin", "secret-username", PASSWORD_A, PASSWORD_B, NOTES_A, "deleted-origin", "deleted-username", "fixture-deleted-password"] {
        assert!(!contains(&bytes, secret), "{secret} reached the vault file");
    }
    // The library and its sync queue never see the vault.
    assert_eq!(lib.store.dirty_count().unwrap(), dirty);
    assert_eq!(lib.store.all_entities().unwrap().len(), entities);
    let library_bytes = std::fs::read(fixture.dir.path().join("crossbow.sqlite")).unwrap();
    assert!(!contains(&library_bytes, PASSWORD_B) && !contains(&library_bytes, "secret-username"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(fixture.dir.path().join(FILE_NAME)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the vault file is owner-only");
    }
}

#[test]
fn debug_output_never_prints_secrets() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let id = vault.add(&input("https://debug-origin.example", "debug-username", PASSWORD_A, NOTES_A)).unwrap();
    let printed = [
        format!("{vault:?}"),
        format!("{:?}", vault.entries()),
        format!("{:?}", vault.secret(&id).unwrap()),
        format!("{:?}", input("https://debug-origin.example", "debug-username", PASSWORD_A, NOTES_A)),
        format!("{:?}", VaultKey::from_slice(&[1; 32]).unwrap()),
        format!("{:?}", VaultKey::from_slice(&[1; 32]).unwrap().cipher()),
        format!("{:?}", Binding { vault_id: "v", entry_id: &id, field: Field::Password, origin: "https://debug-origin.example" }),
    ];
    for text in printed {
        for secret in ["debug-origin", "debug-username", PASSWORD_A, NOTES_A, "[1, 1"] {
            assert!(!text.contains(secret), "{text} prints {secret}");
        }
    }
    // Error messages are fixed text.
    for error in [VaultError::MissingKey, VaultError::Unreadable, VaultError::Storage, VaultError::Locked] {
        assert!(!error.to_string().contains(PASSWORD_A));
    }
}

// -- generator -----------------------------------------------------------------------------

#[test]
fn generated_passwords_meet_their_rules() {
    let all = PasswordRules { avoid_ambiguous: false, ..PasswordRules::default() };
    for length in [generator::MIN_LENGTH, 20, generator::MAX_LENGTH] {
        for rules in [PasswordRules { length, ..all }, PasswordRules { length, ..PasswordRules::default() }] {
            let alphabet = generator::alphabet(&rules);
            for _ in 0..200 {
                let password = generator::generate(&rules).unwrap();
                assert_eq!(password.chars().count(), length);
                assert!(password.chars().all(|c| alphabet.contains(c)), "{rules:?}");
                assert!(password.chars().any(|c| c.is_ascii_lowercase()));
                assert!(password.chars().any(|c| c.is_ascii_uppercase()));
                assert!(password.chars().any(|c| c.is_ascii_digit()));
                assert!(password.chars().any(|c| c.is_ascii_punctuation()));
            }
        }
    }
    let unambiguous = PasswordRules::default();
    for _ in 0..300 {
        let password = generator::generate(&unambiguous).unwrap();
        assert!(!password.chars().any(|c| "0O1lI|".contains(c)), "look-alike characters are left out");
    }
    let digits_only = PasswordRules { lowercase: false, uppercase: false, symbols: false, ..all };
    assert!(generator::generate(&digits_only).unwrap().chars().all(|c| c.is_ascii_digit()));
    let no_symbols = PasswordRules { symbols: false, ..all };
    assert!(generator::generate(&no_symbols).unwrap().chars().all(|c| c.is_ascii_alphanumeric()));

    assert_eq!(generator::generate(&PasswordRules { length: 11, ..all }).unwrap_err(), GeneratorError::Length);
    assert_eq!(generator::generate(&PasswordRules { length: 65, ..all }).unwrap_err(), GeneratorError::Length);
    let none = PasswordRules { lowercase: false, uppercase: false, digits: false, symbols: false, ..all };
    assert_eq!(generator::generate(&none).unwrap_err(), GeneratorError::NoClasses);
}

#[test]
fn generated_characters_are_uniform_and_passwords_unique() {
    // One class, so the per-class guarantee doesn't skew counts.
    let rules = PasswordRules { length: 64, lowercase: false, uppercase: true, digits: false, symbols: false, avoid_ambiguous: true };
    let alphabet: Vec<char> = generator::alphabet(&rules).chars().collect();
    assert_eq!(alphabet.len(), 24, "A–Z without I and O");
    let mut counts = std::collections::HashMap::new();
    let mut seen = std::collections::HashSet::new();
    let rounds = 1_000;
    for _ in 0..rounds {
        let password = generator::generate(&rules).unwrap();
        for c in password.chars() { *counts.entry(c).or_insert(0usize) += 1; }
        assert!(seen.insert(password.to_string()), "a repeated 64-character password means the generator isn't random");
    }
    let expected = (rounds * 64) as f64 / alphabet.len() as f64;
    // Chi-square with 23 degrees of freedom; 60 is far beyond the 0.01% tail.
    let chi: f64 = alphabet.iter().map(|c| {
        let observed = *counts.get(c).unwrap_or(&0) as f64;
        (observed - expected).powi(2) / expected
    }).sum();
    assert!(chi < 60.0, "chi-square {chi} for {counts:?}");
    // Every position sees every character over many passwords: no fixed slots.
    let mut first = std::collections::HashSet::new();
    let full = PasswordRules::default();
    for _ in 0..2_000 { first.insert(generator::generate(&full).unwrap().chars().next().unwrap()); }
    assert!(first.iter().any(|c| c.is_ascii_lowercase()) && first.iter().any(|c| c.is_ascii_digit()) && first.iter().any(|c| c.is_ascii_punctuation()));
}

// Format 2: profile tags, last use and the never-save list.

const PROFILE_A: &str = "0190a1b2-fixture-profile-a";
const PROFILE_B: &str = "0190a1b2-fixture-profile-b";
const FORMAT_1_COLUMNS: [&str; 7] = ["id", "created_ms", "updated_ms", "origin", "username", "password", "notes"];

fn columns(conn: &rusqlite::Connection) -> Vec<String> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('entries') ORDER BY cid").unwrap();
    stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
}

fn has_table(conn: &rusqlite::Connection, name: &str) -> bool {
    conn.query_row("SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = ?1", [name], |r| r.get(0)).unwrap()
}

fn raw_format(dir: &std::path::Path) -> (Option<i64>, Vec<String>, bool, i64) {
    let conn = rusqlite::Connection::open(dir.join(FILE_NAME)).unwrap();
    let format = conn.query_row("SELECT v FROM meta WHERE k = 'format'", [], |r| r.get(0)).optional().unwrap();
    let count = conn.query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0)).unwrap();
    (format, columns(&conn), has_table(&conn, "never_save"), count)
}

/// A vault with three entries, turned back into the format-1 layout: the
/// same seven columns, no never-save table and format 1.
fn format_1_vault() -> (Fixture, Vec<String>) {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let ids = ["https://one.example", "https://two.example", "https://three.example"].iter()
        .map(|origin| vault.add(&input(origin, "fixture", PASSWORD_A, NOTES_A)).unwrap()).collect();
    vault.db().connection().execute_batch(
        "ALTER TABLE entries DROP COLUMN used_ms; ALTER TABLE entries DROP COLUMN profile; DROP TABLE never_save; DROP TABLE addresses; DROP TABLE cards; UPDATE meta SET v = 1 WHERE k = 'format';",
    ).unwrap();
    drop(vault);
    let (format, columns, never_save, count) = raw_format(fixture.dir.path());
    assert_eq!((format, columns, never_save, count), (Some(1), FORMAT_1_COLUMNS.map(String::from).to_vec(), false, 3), "a format-1 file");
    (fixture, ids)
}

#[test]
fn opening_a_format_1_vault_upgrades_it_in_place_and_keeps_every_entry_untagged() {
    let (fixture, ids) = format_1_vault();
    let mut vault = fixture.unlocked();
    let (format, columns, never_save, count) = raw_format(fixture.dir.path());
    assert_eq!(format, Some(8));
    assert!(has_table(&raw(&fixture), "addresses") && has_table(&raw(&fixture), "cards"));
    assert_eq!(columns.last().map(String::as_str), Some("used_ms"));
    assert!(columns.contains(&"profile".to_string()) && never_save && count == 3);
    assert_eq!(vault.entries().len(), 3);
    for id in &ids {
        let entry = vault.entry(id).unwrap();
        assert!(entry.readable(), "a format-1 entry still opens");
        assert_eq!((entry.profile.as_deref(), entry.used_ms), (None, None), "and is in no profile");
        assert_eq!(vault.password(id).unwrap().as_str(), PASSWORD_A);
    }
    assert!(vault.matching("https://one.example", PROFILE_A).is_empty(), "untagged entries are never offered");
    vault.set_profile(&ids[0], PROFILE_A).unwrap();
    assert_eq!(vault.matching("https://one.example", PROFILE_A).len(), 1, "until tagged with Use in Profile");
    drop(vault);
    let reopened = fixture.unlocked();
    assert_eq!(reopened.entry(&ids[0]).unwrap().profile.as_deref(), Some(PROFILE_A), "the tag persists");
    assert_eq!(reopened.entry(&ids[1]).unwrap().profile, None);
}

#[test]
fn a_format_1_vault_that_lost_its_id_is_still_damaged_and_not_given_a_new_one() {
    let (fixture, _) = format_1_vault();
    rusqlite::Connection::open(fixture.dir.path().join(FILE_NAME)).unwrap().execute("DELETE FROM meta WHERE k = 'vault_id'", []).unwrap();
    let error = Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).unwrap_err();
    assert_eq!(error, VaultError::Damaged, "Reset repairs it, as before");
    let id: Option<String> = rusqlite::Connection::open(fixture.dir.path().join(FILE_NAME)).unwrap()
        .query_row("SELECT v FROM meta WHERE k = 'vault_id'", [], |r| r.get(0)).optional().unwrap();
    assert_eq!(id, None, "an upgrade never invents an id for entries sealed under another");
}

#[test]
fn an_upgrade_cut_halfway_leaves_format_1_whole_and_the_next_open_finishes_it() {
    let (fixture, ids) = format_1_vault();
    let snapshot = tempfile::tempdir().unwrap();
    let (source, copy) = (fixture.dir.path().to_path_buf(), snapshot.path().to_path_buf());
    // Copy the files on disk mid-transaction, as a power cut would leave them,
    // then fail the upgrade there, as a crash of this process would.
    super::db::UPGRADE_CUT.set(Some(Box::new(move |_: &rusqlite::Connection| {
        for name in [FILE_NAME.to_string(), format!("{FILE_NAME}-wal")] {
            if source.join(&name).exists() { std::fs::copy(source.join(&name), copy.join(&name)).unwrap(); }
        }
        true
    })));
    let error = Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).unwrap_err();
    super::db::UPGRADE_CUT.set(None);
    assert_eq!(error, VaultError::Storage, "a cut upgrade is a storage error, never damage");
    for (case, dir) in [("rolled back", fixture.dir.path()), ("power cut", snapshot.path())] {
        let (format, columns, never_save, count) = raw_format(dir);
        assert_eq!((format, columns, never_save, count), (Some(1), FORMAT_1_COLUMNS.map(String::from).to_vec(), false, 3), "{case}: wholly format 1");
        let mut vault = Vault::open(dir, Box::new(fixture.keys.clone())).unwrap();
        vault.unlock(Moment::now()).unwrap();
        let (format, _, never_save, count) = raw_format(dir);
        assert_eq!((format, never_save, count), (Some(8), true, 3), "{case}: the next open upgrades it");
        for id in &ids { assert_eq!(vault.password(id).unwrap().as_str(), PASSWORD_A, "{case}: no entry lost"); }
    }
}

#[test]
fn a_profile_tag_is_sealed_and_bound_to_its_entry() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let a = vault.add(&tagged("https://bank.example", "fixture-a", PASSWORD_A, PROFILE_A)).unwrap();
    let b = vault.add(&tagged("https://bank.example", "fixture-b", PASSWORD_B, PROFILE_B)).unwrap();
    assert!(!contains(&fixture.file_bytes(), PROFILE_A) && !contains(&fixture.file_bytes(), PROFILE_B), "profile ids are sealed");
    // Moving B's tag onto A must not put A in profile B.
    let conn = vault.db().connection();
    let tag_b: Vec<u8> = conn.query_row("SELECT profile FROM entries WHERE id = ?1", [&b], |r| r.get(0)).unwrap();
    conn.execute("UPDATE entries SET profile = ?2 WHERE id = ?1", params![a, tag_b]).unwrap();
    vault.lock();
    vault.unlock(Moment::now()).unwrap();
    assert_eq!(vault.entry(&a).unwrap().state, EntryState::Damaged, "a moved tag is damage");
    assert!(vault.matching("https://bank.example", PROFILE_B).iter().all(|entry| entry.id == b));
    // A removed tag leaves the entry in no profile: listed, never offered.
    vault.db().connection().execute("UPDATE entries SET profile = NULL WHERE id = ?1", [&b]).unwrap();
    vault.lock();
    vault.unlock(Moment::now()).unwrap();
    assert!(vault.entry(&b).unwrap().readable());
    assert!(vault.matching("https://bank.example", PROFILE_B).is_empty());
    assert_eq!(vault.set_profile(&a, PROFILE_A), Err(VaultError::Unreadable), "a damaged entry can't be retagged");
    assert!(matches!(vault.add(&tagged("https://bank.example", "x", PASSWORD_A, "")), Err(VaultError::Invalid(_))));
}

#[test]
fn only_exact_origin_entries_of_the_same_profile_match_a_page() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let wanted = vault.add(&tagged("https://login.example", "fixture", PASSWORD_A, PROFILE_A)).unwrap();
    for (origin, profile) in [
        ("https://login.example", PROFILE_B), ("http://login.example", PROFILE_A), ("https://login.example:8443", PROFILE_A),
        ("https://www.login.example", PROFILE_A), ("https://example", PROFILE_A),
    ] {
        vault.add(&tagged(origin, "fixture", PASSWORD_B, profile)).unwrap();
    }
    vault.add(&input("https://login.example", "untagged", PASSWORD_B, "")).unwrap();
    let ids = |origin: &str, profile: &str| vault.matching(origin, profile).iter().map(|entry| entry.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids("https://login.example", PROFILE_A), [wanted.clone()]);
    assert_eq!(ids("https://login.example:443", PROFILE_A), Vec::<String>::new(), "origins are compared normalised, not parsed again");
    assert_eq!(ids("http://login.example", PROFILE_A).len(), 1, "http has its own entry, and never the https one");
    assert_eq!(ids("https://login.example", PROFILE_B).len(), 1);
    assert_eq!(ids("https://login.example", "another-profile").len(), 0);
}

#[test]
fn last_use_is_recorded_per_entry_and_survives_a_reopen() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let used = vault.add(&tagged("https://login.example", "fixture", PASSWORD_A, PROFILE_A)).unwrap();
    let unused = vault.add(&tagged("https://login.example", "other", PASSWORD_B, PROFILE_A)).unwrap();
    vault.mark_used(&used, 1_700_000_000_000).unwrap();
    assert_eq!(vault.entry(&used).unwrap().used_ms, Some(1_700_000_000_000));
    drop(vault);
    let mut vault = fixture.unlocked();
    assert_eq!((vault.entry(&used).unwrap().used_ms, vault.entry(&unused).unwrap().used_ms), (Some(1_700_000_000_000), None));
    vault.update(&used, &tagged("https://login.example", "fixture", PASSWORD_B, PROFILE_A)).unwrap();
    assert_eq!(vault.entry(&used).unwrap().used_ms, Some(1_700_000_000_000), "an edit is not a use");
    vault.lock();
    assert_eq!(vault.mark_used(&used, 1), Err(VaultError::Locked));
}

#[test]
fn the_never_save_list_is_per_profile_persists_and_is_readable_while_locked() {
    let fixture = Fixture::new();
    let vault = fixture.open();
    assert!(!vault.is_unlocked());
    vault.never_save(PROFILE_A, "https://login.example").unwrap();
    vault.never_save(PROFILE_A, "https://login.example").unwrap();
    vault.never_save(PROFILE_A, "http://other.example:8080").unwrap();
    vault.never_save(PROFILE_B, "https://b.example").unwrap();
    for origin in ["login.example", "https://login.example/path", "https://LOGIN.example", "file:///etc", ""] {
        assert!(matches!(vault.never_save(PROFILE_A, origin), Err(VaultError::Invalid(_))), "{origin:?} is not a normalised web origin");
    }
    assert!(matches!(vault.never_save("", "https://login.example"), Err(VaultError::Invalid(_))));
    drop(vault);
    let vault = fixture.open();
    assert_eq!(vault.never_saved(PROFILE_A).unwrap(), ["http://other.example:8080", "https://login.example"]);
    assert_eq!(vault.never_saved(PROFILE_B).unwrap(), ["https://b.example"]);
    assert!(vault.is_never_saved(PROFILE_A, "https://login.example").unwrap());
    assert!(!vault.is_never_saved(PROFILE_B, "https://login.example").unwrap(), "another profile still offers");
    assert!(!vault.is_never_saved(PROFILE_A, "https://login.example:8443").unwrap(), "another port still offers");
    vault.remove_never_save(PROFILE_A, "https://login.example").unwrap();
    drop(vault);
    let vault = fixture.open();
    assert_eq!(vault.never_saved(PROFILE_A).unwrap(), ["http://other.example:8080"]);
}

// -- content damage is never a storage error (round 4) -----------------------------------------

/// Damage left by another tool, a partial restore or a bit flip: a second
/// connection, as the third recheck's probes used.
fn damage(fixture: &Fixture, sql: &str, id: &str) {
    let conn = rusqlite::Connection::open(fixture.dir.path().join(FILE_NAME)).unwrap();
    let changed = if sql.contains("?1") { conn.execute(sql, params![id]) } else { conn.execute(sql, []) }.unwrap();
    assert_eq!(changed, 1, "{sql}");
}

/// Two tagged entries, the first one used, and the vault closed.
fn two_entries() -> (Fixture, [String; 2]) {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let ids = [
        vault.add(&EntryInput { origin: "https://one.example", username: "fixture", password: PASSWORD_A, notes: NOTES_A, profile: Some(PROFILE_A) }).unwrap(),
        vault.add(&tagged("https://two.example", "fixture", PASSWORD_B, PROFILE_A)).unwrap(),
    ];
    vault.mark_used(&ids[0], 1_000).unwrap();
    drop(vault);
    (fixture, ids)
}

/// The fourth recheck's Q1: errors read from real damaged files through the
/// statements this module runs. A renamed column arrives as rusqlite's
/// SqlInputError from prepare, a dropped table as SQLite's generic error, a
/// broken page as corruption, a file of noise as not a database: all damage.
/// Access, locking, I/O, space and memory stay storage errors, and so does an
/// error only a bug in this module can cause, so a bug never offers Reset.
#[test]
fn errors_are_classified_by_their_source_not_by_where_they_surface() {
    use rusqlite::{Error, ffi, types::Type};
    let sqlite = |code| Error::SqliteFailure(ffi::Error::new(code), None);
    // Access, locking, I/O, space and memory: the file may be fine, so Try Again, never Reset.
    for code in [
        ffi::SQLITE_BUSY, ffi::SQLITE_LOCKED, ffi::SQLITE_IOERR, ffi::SQLITE_IOERR_READ, ffi::SQLITE_IOERR_SHORT_READ, ffi::SQLITE_PERM, ffi::SQLITE_CANTOPEN,
        ffi::SQLITE_READONLY, ffi::SQLITE_FULL, ffi::SQLITE_NOMEM, ffi::SQLITE_AUTH, ffi::SQLITE_PROTOCOL, ffi::SQLITE_INTERRUPT, ffi::SQLITE_ABORT, ffi::SQLITE_SCHEMA, ffi::SQLITE_NOLFS,
    ] {
        let error = db::classify(sqlite(code));
        assert_eq!(error, VaultError::Storage, "code {code}");
        assert!(!error.needs_reset());
    }
    let real = |sql: &str, read: &str| -> Error {
        let (fixture, _, _) = probed_vault();
        raw(&fixture).execute_batch(sql).unwrap();
        let conn = raw(&fixture);
        let error = conn.prepare(read).and_then(|mut stmt| stmt.query([]).and_then(|mut rows| rows.next().map(|_| ()))).unwrap_err();
        error
    };
    let renamed = real("ALTER TABLE entries RENAME COLUMN origin TO nrigin", db::ROWS);
    assert!(matches!(renamed, Error::SqlInputError { .. }), "{renamed:?}");
    let dropped = real("DROP TABLE entries", "SELECT COUNT(*) FROM entries");
    assert!(matches!(&dropped, Error::SqliteFailure(failure, _) if failure.extended_code == ffi::SQLITE_ERROR), "{dropped:?}");
    let on_its_index = real(&on_its_own_index(), db::ROWS);
    assert!(matches!(&on_its_index, Error::SqliteFailure(failure, _) if failure.code == rusqlite::ErrorCode::DatabaseCorrupt), "{on_its_index:?}");
    let noise = {
        let fixture = Fixture::new();
        std::fs::write(fixture.dir.path().join(FILE_NAME), vec![0x5a; 8192]).unwrap();
        raw(&fixture).query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)).unwrap_err()
    };
    assert!(matches!(&noise, Error::SqliteFailure(failure, _) if failure.code == rusqlite::ErrorCode::NotADatabase), "{noise:?}");
    // Values read without an SQLite error: the wrong storage class, invalid
    // UTF-8, an integer out of range, a failed decode.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let utf8 = conn.query_row("SELECT CAST(x'3031ff32' AS TEXT)", [], |r| r.get::<_, String>(0)).unwrap_err();
    assert!(matches!(utf8, Error::Utf8Error(..)), "{utf8:?}");
    let class = conn.query_row("SELECT x'00'", [], |r| r.get::<_, i64>(0)).unwrap_err();
    assert!(matches!(class, Error::InvalidColumnType(..)), "{class:?}");
    let missing = conn.query_row("SELECT 1 WHERE 0", [], |r| r.get::<_, i64>(0)).unwrap_err();
    for error in [renamed, dropped, on_its_index, noise, utf8, class, missing, Error::IntegralValueOutOfRange(0, i64::MAX), Error::FromSqlConversionFailure(0, Type::Blob, "wrong length".into())] {
        let shown = format!("{error:?}");
        let error = db::classify(error);
        assert_eq!(error, VaultError::Damaged, "{shown}");
        assert!(error.needs_reset());
    }
    // This module's own misuse of SQLite is never damage, so a bug can't offer
    // to delete a vault. Its statements name fixed columns, so a column index
    // or name out of range is such a bug.
    for error in [
        sqlite(ffi::SQLITE_MISUSE), sqlite(ffi::SQLITE_RANGE), sqlite(ffi::SQLITE_INTERNAL), Error::InvalidParameterName("?9".into()), Error::ExecuteReturnedResults,
        Error::InvalidColumnIndex(7), Error::InvalidColumnName("profile".into()),
    ] {
        assert_eq!(db::classify(error), VaultError::Storage);
    }
}


#[test]
fn a_vault_id_of_the_wrong_type_or_encoding_is_damage_at_open() {
    for sql in [
        "UPDATE meta SET v = CAST(x'3031ff32' AS TEXT) WHERE k = 'vault_id'",
        "UPDATE meta SET v = CAST(v AS BLOB) WHERE k = 'vault_id'",
        "UPDATE meta SET v = 7 WHERE k = 'vault_id'",
        "UPDATE meta SET v = 0.5 WHERE k = 'vault_id'",
    ] {
        let (fixture, _) = two_entries();
        damage(&fixture, sql, "");
        let error = Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).unwrap_err();
        assert_eq!(error, VaultError::Damaged, "{sql}");
        assert!(error.needs_reset(), "Reset is offered: {sql}");
        Vault::replace_damaged_file(fixture.dir.path()).unwrap();
        assert_eq!(fixture.unlocked().count().unwrap(), 0, "a new vault after Reset: {sql}");
    }
}

#[test]
fn a_check_of_the_wrong_type_is_damage_the_stored_key_repairs() {
    for sql in [
        "UPDATE meta SET v = CAST(v AS TEXT) WHERE k = 'check'",
        "UPDATE meta SET v = CAST(x'ff00' AS TEXT) WHERE k = 'check'",
        "UPDATE meta SET v = 7 WHERE k = 'check'",
        "UPDATE meta SET v = 0.5 WHERE k = 'check'",
        "UPDATE meta SET v = '' WHERE k = 'check'",
    ] {
        let (fixture, ids) = two_entries();
        damage(&fixture, sql, "");
        let mut vault = fixture.open();
        assert_eq!(vault.status(), Ok(VaultStatus::Locked), "{sql}");
        assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::CheckRepaired), "the key opens the entries: {sql}");
        assert_eq!(vault.password(&ids[1]).unwrap().as_str(), PASSWORD_B);
        vault.lock();
        assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::Opened), "the check was rewritten: {sql}");
        // Without the key, a check of any type still means one was made: Reset, never a new key.
        let error = Vault::open(fixture.dir.path(), Box::new(MemoryVaultKeyStore::default())).unwrap().unlock(Moment::now()).unwrap_err();
        assert_eq!(error, VaultError::MissingKey, "{sql}");
    }
    // A retyped check in a vault with no entries: nothing can be lost, so the stored key adopts it, as before.
    let fixture = Fixture::new();
    drop(fixture.unlocked());
    damage(&fixture, "UPDATE meta SET v = CAST(v AS TEXT) WHERE k = 'check'", "");
    assert_eq!(fixture.open().unlock(Moment::now()), Ok(UnlockOutcome::CheckRepaired));
}

#[test]
fn a_wrong_typed_cell_damages_only_its_own_entry_which_can_be_deleted() {
    for sql in [
        "UPDATE entries SET id = CAST(id AS BLOB) WHERE id = ?1",
        "UPDATE entries SET id = CAST(x'3031ff32' AS TEXT) WHERE id = ?1",
        "UPDATE entries SET id = NULL WHERE id = ?1",
        "UPDATE entries SET id = '' WHERE id = ?1",
        "UPDATE entries SET id = 'damaged-row:1' WHERE id = ?1",
        "UPDATE entries SET created_ms = 'x' WHERE id = ?1",
        "UPDATE entries SET created_ms = -1 WHERE id = ?1",
        "UPDATE entries SET updated_ms = 1.5 WHERE id = ?1",
        "UPDATE entries SET updated_ms = x'00' WHERE id = ?1",
        "UPDATE entries SET used_ms = 'yesterday' WHERE id = ?1",
        "UPDATE entries SET used_ms = x'00' WHERE id = ?1",
        "UPDATE entries SET origin = CAST(origin AS TEXT) WHERE id = ?1",
        "UPDATE entries SET origin = 5 WHERE id = ?1",
        "UPDATE entries SET username = 5 WHERE id = ?1",
        "UPDATE entries SET username = CAST(username AS TEXT) WHERE id = ?1",
        "UPDATE entries SET profile = CAST(profile AS TEXT) WHERE id = ?1",
        "UPDATE entries SET profile = 1 WHERE id = ?1",
        "UPDATE entries SET password = CAST(password AS TEXT) WHERE id = ?1",
        "UPDATE entries SET password = 0.5 WHERE id = ?1",
        "UPDATE entries SET notes = 0 WHERE id = ?1",
        "UPDATE entries SET notes = CAST(x'ff' AS TEXT) WHERE id = ?1",
    ] {
        let (fixture, [first, second]) = two_entries();
        damage(&fixture, sql, &first);
        let mut vault = fixture.open();
        assert_eq!(vault.status(), Ok(VaultStatus::Locked), "{sql}");
        assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::Opened), "one cell never fails the whole unlock: {sql}");
        assert_eq!(vault.entries().len(), 2, "{sql}");
        let damaged: Vec<String> = vault.entries().iter().filter(|entry| !entry.readable()).map(|entry| entry.id.clone()).collect();
        assert_eq!(damaged.len(), 1, "only the damaged entry: {sql}");
        let entry = vault.entry(&damaged[0]).unwrap();
        assert!(entry.origin.is_empty() && entry.username.is_empty() && entry.profile.is_none(), "nothing from it is shown: {sql}");
        assert_eq!(vault.password(&damaged[0]).unwrap_err(), VaultError::Unreadable, "{sql}");
        assert!(vault.matching("https://one.example", PROFILE_A).is_empty(), "never offered to a page: {sql}");
        assert_eq!(vault.password(&second).unwrap().as_str(), PASSWORD_B, "the other entry opens: {sql}");
        assert_eq!(vault.matching("https://two.example", PROFILE_A).len(), 1);
        vault.delete(&damaged[0]).unwrap();
        assert_eq!((vault.entries().len(), vault.count().unwrap()), (1, 1), "deleted from the list and the file: {sql}");
        drop(vault);
        let mut vault = fixture.open();
        assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::Opened));
        assert_eq!(listed(&vault), [("https://two.example".to_owned(), "fixture".to_owned())], "{sql}");
    }
}

#[test]
fn a_damaged_never_save_row_matches_no_site_and_the_rest_of_the_list_works() {
    let fixture = Fixture::new();
    let vault = fixture.open();
    for origin in ["https://login.example", "https://other.example", "https://third.example"] { vault.never_save(PROFILE_A, origin).unwrap(); }
    drop(vault);
    damage(&fixture, "UPDATE never_save SET origin = CAST(origin AS BLOB) WHERE origin = 'https://other.example'", "");
    damage(&fixture, "UPDATE never_save SET created_ms = 'x' WHERE origin = 'https://third.example'", "");
    damage(&fixture, &format!("INSERT INTO never_save VALUES ('{PROFILE_A}', CAST(x'ff' AS TEXT), 0)"), "");
    damage(&fixture, &format!("INSERT INTO never_save VALUES ('{PROFILE_A}', x'05', 0)"), "");
    damage(&fixture, "INSERT INTO never_save VALUES (CAST(x'ff' AS TEXT), 'https://login.example', 0)", "");
    let vault = fixture.open();
    assert_eq!(vault.never_saved(PROFILE_A).unwrap(), ["https://login.example", "https://third.example"], "only readable rows are listed");
    assert!(vault.is_never_saved(PROFILE_A, "https://login.example").unwrap());
    assert!(!vault.is_never_saved(PROFILE_A, "https://other.example").unwrap(), "a retyped row matches no site");
    vault.never_save(PROFILE_A, "https://other.example").unwrap();
    assert!(vault.is_never_saved(PROFILE_A, "https://other.example").unwrap(), "and can be added again");
    vault.remove_never_save(PROFILE_A, "https://third.example").unwrap();
    assert_eq!(vault.never_saved(PROFILE_A).unwrap(), ["https://login.example", "https://other.example"]);
}

/// The file with its WAL folded in and removed, as one byte image.
fn checkpointed(fixture: &Fixture) -> Vec<u8> {
    let conn = rusqlite::Connection::open(fixture.dir.path().join(FILE_NAME)).unwrap();
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())).unwrap();
    drop(conn);
    for suffix in ["-wal", "-shm"] { let _ = std::fs::remove_file(fixture.dir.path().join(format!("{FILE_NAME}{suffix}"))); }
    std::fs::read(fixture.dir.path().join(FILE_NAME)).unwrap()
}

const DOTLESS_ORIGIN: &str = "Enter the full site address, like bank.example.";

/// The third recheck's V9 gap: a scheme word with its colon and one slash
/// missing became the host. Without a correct http:// or https://, a host
/// needs a dot, unless it is localhost or an IP literal.
#[test]
fn a_dotless_host_needs_an_explicit_web_scheme() {
    for typed in [
        "ttps/bank.example", "htps/bank.example", "htp/bank.example", "ttp/bank.example", "ttps\\bank.example", "htps\\bank.example", "HTPS/Bank.example",
        "router", "intranet:8080", "printer/admin", "nas:5000/login", "münchen",
    ] {
        assert_eq!(normalize_origin(typed), Err(VaultError::Invalid(DOTLESS_ORIGIN)), "{typed}");
    }
    // The earlier refusals keep their own message.
    for typed in ["https_", "https-", "http-", "https.", "https", "https://https-", "https://http_/x", "https:/\\bank.example", "hhttps//bank.example", "https:bank.example:443"] {
        assert_eq!(normalize_origin(typed), Err(VaultError::Invalid(MISTYPED_ORIGIN)), "{typed}");
    }
    for (typed, origin) in [
        ("bank.example", "https://bank.example"), ("https://router", "https://router"), ("http://router", "http://router"), ("HTTP://Router:8080/admin", "http://router:8080"),
        ("localhost:8080", "https://localhost:8080"), ("localhost", "https://localhost"), ("LOCALHOST:3000/login", "https://localhost:3000"),
        ("127.0.0.1:8080", "https://127.0.0.1:8080"), ("[::1]:8443", "https://[::1]:8443"), ("http://[::1]", "http://[::1]"), ("http://10.0.0.1", "http://10.0.0.1"),
        ("münchen.example", "https://xn--mnchen-3ya.example"), ("https-portal.example", "https://https-portal.example"),
        // A residual: a typo that leaves a dot before the first slash is a host as typed.
        ("www.htps/bank.example", "https://www.htps"),
    ] {
        assert_eq!(normalize_origin(typed).as_deref(), Ok(origin), "{typed}");
    }
}

// -- round 5: damage is found by checking the file, and Reset is verified ---------------------

/// Two tagged entries, the second one used, and a never-save row, the vault
/// closed: the layout the fourth recheck's probes damaged.
fn probed_vault() -> (Fixture, String, [String; 2]) {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let ids = [
        vault.add(&tagged("https://one.example", "fixture", PASSWORD_A, PROFILE_A)).unwrap(),
        vault.add(&tagged("https://two.example", "fixture", PASSWORD_B, PROFILE_A)).unwrap(),
    ];
    vault.mark_used(&ids[1], 1_000).unwrap();
    vault.never_save(PROFILE_A, "https://login.example").unwrap();
    let id = vault.id().to_owned();
    drop(vault);
    (fixture, id, ids)
}

/// A second connection, as another tool or a damaged disk would change the file.
fn raw(fixture: &Fixture) -> rusqlite::Connection {
    rusqlite::Connection::open(fixture.dir.path().join(FILE_NAME)).unwrap()
}

/// One flipped bit of the entries table's root page number makes it its own
/// index's page. Written through the schema, as the reviewer's page probe did.
fn on_its_own_index() -> String {
    "PRAGMA writable_schema = ON;
     UPDATE sqlite_master SET rootpage = (SELECT rootpage FROM sqlite_master WHERE name = 'sqlite_autoindex_entries_1') WHERE name = 'entries';
     PRAGMA writable_schema = OFF;".into()
}

fn write_image(dir: &Path, bytes: &[u8]) {
    for suffix in ["-wal", "-shm", "-journal"] { let _ = std::fs::remove_file(dir.join(format!("{FILE_NAME}{suffix}"))); }
    std::fs::write(dir.join(FILE_NAME), bytes).unwrap();
}

/// The files replacements moved aside, by name.
fn moved_aside(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|file| file.unwrap().file_name().to_string_lossy().into_owned()).filter(|name| name.starts_with("vault-damaged-")).collect();
    names.sort();
    names
}

/// The vault in `dir` takes a new entry that opens again after a relaunch.
fn works_after(dir: &Path, keys: &MemoryVaultKeyStore) -> Result<(), String> {
    let mut vault = Vault::open(dir, Box::new(keys.clone())).map_err(|error| format!("reopen {error:?}"))?;
    vault.unlock(Moment::now()).map_err(|error| format!("unlock {error:?}"))?;
    let id = vault.add(&tagged("https://after.example", "fixture", PASSWORD_A, PROFILE_A)).map_err(|error| format!("add {error:?}"))?;
    drop(vault);
    let mut vault = Vault::open(dir, Box::new(keys.clone())).map_err(|error| format!("relaunch {error:?}"))?;
    vault.unlock(Moment::now()).map_err(|error| format!("relaunch unlock {error:?}"))?;
    match vault.password(&id) {
        Ok(password) if password.as_str() == PASSWORD_A => Ok(()),
        _ => Err("the new entry doesn't open after a relaunch".into()),
    }
}

/// The app's recovery, as `Passwords` runs it: open; unlock; for an error
/// Reset resolves, Reset and unlock again; a file that can't be opened, or
/// that Reset leaves damaged, moved aside for a new vault, which must then
/// work. Ok names the outcome. Err is a dead end: a storage error, a Reset
/// loop, entries or a never-save row the table holds gone without an error,
/// or a new vault that doesn't work.
fn through_recovery(dir: &Path, keys: &MemoryVaultKeyStore, original_id: &str, entries: usize) -> Result<String, String> {
    let replace = |stage: String| -> Result<String, String> {
        let name = Vault::replace_damaged_file(dir).map_err(|error| format!("{stage}: replacing it gave {error:?}"))?;
        if !moved_aside(dir).contains(&name) { return Err(format!("{stage}: {name} is not in the folder")); }
        works_after(dir, keys).map_err(|error| format!("{stage}: moved aside, then {error}"))?;
        Ok(format!("{stage}: moved aside"))
    };
    let mut vault = match Vault::open(dir, Box::new(keys.clone())) {
        Err(VaultError::Damaged) => return replace("damaged at open".into()),
        Err(error) => return Err(format!("open {error:?}")),
        Ok(vault) => vault,
    };
    // The list is plaintext without a seal: a flip in its row's text makes it
    // another profile's or an unreadable site, which no check can tell. A
    // readable row the table still holds for this profile must be listed,
    // whatever its index says.
    let mut note = "";
    match vault.never_saved(PROFILE_A) {
        Err(error) if !error.needs_reset() => return Err(format!("never-save list {error:?}")),
        Ok(list) if list.is_empty() => {
            let held = rusqlite::Connection::open(dir.join(FILE_NAME)).and_then(|conn| {
                let mut stmt = conn.prepare("SELECT CAST(origin AS BLOB) FROM never_save NOT INDEXED WHERE profile = ?1 AND typeof(origin) = 'text'")?;
                let rows = stmt.query_map([PROFILE_A], |r| r.get::<_, Vec<u8>>(0))?;
                rows.map(|row| row.map(|origin| std::str::from_utf8(&origin).is_ok())).collect::<Result<Vec<bool>, _>>()
            });
            if held.is_ok_and(|held| held.contains(&true)) { return Err("the never-save list is empty while the table holds its row".into()); }
            note = ", its never-save row's text changed";
        }
        _ => {}
    }
    match vault.unlock(Moment::now()) {
        Ok(outcome) => {
            let listed = vault.entries().len();
            if listed < entries { return Err(format!("unlock {outcome:?} lists {listed} of {entries} entries without an error")); }
            if vault.id() != original_id { return Err(format!("unlock {outcome:?} under a new vault id")); }
            let damaged = vault.entries().iter().filter(|entry| !entry.readable()).count().min(2);
            Ok(format!("unlock {outcome:?}, {damaged}{} damaged{note}", if damaged == 2 { " or more" } else { "" }))
        }
        Err(error) if error.needs_reset() => match vault.reset() {
            Err(VaultError::Damaged) => {
                drop(vault);
                replace(format!("unlock {error:?}, Reset found the file still damaged"))
            }
            Err(again) => Err(format!("unlock {error:?}, Reset {again:?}")),
            Ok(()) => {
                vault.unlock(Moment::now()).map_err(|again| format!("unlock {error:?}, Reset, unlock {again:?}: a Reset loop"))?;
                if !vault.never_saved(PROFILE_A).is_ok_and(|list| list.is_empty()) { return Err(format!("unlock {error:?}, Reset kept the never-save list")); }
                drop(vault);
                works_after(dir, keys).map_err(|again| format!("unlock {error:?}, Reset, then {again}"))?;
                Ok(format!("unlock {error:?}, Reset{note}"))
            }
        },
        Err(error) => Err(format!("unlock {error:?}")),
    }
}

/// Q1: a schema without a column this build reads, a renamed column, a
/// dropped or added table, index or column. Each is damage at open, found by
/// comparing the schema with the format's, and opening writes nothing: the
/// file moved aside is the file as damaged.
#[test]
fn a_schema_other_than_the_formats_is_damage_at_open_and_opening_changes_nothing() {
    for (case, sql) in [
        ("used_ms dropped", "ALTER TABLE entries DROP COLUMN used_ms".to_string()),
        ("profile dropped", "ALTER TABLE entries DROP COLUMN profile".into()),
        ("origin renamed", "ALTER TABLE entries RENAME COLUMN origin TO nrigin".into()),
        ("notes renamed", "ALTER TABLE entries RENAME COLUMN notes TO nktes".into()),
        ("never_save.origin renamed", "ALTER TABLE never_save RENAME COLUMN origin TO nrigin".into()),
        ("entries dropped", "DROP TABLE entries".into()),
        ("never_save dropped", "DROP TABLE never_save".into()),
        ("a column added", "ALTER TABLE entries ADD COLUMN extra BLOB".into()),
        ("a table added", "CREATE TABLE extra (x)".into()),
        ("an index added", "CREATE INDEX extra ON entries (created_ms)".into()),
        ("meta's value column renamed", "ALTER TABLE meta RENAME COLUMN v TO w".into()),
        ("entries on its own index's page", on_its_own_index()),
    ] {
        let (fixture, id, _) = probed_vault();
        raw(&fixture).execute_batch(&sql).unwrap();
        let image = checkpointed(&fixture);
        let error = Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).unwrap_err();
        assert_eq!(error, VaultError::Damaged, "{case}");
        assert_eq!(std::fs::read(fixture.dir.path().join(FILE_NAME)).unwrap(), image, "{case}: opening wrote nothing");
        assert_eq!(through_recovery(fixture.dir.path(), &fixture.keys, &id, 2).as_deref(), Ok("damaged at open: moved aside"), "{case}");
        let moved = moved_aside(fixture.dir.path());
        assert_eq!(std::fs::read(fixture.dir.path().join(&moved[0])).unwrap(), image, "{case}: moved aside whole");
    }
}

/// Q1, ported from the reviewer's probe: one flipped bit of a column name in
/// the entries table's CREATE text. Damage at open, never a storage error;
/// the moved-aside file still holds every entry, which open again once the
/// bit is restored, with the key Reset kept.
#[test]
fn one_flipped_bit_in_a_column_name_is_damage_and_the_moved_file_keeps_every_entry() {
    for (word, flipped) in [("origin BLOB", "nrigin BLOB"), ("profile BLOB", "profilg BLOB"), ("used_ms INTEGER", "used_mr INTEGER"), ("notes BLOB", "nktes BLOB")] {
        let (fixture, id, ids) = probed_vault();
        let mut image = checkpointed(&fixture);
        let at = image.windows(word.len()).position(|window| window == word.as_bytes()).expect("the schema text");
        let changed: Vec<(usize, u8)> = word.bytes().zip(flipped.bytes()).enumerate().filter(|(_, (a, b))| a != b).map(|(i, (a, b))| (at + i, a ^ b)).collect();
        assert!(changed.len() == 1 && changed[0].1.count_ones() == 1, "one bit: {word}");
        let (offset, mask) = changed[0];
        image[offset] ^= mask;
        write_image(fixture.dir.path(), &image);
        assert_eq!(Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).unwrap_err(), VaultError::Damaged, "{word}");
        assert_eq!(through_recovery(fixture.dir.path(), &fixture.keys, &id, 2).as_deref(), Ok("damaged at open: moved aside"), "{word}");
        let mut kept = std::fs::read(fixture.dir.path().join(&moved_aside(fixture.dir.path())[0])).unwrap();
        kept[offset] ^= mask;
        let elsewhere = Fixture { dir: tempfile::tempdir().unwrap(), keys: fixture.keys.clone() };
        write_image(elsewhere.dir.path(), &kept);
        let restored = elsewhere.unlocked();
        assert_eq!(restored.id(), id);
        assert_eq!((restored.password(&ids[0]).unwrap().as_str(), restored.password(&ids[1]).unwrap().as_str()), (PASSWORD_A, PASSWORD_B), "{word}");
    }
}

/// Q3: a file that lost a table or both its format and vault id rows is
/// damage at open. Opening creates no table and no vault id in it, and the
/// vault never opens empty over entries it can't see.
#[test]
fn a_file_that_lost_a_table_or_its_meta_rows_is_damage_and_never_given_a_new_id() {
    let flip = |page: &'static str, offset: usize, bit: u8, was: u8| move |fixture: &Fixture| -> Vec<u8> {
        let mut image = checkpointed(fixture);
        let start = if page == "schema" { 0 } else {
            let (root, size): (i64, i64) = raw(fixture).query_row("SELECT (SELECT rootpage FROM sqlite_master WHERE name = ?1), (SELECT page_size FROM pragma_page_size)", [page], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            ((root - 1) * size) as usize
        };
        assert_eq!(image[start + offset], was, "{page} byte {offset}");
        image[start + offset] ^= 1 << bit;
        image
    };
    let sql = |sql: &'static str| move |fixture: &Fixture| -> Vec<u8> {
        raw(fixture).execute_batch(sql).unwrap();
        checkpointed(fixture)
    };
    let cases: Vec<(&str, Box<dyn Fn(&Fixture) -> Vec<u8>>)> = vec![
        ("format and vault id rows deleted", Box::new(sql("DELETE FROM meta WHERE k IN ('format', 'vault_id')"))),
        ("meta dropped", Box::new(sql("DROP TABLE meta"))),
        ("format 1 layout without its format and id rows", Box::new(sql("ALTER TABLE entries DROP COLUMN used_ms; ALTER TABLE entries DROP COLUMN profile; DROP TABLE never_save; DELETE FROM meta WHERE k IN ('format', 'vault_id')"))),
        ("format 1 without its entries table", Box::new(sql("DROP TABLE entries; DROP TABLE never_save; UPDATE meta SET v = 1 WHERE k = 'format'"))),
        // Page 1's cell count, 10 schema rows read as 2: all but two vanish.
        ("page 1's cell count", Box::new(flip("schema", 104, 3, 10))),
        // The entries page's own cell count, 2 read as 0.
        ("the entries page's cell count", Box::new(flip("entries", 4, 1, 2))),
    ];
    for (case, damage) in cases {
        let (fixture, id, _) = probed_vault();
        let image = damage(&fixture);
        write_image(fixture.dir.path(), &image);
        assert_eq!(Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).unwrap_err(), VaultError::Damaged, "{case}");
        assert_eq!(std::fs::read(fixture.dir.path().join(FILE_NAME)).unwrap(), image, "{case}: no table or id was created");
        assert_eq!(through_recovery(fixture.dir.path(), &fixture.keys, &id, 2).as_deref(), Ok("damaged at open: moved aside"), "{case}");
    }
    // Only the format row lost from a format-2 file is damage too (R5-5): see
    // a_format_2_file_without_its_format_row_or_never_save_table_is_damage_never_an_upgrade.
}

/// Q2: Reset succeeds only on a file it checked afterwards: reopened, the
/// schema compared, quick_check run and every row read as an unlock would.
/// Damage the reset didn't reach makes it report Damaged, and the recovery
/// moves the file aside for a new vault instead of offering Reset again.
#[test]
fn a_reset_that_leaves_the_file_damaged_says_so_and_the_file_is_moved_aside() {
    let (fixture, id, _) = probed_vault();
    let (root, size): (i64, i64) = raw(&fixture).query_row("SELECT (SELECT rootpage FROM sqlite_master WHERE name = 'never_save'), (SELECT page_size FROM pragma_page_size)", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    let path = fixture.dir.path().join(FILE_NAME);
    let mut vault = fixture.open();
    assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::Opened));
    // A page type no b-tree page has, written after the reset's commit.
    super::db::AFTER_RESET.set(Some(Box::new(move || {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(((root - 1) * size) as u64)).unwrap();
        file.write_all(&[0xff]).unwrap();
    })));
    let result = vault.reset();
    super::db::AFTER_RESET.set(None);
    assert_eq!(result, Err(VaultError::Damaged), "never reported as reset");
    drop(vault);
    let name = Vault::replace_damaged_file(fixture.dir.path()).unwrap();
    assert_eq!(moved_aside(fixture.dir.path()), [name.clone()]);
    works_after(fixture.dir.path(), &fixture.keys).unwrap();
    assert!(fixture.keys.contains(&id), "the old key is kept, as Reset keeps it");
    // An undamaged reset is checked the same way and succeeds.
    let (fixture, _, _) = probed_vault();
    let mut vault = fixture.unlocked();
    vault.reset().unwrap();
    assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::Opened));
    assert!(moved_aside(fixture.dir.path()).is_empty());
}

/// A file moved aside keeps its WAL and journal under its own name, readable
/// only by this user, and two replacements in the same second never collide.
#[test]
fn a_damaged_file_is_moved_aside_whole_under_a_dated_private_name() {
    let fixture = Fixture::new();
    std::fs::write(fixture.dir.path().join(FILE_NAME), vec![0x5a; 8192]).unwrap();
    std::fs::write(fixture.dir.path().join(format!("{FILE_NAME}-wal")), b"fixture-wal").unwrap();
    std::fs::write(fixture.dir.path().join(format!("{FILE_NAME}-shm")), b"fixture-shm").unwrap();
    let first = Vault::replace_damaged_file(fixture.dir.path()).unwrap();
    let stamp = first.strip_prefix("vault-damaged-").and_then(|rest| rest.strip_suffix(".sqlite")).unwrap();
    assert!(stamp.len() == 16 && stamp.as_bytes()[8] == b'T' && stamp.ends_with('Z') && stamp.chars().filter(char::is_ascii_digit).count() == 14, "{first}");
    assert_eq!(std::fs::read(fixture.dir.path().join(&first)).unwrap(), vec![0x5a; 8192]);
    assert_eq!(std::fs::read(fixture.dir.path().join(format!("{first}-wal"))).unwrap(), b"fixture-wal", "the WAL goes with it");
    let left: Vec<String> = std::fs::read_dir(fixture.dir.path()).unwrap().map(|file| file.unwrap().file_name().to_string_lossy().into_owned()).filter(|name| name.starts_with(FILE_NAME)).collect();
    assert!(left.is_empty(), "nothing of the old file is left under the vault's name: {left:?}");
    #[cfg(unix)]
    for name in [first.clone(), format!("{first}-wal")] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(fixture.dir.path().join(&name)).unwrap().permissions().mode() & 0o777, 0o600, "{name}");
    }
    std::fs::write(fixture.dir.path().join(FILE_NAME), vec![0x5b; 8192]).unwrap();
    let second = Vault::replace_damaged_file(fixture.dir.path()).unwrap();
    assert_ne!(second, first);
    assert_eq!(std::fs::read(fixture.dir.path().join(&second)).unwrap(), vec![0x5b; 8192]);
    assert_eq!(std::fs::read(fixture.dir.path().join(&first)).unwrap(), vec![0x5a; 8192], "the first is untouched");
    assert_eq!(Vault::replace_damaged_file(fixture.dir.path()), Err(VaultError::Storage), "no file: nothing to move aside");
}

/// Item 5: Reset empties the never-save list too, as its confirmation's
/// "a new, empty vault" says.
#[test]
fn reset_clears_the_never_save_list_too() {
    let (fixture, _, _) = probed_vault();
    let mut vault = fixture.unlocked();
    assert_eq!(vault.never_saved(PROFILE_A).unwrap(), ["https://login.example"]);
    vault.reset().unwrap();
    assert!(vault.never_saved(PROFILE_A).unwrap().is_empty());
    drop(vault);
    assert!(fixture.open().never_saved(PROFILE_A).unwrap().is_empty(), "after a relaunch too");
}

/// Bytes of a b-tree page SQLite reads: header, cell pointers and cells,
/// not the zeroed gap between them. Page 1's header follows the file header.
fn read_bytes(image: &[u8], page_size: usize, page: usize) -> Vec<usize> {
    let start = (page - 1) * page_size;
    let header = start + if page == 1 { 100 } else { 0 };
    let header_len = if matches!(image[header], 0x02 | 0x05) { 12 } else { 8 };
    let cells = u16::from_be_bytes([image[header + 3], image[header + 4]]) as usize;
    let content = u16::from_be_bytes([image[header + 5], image[header + 6]]) as usize;
    let gap = header + header_len + 2 * cells..start + if content == 0 { 65536 } else { content };
    (start..start + page_size).filter(|offset| !gap.contains(offset)).collect()
}

/// Q2, the reviewer's sweep with the recovery run: every single-bit flip of
/// every byte SQLite reads, on every page of the file (the schema page, each
/// table and each index), opened, unlocked, and for an error Reset offers,
/// reset and unlocked again, or moved aside, to a working vault. None ends in
/// a storage error, a Reset loop or entries gone without an error.
#[test]
fn every_single_bit_flip_of_the_file_recovers_through_reset_and_unlock() {
    let (fixture, id, _) = probed_vault();
    let original = checkpointed(&fixture);
    let conn = raw(&fixture);
    let page_size = conn.query_row("PRAGMA page_size", [], |r| r.get::<_, i64>(0)).unwrap() as usize;
    let mut pages: Vec<(String, usize)> = vec![("schema".into(), 1)];
    let mut stmt = conn.prepare("SELECT name, rootpage FROM sqlite_master ORDER BY rootpage").unwrap();
    pages.extend(stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))).unwrap().map(Result::unwrap));
    drop(stmt);
    drop(conn);
    assert_eq!(pages.len() * page_size, original.len(), "every page of the file is swept: {pages:?}");
    let work = Fixture { dir: tempfile::tempdir().unwrap(), keys: fixture.keys.clone() };
    let started = std::time::Instant::now();
    let (mut flips, mut tally, mut dead_ends) = (0, std::collections::BTreeMap::<String, usize>::new(), std::collections::BTreeMap::<String, (usize, String)>::new());
    for (name, page) in &pages {
        for offset in read_bytes(&original, page_size, *page) {
            for bit in 0..8 {
                flips += 1;
                let mut bytes = original.clone();
                bytes[offset] ^= 1 << bit;
                write_image(work.dir.path(), &bytes);
                match through_recovery(work.dir.path(), &work.keys, &id, 2) {
                    Ok(outcome) => *tally.entry(format!("{name}: {outcome}")).or_default() += 1,
                    Err(dead_end) => dead_ends.entry(format!("{name}: {dead_end}")).or_insert((0, format!("byte {offset} bit {bit}"))).0 += 1,
                }
                for moved in moved_aside(work.dir.path()) { std::fs::remove_file(work.dir.path().join(moved)).unwrap(); }
            }
        }
    }
    eprintln!("{flips} single-bit flips in {:?}, by outcome: {tally:#?}", started.elapsed());
    assert!(dead_ends.is_empty(), "{} of {flips} flips end in a dead end: {dead_ends:#?}", dead_ends.values().map(|(n, _)| n).sum::<usize>());
}

/// Q4: a read whose entry's row is gone tells a Reset in another window
/// from a plain delete there, as a write does, so the page can handle it.
#[test]
fn a_read_of_a_row_another_handle_reset_away_reports_the_reset() {
    let (fixture, _, ids) = probed_vault();
    let mut vault = fixture.unlocked();
    fixture.open().reset().unwrap();
    assert_eq!(vault.password(&ids[0]).unwrap_err(), VaultError::ResetElsewhere);
    assert_eq!(vault.secret(&ids[1]).unwrap_err(), VaultError::ResetElsewhere);
    vault.lock();
    vault.unlock(Moment::now()).unwrap();
    assert!(vault.entries().is_empty(), "unlocking again follows the reset");
    let (fixture, _, ids) = probed_vault();
    let vault = fixture.unlocked();
    let mut other = fixture.unlocked();
    other.delete(&ids[0]).unwrap();
    assert_eq!(vault.password(&ids[0]).unwrap_err(), VaultError::NotFound, "a delete elsewhere is not a reset");
}

// -- round 6: SQLite's own tables, format 2 by its columns, and Recover Passwords ----------------

/// R5-4, from the review's probe: SQLite's own tables, which ANALYZE and
/// PRAGMA optimize create (sqlite_stat1, and sqlite_stat4 where SQLite was
/// built with it), aren't damage. The vault opens as it was.
#[test]
fn sqlites_own_statistics_tables_are_part_of_a_healthy_vault() {
    for sql in ["ANALYZE", "PRAGMA optimize", "PRAGMA optimize=0x10002", "VACUUM", "REINDEX", "ANALYZE; PRAGMA optimize"] {
        let (fixture, id, ids) = probed_vault();
        let conn = raw(&fixture);
        conn.execute_batch(sql).unwrap();
        let tables: Vec<String> = conn.prepare("SELECT name FROM sqlite_master WHERE name LIKE 'sqlite_stat%'").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        drop(conn);
        if sql.starts_with("ANALYZE") { assert!(tables.contains(&"sqlite_stat1".to_owned()), "{sql}: {tables:?}"); }
        let mut vault = Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).unwrap_or_else(|error| panic!("{sql} ({tables:?}): {error:?}"));
        assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::Opened), "{sql}");
        assert_eq!((vault.id(), vault.password(&ids[0]).unwrap().as_str(), vault.password(&ids[1]).unwrap().as_str()), (id.as_str(), PASSWORD_A, PASSWORD_B), "{sql}");
        assert_eq!(vault.never_saved(PROFILE_A).unwrap(), ["https://login.example"], "{sql}");
        vault.add(&tagged("https://after.example", "fixture", PASSWORD_A, PROFILE_A)).unwrap();
    }
}

/// R5-5 and Q3: a file whose entries table has a column format 2 added is
/// format 2, whatever it lost. Without its format row or its never_save
/// table it is damage at open, which Recover Passwords resolves, and
/// opening it changes nothing: never an upgrade that creates an empty list.
#[test]
fn a_format_2_file_without_its_format_row_or_never_save_table_is_damage_never_an_upgrade() {
    for (case, sql) in [
        ("format row deleted", "DELETE FROM meta WHERE k = 'format'"),
        ("format row and never_save dropped", "DELETE FROM meta WHERE k = 'format'; DROP TABLE never_save"),
        ("format 1 with format 2's columns", "UPDATE meta SET v = 1 WHERE k = 'format'; DROP TABLE never_save"),
        ("format 1 with format 2's profile column only", "UPDATE meta SET v = 1 WHERE k = 'format'; DROP TABLE never_save; ALTER TABLE entries DROP COLUMN used_ms"),
        ("format 1 with never_save", "UPDATE meta SET v = 1 WHERE k = 'format'; ALTER TABLE entries DROP COLUMN used_ms; ALTER TABLE entries DROP COLUMN profile"),
    ] {
        let (fixture, _, _) = probed_vault();
        raw(&fixture).execute_batch(sql).unwrap();
        let image = checkpointed(&fixture);
        assert_eq!(Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).map(|_| ()), Err(VaultError::Damaged), "{case}");
        assert_eq!(std::fs::read(fixture.dir.path().join(FILE_NAME)).unwrap(), image, "{case}: opening wrote nothing");
    }
}

/// Which of the fixture's two passwords the vault in `dir` opens, unlocked
/// with `keys`, and correct. Zero when it can't be opened or unlocked.
fn passwords_held(dir: &Path, keys: &MemoryVaultKeyStore, ids: &[String; 2]) -> usize {
    let Ok(mut vault) = Vault::open(dir, Box::new(keys.clone())) else { return 0 };
    if vault.unlock(Moment::now()).is_err() { return 0; }
    [PASSWORD_A, PASSWORD_B].iter().zip(ids).filter(|(want, id)| vault.password(id).is_ok_and(|got| got.as_str() == **want)).count()
}

/// What the file's entries still hold without SQLite's structure check: the
/// most any recovery could get, as the review measured it.
fn held_without_structure_check(dir: &Path, keys: &MemoryVaultKeyStore, ids: &[String; 2]) -> usize {
    super::db::SKIP_STRUCTURE.set(true);
    let held = passwords_held(dir, keys, ids);
    super::db::SKIP_STRUCTURE.set(false);
    held
}

fn files_named(dir: &Path, prefix: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|file| file.unwrap().file_name().to_string_lossy().into_owned()).filter(|name| name.starts_with(prefix)).collect();
    names.sort();
    names
}

/// R5-3: Recover Passwords copies every entry that opens and verifies into
/// a new file with the same vault id, check and key, swaps it in, and moves
/// the damaged file aside unchanged. The entries keep their profile tags,
/// times and last use, and the never-save list is kept.
#[test]
fn recover_copies_every_entry_that_opens_into_a_new_file_under_the_same_key() {
    let (fixture, id, ids) = probed_vault();
    let dir = fixture.dir.path();
    let check: Vec<u8> = raw(&fixture).query_row("SELECT v FROM meta WHERE k = 'check'", [], |r| r.get(0)).unwrap();
    // Damage at open whose entries are intact: a renamed column.
    raw(&fixture).execute_batch("ALTER TABLE entries RENAME COLUMN origin TO nrigin").unwrap();
    let image = checkpointed(&fixture);
    assert_eq!(Vault::open(dir, Box::new(fixture.keys.clone())).unwrap_err(), VaultError::Damaged);
    assert!(Vault::can_recover(dir, &fixture.keys));
    let recovery = Vault::recover_damaged_file(dir, &fixture.keys).unwrap();
    assert_eq!((recovery.recovered, recovery.found), (2, 2));
    // With the empty WAL the failed open left beside it.
    assert!(moved_aside(dir).iter().all(|name| name.starts_with(&recovery.moved)), "{:?}", moved_aside(dir));
    assert_eq!(std::fs::read(dir.join(&recovery.moved)).unwrap(), image, "moved aside as it was");
    assert!(files_named(dir, "vault-recovery").is_empty(), "no working file is left");
    let vault = fixture.unlocked();
    assert_eq!(vault.id(), id);
    assert_eq!(raw(&fixture).query_row("SELECT v FROM meta WHERE k = 'check'", [], |r| r.get::<_, Vec<u8>>(0)).unwrap(), check, "the same check");
    assert_eq!((vault.password(&ids[0]).unwrap().as_str(), vault.password(&ids[1]).unwrap().as_str()), (PASSWORD_A, PASSWORD_B));
    let entries: Vec<(&str, &str, Option<&str>, Option<u64>)> = vault.entries().iter().map(|e| (e.id.as_str(), e.origin.as_str(), e.profile.as_deref(), e.used_ms)).collect();
    assert_eq!(entries, [(ids[0].as_str(), "https://one.example", Some(PROFILE_A), None), (ids[1].as_str(), "https://two.example", Some(PROFILE_A), Some(1_000))]);
    assert_eq!(vault.never_saved(PROFILE_A).unwrap(), ["https://login.example"]);
    drop(vault);
    works_after(dir, &fixture.keys).unwrap();
}

/// R5-3: Recover needs the vault's key and a readable vault id, and the
/// damaged file is only ever read: without either nothing changes, and
/// Move Aside and Reset stays the way out.
#[test]
fn recover_needs_the_key_and_the_vault_id_and_never_writes_the_damaged_file() {
    let (fixture, _, _) = probed_vault();
    let dir = fixture.dir.path();
    raw(&fixture).execute_batch("DELETE FROM meta WHERE k = 'format'").unwrap();
    let image = checkpointed(&fixture);
    let stranger = MemoryVaultKeyStore::default();
    assert!(!Vault::can_recover(dir, &stranger));
    assert_eq!(Vault::recover_damaged_file(dir, &stranger), Err(VaultError::MissingKey));
    assert_eq!(std::fs::read(dir.join(FILE_NAME)).unwrap(), image, "unchanged");
    assert!(moved_aside(dir).is_empty() && files_named(dir, "vault-recovery").is_empty());
    raw(&fixture).execute_batch("DELETE FROM meta WHERE k = 'vault_id'").unwrap();
    let image = checkpointed(&fixture);
    assert!(!Vault::can_recover(dir, &fixture.keys));
    assert_eq!(Vault::recover_damaged_file(dir, &fixture.keys), Err(VaultError::Damaged), "no vault id names the key");
    assert_eq!(std::fs::read(dir.join(FILE_NAME)).unwrap(), image, "unchanged");
}

/// R5-3: an entry whose sealed cells no longer open with the key, or whose
/// cells moved to another entry, is left out; the rest are recovered.
#[test]
fn recover_leaves_out_an_entry_that_no_longer_verifies() {
    let (fixture, id, ids) = probed_vault();
    let dir = fixture.dir.path();
    let conn = raw(&fixture);
    // One byte of the first entry's password ciphertext, and the profile tag moved to another entry.
    conn.execute("UPDATE entries SET password = CAST(password AS BLOB) || X'00' WHERE id = ?1", [&ids[0]]).unwrap();
    conn.execute_batch("UPDATE meta SET v = 1 WHERE k = 'format'").unwrap();
    drop(conn);
    assert_eq!(Vault::open(dir, Box::new(fixture.keys.clone())).unwrap_err(), VaultError::Damaged);
    let recovery = Vault::recover_damaged_file(dir, &fixture.keys).unwrap();
    assert_eq!((recovery.recovered, recovery.found), (1, 2));
    let vault = fixture.unlocked();
    assert_eq!(vault.id(), id);
    assert_eq!(vault.entries().iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), [ids[1].as_str()]);
    assert_eq!(vault.password(&ids[1]).unwrap().as_str(), PASSWORD_B);
}

/// R5-3, with the latest writes still in the damaged file's WAL: they are
/// read, and the WAL goes aside with the file, never replayed into the new one.
#[test]
fn recover_reads_the_damaged_files_wal_and_moves_it_aside_with_it() {
    let (copy, [first, second], main, wal) = damaged_in_its_wal();
    let dir = copy.dir.path();
    // Not opened first: a failed open's connection would fold the WAL into the file as it closes.
    let recovery = Vault::recover_damaged_file(dir, &copy.keys).unwrap();
    assert_eq!((recovery.recovered, recovery.found), (2, 2));
    assert_eq!(std::fs::read(dir.join(format!("{}-wal", recovery.moved))).unwrap(), wal, "the WAL went aside with its file");
    assert_eq!(std::fs::read(dir.join(&recovery.moved)).unwrap(), main, "and the file as it was");
    assert!(files_named(dir, FILE_NAME).iter().all(|name| name == FILE_NAME), "nothing of the old file under the vault's name");
    assert_eq!(passwords_held(dir, &copy.keys, &[first, second]), 2);
}

/// A vault of two entries whose latest entry and damage at open (a renamed
/// column) are still in its WAL, copied to a folder of its own, never opened
/// there. The copy, the entries' ids, and the file and WAL as written.
fn damaged_in_its_wal() -> (Fixture, [String; 2], Vec<u8>, Vec<u8>) {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let first = vault.add(&tagged("https://one.example", "fixture", PASSWORD_A, PROFILE_A)).unwrap();
    // Held open, so closing the vault leaves its WAL in place.
    let holder = raw(&fixture);
    holder.execute_batch("PRAGMA wal_autocheckpoint = 0").unwrap();
    let second = vault.add(&tagged("https://two.example", "fixture", PASSWORD_B, PROFILE_A)).unwrap();
    drop(vault);
    holder.execute_batch("ALTER TABLE entries RENAME COLUMN notes TO nktes").unwrap();
    let copy = Fixture { dir: tempfile::tempdir().unwrap(), keys: fixture.keys.clone() };
    let wal = std::fs::read(fixture.dir.path().join(format!("{FILE_NAME}-wal"))).unwrap();
    assert!(!wal.is_empty(), "the writes are in the WAL");
    std::fs::copy(fixture.dir.path().join(FILE_NAME), copy.dir.path().join(FILE_NAME)).unwrap();
    std::fs::write(copy.dir.path().join(format!("{FILE_NAME}-wal")), &wal).unwrap();
    drop(holder);
    let main = std::fs::read(copy.dir.path().join(FILE_NAME)).unwrap();
    (copy, [first, second], main, wal)
}

/// A file macOS lets no one rename or change while the guard lives.
#[cfg(target_os = "macos")]
struct Immutable(std::path::PathBuf);

#[cfg(target_os = "macos")]
impl Immutable {
    fn set(path: &Path) -> Immutable {
        assert!(std::process::Command::new("/usr/bin/chflags").arg("uchg").arg(path).status().unwrap().success());
        Immutable(path.to_owned())
    }
}

#[cfg(target_os = "macos")]
impl Drop for Immutable {
    fn drop(&mut self) {
        let _ = std::process::Command::new("/usr/bin/chflags").arg("nouchg").arg(&self.0).status();
    }
}

/// R7-2, the seventh recheck's K7: Recover links the damaged file under its
/// kept name first, then moves its WAL there. Here that move fails (the WAL
/// can't be renamed), so Recover fails with a storage error, and it takes
/// the kept name back: no second name of the damaged file is left that no
/// notice names. The file and its WAL stay as they were, and a retry keeps
/// the file under one name, the one it reports.
#[cfg(target_os = "macos")]
#[test]
fn a_recover_that_fails_after_its_link_leaves_no_other_name() {
    let (copy, ids, main, wal) = damaged_in_its_wal();
    let dir = copy.dir.path();
    let wal_path = dir.join(format!("{FILE_NAME}-wal"));
    let fixed = Immutable::set(&wal_path);
    assert_eq!(Vault::recover_damaged_file(dir, &copy.keys).map(|recovery| recovery.moved), Err(VaultError::Storage));
    assert!(moved_aside(dir).is_empty(), "no kept name is left: {:?}", moved_aside(dir));
    assert!(files_named(dir, "vault-recovery").is_empty(), "no working file is left");
    assert_eq!((std::fs::read(dir.join(FILE_NAME)).unwrap(), std::fs::read(&wal_path).unwrap()), (main, wal), "the file and its WAL as they were");
    drop(fixed);
    let recovery = Vault::recover_damaged_file(dir, &copy.keys).unwrap();
    assert_eq!(moved_aside(dir), [recovery.moved.clone(), format!("{}-wal", recovery.moved)], "one kept name, the one reported");
    assert_eq!(passwords_held(dir, &copy.keys, &ids), 2);
}

/// R7-2, the seventh recheck's K6: a Recover that crashed after it linked
/// the damaged file under its kept name, before or after it moved the WAL
/// there. The next open undoes those steps: the WAL goes back beside the
/// file, and the kept name and the record go, so the file opens as it did
/// before the Recover, as damaged, never silently without its WAL; the next
/// Recover then keeps it under one name, the one it reports, which a later
/// open leaves alone. Both with the WAL that holds the damage still beside
/// the file, and in the app's order, where a failed open folded it in.
#[cfg(unix)]
#[test]
fn the_next_open_undoes_a_recover_that_stopped_after_its_link() {
    for folded in [false, true] {
        for wal_moved in [false, true] {
            let case = format!("WAL folded first: {folded}; stopped after moving the WAL: {wal_moved}");
            let (copy, ids, _, _) = damaged_in_its_wal();
            let dir = copy.dir.path();
            if folded { assert_eq!(Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()), Err(VaultError::Damaged), "{case}"); }
            crash_recover(dir, &copy.keys, if wal_moved { 5 } else { 2 });
            assert_eq!(Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()), Err(VaultError::Damaged), "{case}");
            assert!(moved_aside(dir).is_empty(), "{case}: {:?}", moved_aside(dir));
            let recovery = Vault::recover_damaged_file(dir, &copy.keys).unwrap();
            assert!(moved_aside(dir).iter().all(|kept| kept.starts_with(&recovery.moved)), "{case}: {:?}", moved_aside(dir));
            assert_eq!(passwords_held(dir, &copy.keys, &ids), 2, "{case}");
            assert!(moved_aside(dir).contains(&recovery.moved), "{case}: an open keeps the kept file");
        }
    }
}

/// R7-2: the undo can itself stop part way, after it moved the WAL back
/// beside the vault file and before it removed the kept name. The next undo
/// finds that name with nothing beside it and only removes it, and the one
/// after finds nothing to do: the file and its WAL are as they were before
/// the Recover, and the next Recover keeps the file under one name. Where
/// the vault file has a WAL of its own by now, the undo never replaces it:
/// the kept name and the WAL beside it stay as they are, and the error names
/// them.
#[cfg(unix)]
#[test]
fn an_undo_that_stopped_after_moving_the_wal_back_runs_again_safely() {
    let (copy, ids, main, wal) = damaged_in_its_wal();
    let dir = copy.dir.path();
    let name = "vault-damaged-20261029T000000Z.sqlite";
    let (wal_path, moved_wal) = (dir.join(format!("{FILE_NAME}-wal")), dir.join(format!("{name}-wal")));
    std::fs::hard_link(dir.join(FILE_NAME), dir.join(name)).unwrap();
    std::fs::rename(&wal_path, &moved_wal).unwrap();
    std::fs::rename(&moved_wal, &wal_path).unwrap();
    for run in 1..=2 {
        undo_stopped_swap(dir).unwrap();
        assert!(moved_aside(dir).is_empty(), "undo {run}: {:?}", moved_aside(dir));
        assert_eq!((std::fs::read(dir.join(FILE_NAME)).unwrap(), std::fs::read(&wal_path).unwrap()), (main.clone(), wal.clone()), "undo {run}: the file and its WAL as they were");
    }
    let recovery = Vault::recover_damaged_file(dir, &copy.keys).unwrap();
    assert_eq!(moved_aside(dir), [recovery.moved.clone(), format!("{}-wal", recovery.moved)], "one kept name, the one reported");
    assert_eq!(passwords_held(dir, &copy.keys, &ids), 2);

    let (copy, _, main, wal) = damaged_in_its_wal();
    let dir = copy.dir.path();
    let wal_path = dir.join(format!("{FILE_NAME}-wal"));
    std::fs::hard_link(dir.join(FILE_NAME), dir.join(name)).unwrap();
    std::fs::copy(&wal_path, dir.join(format!("{name}-wal"))).unwrap();
    std::fs::write(&wal_path, b"the vault's own WAL").unwrap();
    // With no record, nothing shows the kept WAL belongs back (R9-2).
    assert_eq!(undo_stopped_swap(dir), Err(VaultError::UnrecordedStop(vec![name.to_owned(), format!("{name}-wal")])));
    assert_eq!(moved_aside(dir), [name.to_owned(), format!("{name}-wal")], "both names stay");
    assert_eq!((std::fs::read(dir.join(name)).unwrap(), std::fs::read(dir.join(format!("{name}-wal"))).unwrap()), (main, wal), "the kept file and its WAL");
    assert_eq!(std::fs::read(&wal_path).unwrap(), b"the vault's own WAL", "the vault's own WAL is never replaced");
}

/// R5-3, the review's numbers: of the round-5 sweep's single-bit flips,
/// those that are damage at open include 649 whose two entries are intact
/// without the structure check, 57 that still hold one password and 43 that
/// hold none. Recover gets at least what each file still holds, by an
/// ordinary read without the structure check or, when the file still names
/// its key, by a plain table scan whose sealed cells all open (R6-1: 15
/// leaf flips a scan reads that round 6 dropped), counts at least every row
/// the scan sees, and every password it returns is the right one. A row
/// whose password opens but another sealed cell doesn't is counted, not
/// recovered: the sweep tallies those.
#[test]
fn every_flip_that_is_damage_at_open_recovers_what_its_entries_still_hold() {
    let (fixture, id, ids) = probed_vault();
    let original = checkpointed(&fixture);
    let conn = raw(&fixture);
    let page_size = conn.query_row("PRAGMA page_size", [], |r| r.get::<_, i64>(0)).unwrap() as usize;
    let mut pages: Vec<(String, usize)> = vec![("schema".into(), 1)];
    let mut stmt = conn.prepare("SELECT name, rootpage FROM sqlite_master ORDER BY rootpage").unwrap();
    pages.extend(stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))).unwrap().map(Result::unwrap));
    drop(stmt);
    drop(conn);
    let work = Fixture { dir: tempfile::tempdir().unwrap(), keys: fixture.keys.clone() };
    let dir = work.dir.path();
    let (mut tally, mut partial, mut short) = (std::collections::BTreeMap::<(usize, usize), usize>::new(), std::collections::BTreeMap::<String, usize>::new(), Vec::new());
    for (name, page) in &pages {
        for offset in read_bytes(&original, page_size, *page) {
            for bit in 0..8 {
                let mut bytes = original.clone();
                bytes[offset] ^= 1 << bit;
                write_image(dir, &bytes);
                if Vault::open(dir, Box::new(work.keys.clone())).map(|_| ()) != Err(VaultError::Damaged) { continue; }
                let held = held_without_structure_check(dir, &work.keys, &ids);
                write_image(dir, &bytes);
                let scan = scan_entries(dir, &work.keys, &id, &two_passwords(&ids));
                write_image(dir, &bytes);
                let named = Vault::can_recover(dir, &work.keys);
                let held = if named { held.max(scan.whole.len()) } else { held };
                let seen = &scan.seen;
                let got = if named {
                    match Vault::recover_damaged_file(dir, &work.keys) {
                        Ok(recovery) => {
                            let got = passwords_held(dir, &work.keys, &ids);
                            if recovery.found < seen.len() { short.push(format!("{name} byte {offset} bit {bit}: counted {} of the {} rows a scan sees", recovery.found, seen.len())); }
                            if got != recovery.recovered.min(2) || Vault::open(dir, Box::new(work.keys.clone())).map(|vault| vault.id().to_owned()).as_deref() != Ok(id.as_str()) {
                                short.push(format!("{name} byte {offset} bit {bit}: said {} of {}, holds {got}", recovery.recovered, recovery.found));
                            }
                            works_after(dir, &work.keys).unwrap_or_else(|error| short.push(format!("{name} byte {offset} bit {bit}: then {error}")));
                            got
                        }
                        Err(error) => {
                            if held > 0 { short.push(format!("{name} byte {offset} bit {bit}: recover {error:?} with {held} held")); }
                            0
                        }
                    }
                } else { 0 };
                if got < held { short.push(format!("{name} byte {offset} bit {bit}: recovered {got} of the {held} it holds")); }
                if named && scan.password.len() > got { *partial.entry(format!("{name}: {} with the right password, {} whole, {got} recovered", scan.password.len(), scan.whole.len())).or_default() += 1; }
                *tally.entry((held, got)).or_default() += 1;
                for name in moved_aside(dir) { std::fs::remove_file(dir.join(name)).unwrap(); }
            }
        }
    }
    eprintln!("damage at open, by (passwords held without the structure check or read whole by a table scan, passwords recovered): {tally:?}");
    eprintln!("flips where a scan opens more passwords than Recover keeps, as another sealed cell of the row doesn't open:");
    for (outcome, n) in &partial { eprintln!("{n:5}  {outcome}"); }
    assert!(short.is_empty(), "{} flips recover less than they hold: {short:#?}", short.len());
    assert!(tally.get(&(2, 2)).is_some_and(|n| *n > 0), "the files the review counted as intact are recovered whole");
}

/// R5-3, the review's free-page case: a vault that once held a long note
/// has free pages, and a flip in the freelist, which holds no entry, is damage
/// at open. Recover gets every password back.
#[test]
fn a_flip_in_a_free_page_recovers_every_password() {
    let (fixture, id, ids) = probed_vault();
    let mut vault = fixture.unlocked();
    let long = "n".repeat(MAX_NOTES_BYTES);
    let gone = vault.add(&EntryInput { origin: "https://three.example", username: "fixture", password: PASSWORD_A, notes: &long, profile: Some(PROFILE_A) }).unwrap();
    vault.delete(&gone).unwrap();
    drop(vault);
    let original = checkpointed(&fixture);
    let trunk = u32::from_be_bytes(original[32..36].try_into().unwrap()) as usize;
    let page_size = u16::from_be_bytes([original[16], original[17]]) as usize;
    assert!(trunk > 0, "the deleted note left free pages");
    let work = Fixture { dir: tempfile::tempdir().unwrap(), keys: fixture.keys.clone() };
    let dir = work.dir.path();
    let mut damaged = 0;
    // The trunk page's next-trunk pointer, leaf count and first leaf number.
    for offset in (trunk - 1) * page_size..(trunk - 1) * page_size + 12 {
        for bit in 0..8 {
            let mut bytes = original.clone();
            bytes[offset] ^= 1 << bit;
            write_image(dir, &bytes);
            if Vault::open(dir, Box::new(work.keys.clone())).map(|_| ()) != Err(VaultError::Damaged) { continue; }
            damaged += 1;
            let recovery = Vault::recover_damaged_file(dir, &work.keys).unwrap_or_else(|error| panic!("byte {offset} bit {bit}: {error:?}"));
            assert_eq!((recovery.recovered, recovery.found), (2, 2), "byte {offset} bit {bit}");
            assert_eq!(passwords_held(dir, &work.keys, &ids), 2, "byte {offset} bit {bit}");
            assert_eq!(Vault::open(dir, Box::new(work.keys.clone())).unwrap().id(), id);
            for name in moved_aside(dir) { std::fs::remove_file(dir.join(name)).unwrap(); }
        }
    }
    eprintln!("{damaged} of 96 free-page flips are damage at open; each recovered both passwords");
    assert!(damaged > 0);
}

/// The fixture passwords of `probed_vault`, by id.
fn two_passwords(ids: &[String; 2]) -> std::collections::BTreeMap<String, String> {
    [(ids[0].clone(), PASSWORD_A.to_owned()), (ids[1].clone(), PASSWORD_B.to_owned())].into_iter().collect()
}

/// What a plain read-only scan of the entries table reads, both ways.
struct Scan {
    /// The ids it sees.
    seen: std::collections::BTreeSet<String>,
    /// Those whose password opens, bound as unlock binds it, to the one in
    /// `want`, as the sixth recheck measured it.
    password: std::collections::BTreeSet<String>,
    /// Those of them whose username, notes and profile tag open too.
    whole: std::collections::BTreeSet<String>,
}

fn scan_entries(dir: &Path, keys: &MemoryVaultKeyStore, vault_id: &str, want: &std::collections::BTreeMap<String, String>) -> Scan {
    use super::crypto::{Binding, Field};
    let conn = rusqlite::Connection::open_with_flags(dir.join(FILE_NAME), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let cipher = keys.load(vault_id).unwrap().expect("the fixture key").cipher();
    let opened = |id: &str, origin: &[u8], password: &[u8]| -> Option<String> {
        let origin = crypto::open(&cipher, &Binding { vault_id, entry_id: id, field: Field::Origin, origin: "" }, origin).ok()?;
        Some(crypto::open(&cipher, &Binding { vault_id, entry_id: id, field: Field::Password, origin: &origin }, password).ok()?.to_string())
    };
    let rest_opens = |id: &str, origin: &[u8], cells: [Option<Vec<u8>>; 3]| -> bool {
        let Ok(origin) = crypto::open(&cipher, &Binding { vault_id, entry_id: id, field: Field::Origin, origin: "" }, origin) else { return false };
        let [username, notes, profile] = cells;
        let opens = |field, sealed: &[u8]| crypto::open(&cipher, &Binding { vault_id, entry_id: id, field, origin: &origin }, sealed).is_ok();
        username.is_some_and(|cell| opens(Field::Username, &cell)) && notes.is_some_and(|cell| opens(Field::Notes, &cell)) && profile.is_none_or(|cell| opens(Field::Profile, &cell))
    };
    let mut scan = Scan { seen: Default::default(), password: Default::default(), whole: Default::default() };
    for sql in ["SELECT id, origin, password FROM entries NOT INDEXED", "SELECT id, origin, password FROM entries NOT INDEXED ORDER BY rowid DESC"] {
        let Ok(mut stmt) = conn.prepare(sql) else { continue };
        let Ok(mut rows) = stmt.query([]) else { continue };
        while let Ok(Some(row)) = rows.next() {
            let (Ok(id), Ok(origin), Ok(password)) = (row.get::<_, String>(0), row.get::<_, Vec<u8>>(1), row.get::<_, Vec<u8>>(2)) else { continue };
            if opened(&id, &origin, &password).is_some_and(|got| want.get(&id) == Some(&got)) { scan.password.insert(id.clone()); }
            scan.seen.insert(id);
        }
    }
    for sql in ["SELECT id, origin, username, notes, profile FROM entries NOT INDEXED", "SELECT id, origin, username, notes, profile FROM entries NOT INDEXED ORDER BY rowid DESC"] {
        let Ok(mut stmt) = conn.prepare(sql) else { continue };
        let Ok(mut rows) = stmt.query([]) else { continue };
        while let Ok(Some(row)) = rows.next() {
            let (Ok(id), Ok(origin)) = (row.get::<_, String>(0), row.get::<_, Vec<u8>>(1)) else { continue };
            let cells = [row.get::<_, Vec<u8>>(2).ok(), row.get::<_, Vec<u8>>(3).ok(), row.get::<_, Option<Vec<u8>>>(4).unwrap_or(Some(Vec::new()))];
            if scan.password.contains(&id) && rest_opens(&id, &origin, cells) { scan.whole.insert(id); }
        }
    }
    scan
}

/// A vault of `n` entries, enough for its table to need an interior page.
fn many(n: usize) -> (Fixture, String, std::collections::BTreeMap<String, String>) {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let mut want = std::collections::BTreeMap::new();
    for i in 0..n {
        let (origin, password) = (format!("https://site{i:03}.example"), format!("fixture-pw-{i:03}-K"));
        let id = vault.add(&EntryInput { origin: &origin, username: "fixture-user", password: &password, notes: "fixture note, long enough to fill pages faster", profile: Some(PROFILE_A) }).unwrap();
        want.insert(id, password);
    }
    let id = vault.id().to_owned();
    drop(vault);
    (fixture, id, want)
}

/// The passwords of `want` the vault in `dir` opens after an ordinary open and unlock.
fn held_of(dir: &Path, keys: &MemoryVaultKeyStore, want: &std::collections::BTreeMap<String, String>) -> usize {
    let Ok(mut vault) = Vault::open(dir, Box::new(keys.clone())) else { return 0 };
    if vault.unlock(Moment::now()).is_err() { return 0; }
    want.iter().filter(|(id, password)| vault.password(id).is_ok_and(|got| got.as_str() == password.as_str())).count()
}

/// R6-1, the sixth recheck's interior-page probe: in a vault of 40 entries,
/// whose table has an interior page, every bit SQLite reads on that page is
/// flipped. Recover keeps every password a plain table scan of the damaged
/// file reads correctly, counts at least every row that scan sees, and its
/// count is what the new file holds. The flip the review traced (page byte
/// 4 bit 1, in the cell count), where round 6 said "Recovered 31 of 31" with
/// 40 readable, recovers 40 of 40.
#[test]
fn recover_keeps_every_row_a_scan_reads_through_a_damaged_interior_page() {
    let (fixture, id, want) = many(40);
    let original = checkpointed(&fixture);
    let conn = raw(&fixture);
    let page_size = conn.query_row("PRAGMA page_size", [], |r| r.get::<_, i64>(0)).unwrap() as usize;
    let root = conn.query_row("SELECT rootpage FROM sqlite_master WHERE name = 'entries'", [], |r| r.get::<_, i64>(0)).unwrap() as usize;
    drop(conn);
    assert_eq!(original[(root - 1) * page_size], 0x05, "the table's root is an interior page");
    let work = Fixture { dir: tempfile::tempdir().unwrap(), keys: fixture.keys.clone() };
    let dir = work.dir.path();
    let (mut damaged, mut tally, mut short, mut traced, mut worst) = (0, std::collections::BTreeMap::<String, Vec<String>>::new(), Vec::new(), None, (usize::MAX, String::new()));
    for offset in read_bytes(&original, page_size, root) {
        let at = offset - (root - 1) * page_size;
        for bit in 0..8 {
            let mut bytes = original.clone();
            bytes[offset] ^= 1 << bit;
            write_image(dir, &bytes);
            if Vault::open(dir, Box::new(work.keys.clone())).map(|_| ()) != Err(VaultError::Damaged) { continue; }
            damaged += 1;
            write_image(dir, &bytes);
            let Scan { seen, password: read, .. } = scan_entries(dir, &work.keys, &id, &want);
            write_image(dir, &bytes);
            let mut outcome = String::new();
            let got = match Vault::recover_damaged_file(dir, &work.keys) {
                Ok(recovery) => {
                    let got = held_of(dir, &work.keys, &want);
                    outcome = format!("Recovered {} of {}{}; a scan reads {}", recovery.recovered, recovery.found, if recovery.complete { "" } else { ", not read in full" }, read.len());
                    if recovery.recovered != got { short.push(format!("byte {at} bit {bit}: says {} of {}, holds {got}", recovery.recovered, recovery.found)); }
                    if recovery.found < seen.len() { short.push(format!("byte {at} bit {bit}: counted {} of the {} rows a scan sees", recovery.found, seen.len())); }
                    if (at, bit) == (4, 1) { traced = Some((recovery.recovered, recovery.found)); }
                    got
                }
                Err(error) => {
                    short.push(format!("byte {at} bit {bit}: recover {error:?} with {} readable", read.len()));
                    0
                }
            };
            if got < read.len() { short.push(format!("byte {at} bit {bit}: recovered {got} of the {} a scan reads", read.len())); }
            if got < worst.0 { worst = (got, format!("byte {at} bit {bit}")); }
            tally.entry(outcome).or_default().push(format!("{at}.{bit}"));
            for name in moved_aside(dir) { std::fs::remove_file(dir.join(name)).unwrap(); }
        }
    }
    eprintln!("{damaged} interior-page flips damage at open; worst: {} of 40 recovered, at {}. By outcome (page byte.bit):", worst.0, worst.1);
    for (outcome, flips) in &tally { eprintln!("{:5}  {outcome}: {}", flips.len(), if flips.len() > 12 { format!("{} ...", flips[..12].join(" ")) } else { flips.join(" ") }); }
    assert!(short.is_empty(), "{} flips: {short:#?}", short.len());
    assert_eq!(traced, Some((40, 40)), "the flip the review traced");
}

/// The file's page size and the root page of b-tree `name`.
fn root_page(fixture: &Fixture, name: &str) -> (usize, usize) {
    let conn = raw(fixture);
    let page_size = conn.query_row("PRAGMA page_size", [], |r| r.get::<_, i64>(0)).unwrap() as usize;
    (page_size, conn.query_row("SELECT rootpage FROM sqlite_master WHERE name = ?1", [name], |r| r.get::<_, i64>(0)).unwrap() as usize)
}

/// R7-1's fixture, from the seventh recheck's K3: 40 entries, and the one
/// at `place` in id order, X, with one bit of its sealed notes damaged in
/// place. The vault still opens and lists X, X's password reveals and its
/// notes don't, so Recover can't keep X. The vault id, the passwords by id,
/// X's id and the file as one image.
fn forty_with_one_damaged_note(place: usize) -> (Fixture, String, std::collections::BTreeMap<String, String>, String, Vec<u8>) {
    let (fixture, id, want) = many(40);
    let x = want.keys().nth(place).unwrap().clone();
    let conn = raw(&fixture);
    let mut notes: Vec<u8> = conn.query_row("SELECT notes FROM entries WHERE id = ?1", [&x], |r| r.get(0)).unwrap();
    let middle = notes.len() / 2;
    notes[middle] ^= 0x01;
    conn.execute("UPDATE entries SET notes = ?1 WHERE id = ?2", params![notes, x]).unwrap();
    drop(conn);
    let vault = fixture.unlocked();
    assert!(vault.entry(&x).unwrap().readable(), "X is listed as readable");
    assert_eq!(vault.password(&x).unwrap().as_str(), want[&x], "X's password reveals");
    assert!(vault.secret(&x).is_err(), "X's notes don't open");
    drop(vault);
    let image = checkpointed(&fixture);
    (fixture, id, want, x, image)
}

/// R7-1, ported from the seventh recheck's K3 and the flips it traced: with
/// X as above, last or 21st by id, one bit of the entries index, which is
/// one leaf page. Its cell count (0x28 to 0x20 or 0x08), so a read of the
/// whole index ends short of X; the payload size of X's own index cell; or,
/// for X last, that of the entry before it, Y, whose index cell then reads
/// as Y at X's rowid. Round 7 counted only the rows a whole index read
/// listed, and took a row at a recovered entry's rowid for that entry; it
/// said "Recovered 39 of 39 passwords", while the table reads X's row and
/// X's password opens. Every id a read returned is counted now, so 39 of at
/// least 40, and the page says what couldn't be read; X stays in the kept file.
#[test]
fn recover_counts_an_entry_the_table_reads_that_a_short_index_read_leaves_out() {
    for place in [39, 20] {
        let (fixture, id, want, x, original) = forty_with_one_damaged_note(place);
        let (page_size, index) = root_page(&fixture, "sqlite_autoindex_entries_1");
        let start = (index - 1) * page_size;
        assert_eq!((original[start], original[start + 3], original[start + 4]), (0x0a, 0, 40), "the whole index is one leaf of 40 cells");
        // An index cell: its payload size (40), a three-byte record header, the id and the rowid.
        let cell = |id: &str| {
            let at = start + original[start..start + page_size].windows(id.len()).position(|bytes| bytes == id.as_bytes()).unwrap() - 4;
            assert_eq!((original[at], original[at + 1]), (40, 3), "an index cell");
            at
        };
        let (x_cell, y_cell) = (cell(&x), cell(want.keys().nth(place - 1).unwrap()));
        let flips = if place == 39 { vec![(start + 4, 3), (start + 4, 5), (x_cell, 1), (x_cell, 2), (y_cell, 0)] } else { vec![(start + 4, 5), (x_cell, 0), (x_cell, 1)] };
        let work = Fixture { dir: tempfile::tempdir().unwrap(), keys: fixture.keys.clone() };
        let dir = work.dir.path();
        for (at, bit) in flips {
            let flip = format!("X {} by id, index page byte {} bit {bit}", place + 1, at - start);
            let mut bytes = original.clone();
            bytes[at] ^= 1 << bit;
            write_image(dir, &bytes);
            assert_eq!(Vault::open(dir, Box::new(work.keys.clone())).map(|_| ()), Err(VaultError::Damaged), "{flip}");
            write_image(dir, &bytes);
            let scan = scan_entries(dir, &work.keys, &id, &want);
            assert!(scan.seen.len() == 40 && scan.password.contains(&x), "{flip}: a table scan reads all 40 rows and X's password");
            write_image(dir, &bytes);
            let recovery = Vault::recover_damaged_file(dir, &work.keys).unwrap();
            eprintln!("{flip}: Recovered {} of {}{}", recovery.recovered, recovery.found, if recovery.complete { "" } else { ", not read in full" });
            assert_eq!(recovery.recovered, 39, "{flip}");
            assert!(recovery.found >= 40, "{flip}: counted {} of the 40 rows a table scan sees", recovery.found);
            if at == start + 4 { assert_eq!((recovery.found, recovery.complete), (40, true), "{flip}: the table's read saw every row"); }
            assert_eq!(held_of(dir, &work.keys, &want), 39, "{flip}");
            let kept = rusqlite::Connection::open_with_flags(dir.join(&recovery.moved), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            assert_eq!(kept.query_row("SELECT COUNT(*) FROM entries NOT INDEXED WHERE id = ?1", [&x], |r| r.get::<_, i64>(0)).unwrap(), 1, "{flip}: X stays in the kept file");
            drop(kept);
            for name in moved_aside(dir) { std::fs::remove_file(dir.join(name)).unwrap(); }
        }
    }
}

/// R7-1, the seventh recheck's K3 sweep: X last, and every bit SQLite reads
/// on the entries index's page, each damage at open. No flip says every row
/// was recovered while a table scan reads X's password, none counts fewer
/// rows than that scan sees, and each recovers every password it reports.
/// Round 7: 41 of the 1,024 flips said "Recovered 39 of 39 passwords".
#[test]
fn no_flip_of_the_index_hides_an_entry_the_table_reads() {
    let (fixture, id, want, x, original) = forty_with_one_damaged_note(39);
    let (page_size, index) = root_page(&fixture, "sqlite_autoindex_entries_1");
    let work = Fixture { dir: tempfile::tempdir().unwrap(), keys: fixture.keys.clone() };
    let dir = work.dir.path();
    let (mut damaged, mut tally, mut short) = (0, std::collections::BTreeMap::<String, usize>::new(), Vec::new());
    for offset in read_bytes(&original, page_size, index) {
        let at = offset - (index - 1) * page_size;
        for bit in 0..8 {
            let mut bytes = original.clone();
            bytes[offset] ^= 1 << bit;
            write_image(dir, &bytes);
            if Vault::open(dir, Box::new(work.keys.clone())).map(|_| ()) != Err(VaultError::Damaged) { continue; }
            damaged += 1;
            write_image(dir, &bytes);
            let scan = scan_entries(dir, &work.keys, &id, &want);
            write_image(dir, &bytes);
            let flip = format!("byte {at} bit {bit}");
            let recovery = match Vault::recover_damaged_file(dir, &work.keys) {
                Ok(recovery) => recovery,
                Err(error) => {
                    short.push(format!("{flip}: recover {error:?}"));
                    continue;
                }
            };
            let got = held_of(dir, &work.keys, &want);
            if got != recovery.recovered { short.push(format!("{flip}: says {} of {}, holds {got}", recovery.recovered, recovery.found)); }
            if recovery.found < scan.seen.len() { short.push(format!("{flip}: counted {} of the {} rows a scan sees", recovery.found, scan.seen.len())); }
            if scan.password.contains(&x) && recovery.recovered >= recovery.found && recovery.complete { short.push(format!("{flip}: says {} of {} with X's password readable", recovery.recovered, recovery.found)); }
            *tally.entry(format!("Recovered {} of {}{}", recovery.recovered, recovery.found, if recovery.complete { "" } else { ", not read in full" })).or_default() += 1;
            for name in moved_aside(dir) { std::fs::remove_file(dir.join(name)).unwrap(); }
        }
    }
    eprintln!("{damaged} index flips damage at open, by outcome:");
    for (outcome, n) in &tally { eprintln!("{n:5}  {outcome}"); }
    assert!(damaged > 0);
    assert!(short.is_empty(), "{} flips: {short:#?}", short.len());
}

/// R7-1's rules for "read in full": a read is complete only when no other
/// read saw a row it lacks, and when none is, the page says part of the
/// file couldn't be read. X last, the index's cell count short of X (0x28
/// to 0x20), and the type byte of the table's first leaf damaged, so no
/// read of the table reaches its end. The whole index read lacks X, which
/// the table's backward read saw. Round 7 called that index read complete
/// and left X out of the count.
#[test]
fn a_read_that_lacks_a_row_another_read_saw_is_not_read_in_full() {
    let (fixture, id, want, x, original) = forty_with_one_damaged_note(39);
    let (page_size, index) = root_page(&fixture, "sqlite_autoindex_entries_1");
    let (_, root) = root_page(&fixture, "entries");
    let root_at = (root - 1) * page_size;
    assert_eq!(original[root_at], 0x05, "the table's root is an interior page");
    // Its first cell points to its leftmost child.
    let first = root_at + u16::from_be_bytes([original[root_at + 12], original[root_at + 13]]) as usize;
    let leaf = u32::from_be_bytes(original[first..first + 4].try_into().unwrap()) as usize;
    assert_eq!(original[(leaf - 1) * page_size], 0x0d, "a leaf");
    let mut bytes = original.clone();
    bytes[(index - 1) * page_size + 4] ^= 0x08;
    bytes[(leaf - 1) * page_size] ^= 0x01;
    let work = Fixture { dir: tempfile::tempdir().unwrap(), keys: fixture.keys.clone() };
    let dir = work.dir.path();
    write_image(dir, &bytes);
    let scan = scan_entries(dir, &work.keys, &id, &want);
    assert!(scan.password.contains(&x), "the table's backward scan reads X");
    let recovery = Vault::recover_damaged_file(dir, &work.keys).unwrap();
    eprintln!("Recovered {} of {}{}", recovery.recovered, recovery.found, if recovery.complete { "" } else { ", not read in full" });
    assert_eq!((recovery.found, recovery.complete), (40, false), "every row any read saw, and nothing read in full");
    assert_eq!(held_of(dir, &work.keys, &want), recovery.recovered);
    assert!(recovery.recovered < 39, "the first leaf's rows weren't read");
}

/// R7-1: a row a read returned is never left out, even one whose every cell
/// reads as NULL, as SQLite reads a record whose header size reads as 0.
/// With the index's cell count at 0, no other read lists it. Round 7
/// dropped it: "Recovered 1 of 1 password".
#[test]
fn a_row_whose_cells_all_read_empty_is_still_counted() {
    let (fixture, _, ids) = probed_vault();
    let rowid: i64 = raw(&fixture).query_row("SELECT rowid FROM entries WHERE id = ?1", [&ids[1]], |r| r.get(0)).unwrap();
    let original = checkpointed(&fixture);
    let (page_size, table) = root_page(&fixture, "entries");
    let (_, index) = root_page(&fixture, "sqlite_autoindex_entries_1");
    let (start, index_at) = ((table - 1) * page_size, (index - 1) * page_size);
    assert_eq!((original[start], original[index_at], original[index_at + 3], original[index_at + 4]), (0x0d, 0x0a, 0, 2), "one table leaf, one index leaf of two cells");
    // The second entry's cell: its payload size and rowid, then its record, which starts with its header's size.
    let varint = |at: usize| -> (i64, usize) {
        let mut value = 0i64;
        for (n, byte) in original[at..at + 9].iter().enumerate() {
            value = (value << 7) | i64::from(byte & 0x7f);
            if byte & 0x80 == 0 { return (value, n + 1); }
        }
        unreachable!("a varint of at most 8 bytes here")
    };
    let cell = (0..2).map(|n| start + u16::from_be_bytes([original[start + 8 + 2 * n], original[start + 9 + 2 * n]]) as usize)
        .find(|cell| { let (_, size) = varint(*cell); varint(cell + size).0 == rowid }).unwrap();
    let header = cell + varint(cell).1 + varint(cell + varint(cell).1).1;
    assert!(original[header] > 0 && original[header] < 0x80, "a one-byte header size");
    let mut bytes = original.clone();
    bytes[header] = 0;
    bytes[index_at + 4] = 0;
    write_image(fixture.dir.path(), &bytes);
    let read: Vec<bool> = raw(&fixture).prepare("SELECT id IS NULL AND origin IS NULL AND password IS NULL FROM entries NOT INDEXED ORDER BY rowid").unwrap()
        .query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(read, [false, true], "the table read returns the row, every cell NULL");
    let recovery = Vault::recover_damaged_file(fixture.dir.path(), &fixture.keys).unwrap();
    assert_eq!((recovery.recovered, recovery.found, recovery.complete), (1, 2, true));
}

/// A table leaf's cell count damaged upward reads "rows" where there are
/// none: the cell pointers past the count are zero, so each reads the page's
/// own header as a cell, a payload size of 13 (the page type), rowid 0 (the
/// first freeblock's high byte) and a record header of size 0, every cell
/// NULL. The vault never writes a rowid below 1, so a row with no id, no
/// cell that reads and a rowid of 0 or below isn't counted. Round 8's first
/// rule counted it: "Recovered 2 of 3 passwords. 1 couldn't be read".
#[test]
fn a_row_a_damaged_cell_count_reads_at_rowid_0_is_not_counted() {
    let (fixture, _, ids) = probed_vault();
    {
        let mut vault = fixture.unlocked();
        vault.never_save(PROFILE_A, "https://bank.example").unwrap();
        vault.never_save(PROFILE_A, "https://mail.example").unwrap();
        for address in [home(), work(), home()] { vault.add_address(PROFILE_A, &address).unwrap(); }
    }
    let original = checkpointed(&fixture);
    let dir = fixture.dir.path();
    // One bit each: 2 cells to 3, 3 to 7.
    for (table, cells, bit) in [("entries", 2, 0x01), ("never_save", 3, 0x04), ("addresses", 3, 0x04)] {
        let (page_size, root) = root_page(&fixture, table);
        let at = (root - 1) * page_size;
        assert_eq!(&original[at..at + 5], [0x0d, 0, 0, 0, cells], "{table}: one leaf, with no freeblock");
        let mut bytes = original.clone();
        bytes[at + 4] ^= bit;
        write_image(dir, &bytes);
        let read: Vec<(i64, bool)> = raw(&fixture).prepare(&format!("SELECT rowid, * FROM {table} NOT INDEXED")).unwrap()
            .query_map([], |r| Ok((r.get(0)?, (1..r.as_ref().column_count()).all(|i| r.get_ref(i).is_ok_and(|cell| cell == rusqlite::types::ValueRef::Null))))).unwrap()
            .map(Result::unwrap).collect();
        assert!(read.contains(&(0, true)) && read.iter().filter(|(rowid, _)| *rowid > 0).count() == usize::from(cells), "{table}: the real rows, and a row at rowid 0 with no cell: {read:?}");
        assert_eq!(Vault::open(dir, Box::new(fixture.keys.clone())).map(|_| ()), Err(VaultError::Damaged), "{table}");
        write_image(dir, &bytes);
        let recovery = Vault::recover_damaged_file(dir, &fixture.keys).unwrap();
        let counted = |table: &str| match table {
            "entries" => Counted { recovered: recovery.recovered, found: recovery.found, complete: recovery.complete },
            "never_save" => recovery.never_save.clone(),
            _ => recovery.addresses.clone(),
        };
        assert_eq!(counted(table), Counted { recovered: usize::from(cells), found: usize::from(cells), complete: true }, "{table}");
        for other in ["entries", "never_save", "addresses"] { assert_eq!(counted(other).found, counted(other).recovered, "{table}: {other}"); }
        assert_eq!(passwords_held(dir, &fixture.keys, &ids), 2, "{table}");
        for name in moved_aside(dir) { std::fs::remove_file(dir.join(name)).unwrap(); }
    }
}

/// R7-1 for the never-save list: a row whose text no longer reads, which
/// the index's cell count (3 to 2) leaves out, is still counted, as the
/// table read returned it. Round 7 said "2 of 2 never-save sites".
#[test]
fn recover_counts_a_never_save_row_the_table_reads_that_a_short_index_read_leaves_out() {
    let (fixture, _, ids) = probed_vault();
    {
        let vault = fixture.open();
        vault.never_save(PROFILE_A, "https://bank.example").unwrap();
        vault.never_save(PROFILE_A, "https://mail.example").unwrap();
    }
    // Bytes that aren't UTF-8, which also sort last in the index.
    damage(&fixture, "UPDATE never_save SET origin = CAST(X'FF68747470' AS TEXT) WHERE origin = 'https://mail.example'", "");
    let mut bytes = checkpointed(&fixture);
    let (page_size, index) = root_page(&fixture, "sqlite_autoindex_never_save_1");
    let at = (index - 1) * page_size;
    assert_eq!((bytes[at], bytes[at + 3], bytes[at + 4]), (0x0a, 0, 3), "one index leaf of three cells");
    bytes[at + 4] ^= 0x01;
    write_image(fixture.dir.path(), &bytes);
    let recovery = Vault::recover_damaged_file(fixture.dir.path(), &fixture.keys).unwrap();
    assert_eq!(recovery.never_save, Counted { recovered: 2, found: 3, complete: true });
    assert_eq!(never_save_rows(fixture.dir.path()), [(PROFILE_A.to_owned(), "https://bank.example".to_owned()), (PROFILE_A.to_owned(), "https://login.example".to_owned())].into());
    assert_eq!(passwords_held(fixture.dir.path(), &fixture.keys, &ids), 2);
}

/// R7-1 for addresses, which are read and counted as the entries are: an
/// address whose sealed cell no longer opens, which the index's cell count
/// (3 to 2) leaves out, is still counted, as the table read returned it.
#[test]
fn recover_counts_an_address_the_table_reads_that_a_short_index_read_leaves_out() {
    let (fixture, _, ids) = probed_vault();
    let mut vault = fixture.unlocked();
    let added: Vec<String> = [home(), work(), home()].iter().map(|address| vault.add_address(PROFILE_A, address).unwrap()).collect();
    drop(vault);
    // The last by id is the index's last cell.
    damage(&fixture, "UPDATE addresses SET address = zeroblob(64) WHERE id = ?1", added.iter().max().unwrap());
    let mut bytes = checkpointed(&fixture);
    let (page_size, index) = root_page(&fixture, "sqlite_autoindex_addresses_1");
    let at = (index - 1) * page_size;
    assert_eq!((bytes[at], bytes[at + 3], bytes[at + 4]), (0x0a, 0, 3), "one index leaf of three cells");
    bytes[at + 4] ^= 0x01;
    write_image(fixture.dir.path(), &bytes);
    let recovery = Vault::recover_damaged_file(fixture.dir.path(), &fixture.keys).unwrap();
    assert_eq!(recovery.addresses, Counted { recovered: 2, found: 3, complete: true });
    assert_eq!(passwords_held(fixture.dir.path(), &fixture.keys, &ids), 2);
}

/// The never-save rows a plain read-only scan of the table reads, both ways:
/// how many it sees, and the (profile, origin) pairs whose text reads.
fn scan_never_save(dir: &Path) -> (usize, std::collections::BTreeSet<(String, String)>) {
    let conn = rusqlite::Connection::open_with_flags(dir.join(FILE_NAME), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let (mut seen, mut read) = (std::collections::BTreeSet::new(), std::collections::BTreeSet::new());
    for sql in ["SELECT rowid, profile, origin FROM never_save NOT INDEXED", "SELECT rowid, profile, origin FROM never_save NOT INDEXED ORDER BY rowid DESC"] {
        let Ok(mut stmt) = conn.prepare(sql) else { continue };
        let Ok(mut rows) = stmt.query([]) else { continue };
        while let Ok(Some(row)) = rows.next() {
            let Ok(rowid) = row.get::<_, i64>(0) else { continue };
            // A damaged cell count reads cells with no cells at all; a real row has its NOT NULL cells.
            if matches!((row.get_ref(1), row.get_ref(2)), (Ok(rusqlite::types::ValueRef::Null), Ok(rusqlite::types::ValueRef::Null))) { continue; }
            seen.insert(rowid);
            if let (Ok(profile), Ok(origin)) = (row.get::<_, String>(1), row.get::<_, String>(2)) { read.insert((profile, origin)); }
        }
    }
    (seen.len(), read)
}

/// Every never-save row in the vault file in `dir`, whatever its profile.
fn never_save_rows(dir: &Path) -> std::collections::BTreeSet<(String, String)> {
    let conn = rusqlite::Connection::open_with_flags(dir.join(FILE_NAME), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut stmt = conn.prepare("SELECT profile, origin FROM never_save").unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect()
}

/// R6-2, the sixth recheck's never-save probe: three never-save sites, and
/// every damage-at-open flip of the never_save table's pages. Recover keeps
/// every original row a plain scan reads, never a changed one, counts at
/// least every row the scan sees, says how many it kept, and whenever the
/// list it leaves is shorter than the original, says so: fewer kept than
/// seen, or the list not read in full.
#[test]
fn recover_keeps_counts_and_reports_the_never_save_list() {
    let (fixture, _, ids) = probed_vault();
    {
        let vault = fixture.open();
        vault.never_save(PROFILE_A, "https://bank.example").unwrap();
        vault.never_save(PROFILE_A, "https://mail.example").unwrap();
    }
    let original_rows = never_save_rows(fixture.dir.path());
    assert_eq!(original_rows.len(), 3);
    let original = checkpointed(&fixture);
    let conn = raw(&fixture);
    let page_size = conn.query_row("PRAGMA page_size", [], |r| r.get::<_, i64>(0)).unwrap() as usize;
    let mut stmt = conn.prepare("SELECT name, rootpage FROM sqlite_master WHERE tbl_name = 'never_save'").unwrap();
    let pages: Vec<(String, usize)> = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))).unwrap().map(Result::unwrap).collect();
    drop(stmt);
    drop(conn);
    let work = Fixture { dir: tempfile::tempdir().unwrap(), keys: fixture.keys.clone() };
    let dir = work.dir.path();
    let (mut tally, mut short) = (std::collections::BTreeMap::<String, usize>::new(), Vec::new());
    for (name, page) in &pages {
        for offset in read_bytes(&original, page_size, *page) {
            for bit in 0..8 {
                let mut bytes = original.clone();
                bytes[offset] ^= 1 << bit;
                write_image(dir, &bytes);
                if Vault::open(dir, Box::new(work.keys.clone())).map(|_| ()) != Err(VaultError::Damaged) { continue; }
                write_image(dir, &bytes);
                let (seen, read) = scan_never_save(dir);
                write_image(dir, &bytes);
                let flip = format!("{name} byte {offset} bit {bit}");
                let Ok(recovery) = Vault::recover_damaged_file(dir, &work.keys) else {
                    short.push(format!("{flip}: recover failed"));
                    continue;
                };
                let rows = never_save_rows(dir);
                let intact: std::collections::BTreeSet<_> = read.intersection(&original_rows).cloned().collect();
                if !intact.is_subset(&rows) { short.push(format!("{flip}: lost rows a scan reads: {:?}", intact.difference(&rows).collect::<Vec<_>>())); }
                if !rows.is_subset(&original_rows) { short.push(format!("{flip}: holds a changed row: {:?}", rows.difference(&original_rows).collect::<Vec<_>>())); }
                if recovery.never_save.recovered != rows.len() { short.push(format!("{flip}: says {} kept, holds {}", recovery.never_save.recovered, rows.len())); }
                if recovery.never_save.found < seen { short.push(format!("{flip}: counted {} of the {seen} rows a scan sees", recovery.never_save.found)); }
                let lost = original_rows.difference(&rows).count();
                let reported = recovery.never_save.recovered < recovery.never_save.found || !recovery.never_save.complete;
                if lost > 0 && !reported { short.push(format!("{flip}: lost {lost} of 3 without saying so")); }
                if passwords_held(dir, &work.keys, &ids) != 2 { short.push(format!("{flip}: passwords lost")); }
                *tally.entry(format!("{name}: kept {} of {} seen{}, list lost {lost} of 3", recovery.never_save.recovered, recovery.never_save.found, if recovery.never_save.complete { "" } else { ", not read in full" })).or_default() += 1;
                for name in moved_aside(dir) { std::fs::remove_file(dir.join(name)).unwrap(); }
            }
        }
    }
    for (outcome, n) in &tally { eprintln!("{n:5}  {outcome}"); }
    assert!(short.is_empty(), "{} flips: {short:#?}", short.len());
}

/// R5-5 and Q3's remainder: a format-2 file that lost its format row and
/// its never_save table is damage, and Recover gets both passwords back and
/// says the never-save list wasn't read in full, instead of leaving it
/// silently empty.
#[test]
fn recover_says_when_the_never_save_table_is_gone() {
    let (fixture, _, ids) = probed_vault();
    raw(&fixture).execute_batch("DELETE FROM meta WHERE k = 'format'; DROP TABLE never_save;").unwrap();
    assert_eq!(Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).map(|_| ()), Err(VaultError::Damaged));
    let recovery = Vault::recover_damaged_file(fixture.dir.path(), &fixture.keys).unwrap();
    assert_eq!((recovery.recovered, recovery.found), (2, 2));
    assert_eq!((recovery.never_save.recovered, recovery.never_save.found, recovery.never_save.complete), (0, 0, false));
    assert_eq!(passwords_held(fixture.dir.path(), &fixture.keys, &ids), 2);
}

// -- addresses (ASP-P17) ---------------------------------------------------------------------

fn address(parts: &[(AddressField, &str)]) -> Address {
    let mut address = Address::default();
    for (field, value) in parts { address.set(*field, value); }
    address
}

/// A synthetic home address with every part filled.
fn home() -> Address {
    use AddressField::*;
    address(&[
        (Label, "Home"), (Name, "Fixture Person"), (Organization, "Fixture Works"), (Street, "1 Fixture Lane\nFlat 2"), (City, "Testville"),
        (Region, "Testshire"), (PostalCode, "TS1 2AB"), (Country, "United Kingdom"), (Phone, "+44 20 7946 0000"), (Email, "fixture@example.test"),
    ])
}

fn work() -> Address {
    use AddressField::*;
    address(&[(Label, "Work"), (Street, "99 Sample Road"), (City, "Exampleton"), (PostalCode, "90210"), (Country, "US")])
}

fn labels(addresses: &[&SavedAddress]) -> Vec<String> {
    addresses.iter().map(|saved| saved.address.get(AddressField::Label).to_owned()).collect()
}

/// Addresses are sealed with the vault key, each in one profile, opened
/// only while unlocked, and kept across a reopen. Their count, like the
/// entries', is readable while locked; nothing else is, and no part of one
/// is in the file, ids included.
#[test]
fn addresses_are_sealed_per_profile_and_survive_reopening() {
    let bytes = sealed_addresses(&Fixture::new());
    assert!(bytes == sealed_addresses(&Fixture::new()), "the same steps wrote another file");
    assert!(contains(&bytes, ROW_ID) && contains(&bytes, OTHER_ROW_ID), "the search reads the ids");
    for secret in ["Fixture Person", "Fixture Lane", "Testville", "TS1 2AB", "7946", "fixture@example.test", "Sample Road", "Exampleton", PROFILE_A, "Home"] {
        assert!(!contains(&bytes, secret), "{secret} reached the vault file");
    }
}

/// Save a home and a work address under a script, check them through a
/// reopen, and return the vault's file.
fn sealed_addresses(fixture: &Fixture) -> Vec<u8> {
    let script = script(&[VAULT_ID, ROW_ID, OTHER_ROW_ID]);
    let mut vault = fixture.unlocked();
    let home_id = vault.add_address(PROFILE_A, &home()).unwrap();
    let work_id = vault.add_address(PROFILE_B, &work()).unwrap();
    assert_eq!((vault.db().vault_id.as_str(), home_id.as_str(), work_id.as_str()), (VAULT_ID, ROW_ID, OTHER_ROW_ID), "each id is the scripted one");
    assert_eq!(labels(&vault.addresses_in(PROFILE_A)), ["Home"]);
    assert_eq!(labels(&vault.addresses_in(PROFILE_B)), ["Work"]);
    assert!(vault.addresses_in("0190a1b2-fixture-profile-c").is_empty(), "no other profile sees either");
    assert!(vault.entries().is_empty(), "addresses are not passwords");
    drop(vault);

    let vault = fixture.open();
    assert_eq!(vault.status(), Ok(VaultStatus::Locked));
    assert_eq!(vault.address_count(), Ok(2));
    assert!(vault.addresses().is_empty(), "nothing opens while locked");
    drop(vault);
    let vault = fixture.unlocked();
    let saved = vault.saved_address(&home_id).unwrap();
    assert_eq!(saved.address, home());
    assert_eq!((saved.profile.as_str(), saved.readable(), saved.used_ms), (PROFILE_A, true, None));
    assert_eq!(vault.saved_address(&work_id).unwrap().address, work());
    assert_eq!(script.ids_left(), 0);
    fixture.file_bytes()
}

/// Edit keeps the profile, Delete removes the row, and the only refusals
/// are an empty address, a part over its limit or no profile: an unusual
/// address saves as typed.
#[test]
fn addresses_are_edited_and_deleted_and_an_unusual_one_saves_as_typed() {
    use AddressField::*;
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let id = vault.add_address(PROFILE_A, &home()).unwrap();
    let mut moved = home();
    moved.set(Street, "2 Other Street");
    moved.set(Label, "");
    vault.update_address(&id, &moved).unwrap();
    let saved = vault.saved_address(&id).unwrap();
    assert_eq!((saved.address.get(Street), saved.address.get(Label), saved.profile.as_str()), ("2 Other Street", "", PROFILE_A));
    assert!(saved.updated_ms >= saved.created_ms);

    for unusual in [
        address(&[(Country, "Antarctica")]),
        address(&[(Street, "Ñandú 7, 3º izq.\n\n🏠 Rear door"), (PostalCode, "no code"), (Email, "not an email"), (Phone, "ask at the desk")]),
        address(&[(Name, &"N".repeat(Name.limit()))]),
    ] {
        let unusual_id = vault.add_address(PROFILE_A, &unusual).unwrap();
        assert_eq!(vault.saved_address(&unusual_id).unwrap().address, unusual);
    }
    let too_long = address(&[(Street, &"S".repeat(Street.limit() + 1))]);
    for (case, profile, refused) in [("blank", PROFILE_A, address(&[(Label, "Only a label")])), ("too long", PROFILE_A, too_long), ("no profile", "", home())] {
        assert!(matches!(vault.add_address(profile, &refused), Err(VaultError::Invalid(_))), "{case}");
    }
    assert!(matches!(vault.update_address(&id, &Address::default()), Err(VaultError::Invalid(_))));
    assert_eq!(vault.addresses_in(PROFILE_A).len(), 4);

    vault.mark_address_used(&id, 5_000).unwrap();
    vault.delete_address(&id).unwrap();
    assert!(vault.saved_address(&id).is_none());
    assert_eq!(vault.delete_address(&id), Err(VaultError::NotFound));
    assert_eq!(vault.update_address(&id, &home()), Err(VaultError::NotFound));
    let kept = vault.addresses_in(PROFILE_A)[0].id.clone();
    vault.mark_address_used(&kept, 7_000).unwrap();
    drop(vault);
    let vault = fixture.unlocked();
    assert_eq!(vault.addresses().len(), 3);
    assert_eq!(vault.saved_address(&kept).unwrap().used_ms, Some(7_000), "the last fill persists");
}

/// Locking drops every opened address, and no address is read or written
/// while locked.
#[test]
fn locking_drops_every_opened_address() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let id = vault.add_address(PROFILE_A, &home()).unwrap();
    vault.lock();
    assert!(vault.addresses().is_empty() && vault.addresses_in(PROFILE_A).is_empty() && vault.saved_address(&id).is_none());
    assert_eq!(vault.add_address(PROFILE_A, &work()), Err(VaultError::Locked));
    assert_eq!(vault.update_address(&id, &work()), Err(VaultError::Locked));
    assert_eq!(vault.delete_address(&id), Err(VaultError::Locked));
    assert_eq!(vault.mark_address_used(&id, 1), Err(VaultError::Locked));
    vault.unlock(Moment::now()).unwrap();
    assert_eq!(vault.saved_address(&id).unwrap().address, home());
    // Idle time locks addresses too.
    let later = Moment::now().after(IDLE_TIMEOUT + Duration::from_secs(1));
    assert!(vault.lock_if_idle(later));
    assert!(vault.addresses().is_empty());
}

/// A profile tag or an address moved to another row, or a cell of the
/// wrong type, opens nowhere: the row is listed damaged, in no profile,
/// and can only be deleted. The others still open.
#[test]
fn a_moved_or_retyped_address_cell_fails_closed_and_can_be_deleted() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let [a, b, c, d] = [home(), work(), home(), work()].map(|address| vault.add_address(PROFILE_A, &address).unwrap());
    let entry = vault.add(&tagged("https://one.example", "fixture", PASSWORD_A, PROFILE_A)).unwrap();
    drop(vault);
    damage(&fixture, &format!("UPDATE addresses SET profile = (SELECT profile FROM addresses WHERE id = '{b}') WHERE id = ?1"), &a);
    damage(&fixture, &format!("UPDATE addresses SET address = (SELECT origin FROM entries WHERE id = '{entry}') WHERE id = ?1"), &b);
    damage(&fixture, "UPDATE addresses SET address = 'plain text' WHERE id = ?1", &c);
    damage(&fixture, "UPDATE addresses SET id = CAST(id AS BLOB) WHERE id = ?1", &d);
    let mut vault = fixture.unlocked();
    assert_eq!(vault.addresses().len(), 4);
    assert!(vault.addresses().iter().all(|saved| !saved.readable() && saved.profile.is_empty() && saved.address.is_blank()));
    assert!(vault.addresses_in(PROFILE_A).is_empty(), "a damaged address is never filled");
    assert_eq!(vault.password(&entry).unwrap().as_str(), PASSWORD_A, "the entry still opens");
    let damaged: Vec<String> = vault.addresses().iter().map(|saved| saved.id.clone()).collect();
    assert!(damaged.iter().any(|id| id.starts_with("damaged-row:")), "{damaged:?}");
    assert_eq!(vault.update_address(&a, &home()), Err(VaultError::Unreadable));
    for id in &damaged { vault.delete_address(id).unwrap(); }
    drop(vault);
    assert_eq!(fixture.unlocked().addresses().len(), 0);
}

/// Reset empties the addresses with everything else.
#[test]
fn reset_deletes_every_address() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    vault.add_address(PROFILE_A, &home()).unwrap();
    vault.reset().unwrap();
    assert_eq!(vault.address_count(), Ok(0));
    assert_eq!(vault.status(), Ok(VaultStatus::New));
}

/// A vault holding only addresses is an existing vault: with its check
/// gone, the stored key that opens an address rewrites it, a foreign key
/// is a mismatch, and no key at all is a missing key.
#[test]
fn an_address_only_vault_repairs_its_check_with_the_key_that_opens_it() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let id = vault.add_address(PROFILE_A, &home()).unwrap();
    let vault_id = vault.id().to_owned();
    drop(vault);
    raw(&fixture).execute("DELETE FROM meta WHERE k = 'check'", []).unwrap();
    let mut vault = fixture.open();
    assert_eq!(vault.status(), Ok(VaultStatus::Locked), "addresses mean a vault exists");
    assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::CheckRepaired));
    assert_eq!(vault.saved_address(&id).unwrap().address, home());
    drop(vault);
    raw(&fixture).execute("DELETE FROM meta WHERE k = 'check'", []).unwrap();
    let foreign = MemoryVaultKeyStore::default();
    foreign.save(&vault_id, &VaultKey::from_slice(&[9; 32]).unwrap()).unwrap();
    let mut vault = Vault::open(fixture.dir.path(), Box::new(foreign)).unwrap();
    assert_eq!(vault.unlock(Moment::now()), Err(VaultError::KeyMismatch));
    let mut vault = Vault::open(fixture.dir.path(), Box::new(MemoryVaultKeyStore::default())).unwrap();
    assert_eq!(vault.unlock(Moment::now()), Err(VaultError::MissingKey));
}

/// Format 2 with its entries and never-save list, as the build before
/// addresses wrote it.
fn format_2_vault() -> (Fixture, String, [String; 2]) {
    let (fixture, id, ids) = probed_vault();
    raw(&fixture).execute_batch("DROP TABLE addresses; DROP TABLE cards; UPDATE meta SET v = 2 WHERE k = 'format';").unwrap();
    assert_eq!((raw_format(fixture.dir.path()).0, has_table(&raw(&fixture), "addresses"), has_table(&raw(&fixture), "cards")), (Some(2), false, false), "a format-2 file");
    (fixture, id, ids)
}

/// Opening a format-2 vault adds the addresses and cards tables in one
/// transaction that checks the result before it commits; the entries, their
/// tags and the never-save list are untouched, and the next build's file
/// refuses older ones.
#[test]
fn opening_a_format_2_vault_upgrades_it_to_format_8_and_keeps_everything() {
    let (fixture, id, ids) = format_2_vault();
    let mut vault = fixture.unlocked();
    assert_eq!(vault.id(), id);
    let (format, columns, never_save, count) = raw_format(fixture.dir.path());
    assert_eq!((format, columns.len(), never_save, count), (Some(8), 9, true, 2));
    assert!(has_table(&raw(&fixture), "addresses") && has_table(&raw(&fixture), "cards"));
    assert_eq!(passwords_held(fixture.dir.path(), &fixture.keys, &ids), 2);
    assert_eq!(vault.entry(&ids[1]).unwrap().used_ms, Some(1_000));
    assert_eq!(vault.never_saved(PROFILE_A).unwrap(), ["https://login.example"]);
    let address = vault.add_address(PROFILE_A, &home()).unwrap();
    drop(vault);
    assert_eq!(fixture.unlocked().saved_address(&address).unwrap().address, home());
}

#[test]
fn an_upgrade_from_format_2_cut_halfway_leaves_it_whole_and_the_next_open_finishes_it() {
    let (fixture, _, ids) = format_2_vault();
    let snapshot = tempfile::tempdir().unwrap();
    let (source, copy) = (fixture.dir.path().to_path_buf(), snapshot.path().to_path_buf());
    super::db::UPGRADE_CUT.set(Some(Box::new(move |_: &rusqlite::Connection| {
        for name in [FILE_NAME.to_string(), format!("{FILE_NAME}-wal")] {
            if source.join(&name).exists() { std::fs::copy(source.join(&name), copy.join(&name)).unwrap(); }
        }
        true
    })));
    let error = Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).unwrap_err();
    super::db::UPGRADE_CUT.set(None);
    assert_eq!(error, VaultError::Storage);
    for (case, dir) in [("rolled back", fixture.dir.path()), ("power cut", snapshot.path())] {
        let conn = rusqlite::Connection::open(dir.join(FILE_NAME)).unwrap();
        assert_eq!((raw_format(dir).0, has_table(&conn, "addresses")), (Some(2), false), "{case}: wholly format 2");
        drop(conn);
        assert_eq!(passwords_held(dir, &fixture.keys, &ids), 2, "{case}");
        assert_eq!(raw_format(dir).0, Some(8), "{case}: the next open upgrades it");
    }
}

/// A file with any part of format 5 is format 5: one without its addresses
/// table, with a format row other than 5, or with a changed addresses table
/// is damage, and opening it writes nothing.
#[test]
fn a_file_with_part_of_format_5_is_damage_unless_it_is_all_of_it() {
    for (case, sql) in [
        ("addresses dropped", "DROP TABLE addresses"),
        ("format 2 with addresses", "UPDATE meta SET v = 2 WHERE k = 'format'"),
        ("format row deleted", "DELETE FROM meta WHERE k = 'format'"),
        ("format 1 with addresses only", "UPDATE meta SET v = 1 WHERE k = 'format'; DROP TABLE never_save; ALTER TABLE entries DROP COLUMN used_ms; ALTER TABLE entries DROP COLUMN profile"),
        ("address column renamed", "ALTER TABLE addresses RENAME COLUMN address TO adress"),
        ("used_ms dropped", "ALTER TABLE addresses DROP COLUMN used_ms"),
        ("column added", "ALTER TABLE addresses ADD COLUMN extra BLOB"),
    ] {
        let (fixture, _, _) = probed_vault();
        raw(&fixture).execute_batch(sql).unwrap();
        let image = checkpointed(&fixture);
        assert_eq!(Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).map(|_| ()), Err(VaultError::Damaged), "{case}");
        assert_eq!(std::fs::read(fixture.dir.path().join(FILE_NAME)).unwrap(), image, "{case}: opening wrote nothing");
    }
}

/// Recover copies every address that opens, with its profile, times and
/// last use, counts them apart from the passwords, and the new file opens
/// them.
#[test]
fn recover_keeps_and_counts_addresses_apart_from_passwords() {
    let (fixture, _, ids) = probed_vault();
    let mut vault = fixture.unlocked();
    let kept = [vault.add_address(PROFILE_A, &home()).unwrap(), vault.add_address(PROFILE_B, &work()).unwrap()];
    vault.mark_address_used(&kept[0], 3_000).unwrap();
    let lost = vault.add_address(PROFILE_A, &home()).unwrap();
    drop(vault);
    damage(&fixture, "UPDATE addresses SET address = zeroblob(64) WHERE id = ?1", &lost);
    raw(&fixture).execute_batch(&on_its_own_index()).unwrap();
    assert_eq!(Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).map(|_| ()), Err(VaultError::Damaged));
    let recovery = Vault::recover_damaged_file(fixture.dir.path(), &fixture.keys).unwrap();
    assert_eq!(recovery.addresses, Counted { recovered: 2, found: 3, complete: true });
    assert_eq!(passwords_held(fixture.dir.path(), &fixture.keys, &ids), recovery.recovered);
    let vault = fixture.unlocked();
    assert_eq!(vault.addresses().len(), 2);
    let saved = vault.saved_address(&kept[0]).unwrap();
    assert_eq!((saved.address.clone(), saved.profile.as_str(), saved.used_ms), (home(), PROFILE_A, Some(3_000)));
    assert_eq!(vault.saved_address(&kept[1]).unwrap().profile, PROFILE_B);
}

/// A format-5 file that lost its format row and its addresses table: the
/// passwords come back and the page can say the addresses weren't read in
/// full. A format-2 file has no addresses to read, so it is complete.
#[test]
fn recover_says_when_the_addresses_table_is_gone() {
    let (fixture, _, ids) = probed_vault();
    raw(&fixture).execute_batch("DELETE FROM meta WHERE k = 'format'; DROP TABLE addresses;").unwrap();
    let recovery = Vault::recover_damaged_file(fixture.dir.path(), &fixture.keys).unwrap();
    assert_eq!((recovery.recovered, recovery.found), (2, 2));
    assert_eq!(recovery.addresses, Counted { recovered: 0, found: 0, complete: false });
    assert_eq!(passwords_held(fixture.dir.path(), &fixture.keys, &ids), 2);

    let (fixture, _, _) = format_2_vault();
    raw(&fixture).execute_batch(&on_its_own_index()).unwrap();
    let recovery = Vault::recover_damaged_file(fixture.dir.path(), &fixture.keys).unwrap();
    assert_eq!(recovery.addresses, Counted { recovered: 0, found: 0, complete: true });
    assert_eq!(raw_format(fixture.dir.path()).0, Some(8), "the recovered file is the current format");
}

/// Each address row a plain scan of the table reads, both ways: the rowids
/// it sees, and the rows whose id and sealed cells read as stored.
fn scan_addresses(dir: &Path) -> (usize, std::collections::BTreeSet<(String, Vec<u8>, Vec<u8>)>) {
    let conn = rusqlite::Connection::open_with_flags(dir.join(FILE_NAME), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let (mut seen, mut read) = (std::collections::BTreeSet::new(), std::collections::BTreeSet::new());
    for sql in ["SELECT rowid, id, profile, address FROM addresses NOT INDEXED", "SELECT rowid, id, profile, address FROM addresses NOT INDEXED ORDER BY rowid DESC"] {
        let Ok(mut stmt) = conn.prepare(sql) else { continue };
        let Ok(mut rows) = stmt.query([]) else { continue };
        while let Ok(Some(row)) = rows.next() {
            let Ok(rowid) = row.get::<_, i64>(0) else { continue };
            if (1..4).all(|i| matches!(row.get_ref(i), Ok(rusqlite::types::ValueRef::Null))) { continue; }
            seen.insert(rowid);
            if let (Ok(id), Ok(profile), Ok(address)) = (row.get::<_, String>(1), row.get::<_, Vec<u8>>(2), row.get::<_, Vec<u8>>(3)) { read.insert((id, profile, address)); }
        }
    }
    (seen.len(), read)
}

/// The never-save probe's sweep, for addresses: every damage-at-open flip of
/// the addresses table's pages. Recover keeps every address a plain scan
/// reads as stored, counts at least every row the scan sees, says how many
/// it kept, and says so whenever it kept fewer than the file held. The
/// passwords are never touched.
#[test]
fn recover_keeps_every_address_a_scan_reads_through_any_flip_of_its_table() {
    let (fixture, _, ids) = probed_vault();
    let mut vault = fixture.unlocked();
    for address in [home(), work(), home()] { vault.add_address(PROFILE_A, &address).unwrap(); }
    drop(vault);
    let (_, original_rows) = scan_addresses(fixture.dir.path());
    assert_eq!(original_rows.len(), 3);
    let original = checkpointed(&fixture);
    let conn = raw(&fixture);
    let page_size = conn.query_row("PRAGMA page_size", [], |r| r.get::<_, i64>(0)).unwrap() as usize;
    let mut stmt = conn.prepare("SELECT name, rootpage FROM sqlite_master WHERE tbl_name = 'addresses'").unwrap();
    let pages: Vec<(String, usize)> = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))).unwrap().map(Result::unwrap).collect();
    drop(stmt);
    drop(conn);
    let work = Fixture { dir: tempfile::tempdir().unwrap(), keys: fixture.keys.clone() };
    let dir = work.dir.path();
    let (mut tally, mut short) = (std::collections::BTreeMap::<String, usize>::new(), Vec::new());
    for (name, page) in &pages {
        for offset in read_bytes(&original, page_size, *page) {
            for bit in 0..8 {
                let mut bytes = original.clone();
                bytes[offset] ^= 1 << bit;
                write_image(dir, &bytes);
                if Vault::open(dir, Box::new(work.keys.clone())).map(|_| ()) != Err(VaultError::Damaged) { continue; }
                write_image(dir, &bytes);
                let (seen, read) = scan_addresses(dir);
                write_image(dir, &bytes);
                let flip = format!("{name} byte {offset} bit {bit}");
                let Ok(recovery) = Vault::recover_damaged_file(dir, &work.keys) else {
                    short.push(format!("{flip}: recover failed"));
                    continue;
                };
                let (_, rows) = scan_addresses(dir);
                let intact: std::collections::BTreeSet<_> = read.intersection(&original_rows).cloned().collect();
                if !intact.is_subset(&rows) { short.push(format!("{flip}: lost {} addresses a scan reads", intact.difference(&rows).count())); }
                if !rows.is_subset(&original_rows) { short.push(format!("{flip}: holds a changed address")); }
                let counted = &recovery.addresses;
                if counted.recovered != rows.len() { short.push(format!("{flip}: says {} kept, holds {}", counted.recovered, rows.len())); }
                if counted.found < seen { short.push(format!("{flip}: counted {} of the {seen} rows a scan sees", counted.found)); }
                let lost = original_rows.difference(&rows).count();
                if lost > 0 && counted.recovered >= counted.found && counted.complete { short.push(format!("{flip}: lost {lost} of 3 without saying so")); }
                if passwords_held(dir, &work.keys, &ids) != 2 { short.push(format!("{flip}: passwords lost")); }
                *tally.entry(format!("{name}: kept {} of {} seen{}, lost {lost} of 3", counted.recovered, counted.found, if counted.complete { "" } else { ", not read in full" })).or_default() += 1;
                for name in moved_aside(dir) { std::fs::remove_file(dir.join(name)).unwrap(); }
            }
        }
    }
    for (outcome, n) in &tally { eprintln!("{n:5}  {outcome}"); }
    assert!(!tally.is_empty(), "some flip is damage at open");
    assert!(short.is_empty(), "{} flips: {short:#?}", short.len());
}

/// ASP-P17: no part of an address, its label or its profile reaches the
/// vault file in plaintext, through an add, an edit or a delete; the library
/// store and its sync queue never see one.
#[test]
fn no_part_of_an_address_reaches_the_vault_file_or_the_library_store() {
    use AddressField::*;
    let fixture = Fixture::new();
    let store = crate::store::Store::open(&fixture.dir.path().join("crossbow.sqlite")).unwrap();
    let mut lib = crate::library::Library::open(store).unwrap();
    lib.ensure_defaults().unwrap();
    let (dirty, entities) = (lib.store.dirty_count().unwrap(), lib.store.all_entities().unwrap().len());
    let mut vault = fixture.unlocked();
    let secret = address(&[
        (Label, "secret-label-home"), (Name, "secret-name-ada"), (Organization, "secret-org-works"), (Street, "secret-street-one\nsecret-street-two"),
        (City, "secret-city-old"), (Region, "secret-region-shire"), (PostalCode, "secret-postal-77"), (Country, "secret-country-land"),
        (Phone, "secret-phone-555"), (Email, "secret-email@example.test"),
    ]);
    let id = vault.add_address("secret-profile-tag", &secret).unwrap();
    let mut edited = secret.clone();
    edited.set(City, "secret-city-new");
    vault.update_address(&id, &edited).unwrap();
    let gone = vault.add_address(PROFILE_A, &address(&[(Street, "deleted-street-9"), (City, "deleted-city-9")])).unwrap();
    vault.delete_address(&gone).unwrap();
    vault.mark_address_used(&id, 1).unwrap();
    drop(vault);
    let bytes = fixture.file_bytes();
    for text in ["secret-label-home", "secret-name-ada", "secret-org-works", "secret-street-one", "secret-street-two", "secret-city-old", "secret-city-new",
        "secret-region-shire", "secret-postal-77", "secret-country-land", "secret-phone-555", "secret-email@example.test", "secret-profile-tag",
        "deleted-street-9", "deleted-city-9"] {
        assert!(!contains(&bytes, text), "{text} reached the vault file");
    }
    assert_eq!((lib.store.dirty_count().unwrap(), lib.store.all_entities().unwrap().len()), (dirty, entities), "nothing queued for sync");
    let library_bytes = std::fs::read(fixture.dir.path().join("crossbow.sqlite")).unwrap();
    assert!(!contains(&library_bytes, "secret-street-one") && !contains(&library_bytes, "secret-name-ada"));
    // Debug output names neither the parts nor the profile.
    let reopened = { let mut vault = fixture.open(); vault.unlock(Moment::now()).unwrap(); vault };
    let shown = format!("{:?} {:?}", reopened.saved_address(&id).unwrap(), reopened.saved_address(&id).unwrap().address);
    assert!(!shown.contains("secret"), "{shown}");
}

// -- payment cards (ASP-P16) -------------------------------------------------------------------
// Only the card networks' published test numbers and invented names.

const VISA: &str = "4242 4242 4242 4242";
const MASTERCARD: &str = "5555-5555-5555-4444";

fn card(name: &str, number: &str, exp_month: u8, exp_year: u16, nickname: &str) -> Card {
    Card { name: Zeroizing::new(name.into()), number: Zeroizing::new(number.into()), exp_month, exp_year, nickname: Zeroizing::new(nickname.into()) }
}

fn visa() -> Card {
    card("Fixture Cardholder", VISA, 7, 2034, "Fixture Travel")
}

fn mastercard() -> Card {
    card("Sample Holder", MASTERCARD, 12, 2031, "")
}

/// What a list shows of each card: network, last four, expiry and nickname.
fn shown(cards: &[&SavedCard]) -> Vec<(CardNetwork, String, u8, u16, String)> {
    cards.iter().map(|saved| (saved.network, saved.last4.to_string(), saved.exp_month, saved.exp_year, saved.nickname.to_string())).collect()
}

/// The networks' published test numbers name their networks; anything
/// else is Unknown, and the checks behind the editor's hints hold.
#[test]
fn a_cards_network_comes_from_its_number_and_the_hints_never_block() {
    for (number, network) in [
        ("4242424242424242", CardNetwork::Visa), ("4000056655665556", CardNetwork::Visa),
        ("5555555555554444", CardNetwork::Mastercard), ("2223003122003222", CardNetwork::Mastercard),
        ("378282246310005", CardNetwork::Amex), ("371449635398431", CardNetwork::Amex),
        ("6011111111111117", CardNetwork::Discover), ("6011000990139424", CardNetwork::Discover),
        ("3056930009020004", CardNetwork::DinersClub), ("36227206271667", CardNetwork::DinersClub),
        ("3566002020360505", CardNetwork::Jcb), ("6200000000000005", CardNetwork::UnionPay),
        ("0000000000000000", CardNetwork::Unknown), ("9999999999999995", CardNetwork::Unknown), ("", CardNetwork::Unknown),
    ] {
        assert_eq!(CardNetwork::of(number), network, "{number}");
        if network != CardNetwork::Unknown && !number.is_empty() { assert!(luhn_valid(number), "{number} passes the check"); }
    }
    assert!(!luhn_valid("4242424242424241") && !luhn_valid("") && !luhn_valid("42a2"));
    assert_eq!(CardNetwork::Amex.name(), "American Express");
    assert_eq!(CardNetwork::Unknown.name(), "Card");
    // Hints, from 15 October 2026.
    let today = (2026, 10);
    assert!(visa().hints(today).is_empty());
    assert_eq!(card("", "4242 4242 4242 4241", 9, 2026, "").hints(today), [CardHint::FailsCheck, CardHint::Expired]);
    assert!(card("", VISA, 10, 2026, "").hints(today).is_empty(), "a card works to the end of its month");
    assert_eq!(card("", "0000 0000 0000 0000", 1, 2030, "").hints(today), [CardHint::UnknownNetwork]);
    assert_eq!(year_month(0), (1970, 1));
    assert_eq!(year_month(1_792_800_000_000), (2026, 10));
    assert_eq!(year_month(1_798_761_599_999), (2026, 12));
    assert_eq!(year_month(1_798_761_600_000), (2027, 1));
}

/// Cards are sealed with the vault key, each in one profile, opened only
/// while unlocked, and kept across a reopen. The list holds the network,
/// the last four digits, the expiry, the nickname and the name; the number
/// is opened only on request, and is stored as its digits.
#[test]
fn cards_are_sealed_per_profile_and_survive_reopening() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let visa_id = vault.add_card(PROFILE_A, &visa()).unwrap();
    let mastercard_id = vault.add_card(PROFILE_B, &mastercard()).unwrap();
    assert_eq!(shown(&vault.cards_in(PROFILE_A)), [(CardNetwork::Visa, "4242".into(), 7, 2034, "Fixture Travel".into())]);
    assert_eq!(shown(&vault.cards_in(PROFILE_B)), [(CardNetwork::Mastercard, "4444".into(), 12, 2031, String::new())]);
    assert!(vault.cards_in("0190a1b2-fixture-profile-c").is_empty(), "no other profile sees either");
    assert!(vault.entries().is_empty() && vault.addresses().is_empty(), "cards are neither passwords nor addresses");
    drop(vault);

    let vault = fixture.open();
    assert_eq!(vault.status(), Ok(VaultStatus::Locked), "cards mean a vault exists");
    assert_eq!(vault.card_count(), Ok(2));
    assert!(vault.cards().is_empty(), "nothing opens while locked");
    assert_eq!(vault.card_number(&visa_id).map(|_| ()), Err(VaultError::Locked));
    drop(vault);
    let vault = fixture.unlocked();
    let saved = vault.saved_card(&visa_id).unwrap();
    assert_eq!((saved.name.as_str(), saved.profile.as_str(), saved.readable(), saved.used_ms), ("Fixture Cardholder", PROFILE_A, true, None));
    assert_eq!(saved.expiry(), "07/34");
    assert_eq!(vault.card_number(&visa_id).unwrap().as_str(), "4242424242424242");
    assert_eq!(vault.card_number(&mastercard_id).unwrap().as_str(), "5555555555554444");
    let bytes = fixture.file_bytes();
    for secret in ["4242424242424242", "4242 4242", "5555555555554444", "5555-5555", "Fixture Cardholder", "Fixture Travel", "Sample Holder", PROFILE_A] {
        assert!(!contains(&bytes, secret), "{secret} reached the vault file");
    }
}

/// Edit changes the name, expiry and nickname and keeps the number unless
/// a new one is typed, which replaces it. The only refusals are what the
/// vault can't hold; a number that fails the check, an expired card and an
/// unknown network save. Delete removes the row; the last fill persists.
#[test]
fn cards_are_edited_and_deleted_and_a_new_number_replaces_the_old() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let id = vault.add_card(PROFILE_A, &visa()).unwrap();
    vault.update_card(&id, &card("Renamed Holder", "", 1, 2035, "Renamed")).unwrap();
    let saved = vault.saved_card(&id).unwrap();
    assert_eq!((saved.name.as_str(), saved.nickname.as_str(), saved.expiry(), saved.network, saved.last4.as_str()), ("Renamed Holder", "Renamed", "01/35".into(), CardNetwork::Visa, "4242"));
    assert_eq!(saved.profile, PROFILE_A, "an edit keeps the profile");
    assert_eq!(vault.card_number(&id).unwrap().as_str(), "4242424242424242", "a blank number keeps the old one");
    vault.update_card(&id, &card("Renamed Holder", MASTERCARD, 1, 2035, "Renamed")).unwrap();
    let saved = vault.saved_card(&id).unwrap();
    assert_eq!((saved.network, saved.last4.as_str()), (CardNetwork::Mastercard, "4444"));
    assert_eq!(vault.card_number(&id).unwrap().as_str(), "5555555555554444", "a new number replaces the old one");

    for unusual in [card("", "4242 4242 4242 4241", 1, 2030, ""), card("", VISA, 1, 2001, "Expired"), card("", "0000 0000 0000 0000", 1, 2030, "")] {
        let unusual_id = vault.add_card(PROFILE_A, &unusual).unwrap();
        assert_eq!(vault.card_number(&unusual_id).unwrap().as_str(), unusual.digits().as_str(), "saved as typed");
    }
    for (case, profile, refused) in [
        ("letters in the number", PROFILE_A, card("", "4242 4242 4242 42AB", 1, 2030, "")),
        ("seven digits", PROFILE_A, card("", "4242424", 1, 2030, "")),
        ("twenty digits", PROFILE_A, card("", "42424242424242424242", 1, 2030, "")),
        ("no number", PROFILE_A, card("", "", 1, 2030, "")),
        ("month 0", PROFILE_A, card("", VISA, 0, 2030, "")),
        ("month 13", PROFILE_A, card("", VISA, 13, 2030, "")),
        ("year 1999", PROFILE_A, card("", VISA, 1, 1999, "")),
        ("year 2100", PROFILE_A, card("", VISA, 1, 2100, "")),
        ("long name", PROFILE_A, card(&"N".repeat(MAX_CARD_NAME_BYTES + 1), VISA, 1, 2030, "")),
        ("long nickname", PROFILE_A, card("", VISA, 1, 2030, &"K".repeat(MAX_NICKNAME_BYTES + 1))),
        ("no profile", "", visa()),
    ] {
        assert!(matches!(vault.add_card(profile, &refused), Err(VaultError::Invalid(_))), "{case}");
    }
    assert!(matches!(vault.update_card(&id, &card("", "12", 1, 2030, "")), Err(VaultError::Invalid(_))));
    assert_eq!(vault.cards_in(PROFILE_A).len(), 4);
    assert_eq!(vault.card_count(), Ok(4), "nothing refused was written");

    vault.mark_card_used(&id, 5_000).unwrap();
    vault.delete_card(&id).unwrap();
    assert!(vault.saved_card(&id).is_none());
    assert_eq!(vault.delete_card(&id), Err(VaultError::NotFound));
    assert_eq!(vault.update_card(&id, &visa()), Err(VaultError::NotFound));
    assert_eq!(vault.card_number(&id).map(|_| ()), Err(VaultError::NotFound));
    let kept = vault.cards_in(PROFILE_A)[0].id.clone();
    vault.mark_card_used(&kept, 7_000).unwrap();
    drop(vault);
    let vault = fixture.unlocked();
    assert_eq!(vault.cards().len(), 3);
    assert_eq!(vault.saved_card(&kept).unwrap().used_ms, Some(7_000), "the last fill persists");
}

/// Locking drops every opened card, and no card is read or written while
/// locked; the idle lock does the same.
#[test]
fn locking_drops_every_opened_card() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let id = vault.add_card(PROFILE_A, &visa()).unwrap();
    vault.lock();
    assert!(vault.cards().is_empty() && vault.cards_in(PROFILE_A).is_empty() && vault.saved_card(&id).is_none());
    assert_eq!(vault.add_card(PROFILE_A, &mastercard()), Err(VaultError::Locked));
    assert_eq!(vault.update_card(&id, &mastercard()), Err(VaultError::Locked));
    assert_eq!(vault.delete_card(&id), Err(VaultError::Locked));
    assert_eq!(vault.mark_card_used(&id, 1), Err(VaultError::Locked));
    assert_eq!(vault.card_number(&id).map(|_| ()), Err(VaultError::Locked));
    vault.unlock(Moment::now()).unwrap();
    assert_eq!(vault.saved_card(&id).unwrap().last4.as_str(), "4242");
    let later = Moment::now().after(IDLE_TIMEOUT + Duration::from_secs(1));
    assert!(vault.lock_if_idle(later));
    assert!(vault.cards().is_empty());
}

/// A profile tag, a card or a number moved to another row, or a cell of the
/// wrong type, opens nowhere: the row is listed damaged, in no profile, its
/// number never opens, and it can only be deleted. The others still open.
#[test]
fn a_moved_or_retyped_card_cell_fails_closed_and_can_be_deleted() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let [a, b, c, d, e] = [visa(), mastercard(), visa(), mastercard(), visa()].map(|card| vault.add_card(PROFILE_A, &card).unwrap());
    let kept = vault.add_card(PROFILE_A, &mastercard()).unwrap();
    drop(vault);
    damage(&fixture, &format!("UPDATE cards SET profile = (SELECT profile FROM cards WHERE id = '{b}') WHERE id = ?1"), &a);
    damage(&fixture, &format!("UPDATE cards SET number = (SELECT number FROM cards WHERE id = '{c}') WHERE id = ?1"), &b);
    damage(&fixture, &format!("UPDATE cards SET card = (SELECT card FROM cards WHERE id = '{d}') WHERE id = ?1"), &c);
    damage(&fixture, "UPDATE cards SET number = 'plain text' WHERE id = ?1", &d);
    damage(&fixture, "UPDATE cards SET id = CAST(id AS BLOB) WHERE id = ?1", &e);
    let mut vault = fixture.unlocked();
    assert_eq!(vault.cards().len(), 6);
    let damaged: Vec<&SavedCard> = vault.cards().iter().filter(|saved| !saved.readable()).collect();
    assert_eq!(damaged.len(), 5);
    assert!(damaged.iter().all(|saved| saved.profile.is_empty() && saved.last4.is_empty() && saved.name.is_empty() && saved.network == CardNetwork::Unknown));
    assert_eq!(shown(&vault.cards_in(PROFILE_A)), [(CardNetwork::Mastercard, "4444".into(), 12, 2031, String::new())], "a damaged card is never filled");
    let damaged: Vec<String> = damaged.iter().map(|saved| saved.id.clone()).collect();
    assert!(damaged.iter().any(|id| id.starts_with("damaged-row:")), "{damaged:?}");
    for id in [&a, &b] {
        assert_eq!(vault.card_number(id).map(|_| ()), Err(VaultError::Unreadable));
        assert_eq!(vault.update_card(id, &visa()), Err(VaultError::Unreadable));
    }
    assert_eq!(vault.card_number(&kept).unwrap().as_str(), "5555555555554444");
    for id in &damaged { vault.delete_card(id).unwrap(); }
    drop(vault);
    assert_eq!(fixture.unlocked().cards().len(), 1);
}

/// Reset empties the cards with everything else.
#[test]
fn reset_deletes_every_card() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    vault.add_card(PROFILE_A, &visa()).unwrap();
    vault.reset().unwrap();
    assert_eq!(vault.card_count(), Ok(0));
    assert_eq!(vault.status(), Ok(VaultStatus::New));
}

/// A vault holding only cards is an existing vault: with its check gone,
/// the stored key that opens a card rewrites it, and a foreign key is a mismatch.
#[test]
fn a_card_only_vault_repairs_its_check_with_the_key_that_opens_it() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let id = vault.add_card(PROFILE_A, &visa()).unwrap();
    let vault_id = vault.id().to_owned();
    drop(vault);
    raw(&fixture).execute("DELETE FROM meta WHERE k = 'check'", []).unwrap();
    let mut vault = fixture.open();
    assert_eq!(vault.status(), Ok(VaultStatus::Locked));
    assert_eq!(vault.unlock(Moment::now()), Ok(UnlockOutcome::CheckRepaired));
    assert_eq!(vault.card_number(&id).unwrap().as_str(), "4242424242424242");
    drop(vault);
    raw(&fixture).execute("DELETE FROM meta WHERE k = 'check'", []).unwrap();
    let foreign = MemoryVaultKeyStore::default();
    foreign.save(&vault_id, &VaultKey::from_slice(&[9; 32]).unwrap()).unwrap();
    let mut vault = Vault::open(fixture.dir.path(), Box::new(foreign)).unwrap();
    assert_eq!(vault.unlock(Moment::now()), Err(VaultError::KeyMismatch));
}

/// The cards table has these columns and no other: no security code is
/// ever stored, and a file with such a column is damage.
#[test]
fn no_security_code_column_exists() {
    let fixture = Fixture::new();
    drop(fixture.unlocked());
    let conn = raw(&fixture);
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('cards') ORDER BY cid").unwrap();
    let names: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(names, ["id", "created_ms", "updated_ms", "profile", "card", "number", "used_ms"]);
    assert!(names.iter().all(|name| !["cvc", "cvv", "csc", "security", "code"].iter().any(|word| name.contains(word))));
    drop(stmt);
    conn.execute_batch("ALTER TABLE cards ADD COLUMN cvc BLOB").unwrap();
    drop(conn);
    assert_eq!(Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).map(|_| ()), Err(VaultError::Damaged));
}

/// A format-5 vault, as the build before cards wrote it: entries, the
/// never-save list and an address.
fn format_5_vault() -> (Fixture, String, [String; 2], String) {
    let (fixture, id, ids) = probed_vault();
    let mut vault = fixture.unlocked();
    let address = vault.add_address(PROFILE_A, &home()).unwrap();
    drop(vault);
    raw(&fixture).execute_batch("DROP TABLE cards; UPDATE meta SET v = 5 WHERE k = 'format';").unwrap();
    assert_eq!((raw_format(fixture.dir.path()).0, has_table(&raw(&fixture), "cards")), (Some(5), false), "a format-5 file");
    (fixture, id, ids, address)
}

/// Opening a format-5 vault adds the cards table in one transaction that
/// checks the result before it commits; the entries, the never-save list
/// and the addresses are untouched.
#[test]
fn opening_a_format_5_vault_upgrades_it_to_format_8_and_keeps_everything() {
    let (fixture, id, ids, address) = format_5_vault();
    let mut vault = fixture.unlocked();
    assert_eq!(vault.id(), id);
    assert_eq!(raw_format(fixture.dir.path()).0, Some(8));
    assert!(has_table(&raw(&fixture), "cards"));
    assert_eq!(passwords_held(fixture.dir.path(), &fixture.keys, &ids), 2);
    assert_eq!(vault.never_saved(PROFILE_A).unwrap(), ["https://login.example"]);
    assert_eq!(vault.saved_address(&address).unwrap().address, home());
    let card = vault.add_card(PROFILE_A, &visa()).unwrap();
    drop(vault);
    assert_eq!(fixture.unlocked().card_number(&card).unwrap().as_str(), "4242424242424242");
}

#[test]
fn an_upgrade_from_format_5_cut_halfway_leaves_it_whole_and_the_next_open_finishes_it() {
    let (fixture, _, ids, address) = format_5_vault();
    let snapshot = tempfile::tempdir().unwrap();
    let (source, copy) = (fixture.dir.path().to_path_buf(), snapshot.path().to_path_buf());
    super::db::UPGRADE_CUT.set(Some(Box::new(move |_: &rusqlite::Connection| {
        for name in [FILE_NAME.to_string(), format!("{FILE_NAME}-wal")] {
            if source.join(&name).exists() { std::fs::copy(source.join(&name), copy.join(&name)).unwrap(); }
        }
        true
    })));
    let error = Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).unwrap_err();
    super::db::UPGRADE_CUT.set(None);
    assert_eq!(error, VaultError::Storage);
    for (case, dir) in [("rolled back", fixture.dir.path()), ("power cut", snapshot.path())] {
        let conn = rusqlite::Connection::open(dir.join(FILE_NAME)).unwrap();
        assert_eq!((raw_format(dir).0, has_table(&conn, "cards"), has_table(&conn, "addresses")), (Some(5), false, true), "{case}: wholly format 5");
        drop(conn);
        assert_eq!(passwords_held(dir, &fixture.keys, &ids), 2, "{case}");
        assert_eq!(raw_format(dir).0, Some(8), "{case}: the next open upgrades it");
        let mut vault = Vault::open(dir, Box::new(fixture.keys.clone())).unwrap();
        vault.unlock(Moment::now()).unwrap();
        assert_eq!(vault.saved_address(&address).unwrap().address, home(), "{case}: the address is kept");
    }
}

/// An upgrade that would leave a file this build can't read is rolled back
/// before it commits: the check runs inside the transaction.
#[test]
fn an_upgrade_whose_result_fails_its_check_is_rolled_back() {
    let (fixture, _, ids, _) = format_5_vault();
    // Halfway, inside the upgrade's own transaction, another column appears
    // in the new table, as a bug or damage would make it.
    super::db::UPGRADE_CUT.set(Some(Box::new(|tx: &rusqlite::Connection| {
        tx.execute_batch("ALTER TABLE cards ADD COLUMN cvc BLOB").unwrap();
        false
    })));
    let error = Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).map(|_| ());
    super::db::UPGRADE_CUT.set(None);
    assert_eq!(error, Err(VaultError::Damaged), "the upgrade didn't commit");
    let conn = raw(&fixture);
    assert_eq!((raw_format(fixture.dir.path()).0, has_table(&conn, "cards")), (Some(5), false), "wholly format 5");
    drop(conn);
    assert_eq!(passwords_held(fixture.dir.path(), &fixture.keys, &ids), 2, "the next open upgrades it");
}

/// A file with any part of format 8 is format 8: one without its cards
/// table, with a format row other than 8, or with a changed cards table is
/// damage, and opening it writes nothing.
#[test]
fn a_file_with_part_of_format_8_is_damage_unless_it_is_all_of_it() {
    for (case, sql) in [
        ("cards dropped", "DROP TABLE cards"),
        ("format 5 with cards", "UPDATE meta SET v = 5 WHERE k = 'format'"),
        ("format 2 with cards only", "UPDATE meta SET v = 2 WHERE k = 'format'; DROP TABLE addresses"),
        ("format row deleted", "DELETE FROM meta WHERE k = 'format'"),
        ("number column renamed", "ALTER TABLE cards RENAME COLUMN number TO numbr"),
        ("used_ms dropped", "ALTER TABLE cards DROP COLUMN used_ms"),
        ("security code added", "ALTER TABLE cards ADD COLUMN csc BLOB"),
    ] {
        let (fixture, _, _) = probed_vault();
        raw(&fixture).execute_batch(sql).unwrap();
        let image = checkpointed(&fixture);
        assert_eq!(Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).map(|_| ()), Err(VaultError::Damaged), "{case}");
        assert_eq!(std::fs::read(fixture.dir.path().join(FILE_NAME)).unwrap(), image, "{case}: opening wrote nothing");
    }
}

/// Recover copies every card whose profile, details and number open, with
/// its times and last use, counts the cards apart from the passwords and the
/// addresses, and the new file opens them.
#[test]
fn recover_keeps_and_counts_cards_apart_from_passwords_and_addresses() {
    let (fixture, _, ids) = probed_vault();
    let mut vault = fixture.unlocked();
    let kept = [vault.add_card(PROFILE_A, &visa()).unwrap(), vault.add_card(PROFILE_B, &mastercard()).unwrap()];
    vault.mark_card_used(&kept[0], 3_000).unwrap();
    let lost = vault.add_card(PROFILE_A, &visa()).unwrap();
    vault.add_address(PROFILE_A, &home()).unwrap();
    drop(vault);
    damage(&fixture, "UPDATE cards SET number = zeroblob(64) WHERE id = ?1", &lost);
    raw(&fixture).execute_batch(&on_its_own_index()).unwrap();
    assert_eq!(Vault::open(fixture.dir.path(), Box::new(fixture.keys.clone())).map(|_| ()), Err(VaultError::Damaged));
    let recovery = Vault::recover_damaged_file(fixture.dir.path(), &fixture.keys).unwrap();
    assert_eq!(recovery.cards, Counted { recovered: 2, found: 3, complete: true });
    assert_eq!(recovery.addresses, Counted { recovered: 1, found: 1, complete: true });
    assert_eq!(passwords_held(fixture.dir.path(), &fixture.keys, &ids), recovery.recovered);
    let vault = fixture.unlocked();
    assert_eq!(vault.cards().len(), 2);
    let saved = vault.saved_card(&kept[0]).unwrap();
    assert_eq!((saved.last4.as_str(), saved.profile.as_str(), saved.used_ms), ("4242", PROFILE_A, Some(3_000)));
    assert_eq!(vault.card_number(&kept[1]).unwrap().as_str(), "5555555555554444");
}

/// A format-8 file that lost its format row and its cards table: the
/// passwords come back and the page can say the cards weren't read in full.
/// A format-5 file has no cards to read, so it is complete.
#[test]
fn recover_says_when_the_cards_table_is_gone() {
    let (fixture, _, ids) = probed_vault();
    raw(&fixture).execute_batch("DELETE FROM meta WHERE k = 'format'; DROP TABLE cards;").unwrap();
    let recovery = Vault::recover_damaged_file(fixture.dir.path(), &fixture.keys).unwrap();
    assert_eq!((recovery.recovered, recovery.found), (2, 2));
    assert_eq!(recovery.cards, Counted { recovered: 0, found: 0, complete: false });
    assert_eq!(passwords_held(fixture.dir.path(), &fixture.keys, &ids), 2);

    let (fixture, _, _, _) = format_5_vault();
    raw(&fixture).execute_batch(&on_its_own_index()).unwrap();
    let recovery = Vault::recover_damaged_file(fixture.dir.path(), &fixture.keys).unwrap();
    assert_eq!(recovery.cards, Counted { recovered: 0, found: 0, complete: true });
    assert_eq!(recovery.addresses, Counted { recovered: 1, found: 1, complete: true });
    assert_eq!(raw_format(fixture.dir.path()).0, Some(8), "the recovered file is the current format");
}

/// Each card row a plain scan of the table reads, both ways: the rowids it
/// sees, and the rows whose id and sealed cells read as stored.
fn scan_cards(dir: &Path) -> (usize, std::collections::BTreeSet<(String, Vec<u8>, Vec<u8>, Vec<u8>)>) {
    let conn = rusqlite::Connection::open_with_flags(dir.join(FILE_NAME), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let (mut seen, mut read) = (std::collections::BTreeSet::new(), std::collections::BTreeSet::new());
    for sql in ["SELECT rowid, id, profile, card, number FROM cards NOT INDEXED", "SELECT rowid, id, profile, card, number FROM cards NOT INDEXED ORDER BY rowid DESC"] {
        let Ok(mut stmt) = conn.prepare(sql) else { continue };
        let Ok(mut rows) = stmt.query([]) else { continue };
        while let Ok(Some(row)) = rows.next() {
            let Ok(rowid) = row.get::<_, i64>(0) else { continue };
            if (1..5).all(|i| matches!(row.get_ref(i), Ok(rusqlite::types::ValueRef::Null))) { continue; }
            seen.insert(rowid);
            if let (Ok(id), Ok(profile), Ok(card), Ok(number)) = (row.get::<_, String>(1), row.get::<_, Vec<u8>>(2), row.get::<_, Vec<u8>>(3), row.get::<_, Vec<u8>>(4)) {
                read.insert((id, profile, card, number));
            }
        }
    }
    (seen.len(), read)
}

/// The address probe's sweep, for cards: every damage-at-open flip of the
/// cards table's pages. Recover keeps every card a plain scan reads as
/// stored, counts at least every row the scan sees, says how many it kept,
/// and says so whenever it kept fewer than the file held. The passwords
/// are never touched.
#[test]
fn recover_keeps_every_card_a_scan_reads_through_any_flip_of_its_table() {
    let (fixture, _, ids) = probed_vault();
    let mut vault = fixture.unlocked();
    for card in [visa(), mastercard(), visa()] { vault.add_card(PROFILE_A, &card).unwrap(); }
    drop(vault);
    let (_, original_rows) = scan_cards(fixture.dir.path());
    assert_eq!(original_rows.len(), 3);
    let original = checkpointed(&fixture);
    let conn = raw(&fixture);
    let page_size = conn.query_row("PRAGMA page_size", [], |r| r.get::<_, i64>(0)).unwrap() as usize;
    let mut stmt = conn.prepare("SELECT name, rootpage FROM sqlite_master WHERE tbl_name = 'cards'").unwrap();
    let pages: Vec<(String, usize)> = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))).unwrap().map(Result::unwrap).collect();
    drop(stmt);
    drop(conn);
    let work = Fixture { dir: tempfile::tempdir().unwrap(), keys: fixture.keys.clone() };
    let dir = work.dir.path();
    let (mut tally, mut short) = (std::collections::BTreeMap::<String, usize>::new(), Vec::new());
    for (name, page) in &pages {
        for offset in read_bytes(&original, page_size, *page) {
            for bit in 0..8 {
                let mut bytes = original.clone();
                bytes[offset] ^= 1 << bit;
                write_image(dir, &bytes);
                if Vault::open(dir, Box::new(work.keys.clone())).map(|_| ()) != Err(VaultError::Damaged) { continue; }
                write_image(dir, &bytes);
                let (seen, read) = scan_cards(dir);
                write_image(dir, &bytes);
                let flip = format!("{name} byte {offset} bit {bit}");
                let Ok(recovery) = Vault::recover_damaged_file(dir, &work.keys) else {
                    short.push(format!("{flip}: recover failed"));
                    continue;
                };
                let (_, rows) = scan_cards(dir);
                let intact: std::collections::BTreeSet<_> = read.intersection(&original_rows).cloned().collect();
                if !intact.is_subset(&rows) { short.push(format!("{flip}: lost {} cards a scan reads", intact.difference(&rows).count())); }
                if !rows.is_subset(&original_rows) { short.push(format!("{flip}: holds a changed card")); }
                let counted = &recovery.cards;
                if counted.recovered != rows.len() { short.push(format!("{flip}: says {} kept, holds {}", counted.recovered, rows.len())); }
                if counted.found < seen { short.push(format!("{flip}: counted {} of the {seen} rows a scan sees", counted.found)); }
                let lost = original_rows.difference(&rows).count();
                if lost > 0 && counted.recovered >= counted.found && counted.complete { short.push(format!("{flip}: lost {lost} of 3 without saying so")); }
                if passwords_held(dir, &work.keys, &ids) != 2 { short.push(format!("{flip}: passwords lost")); }
                *tally.entry(format!("{name}: kept {} of {} seen{}, lost {lost} of 3", counted.recovered, counted.found, if counted.complete { "" } else { ", not read in full" })).or_default() += 1;
                for name in moved_aside(dir) { std::fs::remove_file(dir.join(name)).unwrap(); }
            }
        }
    }
    for (outcome, n) in &tally { eprintln!("{n:5}  {outcome}"); }
    assert!(!tally.is_empty(), "some flip is damage at open");
    assert!(short.is_empty(), "{} flips: {short:#?}", short.len());
}

/// ASP-P16: no number, name, nickname, expiry or profile of a card reaches
/// the vault file in plaintext, through an add, an edit or a delete, nor
/// any number's last four digits; the library store and its sync queue
/// never see one. A deleted card's row leaves the file, id and all.
#[test]
fn no_part_of_a_card_reaches_the_vault_file_or_the_library_store() {
    let fixture = Fixture::new();
    let store = crate::store::Store::open(&fixture.dir.path().join("crossbow.sqlite")).unwrap();
    let mut lib = crate::library::Library::open(store).unwrap();
    lib.ensure_defaults().unwrap();
    let (dirty, entities) = (lib.store.dirty_count().unwrap(), lib.store.all_entities().unwrap().len());
    let bytes = sealed_cards(&fixture);
    assert!(bytes == sealed_cards(&Fixture::new()), "the same steps wrote another file");
    assert!(contains(&bytes, ROW_ID), "the search reads the ids");
    assert!(!contains(&bytes, OTHER_ROW_ID), "the deleted card's id stayed in the file");
    for text in ["4242424242424242", "5555555555554444", "378282246310005", "4242", "4444", "0005", "secret-holder-ada", "secret-nickname-one", "secret-nickname-two",
        "secret-profile-tag", "deleted-holder-9", "deleted-nickname-9", "09/33", "2033", "03/32", "2032"] {
        assert!(!contains(&bytes, text), "{text} reached the vault file");
    }
    assert_eq!((lib.store.dirty_count().unwrap(), lib.store.all_entities().unwrap().len()), (dirty, entities), "nothing queued for sync");
    let library_bytes = std::fs::read(fixture.dir.path().join("crossbow.sqlite")).unwrap();
    assert!(!contains(&library_bytes, "5555555555554444") && !contains(&library_bytes, "secret-holder-ada"));
}

/// Add, edit, delete and fill cards under a script and return the vault's
/// file once closed.
fn sealed_cards(fixture: &Fixture) -> Vec<u8> {
    let script = script(&[VAULT_ID, ROW_ID, OTHER_ROW_ID]);
    let mut vault = fixture.unlocked();
    let id = vault.add_card("secret-profile-tag", &card("secret-holder-ada", "4242424242424242", 9, 2033, "secret-nickname-one")).unwrap();
    vault.update_card(&id, &card("secret-holder-ada", "5555555555554444", 9, 2033, "secret-nickname-two")).unwrap();
    let gone = vault.add_card(PROFILE_A, &card("deleted-holder-9", "378282246310005", 3, 2032, "deleted-nickname-9")).unwrap();
    vault.delete_card(&gone).unwrap();
    vault.mark_card_used(&id, 1).unwrap();
    assert_eq!((vault.db().vault_id.as_str(), id.as_str(), gone.as_str()), (VAULT_ID, ROW_ID, OTHER_ROW_ID), "each id is the scripted one");
    drop(vault);
    assert_eq!(script.ids_left(), 0);
    fixture.file_bytes()
}

/// Debug output of a card, typed or saved, shows only its network and last
/// four digits: never the number, name, expiry, nickname or profile.
#[test]
fn debug_output_of_a_card_shows_only_its_network_and_last_four() {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let typed = card("debug-holder", "5555 5555 5555 4444", 11, 2032, "debug-nickname");
    let id = vault.add_card("debug-profile", &typed).unwrap();
    assert_eq!(format!("{typed:?}"), r#"Card { network: Mastercard, last4: "4444", .. }"#);
    assert_eq!(format!("{:?}", vault.saved_card(&id).unwrap()), r#"SavedCard { network: Mastercard, last4: "4444", .. }"#);
    assert_eq!(format!("{:?}", card("", "12", 1, 2030, "")), r#"Card { network: Unknown, last4: "", .. }"#, "a short number shows no digits");
    let printed = format!("{typed:#?} {:#?} {:?} {vault:?}", vault.saved_card(&id).unwrap(), vault.cards());
    for secret in ["5555 5555", "555555555555", "debug-holder", "debug-nickname", "debug-profile", "2032", "11"] {
        assert!(!printed.contains(secret), "{printed} prints {secret}");
    }
}

// -- Round 9: a stopped Recover's undo (R8-2, R7-2) ---------------------------------------

/// Every file in `dir` with its bytes.
fn files_and_bytes(dir: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(dir).unwrap().map(|file| file.unwrap()).filter(|file| file.file_type().unwrap().is_file())
        .map(|file| (file.file_name().to_string_lossy().into_owned(), std::fs::read(file.path()).unwrap())).collect()
}

/// R8-2, ported from the eighth recheck's stale-WAL probe: the vault's
/// newest pages are in its WAL; Recover stopped after moving that WAL beside
/// its kept name (done here by hand, as the probe did), and the undo can't
/// move it back (an immutable flag). The vault must not open without that
/// WAL, so no password is saved in its place and no new vault is made on
/// top; Recover and Reset refuse too, and every file stays as it is. With
/// the flag cleared, a stop made by hand has no record of the vault file,
/// so the WAL still isn't put back: nothing is ever replayed over writes
/// that came after it.
#[cfg(target_os = "macos")]
#[test]
fn ported_r8_2_the_vault_never_opens_without_the_wal_an_undo_couldnt_move_back() {
    const NAME: &str = "vault-damaged-20261029T000000Z.sqlite";
    let (copy, _, _, _) = damaged_in_its_wal();
    let dir = copy.dir.path();
    std::fs::hard_link(dir.join(FILE_NAME), dir.join(NAME)).unwrap();
    std::fs::rename(dir.join(format!("{FILE_NAME}-wal")), dir.join(format!("{NAME}-wal"))).unwrap();
    let before = files_and_bytes(dir);
    let fixed = Immutable::set(&dir.join(format!("{NAME}-wal")));
    for (attempt, result) in [
        ("open", Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ())),
        ("recover", Vault::recover_damaged_file(dir, &copy.keys).map(|_| ())),
        ("reset", Vault::replace_damaged_file(dir).map(|_| ())),
    ] {
        let error = result.expect_err(attempt);
        assert!(!error.needs_reset() && error != VaultError::Damaged, "{attempt}: {error:?}");
        for kept in [NAME.to_owned(), format!("{NAME}-wal")] { assert!(error.to_string().contains(&kept), "{attempt} names {kept}: {error}"); }
        assert_eq!(files_and_bytes(dir), before, "{attempt}: every file as it was, and none made");
    }
    drop(fixed);
    let error = Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()).expect_err("no record says the WAL belongs beside this file");
    assert!(error.to_string().contains(NAME), "{error}");
    assert_eq!(files_and_bytes(dir), before);
}

/// Run Recover Passwords on `dir` until it crashes after `step` of its swap:
/// nothing it did is undone, as after a force quit.
fn crash_recover(dir: &Path, keys: &MemoryVaultKeyStore, step: u8) {
    super::test_hooks::SWAP_CRASH.set(Some(step));
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Vault::recover_damaged_file(dir, keys)));
    super::test_hooks::SWAP_CRASH.set(None);
    assert!(crashed.is_err(), "Recover crashed after step {step}");
}

/// The name a crashed Recover's record is kept beside.
fn recorded_name(dir: &Path) -> String {
    moved_aside(dir).into_iter().find_map(|name| name.strip_suffix("-swap").map(str::to_owned)).expect("a record")
}

/// R8-2 and R7-2: a Recover that crashed after any step of its swap. The
/// steps are: its record of the vault file, the link under the kept name,
/// the WAL's move, the journal's, the shared memory's removal, and the
/// rename of the new file over the old. Before that rename, the next open
/// puts everything back: the file opens as it did before the Recover,
/// damaged, with its own WAL, and no kept name or record is left; the next
/// Recover keeps it under one name, the one it reports. After the rename,
/// the swap is done: the vault opens as recovered, and the damaged file the
/// crash kept it from naming is named until it is moved.
#[cfg(unix)]
#[test]
fn a_recover_that_crashed_is_undone_at_the_next_open_or_its_kept_file_is_named() {
    for step in 1..=6u8 {
        let (copy, ids, main, wal) = damaged_in_its_wal();
        let dir = copy.dir.path();
        crash_recover(dir, &copy.keys, step);
        if step < 6 {
            // What the open does first; the open then folds the WAL in.
            undo_stopped_swap(dir).unwrap();
            assert_eq!((std::fs::read(dir.join(FILE_NAME)).unwrap(), std::fs::read(dir.join(format!("{FILE_NAME}-wal"))).unwrap()), (main.clone(), wal.clone()), "step {step}: the file and its WAL");
        }
        let opened = Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ());
        if step < 6 {
            assert_eq!(opened, Err(VaultError::Damaged), "step {step}: as before the Recover");
            assert!(moved_aside(dir).is_empty(), "step {step}: {:?}", moved_aside(dir));
            assert!(Vault::kept_from_stopped_recover(dir).is_empty());
            let recovery = Vault::recover_damaged_file(dir, &copy.keys).unwrap();
            assert_eq!(moved_aside(dir), [recovery.moved.clone(), format!("{}-wal", recovery.moved)], "step {step}: one kept name, and no record");
            assert!(recovery.also_kept.is_empty(), "step {step}");
        } else {
            assert_eq!(opened, Ok(()), "the new file is in place");
            let kept = Vault::kept_from_stopped_recover(dir);
            assert_eq!(kept, [recorded_name(dir), format!("{}-wal", recorded_name(dir))], "the files the crash kept it from naming");
            for name in &kept { std::fs::remove_file(dir.join(name)).unwrap(); }
            assert!(Vault::kept_from_stopped_recover(dir).is_empty(), "named until they are moved");
            Vault::open(dir, Box::new(copy.keys.clone())).unwrap();
            assert!(moved_aside(dir).is_empty(), "their record goes with them: {:?}", moved_aside(dir));
        }
        assert_eq!(passwords_held(dir, &copy.keys, &ids), 2, "step {step}");
    }
}

/// R8-2, the eighth recheck's F6: a Recover crashed after moving the WAL,
/// and the undo can't move it back (here an immutable flag; in practice
/// any rename error). The vault isn't opened, reset or recovered: each
/// says which files are kept, and nothing creates or writes the vault file
/// or a WAL beside it, so no new vault is made on top. Once the WAL can
/// move, the next open puts it back and the file opens as it did before
/// the Recover, which then keeps it under one name.
#[cfg(target_os = "macos")]
#[test]
fn an_undo_that_cant_move_the_wal_back_keeps_the_vault_closed_and_names_every_kept_file() {
    let (copy, ids, _, wal) = damaged_in_its_wal();
    let dir = copy.dir.path();
    crash_recover(dir, &copy.keys, 3);
    let name = recorded_name(dir);
    let kept = vec![name.clone(), format!("{name}-wal"), RECOVERY_FILE.to_owned()];
    let before = files_and_bytes(dir);
    let modified = std::fs::metadata(dir.join(FILE_NAME)).unwrap().modified().unwrap();
    let fixed = Immutable::set(&dir.join(format!("{name}-wal")));
    for attempt in ["open", "recover", "reset", "open again"] {
        let result = match attempt {
            "recover" => Vault::recover_damaged_file(dir, &copy.keys).map(|_| ()),
            "reset" => Vault::replace_damaged_file(dir).map(|_| ()),
            _ => Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()),
        };
        assert_eq!(result, Err(VaultError::UndoIncomplete(kept.clone())), "{attempt}");
        assert!(!VaultError::UndoIncomplete(kept.clone()).needs_reset());
        assert_eq!(files_and_bytes(dir), before, "{attempt}: every file as it was, and none made");
        assert_eq!(std::fs::metadata(dir.join(FILE_NAME)).unwrap().modified().unwrap(), modified, "{attempt}: the vault file unwritten");
    }
    drop(fixed);
    undo_stopped_swap(dir).unwrap();
    assert_eq!(std::fs::read(dir.join(format!("{FILE_NAME}-wal"))).unwrap(), wal, "the WAL is back beside the file");
    assert_eq!(Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()), Err(VaultError::Damaged), "as before the Recover");
    assert!(moved_aside(dir).is_empty(), "{:?}", moved_aside(dir));
    let recovery = Vault::recover_damaged_file(dir, &copy.keys).unwrap();
    assert_eq!(moved_aside(dir), [recovery.moved.clone(), format!("{}-wal", recovery.moved)]);
    assert_eq!(passwords_held(dir, &copy.keys, &ids), 2);
}

/// R8-2, the stale WAL: while a crashed Recover's WAL is kept aside, the
/// vault file changes, as if another process or an older Crossbow opened
/// it without that WAL and saved; or another process makes a WAL of its own
/// beside it; or the kept WAL is no longer the one moved. The undo never
/// puts a WAL back beside a file it didn't come from: every file stays where
/// it is, and the vault stays closed, naming them. Removing the stray WAL
/// lets the next open put the kept one back.
#[cfg(unix)]
#[test]
fn a_kept_wal_is_never_put_back_beside_a_changed_vault_file() {
    for case in ["the vault file written", "a WAL of its own beside it", "the kept WAL replaced"] {
        let (copy, ids, _, wal) = damaged_in_its_wal();
        let dir = copy.dir.path();
        crash_recover(dir, &copy.keys, 3);
        let name = recorded_name(dir);
        match case {
            "the vault file written" => {
                let other = rusqlite::Connection::open(dir.join(FILE_NAME)).unwrap();
                other.execute_batch("PRAGMA journal_mode = DELETE; INSERT INTO meta (k, v) VALUES ('fixture-newer', 'write')").unwrap();
            }
            "a WAL of its own beside it" => std::fs::write(dir.join(format!("{FILE_NAME}-wal")), b"another process's WAL").unwrap(),
            _ => std::fs::write(dir.join(format!("{name}-wal")), b"not the WAL that was moved").unwrap(),
        }
        let before = files_and_bytes(dir);
        for attempt in ["open", "recover", "reset"] {
            let result = match attempt {
                "recover" => Vault::recover_damaged_file(dir, &copy.keys).map(|_| ()),
                "reset" => Vault::replace_damaged_file(dir).map(|_| ()),
                _ => Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()),
            };
            assert_eq!(result, Err(VaultError::UndoIncomplete(vec![name.clone(), format!("{name}-wal"), RECOVERY_FILE.to_owned()])), "{case}: {attempt}");
            assert_eq!(files_and_bytes(dir), before, "{case}: {attempt}: every file where it is");
        }
        if case == "a WAL of its own beside it" {
            std::fs::remove_file(dir.join(format!("{FILE_NAME}-wal"))).unwrap();
            undo_stopped_swap(dir).unwrap();
            assert_eq!(std::fs::read(dir.join(format!("{FILE_NAME}-wal"))).unwrap(), wal, "the kept WAL goes back once the stray one is gone");
            assert_eq!(Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()), Err(VaultError::Damaged));
            assert_eq!(Vault::recover_damaged_file(dir, &copy.keys).map(|_| passwords_held(dir, &copy.keys, &ids)), Ok(2));
        }
    }
}

/// R7-2: files a crashed Recover left beside another vault file, as when it
/// crashed just after its final rename, are named while the vault opens and
/// while it is damaged, and by the next Recover with its own kept name,
/// until they are moved. The open after the crash finds the vault file is
/// the one the Recover made, so the Recover finished (R9-1, round 10: the
/// vault file was damaged here before any open in round 9's test). A vault
/// file written before any open is neither the file the Recover kept nor
/// the one it made, so the vault stays closed, naming the kept files.
#[cfg(unix)]
#[test]
fn the_next_recover_names_every_file_a_crashed_recover_left() {
    let (copy, ids, _, _) = damaged_in_its_wal();
    let dir = copy.dir.path();
    crash_recover(dir, &copy.keys, 6);
    Vault::open(dir, Box::new(copy.keys.clone())).unwrap();
    let earlier = Vault::kept_from_stopped_recover(dir);
    assert_eq!(earlier.len(), 2, "{earlier:?}");
    let damage = rusqlite::Connection::open(dir.join(FILE_NAME)).unwrap();
    damage.execute_batch("ALTER TABLE entries RENAME COLUMN notes TO nktes").unwrap();
    drop(damage);
    assert_eq!(Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()), Err(VaultError::Damaged));
    assert_eq!(Vault::kept_from_stopped_recover(dir), earlier, "named while the vault is damaged");
    let recovery = Vault::recover_damaged_file(dir, &copy.keys).unwrap();
    assert_eq!(recovery.also_kept, earlier, "the next Recover names them with its own");
    assert!(!earlier.contains(&recovery.moved));
    assert_eq!(passwords_held(dir, &copy.keys, &ids), 2);
    assert_eq!(Vault::kept_from_stopped_recover(dir), earlier, "until they are moved");
    for name in &earlier { std::fs::remove_file(dir.join(name)).unwrap(); }
    assert!(Vault::kept_from_stopped_recover(dir).is_empty());

    let (copy, _, _, _) = damaged_in_its_wal();
    let dir = copy.dir.path();
    crash_recover(dir, &copy.keys, 6);
    let name = recorded_name(dir);
    let other = rusqlite::Connection::open(dir.join(FILE_NAME)).unwrap();
    other.execute_batch("PRAGMA journal_mode = DELETE; INSERT INTO meta (k, v) VALUES ('fixture-newer', 'write')").unwrap();
    drop(other);
    let before = files_and_bytes(dir);
    assert_eq!(Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()), Err(VaultError::UndoIncomplete(vec![name.clone(), format!("{name}-wal")])), "written before any open");
    assert!(Vault::kept_from_stopped_recover(dir).is_empty(), "named by the error instead");
    assert_eq!(files_and_bytes(dir), before);
}

// -- round 10: an undo decides by the vault file's identity, never by a name (R9-1) --

const PASSWORD_C: &str = "fixture-password-charlie-9X";

/// Ported from the ninth recheck's probes: a healthy vault of two entries
/// whose second, B, is still only in its WAL.
fn healthy_with_its_newest_entry_in_its_wal() -> (Fixture, [String; 2]) {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let first = vault.add(&tagged("https://one.example", "fixture", PASSWORD_A, PROFILE_A)).unwrap();
    let holder = raw(&fixture);
    holder.execute_batch("PRAGMA wal_autocheckpoint = 0").unwrap();
    // A read, so the holder keeps its lock and the vault's close can't fold the WAL in.
    let _: i64 = holder.query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0)).unwrap();
    let second = vault.add(&tagged("https://two.example", "fixture", PASSWORD_B, PROFILE_A)).unwrap();
    drop(vault);
    let copy = Fixture { dir: tempfile::tempdir().unwrap(), keys: fixture.keys.clone() };
    for suffix in ["", "-wal"] { std::fs::copy(fixture.dir.path().join(format!("{FILE_NAME}{suffix}")), copy.dir.path().join(format!("{FILE_NAME}{suffix}"))).unwrap(); }
    drop(holder);
    (copy, [first, second])
}

/// Ported from the ninth recheck's probes: a damaged vault whose first
/// entry, A, is in its file, and whose second, B, and the damage are still
/// only in its WAL: the state a Recover is offered for, with a commit the
/// file alone lacks.
fn damaged_with_its_newest_entry_in_its_wal() -> (Fixture, [String; 2]) {
    let fixture = Fixture::new();
    let mut vault = fixture.unlocked();
    let first = vault.add(&tagged("https://one.example", "fixture", PASSWORD_A, PROFILE_A)).unwrap();
    let holder = raw(&fixture);
    let busy: i64 = holder.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0)).unwrap();
    assert_eq!(busy, 0, "the schema and A reach the file first");
    holder.execute_batch("PRAGMA wal_autocheckpoint = 0").unwrap();
    let _: i64 = holder.query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0)).unwrap();
    let second = vault.add(&tagged("https://two.example", "fixture", PASSWORD_B, PROFILE_A)).unwrap();
    drop(vault);
    holder.execute_batch("ALTER TABLE entries RENAME COLUMN notes TO nktes").unwrap();
    let copy = Fixture { dir: tempfile::tempdir().unwrap(), keys: fixture.keys.clone() };
    for suffix in ["", "-wal"] { std::fs::copy(fixture.dir.path().join(format!("{FILE_NAME}{suffix}")), copy.dir.path().join(format!("{FILE_NAME}{suffix}"))).unwrap(); }
    drop(holder);
    (copy, [first, second])
}

/// Whether the vault in `dir` opens and holds A, B and C.
fn holds_a_b_and_c(dir: &Path, keys: &MemoryVaultKeyStore, ids: &[String; 2]) -> bool {
    let Ok(mut vault) = Vault::open(dir, Box::new(keys.clone())) else { return false };
    if vault.unlock(Moment::now()).is_err() { return false; }
    let c = vault.entries().iter().any(|entry| vault.password(&entry.id).is_ok_and(|password| password.as_str() == PASSWORD_C));
    drop(vault);
    c && passwords_held(dir, keys, ids) == 2
}

/// R9-1, ported from the ninth recheck's hand-removal probes: a Recover
/// stopped after moving the WAL beside its kept name (step 3) or after
/// dropping the shared memory (step 5), on a damaged vault and on a healthy
/// one, each with its newest entry, B, only in its WAL. The user then
/// removes the kept name in Finder. The vault file is still the very file
/// the record describes, so the Recover didn't finish, whatever happened to
/// that name: no file is named as left over, the next open puts the WAL
/// back first, and B is kept. Nothing is left to delete or move back by
/// hand, and a later save, C, is kept with A and B.
#[cfg(unix)]
#[test]
fn a_kept_name_removed_by_hand_never_lets_the_vault_open_without_its_wal() {
    for (fixture, step) in [("damaged", 3u8), ("damaged", 5), ("healthy", 3), ("healthy", 5)] {
        let case = format!("{fixture} vault, stopped after step {step}");
        let (copy, ids) = if fixture == "damaged" { damaged_with_its_newest_entry_in_its_wal() } else { healthy_with_its_newest_entry_in_its_wal() };
        let dir = copy.dir.path();
        crash_recover(dir, &copy.keys, step);
        let name = recorded_name(dir);
        std::fs::remove_file(dir.join(&name)).unwrap();
        assert!(Vault::kept_from_stopped_recover(dir).is_empty(), "{case}: nothing is left over; the next open puts the WAL back");
        let opened = Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ());
        assert!(moved_aside(dir).is_empty(), "{case}: {:?}", moved_aside(dir));
        assert!(Vault::kept_from_stopped_recover(dir).is_empty(), "{case}");
        if fixture == "damaged" {
            assert_eq!(opened, Err(VaultError::Damaged), "{case}: as before the Recover, damage and all");
            let recovery = Vault::recover_damaged_file(dir, &copy.keys).unwrap();
            assert_eq!((recovery.recovered, recovery.found), (2, 2), "{case}");
        } else {
            assert_eq!(opened, Ok(()), "{case}");
        }
        assert_eq!(passwords_held(dir, &copy.keys, &ids), 2, "{case}: A and B");
        let mut vault = Vault::open(dir, Box::new(copy.keys.clone())).unwrap();
        vault.unlock(Moment::now()).unwrap();
        vault.add(&tagged("https://three.example", "fixture", PASSWORD_C, PROFILE_A)).unwrap();
        drop(vault);
        assert!(holds_a_b_and_c(dir, &copy.keys, &ids), "{case}: C with A and B");
    }
}

/// R9-1 at Try Again: an undo stopped because the kept WAL couldn't move
/// (an immutable flag), so the page named the kept name and its WAL, and the
/// user removed the kept name. Open, Recover and Reset still keep the vault
/// closed, naming the WAL and not the removed name, and change nothing.
/// Once the WAL can move, Try Again puts it back: the vault is as it was
/// before the Recover, and Recover then keeps A and B.
#[cfg(target_os = "macos")]
#[test]
fn a_kept_name_removed_by_hand_while_the_undo_is_stuck_keeps_the_vault_closed() {
    let (copy, ids) = damaged_with_its_newest_entry_in_its_wal();
    let dir = copy.dir.path();
    crash_recover(dir, &copy.keys, 3);
    let name = recorded_name(dir);
    let wal = format!("{name}-wal");
    let fixed = Immutable::set(&dir.join(&wal));
    let names = |result: Result<(), VaultError>| match result { Err(VaultError::UndoIncomplete(kept)) => Some(kept), _ => None };
    assert!(names(Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ())).is_some_and(|kept| kept.contains(&name) && kept.contains(&wal)));
    std::fs::remove_file(dir.join(&name)).unwrap();
    let before = files_and_bytes(dir);
    for attempt in ["open", "recover", "reset", "open again"] {
        let result = match attempt {
            "recover" => Vault::recover_damaged_file(dir, &copy.keys).map(|_| ()),
            "reset" => Vault::replace_damaged_file(dir).map(|_| ()),
            _ => Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()),
        };
        let kept = names(result.clone());
        assert!(kept.as_ref().is_some_and(|kept| kept.contains(&wal) && !kept.contains(&name)), "{attempt}: {result:?}");
        assert_eq!(files_and_bytes(dir), before, "{attempt}: every file as it was, and none made");
    }
    drop(fixed);
    assert_eq!(Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()), Err(VaultError::Damaged), "Try Again puts the WAL back");
    assert!(moved_aside(dir).is_empty(), "{:?}", moved_aside(dir));
    let recovery = Vault::recover_damaged_file(dir, &copy.keys).unwrap();
    assert_eq!((recovery.recovered, recovery.found), (2, 2));
    assert_eq!(passwords_held(dir, &copy.keys, &ids), 2);
}

// -- round 10: Recover's working file is never left unnamed (R9-3) --

/// Every file in `dir` that may hold a copy of the vault's data and isn't the
/// vault file's own: anything but vault.sqlite, its WAL, journal and shared
/// memory, and a Recover's record, which holds only file identities.
fn copies_beside_the_vault(dir: &Path) -> Vec<String> {
    let own = [FILE_NAME.to_owned(), format!("{FILE_NAME}-wal"), format!("{FILE_NAME}-journal"), format!("{FILE_NAME}-shm")];
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|file| file.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !own.contains(name) && !name.ends_with("-swap") && !name.ends_with("-swap-next")).collect();
    names.sort();
    names
}

/// R9-3, ported from the ninth recheck's working-file probe: a Recover that
/// stops after any step of its swap leaves no copy of the vault's data that
/// nothing names. Before its last rename, the open that undoes it deletes
/// the working file vault-recovery.sqlite, which holds every recovered entry
/// sealed under the vault key, so a password deleted afterwards has no copy
/// left there; after it, the open names the kept files. A working file a
/// stop left while it was being built, with no record, goes the same way.
#[cfg(unix)]
#[test]
fn a_stopped_recover_leaves_no_copy_that_nothing_names() {
    for step in 0..=6u8 {
        let (copy, ids) = healthy_with_its_newest_entry_in_its_wal();
        let dir = copy.dir.path();
        if step == 0 {
            std::fs::write(dir.join(RECOVERY_FILE), b"fixture: a working file a stop left while it was built").unwrap();
        } else {
            crash_recover(dir, &copy.keys, step);
        }
        let mut vault = Vault::open(dir, Box::new(copy.keys.clone())).unwrap();
        let named = Vault::kept_from_stopped_recover(dir);
        assert_eq!(copies_beside_the_vault(dir), named, "step {step}: every copy is named");
        assert!(!dir.join(RECOVERY_FILE).exists(), "step {step}: the working file goes");
        vault.unlock(Moment::now()).unwrap();
        vault.delete(&ids[0]).unwrap();
        drop(vault);
        let held: Vec<String> = copies_beside_the_vault(dir).into_iter().filter(|name| std::fs::read(dir.join(name)).is_ok_and(|bytes| bytes.windows(ids[0].len()).any(|w| w == ids[0].as_bytes()))).collect();
        assert!(held.iter().all(|name| named.contains(name)), "step {step}: a deleted entry's id is only in named copies: {held:?}");
    }
}

/// R9-3 with the undo stuck (an immutable kept WAL): the working file is
/// named with the kept files while open, Recover and Reset keep the vault
/// closed, and goes once the undo has put everything back. Reset, which
/// undoes first, then leaves no working file either.
#[cfg(target_os = "macos")]
#[test]
fn a_stuck_undo_names_the_working_file_and_reset_leaves_none() {
    let (copy, _) = damaged_with_its_newest_entry_in_its_wal();
    let dir = copy.dir.path();
    crash_recover(dir, &copy.keys, 3);
    let name = recorded_name(dir);
    let kept = vec![name.clone(), format!("{name}-wal"), RECOVERY_FILE.to_owned()];
    let fixed = Immutable::set(&dir.join(format!("{name}-wal")));
    for attempt in ["open", "recover", "reset"] {
        let result = match attempt {
            "recover" => Vault::recover_damaged_file(dir, &copy.keys).map(|_| ()),
            "reset" => Vault::replace_damaged_file(dir).map(|_| ()),
            _ => Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()),
        };
        assert_eq!(result, Err(VaultError::UndoIncomplete(kept.clone())), "{attempt}");
        assert!(dir.join(RECOVERY_FILE).exists(), "{attempt}: kept while it is named");
    }
    drop(fixed);
    let moved = Vault::replace_damaged_file(dir).unwrap();
    assert!(!dir.join(RECOVERY_FILE).exists(), "Reset leaves no working file");
    assert_eq!(copies_beside_the_vault(dir), [moved.clone(), format!("{moved}-wal")], "only Reset's own kept file, which it names");
}

// -- round 10: a stop with no record says what happened (R9-2) --

/// A stop as a build before round 9 left one, as the ninth recheck's
/// no-record probe made it: the vault file linked under a kept name, its WAL
/// moved there and its shared memory dropped, and no record of either.
fn stop_without_a_record(dir: &Path, name: &str) {
    std::fs::hard_link(dir.join(FILE_NAME), dir.join(name)).unwrap();
    std::fs::rename(dir.join(format!("{FILE_NAME}-wal")), dir.join(format!("{name}-wal"))).unwrap();
    let _ = std::fs::remove_file(dir.join(format!("{FILE_NAME}-shm")));
}

/// R9-2, ported from the ninth recheck's no-record probe: a Recover stopped
/// by a build before round 9 left the vault file under a kept name and its
/// WAL, which holds the newest entry, beside it, with no record. That
/// build's undo opened the vault even when it couldn't put a WAL back, so
/// nothing tells whether the vault file was written since, and the WAL never
/// goes back. Open, Recover and Reset leave every file as it is and say what
/// happened: the named files, with Recover's working file if one is left,
/// hold passwords the vault file may not have; and they don't offer Try
/// Again, which can't change that. The kept pair does hold the newest
/// entry, and the vault file alone doesn't, which is why the README's old
/// step of moving the WAL out lost it.
#[cfg(unix)]
#[test]
fn ported_r9_2_a_stop_with_no_record_keeps_every_file_and_says_they_hold_passwords() {
    const NAME: &str = "vault-damaged-20261029T000000Z.sqlite";
    for working in [false, true] {
        let (copy, ids) = healthy_with_its_newest_entry_in_its_wal();
        let dir = copy.dir.path();
        stop_without_a_record(dir, NAME);
        if working { std::fs::write(dir.join(RECOVERY_FILE), b"fixture: a working file a stop left").unwrap(); }
        let before = files_and_bytes(dir);
        let mut named = vec![NAME.to_owned(), format!("{NAME}-wal")];
        if working { named.push(RECOVERY_FILE.to_owned()); }
        for attempt in ["open", "recover", "reset"] {
            let error = match attempt {
                "recover" => Vault::recover_damaged_file(dir, &copy.keys).map(|_| ()),
                "reset" => Vault::replace_damaged_file(dir).map(|_| ()),
                _ => Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()),
            }.expect_err(attempt);
            let text = error.to_string();
            assert!(text.contains(&named.join(", ")), "{attempt} names {named:?}: {text}");
            for said in ["left no record of the vault file as it was", "passwords the vault file may not have", "Don't move or delete them"] {
                assert!(text.contains(said), "{attempt} says {said:?}: {text}");
            }
            assert!(!text.contains("Try Again"), "{attempt}: Try Again can't change this: {text}");
            assert!(!error.needs_reset(), "{attempt}");
            assert_eq!(files_and_bytes(dir), before, "{attempt}: every file as it was, and none made");
        }
        for (suffixes, both, what) in [(&[""][..], false, "the vault file alone lacks the newest entry"), (&["", "-wal"][..], true, "the kept pair holds both")] {
            let elsewhere = Fixture { dir: tempfile::tempdir().unwrap(), keys: copy.keys.clone() };
            for suffix in suffixes { std::fs::copy(dir.join(format!("{NAME}{suffix}")), elsewhere.dir.path().join(format!("{FILE_NAME}{suffix}"))).unwrap(); }
            assert_eq!(passwords_held(elsewhere.dir.path(), &copy.keys, &ids) == 2, both, "{what}");
        }
    }
}

// -- round 12: stops that file identity can't place, and kept files gone (R10-1, R10-2) --

/// Every file in `dir` written again under its name, as a copy or a restore
/// file by file leaves them: the same bytes in new files, so no two names
/// share one and none keeps its device and inode.
fn copied_file_by_file(dir: &Path) {
    for (name, bytes) in files_and_bytes(dir) {
        std::fs::remove_file(dir.join(&name)).unwrap();
        std::fs::write(dir.join(&name), bytes).unwrap();
    }
}

/// R10-1, ported from the tenth recheck's copied-folder probe: a Recover
/// stopped after its link (step 2), its WAL's move (3), its shared memory's
/// removal (5) or its final rename (6), then the data folder copied file by
/// file. Files are judged by their bytes, so the copy places as the folder
/// did: before the rename, the next open puts everything back, nothing is
/// left over and the next Recover keeps A and B; after it, the vault opens
/// as recovered with A and B, and the kept files are named.
#[cfg(unix)]
#[test]
fn a_stop_in_a_folder_copied_file_by_file_places_as_it_did_in_place() {
    for (fixture, step) in [("damaged", 2u8), ("damaged", 3), ("damaged", 5), ("damaged", 6), ("healthy", 3)] {
        let case = format!("{fixture} vault, stopped after step {step}, folder copied");
        let (copy, ids) = if fixture == "damaged" { damaged_with_its_newest_entry_in_its_wal() } else { healthy_with_its_newest_entry_in_its_wal() };
        let dir = copy.dir.path();
        crash_recover(dir, &copy.keys, step);
        let name = recorded_name(dir);
        copied_file_by_file(dir);
        let opened = Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ());
        if step == 6 {
            assert_eq!(opened, Ok(()), "{case}: finished");
            assert_eq!(Vault::kept_from_stopped_recover(dir), [name.clone(), format!("{name}-wal")], "{case}");
        } else {
            assert!(moved_aside(dir).is_empty(), "{case}: nothing left over: {:?}", moved_aside(dir));
            assert!(!dir.join(RECOVERY_FILE).exists(), "{case}: the working file goes");
            if fixture == "damaged" {
                assert_eq!(opened, Err(VaultError::Damaged), "{case}: as before the Recover");
                let recovery = Vault::recover_damaged_file(dir, &copy.keys).unwrap();
                assert_eq!((recovery.recovered, recovery.found), (2, 2), "{case}");
            } else {
                assert_eq!(opened, Ok(()), "{case}");
            }
        }
        assert_eq!(passwords_held(dir, &copy.keys, &ids), 2, "{case}: A and B");
    }
}

/// Round 9's rule, kept by round 12's comparison by bytes: a kept WAL never
/// goes back beside a vault file whose bytes changed, by as little as one
/// byte, in place or in a folder copied file by file. The vault stays
/// closed, naming every kept file, and no file changes.
#[cfg(unix)]
#[test]
fn a_kept_wal_never_goes_back_beside_a_vault_file_one_byte_different() {
    for copied in [false, true] {
        let case = if copied { "folder copied" } else { "in place" };
        let (copy, _) = healthy_with_its_newest_entry_in_its_wal();
        let dir = copy.dir.path();
        crash_recover(dir, &copy.keys, 3);
        let name = recorded_name(dir);
        if copied { copied_file_by_file(dir); }
        let mut bytes = std::fs::read(dir.join(FILE_NAME)).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        std::fs::write(dir.join(FILE_NAME), &bytes).unwrap();
        let before = files_and_bytes(dir);
        let kept = vec![name.clone(), format!("{name}-wal"), RECOVERY_FILE.to_owned()];
        for attempt in ["open", "Try Again"] {
            assert_eq!(Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()), Err(VaultError::UndoIncomplete(kept.clone())), "{case}, {attempt}");
            assert_eq!(files_and_bytes(dir), before, "{case}, {attempt}: no file changes, and the WAL stays kept");
        }
    }
}

/// What placing a stop costs. While a stop's files exist, each open reads
/// whole and hashes the files its record names: the vault file, and the
/// kept name and each kept WAL or journal it compares. Printed for one 50 MB
/// file; run by name.
#[test]
#[ignore = "a measurement: run by name"]
fn hashing_a_50_mb_vault_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    std::fs::write(&path, (0..50u32 << 20).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect::<Vec<u8>>()).unwrap();
    let mut times = Vec::new();
    for _ in 0..3 {
        let started = std::time::Instant::now();
        let identity = Identity::of(&path).unwrap();
        times.push(started.elapsed());
        assert_eq!(identity.len, 50 << 20);
    }
    eprintln!("hashing a 50 MB vault file: {times:?}");
}

/// The record beside `name`, as JSON, if one reads.
fn record_json(dir: &Path, name: &str) -> Option<serde_json::Value> {
    std::fs::read(dir.join(format!("{name}-swap"))).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// The record beside `name` as round 9 wrote it: no new file, and never finished.
fn as_round_9_record(dir: &Path, name: &str) {
    let mut record = record_json(dir, name).expect("a record");
    let fields = record.as_object_mut().unwrap();
    fields.remove("recovered");
    fields.remove("finished");
    std::fs::write(dir.join(format!("{name}-swap")), serde_json::to_vec(&record).unwrap()).unwrap();
}

/// Each named file's bytes, for a check that none changed.
fn bytes_of(dir: &Path, names: &[String]) -> Vec<Vec<u8>> {
    names.iter().map(|name| std::fs::read(dir.join(name)).unwrap()).collect()
}

/// Save C in the vault in `dir`, which must open and unlock.
fn save_c(dir: &Path, keys: &MemoryVaultKeyStore) {
    let mut vault = Vault::open(dir, Box::new(keys.clone())).unwrap();
    vault.unlock(Moment::now()).unwrap();
    vault.add(&tagged("https://three.example", "fixture", PASSWORD_C, PROFILE_A)).unwrap();
}

/// The passwords the kept file and WAL `name` hold, copied elsewhere as a
/// vault file and its WAL: read as they are, or recovered if damaged.
fn held_by_kept_pair(dir: &Path, name: &str, keys: &MemoryVaultKeyStore, ids: &[String; 2]) -> usize {
    let elsewhere = tempfile::tempdir().unwrap();
    for suffix in ["", "-wal"] { std::fs::copy(dir.join(format!("{name}{suffix}")), elsewhere.path().join(format!("{FILE_NAME}{suffix}"))).unwrap(); }
    match Vault::open(elsewhere.path(), Box::new(keys.clone())).map(|_| ()) {
        Err(VaultError::Damaged) => Vault::recover_damaged_file(elsewhere.path(), keys).map_or(0, |recovery| recovery.recovered),
        _ => passwords_held(elsewhere.path(), keys, ids),
    }
}

/// R10-1, from the tenth recheck's probes: stops nothing can place keep the
/// vault closed, but no longer for good. A round 9 record after its final
/// rename, a vault file one byte different since the stop, and a stop with
/// no record from a build before round 9, in place or in a folder copied
/// file by file (round 10 opened that copy without its WAL, silently).
/// Each on a damaged vault whose damage and newest entry, B, are only in
/// its WAL. Open Without Them sets each aside: the vault then opens as its
/// file is (A, or A and B where that file is the recovered one), at once
/// and after a relaunch; the stop's files stay, unchanged, named as
/// set aside and never as left by a finished Recover; a second name of the
/// vault file becomes a copy of its own, so a later save, C, doesn't reach
/// it; Recover's working file goes; and the kept pair still holds A and B.
#[cfg(unix)]
#[test]
fn open_without_them_opens_a_stop_nothing_can_place_and_keeps_its_files_named() {
    const NAME: &str = "vault-damaged-20261029T000000Z.sqlite";
    for case in ["round 9 record after the final rename", "vault file one byte different", "no record", "no record, folder copied"] {
        let damaged = case.starts_with("round 9");
        let (copy, ids) = damaged_with_its_newest_entry_in_its_wal();
        let dir = copy.dir.path();
        let name = if case.starts_with("no record") {
            stop_without_a_record(dir, NAME);
            NAME.to_owned()
        } else {
            crash_recover(dir, &copy.keys, if damaged { 6 } else { 3 });
            recorded_name(dir)
        };
        if damaged { as_round_9_record(dir, &name); }
        if case.ends_with("copied") { copied_file_by_file(dir); }
        if case.starts_with("vault file") {
            // The header's application id, which neither SQLite nor the vault reads.
            let mut bytes = std::fs::read(dir.join(FILE_NAME)).unwrap();
            bytes[68] ^= 1;
            std::fs::write(dir.join(FILE_NAME), &bytes).unwrap();
        }
        let kept = vec![name.clone(), format!("{name}-wal")];
        let error = Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()).expect_err(case);
        assert!(matches!(&error, VaultError::UndoIncomplete(files) | VaultError::UnrecordedStop(files) if files[..2] == kept[..]), "{case}: {error:?}");
        assert_eq!(error.is_final(), case.starts_with("no record"), "{case}: Try Again only where it can help");
        assert!(error.can_open_without(), "{case}");
        let before = bytes_of(dir, &kept);
        Vault::open_without_stopped_recover(dir).unwrap();
        for when in ["at once", "after a relaunch"] {
            assert!(Vault::open(dir, Box::new(copy.keys.clone())).is_ok(), "{case}: opens {when}");
            assert_eq!(Vault::set_aside_from_stopped_recover(dir), kept, "{case}: named {when}");
            assert!(Vault::kept_from_stopped_recover(dir).is_empty(), "{case}: {when}");
            assert!(!dir.join(RECOVERY_FILE).exists(), "{case}: {when}");
            assert_eq!(bytes_of(dir, &kept), before, "{case}: unchanged {when}");
        }
        assert_eq!(passwords_held(dir, &copy.keys, &ids), if damaged { 2 } else { 1 }, "{case}: the vault file as it is");
        save_c(dir, &copy.keys);
        assert!(Vault::open(dir, Box::new(copy.keys.clone())).is_ok(), "{case}");
        assert_eq!(bytes_of(dir, &kept), before, "{case}: C reaches no kept file");
        if !case.starts_with("vault file") { assert_eq!(held_by_kept_pair(dir, &name, &copy.keys, &ids), 2, "{case}: the kept pair holds A and B"); }
    }
}

/// Open Without Them stopped after each of its steps, as a crash would:
/// the set-aside record written beside the old one (20), renamed over it
/// (21), and a second name of the vault file copied, before the copy
/// replaces it (22). Each leaves the old state, the vault closed as before,
/// or the new one, the vault open with the stop set aside: it never opens
/// while the record still says put back, and the kept files never change.
/// A stop with no record is used, whose kept name is a second name of the
/// vault file, so every step runs.
#[cfg(unix)]
#[test]
fn open_without_them_leaves_the_old_state_or_the_new_one_wherever_it_stops() {
    const NAME: &str = "vault-damaged-20261029T000000Z.sqlite";
    for step in [20u8, 21, 22] {
        let (copy, ids) = healthy_with_its_newest_entry_in_its_wal();
        let dir = copy.dir.path();
        stop_without_a_record(dir, NAME);
        let kept = vec![NAME.to_owned(), format!("{NAME}-wal")];
        let before = bytes_of(dir, &kept);
        super::test_hooks::SWAP_CRASH.set(Some(step));
        let crashed = std::panic::catch_unwind(|| Vault::open_without_stopped_recover(dir));
        super::test_hooks::SWAP_CRASH.set(None);
        assert!(crashed.is_err(), "stopped after step {step}");
        let set_aside = record_json(dir, NAME).is_some_and(|record| record["set_aside"] == true);
        let opened = Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ());
        if step == 20 {
            assert!(matches!(opened, Err(VaultError::UnrecordedStop(_))), "step {step}: the old state: {opened:?}");
            Vault::open_without_stopped_recover(dir).unwrap();
        } else {
            assert!(set_aside, "step {step}: the new state");
            assert_eq!(opened, Ok(()), "step {step}");
        }
        assert!(record_json(dir, NAME).is_some_and(|record| record["set_aside"] == true), "step {step}: opens only once set aside");
        assert_eq!(bytes_of(dir, &kept), before, "step {step}");
        save_c(dir, &copy.keys);
        assert_eq!(bytes_of(dir, &kept), before, "step {step}: C reaches no kept file");
        assert_eq!(Vault::set_aside_from_stopped_recover(dir), kept, "step {step}");
        assert_eq!(held_by_kept_pair(dir, NAME, &copy.keys, &ids), 2, "step {step}");
    }
}

/// R10-2 (3a), from the tenth recheck's deleted-files probe: a Recover
/// stopped after its WAL's move (3) or its shared memory's removal (5), on
/// a damaged and on a healthy vault, and then its kept WAL, or the WAL and
/// the kept name, deleted in Finder. Recover's working file still holds
/// what the record says, read back, so the next open finishes that Recover:
/// the vault opens as recovered with A and B, the vault file as it was is
/// kept, linked under the kept name again if that went, and named as any
/// finished stop's file. A stop during that finish (after the link and
/// moves, 11 and 12, or after the rename, 13) ends the same at the next open.
#[cfg(unix)]
#[test]
fn a_stop_whose_kept_wal_is_gone_finishes_from_its_working_file() {
    for (fixture, step, deleted, stop) in [("damaged", 3u8, "the WAL", None), ("damaged", 5, "both", None), ("healthy", 3, "both", None), ("healthy", 5, "the WAL", None),
                                           ("healthy", 3, "both", Some(11u8)), ("healthy", 3, "both", Some(12)), ("healthy", 3, "both", Some(13))] {
        let case = format!("{fixture} vault, stopped after step {step}, {deleted} deleted, finish stopped after {stop:?}");
        let (copy, ids) = if fixture == "damaged" { damaged_with_its_newest_entry_in_its_wal() } else { healthy_with_its_newest_entry_in_its_wal() };
        let dir = copy.dir.path();
        let original = std::fs::read(dir.join(FILE_NAME)).unwrap();
        crash_recover(dir, &copy.keys, step);
        let name = recorded_name(dir);
        std::fs::remove_file(dir.join(format!("{name}-wal"))).unwrap();
        if deleted == "both" { std::fs::remove_file(dir.join(&name)).unwrap(); }
        if let Some(stop) = stop {
            super::test_hooks::SWAP_CRASH.set(Some(stop));
            let crashed = std::panic::catch_unwind(|| Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()));
            super::test_hooks::SWAP_CRASH.set(None);
            assert!(crashed.is_err(), "{case}");
        }
        assert_eq!(Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()), Ok(()), "{case}");
        assert_eq!(passwords_held(dir, &copy.keys, &ids), 2, "{case}: A and B");
        assert_eq!(Vault::kept_from_stopped_recover(dir), [name.clone()], "{case}");
        assert_eq!(std::fs::read(dir.join(&name)).unwrap(), original, "{case}: the vault file as it was");
        assert!(!dir.join(RECOVERY_FILE).exists(), "{case}");
        assert!(record_json(dir, &name).is_some_and(|record| record["finished"] == true), "{case}");
    }
}

/// R10-2 (3b): the same stop on a damaged vault whose damage and newest
/// entry, B, are only in its WAL, its kept WAL deleted, and Recover's
/// working file deleted too or one byte different, so nothing can finish
/// it. The vault stays closed and says the files are gone, naming them, and
/// no file changes. Put back, as from the Trash, Try Again undoes the stop,
/// leaving nothing behind, and Recover gets A and B; or Open Without Them
/// opens the vault file as it is, with A only, leaving nothing behind. A
/// stop that had nothing to lose still goes silently: one that finished,
/// or one whose WAL never moved.
#[cfg(unix)]
#[test]
fn a_stop_whose_kept_wal_is_gone_says_so_until_it_is_back_or_the_user_opens_without_it() {
    for (working, then) in [("deleted", "put back"), ("one byte different", "put back"), ("deleted", "open without"), ("one byte different", "open without")] {
        let case = format!("working file {working}, then {then}");
        let (copy, ids) = damaged_with_its_newest_entry_in_its_wal();
        let dir = copy.dir.path();
        crash_recover(dir, &copy.keys, 3);
        let name = recorded_name(dir);
        let wal = std::fs::read(dir.join(format!("{name}-wal"))).unwrap();
        std::fs::remove_file(dir.join(format!("{name}-wal"))).unwrap();
        if working == "deleted" {
            std::fs::remove_file(dir.join(RECOVERY_FILE)).unwrap();
        } else {
            let mut bytes = std::fs::read(dir.join(RECOVERY_FILE)).unwrap();
            bytes[100] ^= 1;
            std::fs::write(dir.join(RECOVERY_FILE), &bytes).unwrap();
        }
        let before = files_and_bytes(dir);
        for attempt in ["open", "Try Again"] {
            let error = Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()).expect_err(&case);
            assert_eq!(error, VaultError::KeptFilesGone(vec![format!("{name}-wal")]), "{case}, {attempt}");
            assert!(!error.is_final() && !error.needs_reset() && error.can_open_without(), "{case}");
            assert_eq!(files_and_bytes(dir), before, "{case}, {attempt}: no file changes");
        }
        if then == "put back" {
            std::fs::write(dir.join(format!("{name}-wal")), &wal).unwrap();
            assert_eq!(Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()), Err(VaultError::Damaged), "{case}: as before the Recover");
        } else {
            Vault::open_without_stopped_recover(dir).unwrap();
            assert_eq!(Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()), Ok(()), "{case}");
            assert_eq!(passwords_held(dir, &copy.keys, &ids), 1, "{case}: A, as the user was told");
            assert!(Vault::set_aside_from_stopped_recover(dir).is_empty(), "{case}");
        }
        assert!(moved_aside(dir).is_empty() && !dir.join(RECOVERY_FILE).exists(), "{case}: nothing left over: {:?}", moved_aside(dir));
        if then == "put back" {
            let recovery = Vault::recover_damaged_file(dir, &copy.keys).unwrap();
            assert_eq!((recovery.recovered, recovery.found), (2, 2), "{case}");
        }
    }
    for (step, what) in [(6u8, "finished"), (2, "its WAL never moved")] {
        let (copy, ids) = healthy_with_its_newest_entry_in_its_wal();
        let dir = copy.dir.path();
        crash_recover(dir, &copy.keys, step);
        let name = recorded_name(dir);
        for kept in kept_files(dir, &name) { std::fs::remove_file(dir.join(kept)).unwrap(); }
        assert_eq!(Vault::open(dir, Box::new(copy.keys.clone())).map(|_| ()), Ok(()), "{what}: opens with nothing to say");
        assert_eq!(passwords_held(dir, &copy.keys, &ids), 2, "{what}: A and B");
        assert!(moved_aside(dir).is_empty(), "{what}: {:?}", moved_aside(dir));
    }
}
