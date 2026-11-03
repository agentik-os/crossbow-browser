use super::*;
use std::os::unix::fs::PermissionsExt;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n not a real image, only bytes to keep";
/// 2026-10-29 15:04:05 UTC.
const AT: u64 = 1_793_286_245_000;

fn new<'a>(profile: &'a str, created_ms: u64) -> NewCapture<'a> {
    NewCapture { profile_id: profile, url: "https://news.example/story?id=7", title: "A story", created_ms, offset_secs: 0, width: 640, height: 360, png: PNG }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Every name in `folder`, hidden ones included.
fn names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder).map(|entries| entries.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    names.sort();
    names
}

#[test]
fn a_capture_is_written_privately_then_recorded_and_survives_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let captures = Captures::open(dir.path()).unwrap();
    let saved = captures.save(&new("personal", AT)).unwrap();
    assert_eq!((saved.file.as_str(), saved.url.as_str(), saved.title.as_str(), saved.width, saved.height), ("Capture 2026-10-29 at 15.04.05.png", "https://news.example/story?id=7", "A story", 640, 360));
    let folder = captures.folder("personal").unwrap();
    assert_eq!(folder, dir.path().join("Captures").join("Profile-706572736f6e616c"));
    assert_eq!(names(&folder), [saved.file.clone()], "no partial file is left");
    assert_eq!(fs::read(folder.join(&saved.file)).unwrap(), PNG);
    assert_eq!((mode(&folder.join(&saved.file)), mode(&folder), mode(&dir.path().join("Captures")), mode(&dir.path().join(FILE_NAME))), (0o600, 0o700, 0o700, 0o600));
    drop(captures);

    let reopened = Captures::open(dir.path()).unwrap();
    assert_eq!(reopened.list("personal").unwrap(), [saved.clone()]);
    assert!(reopened.file_exists(&saved));
    assert_eq!(reopened.get(saved.id), Some(saved));
}

/// The library and its backups never hold a capture, its record or its address.
#[test]
fn captures_stay_out_of_the_library_and_its_backups() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(&dir.path().join("crossbow.sqlite")).unwrap();
    let captures = Captures::open(dir.path()).unwrap();
    captures.save(&new("personal", AT)).unwrap();
    let backup = crate::backup::create(&store, crate::backup::Reason::Daily).unwrap().expect("a backup");
    for database in [dir.path().join("crossbow.sqlite"), backup.path.clone()] {
        let conn = Connection::open(&database).unwrap();
        let tables: Vec<String> = conn.prepare("SELECT name FROM sqlite_master").unwrap().query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect();
        assert!(!tables.iter().any(|table| table.contains("capture")), "{database:?}: {tables:?}");
        let text: String = conn.query_row("SELECT group_concat(v, ' ') FROM kv", [], |row| row.get::<_, Option<String>>(0)).unwrap().unwrap_or_default();
        assert!(!text.contains("news.example") && !text.contains("Capture "), "{database:?}");
    }
    assert!(names(&dir.path().join(crate::backup::FOLDER)).iter().all(|name| !name.ends_with(".png")));
}

#[test]
fn only_http_and_https_addresses_are_kept_and_never_a_sign_in() {
    assert_eq!(page_url("https://news.example/a?b=1#c"), "https://news.example/a?b=1#c");
    assert_eq!(page_url("http://localhost:8080/"), "http://localhost:8080/");
    assert_eq!(page_url("https://user:secret@site.example/"), "https://site.example/");
    for refused in ["data:text/html,<b>hi</b>", "file:///Users/someone/notes.html", "about:blank", "crossbow://settings", "javascript:alert(1)", "", "not a url"] {
        assert_eq!(page_url(refused), "", "{refused}");
    }
    let dir = tempfile::tempdir().unwrap();
    let captures = Captures::open(dir.path()).unwrap();
    let saved = captures.save(&NewCapture { url: "file:///Users/someone/private.html", ..new("personal", AT) }).unwrap();
    assert_eq!(saved.url, "");
    let long = "t".repeat(MAX_TITLE + 50);
    assert_eq!(captures.save(&NewCapture { title: &long, ..new("personal", AT) }).unwrap().title.chars().count(), MAX_TITLE);
}

#[test]
fn a_failure_at_any_step_of_a_save_leaves_no_file_and_no_record() {
    let dir = tempfile::tempdir().unwrap();
    let captures = Captures::open(dir.path()).unwrap();
    for at in [Step::Write, Step::Rename, Step::Record] {
        fail_at(Some(at));
        assert_eq!(captures.save(&new("personal", AT)), Err(SAVE_FAILED.to_string()), "{at:?}");
        fail_at(None);
        assert!(names(&captures.folder("personal").unwrap()).is_empty(), "{at:?} left a file");
        assert!(captures.list("personal").unwrap().is_empty(), "{at:?} left a record");
    }
    // A real failure: the Captures folder can't be made.
    let blocked = tempfile::tempdir().unwrap();
    fs::write(blocked.path().join(FOLDER), b"a file where the folder goes").unwrap();
    let captures = Captures::open(blocked.path()).unwrap();
    assert_eq!(captures.save(&new("personal", AT)), Err(SAVE_FAILED.to_string()));
    assert!(captures.list("personal").unwrap().is_empty());
    assert!(captures.save(&new("../escape", AT)).is_err() && captures.folder("../escape").is_none(), "an id that can't name a folder");
}

#[test]
fn each_profile_lists_only_its_own_captures_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let captures = Captures::open(dir.path()).unwrap();
    let first = captures.save(&new("personal", AT)).unwrap();
    let same_second = captures.save(&new("personal", AT + 400)).unwrap();
    let work = captures.save(&new("work", AT + 1000)).unwrap();
    let latest = captures.save(&new("personal", AT + 60_000)).unwrap();
    assert_eq!(same_second.file, "Capture 2026-10-29 at 15.04.05 2.png", "one second, two names");
    assert_eq!(captures.list("personal").unwrap(), [latest.clone(), same_second.clone(), first.clone()]);
    assert_eq!(captures.list("work").unwrap(), [work.clone()]);
    assert_ne!(captures.path(&work).unwrap().parent(), captures.path(&first).unwrap().parent(), "a folder for each profile");
    assert!(captures.list("nobody").unwrap().is_empty());
    assert_eq!(file_stem(AT, 2 * 3600), "Capture 2026-10-29 at 17.04.05", "local time");
}

#[test]
fn delete_removes_the_file_and_the_record_together_or_on_failure_keeps_both() {
    let dir = tempfile::tempdir().unwrap();
    let captures = Captures::open(dir.path()).unwrap();
    let saved = captures.save(&new("personal", AT)).unwrap();
    let folder = captures.folder("personal").unwrap();
    for at in [Step::MoveAside, Step::Forget] {
        fail_at(Some(at));
        assert_eq!(captures.delete(&saved), Err(DELETE_FAILED.to_string()), "{at:?}");
        fail_at(None);
        assert_eq!(names(&folder), [saved.file.clone()], "{at:?}: the file stays where it was");
        assert_eq!(captures.list("personal").unwrap(), [saved.clone()], "{at:?}: the record stays");
    }
    captures.delete(&saved).unwrap();
    assert!(names(&folder).is_empty() && captures.list("personal").unwrap().is_empty());
}

#[test]
fn a_missing_file_is_noticed_and_its_record_can_be_removed() {
    let dir = tempfile::tempdir().unwrap();
    let captures = Captures::open(dir.path()).unwrap();
    let saved = captures.save(&new("personal", AT)).unwrap();
    fs::remove_file(captures.path(&saved).unwrap()).unwrap();
    assert!(!captures.file_exists(&saved));
    assert_eq!(captures.list("personal").unwrap(), [saved.clone()], "the record is still listed");
    captures.remove_record(&saved).unwrap();
    assert!(captures.list("personal").unwrap().is_empty());
}

/// A save or delete cut short by a crash is finished or undone on the next open.
#[test]
fn reopening_clears_what_a_save_or_delete_cut_short_left_behind() {
    let dir = tempfile::tempdir().unwrap();
    let captures = Captures::open(dir.path()).unwrap();
    let kept = captures.save(&new("personal", AT)).unwrap();
    let forgotten = captures.save(&new("personal", AT + 1000)).unwrap();
    let folder = captures.folder("personal").unwrap();
    captures.conn.execute("DELETE FROM captures WHERE id = ?1", [forgotten.id]).unwrap();
    // Set aside by a delete: one still recorded, one whose record went.
    fs::rename(folder.join(&kept.file), folder.join(format!(".{}{DELETING}", kept.file))).unwrap();
    fs::rename(folder.join(&forgotten.file), folder.join(format!(".{}{DELETING}", forgotten.file))).unwrap();
    fs::write(folder.join(".Capture 2026-10-29 at 15.05.00.png.partial"), b"half").unwrap();
    drop(captures);
    let reopened = Captures::open(dir.path()).unwrap();
    assert_eq!(names(&folder), [kept.file.clone()]);
    assert!(reopened.file_exists(&kept));
}

/// A PNG header for `width` by `height`: all `adopt` reads.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
    bytes
}

#[test]
fn a_damaged_file_opens_as_damaged_and_never_as_an_error_that_stops_the_app() {
    let damage: [(&str, fn(&Path)); 4] = [
        ("not a database", |path| fs::write(path, b"this is not a database at all, only some text".repeat(100)).unwrap()),
        ("cut short", |path| { let bytes = fs::read(path).unwrap(); fs::write(path, &bytes[..bytes.len() / 2]).unwrap(); }),
        ("another table", |path| Connection::open(path).unwrap().execute_batch("CREATE TABLE notes (text TEXT);").unwrap()),
        ("other columns", |path| Connection::open(path).unwrap().execute_batch("DROP TABLE captures; CREATE TABLE captures (id INTEGER, name TEXT);").unwrap()),
    ];
    for (case, damage) in damage {
        let dir = tempfile::tempdir().unwrap();
        let captures = Captures::open(dir.path()).unwrap();
        for at in 0..40 { captures.save(&new("personal", AT + at * 1000)).unwrap(); }
        drop(captures);
        Connection::open(dir.path().join(FILE_NAME)).unwrap().execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        damage(&dir.path().join(FILE_NAME));
        assert_eq!(Captures::open(dir.path()).err(), Some(CapturesError::Damaged), "{case}");
        assert_eq!(CapturesError::Damaged.message(), DAMAGED);
    }
    // A folder that refuses is a storage error: the file may be fine.
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(Captures::open(dir.path()).err(), Some(CapturesError::Unavailable));
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    // A new, empty file is no damage.
    fs::write(dir.path().join(FILE_NAME), b"").unwrap();
    assert!(Captures::open(dir.path()).is_ok());
}

#[test]
fn move_aside_keeps_the_damaged_file_and_lists_the_capture_files_again() {
    let dir = tempfile::tempdir().unwrap();
    let captures = Captures::open(dir.path()).unwrap();
    let personal = captures.save(&NewCapture { png: &png(640, 360), ..new("personal", AT) }).unwrap();
    let work = captures.save(&NewCapture { png: &png(320, 200), ..new("work", AT) }).unwrap();
    drop(captures);
    let damaged = b"damaged beyond reading".repeat(200);
    fs::write(dir.path().join(FILE_NAME), &damaged).unwrap();
    fs::write(dir.path().join(format!("{FILE_NAME}-wal")), b"an old log").unwrap();
    // Not a capture: hidden, partial, or not a PNG.
    let folder = dir.path().join(FOLDER).join("Profile-706572736f6e616c");
    fs::write(folder.join("notes.txt"), b"text").unwrap();
    fs::write(folder.join("broken.png"), b"not a png").unwrap();

    let (name, listed) = move_aside(dir.path()).unwrap();
    assert!(name.starts_with("captures-damaged-") && name.ends_with(".sqlite"), "{name}");
    assert_eq!(listed, 2);
    assert_eq!(fs::read(dir.path().join(&name)).unwrap(), damaged, "kept whole");
    assert_eq!(fs::read(dir.path().join(format!("{name}-wal"))).unwrap(), b"an old log", "its log went with it, never into the new file");
    let reopened = Captures::open(dir.path()).unwrap();
    let again = reopened.list("personal").unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!((again[0].file.as_str(), again[0].url.as_str(), again[0].title.as_str(), again[0].width, again[0].height), (personal.file.as_str(), "", "", 640, 360));
    assert_eq!(reopened.list("work").unwrap()[0].file, work.file);
    assert!(reopened.save(&new("personal", AT + 5000)).is_ok(), "and works");
    // A second damage gets its own name.
    drop(reopened);
    fs::write(dir.path().join(FILE_NAME), &damaged).unwrap();
    let (second, _) = move_aside(dir.path()).unwrap();
    assert_ne!(second, name);
}

#[test]
fn deleting_a_profile_sets_its_captures_aside_then_removes_them_or_puts_them_back() {
    let dir = tempfile::tempdir().unwrap();
    assert!(detach_profile(dir.path(), "work").unwrap().is_none(), "no captures at all");
    assert!(!dir.path().join(FILE_NAME).exists(), "and none made");
    let captures = Captures::open(dir.path()).unwrap();
    let kept = captures.save(&new("personal", AT)).unwrap();
    let work = captures.save(&new("work", AT)).unwrap();
    let (personal_folder, work_folder) = (captures.folder("personal").unwrap(), captures.folder("work").unwrap());

    let detached = detach_profile(dir.path(), "work").unwrap().unwrap();
    assert!(!work_folder.exists() && dir.path().join(FOLDER).join(".Profile-776f726b.deleting").join(&work.file).exists());
    detached.restore().unwrap();
    assert_eq!(names(&work_folder), [work.file.clone()], "put back");
    assert_eq!(names(&dir.path().join(FOLDER)), ["Profile-706572736f6e616c", "Profile-776f726b"]);

    detach_profile(dir.path(), "work").unwrap().unwrap().remove().unwrap();
    assert!(captures.list("work").unwrap().is_empty(), "its records are gone");
    assert_eq!(names(&dir.path().join(FOLDER)), ["Profile-706572736f6e616c"], "and its files, with nothing left aside");
    assert_eq!(captures.list("personal").unwrap(), [kept.clone()]);
    assert_eq!(names(&personal_folder), [kept.file.clone()]);

    // Records whose files are all gone still go.
    let gone = captures.save(&new("gone", AT)).unwrap();
    fs::remove_dir_all(captures.folder("gone").unwrap()).unwrap();
    detach_profile(dir.path(), "gone").unwrap().unwrap().remove().unwrap();
    assert!(captures.list("gone").unwrap().is_empty() && captures.get(gone.id).is_none());

    // A damaged file refuses before anything moves.
    drop(captures);
    fs::write(dir.path().join(FILE_NAME), b"damaged".repeat(500)).unwrap();
    assert_eq!(detach_profile(dir.path(), "personal").unwrap_err(), PROFILE_DAMAGED);
    assert_eq!(names(&personal_folder), [kept.file]);
}

/// A stop between setting a profile's captures aside and finishing.
#[test]
fn a_deletion_a_stop_cut_short_is_finished_or_undone_at_launch() {
    let dir = tempfile::tempdir().unwrap();
    let captures = Captures::open(dir.path()).unwrap();
    let kept = captures.save(&new("personal", AT)).unwrap();
    captures.save(&new("work", AT)).unwrap();
    drop(detach_profile(dir.path(), "work").unwrap());
    drop(detach_profile(dir.path(), "personal").unwrap());
    // Work's deletion committed; Personal's never did.
    finish_deletions(dir.path(), |profile| profile == "personal");
    assert!(captures.list("work").unwrap().is_empty());
    assert_eq!(captures.list("personal").unwrap(), [kept.clone()]);
    assert_eq!(names(&dir.path().join(FOLDER)), ["Profile-706572736f6e616c"]);
    assert_eq!(names(&captures.folder("personal").unwrap()), [kept.file]);
}

/// Records go only after their files: a file that won't go keeps every
/// record and the set-aside folder, and the next launch finishes.
#[test]
fn a_deleted_profiles_records_outlive_any_file_that_wont_go() {
    let dir = tempfile::tempdir().unwrap();
    let captures = Captures::open(dir.path()).unwrap();
    captures.save(&new("work", AT)).unwrap();
    captures.save(&new("work", AT + 1000)).unwrap();
    let locked = captures.folder("work").unwrap().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("file"), b"kept").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();

    assert_eq!(detach_profile(dir.path(), "work").unwrap().unwrap().remove().unwrap_err(), DELETE_FAILED);
    assert_eq!(captures.list("work").unwrap().len(), 2, "every record stays");
    let aside = dir.path().join(FOLDER).join(".Profile-776f726b.deleting");
    assert!(aside.join("locked").join("file").exists());

    // Still locked: the launch removes what it can and keeps the records.
    finish_deletions(dir.path(), |_| false);
    assert_eq!(captures.list("work").unwrap().len(), 2);
    fs::set_permissions(aside.join("locked"), fs::Permissions::from_mode(0o700)).unwrap();
    finish_deletions(dir.path(), |_| false);
    assert!(captures.list("work").unwrap().is_empty());
    assert_eq!(names(&dir.path().join(FOLDER)), Vec::<String>::new());
}
