//! The Restore Data panel, drawn as Settings draws it at both window sizes,
//! against real backups of a disposable library.

use super::super::repair_tests::{PreferenceFrame, settings_test_frame};
use super::*;
use crate::shortcuts::tests::Fixture;
use crossbow_core::backup::Reason;

const SIZES: [egui::Vec2; 2] = [vec2(640.0, 420.0), vec2(1360.0, 860.0)];

struct Harness {
    fixture: Fixture,
    lib: Library,
    st: Restore,
    launch_error: Option<String>,
    requests: Vec<Request>,
    ctx: egui::Context,
    size: egui::Vec2,
    frame: u32,
}

impl Harness {
    fn new(size: egui::Vec2) -> Self {
        let fixture = Fixture::new();
        let mut lib = Library::open(fixture.store()).unwrap();
        lib.ensure_defaults().unwrap();
        let space = lib.spaces_ordered()[0].0.clone();
        lib.add_pinned(&space, None, "https://example.test/a", "A").unwrap();
        let ctx = egui::Context::default();
        crate::ui::window::install_fonts(&ctx);
        Harness { fixture, lib, st: Restore::default(), launch_error: None, requests: Vec::new(), ctx, size, frame: 0 }
    }

    fn path(&self) -> PathBuf {
        self.fixture.path()
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> PreferenceFrame {
        settings_test_frame(&self.ctx, Page::Advanced, self.size, 1.0, &mut self.frame, events, |ui, pal| {
            if let Some(request) = show(&mut self.st, &self.lib, self.launch_error.as_deref(), ui, pal) { self.requests.push(request); }
        })
    }

    /// Draw until every check is in, then once more. Every label stays inside
    /// the panel's width and clear of every other label.
    fn settle(&mut self) -> PreferenceFrame {
        for _ in 0..200 {
            self.frame(vec![]);
            if self.st.receiver.is_none() { break; }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        for _ in 0..4 { self.frame(vec![]); }
        let shown = self.frame(vec![]);
        for (i, (text, bounds, clip)) in shown.text.iter().enumerate() {
            assert!(bounds.left() >= clip.left() - 0.5 && bounds.right() <= clip.right() + 0.5, "{text:?} overflows at {:?}: {bounds:?} in {clip:?}", self.size);
            assert!(!text.ends_with('…') || ["Restore Data…", "Restore…", "Delete All Backups…", CHECKING].contains(&text.as_str()), "{text:?} is cut short at {:?}", self.size);
            // Only what shows: a label scrolled under the page title is clipped away.
            let seen = bounds.intersect(*clip).shrink(0.5);
            for (other, other_bounds, other_clip) in &shown.text[i + 1..] {
                let other_seen = other_bounds.intersect(*other_clip).shrink(0.5);
                assert!(!seen.is_positive() || !other_seen.is_positive() || !seen.intersects(other_seen), "{text:?} overlaps {other:?} at {:?}", self.size);
            }
        }
        shown
    }

    /// Scroll until the `nth` label reading `label` is fully in view.
    fn visible(&mut self, label: &str, nth: usize) -> (PreferenceFrame, egui::Pos2) {
        let scroll_point = pos2(self.size.x / 2.0, self.size.y * 0.6);
        for attempt in 0..40 {
            let shown = self.settle();
            let found = shown.text.iter().filter(|(text, ..)| text == label).nth(nth).map(|(_, bounds, clip)| (*bounds, *clip));
            let delta = match found {
                Some((bounds, clip)) if clip.expand(0.5).contains_rect(bounds) => return (shown, bounds.center()),
                Some((bounds, clip)) => if bounds.center().y > clip.center().y { -80.0 } else { 80.0 },
                None => if attempt < 20 { -80.0 } else { 80.0 },
            };
            self.frame(vec![
                egui::Event::PointerMoved(scroll_point),
                egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, delta), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() },
            ]);
        }
        let (size, shown) = (self.size, self.settle());
        panic!("{label:?} (#{nth}) never scrolled into view at {size:?}: {:?}", shown.text);
    }

    fn shows(&mut self, label: &str) -> bool {
        self.settle().text.iter().any(|(text, ..)| text == label)
    }

    fn click(&mut self, label: &str) {
        self.click_nth(label, 0);
    }

    fn click_nth(&mut self, label: &str, nth: usize) {
        let (_, pos) = self.visible(label, nth);
        for pressed in [true, false] {
            self.frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }]);
        }
        self.settle();
    }

    fn key(&mut self, key: egui::Key) {
        for pressed in [true, false] {
            self.frame(vec![egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }]);
        }
        self.settle();
    }

    fn count(&mut self, label: &str) -> usize {
        self.settle().text.iter().filter(|(text, ..)| text == label).count()
    }

    fn reasons(&self) -> Vec<Reason> {
        backup::list(&self.path()).iter().map(|backup| backup.reason).collect()
    }
}

/// A copy of `from` named as a backup made at `stamp`, changed by `change`.
fn planted(from: &Path, stamp: &str, slug: &str, change: impl FnOnce(&Path)) -> PathBuf {
    let path = from.with_file_name(format!("library-{stamp}-{slug}.sqlite"));
    std::fs::copy(from, &path).unwrap();
    change(&path);
    path
}

/// A daily backup, a newer one before Tidy Today, one from a newer Crossbow
/// and a damaged one, the last two older than the others.
fn four_backups(h: &mut Harness) -> Vec<Backup> {
    let daily = backup::create(&h.lib.store, Reason::Daily).unwrap().unwrap();
    let space = h.lib.spaces_ordered()[0].0.clone();
    h.lib.add_pinned(&space, None, "https://example.test/b", "B").unwrap();
    backup::create(&h.lib.store, Reason::TidyToday).unwrap().unwrap();
    planted(&daily.path, "20261001T090000Z", "daily", |path| {
        rusqlite::Connection::open(path).unwrap().execute_batch(&format!("PRAGMA user_version = {}", crossbow_core::store::LIBRARY_FORMAT + 1)).unwrap();
    });
    planted(&daily.path, "20260930T090000Z", "before-tidy-today", |path| std::fs::write(path, vec![0x5Au8; 8192]).unwrap());
    backup::list(&h.path())
}

fn title(backup: &Backup) -> String {
    format!("{} · {} · {}", crate::backups::date(backup.created_ms), backup.reason.label(), crate::ui::overlays::human_size(backup.bytes as i64))
}

fn confirmation(backup: &Backup) -> String {
    format!("Replace this Mac's library with the backup from {}? Your current library is backed up first.", crate::backups::date(backup.created_ms))
}

#[test]
fn restore_data_lists_backups_newest_first_with_what_each_holds_and_refuses_newer_and_damaged_ones() {
    for size in SIZES {
        let mut h = Harness::new(size);
        let backups = four_backups(&mut h);
        assert!(!h.shows(KEPT_HERE), "the panel opens on request");
        h.click("Restore Data…");
        for label in [KEPT_HERE, SCOPE] { h.visible(label, 0); }
        for backup in &backups { h.visible(&title(backup), 0); }
        // Newest first: of the titles in view, each is below the one before it.
        let shown = h.visible(&title(&backups[1]), 0).0;
        let order: Vec<f32> = backups.iter().filter_map(|backup| shown.text.iter().find(|(text, ..)| *text == title(backup)).map(|(_, bounds, _)| bounds.top())).collect();
        assert!(order.len() >= 2, "two titles fit in view at {size:?}");
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?} at {size:?}");
        assert_eq!(backups.iter().map(|backup| backup.reason).collect::<Vec<_>>(), [Reason::TidyToday, Reason::Daily, Reason::Daily, Reason::TidyToday]);
        // What each holds, read from a read-only open.
        h.visible("1 Space · 1 profile · 2 pins · 0 folders · 0 tabs · 0 archived tabs", 0);
        h.visible("1 Space · 1 profile · 1 pin · 0 folders · 0 tabs · 0 archived tabs", 0);
        h.visible(NEWER, 0);
        h.visible(DAMAGED, 0);
        assert_eq!(h.count("Restore…"), 2, "only the two readable backups offer Restore at {size:?}");
        assert!(!h.shows(NONE_YET));
        h.click("Close");
        assert!(!h.st.open && !h.shows(KEPT_HERE));
    }
}

#[test]
fn cancel_and_escape_change_nothing_and_restore_and_quit_backs_up_first() {
    for size in SIZES {
        let mut h = Harness::new(size);
        let backups = four_backups(&mut h);
        h.click("Restore Data…");
        h.click_nth("Restore…", 1);
        h.visible(&confirmation(&backups[1]), 0);
        h.visible(AFTER, 0);
        assert!(h.st.owns_keyboard(), "Escape cancels the confirmation, not Settings");
        h.click("Cancel");
        assert!(!h.shows(&confirmation(&backups[1])) && !h.st.owns_keyboard());
        h.click_nth("Restore…", 1);
        h.key(egui::Key::Escape);
        assert!(!h.shows(&confirmation(&backups[1])) && h.st.confirm.is_none(), "Escape cancels at {size:?}");
        assert!(h.st.open && h.st.confirm.is_none(), "the panel stays open");
        h.visible(SCOPE, 0);
        assert_eq!(backup::pending(&h.path()), None);
        assert_eq!(backup::list(&h.path()), backups, "cancelling made no backup");
        assert!(h.requests.is_empty());

        h.click_nth("Restore…", 1);
        h.click("Restore and Quit");
        assert_eq!(h.requests, [Request::Quit]);
        assert_eq!(backup::pending(&h.path()), Some(backups[1].created_ms));
        assert_eq!(h.reasons()[0], Reason::Restore, "the current library was backed up first");
        assert_eq!(h.lib.items.len(), 2, "nothing changes until Crossbow opens again");
        h.visible(WAITING, 0);
        h.click("Cancel Restore");
        assert_eq!(backup::pending(&h.path()), None);
        assert!(!h.shows(WAITING));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_backup_that_fails_before_a_restore_stops_it_and_says_so_and_failures_are_listed() {
    use std::os::unix::ffi::OsStrExt;
    let lock = |folder: &Path, locked: bool| {
        let path = std::ffi::CString::new(folder.as_os_str().as_bytes()).unwrap();
        // SAFETY: `path` is a valid C string for the duration of the call.
        assert_eq!(unsafe { libc::chflags(path.as_ptr(), if locked { libc::UF_IMMUTABLE } else { 0 }) }, 0);
    };
    for size in SIZES {
        let mut h = Harness::new(size);
        let backups = four_backups(&mut h);
        let folder = backup::folder(&h.path());
        h.click("Restore Data…");
        h.click_nth("Restore…", 0);
        lock(&folder, true);
        h.click("Restore and Quit");
        lock(&folder, false);
        let error = h.st.error.clone().unwrap_or_else(|| panic!("no error at {size:?}"));
        assert!(error.starts_with("Couldn't back up this Mac's library first, so nothing was restored."), "{error}");
        h.visible(&error, 0);
        assert!(error.contains("Couldn't create the backup file"), "{error}");
        assert!(h.requests.is_empty() && backup::pending(&h.path()).is_none());
        assert_eq!(backup::list(&h.path()), backups);
        // Retry works once the folder can be written again.
        h.click("Restore and Quit");
        assert_eq!(h.requests, [Request::Quit]);
        h.click("Cancel Restore");

        // A failed backup before an operation is listed with its reason.
        lock(&folder, true);
        assert!(backup::before(&h.lib.store, Reason::TidyToday).is_some());
        lock(&folder, false);
        h.click("Close");
        h.click("Restore Data…");
        let failure = &backup::failures(&h.lib.store)[0];
        h.visible(&format!("{} · Before Tidy Today · Failed: {}", crate::backups::date(failure.at_ms), failure.error), 0);
        h.click("Reveal in Finder");
        assert_eq!(h.requests.last(), Some(&Request::Reveal(backup::folder(&h.lib.store.path().unwrap()))), "the Backups folder at {size:?}");
    }
}

#[test]
fn after_a_restore_the_section_says_so_and_names_profiles_that_start_empty() {
    for size in SIZES {
        let mut h = Harness::new(size);
        assert!(!h.shows("Restored from the backup from"));
        h.click("Restore Data…");
        h.visible(NONE_YET, 0);
        h.click("Close");
        let work = h.lib.create_named_profile("Work").unwrap();
        let chosen = backup::create(&h.lib.store, Reason::Daily).unwrap().unwrap();
        backup::restore(&h.lib.store, &chosen).unwrap();
        let stand_in = Library::open(crossbow_core::store::Store::open_in_memory().unwrap()).unwrap();
        drop(std::mem::replace(&mut h.lib, stand_in));
        assert_eq!(backup::finish_restore(&h.path()), Ok(true));
        h.lib = Library::open(h.fixture.store()).unwrap();
        backup::mark_shown(&h.lib.store, vec![work]).unwrap();
        h.launch_error = Some("The copy of the backup was damaged, so your library wasn't changed. Choose the backup in Restore Data again.".into());
        h.visible(&format!("Restored from the backup from {}.", crate::backups::date(chosen.created_ms)), 0);
        h.visible("Website data for “Work” wasn't on this Mac, so it starts empty. Sites may ask you to sign in again.", 0);
        h.visible("The copy of the backup was damaged, so your library wasn't changed. Choose the backup in Restore Data again.", 0);
    }
}

/// The confirmation says that a restore leaves this Mac's site permissions
/// and links to other apps as they are, which the backup can't bring back.
#[test]
fn the_restore_confirmation_says_this_macs_site_permissions_stay() {
    for size in SIZES {
        let mut h = Harness::new(size);
        let backups = four_backups(&mut h);
        h.click("Restore Data…");
        h.click_nth("Restore…", 1);
        h.visible(&confirmation(&backups[1]), 0);
        h.visible("Sync turns off on this Mac until you turn it on again. Site permissions and links to other apps stay as they are on this Mac. Crossbow quits to finish; open it again to see the restored library.", 0);
    }
}

#[test]
fn delete_all_backups_asks_first_and_escape_or_cancel_keep_them() {
    for size in SIZES {
        let mut h = Harness::new(size);
        let backups = four_backups(&mut h);
        let ask = format!("Delete all {} backups on this Mac?", backups.len());
        h.click("Delete All Backups…");
        h.visible(&ask, 0);
        h.visible("They can't be restored afterwards.", 0);
        assert!(h.st.owns_keyboard(), "Escape cancels the confirmation, not Settings");
        h.key(egui::Key::Escape);
        assert!(!h.shows(&ask) && !h.st.owns_keyboard(), "Escape cancels at {size:?}");
        h.click("Delete All Backups…");
        h.click("Cancel");
        assert!(!h.shows(&ask) && !h.st.owns_keyboard());
        assert_eq!(backup::list(&h.path()), backups, "nothing was deleted");

        // A restore waiting for the next launch goes too, and the question says so.
        backup::restore(&h.lib.store, &backups[0]).unwrap();
        let backups = backup::list(&h.path());
        h.click("Delete All Backups…");
        h.visible(&format!("Delete all {} backups on this Mac?", backups.len()), 0);
        h.visible("They can't be restored afterwards. The restore waiting for the next launch is cancelled too.", 0);
        h.click("Delete All Backups");
        assert!(backup::list(&h.path()).is_empty() && backup::pending(&h.path()).is_none(), "at {size:?}");
        h.visible(&format!("Deleted {} backups.", backups.len()), 0);
        assert!(!h.shows(WAITING));
        h.click("Restore Data…");
        h.visible(NONE_YET, 0);
    }
}

/// AC5: a restore that can't open the library says so without its path,
/// and offers Reveal in Finder.
#[cfg(unix)]
#[test]
fn backup_messages_name_no_path_and_offer_reveal_in_finder() {
    use std::os::unix::fs::PermissionsExt;
    let set_mode = |path: &Path, mode: u32| std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    for size in SIZES {
        let mut h = Harness::new(size);
        let chosen = backup::create(&h.lib.store, Reason::Daily).unwrap().unwrap();
        backup::restore(&h.lib.store, &chosen).unwrap();
        set_mode(&h.path(), 0o000);
        let launch = crate::backups::Backups::launch(&h.path());
        set_mode(&h.path(), 0o600);
        let error = launch.restore_error.clone().unwrap_or_else(|| panic!("the restore finished at {size:?}"));
        assert!(error.contains("the Crossbow data folder"), "{error}");
        h.launch_error = Some(error.clone());
        let (shown, _) = h.visible(&error, 0);
        let dir = h.path().parent().unwrap().to_path_buf();
        let named: Vec<String> = [Some(dir.clone()), dir.canonicalize().ok(), std::env::var_os("HOME").map(PathBuf::from)]
            .into_iter().flatten().map(|path| path.display().to_string()).filter(|path| path.len() > 1).collect();
        for (text, ..) in &shown.text {
            for path in &named { assert!(!text.contains(path.as_str()), "{text:?} names a path at {size:?}"); }
        }
        h.click("Reveal in Finder");
        assert_eq!(h.requests, [Request::Reveal(h.lib.store.path().unwrap())], "at {size:?}");
    }
}
