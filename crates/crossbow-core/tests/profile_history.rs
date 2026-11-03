//! Disk-backed profile history, legacy-data and failure/retry regressions.

use crossbow_core::store::Store;
use rusqlite::Connection;

#[test]
fn visits_titles_counts_and_clears_stay_in_their_profile_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let store = Store::open(&path).unwrap();
    let url = "https://shared.example/";
    store.record_visit("a", url, "A private title", 1).unwrap();
    store.record_visit("b", url, "B private title", 2).unwrap();
    store.record_visit("a", url, "", 3).unwrap();
    let a = store.recent_history("a", 10).unwrap();
    let b = store.recent_history("b", 10).unwrap();
    assert_eq!((a[0].title.as_str(), a[0].visit_count, a[0].last_visit_ms), ("A private title", 2, 3));
    assert_eq!((b[0].title.as_str(), b[0].visit_count, b[0].last_visit_ms), ("B private title", 1, 2));
    store.update_history_title("a", url, "A renamed").unwrap();
    assert_eq!(store.recent_history("b", 10).unwrap(), b);
    drop(store);

    let store = Store::open(&path).unwrap();
    assert_eq!(store.recent_history("a", 10).unwrap()[0].title, "A renamed");
    assert_eq!(store.recent_history("b", 10).unwrap(), b);
    store.clear_history("a").unwrap();
    assert!(store.recent_history("a", 10).unwrap().is_empty());
    assert_eq!(store.recent_history("b", 10).unwrap(), b);
    drop(store);
    let store = Store::open(&path).unwrap();
    assert!(store.recent_history("a", 10).unwrap().is_empty());
    assert_eq!(store.recent_history("b", 10).unwrap(), b);
    assert!(store.recent_history("unknown", 10).unwrap().is_empty());
}

#[test]
fn legacy_global_history_is_preserved_but_never_assigned_or_exposed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TABLE history (url TEXT PRIMARY KEY, title TEXT NOT NULL, visit_count INTEGER NOT NULL, last_visit_ms INTEGER NOT NULL);
        INSERT INTO history VALUES ('https://old.example/', 'Legacy private title', 99, 123);").unwrap();
    let store = Store::open(&path).unwrap();
    for profile in ["a", "b", ""] {
        assert!(store.recent_history(profile, 10).unwrap().is_empty());
    }
    store.record_visit("a", "https://old.example/", "A new visit", 124).unwrap();
    let a = store.recent_history("a", 10).unwrap();
    assert_eq!((a[0].title.as_str(), a[0].visit_count), ("A new visit", 1));
    assert!(store.recent_history("b", 10).unwrap().is_empty());
    store.clear_history("a").unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert!(store.recent_history("a", 10).unwrap().is_empty());
    assert!(store.recent_history("b", 10).unwrap().is_empty());
    let legacy: (String, u32, i64) = connection.query_row("SELECT title, visit_count, last_visit_ms FROM history", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
    assert_eq!(legacy, ("Legacy private title".into(), 99, 123));
}

#[test]
fn profile_filter_applies_before_recent_limit() {
    let store = Store::open_in_memory().unwrap();
    store.record_visit("a", "https://a.example/old", "Older A", 1).unwrap();
    store.record_visit("a", "https://a.example/new", "Newer A", 2).unwrap();
    for i in 3..20 {
        store.record_visit("b", &format!("https://b.example/{i}"), "B only", i).unwrap();
    }
    let rows = store.recent_history("a", 1).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Newer A");
    assert!(store.recent_history("a", 0).unwrap().is_empty());
}

#[test]
fn failed_writes_keep_rows_and_revision_until_successful_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let store = Store::open(&path).unwrap();
    let url = "https://a.example/";
    store.record_visit("a", url, "Kept", 1).unwrap();
    store.record_visit("b", url, "B kept", 1).unwrap();
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_history_insert BEFORE INSERT ON profile_history BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;
        CREATE TRIGGER reject_history_update BEFORE UPDATE ON profile_history BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;
        CREATE TRIGGER reject_history_delete BEFORE DELETE ON profile_history BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;").unwrap();
    let revision = store.history_revision();
    let before = store.recent_history("a", 10).unwrap();
    let b = store.recent_history("b", 10).unwrap();
    assert!(store.record_visit("a", url, "Rejected visit", 2).is_err());
    assert!(store.update_history_title("a", url, "Rejected title").is_err());
    assert!(store.clear_history("a").is_err());
    assert_eq!(store.history_revision(), revision);
    assert_eq!(store.recent_history("a", 10).unwrap(), before);
    assert_eq!(store.recent_history("b", 10).unwrap(), b);
    connection.execute_batch("DROP TRIGGER reject_history_insert; DROP TRIGGER reject_history_update; DROP TRIGGER reject_history_delete;").unwrap();
    store.record_visit("a", url, "Retry visit", 2).unwrap();
    store.update_history_title("a", url, "Retry title").unwrap();
    assert_eq!(store.history_revision(), revision + 2);
    assert_eq!(store.recent_history("a", 10).unwrap()[0].title, "Retry title");
    store.clear_history("a").unwrap();
    assert_eq!(store.history_revision(), revision + 3);
    drop(store);
    let store = Store::open(&path).unwrap();
    assert!(store.recent_history("a", 10).unwrap().is_empty());
    assert_eq!(store.recent_history("b", 10).unwrap(), b);
    assert_eq!(connection.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0)).unwrap(), "ok");
}
