//! Each profile's saved lists in a disposable data folder: adding, reading
//! back, refusals that keep the previous set, damage and removal.

use super::*;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const ADS: &str = "! Fixture ads list\n||ads.example^\n||track.example^$third-party\n##.banner\n";
const COOKIES: &str = "||consent.example^\n";
/// 2026-10-31 18:00:00 UTC.
const AT: u64 = 1_793_469_600_000;

fn folder(dir: &Path, profile: &str) -> std::path::PathBuf {
    let hex: String = profile.bytes().map(|byte| format!("{byte:02x}")).collect();
    dir.join(FOLDER).join(format!("Profile-{hex}"))
}

/// Every name in `folder`, hidden ones included.
fn names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder).map(|entries| entries.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    names.sort();
    names
}

#[test]
fn a_list_is_added_privately_and_read_back_after_reopening() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(load(dir.path(), "personal"), Saved::default(), "no lists until one is added");
    let (entry, compiled) = add(dir.path(), "personal", ListKind::Ads, "fixture-ads.txt", ADS.as_bytes(), AT).unwrap();
    assert_eq!(entry, ListEntry { id: crate::content_blocking::matcher::sha256_hex(ADS.as_bytes()), name: "fixture-ads.txt".into(), kind: ListKind::Ads, added_ms: AT, rules: 3, unsupported: 0 });
    assert_eq!((compiled.rules, compiled.id.as_str()), (3, entry.id.as_str()), "its element hiding rule counts");
    let (cookies, _) = add(dir.path(), "personal", ListKind::CookieBanners, "fixture-cookies.txt", COOKIES.as_bytes(), AT + 1).unwrap();

    let saved = load(dir.path(), "personal");
    assert_eq!(saved, Saved { lists: vec![entry.clone(), cookies.clone()], notice: None, bundled_off: Default::default() });
    assert_eq!(open(dir.path(), "personal", &entry).unwrap().rules, 3);
    let folder = folder(dir.path(), "personal");
    let mut expected = vec![format!("{}.txt", cookies.id), format!("{}.txt", entry.id), "lists.json".to_owned()];
    expected.sort();
    assert_eq!(names(&folder), expected);
    #[cfg(unix)]
    {
        assert_eq!(fs::metadata(&folder).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(folder.join(format!("{}.txt", entry.id))).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert_eq!(fs::read_to_string(folder.join(format!("{}.txt", entry.id))).unwrap(), ADS, "an exact copy");
    assert_eq!(load(dir.path(), "work"), Saved::default(), "other profiles keep their own");
}

#[test]
fn refused_files_leave_the_lists_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let (entry, _) = add(dir.path(), "personal", ListKind::Ads, "fixture-ads.txt", ADS.as_bytes(), AT).unwrap();
    let before = load(dir.path(), "personal");
    let cases: [(&[u8], AddError); 5] = [
        (b"\x00\x01binary", AddError::NotText),
        (b"||ok.example^\n\xff\xfe", AddError::NotText),
        (b"! only comments\n#?#.banner:has-text(x)\n/regex[0-9]/\n", AddError::NoRules),
        (b"", AddError::NoRules),
        (ADS.as_bytes(), AddError::Duplicate),
    ];
    for (bytes, expected) in cases {
        assert_eq!(add(dir.path(), "personal", ListKind::CookieBanners, "other.txt", bytes, AT).unwrap_err(), expected);
        assert_eq!(load(dir.path(), "personal"), before);
    }
    assert_eq!(names(&folder(dir.path(), "personal")), [format!("{}.txt", entry.id), "lists.json".into()], "nothing left behind");

    let big = dir.path().join("big.txt");
    fs::write(&big, vec![b'a'; MAX_BYTES as usize + 1]).unwrap();
    assert_eq!(read_file(&big).unwrap_err(), AddError::TooLarge);
    assert_eq!(read_file(&dir.path().join("missing.txt")).unwrap_err(), AddError::Unreadable);
    let small = dir.path().join("small.txt");
    fs::write(&small, COOKIES).unwrap();
    assert_eq!(read_file(&small).unwrap(), COOKIES.as_bytes());
    for error in [AddError::Unreadable, AddError::TooLarge, AddError::NotText, AddError::NoRules, AddError::Duplicate, AddError::Save("disk full".into())] {
        assert!(!error.to_string().is_empty() && error.to_string().ends_with('.'), "{error:?} is explained");
    }
    assert_eq!(AddError::TooLarge.to_string(), "This file is larger than 16 MB, so it wasn't added.");
}

#[test]
fn a_failed_save_keeps_the_previous_set() {
    let dir = tempfile::tempdir().unwrap();
    add(dir.path(), "personal", ListKind::Ads, "fixture-ads.txt", ADS.as_bytes(), AT).unwrap();
    let before = load(dir.path(), "personal");
    let folder = folder(dir.path(), "personal");
    // The manifest's partial file can't be made, so no save of it succeeds.
    let blocker = folder.join(".lists.json.partial");
    fs::create_dir(&blocker).unwrap();
    let error = add(dir.path(), "personal", ListKind::CookieBanners, "fixture-cookies.txt", COOKIES.as_bytes(), AT).unwrap_err();
    assert!(matches!(&error, AddError::Save(reason) if reason == "entity already exists"), "{error:?}");
    assert!(error.to_string().starts_with("Couldn't save the list (entity already exists)."));
    assert!(remove(dir.path(), "personal", &before.lists[0].id).is_err(), "a removal that can't be saved says so");
    fs::remove_dir(&blocker).unwrap();
    assert_eq!(load(dir.path(), "personal"), before);
    assert_eq!(names(&folder), [format!("{}.txt", before.lists[0].id), "lists.json".into()], "the new copy is gone and the old one kept");
}

#[test]
fn a_changed_copy_is_refused_and_the_others_still_open() {
    let dir = tempfile::tempdir().unwrap();
    let (ads, _) = add(dir.path(), "personal", ListKind::Ads, "fixture-ads.txt", ADS.as_bytes(), AT).unwrap();
    let (cookies, _) = add(dir.path(), "personal", ListKind::CookieBanners, "fixture-cookies.txt", COOKIES.as_bytes(), AT).unwrap();
    fs::write(folder(dir.path(), "personal").join(format!("{}.txt", ads.id)), "||changed.example^\n").unwrap();
    assert_eq!(open(dir.path(), "personal", &ads).unwrap_err(), "Couldn't read this list. Remove it, then add it again.");
    fs::remove_file(folder(dir.path(), "personal").join(format!("{}.txt", cookies.id))).unwrap();
    assert!(open(dir.path(), "personal", &cookies).is_err(), "a missing copy too");
    let (again, _) = add(dir.path(), "personal", ListKind::Ads, "again.txt", b"||again.example^\n", AT).unwrap();
    assert_eq!(open(dir.path(), "personal", &again).unwrap().rules, 1);
}

#[test]
fn a_damaged_or_newer_manifest_uses_no_list_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    add(dir.path(), "personal", ListKind::Ads, "fixture-ads.txt", ADS.as_bytes(), AT).unwrap();
    let folder = folder(dir.path(), "personal");
    fs::write(folder.join("lists.json"), "{ not json").unwrap();
    let saved = load(dir.path(), "personal");
    assert!(saved.lists.is_empty());
    assert_eq!(saved.notice.as_deref(), Some("Saved content blocking lists couldn't be read, so none are in use. Add them again."));
    assert!(names(&folder).iter().any(|name| name.starts_with("lists.json.damaged-")), "set aside, not deleted: {:?}", names(&folder));
    assert_eq!(load(dir.path(), "personal").notice, None, "said once");
    let (entry, _) = add(dir.path(), "personal", ListKind::Ads, "fixture-ads.txt", ADS.as_bytes(), AT).unwrap();
    assert_eq!(load(dir.path(), "personal").lists, [entry]);

    fs::write(folder.join("lists.json"), r#"{"version":99,"lists":[]}"#).unwrap();
    let saved = load(dir.path(), "personal");
    assert!(saved.lists.is_empty());
    assert_eq!(saved.notice.as_deref(), Some("Content blocking lists were saved by a newer version of Crossbow, so none are in use. Adding a list here replaces them."));
    assert_eq!(fs::read_to_string(folder.join("lists.json")).unwrap(), r#"{"version":99,"lists":[]}"#, "left as it is");
}

#[test]
fn removing_a_list_deletes_its_copy_and_deleting_a_profile_deletes_its_folder() {
    let dir = tempfile::tempdir().unwrap();
    let (a, _) = add(dir.path(), "personal", ListKind::Ads, "fixture-ads.txt", ADS.as_bytes(), AT).unwrap();
    let (b, _) = add(dir.path(), "work", ListKind::Ads, "fixture-ads.txt", ADS.as_bytes(), AT).unwrap();
    assert_eq!(a.id, b.id, "the same file in two profiles");
    remove(dir.path(), "personal", &a.id).unwrap();
    assert!(load(dir.path(), "personal").lists.is_empty());
    assert_eq!(names(&folder(dir.path(), "personal")), ["lists.json"]);
    assert_eq!(load(dir.path(), "work").lists, [b.clone()], "the other profile's copy stays");
    assert!(open(dir.path(), "work", &b).is_ok());
    remove_profile(dir.path(), "work").unwrap();
    assert!(!folder(dir.path(), "work").exists());
    remove_profile(dir.path(), "never-had-lists").unwrap();
    assert!(remove_profile(dir.path(), "../escape").is_err(), "only a profile's own folder");
}

/// Turning a bundled list off for a profile is kept in its manifest beside
/// its added lists; adding or removing a list keeps the choice.
#[test]
fn bundled_lists_turned_off_are_kept_with_the_profile_s_lists() {
    let dir = tempfile::tempdir().unwrap();
    assert!(load(dir.path(), "personal").bundled_off.is_empty(), "every bundled list is on until turned off");
    let off: std::collections::BTreeSet<String> = ["easyprivacy".to_owned()].into();
    set_bundled_off(dir.path(), "personal", &off).unwrap();
    assert_eq!(load(dir.path(), "personal").bundled_off, off);
    assert!(load(dir.path(), "work").bundled_off.is_empty(), "other profiles keep their own");
    let (entry, _) = add(dir.path(), "personal", ListKind::Ads, "fixture-ads.txt", ADS.as_bytes(), AT).unwrap();
    assert_eq!(load(dir.path(), "personal"), Saved { lists: vec![entry.clone()], notice: None, bundled_off: off.clone() }, "adding a list keeps the choice");
    remove(dir.path(), "personal", &entry.id).unwrap();
    assert_eq!(load(dir.path(), "personal").bundled_off, off, "so does removing one");
    set_bundled_off(dir.path(), "personal", &Default::default()).unwrap();
    assert!(load(dir.path(), "personal").bundled_off.is_empty());

    // A manifest from before bundled lists reads as all on; a name that
    // isn't a bundled list's id marks it damaged.
    let folder = folder(dir.path(), "personal");
    fs::write(folder.join("lists.json"), r#"{"version":1,"lists":[]}"#).unwrap();
    assert_eq!(load(dir.path(), "personal"), Saved::default());
    fs::write(folder.join("lists.json"), r#"{"version":1,"lists":[],"bundled_off":["../x"]}"#).unwrap();
    assert!(load(dir.path(), "personal").notice.is_some());
}

#[test]
fn a_failed_save_of_a_bundled_choice_keeps_the_previous_one() {
    let dir = tempfile::tempdir().unwrap();
    let off: std::collections::BTreeSet<String> = ["easylist".to_owned()].into();
    set_bundled_off(dir.path(), "personal", &off).unwrap();
    // The manifest's place is taken by a folder, so the rename fails.
    let folder = folder(dir.path(), "personal");
    fs::remove_file(folder.join("lists.json")).unwrap();
    fs::create_dir(folder.join("lists.json")).unwrap();
    let error = set_bundled_off(dir.path(), "personal", &Default::default()).unwrap_err();
    assert!(error.starts_with("Couldn't save the change."), "{error}");
    fs::remove_dir(folder.join("lists.json")).unwrap();

    // A manifest from a newer version is kept: its lists aren't ours to replace.
    let newer = r#"{"version":2,"lists":[],"bundled_off":[]}"#;
    fs::write(folder.join("lists.json"), newer).unwrap();
    assert!(set_bundled_off(dir.path(), "personal", &off).is_err());
    assert_eq!(fs::read_to_string(folder.join("lists.json")).unwrap(), newer);
}
