//! Device-local, per-profile download preferences. Filesystem paths never
//! enter synced entities; each new request reads the latest saved preference.

use crossbow_core::library::Library;
use crossbow_engine::DownloadInfo;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub directory: Option<PathBuf>,
    pub ask_each_time: bool,
}

fn key(profile: &str) -> String { format!("downloads.v1.{profile}") }

pub fn active_profile<'a>(lib: &'a Library, space: &str) -> Option<&'a str> {
    let profile = lib.spaces.get(space)?.profile_id.as_str();
    lib.profiles.contains_key(profile).then_some(profile)
}

impl Preferences {
    pub fn load(lib: &Library, profile: &str) -> Result<Self, String> {
        if !lib.profiles.contains_key(profile) { return Err("This profile no longer exists.".into()); }
        let raw = lib.store.kv_get(&key(profile)).map_err(|error| format!("Could not read download preferences: {error}"))?;
        let preferences: Self = match raw {
            Some(raw) => serde_json::from_str(&raw).map_err(|_| "The saved download preferences are invalid. Open Downloads in Settings to replace them.")?,
            None => Self::default(),
        };
        if preferences.directory.as_ref().is_some_and(|path| !path.is_absolute() || path.to_str().is_none_or(|s| s.contains('\0'))) {
            return Err("The saved download folder is invalid. Choose a new folder in Settings.".into());
        }
        Ok(preferences)
    }

    pub fn save(&self, lib: &Library, profile: &str) -> Result<(), String> {
        if !lib.profiles.contains_key(profile) { return Err("This profile no longer exists.".into()); }
        if let Some(path) = &self.directory { validate_directory(path)?; }
        let raw = serde_json::to_string(self).map_err(|_| "Could not encode download preferences.".to_string())?;
        lib.store.kv_set(&key(profile), &raw).map_err(|error| format!("Could not save download preferences: {error}. Your saved settings are unchanged. Retry Save or Cancel."))
    }

    pub fn destination(&self, default: &Path) -> Result<PathBuf, String> {
        if let Some(path) = &self.directory { return validate_directory(path); }
        // Only the standard default may be created. A missing custom folder
        // fails rather than silently recreating it or writing somewhere else.
        if !default.is_absolute() { return Err("The default download folder is not an absolute path.".into()); }
        std::fs::create_dir_all(default).map_err(|error| format!("Could not create the default download folder: {error}"))?;
        validate_directory(default)
    }
}

fn validate_directory(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || path.to_str().is_none_or(|s| s.contains('\0')) {
        return Err("Choose an absolute local folder with a valid Unicode path.".into());
    }
    let resolved = path.canonicalize().map_err(|error| format!("The download folder is unavailable: {error}. Choose another folder."))?;
    if !resolved.is_dir() { return Err("Choose a folder, not a file.".into()); }
    Ok(resolved)
}

pub fn for_profile<'a>(rows: &'a [DownloadInfo], profile: Option<&'a str>) -> impl Iterator<Item = &'a DownloadInfo> {
    rows.iter().filter(move |row| Some(row.profile_id.as_str()) == profile)
}

pub fn control_status(download: &DownloadInfo) -> crossbow_engine::DownloadControlStatus {
    if download.complete || download.canceled || download.destination_error.is_some() { return Default::default(); }
    download.control.as_ref().map(|control| control.status()).unwrap_or_default()
}

pub fn control(rows: &[DownloadInfo], profile: Option<&str>, owner: &str, id: u32, command: crossbow_engine::DownloadCommand) -> Result<(), String> {
    if profile != Some(owner) { return Err("Switch back to the download's profile before changing this transfer.".into()); }
    let download = for_profile(rows, profile).find(|row| row.id == id).ok_or("This download is no longer available.")?;
    if !control_status(download).allows(command) { return Err("This download action is unavailable or is still awaiting confirmation.".into()); }
    download.control.as_ref().ok_or("Download controls are unavailable.")?.request(owner, id, command)
}

/// Returns true only for a newly observed completion. CEF can report updates
/// before the destination request, and can cancel after we reject that request.
pub fn update(rows: &mut Vec<DownloadInfo>, mut next: DownloadInfo) -> bool {
    if let Some(previous) = rows.iter_mut().find(|row| row.profile_id == next.profile_id && row.id == next.id) {
        let completed = next.complete && !previous.complete;
        if next.file_name.is_empty() { next.file_name = previous.file_name.clone(); }
        if next.control.is_none() && !next.complete && !next.canceled { next.control = previous.control.clone(); }
        if next.destination_error.is_none() && !next.complete { next.destination_error = previous.destination_error.clone(); }
        *previous = next;
        completed
    } else {
        let completed = next.complete;
        rows.insert(0, next);
        completed
    }
}

pub struct SettingsDraft {
    pub profile: String,
    pub saved: Option<Preferences>,
    pub draft: Preferences,
    pub error: Option<String>,
    pub reveal_error: bool,
    picker: Option<Receiver<Result<Option<PathBuf>, String>>>,
}

impl SettingsDraft {
    pub fn load(lib: &Library, profile: &str) -> Self {
        let result = Preferences::load(lib, profile);
        let saved = result.as_ref().ok().cloned();
        Self { profile: profile.into(), draft: saved.clone().unwrap_or_default(), saved, error: result.err(), reveal_error: true, picker: None }
    }

    pub fn changed(&self) -> bool { self.saved.as_ref() != Some(&self.draft) }
    pub fn picking(&self) -> bool { self.picker.is_some() }

    pub fn save(&mut self, lib: &Library) {
        match self.draft.save(lib, &self.profile) {
            Ok(()) => { self.saved = Some(self.draft.clone()); self.error = None; }
            Err(error) => { self.error = Some(error); self.reveal_error = true; }
        }
    }

    pub fn cancel(&mut self, lib: &Library) { *self = Self::load(lib, &self.profile); }

    pub fn choose_folder(&mut self, ctx: &egui::Context) {
        if self.picking() { return; }
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        match native::pick_folder(move |result| { let _ = sender.send(result); ctx.request_repaint(); }) {
            Ok(()) => self.picker = Some(receiver),
            Err(error) => { self.error = Some(error); self.reveal_error = true; }
        }
    }

    pub fn poll(&mut self) {
        let Some(receiver) = &self.picker else { return };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("The folder chooser closed without a result. Try Choose Folder again.".into()),
        };
        self.picker = None;
        match result {
            Ok(None) => {}
            Ok(Some(path)) => match validate_directory(&path) {
                Ok(path) => { self.draft.directory = Some(path); self.error = None; }
                Err(error) => { self.error = Some(error); self.reveal_error = true; }
            },
            Err(error) => { self.error = Some(error); self.reveal_error = true; }
        }
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSModalResponseCancel, NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::NSString;

    pub fn pick_folder(done: impl Fn(Result<Option<PathBuf>, String>) + 'static) -> Result<(), String> {
        let main = MainThreadMarker::new().ok_or("Open the folder chooser from the main Settings window.")?;
        let panel = NSOpenPanel::openPanel(main);
        panel.setTitle(Some(&NSString::from_str("Choose a download folder")));
        panel.setPrompt(Some(&NSString::from_str("Choose")));
        panel.setCanChooseFiles(false);
        panel.setCanChooseDirectories(true);
        panel.setAllowsMultipleSelection(false);
        panel.setResolvesAliases(true);
        let selected = panel.clone();
        let completion = RcBlock::new(move |response| {
            done(if response == NSModalResponseOK {
                selected.URL().and_then(|url| url.to_file_path()).map(Some).ok_or_else(|| "Choose a local folder and try again.".into())
            } else if response == NSModalResponseCancel {
                Ok(None)
            } else { Err("The folder chooser could not open. Try again; your saved folder is unchanged.".into()) });
        });
        if let Some(window) = NSApplication::sharedApplication(main).keyWindow() {
            panel.beginSheetModalForWindow_completionHandler(&window, &completion);
        } else { panel.beginWithCompletionHandler(&completion); }
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    use super::*;
    pub fn pick_folder(_: impl Fn(Result<Option<PathBuf>, String>) + 'static) -> Result<(), String> {
        Err("Download folder selection is currently available on macOS only.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbow_core::store::Store;

    fn library(path: &Path) -> Library {
        let mut lib = Library::open(Store::open(path).unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        lib
    }

    #[test]
    fn preferences_are_profile_local_unsynced_and_persist_across_sqlite_reopen() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("library.sqlite");
        let mut lib = library(&db);
        let a = lib.default_profile_id();
        let b = lib.create_profile("B").unwrap();
        let before = lib.store.all_entities().unwrap();
        let preferences = Preferences { directory: Some(root.path().canonicalize().unwrap()), ask_each_time: true };
        preferences.save(&lib, &a).unwrap();
        assert_eq!(Preferences::load(&lib, &b).unwrap(), Preferences::default());
        assert_eq!(lib.store.all_entities().unwrap(), before, "local paths must not enter the sync entity stream");
        drop(lib);
        let lib = library(&db);
        assert_eq!(Preferences::load(&lib, &a).unwrap(), preferences);
        assert_eq!(Preferences::load(&lib, &b).unwrap(), Preferences::default());
        assert!(Preferences::load(&lib, "missing").is_err());
        assert!(preferences.save(&lib, "missing").is_err());
        assert!(active_profile(&lib, "missing").is_none());
    }

    #[test]
    fn failed_save_retains_draft_and_old_value_until_explicit_retry_or_cancel() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("library.sqlite");
        let lib = library(&db);
        let profile = lib.default_profile_id();
        let mut state = SettingsDraft::load(&lib, &profile);
        state.draft.ask_each_time = true;
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute_batch("CREATE TRIGGER reject_downloads BEFORE INSERT ON kv WHEN NEW.k LIKE 'downloads.v1.%' BEGIN SELECT RAISE(ABORT, 'fixture denial'); END;").unwrap();
        state.save(&lib);
        assert!(state.error.is_some());
        assert!(state.draft.ask_each_time);
        assert!(!Preferences::load(&lib, &profile).unwrap().ask_each_time);
        state.cancel(&lib);
        assert!(!state.draft.ask_each_time);
        state.draft.ask_each_time = true;
        state.save(&lib);
        connection.execute_batch("DROP TRIGGER reject_downloads").unwrap();
        state.save(&lib);
        assert!(state.error.is_none());
        assert!(!state.changed());
        drop(lib);
        assert!(Preferences::load(&library(&db), &profile).unwrap().ask_each_time);
    }

    #[test]
    fn malformed_settings_and_missing_custom_folders_never_fall_back() {
        let root = tempfile::tempdir().unwrap();
        let lib = library(&root.path().join("library.sqlite"));
        let profile = lib.default_profile_id();
        lib.store.kv_set(&key(&profile), "not json").unwrap();
        assert!(Preferences::load(&lib, &profile).is_err());
        let mut state = SettingsDraft::load(&lib, &profile);
        assert!(state.saved.is_none());
        assert!(state.error.is_some());
        state.save(&lib);
        assert_eq!(Preferences::load(&lib, &profile).unwrap(), Preferences::default());
        let missing = root.path().join("missing");
        let prefs = Preferences { directory: Some(missing.clone()), ask_each_time: false };
        assert!(prefs.save(&lib, &profile).is_err());
        assert!(prefs.destination(&root.path().join("default")).is_err());
        assert!(!missing.exists());
        assert!(!root.path().join("default").exists());
    }

    #[test]
    fn picker_cancel_or_late_result_does_not_save_or_retarget_another_profile() {
        let root = tempfile::tempdir().unwrap();
        let mut lib = library(&root.path().join("library.sqlite"));
        let a = lib.default_profile_id();
        let b = lib.create_profile("B").unwrap();
        let mut state = SettingsDraft::load(&lib, &a);
        let (send, receive) = mpsc::channel();
        state.picker = Some(receive);
        send.send(Ok(None)).unwrap();
        state.poll();
        assert!(!state.changed());
        let (send, receive) = mpsc::channel();
        state.picker = Some(receive);
        state = SettingsDraft::load(&lib, &b);
        assert!(send.send(Ok(Some(root.path().into()))).is_err());
        state.poll();
        assert_eq!(state.profile, b);
        assert!(!state.changed());
        assert_eq!(Preferences::load(&lib, &a).unwrap(), Preferences::default());
    }

    #[test]
    fn successful_picker_result_stays_a_draft_and_removed_folder_save_can_be_retried() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("library.sqlite");
        let lib = library(&db);
        let profile = lib.default_profile_id();
        let chosen = root.path().join("chosen");
        std::fs::create_dir(&chosen).unwrap();
        let mut state = SettingsDraft::load(&lib, &profile);
        let (send, receive) = mpsc::channel();
        state.picker = Some(receive);
        send.send(Ok(Some(chosen.clone()))).unwrap();
        state.poll();
        assert!(state.changed());
        assert!(!state.picking());
        assert_eq!(Preferences::load(&lib, &profile).unwrap().directory, None);
        std::fs::remove_dir(&chosen).unwrap();
        state.save(&lib);
        assert!(state.error.as_deref().unwrap().contains("unavailable"));
        assert!(state.draft.directory.is_some());
        assert_eq!(Preferences::load(&lib, &profile).unwrap().directory, None);
        std::fs::create_dir(&chosen).unwrap();
        state.save(&lib);
        assert!(!state.changed());
        assert!(state.error.is_none());
        drop(lib);
        let lib = library(&db);
        assert_eq!(Preferences::load(&lib, &profile).unwrap().directory, Some(chosen.canonicalize().unwrap()));
        let default = root.path().join("default");
        std::fs::remove_dir(&chosen).unwrap();
        assert!(Preferences::load(&lib, &profile).unwrap().destination(&default).is_err());
        assert!(!default.exists(), "a removed custom folder must not silently fall back");
    }

    #[test]
    fn control_dispatch_rejects_stale_profile_missing_row_and_unavailable_callback() {
        use crossbow_engine::DownloadCommand;
        let rows = vec![DownloadInfo { id: 1, profile_id: "A".into(), in_progress: true, ..Default::default() }];
        for profile in [None, Some("B")] {
            assert!(control(&rows, profile, "A", 1, DownloadCommand::Cancel).unwrap_err().contains("Switch back"));
        }
        assert!(control(&rows, Some("A"), "A", 2, DownloadCommand::Cancel).unwrap_err().contains("no longer available"));
        assert!(control(&rows, Some("A"), "A", 1, DownloadCommand::Cancel).is_err());
        assert!(!rows[0].canceled, "failed or unavailable dispatch never synthesizes completion");
    }

    #[test]
    fn download_updates_preserve_paused_state_without_making_it_complete() {
        let mut rows = Vec::new();
        let info = DownloadInfo { id: 1, profile_id: "A".into(), in_progress: true, paused: true, ..Default::default() };
        assert!(!update(&mut rows, info.clone()));
        assert!(rows[0].paused && rows[0].active());
        assert!(!rows[0].complete);
        assert!(!update(&mut rows, DownloadInfo { paused: false, ..info }));
        assert!(!rows[0].paused);
    }

    #[test]
    fn resumed_download_clears_the_previous_engine_interruption() {
        let current = DownloadInfo { id: 1, profile_id: "A".into(), file_name: "fixture.txt".into(), in_progress: true, ..Default::default() };
        let mut rows = vec![DownloadInfo { in_progress: false, interrupted: true, error: Some("Network interruption".into()), ..current.clone() }];
        assert!(!update(&mut rows, current));
        assert!(rows[0].error.is_none(), "a fresh engine update replaces its old interruption error");
        assert!(rows[0].active());
        assert!(rows[0].in_progress);
    }

    #[test]
    fn canceled_download_replaces_the_previous_engine_interruption() {
        let current = DownloadInfo { id: 1, profile_id: "A".into(), canceled: true, ..Default::default() };
        let mut rows = vec![DownloadInfo { canceled: false, interrupted: true, error: Some("Network interruption".into()), ..current.clone() }];
        assert!(!update(&mut rows, current));
        assert!(rows[0].error.is_none(), "cancellation must not display a stale interruption error");
        assert!(rows[0].canceled);
        assert!(!rows[0].active());
    }

    #[test]
    fn local_destination_rejection_survives_engine_updates_without_stale_engine_errors() {
        let current = DownloadInfo { id: 2, profile_id: "A".into(), ..Default::default() };
        let mut rows = vec![DownloadInfo { destination_error: Some("Folder unavailable".into()), ..current.clone() }];
        for next in [
            DownloadInfo { interrupted: true, error: Some("Network interruption".into()), ..current.clone() },
            DownloadInfo { canceled: true, ..current.clone() },
            DownloadInfo { in_progress: true, ..current.clone() },
        ] {
            let engine_error = next.error.clone();
            assert!(!update(&mut rows, next));
            assert_eq!(rows[0].error, engine_error);
            assert_eq!(rows[0].failure(), Some("Folder unavailable"));
            assert!(!rows[0].active());
        }
        assert!(update(&mut rows, DownloadInfo { complete: true, ..current }));
        assert!(rows[0].failure().is_none());
    }

    #[test]
    fn download_updates_use_profile_and_id_and_preserve_local_errors() {
        let a = DownloadInfo { id: 1, profile_id: "A".into(), file_name: "A.txt".into(), destination_error: Some("Folder unavailable".into()), ..Default::default() };
        let mut rows = vec![a.clone()];
        let b = DownloadInfo { profile_id: "B".into(), file_name: "B.txt".into(), destination_error: None, ..a.clone() };
        assert!(!update(&mut rows, b.clone()));
        assert_eq!(rows.len(), 2);
        assert!(!update(&mut rows, DownloadInfo { file_name: String::new(), destination_error: None, canceled: true, ..a }));
        assert_eq!(for_profile(&rows, Some("A")).next().unwrap().failure(), Some("Folder unavailable"));
        assert_eq!(for_profile(&rows, Some("A")).next().unwrap().file_name, "A.txt");
        assert!(for_profile(&rows, None).next().is_none());
        assert!(update(&mut rows, DownloadInfo { complete: true, ..b.clone() }));
        assert!(!update(&mut rows, DownloadInfo { complete: true, ..b }));
        assert_eq!(for_profile(&rows, Some("B")).count(), 1);
        assert!(!for_profile(&rows, Some("A")).next().unwrap().active());
    }
}
