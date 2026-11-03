//! Library keys stay in the OS keychain, one entry per account, exactly like
//! session credentials. File storage is a separate isolated-QA opt-in
//! (`CROSSBOW_LIBRARY_KEY_FILE=1`) that only debug and test builds honor, and
//! never a fallback after a denied keychain.

use super::e2ee::LibraryKey;
use super::token_store::{PrivateFileError, read_private_file, write_private_file};
use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use zeroize::{Zeroize, Zeroizing};

#[cfg(feature = "keychain")]
const SERVICE: &str = "app.crossbow.e2ee";
const STORAGE_ERROR: &str = "Secure key storage is unavailable or access was declined.";

pub trait KeyStore: Send {
    fn load(&self, account: &str) -> Result<Option<LibraryKey>, String>;
    fn save(&self, account: &str, key: &LibraryKey) -> Result<(), String>;
    /// Removing an absent key succeeds; another account's key is never touched.
    fn clear(&self, account: &str) -> Result<(), String>;
}

/// The production store: OS keychain, or the isolated QA file when opted in.
pub struct OsKeyStore {
    data_dir: PathBuf,
    file: bool,
}

impl OsKeyStore {
    pub fn new(data_dir: &Path) -> OsKeyStore {
        OsKeyStore { data_dir: data_dir.to_path_buf(), file: use_key_file() }
    }
}

fn use_key_file() -> bool {
    key_file_opt_in(std::env::var("CROSSBOW_LIBRARY_KEY_FILE").ok().as_deref())
}

/// Release builds never write a root key to disk, whatever the environment says.
fn key_file_opt_in(value: Option<&str>) -> bool {
    cfg!(any(debug_assertions, test)) && value == Some("1")
}

fn select_backend<T>(file: bool, file_op: impl FnOnce() -> Result<T, String>, keychain_op: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    if file { file_op() } else { keychain_op() }
}

impl KeyStore for OsKeyStore {
    fn load(&self, account: &str) -> Result<Option<LibraryKey>, String> {
        select_backend(self.file, || load_file(&self.data_dir, account), || {
            #[cfg(feature = "keychain")]
            match entry(account)?.get_password() {
                Ok(encoded) => decode(&Zeroizing::new(encoded)).map(Some),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(_) => Err(STORAGE_ERROR.into()),
            }
            #[cfg(not(feature = "keychain"))]
            Err(STORAGE_ERROR.into())
        })
    }

    fn save(&self, account: &str, key: &LibraryKey) -> Result<(), String> {
        select_backend(self.file, || save_file(&self.data_dir, account, key), || {
            #[cfg(feature = "keychain")]
            {
                entry(account)?.set_password(&encode(key)).map_err(|_| STORAGE_ERROR.to_string())
            }
            #[cfg(not(feature = "keychain"))]
            Err(STORAGE_ERROR.into())
        })
    }

    fn clear(&self, account: &str) -> Result<(), String> {
        select_backend(self.file, || clear_file(&self.data_dir, account), || {
            #[cfg(feature = "keychain")]
            match entry(account)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(_) => Err(STORAGE_ERROR.into()),
            }
            #[cfg(not(feature = "keychain"))]
            Err(STORAGE_ERROR.into())
        })
    }
}

#[cfg(feature = "keychain")]
fn entry(account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, account).map_err(|_| STORAGE_ERROR.into())
}

fn encode(key: &LibraryKey) -> Zeroizing<String> {
    Zeroizing::new(BASE64URL_NOPAD.encode(key.as_bytes()))
}

fn decode(encoded: &str) -> Result<LibraryKey, String> {
    let bytes = Zeroizing::new(BASE64URL_NOPAD.decode(encoded.as_bytes()).map_err(|_| "The stored library key is unreadable.".to_string())?);
    LibraryKey::from_slice(&bytes).ok_or_else(|| "The stored library key is unreadable.".into())
}

#[derive(Serialize, Deserialize)]
struct StoredKey {
    account: String,
    key: String,
}

/// One QA file per account, so one account's key never replaces another's.
fn key_file(account: &str) -> String {
    format!("library-{}.key", HEXLOWER.encode(&Sha256::digest(account.as_bytes())[..12]))
}

fn save_file(data_dir: &Path, account: &str, key: &LibraryKey) -> Result<(), String> {
    let mut stored = StoredKey { account: account.into(), key: encode(key).to_string() };
    let data = serde_json::to_vec(&stored).map(Zeroizing::new);
    stored.key.zeroize();
    let data = data.map_err(|_| "Couldn't encode the QA library key.".to_string())?;
    write_private_file(data_dir, &key_file(account), &data).map_err(|_| "Couldn't save the QA library key file.".into())
}

fn load_file(data_dir: &Path, account: &str) -> Result<Option<LibraryKey>, String> {
    let data = match read_private_file(data_dir, &key_file(account)) {
        Ok(Some(data)) => Zeroizing::new(data),
        Ok(None) => return Ok(None),
        Err(PrivateFileError::Unreadable) => return Err("Couldn't read the QA library key file.".into()),
        Err(PrivateFileError::Invalid) => return Err("Invalid QA library key file. Use an isolated profile.".into()),
    };
    let stored: StoredKey = serde_json::from_slice(&data).map_err(|_| "The QA library key file is invalid.".to_string())?;
    let encoded = Zeroizing::new(stored.key);
    // Another account's key is never returned; this account simply has none.
    if stored.account != account {
        return Ok(None);
    }
    decode(&encoded).map(Some)
}

fn clear_file(data_dir: &Path, account: &str) -> Result<(), String> {
    if load_file(data_dir, account)?.is_some() {
        std::fs::remove_file(data_dir.join(key_file(account))).map_err(|_| "Couldn't remove the QA library key file.".to_string())?;
    }
    Ok(())
}

/// Process-local keys for tests and fixtures. Never touches the keychain.
/// Clones share the same keys, so a test can keep a handle on a store it
/// gave away.
#[derive(Clone, Default)]
pub struct MemoryKeyStore {
    inner: Arc<MemoryKeys>,
}

#[derive(Default)]
struct MemoryKeys {
    keys: Mutex<HashMap<String, LibraryKey>>,
    fail_writes: AtomicBool,
    fail_reads: AtomicBool,
}

impl MemoryKeyStore {
    /// Simulate a denied or unavailable store for saves and removals.
    pub fn fail_writes(&self, fail: bool) {
        self.inner.fail_writes.store(fail, Ordering::SeqCst);
    }

    /// Simulate a declined or failed keychain read.
    pub fn fail_reads(&self, fail: bool) {
        self.inner.fail_reads.store(fail, Ordering::SeqCst);
    }

    pub fn contains(&self, account: &str) -> bool {
        self.inner.keys.lock().unwrap().contains_key(account)
    }
}

impl KeyStore for MemoryKeyStore {
    fn load(&self, account: &str) -> Result<Option<LibraryKey>, String> {
        if self.inner.fail_reads.load(Ordering::SeqCst) { return Err(STORAGE_ERROR.into()); }
        Ok(self.inner.keys.lock().unwrap().get(account).and_then(|key| LibraryKey::from_slice(key.as_bytes())))
    }

    fn save(&self, account: &str, key: &LibraryKey) -> Result<(), String> {
        if self.inner.fail_writes.load(Ordering::SeqCst) { return Err(STORAGE_ERROR.into()); }
        let copy = LibraryKey::from_slice(key.as_bytes()).expect("32 bytes");
        self.inner.keys.lock().unwrap().insert(account.into(), copy);
        Ok(())
    }

    fn clear(&self, account: &str) -> Result<(), String> {
        if self.inner.fail_writes.load(Ordering::SeqCst) { return Err(STORAGE_ERROR.into()); }
        self.inner.keys.lock().unwrap().remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_secrets::Hidden;

    #[test]
    fn qa_files_are_private_per_account_and_never_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (LibraryKey::from_slice(&[7; 32]).unwrap(), LibraryKey::from_slice(&[8; 32]).unwrap());
        save_file(dir.path(), "fixture-a", &a).unwrap();
        assert_eq!(Hidden::from(&load_file(dir.path(), "fixture-a").unwrap().unwrap()), Hidden::from(&a));
        assert!(load_file(dir.path(), "fixture-b").unwrap().is_none(), "another account must never receive this key");
        save_file(dir.path(), "fixture-b", &b).unwrap();
        assert_eq!(Hidden::from(&load_file(dir.path(), "fixture-a").unwrap().unwrap()), Hidden::from(&a), "B's key must not replace A's");
        assert_eq!(Hidden::from(&load_file(dir.path(), "fixture-b").unwrap().unwrap()), Hidden::from(&b));
        assert_ne!(key_file("fixture-a"), key_file("fixture-b"));
        assert!(!key_file("fixture-a").contains("fixture"), "file names don't spell out account ids");
        clear_file(dir.path(), "fixture-b").unwrap();
        assert!(load_file(dir.path(), "fixture-a").unwrap().is_some(), "clearing account B must keep account A's key");
        // A file renamed onto another account's slot is still refused.
        std::fs::rename(dir.path().join(key_file("fixture-a")), dir.path().join(key_file("fixture-b"))).unwrap();
        assert!(load_file(dir.path(), "fixture-b").unwrap().is_none());
        std::fs::rename(dir.path().join(key_file("fixture-b")), dir.path().join(key_file("fixture-a"))).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            let path = dir.path().join(key_file("fixture-a"));
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(!text.contains("CB1-"), "the QA file stores the key, not a printable recovery code");
            std::fs::remove_file(&path).unwrap();
            let outside = dir.path().join("unrelated-file");
            std::fs::write(&outside, "preserve me").unwrap();
            symlink(&outside, &path).unwrap();
            assert!(load_file(dir.path(), "fixture-a").is_err());
            save_file(dir.path(), "fixture-a", &a).unwrap();
            assert_eq!(std::fs::read_to_string(outside).unwrap(), "preserve me");
        }
        clear_file(dir.path(), "fixture-a").unwrap();
        assert!(load_file(dir.path(), "fixture-a").unwrap().is_none());
    }

    #[test]
    fn the_key_file_has_its_own_exact_opt_in() {
        // Debug and test builds only; CROSSBOW_TOKEN_FILE no longer applies.
        assert!(key_file_opt_in(Some("1")));
        for value in [None, Some("0"), Some("true"), Some("")] { assert!(!key_file_opt_in(value), "{value:?}"); }
    }

    #[test]
    fn a_denied_keychain_never_falls_back_to_a_file() {
        let result: Result<(), String> = select_backend(false, || panic!("must not write a key file"), || Err(STORAGE_ERROR.into()));
        assert_eq!(result.unwrap_err(), STORAGE_ERROR);
        let dir = tempfile::tempdir().unwrap();
        let store = OsKeyStore { data_dir: dir.path().into(), file: true };
        store.save("fixture-a", &LibraryKey::from_slice(&[1; 32]).unwrap()).unwrap();
        assert!(store.load("fixture-a").unwrap().is_some(), "the QA opt-in uses only its file");
    }

    /// Without the keychain feature and without the opt-in, every operation
    /// fails and nothing is written. Run with `--no-default-features`.
    #[cfg(not(feature = "keychain"))]
    #[test]
    fn without_a_keychain_or_opt_in_keys_are_refused_and_no_file_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let store = OsKeyStore { data_dir: dir.path().into(), file: false };
        let key = LibraryKey::from_slice(&[5; 32]).unwrap();
        assert_eq!(store.save("fixture-a", &key).unwrap_err(), STORAGE_ERROR);
        assert_eq!(store.load("fixture-a").unwrap_err(), STORAGE_ERROR);
        assert_eq!(store.clear("fixture-a").unwrap_err(), STORAGE_ERROR);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn memory_store_is_per_account_shared_by_clones_and_can_simulate_denial() {
        let store = MemoryKeyStore::default();
        let handle = store.clone();
        let key = LibraryKey::from_slice(&[9; 32]).unwrap();
        store.save("fixture-a", &key).unwrap();
        assert!(handle.contains("fixture-a"));
        assert!(store.load("fixture-b").unwrap().is_none());
        handle.fail_writes(true);
        assert!(store.save("fixture-b", &key).is_err());
        assert!(store.clear("fixture-a").is_err());
        assert!(store.contains("fixture-a") && !store.contains("fixture-b"));
        handle.fail_writes(false);
        handle.fail_reads(true);
        assert!(store.load("fixture-a").is_err());
        handle.fail_reads(false);
        assert!(store.load("fixture-a").unwrap().is_some());
        store.clear("fixture-a").unwrap();
        assert!(!store.contains("fixture-a"));
    }

    #[test]
    fn keychain_encoding_round_trips_without_printing_bytes() {
        let key = LibraryKey::from_slice(&[3; 32]).unwrap();
        assert_eq!(Hidden::from(&decode(&encode(&key)).unwrap()), Hidden::from(&key));
        assert!(decode("not-a-key").is_err());
        assert_eq!(format!("{key:?}"), "LibraryKey(..)");
    }
}
