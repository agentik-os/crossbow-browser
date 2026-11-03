//! Vault keys stay in the OS keychain, one entry per vault, under their own
//! service, separate from library keys and session credentials. File storage
//! is an isolated-QA opt-in (`CROSSBOW_VAULT_KEY_FILE=1` with a non-default
//! `CROSSBOW_DATA_DIR`) that only debug and test builds honor, and never a
//! fallback after a denied keychain.

use super::crypto::VaultKey;
use data_encoding::BASE64URL_NOPAD;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

/// The keychain service every vault key is stored under; the account is the vault id.
pub const KEYCHAIN_SERVICE: &str = "app.crossbow.vault";
pub const STORAGE_ERROR: &str = "Secure key storage is unavailable or access was declined, so the vault stays locked. Nothing is stored without it.";

/// Where a vault key is kept, so the page can say so plainly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyLocation {
    /// The OS keychain or credential store.
    SecureStorage,
    /// The QA opt-in: a plain file beside the vault.
    QaFile,
}

pub trait VaultKeyStore: Send {
    fn load(&self, vault_id: &str) -> Result<Option<VaultKey>, String>;
    fn save(&self, vault_id: &str, key: &VaultKey) -> Result<(), String>;
    /// Removing an absent key succeeds; another vault's key is never touched.
    fn clear(&self, vault_id: &str) -> Result<(), String>;
    fn location(&self) -> KeyLocation {
        KeyLocation::SecureStorage
    }
}

/// The production store: OS keychain, or the isolated QA file when opted in.
pub struct OsVaultKeyStore {
    /// The data folder holding the QA key file, when opted in. Always None in
    /// release builds, which have no QA file backend at all.
    qa_dir: Option<PathBuf>,
}

impl OsVaultKeyStore {
    pub fn new(data_dir: &Path) -> OsVaultKeyStore {
        let isolated = std::env::var_os("CROSSBOW_DATA_DIR").map(PathBuf::from);
        let opted_in = key_file_opt_in(std::env::var("CROSSBOW_VAULT_KEY_FILE").ok().as_deref(), isolated.as_deref(), data_dir, &crate::default_data_dir());
        OsVaultKeyStore { qa_dir: opted_in.then(|| data_dir.to_path_buf()) }
    }

    #[cfg(test)]
    fn with_qa_file(data_dir: &Path, qa_file: bool) -> OsVaultKeyStore {
        OsVaultKeyStore { qa_dir: qa_file.then(|| data_dir.to_path_buf()) }
    }
}

/// Release builds never write a vault key to disk, whatever the environment
/// says. Debug and test builds need the exact opt-in and an isolated data
/// folder: a `CROSSBOW_DATA_DIR` that is the folder in use and not the default
/// one, so a variable left exported in a daily debug build can't move that
/// vault's key to a file.
fn key_file_opt_in(value: Option<&str>, isolated: Option<&Path>, data_dir: &Path, default_dir: &Path) -> bool {
    let same = |a: &Path, b: &Path| a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b);
    cfg!(any(debug_assertions, test))
        && value == Some("1")
        && isolated.is_some_and(|isolated| !isolated.as_os_str().is_empty() && same(isolated, data_dir) && !same(isolated, default_dir))
}

/// The QA key file's name in the data folder. One file per vault, so one data
/// directory's key never replaces another's.
pub fn qa_key_file(vault_id: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("vault-{}.key", data_encoding::HEXLOWER.encode(&Sha256::digest(vault_id.as_bytes())[..12]))
}

fn select_backend<T>(qa_dir: Option<&Path>, file_op: impl FnOnce(&Path) -> Result<T, String>, keychain_op: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    match qa_dir {
        Some(dir) => file_op(dir),
        None => keychain_op(),
    }
}

impl VaultKeyStore for OsVaultKeyStore {
    fn load(&self, vault_id: &str) -> Result<Option<VaultKey>, String> {
        select_backend(self.qa_dir.as_deref(), |dir| qa::load_file(dir, vault_id), || {
            #[cfg(feature = "keychain")]
            match entry(vault_id)?.get_password() {
                Ok(encoded) => decode(&Zeroizing::new(encoded)).map(Some),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(_) => Err(STORAGE_ERROR.into()),
            }
            #[cfg(not(feature = "keychain"))]
            Err(STORAGE_ERROR.into())
        })
    }

    fn save(&self, vault_id: &str, key: &VaultKey) -> Result<(), String> {
        select_backend(self.qa_dir.as_deref(), |dir| qa::save_file(dir, vault_id, key), || {
            #[cfg(feature = "keychain")]
            {
                entry(vault_id)?.set_password(&encode(key)).map_err(|_| STORAGE_ERROR.to_string())
            }
            #[cfg(not(feature = "keychain"))]
            Err(STORAGE_ERROR.into())
        })
    }

    fn clear(&self, vault_id: &str) -> Result<(), String> {
        select_backend(self.qa_dir.as_deref(), |dir| qa::clear_file(dir, vault_id), || {
            #[cfg(feature = "keychain")]
            match entry(vault_id)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(_) => Err(STORAGE_ERROR.into()),
            }
            #[cfg(not(feature = "keychain"))]
            Err(STORAGE_ERROR.into())
        })
    }

    fn location(&self) -> KeyLocation {
        if self.qa_dir.is_some() { KeyLocation::QaFile } else { KeyLocation::SecureStorage }
    }
}

#[cfg(feature = "keychain")]
fn entry(vault_id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, vault_id).map_err(|_| STORAGE_ERROR.into())
}

fn encode(key: &VaultKey) -> Zeroizing<String> {
    Zeroizing::new(BASE64URL_NOPAD.encode(key.as_bytes()))
}

fn decode(encoded: &str) -> Result<VaultKey, String> {
    let bytes = Zeroizing::new(BASE64URL_NOPAD.decode(encoded.as_bytes()).map_err(|_| "The stored vault key is unreadable.".to_string())?);
    VaultKey::from_slice(&bytes).ok_or_else(|| "The stored vault key is unreadable.".into())
}

/// The isolated-QA key file. Only debug and test builds compile it.
#[cfg(any(debug_assertions, test))]
mod qa {
    use super::{VaultKey, decode, encode, qa_key_file as key_file};
    use crate::sync::token_store::{PrivateFileError, read_private_file, write_private_file};
    use serde::{Deserialize, Serialize};
    use std::path::Path;
    use zeroize::{Zeroize, Zeroizing};

    #[derive(Serialize, Deserialize)]
    struct StoredKey {
        vault: String,
        key: String,
    }

    pub(super) fn save_file(data_dir: &Path, vault_id: &str, key: &VaultKey) -> Result<(), String> {
        let mut stored = StoredKey { vault: vault_id.into(), key: encode(key).to_string() };
        let data = serde_json::to_vec(&stored).map(Zeroizing::new);
        stored.key.zeroize();
        let data = data.map_err(|_| "Couldn't encode the QA vault key.".to_string())?;
        write_private_file(data_dir, &key_file(vault_id), &data).map_err(|_| "Couldn't save the QA vault key file.".into())
    }

    pub(super) fn load_file(data_dir: &Path, vault_id: &str) -> Result<Option<VaultKey>, String> {
        let data = match read_private_file(data_dir, &key_file(vault_id)) {
            Ok(Some(data)) => Zeroizing::new(data),
            Ok(None) => return Ok(None),
            Err(PrivateFileError::Unreadable) => return Err("Couldn't read the QA vault key file.".into()),
            Err(PrivateFileError::Invalid) => return Err("Invalid QA vault key file. Use an isolated profile.".into()),
        };
        let stored: StoredKey = serde_json::from_slice(&data).map_err(|_| "The QA vault key file is invalid.".to_string())?;
        let encoded = Zeroizing::new(stored.key);
        // Another vault's key is never returned; this vault simply has none.
        if stored.vault != vault_id {
            return Ok(None);
        }
        decode(&encoded).map(Some)
    }

    pub(super) fn clear_file(data_dir: &Path, vault_id: &str) -> Result<(), String> {
        if load_file(data_dir, vault_id)?.is_some() {
            std::fs::remove_file(data_dir.join(key_file(vault_id))).map_err(|_| "Couldn't remove the QA vault key file.".to_string())?;
        }
        Ok(())
    }
}

/// Release builds have no QA key file: `qa_dir` is always None there, and
/// these stand-ins touch no file and refuse, should anything reach them.
#[cfg(not(any(debug_assertions, test)))]
mod qa {
    use super::{STORAGE_ERROR, VaultKey};
    use std::path::Path;

    pub(super) fn save_file(_: &Path, _: &str, _: &VaultKey) -> Result<(), String> {
        Err(STORAGE_ERROR.into())
    }

    pub(super) fn load_file(_: &Path, _: &str) -> Result<Option<VaultKey>, String> {
        Err(STORAGE_ERROR.into())
    }

    pub(super) fn clear_file(_: &Path, _: &str) -> Result<(), String> {
        Err(STORAGE_ERROR.into())
    }
}

/// Process-local keys for tests and fixtures. Never touches the keychain.
/// Clones share the same keys, so a test can keep a handle on a store it
/// gave away.
#[derive(Clone, Default)]
pub struct MemoryVaultKeyStore {
    inner: Arc<MemoryKeys>,
}

#[derive(Default)]
struct MemoryKeys {
    keys: Mutex<HashMap<String, VaultKey>>,
    fail_writes: AtomicBool,
    fail_reads: AtomicBool,
    qa_file: AtomicBool,
}

impl MemoryVaultKeyStore {
    /// Report the QA key-file location, as the opt-in would.
    pub fn report_qa_file(&self, qa_file: bool) {
        self.inner.qa_file.store(qa_file, Ordering::SeqCst);
    }

    /// Simulate a denied or unavailable store for saves and removals.
    pub fn fail_writes(&self, fail: bool) {
        self.inner.fail_writes.store(fail, Ordering::SeqCst);
    }

    /// Simulate a declined or failed keychain read.
    pub fn fail_reads(&self, fail: bool) {
        self.inner.fail_reads.store(fail, Ordering::SeqCst);
    }

    pub fn contains(&self, vault_id: &str) -> bool {
        self.inner.keys.lock().unwrap().contains_key(vault_id)
    }

    /// Replace a vault's key behind its back, as another app or a restore might.
    pub fn replace(&self, vault_id: &str, key: VaultKey) {
        self.inner.keys.lock().unwrap().insert(vault_id.into(), key);
    }

    /// Remove a vault's key behind its back, ignoring simulated failures.
    pub fn forget(&self, vault_id: &str) {
        self.inner.keys.lock().unwrap().remove(vault_id);
    }
}

impl VaultKeyStore for MemoryVaultKeyStore {
    fn load(&self, vault_id: &str) -> Result<Option<VaultKey>, String> {
        if self.inner.fail_reads.load(Ordering::SeqCst) { return Err(STORAGE_ERROR.into()); }
        Ok(self.inner.keys.lock().unwrap().get(vault_id).and_then(|key| VaultKey::from_slice(key.as_bytes())))
    }

    fn save(&self, vault_id: &str, key: &VaultKey) -> Result<(), String> {
        if self.inner.fail_writes.load(Ordering::SeqCst) { return Err(STORAGE_ERROR.into()); }
        let copy = VaultKey::from_slice(key.as_bytes()).expect("32 bytes");
        self.inner.keys.lock().unwrap().insert(vault_id.into(), copy);
        Ok(())
    }

    fn clear(&self, vault_id: &str) -> Result<(), String> {
        if self.inner.fail_writes.load(Ordering::SeqCst) { return Err(STORAGE_ERROR.into()); }
        self.inner.keys.lock().unwrap().remove(vault_id);
        Ok(())
    }

    fn location(&self) -> KeyLocation {
        if self.inner.qa_file.load(Ordering::SeqCst) { KeyLocation::QaFile } else { KeyLocation::SecureStorage }
    }
}

#[cfg(test)]
mod tests {
    use super::qa::{clear_file, load_file, save_file};
    use super::qa_key_file as key_file;
    use super::*;

    #[test]
    fn qa_files_are_private_per_vault_and_never_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (VaultKey::from_slice(&[7; 32]).unwrap(), VaultKey::from_slice(&[8; 32]).unwrap());
        save_file(dir.path(), "vault-a", &a).unwrap();
        assert_eq!(load_file(dir.path(), "vault-a").unwrap().unwrap().as_bytes(), a.as_bytes());
        assert!(load_file(dir.path(), "vault-b").unwrap().is_none(), "another vault must never receive this key");
        save_file(dir.path(), "vault-b", &b).unwrap();
        assert_eq!(load_file(dir.path(), "vault-a").unwrap().unwrap().as_bytes(), a.as_bytes(), "B's key must not replace A's");
        assert_ne!(key_file("vault-a"), key_file("vault-b"));
        assert!(!key_file("vault-a").contains("vault-a"), "file names don't spell out vault ids");
        assert!(!key_file("vault-a").starts_with("library-"), "vault and library QA keys never share a file");
        clear_file(dir.path(), "vault-b").unwrap();
        assert!(load_file(dir.path(), "vault-a").unwrap().is_some(), "clearing vault B must keep vault A's key");
        // A file renamed onto another vault's slot is still refused.
        std::fs::rename(dir.path().join(key_file("vault-a")), dir.path().join(key_file("vault-b"))).unwrap();
        assert!(load_file(dir.path(), "vault-b").unwrap().is_none());
        std::fs::rename(dir.path().join(key_file("vault-b")), dir.path().join(key_file("vault-a"))).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            let path = dir.path().join(key_file("vault-a"));
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            std::fs::remove_file(&path).unwrap();
            let outside = dir.path().join("unrelated-file");
            std::fs::write(&outside, "preserve me").unwrap();
            symlink(&outside, &path).unwrap();
            assert!(load_file(dir.path(), "vault-a").is_err());
            save_file(dir.path(), "vault-a", &a).unwrap();
            assert_eq!(std::fs::read_to_string(outside).unwrap(), "preserve me");
        }
        clear_file(dir.path(), "vault-a").unwrap();
        assert!(load_file(dir.path(), "vault-a").unwrap().is_none());
    }

    #[test]
    fn the_vault_key_file_needs_its_own_exact_opt_in_and_an_isolated_data_folder() {
        let (isolated, default) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (isolated, default) = (isolated.path(), default.path());
        assert!(key_file_opt_in(Some("1"), Some(isolated), isolated, default));
        for value in [None, Some("0"), Some("true"), Some("")] {
            assert!(!key_file_opt_in(value, Some(isolated), isolated, default), "{value:?}");
        }
        // The opt-in alone, as a variable left exported in a daily debug candidate, does nothing.
        assert!(!key_file_opt_in(Some("1"), None, default, default), "no CROSSBOW_DATA_DIR");
        assert!(!key_file_opt_in(Some("1"), Some(default), default, default), "CROSSBOW_DATA_DIR set to the default folder");
        assert!(!key_file_opt_in(Some("1"), Some(&default.join(".")), default, default), "the default folder, spelled differently");
        #[cfg(unix)]
        {
            let link = isolated.join("link-to-default");
            std::os::unix::fs::symlink(default, &link).unwrap();
            assert!(!key_file_opt_in(Some("1"), Some(&link), &link, default), "a link to the default folder");
        }
        assert!(!key_file_opt_in(Some("1"), Some(Path::new("")), default, default), "an empty CROSSBOW_DATA_DIR");
        assert!(!key_file_opt_in(Some("1"), Some(isolated), default, default), "a data folder other than the isolated one");
    }

    #[test]
    fn a_denied_keychain_never_falls_back_to_a_file() {
        let result: Result<(), String> = select_backend(None, |_| panic!("must not write a key file"), || Err(STORAGE_ERROR.into()));
        assert_eq!(result.unwrap_err(), STORAGE_ERROR);
        let dir = tempfile::tempdir().unwrap();
        let store = OsVaultKeyStore::with_qa_file(dir.path(), true);
        store.save("vault-a", &VaultKey::from_slice(&[1; 32]).unwrap()).unwrap();
        assert!(store.load("vault-a").unwrap().is_some(), "the QA opt-in uses only its file");
    }

    /// Without the keychain feature and without the opt-in, every operation
    /// fails and nothing is written. Run with `--no-default-features`.
    #[cfg(not(feature = "keychain"))]
    #[test]
    fn without_a_keychain_or_opt_in_keys_are_refused_and_no_file_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let store = OsVaultKeyStore::with_qa_file(dir.path(), false);
        let key = VaultKey::from_slice(&[5; 32]).unwrap();
        assert_eq!(store.save("vault-a", &key).unwrap_err(), STORAGE_ERROR);
        assert_eq!(store.load("vault-a").unwrap_err(), STORAGE_ERROR);
        assert_eq!(store.clear("vault-a").unwrap_err(), STORAGE_ERROR);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn each_store_reports_where_it_keeps_keys() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(OsVaultKeyStore::with_qa_file(dir.path(), true).location(), KeyLocation::QaFile);
        assert_eq!(OsVaultKeyStore::with_qa_file(dir.path(), false).location(), KeyLocation::SecureStorage);
        let memory = MemoryVaultKeyStore::default();
        assert_eq!(memory.location(), KeyLocation::SecureStorage);
        memory.report_qa_file(true);
        assert_eq!(memory.location(), KeyLocation::QaFile);
    }

    #[test]
    fn keychain_encoding_round_trips_without_printing_bytes() {
        let key = VaultKey::from_slice(&[3; 32]).unwrap();
        assert_eq!(decode(&encode(&key)).unwrap().as_bytes(), key.as_bytes());
        assert!(decode("not-a-key").is_err());
        assert_eq!(format!("{key:?}"), "VaultKey(..)");
    }
}
