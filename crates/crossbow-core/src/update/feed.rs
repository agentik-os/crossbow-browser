//! The signed feed. A feed is a small JSON envelope:
//!
//! ```json
//! { "format": "crossbow-update-feed-1", "payload": "<the release, as JSON text>", "signature": "<base64>" }
//! ```
//!
//! The signature is Ed25519 over the payload's exact bytes, so checking it
//! never re-serializes anything; only a payload that verifies is parsed. The
//! release names the archive's size and SHA-256 and carries the archive's own
//! Ed25519 signature over `"crossbow archive v1\0" || sha256 || size` (size as
//! 8 big-endian bytes), which the download checks again after the feed.
//! The release also carries a signed expiry: an expired feed, or one that
//! names an older release than a feed seen before, offers nothing and says
//! so, so a feed host can neither freeze nor replay what clients see.

use super::install::Stamp;
use super::{DAY, MacVersion, Running, Version};
use crate::sync::server;
use data_encoding::{BASE64, HEXLOWER};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde::{Deserialize, Serialize};

pub const FORMAT: &str = "crossbow-update-feed-1";
/// The only channel so far.
pub const CHANNEL: &str = "stable";
/// Larger feeds are refused unread.
pub const MAX_FEED: usize = 64 * 1024;
/// Release notes: plain text, at most 8 KB.
pub const MAX_NOTES: usize = 8 * 1024;
/// Larger archives are refused before downloading.
pub const MAX_ARCHIVE: u64 = 1 << 30;
/// A feed expires at most this long after it is signed, so a replayed or
/// frozen feed stops counting as current.
pub const MAX_VALIDITY: u64 = 30 * DAY;
const ARCHIVE_DOMAIN: &[u8] = b"crossbow archive v1\0";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format: String,
    payload: String,
    signature: String,
}

/// One release, as the feed's payload names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    pub channel: String,
    /// `MAJOR.MINOR.PATCH`, the bundle's `CFBundleShortVersionString`.
    pub version: String,
    /// The bundle's `CFBundleVersion`.
    pub build: u64,
    /// The oldest macOS it runs on.
    pub minimum_macos: String,
    /// When the feed was signed: `YYYY-MM-DDTHH:MM:SSZ`.
    pub published: String,
    /// When the feed stops counting as current, at most [`MAX_VALIDITY`]
    /// after `published`.
    pub expires: String,
    /// Plain text.
    pub notes: String,
    pub archive: Archive,
}

/// Where the release's `.tar.gz` is, and what it must be.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Archive {
    pub url: String,
    pub size: u64,
    /// 64 lowercase hex digits.
    pub sha256: String,
    /// Base64 Ed25519 signature; see [`archive_message`].
    pub signature: String,
}

/// A release-signing key. Only `cargo xtask release-key` makes one to keep;
/// tests make theirs at run time.
pub struct SigningKey(Ed25519KeyPair);

impl SigningKey {
    /// A new key and its PKCS#8 document, the private key file's contents.
    pub fn generate() -> Option<(SigningKey, zeroize::Zeroizing<Vec<u8>>)> {
        let document = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).ok()?;
        let bytes = zeroize::Zeroizing::new(document.as_ref().to_vec());
        Some((SigningKey::from_pkcs8(&bytes)?, bytes))
    }

    /// The key in a PKCS#8 v2 document, as [`SigningKey::generate`] writes it.
    pub fn from_pkcs8(bytes: &[u8]) -> Option<SigningKey> {
        Ed25519KeyPair::from_pkcs8(bytes).ok().map(SigningKey)
    }

    /// The public key, as `CROSSBOW_UPDATE_KEYS` lists it in hex.
    pub fn public(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap_or([0; 32])
    }

    #[cfg(test)]
    pub(crate) fn raw(&self) -> &Ed25519KeyPair {
        &self.0
    }
}

/// Why a feed was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FeedError {
    #[error("the feed is larger than 64 KB")]
    TooLarge,
    #[error("the reply is not a Crossbow update feed")]
    Format,
    #[error("the feed's signature does not match a key in this build")]
    Signature,
    #[error("the feed's {0} is not valid")]
    Field(&'static str),
}

impl Release {
    /// Every field checked, as both signing and opening require.
    pub fn check(&self) -> Result<(), FeedError> {
        if self.channel != CHANNEL {
            return Err(FeedError::Field("channel"));
        }
        if Version::parse(&self.version).is_none() {
            return Err(FeedError::Field("version"));
        }
        if MacVersion::parse(&self.minimum_macos).is_none() {
            return Err(FeedError::Field("minimum macOS version"));
        }
        let Some(published) = parse_time(&self.published) else { return Err(FeedError::Field("publication time")) };
        if parse_time(&self.expires).is_none_or(|expires| expires <= published || expires - published > MAX_VALIDITY) {
            return Err(FeedError::Field("expiry time"));
        }
        if !plain_text(&self.notes) {
            return Err(FeedError::Field("release notes"));
        }
        if server::check(&self.archive.url).is_err() {
            return Err(FeedError::Field("archive URL"));
        }
        if !(1..=MAX_ARCHIVE).contains(&self.archive.size) {
            return Err(FeedError::Field("archive size"));
        }
        if sha256(&self.archive.sha256).is_none() {
            return Err(FeedError::Field("archive SHA-256"));
        }
        if signature(&self.archive.signature).is_none() {
            return Err(FeedError::Field("archive signature"));
        }
        Ok(())
    }

    /// The checked version; only call on a release [`Release::check`] accepted.
    pub fn version(&self) -> Version {
        Version::parse(&self.version).unwrap_or(Version(0, 0, 0))
    }

    pub fn minimum_macos(&self) -> MacVersion {
        MacVersion::parse(&self.minimum_macos).unwrap_or(MacVersion(u64::MAX, 0, 0))
    }

    pub fn sha256(&self) -> [u8; 32] {
        sha256(&self.archive.sha256).unwrap_or([0; 32])
    }
}

/// Text with no control characters but newlines and tabs, at most 8 KB.
pub fn plain_text(text: &str) -> bool {
    text.len() <= MAX_NOTES && text.chars().all(|c| c == '\n' || c == '\t' || !c.is_control())
}

fn sha256(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    HEXLOWER.decode(hex.as_bytes()).ok()?.try_into().ok()
}

fn signature(text: &str) -> Option<[u8; 64]> {
    BASE64.decode(text.as_bytes()).ok()?.try_into().ok()
}

fn verifies(keys: &[[u8; 32]], message: &[u8], signature: &[u8]) -> bool {
    keys.iter().filter(|key| !super::small_order(key)).any(|key| UnparsedPublicKey::new(&ED25519, key).verify(message, signature).is_ok())
}

/// What the archive's signature covers.
pub fn archive_message(sha256: &[u8; 32], size: u64) -> Vec<u8> {
    [ARCHIVE_DOMAIN, sha256, &size.to_be_bytes()].concat()
}

/// The archive's signature, base64.
pub fn sign_archive(key: &SigningKey, sha256: &[u8; 32], size: u64) -> String {
    BASE64.encode(key.0.sign(&archive_message(sha256, size)).as_ref())
}

/// Whether the release's archive signature covers this SHA-256 and size,
/// under a key of this build.
pub fn archive_verifies(release: &Release, keys: &[[u8; 32]], sha256: &[u8; 32], size: u64) -> bool {
    signature(&release.archive.signature).is_some_and(|signature| verifies(keys, &archive_message(sha256, size), &signature))
}

/// The feed for `release`, signed with `key`: the bytes to publish.
pub fn sign(release: &Release, key: &SigningKey) -> Result<String, FeedError> {
    release.check()?;
    let payload = serde_json::to_string(release).map_err(|_| FeedError::Format)?;
    let signature = BASE64.encode(key.0.sign(payload.as_bytes()).as_ref());
    let envelope = Envelope { format: FORMAT.into(), payload, signature };
    let mut text = serde_json::to_string_pretty(&envelope).map_err(|_| FeedError::Format)?;
    text.push('\n');
    if text.len() > MAX_FEED {
        return Err(FeedError::TooLarge);
    }
    Ok(text)
}

/// The release in `bytes`, when a key in `keys` signed it and every field is
/// valid.
pub fn open(bytes: &[u8], keys: &[[u8; 32]]) -> Result<Release, FeedError> {
    if bytes.len() > MAX_FEED {
        return Err(FeedError::TooLarge);
    }
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(|_| FeedError::Format)?;
    if envelope.format != FORMAT {
        return Err(FeedError::Format);
    }
    let signature = signature(&envelope.signature).ok_or(FeedError::Signature)?;
    if !verifies(keys, envelope.payload.as_bytes(), &signature) {
        return Err(FeedError::Signature);
    }
    let release: Release = serde_json::from_str(&envelope.payload).map_err(|_| FeedError::Format)?;
    release.check()?;
    Ok(release)
}

/// The release for the archive at `path`, as `cargo xtask release-sign`
/// writes it: the archive's size, SHA-256 and signature, and the version,
/// build and minimum macOS its app's `Info.plist` names, current from
/// `published` for `valid_for` seconds.
pub fn describe(path: &std::path::Path, url: &str, bundle: &super::archive::Bundle, notes: &str, published: u64, valid_for: u64, key: &SigningKey) -> std::io::Result<Release> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let (mut hasher, mut size, mut buffer) = (Sha256::new(), 0u64, vec![0u8; 1 << 16]);
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    let digest: [u8; 32] = hasher.finalize().as_slice().try_into().map_err(std::io::Error::other)?;
    let release = Release {
        channel: CHANNEL.into(),
        version: bundle.version.to_string(),
        build: bundle.build,
        minimum_macos: bundle.minimum_macos.to_string(),
        published: format_time(published),
        expires: format_time(published.saturating_add(valid_for)),
        notes: notes.into(),
        archive: Archive { url: url.into(), size, sha256: HEXLOWER.encode(&digest), signature: sign_archive(key, &digest, size) },
    };
    release.check().map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    Ok(release)
}

/// What a verified release means for the running app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Offer {
    /// The feed names this version and build.
    UpToDate,
    /// Newer, and this macOS can run it.
    Available(Release),
    /// Newer, but it needs a later macOS.
    Unsupported { version: Version, needs: MacVersion },
    /// Older than what runs: never installed.
    Older(Version),
    /// The feed's signed expiry passed this many whole days ago: it may be
    /// replayed or frozen, so it says nothing about what is current.
    Expired { days: u64 },
    /// Older than a release an earlier feed named: replayed.
    Replayed { offered: Version, newest: Version },
    /// Newer, but it was rolled back on this Mac; a later build is offered.
    RolledBack(Version),
}

/// What a feed is judged against besides itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    pub running: Running,
    /// The running macOS; None when it can't be read.
    pub mac: Option<MacVersion>,
    /// Seconds since 1970.
    pub now: u64,
    /// The newest release an unexpired feed has named.
    pub newest: Option<Stamp>,
    /// Releases rolled back on this Mac.
    pub rolled_back: Vec<Stamp>,
}

/// Compare a verified release with what runs: newer means a greater version,
/// or the same version with a greater build. An expired feed, or one older
/// than a feed seen before, says nothing either way; a release rolled back
/// here is never offered again; when the running macOS can't be read, a
/// newer release counts as unsupported.
pub fn judge(release: Release, known: &Known) -> Offer {
    let offered = (release.version(), release.build);
    if let Some(expires) = parse_time(&release.expires).filter(|expires| known.now >= *expires) {
        return Offer::Expired { days: (known.now - expires) / DAY };
    }
    let rank = |stamp: &Stamp| Version::parse(&stamp.version).map(|version| (version, stamp.build));
    if let Some(newest) = known.newest.as_ref().and_then(rank).filter(|newest| offered < *newest) {
        return Offer::Replayed { offered: offered.0, newest: newest.0 };
    }
    let current = (known.running.version, known.running.build);
    if offered < current {
        return Offer::Older(offered.0);
    }
    if offered == current {
        return Offer::UpToDate;
    }
    if known.rolled_back.iter().filter_map(rank).any(|rolled_back| rolled_back == offered) {
        return Offer::RolledBack(offered.0);
    }
    let needs = release.minimum_macos();
    if known.mac.is_none_or(|mac| mac < needs) {
        return Offer::Unsupported { version: offered.0, needs };
    }
    Offer::Available(release)
}

/// `secs` since 1970 as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format_time(secs: u64) -> String {
    let c = crate::backup::calendar(secs * 1000, 0);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", c.year, c.month, c.day, c.hour, c.minute, c.second)
}

/// Seconds since 1970 for `YYYY-MM-DDTHH:MM:SSZ`; None for anything else,
/// including dates past the end of their month.
pub fn parse_time(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = &text[range];
        part.bytes().all(|c| c.is_ascii_digit()).then(|| part.parse().ok()).flatten()
    };
    let (year, month, day) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    let (hour, minute, second) = (digits(11..13)?, digits(14..16)?, digits(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = crate::backup::days_from_civil(i64::from(year), month, day);
    let secs = u64::try_from(days * 86_400 + i64::from(hour * 3600 + minute * 60 + second)).ok()?;
    (format_time(secs) == text).then_some(secs)
}

#[cfg(test)]
#[path = "feed_tests.rs"]
mod tests;
