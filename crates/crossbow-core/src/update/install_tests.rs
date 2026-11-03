use super::*;
use crate::update::fixtures::fake_app;

fn from() -> Stamp {
    Stamp { version: "0.1.0".into(), build: 1 }
}

fn to() -> Stamp {
    Stamp { version: "0.2.0".into(), build: 7 }
}

/// How long a starting app waits for the helper, in tests.
const PATIENCE: Duration = Duration::from_millis(300);

/// A file that marks a bundle no Developer ID signed: [`Sim`] refuses it.
const PLANTED: &str = "Contents/Resources/planted";

/// A bundle someone else put there, at `version`.
fn plant(app: &Path, version: &str, build: u64) {
    fake_app(app, version, build);
    fs::write(app.join(PLANTED), "not signed by the team").unwrap();
}

/// The real file system with a stand-in signature check, probe and
/// launcher; it can "crash" (stop changing anything) before its n-th change.
struct Sim {
    probe: Option<Stamp>,
    /// Whether this process has a Team ID to check bundles against.
    signed: bool,
    /// Paths whose bundle fails the check wherever it is.
    refuse: Vec<PathBuf>,
    /// Another copy of the app runs.
    in_use: bool,
    swap_error: Option<i32>,
    crash_before: Option<usize>,
    changes: usize,
    crashed: bool,
    launched: Vec<PathBuf>,
    /// Which change wrote the journal at which step.
    journal_writes: Vec<(usize, Step)>,
}

impl Sim {
    fn new() -> Sim {
        Sim { probe: Some(to()), signed: true, refuse: Vec::new(), in_use: false, swap_error: None, crash_before: None, changes: 0, crashed: false, launched: Vec::new(), journal_writes: Vec::new() }
    }

    fn crashing_before(change: usize) -> Sim {
        Sim { crash_before: Some(change), ..Sim::new() }
    }

    fn change(&mut self) -> io::Result<()> {
        if self.crashed || self.crash_before == Some(self.changes) {
            self.crashed = true;
            return Err(io::Error::other("crashed"));
        }
        self.changes += 1;
        Ok(())
    }
}

impl Ops for Sim {
    fn swap(&mut self, a: &Path, b: &Path) -> io::Result<()> {
        self.change()?;
        if let Some(code) = self.swap_error {
            return Err(io::Error::from_raw_os_error(code));
        }
        swap(a, b)
    }

    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        self.change()?;
        fs::rename(from, to)
    }

    fn remove(&mut self, path: &Path) -> io::Result<()> {
        self.change()?;
        remove(path)
    }

    fn write(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.change()?;
        if let Ok(journal) = serde_json::from_slice::<Journal>(bytes) {
            self.journal_writes.push((self.changes - 1, journal.step));
        }
        write_atomic(path, bytes)
    }

    fn verify(&mut self, app: &Path) -> bool {
        !self.crashed && self.signed && !self.refuse.iter().any(|refused| refused == app) && !app.join(PLANTED).exists()
    }

    fn in_use(&mut self, _app: &Path) -> bool {
        self.in_use
    }

    fn probe(&mut self, _app: &Path) -> Option<Stamp> {
        if self.crashed { None } else { self.probe.clone() }
    }

    fn launch(&mut self, app: &Path) -> io::Result<()> {
        self.change()?;
        self.launched.push(app.to_path_buf());
        Ok(())
    }
}

struct Setup {
    _dir: tempfile::TempDir,
    state: PathBuf,
    paths: Paths,
}

/// `Applications/Crossbow.app` at 0.1.0, and 0.2.0 staged beside it by
/// [`stage`] with its journal.
fn staged(relaunch: bool) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Applications/Crossbow.app");
    fake_app(&app, "0.1.0", 1);
    let download = dir.path().join("download/Crossbow.app");
    fake_app(&download, "0.2.0", 7);
    let state = dir.path().join("state");
    fs::create_dir(&state).unwrap();
    let journal = Journal { app: app.clone(), from: from(), to: to(), pid: 0, relaunch, step: Step::Swapped, launches: 9, reason: None };
    stage(&state, &download, journal).unwrap();
    Setup { paths: Paths::beside(&app), state, _dir: dir }
}

fn quit_at_once(_: u32) -> bool {
    true
}

impl Setup {
    fn journal(&self) -> Option<Journal> {
        read_journal(&self.state)
    }

    /// A copy of the app at `stamp`, running from the installed path.
    fn at(&self, stamp: Stamp) -> Instance {
        Instance { stamp, app: Some(self.paths.app.clone()) }
    }

    fn at_app(&self) -> Option<Stamp> {
        Stamp::at(&self.paths.app)
    }

    /// The folder beside the app: names only.
    fn beside(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.paths.app.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }
}

#[test]
fn staging_copies_beside_the_app_and_leaves_the_app_alone() {
    let setup = staged(true);
    assert_eq!(setup.at_app(), Some(from()));
    assert_eq!(Stamp::at(&setup.paths.staged), Some(to()));
    assert_eq!(setup.beside(), vec![STAGED, "Crossbow.app"]);
    let journal = setup.journal().unwrap();
    assert_eq!((journal.step, journal.launches, journal.reason.clone()), (Step::Staged, 0, None));
    let staged_link = setup.paths.staged.join("Contents/Frameworks/Engine.framework/Versions/Current");
    assert_eq!(fs::read_link(staged_link).unwrap(), Path::new("A"), "symlinks are copied as symlinks");
    // A second install can't start while this one is under way.
    let other = setup._dir.path().join("download/Crossbow.app");
    assert_eq!(stage(&setup.state, &other, journal), Err(InstallError::Busy));
}

#[test]
fn the_helper_swaps_probes_and_the_new_app_confirms_itself() {
    let setup = staged(true);
    let mut sim = Sim::new();
    assert_eq!(helper(&setup.state, &setup.paths.app, &mut sim, quit_at_once).unwrap(), Finished::Probed);
    assert_eq!(setup.at_app(), Some(to()));
    assert_eq!(Stamp::at(&setup.paths.previous), Some(from()), "the old app is kept until the new one has started");
    assert_eq!(setup.beside(), vec![PREVIOUS, "Crossbow.app"]);
    assert_eq!(sim.launched, vec![setup.paths.app.clone()], "Restart to Update opens the new app");
    let mut app = Sim::new();
    assert_eq!(on_start(&setup.state, &setup.at(to()), &mut app, PATIENCE).unwrap(), Start::Continue);
    assert_eq!(setup.journal().unwrap().launches, 1);
    assert_eq!(confirm(&setup.state, &setup.at(to()), &mut app).unwrap(), Some(Outcome::Updated { to: "0.2.0".into() }));
    assert_eq!(setup.beside(), vec!["Crossbow.app"]);
    assert_eq!(setup.journal(), None);
    assert_eq!(take_outcome(&setup.state).map(|o| o.text()).as_deref(), Some("Crossbow was updated to 0.2.0."));
    assert_eq!(take_outcome(&setup.state), None, "said once");
}

#[test]
fn installing_at_quit_does_not_open_the_app() {
    let setup = staged(false);
    let mut sim = Sim::new();
    assert_eq!(helper(&setup.state, &setup.paths.app, &mut sim, quit_at_once).unwrap(), Finished::Probed);
    assert!(sim.launched.is_empty());
}

#[test]
fn the_helper_waits_for_the_app_to_quit_and_changes_nothing_if_it_does_not() {
    let setup = staged(true);
    let mut sim = Sim::new();
    assert_eq!(helper(&setup.state, &setup.paths.app, &mut sim, |_| false).unwrap(), Finished::NotQuit);
    assert_eq!(setup.at_app(), Some(from()));
    assert_eq!(setup.journal().unwrap().step, Step::Staged);
    assert_eq!(sim.changes, 0);
}

#[test]
fn a_new_app_that_fails_to_start_is_rolled_back_at_once() {
    let setup = staged(true);
    let mut sim = Sim { probe: None, ..Sim::new() };
    let finished = helper(&setup.state, &setup.paths.app, &mut sim, quit_at_once).unwrap();
    assert_eq!(finished, Finished::Done(Outcome::RolledBack { to: "0.2.0".into(), kept: "0.1.0".into(), reason: "didn't start".into() }));
    assert_eq!(setup.at_app(), Some(from()));
    assert_eq!(setup.beside(), vec!["Crossbow.app"]);
    assert_eq!(sim.launched, vec![setup.paths.app.clone()], "the old app opens again");
    assert_eq!(take_outcome(&setup.state).unwrap().text(), "Crossbow 0.2.0 didn't start, so Crossbow went back to 0.1.0.");
    // A probe that answers with the wrong version is a failure too.
    let setup = staged(false);
    let mut sim = Sim { probe: Some(from()), ..Sim::new() };
    assert!(matches!(helper(&setup.state, &setup.paths.app, &mut sim, quit_at_once).unwrap(), Finished::Done(Outcome::RolledBack { .. })));
    assert_eq!(setup.at_app(), Some(from()));
}

#[test]
fn a_staged_app_that_fails_its_signature_check_is_never_swapped_in() {
    let setup = staged(true);
    let mut sim = Sim { signed: false, ..Sim::new() };
    let finished = helper(&setup.state, &setup.paths.app, &mut sim, quit_at_once).unwrap();
    assert_eq!(finished, Finished::Done(Outcome::Abandoned { to: "0.2.0".into(), reason: "the staged app failed its signature check".into() }));
    assert_eq!(setup.at_app(), Some(from()));
    assert_eq!(setup.beside(), vec!["Crossbow.app"]);
}

#[test]
fn a_swap_macos_refuses_leaves_the_old_app_in_place() {
    let setup = staged(true);
    let mut sim = Sim { swap_error: Some(libc::EPERM), ..Sim::new() };
    let finished = helper(&setup.state, &setup.paths.app, &mut sim, quit_at_once).unwrap();
    assert_eq!(finished, Finished::Done(Outcome::Abandoned { to: "0.2.0".into(), reason: "macOS refused to replace the app".into() }));
    assert_eq!(setup.at_app(), Some(from()));
    assert_eq!(setup.beside(), vec!["Crossbow.app"]);
}

#[test]
fn a_new_app_that_never_becomes_interactive_is_rolled_back_at_its_third_start() {
    let setup = staged(true);
    assert_eq!(helper(&setup.state, &setup.paths.app, &mut Sim::new(), quit_at_once).unwrap(), Finished::Probed);
    for start in 1..=MAX_UNCONFIRMED_LAUNCHES {
        assert_eq!(on_start(&setup.state, &setup.at(to()), &mut Sim::new(), PATIENCE).unwrap(), Start::Continue, "start {start}");
    }
    let mut sim = Sim::new();
    assert_eq!(on_start(&setup.state, &setup.at(to()), &mut sim, PATIENCE).unwrap(), Start::Quit);
    assert_eq!(setup.at_app(), Some(from()));
    assert_eq!(setup.beside(), vec!["Crossbow.app"]);
    assert_eq!(sim.launched, vec![setup.paths.app.clone()]);
    assert_eq!(take_outcome(&setup.state).unwrap().text(), "Crossbow 0.2.0 didn't finish starting, so Crossbow went back to 0.1.0.");
}

#[test]
fn an_app_started_while_the_helper_works_waits_then_quits_or_opens_the_new_version() {
    // The helper holds the lock throughout: the old app waits, then quits
    // without running beside the install or changing anything.
    let setup = staged(true);
    let held = Lock::take(&setup.state).unwrap().unwrap();
    assert!(Lock::take(&setup.state).unwrap().is_none(), "the lock is exclusive");
    let before = setup.journal();
    let mut sim = Sim::new();
    let started = Instant::now();
    assert_eq!(on_start(&setup.state, &setup.at(from()), &mut sim, PATIENCE).unwrap(), Start::Quit);
    assert!(started.elapsed() >= PATIENCE, "it waited first");
    assert_eq!((sim.changes, sim.launched.len()), (0, 0));
    assert_eq!(helper(&setup.state, &setup.paths.app, &mut Sim::new(), quit_at_once).unwrap(), Finished::Nothing, "a second helper does nothing");
    assert_eq!(setup.journal(), before);
    drop(held);

    // The helper finishes while the old app waits: the old app quits, and
    // opens the new version itself only when the helper won't.
    for relaunch in [true, false] {
        let setup = staged(relaunch);
        let (taken, waiting) = std::sync::mpsc::channel();
        let (start, sim) = std::thread::scope(|scope| {
            let helper_run = scope.spawn(|| {
                let mut sim = Sim::new();
                helper(&setup.state, &setup.paths.app, &mut sim, |_| {
                    taken.send(()).unwrap();
                    std::thread::sleep(Duration::from_millis(200));
                    true
                })
                .unwrap()
            });
            waiting.recv().unwrap();
            let mut sim = Sim::new();
            let start = on_start(&setup.state, &setup.at(from()), &mut sim, Duration::from_secs(20)).unwrap();
            assert_eq!(helper_run.join().unwrap(), Finished::Probed);
            (start, sim)
        });
        assert_eq!(start, Start::Quit, "relaunch {relaunch}");
        assert_eq!(setup.at_app(), Some(to()));
        assert_eq!(sim.launched, if relaunch { vec![] } else { vec![setup.paths.app.clone()] }, "relaunch {relaunch}");
        assert_eq!(setup.journal().map(|journal| (journal.step, journal.launches)), Some((Step::Probed, 0)), "the new version still confirms itself");
        assert_eq!(confirm(&setup.state, &setup.at(from()), &mut Sim::new()).unwrap(), None, "only the new app confirms");
    }
}

#[test]
fn the_helper_leaves_the_app_alone_while_another_copy_of_it_runs() {
    let setup = staged(true);
    let mut sim = Sim { in_use: true, ..Sim::new() };
    let finished = helper(&setup.state, &setup.paths.app, &mut sim, quit_at_once).unwrap();
    assert_eq!(finished, Finished::Done(Outcome::Abandoned { to: "0.2.0".into(), reason: "Crossbow was opened again while it was being updated".into() }));
    assert_eq!(setup.at_app(), Some(from()));
    assert_eq!(setup.beside(), vec!["Crossbow.app"]);
    assert!(sim.launched.is_empty(), "the copy that runs stays the only one");
}

#[test]
fn another_copy_of_the_app_is_found_by_its_executable_not_its_engine_helpers() {
    const TEST: &str = "update::install::tests::another_copy_of_the_app_is_found_by_its_executable_not_its_engine_helpers";
    if std::env::var_os("CROSSBOW_IN_USE_TEST_CHILD").is_some() {
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Applications/Crossbow.app");
    fake_app(&app, "0.1.0", 1);
    let helper_app = app.join("Contents/Frameworks/Crossbow Helper.app/Contents/MacOS");
    fs::create_dir_all(&helper_app).unwrap();
    // A copy of this test binary that waits: a real executable, so the
    // kernel reports its own path (macOS kills copies of its own tools).
    let run = |at: &Path| {
        fs::copy(std::env::current_exe().unwrap(), at).unwrap();
        let child = std::process::Command::new(at).args(["--exact", TEST, "--test-threads=1"]).env("CROSSBOW_IN_USE_TEST_CHILD", "1").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        std::thread::sleep(Duration::from_millis(500));
        child
    };
    assert!(!in_use(&app), "nothing runs from it yet");
    let mut engine = run(&helper_app.join("Crossbow Helper"));
    assert!(!in_use(&app), "an engine helper left over is not a copy of the app");
    let mut copy = run(&app.join("Contents/MacOS/copy"));
    assert!(copy.try_wait().unwrap().is_none(), "the copy runs");
    assert!(in_use(&app));
    assert!(!in_use(&dir.path().join("Elsewhere/Crossbow.app")));
    for child in [&mut copy, &mut engine] {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    assert!(!in_use(&app), "and once it ends");
}

/// The installed app holds a whole bundle, old or new, at every moment.
fn assert_whole(setup: &Setup, when: &str) {
    let at_app = setup.at_app();
    assert!(at_app == Some(from()) || at_app == Some(to()), "{when}: the app's path holds {at_app:?}");
}

/// After a crash before change `n`, the user opens whatever is at the app's
/// path; its start and first interactive moment settle everything.
fn crash_then_reopen(n: usize, probe: Option<Stamp>) -> (Setup, Option<Outcome>) {
    let setup = staged(true);
    let mut sim = Sim { probe, ..Sim::crashing_before(n) };
    let _ = helper(&setup.state, &setup.paths.app, &mut sim, quit_at_once);
    assert_whole(&setup, &format!("after a crash before change {n}"));
    let running = setup.at_app().unwrap();
    let mut app = Sim::new();
    let start = on_start(&setup.state, &setup.at(running.clone()), &mut app, PATIENCE).unwrap();
    assert_whole(&setup, &format!("after the start that followed a crash before change {n}"));
    if start == Start::Continue {
        confirm(&setup.state, &setup.at(running), &mut app).unwrap();
    }
    assert_eq!(setup.journal(), None, "crash before change {n}: the install is settled");
    assert_eq!(setup.beside(), vec!["Crossbow.app"], "crash before change {n}: nothing left beside the app");
    let outcome = take_outcome(&setup.state);
    (setup, outcome)
}

#[test]
fn a_crash_before_any_change_of_an_install_resumes_or_undoes_it() {
    let total = {
        let setup = staged(true);
        let mut sim = Sim::new();
        helper(&setup.state, &setup.paths.app, &mut sim, quit_at_once).unwrap();
        sim.changes
    };
    assert!(total >= 5, "journal, swap, keep previous, journal, launch: {total}");
    for n in 0..=total {
        let (setup, outcome) = crash_then_reopen(n, Some(to()));
        match setup.at_app() {
            Some(stamp) if stamp == to() => assert_eq!(outcome, Some(Outcome::Updated { to: "0.2.0".into() }), "crash before change {n}"),
            _ => assert_eq!(outcome, Some(Outcome::Abandoned { to: "0.2.0".into(), reason: "it was interrupted".into() }), "crash before change {n}"),
        }
    }
}

#[test]
fn a_crash_before_any_change_of_a_rollback_still_ends_on_the_old_app() {
    let total = {
        let setup = staged(true);
        let mut sim = Sim { probe: None, ..Sim::new() };
        helper(&setup.state, &setup.paths.app, &mut sim, quit_at_once).unwrap();
        sim.changes
    };
    for n in 0..=total {
        let (setup, outcome) = crash_then_reopen(n, None);
        // A crash before the probe resumes nothing: whatever opens settles it.
        match outcome {
            Some(Outcome::Updated { .. }) => assert_eq!(setup.at_app(), Some(to()), "crash before change {n}"),
            Some(Outcome::RolledBack { .. } | Outcome::Abandoned { .. }) => assert_eq!(setup.at_app(), Some(from()), "crash before change {n}"),
            None => panic!("crash before change {n}: no outcome"),
        }
    }
    // Every crash after the rollback was decided ends on the old app.
    let decided = {
        let setup = staged(true);
        let mut sim = Sim { probe: None, ..Sim::new() };
        helper(&setup.state, &setup.paths.app, &mut sim, quit_at_once).unwrap();
        sim.journal_writes.iter().find(|(_, step)| *step == Step::RollingBack).map(|(change, _)| *change).unwrap()
    };
    for n in decided + 1..=total {
        let (setup, _) = crash_then_reopen(n, None);
        assert_eq!(setup.at_app(), Some(from()), "crash before change {n}, after the rollback was decided");
    }
}

#[test]
fn only_a_writable_real_bundle_can_be_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Applications/Crossbow.app");
    fake_app(&app, "0.1.0", 1);
    assert_eq!(writable(&app), Ok(()));
    let translocated = dir.path().join("AppTranslocation/X/d/Crossbow.app");
    fake_app(&translocated, "0.1.0", 1);
    assert_eq!(writable(&translocated), Err(InstallError::Translocated));
    let link = dir.path().join("Crossbow.app");
    std::os::unix::fs::symlink(&app, &link).unwrap();
    assert_eq!(writable(&link), Err(InstallError::Symlink));
    assert_eq!(writable(&dir.path().join("Missing.app")), Err(InstallError::NoBundle));
    use std::os::unix::fs::PermissionsExt;
    let parent = app.parent().unwrap();
    fs::set_permissions(parent, fs::Permissions::from_mode(0o555)).unwrap();
    let refused = writable(&app);
    fs::set_permissions(parent, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(refused, Err(InstallError::NotWritable));
    fs::set_permissions(&app, fs::Permissions::from_mode(0o555)).unwrap();
    let refused = writable(&app);
    fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(refused, Err(InstallError::NotWritable));
    // And staging refuses before copying anything.
    let state = dir.path().join("state");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(parent, fs::Permissions::from_mode(0o555)).unwrap();
    let journal = Journal { app: app.clone(), from: from(), to: to(), pid: 0, relaunch: true, step: Step::Staged, launches: 0, reason: None };
    let refused = stage(&state, &app, journal);
    fs::set_permissions(parent, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(refused, Err(InstallError::NotWritable));
    assert_eq!(read_journal(&state), None);
}

#[test]
fn the_running_bundle_is_found_from_its_executable() {
    assert_eq!(running_bundle(Path::new("/Applications/Crossbow.app/Contents/MacOS/crossbow")), Some(PathBuf::from("/Applications/Crossbow.app")));
    assert_eq!(running_bundle(Path::new("/repo/target/debug/crossbow")), None);
    assert_eq!(running_bundle(Path::new("/Applications/Crossbow.app/Contents/Frameworks/crossbow")), None);
}

#[test]
fn the_probe_runs_the_new_executable_once_and_reads_its_answer() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Crossbow.app");
    fake_app(&app, "0.2.0", 7);
    let marker = dir.path().join("probe");
    assert_eq!(probe(&app, &marker, Duration::from_secs(10)), Some(to()));
    assert!(!marker.exists(), "the marker is cleared");
    let executable = app.join("Contents/MacOS/crossbow");
    fs::write(&executable, "#!/bin/sh\nexit 3\n").unwrap();
    assert_eq!(probe(&app, &marker, Duration::from_secs(10)), None, "an executable that fails");
    fs::write(&executable, "#!/bin/sh\nsleep 5\n").unwrap();
    let started = Instant::now();
    assert_eq!(probe(&app, &marker, Duration::from_millis(300)), None, "one that hangs is stopped");
    assert!(started.elapsed() < Duration::from_secs(4));
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(probe(&app, &marker, Duration::from_secs(10)), None, "one that can't run");
}

#[test]
fn the_app_answers_the_probe_with_its_version_and_build() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("probe");
    let args = |extra: &[&str]| -> Vec<OsString> { std::iter::once("crossbow").chain(extra.iter().copied()).map(OsString::from).collect() };
    assert_eq!(answer_probe(&args(&[])), None);
    assert_eq!(answer_probe(&args(&["--other", "x"])), None);
    assert_eq!(answer_probe(&args(&[PROBE_ARG, marker.to_str().unwrap()])), Some(0));
    assert_eq!(fs::read_to_string(&marker).unwrap(), format!("{} {}\n", crate::update::VERSION, crate::update::BUILD_NUMBER));
    assert_eq!(answer_probe(&args(&[PROBE_ARG, marker.to_str().unwrap()])), Some(1), "never overwrites a file");
}

/// The installed 0.1.0 app, its state folder, and a journal written as
/// `journal` says: what another process could leave there.
fn installed_with(journal: impl FnOnce(&Path) -> Journal) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Applications/Crossbow.app");
    fake_app(&app, "0.1.0", 1);
    let state = dir.path().join("state");
    fs::create_dir(&state).unwrap();
    write_atomic(&state.join(JOURNAL), &serde_json::to_vec(&journal(&app)).unwrap()).unwrap();
    Setup { paths: Paths::beside(&app), state, _dir: dir }
}

fn stamp(version: &str, build: u64) -> Stamp {
    Stamp { version: version.into(), build }
}

fn notes(app: &Path) -> String {
    fs::read_to_string(app.join("Contents/Resources/notes.txt")).unwrap()
}

#[test]
fn a_planted_bundle_never_reaches_the_apps_path_whatever_the_journal_says() {
    // A rollback "back" to a planted 9.9.9.
    let setup = installed_with(|app| Journal { app: app.into(), from: stamp("9.9.9", 999), to: from(), pid: 1, relaunch: true, step: Step::RollingBack, launches: 0, reason: None });
    plant(&setup.paths.previous, "9.9.9", 999);
    let mut sim = Sim::new();
    assert_eq!(on_start(&setup.state, &setup.at(from()), &mut sim, PATIENCE).unwrap(), Start::Continue);
    assert_eq!(notes(&setup.paths.app), "Crossbow 0.1.0\n", "the genuine app stays");
    assert_eq!(setup.beside(), vec!["Crossbow.app"], "the planted bundle is deleted");
    assert_eq!((setup.journal(), sim.launched.len()), (None, 0));
    assert_eq!(take_outcome(&setup.state).unwrap().text(), "The update to 0.1.0 was not installed: it didn't start and the previous version failed its signature check.");
    assert!(rolled_back(&setup.state).is_empty(), "nothing is remembered as rolled back");

    // A new version "failing to start" a third time, with a planted previous one.
    let setup = installed_with(|app| Journal { app: app.into(), from: stamp("9.9.9", 999), to: from(), pid: 1, relaunch: false, step: Step::Probed, launches: MAX_UNCONFIRMED_LAUNCHES, reason: None });
    plant(&setup.paths.previous, "9.9.9", 999);
    let mut sim = Sim::new();
    assert_eq!(on_start(&setup.state, &setup.at(from()), &mut sim, PATIENCE).unwrap(), Start::Continue);
    assert_eq!((notes(&setup.paths.app), setup.beside(), sim.launched.len()), ("Crossbow 0.1.0\n".into(), vec!["Crossbow.app".to_string()], 0));

    // A planted staged bundle the helper is told to swap in.
    let setup = installed_with(|app| Journal { app: app.into(), from: from(), to: stamp("9.9.9", 999), pid: 1, relaunch: false, step: Step::Swapping, launches: 0, reason: None });
    plant(&setup.paths.staged, "9.9.9", 999);
    let finished = helper(&setup.state, &setup.paths.app, &mut Sim::new(), quit_at_once).unwrap();
    assert_eq!(finished, Finished::Done(Outcome::Abandoned { to: "9.9.9".into(), reason: "the staged app failed its signature check".into() }));
    assert_eq!((notes(&setup.paths.app), setup.beside()), ("Crossbow 0.1.0\n".into(), vec!["Crossbow.app".to_string()]));

    // A journal for another app: the helper and the app leave it alone.
    let setup = installed_with(|_| Journal { app: PathBuf::from("/Elsewhere/Crossbow.app"), from: from(), to: to(), pid: 1, relaunch: false, step: Step::RollingBack, launches: 0, reason: None });
    let before = setup.journal();
    assert_eq!(helper(&setup.state, &setup.paths.app, &mut Sim::new(), quit_at_once).unwrap(), Finished::Nothing);
    assert_eq!(on_start(&setup.state, &setup.at(from()), &mut Sim::new(), PATIENCE).unwrap(), Start::Continue);
    assert_eq!(setup.journal(), before);
}

#[test]
fn an_ad_hoc_signed_copy_moves_no_bundle_but_still_settles() {
    let setup = staged(true);
    assert_eq!(helper(&setup.state, &setup.paths.app, &mut Sim::new(), quit_at_once).unwrap(), Finished::Probed);
    let mut journal = setup.journal().unwrap();
    journal.launches = MAX_UNCONFIRMED_LAUNCHES;
    write_atomic(&setup.state.join(JOURNAL), &serde_json::to_vec(&journal).unwrap()).unwrap();
    // Its third start: going back needs a check an ad-hoc process can't make.
    let mut ad_hoc = Sim { signed: false, ..Sim::new() };
    assert_eq!(on_start(&setup.state, &setup.at(to()), &mut ad_hoc, PATIENCE).unwrap(), Start::Continue);
    assert_eq!(setup.at_app(), Some(to()), "the new version stays");
    assert_eq!(setup.beside(), vec!["Crossbow.app"]);
    assert_eq!(take_outcome(&setup.state).unwrap().text(), "The update to 0.2.0 was not installed: it didn't finish starting and the previous version failed its signature check.");
}

#[test]
fn a_new_app_that_fails_its_check_where_it_now_is_is_rolled_back() {
    let setup = staged(false);
    let mut sim = Sim { refuse: vec![setup.paths.app.clone()], ..Sim::new() };
    let finished = helper(&setup.state, &setup.paths.app, &mut sim, quit_at_once).unwrap();
    assert_eq!(finished, Finished::Done(Outcome::RolledBack { to: "0.2.0".into(), kept: "0.1.0".into(), reason: "failed its signature check".into() }));
    assert_eq!(setup.at_app(), Some(from()));
    assert_eq!(setup.beside(), vec!["Crossbow.app"]);
}

#[test]
fn a_journal_another_version_replaced_is_settled_at_start() {
    let setup = installed_with(|app| Journal { app: app.into(), from: from(), to: to(), pid: 1, relaunch: false, step: Step::Probed, launches: 0, reason: None });
    // The user installed 0.3.0 by hand over 0.2.0, which never opened.
    remove(&setup.paths.app).unwrap();
    fake_app(&setup.paths.app, "0.3.0", 9);
    fake_app(&setup.paths.previous, "0.1.0", 1);
    let mut sim = Sim::new();
    assert_eq!(on_start(&setup.state, &setup.at(stamp("0.3.0", 9)), &mut sim, PATIENCE).unwrap(), Start::Continue);
    assert_eq!(setup.journal(), None);
    assert_eq!(setup.beside(), vec!["Crossbow.app"], "the hidden old bundle is gone");
    assert_eq!(Stamp::at(&setup.paths.app), Some(stamp("0.3.0", 9)), "what runs stays");
    assert_eq!(take_outcome(&setup.state).unwrap().text(), "The update to 0.2.0 was not installed: Crossbow 0.3.0 replaced it.");
    // So the next update stages.
    let download = setup._dir.path().join("download/Crossbow.app");
    fake_app(&download, "0.4.0", 11);
    let next = Journal { app: setup.paths.app.clone(), from: stamp("0.3.0", 9), to: stamp("0.4.0", 11), pid: 1, relaunch: false, step: Step::Staged, launches: 0, reason: None };
    assert_eq!(stage(&setup.state, &download, next), Ok(()));
}

#[test]
fn only_the_copy_at_the_journals_path_acts_on_it() {
    let setup = staged(false);
    assert_eq!(helper(&setup.state, &setup.paths.app, &mut Sim::new(), quit_at_once).unwrap(), Finished::Probed);
    let before = setup.journal();
    // Another copy at the new version: the download Show Download revealed, say.
    let elsewhere = setup._dir.path().join("Revealed/Crossbow.app");
    fake_app(&elsewhere, "0.2.0", 7);
    let other = Instance { stamp: to(), app: Some(elsewhere.clone()) };
    let mut sim = Sim::new();
    assert_eq!(on_start(&setup.state, &other, &mut sim, PATIENCE).unwrap(), Start::Continue);
    assert_eq!(confirm(&setup.state, &other, &mut sim).unwrap(), None);
    assert_eq!(confirm(&setup.state, &Instance { stamp: to(), app: None }, &mut sim).unwrap(), None, "nor one not run from a bundle");
    assert_eq!(helper(&setup.state, &elsewhere, &mut sim, quit_at_once).unwrap(), Finished::Nothing, "nor its helper");
    assert_eq!((setup.journal(), sim.changes), (before, 0));
    assert_eq!(Stamp::at(&setup.paths.previous), Some(from()), "the installed copy can still roll back");
    // The installed copy itself confirms.
    assert_eq!(on_start(&setup.state, &setup.at(to()), &mut Sim::new(), PATIENCE).unwrap(), Start::Continue);
    assert_eq!(confirm(&setup.state, &setup.at(to()), &mut Sim::new()).unwrap(), Some(Outcome::Updated { to: "0.2.0".into() }));
}

#[test]
fn a_rolled_back_release_is_remembered_once() {
    for _ in 0..2 {
        let setup = staged(false);
        let mut sim = Sim { probe: None, ..Sim::new() };
        assert!(matches!(helper(&setup.state, &setup.paths.app, &mut sim, quit_at_once).unwrap(), Finished::Done(Outcome::RolledBack { .. })));
        assert_eq!(rolled_back(&setup.state), vec![to()]);
        // The same one again changes nothing; at most 32 are kept, newest last.
        let mut ops = Sim::new();
        remember_rolled_back(&setup.state, &mut ops, &to()).unwrap();
        assert_eq!((rolled_back(&setup.state), ops.changes), (vec![to()], 0));
        for build in 0..40 {
            remember_rolled_back(&setup.state, &mut ops, &stamp("0.3.0", build)).unwrap();
        }
        let list = rolled_back(&setup.state);
        assert_eq!((list.len(), list.last()), (32, Some(&stamp("0.3.0", 39))));
    }
}

#[test]
fn a_folder_other_users_can_change_is_refused() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Folder/Crossbow.app");
    fake_app(&app, "0.1.0", 1);
    let folder = app.parent().unwrap();
    let group = fs::metadata(folder).unwrap().gid();
    for (mode, refused) in [(0o777, true), (0o1777, true), (0o757, true), (0o775, group != ADMIN_GROUP), (0o755, false), (0o700, false)] {
        fs::set_permissions(folder, fs::Permissions::from_mode(mode)).unwrap();
        let result = writable(&app);
        fs::set_permissions(folder, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(result == Err(InstallError::Shared), refused, "mode {mode:o}");
    }
    // Staging refuses before copying anything.
    fs::set_permissions(folder, fs::Permissions::from_mode(0o777)).unwrap();
    let state = dir.path().join("state");
    let journal = Journal { app: app.clone(), from: from(), to: to(), pid: 0, relaunch: false, step: Step::Staged, launches: 0, reason: None };
    let staged = stage(&state, &app, journal);
    fs::set_permissions(folder, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(staged, Err(InstallError::Shared));
    assert_eq!(read_journal(&state), None);
    // The rule: /Applications (root:admin 0775) and one's own folders pass.
    let me = 501;
    assert!(!shared(0, ADMIN_GROUP, 0o40775, me));
    assert!(!shared(me, 20, 0o40755, me));
    assert!(!shared(0, 0, 0o40755, me));
    assert!(shared(me, 20, 0o40775, me), "a group of other users");
    assert!(shared(502, 20, 0o40755, me), "another user's folder");
    assert!(shared(0, 0, 0o41777, me), "a shared folder like /Users/Shared");
}

#[test]
fn an_app_needing_a_later_macos_does_not_run_here() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Crossbow.app");
    fake_app(&app, "0.2.0", 7); // needs 11.0
    assert!(runs_here(&app, Some(crate::update::MacVersion(15, 0, 0))));
    assert!(runs_here(&app, Some(crate::update::MacVersion(11, 0, 0))));
    assert!(!runs_here(&app, Some(crate::update::MacVersion(10, 15, 7))));
    assert!(!runs_here(&app, None), "an unknown macOS is not assumed new enough");
    assert!(!runs_here(&dir.path().join("Missing.app"), Some(crate::update::MacVersion(15, 0, 0))));
}

#[test]
fn a_revealed_download_is_quarantined_for_gatekeeper() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Crossbow.app");
    fake_app(&app, "0.2.0", 7);
    quarantine(&app, 0x6900_0000).unwrap();
    let name = std::ffi::CString::new(app.as_os_str().as_encoded_bytes()).unwrap();
    let mut buffer = [0u8; 64];
    // SAFETY: NUL-terminated strings and a buffer of the given length.
    let len = unsafe { libc::getxattr(name.as_ptr(), c"com.apple.quarantine".as_ptr(), buffer.as_mut_ptr().cast(), buffer.len(), 0, libc::XATTR_NOFOLLOW) };
    assert!(len > 0, "{}", io::Error::last_os_error());
    assert_eq!(&buffer[..len as usize], b"0001;69000000;Crossbow;");
    assert!(quarantine(&dir.path().join("Missing.app"), 0).is_err());
}

#[test]
fn the_real_ops_check_bundles_against_this_processs_own_team_only() {
    use crate::update::fixtures::FakeCode;
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Crossbow.app");
    fake_app(&app, "0.2.0", 7);
    let verify = |code: FakeCode| System::new(dir.path(), Box::new(code)).verify(&app);
    assert!(verify(FakeCode::signed("ABCDE12345")), "the same team, on a macOS it runs on");
    assert!(!verify(FakeCode { team: Some("ABCDE12345".into()), valid: true, signed_by: "ZYXWV98765".into() }), "another team");
    assert!(!verify(FakeCode { team: None, valid: true, signed_by: "ZYXWV98765".into() }), "an ad-hoc process moves no bundle, whoever signed it");
    let mut needs_more = fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
    needs_more = needs_more.replace("<string>11.0</string>", "<string>99.0</string>");
    fs::write(app.join("Contents/Info.plist"), needs_more).unwrap();
    assert!(!verify(FakeCode::signed("ABCDE12345")), "nor one this Mac can't run");
    // The Security framework, for this ad-hoc signed test process.
    assert!(!System::new(dir.path(), Box::new(crate::update::codesign::System)).verify(&app));
}
