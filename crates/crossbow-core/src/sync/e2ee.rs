//! End-to-end encryption of library sync payloads (envelope v1).
//!
//! A random 256-bit account root key is created on a trusted device and leaves
//! it only as the recovery code the user writes down. HKDF-SHA256 derives an
//! independent subkey per purpose and epoch. Every live library row is sealed
//! with XChaCha20-Poly1305 before push, bound through AAD to its account, kind,
//! id, clock and epoch; the server stores the text as opaque `Xchacha20V1`.
//!
//! A durable, non-secret local record remembers that an account is encrypted,
//! so no sign-out, empty page or other account can turn its library back into
//! plaintext. v1 does not authenticate deletions (the server clears tombstone
//! payloads), hide metadata (kinds, ids, clocks, sizes, timing) or prevent the
//! server from withholding or replaying rows. See "Encrypted library rows" in
//! `docs/sync-protocol.md` (#encrypted-library-rows-desktop-source-opt-in) and
//! `docs/security-architecture.md`.

use super::key_store::KeyStore;
use crate::library::{Library, Missing, Supersede};
use crate::store::{RawEntity, Store};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use crossbow_proto::{Change, Encoding, Hlc, MAX_PAYLOAD_BYTES, ServerChange, kinds};
use data_encoding::{BASE32_NOPAD, BASE64URL_NOPAD};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use subtle::ConstantTimeEq;
/// Re-exported so the UI wipes recovery-code buffers with the same crate.
pub use zeroize::{Zeroize, Zeroizing};

pub const ENVELOPE_VERSION: u8 = 1;
/// The only library key epoch this client reads or writes.
pub const LIBRARY_EPOCH: u32 = 1;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const HEADER_LEN: usize = 1 + 4 + NONCE_LEN;
const AAD_DOMAIN: &[u8] = b"crossbow.library.envelope.v1";
const HKDF_SALT: &[u8] = b"crossbow.e2ee.v1";
const SUBKEY_DOMAIN: &[u8] = b"crossbow.e2ee.subkey.v1";
const KEY_ID_DOMAIN: &[u8] = b"crossbow.e2ee.key-id.v1";
const RECOVERY_DOMAIN: &[u8] = b"crossbow.e2ee.recovery.v1";
const RECOVERY_PREFIX: &str = "CB1";
const RECOVERY_BODY_LEN: usize = 56;
/// The durable local record: which account is encrypted and how to verify a code.
pub(crate) const FLAG_KEY: &str = "e2ee_library";

/// The reserved row whose sealed check text marks an encrypted account.
pub const MARKER_KIND: &str = "e2ee";
pub const MARKER_ID: &str = "library-v1";
pub const MARKER_PLAINTEXT: &str = r#"{"check":"crossbow-library-e2ee-v1"}"#;

/// Why another account can't sync this device's library.
pub const OTHER_ACCOUNT: &str = "This device's library belongs to another account that uses end-to-end encryption. Sign in to that account.";
/// A code that matches this device's earlier key but not the account's newest marker.
pub const USED_BEFORE: &str = "That's the recovery code this device used before. Encryption was reset on another device since then. Enter the newest recovery code.";
/// A reset whose new key couldn't be stored; its record and marker were put back.
pub const RESET_UNSAVED: &str = "Couldn't store the new key, so the reset wasn't applied. Try again, or Cancel to keep the current code.";
/// The code from before a reset that stopped before its key was stored, once
/// the reset's marker has been uploaded.
pub const RESET_SENT: &str = "That's the recovery code this device used before it reset encryption, and the reset already reached the sync server. Enter the new recovery code.";
/// Any refused code, when the scan before Unlock couldn't run: a newer marker
/// this device hasn't seen may still accept it.
pub const UNCHECKED: &str = "Couldn't check the server for a newer recovery code. If you just reset encryption on another device, connect and try again.";
/// The code of a reset this device never finished, after another device
/// reset encryption since.
pub const SUPERSEDED: &str = "Encryption was reset on another device after this one started a reset. Enter the newest recovery code.";
/// The code of a reset this device never finished and then replaced with
/// another reset of its own.
pub const REPLACED: &str = "That's the code of a reset this device started and then replaced with a newer one. Enter the newest recovery code.";
/// The code of a reset this device never finished, once the account's code
/// was entered over it and its key couldn't be stored, or the app quit.
pub const DROPPED: &str = "That's the code of a reset this device dropped when the account's current recovery code was entered. Enter that code again.";
/// This device's own unfinished reset, when the scan before Unlock couldn't
/// run: a reset elsewhere since then may have replaced it.
pub const UNCHECKED_RESET: &str = "Couldn't check the server for a newer reset from another device, so this device's reset wasn't finished. Connect, then try again.";
const WRONG_CODE: &str = "That recovery code doesn't unlock this account's library. Check it and try again.";
const NO_CHECK_ROW: &str = "Crossbow needs encrypted data from the sync server to check this code. Sync Now, then try again.";
const RESET_DONE: &str = "Encryption was reset. Other devices need the new recovery code to sync.";
/// Replaces RESET_DONE when a newer marker from another device wins after all.
pub const OUTRUN: &str = "Another device set up encryption more recently than this reset. Enter that device's newest recovery code, or reset again.";
/// A sign-in stops when this device can't tell which account its library belongs to.
pub const SIGN_IN_STOPPED: &str = "Couldn't read this device's encryption state, so this sign-in was stopped. Try again.";
/// Non-secret counts behind the notice, kept until the user dismisses it.
pub(crate) const NOTICE_KEY: &str = "e2ee_notice";

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum E2eeError {
    #[error("the system random number generator is unavailable")]
    Random,
    #[error("encrypted data is not valid base64url")]
    Malformed,
    #[error("encrypted data is truncated")]
    Truncated,
    #[error("encrypted data is over the 64 KiB sync limit")]
    TooLarge,
    #[error("unsupported envelope version {0}")]
    UnsupportedVersion(u8),
    #[error("unknown key epoch {0}")]
    UnknownEpoch(u32),
    #[error("encrypted data failed its integrity check")]
    Integrity,
    #[error("decrypted data is not text")]
    NotText,
}

/// Independent subkey purposes. `Vault` is reserved for the separate password
/// vault so its keys can never collide with library keys; nothing derives it yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Library,
    Vault,
}

impl Purpose {
    fn label(self) -> &'static [u8] {
        match self {
            Purpose::Library => b"library",
            Purpose::Vault => b"vault",
        }
    }
}

/// The account root key: 32 bytes from the OS CSPRNG, boxed so moving the key
/// never copies the bytes, and wiped on drop.
pub struct LibraryKey(Box<Zeroizing<[u8; 32]>>);

impl LibraryKey {
    pub fn generate() -> Result<LibraryKey, E2eeError> {
        let mut bytes = Box::new(Zeroizing::new([0u8; 32]));
        getrandom::fill(bytes.as_mut_slice()).map_err(|_| E2eeError::Random)?;
        Ok(LibraryKey(bytes))
    }

    pub fn from_slice(bytes: &[u8]) -> Option<LibraryKey> {
        let mut key = Box::new(Zeroizing::new([0u8; 32]));
        if bytes.len() != key.len() { return None; }
        key.copy_from_slice(bytes);
        Some(LibraryKey(key))
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn subkey(&self, purpose: Purpose, epoch: u32) -> Zeroizing<[u8; 32]> {
        let mut out = Zeroizing::new([0u8; 32]);
        let info = length_prefixed(&[SUBKEY_DOMAIN, purpose.label(), &epoch.to_be_bytes()]);
        Hkdf::<Sha256>::new(Some(HKDF_SALT), self.as_bytes()).expand(&info, out.as_mut_slice()).expect("32 bytes is a valid HKDF-SHA256 length");
        out
    }

    /// The library cipher for the current epoch. Derive it once per key and
    /// keep it: every derivation leaves an unwiped HKDF state behind.
    pub fn library_cipher(&self) -> LibraryCipher {
        let subkey = self.subkey(Purpose::Library, LIBRARY_EPOCH);
        LibraryCipher(Box::new(XChaCha20Poly1305::new(Key::from_slice(subkey.as_slice()))))
    }

    /// A non-secret fingerprint stored locally to recognise this key again.
    pub fn key_id(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        Hkdf::<Sha256>::new(Some(HKDF_SALT), self.as_bytes()).expand(KEY_ID_DOMAIN, &mut out).expect("16 bytes is a valid HKDF-SHA256 length");
        out
    }
}

impl fmt::Debug for LibraryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LibraryKey(..)")
    }
}

/// The library subkey as a ready AEAD, boxed so it is never copied by a move.
/// The AEAD wipes its key when dropped.
pub struct LibraryCipher(Box<XChaCha20Poly1305>);

impl fmt::Debug for LibraryCipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LibraryCipher(..)")
    }
}

/// What a ciphertext is bound to. Moving it to another account, row, clock or
/// epoch fails authentication. The account id and the clock's encoded text are
/// part of every sealed row, so both must stay byte-identical forever.
#[derive(Clone, Copy, Debug)]
pub struct Binding<'a> {
    pub account_id: &'a str,
    pub kind: &'a str,
    pub id: &'a str,
    pub hlc: &'a Hlc,
}

fn length_prefixed(fields: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(fields.iter().map(|f| 4 + f.len()).sum());
    for field in fields {
        out.extend_from_slice(&(field.len() as u32).to_be_bytes());
        out.extend_from_slice(field);
    }
    out
}

fn aad(binding: &Binding<'_>, epoch: u32) -> Vec<u8> {
    let hlc = binding.hlc.encode();
    length_prefixed(&[AAD_DOMAIN, binding.account_id.as_bytes(), binding.kind.as_bytes(), binding.id.as_bytes(), hlc.as_bytes(), &epoch.to_be_bytes()])
}

/// Whether a row of `len` plaintext bytes still fits one sync row once sealed.
fn sealed_fits(len: usize) -> bool {
    BASE64URL_NOPAD.encode_len(HEADER_LEN + len + TAG_LEN) <= MAX_PAYLOAD_BYTES
}

/// Seal one row's JSON text with a fresh random nonce.
pub fn seal(cipher: &LibraryCipher, binding: &Binding<'_>, plaintext: &str) -> Result<String, E2eeError> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|_| E2eeError::Random)?;
    seal_with_nonce(cipher, binding, plaintext, &nonce)
}

fn seal_with_nonce(cipher: &LibraryCipher, binding: &Binding<'_>, plaintext: &str, nonce: &[u8; NONCE_LEN]) -> Result<String, E2eeError> {
    // Refuse before encrypting: the encoded envelope must fit one sync row.
    if !sealed_fits(plaintext.len()) {
        return Err(E2eeError::TooLarge);
    }
    let sealed = cipher.0.encrypt(XNonce::from_slice(nonce), Payload { msg: plaintext.as_bytes(), aad: &aad(binding, LIBRARY_EPOCH) })
        .map_err(|_| E2eeError::TooLarge)?;
    let mut envelope = Vec::with_capacity(HEADER_LEN + sealed.len());
    envelope.push(ENVELOPE_VERSION);
    envelope.extend_from_slice(&LIBRARY_EPOCH.to_be_bytes());
    envelope.extend_from_slice(nonce);
    envelope.extend_from_slice(&sealed);
    Ok(BASE64URL_NOPAD.encode(&envelope))
}

/// Open an envelope for exactly this binding, or fail closed.
pub fn open(cipher: &LibraryCipher, binding: &Binding<'_>, payload: &str) -> Result<String, E2eeError> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(E2eeError::TooLarge);
    }
    let envelope = BASE64URL_NOPAD.decode(payload.as_bytes()).map_err(|_| E2eeError::Malformed)?;
    if envelope.len() < HEADER_LEN + TAG_LEN {
        return Err(E2eeError::Truncated);
    }
    if envelope[0] != ENVELOPE_VERSION {
        return Err(E2eeError::UnsupportedVersion(envelope[0]));
    }
    let epoch = u32::from_be_bytes([envelope[1], envelope[2], envelope[3], envelope[4]]);
    if epoch != LIBRARY_EPOCH {
        return Err(E2eeError::UnknownEpoch(epoch));
    }
    let plaintext = cipher.0.decrypt(XNonce::from_slice(&envelope[5..HEADER_LEN]), Payload { msg: &envelope[HEADER_LEN..], aad: &aad(binding, epoch) })
        .map_err(|_| E2eeError::Integrity)?;
    String::from_utf8(plaintext).map_err(|_| E2eeError::NotText)
}

// -- recovery code -------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RecoveryError {
    #[error("Enter the whole recovery code: CB1 followed by 14 groups of four.")]
    Length,
    #[error("Recovery codes use only the letters A–Z and the digits 2–7.")]
    Character,
    #[error("That recovery code has a typo. Check each group and try again.")]
    Checksum,
}

/// The root key as `CB1-XXXX-…` (14 base32 groups: 32 key bytes and a 3-byte
/// checksum). Shown at setup and on request; never logged.
pub struct RecoveryCode(Zeroizing<String>);

impl RecoveryCode {
    pub fn for_key(key: &LibraryKey) -> RecoveryCode {
        let mut bytes = Zeroizing::new([0u8; 35]);
        bytes[..32].copy_from_slice(key.as_bytes());
        bytes[32..].copy_from_slice(&recovery_checksum(key.as_bytes()));
        let body = Zeroizing::new(BASE32_NOPAD.encode(bytes.as_slice()));
        let mut code = Zeroizing::new(String::with_capacity(RECOVERY_PREFIX.len() + RECOVERY_BODY_LEN + RECOVERY_BODY_LEN / 4));
        code.push_str(RECOVERY_PREFIX);
        for group in body.as_bytes().chunks(4) {
            code.push('-');
            code.push_str(std::str::from_utf8(group).expect("base32 is ASCII"));
        }
        RecoveryCode(code)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Case-insensitive; spaces, line breaks and dashes are ignored. A typo is
    /// caught by the checksum before any decryption is attempted.
    pub fn parse(input: &str) -> Result<LibraryKey, RecoveryError> {
        let mut normalized = Zeroizing::new(String::with_capacity(input.len()));
        normalized.extend(input.chars().filter(|c| !c.is_whitespace() && *c != '-').map(|c| c.to_ascii_uppercase()));
        // The prefix contains "1", which no base32 body can, so it is unambiguous.
        let body = normalized.strip_prefix(RECOVERY_PREFIX).unwrap_or(&normalized);
        if !body.bytes().all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b)) {
            return Err(RecoveryError::Character);
        }
        if body.len() != RECOVERY_BODY_LEN {
            return Err(RecoveryError::Length);
        }
        let bytes = Zeroizing::new(BASE32_NOPAD.decode(body.as_bytes()).map_err(|_| RecoveryError::Character)?);
        let key = LibraryKey::from_slice(&bytes[..32]).ok_or(RecoveryError::Length)?;
        if !bool::from(recovery_checksum(key.as_bytes()).ct_eq(&bytes[32..])) {
            return Err(RecoveryError::Checksum);
        }
        Ok(key)
    }
}

impl fmt::Debug for RecoveryCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecoveryCode(..)")
    }
}

fn recovery_checksum(key: &[u8; 32]) -> [u8; 3] {
    let digest = Sha256::new().chain_update(RECOVERY_DOMAIN).chain_update(key).finalize();
    [digest[0], digest[1], digest[2]]
}

// -- account state -------------------------------------------------------------------

/// A verified key with what is derived from it once.
struct Unlocked {
    key: LibraryKey,
    cipher: LibraryCipher,
    key_id: String,
}

impl Unlocked {
    fn new(key: LibraryKey) -> Unlocked {
        let (cipher, key_id) = (key.library_cipher(), BASE64URL_NOPAD.encode(&key.key_id()));
        Unlocked { key, cipher, key_id }
    }
}

/// The durable record. It is never removed by a sign-out, only replaced by a
/// reset, so this device keeps treating the account as encrypted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Flag {
    account: String,
    /// The root key's fingerprint, once a key has been verified on this device.
    #[serde(default)]
    key_id: Option<String>,
    /// A sealed row seen while locked, used to verify a recovery code.
    #[serde(default)]
    check: Option<CheckRow>,
    /// After an unlock or a reset: every live row is sealed again under this
    /// key after the next complete pull. Kept here so a quit, crash or
    /// sign-out in between can't skip it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    reseal: bool,
    /// Written by a reset with its new marker and dropped once a key is
    /// stored, by the reset or by an unlock, so a failed save or a crash in
    /// between can still be undone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    undo: Option<Undo>,
    /// The sync epoch of the account data this record protects. A record
    /// from before records named it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    epoch: Option<u64>,
}

/// The way back from this device's unsent resets: the state before the first
/// of them, which a later reset carries forward unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Undo {
    /// The record's exact text before the first reset, if there was one.
    record: Option<String>,
    /// The marker row before the first reset, if there was one.
    marker: Option<RawEntity>,
    /// The newest reset's own marker clock.
    hlc: Hlc,
    /// The newest marker from the server this device knew of when it reset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    above: Option<Hlc>,
    /// The key ids of those resets, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    resets: Vec<String>,
    /// The marker of a reset elsewhere that had replaced an earlier one of
    /// these resets when a later one started. Its code is the account's, so
    /// it still unlocks while these resets are unsent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    elsewhere: Option<CheckRow>,
}

impl Undo {
    /// The record and marker row as they are now.
    fn read(store: &Store, hlc: Hlc) -> Result<Undo, String> {
        const READ_ERROR: &str = "Nothing was reset: this device's encryption state couldn't be read. Check disk access, then try again.";
        let record = store.kv_get(FLAG_KEY).map_err(|_| READ_ERROR.to_string())?;
        let marker = store.raw_entity(MARKER_KIND, MARKER_ID).map_err(|_| READ_ERROR.to_string())?;
        Ok(Undo { record, marker, hlc, above: None, resets: Vec::new(), elsewhere: None })
    }

    /// A marker from the server at `hlc` was set by a reset elsewhere after
    /// this device's: it is newer than every marker known when this device
    /// reset, whatever its clock compared with this device's own marker.
    fn outrun_by(&self, hlc: &Hlc) -> bool {
        let known = self.above.clone().or_else(|| self.marker.as_ref().and_then(|row| Hlc::parse(&row.hlc).ok()));
        *hlc != self.hlc && known.is_none_or(|known| *hlc > known)
    }

    /// Put the marker row back as it was before these resets, while the
    /// newest one is still waiting to upload, so it never is.
    fn drop_marker(&self, store: &Store) -> Result<(), String> {
        const ERROR: &str = "Couldn't set aside this device's unfinished reset; check disk access, then try again.";
        if !self.unsent(store)? { return Ok(()); }
        store.put_raw_entity(MARKER_KIND, MARKER_ID, self.marker.as_ref()).map_err(|_| ERROR.to_string())
    }

    /// The record before the reset, if it was readable and this account's.
    fn prior(&self, account: &str) -> Option<Flag> {
        self.record.as_deref().and_then(|text| serde_json::from_str::<Flag>(text).ok()).filter(|flag| flag.account == account)
    }

    /// The reset's marker is still the local version and still waiting to
    /// upload, so no other device can have seen it.
    fn unsent(&self, store: &Store) -> Result<bool, String> {
        let marker = store.raw_entity(MARKER_KIND, MARKER_ID).map_err(|_| "Couldn't read this device's encryption state; check disk access, then try again.".to_string())?;
        Ok(marker.is_some_and(|row| row.dirty && row.hlc == self.hlc.encode()))
    }

    /// Put the record and the marker row back exactly, in one transaction.
    fn restore(&self, store: &Store) -> Result<(), rusqlite::Error> {
        store.begin_transaction()?;
        let restored = (|| {
            match &self.record {
                Some(text) => store.kv_set(FLAG_KEY, text)?,
                None => store.kv_delete(FLAG_KEY)?,
            }
            store.put_raw_entity(MARKER_KIND, MARKER_ID, self.marker.as_ref())?;
            store.commit_transaction()
        })();
        if restored.is_err() { let _ = store.rollback_transaction(); }
        restored
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CheckRow {
    kind: String,
    id: String,
    hlc: Hlc,
    payload: String,
}

impl CheckRow {
    fn of(c: &Change) -> CheckRow {
        CheckRow { kind: c.kind.clone(), id: c.id.clone(), hlc: c.hlc.clone(), payload: c.payload.clone() }
    }

    /// A marker beats any other row, and a newer row an older one, so the
    /// check follows the account's current key after a reset elsewhere.
    fn rank(&self) -> (bool, &Hlc) {
        (is_marker(&self.kind, &self.id), &self.hlc)
    }

    /// The row opens under this key, and a marker holds the marker text.
    fn opens(&self, account: &str, unlocked: &Unlocked) -> bool {
        open_row(account, &unlocked.cipher, &self.kind, &self.id, &self.hlc, &self.payload)
            .is_ok_and(|plaintext| !is_marker(&self.kind, &self.id) || plaintext == MARKER_PLAINTEXT)
    }
}

impl Flag {
    /// A captured server row is the freshest evidence of the account's key, so
    /// it decides when present; otherwise the key this device verified before.
    fn verifies(&self, unlocked: &Unlocked) -> bool {
        if let Some(check) = &self.check {
            return check.opens(&self.account, unlocked);
        }
        self.key_id.as_ref().is_some_and(|stored| bool::from(unlocked.key_id.as_bytes().ct_eq(stored.as_bytes())))
    }

    /// The check row is this device's own unsent reset marker.
    fn checks_own_reset(&self) -> bool {
        self.undo.as_ref().zip(self.check.as_ref()).is_some_and(|(undo, check)| is_marker(&check.kind, &check.id) && check.hlc == undo.hlc)
    }

    /// `row`, from the server, is better evidence of the account's key than
    /// the check row: a marker beats any other row, and a newer row an older
    /// one. This device's own unsent reset gives way to any marker from a
    /// reset elsewhere since, whatever the clocks say.
    fn checks_better(&self, row: &CheckRow) -> bool {
        match &self.undo {
            Some(undo) if self.checks_own_reset() => is_marker(&row.kind, &row.id) && undo.outrun_by(&row.hlc),
            _ => self.check.as_ref().is_none_or(|old| row.rank() > old.rank()),
        }
    }
}

fn open_row(account: &str, cipher: &LibraryCipher, kind: &str, id: &str, hlc: &Hlc, payload: &str) -> Result<String, E2eeError> {
    open(cipher, &Binding { account_id: account, kind, id, hlc }, payload)
}

fn is_marker(kind: &str, id: &str) -> bool {
    kind == MARKER_KIND && id == MARKER_ID
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FlagError {
    /// The read failed; it may work a moment later.
    Read,
    /// The stored text can't be parsed.
    Corrupt,
}

fn read_flag(lib: &Library) -> Result<Option<Flag>, FlagError> {
    match lib.store.kv_get(FLAG_KEY) {
        Ok(None) => Ok(None),
        Ok(Some(text)) => serde_json::from_str(&text).map(Some).map_err(|_| FlagError::Corrupt),
        Err(_) => Err(FlagError::Read),
    }
}

/// A record made now belongs to the epoch the stored cursor is in; one
/// carried over from an earlier record keeps that record's epoch.
fn stamped(flag: Flag, lib: &Library) -> Flag {
    let epoch = flag.epoch.or_else(|| super::deleted::stored_epoch(lib));
    Flag { epoch, ..flag }
}

/// The account an encryption record belongs to; `None` if it can't be read.
pub(crate) fn record_account(text: &str) -> Option<String> {
    serde_json::from_str::<Flag>(text).ok().map(|flag| flag.account)
}

fn flag_text(flag: &Flag) -> Result<String, String> {
    serde_json::to_string(flag).map_err(|_| "Couldn't encode the encryption state.".to_string())
}

fn write_flag(lib: &Library, flag: &Flag) -> Result<(), String> {
    lib.store.kv_set(FLAG_KEY, &flag_text(flag)?).map_err(|_| "Couldn't save the encryption state; check disk access.".to_string())
}

/// How a kind is named in messages.
fn kind_label(kind: &str) -> &str {
    match kind {
        kinds::PROFILE => "Profile",
        kinds::SPACE => "Space",
        kinds::FOLDER => "Folder",
        kinds::ITEM => "Pinned tab or favorite",
        kinds::SETTINGS => "Settings",
        kinds::BOOST => "Boost",
        kinds::NOTE => "Note",
        kinds::EASEL => "Easel",
        other => other,
    }
}

const SEAL_READ_ERROR: &str = "Couldn't read the library to encrypt it; check disk access, then try again.";
const CHECKING: &str = "Checking the server for newer encryption changes. Try again in a moment.";

/// Turning encryption on seals every live row again. A row that can't fit
/// once sealed, or whose clock can't advance above its synced version, would
/// leave that version as the server's latest, so nothing changes until it is
/// fixed. Such a row may be stored without being shown, so the message says so.
fn ready_to_seal(lib: &Library) -> Result<(), String> {
    too_large(lib)?;
    match lib.unadvanceable_live_rows().map_err(|_| SEAL_READ_ERROR.to_string())? {
        0 => Ok(()),
        1 => Err("1 synced item has a clock that can't be advanced, so it can't be encrypted. It's stored on this device even if it isn't shown.".into()),
        n => Err(format!("{n} synced items have clocks that can't be advanced, so they can't be encrypted. They're stored on this device even if they aren't shown.")),
    }
}

/// Resetting seals every live row again too, and a row too large once sealed
/// must be fixed first. Unlock and Reset skip rows whose clock can't advance
/// and report them instead, so a locked device always has a way out; Unlock
/// keeps a too-large row locally with a visible reason when it would be
/// pushed, as on any encrypted device.
fn too_large(lib: &Library) -> Result<(), String> {
    let rows = lib.store.all_entities().map_err(|_| SEAL_READ_ERROR.to_string())?;
    let large: Vec<&str> = rows.iter().filter(|row| !row.deleted && !sealed_fits(row.payload.len())).map(|row| kind_label(&row.kind)).collect();
    if large.is_empty() { return Ok(()); }
    let mut labels = large.clone();
    labels.sort_unstable();
    labels.dedup();
    let (count, them) = if large.len() == 1 { ("1 item is".to_string(), "it") } else { (format!("{} items are", large.len()), "them") };
    Err(format!("{count} too large to encrypt ({}). Shrink or delete {them}, then try again.", labels.join(", ")))
}

/// What this device skipped, ignored or couldn't seal again. Only counts are
/// kept, on disk until the user dismisses the notice, so a restart still
/// says what this device lacks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Counts {
    account: String,
    /// Synced versions that didn't decrypt here and weren't applied.
    #[serde(default)]
    skipped: u64,
    /// Unencrypted versions from another device that were never applied.
    #[serde(default)]
    ignored: u64,
    /// Live rows the latest re-seal had to leave as they were.
    #[serde(default)]
    unsealed: u64,
    /// The newest server sequence counted, so a pull from an earlier cursor
    /// (signing in again) never counts a version twice. Kept on Dismiss.
    #[serde(default)]
    through: u64,
}

impl Counts {
    fn read(lib: &Library, account: &str) -> Counts {
        lib.store.kv_get(NOTICE_KEY).ok().flatten()
            .and_then(|text| serde_json::from_str::<Counts>(&text).ok())
            .filter(|counts| counts.account == account)
            .unwrap_or_else(|| Counts { account: account.into(), ..Counts::default() })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum E2eeStatus {
    Off,
    /// A key exists and its recovery code is on screen; nothing is encrypted yet.
    Confirming,
    /// The account is encrypted but this device has no verified key.
    Locked,
    /// This device's record can't be parsed; nothing syncs until it reloads
    /// or the user resets.
    Unreadable,
    /// Reading this device's record failed; nothing syncs until it reloads.
    Unavailable,
    /// The record belongs to another account; nothing syncs for this one.
    OtherAccount,
    On,
    /// A new key's code is on screen; the current state holds until confirmed.
    Resetting,
}

enum State {
    Off,
    Confirming(Unlocked),
    Locked(Flag),
    Unreadable,
    Unavailable,
    OtherAccount,
    On(Unlocked),
}

/// Why a pulled page was refused whole. The caller acknowledges failure, so
/// the cursor stays put and nothing in the page is applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PageError {
    #[error("Sync is paused: this account's library is end-to-end encrypted. Enter its recovery code below.")]
    Locked,
    #[error("Sync stopped: a synced change failed its encryption check, so nothing was applied. Sync Now retries.")]
    Integrity,
    #[error("Sync is paused: this device's encryption state is unreadable, so nothing was applied. Try again or reset encryption below.")]
    Unreadable,
    #[error("Sync is paused: this device's encryption state couldn't be read, so nothing was applied. Try again below.")]
    Unavailable,
    #[error("Sync is paused. {}", OTHER_ACCOUNT)]
    OtherAccount,
    #[error("Can't save sync data; check disk access, then Sync Now.")]
    Storage,
}

/// One signed-in account's encryption state. It decides what may be pushed and
/// decrypts pulled pages before the library sees them, so library code only
/// ever handles JSON.
pub struct LibraryEncryption {
    account: String,
    state: State,
    /// A complete pull finished since the last connection (and, while
    /// confirming, since Turn On), so this device knows whether the account is
    /// encrypted before it uploads plaintext or adds its own key.
    caught_up: bool,
    /// Some complete pull finished in this session.
    synced_once: bool,
    /// The new key of a reset whose code is on screen.
    reset: Option<Unlocked>,
    /// The record's re-seal mark, mirrored: set by an unlock or a reset and
    /// cleared by the re-seal after the next complete pull.
    reseal: bool,
    /// What this device skipped, ignored or couldn't seal again; durable.
    counts: Counts,
    /// What a reset in this session did, or that another device's won.
    reset_note: Option<&'static str>,
    /// The notice for the UI, built from the note and the counts.
    notice: Option<String>,
    /// The newest marker clock a scan ahead of the held cursor has seen.
    newest_marker: Option<Hlc>,
    /// A scan reached the end of the stream since one was last needed.
    scan_done: bool,
    /// This device knows its key store holds no key for this account.
    key_absent: bool,
}

impl LibraryEncryption {
    fn with_state(account: &str, state: State) -> LibraryEncryption {
        LibraryEncryption {
            account: account.into(), state, caught_up: false, synced_once: false, reset: None, reseal: false,
            counts: Counts { account: account.into(), ..Counts::default() }, reset_note: None, notice: None,
            newest_marker: None, scan_done: false, key_absent: false,
        }
    }

    /// Restore this account's state. Another account's record is left intact
    /// and blocks this one; a stored key is used only after it matches.
    pub fn load(account: &str, lib: &Library, keys: &dyn KeyStore) -> (LibraryEncryption, Option<String>) {
        let flag = match read_flag(lib) {
            Ok(Some(flag)) if flag.account == account => Some(flag),
            Ok(Some(_)) => return (LibraryEncryption::with_state(account, State::OtherAccount), None),
            Ok(None) => None,
            // Fail closed: never assume an account is plaintext. A failed
            // read may succeed on Try Again; unparseable text won't.
            Err(FlagError::Read) => return (LibraryEncryption::with_state(account, State::Unavailable), None),
            Err(FlagError::Corrupt) => {
                let mut loaded = LibraryEncryption { counts: Counts::read(lib, account), ..LibraryEncryption::with_state(account, State::Unreadable) };
                loaded.refresh_notice();
                return (loaded, None);
            }
        };
        let mut reseal = flag.as_ref().is_some_and(|flag| flag.reseal);
        let (mut warning, mut key_absent) = (None, false);
        let state = match (flag, keys.load(account)) {
            (None, Ok(None)) => {
                key_absent = true;
                State::Off
            }
            (None, Ok(Some(_))) => {
                // A key without a record is an abandoned setup; it never
                // encrypted anything, so remove it rather than adopt it.
                match keys.clear(account) {
                    Ok(()) => key_absent = true,
                    Err(error) => warning = Some(error),
                }
                State::Off
            }
            (None, Err(error)) => {
                warning = Some(error);
                State::Off
            }
            (Some(mut flag), Ok(Some(key))) => {
                let unlocked = Unlocked::new(key);
                if flag.verifies(&unlocked) {
                    // Unlock and Reset name a key in the record before storing
                    // it, so a key the record doesn't name was stored without
                    // that step finishing: seal everything again under it.
                    if flag.key_id.as_ref() != Some(&unlocked.key_id) { reseal = true; }
                    // A stored key also ends a reset's way back.
                    if flag.key_id.as_ref() != Some(&unlocked.key_id) || flag.check.is_some() || flag.undo.is_some() {
                        flag = Flag { account: flag.account, key_id: Some(unlocked.key_id.clone()), check: None, reseal, undo: None, epoch: flag.epoch };
                        if let Err(error) = write_flag(lib, &flag) { warning = Some(error); }
                    }
                    State::On(unlocked)
                } else {
                    State::Locked(flag)
                }
            }
            (Some(flag), Ok(None)) => {
                key_absent = true;
                State::Locked(flag)
            }
            (Some(flag), Err(error)) => {
                warning = Some(error);
                State::Locked(flag)
            }
        };
        let mut loaded = LibraryEncryption { reseal, key_absent, counts: Counts::read(lib, account), ..LibraryEncryption::with_state(account, state) };
        loaded.refresh_notice();
        (loaded, warning)
    }

    /// The account this device's library is encrypted for, if any. A failed
    /// read stops a sign-in instead of guessing; an unparseable record names
    /// no account, and loads as Unreadable, which syncs nothing.
    pub fn owner(lib: &Library) -> Result<Option<String>, &'static str> {
        match read_flag(lib) {
            Ok(flag) => Ok(flag.map(|flag| flag.account)),
            Err(FlagError::Corrupt) => Ok(None),
            Err(FlagError::Read) => Err(SIGN_IN_STOPPED),
        }
    }

    /// The sync epoch of this account's record: `None` without a readable
    /// record for the account, `Some(None)` for one from before records
    /// named their epoch.
    pub(crate) fn record_epoch(lib: &Library, account: &str) -> Option<Option<u64>> {
        match read_flag(lib) {
            Ok(Some(flag)) if flag.account == account => Some(flag.epoch),
            _ => None,
        }
    }

    /// A record from before records named their epoch belongs to the epoch
    /// this device resumes its cursor in.
    pub fn adopt_epoch(lib: &Library, account: &str, epoch: u64) -> Result<(), String> {
        match read_flag(lib) {
            Ok(Some(flag)) if flag.account == account && flag.epoch.is_none() => write_flag(lib, &Flag { epoch: Some(epoch), ..flag }),
            _ => Ok(()),
        }
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    pub fn status(&self) -> E2eeStatus {
        if self.reset.is_some() { return E2eeStatus::Resetting; }
        match self.state {
            State::Off => E2eeStatus::Off,
            State::Confirming(_) => E2eeStatus::Confirming,
            State::Locked(_) => E2eeStatus::Locked,
            State::Unreadable => E2eeStatus::Unreadable,
            State::Unavailable => E2eeStatus::Unavailable,
            State::OtherAccount => E2eeStatus::OtherAccount,
            State::On(_) => E2eeStatus::On,
        }
    }

    pub fn caught_up(&self) -> bool {
        self.caught_up
    }

    /// Turn On can finish: a complete pull ran after it began.
    pub fn confirmable(&self) -> bool {
        matches!(self.state, State::Confirming(_)) && self.caught_up
    }

    /// The transport dropped or is reconnecting: uploads wait for another
    /// complete pull, which also says whether the account became encrypted
    /// or was reset under another key meanwhile.
    pub fn reconnecting(&mut self) {
        self.caught_up = false;
    }

    /// Why uploads are waiting, while they are.
    pub fn upload_hold(&self) -> Option<&'static str> {
        match self.state {
            State::Off | State::Confirming(_) | State::On(_) if !self.caught_up => Some(if self.synced_once {
                "Waiting to catch up with the server before uploading."
            } else {
                "Waiting for the first sync before uploading."
            }),
            _ => None,
        }
    }

    /// Locked with a server row to check a code against, so pulling the same
    /// refused page again gains nothing until the user acts.
    pub fn has_check_row(&self) -> bool {
        matches!(&self.state, State::Locked(flag) if flag.check.is_some())
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// The user dismissed the notice: forget its counts here and on disk.
    pub fn dismiss_notice(&mut self, lib: &Library) -> Result<(), String> {
        let counts = Counts { account: self.account.clone(), through: self.counts.through, ..Counts::default() };
        let text = serde_json::to_string(&counts).map_err(|_| "Couldn't clear the notice.".to_string())?;
        lib.store.kv_set(NOTICE_KEY, &text).map_err(|_| "Couldn't clear the notice; check disk access, then try again.".to_string())?;
        self.counts = counts;
        self.reset_note = None;
        self.refresh_notice();
        Ok(())
    }

    fn refresh_notice(&mut self) {
        let counts = &self.counts;
        let parts: Vec<String> = [
            self.reset_note.map(str::to_string),
            (counts.ignored > 0).then(|| ignored_notice(counts.ignored)),
            (counts.skipped > 0).then(|| undecryptable_notice(counts.skipped)),
            (counts.unsealed > 0).then(|| unsealed_notice(counts.unsealed)),
        ].into_iter().flatten().collect();
        self.notice = (!parts.is_empty()).then(|| parts.join(" "));
    }

    /// A key for this account may be stored here, so Sign Out, which removes
    /// it, asks first.
    pub fn may_hold_key(&self) -> bool {
        self.reset.is_some() || match self.state {
            State::On(_) => true,
            State::Locked(_) | State::Unreadable | State::Unavailable => !self.key_absent,
            State::Off | State::Confirming(_) | State::OtherAccount => false,
        }
    }

    /// A read-only scan to the end of the stream would tell this device what
    /// it needs: the account's newest marker while it can't apply pages, and
    /// before a reset is confirmed.
    pub fn needs_scan(&self) -> bool {
        !self.scan_done && (self.reset.is_some() || matches!(self.state, State::Locked(_) | State::Unreadable))
    }

    /// The account's newest marker may have changed since the last scan (a
    /// notice from the server, Sync Now or Unlock): the next scan looks again.
    pub fn rescan(&mut self) {
        self.scan_done = false;
    }

    /// The pending reset can be confirmed: a scan reached the end of the stream.
    pub fn reset_ready(&self) -> bool {
        self.reset.is_some() && self.scan_done
    }

    /// A page read ahead of the held cursor by a scan; nothing in it is
    /// applied. A locked device takes the account's newest marker as its
    /// check row, and a reset lands above it. `more` is false at the end of
    /// the stream.
    pub fn scan_page(&mut self, lib: &Library, changes: &[ServerChange], more: bool) {
        for c in changes.iter().map(|sc| &sc.change).filter(|c| is_marker(&c.kind, &c.id)) {
            if self.newest_marker.as_ref().is_none_or(|seen| c.hlc > *seen) { self.newest_marker = Some(c.hlc.clone()); }
        }
        if let State::Locked(flag) = &self.state {
            if let Some(check) = check_row(changes).filter(|row| flag.checks_better(row)) {
                let flag = Flag { check: Some(check), ..flag.clone() };
                // Best effort: a restart scans again.
                let _ = write_flag(lib, &flag);
                self.state = State::Locked(flag);
            }
        }
        if !more { self.scan_done = true; }
    }

    /// Create and store a key, then wait for the user to confirm its code.
    /// A storage failure leaves encryption off with nothing changed.
    pub fn begin(&mut self, lib: &Library, keys: &dyn KeyStore) -> Result<(), String> {
        if !matches!(self.state, State::Off) {
            return Err("Encryption is already set up for this account.".into());
        }
        if !self.caught_up {
            return Err("Wait for this device to finish syncing, then try again.".into());
        }
        ready_to_seal(lib)?;
        let key = LibraryKey::generate().map_err(|error| format!("Encryption is still off: {error}."))?;
        self.key_absent = false;
        keys.save(&self.account, &key).map_err(|error| format!("Encryption is still off. {error}"))?;
        self.state = State::Confirming(Unlocked::new(key));
        // Confirm needs a complete pull that starts now, so a key another
        // device uploaded meanwhile locks this one instead of competing.
        self.caught_up = false;
        Ok(())
    }

    /// Leave the library unencrypted and remove the unused key.
    pub fn cancel(&mut self, keys: &dyn KeyStore) -> Result<(), String> {
        if !matches!(self.state, State::Confirming(_)) {
            return Ok(());
        }
        self.state = State::Off;
        keys.clear(&self.account).map_err(|error| format!("Encryption is still off, but the unused key couldn't be removed. {error}"))?;
        self.key_absent = true;
        Ok(())
    }

    /// After the user saved the code: mark the account and queue every live
    /// row just above its own version, in one transaction, so the server's
    /// latest rows become ciphertext.
    pub fn confirm(&mut self, lib: &mut Library) -> Result<(), String> {
        let State::Confirming(unlocked) = &self.state else {
            return Err("Turn on encryption first.".into());
        };
        if !self.caught_up {
            return Err("Checking your other devices for changes. Try again in a moment.".into());
        }
        let text = flag_text(&stamped(Flag { account: self.account.clone(), key_id: Some(unlocked.key_id.clone()), check: None, reseal: false, undo: None, epoch: None }, lib))?;
        lib.transaction(|lib| {
            // Checked again here: a row may have grown while the code was shown.
            if let Err(refusal) = ready_to_seal(lib) { return Ok(Err(refusal)); }
            lib.supersede_live_rows()?;
            lib.put_payload(MARKER_KIND, MARKER_ID, MARKER_PLAINTEXT.into())?;
            lib.store.kv_set(FLAG_KEY, &text)?;
            Ok(Ok(()))
        }).map_err(|error| format!("Couldn't turn on encryption ({error}). Nothing was changed; check disk access, then try again or Cancel."))??;
        let State::Confirming(unlocked) = std::mem::replace(&mut self.state, State::Off) else { unreachable!() };
        self.state = State::On(unlocked);
        Ok(())
    }

    /// Verify a recovery code against this library before storing anything.
    /// Everything this device has is sealed again under it after the next
    /// complete pull, including rows it uploaded in plaintext or under an
    /// earlier key; a row whose clock can't advance is skipped and reported.
    /// Call it after a scan that started once the code was entered, so the
    /// code is checked against the account's newest marker.
    pub fn unlock(&mut self, code: &str, lib: &Library, keys: &dyn KeyStore) -> Result<(), String> {
        self.unlock_after(code, lib, keys, true)
    }

    /// Unlock when that scan couldn't run: a code that doesn't verify here may
    /// still match a newer marker, so it is never called wrong.
    pub fn unlock_unchecked(&mut self, code: &str, lib: &Library, keys: &dyn KeyStore) -> Result<(), String> {
        self.unlock_after(code, lib, keys, false)
    }

    fn unlock_after(&mut self, code: &str, lib: &Library, keys: &dyn KeyStore, scanned: bool) -> Result<(), String> {
        let refused = |message: &str| -> String { if scanned { message } else { UNCHECKED }.into() };
        let State::Locked(flag) = &self.state else {
            return Err("This library isn't locked.".into());
        };
        let key = RecoveryCode::parse(code).map_err(|error| error.to_string())?;
        if flag.key_id.is_none() && flag.check.is_none() {
            return Err(refused(NO_CHECK_ROW));
        }
        let unlocked = Unlocked::new(key);
        // A marker from a reset elsewhere has replaced this device's own
        // unsent one as the check row.
        let outrun = flag.undo.is_some() && !flag.checks_own_reset();
        if !flag.verifies(&unlocked) {
            if let Some(undo) = &flag.undo {
                // A code of this device's unsent resets that a later one
                // replaced, or a reset elsewhere outran. When the check row
                // is the marker kept in `elsewhere`, only the account's code
                // put it there (below), and these resets were dropped for it.
                if undo.resets.iter().any(|id| bool::from(unlocked.key_id.as_bytes().ct_eq(id.as_bytes()))) {
                    let dropped = undo.elsewhere.is_some() && flag.check == undo.elsewhere;
                    return Err(refused(if dropped { DROPPED } else if outrun { SUPERSEDED } else { REPLACED }));
                }
                // The code of a reset elsewhere that this device reset over
                // again: while that reset is unsent, it is dropped as when
                // outrun. The record names that reset's marker first, so a
                // failure or quit at any later step leaves the outrun case,
                // which the same code finishes below.
                if let Some(row) = undo.elsewhere.as_ref().filter(|row| row.opens(&self.account, &unlocked)) {
                    if outrun { return Err(refused(SUPERSEDED)); }
                    if undo.unsent(&lib.store)? {
                        let outran = Flag { check: Some(row.clone()), ..flag.clone() };
                        write_flag(lib, &outran)?;
                        self.state = State::Locked(outran);
                        return self.unlock_after(code, lib, keys, scanned);
                    }
                }
                // The code from before those resets, which never finished.
                if let Some(prior) = undo.prior(&self.account).filter(|prior| prior.verifies(&unlocked)) {
                    // A reset elsewhere has replaced it since.
                    if outrun || undo.elsewhere.is_some() { return Err(refused(USED_BEFORE)); }
                    if !undo.unsent(&lib.store)? { return Err(refused(RESET_SENT)); }
                    let undo = undo.clone();
                    return self.undo_reset(&undo, prior, unlocked, lib, keys);
                }
            }
            // In v1 a re-key elsewhere and a forged marker look the same; the
            // earlier key is never adopted either way.
            let earlier = flag.check.is_some() && flag.key_id.as_ref().is_some_and(|stored| bool::from(unlocked.key_id.as_bytes().ct_eq(stored.as_bytes())));
            return Err(refused(if earlier { USED_BEFORE } else { WRONG_CODE }));
        }
        // This device's own unfinished reset: without the scan, a reset
        // elsewhere since may have replaced it, and finishing would upload a
        // marker whose clock could still rank above that one.
        if !scanned && flag.checks_own_reset() { return Err(UNCHECKED_RESET.into()); }
        // The account's newest code: this device's unsent reset is dropped
        // without ever being uploaded.
        if let Some(undo) = flag.undo.as_ref().filter(|_| outrun) { undo.drop_marker(&lib.store)?; }
        let check = flag.check.clone();
        self.adopt(unlocked, check, lib, keys)
    }

    /// Store a verified key. The record names it, with the re-seal mark,
    /// before the key is stored, so a failed or interrupted save leaves this
    /// device locked and the same code still unlocks it. A reset's way back
    /// stays in the record until the key is stored.
    fn adopt(&mut self, unlocked: Unlocked, check: Option<CheckRow>, lib: &Library, keys: &dyn KeyStore) -> Result<(), String> {
        let (undo, epoch) = match &self.state { State::Locked(flag) => (flag.undo.clone(), flag.epoch), _ => (None, None) };
        let flag = stamped(Flag { account: self.account.clone(), key_id: Some(unlocked.key_id.clone()), check, reseal: true, undo, epoch }, lib);
        write_flag(lib, &flag)?;
        self.state = State::Locked(flag.clone());
        self.reseal = true;
        self.key_absent = false;
        keys.save(&self.account, &unlocked.key).map_err(|error| format!("The code is correct, but it couldn't be stored. {error}"))?;
        // Best effort: loading drops the way back too once the key is stored.
        if flag.undo.is_some() { let _ = write_flag(lib, &Flag { undo: None, ..flag }); }
        self.state = State::On(unlocked);
        self.caught_up = false;
        self.reset_note = None;
        self.refresh_notice();
        Ok(())
    }

    /// The code from before a reset that stopped before its key was stored,
    /// while the reset's marker hasn't been uploaded: put the record and the
    /// marker row back exactly, then unlock as before the reset.
    fn undo_reset(&mut self, undo: &Undo, prior: Flag, unlocked: Unlocked, lib: &Library, keys: &dyn KeyStore) -> Result<(), String> {
        undo.restore(&lib.store).map_err(|_| "Couldn't undo the reset; check disk access, then try again.".to_string())?;
        self.reseal = prior.reseal;
        self.state = State::Locked(prior.clone());
        // Locked before the reset: unlock as then.
        if prior.check.is_some() { return self.adopt(unlocked, prior.check, lib, keys); }
        // On before the reset: the restored record already names this key.
        self.key_absent = false;
        keys.save(&self.account, &unlocked.key).map_err(|error| format!("The code is correct, but it couldn't be stored. {error}"))?;
        self.state = State::On(unlocked);
        self.caught_up = false;
        self.reset_note = None;
        self.refresh_notice();
        Ok(())
    }

    /// The code on screen: a pending reset's new key, else this device's key.
    pub fn recovery_code(&self) -> Option<RecoveryCode> {
        if let Some(reset) = &self.reset { return Some(RecoveryCode::for_key(&reset.key)); }
        match &self.state {
            State::Confirming(unlocked) | State::On(unlocked) => Some(RecoveryCode::for_key(&unlocked.key)),
            State::Off | State::Locked(_) | State::Unreadable | State::Unavailable | State::OtherAccount => None,
        }
    }

    /// Start replacing the account's key with a new one from this device. The
    /// key stays in memory, and the current state in force, until confirmed.
    pub fn begin_reset(&mut self, lib: &Library) -> Result<(), String> {
        if !matches!(self.state, State::On(_) | State::Locked(_) | State::Unreadable) {
            return Err("Encryption isn't set up for this account.".into());
        }
        too_large(lib)?;
        let key = LibraryKey::generate().map_err(|error| format!("Nothing was reset: {error}."))?;
        self.reset = Some(Unlocked::new(key));
        // Confirm waits for a scan to the end of the stream, so the new
        // marker lands above the account's newest one.
        self.scan_done = false;
        Ok(())
    }

    pub fn cancel_reset(&mut self) {
        self.reset = None;
    }

    /// After the user saved the new code, and a scan reached the end of the
    /// stream: in one transaction put a new marker above every marker this
    /// device knows of, and record the new key with the re-seal mark, that
    /// marker, sealed, as its check row, and what the transaction replaced.
    /// Only then is the key stored. If that fails, the record and the marker
    /// row are put back exactly in the same call, so nothing changed: the new
    /// code stays on screen for Try Again, and Cancel keeps the current code.
    /// A crash before the key is stored leaves this device locked, never
    /// sealing under the old key: the new code finishes the reset, and the
    /// previous code undoes it while the new marker hasn't been uploaded.
    /// After the next complete pull, every live row this device has is sealed
    /// again under the new key. Nothing is deleted: rows this device can't
    /// read stay on the server until a device that has them enters the new
    /// code.
    pub fn confirm_reset(&mut self, lib: &mut Library, keys: &dyn KeyStore) -> Result<(), String> {
        let Some(new) = &self.reset else {
            return Err("Start the reset first.".into());
        };
        // Only after a failed save whose record couldn't be put back: the
        // record already names the new key, so only the save is retried.
        let recorded = matches!(&self.state, State::Locked(flag) if flag.key_id.as_deref() == Some(new.key_id.as_str()));
        let (flag, replaced) = match &self.state {
            State::Locked(flag) if recorded => (flag.clone(), None),
            _ => self.record_reset(lib).map(|(flag, replaced)| (flag, Some(replaced)))?,
        };
        let new = self.reset.as_ref().expect("checked above");
        self.key_absent = false;
        if let Err(error) = keys.save(&self.account, &new.key) {
            if !recorded {
                // Exactly what this reset replaced, including the way back
                // from an earlier unsent one.
                if replaced.is_some_and(|replaced| replaced.restore(&lib.store).is_ok()) { return Err(RESET_UNSAVED.into()); }
                // Nothing is sealed or uploaded until the new key is stored,
                // and after a restart either code applies as above.
                self.state = State::Locked(flag);
                self.reseal = true;
                self.caught_up = false;
            }
            return Err(format!("The reset is saved, but the new key couldn't be stored. {error} Try again; the code on screen still works."));
        }
        // Best effort: loading drops the way back too once the key is stored.
        if flag.undo.is_some() { let _ = write_flag(lib, &Flag { undo: None, ..flag }); }
        self.state = State::On(self.reset.take().expect("checked above"));
        self.reseal = true;
        self.caught_up = false;
        self.reset_note = Some(RESET_DONE);
        self.refresh_notice();
        Ok(())
    }

    /// Reset's transaction: put the new marker and the record naming the new
    /// key, with the way back. Returns that record and exactly what the
    /// transaction replaced. A reset over an earlier unsent one keeps that
    /// one's way back, so the account's code still undoes both.
    fn record_reset(&self, lib: &mut Library) -> Result<(Flag, Undo), String> {
        let new = self.reset.as_ref().expect("checked by the caller");
        if !self.scan_done { return Err(CHECKING.into()); }
        too_large(lib)?;
        let earlier = match &self.state {
            State::Locked(flag) => flag.undo.as_ref(),
            _ => None,
        };
        // A reset elsewhere has replaced the earlier one: keep its marker so
        // its code can still be checked.
        let outran = match &self.state {
            State::Locked(flag) if flag.undo.is_some() && !flag.checks_own_reset() => flag.check.clone().filter(|check| is_marker(&check.kind, &check.id)),
            _ => None,
        };
        // The newest marker this device knows of: applied, seen while
        // locked, or seen by the scan.
        let seen = match &self.state {
            State::Locked(Flag { check: Some(check), .. }) if is_marker(&check.kind, &check.id) => Some(check.hlc.clone()),
            _ => None,
        };
        let local = lib.store.entity_hlc(MARKER_KIND, MARKER_ID).map_err(|_| SEAL_READ_ERROR.to_string())?;
        // Of those, the newest from the server: an earlier unsent reset's own
        // marker never reached it.
        let own = |hlc: &Hlc| earlier.is_some_and(|undo| *hlc == undo.hlc);
        let above = local.iter().chain(&seen).filter(|hlc| !own(hlc)).cloned()
            .chain(self.newest_marker.clone()).chain(earlier.and_then(|undo| undo.above.clone())).max();
        let floor = local.into_iter().chain(seen).chain(self.newest_marker.clone()).max();
        let hlc = lib.clock_above(floor.as_ref()).ok_or("Nothing was reset: the encryption marker's sync clock can't be advanced.")?;
        let binding = Binding { account_id: &self.account, kind: MARKER_KIND, id: MARKER_ID, hlc: &hlc };
        let payload = seal(&new.cipher, &binding, MARKER_PLAINTEXT).map_err(|error| format!("Nothing was reset: {error}."))?;
        let check = CheckRow { kind: MARKER_KIND.into(), id: MARKER_ID.into(), hlc: hlc.clone(), payload };
        let replaced = Undo::read(&lib.store, hlc.clone())?;
        let resets = earlier.map_or_else(Vec::new, |undo| undo.resets.clone()).into_iter().chain([new.key_id.clone()]).collect();
        let undo = match earlier {
            Some(earlier) => Undo { hlc: hlc.clone(), above, resets, elsewhere: outran.or_else(|| earlier.elsewhere.clone()), ..earlier.clone() },
            None => Undo { above, resets, ..replaced.clone() },
        };
        let flag = stamped(Flag { account: self.account.clone(), key_id: Some(new.key_id.clone()), check: Some(check), reseal: true, undo: Some(undo), epoch: None }, lib);
        let text = flag_text(&flag)?;
        lib.transaction(|lib| {
            lib.put_payload_at(MARKER_KIND, MARKER_ID, MARKER_PLAINTEXT.into(), hlc)?;
            lib.store.kv_set(FLAG_KEY, &text)?;
            Ok(())
        }).map_err(|error| format!("Couldn't reset encryption ({error}). Nothing was changed; check disk access, then try again or Cancel."))?;
        Ok((flag, replaced))
    }

    /// The next page to push. Encrypted accounts never receive plaintext live
    /// rows; a locked device holds its changes locally. Every device waits for
    /// a complete pull after connecting, so it learns of encryption, or of a
    /// reset under another key, before it uploads. Rows too large once
    /// encrypted are paused with a visible reason, not dropped.
    pub fn outgoing(&self, lib: &Library, limit: usize) -> Result<Vec<Change>, String> {
        const READ_ERROR: &str = "Couldn't read sync queue; local data kept. Sync Now retries.";
        match &self.state {
            State::Locked(_) | State::Unreadable | State::Unavailable | State::OtherAccount => Ok(Vec::new()),
            _ if !self.caught_up => Ok(Vec::new()),
            State::Off | State::Confirming(_) => {
                let changes = lib.store.sync_changes(limit).map_err(|_| READ_ERROR.to_string())?;
                // A marker left from another session is never uploaded unsealed.
                Ok(changes.into_iter().filter(|c| c.kind != MARKER_KIND).collect())
            }
            State::On(unlocked) => {
                // The marker goes first, so other devices learn of a new key
                // before any row sealed with it.
                let marker = lib.store.sync_change(MARKER_KIND, MARKER_ID).map_err(|_| READ_ERROR.to_string())?;
                let rows = lib.store.sync_changes(limit).map_err(|_| READ_ERROR.to_string())?;
                let mut outgoing = Vec::new();
                for change in marker.into_iter().chain(rows.into_iter().filter(|c| !is_marker(&c.kind, &c.id))).take(limit) {
                    if change.deleted {
                        outgoing.push(Change { enc: Encoding::Json, payload: String::new(), ..change });
                        continue;
                    }
                    let binding = Binding { account_id: &self.account, kind: &change.kind, id: &change.id, hlc: &change.hlc };
                    match seal(&unlocked.cipher, &binding, &change.payload) {
                        Ok(payload) => outgoing.push(Change { enc: Encoding::Xchacha20V1, payload, ..change }),
                        Err(E2eeError::TooLarge) => {
                            lib.store.reject_sync_change(&change, "Encrypted data over 64 KiB; kept locally. Edit it, then Sync Now.")
                                .map_err(|_| "Couldn't save sync queue; kept locally. Sync Now retries.".to_string())?;
                        }
                        Err(error) => return Err(format!("Couldn't encrypt changes ({error}); nothing was sent. Sync Now retries.")),
                    }
                }
                Ok(outgoing)
            }
        }
    }

    /// Decrypt and commit a pulled page with its cursor, or refuse it whole.
    pub fn apply_page(&mut self, lib: &mut Library, changes: &[ServerChange], cursor: u64, more: bool) -> Result<usize, PageError> {
        let applied = match &self.state {
            State::Unreadable => {
                // Look past the refused page again: a reset needs the newest marker.
                self.scan_done = false;
                return Err(PageError::Unreadable);
            }
            State::Unavailable => return Err(PageError::Unavailable),
            State::OtherAccount => return Err(PageError::OtherAccount),
            State::Locked(flag) => {
                if let Some(check) = check_row(changes).filter(|row| flag.checks_better(row)) {
                    let flag = Flag { check: Some(check), ..flag.clone() };
                    // Best effort: the page stays refused either way.
                    if write_flag(lib, &flag).is_ok() { self.state = State::Locked(flag); }
                }
                // The account's newest marker may lie past the refused page.
                self.scan_done = false;
                return Err(PageError::Locked);
            }
            State::Off | State::Confirming(_) => {
                if changes.iter().any(|sc| is_marker(&sc.change.kind, &sc.change.id) && !sc.change.deleted && sc.change.enc != Encoding::Xchacha20V1) {
                    return Err(PageError::Integrity);
                }
                if let Some(check) = check_row(changes) {
                    // Encrypted elsewhere: never apply ciphertext or push plaintext.
                    // An unconfirmed local key stays unused until a code verifies.
                    let flag = stamped(Flag { account: self.account.clone(), key_id: None, check: Some(check), reseal: false, undo: None, epoch: None }, lib);
                    let saved = write_flag(lib, &flag);
                    self.state = State::Locked(flag);
                    self.scan_done = false;
                    return Err(if saved.is_ok() { PageError::Locked } else { PageError::Storage });
                }
                let page: Vec<ServerChange> = changes.iter().filter(|sc| !is_marker(&sc.change.kind, &sc.change.id)).cloned().collect();
                // Once the whole stream is in, items follow profile changes that crossed.
                lib.apply_sync_page_superseding(&page, &[], &[], cursor, |lib| if more { Ok(()) } else { lib.reconcile_profiles().map(drop) })
                    .map_err(|_| PageError::Storage)?
            }
            State::On(_) => self.apply_encrypted(lib, changes, cursor, more)?,
        };
        if !more {
            if self.reseal { self.reseal(lib)?; }
            self.caught_up = true;
            self.synced_once = true;
        }
        Ok(applied)
    }

    /// After an unlock or a reset, once this device has caught up: queue every
    /// live row again just above its own version, sealed under this key, and
    /// clear the mark, in one transaction.
    fn reseal(&mut self, lib: &mut Library) -> Result<(), PageError> {
        let State::On(unlocked) = &self.state else { return Ok(()) };
        let text = flag_text(&stamped(Flag { account: self.account.clone(), key_id: Some(unlocked.key_id.clone()), check: None, reseal: false, undo: None, epoch: None }, lib)).map_err(|_| PageError::Storage)?;
        let earlier = &self.counts;
        let counts = lib.transaction(|lib| {
            // Rows whose clock can't advance are left as they are and counted.
            let counts = Counts { unsealed: lib.supersede_live_rows()? as u64, ..earlier.clone() };
            if counts != *earlier { lib.store.kv_set(NOTICE_KEY, &serde_json::to_string(&counts)?)?; }
            lib.store.kv_set(FLAG_KEY, &text)?;
            Ok(counts)
        }).map_err(|_| PageError::Storage)?;
        self.reseal = false;
        self.counts = counts;
        self.refresh_notice();
        Ok(())
    }

    /// Only rows that would win last-writer-wins can change this device, so
    /// only they are checked. Plaintext winners are never applied: this
    /// device's version is queued again above them (a row it never had is
    /// left alone). A sealed row that doesn't open is neither applied nor
    /// superseded, the same as if the server had withheld it; the rest of the
    /// page applies. Only a marker under another key stops this device.
    fn apply_encrypted(&mut self, lib: &mut Library, changes: &[ServerChange], cursor: u64, more: bool) -> Result<usize, PageError> {
        let State::On(unlocked) = &self.state else { unreachable!() };
        let mut latest = BTreeMap::<(&str, &str), Option<Hlc>>::new();
        let mut winners = Vec::new();
        for sc in changes {
            let c = &sc.change;
            if sc.device_id.is_empty() && c.kind.is_empty() { continue; }
            let newest = match latest.entry((c.kind.as_str(), c.id.as_str())) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => entry.insert(lib.store.entity_hlc(&c.kind, &c.id).map_err(|_| PageError::Storage)?),
            };
            if newest.as_ref().is_some_and(|local| *local >= c.hlc) { continue; }
            *newest = Some(c.hlc.clone());
            winners.push(sc);
        }
        let supersede = |c: &Change, missing: Missing| Supersede { kind: c.kind.clone(), id: c.id.clone(), over: c.hlc.clone(), missing };
        let decrypted = |sc: &ServerChange, payload: String| ServerChange { change: Change { enc: Encoding::Json, payload, ..sc.change.clone() }, ..sc.clone() };
        let (mut page, mut superseded, mut ignored, mut undecryptable) = (Vec::new(), Vec::new(), 0, Vec::new());
        // Versions at or below `through` were counted on an earlier pull.
        let (mut skipped, mut through) = (0, self.counts.through);
        // The marker first: a new key elsewhere locks this device before any
        // row sealed with that key is taken for tampering.
        for sc in winners.iter().copied().filter(|sc| is_marker(&sc.change.kind, &sc.change.id)) {
            let c = &sc.change;
            let sealed = !c.deleted && c.enc == Encoding::Xchacha20V1;
            let ours = sealed && open_row(&self.account, &unlocked.cipher, &c.kind, &c.id, &c.hlc, &c.payload).is_ok_and(|text| text == MARKER_PLAINTEXT);
            if ours {
                page.push(decrypted(sc, MARKER_PLAINTEXT.into()));
            } else if sealed {
                // Another device set up a different key, or reset encryption.
                // The key id stays, so this device's earlier code is named as such.
                let flag = stamped(Flag { account: self.account.clone(), key_id: Some(unlocked.key_id.clone()), check: Some(CheckRow::of(c)), reseal: self.reseal, undo: None, epoch: None }, lib);
                let saved = write_flag(lib, &flag);
                self.state = State::Locked(flag);
                self.scan_done = false;
                // A reset this device made in this session lost after all.
                if self.reset_note.is_some() {
                    self.reset_note = Some(OUTRUN);
                    self.refresh_notice();
                }
                return Err(if saved.is_ok() { PageError::Locked } else { PageError::Storage });
            } else {
                // Deletions and plaintext are not authenticated in v1: they
                // never turn encryption off. This device puts its marker back.
                superseded.push(supersede(c, Missing::Put(MARKER_PLAINTEXT.into())));
            }
        }
        for sc in winners.iter().copied().filter(|sc| !is_marker(&sc.change.kind, &sc.change.id)) {
            let c = &sc.change;
            if c.deleted {
                page.push(decrypted(sc, String::new()));
                continue;
            }
            let opened = (c.enc == Encoding::Xchacha20V1).then(|| open_row(&self.account, &unlocked.cipher, &c.kind, &c.id, &c.hlc, &c.payload));
            let new = sc.seq > self.counts.through;
            match opened {
                Some(Ok(payload)) => page.push(decrypted(sc, payload)),
                // Another key, or altered: skipped, never applied or replaced.
                // Its clock is still observed, so a later edit here sorts above it.
                Some(Err(_)) => {
                    undecryptable.push(c.hlc.clone());
                    skipped += u64::from(new);
                    through = through.max(sc.seq);
                }
                None => {
                    superseded.push(supersede(c, Missing::Skip));
                    ignored += u64::from(new);
                    through = through.max(sc.seq);
                }
            }
        }
        // The counts add up across pages and commit with the page.
        let counts = Counts { skipped: self.counts.skipped + skipped, ignored: self.counts.ignored + ignored, through, ..self.counts.clone() };
        let changed = counts != self.counts;
        let text = serde_json::to_string(&counts).map_err(|_| PageError::Storage)?;
        let applied = lib.apply_sync_page_superseding(&page, &superseded, &undecryptable, cursor, |lib| {
            if changed { lib.store.kv_set(NOTICE_KEY, &text)?; }
            // Once the whole stream is in, items follow profile changes that crossed.
            if !more { lib.reconcile_profiles()?; }
            Ok(())
        }).map_err(|_| PageError::Storage)?;
        if changed {
            self.counts = counts;
            self.refresh_notice();
        }
        Ok(applied)
    }

    /// The user's own Sign Out: remove this account's key from this device.
    /// The record stays, so signing in again starts Locked and never uploads
    /// plaintext; the recovery code unlocks it.
    pub fn forget_key(account: &str, keys: &dyn KeyStore) -> Result<(), String> {
        keys.clear(account)
    }

    /// The account's sync data was deleted from the server, so the key and
    /// recovery codes that protected it are retired: this account's record,
    /// its notice and the local marker row go, in the caller's transaction,
    /// so turning encryption on again starts a new key. Another account's
    /// record, or one that can't be read, stays.
    pub(crate) fn retire(account: &str, lib: &Library) -> crate::library::Result<()> {
        if matches!(read_flag(lib), Ok(Some(flag)) if flag.account == account) {
            lib.store.kv_delete(FLAG_KEY)?;
            lib.store.put_raw_entity(MARKER_KIND, MARKER_ID, None)?;
        }
        let notice = lib.store.kv_get(NOTICE_KEY)?.and_then(|text| serde_json::from_str::<Counts>(&text).ok());
        if notice.is_some_and(|counts| counts.account == account) {
            lib.store.kv_delete(NOTICE_KEY)?;
        }
        Ok(())
    }
}

/// The best row to verify a code against: the newest marker, else the newest
/// sealed row.
fn check_row(changes: &[ServerChange]) -> Option<CheckRow> {
    changes.iter().filter(|sc| !sc.change.deleted && sc.change.enc == Encoding::Xchacha20V1)
        .map(|sc| CheckRow::of(&sc.change))
        .max_by(|a, b| a.rank().cmp(&b.rank()))
}

fn ignored_notice(count: u64) -> String {
    let what = if count == 1 { "an unencrypted change".to_string() } else { format!("{count} unencrypted changes") };
    format!("Ignored {what} from another device. This library is end-to-end encrypted, so unencrypted changes are never applied.")
}

fn unsealed_notice(count: u64) -> String {
    if count == 1 {
        "1 item has a sync clock that can't be advanced, so it wasn't sealed again under this recovery code.".into()
    } else {
        format!("{count} items have sync clocks that can't be advanced, so they weren't sealed again under this recovery code.")
    }
}

fn undecryptable_notice(count: u64) -> String {
    if count == 1 {
        "1 synced item couldn't be decrypted on this device and wasn't applied. It may come from a device still using an earlier recovery code, or it may have been altered.".into()
    } else {
        format!("{count} synced items couldn't be decrypted on this device and weren't applied. They may come from a device still using an earlier recovery code, or they may have been altered.")
    }
}

#[cfg(test)]
#[path = "e2ee_tests.rs"]
mod tests;
