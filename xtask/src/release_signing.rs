//! `cargo xtask release-key --out DIR` makes the Ed25519 key pair that signs
//! update feeds: the private key as a PKCS#8 file readable only by its
//! owner, never inside this repository and never printed, and the public
//! key, printed for `CROSSBOW_UPDATE_KEYS`. It refuses to overwrite a key.
//!
//! `cargo xtask release-sign --archive FILE --version X.Y.Z --notes FILE
//! --key FILE --url URL --team TEAMID [--valid-days N] --out FILE` writes
//! the signed feed for one archive: a `.tar.gz` of `Crossbow.app` that
//! unpacks as Crossbow would unpack it, names `--version` and is signed with
//! a Developer ID for `--team` (on macOS). The feed's version, build and
//! minimum macOS come from the app's Info.plist; `--url` is the https URL
//! the archive will be published at. The feed expires `--valid-days` after
//! signing (14 unless given, at most 30), so it must be signed again before
//! then. The feed is opened again with the key's public half before it is
//! written. Nothing is published.

use crossbow_core::update::archive::{self, Bundle};
use crossbow_core::update::feed::{self, SigningKey};
use crossbow_core::update::{DAY, Version, codesign};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The private key file: a PKCS#8 v2 document.
pub const PRIVATE: &str = "crossbow-release.pk8";
/// The public key file: 64 hex digits and a newline.
pub const PUBLIC: &str = "crossbow-release.pub";

const KEY_USAGE: &str = "usage: cargo xtask release-key --out DIR";
const SIGN_USAGE: &str = "usage: cargo xtask release-sign --archive FILE --version X.Y.Z --notes FILE --key FILE --url URL --team TEAMID [--valid-days N] --out FILE";
/// How long a feed stays current unless `--valid-days` says otherwise.
pub const VALID_DAYS: u64 = 14;
pub const MAX_VALID_DAYS: u64 = feed::MAX_VALIDITY / DAY;

pub fn key(args: &[String], root: &Path) -> Result<(), String> {
    let [flag, out] = args else { return Err(KEY_USAGE.into()) };
    if flag != "--out" {
        return Err(KEY_USAGE.into());
    }
    for line in make_key(Path::new(out), root)? {
        println!("{line}");
    }
    Ok(())
}

pub fn sign(args: &[String]) -> Result<(), String> {
    let options = Sign::parse(args)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs();
    for line in sign_release(&options, &developer_id, now)? {
        println!("{line}");
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `path` with symlinks resolved, when it or its parent folder exists.
fn resolved(path: &Path) -> Result<PathBuf, String> {
    if let Ok(path) = path.canonicalize() {
        return Ok(path);
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else { return Err(format!("{} is not a folder name", path.display())) };
    let parent = if parent.as_os_str().is_empty() { Path::new(".") } else { parent };
    Ok(parent.canonicalize().map_err(|e| format!("{}: {e}", parent.display()))?.join(name))
}

/// Writes a new key pair into `out`, which may not be inside `repo`: the
/// lines to print, which hold the public key only.
pub fn make_key(out: &Path, repo: &Path) -> Result<Vec<String>, String> {
    let folder = resolved(out)?;
    if folder.starts_with(repo.canonicalize().map_err(|e| e.to_string())?) {
        return Err("a private key never goes inside the repository; choose a folder outside it".into());
    }
    let (private, public) = (folder.join(PRIVATE), folder.join(PUBLIC));
    if fs::symlink_metadata(&private).is_ok() || fs::symlink_metadata(&public).is_ok() {
        return Err(format!("{} already holds a release key; it is never overwritten", folder.display()));
    }
    if !folder.is_dir() {
        fs::DirBuilder::new().mode(0o700).create(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    }
    let (key, document) = SigningKey::generate().ok_or("no key could be generated")?;
    let written = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&private).and_then(|mut file| {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(&document)?;
        file.sync_all()
    });
    written.map_err(|e| format!("{}: {e}", private.display()))?;
    let key_hex = hex(&key.public());
    let published = fs::OpenOptions::new().write(true).create_new(true).mode(0o644).open(&public).and_then(|mut file| file.write_all(format!("{key_hex}\n").as_bytes()));
    if let Err(e) = published {
        let _ = fs::remove_file(&private);
        return Err(format!("{}: {e}", public.display()));
    }
    Ok(vec![
        format!("public key: {key_hex}"),
        format!("CROSSBOW_UPDATE_KEYS={key_hex}"),
        format!("wrote {} (the private key, mode 0600) and {}", private.display(), public.display()),
        "Keep the private key out of the repository and back it up offline; a build trusts only the keys it embeds.".into(),
    ])
}

/// `release-sign`'s arguments.
#[derive(Debug)]
pub struct Sign {
    pub archive: PathBuf,
    pub version: Version,
    pub notes: PathBuf,
    pub key: PathBuf,
    pub url: String,
    /// The Team ID the app must be signed by.
    pub team: String,
    pub valid_days: u64,
    pub out: PathBuf,
}

impl Sign {
    pub fn parse(args: &[String]) -> Result<Sign, String> {
        let mut values: [Option<&String>; 8] = Default::default();
        let names = ["--archive", "--version", "--notes", "--key", "--url", "--team", "--valid-days", "--out"];
        for pair in args.chunks(2) {
            let [flag, value] = pair else { return Err(SIGN_USAGE.into()) };
            let index = names.iter().position(|name| name == flag).ok_or_else(|| SIGN_USAGE.to_string())?;
            if values[index].replace(value).is_some() {
                return Err(format!("{} is given twice", names[index]));
            }
        }
        let [Some(archive), Some(version), Some(notes), Some(key), Some(url), Some(team), valid_days, Some(out)] = values else { return Err(SIGN_USAGE.into()) };
        let version = Version::parse(version).ok_or("--version is MAJOR.MINOR.PATCH, digits only")?;
        if codesign::requirement(team).is_none() {
            return Err("--team is a Team ID: 10 capital letters and digits".into());
        }
        let valid_days = match valid_days {
            None => VALID_DAYS,
            Some(days) => days.parse().ok().filter(|days| (1..=MAX_VALID_DAYS).contains(days)).ok_or(format!("--valid-days is a whole number of days from 1 to {MAX_VALID_DAYS}"))?,
        };
        Ok(Sign { archive: archive.into(), version, notes: notes.into(), key: key.into(), url: url.clone(), team: team.clone(), valid_days, out: out.into() })
    }
}

/// The private key at `path`, which only its owner may read.
fn read_key(path: &Path) -> Result<SigningKey, String> {
    let meta = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
        return Err(format!("{} must be a file only you can read (chmod 600)", path.display()));
    }
    let document = zeroize::Zeroizing::new(fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?);
    SigningKey::from_pkcs8(&document).ok_or_else(|| format!("{} is not a release key from cargo xtask release-key", path.display()))
}

/// A folder of its own for unpacking, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Scratch, String> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let path = std::env::temp_dir().join(format!("crossbow-release-sign-{}-{nanos}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Scratch(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The Team ID of `app`'s valid Developer ID signature.
pub fn developer_id(app: &Path) -> Result<String, String> {
    use codesign::CodeCheck;
    let team = codesign::team_of(app).ok_or("the app in the archive has no Team ID; sign it with a Developer ID first, or Crossbow refuses to install it")?;
    if !(codesign::System.valid(app) && codesign::System.meets(app, &team)) {
        return Err(format!("the app in the archive is not validly signed with a Developer ID for Team ID {team}"));
    }
    Ok(team)
}

/// Checks the archive as Crossbow will, then writes the signed feed: the
/// lines to print. `team` reads the unpacked app's signature.
pub fn sign_release(options: &Sign, team: &dyn Fn(&Path) -> Result<String, String>, now: u64) -> Result<Vec<String>, String> {
    if fs::symlink_metadata(&options.out).is_ok() {
        return Err(format!("{} already exists; choose a new name", options.out.display()));
    }
    // Crossbow takes loopback http for its tests; a published feed never.
    if !options.url.starts_with("https://") {
        return Err("--url must be an https URL: Crossbow downloads updates only over https".into());
    }
    if !(1..=MAX_VALID_DAYS).contains(&options.valid_days) {
        return Err(format!("a feed stays current for 1 to {MAX_VALID_DAYS} days"));
    }
    let key = read_key(&options.key)?;
    let notes = fs::read_to_string(&options.notes).map_err(|e| format!("{}: {e}", options.notes.display()))?;
    let notes = notes.trim_end();
    if !feed::plain_text(notes) {
        return Err(format!("the release notes must be plain text of at most {} KB", feed::MAX_NOTES / 1024));
    }
    let scratch = Scratch::new()?;
    let bundle: Bundle = archive::unpack(&options.archive, &scratch.0).map_err(|e| format!("the archive is refused: {e}"))?;
    if bundle.version != options.version {
        return Err(format!("the archive holds Crossbow {}, not {}", bundle.version, options.version));
    }
    let team = team(&scratch.0.join(archive::APP))?;
    if team != options.team {
        return Err(format!("the app in the archive is signed by Team ID {team}, not {}", options.team));
    }
    let release = feed::describe(&options.archive, &options.url, &bundle, notes, now, options.valid_days * DAY, &key).map_err(|e| format!("the release is refused: {e}"))?;
    let text = feed::sign(&release, &key).map_err(|e| e.to_string())?;
    if feed::open(text.as_bytes(), &[key.public()]).as_ref() != Ok(&release) || !feed::archive_verifies(&release, &[key.public()], &release.sha256(), release.archive.size) {
        return Err("the signed feed did not verify; nothing was written".into());
    }
    let written = fs::OpenOptions::new().write(true).create_new(true).mode(0o644).open(&options.out).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    written.map_err(|e| format!("{}: {e}", options.out.display()))?;
    Ok(vec![
        format!("Crossbow {} (build {}) for macOS {} or later, signed by Team ID {team}", release.version, release.build, release.minimum_macos),
        format!("archive: {} bytes, SHA-256 {}", release.archive.size, release.archive.sha256),
        format!("wrote {}; it verifies with public key {}", options.out.display(), hex(&key.public())),
        format!("the feed expires at {}: sign it again before then, as Crossbow installs nothing from an expired feed", release.expires),
        "Publish the archive at its URL first, then the feed.".into(),
    ])
}

#[cfg(test)]
#[path = "release_signing_tests.rs"]
mod tests;
