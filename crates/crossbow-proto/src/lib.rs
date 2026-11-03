//! Crossbow account + sync wire protocol (v1).
//!
//! The server is deliberately schema-agnostic: it stores opaque entity payloads
//! keyed by `(kind, id)` and resolves concurrent writes with last-writer-wins on a
//! hybrid logical clock. Payload contents are defined by `crossbow-core`, which
//! lets the client switch to end-to-end encryption (`Encoding::XChaCha20V1`)
//! without any server change.
//!
//! This crate has no platform dependencies so it compiles for the desktop app and
//! for `wasm32-unknown-unknown` (the Cloudflare Worker).

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;

pub const PROTOCOL_VERSION: u32 = 1;

/// Maximum number of changes accepted in one push.
pub const MAX_PUSH_CHANGES: usize = 500;
/// Maximum number of changes returned by one pull.
pub const MAX_PULL_CHANGES: usize = 1000;
/// Maximum payload size of one change, in bytes (after encoding).
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
/// Length of the numeric sign-in code sent by email.
pub const AUTH_CODE_LEN: usize = 6;
/// The header with which a dev caller presents the dev Worker's `DEV_CODE_KEY`
/// secret on `POST /v1/auth/start` to get its code back as `dev_code`.
pub const DEV_KEY_HEADER: &str = "x-crossbow-dev-key";

// ---------------------------------------------------------------------------
// Hybrid logical clock
// ---------------------------------------------------------------------------

/// Hybrid logical clock timestamp. Its string form sorts lexicographically in the
/// same order as the clock itself, so the server can compare with plain SQL.
///
/// Format: `{wall_ms:013}-{counter:06}-{node}`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Hlc {
    pub wall_ms: u64,
    pub counter: u32,
    /// Short, stable device identifier (`[A-Za-z0-9]{1,16}`), breaks ties.
    pub node: String,
}

impl Hlc {
    pub fn new(wall_ms: u64, counter: u32, node: impl Into<String>) -> Self {
        Self { wall_ms, counter, node: node.into() }
    }

    pub fn encode(&self) -> String {
        format!("{:013}-{:06}-{}", self.wall_ms, self.counter, self.node)
    }

    pub fn parse(s: &str) -> Result<Self, ProtoError> {
        let mut parts = s.splitn(3, '-');
        let (Some(wall), Some(counter), Some(node)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(ProtoError::InvalidHlc);
        };
        if wall.len() != 13 || counter.len() != 6 || !valid_node(node) {
            return Err(ProtoError::InvalidHlc);
        }
        Ok(Self {
            wall_ms: wall.parse().map_err(|_| ProtoError::InvalidHlc)?,
            counter: counter.parse().map_err(|_| ProtoError::InvalidHlc)?,
            node: node.to_string(),
        })
    }

    /// Advance a local clock for a new local event.
    pub fn tick(prev: Option<&Hlc>, now_ms: u64, node: &str) -> Hlc {
        match prev {
            Some(p) if p.wall_ms >= now_ms => Hlc::new(p.wall_ms, p.counter + 1, node),
            _ => Hlc::new(now_ms, 0, node),
        }
    }

    /// Merge a remote clock into the local one (receive event).
    pub fn observe(local: Option<&Hlc>, remote: &Hlc, now_ms: u64, node: &str) -> Hlc {
        let l_wall = local.map(|l| l.wall_ms).unwrap_or(0);
        let wall = now_ms.max(l_wall).max(remote.wall_ms);
        let counter = if wall == l_wall && wall == remote.wall_ms {
            local.map(|l| l.counter).unwrap_or(0).max(remote.counter) + 1
        } else if wall == l_wall {
            local.map(|l| l.counter).unwrap_or(0) + 1
        } else if wall == remote.wall_ms {
            remote.counter + 1
        } else {
            0
        };
        Hlc::new(wall, counter, node)
    }
}

impl Ord for Hlc {
    fn cmp(&self, other: &Self) -> Ordering {
        self.wall_ms
            .cmp(&other.wall_ms)
            .then(self.counter.cmp(&other.counter))
            .then(self.node.cmp(&other.node))
    }
}

impl PartialOrd for Hlc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Hlc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.encode())
    }
}

impl Serialize for Hlc {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.encode())
    }
}

impl<'de> Deserialize<'de> for Hlc {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Hlc::parse(&s).map_err(serde::de::Error::custom)
    }
}

fn valid_node(node: &str) -> bool {
    !node.is_empty() && node.len() <= 16 && node.bytes().all(|b| b.is_ascii_alphanumeric())
}

// ---------------------------------------------------------------------------
// Entities
// ---------------------------------------------------------------------------

/// Entity kinds the client syncs today. The server accepts any kind matching
/// [`valid_kind`] so new kinds ship without a server deploy.
pub mod kinds {
    pub const PROFILE: &str = "profile";
    pub const SPACE: &str = "space";
    pub const FOLDER: &str = "folder";
    /// A pinned tab or favorite (a saved sidebar item).
    pub const ITEM: &str = "item";
    pub const SETTINGS: &str = "settings";
    pub const BOOST: &str = "boost";
    pub const NOTE: &str = "note";
    pub const EASEL: &str = "easel";
}

/// `[a-z][a-z0-9_]{0,31}`
pub fn valid_kind(kind: &str) -> bool {
    let b = kind.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// `[A-Za-z0-9_-]{1,64}` (UUIDs and ULIDs fit).
pub fn valid_entity_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    /// Payload is plain JSON text.
    #[default]
    Json,
    /// Payload is library envelope v1 of the JSON text: unpadded base64url of
    /// `version (0x01) || key epoch (u32 BE) || 24-byte random nonce ||
    /// XChaCha20-Poly1305 ciphertext and tag`. The AAD binds the account, kind,
    /// id, encoded HLC and epoch (see `crossbow_core::sync::e2ee`). Tombstones
    /// are never encrypted; the server stores this text opaquely.
    Xchacha20V1,
}

/// A change a client pushes. Deletions keep a tombstone (`deleted = true`,
/// empty payload) so other devices learn about them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub kind: String,
    pub id: String,
    pub hlc: Hlc,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub enc: Encoding,
    #[serde(default)]
    pub payload: String,
}

impl Change {
    pub fn validate(&self) -> Result<(), ProtoError> {
        if !valid_kind(&self.kind) {
            return Err(ProtoError::InvalidKind);
        }
        if !valid_entity_id(&self.id) {
            return Err(ProtoError::InvalidId);
        }
        if self.payload.len() > MAX_PAYLOAD_BYTES {
            return Err(ProtoError::PayloadTooLarge);
        }
        Ok(())
    }
}

/// A change as stored by the server, with its account-wide sequence number.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerChange {
    pub seq: u64,
    /// Device that wrote this version (lets a client skip its own echoes).
    pub device_id: String,
    #[serde(flatten)]
    pub change: Change,
}

// ---------------------------------------------------------------------------
// HTTP bodies
// ---------------------------------------------------------------------------

/// `POST /v1/auth/start`
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthStartRequest {
    pub email: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthStartResponse {
    pub challenge_id: String,
    /// Seconds until the emailed code expires.
    pub expires_in: u64,
    /// Only present when the Worker runs with `ENVIRONMENT = "dev"` and the
    /// request's `x-crossbow-dev-key` header matches its `DEV_CODE_KEY`
    /// secret. Never set in production.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dev_code: Option<String>,
}

/// `POST /v1/auth/verify`
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthVerifyRequest {
    pub challenge_id: String,
    pub code: String,
    pub device_name: String,
    /// `macos` | `windows` | `linux`
    pub platform: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthVerifyResponse {
    /// Bearer token. Shown once; the server keeps only its SHA-256.
    pub session_token: String,
    pub account: Account,
    pub device: Device,
    /// The name a browser sign-in's ID token gave, if any. Never set by an
    /// email code. Not stored by the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// `POST /v1/auth/clerk` (sign in) and `POST /v1/auth/clerk/link` (with a
/// session): the authorization code a browser sign-in returned to the app's
/// loopback address, and the PKCE verifier and nonce that go with it. The
/// server redeems the code; the app never holds a provider token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClerkExchangeRequest {
    pub code: String,
    pub code_verifier: String,
    /// The exact loopback address the code was sent to.
    pub redirect_uri: String,
    pub nonce: String,
    pub device_name: String,
    /// `macos` | `windows` | `linux`
    pub platform: String,
}

/// `POST /v1/auth/clerk/link`: the browser sign-in waits for the account
/// owner to confirm the link, naming its email. `link_id` is `None` when the
/// identity already belongs to this account, so there is nothing to confirm.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClerkLinkResponse {
    pub link_id: Option<String>,
    pub email: Option<String>,
    /// Seconds until the link can no longer be confirmed.
    pub expires_in: u64,
}

/// `POST /v1/auth/clerk/link/confirm`
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClerkLinkConfirmRequest {
    pub link_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub email: String,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub created_at: u64,
    pub last_seen_at: u64,
}

/// `GET /v1/me`
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MeResponse {
    pub account: Account,
    pub device: Device,
    pub devices: Vec<Device>,
    /// A browser sign-in is linked to this account. Absent (a Worker from
    /// before browser sign-in) means none.
    #[serde(default)]
    pub browser_sign_in: bool,
}

/// `POST /v1/sync/push`
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PushRequest {
    pub changes: Vec<Change>,
    /// The sync epoch this device's rows belong to, from its pulls. Absent
    /// means 0, the epoch of an account whose sync data was never deleted.
    /// Any other epoch than the account's is refused with
    /// `409 sync_epoch_changed`, so a device holding deleted data can't
    /// upload it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_epoch: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PushResponse {
    /// Number of changes that won LWW and were stored.
    pub accepted: u32,
    /// Changes the server already had a newer version of. The client should
    /// pull to receive the winning versions.
    pub stale: Vec<StaleChange>,
    /// Highest sequence number on the server after this push.
    pub cursor: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StaleChange {
    pub kind: String,
    pub id: String,
    pub server_hlc: Hlc,
}

/// `GET /v1/sync/pull?since=<cursor>&limit=<n>` response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PullResponse {
    pub changes: Vec<ServerChange>,
    /// Pass this as `since` on the next pull.
    pub cursor: u64,
    /// More changes are waiting; pull again immediately.
    pub more: bool,
    /// The account's sync epoch. Deleting the account's sync data starts a
    /// new one; a device that synced an earlier one holds deleted data.
    /// Absent (a Worker from before epochs) means 0.
    #[serde(default)]
    pub sync_epoch: u64,
}

/// `DELETE /v1/sync/data`: every stored row of the account is gone and a new
/// sync epoch has started. Sessions, devices and the account stay.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeleteSyncDataResponse {
    /// The new epoch. A pull now names it and returns no rows until a device
    /// uploads again.
    pub sync_epoch: u64,
    /// Rows deleted, tombstones included; 0 when there was nothing left.
    pub deleted_rows: u64,
}

/// `DELETE /v1/account` (with a session): the account goes once the user
/// typed its email and proved, within 5 minutes, that they hold it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteAccountRequest {
    /// The account's email, as the user typed it.
    pub confirm: String,
    pub step_up: StepUp,
}

/// What proves the user holds the account right now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepUp {
    /// A code from `POST /v1/auth/start` for the account's email, started at
    /// most 5 minutes ago. It is used up.
    EmailCode { challenge_id: String, code: String },
    /// A browser sign-in of an identity linked to the account, redeemed by
    /// the server now. Its device name and platform are not used.
    Browser(ClerkExchangeRequest),
}

/// `DELETE /v1/account`: the account, its devices, sessions and browser
/// identities are gone, and so are its synced rows.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeleteAccountResponse {
    /// Synced rows deleted, tombstones included.
    pub deleted_rows: u64,
}

/// Messages the server sends on `GET /v1/sync/ws` (WebSocket upgrade).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    /// Sent once after connect.
    Hello { cursor: u64 },
    /// New changes were stored; pull from your cursor.
    Changed { cursor: u64, by_device: String },
}

/// `GET /v1/health`
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub protocol: u32,
    pub service: String,
    pub version: String,
    /// The browser sign-in this server accepts. Absent when it has none,
    /// as on a Worker from before browser sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clerk: Option<ClerkConfig>,
}

/// The public values a desktop app needs to start a browser sign-in with
/// the server's Clerk instance. No secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClerkConfig {
    pub issuer: String,
    pub client_id: String,
    pub authorize_endpoint: String,
}

/// Error body for every non-2xx response: `{"error":{"code":"…","message":"…"}}`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// Stable machine code, see [`error_codes`].
    pub code: String,
    pub message: String,
}

pub mod error_codes {
    pub const BAD_REQUEST: &str = "bad_request";
    pub const UNAUTHORIZED: &str = "unauthorized";
    pub const RATE_LIMITED: &str = "rate_limited";
    pub const INVALID_CODE: &str = "invalid_code";
    pub const CODE_EXPIRED: &str = "code_expired";
    pub const TOO_MANY_ATTEMPTS: &str = "too_many_attempts";
    pub const PAYLOAD_TOO_LARGE: &str = "payload_too_large";
    pub const NOT_FOUND: &str = "not_found";
    pub const MAIL_UNAVAILABLE: &str = "mail_unavailable";
    /// The account's sync data was deleted since this device last synced.
    pub const SYNC_EPOCH_CHANGED: &str = "sync_epoch_changed";
    /// This server doesn't offer browser sign-in.
    pub const CLERK_UNAVAILABLE: &str = "clerk_unavailable";
    /// The browser sign-in couldn't be confirmed with the provider.
    pub const SIGN_IN_REJECTED: &str = "sign_in_rejected";
    /// The browser sign-in's provider didn't answer; try again later.
    pub const PROVIDER_UNAVAILABLE: &str = "provider_unavailable";
    /// The provider hasn't verified the browser sign-in's email address.
    pub const EMAIL_UNVERIFIED: &str = "email_unverified";
    /// An account with the browser sign-in's email exists: sign in to it
    /// with an email code, then link the browser sign-in. Never merged.
    pub const LINK_REQUIRED: &str = "link_required";
    /// The browser sign-in is linked to another account.
    pub const IDENTITY_IN_USE: &str = "identity_in_use";
    /// Delete Account: the typed confirmation isn't the account's email.
    pub const CONFIRMATION_MISMATCH: &str = "confirmation_mismatch";
    /// Delete Account: the browser sign-in isn't this account's, or is too old.
    pub const STEP_UP_REJECTED: &str = "step_up_rejected";
    pub const INTERNAL: &str = "internal";
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtoError {
    InvalidHlc,
    InvalidKind,
    InvalidId,
    PayloadTooLarge,
}

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ProtoError::InvalidHlc => "invalid hlc timestamp",
            ProtoError::InvalidKind => "invalid entity kind",
            ProtoError::InvalidId => "invalid entity id",
            ProtoError::PayloadTooLarge => "payload too large",
        })
    }
}

impl std::error::Error for ProtoError {}

/// Normalise an email for account lookup: trimmed, lowercased. Returns `None`
/// for strings that are obviously not an address.
pub fn normalize_email(email: &str) -> Option<String> {
    let e = email.trim().to_ascii_lowercase();
    let (local, domain) = e.split_once('@')?;
    if local.is_empty() || domain.len() < 3 || !domain.contains('.') || e.len() > 254 {
        return None;
    }
    if e.chars().any(|c| c.is_whitespace() || c == '<' || c == '>') {
        return None;
    }
    Some(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hlc_string_order_matches_clock_order() {
        let a = Hlc::new(1_700_000_000_000, 5, "aa");
        let b = Hlc::new(1_700_000_000_000, 12, "aa");
        let c = Hlc::new(1_700_000_000_001, 0, "aa");
        let d = Hlc::new(1_700_000_000_001, 0, "ab");
        let clocks = [&a, &b, &c, &d];
        for w in clocks.windows(2) {
            assert!(w[0] < w[1]);
            assert!(w[0].encode() < w[1].encode());
        }
    }

    #[test]
    fn hlc_roundtrip_and_validation() {
        let h = Hlc::new(42, 7, "dev1");
        assert_eq!(Hlc::parse(&h.encode()).unwrap(), h);
        assert!(Hlc::parse("1-2-x").is_err());
        assert!(Hlc::parse("0000000000042-000007-bad node").is_err());
        let json = serde_json::to_string(&h).unwrap();
        assert_eq!(json, "\"0000000000042-000007-dev1\"");
    }

    #[test]
    fn hlc_tick_and_observe_are_monotonic() {
        let a = Hlc::tick(None, 100, "a");
        let b = Hlc::tick(Some(&a), 90, "a"); // clock went backwards
        assert!(b > a);
        let remote = Hlc::new(500, 3, "b");
        let c = Hlc::observe(Some(&b), &remote, 200, "a");
        assert!(c > remote && c > b);
    }

    #[test]
    fn change_validation() {
        let mut c = Change {
            kind: kinds::SPACE.into(),
            id: "0192f1a0-aaaa-7bbb-8ccc-123456789abc".into(),
            hlc: Hlc::new(1, 0, "n"),
            deleted: false,
            enc: Encoding::Json,
            payload: "{}".into(),
        };
        assert!(c.validate().is_ok());
        c.kind = "Space".into();
        assert_eq!(c.validate(), Err(ProtoError::InvalidKind));
        c.kind = "space".into();
        c.id = "../etc".into();
        assert_eq!(c.validate(), Err(ProtoError::InvalidId));
    }

    #[test]
    fn server_change_flattens() {
        let sc = ServerChange {
            seq: 9,
            device_id: "d1".into(),
            change: Change {
                kind: "item".into(),
                id: "x".into(),
                hlc: Hlc::new(1, 0, "n"),
                deleted: true,
                enc: Encoding::Json,
                payload: String::new(),
            },
        };
        let v: serde_json::Value = serde_json::to_value(&sc).unwrap();
        assert_eq!(v["seq"], 9);
        assert_eq!(v["kind"], "item");
        assert_eq!(v["deleted"], true);
        let back: ServerChange = serde_json::from_value(v).unwrap();
        assert_eq!(back, sc);
    }

    #[test]
    fn sync_epoch_fields_are_optional_on_the_wire() {
        // A Worker from before sync epochs sends none: that is epoch 0.
        let page: PullResponse = serde_json::from_str(r#"{"changes":[],"cursor":4,"more":false}"#).unwrap();
        assert_eq!(page.sync_epoch, 0);
        // A push without a known epoch says nothing, as clients before it did.
        let push = serde_json::to_value(PushRequest { changes: vec![], sync_epoch: None }).unwrap();
        assert!(push.get("sync_epoch").is_none());
        let push: PushRequest = serde_json::from_str(r#"{"changes":[]}"#).unwrap();
        assert_eq!(push.sync_epoch, None);
        let push = serde_json::to_value(PushRequest { changes: vec![], sync_epoch: Some(2) }).unwrap();
        assert_eq!(push["sync_epoch"], 2);
        let deleted: DeleteSyncDataResponse = serde_json::from_str(r#"{"sync_epoch":2,"deleted_rows":7}"#).unwrap();
        assert_eq!((deleted.sync_epoch, deleted.deleted_rows), (2, 7));
    }

    #[test]
    fn browser_sign_in_fields_are_optional_on_the_wire() {
        // A Worker from before browser sign-in names no provider, no display
        // name and no linked identity: browser sign-in isn't offered.
        let health: HealthResponse = serde_json::from_str(r#"{"ok":true,"protocol":1,"service":"crossbow-api","version":"0.1.0"}"#).unwrap();
        assert!(health.clerk.is_none());
        assert!(serde_json::to_value(&health).unwrap().get("clerk").is_none());
        let identity = r#""account":{"id":"acc_a","email":"a@example.com","created_at":1},"device":{"id":"dev_a","name":"Mac","platform":"macos","created_at":1,"last_seen_at":1}"#;
        let verified: AuthVerifyResponse = serde_json::from_str(&format!(r#"{{"session_token":"cbs_x",{identity}}}"#)).unwrap();
        assert_eq!(verified.display_name, None);
        assert!(serde_json::to_value(&verified).unwrap().get("display_name").is_none());
        let me: MeResponse = serde_json::from_str(&format!(r#"{{{identity},"devices":[]}}"#)).unwrap();
        assert!(!me.browser_sign_in);

        let health: HealthResponse = serde_json::from_str(r#"{"ok":true,"protocol":1,"service":"crossbow-api","version":"0.1.0","clerk":{"issuer":"https://issuer.example","client_id":"client","authorize_endpoint":"https://issuer.example/oauth/authorize"}}"#).unwrap();
        assert_eq!(health.clerk, Some(ClerkConfig { issuer: "https://issuer.example".into(), client_id: "client".into(), authorize_endpoint: "https://issuer.example/oauth/authorize".into() }));
        let verified: AuthVerifyResponse = serde_json::from_str(&format!(r#"{{"session_token":"cbs_x",{identity},"display_name":"Ada"}}"#)).unwrap();
        assert_eq!(verified.display_name.as_deref(), Some("Ada"));
        let me: MeResponse = serde_json::from_str(&format!(r#"{{{identity},"devices":[],"browser_sign_in":true}}"#)).unwrap();
        assert!(me.browser_sign_in);
        // A link waiting for confirmation, and one the account already has.
        let pending: ClerkLinkResponse = serde_json::from_str(r#"{"link_id":"lnk_a","email":"b@example.com","expires_in":300}"#).unwrap();
        assert_eq!((pending.link_id.as_deref(), pending.email.as_deref()), (Some("lnk_a"), Some("b@example.com")));
        let linked: ClerkLinkResponse = serde_json::from_str(r#"{"link_id":null,"email":null,"expires_in":0}"#).unwrap();
        assert_eq!(linked.link_id, None);
        let exchange = ClerkExchangeRequest { code: "c".into(), code_verifier: "v".into(), redirect_uri: "http://127.0.0.1:47231/callback".into(), nonce: "n".into(), device_name: "Mac".into(), platform: "macos".into() };
        let back: ClerkExchangeRequest = serde_json::from_value(serde_json::to_value(&exchange).unwrap()).unwrap();
        assert_eq!(back, exchange);
    }

    #[test]
    fn delete_account_names_its_step_up_by_kind() {
        let code = DeleteAccountRequest { confirm: "a@example.com".into(), step_up: StepUp::EmailCode { challenge_id: "chl_a".into(), code: "123456".into() } };
        assert_eq!(serde_json::to_value(&code).unwrap(), serde_json::json!({"confirm": "a@example.com", "step_up": {"kind": "email_code", "challenge_id": "chl_a", "code": "123456"}}));
        let exchange = ClerkExchangeRequest { code: "c".into(), code_verifier: "v".into(), redirect_uri: "http://127.0.0.1:47231/callback".into(), nonce: "n".into(), device_name: "Mac".into(), platform: "macos".into() };
        let browser = DeleteAccountRequest { confirm: "a@example.com".into(), step_up: StepUp::Browser(exchange) };
        let wire = serde_json::to_value(&browser).unwrap();
        assert_eq!(wire["step_up"]["kind"], "browser");
        assert_eq!(wire["step_up"]["code_verifier"], "v");
        assert_eq!(serde_json::from_value::<DeleteAccountRequest>(wire).unwrap(), browser);
        // No step-up, or one of a kind this server doesn't know, isn't a request.
        for body in [r#"{"confirm":"a@example.com"}"#, r#"{"confirm":"a@example.com","step_up":{"kind":"password","password":"x"}}"#] {
            assert!(serde_json::from_str::<DeleteAccountRequest>(body).is_err(), "{body}");
        }
        let deleted: DeleteAccountResponse = serde_json::from_str(r#"{"deleted_rows":3}"#).unwrap();
        assert_eq!(deleted.deleted_rows, 3);
    }

    #[test]
    fn email_normalisation() {
        assert_eq!(normalize_email("  Foo@Example.COM "), Some("foo@example.com".into()));
        assert_eq!(normalize_email("nope"), None);
        assert_eq!(normalize_email("a@b"), None);
    }
}
