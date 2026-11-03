//! Real Library/SQLite regressions for the shared pinned-tree move operation.

use crossbow_core::library::Library;
use crossbow_core::model::Theme;
use crossbow_core::proto::Change;
use crossbow_core::store::{EntityRow, Store};
use rusqlite::Connection;
use serde_json::{Value, json};
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

#[derive(Debug, PartialEq)]
struct Snapshot {
    memory: Value,
    entities: Vec<EntityRow>,
    dirty: Vec<Change>,
    clock: Option<String>,
}

fn snapshot(lib: &Library) -> Snapshot {
    let mut dirty = lib.store.dirty_changes(lib.store.dirty_count().unwrap() as usize).unwrap();
    dirty.sort_by(|a, b| (&a.kind, &a.id).cmp(&(&b.kind, &b.id)));
    Snapshot {
        memory: json!({
            "profiles": lib.profiles,
            "spaces": lib.spaces,
            "folders": lib.folders,
            "items": lib.items,
            "boosts": lib.boosts,
            "settings": lib.settings,
        }),
        entities: lib.store.all_entities().unwrap(),
        dirty,
        clock: lib.store.kv_get("hlc_last").unwrap(),
    }
}

fn mark_clean(lib: &Library) {
    for change in lib.store.dirty_changes(lib.store.dirty_count().unwrap() as usize).unwrap() {
        lib.store.mark_clean(&change.kind, &change.id, &change.hlc).unwrap();
    }
}

fn children(lib: &Library, space: &str, parent: Option<&str>) -> Vec<String> {
    lib.pinned_children(space, parent).iter().map(|node| node.id().to_owned()).collect()
}

fn reject_and_reopen(mut lib: Library, path: &Path, node: &str, space: &str, parent: Option<&str>) -> Library {
    let before = snapshot(&lib);
    let revision = lib.revision;
    let result = lib.move_pinned(node, space, parent, usize::MAX);
    let after = snapshot(&lib);
    let after_revision = lib.revision;
    drop(lib);
    let reopened = open(path);
    assert!(
        result.is_err(),
        "move unexpectedly accepted: node={node}, parent={parent:?}; persisted folders={:?}; root={:?}",
        reopened.folders,
        children(&reopened, space, None),
    );
    assert_eq!(after_revision, revision, "rejected moves must not bump the revision");
    assert_eq!(after, before, "rejected moves must not mutate memory, clock or push queue");
    assert_eq!(snapshot(&reopened), before, "rejected moves must leave SQLite unchanged on reopen");
    reopened
}

fn reopen_unchanged(lib: Library, path: &Path) -> Library {
    let before = snapshot(&lib);
    drop(lib);
    let reopened = open(path);
    assert_eq!(snapshot(&reopened), before);
    reopened
}

#[test]
fn rejects_self_and_descendant_folder_moves_without_persisting_a_cycle() {
    let (_dir, path, mut lib, space) = fixture();
    let a = lib.create_folder(&space, None, "A").unwrap();
    let b = lib.create_folder(&space, Some(&a), "B").unwrap();
    let c = lib.create_folder(&space, Some(&b), "C").unwrap();
    let pin = lib.add_pinned(&space, Some(&c), "https://pin.example/", "Pin").unwrap();
    mark_clean(&lib);

    // The direct-descendant case reproduced A -> B -> A on the old implementation,
    // including after closing/reopening SQLite, with no folder at the space root.
    for parent in [&b, &c, &a] {
        lib = reject_and_reopen(lib, &path, &a, &space, Some(parent));
    }
    assert_eq!(children(&lib, &space, None), vec![a.clone()]);
    assert_eq!(children(&lib, &space, Some(&a)), vec![b.clone()]);
    assert_eq!(children(&lib, &space, Some(&b)), vec![c.clone()]);
    assert_eq!(children(&lib, &space, Some(&c)), vec![pin]);

    // A rejected drag must not prevent a subsequent legal move/retry.
    lib.move_pinned(&c, &space, Some(&a), 0).unwrap();
    let lib = reopen_unchanged(lib, &path);
    assert_eq!(children(&lib, &space, Some(&a)), vec![c, b]);
}

#[test]
fn rejects_missing_deleted_nonfolder_and_wrong_space_destinations() {
    let (_dir, path, mut lib, space) = fixture();
    let folder = lib.create_folder(&space, None, "Move me").unwrap();
    let pin = lib.add_pinned(&space, Some(&folder), "https://pin.example/", "Pin").unwrap();
    let deleted_folder = lib.create_folder(&space, None, "Deleted").unwrap();
    lib.delete_folder(&deleted_folder).unwrap();
    let profile = lib.default_profile_id();
    let other = lib.create_space("Other", "O", Theme::preset(1), &profile).unwrap();
    let other_folder = lib.create_folder(&other, None, "Other folder").unwrap();
    let deleted_space = lib.create_space("Deleted", "D", Theme::preset(1), &profile).unwrap();
    lib.delete_space(&deleted_space).unwrap();
    mark_clean(&lib);
    // Preserve unrelated unsynced data as well as previously acknowledged rows.
    lib.add_favorite(&profile, "https://favorite.example/", "Unsent favorite").unwrap();

    for node in [&folder, &pin] {
        for destination in ["missing-space", deleted_space.as_str()] {
            lib = reject_and_reopen(lib, &path, node, destination, None);
        }
        for parent in ["missing-folder", deleted_folder.as_str(), pin.as_str(), other_folder.as_str()] {
            lib = reject_and_reopen(lib, &path, node, &space, Some(parent));
        }
    }
    lib = reject_and_reopen(lib, &path, "missing-node", &space, None);
    assert_eq!(children(&lib, &space, None), vec![folder.clone()]);
    assert_eq!(children(&lib, &space, Some(&folder)), vec![pin]);
}

#[test]
fn rejects_cross_space_folders_and_cross_profile_items() {
    let (_dir, path, mut lib, space) = fixture();
    let folder = lib.create_folder(&space, None, "Source").unwrap();
    let child = lib.create_folder(&space, Some(&folder), "Child").unwrap();
    let pin = lib.add_pinned(&space, Some(&child), "https://pin.example/", "Pin").unwrap();
    let profile = lib.default_profile_id();
    let other = lib.create_space("Same profile", "S", Theme::preset(1), &profile).unwrap();
    let other_folder = lib.create_folder(&other, None, "Other folder").unwrap();
    let private_profile = lib.create_profile("Separate profile").unwrap();
    let private = lib.create_space("Separate", "P", Theme::preset(2), &private_profile).unwrap();
    let private_folder = lib.create_folder(&private, None, "Private folder").unwrap();
    mark_clean(&lib);

    for (destination, parent) in [(&other, &other_folder), (&private, &private_folder)] {
        for parent in [None, Some(parent.as_str())] {
            lib = reject_and_reopen(lib, &path, &folder, destination, parent);
        }
    }
    for parent in [None, Some(private_folder.as_str())] {
        lib = reject_and_reopen(lib, &path, &pin, &private, parent);
    }
    assert_eq!(children(&lib, &space, None), vec![folder.clone()]);
    assert_eq!(children(&lib, &space, Some(&folder)), vec![child.clone()]);
    assert_eq!(children(&lib, &space, Some(&child)), vec![pin]);
}

#[test]
fn rejects_favorites_and_pinned_items_with_invalid_source_membership() {
    let (_dir, path, mut lib, space) = fixture();
    let profile = lib.default_profile_id();
    let favorite = lib.add_favorite(&profile, "https://favorite.example/", "Favorite").unwrap();
    let pin = lib.add_pinned(&space, None, "https://pin.example/", "Pin").unwrap();
    mark_clean(&lib);
    lib = reject_and_reopen(lib, &path, &favorite, &space, None);

    for source_space in [None, Some("missing-space".to_owned())] {
        lib.update_item(&pin, |item| item.space_id = source_space).unwrap();
        mark_clean(&lib);
        lib = reject_and_reopen(lib, &path, &pin, &space, None);
    }
    let other_profile = lib.create_profile("Different profile").unwrap();
    lib.update_item(&pin, |item| {
        item.space_id = Some(space.clone());
        item.profile_id = other_profile;
    })
    .unwrap();
    mark_clean(&lib);
    lib = reject_and_reopen(lib, &path, &pin, &space, None);

    // Even if the target profile matches the item, a conflicting source space
    // must not be used to transfer a pinned item across the profile boundary.
    let target_profile = lib.items[&pin].profile_id.clone();
    let target = lib.create_space("Target", "T", Theme::preset(2), &target_profile).unwrap();
    reject_and_reopen(lib, &path, &pin, &target, None);
}

#[test]
fn rejects_already_broken_destination_ancestry_without_touching_it() {
    let (_dir, path, mut lib, space) = fixture();
    let folder = lib.create_folder(&space, None, "Move me").unwrap();
    let pin = lib.add_pinned(&space, None, "https://pin.example/", "Pin").unwrap();
    let a = lib.create_folder(&space, None, "A").unwrap();
    let b = lib.create_folder(&space, Some(&a), "B").unwrap();
    let profile = lib.default_profile_id();
    let other = lib.create_space("Other", "O", Theme::preset(1), &profile).unwrap();
    let foreign = lib.create_folder(&other, None, "Foreign").unwrap();

    // Simulate malformed legacy/remote data via the existing low-level editor.
    // The move operation must terminate and reject, not silently repair/delete it.
    for ancestor in [b.as_str(), foreign.as_str(), "missing-ancestor"] {
        lib.update_folder(&a, |a| a.parent_id = Some(ancestor.into())).unwrap();
        mark_clean(&lib);
        for node in [&folder, &pin] {
            lib = reject_and_reopen(lib, &path, node, &space, Some(&b));
        }
        // Explicitly moving the broken tree to a valid root remains possible.
        lib.move_pinned(&a, &space, None, 0).unwrap();
        lib = reopen_unchanged(lib, &path);
        assert_eq!(lib.folders[&a].parent_id, None);
        assert_eq!(children(&lib, &space, Some(&a)), vec![b.clone()]);
    }
}

#[test]
fn preserves_legal_mixed_reordering_and_subtree_moves() {
    let (_dir, path, mut lib, space) = fixture();
    let a = lib.create_folder(&space, None, "A").unwrap();
    let b = lib.create_folder(&space, Some(&a), "B").unwrap();
    let c = lib.create_folder(&space, Some(&b), "C").unwrap();
    let pin = lib.add_pinned(&space, Some(&b), "https://pin.example/", "Pin").unwrap();
    let d = lib.create_folder(&space, None, "D").unwrap();
    let root_pin = lib.add_pinned(&space, None, "https://root.example/", "Root").unwrap();
    lib.update_folder(&b, |b| b.collapsed = true).unwrap();
    lib.update_item(&pin, |pin| pin.custom_title = Some("Custom title".into())).unwrap();
    let original_c = lib.folders[&c].clone();
    let original_pin = lib.items[&pin].clone();

    lib.move_pinned(&a, &space, None, usize::MAX).unwrap();
    assert_eq!(children(&lib, &space, None), vec![d.clone(), root_pin.clone(), a.clone()]);
    lib.move_pinned(&a, &space, None, 0).unwrap();
    assert_eq!(children(&lib, &space, None), vec![a.clone(), d.clone(), root_pin.clone()]);
    lib.move_pinned(&root_pin, &space, None, 1).unwrap();
    assert_eq!(children(&lib, &space, None), vec![a.clone(), root_pin.clone(), d.clone()]);

    lib.move_pinned(&b, &space, Some(&d), 0).unwrap();
    assert!(children(&lib, &space, Some(&a)).is_empty());
    assert_eq!(children(&lib, &space, Some(&d)), vec![b.clone()]);
    assert_eq!(children(&lib, &space, Some(&b)), vec![c.clone(), pin.clone()]);
    assert_eq!(lib.folders[&c], original_c);
    assert_eq!(lib.items[&pin], original_pin);
    assert!(lib.folders[&b].collapsed);
    lib = reopen_unchanged(lib, &path);

    lib.move_pinned(&pin, &space, Some(&b), 0).unwrap();
    assert_eq!(children(&lib, &space, Some(&b)), vec![pin.clone(), c.clone()]);
    lib.move_pinned(&pin, &space, Some(&b), usize::MAX).unwrap();
    assert_eq!(children(&lib, &space, Some(&b)), vec![c, pin.clone()]);
    lib.move_pinned(&pin, &space, None, usize::MAX).unwrap();
    assert_eq!(children(&lib, &space, None), vec![a.clone(), root_pin.clone(), d.clone(), pin.clone()]);
    lib.move_pinned(&b, &space, None, 0).unwrap();
    assert_eq!(children(&lib, &space, None), vec![b.clone(), a, root_pin, d, pin]);
    let lib = reopen_unchanged(lib, &path);
    assert_eq!(lib.folders[&b].parent_id, None);
    assert_eq!(lib.folders.len(), 4);
    assert_eq!(lib.items.len(), 2);
}

#[test]
fn preserves_same_profile_cross_space_item_moves_and_readback() {
    let (_dir, path, mut lib, space) = fixture();
    let source_folder = lib.create_folder(&space, None, "Source").unwrap();
    let pin = lib.add_pinned(&space, Some(&source_folder), "https://pin.example/", "Pin").unwrap();
    lib.update_item(&pin, |pin| pin.custom_title = Some("Kept".into())).unwrap();
    let original = lib.items[&pin].clone();
    let profile = lib.default_profile_id();
    let destination = lib.create_space("Destination", "D", Theme::preset(1), &profile).unwrap();
    let folder = lib.create_folder(&destination, None, "Destination folder").unwrap();
    let sibling = lib.add_pinned(&destination, None, "https://sibling.example/", "Sibling").unwrap();
    mark_clean(&lib);
    let before_revision = lib.revision;

    lib.move_pinned(&pin, &destination, None, 1).unwrap();
    assert_eq!(lib.revision, before_revision + 1);
    assert_eq!(lib.store.dirty_count().unwrap(), 1);
    assert_eq!(children(&lib, &destination, None), vec![folder.clone(), pin.clone(), sibling]);
    assert!(children(&lib, &space, Some(&source_folder)).is_empty());
    lib = reopen_unchanged(lib, &path);
    assert_eq!(lib.items[&pin].space_id.as_deref(), Some(destination.as_str()));
    assert_eq!(lib.items[&pin].profile_id, original.profile_id);

    lib.move_pinned(&pin, &destination, Some(&folder), usize::MAX).unwrap();
    lib = reopen_unchanged(lib, &path);
    assert_eq!(children(&lib, &destination, Some(&folder)), vec![pin.clone()]);
    lib.move_pinned(&pin, &space, Some(&source_folder), 0).unwrap();
    let lib = reopen_unchanged(lib, &path);
    assert_eq!(lib.items[&pin], original);
}

#[test]
fn sqlite_write_and_commit_failures_roll_back_moves_and_allow_retry() {
    for moving_folder in [false, true] {
        for failure in ["clock", "entity", "commit"] {
            let (_dir, path, mut lib, space) = fixture();
            let folder = lib.create_folder(&space, None, "Destination").unwrap();
            let node = if moving_folder {
                let node = lib.create_folder(&space, None, "Move me").unwrap();
                lib.add_pinned(&space, Some(&node), "https://child.example/", "Child").unwrap();
                node
            } else {
                lib.add_pinned(&space, None, "https://pin.example/", "Pin").unwrap()
            };
            mark_clean(&lib);
            let injector = Connection::open(&path).unwrap();
            injector.execute_batch(match failure {
                "clock" => "CREATE TRIGGER reject_move BEFORE UPDATE ON kv WHEN NEW.k = 'hlc_last' BEGIN SELECT RAISE(FAIL, 'fixture clock failure'); END;",
                "entity" => "CREATE TRIGGER reject_move BEFORE UPDATE ON entities BEGIN SELECT RAISE(FAIL, 'fixture entity failure'); END;",
                "commit" => "CREATE TABLE fixture_parent (id INTEGER PRIMARY KEY); CREATE TABLE fixture_deferred (id INTEGER REFERENCES fixture_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER reject_move AFTER UPDATE ON entities BEGIN INSERT INTO fixture_deferred VALUES (99); END;",
                _ => unreachable!(),
            }).unwrap();

            lib = reject_and_reopen(lib, &path, &node, &space, Some(&folder));
            if failure == "commit" {
                let rows: i64 =
                    injector.query_row("SELECT COUNT(*) FROM fixture_deferred", [], |row| row.get(0)).unwrap();
                assert_eq!(rows, 0, "COMMIT failure must roll back trigger writes too");
            }
            injector.execute_batch("DROP TRIGGER reject_move;").unwrap();
            lib.move_pinned(&node, &space, Some(&folder), 0).unwrap();
            let lib = reopen_unchanged(lib, &path);
            assert_eq!(children(&lib, &space, Some(&folder)), vec![node]);
            assert_eq!(lib.store.dirty_count().unwrap(), 1);
        }
    }
}
