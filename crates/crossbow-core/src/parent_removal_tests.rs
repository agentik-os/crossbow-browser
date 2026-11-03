//! Deletion regressions use disposable SQLite files and deterministic abort triggers.

use super::*;
use crate::store::{ArchivedTab, HistoryEntry, TabRow};
use crossbow_proto::Change;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct Fixture {
    _dir: TempDir,
    path: PathBuf,
    lib: Library,
    profile: String,
    space: String,
    other_profile: String,
    other_space: String,
    parent: String,
    folder: String,
    child_folder: String,
    pins: Vec<String>,
}

fn open(path: &Path) -> Library {
    Library::open(Store::open(path).unwrap()).unwrap()
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("parent-removal.sqlite");
    let mut lib = open(&path);
    let profile = lib.create_profile("Profile A").unwrap();
    let space = lib.create_space("Space A", "A", Theme::preset(0), &profile).unwrap();
    let other_profile = lib.create_profile("Profile B").unwrap();
    let other_space = lib.create_space("Space B", "B", Theme::preset(1), &other_profile).unwrap();
    let parent = lib.create_folder(&space, None, "Parent").unwrap();
    let folder = lib.create_folder(&space, Some(&parent), "Delete me").unwrap();
    let child_folder = lib.create_folder(&space, Some(&folder), "Child folder").unwrap();
    let mut pins = Vec::new();
    for title in ["First pin", "Second pin"] {
        pins.push(lib.add_pinned(&space, Some(&folder), "https://fixture.example/pin", title).unwrap());
    }
    pins.sort();
    lib.add_pinned(&other_space, None, "https://other.example/pin", "Other pin").unwrap();
    lib.add_favorite(&other_profile, "https://other.example/favorite", "Other favorite").unwrap();
    lib.store.put_tab(&tab("other-tab", &other_space, None)).unwrap();
    lib.store.archive_tab(&other_profile, "https://other.example/archive", "Other archive", &other_space, 1).unwrap();
    lib.store.record_visit(&other_profile, "https://other.example/history", "Other history", 1).unwrap();
    lib.store.kv_set("ui_state", "fixture saved selection").unwrap();
    mark_clean(&lib);
    lib.add_favorite(&other_profile, "https://other.example/unsynced", "Unsent favorite").unwrap();
    Fixture { _dir: dir, path, lib, profile, space, other_profile, other_space, parent, folder, child_folder, pins }
}

fn tab(id: &str, space: &str, item: Option<&str>) -> TabRow {
    TabRow {
        id: id.into(), space_id: space.into(), item_id: item.map(str::to_owned),
        url: format!("https://fixture.example/{id}"), title: format!("Saved {id}"),
        order: id.into(), created_ms: 1, last_active_ms: 2,
    }
}

fn mark_clean(lib: &Library) {
    for change in lib.store.dirty_changes(usize::MAX >> 1).unwrap() {
        lib.store.mark_clean(&change.kind, &change.id, &change.hlc).unwrap();
    }
}

#[derive(Debug, PartialEq)]
struct Snapshot {
    memory: Value,
    entities: Vec<EntityRow>,
    dirty: Vec<Change>,
    clock: Option<Hlc>,
    tabs: Vec<TabRow>,
    tables: Vec<Vec<Vec<String>>>,
}

fn snapshot(lib: &Library, path: &Path) -> Snapshot {
    let mut dirty = lib.store.dirty_changes(usize::MAX >> 1).unwrap();
    dirty.sort_by(|a, b| (&a.kind, &a.id).cmp(&(&b.kind, &b.id)));
    let conn = Connection::open(path).unwrap();
    let tables = [
        "SELECT k, v FROM kv ORDER BY k",
        "SELECT rowid, url, title, space_id, archived_ms, profile_id FROM archive ORDER BY rowid",
        "SELECT profile_id, url, title, visit_count, last_visit_ms FROM profile_history ORDER BY profile_id, url",
        "SELECT url, title, visit_count, last_visit_ms FROM history ORDER BY url",
        "SELECT kind, id, hlc, reason FROM sync_rejections ORDER BY kind, id",
        "SELECT name, seq FROM sqlite_sequence ORDER BY name",
    ].iter().map(|sql| {
        let mut statement = conn.prepare(sql).unwrap();
        let count = statement.column_count();
        statement.query_map([], |row| {
            (0..count).map(|column| row.get_ref(column).map(|value| format!("{value:?}"))).collect()
        }).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
    }).collect();
    let mut tabs = lib.store.tabs().unwrap();
    tabs.sort_by(|a, b| a.id.cmp(&b.id));
    Snapshot {
        memory: json!({
            "profiles": lib.profiles, "spaces": lib.spaces, "folders": lib.folders,
            "items": lib.items, "boosts": lib.boosts, "settings": lib.settings,
        }),
        entities: lib.store.all_entities().unwrap(), dirty, clock: lib.last_hlc.clone(), tabs, tables,
    }
}

fn assert_unchanged(lib: &Library, path: &Path, before: &Snapshot, revision: u64) {
    let after = snapshot(lib, path);
    let reopened = snapshot(&open(path), path);
    assert!(
        after == *before && reopened == *before && lib.revision == revision,
        "failed deletion changed state: memory={}, entities={}, dirty={}, clock={}, tabs={}, saved_tables={}, revision={}, reopened={}",
        after.memory != before.memory, after.entities != before.entities,
        after.dirty != before.dirty, after.clock != before.clock, after.tabs != before.tabs,
        after.tables != before.tables, lib.revision != revision, reopened != *before,
    );
}

fn reopen_unchanged(f: &mut Fixture) {
    let before = snapshot(&f.lib, &f.path);
    let lib = std::mem::replace(&mut f.lib, Library::open(Store::open_in_memory().unwrap()).unwrap());
    drop(lib);
    f.lib = open(&f.path);
    assert_eq!(snapshot(&f.lib, &f.path), before);
}

fn reject_entity(path: &Path, id: &str, deleted: bool) {
    Connection::open(path).unwrap().execute_batch(&format!(
        "CREATE TRIGGER reject_removal BEFORE UPDATE ON entities WHEN NEW.id = '{id}' AND NEW.deleted = {} BEGIN SELECT RAISE(ABORT, 'fixture late entity failure'); END;",
        i32::from(deleted),
    )).unwrap();
}

fn remove_fault(path: &Path) {
    Connection::open(path).unwrap().execute_batch("DROP TRIGGER reject_removal").unwrap();
}

#[test]
fn space_parent_failure_restores_children_memory_disk_clock_and_dirty() {
    let mut f = fixture();
    reject_entity(&f.path, &f.space, true);
    let before = snapshot(&f.lib, &f.path);
    let revision = f.lib.revision;
    let error = f.lib.delete_space(&f.space).unwrap_err();
    assert!(error.to_string().contains("fixture late entity failure"));
    assert_unchanged(&f.lib, &f.path, &before, revision);
    remove_fault(&f.path);
    f.lib.delete_space(&f.space).unwrap();
    assert!(!f.lib.spaces.contains_key(&f.space));
    assert!(!f.lib.folders.contains_key(&f.folder));
    assert!(!f.lib.items.contains_key(&f.pins[0]));
    reopen_unchanged(&mut f);
}

#[test]
fn space_late_child_failure_restores_prior_tombstones() {
    let mut f = fixture();
    reject_entity(&f.path, &f.pins[1], true);
    let before = snapshot(&f.lib, &f.path);
    let revision = f.lib.revision;
    assert!(f.lib.delete_space(&f.space).is_err());
    assert_unchanged(&f.lib, &f.path, &before, revision);
    remove_fault(&f.path);
    f.lib.delete_space(&f.space).unwrap();
    reopen_unchanged(&mut f);
}

#[test]
fn folder_parent_failure_restores_children_memory_disk_clock_and_dirty() {
    let mut f = fixture();
    reject_entity(&f.path, &f.folder, true);
    let before = snapshot(&f.lib, &f.path);
    let revision = f.lib.revision;
    assert!(f.lib.delete_folder(&f.folder).is_err());
    assert_unchanged(&f.lib, &f.path, &before, revision);
    remove_fault(&f.path);
    f.lib.delete_folder(&f.folder).unwrap();
    for pin in &f.pins { assert_eq!(f.lib.items[pin].folder_id.as_ref(), Some(&f.parent)); }
    assert_eq!(f.lib.folders[&f.child_folder].parent_id.as_ref(), Some(&f.parent));
    reopen_unchanged(&mut f);
}

#[test]
fn folder_late_child_move_failure_restores_prior_moves() {
    let mut f = fixture();
    reject_entity(&f.path, &f.child_folder, false);
    let before = snapshot(&f.lib, &f.path);
    let revision = f.lib.revision;
    assert!(f.lib.delete_folder(&f.folder).is_err());
    assert_unchanged(&f.lib, &f.path, &before, revision);
    remove_fault(&f.path);
    f.lib.delete_folder(&f.folder).unwrap();
    reopen_unchanged(&mut f);
}

#[derive(Debug, PartialEq)]
struct OtherProfile {
    entities: Vec<EntityRow>,
    dirty: Vec<Change>,
    tabs: Vec<TabRow>,
    archive: Vec<ArchivedTab>,
    history: Vec<HistoryEntry>,
}

fn other_profile(f: &Fixture) -> OtherProfile {
    let mut ids: BTreeSet<_> = f.lib.items.iter().filter(|(_, item)| item.profile_id == f.other_profile)
        .map(|(id, _)| id.as_str()).collect();
    ids.extend([f.other_profile.as_str(), f.other_space.as_str()]);
    let mut dirty: Vec<_> = f.lib.store.dirty_changes(usize::MAX >> 1).unwrap().into_iter()
        .filter(|change| ids.contains(change.id.as_str())).collect();
    dirty.sort_by(|a, b| (&a.kind, &a.id).cmp(&(&b.kind, &b.id)));
    OtherProfile {
        entities: f.lib.store.all_entities().unwrap().into_iter().filter(|row| ids.contains(row.id.as_str())).collect(),
        dirty,
        tabs: f.lib.store.tabs().unwrap().into_iter().filter(|tab| tab.space_id == f.other_space).collect(),
        archive: f.lib.store.archive(&f.other_profile, 2001).unwrap(),
        history: f.lib.store.recent_history(&f.other_profile, 2001).unwrap(),
    }
}

fn space_tabs(f: &Fixture) -> Vec<TabRow> {
    for (id, item, url) in [
        ("00-live", None, "https://fixture.example/saved"),
        ("01-cold", None, "https://fixture.example/cold"),
        ("02-pin", Some(f.pins[0].as_str()), "https://fixture.example/pin-live"),
        ("03-empty", None, ""),
        ("04-blank", None, "about:blank"),
    ] {
        let mut row = tab(id, &f.space, item);
        row.url = url.into();
        f.lib.store.put_tab(&row).unwrap();
    }
    let mut live = tab("00-live", &f.space, None);
    live.url = "https://fixture.example/latest".into();
    live.title = "Latest display title".into();
    vec![live.clone(), live, tab("05-unsaved", &f.space, None)]
}

fn assert_space_removed(f: &Fixture, other: &OtherProfile) {
    assert!(!f.lib.spaces.contains_key(&f.space));
    assert!(f.lib.items.values().all(|item| item.space_id.as_deref() != Some(&f.space)));
    assert!(f.lib.folders.values().all(|folder| folder.space_id != f.space));
    assert!(f.lib.store.tabs().unwrap().iter().all(|tab| tab.space_id != f.space));
    let archive = f.lib.store.archive(&f.profile, 2001).unwrap();
    assert_eq!(archive.len(), 3, "saved, live and unsaved Today rows archive once, not pins or blank rows");
    let urls: BTreeSet<_> = archive.iter().map(|row| row.url.as_str()).collect();
    assert_eq!(urls, BTreeSet::from([
        "https://fixture.example/latest", "https://fixture.example/cold", "https://fixture.example/05-unsaved",
    ]));
    let latest = archive.iter().find(|row| row.url.ends_with("/latest")).unwrap();
    assert_eq!(latest.title, "Latest display title");
    assert!(archive.iter().all(|row| row.profile_id == f.profile && row.space_id == f.space));
    assert_eq!(&other_profile(f), other);
    assert_eq!(f.lib.store.kv_get("ui_state").unwrap().as_deref(), Some("fixture saved selection"));
}

#[test]
fn space_batch_parent_and_child_failures_restore_tabs_archives_and_entities() {
    for parent in [false, true] {
        let mut f = fixture();
        let live = space_tabs(&f);
        let other = other_profile(&f);
        reject_entity(&f.path, if parent { &f.space } else { &f.pins[1] }, true);
        let before = snapshot(&f.lib, &f.path);
        let revision = f.lib.revision;
        let error = f.lib.delete_space_and_tabs(&f.space, &live, 100).unwrap_err();
        assert!(error.to_string().contains("fixture late entity failure"));
        assert_unchanged(&f.lib, &f.path, &before, revision);
        reopen_unchanged(&mut f);
        remove_fault(&f.path);
        f.lib.delete_space_and_tabs(&f.space, &live, 101).unwrap();
        assert_space_removed(&f, &other);
        reopen_unchanged(&mut f);
        assert_space_removed(&f, &other);
        let after = snapshot(&f.lib, &f.path);
        let revision = f.lib.revision;
        assert!(f.lib.delete_space_and_tabs(&f.space, &live, 102).is_err());
        assert_unchanged(&f.lib, &f.path, &after, revision);
    }
}

#[test]
fn space_batch_later_tab_failure_restores_earlier_archive_and_close() {
    let mut f = fixture();
    let live = space_tabs(&f);
    let other = other_profile(&f);
    Connection::open(&f.path).unwrap().execute_batch(
        "CREATE TRIGGER reject_removal BEFORE DELETE ON tabs WHEN OLD.id = '01-cold' AND EXISTS (SELECT 1 FROM archive WHERE title = 'Latest display title') BEGIN SELECT RAISE(ABORT, 'fixture later tab close failure'); END;"
    ).unwrap();
    let before = snapshot(&f.lib, &f.path);
    let revision = f.lib.revision;
    let error = f.lib.delete_space_and_tabs(&f.space, &live, 100).unwrap_err();
    assert!(error.to_string().contains("fixture later tab close failure"));
    assert_unchanged(&f.lib, &f.path, &before, revision);
    remove_fault(&f.path);
    f.lib.delete_space_and_tabs(&f.space, &live, 101).unwrap();
    assert_space_removed(&f, &other);
    reopen_unchanged(&mut f);
    assert_space_removed(&f, &other);
}

#[test]
fn space_batch_commit_clock_and_archive_failures_are_retryable() {
    for failure in ["commit", "clock", "archive"] {
        let mut f = fixture();
        let live = space_tabs(&f);
        let other = other_profile(&f);
        let sql = match failure {
            "commit" => format!("CREATE TABLE fixture_parent (id INTEGER PRIMARY KEY); CREATE TABLE fixture_deferred (id INTEGER REFERENCES fixture_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER reject_removal AFTER UPDATE ON entities WHEN NEW.id = '{}' AND NEW.deleted = 1 BEGIN INSERT INTO fixture_deferred VALUES (1); END;", f.space),
            "clock" => "CREATE TRIGGER reject_removal BEFORE UPDATE ON kv WHEN NEW.k = 'hlc_last' BEGIN SELECT RAISE(ABORT, 'fixture clock failure'); END;".into(),
            "archive" => "CREATE TRIGGER reject_removal BEFORE INSERT ON archive WHEN NEW.url = 'https://fixture.example/cold' BEGIN SELECT RAISE(ABORT, 'fixture later archive failure'); END;".into(),
            _ => unreachable!(),
        };
        let conn = Connection::open(&f.path).unwrap();
        conn.execute_batch(&sql).unwrap();
        let before = snapshot(&f.lib, &f.path);
        let revision = f.lib.revision;
        assert!(f.lib.delete_space_and_tabs(&f.space, &live, 100).is_err(), "{failure}");
        assert_unchanged(&f.lib, &f.path, &before, revision);
        if failure == "commit" {
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM fixture_deferred", [], |row| row.get::<_, u32>(0)).unwrap(), 0);
        }
        remove_fault(&f.path);
        f.lib.delete_space_and_tabs(&f.space, &live, 101).unwrap();
        assert_space_removed(&f, &other);
        reopen_unchanged(&mut f);
        assert_space_removed(&f, &other);
    }
}

#[test]
fn space_batch_retention_and_pruning_failures_preserve_other_profiles_and_legacy_rows() {
    for failure in ["pruning", "parent"] {
        let mut f = fixture();
        let live = space_tabs(&f);
        let conn = Connection::open(&f.path).unwrap();
        for (profile, space, count) in [(&f.profile, &f.space, 2000), (&f.other_profile, &f.other_space, 1999)] {
            conn.execute(
                "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<?3) INSERT INTO archive (url, title, space_id, archived_ms, profile_id) SELECT 'https://old.example/' || x, 'Existing', ?2, x, ?1 FROM n",
                rusqlite::params![profile, space, count],
            ).unwrap();
        }
        conn.execute("INSERT INTO archive (url, title, space_id, archived_ms) VALUES ('https://legacy.example/', 'Legacy', ?1, 1)", [&f.space]).unwrap();
        let other = other_profile(&f);
        if failure == "pruning" {
            conn.execute_batch("CREATE TRIGGER reject_removal BEFORE DELETE ON archive BEGIN SELECT RAISE(ABORT, 'fixture archive pruning failure'); END;").unwrap();
        } else {
            reject_entity(&f.path, &f.space, true);
        }
        let before = snapshot(&f.lib, &f.path);
        let revision = f.lib.revision;
        assert!(f.lib.delete_space_and_tabs(&f.space, &live, 3000).is_err());
        assert_unchanged(&f.lib, &f.path, &before, revision);
        remove_fault(&f.path);
        f.lib.delete_space_and_tabs(&f.space, &live, 3001).unwrap();
        let archive = f.lib.store.archive(&f.profile, 2001).unwrap();
        assert_eq!(archive.len(), 2000);
        assert_eq!(archive.iter().filter(|row| row.archived_ms == 3001).count(), 3);
        assert_eq!(other_profile(&f), other);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM archive WHERE profile_id IS NULL AND url = 'https://legacy.example/'", [], |row| row.get::<_, u32>(0)).unwrap(), 1);
        reopen_unchanged(&mut f);
        assert_eq!(f.lib.store.archive(&f.profile, 2001).unwrap(), archive);
        assert_eq!(other_profile(&f), other);
    }
}

#[test]
fn item_batch_pin_and_favorite_failures_restore_every_tab_and_retry_without_archive() {
    for favorite in [false, true] {
        for failure in ["parent", "later_tab", "commit"] {
            let mut f = fixture();
            let item = if favorite {
                f.lib.add_favorite(&f.profile, "https://favorite.example/", "Favorite").unwrap()
            } else { f.pins[0].clone() };
            let space = if favorite { "__favorites__" } else { &f.space };
            let mut live = tab("00-item", space, Some(&item));
            f.lib.store.put_tab(&live).unwrap();
            f.lib.store.put_tab(&tab("01-item-cold", space, Some(&item))).unwrap();
            f.lib.store.put_tab(&tab("unrelated-today", &f.space, None)).unwrap();
            live.title = "Latest pin title".into();
            let live = vec![live, tab("02-item-unsaved", space, Some(&item))];
            let other = other_profile(&f);
            match failure {
                "parent" => reject_entity(&f.path, &item, true),
                "later_tab" => Connection::open(&f.path).unwrap().execute_batch("CREATE TRIGGER reject_removal BEFORE DELETE ON tabs WHEN OLD.id = '01-item-cold' AND NOT EXISTS (SELECT 1 FROM tabs WHERE id = '00-item') BEGIN SELECT RAISE(ABORT, 'fixture later item close failure'); END;").unwrap(),
                "commit" => Connection::open(&f.path).unwrap().execute_batch(&format!("CREATE TABLE fixture_parent (id INTEGER PRIMARY KEY); CREATE TABLE fixture_deferred (id INTEGER REFERENCES fixture_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER reject_removal AFTER UPDATE ON entities WHEN NEW.id = '{item}' AND NEW.deleted = 1 BEGIN INSERT INTO fixture_deferred VALUES (1); END;")).unwrap(),
                _ => unreachable!(),
            }
            let before = snapshot(&f.lib, &f.path);
            let revision = f.lib.revision;
            assert!(f.lib.delete_item_and_tabs(&item, &live, 100).is_err(), "{favorite} {failure}");
            assert_unchanged(&f.lib, &f.path, &before, revision);
            reopen_unchanged(&mut f);
            remove_fault(&f.path);
            f.lib.delete_item_and_tabs(&item, &live, 101).unwrap();
            assert!(!f.lib.items.contains_key(&item));
            assert!(f.lib.store.tabs().unwrap().iter().all(|tab| tab.item_id.as_deref() != Some(&item)));
            assert!(f.lib.store.tabs().unwrap().iter().any(|tab| tab.id == "unrelated-today"));
            assert!(f.lib.store.archive(&f.profile, 2001).unwrap().is_empty());
            assert_eq!(other_profile(&f), other);
            reopen_unchanged(&mut f);
            assert_eq!(other_profile(&f), other);
        }
    }
}

#[test]
fn folder_later_item_and_commit_failures_restore_moved_children() {
    for commit in [false, true] {
        let mut f = fixture();
        if commit {
            Connection::open(&f.path).unwrap().execute_batch(&format!("CREATE TABLE fixture_parent (id INTEGER PRIMARY KEY); CREATE TABLE fixture_deferred (id INTEGER REFERENCES fixture_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER reject_removal AFTER UPDATE ON entities WHEN NEW.id = '{}' AND NEW.deleted = 1 BEGIN INSERT INTO fixture_deferred VALUES (1); END;", f.folder)).unwrap();
        } else { reject_entity(&f.path, &f.pins[1], false); }
        let before = snapshot(&f.lib, &f.path);
        let revision = f.lib.revision;
        assert!(f.lib.delete_folder(&f.folder).is_err());
        assert_unchanged(&f.lib, &f.path, &before, revision);
        remove_fault(&f.path);
        f.lib.delete_folder(&f.folder).unwrap();
        for pin in &f.pins { assert_eq!(f.lib.items[pin].folder_id.as_ref(), Some(&f.parent)); }
        assert_eq!(f.lib.folders[&f.child_folder].parent_id.as_ref(), Some(&f.parent));
        reopen_unchanged(&mut f);
    }
}

#[test]
fn batch_deletion_rejects_wrong_parent_collisions_and_conflicting_duplicates() {
    for space_delete in [false, true] {
        for invalid in ["other_parent", "same_profile_space", "saved_collision", "cross_parent_saved", "changed_item", "duplicate", "empty_id"] {
            let mut f = fixture();
            let item = f.pins[0].clone();
            let mut live = vec![tab("00-input", &f.space, Some(&item))];
            match invalid {
                "other_parent" => live[0] = tab("other-tab", &f.other_space, None),
                "same_profile_space" => {
                    let other = f.lib.create_space("Same profile", "S", Theme::preset(0), &f.profile).unwrap();
                    live[0].space_id = other;
                }
                "saved_collision" => live[0].id = "other-tab".into(),
                "cross_parent_saved" => {
                    f.lib.store.put_tab(&tab("cold-foreign", &f.other_space, Some(&item))).unwrap();
                }
                "changed_item" => {
                    let mut saved = live[0].clone();
                    saved.item_id = Some(f.pins[1].clone());
                    f.lib.store.put_tab(&saved).unwrap();
                }
                "duplicate" => { let mut duplicate = live[0].clone(); duplicate.title.push_str(" conflicting"); live.push(duplicate); }
                "empty_id" => live[0].id.clear(),
                _ => unreachable!(),
            }
            let before = snapshot(&f.lib, &f.path);
            let revision = f.lib.revision;
            let result = if space_delete { f.lib.delete_space_and_tabs(&f.space, &live, 100) }
                else { f.lib.delete_item_and_tabs(&item, &live, 100) };
            assert!(result.is_err(), "{space_delete} {invalid}");
            assert_unchanged(&f.lib, &f.path, &before, revision);
        }
    }
}

#[test]
fn space_batch_rejects_foreign_missing_or_favorite_item_rows_in_its_space() {
    for invalid in ["foreign", "missing", "favorite", "profile"] {
        let mut f = fixture();
        let item = match invalid {
            "foreign" => f.lib.items.iter().find(|(_, item)| item.kind == ItemKind::Pinned && item.profile_id == f.other_profile).unwrap().0.clone(),
            "missing" => "missing-item".into(),
            "favorite" => f.lib.add_favorite(&f.profile, "https://fixture.example/favorite", "Favorite").unwrap(),
            "profile" => {
                f.lib.update_item(&f.pins[0], |item| item.profile_id = f.other_profile.clone()).unwrap();
                f.pins[0].clone()
            }
            _ => unreachable!(),
        };
        f.lib.store.put_tab(&tab("invalid-saved", &f.space, Some(&item))).unwrap();
        let before = snapshot(&f.lib, &f.path);
        let revision = f.lib.revision;
        assert!(f.lib.delete_space_and_tabs(&f.space, &[], 100).is_err(), "{invalid}");
        assert_unchanged(&f.lib, &f.path, &before, revision);
    }
}

#[test]
fn favorite_batch_rejects_real_space_rows_and_malformed_ownership() {
    for invalid in ["tab_space", "item_space", "item_folder", "missing_profile"] {
        let mut f = fixture();
        let item = f.lib.add_favorite(&f.profile, "https://fixture.example/favorite", "Favorite").unwrap();
        let mut row = tab("favorite-tab", "__favorites__", Some(&item));
        match invalid {
            "tab_space" => row.space_id = f.space.clone(),
            "item_space" => f.lib.update_item(&item, |item| item.space_id = Some(f.space.clone())).unwrap(),
            "item_folder" => f.lib.update_item(&item, |item| item.folder_id = Some(f.folder.clone())).unwrap(),
            "missing_profile" => f.lib.update_item(&item, |item| item.profile_id = "missing-profile".into()).unwrap(),
            _ => unreachable!(),
        }
        f.lib.store.put_tab(&row).unwrap();
        let before = snapshot(&f.lib, &f.path);
        let revision = f.lib.revision;
        assert!(f.lib.delete_item_and_tabs(&item, &[row], 100).is_err(), "{invalid}");
        assert_unchanged(&f.lib, &f.path, &before, revision);
    }
}

#[test]
fn entity_cascades_reject_cross_space_children_and_invalid_folder_paths() {
    for invalid in ["foreign_pin", "foreign_folder", "foreign_parent", "missing_parent", "cycle"] {
        let mut f = fixture();
        match invalid {
            "foreign_pin" => { f.lib.add_pinned(&f.other_space, Some(&f.folder), "https://other.example/cross-parent", "Wrong parent").unwrap(); }
            "foreign_folder" => { f.lib.create_folder(&f.other_space, Some(&f.folder), "Wrong parent").unwrap(); }
            "foreign_parent" => {
                let parent = f.lib.create_folder(&f.other_space, None, "Other parent").unwrap();
                f.lib.update_folder(&f.folder, |folder| folder.parent_id = Some(parent)).unwrap();
            }
            "missing_parent" => f.lib.update_folder(&f.folder, |folder| folder.parent_id = Some("missing".into())).unwrap(),
            "cycle" => f.lib.update_folder(&f.parent, |folder| folder.parent_id = Some(f.child_folder.clone())).unwrap(),
            _ => unreachable!(),
        }
        let before = snapshot(&f.lib, &f.path);
        let revision = f.lib.revision;
        assert!(f.lib.delete_folder(&f.folder).is_err(), "{invalid}");
        assert_unchanged(&f.lib, &f.path, &before, revision);
        assert!(f.lib.delete_space(&f.space).is_err(), "{invalid}");
        assert_unchanged(&f.lib, &f.path, &before, revision);
    }
}

#[test]
fn stale_or_malformed_persisted_ownership_is_rejected_without_touching_either_state() {
    for target in ["profile", "space", "item", "folder"] {
        for malformed in [false, true] {
            let mut f = fixture();
            let live = space_tabs(&f);
            let id = match target { "profile" => &f.profile, "space" => &f.space, "item" => &f.pins[0], "folder" => &f.folder, _ => unreachable!() };
            let payload = if malformed { "not-json".into() } else {
                let mut value: Value = serde_json::from_str(&f.lib.store.entity_payload(target, id).unwrap().unwrap()).unwrap();
                value[if target == "profile" { "name" } else if target == "folder" { "space_id" } else { "profile_id" }] = json!("changed-outside-library");
                value.to_string()
            };
            Connection::open(&f.path).unwrap().execute("UPDATE entities SET payload = ?1 WHERE kind = ?2 AND id = ?3", rusqlite::params![payload, target, id]).unwrap();
            let before = snapshot(&f.lib, &f.path);
            let persisted = snapshot(&open(&f.path), &f.path);
            let revision = f.lib.revision;
            assert!(f.lib.delete_space_and_tabs(&f.space, &live, 100).is_err(), "{target} {malformed}");
            assert_eq!(f.lib.revision, revision);
            assert_eq!(snapshot(&f.lib, &f.path), before);
            assert_eq!(snapshot(&open(&f.path), &f.path), persisted);
        }
    }
}

#[test]
fn metadata_only_item_and_space_deletion_do_not_close_or_archive_tabs() {
    for space_delete in [false, true] {
        let mut f = fixture();
        space_tabs(&f);
        let before_tabs = f.lib.store.tabs().unwrap();
        if space_delete { f.lib.delete_space(&f.space).unwrap(); }
        else { f.lib.delete_item(&f.pins[0]).unwrap(); }
        assert_eq!(f.lib.store.tabs().unwrap(), before_tabs);
        assert!(f.lib.store.archive(&f.profile, 2001).unwrap().is_empty());
        reopen_unchanged(&mut f);
        assert_eq!(f.lib.store.tabs().unwrap(), before_tabs);
    }
}

#[test]
fn latest_live_url_decides_archive_eligibility_and_saved_only_rows_still_close() {
    let mut f = fixture();
    let mut live = Vec::new();
    for (id, old_url, latest_url) in [
        ("was-page", "https://fixture.example/old", "about:blank"),
        ("was-blank", "about:blank", "https://fixture.example/latest"),
        ("now-empty", "https://fixture.example/old-empty", ""),
    ] {
        let mut row = tab(id, &f.space, None);
        row.url = old_url.into();
        f.lib.store.put_tab(&row).unwrap();
        row.url = latest_url.into();
        row.title = "Current display title".into();
        live.push(row);
    }
    f.lib.store.put_tab(&tab("saved-only", &f.space, None)).unwrap();
    let favorite = f.lib.add_favorite(&f.profile, "https://favorite.example/", "Kept favorite").unwrap();
    let favorite_tab = tab("kept-favorite", FAVORITES_SPACE, Some(&favorite));
    f.lib.store.put_tab(&favorite_tab).unwrap();
    f.lib.delete_space_and_tabs(&f.space, &live, 100).unwrap();
    let archive = f.lib.store.archive(&f.profile, 2001).unwrap();
    assert_eq!(archive.len(), 2);
    assert_eq!(archive.iter().map(|row| row.url.as_str()).collect::<BTreeSet<_>>(), BTreeSet::from([
        "https://fixture.example/latest", "https://fixture.example/saved-only",
    ]));
    assert_eq!(archive.iter().find(|row| row.url.ends_with("/latest")).unwrap().title, "Current display title");
    assert!(f.lib.items.contains_key(&favorite));
    assert!(f.lib.store.tabs().unwrap().contains(&favorite_tab));
    reopen_unchanged(&mut f);
    assert_eq!(f.lib.store.archive(&f.profile, 2001).unwrap(), archive);
}

#[test]
fn deleted_space_archive_restores_once_into_same_profile_after_reopen() {
    let mut f = fixture();
    let destination = f.lib.create_space("Surviving A", "A", Theme::preset(0), &f.profile).unwrap();
    let live = space_tabs(&f);
    f.lib.delete_space_and_tabs(&f.space, &live, 100).unwrap();
    reopen_unchanged(&mut f);
    let archived = f.lib.store.archive(&f.profile, 10).unwrap().remove(0);
    let before = snapshot(&f.lib, &f.path);
    let revision = f.lib.revision;
    assert!(f.lib.store.restore_archived(&f.other_profile, archived.rowid, "restore", &f.other_space, "a0", 101).unwrap().is_none());
    assert!(f.lib.store.restore_archived(&f.profile, archived.rowid, "restore", &f.other_space, "a0", 101).unwrap().is_none());
    assert_unchanged(&f.lib, &f.path, &before, revision);
    let restored = f.lib.store.restore_archived(&f.profile, archived.rowid, "restore", &destination, "a0", 102).unwrap().unwrap();
    assert_eq!(restored.space_id, destination);
    assert_eq!((&restored.url, &restored.title), (&archived.url, &archived.title));
    assert!(f.lib.store.restore_archived(&f.profile, archived.rowid, "duplicate", &destination, "a1", 103).unwrap().is_none());
    reopen_unchanged(&mut f);
    assert!(f.lib.store.tabs().unwrap().contains(&restored));
    assert_eq!(f.lib.store.archive(&f.profile, 10).unwrap().len(), 2);
}

#[test]
fn favorites_pseudo_space_cannot_be_deleted_even_with_a_colliding_entity() {
    let mut f = fixture();
    f.lib.put(kinds::SPACE, FAVORITES_SPACE, &f.lib.spaces[&f.space].clone()).unwrap();
    let item = f.lib.add_favorite(&f.profile, "https://favorite.example/", "Favorite").unwrap();
    let row = tab("favorite", FAVORITES_SPACE, Some(&item));
    f.lib.store.put_tab(&row).unwrap();
    let before = snapshot(&f.lib, &f.path);
    let revision = f.lib.revision;
    assert!(f.lib.delete_space_and_tabs(FAVORITES_SPACE, &[row], 100).is_err());
    assert!(f.lib.delete_space(FAVORITES_SPACE).is_err());
    assert_unchanged(&f.lib, &f.path, &before, revision);
}

#[test]
fn missing_targets_fail_without_creating_tombstones_or_closing_supplied_tabs() {
    let mut f = fixture();
    let live = space_tabs(&f);
    let before = snapshot(&f.lib, &f.path);
    let revision = f.lib.revision;
    assert!(f.lib.delete_item_and_tabs("missing", &live, 100).is_err());
    assert!(f.lib.delete_space_and_tabs("missing", &live, 100).is_err());
    assert!(f.lib.delete_space("missing").is_err());
    assert!(f.lib.delete_folder("missing").is_err());
    assert_unchanged(&f.lib, &f.path, &before, revision);
}
