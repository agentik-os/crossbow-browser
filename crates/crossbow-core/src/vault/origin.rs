//! Origins are stored as `scheme://host[:port]`, the unit a browser's
//! same-origin checks use.

use super::{MAX_ORIGIN_BYTES, VaultError};

const NOT_WEB: VaultError = VaultError::Invalid("Enter a website address that starts with https:// or http://.");
const MALFORMED: VaultError = VaultError::Invalid("Check the Website field. Enter an address like https://example.com.");
const DOTLESS: VaultError = VaultError::Invalid("Enter the full site address, like bank.example.");

/// Normalise what the user typed to an `http` or `https` origin: a missing
/// scheme means https, the host is lowercased and IDNA-encoded, the default
/// port and everything after the host are dropped. User info is refused so a
/// pasted `https://user:pass@host` never stores a password in the origin.
pub fn normalize_origin(input: &str) -> Result<String, VaultError> {
    let typed = input.trim();
    if typed.is_empty() {
        return Err(VaultError::Invalid("Enter the website this password is for."));
    }
    if typed.len() > MAX_ORIGIN_BYTES {
        return Err(VaultError::Invalid("That website address is too long."));
    }
    // Without "://", "example.com:8080" would parse as a scheme. Anything
    // else that isn't a web address fails to parse once https:// is added.
    if mistyped_web_scheme(typed) || !typed.contains("://") && (mistyped_scheme(typed) || doubled_separator(typed)) {
        return Err(MALFORMED);
    }
    let lower = typed.to_ascii_lowercase();
    let explicit = lower.starts_with("https://") || lower.starts_with("http://");
    let url = if typed.contains("://") { url::Url::parse(typed) } else { url::Url::parse(&format!("https://{typed}")) }.map_err(|_| NOT_WEB)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return Err(NOT_WEB);
    }
    // "https", "https://https_" or "http://http." is a scheme typed as a host.
    if url.host_str().is_some_and(scheme_named) {
        return Err(MALFORMED);
    }
    // "htps/bank.example" or "router": without a typed scheme, a host with
    // no dot is a mistyped scheme or not a site address. localhost and IP
    // literals are hosts as typed; "http://router" is too.
    if !explicit && matches!(url.host(), Some(url::Host::Domain(host)) if !host.contains('.') && host != "localhost") {
        return Err(DOTLESS);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(VaultError::Invalid("Remove the user name and password from the website address."));
    }
    let origin = url.origin().ascii_serialization();
    if origin.len() > MAX_ORIGIN_BYTES {
        return Err(VaultError::Invalid("That website address is too long."));
    }
    Ok(origin)
}

/// `https//`, `http:/`, `https:`, `https_//` or `https \\` before a host: a web
/// scheme followed by anything but `://` and a host. Only punctuation before
/// a slash or backslash is a mistyped separator; a letter, digit or `.` first
/// continues a host name, as in `httpbin.org`, `https-portal.example` or
/// `http_proxy.example`, and so does a `-` or `_` with no slash after it.
fn mistyped_web_scheme(typed: &str) -> bool {
    let lower = typed.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") { return false; }
    let Some(rest) = lower.strip_prefix("https").or_else(|| lower.strip_prefix("http")) else { return false };
    let mut separator = rest.chars().take_while(|c| !(c.is_ascii_alphanumeric() || *c == '.'));
    separator.any(|c| matches!(c, '/' | '\\')) || rest.starts_with(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_')))
}

/// `ttps//bank.example` or `www.https//bank.example`: with no `://`, a doubled
/// slash is a mistyped scheme, whatever its letters, and would otherwise
/// store the letters before it as the host.
fn doubled_separator(typed: &str) -> bool {
    typed.as_bytes().windows(2).any(|pair| pair.iter().all(|b| matches!(b, b'/' | b'\\')))
}

/// A host that is `http` or `https` followed only by punctuation, as left by
/// `https.//` or `http://https-/`.
fn scheme_named(host: &str) -> bool {
    host.strip_prefix("https").or_else(|| host.strip_prefix("http")).is_some_and(|rest| !rest.contains(|c: char| c.is_ascii_alphanumeric()))
}

/// `https:/example.com` or `https:example.com` is a mistyped address, not a
/// host named "https". After a host name, a colon starts a port, which is a
/// digit; user info is refused later with its own message.
fn mistyped_scheme(typed: &str) -> bool {
    let Some((scheme, rest)) = typed.split_once(':') else { return false };
    let is_scheme = scheme.starts_with(|c: char| c.is_ascii_alphabetic()) && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    is_scheme && !rest.starts_with(|c: char| c.is_ascii_digit()) && !rest.contains('@')
}
