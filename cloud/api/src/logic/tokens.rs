//! Token, code, id and hash formats. Randomness is injected so the formats are
//! testable; the Worker feeds bytes from `getrandom` (Web Crypto).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use crossbow_proto::AUTH_CODE_LEN;
use sha2::{Digest, Sha256};

/// Session tokens are `cbs_` + base64url(32 random bytes), no padding.
pub const TOKEN_PREFIX: &str = "cbs_";
pub const TOKEN_RANDOM_BYTES: usize = 32;
/// base64url without padding of 32 bytes is 43 characters.
pub const TOKEN_LEN: usize = TOKEN_PREFIX.len() + 43;

/// Lowercase hex SHA-256.
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// `sha256(AUTH_PEPPER || challenge_id || code)` as hex. Only this is stored.
pub fn code_hash(pepper: &str, challenge_id: &str, code: &str) -> String {
    let mut h = Sha256::new();
    h.update(pepper.as_bytes());
    h.update(challenge_id.as_bytes());
    h.update(code.as_bytes());
    hex(&h.finalize())
}

/// `sha256(token)` as hex. Only this is stored.
pub fn token_hash(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

pub fn make_token(random: &[u8; TOKEN_RANDOM_BYTES]) -> String {
    format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(random))
}

/// Cheap shape check before any database lookup.
pub fn token_well_formed(token: &str) -> bool {
    token.len() == TOKEN_LEN
        && token.starts_with(TOKEN_PREFIX)
        && token[TOKEN_PREFIX.len()..]
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// Extract the token from an `Authorization: Bearer <token>` header value.
pub fn bearer_token(header: Option<&str>) -> Option<&str> {
    let value = header?.trim();
    let (scheme, rest) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = rest.trim();
    token_well_formed(token).then_some(token)
}

/// Largest multiple of 10^6 that fits in u32; values at or above it are
/// rejected so every code is equally likely.
const CODE_SPACE: u32 = 1_000_000;
const CODE_REJECT_AT: u32 = (u32::MAX / CODE_SPACE) * CODE_SPACE;

/// Six decimal digits from uniformly random u32 draws (rejection sampling).
pub fn make_code(mut next_u32: impl FnMut() -> u32) -> String {
    loop {
        let v = next_u32();
        if v < CODE_REJECT_AT {
            return format!("{:0width$}", v % CODE_SPACE, width = AUTH_CODE_LEN);
        }
    }
}

pub fn code_well_formed(code: &str) -> bool {
    code.len() == AUTH_CODE_LEN && code.bytes().all(|c| c.is_ascii_digit())
}

/// Opaque identifiers: a short prefix plus 128 random bits in hex.
pub fn make_id(prefix: &str, random: &[u8; 16]) -> String {
    format!("{prefix}_{}", hex(random))
}

/// `[A-Za-z0-9_]{1,64}`: shape of ids we hand out, checked on path parameters.
pub fn id_well_formed(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

/// Constant-time equality for equal-length secrets (hex digests).
pub fn ct_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn code_hash_is_concatenation() {
        assert_eq!(code_hash("pep", "chl_1", "123456"), sha256_hex(b"pepchl_1123456"));
        assert_ne!(code_hash("pep", "chl_1", "123456"), code_hash("pep2", "chl_1", "123456"));
    }

    #[test]
    fn token_format_roundtrip() {
        let t = make_token(&[0xAB; 32]);
        assert!(t.starts_with("cbs_"));
        assert_eq!(t.len(), TOKEN_LEN);
        assert!(token_well_formed(&t));
        assert!(!token_well_formed("cbs_short"));
        assert!(!token_well_formed(&t.replace("cbs_", "xyz_")));
        let bad = format!("{}!", &t[..t.len() - 1]);
        assert!(!token_well_formed(&bad));
        assert_eq!(token_hash(&t).len(), 64);
    }

    #[test]
    fn bearer_parsing() {
        let t = make_token(&[7; 32]);
        assert_eq!(bearer_token(Some(&format!("Bearer {t}"))), Some(t.as_str()));
        assert_eq!(bearer_token(Some(&format!("bearer   {t} "))), Some(t.as_str()));
        assert_eq!(bearer_token(Some(&format!("Basic {t}"))), None);
        assert_eq!(bearer_token(Some("Bearer nope")), None);
        assert_eq!(bearer_token(None), None);
    }

    #[test]
    fn codes_are_six_digits_and_unbiased_by_rejection() {
        let mut draws = vec![u32::MAX, CODE_REJECT_AT, 42].into_iter();
        let code = make_code(|| draws.next().unwrap());
        assert_eq!(code, "000042");
        assert!(code_well_formed(&code));
        assert_eq!(make_code(|| CODE_REJECT_AT - 1), "999999");
        assert!(!code_well_formed("12345"));
        assert!(!code_well_formed("12345a"));
    }

    #[test]
    fn ids() {
        let id = make_id("dev", &[0x0f; 16]);
        assert_eq!(id, "dev_0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f");
        assert!(id_well_formed(&id));
        assert!(!id_well_formed("../x"));
        assert!(!id_well_formed(""));
    }

    #[test]
    fn constant_time_eq() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "ab"));
    }
}
