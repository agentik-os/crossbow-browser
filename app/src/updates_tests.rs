use super::*;
use crate::shortcuts::tests::Fixture;
use crossbow_core::update::archive;
use crossbow_core::update::feed::{self, FeedError, SigningKey};
use crossbow_core::update::{DAY, GRACE, MAX_JITTER, Version};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

pub(crate) const TEAM: &str = "ABCDE12345";
const NOW: u64 = 1_793_532_222;

/// The Security framework's answers: the running app is signed by `team`
/// (None: ad hoc) and new apps by [`TEAM`].
pub(crate) struct Code(pub Option<&'static str>);

impl CodeCheck for Code {
    fn running_team(&self) -> Option<String> {
        self.0.map(Into::into)
    }

    fn signer(&self, _app: &Path) -> Option<String> {
        Some(TEAM.into())
    }

    fn valid(&self, _app: &Path) -> bool {
        true
    }

    fn meets(&self, _app: &Path, team: &str) -> bool {
        team == TEAM
    }
}

pub(crate) fn fake_app(app: &Path, version: &str, build: u64) {
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
         <key>CFBundleExecutable</key><string>crossbow</string>\
         <key>CFBundleIdentifier</key><string>app.crossbow.Crossbow</string>\
         <key>CFBundleShortVersionString</key><string>{version}</string>\
         <key>CFBundleVersion</key><string>{build}</string>\
         <key>LSMinimumSystemVersion</key><string>11.0</string>\
         </dict></plist>\n"
    );
    std::fs::write(app.join("Contents/Info.plist"), plist).unwrap();
    std::fs::write(app.join("Contents/MacOS/crossbow"), "#!/bin/sh\n").unwrap();
}

/// An HTTP server on 127.0.0.1 with fixed answers, counting requests.
struct Server {
    addr: std::net::SocketAddr,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl Server {
    fn serve(listener: TcpListener, answers: Vec<(String, Vec<u8>)>) -> Server {
        let addr = listener.local_addr().unwrap();
        let (requests, stop) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
        let (counted, stopping) = (requests.clone(), stop.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else { continue };
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                    head.push(byte[0]);
                }
                counted.fetch_add(1, Ordering::SeqCst);
                let path = String::from_utf8_lossy(&head).split_whitespace().nth(1).unwrap_or("/").to_owned();
                let (status, body) = answers.iter().find(|(p, _)| *p == path).map_or((404, &b"missing"[..]), |(_, body)| (200, &body[..]));
                let _ = stream.write_all(format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
                let _ = stream.write_all(body);
            }
        });
        Server { addr, requests, stop }
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
    }
}

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A data folder, an installed 0.1.0 app, and a loopback feed offering
/// `version` signed with a key made here.
struct World {
    tmp: tempfile::TempDir,
    fixture: Fixture,
    server: Server,
    build: Option<Build>,
    helpers: Arc<Mutex<Vec<PathBuf>>>,
}

impl World {
    fn new(version: &str, build: u64) -> World {
        let tmp = tempfile::tempdir().unwrap();
        fake_app(&tmp.path().join("Applications/Crossbow.app"), "0.1.0", 1);
        let packed = tmp.path().join("pack/Crossbow.app");
        fake_app(&packed, version, build);
        let archive_path = tmp.path().join("pack/update.tar.gz");
        archive::pack(&packed, &archive_path).unwrap();
        let bundle = archive::read_info(&packed).unwrap();
        let (key, _) = SigningKey::generate().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let release = feed::describe(&archive_path, &format!("{base}/a.tar.gz"), &bundle, "Faster tabs.\nFewer crashes.", NOW, 14 * DAY, &key).unwrap();
        let text = feed::sign(&release, &key).unwrap();
        let server = Server::serve(listener, vec![("/stable.json".into(), text.into_bytes()), ("/a.tar.gz".into(), std::fs::read(&archive_path).unwrap())]);
        let build = crossbow_core::update::build(Some(leak(format!("{base}/stable.json"))), Some(leak(hex(&key.public()))));
        World { tmp, fixture: Fixture::new(), server, build, helpers: Arc::default() }
    }

    fn installed(&self) -> PathBuf {
        self.tmp.path().join("Applications/Crossbow.app")
    }

    fn state_dir(&self) -> PathBuf {
        self.tmp.path().join("data/updates")
    }

    /// The app's updater, as it loads at launch, waking `wakes` after each reply.
    fn updates(&self, code: Code) -> (Updates, mpsc::Receiver<()>) {
        let (woke, wakes) = mpsc::channel();
        let mut updates = Updates::load(&self.fixture.store(), &self.tmp.path().join("data"), self.build, Arc::new(code), move || {
            let _ = woke.send(());
        });
        updates.app = Some(self.installed());
        updates.mac = Some(MacVersion(15, 0, 0));
        updates.clock = || NOW + 60;
        let helpers = self.helpers.clone();
        updates.spawn_helper = Box::new(move |state| {
            helpers.lock().unwrap().push(state.to_path_buf());
            Ok(())
        });
        (updates, wakes)
    }
}

/// Read replies until `done` holds.
fn wait(updates: &mut Updates, store: &Store, wakes: &mpsc::Receiver<()>, done: impl Fn(&State) -> bool) {
    while !done(&updates.state) {
        wakes.recv_timeout(Duration::from_secs(20)).expect("a reply");
        updates.poll(store, NOW);
    }
}

#[test]
fn check_now_finds_the_update_downloads_it_and_restart_to_update_hands_it_to_the_helper() {
    let world = World::new("0.2.0", 7);
    let store = world.fixture.store();
    let (mut updates, wakes) = world.updates(Code(Some(TEAM)));
    assert_eq!(updates.status(), "Crossbow hasn't checked for updates yet.");
    updates.check_now();
    assert_eq!(updates.status(), "Checking for updates…");
    wait(&mut updates, &store, &wakes, |state| !matches!(state, State::Checking));
    let size = std::fs::metadata(world.tmp.path().join("pack/update.tar.gz")).unwrap().len();
    assert_eq!(updates.status(), format!("Crossbow 0.2.0 is available ({}).", size_text(size)));
    assert!(updates.last_line().is_some_and(|line| line.starts_with("Last checked ")));
    updates.download();
    assert!(updates.status().starts_with("Downloading Crossbow 0.2.0…"), "{}", updates.status());
    wait(&mut updates, &store, &wakes, |state| !matches!(state, State::Downloading { .. }));
    assert_eq!(updates.status(), "Crossbow 0.2.0 is downloaded and verified. It installs when you quit, or now with Restart to Update.");

    assert_eq!(updates.restart(2, false), Restart::Ask(2), "browser downloads in progress: ask first");
    assert_eq!(updates.asking, Some(2));
    assert!(world.helpers.lock().unwrap().is_empty() && install::read_journal(&world.state_dir()).is_none(), "nothing staged while asking");
    assert_eq!(updates.restart(2, true), Restart::Quit);
    assert_eq!(*world.helpers.lock().unwrap(), vec![world.state_dir()], "the helper starts with the update folder");
    let journal = install::read_journal(&world.state_dir()).unwrap();
    assert_eq!((journal.step, journal.relaunch, journal.pid), (Step::Staged, true, std::process::id()));
    assert_eq!(journal.to, Stamp { version: "0.2.0".into(), build: 7 });
    assert_eq!(Stamp::at(&install::Paths::beside(&world.installed()).staged), Some(journal.to.clone()));
    assert_eq!(Stamp::at(&world.installed()), Some(Stamp { version: "0.1.0".into(), build: 1 }), "the installed app is untouched until the helper runs");
    assert_eq!(world.server.requests(), 2, "one feed, one archive");
    // Crossbow now quits, and quitting doesn't stage it again.
    assert!(!updates.install_at_quit(0));
    assert_eq!(world.helpers.lock().unwrap().len(), 1, "one helper");
    assert!(install::read_journal(&world.state_dir()).unwrap().relaunch, "the helper still opens the new version");
    assert_eq!(updates.status(), "Crossbow 0.2.0 installs as Crossbow restarts.");
}

#[test]
fn the_switch_and_last_check_survive_reopening_and_automatic_checks_keep_their_schedule() {
    let world = World::new("0.2.0", 7);
    let store = world.fixture.store();
    let (mut updates, wakes) = world.updates(Code(Some(TEAM)));
    assert!(updates.auto.enabled(), "on by default");
    updates.auto.choose(&store, false);
    assert!(!world.updates(Code(Some(TEAM))).0.auto.enabled(), "off after reopening");

    // Off: never checks on its own.
    let start = Instant::now();
    updates.drawn(start);
    for minutes in [0, 2, 60 * 24 * 3] {
        assert_eq!(updates.tick(&store, NOW + minutes * 60, start + Duration::from_secs(minutes * 60), 0), None);
    }
    assert!(matches!(updates.state, State::Idle) && world.server.requests() == 0, "the switch off: no automatic check");

    updates.auto.choose(&store, true);
    let (mut updates, wakes2) = world.updates(Code(Some(TEAM)));
    drop(wakes);
    assert!(updates.auto.enabled(), "on after reopening");
    updates.drawn(start);
    updates.tick(&store, NOW, start, 0);
    assert!(matches!(updates.state, State::Idle), "not before the window has been interactive a minute");
    updates.tick(&store, NOW + 61, start + GRACE + Duration::from_secs(1), 0);
    assert!(matches!(updates.state, State::Checking), "then it checks");
    updates.tick(&store, NOW + 62, start + GRACE + Duration::from_secs(2), 0);
    wait(&mut updates, &store, &wakes2, |state| !matches!(state, State::Checking | State::Downloading { .. }));
    assert!(matches!(updates.state, State::Ready { refusal: None, .. }), "an automatic check downloads what it finds: {:?}", updates.status());
    assert_eq!(world.server.requests(), 2, "one check while one ran, then the download");
    let last = updates.last.clone().unwrap();
    assert_eq!(world.updates(Code(Some(TEAM))).0.last, Some(last.clone()), "the last check is kept on this device");

    let (mut later, _) = world.updates(Code(Some(TEAM)));
    later.drawn(start);
    later.tick(&store, last.at + 3600, start, 0);
    later.tick(&store, last.at + DAY - 1, start + GRACE * 2, 0);
    assert!(matches!(later.state, State::Idle), "not again within a day");
    later.tick(&store, last.at + DAY + MAX_JITTER, start + GRACE * 3, 0);
    assert!(matches!(later.state, State::Checking), "a day plus the jitter later, it checks again");
}

#[test]
fn an_update_found_automatically_installs_at_quit_unless_browser_downloads_are_in_progress() {
    let world = World::new("0.2.0", 7);
    let store = world.fixture.store();
    let (mut updates, wakes) = world.updates(Code(Some(TEAM)));
    let start = Instant::now();
    updates.drawn(start);
    updates.tick(&store, NOW, start, 0);
    updates.tick(&store, NOW + 61, start + GRACE, 0);
    wait(&mut updates, &store, &wakes, |state| matches!(state, State::Ready { .. }));
    assert!(!updates.install_at_quit(1), "a browser download is in progress: wait for the next quit");
    assert!(install::read_journal(&world.state_dir()).is_none());
    assert!(updates.install_at_quit(0));
    let journal = install::read_journal(&world.state_dir()).unwrap();
    assert!(!journal.relaunch, "installing at quit doesn't reopen Crossbow");
    // With the switch off, quitting installs nothing.
    let world = World::new("0.2.0", 7);
    let store = world.fixture.store();
    let (mut updates, wakes) = world.updates(Code(Some(TEAM)));
    updates.auto.choose(&store, false);
    updates.check_now();
    wait(&mut updates, &store, &wakes, |state| matches!(state, State::Available(_)));
    updates.download();
    wait(&mut updates, &store, &wakes, |state| matches!(state, State::Ready { .. }));
    assert!(!updates.install_at_quit(0));
}

#[test]
fn an_ad_hoc_signed_app_or_a_folder_it_cant_write_offers_show_download_instead() {
    let world = World::new("0.2.0", 7);
    let store = world.fixture.store();
    let (mut updates, wakes) = world.updates(Code(None));
    updates.check_now();
    wait(&mut updates, &store, &wakes, |state| matches!(state, State::Available(_)));
    updates.download();
    wait(&mut updates, &store, &wakes, |state| !matches!(state, State::Downloading { .. }));
    assert_eq!(
        updates.status(),
        "Crossbow 0.2.0 is downloaded. Its update signature and hash match, and the app is validly signed by Developer ID team ABCDE12345, but this ad-hoc signed copy has no team of its own to match, so it won't install it. Show Download to install it yourself."
    );
    assert!(!updates.install_at_quit(0) && install::read_outcome(&world.state_dir()).is_none(), "nothing installs at quit, and nothing failed");
    assert!(matches!(updates.restart(0, true), Restart::Refused(_)));
    assert!(world.helpers.lock().unwrap().is_empty());

    let world = World::new("0.2.0", 7);
    let (mut updates, wakes) = world.updates(Code(Some(TEAM)));
    use std::os::unix::fs::PermissionsExt;
    let folder = world.installed().parent().unwrap().to_path_buf();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
    updates.check_now();
    wait(&mut updates, &store, &wakes, |state| matches!(state, State::Available(_)));
    updates.download();
    wait(&mut updates, &store, &wakes, |state| !matches!(state, State::Downloading { .. }));
    let status = updates.status();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(status.ends_with("it can't install itself: you can't write to the folder that holds Crossbow. Show Download to install it yourself."), "{status}");
}

#[test]
fn offline_is_said_plainly_and_recorded_as_the_last_check() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/stable.json", listener.local_addr().unwrap());
    drop(listener);
    let world = World::new("0.2.0", 7);
    let store = world.fixture.store();
    let (mut updates, wakes) = world.updates(Code(Some(TEAM)));
    updates.build = crossbow_core::update::build(Some(leak(url)), world.build.map(|build| build.keys));
    updates.check_now();
    wait(&mut updates, &store, &wakes, |state| !matches!(state, State::Checking));
    let offline = "Crossbow couldn't reach the update server. Check your connection and try again.";
    assert_eq!(updates.status(), offline);
    assert_eq!(world.updates(Code(Some(TEAM))).0.last.map(|last| last.result).as_deref(), Some(offline));
}

#[test]
fn every_state_has_its_own_truthful_text() {
    let mac = Some(MacVersion(15, 0, 0));
    let text = |result: Result<Offer, CheckError>| check_text(&result, mac);
    assert_eq!(text(Ok(Offer::UpToDate)), format!("Crossbow {} is up to date.", crossbow_core::update::VERSION));
    assert_eq!(text(Ok(Offer::Unsupported { version: Version(0, 3, 0), needs: MacVersion(26, 0, 0) })), "Crossbow 0.3.0 needs macOS 26.0 or later; this Mac has macOS 15.0.");
    assert_eq!(text(Ok(Offer::Older(Version(0, 0, 9)))), "The update server offers Crossbow 0.0.9, older than this version, so nothing was installed.");
    assert_eq!(text(Ok(Offer::Expired { days: 0 })), "The update server's feed expired, so Crossbow can't tell whether it is up to date. Try again later.");
    assert_eq!(text(Ok(Offer::Expired { days: 1 })), "The update server's feed expired a day ago, so Crossbow can't tell whether it is up to date. Try again later.");
    assert_eq!(text(Ok(Offer::Expired { days: 9 })), "The update server's feed expired 9 days ago, so Crossbow can't tell whether it is up to date. Try again later.");
    assert_eq!(text(Ok(Offer::Replayed { offered: Version(0, 2, 0), newest: Version(0, 3, 0) })), "The update server offers Crossbow 0.2.0, older than the 0.3.0 it offered before, so Crossbow can't tell whether it is up to date. Nothing was installed.");
    assert_eq!(text(Ok(Offer::RolledBack(Version(0, 2, 0)))), "Crossbow 0.2.0 was rolled back on this Mac, so it isn't offered again. A later build will be.");
    assert_eq!(text(Err(CheckError::Offline)), "Crossbow couldn't reach the update server. Check your connection and try again.");
    assert_eq!(text(Err(CheckError::Status(503))), "The update server answered with an error (HTTP 503). Try again later.");
    assert_eq!(text(Err(CheckError::Reply)), "The update server's reply couldn't be read. Try again later.");
    assert_eq!(text(Err(CheckError::Feed(FeedError::Signature))), "The update's signature didn't match this build's keys, so nothing was downloaded or installed.");
    assert_eq!(text(Err(CheckError::Feed(FeedError::Field("version")))), "The update server sent a feed Crossbow can't use: the feed's version is not valid.");
    for (error, expected) in [
        (DownloadError::Size, "Crossbow 0.2.0 was not installed: the size check failed: the download is not the size the feed names. The download was deleted."),
        (DownloadError::Hash, "Crossbow 0.2.0 was not installed: the SHA-256 check failed: the download is not the file the feed names. The download was deleted."),
        (DownloadError::Signature, "Crossbow 0.2.0 was not installed: the signature check failed: the download's signature does not match a key in this build. The download was deleted."),
        (DownloadError::Code(CodeError::Team), "Crossbow 0.2.0 was not installed: the code signature check failed: the new app is not signed with this app's Team ID. The download was deleted."),
        (DownloadError::Offline, "Crossbow 0.2.0 couldn't be downloaded: Crossbow couldn't reach the update server."),
        (DownloadError::TooSlow, "Crossbow 0.2.0 couldn't be downloaded: the download was too slow, so it was stopped."),
        (DownloadError::MacOS, "Crossbow 0.2.0 was not installed: the macOS check failed: the app in the download needs another macOS than the feed names, or a later one than this Mac has. The download was deleted."),
        (DownloadError::Code(CodeError::DeveloperId), "Crossbow 0.2.0 was not installed: the code signature check failed: the new app is not signed with a Developer ID. The download was deleted."),
    ] {
        assert_eq!(download_text("Crossbow 0.2.0", &error), expected);
    }
    let world = World::new("0.2.0", 7);
    let (mut updates, _) = world.updates(Code(Some(TEAM)));
    updates.build = None;
    assert_eq!(updates.status(), "This build has no updater.");
    updates.check_now();
    assert!(matches!(updates.state, State::Idle), "a build without an updater never checks");
}

/// The helper swapped this build in for 0.1.0 and probed it: the journal a
/// new version finds at its start, with the old bundle kept beside it.
fn probed(world: &World) -> install::Paths {
    let state = world.state_dir();
    net::private_folder(&state).unwrap();
    let paths = install::Paths::beside(&world.installed());
    fake_app(&paths.previous, "0.1.0", 1);
    let journal = Journal { app: world.installed(), from: Stamp { version: "0.1.0".into(), build: 1 }, to: Stamp::running(), pid: 1, relaunch: false, step: Step::Probed, launches: 1, reason: None };
    install::write_atomic(&state.join(install::JOURNAL), &serde_json::to_vec(&journal).unwrap()).unwrap();
    paths
}

#[test]
fn a_new_version_confirms_itself_only_after_drawing_and_running_a_while_or_at_a_clean_quit() {
    let world = World::new("0.2.0", 7);
    let store = world.fixture.store();
    let paths = probed(&world);
    let (mut updates, wakes) = world.updates(Code(Some(TEAM)));
    let start = Instant::now();
    updates.tick(&store, NOW, start + CONFIRM_AFTER * 10, 0);
    assert!(install::read_journal(&world.state_dir()).is_some(), "no frame drawn yet: a crash now still rolls back");
    updates.drawn(start);
    updates.tick(&store, NOW, start + CONFIRM_AFTER - Duration::from_millis(1), 0);
    std::thread::sleep(Duration::from_millis(200));
    updates.tick(&store, NOW, start + CONFIRM_AFTER - Duration::from_millis(1), 0);
    assert!(install::read_journal(&world.state_dir()).is_some() && paths.previous.exists(), "nor just after the first frame");
    updates.tick(&store, NOW, start + CONFIRM_AFTER, 0);
    while updates.outcome.is_none() {
        wakes.recv_timeout(Duration::from_secs(20)).expect("confirmed");
        updates.tick(&store, NOW, start + CONFIRM_AFTER, 0);
    }
    let updated = format!("Crossbow was updated to {}.", crossbow_core::update::VERSION);
    assert_eq!((updates.outcome.as_deref(), updates.notices.clone()), (Some(updated.as_str()), vec![updated.clone()]), "said once, as a toast and in Settings");
    assert!(install::read_journal(&world.state_dir()).is_none() && !paths.previous.exists(), "the old version is gone");
    assert_eq!(install::read_outcome(&world.state_dir()), None, "and not said again at the next start");

    // A clean quit confirms too, but only after a frame.
    let world = World::new("0.2.0", 7);
    let paths = probed(&world);
    let (mut updates, _) = world.updates(Code(Some(TEAM)));
    updates.confirm_at_quit();
    assert!(install::read_journal(&world.state_dir()).is_some() && paths.previous.exists(), "quit before any frame");
    let (mut updates, _) = world.updates(Code(Some(TEAM)));
    updates.drawn(Instant::now());
    updates.confirm_at_quit();
    assert!(install::read_journal(&world.state_dir()).is_none() && !paths.previous.exists());
    let (reopened, _) = world.updates(Code(Some(TEAM)));
    assert_eq!(reopened.outcome, Some(format!("Crossbow was updated to {}.", crossbow_core::update::VERSION)), "said at the next start");
}

#[test]
fn a_ready_download_stays_ready_through_later_checks_unless_a_newer_build_appears() {
    let world = World::new("0.2.0", 7);
    let store = world.fixture.store();
    let (mut updates, wakes) = world.updates(Code(Some(TEAM)));
    updates.check_now();
    wait(&mut updates, &store, &wakes, |state| matches!(state, State::Available(_)));
    updates.download();
    wait(&mut updates, &store, &wakes, |state| matches!(state, State::Ready { .. }));
    let ready_status = updates.status();
    // No automatic check runs while it is ready, however long Crossbow runs.
    let start = Instant::now();
    updates.drawn(start);
    updates.tick(&store, NOW + 3 * DAY, start + GRACE * 100, 0);
    assert!(matches!(updates.state, State::Ready { .. }) && world.server.requests() == 2, "no automatic check");
    // Check Now finds the same release: still ready, not downloaded again.
    updates.check_now();
    wait(&mut updates, &store, &wakes, |state| !matches!(state, State::Checking));
    assert_eq!((updates.status(), world.server.requests()), (ready_status.clone(), 3));
    assert!(updates.restart(0, true) == Restart::Quit, "and it still installs");
    // A failed check keeps it too: the server has gone.
    let world = World::new("0.2.0", 7);
    let store = world.fixture.store();
    let (mut updates, wakes) = world.updates(Code(Some(TEAM)));
    updates.check_now();
    wait(&mut updates, &store, &wakes, |state| matches!(state, State::Available(_)));
    updates.download();
    wait(&mut updates, &store, &wakes, |state| matches!(state, State::Ready { .. }));
    drop(world.server);
    updates.check_now();
    wait(&mut updates, &store, &wakes, |state| !matches!(state, State::Checking));
    assert!(matches!(updates.state, State::Ready { .. }), "{}", updates.status());
    assert_eq!(updates.last.as_ref().map(|last| last.result.as_str()), Some("Crossbow couldn't reach the update server. Check your connection and try again."));
}

#[test]
fn an_install_at_quit_that_cant_start_is_said_at_the_next_start_until_a_frame_shows_it() {
    let world = World::new("0.2.0", 7);
    let store = world.fixture.store();
    let (mut updates, wakes) = world.updates(Code(Some(TEAM)));
    updates.check_now();
    wait(&mut updates, &store, &wakes, |state| matches!(state, State::Available(_)));
    updates.download();
    wait(&mut updates, &store, &wakes, |state| matches!(state, State::Ready { .. }));
    updates.spawn_helper = Box::new(|_| Err(std::io::Error::other("no helper")));
    assert!(!updates.install_at_quit(0));
    let said = "The update to 0.2.0 was not installed: the installer couldn't start (other error).";
    // Reopened, and quit again before drawing: it is still to be said.
    for _ in 0..2 {
        let (reopened, _) = world.updates(Code(Some(TEAM)));
        assert_eq!((reopened.outcome.as_deref(), reopened.notices.clone()), (Some(said), vec![said.to_string()]));
    }
    let (mut reopened, _) = world.updates(Code(Some(TEAM)));
    reopened.drawn(Instant::now());
    assert_eq!(reopened.outcome.as_deref(), Some(said), "still in Settings this session");
    assert_eq!(world.updates(Code(Some(TEAM))).0.outcome, None, "once a frame has shown it, not again");
}

#[test]
fn a_stale_feed_never_says_up_to_date() {
    let world = World::new("0.2.0", 7);
    let store = world.fixture.store();
    let (mut updates, wakes) = world.updates(Code(Some(TEAM)));
    updates.clock = || NOW + 16 * DAY;
    updates.check_now();
    wait(&mut updates, &store, &wakes, |state| !matches!(state, State::Checking));
    assert_eq!(updates.status(), "The update server's feed expired 2 days ago, so Crossbow can't tell whether it is up to date. Try again later.");
    assert_eq!(world.server.requests(), 1, "nothing downloaded");
}
