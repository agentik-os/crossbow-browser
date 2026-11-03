//! Signed updates in the app: Settings → General → Updates and Crossbow →
//! Check for Updates…. Checks and downloads run on a worker thread; see
//! `crossbow_core::update` for the feed, the checks and the swap.
//!
//! With automatic checks on, a check runs at most once a day (plus up to
//! two hours at random), an update it finds downloads in the background,
//! and it installs at the next quit with no browser downloads in progress.
//! Restart to Update installs at once, after asking when downloads are in
//! progress. The switch and the last check are kept on this device only.
//!
//! A new version confirms itself, which deletes the old one, once its
//! window has drawn and kept running for [`CONFIRM_AFTER`], or at a clean
//! quit after it drew. A crash before either leaves it unconfirmed, and a
//! third start that never confirms rolls back. How an install ended is said
//! at the next start, until a frame has shown it.

use crate::link_prefs::{DeviceSwitch, SwitchSpec};
use crossbow_core::store::Store;
use crossbow_core::update::codesign::{CodeCheck, CodeError};
use crossbow_core::update::feed::{Offer, Release};
use crossbow_core::update::install::{self, InstallError, Instance, Journal, Outcome, Stamp, Step};
use crossbow_core::update::net::{self, CheckError, DownloadError, Job, Progress, Ready, Reply, Worker};
use crossbow_core::update::{self, Build, MacVersion, Running, Schedule};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

pub const MENU_TITLE: &str = "Check for Updates…";
/// The helper beside the app's executable in `Contents/MacOS`.
pub const HELPER: &str = "crossbow-update";
const LAST_KEY: &str = "updates_last_check";
/// How long a new version runs after its first frame before it confirms itself.
pub const CONFIRM_AFTER: Duration = Duration::from_secs(30);

/// Settings → General → Automatically check for updates.
#[derive(Debug)]
pub struct AutoUpdate;

impl SwitchSpec for AutoUpdate {
    const KEY: &'static str = "automatically_check_for_updates";
    const DEFAULT: bool = true;
    const TITLE: &'static str = "Automatically check for updates";
    const DESCRIPTION: &'static str = "Once a day, download a signed update in the background and install it when you quit. This device only.";
    const STILL_OFF: &'static str = "Crossbow still checks only when you ask";
    const STILL_ON: &'static str = "Crossbow still checks once a day";
    const UNREADABLE: &'static str = "The saved choice for update checks could not be read, so Crossbow checks once a day. Changing it saves over it.";
}

/// The last check: when, and what it found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastCheck {
    pub at: u64,
    pub result: String,
}

#[derive(Debug)]
pub enum State {
    Idle,
    Checking,
    /// A line with nothing to do: the check's answer when it offers nothing
    /// to download, or an update handed to the helper.
    Checked(String),
    Available(Release),
    Downloading { release: Release, progress: Arc<Progress> },
    Ready { ready: Ready, refusal: Option<String> },
    /// Restart to Update is waiting for browser downloads to finish.
    Waiting(Ready),
    Failed(String),
}

/// What Restart to Update did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Restart {
    /// The helper is waiting; quit now.
    Quit,
    /// Browser downloads are in progress: ask first.
    Ask(usize),
    Refused(String),
}

pub struct Updates {
    build: Option<Build>,
    pub auto: DeviceSwitch<AutoUpdate>,
    pub state: State,
    pub last: Option<LastCheck>,
    schedule: Schedule,
    worker: Option<Worker>,
    automatic: bool,
    state_dir: PathBuf,
    code: Arc<dyn CodeCheck>,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// When the window first drew a frame.
    drawn_at: Option<Instant>,
    confirm: Confirm,
    /// A ready download a running check keeps unless it finds a newer one.
    kept: Option<(Ready, Option<String>)>,
    /// The installed app; None when not running from a bundle.
    pub app: Option<PathBuf>,
    /// Starts the helper; replaced in tests.
    pub spawn_helper: Box<dyn Fn(&Path) -> std::io::Result<()>>,
    /// Seconds since 1970; replaced in tests.
    pub clock: fn() -> u64,
    pub show_notes: bool,
    /// Restart to Update found downloads in progress.
    pub asking: Option<usize>,
    /// How the last install ended.
    pub outcome: Option<String>,
    /// Lines to show as toasts.
    pub notices: Vec<String>,
    /// The menu asked to show this group.
    pub reveal: bool,
    pub mac: Option<MacVersion>,
}

/// Confirming a new version, once per run.
enum Confirm {
    Waiting,
    Running(mpsc::Receiver<Option<Outcome>>),
    Done,
}

impl Updates {
    pub fn load(store: &Store, data_dir: &Path, build: Option<Build>, code: Arc<dyn CodeCheck>, wake: impl Fn() + Send + Sync + 'static) -> Updates {
        let state_dir = data_dir.join("updates");
        // Kept until a frame has shown it.
        let outcome = install::read_outcome(&state_dir).map(|outcome| outcome.text());
        let last = store.kv_get(LAST_KEY).ok().flatten().and_then(|raw| serde_json::from_str(&raw).ok());
        Updates {
            build,
            auto: DeviceSwitch::load(store),
            state: State::Idle,
            last,
            schedule: Schedule::random(),
            worker: None,
            automatic: false,
            state_dir,
            code,
            wake: Arc::new(wake),
            drawn_at: None,
            confirm: Confirm::Waiting,
            kept: None,
            app: running_app(),
            spawn_helper: Box::new(spawn_helper),
            clock: || crossbow_core::now_ms() / 1000,
            show_notes: false,
            asking: None,
            notices: outcome.iter().cloned().collect(),
            outcome,
            reveal: false,
            mac: MacVersion::running(),
        }
    }

    /// The window drew a frame. The first one starts the wait to confirm a
    /// new version and the minute before automatic checks, and has shown
    /// the outcome loaded at start, which is then dropped.
    pub fn drawn(&mut self, now: Instant) {
        if self.drawn_at.is_none() {
            self.drawn_at = Some(now);
            install::clear_outcome(&self.state_dir);
        }
    }

    /// At a clean quit: a new version that drew a frame confirms itself.
    pub fn confirm_at_quit(&mut self) {
        if self.drawn_at.is_none() {
            return;
        }
        if matches!(self.confirm, Confirm::Waiting) {
            confirm_new_version(&self.state_dir, &self.instance());
        } else if let Confirm::Running(done) = &self.confirm {
            let _ = done.recv_timeout(Duration::from_secs(10));
        }
        self.confirm = Confirm::Done;
    }

    fn instance(&self) -> Instance {
        Instance { stamp: Stamp::running(), app: self.app.clone() }
    }

    /// Off the UI thread: deleting the old bundle can take a while.
    fn start_confirm(&mut self) {
        let (state_dir, me, wake) = (self.state_dir.clone(), self.instance(), self.wake.clone());
        let (done, receiver) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("crossbow-update-confirm".into()).spawn(move || {
            let _ = done.send(confirm_new_version(&state_dir, &me));
            wake();
        });
        self.confirm = if spawned.is_ok() { Confirm::Running(receiver) } else { Confirm::Done };
    }

    fn poll_confirm(&mut self) {
        let Confirm::Running(done) = &self.confirm else { return };
        let outcome = match done.try_recv() {
            Ok(outcome) => outcome,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        self.confirm = Confirm::Done;
        if let Some(outcome) = outcome {
            // Said now, so not again at the next start.
            install::clear_outcome(&self.state_dir);
            self.outcome = Some(outcome.text());
            self.notices.push(outcome.text());
        }
    }

    pub fn has_updater(&self) -> bool {
        self.build.is_some()
    }

    pub fn busy(&self) -> bool {
        matches!(self.state, State::Checking | State::Downloading { .. })
    }

    /// Check Now, or Crossbow → Check for Updates….
    pub fn check_now(&mut self) {
        self.start_check(false);
    }

    fn start_check(&mut self, automatic: bool) {
        let Some(build) = self.build else { return };
        if self.busy() || matches!(self.state, State::Waiting(_)) {
            return;
        }
        if self.worker.is_none() {
            let config = net::Config {
                url: build.url.into(),
                keys: build.keys(),
                running: Running::this(),
                mac: self.mac,
                state: self.state_dir.clone(),
                code: self.code.clone(),
            };
            let wake = self.wake.clone();
            match Worker::spawn(config, move || wake()) {
                Ok(worker) => self.worker = Some(worker),
                Err(_) => {
                    self.state = State::Failed("Crossbow couldn't start checking for updates. Try again.".into());
                    return;
                }
            }
        }
        if let Some(worker) = &self.worker {
            worker.send(Job::Check((self.clock)()));
        }
        if let State::Ready { ready, refusal } = &self.state {
            self.kept = Some((ready.clone(), refusal.clone()));
        }
        self.automatic = automatic;
        self.show_notes = false;
        self.state = State::Checking;
    }

    pub fn download(&mut self) {
        let State::Available(release) = &self.state else { return };
        let (release, progress) = (release.clone(), Arc::new(Progress::default()));
        let Some(worker) = &self.worker else { return };
        worker.send(Job::Download(release.clone(), progress.clone()));
        self.state = State::Downloading { release, progress };
    }

    pub fn cancel(&mut self) {
        match &self.state {
            State::Downloading { progress, .. } => progress.cancel(),
            State::Waiting(ready) => self.state = State::Ready { ready: ready.clone(), refusal: None },
            _ => {}
        }
        self.asking = None;
    }

    /// Each frame: read replies, confirm a new version once it has run a
    /// while, and start an automatic check when one is due, unless a
    /// verified download is ready to install.
    pub fn tick(&mut self, store: &Store, now_secs: u64, now: Instant, browser_downloads: usize) -> Option<Restart> {
        let drawn_for = self.drawn_at.map(|at| now.saturating_duration_since(at));
        if matches!(self.confirm, Confirm::Waiting) && drawn_for.is_some_and(|drawn_for| drawn_for >= CONFIRM_AFTER) {
            self.start_confirm();
        }
        self.poll_confirm();
        self.poll(store, now_secs);
        let on = self.has_updater() && self.auto.enabled();
        let held = self.busy() || matches!(self.state, State::Ready { .. } | State::Waiting(_));
        if self.schedule.due(now_secs, self.last.as_ref().map(|last| last.at), on, drawn_for, held) {
            self.start_check(true);
        }
        if matches!(self.state, State::Waiting(_)) && browser_downloads == 0 {
            return Some(self.restart(0, true));
        }
        None
    }

    pub fn poll(&mut self, store: &Store, now_secs: u64) {
        while let Some(reply) = self.worker.as_ref().and_then(Worker::try_recv) {
            match reply {
                Reply::Checked(result) => {
                    let text = check_text(&result, self.mac);
                    self.last = Some(LastCheck { at: now_secs, result: text.clone() });
                    if let Some(last) = &self.last
                        && let Ok(raw) = serde_json::to_string(last)
                    {
                        let _ = store.kv_set(LAST_KEY, &raw);
                    }
                    self.state = match (result, self.kept.take()) {
                        (Ok(Offer::Available(release)), Some((ready, refusal))) if !newer(&release, &ready.release) => State::Ready { ready, refusal },
                        (Ok(Offer::Available(release)), _) => State::Available(release),
                        // A ready download stays ready whatever the check says.
                        (_, Some((ready, refusal))) => State::Ready { ready, refusal },
                        (Ok(_), None) => State::Checked(text),
                        (Err(_), None) => State::Failed(text),
                    };
                    if self.automatic && self.auto.enabled() {
                        self.download();
                    }
                }
                Reply::Downloaded(Ok(ready)) => {
                    let refusal = refusal(&ready, self.app.as_deref());
                    self.state = State::Ready { ready, refusal };
                }
                Reply::Downloaded(Err(DownloadError::Cancelled)) => self.state = State::Checked("Download cancelled.".into()),
                Reply::Downloaded(Err(error)) => {
                    let subject = match &self.state {
                        State::Downloading { release, .. } => format!("Crossbow {}", release.version),
                        _ => "The update".into(),
                    };
                    self.state = State::Failed(download_text(&subject, &error));
                }
            }
        }
    }

    /// The group's main line.
    pub fn status(&self) -> String {
        if !self.has_updater() {
            return update::NO_UPDATER.into();
        }
        match &self.state {
            State::Idle => self.last.as_ref().map_or_else(|| "Crossbow hasn't checked for updates yet.".into(), |last| last.result.clone()),
            State::Checking => "Checking for updates…".into(),
            State::Checked(text) | State::Failed(text) => text.clone(),
            State::Available(release) => available_text(release),
            State::Downloading { release, progress } => {
                let (received, total) = (progress.bytes().0, release.archive.size.max(1));
                format!("Downloading Crossbow {}… {}% of {}.", release.version, received.min(total) * 100 / total, size_text(release.archive.size))
            }
            State::Ready { ready, refusal: None } => {
                let at_quit = if self.auto.enabled() { " It installs when you quit, or now with Restart to Update." } else { "" };
                format!("Crossbow {} is downloaded and verified.{at_quit}", ready.release.version)
            }
            State::Ready { ready, refusal: Some(_) } if ready.install == Err(CodeError::AdHoc) => format!(
                "Crossbow {} is downloaded. Its update signature and hash match, and the app is validly signed by Developer ID team {}, but this ad-hoc signed copy has no team of its own to match, so it won't install it. Show Download to install it yourself.",
                ready.release.version, ready.team
            ),
            State::Ready { ready, refusal: Some(why) } => format!("Crossbow {} is downloaded and verified, but it can't install itself: {why}. Show Download to install it yourself.", ready.release.version),
            State::Waiting(ready) => format!("Crossbow restarts to install {} when the downloads in progress finish.", ready.release.version),
        }
    }

    /// `Last checked Nov 1, 2026 at 11:23.`
    pub fn last_line(&self) -> Option<String> {
        let last = self.last.as_ref()?;
        Some(format!("Last checked {}.", crate::backups::date(last.at * 1000)))
    }

    /// Restart to Update: stage the verified app beside the installed one,
    /// start the helper and say to quit. With browser downloads in progress
    /// and no answer yet, ask instead.
    pub fn restart(&mut self, browser_downloads: usize, answered: bool) -> Restart {
        let ready = match &self.state {
            State::Ready { ready, refusal: None } | State::Waiting(ready) => ready.clone(),
            State::Ready { refusal: Some(why), .. } => return Restart::Refused(why.clone()),
            _ => return Restart::Refused("there is no verified update to install".into()),
        };
        if browser_downloads > 0 && !answered {
            self.asking = Some(browser_downloads);
            return Restart::Ask(browser_downloads);
        }
        self.asking = None;
        match self.install(&ready, true) {
            Ok(()) => Restart::Quit,
            Err(why) => {
                self.state = State::Ready { ready, refusal: Some(why.clone()) };
                Restart::Refused(why)
            }
        }
    }

    /// Wait for browser downloads, then restart.
    pub fn wait_for_downloads(&mut self) {
        if let State::Ready { ready, refusal: None } = &self.state {
            self.state = State::Waiting(ready.clone());
        }
        self.asking = None;
    }

    /// At quit, with the switch on and a verified update ready: install it
    /// unless browser downloads were in progress. Why it couldn't start is
    /// said at the next start.
    pub fn install_at_quit(&mut self, browser_downloads: usize) -> bool {
        let State::Ready { ready, refusal: None } = &self.state else { return false };
        if !self.auto.enabled() || browser_downloads > 0 {
            return false;
        }
        let ready = ready.clone();
        match self.install(&ready, false) {
            Ok(()) => true,
            Err(reason) => {
                let _ = install::record_outcome(&self.state_dir, &Outcome::Abandoned { to: ready.release.version.clone(), reason });
                false
            }
        }
    }

    fn install(&mut self, ready: &Ready, relaunch: bool) -> Result<(), String> {
        ready.install.clone().map_err(|error| error.to_string())?;
        let app = self.app.clone().ok_or_else(|| InstallError::NoBundle.to_string())?;
        let staged = install::Paths::beside(&app).staged;
        let journal = Journal {
            app,
            from: Stamp::running(),
            to: Stamp::of(&ready.bundle),
            pid: std::process::id(),
            relaunch,
            step: Step::Staged,
            launches: 0,
            reason: None,
        };
        install::stage(&self.state_dir, &ready.app, journal).map_err(|error| error.to_string())?;
        if let Err(error) = (self.spawn_helper)(&self.state_dir) {
            let _ = install::remove(&self.state_dir.join(install::JOURNAL));
            let _ = install::remove(&staged);
            return Err(format!("the installer couldn't start ({})", error.kind()));
        }
        self.state = State::Checked(format!("Crossbow {} installs as Crossbow {}.", ready.release.version, if relaunch { "restarts" } else { "quits" }));
        Ok(())
    }

    /// Show Download: the verified app in Finder, quarantined first, so
    /// Gatekeeper assesses it when the user opens it.
    pub fn reveal_download(&mut self) {
        let State::Ready { ready, .. } = &self.state else { return };
        match install::quarantine(&ready.app, (self.clock)()) {
            Ok(()) => {
                let _ = std::process::Command::new("open").arg("-R").arg(&ready.app).spawn();
            }
            Err(error) => self.state = State::Failed(format!("The download couldn't be marked for macOS to check when it opens ({}), so it wasn't shown.", error.kind())),
        }
    }
}

/// Whether `release` is a later build than `than`.
fn newer(release: &Release, than: &Release) -> bool {
    (release.version(), release.build) > (than.version(), than.build)
}

/// The installed app this process runs from.
fn running_app() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|exe| install::running_bundle(&exe))
}

/// Why a verified download can't install itself, if it can't.
fn refusal(ready: &Ready, app: Option<&Path>) -> Option<String> {
    if let Err(error) = &ready.install {
        return Some(error.to_string());
    }
    let app = match app {
        Some(app) => app,
        None => return Some(InstallError::NoBundle.to_string()),
    };
    install::can_install(app).err().map(|error| error.to_string())
}

/// One MB is 1,000,000 bytes, as Finder counts.
pub fn size_text(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

fn available_text(release: &Release) -> String {
    format!("Crossbow {} is available ({}).", release.version, size_text(release.archive.size))
}

/// What a check found, in words.
pub fn check_text(result: &Result<Offer, CheckError>, mac: Option<MacVersion>) -> String {
    use crossbow_core::update::feed::FeedError;
    match result {
        Ok(Offer::UpToDate) => format!("Crossbow {} is up to date.", update::VERSION),
        Ok(Offer::Available(release)) => available_text(release),
        Ok(Offer::Unsupported { version, needs }) => {
            let this = mac.map_or("this Mac's macOS can't be read".to_string(), |mac| format!("this Mac has macOS {mac}"));
            format!("Crossbow {version} needs macOS {needs} or later; {this}.")
        }
        Ok(Offer::Older(version)) => format!("The update server offers Crossbow {version}, older than this version, so nothing was installed."),
        Ok(Offer::Expired { days }) => {
            let ago = match days {
                0 => String::new(),
                1 => " a day ago".into(),
                days => format!(" {days} days ago"),
            };
            format!("The update server's feed expired{ago}, so Crossbow can't tell whether it is up to date. Try again later.")
        }
        Ok(Offer::Replayed { offered, newest }) => format!("The update server offers Crossbow {offered}, older than the {newest} it offered before, so Crossbow can't tell whether it is up to date. Nothing was installed."),
        Ok(Offer::RolledBack(version)) => format!("Crossbow {version} was rolled back on this Mac, so it isn't offered again. A later build will be."),
        Err(CheckError::Offline) => "Crossbow couldn't reach the update server. Check your connection and try again.".into(),
        Err(CheckError::Status(status)) => format!("The update server answered with an error (HTTP {status}). Try again later."),
        Err(CheckError::Reply) => "The update server's reply couldn't be read. Try again later.".into(),
        Err(CheckError::Feed(FeedError::Signature)) => "The update's signature didn't match this build's keys, so nothing was downloaded or installed.".into(),
        Err(CheckError::Feed(error)) => format!("The update server sent a feed Crossbow can't use: {error}."),
    }
}

/// Why a download was refused, naming the check; the download is gone.
pub fn download_text(subject: &str, error: &DownloadError) -> String {
    match error {
        DownloadError::Offline => format!("{subject} couldn't be downloaded: Crossbow couldn't reach the update server."),
        DownloadError::Status(_) | DownloadError::Reply | DownloadError::Disk(_) | DownloadError::Cancelled | DownloadError::TooSlow => format!("{subject} couldn't be downloaded: {error}."),
        _ => format!("{subject} was not installed: {error}. The download was deleted."),
    }
}

/// The new version has shown it works: drop the old bundle. How the
/// install ended, when this process is the version it installed.
fn confirm_new_version(state_dir: &Path, me: &Instance) -> Option<Outcome> {
    let mut ops = install::System::new(state_dir, Box::new(crossbow_core::update::codesign::System));
    install::confirm(state_dir, me, &mut ops).ok().flatten()
}

/// First thing in `main`, before the engine starts: count this start of a
/// new version, roll back one that keeps failing to start, and wait while
/// the helper installs. True when this process should quit: the old
/// version was put back and opened, the new one opened instead, or the
/// helper is still at work.
pub fn on_start(data_dir: &Path) -> bool {
    let state_dir = data_dir.join("updates");
    if !state_dir.join(install::JOURNAL).exists() {
        return false;
    }
    let me = Instance { stamp: Stamp::running(), app: running_app() };
    let mut ops = install::System::new(&state_dir, Box::new(crossbow_core::update::codesign::System));
    matches!(install::on_start(&state_dir, &me, &mut ops, install::START_WAIT), Ok(install::Start::Quit))
}

fn spawn_helper(state_dir: &Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let helper = std::env::current_exe()?.with_file_name(HELPER);
    Command::new(helper).arg(state_dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0).spawn().map(drop)
}

/// Crossbow → Check for Updates… in the app menu.
#[cfg(target_os = "macos")]
pub(crate) mod menu {
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{MainThreadMarker, class, msg_send, sel};
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, Ordering};

    static REQUESTED: AtomicBool = AtomicBool::new(false);

    thread_local! {
        static WAKE: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
    }

    /// `-crossbowCheckForUpdates:` added to winit's application delegate.
    extern "C-unwind" fn check_for_updates(_this: &AnyObject, _cmd: Sel, _sender: *mut AnyObject) {
        REQUESTED.store(true, Ordering::Release);
        if let Some(ctx) = WAKE.with(|wake| wake.borrow().clone()) {
            // AppKit may call this while egui holds its context lock.
            let _ = std::thread::Builder::new().name("crossbow-menu-wake".into()).spawn(move || ctx.request_repaint_of(egui::ViewportId::ROOT));
        }
    }

    /// Whether the menu item was chosen since the last call.
    pub(crate) fn take() -> bool {
        REQUESTED.swap(false, Ordering::AcqRel)
    }

    /// Right after the item titled About…, or first.
    pub(crate) fn position(titles: &[String]) -> isize {
        titles.iter().position(|title| title.starts_with("About ")).map_or(0, |about| about as isize + 1)
    }

    /// Add Check for Updates… after About in the app menu. Returns why it
    /// could not be added.
    pub(crate) fn install(ctx: &egui::Context) -> Result<(), String> {
        let main = MainThreadMarker::new().ok_or("not on the main thread")?;
        let app = NSApplication::sharedApplication(main);
        let delegate = app.delegate().ok_or("the application has no delegate")?;
        let class: &AnyClass = AsRef::<AnyObject>::as_ref(&*delegate).class();
        // SAFETY: The function matches `-(void)crossbowCheckForUpdates:(id)sender`
        // ("v@:@"); class_addMethod refuses a class that already has the method.
        let added = unsafe {
            let imp = std::mem::transmute::<extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject), Imp>(check_for_updates);
            objc2::ffi::class_addMethod(class as *const AnyClass as *mut AnyClass, sel!(crossbowCheckForUpdates:), imp, c"v@:@".as_ptr())
        };
        if !added.as_bool() {
            return Err(format!("{} already has a Check for Updates action", class.name().to_string_lossy()));
        }
        // SAFETY: AppKit's menu API on the main thread, with objects it returned.
        unsafe {
            let menu: Option<Retained<AnyObject>> = msg_send![&*app, mainMenu];
            let menu = menu.ok_or("the app has no menu bar")?;
            let first: Option<Retained<AnyObject>> = msg_send![&*menu, itemAtIndex: 0isize];
            let submenu: Option<Retained<AnyObject>> = msg_send![&*first.ok_or("the menu bar is empty")?, submenu];
            let submenu = submenu.ok_or("the app menu has no items")?;
            let count: isize = msg_send![&*submenu, numberOfItems];
            let titles: Vec<String> = (0..count)
                .map(|index| {
                    let item: Option<Retained<AnyObject>> = msg_send![&*submenu, itemAtIndex: index];
                    let title: Option<Retained<NSString>> = item.and_then(|item| msg_send![&*item, title]);
                    title.map_or_else(String::new, |title| title.to_string())
                })
                .collect();
            let title = NSString::from_str(super::MENU_TITLE);
            let none = NSString::from_str("");
            let item: Allocated<AnyObject> = msg_send![class!(NSMenuItem), alloc];
            let item: Retained<AnyObject> = msg_send![item, initWithTitle: &*title, action: sel!(crossbowCheckForUpdates:), keyEquivalent: &*none];
            let _: () = msg_send![&*item, setTarget: &*delegate];
            let _: () = msg_send![&*submenu, insertItem: &*item, atIndex: position(&titles)];
        }
        WAKE.with(|wake| *wake.borrow_mut() = Some(ctx.clone()));
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_menu_item_asks_for_one_check_and_sits_right_after_about() {
            let this = objc2::runtime::NSObject::new();
            take();
            assert!(!take());
            check_for_updates(&this, sel!(crossbowCheckForUpdates:), std::ptr::null_mut());
            assert!(take(), "choosing the item asks for a check");
            assert!(!take(), "once");
            let titles = |names: &[&str]| names.iter().map(|name| name.to_string()).collect::<Vec<_>>();
            assert_eq!(position(&titles(&["About Crossbow", "", "New Little Window", "", "Settings…"])), 1);
            assert_eq!(position(&titles(&["Settings…"])), 0);
            assert_eq!(super::super::MENU_TITLE, "Check for Updates…");
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) mod menu {
    pub(crate) fn take() -> bool {
        false
    }

    pub(crate) fn install(_ctx: &egui::Context) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "updates_tests.rs"]
pub(crate) mod tests;
