//! What this device does when its account's sync data is deleted from the
//! server, by this device or another: keep the library exactly as it is, stop
//! syncing, and upload it again only when the user turns sync on.

use serde::{Deserialize, Serialize};

use super::e2ee::LibraryEncryption;
use crate::library::{Library, SYNC_CURSOR_KEY};

/// The sync epoch the stored cursor belongs to (`PullResponse::sync_epoch`).
pub const SYNC_EPOCH_KEY: &str = "sync_epoch";
/// Sync is off because the account's sync data was deleted.
pub(crate) const SYNC_OFF_KEY: &str = "sync_off";
/// This library was end-to-end encrypted when its account's sync data was
/// deleted, so no plaintext row uploads to any account until encryption is
/// on for it, or the user lets the library go unencrypted to it. The hold
/// belongs to the library on this device, not to an account: `kv` never
/// syncs, and it stays through sign-out and account changes.
pub(crate) const ENCRYPT_FIRST_KEY: &str = "sync_encrypt_first";
/// A delete of this account's sync data was sent from this device and isn't
/// settled yet: written before the request goes, so after a crash, an
/// unconfirmed readback or a refused upload this device knows the new epoch
/// is its own doing.
pub(crate) const DELETING_KEY: &str = "sync_deleting";

/// Sync stopped on this device because the account's sync data was deleted,
/// or because this Mac restored its library from a backup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncOff {
    pub account: String,
    /// This device deleted it, not another.
    pub here: bool,
    /// The library was encrypted for this account.
    pub encrypted: bool,
    /// This Mac restored its library from a backup; the server copy is intact.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restored: bool,
    /// The stored record couldn't be read, so sync stays off without
    /// knowing why it went off.
    #[serde(skip)]
    pub unreadable: bool,
}

impl SyncOff {
    /// Whether sync is off for this account here. A record that can't be
    /// read or parsed keeps sync off: its cursor and epoch went with it, so
    /// resuming would start afresh and upload only what changed while off.
    pub fn read(lib: &Library, account: &str) -> Option<SyncOff> {
        let unreadable = || SyncOff { account: account.into(), here: false, encrypted: false, restored: false, unreadable: true };
        match lib.store.kv_get(SYNC_OFF_KEY) {
            Ok(None) => None,
            Ok(Some(text)) => match serde_json::from_str::<SyncOff>(&text) {
                Ok(off) => (off.account == account).then_some(off),
                Err(_) => Some(unreadable()),
            },
            Err(_) => Some(unreadable()),
        }
    }

    /// Stop syncing, in one transaction: nothing of the deleted data waits to
    /// upload (Turn Sync On queues the whole library), the cursor and epoch
    /// go, and so do this account's encryption record, notice and marker row,
    /// because the key and recovery codes now protect nothing. A library that
    /// was encrypted holds its uploads for encryption, for every account. The
    /// delete this device sent, if any, is settled. The library itself is
    /// unchanged. Remove the key with `LibraryEncryption::forget_key` after
    /// this commits.
    pub fn record(lib: &mut Library, account: &str, here: bool) -> Result<SyncOff, String> {
        let encrypted = matches!(LibraryEncryption::owner(lib), Ok(Some(owner)) if owner == account);
        let off = SyncOff { account: account.into(), here, encrypted, restored: false, unreadable: false };
        let text = serde_json::to_string(&off).map_err(|_| "Couldn't encode the sync state.".to_string())?;
        lib.transaction(|lib| {
            lib.store.mark_all_clean()?;
            lib.store.retry_sync_rejections()?;
            lib.store.kv_delete(SYNC_CURSOR_KEY)?;
            lib.store.kv_delete(SYNC_EPOCH_KEY)?;
            lib.store.kv_delete(DELETING_KEY)?;
            lib.store.kv_set(SYNC_OFF_KEY, &text)?;
            if encrypted { hold_for_encryption(lib, account)?; }
            LibraryEncryption::retire(account, lib)?;
            Ok(())
        }).map_err(|error| format!("This Mac couldn't record that sync stopped ({error}); check disk access. Sync stays off until it can."))?;
        Ok(off)
    }

    /// Turn Sync On: upload this library from a fresh start, as a first
    /// sign-in does, merging with anything another device uploaded since.
    pub fn turn_on(lib: &mut Library) -> Result<(), String> {
        lib.transaction(|lib| {
            lib.store.kv_set(SYNC_CURSOR_KEY, "0")?;
            lib.store.kv_delete(SYNC_EPOCH_KEY)?;
            lib.store.mark_all_dirty()?;
            lib.store.retry_sync_rejections()?;
            lib.store.kv_delete(SYNC_OFF_KEY)?;
            Ok(())
        }).map_err(|error| format!("Couldn't turn sync on ({error}). Nothing changed; check disk access, then try again."))
    }

    /// Sign Out ends what this record was about, and any delete in progress.
    pub fn clear(lib: &Library) -> Result<(), String> {
        lib.store.kv_delete(DELETING_KEY).and_then(|()| lib.store.kv_delete(SYNC_OFF_KEY)).map_err(|_| "Couldn't clear the sync state; check disk access.".to_string())
    }
}

/// The hold's record: the account the library was encrypted for, and the
/// accounts the user chose to upload it to without encryption.
#[derive(Serialize, Deserialize)]
struct EncryptFirst {
    account: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unencrypted: Vec<String>,
}

impl EncryptFirst {
    /// A hold from before this version is the account's id alone, and holds
    /// for every account, as does one that can't be parsed.
    fn parse(text: &str) -> EncryptFirst {
        serde_json::from_str(text).unwrap_or_else(|_| EncryptFirst { account: text.into(), unencrypted: Vec::new() })
    }
}

/// The hold for a library that was encrypted for `account`, with no account
/// let through yet. A restore writes it too.
pub(crate) fn hold_text(account: &str) -> String {
    serde_json::json!({ "account": account }).to_string()
}

/// The account a hold's library was encrypted for.
pub(crate) fn hold_account(text: &str) -> String {
    EncryptFirst::parse(text).account
}

/// Whether a hold keeps this account's uploads waiting for encryption.
pub(crate) fn holds(text: &str, account: &str) -> bool {
    !EncryptFirst::parse(text).unencrypted.iter().any(|chosen| chosen == account)
}

/// Hold this library's uploads for encryption, for every account, in the
/// caller's transaction.
fn hold_for_encryption(lib: &Library, account: &str) -> crate::library::Result<()> {
    lib.store.kv_set(ENCRYPT_FIRST_KEY, &hold_text(account))?;
    Ok(())
}

/// Uploads to this account wait until encryption is on for it; `None` if
/// that can't be read just now.
pub fn encrypt_first(lib: &Library, account: &str) -> Option<bool> {
    let stored = lib.store.kv_get(ENCRYPT_FIRST_KEY).ok()?;
    Some(stored.is_some_and(|text| holds(&text, account)))
}

/// Encryption is on for the account this library syncs with, so it can't
/// reach another account at all: the hold ends.
pub fn release_encrypt_first(lib: &Library) -> Result<(), String> {
    lib.store.kv_delete(ENCRYPT_FIRST_KEY).map_err(|_| "Couldn't save that choice; check disk access, then try again.".to_string())
}

/// The user chose to upload this library to this account without
/// encryption. Every other account stays held.
pub fn upload_unencrypted(lib: &Library, account: &str) -> Result<(), String> {
    const FAILED: &str = "Couldn't save that choice; check disk access, then try again.";
    let Some(text) = lib.store.kv_get(ENCRYPT_FIRST_KEY).map_err(|_| FAILED.to_string())? else { return Ok(()) };
    let mut hold = EncryptFirst::parse(&text);
    if !hold.unencrypted.iter().any(|chosen| chosen == account) { hold.unencrypted.push(account.into()); }
    let text = serde_json::to_string(&hold).map_err(|_| FAILED.to_string())?;
    lib.store.kv_set(ENCRYPT_FIRST_KEY, &text).map_err(|_| FAILED.to_string())
}

/// This account's sync data is in `epoch`: the epoch the first pull after
/// signing in or turning sync on named (`fresh`), or the one a page this
/// device applies is in. An encryption record for the account from an
/// earlier epoch protected data deleted since, while this device was signed
/// out or its session had ended. Retire it as `SyncOff::record` does and
/// hold uploads for encryption, in one transaction, so nothing is sealed
/// under the retired key into this epoch and its recovery code opens
/// nothing. A record from before records named their epoch counts as epoch
/// 0 at a fresh start, and is left alone by a page. True if a record was
/// retired; remove the key with `LibraryEncryption::forget_key` after this
/// commits.
pub fn retire_outdated(lib: &mut Library, account: &str, epoch: u64, fresh: bool) -> Result<bool, String> {
    let outdated = match LibraryEncryption::record_epoch(lib, account) {
        Some(Some(held)) => held < epoch,
        Some(None) => fresh && epoch > 0,
        None => false,
    };
    if !outdated { return Ok(false); }
    lib.transaction(|lib| {
        hold_for_encryption(lib, account)?;
        LibraryEncryption::retire(account, lib)?;
        Ok(())
    }).map_err(|error| format!("This Mac couldn't retire the encryption key for this account's deleted sync data ({error}); check disk access. Sync stays off until it can."))?;
    Ok(true)
}

/// Note that this device is deleting this account's sync data. It commits
/// before the request goes.
pub fn begin_delete(lib: &Library, account: &str) -> Result<(), String> {
    lib.store.kv_set(DELETING_KEY, account)
        .map_err(|_| "Couldn't prepare the delete, so nothing was deleted and nothing changed on this Mac. Check disk access, then try again.".to_string())
}

/// This device sent a delete of this account's sync data that isn't settled.
pub fn deleting_here(lib: &Library, account: &str) -> bool {
    lib.store.kv_get(DELETING_KEY).ok().flatten().as_deref() == Some(account)
}

/// The delete this device sent didn't happen: the server refused it, or a
/// pull since found the data still there.
pub fn delete_settled(lib: &Library) -> Result<(), String> {
    lib.store.kv_delete(DELETING_KEY).map_err(|_| "Couldn't save the sync state; check disk access.".to_string())
}

/// The epoch the stored cursor belongs to, if one was stored.
pub fn stored_epoch(lib: &Library) -> Option<u64> {
    lib.store.kv_get(SYNC_EPOCH_KEY).ok().flatten().and_then(|text| text.parse().ok())
}

/// The epoch to resume `cursor` in. A cursor stored before this version
/// belongs to epoch 0, the only one there was; from cursor 0 the first pull
/// names it.
pub fn resume_epoch(lib: &Library, cursor: u64) -> Option<u64> {
    stored_epoch(lib).or((cursor > 0).then_some(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn open(path: &std::path::Path) -> Library {
        let mut lib = Library::open(Store::open(path).unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        lib
    }

    fn snapshot(lib: &Library) -> (usize, usize, Vec<(String, String)>) {
        let mut items: Vec<_> = lib.items.values().map(|item| (item.url.clone(), item.title.clone())).collect();
        items.sort();
        (lib.spaces.len(), lib.profiles.len(), items)
    }

    #[test]
    fn a_deleted_accounts_library_stays_here_and_waits_for_turn_sync_on() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let mut lib = open(&path);
        let space = lib.spaces.keys().next().unwrap().clone();
        lib.add_pinned(&space, None, "https://example.test/pinned", "Pinned fixture").unwrap();
        lib.store.kv_set(SYNC_CURSOR_KEY, "41").unwrap();
        lib.store.kv_set(SYNC_EPOCH_KEY, "0").unwrap();
        let before = snapshot(&lib);
        assert!(lib.store.dirty_count().unwrap() > 0);

        let off = SyncOff::record(&mut lib, "account-a", true).unwrap();
        assert_eq!(off, SyncOff { account: "account-a".into(), here: true, encrypted: false, restored: false, unreadable: false });
        drop(lib);
        let mut lib = open(&path);
        assert_eq!(snapshot(&lib), before, "the library changed");
        assert_eq!(lib.store.dirty_count().unwrap(), 0, "the deleted data still waits to upload");
        assert_eq!(lib.store.kv_get(SYNC_CURSOR_KEY).unwrap(), None);
        assert_eq!(stored_epoch(&lib), None);
        assert_eq!(SyncOff::read(&lib, "account-a"), Some(off));
        assert_eq!(SyncOff::read(&lib, "account-b"), None, "another account's sync isn't off");
        assert_eq!(encrypt_first(&lib, "account-a"), Some(false));

        // An edit while sync is off waits too, and Turn Sync On queues everything.
        let rows: i64 = rusqlite::Connection::open(&path).unwrap().query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0)).unwrap();
        SyncOff::turn_on(&mut lib).unwrap();
        drop(lib);
        let lib = open(&path);
        assert_eq!(lib.store.dirty_count().unwrap(), rows as u64);
        assert_eq!(lib.store.kv_get(SYNC_CURSOR_KEY).unwrap().as_deref(), Some("0"));
        assert_eq!(stored_epoch(&lib), None, "a fresh start takes the epoch its first pull names");
        assert_eq!(SyncOff::read(&lib, "account-a"), None);
        assert_eq!(snapshot(&lib), before);
    }

    #[test]
    fn a_cursor_from_before_epochs_belongs_to_epoch_zero() {
        let dir = tempfile::tempdir().unwrap();
        let lib = open(&dir.path().join("library.sqlite"));
        assert_eq!(resume_epoch(&lib, 0), None, "a fresh start");
        assert_eq!(resume_epoch(&lib, 12), Some(0), "synced before this version, when every account was on epoch 0");
        lib.store.kv_set(SYNC_EPOCH_KEY, "3").unwrap();
        assert_eq!(resume_epoch(&lib, 12), Some(3));
    }

    #[test]
    fn a_failed_write_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let mut lib = open(&path);
        lib.store.kv_set(SYNC_CURSOR_KEY, "41").unwrap();
        let dirty = lib.store.dirty_count().unwrap();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TRIGGER refuse_off BEFORE INSERT ON kv WHEN NEW.k = 'sync_off' BEGIN SELECT RAISE(FAIL, 'fixture write failure'); END;").unwrap();
        assert!(SyncOff::record(&mut lib, "account-a", true).is_err());
        assert_eq!(lib.store.dirty_count().unwrap(), dirty);
        assert_eq!(lib.store.kv_get(SYNC_CURSOR_KEY).unwrap().as_deref(), Some("41"));
        assert_eq!(SyncOff::read(&lib, "account-a"), None);
    }

    #[test]
    fn the_hold_is_this_librarys_for_every_account_until_released_for_one() {
        let dir = tempfile::tempdir().unwrap();
        let lib = open(&dir.path().join("library.sqlite"));
        assert_eq!(encrypt_first(&lib, "account-a"), Some(false));
        // A hold from before this version names only the account it was encrypted for.
        lib.store.kv_set(ENCRYPT_FIRST_KEY, "account-a").unwrap();
        assert_eq!(encrypt_first(&lib, "account-a"), Some(true));
        assert_eq!(encrypt_first(&lib, "account-b"), Some(true), "another account gets the library unencrypted");
        upload_unencrypted(&lib, "account-b").unwrap();
        assert_eq!(encrypt_first(&lib, "account-b"), Some(false));
        assert_eq!(encrypt_first(&lib, "account-a"), Some(true));
        assert_eq!(encrypt_first(&lib, "account-c"), Some(true), "a choice for one account went to another");
        // A damaged hold still holds.
        lib.store.kv_set(ENCRYPT_FIRST_KEY, "{\"unencrypted\":[\"account-c\"]").unwrap();
        assert_eq!(encrypt_first(&lib, "account-c"), Some(true));
        release_encrypt_first(&lib).unwrap();
        assert_eq!(encrypt_first(&lib, "account-c"), Some(false));
    }

    #[test]
    fn an_unreadable_sync_off_record_keeps_sync_off() {
        let dir = tempfile::tempdir().unwrap();
        let lib = open(&dir.path().join("library.sqlite"));
        lib.store.kv_set(SYNC_OFF_KEY, "{not json").unwrap();
        let off = SyncOff::read(&lib, "account-a").expect("sync came back on");
        assert!(off.unreadable && !off.here && off.account == "account-a");
    }

    #[test]
    fn a_delete_sent_from_here_is_known_until_it_settles() {
        let dir = tempfile::tempdir().unwrap();
        let mut lib = open(&dir.path().join("library.sqlite"));
        assert!(!deleting_here(&lib, "account-a"));
        begin_delete(&lib, "account-a").unwrap();
        assert!(deleting_here(&lib, "account-a") && !deleting_here(&lib, "account-b"));
        SyncOff::record(&mut lib, "account-a", true).unwrap();
        assert!(!deleting_here(&lib, "account-a"), "stopping settles the delete");
        begin_delete(&lib, "account-a").unwrap();
        delete_settled(&lib).unwrap();
        assert!(!deleting_here(&lib, "account-a"));
        begin_delete(&lib, "account-a").unwrap();
        SyncOff::clear(&lib).unwrap();
        assert!(!deleting_here(&lib, "account-a"), "signing out settles it");
    }

    #[test]
    fn a_record_from_an_earlier_epoch_is_retired_and_holds_uploads() {
        let dir = tempfile::tempdir().unwrap();
        let mut lib = open(&dir.path().join("library.sqlite"));
        let record = |lib: &Library, text: &str| lib.store.kv_set("e2ee_library", text).unwrap();
        let held = |lib: &Library| lib.store.kv_get("e2ee_library").unwrap().is_some();
        record(&lib, r#"{"account":"account-a","epoch":1}"#);
        assert_eq!(retire_outdated(&mut lib, "account-a", 1, true), Ok(false));
        assert_eq!(retire_outdated(&mut lib, "account-b", 2, true), Ok(false), "another account's record");
        assert!(held(&lib) && encrypt_first(&lib, "account-a") == Some(false));
        assert_eq!(retire_outdated(&mut lib, "account-a", 2, false), Ok(true));
        assert!(!held(&lib));
        assert_eq!(encrypt_first(&lib, "account-a"), Some(true));
        assert_eq!(encrypt_first(&lib, "account-b"), Some(true));

        // A record from before records named their epoch: epoch 0 at a fresh start, untouched by a page.
        release_encrypt_first(&lib).unwrap();
        record(&lib, r#"{"account":"account-a"}"#);
        assert_eq!(retire_outdated(&mut lib, "account-a", 0, true), Ok(false));
        assert_eq!(retire_outdated(&mut lib, "account-a", 3, false), Ok(false));
        assert!(held(&lib));
        assert_eq!(retire_outdated(&mut lib, "account-a", 1, true), Ok(true));
        assert!(!held(&lib) && encrypt_first(&lib, "account-a") == Some(true));
    }
}
