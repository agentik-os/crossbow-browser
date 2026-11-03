//! Local library backups and Restore Data.
//!
//! A backup is a checked copy of the library database in the Backups folder
//! beside it, named for its UTC time and reason. The app makes one at launch
//! once a day and just before operations that rewrite much of the library.
//! Backups stay on this Mac: nothing here syncs or uploads. Saved passwords
//! (their own vault file and key), website data (the Chromium folders) and
//! downloads are not in the library, so they are not backed up.
//!
//! A restore backs up the current library, then puts a checked copy of the
//! chosen backup beside it. The next launch swaps that copy in before the
//! library opens. It keeps this Mac's sync session, clock and encryption
//! state, so a backup never brings back a retired key or a stale cursor,
//! turns sync off, and gives the restored rows fresh clocks so they replace
//! the synced copies when sync is turned on again.
//!
//! Site permissions and links to other apps are this Mac's too: a restore
//! keeps the current library's decisions as they are and never takes them
//! from the backup, so a grant revoked since the backup stays revoked.

use crate::library::{KV_LAST_HLC, KV_NODE, SYNC_CURSOR_KEY};
use crate::store::{LIBRARY_FORMAT, SITE_PERMISSIONS_TABLE, Store};
use crate::sync::deleted::{self, DELETING_KEY, ENCRYPT_FIRST_KEY, SYNC_EPOCH_KEY, SYNC_OFF_KEY, SyncOff};
use crate::sync::{BASE_URL_KEY, SESSION_KEY};
use crate::sync::e2ee::{self, MARKER_KIND};
use crossbow_proto::Hlc;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The folder beside the library that holds its backups.
pub const FOLDER: &str = "Backups";
const PREFIX: &str = "library-";
const EXTENSION: &str = ".sqlite";
/// A backup or restore copy still being written: never listed or restored.
const PARTIAL: &str = ".partial";
/// The newest failed backups, for the backups list: this Mac's, never a backup's.
const FAILURES_KEY: &str = "backup_failures";
const KEEP_FAILURES: usize = 10;
/// Retention: the newest 7 daily backups, the newest daily backup of each of
/// the last 4 weeks, the newest 10 made before an operation, and 500 MB in all.
const KEEP_DAILY: usize = 7;
const KEEP_WEEKLY: usize = 4;
const KEEP_BEFORE: usize = 10;
pub const SIZE_CAP: u64 = 500 * 1024 * 1024;
const DAY_MS: u64 = 86_400_000;
/// The restore record: in the restore copy, then in the restored library.
pub const RESTORED_KEY: &str = "library_restored";

/// Backups, retention and restores run one at a time in this process.
static LOCK: Mutex<()> = Mutex::new(());

/// Why a backup was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    Daily,
    Upgrade,
    Restore,
    DeleteSyncData,
    TurnSyncOn,
    TurnOnEncryption,
    ResetEncryption,
    TidyToday,
}

impl Reason {
    const ALL: [Reason; 8] = [Reason::Daily, Reason::Upgrade, Reason::Restore, Reason::DeleteSyncData, Reason::TurnSyncOn, Reason::TurnOnEncryption, Reason::ResetEncryption, Reason::TidyToday];

    /// The reason in a backup's file name.
    pub fn slug(self) -> &'static str {
        match self {
            Reason::Daily => "daily",
            Reason::Upgrade => "before-upgrade",
            Reason::Restore => "before-restore",
            Reason::DeleteSyncData => "before-delete-sync-data",
            Reason::TurnSyncOn => "before-turn-sync-on",
            Reason::TurnOnEncryption => "before-turn-on-encryption",
            Reason::ResetEncryption => "before-reset-encryption",
            Reason::TidyToday => "before-tidy-today",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Reason::Daily => "Daily",
            Reason::Upgrade => "Before an update",
            Reason::Restore => "Before a restore",
            Reason::DeleteSyncData => "Before Delete Sync Data",
            Reason::TurnSyncOn => "Before Turn Sync On",
            Reason::TurnOnEncryption => "Before turning on encryption",
            Reason::ResetEncryption => "Before resetting encryption",
            Reason::TidyToday => "Before Tidy Today",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Reason> {
        Reason::ALL.into_iter().find(|reason| reason.slug() == slug)
    }
}

/// One backup file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backup {
    pub path: PathBuf,
    /// When it was made, to the second (UTC, from its name).
    pub created_ms: u64,
    pub reason: Reason,
    pub bytes: u64,
    /// Orders backups made in the same second.
    seq: u32,
}

/// A backup that failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub at_ms: u64,
    /// A [`Reason::slug`].
    pub reason: String,
    pub error: String,
}

impl Failure {
    pub fn reason(&self) -> Option<Reason> {
        Reason::from_slug(&self.reason)
    }
}

/// What a backup holds, counted from a read-only open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Preview {
    pub spaces: u64,
    pub profiles: u64,
    pub pins: u64,
    pub folders: u64,
    pub tabs: u64,
    pub archived: u64,
}

/// Whether a backup can be restored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    Ready(Preview),
    /// Made by a newer Crossbow, in a format this one can't promise to read.
    Newer,
    /// Fails its integrity check or can't be read.
    Damaged,
}

/// Why a restore didn't start. In every case nothing changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoreError {
    /// The current library couldn't be backed up first.
    Backup(String),
    /// The chosen backup can't be restored.
    Refused(Check),
    /// The copy for the next launch couldn't be prepared.
    Stage(String),
    /// The restored rows can't get clocks above every clock either library
    /// holds, so they couldn't replace their synced copies: [`CLOCK_LIMIT`].
    Clock,
}

/// Why a restore was refused when its rows can't get newer clocks.
pub const CLOCK_LIMIT: &str = "This backup can't be restored: this library or the backup has an item synced with the latest time Crossbow can record, so restored items couldn't replace it. Nothing was changed.";

/// A restore, recorded in the copy that waits for the next launch and kept in
/// the restored library for its notice. Never synced.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Restored {
    /// When the restored backup was made.
    pub backup_ms: u64,
    /// The backup's [`Reason::slug`].
    pub reason: String,
    /// The account the backup's library was encrypted for, or waited for
    /// encryption for: its uploads wait for encryption after the restore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_for: Option<String>,
    /// When the next launch put it in place.
    #[serde(default)]
    pub applied_ms: u64,
    /// The launch notice was shown.
    #[serde(default)]
    pub shown: bool,
    /// Profiles whose website data wasn't on this Mac when it was restored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fresh_profiles: Vec<String>,
}

/// Where a test stops a backup or restore, as a crash would.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// The backup's temporary file is written, not yet checked.
    BackupWritten,
    /// The temporary file is checked and synced, not yet renamed.
    BackupChecked,
    /// The restore copy is written, not yet renamed into place.
    Staged,
    /// The current library's sync state is read and its log folded in.
    CurrentRead,
    /// Inside the prepare transaction, before it commits.
    Restamped,
    /// The restore copy is prepared, not yet swapped in.
    Prepared,
    /// The restore copy replaced the library; cleanup hasn't run.
    Swapped,
}

#[cfg(test)]
thread_local! {
    static CRASH_AT: std::cell::Cell<Option<Step>> = const { std::cell::Cell::new(None) };
    static CRASHED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Stop here as a crash would: no cleanup and no failure record follow.
fn crash(step: Step) -> Result<(), String> {
    #[cfg(test)]
    if CRASH_AT.get() == Some(step) {
        CRASHED.set(true);
        return Err(format!("simulated crash at {step:?}"));
    }
    let _ = step;
    Ok(())
}

fn crashed() -> bool {
    #[cfg(test)]
    return CRASHED.get();
    #[cfg(not(test))]
    false
}

fn locked() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The Backups folder for the library at `library`.
pub fn folder(library: &Path) -> PathBuf {
    library.parent().unwrap_or(Path::new(".")).join(FOLDER)
}

/// The checked copy that replaces the library at the next launch.
fn staged_path(library: &Path) -> PathBuf {
    suffixed(library, ".restore")
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

// -- calendar ----------------------------------------------------------------

/// Calendar fields of an instant at a fixed offset from UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Calendar {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// `ms` since 1970 as calendar fields, `offset_secs` east of UTC.
pub fn calendar(ms: u64, offset_secs: i64) -> Calendar {
    let secs = (ms / 1000) as i64 + offset_secs;
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    Calendar { year, month, day, hour: (rest / 3600) as u32, minute: (rest % 3600 / 60) as u32, second: (rest % 60) as u32 }
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `20261029T101500Z`.
fn stamp(ms: u64) -> String {
    let c = calendar(ms, 0);
    format!("{:04}{:02}{:02}T{:02}{:02}{:02}Z", c.year, c.month, c.day, c.hour, c.minute, c.second)
}

fn parse_stamp(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() != 16 || b[8] != b'T' || b[15] != b'Z' { return None; }
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = &text[range];
        part.bytes().all(|c| c.is_ascii_digit()).then(|| part.parse().ok()).flatten()
    };
    let (year, month, day) = (digits(0..4)?, digits(4..6)?, digits(6..8)?);
    let (hour, minute, second) = (digits(9..11)?, digits(11..13)?, digits(13..15)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 59 { return None; }
    let days = days_from_civil(i64::from(year), month, day);
    let secs = days * 86_400 + i64::from(hour * 3600 + minute * 60 + second);
    // A day past the end of its month reads as another date: refused.
    u64::try_from(secs).ok().map(|secs| secs * 1000).filter(|ms| stamp(*ms) == text)
}

// -- names -------------------------------------------------------------------

fn file_name(created_ms: u64, seq: u32, reason: Reason) -> String {
    let seq = if seq == 0 { String::new() } else { format!("-{seq}") };
    format!("{PREFIX}{}-{}{seq}{EXTENSION}", stamp(created_ms), reason.slug())
}

fn parse_name(name: &str) -> Option<(u64, u32, Reason)> {
    let stem = name.strip_prefix(PREFIX)?.strip_suffix(EXTENSION)?;
    let (stamp, rest) = stem.split_at_checked(16)?;
    let created_ms = parse_stamp(stamp)?;
    let rest = rest.strip_prefix('-')?;
    let (slug, seq) = match rest.rsplit_once('-') {
        Some((slug, seq)) if !seq.is_empty() && seq.bytes().all(|c| c.is_ascii_digit()) => (slug, seq.parse().ok()?),
        _ => (rest, 0),
    };
    Some((created_ms, seq, Reason::from_slug(slug)?))
}

// -- files -------------------------------------------------------------------

/// The Backups folder, created 0700 if missing. A link or a file in its place
/// is refused, so a backup never leaves the app's data folder.
fn prepare_folder(library: &Path) -> Result<PathBuf, String> {
    let folder = folder(library);
    match fs::symlink_metadata(&folder) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err("The Backups folder isn't a folder.".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder.create(&folder).map_err(|error| format!("Couldn't create the Backups folder ({error})."))?;
        }
        Err(error) => return Err(format!("Couldn't read the Backups folder ({error}).")),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::metadata(&folder).map_err(|error| format!("Couldn't read the Backups folder ({error})."))?;
        if meta.permissions().mode() & 0o777 != 0o700 {
            fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).map_err(|error| format!("Couldn't protect the Backups folder ({error})."))?;
        }
    }
    Ok(folder)
}

/// A new empty file only this user can read.
fn private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

fn sync_file(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

/// Best effort: some file systems can't sync a directory.
fn sync_folder(path: &Path) {
    let _ = fs::File::open(path).and_then(|folder| folder.sync_all());
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// A file and the journal, log and index SQLite may keep beside it. False
/// if any of them stays.
fn remove_with_sidecars(path: &Path) -> bool {
    let mut gone = true;
    for suffix in ["", "-journal", "-wal", "-shm"] {
        gone &= remove_if_present(&suffixed(path, suffix)).is_ok();
    }
    gone
}

/// Leftovers of a backup that stopped before its rename, with their
/// journals. Returns how many of those copies stay, in whole or in part.
fn remove_partials(folder: &Path) -> usize {
    let Ok(entries) = fs::read_dir(folder) else { return 0 };
    let mut left = std::collections::BTreeSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| name.starts_with(PREFIX)) else { continue };
        if let Some(at) = name.find(PARTIAL) {
            if remove_if_present(&entry.path()).is_err() { left.insert(name[..at].to_string()); }
        }
    }
    left.len()
}

/// `text` without this Mac's folder names, for logs and Settings: SQLite's
/// errors name the file they couldn't open. A path in the data folder
/// becomes "the Crossbow data folder", any other in the home folder "your
/// home folder".
pub fn without_paths(text: &str, library: &Path) -> String {
    let data = library.parent().unwrap_or(Path::new("."));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut roots: Vec<(String, &str)> = [(Some(data.to_path_buf()), "the Crossbow data folder"), (data.canonicalize().ok(), "the Crossbow data folder")]
        .into_iter()
        .chain(home.iter().flat_map(|home| [(Some(home.clone()), "your home folder"), (home.canonicalize().ok(), "your home folder")]))
        .filter_map(|(path, name)| Some((path?.display().to_string(), name)))
        .filter(|(root, _)| root.len() > 1)
        .collect();
    // Longest first, so the data folder is named even inside the home folder.
    roots.sort_by_key(|(root, _)| std::cmp::Reverse(root.len()));
    let mut text = text.to_string();
    for (root, name) in &roots {
        while let Some(start) = text.find(root.as_str()) {
            // The rest of the path, up to the punctuation around it.
            let rest = start + root.len();
            let end = text[rest..].find(|c: char| c.is_whitespace() || matches!(c, ')' | '"' | '\'' | ',')).map_or(text.len(), |n| rest + n);
            let end = if end > rest && text[..end].ends_with('.') { end - 1 } else { end };
            let (start, name) = match text[..start].strip_suffix(": ") {
                Some(before) => (before.len(), format!(" in {name}")),
                None => (start, (*name).to_string()),
            };
            text.replace_range(start..end, &name);
        }
    }
    text
}

fn intact(conn: &Connection) -> rusqlite::Result<bool> {
    let mut statement = conn.prepare("PRAGMA integrity_check")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows == ["ok"])
}

fn kv(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT v FROM kv WHERE k = ?1", [key], |row| row.get(0)).optional()
}

/// Leave the rollback journal as the file's mode, so the copy is one
/// self-contained file, and check it.
fn seal(path: &Path) -> Result<(), String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(|error| error.to_string())?;
    let mode: String = conn.query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0)).map_err(|error| error.to_string())?;
    if !mode.eq_ignore_ascii_case("delete") { return Err("the copy kept a write-ahead log".into()); }
    if !intact(&conn).map_err(|error| error.to_string())? { return Err("the copy failed its integrity check".into()); }
    conn.close().map_err(|(_, error)| error.to_string())?;
    sync_file(path).map_err(|error| error.to_string())
}

// -- backups -----------------------------------------------------------------

/// The library's backups, newest first. Unfinished and unknown files are left out.
pub fn list(library: &Path) -> Vec<Backup> {
    let Ok(entries) = fs::read_dir(folder(library)) else { return Vec::new() };
    let mut backups: Vec<Backup> = entries.flatten().filter_map(|entry| {
        let (created_ms, seq, reason) = parse_name(entry.file_name().to_str()?)?;
        let meta = entry.metadata().ok().filter(fs::Metadata::is_file)?;
        Some(Backup { path: entry.path(), created_ms, reason, bytes: meta.len(), seq })
    }).collect();
    backups.sort_by(|a, b| (b.created_ms, b.seq).cmp(&(a.created_ms, a.seq)));
    backups
}

/// The newest failed backups, newest first.
pub fn failures(store: &Store) -> Vec<Failure> {
    store.kv_get(FAILURES_KEY).ok().flatten().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

/// Note a failed backup in the library itself, which is usually still
/// writable when the Backups folder isn't. Never names a path.
fn record_failure(conn: &Connection, reason: Reason, error: &str, now: u64) {
    log::warn!("Library backup failed ({}): {error}", reason.label());
    let mut all: Vec<Failure> = kv(conn, FAILURES_KEY).ok().flatten().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
    all.insert(0, Failure { at_ms: now, reason: reason.slug().into(), error: error.into() });
    all.truncate(KEEP_FAILURES);
    let Ok(text) = serde_json::to_string(&all) else { return };
    if let Err(error) = conn.execute("INSERT INTO kv (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v", params![FAILURES_KEY, text]) {
        log::warn!("Couldn't note the failed backup: {error}");
    }
}

/// Whether the launch backup is due: no backup yet, or the newest is a day old.
pub fn daily_due(library: &Path, now: u64) -> bool {
    list(library).first().is_none_or(|newest| now.saturating_sub(newest.created_ms) >= DAY_MS)
}

/// Back up the library `store` holds. `Ok(None)` for a library with no file.
pub fn create(store: &Store, reason: Reason) -> Result<Option<Backup>, String> {
    let Some(library) = store.path() else { return Ok(None) };
    let _lock = locked();
    write_backup(store.connection(), &library, reason, crate::now_ms()).map(Some)
}

/// Back up before `reason`'s operation, which goes ahead either way: a
/// failure is recorded for the backups list and returned.
pub fn before(store: &Store, reason: Reason) -> Option<String> {
    create(store, reason).err()
}

/// The launch backup, from its own connection so it can run on another
/// thread. `Ok(None)` when none is due or there is no library yet.
pub fn daily(library: &Path, now: u64) -> Result<Option<Backup>, String> {
    let _lock = locked();
    if !library.exists() || !daily_due(library, now) { return Ok(None); }
    let conn = open_existing(library).map_err(|error| {
        let error = without_paths(&format!("Couldn't open the library ({error})."), library);
        log::warn!("Library backup failed ({}): {error}", Reason::Daily.label());
        error
    })?;
    write_backup(&conn, library, Reason::Daily, now).map(Some)
}

/// Back up a library the next `Store::open` would upgrade, before it does.
pub fn before_upgrade(library: &Path) -> Result<Option<Backup>, String> {
    let _lock = locked();
    if !library.exists() { return Ok(None); }
    let result = open_existing(library).and_then(|conn| {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        Ok((version < LIBRARY_FORMAT).then_some(conn))
    });
    match result {
        Ok(Some(conn)) => write_backup(&conn, library, Reason::Upgrade, crate::now_ms()).map(Some),
        Ok(None) => Ok(None),
        Err(error) => {
            let error = without_paths(&format!("Couldn't read the library ({error})."), library);
            log::warn!("Library backup failed ({}): {error}", Reason::Upgrade.label());
            Err(error)
        }
    }
}

fn open_existing(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

/// Write a backup of `conn`'s database: a temporary file in the Backups
/// folder, synced and checked, then renamed. Retention runs after a success.
fn write_backup(conn: &Connection, library: &Path, reason: Reason, now: u64) -> Result<Backup, String> {
    let folder = prepare_folder(library).map_err(|error| without_paths(&error, library)).inspect_err(|error| record_failure(conn, reason, error, now))?;
    remove_partials(&folder);
    // Backups made in the same second, for any reason, are numbered in order.
    let second = now - now % 1000;
    let mut seq = list(library).iter().filter(|backup| backup.created_ms == second).map(|backup| backup.seq + 1).max().unwrap_or(0);
    let path = loop {
        let path = folder.join(file_name(now, seq, reason));
        if !path.exists() { break path; }
        seq += 1;
    };
    let partial = suffixed(&path, PARTIAL);
    let written = (|| -> Result<(), String> {
        drop(private_file(&partial).map_err(|error| format!("Couldn't create the backup file ({error})."))?);
        let target = partial.to_str().ok_or("The data folder's path isn't valid text.")?;
        // A consistent snapshot, even while the app writes; the file was made
        // empty and private above, and VACUUM INTO fills an empty file.
        conn.execute("VACUUM INTO ?1", [target]).map_err(|error| format!("Couldn't copy the library ({error})."))?;
        crash(Step::BackupWritten)?;
        seal(&partial).map_err(|error| format!("The backup didn't check out ({error})."))?;
        crash(Step::BackupChecked)?;
        fs::rename(&partial, &path).map_err(|error| format!("Couldn't finish the backup file ({error})."))?;
        sync_folder(&folder);
        Ok(())
    })();
    if let Err(error) = written {
        let error = without_paths(&error, library);
        if !crashed() {
            remove_with_sidecars(&partial);
            record_failure(conn, reason, &error, now);
        }
        return Err(error);
    }
    let bytes = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    prune(library, SIZE_CAP);
    Ok(Backup { path, created_ms: second, reason, bytes, seq })
}

/// Remove backups the retention policy no longer keeps.
fn prune(library: &Path, cap: u64) {
    for backup in expired(&list(library), cap) {
        let _ = remove_if_present(&backup.path);
    }
}

/// Backups (newest first) outside the retention policy: beyond the newest 7
/// daily ones and the newest daily one of each of the last 4 weeks, beyond
/// the newest 10 made before an operation, then the oldest kept ones while
/// all of them pass `cap`. The newest backup is always kept.
fn expired(backups: &[Backup], cap: u64) -> Vec<Backup> {
    let mut keep = vec![false; backups.len()];
    let (mut daily, mut before, mut weeks) = (0, 0, Vec::new());
    for (i, backup) in backups.iter().enumerate() {
        if backup.reason == Reason::Daily {
            // Weeks start on Monday; 1970-01-01 was a Thursday.
            let week = (backup.created_ms / DAY_MS + 3) / 7;
            if daily < KEEP_DAILY { keep[i] = true; }
            if !weeks.contains(&week) && weeks.len() < KEEP_WEEKLY {
                weeks.push(week);
                keep[i] = true;
            }
            daily += 1;
        } else {
            keep[i] = before < KEEP_BEFORE;
            before += 1;
        }
    }
    let mut total: u64 = backups.iter().zip(&keep).filter(|(_, kept)| **kept).map(|(backup, _)| backup.bytes).sum();
    for i in (1..backups.len()).rev() {
        if total <= cap { break; }
        if keep[i] {
            keep[i] = false;
            total -= backups[i].bytes;
        }
    }
    backups.iter().zip(keep).filter(|(_, kept)| !kept).map(|(backup, _)| backup.clone()).collect()
}

// -- forgetting --------------------------------------------------------------

/// What the library just removed, which its backups lose too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forget<'a> {
    /// Clear History: this profile's history.
    History(&'a str),
    /// A deleted profile: its history, archive, site permissions and sites
    /// with blocking off.
    Profile(&'a str),
}

/// Backups removed by [`forget`] or [`delete_all`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Removed {
    /// Deleted: by `forget`, those it couldn't clean.
    pub deleted: usize,
    /// Couldn't be deleted either, so they still hold what they held.
    pub left: usize,
}

/// Remove `what` from every backup of the library at `library`, and from a
/// restore waiting for the next launch. Each is rewritten as a backup is
/// written: a private copy beside it, changed with its freed pages wiped,
/// checked and synced, then renamed over it. One that can't be cleaned is
/// deleted. Copies a crash left unfinished, in Backups and beside the
/// library, are deleted; one that stays counts as left.
pub fn forget(library: &Path, what: Forget) -> Removed {
    let _lock = locked();
    let mut removed = Removed::default();
    let folder = folder(library);
    removed.left += remove_partials(&folder);
    sync_folder(&folder);
    let staged = staged_path(library);
    if !remove_with_sidecars(&suffixed(&staged, PARTIAL)) { removed.left += 1; }
    sync_folder(library.parent().unwrap_or(Path::new(".")));
    let copies = list(library).into_iter().map(|backup| backup.path).chain(staged.exists().then_some(staged));
    for path in copies {
        if let Err(error) = scrub(&path, what) {
            log::warn!("Couldn't clean a library backup, so it is deleted: {}", without_paths(&error, library));
            match remove_if_present(&path) {
                Ok(()) => {
                    remove_with_sidecars(&path);
                    removed.deleted += 1;
                }
                Err(_) => removed.left += 1,
            }
        }
    }
    removed
}

/// Rewrite the backup at `path` without `what`, through a private copy
/// beside it that replaces it only once checked and synced.
fn scrub(path: &Path, what: Forget) -> Result<(), String> {
    let partial = suffixed(path, PARTIAL);
    remove_with_sidecars(&partial);
    let result = (|| -> Result<(), String> {
        let mut source = fs::File::open(path).map_err(|error| error.to_string())?;
        let mut target = private_file(&partial).map_err(|error| error.to_string())?;
        io::copy(&mut source, &mut target).map_err(|error| error.to_string())?;
        target.sync_all().map_err(|error| error.to_string())?;
        drop(target);
        let conn = open_existing(&partial).map_err(|error| error.to_string())?;
        if !intact(&conn).map_err(|error| error.to_string())? { return Err("the backup failed its integrity check".into()); }
        (|| -> rusqlite::Result<()> {
            // Freed pages are zeroed, then dropped from the file.
            conn.execute_batch("PRAGMA secure_delete = ON")?;
            let (profile, whole) = match what {
                Forget::History(profile) => (profile, false),
                Forget::Profile(profile) => (profile, true),
            };
            // A backup from before profiles or site permissions has no such rows.
            if has_column(&conn, "profile_history", "profile_id")? {
                conn.execute("DELETE FROM profile_history WHERE profile_id = ?1", [profile])?;
            }
            if whole && has_column(&conn, "archive", "profile_id")? {
                conn.execute("DELETE FROM archive WHERE profile_id = ?1", [profile])?;
            }
            if whole && has_column(&conn, "site_permissions", "profile_id")? {
                conn.execute("DELETE FROM site_permissions WHERE profile_id = ?1", [profile])?;
            }
            if whole && has_column(&conn, "kv", "k")? {
                conn.execute("DELETE FROM kv WHERE k = ?1", [crate::content_blocking::exceptions::key(profile)])?;
            }
            conn.execute_batch("VACUUM")
        })().map_err(|error| error.to_string())?;
        conn.close().map_err(|(_, error)| error.to_string())?;
        seal(&partial)?;
        fs::rename(&partial, path).map_err(|error| error.to_string())?;
        sync_folder(path.parent().unwrap_or(Path::new(".")));
        Ok(())
    })();
    if result.is_err() { remove_with_sidecars(&partial); }
    result
}

fn has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2", [table, column], |row| row.get::<_, i64>(0)).map(|n| n > 0)
}

/// Delete every backup of the library at `library`, and the restore waiting
/// for the next launch, if one waits.
pub fn delete_all(library: &Path) -> Removed {
    let _lock = locked();
    let mut removed = Removed::default();
    for backup in list(library) {
        match remove_if_present(&backup.path) {
            Ok(()) => {
                remove_with_sidecars(&backup.path);
                removed.deleted += 1;
            }
            Err(_) => removed.left += 1,
        }
    }
    let folder = folder(library);
    remove_partials(&folder);
    sync_folder(&folder);
    let staged = staged_path(library);
    remove_with_sidecars(&suffixed(&staged, PARTIAL));
    if remove_if_present(&staged).is_err() { removed.left += 1; }
    remove_with_sidecars(&staged);
    removed
}

// -- preview -----------------------------------------------------------------

/// Whether `path` can be restored, and what it holds. The file is opened
/// read-only and nothing is written beside it.
pub fn inspect(path: &Path) -> Check {
    read_backup(path).unwrap_or(Check::Damaged)
}

fn read_backup(path: &Path) -> rusqlite::Result<Check> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    if !intact(&conn)? { return Ok(Check::Damaged); }
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > LIBRARY_FORMAT { return Ok(Check::Newer); }
    // A restore must know which account an encrypted backup belongs to.
    if kv(&conn, e2ee::FLAG_KEY)?.is_some_and(|text| e2ee::record_account(&text).is_none()) { return Ok(Check::Damaged); }
    let count = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0)).map(|n| n.max(0) as u64);
    Ok(Check::Ready(Preview {
        spaces: count("SELECT COUNT(*) FROM entities WHERE kind = 'space' AND deleted = 0")?,
        profiles: count("SELECT COUNT(*) FROM entities WHERE kind = 'profile' AND deleted = 0")?,
        pins: count("SELECT COUNT(*) FROM entities WHERE kind = 'item' AND deleted = 0 AND json_valid(payload) AND json_extract(payload, '$.kind') = 'pinned'")?,
        folders: count("SELECT COUNT(*) FROM entities WHERE kind = 'folder' AND deleted = 0")?,
        tabs: count("SELECT COUNT(*) FROM tabs")?,
        archived: count("SELECT COUNT(*) FROM archive")?,
    }))
}

// -- restore -----------------------------------------------------------------

/// Restore `backup` at the next launch. The chosen backup is checked and
/// copied beside the library, the current library is backed up, then the copy
/// is put in place for the next launch. If any step fails, nothing changed.
pub fn restore(store: &Store, backup: &Backup) -> Result<(), RestoreError> {
    let library = store.path().ok_or_else(|| RestoreError::Stage("This library isn't saved in a file.".into()))?;
    let _lock = locked();
    match inspect(&backup.path) {
        Check::Ready(_) => {}
        refused => return Err(RestoreError::Refused(refused)),
    }
    match clocks_fit(store.connection(), &backup.path, crate::now_ms()) {
        Ok(true) => {}
        Ok(false) => return Err(RestoreError::Clock),
        Err(_) => return Err(RestoreError::Refused(Check::Damaged)),
    }
    let staged = staged_path(&library);
    let partial = suffixed(&staged, PARTIAL);
    remove_with_sidecars(&partial);
    // Copy first, so retention after the backup below can't remove the chosen one.
    if let Err(error) = copy_for_restore(backup, &partial) {
        if !crashed() { remove_with_sidecars(&partial); }
        return Err(RestoreError::Stage(without_paths(&error, &library)));
    }
    if let Err(error) = write_backup(store.connection(), &library, Reason::Restore, crate::now_ms()) {
        if !crashed() { remove_with_sidecars(&partial); }
        return Err(RestoreError::Backup(error));
    }
    let placed = crash(Step::Staged).and_then(|()| {
        fs::rename(&partial, &staged).map_err(|error| format!("Couldn't prepare the restore ({error})."))?;
        sync_folder(library.parent().unwrap_or(Path::new(".")));
        Ok(())
    });
    if let Err(error) = placed {
        if !crashed() { remove_with_sidecars(&partial); }
        return Err(RestoreError::Stage(without_paths(&error, &library)));
    }
    Ok(())
}

/// Copy `backup` to `partial`, private and synced, checked again, with the
/// restore record that the next launch reads.
fn copy_for_restore(backup: &Backup, partial: &Path) -> Result<(), String> {
    let mut source = fs::File::open(&backup.path).map_err(|error| format!("Couldn't read the backup ({error})."))?;
    let mut target = private_file(partial).map_err(|error| format!("Couldn't create the restore copy ({error})."))?;
    io::copy(&mut source, &mut target).map_err(|error| format!("Couldn't copy the backup ({error})."))?;
    target.sync_all().map_err(|error| format!("Couldn't save the restore copy ({error})."))?;
    drop(target);
    let conn = open_existing(partial).map_err(|error| format!("Couldn't open the restore copy ({error})."))?;
    let record = (|| -> rusqlite::Result<Option<Restored>> {
        if !intact(&conn)? { return Ok(None); }
        // What this backup's library was encrypted for, before the next
        // launch replaces its encryption state with this Mac's.
        let flag = kv(&conn, e2ee::FLAG_KEY)?.and_then(|text| e2ee::record_account(&text));
        let encrypted_for = flag.or(kv(&conn, ENCRYPT_FIRST_KEY)?.map(|text| deleted::hold_account(&text)));
        Ok(Some(Restored { backup_ms: backup.created_ms, reason: backup.reason.slug().into(), encrypted_for, ..Restored::default() }))
    })().map_err(|error| format!("Couldn't read the restore copy ({error})."))?;
    let record = record.ok_or("The backup failed its integrity check.")?;
    let text = serde_json::to_string(&record).map_err(|_| "Couldn't record the restore.".to_string())?;
    conn.execute("INSERT INTO kv (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v", params![RESTORED_KEY, text])
        .map_err(|error| format!("Couldn't record the restore ({error})."))?;
    conn.close().map_err(|(_, error)| format!("Couldn't save the restore copy ({error})."))?;
    sync_file(partial).map_err(|error| format!("Couldn't save the restore copy ({error})."))
}

/// When the backup waiting for the next launch was made, if one waits; 0
/// if its record can't be read.
pub fn pending(library: &Path) -> Option<u64> {
    let staged = staged_path(library);
    if !staged.exists() { return None; }
    let read = || -> rusqlite::Result<Option<u64>> {
        let conn = Connection::open_with_flags(&staged, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        Ok(kv(&conn, RESTORED_KEY)?.and_then(|text| serde_json::from_str::<Restored>(&text).ok()).map(|record| record.backup_ms))
    };
    Some(read().ok().flatten().unwrap_or(0))
}

/// Drop the restore waiting for the next launch.
pub fn cancel_pending(library: &Path) -> Result<(), String> {
    let _lock = locked();
    let staged = staged_path(library);
    remove_with_sidecars(&suffixed(&staged, PARTIAL));
    remove_if_present(&staged).map_err(|error| without_paths(&format!("Couldn't cancel the restore ({error})."), library))?;
    remove_with_sidecars(&staged);
    Ok(())
}

/// This Mac's sync and encryption state, read from the library a restore
/// replaces: the backup's could name a retired key, an old session or a
/// stale cursor.
#[derive(Default)]
struct Device {
    /// Session, server, clock node, encryption record and notice, verbatim.
    kv: Vec<(&'static str, String)>,
    /// The encryption marker rows: `id, hlc, deleted, payload`.
    markers: Vec<(String, String, i64, String)>,
    /// The newest clock this library issued or stored.
    newest: Option<Hlc>,
    session_account: Option<String>,
    encrypted: bool,
    hold: Option<String>,
    /// Site permissions and links to other apps, every row as stored.
    permissions: Vec<PermissionRow>,
}

/// A `site_permissions` row: profile, origin, top origin, permission, decision, updated.
type PermissionRow = (String, String, String, String, String, i64);

/// The kv keys a restore keeps from this Mac's library.
const DEVICE_KEYS: [&str; 6] = [SESSION_KEY, BASE_URL_KEY, KV_NODE, e2ee::FLAG_KEY, e2ee::NOTICE_KEY, FAILURES_KEY];
/// The kv keys a restore never takes from a backup, including a delete the
/// backup was taken during.
const SYNC_KEYS: [&str; 7] = [KV_LAST_HLC, SYNC_CURSOR_KEY, SYNC_EPOCH_KEY, SYNC_OFF_KEY, ENCRYPT_FIRST_KEY, DELETING_KEY, RESTORED_KEY];

/// The newest clock in the library's own clock and its rows, of those sync
/// can send: text that doesn't parse is ignored, even where it sorts last.
fn newest_clock(conn: &Connection) -> rusqlite::Result<Option<Hlc>> {
    let stored: Option<String> = kv(conn, KV_LAST_HLC)?;
    let mut statement = conn.prepare("SELECT DISTINCT hlc FROM entities")?;
    let rows = statement.query_map([], |row| row.get::<_, Option<String>>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(stored.into_iter().chain(rows.into_iter().flatten()).filter_map(|text| Hlc::parse(&text).ok()).max())
}

/// The rows a restore gives fresh clocks: every live row but the markers.
fn live_rows(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    conn.prepare("SELECT kind, id FROM entities WHERE deleted = 0 AND kind != ?1 ORDER BY kind, id")?
        .query_map([MARKER_KIND], |row| Ok((row.get(0)?, row.get(1)?)))?.collect()
}

/// Fresh clocks for `count` restored rows, each above `newest` and the one
/// before it: the clock's own tick, or the next clock up where that tick
/// would pass what sync can send. None when the clocks run out.
fn restamps(newest: Option<&Hlc>, count: usize, now: u64, node: &str) -> Option<Vec<Hlc>> {
    let mut clock = newest.cloned();
    (0..count).map(|_| {
        let ticked = Hlc::tick(clock.as_ref(), now, node);
        let next = if Hlc::parse(&ticked.encode()).is_ok() { ticked } else { crate::library::successor(clock.as_ref()?, node)? };
        clock = Some(next.clone());
        Some(next)
    }).collect()
}

/// Whether every row `backup` would restore can get a clock above the
/// newest clock in it and in the library `live`.
fn clocks_fit(live: &Connection, backup: &Path, now: u64) -> rusqlite::Result<bool> {
    let conn = Connection::open_with_flags(backup, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    let newest = [newest_clock(live)?, newest_clock(&conn)?].into_iter().flatten().max();
    let node = kv(live, KV_NODE)?.unwrap_or_else(|| "restore".into());
    Ok(restamps(newest.as_ref(), live_rows(&conn)?.len(), now, &node).is_some())
}

impl Device {
    /// Read this Mac's state, then fold the library's write-ahead log into
    /// it and stop using one, so the swap can't leave the old log beside
    /// the restored file.
    fn read(library: &Path) -> Result<Device, String> {
        if !library.exists() { return Ok(Device::default()); }
        let conn = open_existing(library).map_err(|error| format!("Couldn't open the current library ({error})."))?;
        let device = (|| -> rusqlite::Result<Device> {
            let mut device = Device::default();
            for key in DEVICE_KEYS {
                if let Some(value) = kv(&conn, key)? { device.kv.push((key, value)); }
            }
            let mut statement = conn.prepare("SELECT id, hlc, deleted, payload FROM entities WHERE kind = ?1")?;
            device.markers = statement.query_map([MARKER_KIND], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
            device.newest = newest_clock(&conn)?;
            device.session_account = kv(&conn, SESSION_KEY)?
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|session| session["account_id"].as_str().map(str::to_owned));
            device.encrypted = kv(&conn, e2ee::FLAG_KEY)?.is_some();
            device.hold = kv(&conn, ENCRYPT_FIRST_KEY)?;
            // A library no build with site permissions has opened has none.
            let table: Option<i64> = conn.query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'site_permissions'", [], |row| row.get(0)).optional()?;
            if table.is_some() {
                let mut statement = conn.prepare("SELECT profile_id, origin, top_origin, permission, decision, updated_ms FROM site_permissions")?;
                device.permissions = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))?.collect::<rusqlite::Result<_>>()?;
            }
            Ok(device)
        })().map_err(|error| format!("Couldn't read the current library ({error})."))?;
        let mode: String = conn.query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0)).map_err(|error| format!("Couldn't prepare the current library ({error})."))?;
        conn.close().map_err(|(_, error)| format!("Couldn't close the current library ({error})."))?;
        if !mode.eq_ignore_ascii_case("delete") || suffixed(library, "-wal").exists() {
            return Err("The current library still has a write-ahead log.".into());
        }
        Ok(device)
    }
}

/// Put the waiting restore in place, before the library opens at launch.
/// `Ok(false)` when none waits. Any failure leaves the library as it was:
/// a damaged copy is dropped, anything else is tried again next launch.
pub fn finish_restore(library: &Path) -> Result<bool, String> {
    let _lock = locked();
    let staged = staged_path(library);
    remove_with_sidecars(&suffixed(&staged, PARTIAL));
    if !staged.exists() { return Ok(false); }
    let result = (|| -> Result<(), Failed> {
        // Opening rolls back a prepare a crash interrupted.
        let conn = open_existing(&staged).map_err(|_| Failed::Damaged)?;
        if !intact(&conn).unwrap_or(false) { return Err(Failed::Damaged); }
        let record = kv(&conn, RESTORED_KEY).ok().flatten().and_then(|text| serde_json::from_str::<Restored>(&text).ok()).ok_or(Failed::Damaged)?;
        let device = Device::read(library).map_err(Failed::Other)?;
        crash(Step::CurrentRead).map_err(Failed::Other)?;
        prepare(&conn, &device, record, crate::now_ms())?;
        crash(Step::Prepared).map_err(Failed::Other)?;
        conn.close().map_err(|(_, error)| Failed::Other(format!("Couldn't save the restored library ({error}).")))?;
        sync_file(&staged).map_err(|error| Failed::Other(format!("Couldn't save the restored library ({error}).")))?;
        fs::rename(&staged, library).map_err(|error| Failed::Other(format!("Couldn't put the restored library in place ({error}).")))?;
        sync_folder(library.parent().unwrap_or(Path::new(".")));
        crash(Step::Swapped).map_err(Failed::Other)?;
        // The old library's index of its log; the log itself is gone.
        let _ = remove_if_present(&suffixed(library, "-shm"));
        Ok(())
    })();
    match result {
        Ok(()) => Ok(true),
        Err(Failed::Damaged) => {
            let _ = remove_if_present(&staged);
            remove_with_sidecars(&staged);
            Err("The copy of the backup was damaged, so your library wasn't changed. Choose the backup in Restore Data again.".into())
        }
        // Clocks only grow, so trying again at the next launch can't help.
        Err(Failed::Clock) => {
            let _ = remove_if_present(&staged);
            remove_with_sidecars(&staged);
            Err(CLOCK_LIMIT.into())
        }
        Err(Failed::Other(error)) => Err(without_paths(&format!("{error} Your library wasn't changed; Crossbow tries again when it next opens."), library)),
    }
}

enum Failed {
    Damaged,
    /// The restored rows can't get clocks above the library's.
    Clock,
    Other(String),
}

/// In one transaction on the restore copy: this Mac's sync and encryption
/// state, and its site permissions and links to other apps, replace the
/// backup's, the restored rows get fresh clocks above every clock either
/// library holds, nothing waits to upload, and sync stays off for a
/// signed-in account until the user turns it on.
fn prepare(conn: &Connection, device: &Device, mut record: Restored, now: u64) -> Result<(), Failed> {
    let failed = |error: String| Failed::Other(format!("Couldn't prepare the restored library ({error})."));
    let sql = |error: rusqlite::Error| failed(error.to_string());
    // A library encrypted when it was backed up waits for encryption again,
    // for every account, unless this Mac's own encryption record already
    // keeps it encrypted. The hold is new: an account the library was let
    // through to unencrypted before the restore waits again too.
    let hold = match &record.encrypted_for {
        Some(account) if !device.encrypted => Some(deleted::hold_text(account)),
        _ => device.hold.clone(),
    };
    let off = device.session_account.as_ref()
        .map(|account| serde_json::to_string(&SyncOff { account: account.clone(), here: true, encrypted: hold.as_deref().is_some_and(|text| deleted::holds(text, account)), restored: true, unreadable: false }))
        .transpose().map_err(|_| failed("Couldn't encode the sync state.".into()))?;
    record.applied_ms = now;
    record.shown = false;
    record.fresh_profiles.clear();
    let record = serde_json::to_string(&record).map_err(|_| failed("Couldn't encode the restore record.".into()))?;
    let node = match device.kv.iter().find(|(key, _)| *key == KV_NODE) {
        Some((_, node)) => node.clone(),
        None => kv(conn, KV_NODE).map_err(sql)?.unwrap_or_else(|| crate::new_id().chars().filter(char::is_ascii_alphanumeric).rev().take(10).collect()),
    };
    // Every clock is made before anything changes: if they run out, the
    // restore is refused as it stands.
    let newest = [device.newest.clone(), newest_clock(conn).map_err(sql)?].into_iter().flatten().max();
    let live = live_rows(conn).map_err(sql)?;
    let clocks = restamps(newest.as_ref(), live.len(), now, &node).ok_or(Failed::Clock)?;
    let transaction = conn.unchecked_transaction().map_err(sql)?;
    (|| -> rusqlite::Result<()> {
        for key in DEVICE_KEYS.iter().chain(&SYNC_KEYS) {
            conn.execute("DELETE FROM kv WHERE k = ?1", [key])?;
        }
        let set = |key: &str, value: &str| conn.execute("INSERT INTO kv (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v", params![key, value]);
        for (key, value) in &device.kv { set(key, value)?; }
        set(KV_NODE, &node)?;
        conn.execute("DELETE FROM entities WHERE kind = ?1", [MARKER_KIND])?;
        for (id, hlc, deleted, payload) in &device.markers {
            conn.execute("INSERT INTO entities (kind, id, hlc, deleted, payload, dirty) VALUES (?1, ?2, ?3, ?4, ?5, 0)", params![MARKER_KIND, id, hlc, deleted, payload])?;
        }
        // The restore counts as an edit made now: restored rows replace the
        // synced copies they meet, and later edits anywhere win as usual.
        let mut restamp = conn.prepare_cached("UPDATE entities SET hlc = ?1 WHERE kind = ?2 AND id = ?3")?;
        for ((kind, id), hlc) in live.iter().zip(&clocks) {
            restamp.execute(params![hlc.encode(), kind, id])?;
        }
        conn.execute("UPDATE entities SET dirty = 0", [])?;
        conn.execute("DELETE FROM sync_rejections", [])?;
        // This Mac's decisions, never the backup's: a grant revoked since stays revoked.
        conn.execute_batch(SITE_PERMISSIONS_TABLE)?;
        conn.execute("DELETE FROM site_permissions", [])?;
        let mut keep = conn.prepare_cached("INSERT INTO site_permissions (profile_id, origin, top_origin, permission, decision, updated_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
        for (profile, origin, top_origin, permission, decision, updated_ms) in &device.permissions {
            keep.execute(params![profile, origin, top_origin, permission, decision, updated_ms])?;
        }
        if let Some(clock) = clocks.last().or(newest.as_ref()) { set(KV_LAST_HLC, &clock.encode())?; }
        if let Some(hold) = &hold { set(ENCRYPT_FIRST_KEY, hold)?; }
        if let Some(off) = &off { set(SYNC_OFF_KEY, off)?; }
        set(RESTORED_KEY, &record)?;
        Ok(())
    })().map_err(sql)?;
    // Dropping the transaction here rolls it back, as reopening after a crash would.
    crash(Step::Restamped).map_err(failed)?;
    transaction.commit().map_err(sql)
}

/// The restore record in an open library, if this library was restored.
pub fn restored(store: &Store) -> Option<Restored> {
    store.kv_get(RESTORED_KEY).ok().flatten().and_then(|text| serde_json::from_str(&text).ok())
}

/// The launch notice was shown; `fresh` lists profiles whose website data
/// wasn't on this Mac.
pub fn mark_shown(store: &Store, fresh: Vec<String>) -> Result<(), String> {
    let mut record = restored(store).ok_or("This library wasn't restored.")?;
    record.shown = true;
    record.fresh_profiles = fresh;
    let text = serde_json::to_string(&record).map_err(|_| "Couldn't save the restore notice.".to_string())?;
    store.kv_set(RESTORED_KEY, &text).map_err(|error| {
        let error = format!("Couldn't save the restore notice ({error}).");
        store.path().map_or(error.clone(), |library| without_paths(&error, &library))
    })
}

#[cfg(test)]
#[path = "backup_tests.rs"]
mod tests;
