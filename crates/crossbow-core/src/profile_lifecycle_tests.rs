//! Moving spaces between profiles, renaming and deleting profiles. Fixtures are
//! disposable SQLite files; no browser data is read.

use super::*;
use crate::store::TabRow;
use crossbow_proto::{Change, Encoding};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const FAVORITES: &str = "__favorites__";

struct Fixture {
    _dir: TempDir,
    path: PathBuf,
    lib: Library,
    /// The default profile and its two spaces.
    a: String,
    space: String,
    sibling: String,
    /// A second profile with its own favorite and no space.
    b: String,
    folder: String,
    root_pin: String,
    folder_pin: String,
    sibling_pin: String,
    a_favorite: String,
    b_favorite: String,
}

fn open(path: &Path) -> Library {
    Library::open(Store::open(path).unwrap()).unwrap()
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profile-lifecycle.sqlite");
    let mut lib = open(&path);
    lib.ensure_defaults().unwrap();
    let space = lib.spaces_ordered()[0].0.clone();
    let a = lib.spaces[&space].profile_id.clone();
    let sibling = lib.create_space("Sibling", "S", Theme::preset(1), &a).unwrap();
    let b = lib.create_profile("Work").unwrap();
    let folder = lib.create_folder(&space, None, "Reading").unwrap();
    let root_pin = lib.add_pinned(&space, None, "https://fixture.example/root", "Root").unwrap();
    let folder_pin = lib.add_pinned(&space, Some(&folder), "https://fixture.example/folder", "Folder").unwrap();
    let sibling_pin = lib.add_pinned(&sibling, None, "https://fixture.example/sibling", "Sibling").unwrap();
    let a_favorite = lib.add_favorite(&a, "https://fixture.example/a-favorite", "A favorite").unwrap();
    let b_favorite = lib.add_favorite(&b, "https://fixture.example/b-favorite", "B favorite").unwrap();
    for change in lib.store.dirty_changes(1000).unwrap() {
        lib.store.mark_clean(&change.kind, &change.id, &change.hlc).unwrap();
    }
    Fixture { _dir: dir, path, lib, a, space, sibling, b, folder, root_pin, folder_pin, sibling_pin, a_favorite, b_favorite }
}

fn tab(id: &str, space: &str, item: Option<&str>) -> TabRow {
    TabRow {
        id: id.into(), space_id: space.into(), item_id: item.map(str::to_owned),
        url: format!("https://fixture.example/{id}"), title: id.into(), order: "a0".into(),
        created_ms: 1, last_active_ms: 2,
    }
}

/// Everything a refused operation must leave untouched.
fn snapshot(lib: &Library) -> (Vec<crate::store::EntityRow>, Option<String>, u64, Vec<TabRow>) {
    (lib.store.all_entities().unwrap(), lib.store.kv_get(KV_LAST_HLC).unwrap(), lib.revision, lib.store.tabs().unwrap())
}

/// The item without its profile and the space version it took it from.
fn without_profile(item: &Item) -> Item {
    Item { profile_id: String::new(), space_version: None, ..item.clone() }
}

fn ids(nodes: Vec<PinnedNode>) -> Vec<String> {
    nodes.iter().map(|node| node.id().to_owned()).collect()
}

#[test]
fn space_profile_switch_moves_pins_keeps_layout_and_survives_reopen() {
    let mut f = fixture();
    let before_items = f.lib.items.clone();
    let before_folders = f.lib.folders.clone();
    let root = ids(f.lib.pinned_children(&f.space, None));
    let nested = ids(f.lib.pinned_children(&f.space, Some(&f.folder)));
    let saved = [tab("today", &f.space, None), tab("pinned", &f.space, Some(&f.root_pin))];
    for row in &saved { f.lib.store.put_tab(row).unwrap(); }

    f.lib.set_space_profile(&f.space, &f.b).unwrap();

    assert_eq!(f.lib.spaces[&f.space].profile_id, f.b);
    assert_eq!(f.lib.spaces[&f.sibling].profile_id, f.a, "other spaces keep their profile");
    for pin in [&f.root_pin, &f.folder_pin] {
        assert_eq!(f.lib.items[pin].profile_id, f.b, "pins follow their space");
        assert_eq!(without_profile(&f.lib.items[pin]), without_profile(&before_items[pin]), "URL, title, folder and order are kept");
    }
    for unchanged in [&f.sibling_pin, &f.a_favorite, &f.b_favorite] {
        assert_eq!(f.lib.items[unchanged], before_items[unchanged]);
    }
    assert_eq!(f.lib.folders, before_folders);
    assert_eq!(ids(f.lib.pinned_children(&f.space, None)), root);
    assert_eq!(ids(f.lib.pinned_children(&f.space, Some(&f.folder))), nested);
    assert_eq!(f.lib.favorites(&f.b).len(), 1, "the space now shows the new profile's favorites");
    assert_eq!(f.lib.store.tabs().unwrap(), saved.to_vec(), "saved tabs keep their space and item");
    assert_eq!(f.lib.spaces_using_profile(&f.b), vec![f.space.clone()]);
    assert_eq!(f.lib.spaces_using_profile(&f.a), vec![f.sibling.clone()]);

    // The space and both pins are queued for sync as ordinary JSON rows.
    let pushed = f.lib.store.dirty_changes(1000).unwrap();
    let mut queued: Vec<_> = pushed.iter().map(|change| (change.kind.as_str(), change.id.as_str())).collect();
    queued.sort();
    let mut expected = vec![(kinds::SPACE, f.space.as_str()), (kinds::ITEM, f.root_pin.as_str()), (kinds::ITEM, f.folder_pin.as_str())];
    expected.sort();
    assert_eq!(queued, expected);
    assert!(pushed.iter().all(|change| change.enc == Encoding::Json && !change.deleted && change.payload.contains(&format!("\"profile_id\":\"{}\"", f.b))));

    // Pins can now move within the space but not back across profiles.
    f.lib.move_pinned(&f.root_pin, &f.space, Some(&f.folder), 0).unwrap();
    assert!(matches!(f.lib.move_pinned(&f.root_pin, &f.sibling, None, 0), Err(LibraryError::InvalidMove(_))));
    // Deletion ownership checks agree with the moved rows.
    f.lib.delete_item_and_tabs(&f.folder_pin, &[], 10).unwrap();

    let (spaces, items) = (f.lib.spaces.clone(), f.lib.items.clone());
    drop(f.lib);
    let reopened = open(&f.path);
    assert_eq!(reopened.spaces, spaces);
    assert_eq!(reopened.items, items);
}

#[test]
fn pins_keep_the_space_version_their_profile_came_from_and_repair_never_goes_back_to_an_older_one() {
    let mut f = fixture();
    let version = |lib: &Library, space: &str| lib.store.entity_hlc(kinds::SPACE, space).unwrap().map(|hlc| hlc.encode());
    assert_eq!(f.lib.items[&f.sibling_pin].space_version, version(&f.lib, &f.sibling));
    f.lib.move_pinned(&f.sibling_pin, &f.space, None, 0).unwrap();
    assert_eq!(f.lib.items[&f.sibling_pin].space_version, version(&f.lib, &f.space), "a pin moved to another space takes that space's version");
    f.lib.favorite_pinned(&f.sibling_pin).unwrap();
    assert_eq!(f.lib.items[&f.sibling_pin].space_version, None);
    f.lib.set_space_profile(&f.space, &f.b).unwrap();
    let moved = version(&f.lib, &f.space).unwrap();
    assert_eq!(f.lib.items[&f.root_pin].space_version.as_ref(), Some(&moved));

    // A pin given its profile by a newer version of its space than this
    // library holds is left alone; one from an older version is repaired.
    let a = f.lib.items[&f.sibling_pin].profile_id.clone();
    let clock = Hlc::parse(&moved).unwrap();
    let newer = Hlc::new(clock.wall_ms + 1, 0, "elsewhere").encode();
    f.lib.update_item(&f.root_pin, |item| { item.profile_id = a.clone(); item.space_version = Some(newer); }).unwrap();
    assert_eq!(f.lib.transaction(|lib| lib.reconcile_profiles()).unwrap(), 0);
    let older = Hlc::new(clock.wall_ms - 1, 0, "elsewhere").encode();
    f.lib.update_item(&f.root_pin, |item| item.space_version = Some(older)).unwrap();
    assert_eq!(f.lib.transaction(|lib| lib.reconcile_profiles()).unwrap(), 1);
    assert_eq!(f.lib.items[&f.root_pin].profile_id, f.b);
    assert_eq!(f.lib.items[&f.root_pin].space_version.as_ref(), Some(&moved));
    assert_eq!(f.lib.transaction(|lib| lib.reconcile_profiles()).unwrap(), 0, "a consistent library writes nothing");

    let items = f.lib.items.clone();
    drop(f.lib);
    assert_eq!(open(&f.path).items, items);
}

/// A pin from before pins kept their Space's version gains it when this
/// device writes it while it matches the Space here, so a device holding a
/// newer version of the Space still repairs it (Z1). A pin that doesn't
/// match its Space here, a favorite, and a pin left alone stay without one.
#[test]
fn a_pin_this_device_edits_records_the_version_of_its_space_it_matches() {
    let mut f = fixture();
    let version = |lib: &Library, space: &str| lib.store.entity_hlc(kinds::SPACE, space).unwrap().map(|hlc| hlc.encode());
    let (space, pin, a, b) = (f.space.clone(), f.root_pin.clone(), f.a.clone(), f.b.clone());
    for id in [&f.root_pin, &f.folder_pin, &f.sibling_pin] {
        f.lib.put_item_as_older_crossbow(id, |item| item.space_version = None).unwrap();
    }
    f.lib.update_item(&pin, |item| item.custom_title = Some("Renamed".into())).unwrap();
    assert_eq!(f.lib.items[&pin].space_version, version(&f.lib, &space), "a rename records the version");
    f.lib.move_pinned(&f.folder_pin, &space, Some(&f.folder), 0).unwrap();
    assert_eq!(f.lib.items[&f.folder_pin].space_version, version(&f.lib, &space), "a reorder records it");
    let copy = f.lib.duplicate_item(&f.sibling_pin).unwrap();
    assert_eq!(f.lib.items[&copy].space_version, version(&f.lib, &f.sibling), "a duplicate records it");
    assert_eq!(f.lib.items[&f.sibling_pin].space_version, None, "a pin left alone is not rewritten");
    // A pin in another profile than its Space here makes no such claim.
    f.lib.put_item_as_older_crossbow(&pin, |item| { item.profile_id = b.clone(); item.space_version = None; }).unwrap();
    f.lib.update_item(&pin, |item| item.custom_title = Some("Elsewhere".into())).unwrap();
    assert_eq!(f.lib.items[&pin].space_version, None);
    assert_eq!(f.lib.items[&f.a_favorite].space_version, None);
    f.lib.update_item(&f.a_favorite.clone(), |item| item.custom_title = Some("Favorite".into())).unwrap();
    assert_eq!(f.lib.items[&f.a_favorite].space_version, None, "favorites have none");
    assert_eq!(f.lib.items[&f.a_favorite].profile_id, a);
}

#[test]
fn a_pin_an_older_crossbow_rewrites_is_repaired_once_per_version_and_profile_of_its_space() {
    let mut f = fixture();
    let (space, pin, a, b) = (f.space.clone(), f.root_pin.clone(), f.a.clone(), f.b.clone());
    let reconcile = |lib: &mut Library| lib.transaction(|lib| lib.reconcile_profiles()).unwrap();
    // An older Crossbow holding an earlier version of the Space puts the pin
    // back in that version's profile and drops the field it doesn't know.
    let older = |lib: &mut Library| lib.put_item_as_older_crossbow(&pin, |item| { item.profile_id = a.clone(); item.space_version = None; }).unwrap();

    // A newer version of the Space arrives: the pin is repaired once, and
    // not again for that version and profile after the older Crossbow
    // rewrote it, here or after a restart.
    f.lib.update_space(&space, |space| space.profile_id = b.clone()).unwrap();
    assert_eq!(reconcile(&mut f.lib), 2, "both pins in the Space");
    assert_eq!(f.lib.items[&pin].profile_id, b);
    older(&mut f.lib);
    assert_eq!(reconcile(&mut f.lib), 0, "the same basis was repaired again");
    drop(f.lib);
    f.lib = open(&f.path);
    assert_eq!(reconcile(&mut f.lib), 0, "the same basis was repaired again after a restart");
    assert_eq!(f.lib.items[&pin].profile_id, a);

    // Another version of the Space is a new basis: one more repair.
    f.lib.update_space(&space, |space| space.name = "Renamed".into()).unwrap();
    assert_eq!(reconcile(&mut f.lib), 1);
    assert_eq!(f.lib.items[&pin].profile_id, b);
    older(&mut f.lib);
    assert_eq!(reconcile(&mut f.lib), 0);

    // A pin that carries the field is repaired as before, and the note is
    // device-local: nothing but the pin and the Space waits to sync.
    let current = f.lib.store.entity_hlc(kinds::SPACE, &space).unwrap().unwrap();
    let earlier = Hlc::new(current.wall_ms - 1, 0, "elsewhere").encode();
    f.lib.update_item(&pin, |item| item.space_version = Some(earlier)).unwrap();
    assert_eq!(reconcile(&mut f.lib), 1);
    let queued: BTreeSet<_> = f.lib.store.dirty_changes(100).unwrap().into_iter().map(|change| change.kind).collect();
    assert_eq!(queued, BTreeSet::from([kinds::ITEM.to_string(), kinds::SPACE.to_string()]));
}

#[test]
fn space_profile_switch_failure_changes_nothing_and_retry_commits() {
    let mut f = fixture();
    let (spaces, items) = (f.lib.spaces.clone(), f.lib.items.clone());
    let before = snapshot(&f.lib);
    let connection = Connection::open(&f.path).unwrap();
    connection.execute_batch(&format!("CREATE TRIGGER reject_space_move BEFORE UPDATE ON entities WHEN NEW.id = '{}' BEGIN SELECT RAISE(ABORT, 'fixture move failure'); END;", f.space)).unwrap();

    assert!(f.lib.set_space_profile(&f.space, &f.b).is_err());

    // The pins were written before the space; the rollback must undo them too.
    assert_eq!(f.lib.spaces, spaces);
    assert_eq!(f.lib.items, items);
    assert_eq!(snapshot(&f.lib), before);
    assert_eq!(f.lib.store.dirty_count().unwrap(), 0);

    connection.execute_batch("DROP TRIGGER reject_space_move;").unwrap();
    f.lib.set_space_profile(&f.space, &f.b).unwrap();
    assert_eq!(f.lib.spaces[&f.space].profile_id, f.b);
    assert_eq!(f.lib.items[&f.root_pin].profile_id, f.b);
    drop(f.lib);
    assert_eq!(open(&f.path).spaces[&f.space].profile_id, f.b);
}

#[test]
fn space_profile_switch_refuses_missing_unreadable_and_same_profile_without_writes() {
    let mut f = fixture();
    let before = snapshot(&f.lib);
    assert!(matches!(f.lib.set_space_profile(&f.space, "missing-profile"), Err(LibraryError::NotFound(_))));
    assert!(matches!(f.lib.set_space_profile(&f.space, ""), Err(LibraryError::NotFound(_))));
    assert!(matches!(f.lib.set_space_profile("missing-space", &f.b), Err(LibraryError::NotFound(_))));
    f.lib.set_space_profile(&f.space, &f.a).unwrap();
    assert_eq!(snapshot(&f.lib), before, "refusals and no-op moves write nothing");

    // A profile whose stored policy is unreadable keeps its identity but is not a destination.
    let broken = "broken-profile";
    f.lib.store.put_entity(&crate::store::EntityRow {
        kind: kinds::PROFILE.into(), id: broken.into(), hlc: Hlc::new(1, 0, "fixture"), deleted: false,
        payload: r#"{"name":"Broken","created_ms":3,"preferences":{"auto_archive_hours":2}}"#.into(),
    }, false).unwrap();
    drop(f.lib);
    let mut lib = open(&f.path);
    let before = snapshot(&lib);
    assert!(matches!(lib.set_space_profile(&f.space, broken), Err(LibraryError::InvalidProfile(_))));
    assert!(matches!(lib.rename_profile(broken, "Fixed"), Err(LibraryError::InvalidProfile(_))));
    assert_eq!(snapshot(&lib), before);
}

#[test]
fn profile_names_are_trimmed_capped_unique_and_renames_persist() {
    let mut f = fixture();
    let preferences = ProfilePreferences { search_engine: Some(SearchEngine::Kagi), auto_archive_hours: Some(24) };
    f.lib.set_profile_preferences(&f.b, Some(preferences)).unwrap();
    let personal = f.lib.profiles[&f.a].name.clone();
    let before = snapshot(&f.lib);

    let message = |result: Result<()>| match result {
        Err(LibraryError::InvalidProfile(message)) => message,
        other => panic!("expected a refused name, got {other:?}"),
    };
    assert_eq!(message(f.lib.rename_profile(&f.b, "   ")), "Enter a profile name.");
    assert_eq!(message(f.lib.rename_profile(&f.b, &"x".repeat(PROFILE_NAME_MAX + 1))), "Use 40 characters or fewer.");
    assert_eq!(message(f.lib.rename_profile(&f.b, &personal.to_uppercase())), "Another profile already uses this name.");
    assert_eq!(message(f.lib.rename_profile(&f.b, "Two\nlines")), "Remove line breaks and other control characters.");
    assert!(matches!(f.lib.rename_profile("missing", "Anything"), Err(LibraryError::NotFound(_))));
    assert_eq!(snapshot(&f.lib), before, "refused renames write nothing");
    assert_eq!(f.lib.create_named_profile(&format!(" {} ", personal.to_lowercase())).unwrap_err().to_string(), "Another profile already uses this name.");
    assert_eq!(f.lib.create_named_profile("").unwrap_err().to_string(), "Enter a profile name.");
    assert_eq!(snapshot(&f.lib), before);

    f.lib.rename_profile(&f.b, "  Travel  ").unwrap();
    assert_eq!(f.lib.profiles[&f.b].name, "Travel");
    assert_eq!(f.lib.profiles[&f.b].preferences, Some(preferences), "renaming keeps preferences");
    f.lib.rename_profile(&f.b, "TRAVEL").unwrap();
    assert_eq!(f.lib.profiles[&f.b].name, "TRAVEL", "a profile may change the case of its own name");
    let longest = "é".repeat(PROFILE_NAME_MAX);
    f.lib.rename_profile(&f.b, &longest).unwrap();
    let pushed = f.lib.store.dirty_changes(100).unwrap();
    assert!(pushed.iter().any(|change| change.kind == kinds::PROFILE && change.id == f.b && change.payload.contains(&longest)));
    let created = f.lib.create_named_profile("  Errands ").unwrap();
    assert_eq!(f.lib.profiles[&created].name, "Errands");

    let profiles = f.lib.profiles.clone();
    drop(f.lib);
    assert_eq!(open(&f.path).profiles, profiles);
}

#[test]
fn profile_delete_refuses_used_and_last_profiles_without_writes() {
    let mut f = fixture();
    let before = snapshot(&f.lib);
    match f.lib.delete_profile(&f.a) {
        Err(LibraryError::InvalidProfile(message)) => assert!(message.contains("Spaces still use this profile"), "{message}"),
        other => panic!("a used profile must not be deleted: {other:?}"),
    }
    assert!(matches!(f.lib.delete_profile("missing"), Err(LibraryError::NotFound(_))));
    assert_eq!(snapshot(&f.lib), before);

    // With only one profile known, even an unused one stays.
    let dir = tempfile::tempdir().unwrap();
    let mut lib = open(&dir.path().join("single.sqlite"));
    lib.ensure_defaults().unwrap();
    let only = lib.default_profile_id();
    let space = lib.spaces_ordered()[0].0.clone();
    lib.update_space(&space, |space| space.profile_id = "synced-elsewhere".into()).unwrap();
    let before = snapshot(&lib);
    assert_eq!(lib.delete_profile(&only).unwrap_err().to_string(), "Keep at least one profile.");
    assert_eq!(snapshot(&lib), before);
}

#[test]
fn profile_delete_tombstones_profile_favorites_and_local_rows_then_survives_reopen() {
    let mut f = fixture();
    // Give the unused profile local history, archive and a saved favorite tab.
    let temporary = f.lib.create_space("Temporary", "T", Theme::preset(2), &f.b).unwrap();
    f.lib.store.archive_tab(&f.b, "https://fixture.example/archived-b", "Archived B", &temporary, 5).unwrap();
    f.lib.store.archive_tab(&f.a, "https://fixture.example/archived-a", "Archived A", &f.space, 6).unwrap();
    f.lib.delete_space(&temporary).unwrap();
    f.lib.store.record_visit(&f.b, "https://fixture.example/history-b", "History B", 7).unwrap();
    f.lib.store.record_visit(&f.a, "https://fixture.example/history-a", "History A", 8).unwrap();
    let second_b_favorite = f.lib.add_favorite(&f.b, "https://fixture.example/b-second", "B second").unwrap();
    let kept = [tab("a-favorite-tab", FAVORITES, Some(&f.a_favorite)), tab("today", &f.space, None)];
    for row in kept.iter().chain([&tab("b-favorite-tab", FAVORITES, Some(&f.b_favorite))]) { f.lib.store.put_tab(row).unwrap(); }
    for change in f.lib.store.dirty_changes(1000).unwrap() {
        f.lib.store.mark_clean(&change.kind, &change.id, &change.hlc).unwrap();
    }

    f.lib.delete_profile(&f.b).unwrap();

    assert!(!f.lib.profiles.contains_key(&f.b));
    assert!(!f.lib.items.contains_key(&f.b_favorite) && !f.lib.items.contains_key(&second_b_favorite));
    assert!(f.lib.items.contains_key(&f.a_favorite) && f.lib.items.contains_key(&f.root_pin));
    assert_eq!(f.lib.store.tabs().unwrap(), kept.to_vec(), "only the deleted profile's favorite tabs close");
    assert!(f.lib.store.recent_history(&f.b, 10).unwrap().is_empty());
    assert_eq!(f.lib.store.recent_history(&f.a, 10).unwrap().len(), 1);
    let connection = Connection::open(&f.path).unwrap();
    let archived = |profile: &str| connection.query_row("SELECT COUNT(*) FROM archive WHERE profile_id = ?1", [profile], |row| row.get::<_, u32>(0)).unwrap();
    assert_eq!(archived(&f.b), 0);
    assert_eq!(archived(&f.a), 1);

    let mut tombstones: Vec<_> = f.lib.store.dirty_changes(100).unwrap().into_iter()
        .map(|change| (change.kind, change.id, change.deleted)).collect();
    tombstones.sort();
    let mut expected = vec![
        (kinds::PROFILE.to_string(), f.b.clone(), true),
        (kinds::ITEM.to_string(), f.b_favorite.clone(), true),
        (kinds::ITEM.to_string(), second_b_favorite.clone(), true),
    ];
    expected.sort();
    assert_eq!(tombstones, expected);

    let (profiles, items) = (f.lib.profiles.clone(), f.lib.items.clone());
    drop(f.lib);
    let reopened = open(&f.path);
    assert_eq!(reopened.profiles, profiles);
    assert_eq!(reopened.items, items);
    assert_eq!(reopened.store.tabs().unwrap(), kept.to_vec());
}

#[test]
fn profile_delete_failure_changes_nothing_and_retry_commits() {
    let mut f = fixture();
    f.lib.store.put_tab(&tab("b-favorite-tab", FAVORITES, Some(&f.b_favorite))).unwrap();
    f.lib.store.record_visit(&f.b, "https://fixture.example/history-b", "History B", 7).unwrap();
    let (profiles, items) = (f.lib.profiles.clone(), f.lib.items.clone());
    let before = snapshot(&f.lib);
    let connection = Connection::open(&f.path).unwrap();
    connection.execute_batch(&format!("CREATE TRIGGER reject_profile_delete BEFORE UPDATE ON entities WHEN NEW.id = '{}' AND NEW.deleted = 1 BEGIN SELECT RAISE(ABORT, 'fixture delete failure'); END;", f.b)).unwrap();

    assert!(f.lib.delete_profile(&f.b).is_err());
    assert_eq!(f.lib.profiles, profiles);
    assert_eq!(f.lib.items, items);
    assert_eq!(snapshot(&f.lib), before);
    assert_eq!(f.lib.store.recent_history(&f.b, 10).unwrap().len(), 1, "history is kept when the batch fails");

    connection.execute_batch("DROP TRIGGER reject_profile_delete;").unwrap();
    f.lib.delete_profile(&f.b).unwrap();
    assert!(!f.lib.profiles.contains_key(&f.b));
    assert!(f.lib.store.tabs().unwrap().is_empty());
}

/// A deleted profile's site permissions and links to other apps go in the
/// commit that deletes it: a failed delete keeps them with everything else.
#[test]
fn profile_delete_removes_its_site_permissions_in_the_same_commit() {
    use crate::site_permissions::{self as rules, Decision, Permission};
    let mut f = fixture();
    for profile in [&f.a, &f.b] {
        rules::set_decision(&f.lib.store, profile, "https://grant.example", "https://grant.example", Permission::Camera, Some(Decision::Allow), 1).unwrap();
        rules::set_opens_links(&f.lib.store, profile, "https://meet.example", "zoommtg", true, 1).unwrap();
    }
    let rows = |lib: &Library, profile: &str| lib.store.connection()
        .query_row("SELECT COUNT(*) FROM site_permissions WHERE profile_id = ?1", [profile], |row| row.get::<_, i64>(0)).unwrap();
    assert_eq!((rows(&f.lib, &f.a), rows(&f.lib, &f.b)), (2, 2));
    let connection = Connection::open(&f.path).unwrap();
    connection.execute_batch(&format!("CREATE TRIGGER reject_profile_delete BEFORE UPDATE ON entities WHEN NEW.id = '{}' AND NEW.deleted = 1 BEGIN SELECT RAISE(ABORT, 'fixture delete failure'); END;", f.b)).unwrap();

    assert!(f.lib.delete_profile(&f.b).is_err());
    assert_eq!(rows(&f.lib, &f.b), 2, "a failed delete keeps them");
    connection.execute_batch("DROP TRIGGER reject_profile_delete;").unwrap();
    f.lib.delete_profile(&f.b).unwrap();
    assert_eq!((rows(&f.lib, &f.a), rows(&f.lib, &f.b)), (2, 0), "only the deleted profile's go");
    drop(f.lib);
    let reopened = open(&f.path);
    assert_eq!((rows(&reopened, &f.a), rows(&reopened, &f.b)), (2, 0));
}

#[test]
fn profile_lifecycle_rows_apply_on_another_library_through_the_sync_payloads() {
    let mut f = fixture();
    f.lib.set_space_profile(&f.space, &f.b).unwrap();
    f.lib.rename_profile(&f.b, "Renamed Work").unwrap();
    let errands = f.lib.create_named_profile("Errands").unwrap();
    f.lib.set_space_profile(&f.sibling, &errands).unwrap();
    f.lib.delete_profile(&f.a).unwrap();

    // A second device that saw the starting library receives every queued row.
    let dir = tempfile::tempdir().unwrap();
    let mut other = open(&dir.path().join("other.sqlite"));
    let all: Vec<ServerChange> = f.lib.store.all_entities().unwrap().into_iter().enumerate().map(|(seq, row)| ServerChange {
        seq: seq as u64 + 1, device_id: "first".into(),
        change: Change { kind: row.kind, id: row.id, hlc: row.hlc, deleted: row.deleted, enc: Encoding::Json, payload: row.payload },
    }).collect();
    other.apply_remote(&all).unwrap();
    assert_eq!(other.profiles, f.lib.profiles);
    assert_eq!(other.spaces, f.lib.spaces);
    assert_eq!(other.items, f.lib.items);
    assert_eq!(other.profiles[&f.b].name, "Renamed Work");
    assert!(!other.profiles.contains_key(&f.a));
    assert!(!other.items.contains_key(&f.a_favorite));
    assert_eq!(other.items[&f.sibling_pin].profile_id, errands);
}
