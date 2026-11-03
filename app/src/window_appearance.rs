//! Device-local window chrome preferences. Drafts never change the live window.

use crossbow_core::store::Store;
use serde::{Deserialize, Serialize};

const KEY: &str = "window_appearance";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChromeSettings {
    pub transparent: bool,
    pub tint: u8,
}

impl Default for ChromeSettings {
    fn default() -> Self { Self { transparent: false, tint: 35 } }
}

pub struct WindowChrome {
    saved: ChromeSettings,
    pub draft: ChromeSettings,
    pub error: Option<String>,
}

impl WindowChrome {
    pub fn load(store: &Store) -> Self {
        let loaded = store.kv_get(KEY).map_err(|e| e.to_string()).and_then(|value| {
            value.map(|value| serde_json::from_str::<ChromeSettings>(&value).map_err(|e| e.to_string())).transpose()
        });
        let (saved, error) = match loaded {
            Ok(value) => {
                let mut saved = value.unwrap_or_default();
                saved.tint = saved.tint.min(100);
                (saved, None)
            }
            Err(_) => (ChromeSettings::default(), Some("Could not load window appearance. Using an opaque window; choose your settings and Apply to retry saving.".into())),
        };
        Self { saved, draft: saved, error }
    }

    pub fn saved(&self) -> ChromeSettings { self.saved }
    pub fn changed(&self) -> bool { self.draft != self.saved }

    pub fn material_requested(&self, onboarding: bool) -> bool {
        onboarding || self.saved.transparent
    }

    pub fn onboarding_tint_opacity(material_active: bool) -> f32 {
        if material_active { 0.12 } else { 1.0 }
    }

    pub fn apply(&mut self, store: &Store) -> bool {
        let candidate = ChromeSettings { tint: self.draft.tint.min(100), ..self.draft };
        let result = serde_json::to_string(&candidate).map_err(|e| e.to_string())
            .and_then(|value| store.kv_set(KEY, &value).map_err(|e| e.to_string()));
        match result {
            Ok(()) => {
                self.saved = candidate;
                self.draft = candidate;
                self.error = None;
                true
            }
            Err(_) => {
                self.error = Some("Could not save window appearance. The window is unchanged and your draft is kept. Check disk space and data-folder permissions, then Retry or Cancel.".into());
                false
            }
        }
    }

    pub fn cancel(&mut self) {
        self.draft = self.saved;
        self.error = None;
    }

    pub fn tint_opacity(&self, material_active: bool) -> f32 {
        if material_active && self.saved.transparent { f32::from(self.saved.tint) / 100.0 } else { 1.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_and_invalid_preferences_fail_opaque_without_overwriting_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("library.sqlite")).unwrap();
        assert_eq!(WindowChrome::load(&store).saved(), ChromeSettings::default());
        store.kv_set(KEY, "{}").unwrap();
        assert_eq!(WindowChrome::load(&store).saved(), ChromeSettings::default());
        store.kv_set(KEY, "invalid fixture").unwrap();
        let mut chrome = WindowChrome::load(&store);
        assert!(!chrome.saved().transparent);
        assert!(chrome.error.is_some());
        assert_eq!(store.kv_get(KEY).unwrap().as_deref(), Some("invalid fixture"));
        assert!(chrome.apply(&store));
        assert!(WindowChrome::load(&store).error.is_none());
    }

    #[test]
    fn draft_cancel_apply_and_reopen_preserve_local_only_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let store = Store::open(&path).unwrap();
        let mut chrome = WindowChrome::load(&store);
        chrome.draft = ChromeSettings { transparent: true, tint: 12 };
        assert!(!chrome.saved().transparent);
        chrome.cancel();
        assert_eq!(chrome.draft, ChromeSettings::default());
        assert!(store.kv_get(KEY).unwrap().is_none());
        chrome.draft = ChromeSettings { transparent: true, tint: 24 };
        assert!(chrome.apply(&store));
        drop(store);
        let store = Store::open(&path).unwrap();
        let chrome = WindowChrome::load(&store);
        assert_eq!(chrome.saved(), ChromeSettings { transparent: true, tint: 24 });
        assert!(!chrome.changed());
        let db = rusqlite::Connection::open(&path).unwrap();
        let synced: i64 = db.query_row("SELECT count(*) FROM entities", [], |r| r.get(0)).unwrap();
        assert_eq!(synced, 0, "device material preferences must not enter account sync");
    }

    #[test]
    fn failed_save_preserves_window_and_draft_then_retry_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let store = Store::open(&path).unwrap();
        let mut chrome = WindowChrome::load(&store);
        assert!(chrome.apply(&store));
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TRIGGER block_appearance BEFORE UPDATE ON kv WHEN NEW.k = 'window_appearance' BEGIN SELECT RAISE(ABORT, 'fixture save failure'); END;").unwrap();
        chrome.draft = ChromeSettings { transparent: true, tint: 18 };
        assert!(!chrome.apply(&store));
        assert_eq!(chrome.saved(), ChromeSettings::default());
        assert_eq!(chrome.draft, ChromeSettings { transparent: true, tint: 18 });
        assert!(chrome.error.as_deref().unwrap().contains("Retry or Cancel"));
        assert_eq!(WindowChrome::load(&store).saved(), ChromeSettings::default());
        db.execute_batch("DROP TRIGGER block_appearance;").unwrap();
        assert!(chrome.apply(&store));
        drop(store);
        assert_eq!(WindowChrome::load(&Store::open(&path).unwrap()).saved(), ChromeSettings { transparent: true, tint: 18 });
    }

    #[test]
    fn first_run_material_is_temporary_and_falls_back_without_changing_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("library.sqlite")).unwrap();
        let chrome = WindowChrome::load(&store);
        assert!(chrome.material_requested(true));
        assert!(!chrome.material_requested(false));
        assert_eq!(WindowChrome::onboarding_tint_opacity(true), 0.12);
        assert_eq!(WindowChrome::onboarding_tint_opacity(false), 1.0);
        assert_eq!(chrome.saved(), ChromeSettings::default());
        assert!(store.kv_get(KEY).unwrap().is_none());
    }

    #[test]
    fn fallback_is_opaque_at_every_saved_tint() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("library.sqlite")).unwrap();
        let mut chrome = WindowChrome::load(&store);
        for tint in [0, 35, 100, 255] {
            chrome.draft = ChromeSettings { transparent: true, tint };
            assert!(chrome.apply(&store));
            assert_eq!(chrome.tint_opacity(false), 1.0);
            assert_eq!(chrome.tint_opacity(true), f32::from(tint.min(100)) / 100.0);
        }
        chrome.draft.transparent = false;
        assert!(chrome.apply(&store));
        assert_eq!(chrome.tint_opacity(true), 1.0);
    }
}
