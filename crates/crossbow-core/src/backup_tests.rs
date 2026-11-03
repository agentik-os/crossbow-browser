use super::*;
use crate::library::Library;
use crate::model::Theme;
use crate::store::TabRow;
use std::collections::BTreeSet;

const ACCOUNT: &str = "account-a";
/// 2026-10-29T10:15:00Z, a Thursday.
const THURSDAY: u64 = 1_793_268_900_000;

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        Fixture { dir: tempfile::tempdir().unwrap() }
    }

    fn path(&self) -> PathBuf {
        self.dir.path().join("crossbow.sqlite")
    }

    fn open(&self) -> Library {
        let mut lib = Library::open(Store::open(&self.path()).unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        lib
    }

    fn folder(&self) -> PathBuf {
        folder(&self.path())
    }

    /// Every file name in the data folder and the Backups folder.
    fn files(&self) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        for dir in [self.dir.path().to_path_buf(), self.folder()] {
            for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                names.insert(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names
    }
}

fn crash_at(step: Option<Step>) {
    CRASH_AT.set(step);
    CRASHED.set(false);
}

/// A signed-in library with a pin, a cursor and an epoch, as sync leaves it.
fn signed_in(lib: &mut Library, account: &str) -> String {
    let space = lib.spaces_ordered()[0].0.clone();
    let pin = lib.add_pinned(&space, None, "https://example.test/pin", "Pin from the backup").unwrap();
    lib.store.kv_set(SESSION_KEY, &serde_json::json!({ "base_url": "http://127.0.0.1:9", "account_id": account, "email": "fixture@example.com", "device_id": "device-a" }).to_string()).unwrap();
    lib.store.kv_set(SYNC_CURSOR_KEY, "41").unwrap();
    lib.store.kv_set(SYNC_EPOCH_KEY, "3").unwrap();
    lib.store.mark_all_clean().unwrap();
    pin
}

/// The pin titles, sorted, read from a plain connection.
fn titles(path: &Path) -> Vec<String> {
    let conn = Connection::open(path).unwrap();
    let mut titles: Vec<String> = conn.prepare("SELECT json_extract(payload, '$.title') FROM entities WHERE kind = 'item' AND deleted = 0").unwrap()
        .query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    titles.sort();
    titles
}

fn kv_at(path: &Path, key: &str) -> Option<String> {
    kv(&Connection::open(path).unwrap(), key).unwrap()
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn names_carry_the_utc_time_and_the_reason() {
    assert_eq!(file_name(THURSDAY, 0, Reason::DeleteSyncData), "library-20261029T101500Z-before-delete-sync-data.sqlite");
    assert_eq!(file_name(THURSDAY + 999, 2, Reason::Daily), "library-20261029T101500Z-daily-2.sqlite");
    for reason in Reason::ALL {
        for seq in [0, 1, 12] {
            assert_eq!(parse_name(&file_name(THURSDAY, seq, reason)), Some((THURSDAY, seq, reason)));
        }
    }
    for name in ["library-20261029T101500Z-before-delete-sync-data.sqlite.partial", "library-20261029T101500Z-unknown.sqlite", "library-20261329T101500Z-daily.sqlite", "notes.sqlite", "library-20261029T101500Z.sqlite"] {
        assert_eq!(parse_name(name), None, "{name}");
    }
    // Leap days and the turn of a year.
    for ms in [951_782_400_000, 1_709_164_800_000, 1_798_761_599_000, 1_798_761_600_000, 0] {
        assert_eq!(parse_stamp(&stamp(ms)), Some(ms));
    }
    assert_eq!(stamp(1_709_164_800_000), "20240229T000000Z");
    assert_eq!(calendar(THURSDAY, -11 * 3600), Calendar { year: 2026, month: 10, day: 28, hour: 23, minute: 15, second: 0 });
}

#[test]
fn a_backup_is_a_checked_private_copy_in_the_backups_folder() {
    let fixture = Fixture::new();
    let mut lib = fixture.open();
    signed_in(&mut lib, ACCOUNT);
    let backup = create(&lib.store, Reason::DeleteSyncData).unwrap().unwrap();
    assert_eq!(backup.path.parent(), Some(fixture.folder().as_path()));
    assert_eq!(backup.reason, Reason::DeleteSyncData);
    assert!(backup.bytes > 0 && backup.bytes == fs::metadata(&backup.path).unwrap().len());
    #[cfg(unix)]
    {
        assert_eq!(mode(&fixture.folder()), 0o700);
        assert_eq!(mode(&backup.path), 0o600);
    }
    let conn = Connection::open_with_flags(&backup.path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert!(intact(&conn).unwrap());
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0)).unwrap();
    assert_eq!(mode, "delete", "one self-contained file");
    drop(conn);
    assert_eq!(titles(&backup.path), vec!["Pin from the backup"]);
    assert_eq!(list(&fixture.path()), vec![backup.clone()]);
    let names: Vec<String> = fs::read_dir(fixture.folder()).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names.len(), 1, "nothing beside the backup: {names:?}");
    // A second one in the same second gets its own name.
    let again = create(&lib.store, Reason::DeleteSyncData).unwrap().unwrap();
    assert_ne!(again.path, backup.path);
    assert_eq!(list(&fixture.path()).len(), 2);
    assert_eq!(list(&fixture.path())[0], again, "newest first");
    // An in-memory library has nothing to back up.
    assert_eq!(create(&Store::open_in_memory().unwrap(), Reason::Daily), Ok(None));
}

#[cfg(unix)]
#[test]
fn the_backups_folder_is_kept_private_and_never_followed_elsewhere() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let lib = fixture.open();
    fs::create_dir(fixture.folder()).unwrap();
    fs::set_permissions(fixture.folder(), fs::Permissions::from_mode(0o755)).unwrap();
    create(&lib.store, Reason::Daily).unwrap();
    assert_eq!(mode(&fixture.folder()), 0o700);

    // A link in its place, to a folder outside the data folder, is refused.
    let elsewhere = Fixture::new();
    fs::remove_dir_all(fixture.folder()).unwrap();
    std::os::unix::fs::symlink(elsewhere.dir.path(), fixture.folder()).unwrap();
    let error = create(&lib.store, Reason::Daily).unwrap_err();
    assert_eq!(error, "The Backups folder isn't a folder.");
    assert_eq!(fs::read_dir(elsewhere.dir.path()).unwrap().count(), 0, "nothing was written through the link");
    let failure = &failures(&lib.store)[0];
    assert_eq!((failure.reason(), failure.error.as_str()), (Some(Reason::Daily), "The Backups folder isn't a folder."));
}

#[test]
fn the_launch_backup_is_due_once_a_day() {
    let fixture = Fixture::new();
    assert_eq!(daily(&fixture.path(), THURSDAY), Ok(None), "no library yet");
    drop(fixture.open());
    assert!(daily_due(&fixture.path(), THURSDAY));
    let first = daily(&fixture.path(), THURSDAY).unwrap().unwrap();
    assert_eq!((first.reason, first.created_ms), (Reason::Daily, THURSDAY));
    let hour = 3_600_000;
    assert!(!daily_due(&fixture.path(), THURSDAY + 23 * hour));
    assert_eq!(daily(&fixture.path(), THURSDAY + 23 * hour), Ok(None));
    assert!(daily_due(&fixture.path(), THURSDAY + 24 * hour));
    let second = daily(&fixture.path(), THURSDAY + 25 * hour).unwrap().unwrap();
    assert_eq!(second.created_ms, THURSDAY + 25 * hour);
    assert_eq!(list(&fixture.path()).len(), 2);
}

#[test]
fn a_crash_before_the_rename_leaves_no_backup() {
    for step in [Step::BackupWritten, Step::BackupChecked] {
        let fixture = Fixture::new();
        let lib = fixture.open();
        crash_at(Some(step));
        assert!(create(&lib.store, Reason::TidyToday).is_err());
        crash_at(None);
        assert!(list(&fixture.path()).is_empty(), "a partial copy is listed after a crash at {step:?}");
        assert!(fixture.files().iter().any(|name| name.ends_with(PARTIAL)), "the crash left its temporary file");
        assert!(failures(&lib.store).is_empty(), "a crash notes nothing");
        // The next backup clears what the crash left.
        create(&lib.store, Reason::TidyToday).unwrap();
        assert_eq!(list(&fixture.path()).len(), 1);
        assert!(!fixture.files().iter().any(|name| name.contains(PARTIAL)), "{:?}", fixture.files());
    }
}

/// Overwrite the header of the database's second page, a table's root.
fn damage_second_page(path: &Path) {
    let mut bytes = fs::read(path).unwrap();
    let page = match u16::from_be_bytes([bytes[16], bytes[17]]) { 1 => 65_536, size => size as usize };
    assert!(bytes.len() >= 2 * page, "a library has more than one page");
    for byte in &mut bytes[page..page + 12] { *byte = 0xA5; }
    fs::write(path, &bytes).unwrap();
}

#[test]
fn a_copy_that_fails_its_check_is_never_kept() {
    let fixture = Fixture::new();
    let lib = fixture.open();
    let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
    let damaged = fixture.dir.path().join("damaged.sqlite");
    fs::copy(&backup.path, &damaged).unwrap();
    damage_second_page(&damaged);
    assert!(seal(&damaged).is_err());
}

/// Lock `folder` as Finder's Locked box does: no file can be added to it.
#[cfg(target_os = "macos")]
fn lock(folder: &Path, locked: bool) {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(folder.as_os_str().as_bytes()).unwrap();
    // SAFETY: `path` is a valid C string for the duration of the call.
    assert_eq!(unsafe { libc::chflags(path.as_ptr(), if locked { libc::UF_IMMUTABLE } else { 0 }) }, 0);
}

#[cfg(target_os = "macos")]
#[test]
fn a_failed_backup_is_noted_and_leaves_nothing_behind() {
    let fixture = Fixture::new();
    let lib = fixture.open();
    create(&lib.store, Reason::Daily).unwrap();
    lock(&fixture.folder(), true);
    let error = create(&lib.store, Reason::DeleteSyncData).unwrap_err();
    lock(&fixture.folder(), false);
    assert!(error.starts_with("Couldn't create the backup file"), "{error}");
    assert_eq!(list(&fixture.path()).len(), 1);
    let noted = failures(&lib.store);
    assert_eq!(noted.len(), 1);
    assert_eq!(noted[0].reason(), Some(Reason::DeleteSyncData));
    assert!(!noted[0].error.contains(&*fixture.dir.path().to_string_lossy()), "a failure never names a path");
}

/// Make `path` unreadable to its owner, or readable again.
#[cfg(unix)]
fn unreadable(path: &Path, yes: bool) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(if yes { 0o000 } else { 0o600 })).unwrap();
}

/// AC5: SQLite names the file it can't open; the messages never do.
#[cfg(unix)]
#[test]
fn backup_and_restore_messages_never_name_a_path() {
    let fixture = Fixture::new();
    let mut lib = fixture.open();
    signed_in(&mut lib, ACCOUNT);
    let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
    restore(&lib.store, &backup).unwrap();
    drop(lib);
    let folder = fixture.dir.path();
    let named: Vec<String> = [Some(folder.to_path_buf()), folder.canonicalize().ok(), std::env::var_os("HOME").map(PathBuf::from)]
        .into_iter().flatten().map(|path| path.display().to_string()).filter(|path| path.len() > 1).collect();
    // The launch backup, the backup before an upgrade and the restore
    // waiting for this launch, each unable to open the library.
    unreadable(&fixture.path(), true);
    let daily_error = daily(&fixture.path(), crate::now_ms() + 2 * DAY_MS).unwrap_err();
    let upgrade_error = before_upgrade(&fixture.path()).unwrap_err();
    let restore_error = finish_restore(&fixture.path()).unwrap_err();
    unreadable(&fixture.path(), false);
    for (what, text) in [("the launch backup", &daily_error), ("the upgrade backup", &upgrade_error), ("the restore", &restore_error)] {
        for path in &named { assert!(!text.contains(path.as_str()), "{what} names a path: {text}"); }
        assert!(text.contains("the Crossbow data folder"), "{what}: {text}");
    }
    assert_eq!(finish_restore(&fixture.path()), Ok(true), "the restore still finishes once the library opens");
}

#[test]
fn paths_in_the_home_folder_are_named_by_their_folder() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from).filter(|home| home.as_os_str().len() > 1) else { return };
    let data = home.join("Library/Application Support/Crossbow");
    let library = data.join("crossbow.sqlite");
    let text = format!(
        "Couldn't open the library (unable to open database file: {}). Also {}, and {}.",
        library.display(), home.join("Elsewhere/notes.sqlite").display(), data.join("Backups/library-x.sqlite.partial").display(),
    );
    assert_eq!(without_paths(&text, &library), "Couldn't open the library (unable to open database file in the Crossbow data folder). Also your home folder, and the Crossbow data folder.");
}

fn backup_at(ms: u64, reason: Reason, bytes: u64) -> Backup {
    Backup { path: PathBuf::from(file_name(ms, 0, reason)), created_ms: ms, reason, bytes, seq: 0 }
}

#[test]
fn retention_keeps_a_week_of_days_four_weeks_and_ten_before_backups() {
    // One daily backup a day for six weeks, newest on Sunday 2026-11-01.
    let sunday = THURSDAY + 3 * DAY_MS;
    let mut backups: Vec<Backup> = (0..42).map(|day| backup_at(sunday - day * DAY_MS, Reason::Daily, 1)).collect();
    backups.extend((0..15).map(|n| backup_at(sunday - n * 3_600_000 - 1000, Reason::TurnSyncOn, 1)));
    backups.sort_by(|a, b| b.created_ms.cmp(&a.created_ms));
    let gone: BTreeSet<PathBuf> = expired(&backups, SIZE_CAP).into_iter().map(|backup| backup.path).collect();
    let kept: Vec<&Backup> = backups.iter().filter(|backup| !gone.contains(&backup.path)).collect();
    let days: Vec<u64> = kept.iter().filter(|b| b.reason == Reason::Daily).map(|b| (sunday - b.created_ms) / DAY_MS).collect();
    // Monday to Sunday of this week, then the Sunday of each of the three weeks before.
    assert_eq!(days, vec![0, 1, 2, 3, 4, 5, 6, 7, 14, 21]);
    assert_eq!(kept.iter().filter(|b| b.reason == Reason::TurnSyncOn).count(), 10);
    let newest_before: Vec<u64> = backups.iter().filter(|b| b.reason == Reason::TurnSyncOn).take(10).map(|b| b.created_ms).collect();
    assert!(kept.iter().filter(|b| b.reason == Reason::TurnSyncOn).all(|b| newest_before.contains(&b.created_ms)));
}

#[test]
fn retention_removes_the_oldest_past_the_size_cap_but_never_the_newest() {
    let backups: Vec<Backup> = (0..5).map(|n| backup_at(THURSDAY - n * DAY_MS, Reason::Daily, 100)).collect();
    let gone: Vec<u64> = expired(&backups, 250).iter().map(|b| b.created_ms).collect();
    assert_eq!(gone, vec![THURSDAY - 2 * DAY_MS, THURSDAY - 3 * DAY_MS, THURSDAY - 4 * DAY_MS]);
    let huge = vec![backup_at(THURSDAY, Reason::Restore, 900), backup_at(THURSDAY - 1000, Reason::Daily, 900)];
    assert_eq!(expired(&huge, 250).len(), 1, "the newest stays even alone over the cap");
    assert_eq!(expired(&huge, 250)[0].created_ms, THURSDAY - 1000);
}

#[test]
fn pruning_on_disk_removes_only_expired_backups() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.folder()).unwrap();
    for n in 0..13 {
        fs::write(fixture.folder().join(file_name(THURSDAY - n * 60_000, 0, Reason::TidyToday)), vec![0u8; 10]).unwrap();
    }
    fs::write(fixture.folder().join("notes.txt"), b"not a backup").unwrap();
    fs::write(fixture.folder().join(format!("{}{PARTIAL}", file_name(THURSDAY, 0, Reason::Daily))), b"partial").unwrap();
    prune(&fixture.path(), 55);
    let left = list(&fixture.path());
    assert_eq!(left.len(), 5, "ten by count, then five by size");
    assert_eq!(left[0].created_ms, THURSDAY);
    assert!(fixture.folder().join("notes.txt").exists(), "an unknown file is never removed");
}

/// A library with a known shape.
fn shaped(fixture: &Fixture) -> Library {
    let mut lib = fixture.open();
    let personal = lib.default_profile_id();
    let work = lib.create_named_profile("Work").unwrap();
    let home = lib.spaces_ordered()[0].0.clone();
    let office = lib.create_space("Office", "🏢", Theme::preset(1), &work).unwrap();
    let folder = lib.create_folder(&office, None, "Reading").unwrap();
    lib.create_folder(&home, None, "Later").unwrap();
    lib.add_pinned(&office, Some(&folder), "https://example.test/a", "A").unwrap();
    lib.add_pinned(&office, None, "https://example.test/b", "B").unwrap();
    lib.add_pinned(&home, None, "https://example.test/c", "C").unwrap();
    lib.add_favorite(&personal, "https://example.test/f", "F").unwrap();
    for (n, space) in [&home, &home, &office, &office].into_iter().enumerate() {
        lib.store.put_tab(&TabRow { id: format!("tab-{n}"), space_id: space.clone(), item_id: None, url: format!("https://example.test/t{n}"), title: format!("Tab {n}"), order: format!("a{n}"), created_ms: 1, last_active_ms: 1 }).unwrap();
    }
    lib.store.archive_tab(&work, "https://example.test/old", "Old", &office, 10).unwrap();
    lib.store.archive_tab(&personal, "https://example.test/older", "Older", &home, 11).unwrap();
    lib.store.record_visit(&work, "https://example.test/visited", "Visited", 12).unwrap();
    lib
}

#[test]
fn the_preview_counts_from_a_read_only_open() {
    let fixture = Fixture::new();
    let lib = shaped(&fixture);
    let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
    let (bytes, modified) = (fs::read(&backup.path).unwrap(), fs::metadata(&backup.path).unwrap().modified().unwrap());
    let before = fixture.files();
    assert_eq!(inspect(&backup.path), Check::Ready(Preview { spaces: 2, profiles: 2, pins: 3, folders: 2, tabs: 4, archived: 2 }));
    assert_eq!(fs::read(&backup.path).unwrap(), bytes, "reading changed the backup");
    assert_eq!(fs::metadata(&backup.path).unwrap().modified().unwrap(), modified);
    assert_eq!(fixture.files(), before, "reading left a file beside the backup");
}

#[test]
fn newer_and_damaged_backups_are_refused() {
    let fixture = Fixture::new();
    let lib = shaped(&fixture);
    let good = create(&lib.store, Reason::Daily).unwrap().unwrap();
    let copy = |name: &str| -> PathBuf {
        let path = fixture.folder().join(name);
        fs::copy(&good.path, &path).unwrap();
        path
    };
    let newer = copy(&file_name(THURSDAY, 0, Reason::Daily));
    Connection::open(&newer).unwrap().execute_batch(&format!("PRAGMA user_version = {}", LIBRARY_FORMAT + 1)).unwrap();
    assert_eq!(inspect(&newer), Check::Newer);

    let truncated = copy(&file_name(THURSDAY, 1, Reason::Daily));
    let length = fs::metadata(&truncated).unwrap().len();
    fs::OpenOptions::new().write(true).open(&truncated).unwrap().set_len(length / 3).unwrap();
    assert_eq!(inspect(&truncated), Check::Damaged);

    let garbage = copy(&file_name(THURSDAY, 2, Reason::Daily));
    fs::write(&garbage, vec![0x5Au8; 8192]).unwrap();
    assert_eq!(inspect(&garbage), Check::Damaged);

    let corrupt = copy(&file_name(THURSDAY, 3, Reason::Daily));
    damage_second_page(&corrupt);
    assert_eq!(inspect(&corrupt), Check::Damaged);

    let unreadable_record = copy(&file_name(THURSDAY, 4, Reason::Daily));
    Connection::open(&unreadable_record).unwrap().execute("INSERT INTO kv (k, v) VALUES (?1, 'not a record')", [e2ee::FLAG_KEY]).unwrap();
    assert_eq!(inspect(&unreadable_record), Check::Damaged, "a restore must know which account an encrypted backup belongs to");

    // A refused backup changes nothing: no backup of the library, no copy waiting.
    let count = list(&fixture.path()).len();
    for (path, check) in [(&newer, Check::Newer), (&truncated, Check::Damaged)] {
        let chosen = list(&fixture.path()).into_iter().find(|backup| &backup.path == path).unwrap();
        assert_eq!(restore(&lib.store, &chosen), Err(RestoreError::Refused(check)));
    }
    assert_eq!(list(&fixture.path()).len(), count);
    assert_eq!(pending(&fixture.path()), None);
}

/// Restore `backup` and relaunch: close the library, finish, reopen.
fn restore_and_relaunch(fixture: &Fixture, lib: Library, backup: &Backup) -> Library {
    restore(&lib.store, backup).unwrap();
    drop(lib);
    assert_eq!(finish_restore(&fixture.path()), Ok(true));
    fixture.open()
}

fn live_clocks(lib: &Library) -> Vec<Hlc> {
    lib.store.all_entities().unwrap().into_iter().filter(|row| !row.deleted && row.kind != MARKER_KIND).map(|row| row.hlc).collect()
}

/// The largest clock sync can send, as a hostile or corrupt server row can
/// leave it on this Mac.
const LARGEST: &str = "9999999999999-999999-legacy";

/// A deleted row at `hlc`, written as a pull would leave it.
fn row_at(lib: &Library, id: &str, hlc: &str) {
    lib.store.connection().execute("INSERT INTO entities (kind, id, hlc, deleted, payload, dirty) VALUES ('item', ?1, ?2, 1, '', 0)", params![id, hlc]).unwrap();
}

#[test]
fn a_restore_is_refused_before_any_change_when_restored_rows_cant_get_newer_clocks() {
    for (in_backup, in_library) in [(true, false), (false, true), (true, true)] {
        let case = format!("largest clock in the backup: {in_backup}, in this library: {in_library}");
        let fixture = Fixture::new();
        let mut lib = fixture.open();
        signed_in(&mut lib, ACCOUNT);
        if in_backup { row_at(&lib, "largest", LARGEST); }
        let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
        if !in_library { lib.store.connection().execute("DELETE FROM entities WHERE id = 'largest'", []).unwrap(); }
        if in_library && !in_backup { row_at(&lib, "largest", LARGEST); }
        let space = lib.spaces_ordered()[0].0.clone();
        lib.add_pinned(&space, None, "https://example.test/new", "Added after the backup").unwrap();
        let files = fixture.files();
        assert_eq!(restore(&lib.store, &backup), Err(RestoreError::Clock), "{case}");
        assert_eq!(fixture.files(), files, "nothing was written, not even the backup before a restore: {case}");
        assert_eq!(pending(&fixture.path()), None, "{case}");
        drop(lib);
        assert_eq!(finish_restore(&fixture.path()), Ok(false), "{case}");
        assert_eq!(titles(&fixture.path()), vec!["Added after the backup", "Pin from the backup"], "{case}");
    }
}

#[test]
fn a_restore_that_cant_get_newer_clocks_at_launch_is_dropped_and_the_library_kept() {
    let fixture = Fixture::new();
    let mut lib = fixture.open();
    signed_in(&mut lib, ACCOUNT);
    let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
    let space = lib.spaces_ordered()[0].0.clone();
    lib.add_pinned(&space, None, "https://example.test/new", "Added after the backup").unwrap();
    restore(&lib.store, &backup).unwrap();
    // A pull after the restore was chosen brings the largest clock.
    row_at(&lib, "largest", LARGEST);
    drop(lib);
    assert_eq!(finish_restore(&fixture.path()), Err(CLOCK_LIMIT.to_string()));
    assert_eq!(pending(&fixture.path()), None, "dropped, not tried again at every launch");
    assert_eq!(finish_restore(&fixture.path()), Ok(false));
    assert_eq!(titles(&fixture.path()), vec!["Added after the backup", "Pin from the backup"]);
    let lib = fixture.open();
    assert!(restored(&lib.store).is_none());
}

#[test]
fn restored_clocks_are_ones_sync_can_send_above_the_newest_it_could() {
    let fixture = Fixture::new();
    let mut lib = fixture.open();
    signed_in(&mut lib, ACCOUNT);
    let space = lib.spaces_ordered()[0].0.clone();
    for n in 0..3 { lib.add_pinned(&space, None, &format!("https://example.test/{n}"), &format!("Pin {n}")).unwrap(); }
    let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
    // A clock two ticks short of its counter's end, and one sync can't send
    // that sorts above it as text.
    let near = "9999999999990-999998-legacy";
    row_at(&lib, "near", near);
    row_at(&lib, "unsendable", "9999999999999-1000000-legacy");
    let lib = restore_and_relaunch(&fixture, lib, &backup);
    let near = Hlc::parse(near).unwrap();
    let clocks = live_clocks(&lib);
    assert!(clocks.len() > 3);
    for hlc in &clocks {
        assert!(Hlc::parse(&hlc.encode()).is_ok_and(|parsed| parsed == *hlc) && *hlc > near, "{} isn't a sendable clock above {}", hlc.encode(), near.encode());
    }
    let last = Hlc::parse(&lib.store.kv_get(KV_LAST_HLC).unwrap().unwrap()).unwrap();
    assert!(last >= *clocks.iter().max().unwrap());
}

#[test]
fn a_restore_replaces_the_library_at_the_next_launch_and_keeps_this_macs_sync_state() {
    let fixture = Fixture::new();
    let mut lib = fixture.open();
    let pin = signed_in(&mut lib, ACCOUNT);
    let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
    // After the backup: an edit, a new pin, and newer sync state on this Mac.
    lib.update_item(&pin, |item| item.title = "Renamed after the backup".into()).unwrap();
    let space = lib.spaces_ordered()[0].0.clone();
    lib.add_pinned(&space, None, "https://example.test/new", "Added after the backup").unwrap();
    lib.store.kv_set(SYNC_CURSOR_KEY, "99").unwrap();
    lib.store.kv_set(BASE_URL_KEY, "http://127.0.0.1:10").unwrap();
    let node = lib.node().to_string();
    let newest = lib.store.all_entities().unwrap().into_iter().map(|row| row.hlc).max().unwrap();

    restore(&lib.store, &backup).unwrap();
    assert_eq!(pending(&fixture.path()), Some(backup.created_ms));
    assert_eq!(list(&fixture.path())[0].reason, Reason::Restore, "the current library was backed up first");
    assert_eq!(titles(&fixture.path()), vec!["Added after the backup", "Renamed after the backup"], "nothing changes until the next launch");
    drop(lib);
    assert_eq!(finish_restore(&fixture.path()), Ok(true));
    assert_eq!(pending(&fixture.path()), None);
    assert!(!suffixed(&fixture.path(), "-wal").exists() && !staged_path(&fixture.path()).exists());

    for _ in 0..2 {
        let lib = fixture.open();
        assert_eq!(lib.items.values().map(|item| item.title.as_str()).collect::<Vec<_>>(), vec!["Pin from the backup"]);
        // This Mac's sync state, not the backup's.
        assert_eq!(lib.store.kv_get(BASE_URL_KEY).unwrap().as_deref(), Some("http://127.0.0.1:10"));
        assert!(lib.store.kv_get(SESSION_KEY).unwrap().unwrap().contains(ACCOUNT));
        assert_eq!(lib.node(), node);
        assert_eq!(lib.store.kv_get(SYNC_CURSOR_KEY).unwrap(), None);
        assert_eq!(lib.store.kv_get(SYNC_EPOCH_KEY).unwrap(), None);
        assert_eq!(SyncOff::read(&lib, ACCOUNT), Some(SyncOff { account: ACCOUNT.into(), here: true, encrypted: false, restored: true, unreadable: false }));
        assert_eq!(lib.store.dirty_count().unwrap(), 0, "a restore never uploads by itself");
        // Fresh clocks: the restored rows replace the synced copies they meet.
        assert!(live_clocks(&lib).iter().all(|hlc| *hlc > newest && hlc.node == node));
        assert!(Hlc::parse(&lib.store.kv_get(KV_LAST_HLC).unwrap().unwrap()).unwrap() >= *live_clocks(&lib).iter().max().unwrap());
        let record = restored(&lib.store).unwrap();
        assert_eq!((record.backup_ms, record.reason.as_str(), record.shown, record.encrypted_for), (backup.created_ms, "daily", false, None));
        assert!(record.applied_ms >= backup.created_ms);
    }
    assert_eq!(finish_restore(&fixture.path()), Ok(false), "once");
}

#[test]
fn a_signed_out_restore_turns_nothing_off() {
    let fixture = Fixture::new();
    let lib = shaped(&fixture);
    let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
    let lib = restore_and_relaunch(&fixture, lib, &backup);
    assert_eq!(lib.store.kv_get(SYNC_OFF_KEY).unwrap(), None);
    assert_eq!(lib.store.kv_get(SESSION_KEY).unwrap(), None);
    assert_eq!(lib.store.kv_get(ENCRYPT_FIRST_KEY).unwrap(), None);
    assert_eq!(lib.store.dirty_count().unwrap(), 0, "signing in queues the whole library, as always");
}

fn flag(account: &str, key: &str) -> String {
    serde_json::json!({ "account": account, "key_id": key }).to_string()
}

/// Restore a backup encrypted (or not) into a library in a given state.
fn hold_after(backup_flag: Option<&str>, backup_hold: Option<&str>, current_flag: Option<&str>, current_hold: Option<&str>, session: Option<&str>) -> (Option<String>, Option<String>, Option<SyncOff>) {
    let fixture = Fixture::new();
    let mut lib = fixture.open();
    let set = |lib: &Library, key: &str, value: Option<&str>| match value {
        Some(value) => lib.store.kv_set(key, value).unwrap(),
        None => lib.store.kv_delete(key).unwrap(),
    };
    if let Some(account) = session { signed_in(&mut lib, account); }
    set(&lib, e2ee::FLAG_KEY, backup_flag);
    set(&lib, ENCRYPT_FIRST_KEY, backup_hold);
    let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
    set(&lib, e2ee::FLAG_KEY, current_flag);
    set(&lib, ENCRYPT_FIRST_KEY, current_hold);
    let lib = restore_and_relaunch(&fixture, lib, &backup);
    let off = session.and_then(|account| SyncOff::read(&lib, account));
    (lib.store.kv_get(e2ee::FLAG_KEY).unwrap(), lib.store.kv_get(ENCRYPT_FIRST_KEY).unwrap(), off)
}

#[test]
fn an_encrypted_backup_waits_for_encryption_and_never_brings_back_its_key() {
    let old = flag(ACCOUNT, "old-key");
    let current = flag(ACCOUNT, "current-key");
    let held = Some(crate::sync::deleted::hold_text(ACCOUNT));
    // Encrypted when backed up; its key was retired since (Delete Sync Data).
    let (record, hold, off) = hold_after(Some(&old), None, None, Some(ACCOUNT), Some(ACCOUNT));
    assert_eq!((record, hold), (None, held.clone()), "the retired key's record stays gone and uploads wait");
    assert!(off.unwrap().encrypted);
    // The same, never deleted: the hold is new.
    assert_eq!(hold_after(Some(&old), None, None, None, Some(ACCOUNT)).1, held);
    // Signed out, or signed in to another account: the same hold, which
    // names the backup's account and waits for every account.
    assert_eq!(hold_after(Some(&old), None, None, None, None).1, held);
    assert_eq!(hold_after(Some(&old), None, None, None, Some("account-b")).1, held);
    // Held when backed up (after a delete): still held.
    assert_eq!(hold_after(None, Some(ACCOUNT), None, None, Some(ACCOUNT)).1, held);
    // Encrypted now: this Mac's record and key stay, whatever the backup had.
    let (record, hold, off) = hold_after(Some(&old), None, Some(&current), None, Some(ACCOUNT));
    assert_eq!((record.as_deref(), hold), (Some(current.as_str()), None));
    assert!(!off.unwrap().encrypted);
    assert_eq!(hold_after(None, None, Some(&current), None, Some(ACCOUNT)).0.as_deref(), Some(current.as_str()));
    // Plaintext then and now.
    assert_eq!(hold_after(None, None, None, None, Some(ACCOUNT)), (None, None, Some(SyncOff { account: ACCOUNT.into(), here: true, encrypted: false, restored: true, unreadable: false })));
}

/// Recheck 9's AB1, for restores: an encrypted backup brings back a hold
/// that belongs to the library, so uploads to every account wait for
/// encryption, whichever account is signed in, or none. Neither the
/// backup's own choices to upload unencrypted, nor this Mac's, carry over
/// to the restored library, and a delete the backup was taken during
/// doesn't come back as this Mac's.
#[test]
fn a_restored_encrypted_library_waits_for_encryption_for_every_account() {
    use crate::sync::deleted::{DELETING_KEY, encrypt_first};
    let old = flag(ACCOUNT, "old-key");
    let released = |account: &str| serde_json::json!({ "account": ACCOUNT, "unencrypted": [account] }).to_string();
    let cases = [
        ("an encrypted backup", Some(old.clone()), None, None),
        ("a backup held after a delete, released for account-b", None, Some(released("account-b")), None),
        ("an encrypted backup onto a Mac released for account-c", Some(old.clone()), None, Some(released("account-c"))),
    ];
    for session in [None, Some(ACCOUNT), Some("account-b")] {
        for (what, backup_flag, backup_hold, current_hold) in &cases {
            let fixture = Fixture::new();
            let mut lib = fixture.open();
            if let Some(account) = session { signed_in(&mut lib, account); }
            if let Some(text) = backup_flag { lib.store.kv_set(e2ee::FLAG_KEY, text).unwrap(); }
            if let Some(text) = backup_hold { lib.store.kv_set(ENCRYPT_FIRST_KEY, text).unwrap(); }
            lib.store.kv_set(DELETING_KEY, ACCOUNT).unwrap();
            let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
            for key in [e2ee::FLAG_KEY, ENCRYPT_FIRST_KEY, DELETING_KEY] { lib.store.kv_delete(key).unwrap(); }
            if let Some(text) = current_hold { lib.store.kv_set(ENCRYPT_FIRST_KEY, text).unwrap(); }
            let lib = restore_and_relaunch(&fixture, lib, &backup);
            for account in [ACCOUNT, "account-b", "account-c"] {
                assert_eq!(encrypt_first(&lib, account), Some(true), "{what}, signed in to {session:?}: uploads to {account} don't wait for encryption");
            }
            assert_eq!(lib.store.kv_get(DELETING_KEY).unwrap(), None, "{what}: the backup's delete came back as this Mac's");
            if let Some(account) = session { assert!(SyncOff::read(&lib, account).unwrap().encrypted, "{what}"); }
        }
    }
}

#[test]
fn the_encryption_marker_and_notice_are_this_macs() {
    let fixture = Fixture::new();
    let mut lib = fixture.open();
    signed_in(&mut lib, ACCOUNT);
    lib.store.put_raw_entity(MARKER_KIND, e2ee::MARKER_ID, Some(&crate::store::RawEntity { hlc: Hlc::new(5, 0, "nodea").encode(), deleted: false, payload: "backup marker".into(), dirty: false })).unwrap();
    lib.store.kv_set(e2ee::NOTICE_KEY, "backup notice").unwrap();
    let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
    let current = crate::store::RawEntity { hlc: Hlc::new(9, 0, "nodeb").encode(), deleted: false, payload: "current marker".into(), dirty: false };
    lib.store.put_raw_entity(MARKER_KIND, e2ee::MARKER_ID, Some(&current)).unwrap();
    lib.store.kv_delete(e2ee::NOTICE_KEY).unwrap();
    let lib = restore_and_relaunch(&fixture, lib, &backup);
    assert_eq!(lib.store.raw_entity(MARKER_KIND, e2ee::MARKER_ID).unwrap(), Some(current), "the marker keeps its clock");
    assert_eq!(lib.store.kv_get(e2ee::NOTICE_KEY).unwrap(), None);
}

#[test]
/// AC4: a deleted profile's history and archive leave every backup with it,
/// so a restore brings back the profile, its Spaces and its items, not those.
fn a_restored_profile_comes_back_without_the_history_and_archive_it_was_deleted_with() {
    let fixture = Fixture::new();
    let mut lib = shaped(&fixture);
    let work = lib.profiles.iter().find(|(_, profile)| profile.name == "Work").unwrap().0.clone();
    let personal = lib.default_profile_id();
    let office = lib.spaces.iter().find(|(_, space)| space.name == "Office").unwrap().0.clone();
    let owned = |lib: &Library| -> Vec<(String, String)> {
        let mut items: Vec<_> = lib.items.values().map(|item| (item.title.clone(), item.profile_id.clone())).collect();
        items.sort();
        items
    };
    let before = owned(&lib);
    let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
    lib.set_space_profile(&office, &personal).unwrap();
    lib.delete_profile(&work).unwrap();
    assert!(!lib.profiles.contains_key(&work) && lib.store.archive(&work, 10).unwrap().is_empty());

    let lib = restore_and_relaunch(&fixture, lib, &backup);
    assert_eq!(lib.profiles[&work].name, "Work");
    assert_eq!(lib.spaces[&office].profile_id, work);
    assert_eq!(owned(&lib), before);
    assert!(lib.store.archive(&work, 10).unwrap().is_empty() && lib.store.recent_history(&work, 10).unwrap().is_empty());
    assert_eq!(lib.store.archive(&personal, 10).unwrap().len(), 1, "each profile keeps only its own");
}

/// What a crash leaves at `library`: the old library or the restored one.
fn state(path: &Path) -> &'static str {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    assert!(intact(&conn).unwrap());
    drop(conn);
    match (titles(path).as_slice(), kv_at(path, SYNC_CURSOR_KEY), kv_at(path, SYNC_OFF_KEY)) {
        ([only], None, Some(_)) if only == "Pin from the backup" => "restored",
        ([added, renamed], Some(cursor), None) if added == "Added after the backup" && renamed == "Renamed after the backup" && cursor == "99" => "old",
        (titles, cursor, off) => panic!("a mix: {titles:?}, cursor {cursor:?}, sync off {off:?}"),
    }
}

#[test]
fn a_crash_at_any_restore_step_leaves_the_old_library_or_the_restored_one() {
    for (step, after_crash) in [(Step::Staged, "old"), (Step::CurrentRead, "old"), (Step::Restamped, "old"), (Step::Prepared, "old"), (Step::Swapped, "restored")] {
        let fixture = Fixture::new();
        let mut lib = fixture.open();
        let pin = signed_in(&mut lib, ACCOUNT);
        let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
        lib.update_item(&pin, |item| item.title = "Renamed after the backup".into()).unwrap();
        let space = lib.spaces_ordered()[0].0.clone();
        lib.add_pinned(&space, None, "https://example.test/new", "Added after the backup").unwrap();
        lib.store.kv_set(SYNC_CURSOR_KEY, "99").unwrap();
        crash_at(Some(step));
        let staged = restore(&lib.store, &backup);
        drop(lib);
        if step == Step::Staged {
            assert!(staged.is_err());
            assert_eq!(pending(&fixture.path()), None, "a copy that wasn't renamed into place isn't a restore");
        } else {
            staged.unwrap();
            assert!(finish_restore(&fixture.path()).is_err(), "{step:?}");
        }
        crash_at(None);
        assert_eq!(state(&fixture.path()), after_crash, "after a crash at {step:?}");
        // The next launch finishes what was started, or there was nothing to finish.
        let finished = finish_restore(&fixture.path()).unwrap();
        assert_eq!(finished, !matches!(step, Step::Staged | Step::Swapped), "{step:?}");
        let expected = if step == Step::Staged { "old" } else { "restored" };
        assert_eq!(state(&fixture.path()), expected, "after relaunching from a crash at {step:?}");
        assert!(!fixture.files().iter().any(|name| name.contains(".restore")), "{step:?} left {:?}", fixture.files());
        // The old library keeps its own edits waiting; the restored one waits for Turn Sync On.
        let lib = fixture.open();
        assert_eq!(lib.store.dirty_count().unwrap(), if expected == "old" { 2 } else { 0 }, "{step:?}");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_failed_backup_before_a_restore_stops_it() {
    let fixture = Fixture::new();
    let mut lib = fixture.open();
    signed_in(&mut lib, ACCOUNT);
    let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
    let space = lib.spaces_ordered()[0].0.clone();
    lib.add_pinned(&space, None, "https://example.test/new", "Added after the backup").unwrap();
    lock(&fixture.folder(), true);
    let result = restore(&lib.store, &backup);
    lock(&fixture.folder(), false);
    assert!(matches!(&result, Err(RestoreError::Backup(error)) if error.starts_with("Couldn't create the backup file")), "{result:?}");
    assert_eq!(pending(&fixture.path()), None);
    assert!(!fixture.files().iter().any(|name| name.contains(".restore")), "{:?}", fixture.files());
    drop(lib);
    assert_eq!(finish_restore(&fixture.path()), Ok(false));
    assert_eq!(titles(&fixture.path()), vec!["Added after the backup", "Pin from the backup"]);
}

#[test]
fn cancelling_a_waiting_restore_keeps_the_library() {
    let fixture = Fixture::new();
    let mut lib = fixture.open();
    signed_in(&mut lib, ACCOUNT);
    let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
    let space = lib.spaces_ordered()[0].0.clone();
    lib.add_pinned(&space, None, "https://example.test/new", "Added after the backup").unwrap();
    restore(&lib.store, &backup).unwrap();
    assert!(pending(&fixture.path()).is_some());
    cancel_pending(&fixture.path()).unwrap();
    assert_eq!(pending(&fixture.path()), None);
    drop(lib);
    assert_eq!(finish_restore(&fixture.path()), Ok(false));
    assert_eq!(titles(&fixture.path()), vec!["Added after the backup", "Pin from the backup"]);
}

#[test]
fn a_damaged_copy_is_dropped_and_the_library_kept() {
    let fixture = Fixture::new();
    let lib = fixture.open();
    let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
    restore(&lib.store, &backup).unwrap();
    drop(lib);
    fs::write(staged_path(&fixture.path()), vec![0x5Au8; 8192]).unwrap();
    let error = finish_restore(&fixture.path()).unwrap_err();
    assert!(error.starts_with("The copy of the backup was damaged"), "{error}");
    assert_eq!(pending(&fixture.path()), None);
    assert!(fixture.open().spaces.len() == 1);
}

#[test]
fn a_library_from_before_format_marks_is_backed_up_before_its_upgrade() {
    let fixture = Fixture::new();
    // The archive as it was before profiles and sources, with no format mark.
    let conn = Connection::open(fixture.path()).unwrap();
    conn.execute_batch("PRAGMA journal_mode = WAL; CREATE TABLE archive (rowid INTEGER PRIMARY KEY AUTOINCREMENT, url TEXT NOT NULL, title TEXT NOT NULL, space_id TEXT NOT NULL, archived_ms INTEGER NOT NULL); INSERT INTO archive (url, title, space_id, archived_ms) VALUES ('https://example.test/legacy', 'Legacy', 'space', 1);").unwrap();
    drop(conn);
    let backup = before_upgrade(&fixture.path()).unwrap().unwrap();
    assert_eq!(backup.reason, Reason::Upgrade);
    let columns: Vec<String> = Connection::open(&backup.path).unwrap().prepare("SELECT name FROM pragma_table_info('archive')").unwrap()
        .query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert!(!columns.contains(&"profile_id".to_string()), "the backup is from before the upgrade");
    drop(Store::open(&fixture.path()).unwrap());
    let version: i64 = Connection::open(fixture.path()).unwrap().query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
    assert_eq!(version, LIBRARY_FORMAT);
    assert_eq!(before_upgrade(&fixture.path()), Ok(None), "once");
    assert_eq!(before_upgrade(&Fixture::new().path()), Ok(None), "a first launch has nothing to back up");
}

/// Every site permission and link grant in the library at `path`, as stored.
fn permission_rows(path: &Path) -> Vec<(String, String, String, String, String, i64)> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut statement = conn.prepare("SELECT profile_id, origin, top_origin, permission, decision, updated_ms FROM site_permissions ORDER BY 1, 2, 3, 4").unwrap();
    statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))).unwrap().map(Result::unwrap).collect()
}

/// A restore keeps this Mac's site permissions and links to other apps as
/// they are, whatever the backup held: a grant revoked since the backup
/// stays revoked, and one made since stays. Decisions are device-local, so
/// they are this Mac's, like its sync state, and never part of the restore.
/// A crash inside the restore's launch transaction changes none of them.
#[test]
fn a_restore_keeps_this_macs_site_permissions_and_never_brings_back_a_revoked_grant() {
    use crate::site_permissions::{self as rules, Decision, Permission};
    for crash_first in [false, true] {
        let fixture = Fixture::new();
        let mut lib = fixture.open();
        signed_in(&mut lib, ACCOUNT);
        let profile = lib.default_profile_id();
        let camera = |lib: &Library| rules::decision(&lib.store, &profile, "https://grant.example", "https://grant.example", Permission::Camera).unwrap();
        let opens = |lib: &Library| rules::opens_links(&lib.store, &profile, "https://meet.example", "zoommtg").unwrap();
        rules::set_decision(&lib.store, &profile, "https://grant.example", "https://grant.example", Permission::Camera, Some(Decision::Allow), THURSDAY).unwrap();
        rules::set_opens_links(&lib.store, &profile, "https://meet.example", "zoommtg", true, THURSDAY).unwrap();
        rules::set_decision(&lib.store, &profile, "https://frame.example", "https://top.example", Permission::Microphone, Some(Decision::Block), THURSDAY).unwrap();
        let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
        assert_eq!(permission_rows(&backup.path).len(), 3, "the backup holds the grants");
        let space = lib.spaces_ordered()[0].0.clone();
        lib.add_pinned(&space, None, "https://example.test/new", "Added after the backup").unwrap();

        // Since the backup: both grants revoked, the block turned into an
        // allow, and a new grant.
        rules::set_decision(&lib.store, &profile, "https://grant.example", "https://grant.example", Permission::Camera, None, THURSDAY + 1).unwrap();
        rules::set_opens_links(&lib.store, &profile, "https://meet.example", "zoommtg", false, THURSDAY + 1).unwrap();
        rules::set_decision(&lib.store, &profile, "https://frame.example", "https://top.example", Permission::Microphone, Some(Decision::Allow), THURSDAY + 2).unwrap();
        rules::set_decision(&lib.store, &profile, "https://new.example", "https://new.example", Permission::Notifications, Some(Decision::Allow), THURSDAY + 3).unwrap();
        assert_eq!((camera(&lib), opens(&lib)), (None, false));
        let current = permission_rows(&fixture.path());
        assert_eq!(current.len(), 2);

        if crash_first {
            crash_at(Some(Step::Restamped));
            restore(&lib.store, &backup).unwrap();
            drop(lib);
            assert!(finish_restore(&fixture.path()).is_err());
            crash_at(None);
            assert_eq!(permission_rows(&fixture.path()), current, "a crash in the launch transaction changes nothing");
            assert_eq!(finish_restore(&fixture.path()), Ok(true));
            lib = fixture.open();
        } else {
            lib = restore_and_relaunch(&fixture, lib, &backup);
        }
        assert_eq!(titles(&fixture.path()), vec!["Pin from the backup".to_owned()], "the library was restored");
        assert_eq!(permission_rows(&fixture.path()), current, "exactly this Mac's rows, as they were");
        assert_eq!(camera(&lib), None, "a camera grant revoked since the backup stays revoked");
        assert!(!opens(&lib), "a link grant revoked since the backup stays revoked");
        assert_eq!(rules::decision(&lib.store, &profile, "https://frame.example", "https://top.example", Permission::Microphone).unwrap(), Some(Decision::Allow));
        assert_eq!(rules::decision(&lib.store, &profile, "https://new.example", "https://new.example", Permission::Notifications).unwrap(), Some(Decision::Allow));
        drop(lib);
        let lib = fixture.open();
        assert_eq!(camera(&lib), None, "and after another relaunch");
    }
}

/// A backup made before site permissions existed has no table for them;
/// the restore still keeps this Mac's.
#[test]
fn a_backup_from_before_site_permissions_restores_with_this_macs_grants() {
    use crate::site_permissions::{self as rules, Decision, Permission};
    let fixture = Fixture::new();
    let mut lib = fixture.open();
    signed_in(&mut lib, ACCOUNT);
    let profile = lib.default_profile_id();
    let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
    Connection::open(&backup.path).unwrap().execute_batch("DROP TABLE site_permissions").unwrap();
    assert!(matches!(inspect(&backup.path), Check::Ready(_)));
    rules::set_decision(&lib.store, &profile, "https://grant.example", "https://grant.example", Permission::Camera, Some(Decision::Allow), THURSDAY).unwrap();
    let current = permission_rows(&fixture.path());
    let lib = restore_and_relaunch(&fixture, lib, &backup);
    assert_eq!(permission_rows(&fixture.path()), current);
    assert_eq!(rules::decision(&lib.store, &profile, "https://grant.example", "https://grant.example", Permission::Camera).unwrap(), Some(Decision::Allow));
}

// -- forgetting --------------------------------------------------------------

fn visit(lib: &Library, profile: &str, url: &str) {
    lib.store.record_visit(profile, url, "Visited", THURSDAY).unwrap();
}

fn archived(lib: &Library, profile: &str, url: &str) {
    lib.store.connection().execute("INSERT INTO archive (url, title, space_id, archived_ms, profile_id) VALUES (?1, 'Archived', 'space', 1, ?2)", params![url, profile]).unwrap();
}

/// Whether any byte of the file spells `needle`, freed pages included.
fn holds(path: &Path, needle: &str) -> bool {
    fs::read(path).unwrap().windows(needle.len()).any(|window| window == needle.as_bytes())
}

fn urls_at(path: &Path, table: &str, profile: &str) -> Vec<String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    conn.prepare(&format!("SELECT url FROM {table} WHERE profile_id = ?1 ORDER BY url")).unwrap()
        .query_map([profile], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
}

/// Every backup and the waiting restore copy is cleaned, private and checked.
fn cleaned(fixture: &Fixture, gone: &[&str]) {
    let staged = staged_path(&fixture.path());
    let copies: Vec<PathBuf> = list(&fixture.path()).into_iter().map(|backup| backup.path).chain(staged.exists().then_some(staged)).collect();
    assert!(!copies.is_empty());
    for path in &copies {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        for needle in gone { assert!(!holds(path, needle), "{name} still holds {needle}"); }
        assert!(matches!(inspect(path), Check::Ready(_)), "{name} no longer checks out");
        #[cfg(unix)]
        assert_eq!(mode(path), 0o600, "{name}");
    }
    assert!(!fixture.files().iter().any(|name| name.contains(PARTIAL)), "{:?}", fixture.files());
}

#[test]
fn clear_history_removes_it_from_every_backup_and_a_restore_never_brings_it_back() {
    let fixture = Fixture::new();
    let mut lib = fixture.open();
    let mine = lib.default_profile_id();
    let work = lib.create_named_profile("Work").unwrap();
    visit(&lib, &mine, "https://cleared.example/secret");
    visit(&lib, &work, "https://kept.example/work");
    let daily = create(&lib.store, Reason::Daily).unwrap().unwrap();
    let before = create(&lib.store, Reason::TidyToday).unwrap().unwrap();
    // A restore of the daily backup waits for the next launch.
    restore(&lib.store, &daily).unwrap();
    let count = list(&fixture.path()).len();
    assert!(count == 3 && list(&fixture.path()).iter().all(|backup| holds(&backup.path, "cleared.example")));
    assert!(holds(&staged_path(&fixture.path()), "cleared.example"));

    assert_eq!(lib.clear_history(&mine).unwrap(), Removed::default());
    cleaned(&fixture, &["cleared.example"]);
    assert_eq!(list(&fixture.path()).len(), count, "none was deleted");
    for backup in list(&fixture.path()) {
        assert_eq!(urls_at(&backup.path, "profile_history", &work), ["https://kept.example/work"], "another profile's history stays");
    }
    // The waiting restore finishes without it, and so does one chosen now.
    drop(lib);
    assert_eq!(finish_restore(&fixture.path()), Ok(true));
    let lib = fixture.open();
    assert!(lib.store.recent_history(&mine, 10).unwrap().is_empty());
    assert_eq!(lib.store.recent_history(&work, 10).unwrap().len(), 1);
    let lib = restore_and_relaunch(&fixture, lib, &before);
    assert!(lib.store.recent_history(&mine, 10).unwrap().is_empty());
    assert_eq!(lib.store.recent_history(&work, 10).unwrap().len(), 1);
}

#[test]
fn deleting_a_profile_removes_its_history_and_archive_from_every_backup() {
    let fixture = Fixture::new();
    let mut lib = fixture.open();
    let mine = lib.default_profile_id();
    let work = lib.create_named_profile("Work").unwrap();
    visit(&lib, &work, "https://visited.example/work");
    archived(&lib, &work, "https://archived.example/work");
    visit(&lib, &mine, "https://kept.example/mine");
    archived(&lib, &mine, "https://kept.example/archived");
    create(&lib.store, Reason::Daily).unwrap().unwrap();
    create(&lib.store, Reason::TidyToday).unwrap().unwrap();
    assert!(list(&fixture.path()).iter().all(|backup| holds(&backup.path, "visited.example") && holds(&backup.path, "archived.example")));

    assert_eq!(lib.delete_profile(&work).unwrap(), Removed::default());
    cleaned(&fixture, &["visited.example", "archived.example"]);
    for backup in list(&fixture.path()) {
        assert_eq!(urls_at(&backup.path, "profile_history", &mine), ["https://kept.example/mine"]);
        assert_eq!(urls_at(&backup.path, "archive", &mine), ["https://kept.example/archived"]);
    }
}

/// The library file and its log keep no byte of what Clear History or a
/// profile delete removed: freed space is zeroed, and the log is folded into
/// the file and emptied. A single row shares its page with kept rows; the
/// many fill pages that are freed whole.
#[test]
fn cleared_rows_leave_no_bytes_in_the_library_file_or_its_log() {
    use crate::site_permissions::{self as rules, Decision, Permission};
    for deleting in [false, true] {
        let fixture = Fixture::new();
        let mut lib = fixture.open();
        let mine = lib.default_profile_id();
        let work = lib.create_named_profile("Work").unwrap();
        let (gone, kept) = if deleting { (work.clone(), mine.clone()) } else { (mine.clone(), work.clone()) };
        visit(&lib, &kept, "https://kept.example/before");
        visit(&lib, &gone, "https://cleared.example/one");
        for n in 0..400 {
            lib.store.record_visit(&gone, &format!("https://cleared.example/{n}/{}", "x".repeat(100)), "Cleared", THURSDAY + n).unwrap();
        }
        if deleting {
            archived(&lib, &gone, "https://cleared.example/archived");
            rules::set_decision(&lib.store, &gone, "https://cleared.example", "https://cleared.example", Permission::Camera, Some(Decision::Allow), THURSDAY).unwrap();
        }
        visit(&lib, &kept, "https://kept.example/after");
        // Some of it is in the file and some only in the log.
        lib.store.connection().execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        visit(&lib, &gone, "https://cleared.example/one");
        let (file, log) = (fixture.path(), suffixed(&fixture.path(), "-wal"));
        assert!(holds(&file, "cleared.example") && holds(&log, "cleared.example"));

        if deleting { lib.delete_profile(&gone).unwrap(); } else { lib.clear_history(&gone).unwrap(); }
        assert!(!holds(&file, "cleared.example"), "the library file still holds it (deleting {deleting})");
        assert_eq!(fs::metadata(&log).map_or(0, |meta| meta.len()), 0, "the log is emptied (deleting {deleting})");
        assert!(holds(&file, "kept.example/before") && holds(&file, "kept.example/after"), "the other profile's history is in the file");
        assert_eq!(lib.store.recent_history(&kept, 10).unwrap().len(), 2);
        drop(lib);
        let lib = fixture.open();
        assert!(lib.store.recent_history(&gone, 10).unwrap().is_empty());
        assert_eq!(lib.store.recent_history(&kept, 10).unwrap().len(), 2);
    }
}

/// What secure_delete and the log fold cost on a large library: 400,000
/// history rows over two profiles, of which one profile's are cleared. Each
/// mode runs three times on a fresh copy and prints its timings.
#[test]
#[ignore = "measures secure_delete's cost on a large library; run by hand with --nocapture"]
fn secure_delete_cost_on_a_large_library() {
    use std::time::Instant;
    const ROWS: u64 = 200_000;
    let fixture = Fixture::new();
    let template = fixture.dir.path().join("template.sqlite");
    let (cleared, kept) = {
        let mut lib = Library::open(Store::open(&template).unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let (cleared, kept) = (lib.default_profile_id(), lib.create_named_profile("Work").unwrap());
        let conn = lib.store.connection();
        conn.execute_batch("BEGIN").unwrap();
        let mut insert = conn.prepare("INSERT INTO profile_history (profile_id, url, title, visit_count, last_visit_ms) VALUES (?1, ?2, ?3, 1, ?4)").unwrap();
        for n in 0..ROWS {
            for profile in [&cleared, &kept] {
                let url = format!("https://site-{}.example/articles/{n}/a-readable-slug-for-this-page", n % 5000);
                insert.execute(params![profile, url, format!("Article {n}, with a title of a usual length"), (THURSDAY + n) as i64]).unwrap();
            }
        }
        drop(insert);
        conn.execute_batch("COMMIT").unwrap();
        assert!(lib.store.fold_log().unwrap());
        (cleared, kept)
    };
    let megabytes = |path: &Path| fs::metadata(path).map_or(0.0, |meta| meta.len() as f64 / 1e6);
    println!("fixture: {} history rows, {ROWS} per profile, {:.1} MB", ROWS * 2, megabytes(&template));
    for secure in [false, true] {
        for run in 1..=3 {
            let path = fixture.dir.path().join("run.sqlite");
            fs::copy(&template, &path).unwrap();
            let store = Store::open(&path).unwrap();
            if !secure { store.connection().execute_batch("PRAGMA secure_delete = OFF").unwrap(); }
            let started = Instant::now();
            for n in 0..1000 { store.record_visit(&kept, &format!("https://new.example/{n}"), "New", THURSDAY + n).unwrap(); }
            for n in 0..1000 { store.record_visit(&kept, &format!("https://site-{}.example/articles/{n}/a-readable-slug-for-this-page", n % 5000), "", THURSDAY + ROWS + n).unwrap(); }
            let visits = started.elapsed();
            let started = Instant::now();
            store.clear_history(&cleared).unwrap();
            let clear = started.elapsed();
            let log = megabytes(&suffixed(&path, "-wal"));
            let started = Instant::now();
            assert!(store.fold_log().unwrap());
            let fold = started.elapsed();
            println!("secure_delete {}, run {run}: 2,000 visits {:.0} ms; clearing {ROWS} rows {:.0} ms, log {log:.1} MB; fold {:.0} ms; file {:.1} MB",
                if secure { "ON" } else { "OFF" }, visits.as_secs_f64() * 1e3, clear.as_secs_f64() * 1e3, fold.as_secs_f64() * 1e3, megabytes(&path));
            drop(store);
            remove_with_sidecars(&path);
        }
    }
}

/// A restore never brings back site permissions, but a backup still holds a
/// deleted profile's until it is cleaned like its history.
#[test]
fn deleting_a_profile_removes_its_site_permissions_from_every_backup() {
    use crate::site_permissions::{self as rules, Decision, Permission};
    let fixture = Fixture::new();
    let mut lib = fixture.open();
    let mine = lib.default_profile_id();
    let work = lib.create_named_profile("Work").unwrap();
    rules::set_decision(&lib.store, &work, "https://work-grant.example", "https://work-grant.example", Permission::Camera, Some(Decision::Allow), THURSDAY).unwrap();
    rules::set_opens_links(&lib.store, &work, "https://work-meet.example", "zoommtg", true, THURSDAY).unwrap();
    rules::set_decision(&lib.store, &mine, "https://kept-grant.example", "https://kept-grant.example", Permission::Camera, Some(Decision::Allow), THURSDAY).unwrap();
    let daily = create(&lib.store, Reason::Daily).unwrap().unwrap();
    create(&lib.store, Reason::TidyToday).unwrap().unwrap();
    restore(&lib.store, &daily).unwrap();
    let staged = staged_path(&fixture.path());
    let copies = || list(&fixture.path()).into_iter().map(|backup| backup.path).chain([staged.clone()]).collect::<Vec<_>>();
    assert_eq!(copies().len(), 4);
    assert!(copies().iter().all(|path| holds(path, "work-grant.example") && holds(path, "work-meet.example") && permission_rows(path).len() == 3));

    assert_eq!(lib.delete_profile(&work).unwrap(), Removed::default());
    cleaned(&fixture, &["work-grant.example", "work-meet.example"]);
    for path in copies() {
        let kept: Vec<String> = permission_rows(&path).into_iter().map(|row| row.0 + " " + &row.1).collect();
        assert_eq!(kept, [format!("{mine} https://kept-grant.example")], "another profile's stay");
    }
}

/// Sites with blocking off are kept in the library, so its backups hold them
/// too, until their profile is deleted.
#[test]
fn deleting_a_profile_removes_its_sites_with_blocking_off_from_every_backup() {
    use crate::content_blocking::exceptions;
    let fixture = Fixture::new();
    let mut lib = fixture.open();
    let mine = lib.default_profile_id();
    let work = lib.create_named_profile("Work").unwrap();
    exceptions::save(&lib.store, &work, &["work-unblocked.example".to_owned()].into()).unwrap();
    exceptions::save(&lib.store, &mine, &["kept-unblocked.example".to_owned()].into()).unwrap();
    let daily = create(&lib.store, Reason::Daily).unwrap().unwrap();
    create(&lib.store, Reason::TidyToday).unwrap().unwrap();
    restore(&lib.store, &daily).unwrap();
    let staged = staged_path(&fixture.path());
    let copies = || list(&fixture.path()).into_iter().map(|backup| backup.path).chain([staged.clone()]).collect::<Vec<_>>();
    assert_eq!(copies().len(), 4);
    assert!(copies().iter().all(|path| holds(path, "work-unblocked.example") && holds(path, "kept-unblocked.example")));

    assert_eq!(lib.delete_profile(&work).unwrap(), Removed::default());
    cleaned(&fixture, &["work-unblocked.example"]);
    for path in copies() {
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut statement = conn.prepare("SELECT k, v FROM kv WHERE k LIKE 'content_blocking.%' ORDER BY k").unwrap();
        let rows: Vec<(String, String)> = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().map(Result::unwrap).collect();
        assert_eq!(rows, [(exceptions::key(&mine), "[\"kept-unblocked.example\"]".to_owned())], "another profile's stay");
    }
}

/// The files beside the library and in the Backups folder whose bytes spell
/// `needle`, the library and its log aside.
fn copies_holding(fixture: &Fixture, needle: &str) -> Vec<String> {
    let own: Vec<PathBuf> = ["", "-wal", "-shm"].iter().map(|suffix| suffixed(&fixture.path(), suffix)).collect();
    let mut names: Vec<String> = [fixture.dir.path().to_path_buf(), fixture.folder()].iter()
        .flat_map(|dir| fs::read_dir(dir).into_iter().flatten().flatten())
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && !own.contains(path) && holds(path, needle))
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Copies a crash left unfinished hold what the library held and are never
/// listed: a backup's temporary file in Backups, and a restore copy never
/// put in place, each with a journal beside it. Cleaning deletes them.
#[test]
fn cleaning_deletes_unfinished_backup_and_restore_copies() {
    for deleting in [false, true] {
        let fixture = Fixture::new();
        let mut lib = fixture.open();
        let work = lib.create_named_profile("Work").unwrap();
        let profile = if deleting { work.clone() } else { lib.default_profile_id() };
        visit(&lib, &profile, "https://cleared.example/secret");
        let daily = create(&lib.store, Reason::Daily).unwrap().unwrap();
        crash_at(Some(Step::Staged));
        assert!(restore(&lib.store, &daily).is_err());
        crash_at(Some(Step::BackupWritten));
        assert!(create(&lib.store, Reason::TidyToday).is_err());
        crash_at(None);
        let restore_copy = suffixed(&staged_path(&fixture.path()), PARTIAL);
        let backup_copy = fs::read_dir(fixture.folder()).unwrap().flatten().map(|entry| entry.path())
            .find(|path| path.to_string_lossy().ends_with(PARTIAL)).expect("the crash left its temporary file");
        for copy in [&restore_copy, &backup_copy] {
            fs::write(suffixed(copy, "-journal"), "a journal of https://cleared.example/secret").unwrap();
        }
        assert_eq!(pending(&fixture.path()), None, "no restore waits");
        assert_eq!(copies_holding(&fixture, "cleared.example").len(), 6, "two backups, two unfinished copies and their journals: {:?}", fixture.files());

        let removed = if deleting { lib.delete_profile(&work).unwrap() } else { lib.clear_history(&profile).unwrap() };
        assert_eq!(removed, Removed::default());
        assert_eq!(copies_holding(&fixture, "cleared.example"), Vec::<String>::new(), "deleting {deleting}");
        assert!(!fixture.files().iter().any(|name| name.contains(PARTIAL)), "{:?}", fixture.files());
        assert_eq!(list(&fixture.path()).len(), 2, "both finished backups are cleaned, not deleted");
        cleaned(&fixture, &["cleared.example"]);
    }
}

/// An unfinished copy that can't be deleted still holds what was removed,
/// so it is counted with the backups that couldn't be cleaned.
#[cfg(target_os = "macos")]
#[test]
fn an_unfinished_copy_that_cant_be_deleted_is_counted() {
    let fixture = Fixture::new();
    let lib = fixture.open();
    let mine = lib.default_profile_id();
    visit(&lib, &mine, "https://cleared.example/secret");
    let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
    crash_at(Some(Step::BackupChecked));
    assert!(create(&lib.store, Reason::TidyToday).is_err());
    crash_at(None);
    let copy = fs::read_dir(fixture.folder()).unwrap().flatten().map(|entry| entry.path())
        .find(|path| path.to_string_lossy().ends_with(PARTIAL)).expect("the crash left its temporary file");
    fs::write(suffixed(&copy, "-journal"), "a journal of https://cleared.example/secret").unwrap();
    lock(&fixture.folder(), true);
    let removed = lib.clear_history(&mine).unwrap();
    lock(&fixture.folder(), false);
    assert_eq!(removed, Removed { deleted: 0, left: 2 }, "the backup and the unfinished copy, once with its journal");
    assert!(holds(&backup.path, "cleared.example"));
    assert_eq!(copies_holding(&fixture, "cleared.example").len(), 3);
}

#[test]
fn a_backup_that_cant_be_cleaned_is_deleted_and_counted() {
    let fixture = Fixture::new();
    let lib = fixture.open();
    let mine = lib.default_profile_id();
    visit(&lib, &mine, "https://cleared.example/secret");
    let good = create(&lib.store, Reason::Daily).unwrap().unwrap();
    // A copy that fails its integrity check can't be cleaned.
    let damaged = fixture.folder().join("library-20261001T090000Z-daily.sqlite");
    fs::copy(&good.path, &damaged).unwrap();
    damage_second_page(&damaged);
    assert_eq!(list(&fixture.path()).len(), 2);
    assert_eq!(lib.clear_history(&mine).unwrap(), Removed { deleted: 1, left: 0 });
    assert!(!damaged.exists());
    assert_eq!(list(&fixture.path()).iter().map(|backup| backup.path.clone()).collect::<Vec<_>>(), [good.path.clone()]);
    cleaned(&fixture, &["cleared.example"]);
}

#[cfg(target_os = "macos")]
#[test]
fn a_backup_that_can_be_neither_cleaned_nor_deleted_is_counted() {
    let fixture = Fixture::new();
    let lib = fixture.open();
    let mine = lib.default_profile_id();
    visit(&lib, &mine, "https://cleared.example/secret");
    let backup = create(&lib.store, Reason::Daily).unwrap().unwrap();
    lock(&fixture.folder(), true);
    let removed = lib.clear_history(&mine).unwrap();
    lock(&fixture.folder(), false);
    assert_eq!(removed, Removed { deleted: 0, left: 1 });
    assert!(holds(&backup.path, "cleared.example"), "the count is honest: it still holds the history");
    assert!(!fixture.files().iter().any(|name| name.contains(PARTIAL)));
}

#[test]
fn delete_all_removes_every_backup_and_the_waiting_restore() {
    let fixture = Fixture::new();
    let lib = fixture.open();
    let daily = create(&lib.store, Reason::Daily).unwrap().unwrap();
    create(&lib.store, Reason::TidyToday).unwrap().unwrap();
    restore(&lib.store, &daily).unwrap();
    assert_eq!(list(&fixture.path()).len(), 3);
    assert_eq!(delete_all(&fixture.path()), Removed { deleted: 3, left: 0 });
    assert!(list(&fixture.path()).is_empty());
    assert_eq!(pending(&fixture.path()), None);
    assert!(!fixture.files().iter().any(|name| name.contains(".restore") || name.contains(PARTIAL) || name.starts_with(PREFIX)), "{:?}", fixture.files());
    assert_eq!(delete_all(&fixture.path()), Removed::default());
    drop(lib);
    assert_eq!(finish_restore(&fixture.path()), Ok(false));
}
