//! Library backups in the running app: the restore that waits for launch,
//! the backup before an upgrade, the daily backup on its own thread, the
//! notice after a restore, and the dates the backups list shows. See
//! `crossbow_core::backup`.

use crossbow_core::backup;
use crossbow_core::library::Library;
use std::path::{Path, PathBuf};

/// How often a running app checks whether the daily backup is due.
const CHECK_EVERY_SECS: f64 = 3600.0;

pub struct Backups {
    library: PathBuf,
    daily: Option<std::thread::JoinHandle<()>>,
    checked_at: Option<f64>,
    /// Why the restore waiting for this launch didn't finish.
    pub restore_error: Option<String>,
}

impl Backups {
    /// Before the library opens: put a waiting restore in place, then back up
    /// a library this version is about to upgrade.
    pub fn launch(library: &Path) -> Backups {
        let restore_error = backup::finish_restore(library).err();
        if let Some(error) = &restore_error { log::warn!("Restore Data: {error}"); }
        // A failure is logged and listed; the upgrade goes ahead as before.
        let _ = backup::before_upgrade(library);
        Backups { library: library.to_path_buf(), daily: None, checked_at: None, restore_error }
    }

    /// At launch and hourly: the daily backup, on its own thread, when due.
    pub fn tick(&mut self, now: f64) {
        if self.checked_at.is_some_and(|at| now - at < CHECK_EVERY_SECS) { return; }
        if self.daily.as_ref().is_some_and(|daily| !daily.is_finished()) { return; }
        self.checked_at = Some(now);
        let library = self.library.clone();
        self.daily = std::thread::Builder::new().name("crossbow-backup".into())
            .spawn(move || { let _ = backup::daily(&library, crossbow_core::now_ms()); })
            .map_err(|error| log::warn!("Couldn't start the daily backup: {error}"))
            .ok();
    }

    #[cfg(test)]
    fn wait(&mut self) {
        if let Some(daily) = self.daily.take() { daily.join().unwrap(); }
    }
}

/// The first launch after a restore: note which profiles have no website
/// data on this Mac, and the toast that says what happened.
pub fn restored_notice(lib: &Library, data_dir: &Path) -> Option<String> {
    let record = backup::restored(&lib.store).filter(|record| !record.shown)?;
    let mut fresh: Vec<String> = lib.profiles.keys().filter(|id| !crossbow_engine::has_profile_data(data_dir, id)).cloned().collect();
    fresh.sort();
    let some_fresh = !fresh.is_empty();
    if let Err(error) = backup::mark_shown(&lib.store, fresh) { log::warn!("Restore Data: {error}"); }
    let date = date(record.backup_ms);
    Some(if some_fresh {
        format!("Restored the backup from {date}. Some profiles start without website data.")
    } else {
        format!("Restored the backup from {date}. Sync is off on this Mac.")
    })
}

/// What cleaning the backups couldn't do, after Clear History or a deleted
/// profile; None when every backup was cleaned.
pub fn removed_note(removed: backup::Removed) -> Option<String> {
    let deleted = match removed.deleted {
        0 => None,
        1 => Some("1 backup couldn't be cleaned, so it was deleted.".to_string()),
        n => Some(format!("{n} backups couldn't be cleaned, so they were deleted.")),
    };
    let left = match removed.left {
        0 => None,
        1 => Some("1 backup couldn't be cleaned or deleted, so it still has what was removed.".to_string()),
        n => Some(format!("{n} backups couldn't be cleaned or deleted, so they still have what was removed.")),
    };
    let parts: Vec<String> = deleted.into_iter().chain(left).collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// Seconds east of UTC on this Mac at `ms`.
pub(crate) fn local_offset(ms: u64) -> i64 {
    #[cfg(unix)]
    {
        let time = (ms / 1000) as libc::time_t;
        // SAFETY: localtime_r writes only into `tm`, which outlives the call.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if !unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
            return tm.tm_gmtoff as i64;
        }
    }
    let _ = ms;
    0
}

/// `Oct 29, 2026 at 10:15`, in this Mac's time zone.
pub fn date(ms: u64) -> String {
    date_at(ms, local_offset(ms))
}

fn date_at(ms: u64, offset_secs: i64) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let c = backup::calendar(ms, offset_secs);
    format!("{} {}, {} at {:02}:{:02}", MONTHS[(c.month - 1) as usize], c.day, c.year, c.hour, c.minute)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcuts::tests::Fixture;
    use crossbow_core::backup::Reason;
    use crossbow_core::store::Store;

    #[test]
    fn the_note_after_cleaning_backups_counts_what_couldnt_be_cleaned() {
        let note = |deleted, left| removed_note(backup::Removed { deleted, left });
        assert_eq!(note(0, 0), None);
        assert_eq!(note(1, 0).as_deref(), Some("1 backup couldn't be cleaned, so it was deleted."));
        assert_eq!(note(2, 1).as_deref(), Some("2 backups couldn't be cleaned, so they were deleted. 1 backup couldn't be cleaned or deleted, so it still has what was removed."));
        assert_eq!(note(0, 3).as_deref(), Some("3 backups couldn't be cleaned or deleted, so they still have what was removed."));
    }

    #[test]
    fn dates_read_in_the_given_zone() {
        // 2026-10-29T10:15:00Z
        let ms = 1_793_268_900_000;
        assert_eq!(date_at(ms, 0), "Oct 29, 2026 at 10:15");
        assert_eq!(date_at(ms, 3600), "Oct 29, 2026 at 11:15");
        assert_eq!(date_at(ms, -11 * 3600), "Oct 28, 2026 at 23:15");
        assert_eq!(date_at(0, 0), "Jan 1, 1970 at 00:00");
    }

    #[test]
    fn the_running_app_checks_for_the_daily_backup_at_launch_then_hourly() {
        let fixture = Fixture::new();
        let mut lib = Library::open(fixture.store()).unwrap();
        lib.ensure_defaults().unwrap();
        let mut backups = Backups::launch(&fixture.path());
        assert!(backups.restore_error.is_none());
        assert!(backup::list(&fixture.path()).is_empty(), "a library in this format needs no upgrade backup");
        backups.tick(0.0);
        backups.wait();
        let daily = backup::list(&fixture.path());
        assert_eq!(daily.iter().map(|backup| backup.reason).collect::<Vec<_>>(), [Reason::Daily]);
        // Within the hour nothing is checked; after it, the day-old rule holds.
        backups.tick(CHECK_EVERY_SECS - 1.0);
        assert!(backups.daily.is_none());
        backups.tick(CHECK_EVERY_SECS);
        backups.wait();
        assert_eq!(backup::list(&fixture.path()), daily, "one a day");
    }

    #[test]
    fn launch_finishes_a_waiting_restore_and_backs_up_an_older_library_first() {
        let fixture = Fixture::new();
        let reasons = || -> Vec<Reason> { backup::list(&fixture.path()).iter().map(|backup| backup.reason).collect() };
        let mut lib = Library::open(fixture.store()).unwrap();
        lib.ensure_defaults().unwrap();
        let space = lib.spaces_ordered()[0].0.clone();
        lib.add_pinned(&space, None, "https://example.test/kept", "In the backup").unwrap();
        let chosen = backup::create(&lib.store, Reason::Daily).unwrap().unwrap();
        lib.add_pinned(&space, None, "https://example.test/later", "After the backup").unwrap();
        backup::restore(&lib.store, &chosen).unwrap();
        drop(lib);

        let backups = Backups::launch(&fixture.path());
        assert!(backups.restore_error.is_none());
        let lib = Library::open(Store::open(&fixture.path()).unwrap()).unwrap();
        assert_eq!(lib.items.values().map(|item| item.title.as_str()).collect::<Vec<_>>(), ["In the backup"]);
        assert_eq!(reasons(), [Reason::Restore, Reason::Daily]);
        drop(lib);

        // A library from before format marks is backed up before this version opens it.
        rusqlite::Connection::open(fixture.path()).unwrap().execute_batch("PRAGMA user_version = 0").unwrap();
        let backups = Backups::launch(&fixture.path());
        assert!(backups.restore_error.is_none());
        assert_eq!(reasons(), [Reason::Upgrade, Reason::Restore, Reason::Daily]);

        // A damaged copy is dropped at launch; the library stays.
        let lib = Library::open(Store::open(&fixture.path()).unwrap()).unwrap();
        backup::restore(&lib.store, &chosen).unwrap();
        drop(lib);
        std::fs::write(fixture.path().with_file_name("fixture.sqlite.restore"), vec![0x5Au8; 8192]).unwrap();
        let backups = Backups::launch(&fixture.path());
        assert!(backups.restore_error.as_deref().unwrap().starts_with("The copy of the backup was damaged"), "{:?}", backups.restore_error);
        assert_eq!(backup::pending(&fixture.path()), None);
        let lib = Library::open(Store::open(&fixture.path()).unwrap()).unwrap();
        assert_eq!(lib.items.values().map(|item| item.title.as_str()).collect::<Vec<_>>(), ["In the backup"]);
    }

    #[test]
    fn the_notice_after_a_restore_names_profiles_without_website_data_once() {
        let fixture = Fixture::new();
        let data_dir = fixture.path().parent().unwrap().to_path_buf();
        let mut lib = Library::open(fixture.store()).unwrap();
        lib.ensure_defaults().unwrap();
        let used = lib.default_profile_id();
        let work = lib.create_named_profile("Work").unwrap();
        let chosen = backup::create(&lib.store, Reason::Daily).unwrap().unwrap();
        backup::restore(&lib.store, &chosen).unwrap();
        drop(lib);
        // Only the default profile has a website data folder on this Mac.
        let name = |id: &str| format!("Profile-{}", id.bytes().map(|byte| format!("{byte:02x}")).collect::<String>());
        std::fs::create_dir_all(data_dir.join("Chromium").join(name(&used))).unwrap();
        assert!(crossbow_engine::has_profile_data(&data_dir, &used) && !crossbow_engine::has_profile_data(&data_dir, &work));

        let _ = Backups::launch(&fixture.path());
        let lib = Library::open(Store::open(&fixture.path()).unwrap()).unwrap();
        let notice = restored_notice(&lib, &data_dir).unwrap();
        assert!(notice.starts_with(&format!("Restored the backup from {}.", date(chosen.created_ms))), "{notice}");
        assert!(notice.ends_with("Some profiles start without website data."), "{notice}");
        assert_eq!(backup::restored(&lib.store).unwrap().fresh_profiles, [work]);
        assert_eq!(restored_notice(&lib, &data_dir), None, "shown once");
        let reopened = Library::open(Store::open(&fixture.path()).unwrap()).unwrap();
        assert_eq!(restored_notice(&reopened, &data_dir), None);
    }
}
