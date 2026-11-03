//! Signed updates. A build names its feed with `CROSSBOW_UPDATE_URL` and the
//! Ed25519 public keys that sign it with `CROSSBOW_UPDATE_KEYS`, both at
//! compile time; a build without either has no updater.
//!
//! - [`feed`]: the signed feed and the archive's signature.
//! - [`net`]: checking the feed and downloading the archive, verified.
//! - [`archive`]: unpacking a `.tar.gz` of `Crossbow.app`, safely.
//! - [`codesign`]: the new app's code signature and Team ID.
//! - [`install`]: staging beside the installed app, the swap, the launch
//!   check and the rollback, each recoverable after a crash.

pub mod archive;
pub mod codesign;
pub mod feed;
pub mod install;
pub mod net;

use crate::sync::server;

/// The feed and keys this build was made with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Build {
    pub url: &'static str,
    /// Comma-separated public keys, 64 hex digits each.
    pub keys: &'static str,
}

/// The updater this build was made for, if any.
pub const BUILD: Option<Build> = build(option_env!("CROSSBOW_UPDATE_URL"), option_env!("CROSSBOW_UPDATE_KEYS"));

/// This build's number, from `CROSSBOW_BUILD_NUMBER` (0 when unset); a
/// bundle's `CFBundleVersion`.
pub const BUILD_NUMBER: u64 = build_number(option_env!("CROSSBOW_BUILD_NUMBER"));

// Feeds and installs tell bundles apart by version and build number.
const _: () = assert!(BUILD.is_none() || option_env!("CROSSBOW_BUILD_NUMBER").is_some(), "a build with an updater needs CROSSBOW_BUILD_NUMBER");
const _: () = assert!(BUILD.is_none() || plain_version(VERSION), "a build with an updater needs a MAJOR.MINOR.PATCH version, digits only");

/// This build's version: `MAJOR.MINOR.PATCH`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What the app says when it has no updater.
pub const NO_UPDATER: &str = "This build has no updater.";

/// At most this many embedded keys.
pub const MAX_KEYS: usize = 8;

/// The build's values checked: `None` when either is unset or empty, and a
/// compile-time panic (so a failed build) when one is malformed.
pub const fn build(url: Option<&'static str>, keys: Option<&'static str>) -> Option<Build> {
    let (Some(url), Some(keys)) = (url, keys) else { return None };
    if url.is_empty() || keys.is_empty() {
        return None;
    }
    if server::check(url).is_err() {
        panic!("CROSSBOW_UPDATE_URL must be an https:// URL, or an http:// URL on 127.0.0.1, [::1] or localhost, with no user, query or fragment");
    }
    if check_keys(keys).is_err() {
        panic!("CROSSBOW_UPDATE_KEYS must be 1 to 8 Ed25519 public keys, 64 hex digits each, separated by commas");
    }
    Some(Build { url, keys })
}

/// Whether `keys` is 1 to [`MAX_KEYS`] keys of 64 hex digits, comma-separated,
/// none of them [`small_order`].
pub const fn check_keys(keys: &str) -> Result<(), &'static str> {
    let b = keys.as_bytes();
    let (mut i, mut count, mut digits) = (0, 0, 0);
    let mut key = [0u8; 32];
    while i <= b.len() {
        if i == b.len() || b[i] == b',' {
            if digits != 64 {
                return Err("each key must be 64 hex digits");
            }
            if small_order(&key) {
                return Err("a key is a degenerate point that anyone can forge signatures for");
            }
            count += 1;
            digits = 0;
            key = [0; 32];
        } else if let Some(nibble) = hex_digit(b[i]) {
            if digits < 64 {
                key[digits / 2] |= nibble << if digits % 2 == 0 { 4 } else { 0 };
            }
            digits += 1;
        } else {
            return Err("keys hold only hex digits and commas");
        }
        i += 1;
    }
    if count > MAX_KEYS {
        return Err("at most 8 keys");
    }
    Ok(())
}

const fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Whether an Ed25519 public key encodes a point of small order, or a y
/// coordinate of p or more: under such a key, signatures can be made
/// without the private key. The sign bit (the top bit) doesn't matter.
pub const fn small_order(key: &[u8; 32]) -> bool {
    // The y coordinates of the small-order points, low byte first, as
    // libsodium lists them: 0 (order 4), 1 (order 1), the two of order 8,
    // and p - 1 (order 2).
    const SMALL: [[u8; 32]; 5] = [
        [0; 32],
        [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        [
            0x26, 0xe8, 0x95, 0x8f, 0xc2, 0xb2, 0x27, 0xb0, 0x45, 0xc3, 0xf4, 0x89, 0xf2, 0xef, 0x98, 0xf0, 0xd5, 0xdf, 0xac, 0x05, 0xd3, 0xc6, 0x33, 0x39, 0xb1, 0x38, 0x02, 0x88,
            0x6d, 0x53, 0xfc, 0x05,
        ],
        [
            0xc7, 0x17, 0x6a, 0x70, 0x3d, 0x4d, 0xd8, 0x4f, 0xba, 0x3c, 0x0b, 0x76, 0x0d, 0x10, 0x67, 0x0f, 0x2a, 0x20, 0x53, 0xfa, 0x2c, 0x39, 0xcc, 0xc6, 0x4e, 0xc7, 0xfd, 0x77,
            0x92, 0xac, 0x03, 0x7a,
        ],
        [
            0xec, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0x7f,
        ],
    ];
    let mut n = 0;
    while n < SMALL.len() {
        let mut i = 0;
        while i < 32 && (if i == 31 { key[i] & 0x7f } else { key[i] }) == SMALL[n][i] {
            i += 1;
        }
        if i == 32 {
            return true;
        }
        n += 1;
    }
    // y >= p = 2^255 - 19: every byte 0xff (0x7f at the top) but the lowest, which is 0xed or more.
    let mut i = 1;
    while i < 31 && key[i] == 0xff {
        i += 1;
    }
    i == 31 && key[31] & 0x7f == 0x7f && key[0] >= 0xed
}

/// Whether `text` is `MAJOR.MINOR.PATCH` as [`Version::parse`] reads it.
pub const fn plain_version(text: &str) -> bool {
    let b = text.as_bytes();
    let (mut i, mut parts, mut digits, mut zero_first) = (0, 0, 0, false);
    while i <= b.len() {
        if i == b.len() || b[i] == b'.' {
            if digits == 0 || digits > 9 || (zero_first && digits > 1) {
                return false;
            }
            parts += 1;
            digits = 0;
        } else if b[i].is_ascii_digit() {
            if digits == 0 {
                zero_first = b[i] == b'0';
            }
            digits += 1;
        } else {
            return false;
        }
        i += 1;
    }
    parts == 3
}

/// `CROSSBOW_BUILD_NUMBER` as a number: digits only, 0 when unset.
pub const fn build_number(value: Option<&'static str>) -> u64 {
    let Some(value) = value else { return 0 };
    let b = value.as_bytes();
    if b.is_empty() || b.len() > 18 {
        panic!("CROSSBOW_BUILD_NUMBER must be 1 to 18 digits");
    }
    let (mut i, mut n) = (0, 0u64);
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            panic!("CROSSBOW_BUILD_NUMBER must be 1 to 18 digits");
        }
        n = n * 10 + (b[i] - b'0') as u64;
        i += 1;
    }
    n
}

impl Build {
    /// The embedded public keys.
    pub fn keys(&self) -> Vec<[u8; 32]> {
        self.keys.split(',').filter_map(|key| data_encoding::HEXLOWER_PERMISSIVE.decode(key.as_bytes()).ok()?.try_into().ok()).collect()
    }
}

/// A version as feeds and bundles write it: `MAJOR.MINOR.PATCH`, digits only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        let mut parts = text.split('.');
        let mut next = || parts.next().filter(|p| !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()) && (p.len() == 1 || !p.starts_with('0')))?.parse().ok();
        let version = Version(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// A macOS version: `MAJOR.MINOR` or `MAJOR.MINOR.PATCH`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MacVersion(pub u64, pub u64, pub u64);

impl MacVersion {
    pub fn parse(text: &str) -> Option<MacVersion> {
        let parts: Vec<&str> = text.split('.').collect();
        if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| p.is_empty() || p.len() > 4 || !p.bytes().all(|b| b.is_ascii_digit())) {
            return None;
        }
        let n = |i: usize| parts.get(i).map_or(Some(0), |p| p.parse().ok());
        Some(MacVersion(n(0)?, n(1)?, n(2)?))
    }

    /// The macOS this runs on; None elsewhere or when it can't be read.
    pub fn running() -> Option<MacVersion> {
        #[cfg(target_os = "macos")]
        {
            let mut buffer = [0u8; 32];
            let mut len = buffer.len();
            // SAFETY: sysctlbyname writes at most `len` bytes into `buffer` and the new length into `len`.
            let status = unsafe { libc::sysctlbyname(c"kern.osproductversion".as_ptr(), buffer.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
            if status != 0 { return None; }
            let text = std::str::from_utf8(&buffer[..len]).ok()?.trim_end_matches('\0');
            MacVersion::parse(text)
        }
        #[cfg(not(target_os = "macos"))]
        None
    }
}

impl std::fmt::Display for MacVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.2 == 0 { write!(f, "{}.{}", self.0, self.1) } else { write!(f, "{}.{}.{}", self.0, self.1, self.2) }
    }
}

/// The app that is running: what a release must be newer than.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Running {
    pub version: Version,
    pub build: u64,
}

impl Running {
    /// This build.
    pub fn this() -> Running {
        Running { version: Version::parse(VERSION).unwrap_or(Version(0, 0, 0)), build: BUILD_NUMBER }
    }
}

/// A day, in seconds.
pub const DAY: u64 = 86_400;
/// Each launch waits up to this much longer than a day, at random, so
/// copies started together don't all ask at once.
pub const MAX_JITTER: u64 = 2 * 3600;
/// Automatic checks wait this long after the window is interactive.
pub const GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// When an automatic check may start: at most once a day plus this launch's
/// jitter, never before the window has been interactive for [`GRACE`] and
/// never while a check or download runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schedule {
    pub jitter: u64,
}

impl Schedule {
    pub fn random() -> Schedule {
        let mut bytes = [0u8; 8];
        let jitter = match getrandom::fill(&mut bytes) {
            Ok(()) => u64::from_le_bytes(bytes) % (MAX_JITTER + 1),
            Err(_) => MAX_JITTER / 2,
        };
        Schedule { jitter }
    }

    /// `now` and `last` (the last check, automatic or not) are seconds since
    /// 1970; a last check more than a day in the future (a clock set back)
    /// doesn't hold checks off.
    pub fn due(&self, now: u64, last: Option<u64>, on: bool, interactive_for: Option<std::time::Duration>, busy: bool) -> bool {
        let waited = last.is_none_or(|last| now.saturating_sub(last) >= DAY + self.jitter || last > now + DAY);
        on && !busy && interactive_for.is_some_and(|elapsed| elapsed >= GRACE) && waited
    }
}

#[cfg(test)]
pub(crate) mod fixtures;

#[cfg(test)]
#[path = "update/update_tests.rs"]
mod tests;
