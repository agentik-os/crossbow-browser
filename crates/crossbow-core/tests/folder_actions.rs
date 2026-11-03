//! Real Library/SQLite regressions for the sidebar's folder and pinned-item menu actions.

use crossbow_core::library::{Library, PinnedNode};
use crossbow_core::model::{ItemKind, Theme};
use crossbow_core::proto::ServerChange;
use crossbow_core::store::Store;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn open(path: &Path) -> Library {
    Library::open(Store::open(path).unwrap()).unwrap()
}

fn fixture() -> (TempDir, PathBuf, Library, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut lib = open(&path);
    lib.ensure_defaults().unwrap();
    let space = lib.spaces_ordered()[0].0.clone();
    (dir, path, lib, space)
}

/// The space's pinned tree as readable lines, depth-first in sidebar order.
fn tree(lib: &Library, space: &str) -> Vec<String> {
    fn walk(lib: &Library, space: &str, folder: Option<&str>, depth: usize, out: &mut Vec<String>) {
        for node in lib.pinned_children(space, folder) {
            match node {
                PinnedNode::Item(_, item) => out.push(format!("{}{}", "  ".repeat(depth), item.display_title())),
                PinnedNode::Folder(id, f) => {
                    out.push(format!("{}[{}{}]", "  ".repeat(depth), f.name, if f.collapsed { " collapsed" } else { "" }));
                    walk(lib, space, Some(&id), depth + 1, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(lib, space, None, 0, &mut out);
    out
}

fn favorites(lib: &Library) -> Vec<String> {
    lib.favorites(&lib.default_profile_id()).into_iter().map(|(_, item)| item.display_title().to_owned()).collect()
}

/// Apply every queued local row to an empty library, as another device would.
fn replay(lib: &Library) -> Library {
    let changes = lib.store.dirty_changes(lib.store.dirty_count().unwrap() as usize).unwrap();
    let pulled: Vec<ServerChange> = changes.into_iter().enumerate()
        .map(|(seq, change)| ServerChange { seq: seq as u64 + 1, device_id: "other-device".into(), change }).collect();
    let mut other = Library::open(Store::open_in_memory().unwrap()).unwrap();
    assert_eq!(other.apply_remote(&pulled).unwrap(), pulled.len());
    other
}

fn assert_round_trips(path: &Path, lib: &Library, space: &str) {
    let expected = (tree(lib, space), favorites(lib));
    assert_eq!((tree(&open(path), space), favorites(&open(path))), expected, "reopened SQLite library");
    let other = replay(lib);
    assert_eq!((tree(&other, space), favorites(&other)), expected, "sync payloads on another device");
}

#[test]
fn new_folder_inside_expands_its_parent_and_round_trips() {
    let (_dir, path, mut lib, space) = fixture();
    let outer = lib.create_folder(&space, None, "Work").unwrap();
    lib.add_pinned(&space, Some(&outer), "https://a.example/", "A").unwrap();
    lib.update_folder(&outer, |f| f.collapsed = true).unwrap();
    let inner = lib.create_folder_in(&outer, "Inner").unwrap();
    assert_eq!(lib.folders[&inner].parent_id.as_deref(), Some(outer.as_str()));
    assert_eq!(lib.folders[&inner].space_id, space);
    assert_eq!(tree(&lib, &space), ["[Work]", "  A", "  [Inner]"]);
    assert_round_trips(&path, &lib, &space);

    let entities = lib.store.all_entities().unwrap();
    let dirty = lib.store.dirty_count().unwrap();
    assert!(lib.create_folder_in("missing-folder", "Lost").is_err());
    assert_eq!((lib.store.all_entities().unwrap(), lib.store.dirty_count().unwrap()), (entities, dirty), "a missing parent writes nothing");
}

#[test]
fn new_folder_with_an_item_takes_its_place_and_round_trips() {
    let (_dir, path, mut lib, space) = fixture();
    lib.add_pinned(&space, None, "https://a.example/", "A").unwrap();
    let b = lib.add_pinned(&space, None, "https://b.example/", "B").unwrap();
    lib.add_pinned(&space, None, "https://c.example/", "C").unwrap();
    let folder = lib.create_folder_with_item(&b, "Reading").unwrap();
    assert_eq!(tree(&lib, &space), ["A", "[Reading]", "  B", "C"]);
    assert_eq!(lib.items[&b].folder_id.as_deref(), Some(folder.as_str()));

    // Inside a folder, the new folder nests where the item was.
    let nested = lib.create_folder_with_item(&b, "Later").unwrap();
    assert_eq!(lib.folders[&nested].parent_id.as_deref(), Some(folder.as_str()));
    assert_eq!(tree(&lib, &space), ["A", "[Reading]", "  [Later]", "    B", "C"]);
    assert_round_trips(&path, &lib, &space);

    let favorite = lib.add_favorite(&lib.default_profile_id(), "https://f.example/", "F").unwrap();
    let entities = lib.store.all_entities().unwrap();
    assert!(lib.create_folder_with_item(&favorite, "Nope").is_err());
    assert!(lib.create_folder_with_item("missing-item", "Nope").is_err());
    assert_eq!(lib.store.all_entities().unwrap(), entities, "refused requests write nothing");
}

#[test]
fn duplicate_lands_right_after_the_original_and_round_trips() {
    let (_dir, path, mut lib, space) = fixture();
    let folder = lib.create_folder(&space, None, "Work").unwrap();
    let a = lib.add_pinned(&space, Some(&folder), "https://a.example/", "A").unwrap();
    lib.add_pinned(&space, Some(&folder), "https://b.example/", "B").unwrap();
    lib.update_item(&a, |item| item.custom_title = Some("Renamed A".into())).unwrap();
    let copy = lib.duplicate_item(&a).unwrap();
    assert_ne!(copy, a);
    assert_eq!(lib.items[&copy].url, "https://a.example/");
    assert_eq!(lib.items[&copy].folder_id.as_deref(), Some(folder.as_str()));
    assert_eq!(tree(&lib, &space), ["[Work]", "  Renamed A", "  Renamed A", "  B"]);
    // The last row duplicates to the end of its list.
    let last = lib.add_pinned(&space, None, "https://z.example/", "Z").unwrap();
    lib.duplicate_item(&last).unwrap();
    assert_eq!(tree(&lib, &space), ["[Work]", "  Renamed A", "  Renamed A", "  B", "Z", "Z"]);

    let profile = lib.default_profile_id();
    let f1 = lib.add_favorite(&profile, "https://f1.example/", "F1").unwrap();
    lib.add_favorite(&profile, "https://f2.example/", "F2").unwrap();
    let favorite_copy = lib.duplicate_item(&f1).unwrap();
    assert_eq!(lib.items[&favorite_copy].kind, ItemKind::Favorite);
    assert_eq!(favorites(&lib), ["F1", "F1", "F2"]);
    assert_round_trips(&path, &lib, &space);
    assert!(lib.duplicate_item("missing-item").is_err());
}

#[test]
fn favorite_pinned_keeps_the_id_leaves_the_tree_and_round_trips() {
    let (_dir, path, mut lib, space) = fixture();
    let profile = lib.default_profile_id();
    lib.add_favorite(&profile, "https://f.example/", "F").unwrap();
    let folder = lib.create_folder(&space, None, "Work").unwrap();
    let pin = lib.add_pinned(&space, Some(&folder), "https://p.example/", "P").unwrap();
    lib.favorite_pinned(&pin).unwrap();
    let item = &lib.items[&pin];
    assert_eq!((item.kind, item.space_id.as_deref(), item.folder_id.as_deref()), (ItemKind::Favorite, None, None));
    assert_eq!(item.profile_id, profile);
    assert_eq!(tree(&lib, &space), ["[Work]"]);
    assert_eq!(favorites(&lib), ["F", "P"]);
    assert_round_trips(&path, &lib, &space);

    let entities = lib.store.all_entities().unwrap();
    assert!(lib.favorite_pinned(&pin).is_err(), "already a favorite");
    assert!(lib.favorite_pinned("missing-item").is_err());
    assert_eq!(lib.store.all_entities().unwrap(), entities);
}

#[test]
fn folder_actions_stay_inside_their_space() {
    let (_dir, _path, mut lib, space) = fixture();
    let profile = lib.default_profile_id();
    let other = lib.create_space("Other", "O", Theme::preset(1), &profile).unwrap();
    let folder = lib.create_folder(&other, None, "Elsewhere").unwrap();
    let child = lib.create_folder_in(&folder, "Child").unwrap();
    assert_eq!(lib.folders[&child].space_id, other);
    assert!(tree(&lib, &space).is_empty());
    assert_eq!(tree(&lib, &other), ["[Elsewhere]", "  [Child]"]);
}
