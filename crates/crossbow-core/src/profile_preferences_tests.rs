//! Profile preference regressions use disposable SQLite files, never browser data.

use super::*;
use crossbow_proto::{Change, Encoding};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct Fixture {
    _dir: TempDir,
    path: PathBuf,
    lib: Library,
    a: String,
    b: String,
    a_spaces: [String; 2],
    b_space: String,
}

fn open(path: &Path) -> Library {
    Library::open(Store::open(path).unwrap()).unwrap()
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profile-preferences.sqlite");
    let mut lib = open(&path);
    let a = lib.create_profile("Profile A").unwrap();
    let b = lib.create_profile("Profile B").unwrap();
    let a_spaces = [
        lib.create_space("A first", "A", Theme::preset(0), &a).unwrap(),
        lib.create_space("A second", "A", Theme::preset(1), &a).unwrap(),
    ];
    let b_space = lib.create_space("B only", "B", Theme::preset(2), &b).unwrap();
    lib.set_settings(Settings { search_engine: SearchEngine::DuckDuckGo, auto_archive_hours: 48, ..Settings::default() }).unwrap();
    for change in lib.store.dirty_changes(100).unwrap() {
        lib.store.mark_clean(&change.kind, &change.id, &change.hlc).unwrap();
    }
    Fixture { _dir: dir, path, lib, a, b, a_spaces, b_space }
}

fn preferences(engine: Option<SearchEngine>, hours: Option<u32>) -> Option<ProfilePreferences> {
    Some(ProfilePreferences { search_engine: engine, auto_archive_hours: hours })
}

fn resolved(engine: SearchEngine, hours: u32) -> Option<ResolvedProfilePreferences> {
    Some(ResolvedProfilePreferences { search_engine: engine, auto_archive_hours: hours })
}

#[derive(Debug, PartialEq)]
struct Snapshot {
    memory: Value,
    entities: Vec<EntityRow>,
    dirty: Vec<Change>,
    clock: Option<Hlc>,
    saved_clock: Option<String>,
    cursor: Option<String>,
}

fn snapshot(lib: &Library) -> Snapshot {
    let mut dirty = lib.store.dirty_changes(100).unwrap();
    dirty.sort_by(|a, b| (&a.kind, &a.id).cmp(&(&b.kind, &b.id)));
    Snapshot {
        memory: json!({ "profiles": lib.profiles, "unavailable_profiles": lib.unavailable_profiles, "spaces": lib.spaces, "settings": lib.settings }),
        entities: lib.store.all_entities().unwrap(), dirty,
        clock: lib.last_hlc.clone(), saved_clock: lib.store.kv_get(KV_LAST_HLC).unwrap(),
        cursor: lib.store.kv_get(SYNC_CURSOR_KEY).unwrap(),
    }
}

fn assert_unchanged(f: &Fixture, before: &Snapshot, revision: u64) {
    assert_eq!(snapshot(&f.lib), *before);
    assert_eq!(f.lib.revision, revision);
    assert_eq!(snapshot(&open(&f.path)), *before);
}

fn incoming(lib: &Library, id: &str, payload: String) -> ServerChange {
    ServerChange {
        seq: 1, device_id: "fixture-device".into(),
        change: Change {
            kind: kinds::PROFILE.into(), id: id.into(),
            hlc: Hlc::tick(lib.last_hlc.as_ref(), now_ms(), "fixture"),
            deleted: false, enc: Encoding::Json, payload,
        },
    }
}

#[test]
fn saved_profile_read_is_exact_read_only_and_does_not_take_a_writer_lock() {
    let f = fixture();
    let before = snapshot(&f.lib);
    let revision = f.lib.revision;
    let writer = Connection::open(&f.path).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(f.lib.read_profile(&f.a).unwrap(), f.lib.profiles[&f.a]);
    assert!(f.lib.read_profile("").is_err());
    assert!(f.lib.read_profile("missing").is_err());
    writer.execute_batch("ROLLBACK").unwrap();
    assert_unchanged(&f, &before, revision);

    let row = f.lib.store.all_entities().unwrap().into_iter().find(|row| row.kind == kinds::PROFILE && row.id == f.a).unwrap();
    for (deleted, payload) in [
        (false, r#"{"name":"Fresh saved name","created_ms":17}"#),
        (false, r#"{"name":"Invalid policy","created_ms":17,"preferences":{"auto_archive_hours":2}}"#),
        (false, "not json"),
        (true, r#"{"name":"Deleted profile","created_ms":17}"#),
    ] {
        f.lib.store.put_entity(&EntityRow { deleted, payload: payload.into(), ..row.clone() }, false).unwrap();
        let saved = snapshot(&f.lib);
        let result = f.lib.read_profile(&f.a);
        if payload.contains("Fresh saved name") { assert_eq!(result.unwrap().name, "Fresh saved name"); }
        else { assert!(result.is_err()); }
        assert_eq!(snapshot(&f.lib), saved);
        assert_eq!(f.lib.revision, revision);
        assert_eq!(f.lib.profiles[&f.a].name, "Profile A");
        assert_eq!(f.lib.read_profile(&f.b).unwrap().name, "Profile B");
    }
}

#[test]
fn profile_reload_recovers_exact_cache_without_writes_or_clock_changes() {
    let mut f = fixture();
    let row = f.lib.store.all_entities().unwrap().into_iter().find(|row| row.kind == kinds::PROFILE && row.id == f.a).unwrap();
    let mut invalid: Value = serde_json::from_str(&row.payload).unwrap();
    invalid["preferences"] = json!({"auto_archive_hours":2});
    f.lib.store.put_entity(&EntityRow { payload: invalid.to_string(), ..row.clone() }, false).unwrap();
    f.lib = open(&f.path);
    assert!(f.lib.effective_profile_preferences(&f.a).is_none());
    let external = open(&f.path);
    external.store.put_entity(&row, false).unwrap();
    let before = snapshot(&f.lib);
    let revision = f.lib.revision;
    let other = f.lib.profiles[&f.b].clone();
    let writer = Connection::open(&f.path).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let loaded = f.lib.reload_profile(&f.a).unwrap();
    assert_eq!(loaded.name, "Profile A");
    assert_eq!(f.lib.effective_profile_preferences(&f.a), resolved(SearchEngine::DuckDuckGo, 48));
    assert_eq!(f.lib.revision, revision + 1);
    assert_eq!(f.lib.reload_profile(&f.a).unwrap(), loaded);
    assert_eq!(f.lib.revision, revision + 1);
    writer.execute_batch("ROLLBACK").unwrap();
    let after = snapshot(&f.lib);
    assert_eq!(after.entities, before.entities);
    assert_eq!(after.dirty, before.dirty);
    assert_eq!(after.clock, before.clock);
    assert_eq!(after.saved_clock, before.saved_clock);
    assert_eq!(after.cursor, before.cursor);
    assert_eq!(f.lib.profiles[&f.b], other);
    assert_eq!(open(&f.path).profiles[&f.a], loaded);
}

#[test]
fn profile_reload_refuses_deleted_invalid_and_unreadable_clock_without_fallback() {
    for fault in ["missing", "deleted", "malformed", "policy", "clock"] {
        let mut f = fixture();
        let row = f.lib.store.all_entities().unwrap().into_iter().find(|row| row.kind == kinds::PROFILE && row.id == f.a).unwrap();
        match fault {
            "missing" => { Connection::open(&f.path).unwrap().execute("DELETE FROM entities WHERE kind='profile' AND id=?1", [&f.a]).unwrap(); }
            "deleted" => f.lib.store.put_entity(&EntityRow { deleted: true, ..row.clone() }, false).unwrap(),
            "malformed" => f.lib.store.put_entity(&EntityRow { payload: "invalid".into(), ..row.clone() }, false).unwrap(),
            "policy" => {
                let mut payload: Value = serde_json::from_str(&row.payload).unwrap();
                payload["preferences"] = json!({"auto_archive_hours":2});
                f.lib.store.put_entity(&EntityRow { payload: payload.to_string(), ..row.clone() }, false).unwrap();
            }
            _ => f.lib.store.kv_set(KV_LAST_HLC, "unreadable").unwrap(),
        }
        let before = snapshot(&f.lib);
        let other = f.lib.profiles[&f.b].clone();
        assert!(f.lib.reload_profile(&f.a).is_err(), "accepted {fault}");
        assert!(f.lib.effective_profile_preferences(&f.a).is_none());
        assert_eq!(f.lib.profiles[&f.b], other);
        assert!(f.lib.reload_profile("").is_err());
        let after = snapshot(&f.lib);
        assert_eq!(after.entities, before.entities);
        assert_eq!(after.dirty, before.dirty);
        assert_eq!(after.clock, before.clock);
        assert_eq!(after.saved_clock, before.saved_clock);
        assert_eq!(after.cursor, before.cursor);
    }
}

#[test]
fn legacy_absent_null_and_empty_overrides_inherit_without_load_or_read_writes() {
    for suffix in ["", ",\"preferences\":null", ",\"preferences\":{}", ",\"preferences\":{\"search_engine\":null,\"auto_archive_hours\":null}"] {
        let mut f = fixture();
        let row = EntityRow {
            kind: kinds::PROFILE.into(), id: f.a.clone(), hlc: Hlc::new(1, 0, "legacy"), deleted: false,
            payload: format!("{{\"name\":\"Unchanged legacy profile\",\"created_ms\":17{suffix}}}"),
        };
        f.lib.store.put_entity(&row, false).unwrap();
        f.lib = open(&f.path);
        let before = snapshot(&f.lib);
        let revision = f.lib.revision;
        let conn = Connection::open(&f.path).unwrap();
        for table in ["entities", "kv"] {
            for operation in ["INSERT", "UPDATE", "DELETE"] {
                conn.execute_batch(&format!("CREATE TRIGGER reject_{table}_{operation} BEFORE {operation} ON {table} BEGIN SELECT RAISE(ABORT, 'read attempted a write'); END;")).unwrap();
            }
        }
        for _ in 0..3 {
            assert_eq!(f.lib.effective_profile_preferences(&f.a), resolved(SearchEngine::DuckDuckGo, 48));
            assert_eq!(f.lib.effective_profile_preferences(&f.b), resolved(SearchEngine::DuckDuckGo, 48));
        }
        assert_unchanged(&f, &before, revision);
        assert_eq!(open(&f.path).profiles[&f.a].name, "Unchanged legacy profile");
    }
}

#[test]
fn profile_overrides_are_independent_shared_by_same_profile_spaces_and_reopened() {
    let mut f = fixture();
    let a = f.lib.profiles[&f.a].clone();
    let b = f.lib.profiles[&f.b].clone();
    let spaces = f.lib.spaces.clone();
    let settings = f.lib.settings.clone();
    let default_profile = f.lib.default_profile_id();
    let revision = f.lib.revision;
    f.lib.set_profile_preferences(&f.a, preferences(Some(SearchEngine::Kagi), Some(1))).unwrap();
    f.lib.set_profile_preferences(&f.b, preferences(Some(SearchEngine::Bing), Some(0))).unwrap();
    assert_eq!(f.lib.revision, revision + 2);
    assert_eq!(f.lib.profiles.len(), 2);
    assert_eq!(f.lib.default_profile_id(), default_profile);
    assert_eq!(f.lib.settings, settings);
    assert_eq!(f.lib.spaces, spaces);
    for lib in [&f.lib, &open(&f.path)] {
        for space in &f.a_spaces {
            assert_eq!(lib.effective_profile_preferences(&lib.spaces[space].profile_id), resolved(SearchEngine::Kagi, 1));
        }
        assert_eq!(lib.effective_profile_preferences(&lib.spaces[&f.b_space].profile_id), resolved(SearchEngine::Bing, 0));
        assert_eq!((&lib.profiles[&f.a].name, lib.profiles[&f.a].created_ms), (&a.name, a.created_ms));
        assert_eq!((&lib.profiles[&f.b].name, lib.profiles[&f.b].created_ms), (&b.name, b.created_ms));
    }
    let dirty = f.lib.store.dirty_changes(100).unwrap();
    assert_eq!(dirty.len(), 2);
    assert!(dirty.iter().all(|change| change.kind == kinds::PROFILE && change.enc == Encoding::Json));
}

#[test]
fn field_overrides_and_reset_keep_untouched_custom_inheritance() {
    let mut f = fixture();
    f.lib.set_profile_preferences(&f.a, preferences(Some(SearchEngine::Kagi), None)).unwrap();
    f.lib.set_profile_preferences(&f.b, preferences(None, Some(0))).unwrap();
    assert_eq!(f.lib.effective_profile_preferences(&f.a), resolved(SearchEngine::Kagi, 48));
    assert_eq!(f.lib.effective_profile_preferences(&f.b), resolved(SearchEngine::DuckDuckGo, 0));
    let saved_a = f.lib.store.entity_payload(kinds::PROFILE, &f.a).unwrap().unwrap();
    assert!(!saved_a.contains("auto_archive_hours"));
    let before_profiles = f.lib.profiles.clone();
    f.lib.set_settings(Settings { search_engine: SearchEngine::Ecosia, auto_archive_hours: 36, ..f.lib.settings.clone() }).unwrap();
    assert_eq!(f.lib.profiles, before_profiles);
    assert_eq!(f.lib.effective_profile_preferences(&f.a), resolved(SearchEngine::Kagi, 36));
    assert_eq!(f.lib.effective_profile_preferences(&f.b), resolved(SearchEngine::Ecosia, 0));
    f.lib.set_profile_preferences(&f.a, None).unwrap();
    assert_eq!(f.lib.profiles[&f.a].preferences, None);
    assert_eq!(f.lib.effective_profile_preferences(&f.a), resolved(SearchEngine::Ecosia, 36));
    assert!(!f.lib.store.entity_payload(kinds::PROFILE, &f.a).unwrap().unwrap().contains("preferences"));
    let reopened = open(&f.path);
    assert_eq!(reopened.effective_profile_preferences(&f.a), resolved(SearchEngine::Ecosia, 36));
    assert_eq!(reopened.effective_profile_preferences(&f.b), resolved(SearchEngine::Ecosia, 0));
}

#[test]
fn every_supported_provider_and_duration_round_trips_in_profile_json() {
    assert_eq!(ARCHIVE_CHOICES, [1, 6, 12, 24, 168, 720, 0]);
    assert_eq!(ARCHIVE_CHOICES.map(archive_label), ["1 hour", "6 hours", "12 hours", "24 hours", "7 days", "30 days", "Never"]);
    let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
    let id = lib.create_profile("Round trip").unwrap();
    for engine in SearchEngine::ALL {
        for hours in ARCHIVE_CHOICES {
            let profile = Profile { name: "Round trip".into(), created_ms: 123, preferences: preferences(Some(engine), Some(hours)) };
            let json = serde_json::to_string(&profile).unwrap();
            assert_eq!(serde_json::from_str::<Profile>(&json).unwrap(), profile);
            lib.set_profile_preferences(&id, profile.preferences).unwrap();
            assert_eq!(lib.effective_profile_preferences(&id), resolved(engine, hours));
        }
    }
}

#[test]
fn unsupported_or_malformed_explicit_preferences_do_not_decode_as_inheritance() {
    for preferences in [
        json!({"search_engine": "unknown_provider"}), json!({"search_engine": 1}),
        json!({"auto_archive_hours": 2}), json!({"auto_archive_hours": 48}),
        json!({"auto_archive_hours": -1}), json!({"auto_archive_hours": 1.5}),
        json!({"auto_archive_hours": "never"}), json!({"auto_archive_hours": u64::MAX}),
        json!({"archive_hours": 0}), json!(true), json!([]),
    ] {
        let profile = json!({"name": "Invalid profile", "created_ms": 1, "preferences": preferences});
        assert!(serde_json::from_value::<Profile>(profile.clone()).is_err(), "accepted {profile}");
    }
    let legacy: Profile = serde_json::from_value(json!({"name": "Legacy", "created_ms": 1})).unwrap();
    assert_eq!(legacy.preferences, None);
}

#[test]
fn invalid_explicit_durations_are_rejected_without_object_clock_or_revision_changes() {
    let mut f = fixture();
    let before = snapshot(&f.lib);
    let revision = f.lib.revision;
    for hours in [2, 5, 11, 48, 721, u32::MAX] {
        assert!(matches!(f.lib.set_profile_preferences(&f.a, preferences(Some(SearchEngine::Bing), Some(hours))), Err(LibraryError::InvalidProfilePreferences(_))));
        assert_unchanged(&f, &before, revision);
    }
}

#[test]
fn missing_and_deleted_profiles_never_fall_back_or_resurrect() {
    let mut f = fixture();
    f.lib.set_profile_preferences(&f.b, preferences(Some(SearchEngine::Bing), Some(1))).unwrap();
    f.lib.tombstone(kinds::PROFILE, &f.a).unwrap();
    let before = snapshot(&f.lib);
    let revision = f.lib.revision;
    for id in ["", "unknown-profile", f.a.as_str()] {
        assert_eq!(f.lib.effective_profile_preferences(id), None);
        assert!(matches!(f.lib.set_profile_preferences(id, preferences(Some(SearchEngine::Kagi), Some(6))), Err(LibraryError::NotFound(_))));
        assert_unchanged(&f, &before, revision);
    }
    assert_eq!(f.lib.effective_profile_preferences(&f.b), resolved(SearchEngine::Bing, 1));
}

#[test]
fn changed_or_deleted_saved_profile_rejects_stale_memory_then_accepts_reload_retry() {
    for deleted in [false, true] {
        let mut f = fixture();
        let prior = f.lib.profiles[&f.a].clone();
        let conn = Connection::open(&f.path).unwrap();
        let renamed = Profile { name: "Latest saved name".into(), ..prior.clone() };
        conn.execute("UPDATE entities SET payload = ?1, deleted = ?2 WHERE kind = 'profile' AND id = ?3",
            rusqlite::params![serde_json::to_string(&renamed).unwrap(), deleted, f.a]).unwrap();
        let saved = f.lib.store.all_entities().unwrap();
        let revision = f.lib.revision;
        let clock = f.lib.last_hlc.clone();
        assert!(f.lib.set_profile_preferences(&f.a, preferences(Some(SearchEngine::Kagi), Some(6))).is_err());
        assert_eq!(f.lib.profiles[&f.a], prior);
        assert_eq!(f.lib.store.all_entities().unwrap(), saved);
        assert_eq!(f.lib.revision, revision);
        assert_eq!(f.lib.last_hlc, clock);
        f.lib = open(&f.path);
        if deleted {
            assert_eq!(f.lib.effective_profile_preferences(&f.a), None);
            assert!(matches!(f.lib.set_profile_preferences(&f.a, None), Err(LibraryError::NotFound(_))));
        } else {
            f.lib.set_profile_preferences(&f.a, preferences(Some(SearchEngine::Kagi), Some(6))).unwrap();
            assert_eq!(f.lib.profiles[&f.a].name, "Latest saved name");
            assert_eq!(f.lib.profiles[&f.a].created_ms, prior.created_ms);
            assert_eq!(open(&f.path).effective_profile_preferences(&f.a), resolved(SearchEngine::Kagi, 6));
        }
    }
}

fn failed_save_then_retry(fault: &str, advance_saved_clock: bool) {
    let mut f = fixture();
    f.lib.set_profile_preferences(&f.a, preferences(Some(SearchEngine::Bing), Some(24))).unwrap();
    let prior = f.lib.profiles[&f.a].clone();
    if advance_saved_clock {
        let mut other = open(&f.path);
        let mut remote = incoming(&other, &f.a, serde_json::to_string(&prior).unwrap());
        remote.change.hlc = Hlc::new(now_ms() + 86_400_000, 9, "remote");
        other.apply_sync_page(&[remote], 20).unwrap();
    }
    let before = snapshot(&f.lib);
    let reopened_before = snapshot(&open(&f.path));
    let revision = f.lib.revision;
    let conn = Connection::open(&f.path).unwrap();
    let sql = match fault {
        "clock" => "CREATE TRIGGER reject_preference BEFORE UPDATE ON kv WHEN NEW.k = 'hlc_last' BEGIN SELECT RAISE(ABORT, 'fixture clock failure'); END;",
        "profile" => "CREATE TRIGGER reject_preference BEFORE UPDATE ON entities WHEN NEW.kind = 'profile' BEGIN SELECT RAISE(ABORT, 'fixture profile failure'); END;",
        "commit" => "CREATE TABLE fixture_parent (id INTEGER PRIMARY KEY); CREATE TABLE fixture_deferred (id INTEGER REFERENCES fixture_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER reject_preference AFTER UPDATE ON entities WHEN NEW.kind = 'profile' BEGIN INSERT INTO fixture_deferred VALUES (1); END;",
        _ => panic!("unknown fixture fault"),
    };
    conn.execute_batch(sql).unwrap();
    let requested = preferences(Some(SearchEngine::Perplexity), Some(6));
    assert!(f.lib.set_profile_preferences(&f.a, requested).is_err());
    assert_eq!(snapshot(&f.lib), before);
    assert_eq!(f.lib.revision, revision);
    assert_eq!(snapshot(&open(&f.path)), reopened_before);
    assert_eq!(f.lib.profiles[&f.a], prior);
    assert_eq!(f.lib.effective_profile_preferences(&f.a), resolved(SearchEngine::Bing, 24));
    conn.execute_batch("DROP TRIGGER reject_preference").unwrap();
    f.lib.set_profile_preferences(&f.a, requested).unwrap();
    assert_eq!(f.lib.revision, revision + 1);
    assert_eq!(f.lib.profiles[&f.a], Profile { preferences: requested, ..prior });
    assert_eq!(f.lib.profiles.len(), 2);
    let saved = snapshot(&f.lib);
    assert!(saved.clock > reopened_before.clock);
    drop(f.lib);
    let reopened = open(&f.path);
    assert_eq!(snapshot(&reopened), saved);
    assert_eq!(reopened.effective_profile_preferences(&f.a), resolved(SearchEngine::Perplexity, 6));
    assert_eq!(reopened.effective_profile_preferences(&f.b), resolved(SearchEngine::DuckDuckGo, 48));
    assert_eq!(conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
    if fault == "commit" {
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM fixture_deferred", [], |row| row.get::<_, u32>(0)).unwrap(), 0);
    }
}

#[test]
fn failed_clock_save_preserves_profile_then_retries_and_reopens() { failed_save_then_retry("clock", false); }

#[test]
fn failed_profile_save_preserves_profile_then_retries_and_reopens() { failed_save_then_retry("profile", false); }

#[test]
fn failed_commit_preserves_profile_then_retries_and_reopens() { failed_save_then_retry("commit", false); }

#[test]
fn review_pp02_merged_remote_clock_rolls_back_on_clock_entity_and_commit_failure() {
    for fault in ["clock", "profile", "commit"] { failed_save_then_retry(fault, true); }
}

#[test]
fn existing_json_sync_round_trip_preserves_override_inheritance_and_tombstone() {
    let mut f = fixture();
    f.lib.set_profile_preferences(&f.a, preferences(Some(SearchEngine::Kagi), Some(6))).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("receiving-library.sqlite");
    let mut recipient = open(&path);
    let rows = f.lib.store.all_entities().unwrap();
    let changes: Vec<_> = rows.iter().enumerate().map(|(index, row)| ServerChange {
        seq: index as u64 + 1, device_id: "fixture-device".into(),
        change: Change {
            kind: row.kind.clone(), id: row.id.clone(), hlc: row.hlc.clone(), deleted: row.deleted,
            enc: Encoding::Json, payload: row.payload.clone(),
        },
    }).collect();
    for change in &changes { change.change.validate().unwrap(); }
    assert_eq!(recipient.apply_sync_page(&changes, 7).unwrap(), changes.len());
    assert_eq!(recipient.store.dirty_count().unwrap(), 0);
    assert_eq!(recipient.profiles, f.lib.profiles);
    assert_eq!(recipient.effective_profile_preferences(&f.a), resolved(SearchEngine::Kagi, 6));
    assert_eq!(recipient.effective_profile_preferences(&f.b), resolved(SearchEngine::DuckDuckGo, 48));
    assert_eq!(open(&path).profiles, f.lib.profiles);
    assert_eq!(recipient.apply_sync_page(&changes, 7).unwrap(), 0);
    let mut tombstone = incoming(&recipient, &f.a, String::new());
    tombstone.change.deleted = true;
    recipient.apply_sync_page(&[tombstone], 8).unwrap();
    assert_eq!(recipient.effective_profile_preferences(&f.a), None);
    assert_eq!(open(&path).effective_profile_preferences(&f.a), None);
}

#[test]
fn invalid_incoming_profile_rejects_whole_page_before_disk_memory_clock_or_cursor_changes() {
    for invalid in [
        json!({"search_engine": "future-provider", "auto_archive_hours": 1}),
        json!({"search_engine": "bing", "auto_archive_hours": 2}),
        json!({"search_engine": "bing", "auto_archive_hours": "0"}),
        json!({"future_archive_setting": 0}), json!([]), json!(true),
    ] {
        for page in [false, true] {
            let mut f = fixture();
            f.lib.set_profile_preferences(&f.a, preferences(Some(SearchEngine::Kagi), Some(0))).unwrap();
            f.lib.store.kv_set(SYNC_CURSOR_KEY, "4").unwrap();
            let before = snapshot(&f.lib);
            let revision = f.lib.revision;
            let mut valid = f.lib.profiles[&f.b].clone();
            valid.name = "Must not apply partial page".into();
            let changes = [
                incoming(&f.lib, &f.b, serde_json::to_string(&valid).unwrap()),
                incoming(&f.lib, &f.a, json!({"name": "Rejected", "created_ms": 1, "preferences": invalid}).to_string()),
            ];
            let result = if page { f.lib.apply_sync_page(&changes, 9) } else { f.lib.apply_remote(&changes) };
            assert!(result.is_err(), "accepted malformed profile in page={page}");
            assert_unchanged(&f, &before, revision);
            assert_eq!(f.lib.effective_profile_preferences(&f.a), resolved(SearchEngine::Kagi, 0));
        }
    }
}

#[test]
fn malformed_or_non_json_incoming_profile_rejects_the_batch_before_any_write() {
    for (payload, encoding) in [
        ("not JSON", Encoding::Json), ("null", Encoding::Json),
        ("{\"name\":\"Missing creation time\"}", Encoding::Json),
        ("[\"Array profile\",1]", Encoding::Json),
        ("{\"name\":\"Wrong encoding\",\"created_ms\":1}", Encoding::Xchacha20V1),
    ] {
        for page in [false, true] {
            let mut f = fixture();
            let before = snapshot(&f.lib);
            let revision = f.lib.revision;
            let mut first = incoming(&f.lib, "valid-first", "opaque future payload".into());
            first.change.kind = "future_kind".into();
            let mut invalid = incoming(&f.lib, &f.a, payload.into());
            invalid.change.enc = encoding;
            let changes = [first, invalid];
            let result = if page { f.lib.apply_sync_page(&changes, 10) } else { f.lib.apply_remote(&changes) };
            assert!(result.is_err(), "accepted {payload} with {encoding:?}, page={page}");
            assert_unchanged(&f, &before, revision);
        }
    }
}

#[test]
fn profile_validation_preserves_tombstones_and_other_entity_handling() {
    for encoding in [Encoding::Json, Encoding::Xchacha20V1] {
        let mut f = fixture();
        let mut tombstone = incoming(&f.lib, &f.a, String::new());
        tombstone.change.deleted = true;
        tombstone.change.enc = encoding;
        let mut unknown = incoming(&f.lib, "unknown-kind-record", "opaque future payload".into());
        unknown.change.kind = "future_kind".into();
        unknown.change.enc = encoding;
        let revision = f.lib.revision;
        assert_eq!(f.lib.apply_sync_page(&[tombstone, unknown], 11).unwrap(), 2);
        assert_eq!(f.lib.revision, revision + 2);
        assert_eq!(f.lib.effective_profile_preferences(&f.a), None);
        assert_eq!(f.lib.effective_profile_preferences(&f.b), resolved(SearchEngine::DuckDuckGo, 48));
        assert_eq!(f.lib.store.entity_payload("future_kind", "unknown-kind-record").unwrap().as_deref(), Some("opaque future payload"));
        assert_eq!(snapshot(&open(&f.path)), snapshot(&f.lib));
    }
}

#[test]
fn review_pp02_equal_payload_save_never_rewinds_another_handles_remote_clock() {
    for target_only in [false, true] {
        let mut f = fixture();
        let mut other = open(&f.path);
        let mut remote = incoming(&other, &f.a, serde_json::to_string(&other.profiles[&f.a]).unwrap());
        remote.change.hlc = Hlc::new(now_ms() + 86_400_000, 7, "remote");
        let remote_hlc = remote.change.hlc.clone();
        if target_only {
            Connection::open(&f.path).unwrap().execute("UPDATE entities SET hlc = ?1 WHERE kind = 'profile' AND id = ?2",
                rusqlite::params![remote_hlc.encode(), f.a]).unwrap();
        } else {
            other.apply_sync_page(&[remote], 21).unwrap();
        }
        let saved_clock = f.lib.store.kv_get(KV_LAST_HLC).unwrap().unwrap();
        f.lib.set_profile_preferences(&f.a, preferences(Some(SearchEngine::Kagi), Some(6))).unwrap();
        let saved = f.lib.store.entity_hlc(kinds::PROFILE, &f.a).unwrap().unwrap();
        assert!(saved > remote_hlc, "local preference save moved remote clock backwards: {saved} <= {remote_hlc}");
        assert!(f.lib.store.kv_get(KV_LAST_HLC).unwrap().unwrap() >= saved_clock);
        assert_eq!(open(&f.path).effective_profile_preferences(&f.a), resolved(SearchEngine::Kagi, 6));
    }
}

#[test]
fn review_pp03_invalid_optional_policy_preserves_startup_identity_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("invalid-startup.sqlite");
    let mut lib = open(&path);
    let id = lib.create_profile("Original identity").unwrap();
    let space = lib.create_space("Existing space", "S", Theme::preset(0), &id).unwrap();
    let created = lib.profiles[&id].created_ms;
    let conn = Connection::open(&path).unwrap();
    conn.execute("UPDATE entities SET payload = ?1 WHERE kind = 'profile' AND id = ?2",
        rusqlite::params![json!({"name": "Original identity", "created_ms": created, "preferences": {"auto_archive_hours": 2}}).to_string(), id]).unwrap();
    lib = open(&path);
    let rows = lib.store.all_entities().unwrap();
    lib.ensure_defaults().unwrap();
    assert_eq!(lib.store.all_entities().unwrap(), rows, "unreadable optional preferences created a replacement identity");
    assert_eq!(lib.profiles.len(), 1);
    assert_eq!(lib.profiles[&id].name, "Original identity");
    assert_eq!(lib.profiles[&id].created_ms, created);
    assert_eq!(lib.spaces[&space].profile_id, id);
    assert_eq!(lib.effective_profile_preferences(&id), None);
    assert!(lib.set_profile_preferences(&id, preferences(Some(SearchEngine::Kagi), None)).is_err());
    assert_eq!(lib.store.all_entities().unwrap(), rows);
}

#[test]
fn review_pp03_fully_unreadable_live_profile_is_not_an_empty_library() {
    for with_space in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unreadable-startup.sqlite");
        let mut lib = open(&path);
        let id = lib.create_profile("Stored identity").unwrap();
        if with_space { lib.create_space("Existing space", "S", Theme::preset(0), &id).unwrap(); }
        Connection::open(&path).unwrap().execute("UPDATE entities SET payload = 'invalid JSON' WHERE kind = 'profile' AND id = ?1", [&id]).unwrap();
        lib = open(&path);
        lib.ensure_defaults().unwrap();
        let profiles: Vec<_> = lib.store.all_entities().unwrap().into_iter().filter(|row| row.kind == kinds::PROFILE && !row.deleted).collect();
        assert_eq!(profiles.len(), 1, "startup manufactured a replacement profile");
        assert_eq!(profiles[0].id, id);
        assert_eq!(profiles[0].payload, "invalid JSON");
        assert!(!lib.spaces.is_empty(), "startup must still have a selectable Space");
        assert!(lib.spaces.values().all(|space| space.profile_id == id));
        assert_eq!(lib.effective_profile_preferences(&id), None);
    }
}

#[test]
fn review_pp04_stale_malformed_profiles_do_not_block_valid_page_or_cursor() {
    for tombstone in [false, true] {
        for encoding in [Encoding::Json, Encoding::Xchacha20V1] {
            let mut f = fixture();
            if tombstone { f.lib.tombstone(kinds::PROFILE, &f.a).unwrap(); }
            let prior = f.lib.store.entity_payload(kinds::PROFILE, &f.a).unwrap();
            let mut stale = incoming(&f.lib, &f.a, "malformed obsolete JSON".into());
            stale.change.hlc = Hlc::new(1, 0, "stale");
            stale.change.enc = encoding;
            let mut valid = incoming(&f.lib, "future-record", "opaque payload".into());
            valid.change.kind = "future_kind".into();
            assert_eq!(f.lib.apply_sync_page(&[stale, valid], 22).unwrap(), 1);
            assert_eq!(f.lib.store.entity_payload(kinds::PROFILE, &f.a).unwrap(), prior);
            assert_eq!(f.lib.store.kv_get(SYNC_CURSOR_KEY).unwrap().as_deref(), Some("22"));
            assert_eq!(snapshot(&open(&f.path)), snapshot(&f.lib));
        }
    }
}

#[test]
fn review_pp04_sequential_duplicate_ids_respect_prior_page_winners() {
    for deleted in [false, true] {
        for equal_clock in [false, true] {
            let mut f = fixture();
            let mut first = incoming(&f.lib, &f.a, serde_json::to_string(&f.lib.profiles[&f.a]).unwrap());
            first.change.hlc = Hlc::new(now_ms() + 10_000, 5, "remote");
            first.change.deleted = deleted;
            if deleted { first.change.payload.clear(); }
            let mut obsolete = first.clone();
            obsolete.change.deleted = false;
            obsolete.change.payload = "invalid obsolete profile".into();
            obsolete.change.enc = Encoding::Xchacha20V1;
            if !equal_clock { obsolete.change.hlc.counter -= 1; }
            let revision = f.lib.revision;
            assert_eq!(f.lib.apply_sync_page(&[first, obsolete], 23).unwrap(), 1);
            assert_eq!(f.lib.revision, revision + 1);
            assert_eq!(f.lib.profiles.contains_key(&f.a), !deleted);
            assert_eq!(f.lib.store.kv_get(SYNC_CURSOR_KEY).unwrap().as_deref(), Some("23"));
        }
    }
}

#[test]
fn review_pp04_any_sequential_malformed_winner_rejects_before_writes() {
    for invalid_first in [false, true] {
        let mut f = fixture();
        let before = snapshot(&f.lib);
        let revision = f.lib.revision;
        let mut first = incoming(&f.lib, &f.a, serde_json::to_string(&f.lib.profiles[&f.a]).unwrap());
        first.change.hlc = Hlc::new(now_ms() + 10_000, 5, "remote");
        let mut second = first.clone();
        second.change.hlc.counter += 1;
        if invalid_first { first.change.payload = "invalid first winner".into(); }
        else { second.change.payload = "invalid second winner".into(); }
        assert!(f.lib.apply_sync_page(&[first, second], 24).is_err());
        assert_unchanged(&f, &before, revision);
    }
}

#[test]
fn review_pp05_existing_retry_cannot_recover_stale_saved_profile_without_refresh() {
    let mut f = fixture();
    let mut other = open(&f.path);
    let original = f.lib.profiles[&f.a].clone();
    let mut remote = original.clone();
    remote.name = "New remote name".into();
    remote.preferences = preferences(None, Some(0));
    let change = incoming(&other, &f.a, serde_json::to_string(&remote).unwrap());
    other.apply_sync_page(&[change], 25).unwrap();
    let requested = preferences(Some(SearchEngine::Kagi), None);
    assert!(f.lib.set_profile_preferences(&f.a, requested).is_err());
    assert!(f.lib.set_profile_preferences(&f.a, requested).is_err(), "candidate refuted: retry already recovered");
    assert_eq!(f.lib.profiles[&f.a], original);
    assert_eq!(open(&f.path).profiles[&f.a], remote);
}

#[test]
fn review_pp02_refresh_observes_saved_and_target_only_clocks_without_writes_or_revision_changes() {
    for target_only in [false, true] {
        let mut f = fixture();
        let future = Hlc::new(now_ms() + 86_400_000, 13, "remote");
        if target_only {
            Connection::open(&f.path).unwrap().execute("UPDATE entities SET hlc = ?1 WHERE kind = 'profile' AND id = ?2",
                rusqlite::params![future.encode(), f.a]).unwrap();
        } else {
            f.lib.store.kv_set(KV_LAST_HLC, &future.encode()).unwrap();
        }
        let before = snapshot(&f.lib);
        let revision = f.lib.revision;
        let original = f.lib.profiles[&f.a].clone();
        assert_eq!(f.lib.refresh_profile(&f.a).unwrap(), original);
        assert_eq!(f.lib.last_hlc.as_ref(), Some(&future));
        assert_eq!(f.lib.revision, revision);
        let after = snapshot(&f.lib);
        assert_eq!((after.entities, after.dirty, after.saved_clock, after.cursor), (before.entities, before.dirty, before.saved_clock, before.cursor));
        f.lib.set_profile_preferences(&f.a, preferences(Some(SearchEngine::Kagi), Some(6))).unwrap();
        assert!(f.lib.store.entity_hlc(kinds::PROFILE, &f.a).unwrap().unwrap() > future);
        assert_eq!(snapshot(&f.lib), snapshot(&open(&f.path)));
    }
}

#[test]
fn review_pp05_refresh_clears_unavailability_only_after_same_identity_becomes_valid() {
    let mut f = fixture();
    let original = f.lib.profiles[&f.a].clone();
    let conn = Connection::open(&f.path).unwrap();
    let invalid = Profile { preferences: preferences(None, Some(2)), ..original.clone() };
    conn.execute("UPDATE entities SET payload = ?1 WHERE kind = 'profile' AND id = ?2",
        rusqlite::params![serde_json::to_string(&invalid).unwrap(), f.a]).unwrap();
    let revision = f.lib.revision;
    assert!(f.lib.refresh_profile(&f.a).is_err());
    assert_eq!(f.lib.profiles[&f.a], original, "readable identity is retained without enabling inherited policy");
    assert_eq!(f.lib.effective_profile_preferences(&f.a), None);
    assert_eq!(f.lib.revision, revision + 1);
    conn.execute("UPDATE entities SET payload = ?1 WHERE kind = 'profile' AND id = ?2",
        rusqlite::params![serde_json::to_string(&original).unwrap(), f.a]).unwrap();
    let rows = f.lib.store.all_entities().unwrap();
    assert_eq!(f.lib.refresh_profile(&f.a).unwrap(), original);
    assert_eq!(f.lib.effective_profile_preferences(&f.a), resolved(SearchEngine::DuckDuckGo, 48));
    assert_eq!(f.lib.revision, revision + 2, "availability changes must invalidate the UI cache");
    assert_eq!(f.lib.store.all_entities().unwrap(), rows);
    let before = snapshot(&f.lib);
    for id in ["", "never-existed"] { assert!(matches!(f.lib.refresh_profile(id), Err(LibraryError::NotFound(_)))); }
    assert_unchanged(&f, &before, revision + 2);
}

#[test]
fn review_pp05_refresh_retries_only_edited_fields_against_exact_saved_profile() {
    let mut f = fixture();
    let original_b = f.lib.profiles[&f.b].clone();
    let mut other = open(&f.path);
    let mut remote = other.profiles[&f.a].clone();
    remote.name = "Latest saved A name".into();
    remote.preferences = preferences(Some(SearchEngine::Bing), Some(0));
    let mut changed_b = original_b.clone();
    changed_b.name = "Unrelated saved B name".into();
    let mut a = incoming(&other, &f.a, serde_json::to_string(&remote).unwrap());
    a.change.hlc = Hlc::new(now_ms() + 86_400_000, 11, "remote");
    let b = incoming(&other, &f.b, serde_json::to_string(&changed_b).unwrap());
    other.apply_sync_page(&[a, b], 26).unwrap();
    assert!(f.lib.set_profile_preferences(&f.a, preferences(Some(SearchEngine::Kagi), None)).is_err());
    let before = snapshot(&f.lib);
    let revision = f.lib.revision;
    let conn = Connection::open(&f.path).unwrap();
    for table in ["entities", "kv"] {
        for operation in ["INSERT", "UPDATE", "DELETE"] {
            conn.execute_batch(&format!("CREATE TRIGGER reject_{table}_{operation} BEFORE {operation} ON {table} BEGIN SELECT RAISE(ABORT, 'refresh attempted a write'); END;")).unwrap();
        }
    }
    let refreshed = f.lib.refresh_profile(&f.a).unwrap();
    assert_eq!(refreshed, remote);
    assert_eq!(f.lib.profiles[&f.b], original_b, "refresh must not replace unrelated cached profiles");
    assert_eq!(f.lib.revision, revision + 1);
    assert_eq!(f.lib.last_hlc, other.last_hlc);
    assert_eq!(f.lib.effective_profile_preferences(&f.a), resolved(SearchEngine::Bing, 0));
    assert_eq!(f.lib.refresh_profile(&f.a).unwrap(), remote);
    assert_eq!(f.lib.revision, revision + 1, "unchanged refresh must not invalidate caches repeatedly");
    let after = snapshot(&f.lib);
    assert_eq!((after.entities, after.dirty, after.saved_clock, after.cursor), (before.entities, before.dirty, before.saved_clock, before.cursor));
    for table in ["entities", "kv"] {
        for operation in ["INSERT", "UPDATE", "DELETE"] {
            conn.execute_batch(&format!("DROP TRIGGER reject_{table}_{operation}")).unwrap();
        }
    }
    let mut merged = refreshed.preferences.unwrap_or_default();
    merged.search_engine = Some(SearchEngine::Kagi);
    f.lib.set_profile_preferences(&f.a, Some(merged)).unwrap();
    let reopened = open(&f.path);
    assert_eq!(reopened.profiles[&f.a], Profile { preferences: Some(merged), ..remote });
    assert_eq!(reopened.profiles[&f.b], changed_b);
    assert_eq!(reopened.effective_profile_preferences(&f.a), resolved(SearchEngine::Kagi, 0));
    assert_eq!(reopened.store.dirty_changes(100).unwrap().len(), 1);
}

#[test]
fn review_pp05_refresh_missing_deleted_and_malformed_targets_invalidates_without_fallback() {
    for fault in ["missing", "deleted", "policy", "identity"] {
        let mut f = fixture();
        let conn = Connection::open(&f.path).unwrap();
        match fault {
            "missing" => { conn.execute("DELETE FROM entities WHERE kind = 'profile' AND id = ?1", [&f.a]).unwrap(); }
            "deleted" => {
                let mut other = open(&f.path);
                let mut change = incoming(&other, &f.a, String::new());
                change.change.deleted = true;
                other.apply_sync_page(&[change], 27).unwrap();
            }
            _ => {
                let payload = if fault == "policy" {
                    json!({"name": "Original invalid policy", "created_ms": 1, "preferences": {"auto_archive_hours": 2}}).to_string()
                } else { "unreadable profile identity".into() };
                conn.execute("UPDATE entities SET payload = ?1 WHERE kind = 'profile' AND id = ?2", rusqlite::params![payload, f.a]).unwrap();
            }
        }
        let before = snapshot(&f.lib);
        let revision = f.lib.revision;
        for _ in 0..2 {
            assert!(f.lib.refresh_profile(&f.a).is_err(), "accepted {fault} profile");
            assert_eq!(f.lib.effective_profile_preferences(&f.a), None);
            assert_eq!(f.lib.effective_profile_preferences(&f.b), resolved(SearchEngine::DuckDuckGo, 48));
            assert!(f.lib.set_profile_preferences(&f.a, preferences(Some(SearchEngine::Kagi), Some(1))).is_err());
            assert_eq!(f.lib.revision, revision + 1);
        }
        let after = snapshot(&f.lib);
        assert_eq!((after.entities, after.dirty, after.saved_clock, after.cursor), (before.entities, before.dirty, before.saved_clock, before.cursor));
        assert_eq!(snapshot(&f.lib), snapshot(&open(&f.path)));
    }
}

#[test]
fn review_pp02_and_pp05_unreadable_saved_clocks_reject_save_and_invalidate_refresh() {
    for fault in ["library", "profile"] {
        let mut f = fixture();
        let conn = Connection::open(&f.path).unwrap();
        if fault == "library" {
            f.lib.store.kv_set(KV_LAST_HLC, "unreadable saved clock").unwrap();
        } else {
            conn.execute("UPDATE entities SET hlc = 'unreadable saved clock' WHERE kind = 'profile' AND id = ?1", [&f.a]).unwrap();
        }
        let before = snapshot(&f.lib);
        let revision = f.lib.revision;
        assert!(f.lib.set_profile_preferences(&f.a, preferences(Some(SearchEngine::Kagi), None)).is_err());
        assert_eq!(snapshot(&f.lib), before);
        assert_eq!(f.lib.revision, revision);
        assert!(f.lib.refresh_profile(&f.a).is_err());
        assert_eq!(f.lib.effective_profile_preferences(&f.a), None, "unreadable {fault} clock left usable stale policy");
        assert_eq!(f.lib.last_hlc, before.clock);
        assert_eq!(f.lib.revision, revision + 1);
        let after = snapshot(&f.lib);
        assert_eq!((after.entities, after.dirty, after.saved_clock, after.cursor), (before.entities, before.dirty, before.saved_clock, before.cursor));
    }
}

#[test]
fn review_pp03_unavailable_policy_marker_rolls_back_with_sync_cursor_and_commit_failure() {
    for fault in ["cursor", "commit"] {
        let mut f = fixture();
        let conn = Connection::open(&f.path).unwrap();
        conn.execute("UPDATE entities SET payload = ?1 WHERE kind = 'profile' AND id = ?2",
            rusqlite::params![json!({"name": "Invalid policy", "created_ms": 1, "preferences": {"auto_archive_hours": 2}}).to_string(), f.a]).unwrap();
        f.lib = open(&f.path);
        f.lib.store.kv_set(SYNC_CURSOR_KEY, "27").unwrap();
        let before = snapshot(&f.lib);
        let revision = f.lib.revision;
        let sql = if fault == "cursor" {
            "CREATE TRIGGER reject_repair BEFORE UPDATE ON kv WHEN NEW.k = 'sync_cursor' BEGIN SELECT RAISE(ABORT, 'fixture cursor failure'); END;"
        } else {
            "CREATE TABLE fixture_parent (id INTEGER PRIMARY KEY); CREATE TABLE fixture_deferred (id INTEGER REFERENCES fixture_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER reject_repair AFTER UPDATE ON entities WHEN NEW.kind = 'profile' BEGIN INSERT INTO fixture_deferred VALUES (1); END;"
        };
        conn.execute_batch(sql).unwrap();
        let profile = Profile { name: "Valid repaired policy".into(), created_ms: 1, preferences: preferences(Some(SearchEngine::Kagi), Some(0)) };
        let changes = [incoming(&f.lib, &f.a, serde_json::to_string(&profile).unwrap())];
        assert!(f.lib.apply_sync_page(&changes, 28).is_err());
        assert_unchanged(&f, &before, revision);
        assert_eq!(f.lib.effective_profile_preferences(&f.a), None);
        conn.execute_batch("DROP TRIGGER reject_repair").unwrap();
        assert_eq!(f.lib.apply_sync_page(&changes, 28).unwrap(), 1);
        assert_eq!(f.lib.effective_profile_preferences(&f.a), resolved(SearchEngine::Kagi, 0));
        assert_eq!(snapshot(&open(&f.path)), snapshot(&f.lib));
    }
}

#[test]
fn malformed_persisted_profile_is_unavailable_not_another_profiles_preferences() {
    for payload in [
        json!({"name": "Invalid", "created_ms": 1, "preferences": {"auto_archive_hours": 2}}).to_string(),
        json!(["Array profile", 1]).to_string(),
    ] {
        let mut f = fixture();
        let before = f.lib.store.all_entities().unwrap();
        let mut malformed = before.iter().find(|row| row.kind == kinds::PROFILE && row.id == f.a).unwrap().clone();
        malformed.payload = payload;
        f.lib.store.put_entity(&malformed, false).unwrap();
        let saved = f.lib.store.all_entities().unwrap();
        f.lib = open(&f.path);
        assert_eq!(f.lib.effective_profile_preferences(&f.a), None);
        assert_eq!(f.lib.effective_profile_preferences(&f.b), resolved(SearchEngine::DuckDuckGo, 48));
        assert!(matches!(f.lib.set_profile_preferences(&f.a, None), Err(LibraryError::NotFound(_) | LibraryError::Json(_))));
        assert_eq!(f.lib.store.all_entities().unwrap(), saved);
    }
}
