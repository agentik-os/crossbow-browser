//! Vault envelope v1: XChaCha20-Poly1305 per field, keyed by an HKDF-SHA256
//! subkey of the vault's own random key, never of the library key.

use super::VaultError;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use sha2::Sha256;
use std::fmt;
use zeroize::{Zeroize, Zeroizing};

pub(super) const ENVELOPE_VERSION: u8 = 1;
/// The only vault key epoch this client reads or writes.
pub(super) const VAULT_EPOCH: u32 = 1;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const HEADER_LEN: usize = 1 + 4 + NONCE_LEN;
const AAD_DOMAIN: &[u8] = b"crossbow.vault.entry.v1";
const HKDF_SALT: &[u8] = b"crossbow.vault.v1";
const SUBKEY_DOMAIN: &[u8] = b"crossbow.vault.subkey.v1";
/// The same purpose label library E2EE reserves for the vault.
const PURPOSE: &[u8] = b"vault";

/// The vault key: 32 bytes from the OS CSPRNG, boxed so moves never copy it,
/// and wiped on drop. A distinct type from the library key, so one can never
/// be passed where the other is expected.
pub struct VaultKey(Box<Zeroizing<[u8; 32]>>);

impl VaultKey {
    pub fn generate() -> Result<VaultKey, VaultError> {
        let mut bytes = Box::new(Zeroizing::new([0u8; 32]));
        fill_random(bytes.as_mut_slice())?;
        Ok(VaultKey(bytes))
    }

    pub fn from_slice(bytes: &[u8]) -> Option<VaultKey> {
        let mut key = Box::new(Zeroizing::new([0u8; 32]));
        if bytes.len() != key.len() { return None; }
        key.copy_from_slice(bytes);
        Some(VaultKey(key))
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The field cipher for the current epoch. Derive it once per unlock.
    pub(super) fn cipher(&self) -> VaultCipher {
        let mut subkey = Zeroizing::new([0u8; 32]);
        let info = length_prefixed(&[SUBKEY_DOMAIN, PURPOSE, &VAULT_EPOCH.to_be_bytes()]);
        Hkdf::<Sha256>::new(Some(HKDF_SALT), self.as_bytes()).expand(&info, subkey.as_mut_slice()).expect("32 bytes is a valid HKDF-SHA256 length");
        VaultCipher(Box::new(XChaCha20Poly1305::new(Key::from_slice(subkey.as_slice()))))
    }
}

impl fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VaultKey(..)")
    }
}

/// The vault subkey as a ready AEAD, boxed so it is never copied by a move.
/// The AEAD wipes its key when dropped.
pub(super) struct VaultCipher(Box<XChaCha20Poly1305>);

impl fmt::Debug for VaultCipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VaultCipher(..)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Field {
    Origin,
    Username,
    Password,
    Notes,
    Check,
    Profile,
    /// A saved address, every part in one envelope.
    Address,
    /// The profile a saved address belongs to.
    AddressProfile,
    /// A saved card's details: name, nickname, expiry, network and last four digits.
    Card,
    /// A saved card's number, sealed on its own.
    CardNumber,
    /// The profile a saved card belongs to.
    CardProfile,
}

impl Field {
    pub(super) fn column(self) -> &'static str {
        match self {
            Field::Origin => "origin",
            Field::Username => "username",
            Field::Password => "password",
            Field::Notes => "notes",
            Field::Check => "check",
            Field::Profile => "profile",
            Field::Address => "address",
            Field::AddressProfile => "address_profile",
            Field::Card => "card",
            Field::CardNumber => "card_number",
            Field::CardProfile => "card_profile",
        }
    }
}

/// What a sealed field is bound to. Moving it to another vault, entry or
/// field, or opening it for another origin, fails authentication. `origin` is
/// the normalised origin for username, password and notes, and empty for the
/// origin itself and the check value.
#[derive(Clone, Copy)]
pub(super) struct Binding<'a> {
    pub vault_id: &'a str,
    pub entry_id: &'a str,
    pub field: Field,
    pub origin: &'a str,
}

impl fmt::Debug for Binding<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Binding").field("entry_id", &self.entry_id).field("field", &self.field).finish_non_exhaustive()
    }
}

fn length_prefixed(fields: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(fields.iter().map(|f| 4 + f.len()).sum());
    for field in fields {
        out.extend_from_slice(&(field.len() as u32).to_be_bytes());
        out.extend_from_slice(field);
    }
    out
}

/// The AAD holds an origin, which is secret, so it is wiped after use.
fn aad(binding: &Binding<'_>, epoch: u32) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(length_prefixed(&[AAD_DOMAIN, binding.vault_id.as_bytes(), binding.entry_id.as_bytes(), binding.field.column().as_bytes(), binding.origin.as_bytes(), &epoch.to_be_bytes()]))
}

/// Bytes from the OS CSPRNG. This crate's tests may fix them instead
/// (`crate::test_script`), so that a vault file is the same on every run.
fn fill_random(bytes: &mut [u8]) -> Result<(), VaultError> {
    #[cfg(test)]
    if crate::test_script::fill(bytes) { return Ok(()); }
    getrandom::fill(bytes).map_err(|_| VaultError::Random)
}

/// Seal one field with a fresh random nonce.
pub(super) fn seal(cipher: &VaultCipher, binding: &Binding<'_>, plaintext: &str) -> Result<Vec<u8>, VaultError> {
    let mut nonce = [0u8; NONCE_LEN];
    fill_random(&mut nonce)?;
    seal_with_nonce(cipher, binding, plaintext, &nonce)
}

fn seal_with_nonce(cipher: &VaultCipher, binding: &Binding<'_>, plaintext: &str, nonce: &[u8; NONCE_LEN]) -> Result<Vec<u8>, VaultError> {
    let sealed = cipher.0.encrypt(XNonce::from_slice(nonce), Payload { msg: plaintext.as_bytes(), aad: &aad(binding, VAULT_EPOCH) })
        .map_err(|_| VaultError::Random)?;
    let mut envelope = Vec::with_capacity(HEADER_LEN + sealed.len());
    envelope.push(ENVELOPE_VERSION);
    envelope.extend_from_slice(&VAULT_EPOCH.to_be_bytes());
    envelope.extend_from_slice(nonce);
    envelope.extend_from_slice(&sealed);
    Ok(envelope)
}

/// Why a sealed field didn't open. Callers map every case to a fail-closed error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OpenError {
    Truncated,
    /// Any version or epoch but the current one. A newer envelope only ever
    /// comes with a newer file format, which is refused before any field is
    /// opened, so inside a file this client reads, another value is damage.
    BadHeader,
    Integrity,
    NotText,
}

/// Open a field for exactly this binding, or fail closed.
pub(super) fn open(cipher: &VaultCipher, binding: &Binding<'_>, envelope: &[u8]) -> Result<Zeroizing<String>, OpenError> {
    if envelope.len() < HEADER_LEN + TAG_LEN {
        return Err(OpenError::Truncated);
    }
    let epoch = u32::from_be_bytes([envelope[1], envelope[2], envelope[3], envelope[4]]);
    if envelope[0] != ENVELOPE_VERSION || epoch != VAULT_EPOCH {
        return Err(OpenError::BadHeader);
    }
    let plaintext = cipher.0.decrypt(XNonce::from_slice(&envelope[5..HEADER_LEN]), Payload { msg: &envelope[HEADER_LEN..], aad: &aad(binding, epoch) })
        .map_err(|_| OpenError::Integrity)?;
    match String::from_utf8(plaintext) {
        Ok(text) => Ok(Zeroizing::new(text)),
        Err(error) => {
            error.into_bytes().zeroize();
            Err(OpenError::NotText)
        }
    }
}

#[cfg(test)]
pub(super) fn seal_fixed(cipher: &VaultCipher, binding: &Binding<'_>, plaintext: &str, nonce: &[u8; NONCE_LEN]) -> Result<Vec<u8>, VaultError> {
    seal_with_nonce(cipher, binding, plaintext, nonce)
}
