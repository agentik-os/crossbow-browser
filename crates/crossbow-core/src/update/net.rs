//! Checking the feed and downloading the archive, on a worker thread. URLs
//! are never logged; reqwest errors carry them, so they are reduced to a
//! kind before leaving this module.
//!
//! A check judges the feed against what this Mac knows (see
//! [`feed::Known`]): the newest release an unexpired feed named, kept in
//! the update folder, and the releases rolled back here.
//!
//! A download goes to a private (0700) folder, as a 0600 file, within an
//! overall deadline, and passes five checks in order: size, SHA-256, the
//! archive's Ed25519 signature and, once unpacked, the app's version and
//! minimum macOS, and its code signature. Any failure deletes what was
//! downloaded and names the check.

use super::archive::{self, ArchiveError, Bundle};
use super::codesign::{self, CodeCheck, CodeError};
use super::feed::{self, FeedError, Known, Offer, Release};
use super::install::{self, Stamp};
use super::{MacVersion, Running};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

/// Covers connecting, the reply and the feed's body.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// A download that receives nothing for this long fails.
pub const STALL: Duration = Duration::from_secs(60);
/// A whole download may take this long, plus a second for each
/// [`MIN_RATE`] bytes of the archive.
const DEADLINE_BASE: Duration = Duration::from_secs(5 * 60);
/// The slowest average a download may keep: 32 KiB a second.
pub const MIN_RATE: u64 = 32 * 1024;
/// The download folder in the update folder; in it, the archive as
/// downloaded, then the unpacked app.
pub const DOWNLOAD: &str = "download";
pub const ARCHIVE_FILE: &str = "update.tar.gz";
pub const APP_FOLDER: &str = "app";
/// The newest release an unexpired feed has named, in the update folder.
pub const NEWEST: &str = "newest.json";

/// Why a check found nothing to offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CheckError {
    #[error("Crossbow couldn't reach the update server")]
    Offline,
    #[error("the update server answered with HTTP {0}")]
    Status(u16),
    #[error("the update server's reply couldn't be read")]
    Reply,
    #[error(transparent)]
    Feed(#[from] FeedError),
}

/// Why a download was refused; each names the check that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DownloadError {
    #[error("the download was cancelled")]
    Cancelled,
    #[error("Crossbow couldn't reach the update server")]
    Offline,
    #[error("the update server answered with HTTP {0}")]
    Status(u16),
    #[error("the update server's reply couldn't be read")]
    Reply,
    #[error("the download was too slow, so it was stopped")]
    TooSlow,
    #[error("the size check failed: the download is not the size the feed names")]
    Size,
    #[error("the SHA-256 check failed: the download is not the file the feed names")]
    Hash,
    #[error("the signature check failed: the download's signature does not match a key in this build")]
    Signature,
    #[error("the archive check failed: {0}")]
    Archive(ArchiveError),
    #[error("the version check failed: the app in the download is not the version the feed names")]
    Version,
    #[error("the macOS check failed: the app in the download needs another macOS than the feed names, or a later one than this Mac has")]
    MacOS,
    #[error("the code signature check failed: {0}")]
    Code(CodeError),
    #[error("the download couldn't be saved ({0})")]
    Disk(io::ErrorKind),
}

impl From<io::Error> for DownloadError {
    fn from(error: io::Error) -> Self {
        DownloadError::Disk(error.kind())
    }
}

/// A download's progress, shared with the UI, and its Cancel.
#[derive(Debug, Default)]
pub struct Progress {
    received: AtomicU64,
    total: AtomicU64,
    cancelled: AtomicBool,
    cancel: tokio::sync::Notify,
}

impl Progress {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.cancel.notify_one();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Bytes received and expected.
    pub fn bytes(&self) -> (u64, u64) {
        (self.received.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed))
    }
}

/// A verified download, unpacked and ready to stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ready {
    pub release: Release,
    /// The unpacked `Crossbow.app`.
    pub app: PathBuf,
    pub bundle: Bundle,
    /// The Developer ID team that validly signed it.
    pub team: String,
    /// Whether it may install itself: not when the running app is ad-hoc
    /// signed, so has no team to match.
    pub install: Result<(), CodeError>,
}

/// The client for update requests: no cookies, no retries, and redirects
/// only to https (or within loopback http, for tests).
pub fn client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("Crossbow/", env!("CARGO_PKG_VERSION")))
        .retry(reqwest::retry::never())
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let loopback = |url: &url::Url| url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"));
            let first_loopback = attempt.previous().first().is_some_and(loopback);
            let allowed = attempt.url().scheme() == "https" || (first_loopback && loopback(attempt.url()));
            if allowed && attempt.previous().len() <= 5 { attempt.follow() } else { attempt.stop() }
        }))
        .build()
}

fn check_kind(error: reqwest::Error) -> CheckError {
    if error.is_connect() || error.is_timeout() { CheckError::Offline } else { CheckError::Reply }
}

/// Fetch and verify the feed, judge it against what this Mac knows at
/// `now`, and keep the release it names as the newest seen when the feed
/// is current.
pub async fn check(client: &reqwest::Client, config: &Config, now: u64) -> Result<Offer, CheckError> {
    let request = async {
        let mut response = client.get(&config.url).header(reqwest::header::ACCEPT, "application/json").send().await.map_err(check_kind)?;
        if !response.status().is_success() {
            return Err(CheckError::Status(response.status().as_u16()));
        }
        if response.content_length().is_some_and(|length| length > feed::MAX_FEED as u64) {
            return Err(FeedError::TooLarge.into());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(check_kind)? {
            if body.len() + chunk.len() > feed::MAX_FEED {
                return Err(FeedError::TooLarge.into());
            }
            body.extend_from_slice(&chunk);
        }
        let release = feed::open(&body, &config.keys)?;
        let known = known(config, now);
        let named = Stamp { version: release.version.clone(), build: release.build };
        let offer = feed::judge(release, &known);
        if !matches!(offer, Offer::Expired { .. } | Offer::Replayed { .. }) && known.newest.as_ref() != Some(&named) {
            let _ = private_folder(&config.state).and_then(|()| install::write_atomic(&config.state.join(NEWEST), &serde_json::to_vec(&named).map_err(io::Error::other)?));
        }
        Ok(offer)
    };
    tokio::time::timeout(CHECK_TIMEOUT, request).await.map_err(|_| CheckError::Offline)?
}

/// What this Mac knows at `now`, from the update folder.
pub fn known(config: &Config, now: u64) -> Known {
    let newest = fs::read(config.state.join(NEWEST)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok());
    Known { running: config.running, mac: config.mac, now, newest, rolled_back: install::rolled_back(&config.state) }
}

/// How long downloading an archive of `size` bytes may take in all.
pub fn deadline(size: u64) -> Duration {
    DEADLINE_BASE + Duration::from_secs(size / MIN_RATE)
}

/// Make `dir` a folder only this user can read.
pub fn private_folder(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    match fs::DirBuilder::new().recursive(true).mode(0o700).create(dir) {
        Ok(()) => fs::set_permissions(dir, fs::Permissions::from_mode(0o700)),
        Err(error) => Err(error),
    }
}

/// Download `release`'s archive into the private download folder, check
/// it, unpack it and check the app inside. On failure the folder keeps
/// nothing.
pub async fn download(client: &reqwest::Client, release: &Release, config: &Config, progress: &Progress) -> Result<Ready, DownloadError> {
    let dir = config.state.join(DOWNLOAD);
    private_folder(&dir)?;
    let (archive_path, app_dir) = (dir.join(ARCHIVE_FILE), dir.join(APP_FOLDER));
    let clear = || {
        let _ = install::remove(&archive_path);
        let _ = install::remove(&app_dir);
    };
    clear();
    let result = async {
        let sha256 = fetch(client, release, &archive_path, progress, deadline(release.archive.size)).await?;
        verify(release, &config.keys, &archive_path, &app_dir, sha256, config.mac, config.code.as_ref())
    }
    .await;
    if result.is_err() {
        clear();
    }
    let _ = fs::remove_file(&archive_path);
    result
}

async fn fetch(client: &reqwest::Client, release: &Release, path: &Path, progress: &Progress, deadline: Duration) -> Result<[u8; 32], DownloadError> {
    tokio::time::timeout(deadline, fetch_all(client, release, path, progress)).await.map_err(|_| DownloadError::TooSlow)?
}

async fn fetch_all(client: &reqwest::Client, release: &Release, path: &Path, progress: &Progress) -> Result<[u8; 32], DownloadError> {
    use std::os::unix::fs::OpenOptionsExt;
    let offline = |error: reqwest::Error| if error.is_connect() || error.is_timeout() { DownloadError::Offline } else { DownloadError::Reply };
    let size = release.archive.size;
    progress.total.store(size, Ordering::Relaxed);
    progress.received.store(0, Ordering::Relaxed);
    let mut response = tokio::select! {
        response = client.get(&release.archive.url).send() => response.map_err(offline)?,
        () = progress.cancel.notified() => return Err(DownloadError::Cancelled),
    };
    if !response.status().is_success() {
        return Err(DownloadError::Status(response.status().as_u16()));
    }
    if response.content_length().is_some_and(|length| length != size) {
        return Err(DownloadError::Size);
    }
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    let (mut hasher, mut received) = (Sha256::new(), 0u64);
    loop {
        if progress.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let next = tokio::select! {
            next = tokio::time::timeout(STALL, response.chunk()) => next,
            () = progress.cancel.notified() => return Err(DownloadError::Cancelled),
        };
        let Some(chunk) = next.map_err(|_| DownloadError::Offline)?.map_err(offline)? else { break };
        received += chunk.len() as u64;
        if received > size {
            return Err(DownloadError::Size);
        }
        hasher.update(&chunk);
        file.write_all(&chunk)?;
        progress.received.store(received, Ordering::Relaxed);
    }
    file.sync_all()?;
    if received != size {
        return Err(DownloadError::Size);
    }
    <[u8; 32]>::try_from(hasher.finalize().as_slice()).map_err(|_| DownloadError::Hash)
}

fn verify(release: &Release, keys: &[[u8; 32]], archive_path: &Path, app_dir: &Path, sha256: [u8; 32], mac: Option<MacVersion>, code: &dyn CodeCheck) -> Result<Ready, DownloadError> {
    if sha256 != release.sha256() {
        return Err(DownloadError::Hash);
    }
    if !feed::archive_verifies(release, keys, &sha256, release.archive.size) {
        return Err(DownloadError::Signature);
    }
    private_folder(app_dir)?;
    let bundle = archive::unpack(archive_path, app_dir).map_err(DownloadError::Archive)?;
    if bundle.version != release.version() || bundle.build != release.build {
        return Err(DownloadError::Version);
    }
    // LaunchServices refuses to open an app that needs a later macOS, and
    // the probe would not notice, so nothing could roll it back.
    if bundle.minimum_macos != release.minimum_macos() || mac.is_none_or(|mac| mac < bundle.minimum_macos) {
        return Err(DownloadError::MacOS);
    }
    let app = app_dir.join(archive::APP);
    let signed = codesign::check(code, &app).map_err(DownloadError::Code)?;
    Ok(Ready { release: release.clone(), app, bundle, team: signed.team, install: signed.install })
}

/// What the worker needs for every request.
#[derive(Clone)]
pub struct Config {
    pub url: String,
    pub keys: Vec<[u8; 32]>,
    pub running: Running,
    pub mac: Option<MacVersion>,
    /// The private update folder.
    pub state: PathBuf,
    pub code: Arc<dyn CodeCheck>,
}

pub enum Job {
    /// A check at this time, in seconds since 1970.
    Check(u64),
    Download(Release, Arc<Progress>),
}

#[derive(Debug)]
pub enum Reply {
    Checked(Result<Offer, CheckError>),
    Downloaded(Result<Ready, DownloadError>),
}

/// A thread that runs jobs one after another and calls `wake` after each
/// reply. Dropping it ends the thread once a running job finishes.
pub struct Worker {
    jobs: Sender<Job>,
    replies: Receiver<Reply>,
}

impl Worker {
    pub fn spawn(config: Config, wake: impl Fn() + Send + 'static) -> io::Result<Worker> {
        let (jobs, queue) = mpsc::channel::<Job>();
        let (answer, replies) = mpsc::channel();
        std::thread::Builder::new().name("crossbow-update".into()).spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().ok();
            let client = client().ok();
            while let Ok(job) = queue.recv() {
                let reply = match (&runtime, &client, job) {
                    (Some(runtime), Some(client), Job::Check(now)) => Reply::Checked(runtime.block_on(check(client, &config, now))),
                    (Some(runtime), Some(client), Job::Download(release, progress)) => Reply::Downloaded(runtime.block_on(download(client, &release, &config, &progress))),
                    (_, _, Job::Check(_)) => Reply::Checked(Err(CheckError::Offline)),
                    (_, _, Job::Download(..)) => Reply::Downloaded(Err(DownloadError::Offline)),
                };
                if answer.send(reply).is_err() {
                    break;
                }
                wake();
            }
        })?;
        Ok(Worker { jobs, replies })
    }

    pub fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
    }

    pub fn try_recv(&self) -> Option<Reply> {
        self.replies.try_recv().ok()
    }
}

#[cfg(test)]
#[path = "net_tests.rs"]
mod tests;
