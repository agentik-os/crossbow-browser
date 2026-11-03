//! Session credentials stay in the OS keychain. File storage is an explicit
//! isolated-QA opt-in, never a fallback after a denied or unavailable keychain.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[cfg(feature = "keychain")]
const SERVICE: &str = "app.crossbow.sync";
const STORAGE_ERROR: &str = "Secure credential storage is unavailable or access was declined.";

#[derive(Serialize, Deserialize)]
struct StoredToken {
    account: String,
    token: String,
}

pub fn save(data_dir: &Path, account: &str, token: &str) -> Result<(), String> {
    select_backend(
        use_file(),
        || save_file(data_dir, account, token),
        || {
            #[cfg(feature = "keychain")]
            {
                entry(account)?.set_password(token).map_err(|_| STORAGE_ERROR.to_string())
            }
            #[cfg(not(feature = "keychain"))]
            Err(STORAGE_ERROR.into())
        },
    )
}

pub fn load(data_dir: &Path, account: &str) -> Result<Option<String>, String> {
    select_backend(
        use_file(),
        || load_file(data_dir, account),
        || {
            #[cfg(feature = "keychain")]
            match entry(account)?.get_password() {
                Ok(token) => Ok(Some(token)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(_) => Err(STORAGE_ERROR.into()),
            }
            #[cfg(not(feature = "keychain"))]
            Err(STORAGE_ERROR.into())
        },
    )
}

pub fn clear(data_dir: &Path, account: &str) -> Result<(), String> {
    select_backend(
        use_file(),
        || clear_file(data_dir, account),
        || {
            #[cfg(feature = "keychain")]
            match entry(account)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(_) => Err(STORAGE_ERROR.into()),
            }
            #[cfg(not(feature = "keychain"))]
            Err(STORAGE_ERROR.into())
        },
    )
}

fn clear_file(data_dir: &Path, account: &str) -> Result<(), String> {
    // Never remove another account's fixture token.
    if load_file(data_dir, account)?.is_some() {
        std::fs::remove_file(data_dir.join("session.token")).map_err(|_| "Couldn't remove the QA session file.".to_string())?;
    }
    Ok(())
}

fn select_backend<T>(file: bool, file_op: impl FnOnce() -> Result<T, String>, keychain_op: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    if file { file_op() } else { keychain_op() }
}

pub(super) fn use_file() -> bool {
    std::env::var("CROSSBOW_TOKEN_FILE").is_ok_and(|v| v == "1")
}

#[cfg(feature = "keychain")]
fn entry(account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, account).map_err(|_| STORAGE_ERROR.into())
}

fn save_file(data_dir: &Path, account: &str, token: &str) -> Result<(), String> {
    let data = serde_json::to_vec(&StoredToken { account: account.into(), token: token.into() })
        .map_err(|_| "Couldn't encode the QA session.".to_string())?;
    write_private_file(data_dir, "session.token", &data).map_err(|_| "Couldn't save the QA session file.".into())
}

/// Atomically replace `data_dir/name` with a new owner-only file.
pub(crate) fn write_private_file(data_dir: &Path, name: &str, data: &[u8]) -> std::io::Result<()> {
    // A new private inode also repairs permissions on old files, and rename
    // replaces a destination symlink instead of following it.
    let temporary = data_dir.join(format!(".{name}-{}.tmp", crate::new_id()));
    let result = (|| {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&temporary)?;
        file.write_all(data)?;
        file.sync_all()?;
        std::fs::rename(&temporary, data_dir.join(name))
    })();
    if result.is_err() { let _ = std::fs::remove_file(&temporary); }
    result
}

pub(crate) enum PrivateFileError {
    Unreadable,
    /// A symlink, directory or oversized file.
    Invalid,
}

/// Read a small regular file without following a symlink. On Unix the open
/// itself refuses a symlink and the checks run on the opened handle, so
/// nothing can be swapped in between the check and the read.
pub(crate) fn read_private_file(data_dir: &Path, name: &str) -> Result<Option<Vec<u8>>, PrivateFileError> {
    use std::io::Read;
    const MAX_LEN: u64 = 16_384;
    let path = data_dir.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Non-blocking, so a FIFO planted here can't hang the open.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => return Err(PrivateFileError::Invalid),
        _ => {}
    }
    let file = match options.open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        #[cfg(unix)]
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Err(PrivateFileError::Invalid),
        Err(_) => return Err(PrivateFileError::Unreadable),
    };
    let metadata = file.metadata().map_err(|_| PrivateFileError::Unreadable)?;
    if !metadata.is_file() || metadata.len() > MAX_LEN {
        return Err(PrivateFileError::Invalid);
    }
    let mut data = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_LEN + 1).read_to_end(&mut data).map_err(|_| PrivateFileError::Unreadable)?;
    if data.len() as u64 > MAX_LEN {
        return Err(PrivateFileError::Invalid);
    }
    Ok(Some(data))
}

fn load_file(data_dir: &Path, account: &str) -> Result<Option<String>, String> {
    let data = match read_private_file(data_dir, "session.token") {
        Ok(Some(data)) => data,
        Ok(None) => return Ok(None),
        Err(PrivateFileError::Unreadable) => return Err("Couldn't read the QA session file.".into()),
        Err(PrivateFileError::Invalid) => return Err("Invalid QA session file. Sign in again in an isolated profile.".into()),
    };
    let stored: StoredToken = serde_json::from_slice(&data)
        .map_err(|_| "The QA session uses an old or invalid format. Sign in again.".to_string())?;
    if stored.account != account || stored.token.is_empty() {
        return Err("The QA session does not match this account.".into());
    }
    Ok(Some(stored.token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_keychain_never_falls_back_to_file() {
        let result: Result<(), String> = select_backend(false, || panic!("must not write plaintext"), || Err(STORAGE_ERROR.into()));
        assert_eq!(result.unwrap_err(), STORAGE_ERROR);
    }

    #[test]
    fn qa_save_load_and_clear_never_touch_keychain() {
        let dir = tempfile::tempdir().unwrap();
        select_backend(true, || save_file(dir.path(), "fixture-a", "fixture-token"), || panic!("QA must not request a keychain authorization")).unwrap();
        let token = select_backend(true, || load_file(dir.path(), "fixture-a"), || panic!("QA must not read the keychain")).unwrap();
        assert_eq!(token.as_deref(), Some("fixture-token"));
        assert!(clear_file(dir.path(), "fixture-b").is_err());
        select_backend(true, || clear_file(dir.path(), "fixture-a"), || panic!("QA must not delete from keychain")).unwrap();
        assert!(load_file(dir.path(), "fixture-a").unwrap().is_none());
    }

    #[test]
    fn fixture_token_is_account_bound_and_legacy_plaintext_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        save_file(dir.path(), "fixture-a", "fixture-token").unwrap();
        assert!(load_file(dir.path(), "fixture-b").is_err());
        std::fs::write(dir.path().join("session.token"), "old-fixture-token").unwrap();
        let error = load_file(dir.path(), "fixture-a").unwrap_err();
        assert!(!error.contains("old-fixture-token"));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_save_repairs_permissions_without_following_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("unrelated-file");
        let path = dir.path().join("session.token");
        std::fs::write(&outside, "preserve me").unwrap();
        symlink(&outside, &path).unwrap();
        assert!(load_file(dir.path(), "fixture-a").is_err());
        save_file(dir.path(), "fixture-a", "fixture-token").unwrap();
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "preserve me");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        save_file(dir.path(), "fixture-a", "replacement").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(load_file(dir.path(), "fixture-a").unwrap().as_deref(), Some("replacement"));
    }
}
