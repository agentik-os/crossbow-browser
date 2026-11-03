//! Browser sign-in with Clerk: an OpenID Connect authorization code with
//! PKCE that the desktop app receives on a loopback address (RFC 8252) and
//! sends here. The Worker redeems it at the issuer's token endpoint, checks
//! the ID token and maps the identity to an account. These are the parts
//! that don't touch the runtime.
//!
//! The ID token comes straight from the issuer's token endpoint over TLS,
//! which OpenID Connect Core 3.1.3.7 accepts in place of checking its
//! signature; the Worker checks the RS256 signature against the issuer's
//! keys as well (`auth::clerk`). Clerk's access and refresh tokens in the same
//! response are dropped unread.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use crossbow_proto::{ClerkConfig, ClerkExchangeRequest, error_codes, normalize_email};
use serde::Deserialize;

use super::MINUTE_MS;
use super::error::ApiError;
use super::tokens::ct_eq;
use super::validate::{PLATFORMS, device_name};

/// The loopback addresses registered with the OAuth client, exactly. A code
/// sent to any other address is refused before it is redeemed.
pub const REDIRECT_URIS: [&str; 4] = [
    "http://127.0.0.1:47231/callback",
    "http://127.0.0.1:47232/callback",
    "http://127.0.0.1:47233/callback",
    "http://127.0.0.1/callback",
];
/// Clock skew allowed on `exp` and `iat`.
pub const SKEW_MS: u64 = MINUTE_MS;
/// How long a browser sign-in waits for its account owner to confirm a link.
pub const LINK_TTL_MS: u64 = 5 * MINUTE_MS;
/// Longer ID tokens are refused unparsed.
pub const MAX_ID_TOKEN_CHARS: usize = 16 * 1024;
const MAX_CODE_CHARS: usize = 512;
const MAX_SUBJECT_CHARS: usize = 255;
const MAX_NAME_CHARS: usize = 128;

/// The Clerk instance a Worker accepts browser sign-ins from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    pub issuer: String,
    pub client_id: String,
}

impl Provider {
    /// From the `CLERK_ISSUER` and `CLERK_CLIENT_ID` vars: both set, and the
    /// issuer an https origin with no path. Anything else is no provider, as
    /// on the production Worker, which has neither.
    pub fn from_vars(issuer: Option<&str>, client_id: Option<&str>) -> Option<Provider> {
        let (issuer, client_id) = (issuer?.trim(), client_id?.trim());
        let host = issuer.strip_prefix("https://")?;
        let origin = !host.is_empty() && host.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-' || c == b':');
        let client = !client_id.is_empty() && client_id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-');
        (origin && client).then(|| Provider { issuer: issuer.to_string(), client_id: client_id.to_string() })
    }

    pub fn token_endpoint(&self) -> String {
        format!("{}/oauth/token", self.issuer)
    }

    pub fn jwks_uri(&self) -> String {
        format!("{}/.well-known/jwks.json", self.issuer)
    }

    /// What `GET /v1/health` tells the app.
    pub fn public(&self) -> ClerkConfig {
        ClerkConfig { issuer: self.issuer.clone(), client_id: self.client_id.clone(), authorize_endpoint: format!("{}/oauth/authorize", self.issuer) }
    }
}

/// A checked exchange request with a trimmed device name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exchange {
    pub code: String,
    pub code_verifier: String,
    pub redirect_uri: String,
    pub nonce: String,
    pub device_name: String,
    pub platform: String,
}

/// RFC 3986 unreserved characters: the PKCE verifier's alphabet.
fn unreserved(s: &str) -> bool {
    s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~'))
}

pub fn exchange(req: ClerkExchangeRequest) -> Result<Exchange, ApiError> {
    if !REDIRECT_URIS.contains(&req.redirect_uri.as_str()) {
        return Err(ApiError::bad_request("redirect_uri isn't a registered loopback address"));
    }
    if req.code.is_empty() || req.code.len() > MAX_CODE_CHARS || !req.code.bytes().all(|c| c.is_ascii_graphic()) {
        return Err(ApiError::bad_request("invalid code"));
    }
    if !(43..=128).contains(&req.code_verifier.len()) || !unreserved(&req.code_verifier) {
        return Err(ApiError::bad_request("invalid code_verifier"));
    }
    if !(16..=128).contains(&req.nonce.len()) || !unreserved(&req.nonce) {
        return Err(ApiError::bad_request("invalid nonce"));
    }
    let device_name = device_name(&req.device_name)?;
    if !PLATFORMS.contains(&req.platform.as_str()) {
        return Err(ApiError::bad_request("platform must be macos, windows or linux"));
    }
    Ok(Exchange { code: req.code, code_verifier: req.code_verifier, redirect_uri: req.redirect_uri, nonce: req.nonce, device_name, platform: req.platform })
}

/// `application/x-www-form-urlencoded` value: every byte but the unreserved
/// ones percent-encoded.
fn form(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The token request body: a public client (no secret) redeeming its code
/// with the PKCE verifier.
pub fn token_request(provider: &Provider, x: &Exchange) -> String {
    format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        form(&x.code), form(&x.redirect_uri), form(&provider.client_id), form(&x.code_verifier),
    )
}

/// The ID token from the token endpoint's answer. Nothing else in it is
/// read, kept or logged.
pub fn id_token(status: u16, body: &[u8]) -> Result<String, ApiError> {
    #[derive(Deserialize)]
    struct Tokens {
        id_token: Option<String>,
    }
    match status {
        200 => serde_json::from_slice::<Tokens>(body).ok().and_then(|t| t.id_token).ok_or_else(provider_failed),
        // invalid_grant: the code expired, was used, or its verifier or address don't match.
        400 | 401 => Err(ApiError::new(401, error_codes::SIGN_IN_REJECTED, "the browser sign-in has expired or was already used; start again")),
        _ => Err(provider_failed()),
    }
}

pub fn provider_failed() -> ApiError {
    ApiError::new(502, error_codes::PROVIDER_UNAVAILABLE, "the sign-in provider didn't answer; try again")
}

/// Why an ID token was refused. Each is logged by name only; the app is told
/// only that the sign-in couldn't be confirmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    Malformed,
    Algorithm,
    Issuer,
    Audience,
    AuthorizedParty,
    Expired,
    IssuedInFuture,
    Nonce,
    Subject,
    Signature,
}

impl Rejection {
    pub fn error(self) -> ApiError {
        ApiError::new(401, error_codes::SIGN_IN_REJECTED, "the browser sign-in couldn't be confirmed; start again")
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Deserialize)]
struct Header {
    alg: String,
    kid: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Claims {
    iss: Option<String>,
    sub: Option<String>,
    aud: Option<Audience>,
    azp: Option<String>,
    exp: Option<f64>,
    iat: Option<f64>,
    nonce: Option<String>,
    email: Option<String>,
    email_verified: Option<bool>,
    name: Option<String>,
}

/// A compact JWS, decoded but not yet checked.
#[derive(Debug)]
pub struct IdToken {
    pub alg: String,
    pub kid: Option<String>,
    claims: Claims,
    /// `header.payload`, the bytes the signature covers.
    pub signed: String,
    pub signature: Vec<u8>,
}

pub fn parse_id_token(jwt: &str) -> Result<IdToken, Rejection> {
    if jwt.len() > MAX_ID_TOKEN_CHARS {
        return Err(Rejection::Malformed);
    }
    let mut parts = jwt.split('.');
    let (Some(header), Some(payload), Some(signature), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(Rejection::Malformed);
    };
    let decode = |part: &str| URL_SAFE_NO_PAD.decode(part).map_err(|_| Rejection::Malformed);
    let head: Header = serde_json::from_slice(&decode(header)?).map_err(|_| Rejection::Malformed)?;
    let claims: Claims = serde_json::from_slice(&decode(payload)?).map_err(|_| Rejection::Malformed)?;
    let signature = decode(signature)?;
    if signature.is_empty() {
        return Err(Rejection::Malformed);
    }
    Ok(IdToken { alg: head.alg, kid: head.kid, claims, signed: format!("{header}.{payload}"), signature })
}

/// Who signed in, as the issuer says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub issuer: String,
    pub subject: String,
    /// Normalised; `None` when absent or not an address.
    pub email: Option<String>,
    pub email_verified: bool,
    /// Trimmed, printable, at most 128 characters.
    pub name: Option<String>,
    /// The token's `iat`, which a step-up's age is counted from.
    pub issued_at_ms: u64,
}

impl Identity {
    /// The email to name this identity by, or to keep with it: only one the
    /// provider verified.
    pub fn verified_email(&self) -> Option<&str> {
        self.email.as_deref().filter(|_| self.email_verified)
    }
}

/// The claims checks, in order: the algorithm the header names, issuer,
/// audience, authorized party, expiry and issue time (with `SKEW_MS`), the
/// nonce this sign-in sent, and a subject.
pub fn check(token: &IdToken, provider: &Provider, nonce: &str, now_ms: u64) -> Result<Identity, Rejection> {
    let c = &token.claims;
    if token.alg != "RS256" {
        return Err(Rejection::Algorithm);
    }
    if c.iss.as_deref() != Some(provider.issuer.as_str()) {
        return Err(Rejection::Issuer);
    }
    let audience = match &c.aud {
        Some(Audience::One(aud)) => *aud == provider.client_id,
        Some(Audience::Many(auds)) => auds.contains(&provider.client_id),
        None => false,
    };
    if !audience {
        return Err(Rejection::Audience);
    }
    if c.azp.as_ref().is_some_and(|azp| *azp != provider.client_id) {
        return Err(Rejection::AuthorizedParty);
    }
    let ms = |seconds: f64| if seconds.is_finite() && seconds > 0.0 { (seconds * 1000.0) as u64 } else { 0 };
    if !c.exp.is_some_and(|exp| ms(exp) + SKEW_MS > now_ms) {
        return Err(Rejection::Expired);
    }
    if !c.iat.is_some_and(|iat| ms(iat) <= now_ms + SKEW_MS) {
        return Err(Rejection::IssuedInFuture);
    }
    if !c.nonce.as_deref().is_some_and(|sent| ct_eq(sent, nonce)) {
        return Err(Rejection::Nonce);
    }
    let subject = c.sub.as_deref().unwrap_or_default();
    if subject.is_empty() || subject.chars().count() > MAX_SUBJECT_CHARS || subject.chars().any(char::is_control) {
        return Err(Rejection::Subject);
    }
    let name = c.name.as_deref().map(str::trim).filter(|n| !n.is_empty() && n.chars().count() <= MAX_NAME_CHARS && !n.chars().any(char::is_control));
    Ok(Identity {
        issuer: provider.issuer.clone(),
        subject: subject.to_string(),
        email: c.email.as_deref().and_then(normalize_email),
        email_verified: c.email_verified == Some(true),
        name: name.map(str::to_string),
        issued_at_ms: c.iat.map(ms).unwrap_or_default(),
    })
}

/// What a browser sign-in with no session does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignIn {
    /// The identity's account, by its id, unchanged.
    Account(String),
    /// A new account for this verified email.
    Create { email: String },
    /// An account already uses this email: sign in to it with an email code
    /// and link from there. Accounts are never merged by email.
    LinkRequired,
    /// No verified email to create an account for.
    EmailUnverified,
}

impl SignIn {
    pub fn error(&self) -> Option<ApiError> {
        match self {
            SignIn::LinkRequired => Some(ApiError::new(409, error_codes::LINK_REQUIRED, "an account already uses this email; sign in with an email code and link this sign-in from Account & Sync")),
            SignIn::EmailUnverified => Some(ApiError::new(403, error_codes::EMAIL_UNVERIFIED, "the sign-in provider hasn't verified this email address")),
            SignIn::Account(_) | SignIn::Create { .. } => None,
        }
    }
}

/// `known`: the account the identity is bound to; `email_owner`: the account
/// using the identity's email, looked up only when it is unknown.
pub fn sign_in(known: Option<String>, identity: &Identity, email_owner: Option<String>) -> SignIn {
    if let Some(account) = known {
        return SignIn::Account(account);
    }
    // Checked first, so an unverified address can't tell who uses it.
    let Some(email) = identity.email.clone().filter(|_| identity.email_verified) else { return SignIn::EmailUnverified };
    if email_owner.is_some() { SignIn::LinkRequired } else { SignIn::Create { email } }
}

/// What a browser sign-in with a session does to link the identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// Bound to no account: wait for the caller to confirm.
    Pending,
    /// Already the caller's.
    Already,
    /// Another account's.
    InUse,
}

pub fn link(known: Option<&str>, caller: &str) -> Link {
    match known {
        None => Link::Pending,
        Some(account) if account == caller => Link::Already,
        Some(_) => Link::InUse,
    }
}

/// One key of the issuer's JSON Web Key Set.
#[derive(Clone, Debug, Deserialize)]
pub struct Jwk {
    pub kty: String,
    pub kid: Option<String>,
    pub alg: Option<String>,
    #[serde(rename = "use")]
    pub usage: Option<String>,
    #[serde(default)]
    pub n: String,
    #[serde(default)]
    pub e: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Jwks {
    pub keys: Vec<Jwk>,
}

/// The issuer's RSA signing key for a token: the one with its `kid`, or,
/// for a token without one, the set's only usable key. None means refuse.
pub fn signing_key<'a>(jwks: &'a Jwks, kid: Option<&str>) -> Option<&'a Jwk> {
    let mut usable = jwks.keys.iter().filter(|k| {
        k.kty == "RSA" && k.alg.as_deref().is_none_or(|alg| alg == "RS256") && k.usage.as_deref().is_none_or(|u| u == "sig") && !k.n.is_empty() && !k.e.is_empty()
    });
    match kid {
        Some(kid) => usable.find(|k| k.kid.as_deref() == Some(kid)),
        None => {
            let only = usable.next()?;
            usable.next().is_none().then_some(only)
        }
    }
}

#[cfg(test)]
#[path = "clerk_tests.rs"]
mod tests;
