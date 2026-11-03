//! Moving a Space to another profile and deleting unused profiles. The library
//! commits first; live views close only after it succeeds, and a view opened
//! under one profile is never reused by another.

use super::*;
use crossbow_core::library::LibraryError;
use std::path::Path;

/// How a confirmed profile deletion ended.
#[derive(Debug, PartialEq, Eq)]
pub struct ProfileDeletion {
    /// The profile is gone, but some of its site data or captures are still
    /// on disk and are removed the next time Crossbow starts.
    pub data_pending: Option<String>,
    /// Backups that couldn't be cleaned of its history, archive and site
    /// permissions.
    pub backups: crossbow_core::backup::Removed,
}

/// Commit `space`'s move to `profile` and return its tabs (Today and pinned),
/// whose live views belong to the old profile. Nothing changes on failure.
pub(crate) fn commit_space_profile(lib: &mut Library, tabs: &[Tab], space: &str, profile: &str) -> Result<Vec<String>, LibraryError> {
    lib.set_space_profile(space, profile)?;
    Ok(tabs.iter().filter(|tab| tab.space_id == space).map(|tab| tab.id.clone()).collect())
}

/// Drop split panes that no longer belong to `space`'s profile, such as the
/// old profile's favorites, keeping the other panes in order.
pub(crate) fn retain_split_panes(lib: &Library, tabs: &[Tab], splits: &mut HashMap<String, Split>, space: &str) {
    let Some(split) = splits.get_mut(space) else { return };
    let profile = crate::downloads::active_profile(lib, space);
    let stale: Vec<String> = split.tabs.iter()
        .filter(|id| profile.is_none() || tabs.iter().find(|tab| tab.id == **id).and_then(|tab| tab_metadata_profile(lib, tab)) != profile)
        .cloned().collect();
    for id in &stale { split.remove(id); }
    if split.tabs.len() < 2 { splits.remove(space); }
}

/// Take Peek when it was opened under another profile than the active Space's.
pub(crate) fn take_stale_peek<T>(lib: &Library, active_space: &str, peek: &mut Option<T>, owner: impl FnOnce(&T) -> &str) -> Option<T> {
    let profile = crate::downloads::active_profile(lib, active_space);
    peek.take_if(|peek| profile != Some(owner(peek)))
}

/// The profile for a new Space: the one chosen in Settings → Spaces while it
/// can still be used, otherwise the active Space's.
pub(crate) fn new_space_profile(lib: &Library, chosen: Option<&str>, active_space: &str) -> String {
    match chosen {
        Some(id) if lib.effective_profile_preferences(id).is_some() => id.to_owned(),
        _ => crate::downloads::active_profile(lib, active_space).map_or_else(|| lib.default_profile_id(), str::to_owned),
    }
}

/// Live tabs opened from `profile`'s favorites.
pub(crate) fn profile_favorite_tabs(lib: &Library, tabs: &[Tab], profile: &str) -> Vec<String> {
    tabs.iter()
        .filter(|tab| tab.space_id == FAVORITES_SPACE && tab.item_id.as_ref().and_then(|id| lib.items.get(id))
            .is_some_and(|item| item.kind == ItemKind::Favorite && item.profile_id == profile))
        .map(|tab| tab.id.clone()).collect()
}

/// Move `profile`'s Chromium folder and captures aside, commit its library
/// deletion, then remove them and its content blocking lists. A failed
/// commit puts both back. Call once the profile's views are closed.
pub(crate) fn delete_profile_and_data(lib: &mut Library, data_dir: &Path, profile: &str) -> Result<ProfileDeletion, String> {
    lib.check_profile_deletion(profile).map_err(|error| error.to_string())?;
    let detached = crossbow_engine::detach_profile_data(data_dir, profile)
        .map_err(|error| format!("Its site data could not be removed ({error}). Nothing was deleted."))?;
    let captures = match crossbow_core::captures::detach_profile(data_dir, profile) {
        Ok(captures) => captures,
        Err(error) => {
            let restored = detached.map(crossbow_engine::DetachedProfileData::restore).transpose();
            return Err(match restored {
                Ok(_) => format!("{error} Nothing was deleted."),
                Err(restore) => format!("{error} Its site data was set aside and could not be put back ({restore}); Crossbow removes it the next time it starts."),
            });
        }
    };
    let backups = match lib.delete_profile(profile) {
        Ok(backups) => backups,
        Err(error) => {
            let restored = detached.map(crossbow_engine::DetachedProfileData::restore).transpose();
            let captures = captures.map(crossbow_core::captures::DetachedCaptures::restore).transpose();
            return Err(match (restored, captures) {
                (Ok(_), Ok(_)) => format!("{error} Nothing was deleted."),
                (Err(restore), _) => format!("{error} Its site data was set aside and could not be put back ({restore}); Crossbow removes it the next time it starts."),
                // The profile still exists, so the next launch puts them back.
                (Ok(_), Err(_)) => format!("{error} Nothing was deleted. Its captures go back in place the next time Crossbow starts."),
            });
        }
    };
    let site_data = detached.map(crossbow_engine::DetachedProfileData::remove).transpose().map_err(|error| error.to_string());
    // Said as what is left, inside the note's own sentence.
    let captures = captures.map(crossbow_core::captures::DetachedCaptures::remove).transpose().map_err(|_| "its captures".to_string());
    let lists = crossbow_core::content_blocking::lists::remove_profile(data_dir, profile).map_err(|_| "its content blocking lists".to_string());
    let data_pending = match (site_data, captures, lists) {
        (Ok(_), Ok(_), Ok(())) => None,
        (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => Some(error),
    };
    Ok(ProfileDeletion { data_pending, backups })
}

impl CrossbowApp {
    /// Move a Space to another profile. Its live views (tabs, split panes and
    /// a Peek over it) close; the visible ones reopen in the new profile on the
    /// next frame and the others when shown. Failure changes nothing.
    pub(super) fn move_space_to_profile(&mut self, space: &str, profile: &str) -> Result<(), String> {
        let selection = self.tab_selection();
        let close = commit_space_profile(&mut self.lib, &self.tabs, space, profile).map_err(|error| error.to_string())?;
        for id in &close { self.close_live_view(id); }
        retain_split_panes(&self.lib, &self.tabs, &mut self.splits, space);
        if let Some(peek) = take_stale_peek(&self.lib, &self.active_space, &mut self.ui.peek, |peek| peek.view.profile_id()) {
            retire_peek_textures(&self.texture_retirement, peek.texture, peek.popup_texture);
            peek.view.close();
        }
        self.ui.menu = None;
        self.reconcile_selection(selection);
        Ok(())
    }

    /// Delete a profile no Space uses: close its views, then remove its data
    /// with [`delete_profile_and_data`].
    pub(super) fn delete_profile(&mut self, profile: &str) -> Result<ProfileDeletion, String> {
        self.lib.check_profile_deletion(profile).map_err(|error| error.to_string())?;
        let favorites = profile_favorite_tabs(&self.lib, &self.tabs, profile);
        let live: Vec<String> = self.tabs.iter()
            .filter(|tab| tab.view.as_ref().is_some_and(|view| view.profile_id() == profile))
            .map(|tab| tab.id.clone()).collect();
        for id in favorites.iter().chain(&live) { self.close_live_view(id); }
        if let Some(peek) = self.ui.peek.take_if(|peek| peek.view.profile_id() == profile) {
            retire_peek_textures(&self.texture_retirement, peek.texture, peek.popup_texture);
            peek.view.close();
        }
        self.close_profile_little_windows(profile);
        let selection = self.tab_selection();
        let deleted = delete_profile_and_data(&mut self.lib, &self.data_dir, profile)?;
        self.content_blocking.forget(profile);
        // Its sites went in the same commit.
        self.site_profiles = crossbow_core::site_profiles::SiteProfiles::load(&self.lib.store);
        let removed: Vec<Tab> = self.tabs.extract_if(.., |tab| favorites.contains(&tab.id)).collect();
        for tab in removed { self.finish_closed_tab(tab); }
        self.reconcile_selection(selection);
        if self.ui.new_space_profile.as_deref() == Some(profile) { self.ui.new_space_profile = None; }
        Ok(deleted)
    }

    /// Close one tab's live view and forget its frame state. The tab record,
    /// URL and position stay; ensure_view opens a fresh view when it is shown.
    fn close_live_view(&mut self, id: &str) {
        let Some(tab) = self.tab_mut(id) else { return };
        if tab.view.is_none() { return; }
        tab.unload();
        self.texture_retirement.retire_tab(id, &mut self.textures, &mut self.popup_textures);
        self.hidden_views.remove(id);
        self.errors.remove(id);
        self.ui.pointer_over.remove(id);
        if self.ui.pointer_capture.as_deref() == Some(id) { self.ui.pointer_capture = None; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbow_core::store::TabRow;
    use std::rc::Rc;

    fn tab(id: &str, space: &str, item: Option<&str>) -> Tab {
        Tab::from_row(TabRow {
            id: id.into(), space_id: space.into(), item_id: item.map(str::to_owned),
            url: format!("https://fixture.example/{id}"), title: id.into(), order: "a0".into(),
            created_ms: 1, last_active_ms: 2,
        })
    }

    struct Fixture {
        lib: Library,
        a: String,
        b: String,
        space: String,
        sibling: String,
        b_space: String,
        a_favorite: String,
        b_favorite: String,
        tabs: Vec<Tab>,
    }

    fn fixture() -> Fixture {
        let mut lib = Library::open(Store::open(std::path::Path::new(":memory:")).unwrap()).unwrap();
        let a = lib.create_profile("Personal").unwrap();
        let b = lib.create_profile("Work").unwrap();
        let space = lib.create_space("Moved", "M", Theme::preset(0), &a).unwrap();
        let sibling = lib.create_space("Sibling", "S", Theme::preset(1), &a).unwrap();
        let b_space = lib.create_space("Work", "W", Theme::preset(2), &b).unwrap();
        let pin = lib.add_pinned(&space, None, "https://fixture.example/pin", "Pin").unwrap();
        let a_favorite = lib.add_favorite(&a, "https://fixture.example/a-favorite", "A favorite").unwrap();
        let b_favorite = lib.add_favorite(&b, "https://fixture.example/b-favorite", "B favorite").unwrap();
        let tabs = vec![
            tab("today", &space, None), tab("pinned", &space, Some(&pin)), tab("sibling", &sibling, None),
            tab("a-favorite", FAVORITES_SPACE, Some(&a_favorite)), tab("b-favorite", FAVORITES_SPACE, Some(&b_favorite)),
            tab("work", &b_space, None),
        ];
        Fixture { lib, a, b, space, sibling, b_space, a_favorite, b_favorite, tabs }
    }

    fn find<'a>(tabs: &'a [Tab], id: &str) -> &'a Tab {
        tabs.iter().find(|tab| tab.id == id).unwrap()
    }

    #[test]
    fn space_move_closes_only_that_spaces_views_and_reopens_them_under_the_new_profile() {
        let mut f = fixture();
        for id in ["today", "pinned"] {
            assert_eq!(tab_profile_with_view_owner(&f.lib, find(&f.tabs, id), Some(&f.a)), Some(f.a.as_str()));
        }
        let mut closed = commit_space_profile(&mut f.lib, &f.tabs, &f.space, &f.b).unwrap();
        closed.sort();
        assert_eq!(closed, ["pinned", "today"], "only the moved Space's live views close");
        for id in ["today", "pinned"] {
            let tab = find(&f.tabs, id);
            assert_eq!(tab_profile_with_view_owner(&f.lib, tab, Some(&f.a)), None, "{id}: an old-profile view is never reused");
            assert_eq!(tab_profile(&f.lib, tab), Some(f.b.as_str()), "{id}: the reopened view uses the new profile");
        }
        for (id, profile) in [("sibling", &f.a), ("a-favorite", &f.a), ("b-favorite", &f.b), ("work", &f.b)] {
            assert_eq!(tab_profile_with_view_owner(&f.lib, find(&f.tabs, id), Some(profile)), Some(profile.as_str()), "{id} keeps its view");
        }
        let favorites: Vec<_> = f.lib.favorites(&f.lib.spaces[&f.space].profile_id).into_iter().map(|(id, _)| id).collect();
        assert_eq!(favorites, [f.b_favorite.clone()], "the favorites row shows the new profile's favorites");
        assert!(can_move_tab(&f.lib, find(&f.tabs, "today"), &f.b_space));
        assert!(!can_move_tab(&f.lib, find(&f.tabs, "today"), &f.sibling));
    }

    #[test]
    fn space_move_split_drops_old_favorites_keeps_pane_order_and_selects_a_survivor() {
        let mut f = fixture();
        let mut splits = HashMap::from([
            (f.space.clone(), Split { tabs: vec!["a-favorite".into(), "today".into(), "pinned".into()], ratios: vec![0.5, 0.25, 0.25] }),
            (f.sibling.clone(), Split { tabs: vec!["sibling".into(), "a-favorite".into()], ratios: vec![0.5, 0.5] }),
        ]);
        let mut selection = RemovalSelection {
            space: f.space.clone(), profile: f.a.clone(),
            active: HashMap::from([(f.space.clone(), "a-favorite".into()), (f.sibling.clone(), "sibling".into())]),
            focused: Some("a-favorite".into()), panes: vec!["a-favorite".into(), "today".into(), "pinned".into()],
            splits: splits.clone(), order: vec!["a-favorite".into(), "pinned".into(), "today".into()],
        };
        commit_space_profile(&mut f.lib, &f.tabs, &f.space, &f.b).unwrap();
        retain_split_panes(&f.lib, &f.tabs, &mut splits, &f.space);
        let moved = &splits[&f.space];
        assert_eq!(moved.tabs, ["today", "pinned"], "pinned and Today panes keep their order");
        assert!((moved.ratios.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert_eq!(splits[&f.sibling].tabs, ["sibling", "a-favorite"], "other Spaces keep their layout");

        selection.reconcile(&f.lib, &f.tabs);
        assert_eq!(selection.active.get(&f.space).map(String::as_str), Some("today"));
        assert_eq!(selection.focused.as_deref(), Some("today"));
        assert_eq!(selection.active.get(&f.sibling).map(String::as_str), Some("sibling"));

        // A split left with one pane is dissolved, like any other removal.
        let mut pair = HashMap::from([(f.space.clone(), Split::new("b-favorite".into(), "today".into())), (f.b_space.clone(), Split::new("work".into(), "b-favorite".into()))]);
        commit_space_profile(&mut f.lib, &f.tabs, &f.space, &f.a).unwrap();
        retain_split_panes(&f.lib, &f.tabs, &mut pair, &f.space);
        assert!(!pair.contains_key(&f.space));
        assert_eq!(pair[&f.b_space].tabs, ["work", "b-favorite"]);
    }

    #[test]
    fn failed_space_move_reports_no_views_to_close_and_retry_commits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("move.sqlite");
        let mut lib = Library::open(Store::open(&path).unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let space = lib.spaces_ordered()[0].0.clone();
        let other = lib.create_profile("Work").unwrap();
        let tabs = vec![tab("today", &space, None)];
        let (spaces, entities) = (lib.spaces.clone(), lib.store.all_entities().unwrap());
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(&format!("CREATE TRIGGER reject_move BEFORE UPDATE ON entities WHEN NEW.id = '{space}' BEGIN SELECT RAISE(ABORT, 'fixture'); END;")).unwrap();
        assert!(commit_space_profile(&mut lib, &tabs, &space, &other).is_err());
        assert_eq!(lib.spaces, spaces);
        assert_eq!(lib.store.all_entities().unwrap(), entities);
        assert_eq!(tab_profile(&lib, &tabs[0]), Some(lib.default_profile_id().as_str()), "the tab still belongs to its old profile");
        connection.execute_batch("DROP TRIGGER reject_move;").unwrap();
        assert_eq!(commit_space_profile(&mut lib, &tabs, &space, &other).unwrap(), ["today"]);
        drop(lib);
        assert_eq!(Library::open(Store::open(&path).unwrap()).unwrap().spaces[&space].profile_id, other);
    }

    #[test]
    fn peek_closes_only_when_another_profile_owns_it() {
        let mut f = fixture();
        let original = Rc::new((f.a.clone(), "live Peek"));
        let mut peek = Some(original.clone());
        assert!(take_stale_peek(&f.lib, &f.space, &mut peek, |peek| peek.0.as_str()).is_none());
        assert!(peek.is_some(), "Peek in the active Space's profile stays open");
        commit_space_profile(&mut f.lib, &f.tabs, &f.space, &f.b).unwrap();
        assert!(take_stale_peek(&f.lib, &f.sibling, &mut peek, |peek| peek.0.as_str()).is_none(), "a Peek over a Space still in its profile stays");
        let taken = take_stale_peek(&f.lib, &f.space, &mut peek, |peek| peek.0.as_str()).unwrap();
        assert!(Rc::ptr_eq(&taken, &original));
        assert!(peek.is_none());
    }

    #[test]
    fn new_spaces_use_the_settings_choice_while_it_exists() {
        let mut f = fixture();
        assert_eq!(new_space_profile(&f.lib, None, &f.space), f.a);
        assert_eq!(new_space_profile(&f.lib, None, &f.b_space), f.b);
        assert_eq!(new_space_profile(&f.lib, Some(&f.b), &f.space), f.b);
        assert_eq!(new_space_profile(&f.lib, Some("deleted"), &f.space), f.a);
        assert_eq!(new_space_profile(&f.lib, Some(""), &f.b_space), f.b);
        // A profile whose saved policy is unreadable is never chosen for new data.
        f.lib.store.put_entity(&crossbow_core::store::EntityRow {
            kind: crossbow_core::proto::kinds::PROFILE.into(), id: f.b.clone(), hlc: crossbow_core::proto::Hlc::new(crossbow_core::now_ms() + 60_000, 0, "zz"), deleted: false,
            payload: r#"{"name":"Work","created_ms":1,"preferences":{"auto_archive_hours":2}}"#.into(),
        }, false).unwrap();
        assert!(f.lib.reload_profile(&f.b).is_err());
        assert_eq!(new_space_profile(&f.lib, Some(&f.b), &f.space), f.a);
    }

    #[test]
    fn profile_deletion_closes_only_that_profiles_favorite_tabs() {
        let f = fixture();
        assert_eq!(profile_favorite_tabs(&f.lib, &f.tabs, &f.b), ["b-favorite"]);
        assert_eq!(profile_favorite_tabs(&f.lib, &f.tabs, &f.a), ["a-favorite"]);
        assert!(profile_favorite_tabs(&f.lib, &f.tabs, "missing").is_empty());
        let _ = (&f.a_favorite, &f.b_favorite);
    }

    /// The deletion commits but some capture files won't go: the profile is
    /// gone, the note names what is left, its records stay until their files
    /// have gone, and the next launch finishes.
    #[test]
    fn captures_a_committed_deletion_leaves_are_reported_and_removed_at_the_next_launch() {
        use crossbow_core::captures::{Captures, NewCapture, finish_deletions};
        use std::os::unix::fs::PermissionsExt;
        let mut f = fixture();
        let spare = f.lib.create_profile("Spare").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let captures = Captures::open(dir.path()).unwrap();
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x03\0\0\0\x02";
        captures.save(&NewCapture { profile_id: &spare, url: "", title: "", created_ms: 1_793_286_245_000, offset_secs: 0, width: 3, height: 2, png }).unwrap();
        let folder = captures.folder(&spare).unwrap();
        let locked = folder.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("file"), b"kept").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();

        assert_eq!(delete_profile_and_data(&mut f.lib, dir.path(), &spare), Ok(ProfileDeletion { data_pending: Some("its captures".into()), backups: Default::default() }));
        assert!(!f.lib.profiles.contains_key(&spare) && captures.list(&spare).unwrap().len() == 1);
        let aside = folder.with_file_name(format!(".{}.deleting", folder.file_name().unwrap().to_string_lossy()));
        std::fs::set_permissions(aside.join("locked"), std::fs::Permissions::from_mode(0o700)).unwrap();
        finish_deletions(dir.path(), |profile| f.lib.profiles.contains_key(profile));
        assert_eq!(std::fs::read_dir(dir.path().join("Captures")).unwrap().count(), 0, "nothing left");
        assert!(captures.list(&spare).unwrap().is_empty());
    }

    #[test]
    fn deleting_a_profile_deletes_its_content_blocking_lists_only() {
        use crossbow_core::content_blocking::lists::{self, ListKind};
        let mut f = fixture();
        let spare = f.lib.create_profile("Spare").unwrap();
        let dir = tempfile::tempdir().unwrap();
        for profile in [&spare, &f.a] {
            lists::add(dir.path(), profile, ListKind::Ads, "ads.txt", b"||ads.example^\n", 1).unwrap();
        }
        assert_eq!(delete_profile_and_data(&mut f.lib, dir.path(), &spare), Ok(ProfileDeletion { data_pending: None, backups: Default::default() }));
        assert!(lists::load(dir.path(), &spare).lists.is_empty());
        assert_eq!(std::fs::read_dir(dir.path().join(lists::FOLDER)).unwrap().count(), 1, "its folder is gone");
        assert_eq!(lists::load(dir.path(), &f.a).lists.len(), 1, "another profile keeps its lists");
    }
}
