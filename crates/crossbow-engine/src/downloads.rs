//! Deferred, profile-bound download destinations. Dropping an unexecuted
//! before-download callback cancels the request in both CEF runtime styles.

use cef::{BeforeDownloadCallback, CefString, DownloadItemCallback, ImplBeforeDownloadCallback, ImplDownloadItemCallback};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

thread_local! {
    // CEF dispatch and Continue run on the browser UI thread. Reserve before
    // Continue: it may not create a file until after another request arrives.
    static DESTINATIONS: RefCell<HashMap<(String, u32), PathBuf>> = RefCell::new(HashMap::new());
}

pub(crate) fn observe(info: &DownloadInfo) {
    if info.complete || info.canceled {
        DESTINATIONS.with_borrow_mut(|paths| { paths.remove(&(info.profile_id.clone(), info.id)); });
    }
    // Interrupted downloads can still resume. Retain their reservation until
    // completion/cancellation, or process exit if no final callback arrives.
}

const CONTROL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadCommand { Pause, Resume, Cancel }

impl DownloadCommand {
    pub fn label(self) -> &'static str {
        match self { Self::Pause => "Pause", Self::Resume => "Resume", Self::Cancel => "Cancel" }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownloadPending {
    pub command: DownloadCommand,
    pub overdue: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DownloadControlStatus {
    pub pause: bool,
    pub resume: bool,
    pub cancel: bool,
    pub available: bool,
    pub pending: Option<DownloadPending>,
}

impl DownloadControlStatus {
    pub fn allows(self, command: DownloadCommand) -> bool {
        match command { DownloadCommand::Pause => self.pause, DownloadCommand::Resume => self.resume, DownloadCommand::Cancel => self.cancel }
    }
}

struct ControlState {
    profile: String,
    id: u32,
    in_progress: bool,
    paused: bool,
    interrupted: bool,
    terminal: bool,
    callback: Option<DownloadItemCallback>,
    pending: Option<(DownloadCommand, std::time::Instant)>,
}

/// Main/UI-thread handle shared by snapshots. It retains only the asynchronous
/// control callback, never a DownloadItem. Old clones see terminal revocation.
#[derive(Clone)]
pub struct DownloadControl(Rc<RefCell<ControlState>>);

impl std::fmt::Debug for DownloadControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadControl").field("status", &self.status()).finish_non_exhaustive()
    }
}

impl PartialEq for DownloadControl {
    fn eq(&self, other: &Self) -> bool { Rc::ptr_eq(&self.0, &other.0) }
}

impl DownloadControl {
    pub(crate) fn new(info: &DownloadInfo) -> Self {
        Self(Rc::new(RefCell::new(ControlState {
            profile: info.profile_id.clone(), id: info.id, in_progress: false,
            paused: false, interrupted: false, terminal: false, callback: None, pending: None,
        })))
    }

    pub(crate) fn update(&self, info: &DownloadInfo, callback: Option<DownloadItemCallback>) {
        let mut state = self.0.borrow_mut();
        debug_assert_eq!((&state.profile, state.id), (&info.profile_id, info.id));
        state.in_progress = info.in_progress;
        state.paused = info.paused;
        state.interrupted = info.interrupted;
        state.terminal = info.complete || info.canceled;
        state.callback = if state.terminal { None } else { callback };
        if state.terminal || state.pending.is_some_and(|(command, _)| match command {
            // Interruption supersedes Pause rather than acknowledging it; allow
            // an explicit Resume attempt instead of trapping an obsolete command.
            DownloadCommand::Pause => info.paused || info.interrupted,
            DownloadCommand::Resume => info.in_progress && !info.paused && !info.interrupted,
            DownloadCommand::Cancel => info.canceled,
        }) { state.pending = None; }
    }

    pub(crate) fn invalidate(&self) {
        let mut state = self.0.borrow_mut();
        state.callback = None;
        state.pending = None;
        state.terminal = true;
    }

    pub fn status(&self) -> DownloadControlStatus { self.status_at(std::time::Instant::now()) }

    fn status_at(&self, now: std::time::Instant) -> DownloadControlStatus {
        let state = self.0.borrow();
        let available = state.callback.is_some() && !state.terminal;
        let pending = state.pending.map(|(command, at)| DownloadPending { command, overdue: now.saturating_duration_since(at) >= CONTROL_TIMEOUT });
        let allowed = |command| available && pending.is_none_or(|pending| {
            (command == DownloadCommand::Cancel && pending.command != DownloadCommand::Cancel)
                || (pending.overdue && pending.command == command)
        });
        DownloadControlStatus {
            pause: state.in_progress && !state.paused && !state.interrupted && allowed(DownloadCommand::Pause),
            resume: ((state.in_progress && state.paused) || state.interrupted) && allowed(DownloadCommand::Resume),
            cancel: allowed(DownloadCommand::Cancel), available, pending,
        }
    }

    /// Submission only. CEF's void methods expose no acceptance/completion
    /// result. Wait for a matching update; overdue requests may be retried.
    pub fn request(&self, profile: &str, id: u32, command: DownloadCommand) -> Result<(), String> {
        self.request_at(profile, id, command, std::time::Instant::now())
    }

    fn request_at(&self, profile: &str, id: u32, command: DownloadCommand, now: std::time::Instant) -> Result<(), String> {
        {
            let state = self.0.borrow();
            if state.profile != profile || state.id != id { return Err("This download belongs to a different profile or transfer.".into()); }
        }
        if !self.status_at(now).allows(command) { return Err("This download action is unavailable or is still awaiting confirmation.".into()); }
        let callback = {
            let mut state = self.0.borrow_mut();
            let callback = state.callback.clone().ok_or("Download controls are unavailable. Retry from the original page.")?;
            state.pending = Some((command, now));
            callback
        };
        // A CEF call can re-enter update. No state/queue borrow survives dispatch.
        match command { DownloadCommand::Pause => callback.pause(), DownloadCommand::Resume => callback.resume(), DownloadCommand::Cancel => callback.cancel() }
        crate::wake(std::time::Duration::ZERO);
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DownloadInfo {
    pub id: u32,
    pub profile_id: String,
    pub url: String,
    pub path: String,
    pub file_name: String,
    pub received: i64,
    pub total: i64,
    pub percent: i32,
    pub in_progress: bool,
    pub paused: bool,
    pub control: Option<DownloadControl>,
    pub complete: bool,
    pub canceled: bool,
    pub interrupted: bool,
    /// Latest engine failure; replaced by every CEF update.
    pub error: Option<String>,
    /// Local preference/destination rejection, retained through CEF cancellation.
    pub destination_error: Option<String>,
    /// The type the server declared, for a file-name suggestion; may be empty.
    pub mime_type: String,
}

impl DownloadInfo {
    pub fn failure(&self) -> Option<&str> {
        self.destination_error.as_deref().or(self.error.as_deref())
    }

    pub fn active(&self) -> bool {
        !self.complete && !self.canceled && !self.interrupted && self.failure().is_none()
            // A terminal update or browser closure revokes even old snapshots.
            // Losing observation stops activity, not a fabricated transfer outcome.
            && self.control.as_ref().is_none_or(|control| !control.0.borrow().terminal)
    }
}

#[derive(Clone)]
pub struct DownloadRequest {
    pub info: DownloadInfo,
    callback: Rc<RefCell<Option<BeforeDownloadCallback>>>,
}

impl std::fmt::Debug for DownloadRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadRequest").field("info", &self.info).finish_non_exhaustive()
    }
}

impl DownloadRequest {
    pub(crate) fn new(info: DownloadInfo, callback: BeforeDownloadCallback) -> Self {
        Self { info, callback: Rc::new(RefCell::new(Some(callback))) }
    }

    /// Called on the browser UI thread. This submits a destination, not a
    /// completion result. A native Save As cancellation stays CEF-owned.
    pub fn continue_in(self, directory: &Path, ask_each_time: bool) -> Result<(), String> {
        let callback = self.callback.borrow_mut().take().ok_or("This download request was already handled.")?;
        let path = destination(directory, &self.info.file_name).map_err(|error| format!("Could not use the download folder: {error}"))?;
        let path_string = path.to_str().ok_or("The download path is not valid Unicode. Choose another folder.")?;
        DESTINATIONS.with_borrow_mut(|paths| { paths.insert((self.info.profile_id.clone(), self.info.id), path.clone()); });
        callback.cont(Some(&CefString::from(path_string)), i32::from(ask_each_time));
        Ok(())
    }
}

fn destination_key(path: &Path) -> String {
    let path = path.to_string_lossy();
    #[cfg(target_os = "macos")]
    let path = objc2_foundation::NSString::from_str(&path).decomposedStringWithCanonicalMapping().to_string();
    path.to_lowercase()
}

fn destination(directory: &Path, name: &str) -> std::io::Result<PathBuf> {
    if !directory.is_absolute() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Choose an absolute local folder."));
    }
    let directory = directory.canonicalize()?;
    if !directory.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotADirectory, "Choose a folder, not a file."));
    }
    let clean: String = name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*') || c.is_control() { '_' } else { c }).collect();
    let clean = clean.trim().trim_end_matches(['.', ' ']);
    let clean = if clean.is_empty() { "download" } else { clean };
    // A bounded, UTF-8-safe basename leaves room for collision suffixes.
    let mut end = clean.len().min(200);
    while !clean.is_char_boundary(end) { end -= 1; }
    let clean = &clean[..end];
    let p = Path::new(clean);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("download");
    let ext = p.extension().and_then(|s| s.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    for i in 0..1000 {
        let candidate = directory.join(if i == 0 { clean.to_string() } else { format!("{stem} ({i}){ext}") });
        // Conservative case folding also protects case-insensitive volumes.
        let folded = destination_key(&candidate);
        if DESTINATIONS.with_borrow(|paths| paths.values().any(|path| destination_key(path) == folded)) { continue; }
        match std::fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Err(error) => return Err(error),
            Ok(_) => {}
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "Too many matching filenames. Choose another folder or rename existing files."))
}

#[cfg(test)]
#[path = "download_controls_tests.rs"]
mod control_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!("crossbow-download-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&root).unwrap();
        root.canonicalize().unwrap()
    }

    #[test]
    fn active_includes_pending_requests_but_excludes_every_terminal_state() {
        let pending = DownloadInfo::default();
        assert!(pending.active());
        assert!(DownloadInfo { in_progress: true, ..pending.clone() }.active());
        for terminal in [
            DownloadInfo { complete: true, ..pending.clone() },
            DownloadInfo { canceled: true, ..pending.clone() },
            DownloadInfo { interrupted: true, ..pending.clone() },
            DownloadInfo { error: Some("Engine failure".into()), ..pending.clone() },
            DownloadInfo { destination_error: Some("Destination rejected".into()), ..pending },
        ] {
            assert!(!terminal.active(), "{terminal:?}");
            assert!(!DownloadInfo { in_progress: true, ..terminal }.active());
        }
    }

    #[test]
    fn pending_destinations_stay_reserved_through_interruption_and_release_by_owner() {
        let root = fixture();
        let a = DownloadInfo { profile_id: "reservation-a".into(), id: 1, ..Default::default() };
        let b = DownloadInfo { profile_id: "reservation-b".into(), id: 1, ..Default::default() };
        DESTINATIONS.with_borrow_mut(|paths| {
            paths.insert((a.profile_id.clone(), a.id), root.join("same.txt"));
            paths.insert((b.profile_id.clone(), b.id), root.join("same (1).txt"));
        });
        observe(&DownloadInfo { interrupted: true, ..a.clone() });
        assert_eq!(destination(&root, "SAME.txt").unwrap(), root.join("SAME (2).txt"));
        observe(&DownloadInfo { canceled: true, ..a });
        assert_eq!(destination(&root, "same.txt").unwrap(), root.join("same.txt"));
        observe(&DownloadInfo { complete: true, ..b });
        assert_eq!(destination(&root, "same (1).txt").unwrap(), root.join("same (1).txt"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_pending_names_compare_canonical_unicode_equivalents() {
        assert_eq!(destination_key(Path::new("/synthetic/CAFÉ.txt")), destination_key(Path::new("/synthetic/cafe\u{301}.txt")));
    }

    #[test]
    #[cfg(unix)]
    fn destination_uses_the_canonical_selected_folder() {
        let root = fixture();
        let selected = root.join("selected");
        std::fs::create_dir(&selected).unwrap();
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&selected, &alias).unwrap();
        assert_eq!(destination(&alias, "résumé.txt").unwrap(), selected.join("résumé.txt"));
        assert!(std::fs::read_dir(&selected).unwrap().next().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn destinations_are_bounded_children_and_do_not_create_missing_folders() {
        let root = fixture();
        for name in ["", ".", "..", "../outside.txt", "/absolute.txt", "a\\b:c\n.txt", "é.txt"] {
            let path = destination(&root, name).unwrap();
            assert_eq!(path.parent(), Some(root.as_path()));
            assert!(!path.file_name().unwrap().is_empty());
            assert!(!path.exists());
        }
        assert_eq!(destination(&root, "..").unwrap(), root.join("download"));
        assert!(destination(Path::new("relative"), "test").is_err());
        assert!(destination(&root.join("missing"), "test").is_err());
        assert!(!root.join("missing").exists());
        std::fs::write(root.join("file"), "keep").unwrap();
        assert!(destination(&root.join("file"), "test").is_err());
        let long = "é".repeat(200);
        assert!(destination(&root, &long).unwrap().file_name().unwrap().len() <= 200);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collision_exhaustion_fails_instead_of_reusing_an_existing_destination() {
        let root = fixture();
        std::fs::write(root.join("test.txt"), "keep").unwrap();
        assert_eq!(destination(&root, "test.txt").unwrap(), root.join("test (1).txt"));
        for i in 1..1000 { std::fs::write(root.join(format!("test ({i}).txt")), "keep").unwrap(); }
        assert_eq!(destination(&root, "test.txt").unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(root.join("test.txt")).unwrap(), "keep");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn dangling_symlink_is_not_an_available_download_destination() {
        let root = fixture();
        std::os::unix::fs::symlink(root.join("absent"), root.join("test.txt")).unwrap();
        assert_eq!(destination(&root, "test.txt").unwrap(), root.join("test (1).txt"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
