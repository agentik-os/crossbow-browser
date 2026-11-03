//! Tidy Downloads: an opt-in file-name suggestion for each finished download.
//! Only the file name, the source host and the declared type go to the saved
//! loopback model, never the contents. Nothing changes until the user accepts:
//! the rename stays in the same folder, keeps the extension, never replaces a
//! file and is refused if the file changed or moved since it finished. Revert
//! restores the original name while it is free. Suggestions are session-only,
//! like the downloads list; the setting and the name on disk persist.

use crate::ai_features::{Pending, Provider};
use crossbow_core::library::Library;
use crossbow_engine::DownloadInfo;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

pub const FEATURE: &str = "tidy_downloads";
pub const NAME: &str = "Tidy Downloads";
pub const MAX_NAME_CHARS: usize = 80;
/// Names tried before giving up: "Name.pdf", "Name 2.pdf" … "Name 20.pdf".
const UNIQUE_ATTEMPTS: u32 = 20;
const BUSY_RETRY: Duration = Duration::from_secs(1);
const CHANGED: &str = "The file changed or moved since it finished downloading. Nothing was renamed.";
const UNUSABLE: &str = "The suggestion was not a usable file name.";

pub fn prompt(file_name: &str, host: Option<&str>, mime: &str) -> String {
    let mime = Some(mime).filter(|mime| !mime.is_empty() && mime.len() <= 100 && mime.chars().all(|c| c.is_ascii_graphic() || c == ' '));
    format!(
        "Suggest a short, clear name for a downloaded file. Reply with the name only, on one line, without the extension, a folder, quotes or formatting.\n\nCurrent name: {file_name}\nSite: {}\nType: {}",
        host.unwrap_or("unknown"), mime.unwrap_or("unknown"),
    )
}

/// Splits "name.ext", keeping ".tar.gz" whole. A leading dot, or a suffix
/// with spaces or symbols, is part of the name rather than an extension.
fn split(name: &str) -> (&str, &str) {
    let dot = name.rfind('.').filter(|&at| {
        at > 0 && (2..=11).contains(&(name.len() - at)) && name[at + 1..].chars().all(|c| c.is_ascii_alphanumeric())
    });
    let Some(dot) = dot else { return (name, "") };
    match name[..dot].rfind('.').filter(|&at| at > 0 && name[at..dot].eq_ignore_ascii_case(".tar")) {
        Some(tar) => name.split_at(tar),
        None => name.split_at(dot),
    }
}

/// A file name that stays in its folder: no separators, `..`, leading or
/// trailing dot, control or invisible characters, and at most 255 bytes.
fn check_name(name: &str) -> Result<(), &'static str> {
    let usable = !name.is_empty() && name.len() <= 255 && !name.starts_with('.') && !name.ends_with('.') && !name.contains("..")
        && !name.contains(['/', '\\', ':']) && !name.chars().any(|c| c.is_control() || crate::ai_features::invisible(c));
    if usable { Ok(()) } else { Err(UNUSABLE) }
}

/// The suggested name with the original extension, or why it is unusable.
pub fn parse(answer: &str, original: &str) -> Result<String, &'static str> {
    let line = crate::ai_features::one_line(answer, MAX_NAME_CHARS).map_err(|_| UNUSABLE)?;
    let (_, extension) = split(original);
    // A repeated extension is dropped; the original one is always kept.
    let at = line.len().saturating_sub(extension.len());
    let stem = match line.as_bytes().get(at..) {
        Some(tail) if !extension.is_empty() && at > 0 && tail.eq_ignore_ascii_case(extension.as_bytes()) => &line[..at],
        _ => line.as_str(),
    };
    let stem = stem.trim_end();
    check_name(stem)?;
    let name = format!("{stem}{extension}");
    check_name(&name)?;
    if name == original { return Err("The suggestion was the current name."); }
    Ok(name)
}

/// What identifies the finished file: a different file, size or modification
/// time at the same path means it changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    dev: u64,
    ino: u64,
    len: u64,
    modified: Option<SystemTime>,
}

impl Fingerprint {
    pub fn of(path: &Path) -> Result<Self, String> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(path).map_err(|_| CHANGED.to_string())?;
        if !meta.file_type().is_file() { return Err(CHANGED.into()); }
        Ok(Self { dev: meta.dev(), ino: meta.ino(), len: meta.len(), modified: meta.modified().ok() })
    }
}

/// Renames `from` to `name`, or the first free "name N", in the same folder.
pub fn rename(from: &Path, name: &str, expected: Fingerprint) -> Result<PathBuf, String> {
    check_name(name).map_err(|_| "That name cannot be used in the download folder. Nothing was renamed.".to_string())?;
    let folder = from.parent().ok_or(CHANGED)?;
    if Fingerprint::of(from)? != expected { return Err(CHANGED.into()); }
    let (stem, extension) = split(name);
    for attempt in 1..=UNIQUE_ATTEMPTS {
        let to = folder.join(if attempt == 1 { name.to_owned() } else { format!("{stem} {attempt}{extension}") });
        match rename_exclusive(from, &to) {
            Ok(()) => {
                // A file swapped in after the check is put back, not kept renamed.
                if Fingerprint::of(&to).ok() != Some(expected) {
                    let _ = rename_exclusive(&to, from);
                    return Err(CHANGED.into());
                }
                return Ok(to);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(CHANGED.into()),
            Err(error) => return Err(format!("Could not rename the file: {error}. Nothing was renamed.")),
        }
    }
    Err("Every similar name is taken in this folder. Nothing was renamed.".into())
}

/// Restores `original` if that name is free and the file is unchanged.
pub fn revert(current: &Path, original: &Path, expected: Fingerprint) -> Result<(), String> {
    const MOVED: &str = "The file changed or moved since it was renamed. Nothing was changed.";
    if current.parent() != original.parent() || Fingerprint::of(current).ok() != Some(expected) { return Err(MOVED.into()); }
    match rename_exclusive(current, original) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err("Another file now uses the original name. Nothing was changed.".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(MOVED.into()),
        Err(error) => Err(format!("Could not restore the name: {error}. Nothing was changed.")),
    }
}

/// One atomic rename that fails rather than replace an existing file.
#[cfg(target_os = "macos")]
fn rename_exclusive(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let from = std::ffi::CString::new(from.as_os_str().as_bytes())?;
    let to = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both arguments are NUL-terminated paths that outlive the call.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

/// Without RENAME_EXCL a hard link never replaces a file, but two steps are not atomic.
#[cfg(not(target_os = "macos"))]
fn rename_exclusive(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::hard_link(from, to)?;
    std::fs::remove_file(from).inspect_err(|_| { let _ = std::fs::remove_file(to); })
}

pub enum State {
    Suggesting(Box<dyn Pending>),
    /// Waiting for the shared request worker.
    Queued,
    Offered(String),
    Renamed { original: PathBuf, error: Option<String> },
    Notice { text: String, retry: bool },
}

pub struct Entry {
    pub profile: String,
    pub id: u32,
    /// Where the file is now.
    pub path: PathBuf,
    name: String,
    fingerprint: Fingerprint,
    host: Option<String>,
    mime: String,
    pub state: State,
}

impl Entry {
    pub fn file_name(&self) -> String {
        self.path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

#[cfg(test)]
impl Entry {
    /// An entry already in `state`, for drawing tests.
    pub(crate) fn fixture(profile: &str, id: u32, path: &str, state: State) -> Self {
        let path = PathBuf::from(path);
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let fingerprint = Fingerprint { dev: 0, ino: 0, len: 0, modified: None };
        Self { profile: profile.into(), id, path, name, fingerprint, host: None, mime: String::new(), state }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command { Accept, Dismiss, Revert, Cancel, Retry }

#[derive(Default)]
pub struct Tidy {
    entries: Vec<Entry>,
    retry_at: Option<Instant>,
}

impl Tidy {
    pub fn entry(&self, profile: &str, id: u32) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.profile == profile && entry.id == id)
    }

    /// Keeps the downloads card open while a suggestion is pending or offered.
    pub fn waiting(&self, profile: &str) -> bool {
        self.entries.iter().any(|entry| entry.profile == profile && matches!(entry.state, State::Suggesting(_) | State::Queued | State::Offered(_)))
    }

    /// A newly finished download. Off, private, already seen or without a
    /// file on disk: nothing is shown or sent.
    pub fn completed(&mut self, info: &DownloadInfo, lib: &Library, private: bool, provider: &dyn Provider, ctx: &egui::Context) {
        if private || !info.complete || info.path.is_empty() || self.entry(&info.profile_id, info.id).is_some() { return; }
        if !matches!(crate::ai_features::consented(lib, &info.profile_id, FEATURE), Ok(Some(_))) { return; }
        let path = PathBuf::from(&info.path);
        let Some(name) = path.file_name().and_then(|name| name.to_str()).map(str::to_owned) else { return };
        let Ok(fingerprint) = Fingerprint::of(&path) else { return };
        let host = (info.url.starts_with("http://") || info.url.starts_with("https://")).then(|| crossbow_core::search::host_of(&info.url)).flatten();
        let mut entry = Entry { profile: info.profile_id.clone(), id: info.id, path, name, fingerprint, host, mime: info.mime_type.clone(), state: State::Queued };
        if !self.suggesting() { entry.state = request(&entry, lib, provider, ctx); }
        self.entries.push(entry);
    }

    fn suggesting(&self) -> bool {
        self.entries.iter().any(|entry| matches!(entry.state, State::Suggesting(_)))
    }

    pub fn poll(&mut self, lib: &Library, provider: &dyn Provider, ctx: &egui::Context, now: Instant) {
        for entry in &mut self.entries {
            let State::Suggesting(task) = &entry.state else { continue };
            let Some(result) = task.poll() else { continue };
            let name = result.map_err(|error| error.to_string()).and_then(|answer| parse(&answer, &entry.name).map_err(str::to_owned));
            entry.state = match name {
                Ok(name) => State::Offered(name),
                Err(error) => State::Notice { text: format!("{error} The name is unchanged."), retry: true },
            };
        }
        if self.suggesting() || self.retry_at.is_some_and(|at| now < at) { return; }
        if let Some(entry) = self.entries.iter_mut().find(|entry| matches!(entry.state, State::Queued)) {
            entry.state = request(entry, lib, provider, ctx);
            self.retry_at = matches!(entry.state, State::Queued).then(|| now + BUSY_RETRY);
            if self.retry_at.is_some() { ctx.request_repaint_after(BUSY_RETRY); }
        }
    }

    pub fn command(&mut self, profile: &str, id: u32, command: Command, lib: &Library, provider: &dyn Provider, ctx: &egui::Context) {
        let suggesting = self.suggesting();
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.profile == profile && entry.id == id) else { return };
        match (command, &entry.state) {
            (Command::Accept, State::Offered(name)) => {
                entry.state = match rename(&entry.path, name, entry.fingerprint) {
                    Ok(to) => State::Renamed { original: std::mem::replace(&mut entry.path, to), error: None },
                    Err(text) => State::Notice { text, retry: false },
                };
            }
            (Command::Dismiss, State::Offered(_)) => entry.state = State::Notice { text: "Kept the original name.".into(), retry: false },
            (Command::Revert, State::Renamed { original, .. }) => {
                let original = original.clone();
                entry.state = match revert(&entry.path, &original, entry.fingerprint) {
                    Ok(()) => {
                        entry.path = original;
                        State::Notice { text: "Restored the original name.".into(), retry: false }
                    }
                    Err(error) => State::Renamed { original, error: Some(error) },
                };
            }
            (Command::Cancel, State::Suggesting(_) | State::Queued) => {
                if let State::Suggesting(task) = &entry.state { task.cancel(); }
                entry.state = State::Notice { text: "Canceled. The name is unchanged.".into(), retry: true };
            }
            (Command::Retry, State::Notice { retry: true, .. }) => {
                entry.state = if suggesting { State::Queued } else { request(entry, lib, provider, ctx) };
            }
            _ => {}
        }
    }
}

/// Asks for a name if the feature is still allowed; a busy worker queues it.
fn request(entry: &Entry, lib: &Library, provider: &dyn Provider, ctx: &egui::Context) -> State {
    let ready = match crate::ai_features::allowed(lib, &entry.profile, FEATURE, NAME) {
        Ok(ready) => ready,
        Err(text) => return State::Notice { text, retry: true },
    };
    match provider.start(&ready, prompt(&entry.name, entry.host.as_deref(), &entry.mime), ctx) {
        Ok(task) => State::Suggesting(task),
        Err(crossbow_ai::local::LocalError::Busy) => State::Queued,
        Err(error) => State::Notice { text: format!("{error} The name is unchanged."), retry: true },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_features::tests::{Fake, Script, configured};
    use crossbow_ai::local::{LocalEndpoint, LocalError};
    use crossbow_core::store::Store;
    use std::fs;

    fn folder() -> tempfile::TempDir { tempfile::tempdir_in(std::env::temp_dir()).unwrap() }

    fn file(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    fn finished(profile: &str, id: u32, path: &Path) -> DownloadInfo {
        DownloadInfo {
            id, profile_id: profile.into(), url: "https://files.example.test/get?id=SECRET_QUERY".into(),
            path: path.to_string_lossy().into_owned(), file_name: path.file_name().unwrap().to_string_lossy().into_owned(),
            received: 12, total: 12, percent: 100, complete: true, mime_type: "application/pdf".into(), ..Default::default()
        }
    }

    fn enabled(store: Store) -> (Library, String) {
        let (lib, _, profile) = configured(store, LocalEndpoint { port: 41_200, ..Default::default() });
        let ready = crate::ai_features::ready(&lib, &profile).unwrap();
        crate::ai_features::enable(&lib, &profile, FEATURE, &ready).unwrap();
        (lib, profile)
    }

    fn notice(tidy: &Tidy, profile: &str, id: u32) -> String {
        match &tidy.entry(profile, id).expect("a row").state { State::Notice { text, .. } => text.clone(), _ => panic!("expected a notice") }
    }

    #[test]
    fn suggestions_are_strict_single_names_that_keep_the_extension() {
        assert_eq!(parse("Quarterly Report 2026", "q3_rpt_final(1).pdf"), Ok("Quarterly Report 2026.pdf".into()));
        assert_eq!(parse("Quarterly Report.PDF", "q3.pdf"), Ok("Quarterly Report.pdf".into()), "a repeated extension is not doubled");
        assert_eq!(parse("\"Site backup\"", "bk-0193.tar.gz"), Ok("Site backup.tar.gz".into()));
        assert_eq!(parse("Project notes", "README"), Ok("Project notes".into()));
        assert_eq!(parse("Release v1.2 notes", "rel.txt"), Ok("Release v1.2 notes.txt".into()));
        assert_eq!(parse("Invoice", "scan 3.final draft"), Ok("Invoice".into()), "a long or spaced suffix is no extension");
        let long = "n".repeat(MAX_NAME_CHARS + 1);
        for bad in ["", "../etc/passwd", "a/b", "a\\b", "a:b", ".hidden", "..", "name.", "two\nlines", "tab\tname", "**Report**", "`x`", "evil\u{202E}fdp.exe", long.as_str()] {
            assert!(parse(bad, "file.pdf").is_err(), "{bad:?} must be refused");
        }
        assert!(parse("file", "file.pdf").is_err(), "the same name is no suggestion");
    }

    #[test]
    fn the_prompt_names_only_the_file_name_host_and_type() {
        assert_eq!(
            prompt("q3_rpt(1).pdf", Some("files.example.test"), "application/pdf"),
            "Suggest a short, clear name for a downloaded file. Reply with the name only, on one line, without the extension, a folder, quotes or formatting.\n\nCurrent name: q3_rpt(1).pdf\nSite: files.example.test\nType: application/pdf"
        );
        let unknown = prompt("x.bin", None, "text/\u{7}weird");
        assert!(unknown.ends_with("Site: unknown\nType: unknown"), "{unknown}");
    }

    #[test]
    fn rename_stays_in_the_folder_keeps_contents_and_never_replaces_a_file() {
        let dir = folder();
        let original = file(dir.path(), "q3_rpt.pdf", "download");
        file(dir.path(), "Report.pdf", "someone else's");
        file(dir.path(), "Report 2.pdf", "another");
        let print = Fingerprint::of(&original).unwrap();
        let renamed = rename(&original, "Report.pdf", print).unwrap();
        assert_eq!(renamed, dir.path().join("Report 3.pdf"));
        assert_eq!(names(dir.path()), ["Report 2.pdf", "Report 3.pdf", "Report.pdf"]);
        assert_eq!(fs::read_to_string(&renamed).unwrap(), "download");
        assert_eq!(fs::read_to_string(dir.path().join("Report.pdf")).unwrap(), "someone else's");
        assert_eq!(Fingerprint::of(&renamed).unwrap(), print, "the same file, unchanged");
        // Names that leave the folder are refused before anything moves.
        for name in ["../escape.pdf", "sub/inner.pdf", "..", ".hidden.pdf", "", "nul\0.pdf", "ctl\u{1}.pdf"] {
            assert!(rename(&renamed, name, print).is_err(), "{name:?}");
        }
        assert_eq!(names(dir.path()), ["Report 2.pdf", "Report 3.pdf", "Report.pdf"]);
        assert!(!dir.path().parent().unwrap().join("escape.pdf").exists());
    }

    #[test]
    fn rename_is_refused_when_the_file_changed_or_moved_since_it_finished() {
        let dir = folder();
        for change in ["contents", "time", "replaced", "moved", "folder"] {
            let path = file(dir.path(), "fixture.txt", "first");
            let print = Fingerprint::of(&path).unwrap();
            match change {
                "contents" => fs::write(&path, "changed contents").unwrap(),
                "time" => fs::File::options().write(true).open(&path).unwrap().set_modified(SystemTime::UNIX_EPOCH).unwrap(),
                "replaced" => { fs::remove_file(&path).unwrap(); fs::write(&path, "other").unwrap(); }
                "moved" => fs::rename(&path, dir.path().join("elsewhere.txt")).unwrap(),
                _ => { fs::remove_file(&path).unwrap(); fs::create_dir(&path).unwrap(); }
            }
            let before = names(dir.path());
            let error = rename(&path, "Tidy.txt", print).unwrap_err();
            assert!(error.contains("changed or moved") && error.contains("Nothing was renamed"), "{change}: {error}");
            assert_eq!(names(dir.path()), before, "{change} renames nothing");
            for name in names(dir.path()) { let _ = fs::remove_file(dir.path().join(&name)).or_else(|_| fs::remove_dir(dir.path().join(&name))); }
        }
    }

    #[test]
    fn revert_restores_the_original_name_only_while_it_is_free() {
        let dir = folder();
        let original = file(dir.path(), "q3.pdf", "download");
        let print = Fingerprint::of(&original).unwrap();
        let renamed = rename(&original, "Report.pdf", print).unwrap();
        revert(&renamed, &original, print).unwrap();
        assert_eq!(names(dir.path()), ["q3.pdf"]);
        let renamed = rename(&original, "Report.pdf", print).unwrap();
        file(dir.path(), "q3.pdf", "a new file took the name");
        let error = revert(&renamed, &original, print).unwrap_err();
        assert!(error.contains("original name") && error.contains("Nothing was changed"), "{error}");
        assert_eq!(names(dir.path()), ["Report.pdf", "q3.pdf"]);
        assert_eq!(fs::read_to_string(&original).unwrap(), "a new file took the name");
        fs::remove_file(&original).unwrap();
        fs::write(&renamed, "edited after the rename").unwrap();
        assert!(revert(&renamed, &original, print).unwrap_err().contains("changed"));
        assert_eq!(names(dir.path()), ["Report.pdf"]);
    }

    #[test]
    fn nothing_is_sent_when_off_private_without_provider_or_without_a_file() {
        let ctx = egui::Context::default();
        let dir = folder();
        let path = file(dir.path(), "q3.pdf", "x");
        let fake = Fake::default();
        let (lib, _, profile) = configured(Store::open_in_memory().unwrap(), LocalEndpoint::default());
        let mut tidy = Tidy::default();
        tidy.completed(&finished(&profile, 1, &path), &lib, false, &fake, &ctx);
        assert!(tidy.entry(&profile, 1).is_none(), "off: no suggestion row");
        let (lib, profile) = enabled(Store::open_in_memory().unwrap());
        tidy.completed(&finished(&profile, 2, &path), &lib, true, &fake, &ctx);
        assert!(tidy.entry(&profile, 2).is_none(), "private profiles never send");
        tidy.completed(&DownloadInfo { complete: false, ..finished(&profile, 3, &path) }, &lib, false, &fake, &ctx);
        tidy.completed(&finished(&profile, 4, &dir.path().join("missing.pdf")), &lib, false, &fake, &ctx);
        tidy.completed(&DownloadInfo { path: String::new(), ..finished(&profile, 5, &path) }, &lib, false, &fake, &ctx);
        assert!(tidy.entry(&profile, 3).is_none() && tidy.entry(&profile, 4).is_none() && tidy.entry(&profile, 5).is_none());
        lib.store.kv_delete(&format!("ai.providers.v1.{profile}")).unwrap();
        tidy.completed(&finished(&profile, 6, &path), &lib, false, &fake, &ctx);
        assert!(notice(&tidy, &profile, 6).contains("Save Selection"), "on without a provider says why");
        tidy.command(&profile, 6, Command::Retry, &lib, &fake, &ctx);
        assert!(notice(&tidy, &profile, 6).contains("Save Selection"));
        assert_eq!(fake.count(), 0, "zero provider calls");
        assert_eq!(names(dir.path()), ["q3.pdf"]);
    }

    #[test]
    fn a_finished_download_offers_a_name_that_accept_applies_and_revert_undoes() {
        let ctx = egui::Context::default();
        let dir = folder();
        let path = file(dir.path(), "q3_rpt(1).pdf", "FILE_CONTENTS_MARKER");
        let (lib, profile) = enabled(Store::open_in_memory().unwrap());
        let fake = Fake::with([Script::Answer(Ok("Quarterly Report".into()))]);
        let mut tidy = Tidy::default();
        let info = finished(&profile, 9, &path);
        tidy.completed(&info, &lib, false, &fake, &ctx);
        tidy.completed(&info, &lib, false, &fake, &ctx);
        assert_eq!(fake.count(), 1, "one request per download");
        let sent = fake.prompt(0);
        assert!(sent.ends_with("Current name: q3_rpt(1).pdf\nSite: files.example.test\nType: application/pdf"), "{sent}");
        assert!(!sent.contains("SECRET_QUERY") && !sent.contains("FILE_CONTENTS_MARKER") && !sent.contains(&dir.path().to_string_lossy().into_owned()), "no address, contents or folder");
        assert!(tidy.waiting(&profile));
        tidy.poll(&lib, &fake, &ctx, Instant::now());
        assert!(matches!(&tidy.entry(&profile, 9).unwrap().state, State::Offered(name) if name == "Quarterly Report.pdf"));
        assert_eq!(names(dir.path()), ["q3_rpt(1).pdf"], "nothing changes before Accept");
        tidy.command(&profile, 9, Command::Accept, &lib, &fake, &ctx);
        let entry = tidy.entry(&profile, 9).unwrap();
        assert!(matches!(&entry.state, State::Renamed { original, error: None } if *original == path));
        assert_eq!(entry.file_name(), "Quarterly Report.pdf");
        assert_eq!(names(dir.path()), ["Quarterly Report.pdf"]);
        assert!(!tidy.waiting(&profile));
        tidy.command(&profile, 9, Command::Revert, &lib, &fake, &ctx);
        assert_eq!(names(dir.path()), ["q3_rpt(1).pdf"]);
        assert!(notice(&tidy, &profile, 9).contains("Restored"));
        assert_eq!(tidy.entry(&profile, 9).unwrap().path, path);

        // Dismiss keeps the name and says so.
        let other = file(dir.path(), "b.txt", "x");
        fake.then(Script::Answer(Ok("Notes".into())));
        tidy.completed(&finished(&profile, 10, &other), &lib, false, &fake, &ctx);
        tidy.poll(&lib, &fake, &ctx, Instant::now());
        tidy.command(&profile, 10, Command::Dismiss, &lib, &fake, &ctx);
        assert!(notice(&tidy, &profile, 10).contains("Kept the original name"));
        assert_eq!(names(dir.path()), ["b.txt", "q3_rpt(1).pdf"]);
    }

    #[test]
    fn cancel_timeout_errors_bad_answers_and_disk_errors_change_nothing_and_say_so() {
        let ctx = egui::Context::default();
        let dir = folder();
        let (lib, profile) = enabled(Store::open_in_memory().unwrap());
        let mut tidy = Tidy::default();
        let fake = Fake::with([
            Script::Hold,
            Script::Answer(Err(LocalError::Timeout)),
            Script::Answer(Ok("**Bold** name".into())),
            Script::Answer(Ok("Moved away".into())),
        ]);
        let first = file(dir.path(), "one.txt", "1");
        tidy.completed(&finished(&profile, 1, &first), &lib, false, &fake, &ctx);
        let held = fake.task();
        tidy.command(&profile, 1, Command::Cancel, &lib, &fake, &ctx);
        assert!(held.cancelled.get());
        assert!(notice(&tidy, &profile, 1).contains("Canceled"));
        held.finish(Ok("Late".into()));
        tidy.poll(&lib, &fake, &ctx, Instant::now());
        assert!(notice(&tidy, &profile, 1).contains("Canceled"), "a late answer is ignored");

        tidy.command(&profile, 1, Command::Retry, &lib, &fake, &ctx);
        tidy.poll(&lib, &fake, &ctx, Instant::now());
        assert!(notice(&tidy, &profile, 1).contains("time limit"));
        tidy.command(&profile, 1, Command::Retry, &lib, &fake, &ctx);
        tidy.poll(&lib, &fake, &ctx, Instant::now());
        assert!(notice(&tidy, &profile, 1).contains("not a usable file name"));
        tidy.command(&profile, 1, Command::Retry, &lib, &fake, &ctx);
        tidy.poll(&lib, &fake, &ctx, Instant::now());
        fs::rename(&first, dir.path().join("user-moved.txt")).unwrap();
        tidy.command(&profile, 1, Command::Accept, &lib, &fake, &ctx);
        assert!(notice(&tidy, &profile, 1).contains("changed or moved"));
        assert_eq!(names(dir.path()), ["user-moved.txt"]);
        assert_eq!(fake.count(), 4);
    }

    #[test]
    fn a_busy_worker_queues_suggestions_and_retries_later() {
        let ctx = egui::Context::default();
        let dir = folder();
        let (lib, profile) = enabled(Store::open_in_memory().unwrap());
        let mut tidy = Tidy::default();
        let fake = Fake::with([Script::Refuse(LocalError::Busy), Script::Hold]);
        let a = file(dir.path(), "a.txt", "a");
        let b = file(dir.path(), "b.txt", "b");
        let now = Instant::now();
        tidy.completed(&finished(&profile, 1, &a), &lib, false, &fake, &ctx);
        assert!(matches!(tidy.entry(&profile, 1).unwrap().state, State::Queued), "another request holds the worker");
        tidy.poll(&lib, &fake, &ctx, now);
        assert!(matches!(tidy.entry(&profile, 1).unwrap().state, State::Suggesting(_)), "retried once the worker may be free");
        tidy.completed(&finished(&profile, 2, &b), &lib, false, &fake, &ctx);
        assert!(matches!(tidy.entry(&profile, 2).unwrap().state, State::Queued), "one suggestion at a time");
        assert_eq!(fake.count(), 1);
        tidy.command(&profile, 2, Command::Cancel, &lib, &fake, &ctx);
        assert!(notice(&tidy, &profile, 2).contains("Canceled"));
    }

    #[test]
    fn the_setting_and_the_accepted_name_survive_reopening() {
        let ctx = egui::Context::default();
        let dir = folder();
        let root = folder();
        let db = root.path().join("tidy.sqlite");
        let path = file(dir.path(), "dl.pdf", "x");
        let (lib, profile) = enabled(Store::open(&db).unwrap());
        let fake = Fake::with([Script::Answer(Ok("Tidy name".into()))]);
        let mut tidy = Tidy::default();
        tidy.completed(&finished(&profile, 1, &path), &lib, false, &fake, &ctx);
        tidy.poll(&lib, &fake, &ctx, Instant::now());
        tidy.command(&profile, 1, Command::Accept, &lib, &fake, &ctx);
        drop((tidy, lib));
        let lib = Library::open(Store::open(&db).unwrap()).unwrap();
        assert!(crate::ai_features::allowed(&lib, &profile, FEATURE, NAME).is_ok(), "still on after reopening");
        assert_eq!(names(dir.path()), ["Tidy name.pdf"]);
        let conn = rusqlite::Connection::open(&db).unwrap();
        let stored: i64 = conn.query_row("SELECT count(*) FROM kv WHERE v LIKE '%dl.pdf%' OR v LIKE '%Tidy name%'", [], |row| row.get(0)).unwrap();
        assert_eq!(stored, 0, "file names are not stored");
    }
}
