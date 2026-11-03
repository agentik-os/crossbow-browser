//! Tidy Today's library writes: one commit to apply, one to undo, and nothing
//! written when a step fails or what it made has changed since.

use super::*;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct Fixture {
    _dir: TempDir,
    path: PathBuf,
    lib: Library,
    space: String,
    pin: String,
    folder: String,
    favorite: String,
}

fn open(path: &Path) -> Library {
    Library::open(Store::open(path).unwrap()).unwrap()
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tidy.sqlite");
    let mut lib = open(&path);
    lib.ensure_defaults().unwrap();
    let space = lib.spaces_ordered()[0].0.clone();
    let profile = lib.spaces[&space].profile_id.clone();
    let pin = lib.add_pinned(&space, None, "https://pinned.test/", "Pinned").unwrap();
    let folder = lib.create_folder(&space, None, "Existing").unwrap();
    lib.add_pinned(&space, Some(&folder), "https://inside.test/", "Inside").unwrap();
    let favorite = lib.add_favorite(&profile, "https://favorite.test/", "Favorite").unwrap();
    Fixture { _dir: dir, path, lib, space, pin, folder, favorite }
}

fn proposal() -> Vec<(String, Vec<(String, String)>)> {
    vec![
        ("Reading".into(), vec![("https://a.test/".into(), "A".into()), ("https://b.test/".into(), "B".into())]),
        ("Work".into(), vec![("https://c.test/".into(), "C".into())]),
    ]
}

/// Everything the library holds and the store has queued, for "nothing changed".
fn state(lib: &Library) -> (BTreeMap<String, Folder>, BTreeMap<String, Item>, Vec<EntityRow>, u64) {
    (lib.folders.clone(), lib.items.clone(), lib.store.all_entities().unwrap(), lib.store.dirty_count().unwrap())
}

fn reject(path: &Path, sql: &str) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection.execute_batch(sql).unwrap();
    connection
}

#[test]
fn tidy_adds_folders_after_the_pinned_area_and_survives_reopening() {
    let mut f = fixture();
    let before: Vec<String> = f.lib.pinned_children(&f.space, None).iter().map(|node| node.id().to_owned()).collect();
    let (existing_folders, existing_items) = (f.lib.folders.clone(), f.lib.items.clone());
    let dirty = f.lib.store.dirty_count().unwrap();
    let made = f.lib.tidy_into_folders(&f.space, &proposal()).unwrap();
    assert_eq!(made.len(), 2);
    assert_eq!(made.iter().map(|(_, items)| items.len()).collect::<Vec<_>>(), [2, 1]);
    assert_eq!(f.lib.store.dirty_count().unwrap(), dirty + 5, "the new folders and pins sync like any other");

    let mut reopened = open(&f.path);
    for lib in [&f.lib, &mut reopened] {
        let root: Vec<String> = lib.pinned_children(&f.space, None).iter().map(|node| node.id().to_owned()).collect();
        assert_eq!(root[..before.len()], before[..], "existing pins and folders keep their place");
        assert_eq!(root[before.len()..], [made[0].0.clone(), made[1].0.clone()], "new folders come last, in proposal order");
        assert_eq!(lib.folders[&made[0].0].name, "Reading");
        let inside: Vec<(String, String)> = lib.pinned_children(&f.space, Some(&made[0].0)).iter().map(|node| match node {
            PinnedNode::Item(_, item) => (item.url.clone(), item.title.clone()),
            PinnedNode::Folder(..) => panic!("no subfolders"),
        }).collect();
        assert_eq!(inside, proposal()[0].1);
        for (id, item) in &existing_items { assert_eq!(lib.items.get(id), Some(item), "existing item {id} unchanged"); }
        for (id, folder) in &existing_folders { assert_eq!(lib.folders.get(id), Some(folder)); }
        assert!(lib.items[&made[1].1[0]].custom_title.is_none());
    }
    assert!(f.lib.items.contains_key(&f.favorite) && f.lib.items.contains_key(&f.pin) && f.lib.folders.contains_key(&f.folder));
}

#[test]
fn a_failed_tidy_writes_nothing() {
    let mut f = fixture();
    let before = state(&f.lib);
    let _trigger = reject(&f.path, "CREATE TRIGGER reject_tidy BEFORE INSERT ON entities WHEN NEW.payload LIKE '%https://c.test/%' BEGIN SELECT RAISE(ABORT, 'fixture tidy failure'); END;");
    assert!(f.lib.tidy_into_folders(&f.space, &proposal()).is_err());
    assert_eq!(state(&f.lib), before, "the folders and pins made before the failure were rolled back");
    assert_eq!(state(&open(&f.path)), before);
    assert!(matches!(f.lib.tidy_into_folders("missing-space", &proposal()), Err(LibraryError::NotFound(_))));
    assert_eq!(state(&f.lib), before);
}

#[test]
fn undo_removes_exactly_what_tidy_made_in_one_commit() {
    let mut f = fixture();
    let before = (f.lib.folders.clone(), f.lib.items.clone());
    let made = f.lib.tidy_into_folders(&f.space, &proposal()).unwrap();
    f.lib.untidy_folders(&f.space, &made).unwrap();
    assert_eq!((f.lib.folders.clone(), f.lib.items.clone()), before);
    let reopened = open(&f.path);
    assert_eq!((reopened.folders.clone(), reopened.items.clone()), before, "the removal is saved and queued");
    let tombstones = f.lib.store.all_entities().unwrap().into_iter().filter(|row| row.deleted).count();
    assert_eq!(tombstones, 5);
}

#[test]
fn undo_refuses_after_later_changes_and_a_failed_undo_removes_nothing() {
    for change in ["extra-pin", "pin-moved-out", "pin-deleted", "subfolder", "folder-nested", "folder-deleted", "pin-favorited", "store-failure"] {
        let mut f = fixture();
        let made = f.lib.tidy_into_folders(&f.space, &proposal()).unwrap();
        let (folder, first) = (made[0].0.clone(), made[0].1[0].clone());
        let _trigger = match change {
            "extra-pin" => { f.lib.add_pinned(&f.space, Some(&folder), "https://later.test/", "Later").unwrap(); None }
            "pin-moved-out" => { f.lib.move_pinned(&first, &f.space, None, 0).unwrap(); None }
            "pin-deleted" => { f.lib.delete_item(&first).unwrap(); None }
            "subfolder" => { f.lib.create_folder(&f.space, Some(&folder), "Nested").unwrap(); None }
            "folder-nested" => { f.lib.update_folder(&folder, |x| x.parent_id = Some(f.folder.clone())).unwrap(); None }
            "folder-deleted" => { f.lib.delete_folder(&folder).unwrap(); None }
            "pin-favorited" => { f.lib.favorite_pinned(&first).unwrap(); None }
            "store-failure" => Some(reject(&f.path, &format!("CREATE TRIGGER reject_undo BEFORE UPDATE ON entities WHEN NEW.id = '{}' AND NEW.deleted = 1 BEGIN SELECT RAISE(ABORT, 'fixture undo failure'); END;", made[1].0))),
            _ => unreachable!(),
        };
        let before = state(&f.lib);
        assert!(f.lib.untidy_folders(&f.space, &made).is_err(), "undo after {change}");
        assert_eq!(state(&f.lib), before, "nothing removed after {change}");
        assert_eq!(state(&open(&f.path)), before);
    }
}
