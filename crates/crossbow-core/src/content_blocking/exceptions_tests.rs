//! Each profile's sites with blocking off, in a disposable library.

use super::*;
use crate::library::Library;

fn sites(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[test]
fn sites_with_blocking_off_are_kept_per_profile_and_read_back_after_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let store = Store::open(&path).unwrap();
    assert_eq!(load(&store, "personal"), Exceptions::default(), "none until one is turned off");
    save(&store, "personal", &sites(&["example.com", "news.example"])).unwrap();
    save(&store, "work", &sites(&["intranet.example"])).unwrap();
    drop(store);

    let store = Store::open(&path).unwrap();
    assert_eq!(load(&store, "personal"), Exceptions { sites: sites(&["example.com", "news.example"]), notice: None });
    assert_eq!(load(&store, "work").sites, sites(&["intranet.example"]), "each profile's own");
    save(&store, "personal", &BTreeSet::new()).unwrap();
    assert_eq!(store.kv_get(&key("personal")).unwrap(), None, "none left: the record goes");
    assert_eq!(load(&store, "personal"), Exceptions::default());
}

#[test]
fn an_unreadable_record_turns_blocking_on_everywhere_and_says_so() {
    let store = Store::open_in_memory().unwrap();
    for damaged in ["{not json", "[1, 2]", "{\"sites\": []}"] {
        store.kv_set(&key("personal"), damaged).unwrap();
        assert_eq!(load(&store, "personal"), Exceptions { sites: BTreeSet::new(), notice: Some(UNREADABLE.into()) }, "{damaged}");
    }
    save(&store, "personal", &sites(&["example.com"])).unwrap();
    assert_eq!(load(&store, "personal"), Exceptions { sites: sites(&["example.com"]), notice: None }, "saving over it");
}

#[test]
fn a_failed_save_leaves_the_saved_sites_as_they_were() {
    let store = Store::open_in_memory().unwrap();
    save(&store, "personal", &sites(&["example.com"])).unwrap();
    store.connection().execute_batch("CREATE TRIGGER refuse_update BEFORE UPDATE ON kv BEGIN SELECT RAISE(ABORT, 'fixture'); END;
        CREATE TRIGGER refuse_delete BEFORE DELETE ON kv BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
    assert_eq!(save(&store, "personal", &sites(&["example.com", "news.example"])), Err(SAVE_FAILED.into()));
    assert_eq!(save(&store, "personal", &BTreeSet::new()), Err(SAVE_FAILED.into()));
    assert_eq!(load(&store, "personal").sites, sites(&["example.com"]));
}

#[test]
fn a_site_is_the_top_pages_host_by_the_simplified_site_rule() {
    for (url, site) in [
        ("https://www.news.example.com/a?b=1#c", "example.com"),
        ("http://shop.example.co.uk/", "example.co.uk"),
        ("https://user.github.io/x", "github.io"),
        ("https://Example.COM./", "example.com"),
        ("http://127.0.0.1:8123/index.html", "127.0.0.1"),
        ("http://[::1]:8080/", "[::1]"),
        ("https://fixture-user:fixture-secret@login.example.org/", "example.org"),
    ] {
        assert_eq!(site_of(url).as_deref(), Some(site), "{url}");
    }
    for other in ["file:///fixture/notes.html", "about:blank", "data:text/html,x", "chrome://settings", "not a page", ""] {
        assert_eq!(site_of(other), None, "{other:?}");
    }
}

#[test]
fn deleting_a_profile_deletes_its_sites_with_blocking_off_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
    lib.ensure_defaults().unwrap();
    let personal = lib.spaces[&lib.spaces_ordered()[0].0].profile_id.clone();
    let work = lib.create_profile("Work").unwrap();
    save(&lib.store, &personal, &sites(&["example.com"])).unwrap();
    save(&lib.store, &work, &sites(&["intranet.example"])).unwrap();
    lib.delete_profile(&work).unwrap();
    let store = Store::open(&path).unwrap();
    assert_eq!(store.kv_get(&key(&work)).unwrap(), None, "gone with the profile");
    assert_eq!(load(&store, &personal).sites, sites(&["example.com"]), "the other profile's stay");
}
