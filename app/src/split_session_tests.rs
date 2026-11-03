use super::*;
use crate::split_session::{self, SessionLayout};
use crossbow_core::store::TabRow;

struct Fixture {
    lib: Library,
    tabs: Vec<Tab>,
    a: String,
    a2: String,
    b: String,
    pa: String,
    pb: String,
}

impl Fixture {
    fn new(store: Store) -> Self {
        let mut lib = Library::open(store).unwrap();
        let pa = lib.create_profile("Split A").unwrap();
        let pb = lib.create_profile("Split B").unwrap();
        let a = lib.create_space("A", "A", Theme::preset(0), &pa).unwrap();
        let a2 = lib.create_space("A2", "A", Theme::preset(0), &pa).unwrap();
        let b = lib.create_space("B", "B", Theme::preset(0), &pb).unwrap();
        let favorite_a = lib.add_favorite(&pa, "http://127.0.0.1/favorite-a", "Favorite A").unwrap();
        let favorite_b = lib.add_favorite(&pb, "http://127.0.0.1/favorite-b", "Favorite B").unwrap();
        let pin = lib.add_pinned(&a, None, "http://127.0.0.1/pin", "Pin").unwrap();
        let tabs = [
            ("a-one", a.as_str(), None), ("a-two", &a, None), ("a-three", &a, None),
            ("a-four", &a, None), ("a-other", &a, None), ("a2-one", &a2, None),
            ("b-one", &b, None), ("b-two", &b, None),
            ("favorite-a", FAVORITES_SPACE, Some(favorite_a)),
            ("favorite-b", FAVORITES_SPACE, Some(favorite_b)), ("pin-a", &a, Some(pin)),
        ].into_iter().map(|(id, space, item_id)| Tab::from_row(TabRow {
            id: id.into(), space_id: space.into(), item_id, url: format!("http://127.0.0.1/{id}"),
            title: id.into(), order: "V".into(), created_ms: 1, last_active_ms: 2,
        })).collect::<Vec<_>>();
        for tab in &tabs { lib.store.put_tab(&tab.row()).unwrap(); }
        Self { lib, tabs, a, a2, b, pa, pb }
    }

    fn memory() -> Self { Self::new(Store::open(std::path::Path::new(":memory:")).unwrap()) }

    fn layout(&self, splits: &HashMap<String, Split>, active: &str, focused: Option<&str>) -> String {
        SessionLayout {
            space: &self.a, active_tabs: &HashMap::from([(self.a.clone(), active.to_string())]),
            splits, focused_pane: focused, sidebar: false, sidebar_width: 273.0,
        }.encode(&self.lib, &self.tabs)
    }
}

fn split(ids: &[&str], ratios: &[f32]) -> Split {
    Split { tabs: ids.iter().map(|id| id.to_string()).collect(), ratios: ratios.to_vec() }
}

fn state(encoded: &str) -> serde_json::Value { serde_json::from_str(encoded).unwrap() }

fn restored_splits(lib: &Library, tabs: &[Tab], state: &serde_json::Value) -> HashMap<String, Split> {
    restore_tab_layout(lib, tabs, state).1
}

fn assert_split(actual: &Split, ids: &[&str], ratios: &[f32]) {
    assert_eq!(actual.tabs, ids);
    assert_eq!(actual.ratios.len(), ratios.len());
    for (actual, expected) in actual.ratios.iter().zip(ratios) { assert!((actual - expected).abs() < 0.00001); }
    assert!((actual.ratios.iter().sum::<f32>() - 1.0).abs() < 0.00001);
}

#[test]
fn split_layout_roundtrips_two_to_four_panes_in_order_without_loading_views() {
    let f = Fixture::memory();
    for (ids, ratios) in [
        (vec!["a-two", "a-one"], vec![0.7, 0.3]),
        (vec!["a-three", "a-one", "a-two"], vec![0.2, 0.3, 0.5]),
        (vec!["a-four", "a-two", "a-one", "a-three"], vec![0.15, 0.2, 0.25, 0.4]),
    ] {
        let splits = HashMap::from([(f.a.clone(), split(&ids, &ratios))]);
        let saved = state(&f.layout(&splits, ids[0], Some(ids[1])));
        let restored = restored_splits(&f.lib, &f.tabs, &saved);
        assert_split(&restored[&f.a], &ids, &ratios);
        assert_eq!(saved["focused_pane"], ids[1]);
        assert_eq!(saved["sidebar"], false);
        assert_eq!(saved["sidebar_width"], 273.0);
        assert!(f.tabs.iter().all(|tab| tab.view.is_none()));
        assert_eq!(state(&f.layout(&restored, ids[0], Some(ids[1]))), saved, "no ratio drift on later saves");
    }
}

#[test]
fn split_layout_keeps_same_profile_shared_favorites_and_separate_spaces() {
    let f = Fixture::memory();
    let splits = HashMap::from([
        (f.a.clone(), split(&["a-one", "favorite-a", "pin-a"], &[0.2, 0.3, 0.5])),
        (f.a2.clone(), split(&["a2-one", "favorite-a"], &[0.6, 0.4])),
        (f.b.clone(), split(&["b-one", "favorite-b"], &[0.4, 0.6])),
    ]);
    let restored = restored_splits(&f.lib, &f.tabs, &state(&f.layout(&splits, "a-one", Some("favorite-a"))));
    assert_eq!(restored.len(), 3);
    assert_split(&restored[&f.a], &["a-one", "favorite-a", "pin-a"], &[0.2, 0.3, 0.5]);
    assert_split(&restored[&f.a2], &["a2-one", "favorite-a"], &[0.6, 0.4]);
    assert_split(&restored[&f.b], &["b-one", "favorite-b"], &[0.4, 0.6]);
}

#[test]
fn split_layout_prunes_deleted_moved_duplicate_and_foreign_tabs_with_matching_ratios() {
    let mut f = Fixture::memory();
    let original = HashMap::from([(f.a.clone(), split(&["a-one", "a-two", "a-three", "a-four"], &[0.2, 0.2, 0.3, 0.3]))]);
    let saved = state(&f.layout(&original, "a-one", Some("a-two")));
    f.tabs.retain(|tab| tab.id != "a-two");
    f.tabs.iter_mut().find(|tab| tab.id == "a-four").unwrap().space_id = f.a2.clone();
    let restored = restored_splits(&f.lib, &f.tabs, &saved);
    assert_split(&restored[&f.a], &["a-one", "a-three"], &[0.4, 0.6]);
    for ids in [vec!["a-one", "a-one", "a-three"], vec!["favorite-b", "a-one", "a-three"], vec!["b-one", "a-one", "a-three"]] {
        let mut corrupt = saved.clone();
        corrupt["splits"]["spaces"][&f.a]["tabs"] = serde_json::json!(ids);
        corrupt["splits"]["spaces"][&f.a]["ratios"] = serde_json::json!([0.2, 0.3, 0.5]);
        let restored = restored_splits(&f.lib, &f.tabs, &corrupt);
        assert_eq!(restored[&f.a].tabs, ["a-one", "a-three"]);
        assert_eq!(restored[&f.a].ratios.len(), 2);
    }
    f.tabs.retain(|tab| tab.id != "a-three");
    assert!(restored_splits(&f.lib, &f.tabs, &saved).is_empty());
}

#[test]
fn split_layout_rejects_reassigned_or_deleted_owners_and_invalid_pins() {
    let mut f = Fixture::memory();
    let splits = HashMap::from([(f.a.clone(), split(&["a-one", "pin-a"], &[0.4, 0.6]))]);
    let saved = state(&f.layout(&splits, "a-one", Some("pin-a")));
    f.lib.spaces.get_mut(&f.a).unwrap().profile_id = f.pb.clone();
    assert!(restored_splits(&f.lib, &f.tabs, &saved).is_empty());
    f.lib.spaces.get_mut(&f.a).unwrap().profile_id = f.pa.clone();
    let pin = f.tabs.iter().find(|tab| tab.id == "pin-a").unwrap().item_id.clone().unwrap();
    f.lib.items.get_mut(&pin).unwrap().space_id = Some(f.a2.clone());
    assert!(restored_splits(&f.lib, &f.tabs, &saved).is_empty());
    f.lib.items.get_mut(&pin).unwrap().space_id = Some(f.a.clone());
    f.lib.profiles.remove(&f.pa);
    assert!(restored_splits(&f.lib, &f.tabs, &saved).is_empty());
    f.lib.spaces.remove(&f.a);
    assert!(restored_splits(&f.lib, &f.tabs, &saved).is_empty());
}

#[test]
fn split_layout_bad_record_does_not_discard_other_spaces_or_legacy_selection() {
    let f = Fixture::memory();
    let splits = HashMap::from([
        (f.a.clone(), split(&["a-one", "a-two"], &[0.4, 0.6])),
        (f.b.clone(), split(&["b-one", "b-two"], &[0.5, 0.5])),
    ]);
    let saved = state(&f.layout(&splits, "a-one", None));
    for invalid in [serde_json::json!({}), serde_json::json!("corrupt"), serde_json::json!({"tabs": 2})] {
        let mut corrupt = saved.clone();
        corrupt["splits"]["spaces"][&f.a] = invalid;
        let restored = restored_splits(&f.lib, &f.tabs, &corrupt);
        assert_eq!(restored.len(), 1);
        assert_split(&restored[&f.b], &["b-one", "b-two"], &[0.5, 0.5]);
    }
    for invalid in [serde_json::Value::Null, serde_json::json!({"version": 2}), serde_json::json!({"version": 1, "spaces": []})] {
        let mut legacy = saved.clone();
        legacy["splits"] = invalid;
        assert!(restored_splits(&f.lib, &f.tabs, &legacy).is_empty());
        let mut active = HashMap::new();
        restore_active_tabs(&f.lib, &f.tabs, &legacy, &mut active);
        assert_eq!(active[&f.a], "a-one");
    }
    for ids in [vec!["a-one"], vec!["a-one", "a-two", "a-three", "a-four", "a-other"]] {
        let mut corrupt = saved.clone();
        corrupt["splits"]["spaces"][&f.a]["tabs"] = serde_json::json!(ids);
        assert!(!restored_splits(&f.lib, &f.tabs, &corrupt).contains_key(&f.a));
    }
}

#[test]
fn split_layout_invalid_ratios_fall_back_and_valid_weights_normalize() {
    let f = Fixture::memory();
    for ratios in [vec![], vec![1.0], vec![0.0, 1.0], vec![-1.0, 2.0], vec![f32::NAN, 1.0], vec![f32::INFINITY, 1.0], vec![0.001, 0.999]] {
        let splits = HashMap::from([(f.a.clone(), split(&["a-one", "a-two"], &ratios))]);
        let restored = restored_splits(&f.lib, &f.tabs, &state(&f.layout(&splits, "a-one", None)));
        assert_split(&restored[&f.a], &["a-one", "a-two"], &[0.5, 0.5]);
    }
    let splits = HashMap::from([(f.a.clone(), split(&["a-one", "a-two"], &[3.0, 2.0]))]);
    let mut saved = state(&f.layout(&splits, "a-one", None));
    assert_split(&restored_splits(&f.lib, &f.tabs, &saved)[&f.a], &["a-one", "a-two"], &[0.6, 0.4]);
    saved["splits"]["spaces"][&f.a]["ratios"] = serde_json::json!([0, 0]);
    assert_split(&restored_splits(&f.lib, &f.tabs, &saved)[&f.a], &["a-one", "a-two"], &[0.5, 0.5]);
}

#[test]
fn split_layout_restores_focus_only_in_the_visible_current_space() {
    let f = Fixture::memory();
    let splits = HashMap::from([(f.a.clone(), split(&["a-one", "a-two"], &[0.4, 0.6]))]);
    let focused = |active, requested| split_session::focused_pane(&f.lib, &f.tabs, &f.a, active, &splits, requested).map(str::to_owned);
    assert_eq!(focused(Some("a-one"), Some("a-two")).as_deref(), Some("a-two"));
    for requested in [None, Some("gone"), Some("b-one"), Some("favorite-b"), Some("a2-one"), Some("a-other")] {
        assert_eq!(focused(Some("a-one"), requested).as_deref(), Some("a-one"));
    }
    assert_eq!(focused(Some("a-other"), Some("a-two")).as_deref(), Some("a-other"));
    assert_eq!(focused(Some("b-one"), Some("a-two")), None);
    assert_eq!(focused(None, Some("a-two")), None);
    let saved = state(&f.layout(&splits, "a-other", Some("a-two")));
    assert_eq!(saved["focused_pane"], "a-other");
    assert_eq!(restored_splits(&f.lib, &f.tabs, &saved).len(), 1, "a hidden split remains available");
}

#[test]
fn split_layout_recovers_surviving_favorites_after_close_commits_but_layout_write_fails() {
    for survivors in [1, 2] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.sqlite");
        let mut f = Fixture::new(Store::open(&path).unwrap());
        for (id, title) in [("favorite-a2", "Favorite A2"), ("favorite-a3", "Favorite A3")] {
            let item = f.lib.add_favorite(&f.pa, "http://127.0.0.1/favorite", title).unwrap();
            let mut row = f.tabs.iter().find(|tab| tab.id == "favorite-a").unwrap().row();
            row.id = id.into();
            row.item_id = Some(item);
            f.lib.store.put_tab(&row).unwrap();
            f.tabs.push(Tab::from_row(row));
        }
        let ids = ["favorite-a", "favorite-a2", "favorite-a3"];
        let splits = HashMap::from([(f.a.clone(), split(&ids[..=survivors], &vec![1.0 / (survivors + 1) as f32; survivors + 1]))]);
        let saved = f.layout(&splits, "favorite-a", Some("favorite-a2"));
        f.lib.store.kv_set("ui_state", &saved).unwrap();
        let unrelated: Vec<_> = f.tabs.iter().filter(|tab| tab.space_id != FAVORITES_SPACE).map(|tab| tab.id.clone()).collect();
        for id in unrelated { take_closed_tab(&f.lib, &mut f.tabs, &id).unwrap(); }
        assert!(take_closed_tab(&f.lib, &mut f.tabs, "favorite-a").unwrap().is_some());
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TRIGGER reject_closed_split_layout BEFORE INSERT ON kv WHEN NEW.k = 'ui_state' BEGIN SELECT RAISE(ABORT, 'fixture split failure'); END;").unwrap();
        let mut current_splits = splits;
        current_splits.get_mut(&f.a).unwrap().remove("favorite-a");
        current_splits.retain(|_, split| split.tabs.len() >= 2);
        let latest = f.layout(&current_splits, "favorite-a2", Some("favorite-a2"));
        let mut cached = saved.clone();
        save_session_checkpoint(&f.lib.store, &mut f.tabs, &latest, &mut cached);
        assert_eq!(cached, saved);
        conn.execute_batch("DROP TRIGGER reject_closed_split_layout;").unwrap();
        let a = f.a.clone();
        drop(conn);
        drop(f);
        let lib = Library::open(Store::open(&path).unwrap()).unwrap();
        let tabs: Vec<_> = lib.store.tabs().unwrap().into_iter().map(Tab::from_row).collect();
        let saved = state(&lib.store.kv_get("ui_state").unwrap().unwrap());
        let (active, restored) = restore_tab_layout(&lib, &tabs, &saved);
        assert_eq!(active.get(&a).map(String::as_str), Some("favorite-a2"), "a committed close must not strand the surviving favorite panes");
        assert_eq!(restored.contains_key(&a), survivors == 2);
        assert_eq!(split_session::focused_pane(&lib, &tabs, &a, active.get(&a).map(String::as_str), &restored, saved["focused_pane"].as_str()), Some("favorite-a2"));
    }
}

#[test]
fn split_layout_selection_recovery_preserves_hidden_splits_and_rejects_changed_owners() {
    let mut f = Fixture::memory();
    let splits = HashMap::from([(f.a.clone(), split(&["a-one", "a-two", "a-three"], &[0.2, 0.3, 0.5]))]);
    let visible = state(&f.layout(&splits, "a-one", Some("a-three")));
    let hidden = state(&f.layout(&splits, "a-other", Some("a-other")));
    f.tabs.retain(|tab| tab.id != "a-one");
    let (active, _) = restore_tab_layout(&f.lib, &f.tabs, &visible);
    assert_eq!(active[&f.a], "a-three", "prefer the surviving saved focus, not an unrelated MRU fallback");
    let (active, _) = restore_tab_layout(&f.lib, &f.tabs, &hidden);
    assert_eq!(active[&f.a], "a-other", "do not reveal a previously hidden split");
    f.tabs.retain(|tab| tab.id != "a-other");
    let (active, _) = restore_tab_layout(&f.lib, &f.tabs, &hidden);
    assert_eq!(active[&f.a], "pin-a", "closing an unrelated selection must not reveal the hidden split");
    let mut other_space = visible.clone();
    other_space["space"] = serde_json::json!(f.b);
    let (active, _) = restore_tab_layout(&f.lib, &f.tabs, &other_space);
    assert_eq!(active[&f.a], "a-two", "global focus belongs only to the saved active Space");
    f.lib.spaces.get_mut(&f.a).unwrap().profile_id = f.pb.clone();
    let (active, restored) = restore_tab_layout(&f.lib, &f.tabs, &visible);
    assert!(!restored.contains_key(&f.a));
    assert_ne!(active.get(&f.a).map(String::as_str), Some("a-three"));
    f.lib.spaces.get_mut(&f.a).unwrap().profile_id = f.pa.clone();
    let mut unversioned = visible;
    unversioned["splits"]["version"] = serde_json::json!(2);
    let (active, restored) = restore_tab_layout(&f.lib, &f.tabs, &unversioned);
    assert!(restored.is_empty());
    assert_ne!(active.get(&f.a).map(String::as_str), Some("a-three"));
}

#[test]
fn split_layout_real_store_retry_reopen_and_dissolution_do_not_resurrect_panes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.sqlite");
    let f = Fixture::new(Store::open(&path).unwrap());
    let a = f.a.clone();
    let mut splits = HashMap::from([(a.clone(), split(&["a-three", "a-one", "a-two"], &[0.2, 0.3, 0.5]))]);
    let desired = f.layout(&splits, "a-one", Some("a-two"));
    f.lib.store.kv_set("ui_state", "{}").unwrap();
    let mut cached = "{}".to_string();
    let mut tabs = f.tabs;
    drop(f.lib);
    let uri = path.to_string_lossy().replace('%', "%25").replace('?', "%3F").replace('#', "%23");
    let ro = Store::open(std::path::Path::new(&format!("file:{uri}?mode=ro"))).unwrap();
    save_session_checkpoint(&ro, &mut tabs, &desired, &mut cached);
    assert_eq!(cached, "{}");
    assert_eq!(ro.kv_get("ui_state").unwrap().as_deref(), Some("{}"));
    drop(ro);
    let store = Store::open(&path).unwrap();
    save_session_checkpoint(&store, &mut tabs, &desired, &mut cached);
    assert_eq!(cached, desired);
    drop(store);
    let lib = Library::open(Store::open(&path).unwrap()).unwrap();
    let tabs: Vec<_> = lib.store.tabs().unwrap().into_iter().map(Tab::from_row).collect();
    let saved = state(&lib.store.kv_get("ui_state").unwrap().unwrap());
    let restored = restored_splits(&lib, &tabs, &saved);
    assert_split(&restored[&a], &["a-three", "a-one", "a-two"], &[0.2, 0.3, 0.5]);
    let mut active = HashMap::new();
    restore_active_tabs(&lib, &tabs, &saved, &mut active);
    assert_eq!(active[&a], "a-one");
    assert_eq!(split_session::focused_pane(&lib, &tabs, &a, active.get(&a).map(String::as_str), &restored, saved["focused_pane"].as_str()), Some("a-two"));
    assert!(tabs.iter().all(|tab| tab.view.is_none()));
    splits.clear();
    let dissolved = SessionLayout { space: &a, active_tabs: &active, splits: &splits, focused_pane: Some("a-one"), sidebar: true, sidebar_width: 252.0 }.encode(&lib, &tabs);
    let mut tabs = tabs;
    save_session_checkpoint(&lib.store, &mut tabs, &dissolved, &mut cached);
    drop(lib);
    let lib = Library::open(Store::open(&path).unwrap()).unwrap();
    assert!(restored_splits(&lib, &tabs, &state(&lib.store.kv_get("ui_state").unwrap().unwrap())).is_empty());
    assert_eq!(lib.store.tabs().unwrap().len(), tabs.len(), "dissolving a split must not close its tabs");
}

#[test]
fn split_layout_checkpoint_does_not_advance_past_failed_tab_writes_and_retries_latest_layout() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.sqlite");
    let mut f = Fixture::new(Store::open(&path).unwrap());
    let splits = HashMap::from([(f.a.clone(), split(&["a-one", "a-two"], &[0.4, 0.6]))]);
    let previous = f.layout(&splits, "a-one", Some("a-one"));
    let desired = f.layout(&splits, "a-one", Some("a-two"));
    f.lib.store.kv_set("ui_state", &previous).unwrap();
    f.tabs[0].title = "Edited synthetic title".into();
    f.tabs[0].dirty = true;
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_fixture_tab BEFORE INSERT ON tabs BEGIN SELECT RAISE(ABORT, 'fixture tab failure'); END;").unwrap();
    let mut cached = previous.clone();
    save_session_checkpoint(&f.lib.store, &mut f.tabs, &desired, &mut cached);
    assert!(f.tabs[0].dirty);
    assert_eq!(cached, previous);
    assert_eq!(f.lib.store.kv_get("ui_state").unwrap().as_deref(), Some(previous.as_str()));
    conn.execute_batch("DROP TRIGGER reject_fixture_tab; CREATE TRIGGER reject_fixture_layout BEFORE INSERT ON kv WHEN NEW.k = 'ui_state' BEGIN SELECT RAISE(ABORT, 'fixture layout failure'); END;").unwrap();
    save_session_checkpoint(&f.lib.store, &mut f.tabs, &desired, &mut cached);
    assert!(!f.tabs[0].dirty);
    assert_eq!(cached, previous);
    conn.execute_batch("DROP TRIGGER reject_fixture_layout;").unwrap();
    let changed_splits = HashMap::from([(f.a.clone(), split(&["a-two", "a-one"], &[0.7, 0.3]))]);
    let latest = f.layout(&changed_splits, "a-two", Some("a-two"));
    save_session_checkpoint(&f.lib.store, &mut f.tabs, &latest, &mut cached);
    assert_eq!(cached, latest);
    conn.execute_batch("CREATE TRIGGER reject_duplicate_layout BEFORE INSERT ON kv WHEN NEW.k = 'ui_state' BEGIN SELECT RAISE(ABORT, 'unchanged layout wrote again'); END;").unwrap();
    save_session_checkpoint(&f.lib.store, &mut f.tabs, &latest, &mut cached);
    assert_eq!(cached, latest);
    conn.execute_batch("DROP TRIGGER reject_duplicate_layout;").unwrap();
    let a = f.a.clone();
    drop(conn);
    drop(f);
    let lib = Library::open(Store::open(&path).unwrap()).unwrap();
    let tabs = lib.store.tabs().unwrap().into_iter().map(Tab::from_row).collect::<Vec<_>>();
    let restored = restored_splits(&lib, &tabs, &state(&lib.store.kv_get("ui_state").unwrap().unwrap()));
    assert_split(&restored[&a], &["a-two", "a-one"], &[0.7, 0.3]);
    assert_eq!(tabs.iter().find(|tab| tab.id == "a-one").unwrap().title, "Edited synthetic title");
}
