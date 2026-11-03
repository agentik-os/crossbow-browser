//! Crossbow core: everything that is not the window, the UI or the web engine.

pub mod backup;
pub mod captures;
pub mod content_blocking;
pub mod document_app;
pub mod import;
pub mod library;
pub mod link_preview;
pub mod link_routing;
pub mod site_profiles;
pub mod model;
pub mod order;
pub mod search;
pub mod site_permissions;
pub mod store;
pub mod suggest;
pub mod sync;
pub mod update;
pub mod vault;
#[cfg(test)]
mod test_script;

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub use crossbow_proto as proto;

pub fn now_ms() -> u64 {
    #[cfg(test)]
    if let Some(now) = test_script::now_ms() {
        return now;
    }
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Time-ordered unique id (UUID v7).
pub fn new_id() -> String {
    #[cfg(test)]
    if let Some(id) = test_script::id() {
        return id;
    }
    uuid::Uuid::now_v7().to_string()
}

/// Per-user data directory (`~/Library/Application Support/Crossbow` on macOS,
/// `%APPDATA%\Crossbow` on Windows, `~/.local/share/crossbow` on Linux).
/// `CROSSBOW_DATA_DIR` overrides it, which tests and second instances use.
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CROSSBOW_DATA_DIR") {
        return PathBuf::from(dir);
    }
    default_data_dir()
}

/// The data directory without the `CROSSBOW_DATA_DIR` override.
pub fn default_data_dir() -> PathBuf {
    directories::ProjectDirs::from("app", "Crossbow", "Crossbow")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("crossbow"))
}

pub fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}
