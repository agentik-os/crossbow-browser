use super::*;
use crate::update::fixtures::{Answer, FakeCode, Server, closed_port, fake_archive, free_base, keypair, release, sha256};
use crate::update::install::Stamp;
use crate::update::{DAY, Version};
use std::collections::HashMap;

const TEAM: &str = "ABCDE12345";
const MAC: Option<MacVersion> = Some(MacVersion(15, 0, 0));

fn running() -> Running {
    Running { version: Version(0, 1, 0), build: 1 }
}

fn now() -> u64 {
    crate::update::fixtures::now()
}

/// Everything under `dir`, relative.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir).map(|entries| entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    names.sort();
    names
}

struct World {
    tmp: tempfile::TempDir,
    key: crate::update::feed::SigningKey,
    public: [u8; 32],
    archive: Vec<u8>,
}

impl World {
    fn new() -> World {
        let tmp = tempfile::tempdir().unwrap();
        let (key, public) = keypair();
        let archive = fake_archive(tmp.path(), "0.2.0", 7);
        World { tmp, key, public, archive }
    }

    /// The update folder.
    fn state(&self) -> PathBuf {
        self.tmp.path().join("data/updates")
    }

    fn downloads(&self) -> PathBuf {
        self.state().join(DOWNLOAD)
    }

    fn config(&self, url: &str, code: impl CodeCheck + 'static) -> Config {
        Config { url: url.into(), keys: vec![self.public], running: running(), mac: MAC, state: self.state(), code: Arc::new(code) }
    }

    /// A server with the feed for `release` at /stable.json and `archive` at /a.tar.gz.
    fn serve(&self, feed: String, archive: Vec<u8>) -> Server {
        Server::start(HashMap::from([("/stable.json".into(), Answer::ok(feed)), ("/a.tar.gz".into(), Answer::ok(archive))]))
    }

    fn release_at(&self, server_base: &str) -> Release {
        release(&format!("{server_base}/a.tar.gz"), &self.archive, &self.key, "0.2.0", 7)
    }
}

/// A server whose feed points at its own archive, made by `edit`.
fn server_with(world: &World, edit: impl FnOnce(&mut Release, &mut Vec<u8>)) -> (Server, Release) {
    // The archive URL names the port, so the port is chosen first.
    let base = free_base();
    let mut release = world.release_at(&base);
    let mut archive = world.archive.clone();
    edit(&mut release, &mut archive);
    let feed = feed::sign(&release, &world.key).unwrap();
    let server = Server::start_at(&base, HashMap::from([("/stable.json".into(), Answer::ok(feed)), ("/a.tar.gz".into(), Answer::ok(archive))]));
    (server, release)
}

async fn check_at(world: &World, server: &Server, keys: &[[u8; 32]]) -> Result<Offer, CheckError> {
    let config = Config { keys: keys.to_vec(), ..world.config(&server.url("/stable.json"), FakeCode::signed(TEAM)) };
    check(&client().unwrap(), &config, now()).await
}

async fn download_from(world: &World, release: &Release, code: impl CodeCheck + 'static) -> Result<Ready, DownloadError> {
    download(&client().unwrap(), release, &world.config("", code), &Progress::default()).await
}

#[tokio::test(flavor = "current_thread")]
async fn no_update_when_the_feed_names_this_build() {
    let world = World::new();
    let current = release("http://127.0.0.1:1/a.tar.gz", &world.archive, &world.key, "0.1.0", 1);
    let server = world.serve(feed::sign(&current, &world.key).unwrap(), Vec::new());
    assert_eq!(check_at(&world, &server, &[world.public]).await, Ok(Offer::UpToDate));
    assert_eq!(*server.requests.lock().unwrap(), vec!["/stable.json".to_string()], "only the feed is fetched");
}

#[tokio::test(flavor = "current_thread")]
async fn an_available_update_downloads_verifies_and_unpacks_into_a_private_folder() {
    let world = World::new();
    let (server, release) = server_with(&world, |_, _| {});
    let Ok(Offer::Available(offered)) = check_at(&world, &server, &[world.public]).await else { panic!("an update is available") };
    assert_eq!(offered, release);
    let progress = Progress::default();
    let ready = download(&client().unwrap(), &offered, &world.config("", FakeCode::signed(TEAM)), &progress).await.unwrap();
    assert_eq!((ready.team.as_str(), ready.install), (TEAM, Ok(())));
    assert_eq!(Stamp::at(&ready.app), Some(Stamp { version: "0.2.0".into(), build: 7 }));
    assert_eq!(progress.bytes(), (world.archive.len() as u64, world.archive.len() as u64));
    assert_eq!(listing(&world.downloads()), vec![APP_FOLDER], "the archive is deleted once unpacked");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(fs::metadata(world.downloads()).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(world.downloads().join(APP_FOLDER)).unwrap().permissions().mode() & 0o777, 0o700);
}

#[tokio::test(flavor = "current_thread")]
async fn a_tampered_feed_or_one_signed_by_a_key_not_embedded_is_a_bad_signature() {
    let world = World::new();
    let (_, other) = keypair();
    let (server, _) = server_with(&world, |_, _| {});
    assert_eq!(check_at(&world, &server, &[other]).await, Err(CheckError::Feed(FeedError::Signature)), "signed by a key this build doesn't embed");
    let feed = feed::sign(&world.release_at("http://127.0.0.1:1"), &world.key).unwrap().replacen("0.2.0", "0.2.1", 1);
    let server = world.serve(feed, Vec::new());
    assert_eq!(check_at(&world, &server, &[world.public]).await, Err(CheckError::Feed(FeedError::Signature)), "tampered after signing");
}

#[tokio::test(flavor = "current_thread")]
async fn each_failed_download_check_is_named_and_leaves_nothing() {
    let world = World::new();
    let (other, _) = keypair();
    type Edit = Box<dyn FnOnce(&mut Release, &mut Vec<u8>)>;
    let cases: Vec<(&str, Edit, DownloadError)> = vec![
        ("a tampered archive", Box::new(|_, archive: &mut Vec<u8>| *archive.last_mut().unwrap() ^= 1), DownloadError::Hash),
        ("a wrong size", Box::new(|release: &mut Release, _| release.archive.size += 1), DownloadError::Size),
        ("a shorter file", Box::new(|_, archive: &mut Vec<u8>| { archive.pop(); }), DownloadError::Size),
        ("a wrong hash", Box::new(|release: &mut Release, _| release.archive.sha256 = data_encoding::HEXLOWER.encode(&sha256(b"other"))), DownloadError::Hash),
        ("an archive signed by the wrong key", Box::new(move |release: &mut Release, archive: &mut Vec<u8>| {
            release.archive.signature = feed::sign_archive(&other, &sha256(archive), archive.len() as u64);
        }), DownloadError::Signature),
        ("an app that isn't the version the feed names", Box::new(|release: &mut Release, _| release.build = 8), DownloadError::Version),
    ];
    for (name, edit, expected) in cases {
        let (server, release) = server_with(&world, edit);
        assert!(matches!(check_at(&world, &server, &[world.public]).await, Ok(Offer::Available(_))), "{name}: the feed itself verifies");
        let result = download_from(&world, &release, FakeCode::signed(TEAM)).await;
        assert_eq!(result.as_ref().err(), Some(&expected), "{name}");
        assert!(result.unwrap_err().to_string().contains("check failed"), "{name}: says which check failed");
        assert_eq!(listing(&world.downloads()), Vec::<String>::new(), "{name}: the download is deleted");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn an_archive_that_escapes_its_bundle_is_refused_and_writes_nothing_outside() {
    use crate::update::fixtures::{Raw, minimal_bundle, raw_tar_gz};
    let world = World::new();
    let (plist, script) = minimal_bundle("0.2.0", 7);
    let hostile = raw_tar_gz(&[
        Raw::File("Crossbow.app/Contents/Info.plist", plist.as_bytes()),
        Raw::File("Crossbow.app/Contents/MacOS/crossbow", script.as_bytes()),
        Raw::Symlink("Crossbow.app/Contents/out", "../../../.."),
    ]);
    let climbing = raw_tar_gz(&[Raw::File("Crossbow.app/Contents/Info.plist", plist.as_bytes()), Raw::File("Crossbow.app/../../escaped", b"x")]);
    for (bytes, expected) in [(hostile, ArchiveError::Symlink), (climbing, ArchiveError::Parent)] {
        let (server, release) = server_with(&world, |release, archive| {
            *archive = bytes.clone();
            release.archive.size = archive.len() as u64;
            release.archive.sha256 = data_encoding::HEXLOWER.encode(&sha256(archive));
            release.archive.signature = feed::sign_archive(&world.key, &sha256(archive), archive.len() as u64);
        });
        assert_eq!(download_from(&world, &release, FakeCode::signed(TEAM)).await.err(), Some(DownloadError::Archive(expected)));
        drop(server);
        assert_eq!(listing(&world.downloads()), Vec::<String>::new());
        assert!(!world.tmp.path().join("data/escaped").exists() && !world.tmp.path().join("escaped").exists());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_downgrade_is_never_offered_and_a_newer_macos_requirement_is_reported() {
    let world = World::new();
    let older = release("http://127.0.0.1:1/a.tar.gz", &world.archive, &world.key, "0.0.9", 99);
    let server = world.serve(feed::sign(&older, &world.key).unwrap(), Vec::new());
    assert_eq!(check_at(&world, &server, &[world.public]).await, Ok(Offer::Older(Version(0, 0, 9))));
    let mut needs = world.release_at("http://127.0.0.1:1");
    needs.minimum_macos = "99.0".into();
    let server = world.serve(feed::sign(&needs, &world.key).unwrap(), Vec::new());
    assert_eq!(check_at(&world, &server, &[world.public]).await, Ok(Offer::Unsupported { version: Version(0, 2, 0), needs: MacVersion(99, 0, 0) }));
}

#[tokio::test(flavor = "current_thread")]
async fn offline_and_http_errors_are_told_apart() {
    let world = World::new();
    let nowhere = closed_port();
    assert_eq!(check(&client().unwrap(), &world.config(&format!("{nowhere}/stable.json"), FakeCode::signed(TEAM)), now()).await, Err(CheckError::Offline));
    let release = world.release_at(&nowhere);
    assert_eq!(download_from(&world, &release, FakeCode::signed(TEAM)).await.err(), Some(DownloadError::Offline));
    for status in [404, 500, 503] {
        let server = Server::start(HashMap::from([("/stable.json".into(), Answer::status(status)), ("/a.tar.gz".into(), Answer::status(status))]));
        assert_eq!(check_at(&world, &server, &[world.public]).await, Err(CheckError::Status(status)));
        let release = world.release_at(&server.base);
        assert_eq!(download_from(&world, &release, FakeCode::signed(TEAM)).await.err(), Some(DownloadError::Status(status)));
    }
    let server = world.serve("x".repeat(feed::MAX_FEED + 1), Vec::new());
    assert_eq!(check_at(&world, &server, &[world.public]).await, Err(CheckError::Feed(FeedError::TooLarge)));
    assert_eq!(listing(&world.downloads()), Vec::<String>::new());
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_stops_a_download_midway_and_deletes_it() {
    let world = World::new();
    let big = vec![7u8; 512 * 1024];
    let base = free_base();
    let release = release(&format!("{base}/a.tar.gz"), &big, &world.key, "0.2.0", 7);
    let server = Server::start_at(&base, HashMap::from([("/a.tar.gz".into(), Answer { status: 200, body: big, stall_after: Some(64 * 1024), trickle: None })]));
    let progress = Arc::new(Progress::default());
    let watcher = progress.clone();
    let canceller = std::thread::spawn(move || {
        while watcher.bytes().0 == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
        watcher.cancel();
        watcher.bytes().0
    });
    let started = std::time::Instant::now();
    let result = download(&client().unwrap(), &release, &world.config("", FakeCode::signed(TEAM)), &progress).await;
    let received = canceller.join().unwrap();
    assert_eq!(result.err(), Some(DownloadError::Cancelled));
    assert!(received > 0 && received < 512 * 1024, "cancelled midway, at {received} bytes");
    assert!(started.elapsed() < STALL / 4, "promptly, though the server has gone quiet");
    assert_eq!(listing(&world.downloads()), Vec::<String>::new());
    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn an_ad_hoc_signed_running_app_checks_the_new_apps_developer_id_and_leaves_installing_it_to_the_user() {
    let world = World::new();
    let (server, release) = server_with(&world, |_, _| {});
    let ad_hoc = FakeCode { team: None, valid: true, signed_by: "ZYXWV98765".into() };
    let ready = download_from(&world, &release, ad_hoc).await.unwrap();
    assert_eq!((ready.team.as_str(), ready.install), ("ZYXWV98765", Err(CodeError::AdHoc)), "validly signed, by any Developer ID team");
    assert!(ready.app.join("Contents/Info.plist").exists(), "kept, to reveal");
    assert!(ready.install.unwrap_err().to_string().contains("ad-hoc signed"));
    for (code, expected) in [
        (FakeCode { team: None, valid: false, signed_by: "ZYXWV98765".into() }, CodeError::Invalid),
        (FakeCode { team: None, valid: true, signed_by: String::new() }, CodeError::DeveloperId),
    ] {
        assert_eq!(download_from(&world, &release, code).await.err(), Some(DownloadError::Code(expected)));
        assert_eq!(listing(&world.downloads()), Vec::<String>::new(), "{expected:?}: deleted");
    }
    // This test process is ad-hoc signed, and the fixture app not signed at
    // all: the Security framework refuses it, and it is deleted.
    assert_eq!(download_from(&world, &release, codesign::System).await.err(), Some(DownloadError::Code(CodeError::Invalid)));
    assert_eq!(listing(&world.downloads()), Vec::<String>::new());
    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn a_new_app_with_a_bad_code_signature_or_another_team_is_deleted() {
    let world = World::new();
    let (server, release) = server_with(&world, |_, _| {});
    for (code, expected) in [
        (FakeCode { team: Some(TEAM.into()), valid: false, signed_by: TEAM.into() }, CodeError::Invalid),
        (FakeCode { team: Some(TEAM.into()), valid: true, signed_by: "ZYXWV98765".into() }, CodeError::Team),
    ] {
        assert_eq!(download_from(&world, &release, code).await.err(), Some(DownloadError::Code(expected)));
        assert_eq!(listing(&world.downloads()), Vec::<String>::new());
    }
    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn a_stale_or_replayed_feed_is_never_up_to_date_and_the_newest_release_seen_is_kept() {
    let world = World::new();
    let serve = |release: &Release| world.serve(feed::sign(release, &world.key).unwrap(), Vec::new());
    let check_now = |server: &Server, now: u64| {
        let config = world.config(&server.url("/stable.json"), FakeCode::signed(TEAM));
        async move { check(&client().unwrap(), &config, now).await }
    };
    // The feed names this very build, but expired two days ago.
    let current = release("http://127.0.0.1:1/a.tar.gz", &world.archive, &world.key, "0.1.0", 1);
    let expired_at = feed::parse_time(&current.expires).unwrap();
    assert_eq!(check_now(&serve(&current), expired_at + 2 * DAY + 5).await, Ok(Offer::Expired { days: 2 }));
    assert_eq!(listing(&world.state()), Vec::<String>::new(), "an expired feed records nothing");
    // A current feed offering 0.3.0 is remembered...
    let newer = release("http://127.0.0.1:1/a.tar.gz", &world.archive, &world.key, "0.3.0", 9);
    assert!(matches!(check_now(&serve(&newer), now()).await, Ok(Offer::Available(_))));
    assert_eq!(known(&world.config("", FakeCode::signed(TEAM)), now()).newest, Some(Stamp { version: "0.3.0".into(), build: 9 }));
    // ...so an older one, validly signed and unexpired, is a replay.
    let older = release("http://127.0.0.1:1/a.tar.gz", &world.archive, &world.key, "0.2.0", 7);
    assert_eq!(check_now(&serve(&older), now()).await, Ok(Offer::Replayed { offered: Version(0, 2, 0), newest: Version(0, 3, 0) }));
    assert_eq!(check_now(&serve(&current), now()).await, Ok(Offer::Replayed { offered: Version(0, 1, 0), newest: Version(0, 3, 0) }), "even one naming this build");
    assert_eq!(known(&world.config("", FakeCode::signed(TEAM)), now()).newest, Some(Stamp { version: "0.3.0".into(), build: 9 }), "a replay changes nothing");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(fs::metadata(world.state()).unwrap().permissions().mode() & 0o777, 0o700);
}

#[tokio::test(flavor = "current_thread")]
async fn a_release_rolled_back_here_is_not_offered_again() {
    let world = World::new();
    let (server, release) = server_with(&world, |_, _| {});
    let config = world.config(&server.url("/stable.json"), FakeCode::signed(TEAM));
    fs::create_dir_all(world.state()).unwrap();
    // As the helper or the app leaves it after a rollback.
    fs::write(world.state().join("rolled_back.json"), br#"[{"version":"0.2.0","build":7}]"#).unwrap();
    assert_eq!(check(&client().unwrap(), &config, now()).await, Ok(Offer::RolledBack(Version(0, 2, 0))));
    let mut rebuilt = release.clone();
    rebuilt.build = 8;
    let server = world.serve(feed::sign(&rebuilt, &world.key).unwrap(), Vec::new());
    let config = world.config(&server.url("/stable.json"), FakeCode::signed(TEAM));
    assert_eq!(check(&client().unwrap(), &config, now()).await, Ok(Offer::Available(rebuilt)), "a later build is");
}

#[tokio::test(flavor = "current_thread")]
async fn a_download_slower_than_its_deadline_stops_and_leaves_nothing() {
    let world = World::new();
    let base = free_base();
    let release = world.release_at(&base);
    // Never quiet for STALL, but far too slow in all.
    let server = Server::start_at(&base, HashMap::from([("/a.tar.gz".into(), Answer { trickle: Some(Duration::from_millis(20)), ..Answer::ok(world.archive.clone()) })]));
    let path = world.tmp.path().join("slow.tar.gz");
    let started = std::time::Instant::now();
    let result = fetch(&client().unwrap(), &release, &path, &Progress::default(), Duration::from_millis(400)).await;
    assert_eq!(result.err(), Some(DownloadError::TooSlow));
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert!(DownloadError::TooSlow.to_string().contains("too slow"));
    drop(server);
    // The deadline: five minutes, plus a second for each 32 KiB.
    assert_eq!(deadline(0), Duration::from_secs(300));
    assert_eq!(deadline(250 << 20), Duration::from_secs(300 + 8000));
    assert!(deadline(feed::MAX_ARCHIVE) < Duration::from_secs(12 * 3600));
}

#[tokio::test(flavor = "current_thread")]
async fn an_app_whose_minimum_macos_differs_from_the_feed_or_this_mac_is_refused() {
    let world = World::new();
    // The bundle needs 11.0 (the fixture's), and the feed says so.
    let (server, release) = server_with(&world, |_, _| {});
    let on = |mac| Config { mac, ..world.config("", FakeCode::signed(TEAM)) };
    for (mac, ok) in [(Some(MacVersion(11, 0, 0)), true), (Some(MacVersion(10, 15, 7)), false), (None, false)] {
        let result = download(&client().unwrap(), &release, &on(mac), &Progress::default()).await;
        assert_eq!(result.as_ref().err(), if ok { None } else { Some(&DownloadError::MacOS) }, "{mac:?}");
    }
    drop(server);
    // The feed claims less than the bundle needs.
    let (server, release) = server_with(&world, |release, _| release.minimum_macos = "10.15".into());
    assert_eq!(download_from(&world, &release, FakeCode::signed(TEAM)).await.err(), Some(DownloadError::MacOS));
    assert_eq!(listing(&world.downloads()), Vec::<String>::new());
    assert!(DownloadError::MacOS.to_string().contains("check failed"));
    drop(server);
}

#[test]
fn the_worker_runs_checks_off_the_calling_thread() {
    let world = World::new();
    let (server, release) = server_with(&world, |_, _| {});
    let (woke, wakes) = std::sync::mpsc::channel();
    let config = world.config(&server.url("/stable.json"), FakeCode::signed(TEAM));
    let worker = Worker::spawn(config, move || {
        let _ = woke.send(());
    })
    .unwrap();
    worker.send(Job::Check(now()));
    wakes.recv_timeout(Duration::from_secs(10)).unwrap();
    let Some(Reply::Checked(Ok(Offer::Available(offered)))) = worker.try_recv() else { panic!("checked") };
    assert_eq!(offered, release);
    worker.send(Job::Download(offered, Arc::new(Progress::default())));
    wakes.recv_timeout(Duration::from_secs(10)).unwrap();
    let Some(Reply::Downloaded(Ok(ready))) = worker.try_recv() else { panic!("downloaded") };
    assert_eq!(ready.install, Ok(()));
}
