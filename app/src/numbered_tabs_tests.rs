use super::*;
use crate::shortcuts::tests::Fixture;
use crossbow_core::model::Theme;
use crossbow_core::store::TabRow;

fn item(id: &str) -> Row {
    Row::Item(id.into())
}

fn tab_row(id: &str) -> Row {
    Row::Tab(id.into())
}

/// Two favorites, then `space` pinned or Today rows named s1, s2, …
fn fixture_rows(space: usize) -> Rows {
    Rows {
        favorites: vec![item("f1"), item("f2")],
        space: (1..=space).map(|n| if n % 2 == 0 { tab_row(&format!("s{n}")) } else { item(&format!("s{n}")) }).collect(),
    }
}

fn picks(rows: &Rows, favorites: bool, ninth: Ninth) -> Vec<Option<Row>> {
    (1..=9).map(|n| pick(rows, favorites, ninth, n)).collect()
}

#[test]
fn favorites_count_first_or_not_and_the_ninth_number_follows_the_choice() {
    // Fewer than nine rows: numbers past them do nothing, ⌘9 goes to the last only when chosen.
    let few = fixture_rows(3);
    let with = picks(&few, true, Ninth::Last);
    assert_eq!(with, [Some(item("f1")), Some(item("f2")), Some(item("s1")), Some(tab_row("s2")), Some(item("s3")), None, None, None, Some(item("s3"))]);
    let without = picks(&few, false, Ninth::Last);
    assert_eq!(without, [Some(item("s1")), Some(tab_row("s2")), Some(item("s3")), None, None, None, None, None, Some(item("s3"))]);
    assert_eq!(pick(&few, true, Ninth::Ninth, 9), None, "no ninth row, so nothing");

    // More than nine: ⌘9 is the last row or the ninth.
    let many = fixture_rows(12);
    assert_eq!(pick(&many, true, Ninth::Last, 9), Some(tab_row("s12")));
    assert_eq!(pick(&many, true, Ninth::Ninth, 9), Some(item("s7")));
    assert_eq!(pick(&many, false, Ninth::Ninth, 9), Some(item("s9")));
    assert_eq!(pick(&many, false, Ninth::Ninth, 8), Some(tab_row("s8")));
    assert_eq!(pick(&many, true, Ninth::Ninth, 1), Some(item("f1")));

    // Exactly nine: both choices agree.
    let nine = fixture_rows(7);
    assert_eq!(pick(&nine, true, Ninth::Last, 9), pick(&nine, true, Ninth::Ninth, 9));

    // Nothing to go to, or a number that isn't one.
    for ninth in Ninth::ALL {
        assert!(picks(&Rows::default(), true, ninth).iter().all(Option::is_none));
        assert!(picks(&Rows { favorites: vec![item("f1")], space: vec![] }, false, ninth).iter().all(Option::is_none), "only favorites, and they don't count");
        assert_eq!(pick(&many, true, ninth, 0), None);
        assert_eq!(pick(&many, true, ninth, 10), None);
    }
}

fn open_tab(id: &str, space: &str, item: Option<&str>, order: &str) -> Tab {
    Tab::from_row(TabRow {
        id: id.into(), space_id: space.into(), item_id: item.map(str::to_owned),
        url: format!("https://fixture.example/{id}"), title: id.into(), order: order.into(),
        created_ms: 1, last_active_ms: 2,
    })
}

#[test]
fn rows_are_the_sidebars_own_skip_collapsed_folders_and_never_cross_a_space_or_profile() {
    let fixture = Fixture::new();
    let mut lib = Library::open(fixture.store()).unwrap();
    lib.ensure_defaults().unwrap();
    let personal = lib.spaces_ordered()[0].0.clone();
    let personal_profile = lib.spaces[&personal].profile_id.clone();
    let reading = lib.create_space("Reading", "R", Theme::preset(1), &personal_profile).unwrap();
    let work_profile = lib.create_profile("Work").unwrap();
    let work = lib.create_space("Work", "W", Theme::preset(2), &work_profile).unwrap();

    let mail = lib.add_favorite(&personal_profile, "https://fixture.example/mail", "Mail").unwrap();
    let calendar = lib.add_favorite(&personal_profile, "https://fixture.example/calendar", "Calendar").unwrap();
    let tracker = lib.add_favorite(&work_profile, "https://fixture.example/tracker", "Tracker").unwrap();
    let docs = lib.add_pinned(&personal, None, "https://fixture.example/docs", "Docs").unwrap();
    let folder = lib.create_folder(&personal, None, "Projects").unwrap();
    let inside = lib.add_pinned(&personal, Some(&folder), "https://fixture.example/inside", "Inside").unwrap();
    let nested = lib.create_folder(&personal, Some(&folder), "Nested").unwrap();
    let deep = lib.add_pinned(&personal, Some(&nested), "https://fixture.example/deep", "Deep").unwrap();
    let after = lib.add_pinned(&personal, None, "https://fixture.example/after", "After").unwrap();
    let elsewhere = lib.add_pinned(&reading, None, "https://fixture.example/elsewhere", "Elsewhere").unwrap();
    // Open or not, an item counts once; Today tabs follow in their order.
    let tabs = vec![
        open_tab("docs-tab", &personal, Some(&docs), "a0"),
        open_tab("today-2", &personal, None, "a2"),
        open_tab("today-1", &personal, None, "a1"),
        open_tab("reading-today", &reading, None, "a0"),
        open_tab("work-today", &work, None, "a0"),
    ];

    let shown = rows(&lib, &tabs, &personal);
    assert_eq!(shown.favorites, [item(&mail), item(&calendar)], "this profile's favorites, open or not");
    assert_eq!(shown.space, [item(&docs), item(&inside), item(&deep), item(&after), tab_row("today-1"), tab_row("today-2")]);

    // A collapsed folder hides its rows, nested ones included; reopening it shows them again.
    lib.update_folder(&folder, |f| f.collapsed = true).unwrap();
    assert_eq!(rows(&lib, &tabs, &personal).space, [item(&docs), item(&after), tab_row("today-1"), tab_row("today-2")]);
    lib.update_folder(&folder, |f| f.collapsed = false).unwrap();
    lib.update_folder(&nested, |f| f.collapsed = true).unwrap();
    assert_eq!(rows(&lib, &tabs, &personal).space, [item(&docs), item(&inside), item(&after), tab_row("today-1"), tab_row("today-2")]);

    // Another Space of the same profile shares its favorites, not its tabs.
    let other = rows(&lib, &tabs, &reading);
    assert_eq!((&other.favorites, &other.space), (&vec![item(&mail), item(&calendar)], &vec![item(&elsewhere), tab_row("reading-today")]));
    // Another profile's Space has only its own.
    let other = rows(&lib, &tabs, &work);
    assert_eq!((&other.favorites, &other.space), (&vec![item(&tracker)], &vec![tab_row("work-today")]));
    // ⌘1 in Work never reaches Personal's favorites, even with them off.
    assert_eq!(pick(&other, false, Ninth::Last, 1), Some(tab_row("work-today")));
    assert_eq!(pick(&other, true, Ninth::Ninth, 3), None);

    // A removed Space or profile never falls back to another profile's rows.
    assert_eq!(rows(&lib, &tabs, "missing"), Rows::default());
    lib.profiles.remove(&work_profile);
    assert_eq!(rows(&lib, &tabs, &work), Rows::default());
    // A stale item or live tab never makes a number leave this Space.
    lib.items.get_mut(&inside).unwrap().profile_id = work_profile;
    let mut tabs = tabs;
    tabs[0].space_id = reading;
    let own = rows(&lib, &tabs, &personal);
    assert!(!own.space.contains(&item(&inside)) && !own.space.contains(&item(&docs)));
}

#[test]
fn both_choices_stay_on_this_device_and_a_failed_save_keeps_the_old_mapping() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let rows = fixture_rows(12);
    let mut numbered = NumberedTabs::default();
    assert!(numbered.favorites_switch(&store).enabled(), "on by default, as before");
    assert_eq!(numbered.ninth_choice(&store).choice(), Ninth::Last, "the last tab by default, as before");
    assert_eq!((numbered.target(&store, &rows, 1), numbered.target(&store, &rows, 9)), (Some(item("f1")), Some(tab_row("s12"))));

    numbered.favorites_switch(&store).choose(&store, false);
    numbered.ninth_choice(&store).choose(&store, Ninth::Ninth);
    let mut reopened = NumberedTabs::default();
    let store = fixture.store();
    assert!(!reopened.favorites_switch(&store).enabled());
    assert_eq!(reopened.ninth_choice(&store).choice(), Ninth::Ninth);
    assert_eq!((reopened.target(&store, &rows, 1), reopened.target(&store, &rows, 9)), (Some(item("s1")), Some(item("s9"))));

    // The store refuses: each choice stays as it was, offered again, and the numbers still follow it.
    let read_only = fixture.read_only();
    reopened.favorites_switch(&read_only).choose(&read_only, true);
    reopened.ninth_choice(&read_only).choose(&read_only, Ninth::Last);
    assert!(!reopened.favorites_switch(&read_only).enabled());
    assert_eq!(reopened.favorites_switch(&read_only).failure.as_ref().map(|failure| failure.wanted), Some(true));
    assert_eq!(reopened.ninth_choice(&read_only).choice(), Ninth::Ninth);
    assert_eq!(reopened.ninth_choice(&read_only).failure.as_ref().map(|failure| failure.wanted), Some(Ninth::Last));
    assert_eq!((reopened.target(&read_only, &rows, 1), reopened.target(&read_only, &rows, 9)), (Some(item("s1")), Some(item("s9"))));
    let mut again = NumberedTabs::default();
    assert_eq!(again.ninth_choice(&fixture.store()).choice(), Ninth::Ninth, "nothing was written");

    // A value this build doesn't know reads as the default and says so.
    store.kv_set(NINTH_KEY, "tenth").unwrap();
    let unknown = NinthChoice::load(&store);
    assert_eq!(unknown.choice(), Ninth::Last);
    assert!(unknown.notice.as_deref().is_some_and(|notice| notice.contains("could not be read")));
}

/// ⌘1–⌘8 and ⌘9 reach the choice through one action, and its target opens
/// as a click on that row would.
#[test]
fn every_number_dispatches_through_the_choice() {
    let app = include_str!("app.rs");
    assert!(app.contains("Command::Tab(n) => push(Action::SelectNumbered(n)),"));
    assert!(app.contains("Command::LastTab => push(Action::SelectNumbered(9)),"));
    assert!(!app.contains("SelectTabIndex"), "the old index action is gone");
    let apply = app.split("Action::SelectNumbered(n) => {").nth(1).expect("handled");
    let apply = &apply[..apply.find("Action::NextTab").expect("next arm")];
    for line in [
        "crate::numbered_tabs::rows(&self.lib, &self.tabs, &self.active_space)",
        "self.numbered.target(&self.lib.store, &rows, n)",
        "Some(crate::numbered_tabs::Row::Item(id)) => self.open_item(&id),",
        "Some(crate::numbered_tabs::Row::Tab(id)) => self.activate(&id),",
        "None => {}",
    ] {
        assert!(apply.contains(line), "{line}");
    }
}
