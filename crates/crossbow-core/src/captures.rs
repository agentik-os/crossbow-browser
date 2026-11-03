//! The Captures library: each saved capture is a PNG in a private folder for
//! its profile, `Captures/Profile-<hex id>`, and a record in its own
//! `captures.sqlite` beside the library. Neither is in the library database,
//! so library backups, restores and sync never hold or rewind them, as for
//! the vault and website data.
//!
//! A capture is written to a private `.partial` file, synced and renamed, and
//! only then recorded; a failure at any step leaves no file and no record.
//! Delete moves the file aside, removes the record, then the file, and puts
//! the file back if the record stays.
//!
//! A damaged `captures.sqlite` never stops the app: it opens as
//! [`CapturesError::Damaged`], and [`move_aside`] keeps it under another name,
//! starts a new one and lists the capture files on disk again.
//!
//! Deleting a profile deletes its captures with it: [`detach_profile`] sets
//! its folder aside before the library commits. After, its files go, then
//! its records; if the commit fails, the folder is put back.

use rusqlite::{Connection, OptionalExtension, params};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "captures.sqlite";
pub const FOLDER: &str = "Captures";
const PARTIAL: &str = ".partial";
const DELETING: &str = ".deleting";
const COLUMNS: [&str; 8] = ["id", "profile_id", "file", "url", "title", "created_ms", "width", "height"];
/// A title is kept to this many characters.
const MAX_TITLE: usize = 300;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
CREATE TABLE IF NOT EXISTS captures (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    profile_id TEXT NOT NULL,
    file TEXT NOT NULL,
    url TEXT NOT NULL,
    title TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    UNIQUE (profile_id, file)
);
CREATE INDEX IF NOT EXISTS captures_by_profile ON captures (profile_id, created_ms DESC, id DESC);
"#;

pub const SAVE_FAILED: &str = "Couldn't save the capture to Captures. Check free disk space, then try again.";
pub const DELETE_FAILED: &str = "Couldn't delete the capture. It's still in Captures; try again.";
pub const UNAVAILABLE: &str = "Captures can't be opened. Check the data folder's permissions and free disk space, then reopen Crossbow.";
pub const DAMAGED: &str = "Captures can't be read: its file is damaged. Move Aside keeps that file and starts a new one, and your capture files are listed again.";
pub const MOVE_ASIDE_FAILED: &str = "Couldn't move the damaged file aside. Check the data folder's permissions, then try again.";
pub const PROFILE_DAMAGED: &str = "Its captures can't be read because the Captures file is damaged. Use Move Aside in Library → Captures, then try again.";
pub const PROFILE_FAILED: &str = "Its captures couldn't be set aside. Check the data folder's permissions, then try again.";

/// Why Captures can't be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapturesError {
    /// The file's content can't be read. Move Aside starts a new one.
    Damaged,
    /// The folder or disk refused. The file may be fine; trying again can work.
    Unavailable,
}

impl CapturesError {
    pub fn message(self) -> &'static str {
        match self {
            CapturesError::Damaged => DAMAGED,
            CapturesError::Unavailable => UNAVAILABLE,
        }
    }
}

/// Damage is the file's content: SQLite's corruption codes, a schema this
/// build doesn't know, a value of the wrong kind. Access, locking, I/O and
/// space are storage errors, as for the vault.
fn classify(error: rusqlite::Error) -> CapturesError {
    use rusqlite::{Error, ErrorCode};
    match error {
        Error::SqliteFailure(failure, _) => match failure.code {
            ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase | ErrorCode::TypeMismatch | ErrorCode::TooBig => CapturesError::Damaged,
            ErrorCode::Unknown if failure.extended_code & 0xff == rusqlite::ffi::SQLITE_ERROR => CapturesError::Damaged,
            _ => CapturesError::Unavailable,
        },
        Error::SqlInputError { error, .. } if error.extended_code & 0xff == rusqlite::ffi::SQLITE_ERROR => CapturesError::Damaged,
        Error::FromSqlConversionFailure(..) | Error::IntegralValueOutOfRange(..) | Error::Utf8Error(..) | Error::InvalidColumnType(..) => CapturesError::Damaged,
        _ => CapturesError::Unavailable,
    }
}

/// A saved capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    pub id: i64,
    pub profile_id: String,
    /// The PNG's name in its profile's folder.
    pub file: String,
    /// The page's address: http(s) only, otherwise empty.
    pub url: String,
    pub title: String,
    pub created_ms: u64,
    pub width: u32,
    pub height: u32,
}

/// A capture to save.
#[derive(Clone, Copy, Debug)]
pub struct NewCapture<'a> {
    pub profile_id: &'a str,
    pub url: &'a str,
    pub title: &'a str,
    pub created_ms: u64,
    /// Seconds east of UTC, for the file's name.
    pub offset_secs: i64,
    pub width: u32,
    pub height: u32,
    pub png: &'a [u8],
}

/// Steps a test can make fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Write,
    Rename,
    Record,
    MoveAside,
    Forget,
}

#[cfg(test)]
thread_local! {
    static FAIL: std::cell::Cell<Option<Step>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn fail_at(step: Option<Step>) {
    FAIL.with(|fail| fail.set(step));
}

#[cfg(test)]
fn step(at: Step) -> io::Result<()> {
    if FAIL.with(|fail| fail.get()) == Some(at) { Err(io::Error::other("injected")) } else { Ok(()) }
}

#[cfg(not(test))]
#[inline]
fn step(_: Step) -> io::Result<()> {
    Ok(())
}

pub struct Captures {
    conn: Connection,
    data_dir: PathBuf,
}

impl Captures {
    /// Open or create `data_dir/captures.sqlite`, readable only by this user.
    /// A file that fails SQLite's structure check, or holds anything but this
    /// build's table, is damaged and left as it is.
    pub fn open(data_dir: &Path) -> Result<Captures, CapturesError> {
        let path = data_dir.join(FILE_NAME);
        if fs::symlink_metadata(&path).is_err() {
            drop(private_file(&path).map_err(|_| CapturesError::Unavailable)?);
        }
        let conn = Connection::open(&path).map_err(classify)?;
        check(&conn)?;
        conn.execute_batch(SCHEMA).map_err(classify)?;
        let captures = Captures { conn, data_dir: data_dir.to_path_buf() };
        captures.tidy();
        Ok(captures)
    }

    /// Open `data_dir/captures.sqlite` only if it exists.
    pub fn open_existing(data_dir: &Path) -> Result<Option<Captures>, CapturesError> {
        if fs::symlink_metadata(data_dir.join(FILE_NAME)).is_err() { return Ok(None); }
        Captures::open(data_dir).map(Some)
    }

    /// `profile_id`'s folder, made private if it exists. None for an id that
    /// can't name one.
    pub fn folder(&self, profile_id: &str) -> Option<PathBuf> {
        folder_name(profile_id).map(|name| self.data_dir.join(FOLDER).join(name))
    }

    /// Where `capture`'s PNG is.
    pub fn path(&self, capture: &Capture) -> Option<PathBuf> {
        Some(self.folder(&capture.profile_id)?.join(&capture.file))
    }

    /// Whether `capture`'s PNG is still there.
    pub fn file_exists(&self, capture: &Capture) -> bool {
        self.path(capture).and_then(|path| fs::symlink_metadata(path).ok()).is_some_and(|meta| meta.is_file())
    }

    /// Write `new` atomically and record it.
    pub fn save(&self, new: &NewCapture<'_>) -> Result<Capture, String> {
        let folder = self.folder(new.profile_id).ok_or(SAVE_FAILED)?;
        prepare_folder(&self.data_dir.join(FOLDER)).and_then(|_| prepare_folder(&folder)).map_err(|_| SAVE_FAILED.to_string())?;
        let stem = file_stem(new.created_ms, new.offset_secs);
        let taken = |name: &str| -> bool {
            fs::symlink_metadata(folder.join(name)).is_ok()
                || self.conn.query_row("SELECT 1 FROM captures WHERE profile_id = ?1 AND file = ?2", params![new.profile_id, name], |_| Ok(())).optional().ok().flatten().is_some()
        };
        let file = (1..1000).map(|n| if n == 1 { format!("{stem}.png") } else { format!("{stem} {n}.png") }).find(|name| !taken(name)).ok_or(SAVE_FAILED)?;
        let path = folder.join(&file);
        let partial = folder.join(format!(".{file}{PARTIAL}"));
        let written = (|| -> io::Result<()> {
            let mut out = private_file(&partial)?;
            step(Step::Write)?;
            out.write_all(new.png)?;
            out.sync_all()?;
            drop(out);
            step(Step::Rename)?;
            fs::rename(&partial, &path)?;
            sync_folder(&folder);
            Ok(())
        })();
        if written.is_err() {
            let _ = fs::remove_file(&partial);
            return Err(SAVE_FAILED.into());
        }
        let capture = Capture {
            id: 0,
            profile_id: new.profile_id.to_owned(),
            file,
            url: page_url(new.url),
            title: new.title.chars().take(MAX_TITLE).collect(),
            created_ms: new.created_ms,
            width: new.width,
            height: new.height,
        };
        let recorded = step(Step::Record).map_err(|_| rusqlite::Error::InvalidQuery).and_then(|()| self.conn.execute(
            "INSERT INTO captures (profile_id, file, url, title, created_ms, width, height) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![capture.profile_id, capture.file, capture.url, capture.title, capture.created_ms as i64, capture.width, capture.height],
        ));
        if recorded.is_err() {
            let _ = fs::remove_file(&path);
            sync_folder(&folder);
            return Err(SAVE_FAILED.into());
        }
        Ok(Capture { id: self.conn.last_insert_rowid(), ..capture })
    }

    /// `profile_id`'s captures, newest first.
    pub fn list(&self, profile_id: &str) -> Result<Vec<Capture>, CapturesError> {
        let read = || -> rusqlite::Result<Vec<Capture>> {
            let mut statement = self.conn.prepare("SELECT id, profile_id, file, url, title, created_ms, width, height FROM captures WHERE profile_id = ?1 ORDER BY created_ms DESC, id DESC")?;
            statement.query_map([profile_id], row)?.collect()
        };
        read().map_err(classify)
    }

    pub fn get(&self, id: i64) -> Option<Capture> {
        self.conn.query_row("SELECT id, profile_id, file, url, title, created_ms, width, height FROM captures WHERE id = ?1", [id], row).optional().ok().flatten()
    }

    /// Remove the file and the record together; a failure keeps both.
    pub fn delete(&self, capture: &Capture) -> Result<(), String> {
        let path = self.path(capture).ok_or(DELETE_FAILED)?;
        let aside = path.with_file_name(format!(".{}{DELETING}", capture.file));
        let present = fs::symlink_metadata(&path).is_ok();
        if present {
            step(Step::MoveAside).and_then(|()| fs::rename(&path, &aside)).map_err(|_| DELETE_FAILED.to_string())?;
        }
        let forgotten = step(Step::Forget).map_err(|_| rusqlite::Error::InvalidQuery)
            .and_then(|()| self.conn.execute("DELETE FROM captures WHERE id = ?1", [capture.id]));
        if forgotten.is_err() {
            if present { let _ = fs::rename(&aside, &path); }
            return Err(DELETE_FAILED.into());
        }
        if present { let _ = fs::remove_file(&aside); }
        Ok(())
    }

    /// Forget a capture whose file is gone.
    pub fn remove_record(&self, capture: &Capture) -> Result<(), String> {
        if self.file_exists(capture) { return self.delete(capture); }
        self.conn.execute("DELETE FROM captures WHERE id = ?1", [capture.id]).map(|_| ()).map_err(|_| DELETE_FAILED.to_string())
    }

    /// Leftovers of a save or delete that stopped part way: a partial file
    /// was never recorded, and a file set aside whose record is gone goes.
    fn tidy(&self) {
        let Ok(profiles) = fs::read_dir(self.data_dir.join(FOLDER)) else { return };
        for folder in profiles.flatten().filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()) && !entry.file_name().to_string_lossy().starts_with('.')) {
            let Ok(files) = fs::read_dir(folder.path()) else { continue };
            for file in files.flatten() {
                let name = file.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') && name.ends_with(PARTIAL) {
                    let _ = fs::remove_file(file.path());
                } else if let Some(original) = name.strip_prefix('.').and_then(|name| name.strip_suffix(DELETING)) {
                    let recorded = self.conn.query_row("SELECT 1 FROM captures WHERE file = ?1", [original], |_| Ok(())).optional().ok().flatten().is_some();
                    let target = file.path().with_file_name(original);
                    if recorded && fs::symlink_metadata(&target).is_err() { let _ = fs::rename(file.path(), target); } else { let _ = fs::remove_file(file.path()); }
                }
            }
        }
    }

    /// Record every PNG in the profiles' folders that has no record, by its
    /// size and the file's time, with no address or title. Run on a new file,
    /// after the damaged one was moved aside. How many were listed again.
    fn adopt(&self) -> usize {
        let Ok(profiles) = fs::read_dir(self.data_dir.join(FOLDER)) else { return 0 };
        let mut adopted = 0;
        for folder in profiles.flatten().filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir())) {
            let name = folder.file_name().to_string_lossy().into_owned();
            let Some(profile_id) = profile_of(&name) else { continue };
            let Ok(files) = fs::read_dir(folder.path()) else { continue };
            for file in files.flatten().filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file())) {
                let file_name = file.file_name().to_string_lossy().into_owned();
                if file_name.starts_with('.') || !file_name.ends_with(".png") { continue; }
                let Some((width, height)) = png_size(&file.path()) else { continue };
                let created_ms = file.metadata().ok().and_then(|meta| meta.modified().ok()).and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |since| since.as_millis() as u64);
                let inserted = self.conn.execute(
                    "INSERT OR IGNORE INTO captures (profile_id, file, url, title, created_ms, width, height) VALUES (?1, ?2, '', '', ?3, ?4, ?5)",
                    params![profile_id, file_name, created_ms as i64, width, height],
                );
                if inserted.is_ok_and(|rows| rows == 1) { adopted += 1; }
            }
        }
        adopted
    }
}

/// The checks behind a damaged file: SQLite's structure check, then the
/// schema. A new, empty file passes; anything but this build's table and
/// index is damage, never something to upgrade.
fn check(conn: &Connection) -> Result<(), CapturesError> {
    let result: String = conn.query_row("PRAGMA quick_check(1)", [], |r| r.get(0)).map_err(classify)?;
    if result != "ok" { return Err(CapturesError::Damaged); }
    let objects = || -> rusqlite::Result<Vec<(String, String)>> {
        let mut statement = conn.prepare("SELECT type, name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name")?;
        statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
    };
    let objects = objects().map_err(classify)?;
    if objects.is_empty() { return Ok(()); }
    let expected = [("table".to_owned(), "captures".to_owned()), ("index".to_owned(), "captures_by_profile".to_owned())];
    if !objects.iter().all(|object| expected.contains(object)) || !objects.contains(&expected[0]) { return Err(CapturesError::Damaged); }
    let columns = || -> rusqlite::Result<Vec<String>> {
        let mut statement = conn.prepare("SELECT name FROM pragma_table_info('captures') ORDER BY cid")?;
        statement.query_map([], |r| r.get(0))?.collect()
    };
    if columns().map_err(classify)? != COLUMNS { return Err(CapturesError::Damaged); }
    Ok(())
}

/// Keep a damaged `captures.sqlite` as `captures-damaged-<UTC time>.sqlite`,
/// with its WAL and journal, then open a new one and list the capture files
/// on disk again. Its WAL moves first, so it is never replayed into the new
/// file. The name it was kept as, and how many captures were listed again.
pub fn move_aside(data_dir: &Path) -> Result<(String, usize), String> {
    let main = data_dir.join(FILE_NAME);
    let stamp = {
        let c = crate::backup::calendar(crate::now_ms(), 0);
        format!("{:04}{:02}{:02}T{:02}{:02}{:02}Z", c.year, c.month, c.day, c.hour, c.minute, c.second)
    };
    let taken = |name: &str| ["", "-wal", "-journal"].iter().any(|suffix| fs::symlink_metadata(data_dir.join(format!("{name}{suffix}"))).is_ok());
    let name = (1..1000).map(|n| if n == 1 { format!("captures-damaged-{stamp}.sqlite") } else { format!("captures-damaged-{stamp}-{n}.sqlite") })
        .find(|name| !taken(name)).ok_or(MOVE_ASIDE_FAILED)?;
    for suffix in ["-wal", "-journal", "-shm"] {
        let from = data_dir.join(format!("{FILE_NAME}{suffix}"));
        let moved = if suffix == "-shm" { fs::remove_file(&from) } else { fs::rename(&from, data_dir.join(format!("{name}{suffix}"))) };
        match moved {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(MOVE_ASIDE_FAILED.into()),
        }
    }
    match fs::rename(&main, data_dir.join(&name)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(MOVE_ASIDE_FAILED.into()),
    }
    sync_folder(data_dir);
    let captures = Captures::open(data_dir).map_err(|error| error.message().to_string())?;
    Ok((name, captures.adopt()))
}

/// A profile's captures set aside while the profile is deleted.
#[derive(Debug)]
pub struct DetachedCaptures {
    profile_id: String,
    data_dir: PathBuf,
    original: PathBuf,
    aside: PathBuf,
    /// There was no folder; `aside` is an empty marker made for the records.
    made: bool,
}

/// Set `profile_id`'s captures aside before its deletion commits: its folder
/// is renamed to a hidden `.Profile-<hex>.deleting`, made empty if it had
/// none, which also marks its records for removal. None when there are no
/// captures at all. A damaged file refuses, so nothing is half deleted.
pub fn detach_profile(data_dir: &Path, profile_id: &str) -> Result<Option<DetachedCaptures>, String> {
    let Some(name) = folder_name(profile_id) else { return Ok(None) };
    let root = data_dir.join(FOLDER);
    let original = root.join(&name);
    let has_folder = fs::symlink_metadata(&original).is_ok_and(|meta| meta.is_dir());
    match Captures::open_existing(data_dir) {
        Ok(None) if !has_folder => return Ok(None),
        Ok(_) => {}
        Err(CapturesError::Damaged) => return Err(PROFILE_DAMAGED.into()),
        Err(CapturesError::Unavailable) => return Err(PROFILE_FAILED.into()),
    }
    let aside = root.join(format!(".{name}{DELETING}"));
    if fs::symlink_metadata(&aside).is_ok() { fs::remove_dir_all(&aside).map_err(|_| PROFILE_FAILED.to_string())?; }
    if has_folder {
        fs::rename(&original, &aside).map_err(|_| PROFILE_FAILED.to_string())?;
    } else {
        prepare_folder(&root).and_then(|()| prepare_folder(&aside)).map_err(|_| PROFILE_FAILED.to_string())?;
    }
    sync_folder(&root);
    Ok(Some(DetachedCaptures { profile_id: profile_id.to_owned(), data_dir: data_dir.to_path_buf(), original, aside, made: !has_folder }))
}

impl DetachedCaptures {
    /// The deletion didn't commit: the folder goes back as it was.
    pub fn restore(self) -> Result<(), String> {
        let restored = if self.made { fs::remove_dir(&self.aside) } else { fs::rename(&self.aside, &self.original) };
        restored.map_err(|_| PROFILE_FAILED.to_string())
    }

    /// The deletion committed: remove the files, then the records. What
    /// fails is finished the next time Crossbow starts.
    pub fn remove(self) -> Result<(), String> {
        finish_deletion(&self.data_dir, &self.profile_id, &self.aside)
    }
}

/// Empty the set-aside folder, forget the profile's records, then remove the
/// folder, which marks the deletion until its records are gone. Records go
/// only once every file has.
fn finish_deletion(data_dir: &Path, profile_id: &str, aside: &Path) -> Result<(), String> {
    let empty = || -> io::Result<()> {
        for entry in fs::read_dir(aside)? {
            let entry = entry?;
            // A link is removed, never followed.
            if entry.file_type()?.is_dir() { fs::remove_dir_all(entry.path())? } else { fs::remove_file(entry.path())? }
        }
        Ok(())
    };
    empty().map_err(|_| DELETE_FAILED.to_string())?;
    let captures = Captures::open(data_dir).map_err(|error| error.message().to_string())?;
    captures.forget_profile(profile_id).map_err(|_| DELETE_FAILED.to_string())?;
    fs::remove_dir(aside).map_err(|_| DELETE_FAILED.to_string())
}

impl Captures {
    fn forget_profile(&self, profile_id: &str) -> rusqlite::Result<usize> {
        self.conn.execute("DELETE FROM captures WHERE profile_id = ?1", [profile_id])
    }
}

/// Finish profile deletions a stop or a failure cut short, at launch. A
/// profile that `exists` still gets its folder back; a deleted one loses its
/// files, then its records. Opens nothing when nothing waits.
pub fn finish_deletions(data_dir: &Path, exists: impl Fn(&str) -> bool) {
    let root = data_dir.join(FOLDER);
    let Ok(entries) = fs::read_dir(&root) else { return };
    let waiting: Vec<(String, PathBuf)> = entries.flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let folder = name.strip_prefix('.')?.strip_suffix(DELETING)?.to_owned();
            profile_of(&folder).map(|profile| (profile, entry.path()))
        }).collect();
    for (profile, aside) in waiting {
        let original = folder_name(&profile).map(|name| root.join(name));
        if exists(&profile) {
            match original {
                Some(original) if fs::symlink_metadata(&original).is_err() => { let _ = fs::rename(&aside, original); }
                _ => { let _ = fs::remove_dir(&aside); }
            }
        } else {
            let _ = finish_deletion(data_dir, &profile, &aside);
        }
    }
}

/// The profile id a `Profile-<hex>` folder is named for.
fn profile_of(folder: &str) -> Option<String> {
    let hex = folder.strip_prefix("Profile-")?;
    if hex.is_empty() || hex.len() % 2 != 0 { return None; }
    let bytes: Option<Vec<u8>> = (0..hex.len()).step_by(2).map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok()).collect();
    let id = String::from_utf8(bytes?).ok()?;
    (folder_name(&id).as_deref() == Some(folder)).then_some(id)
}

/// A PNG's width and height, from its header.
fn png_size(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut header = [0u8; 24];
    fs::File::open(path).ok()?.read_exact(&mut header).ok()?;
    if header[..8] != *b"\x89PNG\r\n\x1a\n" || header[12..16] != *b"IHDR" { return None; }
    let width = u32::from_be_bytes(header[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(header[20..24].try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Capture> {
    Ok(Capture {
        id: row.get(0)?,
        profile_id: row.get(1)?,
        file: row.get(2)?,
        url: row.get(3)?,
        title: row.get(4)?,
        created_ms: row.get::<_, i64>(5)?.max(0) as u64,
        width: row.get(6)?,
        height: row.get(7)?,
    })
}

/// The address kept with a capture: http(s) pages only, never data:, file:
/// or another scheme, and never a username or password in it.
pub fn page_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) if matches!(parsed.scheme(), "http" | "https") => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.to_string()
        }
        _ => String::new(),
    }
}

/// `Profile-` and the id's bytes in hex, as for the profile's website data.
pub(crate) fn folder_name(profile_id: &str) -> Option<String> {
    if profile_id.is_empty() || profile_id.len() > 64 || !profile_id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_') { return None; }
    Some(profile_id.bytes().fold(String::from("Profile-"), |mut name, byte| { name.push_str(&format!("{byte:02x}")); name }))
}

/// "Capture 2026-10-29 at 15.04.05", in local time.
pub fn file_stem(ms: u64, offset_secs: i64) -> String {
    let c = crate::backup::calendar(ms, offset_secs);
    format!("Capture {:04}-{:02}-{:02} at {:02}.{:02}.{:02}", c.year, c.month, c.day, c.hour, c.minute, c.second)
}

/// A folder only this user can open, made if missing; never a link.
pub(crate) fn prepare_folder(folder: &Path) -> io::Result<()> {
    match fs::symlink_metadata(folder) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(io::Error::new(io::ErrorKind::AlreadyExists, "not a folder")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder.create(folder)?;
        }
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(folder)?.permissions().mode() & 0o777 != 0o700 { fs::set_permissions(folder, fs::Permissions::from_mode(0o700))?; }
    }
    Ok(())
}

/// A new empty file only this user can read.
pub(crate) fn private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Best effort: some file systems can't sync a directory.
pub(crate) fn sync_folder(path: &Path) {
    let _ = fs::File::open(path).and_then(|folder| folder.sync_all());
}

#[cfg(test)]
#[path = "captures_tests.rs"]
mod tests;
