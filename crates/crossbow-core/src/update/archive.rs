//! Unpacking an update: a `.tar.gz` of `Crossbow.app` and nothing else.
//!
//! Each entry must sit inside `Crossbow.app` (a leading `./` is allowed):
//! absolute paths, `..`, hard links, devices, duplicates, macOS `._` metadata
//! files and writes through an earlier symlink are refused. A symlink's
//! target is relative, climbs only with leading `..` and never above the
//! bundle, so it resolves inside it even through other symlinks. Entries
//! (every header counts), each file, the total and everything decompressed
//! are size-limited. Modes are reduced to 0644 or
//! 0755; owners, times and extended attributes are not restored.

use super::{MacVersion, Version};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

pub const APP: &str = "Crossbow.app";
pub const BUNDLE_ID: &str = "app.crossbow.Crossbow";
pub const MAX_ENTRIES: usize = 100_000;
pub const MAX_FILE: u64 = 1 << 30;
pub const MAX_UNPACKED: u64 = 4 << 30;
/// Decompressed bytes in all: the files, plus a header and padding for
/// every entry. Long names and PAX headers count too, so no archive costs
/// more than this to read.
pub const MAX_STREAM: u64 = MAX_UNPACKED + MAX_ENTRIES as u64 * 1024;
const MAX_INFO_PLIST: u64 = 1 << 20;

/// Why an archive was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ArchiveError {
    #[error("it is not a .tar.gz")]
    Format,
    #[error("an entry is outside Crossbow.app")]
    Outside,
    #[error("an entry's path is absolute or climbs out with ..")]
    Parent,
    #[error("a symlink points outside Crossbow.app")]
    Symlink,
    #[error("an entry is written through a symlink")]
    ThroughSymlink,
    #[error("an entry is a hard link, device or other special file")]
    Special,
    #[error("an entry appears twice")]
    Duplicate,
    #[error("it holds macOS metadata (._ files); make it with COPYFILE_DISABLE=1")]
    AppleDouble,
    #[error("it exceeds the size limits")]
    TooLarge,
    #[error("Crossbow.app has no valid Info.plist")]
    InfoPlist,
    #[error("the app in it is not Crossbow")]
    Identity,
    #[error("unpacking failed ({0})")]
    Io(io::ErrorKind),
}

impl From<io::Error> for ArchiveError {
    fn from(error: io::Error) -> Self {
        ArchiveError::Io(error.kind())
    }
}

/// What a bundle's `Info.plist` says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundle {
    pub version: Version,
    pub build: u64,
    pub minimum_macos: MacVersion,
    /// `CFBundleExecutable`: the file in `Contents/MacOS` to run.
    pub executable: String,
}

/// Unpack `archive` into the empty folder `into`, as `into/Crossbow.app`,
/// and read its `Info.plist`. On failure nothing unpacked is left behind.
pub fn unpack(archive: &Path, into: &Path) -> Result<Bundle, ArchiveError> {
    let app = into.join(APP);
    let result = fs::File::open(archive).map_err(ArchiveError::from).and_then(|file| unpack_from(file, &app, MAX_STREAM)).and_then(|()| read_info(&app));
    if result.is_err() {
        let _ = fs::remove_dir_all(&app);
    }
    result
}

/// Reads at most `left` bytes, then fails and says so in `over`.
struct Capped<R> {
    inner: R,
    left: u64,
    over: std::rc::Rc<std::cell::Cell<bool>>,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        if read as u64 > self.left {
            self.over.set(true);
            return Err(io::Error::other("the archive is larger than its limit"));
        }
        self.left -= read as u64;
        Ok(read)
    }
}

fn unpack_from(file: fs::File, app: &Path, max_stream: u64) -> Result<(), ArchiveError> {
    let over = std::rc::Rc::new(std::cell::Cell::new(false));
    let stream = Capped { inner: flate2::read::GzDecoder::new(io::BufReader::new(file)), left: max_stream, over: over.clone() };
    let result = unpack_entries(tar::Archive::new(stream), app);
    if over.get() { Err(ArchiveError::TooLarge) } else { result }
}

fn unpack_entries(mut archive: tar::Archive<impl Read>, app: &Path) -> Result<(), ArchiveError> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let (mut count, mut total) = (0usize, 0u64);
    fs::create_dir(app)?;
    for entry in archive.entries().map_err(|_| ArchiveError::Format)? {
        let mut entry = entry.map_err(|_| ArchiveError::Format)?;
        count += 1;
        if count > MAX_ENTRIES {
            return Err(ArchiveError::TooLarge);
        }
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            // Read past, but counted.
            total = total.saturating_add(entry.size());
            if total > MAX_UNPACKED {
                return Err(ArchiveError::TooLarge);
            }
            continue;
        }
        let path = entry.path().map_err(|_| ArchiveError::Format)?.into_owned();
        let Some(inner) = inside(&path)? else { continue };
        if inner.as_os_str().is_empty() {
            // The bundle itself.
            if !kind.is_dir() {
                return Err(ArchiveError::Special);
            }
            continue;
        }
        if !seen.insert(inner.clone()) {
            return Err(ArchiveError::Duplicate);
        }
        // Checked on disk, not by name: the file system may ignore case.
        let target = real_folders(app, &inner)?;
        let made = if kind.is_dir() {
            match fs::symlink_metadata(&target) {
                Ok(meta) if meta.file_type().is_symlink() => return Err(ArchiveError::ThroughSymlink),
                Ok(meta) if meta.is_dir() => Ok(()),
                Ok(_) => return Err(ArchiveError::Duplicate),
                Err(_) => fs::create_dir(&target),
            }
        } else if kind.is_symlink() {
            let link = entry.link_name().map_err(|_| ArchiveError::Format)?.map(|link| link.into_owned()).unwrap_or_default();
            if !stays_inside(&inner, &link) {
                return Err(ArchiveError::Symlink);
            }
            std::os::unix::fs::symlink(&link, &target)
        } else if kind.is_file() {
            let size = entry.size();
            total = total.saturating_add(size);
            if size > MAX_FILE || total > MAX_UNPACKED {
                return Err(ArchiveError::TooLarge);
            }
            let executable = entry.header().mode().map_err(|_| ArchiveError::Format)? & 0o111 != 0;
            write_file(&mut entry, &target, size, if executable { 0o755 } else { 0o644 })
        } else {
            return Err(ArchiveError::Special);
        };
        made.map_err(|error| if error.kind() == io::ErrorKind::AlreadyExists { ArchiveError::Duplicate } else { error.into() })?;
    }
    Ok(())
}

/// `app/inner`, once every folder above it inside the bundle is a real
/// folder (made when missing), never a symlink.
fn real_folders(app: &Path, inner: &Path) -> Result<PathBuf, ArchiveError> {
    let mut at = app.to_path_buf();
    let mut components = inner.components().peekable();
    while let Some(component) = components.next() {
        at.push(component);
        if components.peek().is_none() {
            break;
        }
        match fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(ArchiveError::ThroughSymlink),
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(ArchiveError::Duplicate),
            Err(_) => fs::create_dir(&at)?,
        }
    }
    Ok(at)
}

/// The part of `path` inside `Crossbow.app`; None for a bare `./`.
fn inside(path: &Path) -> Result<Option<PathBuf>, ArchiveError> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => {
                if name.as_encoded_bytes().starts_with(b"._") {
                    return Err(ArchiveError::AppleDouble);
                }
                parts.push(name);
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return Err(ArchiveError::Parent),
        }
    }
    let Some((first, rest)) = parts.split_first() else { return Ok(None) };
    if *first != APP {
        return Err(ArchiveError::Outside);
    }
    Ok(Some(rest.iter().collect()))
}

/// Whether a symlink at `at` (inside the bundle) pointing to `link` resolves
/// inside the bundle: relative, any `..` only at the start, and no more of
/// them than the folders above it.
fn stays_inside(at: &Path, link: &Path) -> bool {
    let depth = at.components().count() - 1;
    let mut ups = 0;
    let mut descended = false;
    for component in link.components() {
        match component {
            Component::ParentDir if !descended => ups += 1,
            Component::Normal(_) => descended = true,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    !link.as_os_str().is_empty() && ups <= depth
}

fn write_file(entry: &mut impl Read, target: &Path, size: u64, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(target)?;
    let written = io::copy(&mut entry.take(size), &mut file)?;
    if written != size {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(())
}

/// What `app/Contents/Info.plist` says, when it names Crossbow, a version,
/// a build number, a minimum macOS and an executable that is a plain file.
pub fn read_info(app: &Path) -> Result<Bundle, ArchiveError> {
    let path = app.join("Contents/Info.plist");
    let meta = fs::symlink_metadata(&path).map_err(|_| ArchiveError::InfoPlist)?;
    if !meta.is_file() || meta.len() > MAX_INFO_PLIST {
        return Err(ArchiveError::InfoPlist);
    }
    let value = plist::Value::from_file(&path).map_err(|_| ArchiveError::InfoPlist)?;
    let dict = value.as_dictionary().ok_or(ArchiveError::InfoPlist)?;
    let text = |key: &str| dict.get(key).and_then(plist::Value::as_string);
    if text("CFBundleIdentifier") != Some(BUNDLE_ID) {
        return Err(ArchiveError::Identity);
    }
    let version = text("CFBundleShortVersionString").and_then(Version::parse).ok_or(ArchiveError::InfoPlist)?;
    let build = text("CFBundleVersion").filter(|b| !b.is_empty() && b.len() <= 18 && b.bytes().all(|c| c.is_ascii_digit())).and_then(|b| b.parse().ok()).ok_or(ArchiveError::InfoPlist)?;
    let minimum_macos = text("LSMinimumSystemVersion").and_then(MacVersion::parse).ok_or(ArchiveError::InfoPlist)?;
    let executable = text("CFBundleExecutable").filter(|name| matches!(Path::new(name).components().collect::<Vec<_>>()[..], [Component::Normal(_)])).ok_or(ArchiveError::InfoPlist)?;
    let binary = fs::symlink_metadata(app.join("Contents/MacOS").join(executable)).map_err(|_| ArchiveError::InfoPlist)?;
    if !binary.is_file() {
        return Err(ArchiveError::InfoPlist);
    }
    Ok(Bundle { version, build, minimum_macos, executable: executable.to_owned() })
}

/// Pack `app` as an update archive: `Crossbow.app/…` with symlinks kept,
/// owners and times zeroed, and no macOS metadata files.
pub fn pack(app: &Path, out: &Path) -> io::Result<()> {
    let file = fs::OpenOptions::new().write(true).create_new(true).open(out)?;
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(file, flate2::Compression::default()));
    builder.mode(tar::HeaderMode::Deterministic);
    builder.follow_symlinks(false);
    builder.append_dir_all(APP, app)?;
    builder.into_inner()?.finish()?.sync_all()
}

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
