//! Installing a verified update, in steps a crash can interrupt anywhere.
//!
//! The app copies the verified bundle beside the installed one
//! (`.Crossbow.app.update`) and writes a journal. A small helper,
//! `crossbow-update` in `Contents/MacOS`, waits for the app to quit, swaps
//! the two bundles with one `renamex_np(RENAME_SWAP)`, keeps the old one as
//! `.Crossbow.app.previous`, checks the new one's signature again where it
//! now is and runs its executable once with `--update-probe`; when that
//! fails it swaps back. The new app deletes the old bundle once it has
//! drawn its window and stayed up, and a new app that starts twice without
//! getting there is rolled back at its third start. Each step reads the
//! journal and the bundles' own `Info.plist`, so a crash at any point
//! resumes or undoes.
//!
//! Nothing is moved into the app's path unless its signature meets the
//! Developer ID requirement for the Team ID of the process moving it, read
//! from that process's own signature, never from the journal; an ad-hoc
//! signed copy moves nothing. Only the copy at the journal's path acts on
//! it, and while the helper works another copy that starts waits, then
//! quits, rather than run beside the install.

use super::archive;
use super::MacVersion;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const STAGED: &str = ".Crossbow.app.update";
pub const PREVIOUS: &str = ".Crossbow.app.previous";
/// Two empty folders swapped once to learn whether the volume can swap.
const SWAP_TEST: [&str; 2] = [".Crossbow.app.swaptest", ".Crossbow.app.swaptest2"];
pub const JOURNAL: &str = "install.json";
pub const OUTCOME: &str = "outcome.json";
/// Releases rolled back on this Mac, which are never offered again.
pub const ROLLED_BACK: &str = "rolled_back.json";
const MAX_ROLLED_BACK: usize = 32;
const LOCK: &str = "install.lock";
/// How long a starting app waits for the helper to finish.
pub const START_WAIT: Duration = Duration::from_secs(90);
/// macOS's admin group, which may write /Applications.
const ADMIN_GROUP: u32 = 80;
const PROBE_MARKER: &str = "probe";
pub const PROBE_ARG: &str = "--update-probe";
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// Starts of the new app that never became interactive before it is rolled back.
pub const MAX_UNCONFIRMED_LAUNCHES: u32 = 2;

/// A bundle's version and build.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    pub version: String,
    pub build: u64,
}

impl Stamp {
    pub fn of(bundle: &archive::Bundle) -> Stamp {
        Stamp { version: bundle.version.to_string(), build: bundle.build }
    }

    /// The app this process is.
    pub fn running() -> Stamp {
        Stamp { version: super::VERSION.into(), build: super::BUILD_NUMBER }
    }

    /// What the bundle at `app` says it is.
    pub fn at(app: &Path) -> Option<Stamp> {
        archive::read_info(app).ok().map(|bundle| Stamp::of(&bundle))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// The new bundle is staged; the installed app is untouched.
    Staged,
    /// About to swap; either bundle may be at the app's path.
    Swapping,
    /// The new bundle is at the app's path and about to be probed.
    Swapped,
    /// The new bundle started once; it confirms when interactive.
    Probed,
    /// Going back to the old bundle.
    RollingBack,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    /// The installed app's path.
    pub app: PathBuf,
    pub from: Stamp,
    pub to: Stamp,
    /// The app process to wait for.
    pub pid: u32,
    /// Open the app again when done.
    pub relaunch: bool,
    pub step: Step,
    /// Starts of the new bundle not yet confirmed.
    pub launches: u32,
    pub reason: Option<String>,
}

/// How the last install ended, for the app to say once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Updated { to: String },
    RolledBack { to: String, kept: String, reason: String },
    Abandoned { to: String, reason: String },
}

impl Outcome {
    pub fn text(&self) -> String {
        match self {
            Outcome::Updated { to } => format!("Crossbow was updated to {to}."),
            Outcome::RolledBack { to, kept, reason } => format!("Crossbow {to} {reason}, so Crossbow went back to {kept}."),
            Outcome::Abandoned { to, reason } => format!("The update to {to} was not installed: {reason}."),
        }
    }
}

/// Why an install can't start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    #[error("Crossbow isn't running from an app bundle")]
    NoBundle,
    #[error("macOS is running Crossbow from a temporary read-only copy; move Crossbow to Applications first")]
    Translocated,
    #[error("you can't write to the folder that holds Crossbow")]
    NotWritable,
    #[error("other users can change the folder that holds Crossbow")]
    Shared,
    #[error("Crossbow is opened through a symlink")]
    Symlink,
    #[error("another update is being installed")]
    Busy,
    #[error("the disk that holds Crossbow can't swap apps in one step")]
    NoSwap,
    #[error("the update couldn't be staged ({0})")]
    Io(io::ErrorKind),
}

impl From<io::Error> for InstallError {
    fn from(error: io::Error) -> Self {
        InstallError::Io(error.kind())
    }
}

/// The folders an install uses, all beside the installed app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub app: PathBuf,
    pub staged: PathBuf,
    pub previous: PathBuf,
}

impl Paths {
    pub fn beside(app: &Path) -> Paths {
        let sibling = |name: &str| app.with_file_name(name);
        Paths { app: app.to_path_buf(), staged: sibling(STAGED), previous: sibling(PREVIOUS) }
    }
}

/// This process: what it is, and the bundle it runs from, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    pub stamp: Stamp,
    pub app: Option<PathBuf>,
}

impl Instance {
    /// Whether this process runs from the app the journal installs into.
    fn owns(&self, journal: &Journal) -> bool {
        self.app.as_deref().is_some_and(|app| same_path(app, &journal.app))
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    real(a) == real(b)
}

/// Every change an install makes, so tests can stop at any one of them.
pub trait Ops {
    /// Exchange two folders in one step.
    fn swap(&mut self, a: &Path, b: &Path) -> io::Result<()>;
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()>;
    /// Delete a folder or file; nothing to delete is fine.
    fn remove(&mut self, path: &Path) -> io::Result<()>;
    /// Replace a file in one step.
    fn write(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    /// Whether the bundle may go at the app's path: it runs on this macOS
    /// and is validly signed under a Developer ID for the Team ID in this
    /// process's own signature. Never for an ad-hoc signed process.
    fn verify(&mut self, app: &Path) -> bool;
    /// Whether another copy of the app runs from `app`.
    fn in_use(&mut self, app: &Path) -> bool;
    /// Run the bundle's executable with [`PROBE_ARG`]; what it says it is.
    fn probe(&mut self, app: &Path) -> Option<Stamp>;
    /// Open the app, as a new instance.
    fn launch(&mut self, app: &Path) -> io::Result<()>;
}

/// The installed app holding the running executable, if it runs from one.
pub fn running_bundle(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let is_bundle = macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && app.extension()? == "app";
    is_bundle.then(|| app.to_path_buf())
}

/// Whether this user may replace the bundle at `app`, in a folder no other
/// user can change.
pub fn writable(app: &Path) -> Result<(), InstallError> {
    use std::os::unix::fs::MetadataExt;
    if app.components().any(|component| component.as_os_str() == "AppTranslocation") {
        return Err(InstallError::Translocated);
    }
    let meta = fs::symlink_metadata(app).map_err(|_| InstallError::NoBundle)?;
    if meta.file_type().is_symlink() {
        return Err(InstallError::Symlink);
    }
    let parent = app.parent().ok_or(InstallError::NoBundle)?;
    let folder = fs::metadata(parent).map_err(|_| InstallError::NoBundle)?;
    // SAFETY: geteuid has no preconditions.
    if shared(folder.uid(), folder.gid(), folder.mode(), unsafe { libc::geteuid() }) {
        return Err(InstallError::Shared);
    }
    let may = |path: &Path, mode: libc::c_int| {
        let Ok(path) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else { return false };
        // SAFETY: a NUL-terminated path that outlives the call.
        unsafe { libc::access(path.as_ptr(), mode) == 0 }
    };
    let all = libc::W_OK | libc::X_OK;
    if !may(parent, all) || !may(app, all) || !may(&app.join("Contents"), all) {
        return Err(InstallError::NotWritable);
    }
    Ok(())
}

/// Whether users other than `me` can change a folder with this owner,
/// group and mode: it belongs to someone else (root aside), everyone may
/// write it, or a group other than admin may (/Applications is root:admin
/// 0775, and admins can change anything anyway).
pub fn shared(owner: u32, group: u32, mode: u32, me: u32) -> bool {
    (owner != me && owner != 0) || mode & 0o002 != 0 || (mode & 0o020 != 0 && group != ADMIN_GROUP)
}

/// Whether the update can install itself into `app`: [`writable`], on a
/// volume that swaps folders in one step.
pub fn can_install(app: &Path) -> Result<(), InstallError> {
    writable(app)?;
    can_swap(&Paths::beside(app))
}

/// The journal, when an install is under way.
pub fn read_journal(state: &Path) -> Option<Journal> {
    serde_json::from_slice(&fs::read(state.join(JOURNAL)).ok()?).ok()
}

/// How the last install ended, until [`clear_outcome`].
pub fn read_outcome(state: &Path) -> Option<Outcome> {
    serde_json::from_slice(&fs::read(state.join(OUTCOME)).ok()?).ok()
}

/// The outcome has been shown.
pub fn clear_outcome(state: &Path) {
    let _ = fs::remove_file(state.join(OUTCOME));
}

/// How the last install ended, read once.
pub fn take_outcome(state: &Path) -> Option<Outcome> {
    let outcome = read_outcome(state);
    clear_outcome(state);
    outcome
}

/// Keep `outcome` for the app to show at its next start, as when an install
/// at quit could not start.
pub fn record_outcome(state: &Path, outcome: &Outcome) -> io::Result<()> {
    super::net::private_folder(state)?;
    write_atomic(&state.join(OUTCOME), &serde_json::to_vec(outcome).map_err(io::Error::other)?)
}

/// Releases rolled back on this Mac, oldest first.
pub fn rolled_back(state: &Path) -> Vec<Stamp> {
    fs::read(state.join(ROLLED_BACK)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

fn remember_rolled_back(state: &Path, ops: &mut dyn Ops, stamp: &Stamp) -> io::Result<()> {
    let mut list = rolled_back(state);
    if list.contains(stamp) {
        return Ok(());
    }
    list.push(stamp.clone());
    let keep = &list[list.len().saturating_sub(MAX_ROLLED_BACK)..];
    ops.write(&state.join(ROLLED_BACK), &serde_json::to_vec(keep).map_err(io::Error::other)?)
}

fn save(state: &Path, ops: &mut dyn Ops, journal: &Journal) -> io::Result<()> {
    ops.write(&state.join(JOURNAL), &serde_json::to_vec_pretty(journal).map_err(io::Error::other)?)
}

/// Record the outcome, then drop the journal: a crash between the two
/// repeats the last step, which changes nothing the second time.
fn end(state: &Path, ops: &mut dyn Ops, outcome: &Outcome) -> io::Result<()> {
    ops.write(&state.join(OUTCOME), &serde_json::to_vec(outcome).map_err(io::Error::other)?)?;
    ops.remove(&state.join(JOURNAL))
}

/// App side: copy the verified `new_app` beside the installed app and write
/// the journal at [`Step::Staged`]. Nothing at the app's path changes.
pub fn stage(state: &Path, new_app: &Path, mut journal: Journal) -> Result<(), InstallError> {
    writable(&journal.app)?;
    super::net::private_folder(state)?;
    let lock = Lock::take(state)?.ok_or(InstallError::Busy)?;
    if read_journal(state).is_some() {
        return Err(InstallError::Busy);
    }
    let paths = Paths::beside(&journal.app);
    can_swap(&paths)?;
    remove(&paths.staged)?;
    if let Err(error) = copy_tree(new_app, &paths.staged) {
        let _ = remove(&paths.staged);
        return Err(error.into());
    }
    strip_quarantine(&paths.staged);
    journal.step = Step::Staged;
    journal.launches = 0;
    journal.reason = None;
    write_atomic(&state.join(JOURNAL), &serde_json::to_vec_pretty(&journal).map_err(io::Error::other)?)?;
    drop(lock);
    Ok(())
}

/// Whether the volume holding the app swaps two folders in one step:
/// tried on two empty folders beside it.
fn can_swap(paths: &Paths) -> Result<(), InstallError> {
    let (a, b) = (paths.app.with_file_name(SWAP_TEST[0]), paths.app.with_file_name(SWAP_TEST[1]));
    remove(&a)?;
    remove(&b)?;
    fs::create_dir(&a)?;
    fs::create_dir(&b)?;
    let swapped = swap(&a, &b);
    remove(&a)?;
    remove(&b)?;
    match swapped {
        Err(error) if matches!(error.raw_os_error(), Some(libc::ENOTSUP | libc::EINVAL)) => Err(InstallError::NoSwap),
        other => Ok(other?),
    }
}

/// Where the helper's run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finished {
    /// No install under way, or another helper is at it.
    Nothing,
    /// The app didn't quit in time; nothing changed.
    NotQuit,
    /// The new app is in place and started once; it confirms itself.
    Probed,
    Done(Outcome),
}

/// The helper's whole run: hold the lock, wait (with `quit`) for the app to
/// end, carry the install through, then open the new app when asked.
/// `here` is the bundle the helper runs from; it carries out only that
/// app's install.
pub fn helper(state: &Path, here: &Path, ops: &mut dyn Ops, quit: impl FnOnce(u32) -> bool) -> io::Result<Finished> {
    let (finished, journal) = {
        let Some(_lock) = Lock::take(state)? else { return Ok(Finished::Nothing) };
        let Some(journal) = read_journal(state).filter(|journal| same_path(here, &journal.app)) else { return Ok(Finished::Nothing) };
        if journal.step == Step::Staged && !quit(journal.pid) {
            return Ok(Finished::NotQuit);
        }
        (finish(state, ops)?, journal)
    };
    if finished == Finished::Probed && journal.relaunch {
        ops.launch(&journal.app)?;
    }
    Ok(finished)
}

/// Carry the journal forward from whatever step it reached.
pub fn finish(state: &Path, ops: &mut dyn Ops) -> io::Result<Finished> {
    loop {
        let Some(mut journal) = read_journal(state) else { return Ok(Finished::Nothing) };
        let paths = Paths::beside(&journal.app);
        match journal.step {
            Step::Staged => {
                if ops.in_use(&journal.app) {
                    // That copy is starting, or running: it keeps the old app.
                    journal.relaunch = false;
                    return abandon(state, ops, &journal, &paths, "Crossbow was opened again while it was being updated").map(Finished::Done);
                }
                journal.step = Step::Swapping;
                save(state, ops, &journal)?;
            }
            Step::Swapping | Step::Swapped => {
                match place(ops, &paths, &journal.to) {
                    Ok(Placed::Done) => {}
                    Ok(Placed::Missing) => return abandon(state, ops, &journal, &paths, "the staged app went missing").map(Finished::Done),
                    Ok(Placed::Unverified) => return abandon(state, ops, &journal, &paths, "the staged app failed its signature check").map(Finished::Done),
                    Err(error) if refused(&error) => return abandon(state, ops, &journal, &paths, "macOS refused to replace the app").map(Finished::Done),
                    Err(error) => return Err(error),
                }
                keep_previous(ops, &paths, &journal.from)?;
                journal.step = Step::Swapped;
                save(state, ops, &journal)?;
                // Checked again where it now is: the folder it came from
                // could have changed between the first check and the swap.
                let reason = if !ops.verify(&paths.app) {
                    "failed its signature check"
                } else if ops.probe(&paths.app).as_ref() != Some(&journal.to) {
                    "didn't start"
                } else {
                    journal.step = Step::Probed;
                    save(state, ops, &journal)?;
                    return Ok(Finished::Probed);
                };
                journal.step = Step::RollingBack;
                journal.reason = Some(reason.into());
                save(state, ops, &journal)?;
            }
            Step::Probed => return Ok(Finished::Probed),
            Step::RollingBack => return roll_back(state, ops, &journal, &paths).map(Finished::Done),
        }
    }
}

/// An error macOS gives for a change it won't make, as opposed to one cut
/// short.
fn refused(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EPERM | libc::EACCES | libc::ENOTSUP | libc::EINVAL | libc::EXDEV | libc::EROFS))
}

/// What the app should do at start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Continue,
    /// The old app was put back and opened; this one should quit.
    Quit,
}

/// App side, first thing at start: count a start of the new bundle, roll
/// back one that keeps failing to start, and settle an install the helper
/// left half done or another version replaced. While the helper holds the
/// lock it waits up to `patience`, then quits rather than run beside the
/// install; a copy whose bundle the helper replaced meanwhile quits into
/// the new one. Only the copy at the journal's path changes anything.
pub fn on_start(state: &Path, me: &Instance, ops: &mut dyn Ops, patience: Duration) -> io::Result<Start> {
    let started = Instant::now();
    let _lock = loop {
        if let Some(lock) = Lock::take(state)? {
            break lock;
        }
        if started.elapsed() >= patience {
            return Ok(Start::Quit);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let Some(mut journal) = read_journal(state).filter(|journal| me.owns(journal)) else { return Ok(Start::Continue) };
    let paths = Paths::beside(&journal.app);
    let is_new = me.stamp == journal.to;
    if journal.step == Step::RollingBack {
        journal.relaunch = false;
        let outcome = roll_back(state, ops, &journal, &paths)?;
        return quit_into_old(ops, &paths, is_new && matches!(outcome, Outcome::RolledBack { .. }));
    }
    if is_new {
        // This process is the new bundle, so the swap happened.
        if journal.step != Step::Probed {
            place(ops, &paths, &journal.to)?;
            keep_previous(ops, &paths, &journal.from)?;
            journal.step = Step::Probed;
        }
        journal.launches += 1;
        if journal.launches > MAX_UNCONFIRMED_LAUNCHES {
            journal.step = Step::RollingBack;
            journal.reason = Some("didn't finish starting".into());
            journal.relaunch = false;
            save(state, ops, &journal)?;
            let outcome = roll_back(state, ops, &journal, &paths)?;
            return quit_into_old(ops, &paths, matches!(outcome, Outcome::RolledBack { .. }));
        }
        save(state, ops, &journal)?;
    } else if me.stamp == journal.from && matches!(journal.step, Step::Staged | Step::Swapping) {
        journal.relaunch = false;
        abandon(state, ops, &journal, &paths, "it was interrupted")?;
    } else if me.stamp != journal.from {
        // Another version was put at the app's path since: it stays.
        for leftover in [&paths.staged, &paths.previous] {
            ops.remove(leftover)?;
        }
        end(state, ops, &Outcome::Abandoned { to: journal.to.version.clone(), reason: format!("Crossbow {} replaced it", me.stamp.version) })?;
    } else if Stamp::at(&paths.app).is_some_and(|there| there != me.stamp) {
        // The old version, started while the helper swapped the new one
        // in: the new one opens instead (the helper opens it when asked to).
        if !journal.relaunch {
            ops.launch(&paths.app)?;
        }
        return Ok(Start::Quit);
    }
    Ok(Start::Continue)
}

fn quit_into_old(ops: &mut dyn Ops, paths: &Paths, is_new: bool) -> io::Result<Start> {
    if !is_new {
        return Ok(Start::Continue);
    }
    ops.launch(&paths.app)?;
    Ok(Start::Quit)
}

/// App side, once the new bundle has shown it works (see the app's rule):
/// delete the old one.
pub fn confirm(state: &Path, me: &Instance, ops: &mut dyn Ops) -> io::Result<Option<Outcome>> {
    let Some(_lock) = Lock::take(state)? else { return Ok(None) };
    let Some(journal) = read_journal(state) else { return Ok(None) };
    if journal.step != Step::Probed || me.stamp != journal.to || !me.owns(&journal) {
        return Ok(None);
    }
    let paths = Paths::beside(&journal.app);
    ops.remove(&paths.previous)?;
    let outcome = Outcome::Updated { to: journal.to.version.clone() };
    end(state, ops, &outcome)?;
    Ok(Some(outcome))
}

/// What [`place`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placed {
    Done,
    /// Neither folder holds the version asked for.
    Missing,
    /// One does, but it failed [`Ops::verify`]; nothing moved.
    Unverified,
}

/// Make the app's path hold `want`, from the staged or previous folder,
/// once that bundle passes [`Ops::verify`].
fn place(ops: &mut dyn Ops, paths: &Paths, want: &Stamp) -> io::Result<Placed> {
    if Stamp::at(&paths.app).as_ref() == Some(want) {
        return Ok(Placed::Done);
    }
    let mut found = Placed::Missing;
    for source in [&paths.staged, &paths.previous] {
        if Stamp::at(source).as_ref() != Some(want) {
            continue;
        }
        if !ops.verify(source) {
            found = Placed::Unverified;
            continue;
        }
        if fs::symlink_metadata(&paths.app).is_err() {
            ops.rename(source, &paths.app)?;
        } else {
            ops.swap(source, &paths.app)?;
        }
        return Ok(Placed::Done);
    }
    Ok(found)
}

/// Keep the old bundle (`from`) as the previous one and drop what else is
/// left in the staged folder. After the swap the staged folder holds what
/// was at the app's path, so that is kept over any other previous bundle.
fn keep_previous(ops: &mut dyn Ops, paths: &Paths, from: &Stamp) -> io::Result<()> {
    if Stamp::at(&paths.staged).as_ref() == Some(from) {
        ops.remove(&paths.previous)?;
        ops.rename(&paths.staged, &paths.previous)?;
    }
    ops.remove(&paths.staged)
}

fn roll_back(state: &Path, ops: &mut dyn Ops, journal: &Journal, paths: &Paths) -> io::Result<Outcome> {
    let reason = journal.reason.clone().unwrap_or_else(|| "didn't start".into());
    let missing = match place(ops, paths, &journal.from)? {
        Placed::Done => None,
        Placed::Missing => Some("the previous version was missing"),
        Placed::Unverified => Some("the previous version failed its signature check"),
    };
    // Whatever else is left beside a whole app goes, a bundle that failed
    // its check included.
    if Stamp::at(&paths.app).is_some() {
        for leftover in [&paths.staged, &paths.previous] {
            ops.remove(leftover)?;
        }
    }
    if let Some(missing) = missing {
        // Nothing to go back to: the new bundle stays.
        let outcome = Outcome::Abandoned { to: journal.to.version.clone(), reason: format!("it {reason} and {missing}") };
        end(state, ops, &outcome)?;
        return Ok(outcome);
    }
    remember_rolled_back(state, ops, &journal.to)?;
    let outcome = Outcome::RolledBack { to: journal.to.version.clone(), kept: journal.from.version.clone(), reason };
    end(state, ops, &outcome)?;
    if journal.relaunch {
        let _ = ops.launch(&paths.app);
    }
    Ok(outcome)
}

fn abandon(state: &Path, ops: &mut dyn Ops, journal: &Journal, paths: &Paths, reason: &str) -> io::Result<Outcome> {
    place(ops, paths, &journal.from)?;
    ops.remove(&paths.staged)?;
    let outcome = Outcome::Abandoned { to: journal.to.version.clone(), reason: reason.into() };
    end(state, ops, &outcome)?;
    if journal.relaunch {
        let _ = ops.launch(&paths.app);
    }
    Ok(outcome)
}

/// An exclusive hold on the install, released when dropped.
pub struct Lock {
    _file: fs::File,
}

impl Lock {
    /// None while another process holds it.
    pub fn take(state: &Path) -> io::Result<Option<Lock>> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(state.join(LOCK))?;
        // SAFETY: flock on a descriptor this function owns.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::EWOULDBLOCK) { Ok(None) } else { Err(error) };
        }
        Ok(Some(Lock { _file: file }))
    }
}

/// The real file system, Security framework and launcher.
pub struct System {
    state: PathBuf,
    code: Box<dyn super::codesign::CodeCheck>,
    /// This process's Team ID, from its own signature.
    team: Option<String>,
    mac: Option<MacVersion>,
}

impl System {
    pub fn new(state: &Path, code: Box<dyn super::codesign::CodeCheck>) -> System {
        let team = code.running_team();
        System { state: state.to_path_buf(), code, team, mac: MacVersion::running() }
    }
}

impl Ops for System {
    fn swap(&mut self, a: &Path, b: &Path) -> io::Result<()> {
        swap(a, b)
    }

    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn remove(&mut self, path: &Path) -> io::Result<()> {
        remove(path)
    }

    fn write(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        write_atomic(path, bytes)
    }

    fn verify(&mut self, app: &Path) -> bool {
        // A requirement check validates the whole signature too.
        self.team.as_deref().is_some_and(|team| runs_here(app, self.mac) && self.code.meets(app, team))
    }

    fn in_use(&mut self, app: &Path) -> bool {
        in_use(app)
    }

    fn probe(&mut self, app: &Path) -> Option<Stamp> {
        probe(app, &self.state.join(PROBE_MARKER), PROBE_TIMEOUT)
    }

    fn launch(&mut self, app: &Path) -> io::Result<()> {
        Command::new("/usr/bin/open").arg("-n").arg(app).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(drop)
    }
}

/// Whether the bundle at `app` runs on macOS `mac`.
pub fn runs_here(app: &Path, mac: Option<MacVersion>) -> bool {
    archive::read_info(app).is_ok_and(|bundle| mac.is_some_and(|mac| bundle.minimum_macos <= mac))
}

/// Whether a process other than this one runs an executable straight from
/// `app/Contents/MacOS`: another copy of the app, not its engine helpers.
pub fn in_use(app: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let macos = real(&app.join("Contents/MacOS"));
        let me = std::process::id() as libc::c_int;
        // SAFETY: a null buffer asks only for the count.
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if count <= 0 {
            return false;
        }
        let mut pids = vec![0 as libc::c_int; count as usize + 64];
        let bytes = (pids.len() * std::mem::size_of::<libc::c_int>()) as libc::c_int;
        // SAFETY: the buffer holds `bytes` bytes.
        let listed = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        pids.truncate(listed.max(0) as usize);
        pids.into_iter().filter(|&pid| pid > 0 && pid != me).any(|pid| {
            let mut buffer = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
            // SAFETY: proc_pidpath writes at most the buffer's size.
            let len = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
            len > 0 && Path::new(std::ffi::OsStr::from_bytes(&buffer[..len as usize])).parent() == Some(macos.as_path())
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        false
    }
}

fn real(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `renamex_np(RENAME_SWAP)`: both paths stay occupied throughout.
pub fn swap(a: &Path, b: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let path = |p: &Path| std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).map_err(io::Error::other);
        let (a, b) = (path(a)?, path(b)?);
        // SAFETY: two NUL-terminated paths that outlive the call.
        if unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (a, b);
        Err(io::Error::from_raw_os_error(libc::ENOTSUP))
    }
}

/// Delete a folder or file; nothing there is fine.
pub fn remove(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
    }
}

/// Write beside `path`, flush, then rename over it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let temp = path.with_file_name(name);
    let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temp, path)
}

/// Copy a bundle, keeping symlinks as symlinks and file modes.
pub fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&source)?, &target)?;
        } else if kind.is_dir() {
            copy_tree(&source, &target)?;
        } else {
            fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

/// Clear `com.apple.quarantine` from every item: the app quarantines what
/// it writes, and this bundle's signature was checked here instead.
pub fn strip_quarantine(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        if let Ok(name) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) {
            // SAFETY: NUL-terminated strings that outlive the call.
            unsafe { libc::removexattr(name.as_ptr(), c"com.apple.quarantine".as_ptr(), libc::XATTR_NOFOLLOW) };
        }
    }
    if fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
        && let Ok(entries) = fs::read_dir(path)
    {
        for entry in entries.flatten() {
            strip_quarantine(&entry.path());
        }
    }
}

/// Mark the bundle at `path` as downloaded at `now`, as a browser would, so
/// Gatekeeper assesses it when the user first opens it themselves.
pub fn quarantine(path: &Path, now: u64) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(io::Error::other)?;
        let value = format!("0001;{now:08x};Crossbow;");
        // SAFETY: NUL-terminated strings and a buffer of the given length, all outliving the call.
        if unsafe { libc::setxattr(name.as_ptr(), c"com.apple.quarantine".as_ptr(), value.as_ptr().cast(), value.len(), 0, libc::XATTR_NOFOLLOW) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (path, now);
    Ok(())
}

/// Run the bundle's executable with [`PROBE_ARG`] and read what it wrote,
/// giving up after `timeout`.
pub fn probe(app: &Path, marker: &Path, timeout: Duration) -> Option<Stamp> {
    let bundle = archive::read_info(app).ok()?;
    let _ = fs::remove_file(marker);
    let mut child = Command::new(app.join("Contents/MacOS").join(&bundle.executable))
        .arg(PROBE_ARG)
        .arg(marker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().ok()? {
            break status;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(marker);
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let text = fs::read_to_string(marker).ok();
    let _ = fs::remove_file(marker);
    let (version, build) = text?.trim_end().split_once(' ').map(|(v, b)| (v.to_owned(), b.parse().ok()))?;
    status.success().then_some(Stamp { version, build: build? })
}

/// The new app's half of the probe, first thing in `main`: with
/// `--update-probe <file>`, write this build's version and build number to
/// a new file there and say which exit code to end with.
pub fn answer_probe(args: &[OsString]) -> Option<i32> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let [_, flag, marker] = args else { return None };
    if flag != PROBE_ARG {
        return None;
    }
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(marker)
        .and_then(|mut file| writeln!(file, "{} {}", super::VERSION, super::BUILD_NUMBER));
    Some(if written.is_ok() { 0 } else { 1 })
}

/// Whether process `pid` has ended, waiting up to `timeout`.
pub fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let started = Instant::now();
    loop {
        // SAFETY: signal 0 only checks that the process exists.
        let gone = unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if gone {
            return true;
        }
        if started.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
