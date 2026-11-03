//! D1 queries (accounts, devices, sessions, sign-in challenges, browser
//! identities).
//!
//! Numbers come back from D1 as JS numbers, so row structs use `f64` and convert;
//! Unix-millisecond timestamps are far inside the safe-integer range.

use crossbow_proto::{Account, Device};
use serde::Deserialize;
use worker::wasm_bindgen::JsValue;
use worker::{D1Database, D1PreparedStatement, Env};

use crate::logic::clerk::Identity;
use crate::rt::{ApiResult, internal};

pub fn db(env: &Env) -> ApiResult<D1Database> {
    env.d1("DB").map_err(internal("d1 binding"))
}

fn s(v: &str) -> JsValue {
    JsValue::from_str(v)
}

fn n(v: u64) -> JsValue {
    JsValue::from_f64(v as f64)
}

fn opt(v: Option<&str>) -> JsValue {
    v.map(JsValue::from_str).unwrap_or(JsValue::NULL)
}

fn stmt(db: &D1Database, sql: &str, args: &[JsValue]) -> ApiResult<D1PreparedStatement> {
    db.prepare(sql).bind(args).map_err(internal("d1 bind"))
}

async fn run(st: D1PreparedStatement, what: &str) -> ApiResult<usize> {
    let r = st.run().await.map_err(internal(what))?;
    Ok(r.meta().ok().flatten().and_then(|m| m.changes).unwrap_or(0))
}

#[derive(Deserialize)]
struct Count {
    n: f64,
}

/// Sign-in starts inside the window for this email and (if known) this IP.
pub async fn count_starts(
    db: &D1Database,
    email: &str,
    ip: Option<&str>,
    since: u64,
) -> ApiResult<(u64, Option<u64>)> {
    let by_email = stmt(
        db,
        "SELECT COUNT(*) AS n FROM auth_challenges WHERE email = ?1 AND created_at > ?2",
        &[s(email), n(since)],
    )?;
    let email_n = by_email.first::<Count>(None).await.map_err(internal("count starts"))?;
    let ip_n = match ip {
        Some(ip) => {
            let by_ip = stmt(
                db,
                "SELECT COUNT(*) AS n FROM auth_challenges WHERE ip = ?1 AND created_at > ?2",
                &[s(ip), n(since)],
            )?;
            let r = by_ip.first::<Count>(None).await.map_err(internal("count starts by ip"))?;
            Some(r.map(|c| c.n as u64).unwrap_or(0))
        }
        None => None,
    };
    Ok((email_n.map(|c| c.n as u64).unwrap_or(0), ip_n))
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_challenge(
    db: &D1Database,
    id: &str,
    email: &str,
    code_hash: &str,
    ip: Option<&str>,
    now: u64,
    expires_at: u64,
) -> ApiResult<()> {
    let st = stmt(
        db,
        "INSERT INTO auth_challenges (id, email, code_hash, attempts, ip, created_at, expires_at) \
         VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6)",
        &[s(id), s(email), s(code_hash), opt(ip), n(now), n(expires_at)],
    )?;
    run(st, "insert challenge").await.map(|_| ())
}

/// Drop this email's challenges that no longer count for anything.
pub async fn purge_challenges_for(db: &D1Database, email: &str, before: u64) -> ApiResult<()> {
    let st = stmt(
        db,
        "DELETE FROM auth_challenges WHERE email = ?1 AND created_at < ?2",
        &[s(email), n(before)],
    )?;
    run(st, "purge challenges").await.map(|_| ())
}

#[derive(Deserialize)]
pub struct ChallengeRow {
    pub email: String,
    pub code_hash: String,
    pub attempts: f64,
    pub created_at: f64,
    pub expires_at: f64,
    pub consumed_at: Option<f64>,
}

pub async fn challenge(db: &D1Database, id: &str) -> ApiResult<Option<ChallengeRow>> {
    stmt(
        db,
        "SELECT email, code_hash, attempts, created_at, expires_at, consumed_at FROM auth_challenges WHERE id = ?1",
        &[s(id)],
    )?
    .first::<ChallengeRow>(None)
    .await
    .map_err(internal("load challenge"))
}

pub async fn record_wrong_attempt(db: &D1Database, id: &str) -> ApiResult<()> {
    let st = stmt(db, "UPDATE auth_challenges SET attempts = attempts + 1 WHERE id = ?1", &[s(id)])?;
    run(st, "record attempt").await.map(|_| ())
}

/// Atomically mark the challenge used. False if another request won the race,
/// it expired, or it locked in the meantime.
pub async fn consume_challenge(db: &D1Database, id: &str, now: u64, max_attempts: u64) -> ApiResult<bool> {
    let st = stmt(
        db,
        "UPDATE auth_challenges SET consumed_at = ?2 \
         WHERE id = ?1 AND consumed_at IS NULL AND expires_at > ?2 AND attempts < ?3",
        &[s(id), n(now), n(max_attempts)],
    )?;
    Ok(run(st, "consume challenge").await? == 1)
}

#[derive(Deserialize)]
struct AccountRow {
    id: String,
    email: String,
    created_at: f64,
}

/// Whose account a new session is for.
#[derive(Clone, Copy)]
pub enum Holder<'a> {
    /// Email-code sign-in: the account with this email, created if missing.
    Email(&'a str),
    /// An existing account, by id; nothing if it is gone.
    Existing(&'a str),
    /// A new account bound to a new browser identity; nothing if the email
    /// or the identity is taken by then. Never merges with another account.
    NewIdentity { email: &'a str, issuer: &'a str, subject: &'a str },
}

pub struct NewSignIn<'a> {
    pub holder: Holder<'a>,
    pub new_account_id: &'a str,
    pub device_id: &'a str,
    pub device_name: &'a str,
    pub platform: &'a str,
    pub token_hash: &'a str,
    pub now: u64,
    pub session_expires_at: u64,
}

/// A new device and session for the holder's account, creating the
/// account when the holder says so, in one D1 batch (a single transaction).
/// `None` when that account isn't there to sign in to.
pub async fn sign_in(db: &D1Database, x: NewSignIn<'_>) -> ApiResult<Option<Account>> {
    // The holder's account is found by email or by id; both are unique.
    let (column, key) = match x.holder {
        Holder::Email(email) => ("email", email),
        Holder::Existing(id) => ("id", id),
        Holder::NewIdentity { .. } => ("id", x.new_account_id),
    };
    let mut stmts = match x.holder {
        Holder::Email(email) => vec![stmt(
            db,
            "INSERT INTO accounts (id, email, created_at) VALUES (?1, ?2, ?3) ON CONFLICT(email) DO NOTHING",
            &[s(x.new_account_id), s(email), n(x.now)],
        )?],
        Holder::Existing(_) => Vec::new(),
        Holder::NewIdentity { email, issuer, subject } => vec![
            stmt(
                db,
                "INSERT INTO accounts (id, email, created_at) SELECT ?1, ?2, ?3 \
                 WHERE NOT EXISTS (SELECT 1 FROM accounts WHERE email = ?2) \
                 AND NOT EXISTS (SELECT 1 FROM identities WHERE issuer = ?4 AND subject = ?5)",
                &[s(x.new_account_id), s(email), n(x.now), s(issuer), s(subject)],
            )?,
            stmt(
                db,
                "INSERT INTO identities (issuer, subject, account_id, created_at) SELECT ?2, ?3, id, ?4 FROM accounts WHERE id = ?1",
                &[s(x.new_account_id), s(issuer), s(subject), n(x.now)],
            )?,
        ],
    };
    stmts.extend([
        stmt(
            db,
            &format!(
                "INSERT INTO devices (id, account_id, name, platform, created_at, last_seen_at) \
                 SELECT ?1, id, ?2, ?3, ?4, ?4 FROM accounts WHERE {column} = ?5"
            ),
            &[s(x.device_id), s(x.device_name), s(x.platform), n(x.now), s(key)],
        )?,
        stmt(
            db,
            &format!(
                "INSERT INTO sessions (token_hash, account_id, device_id, created_at, expires_at) \
                 SELECT ?1, id, ?2, ?3, ?4 FROM accounts WHERE {column} = ?5"
            ),
            &[s(x.token_hash), s(x.device_id), n(x.now), n(x.session_expires_at), s(key)],
        )?,
        stmt(db, &format!("SELECT id, email, created_at FROM accounts WHERE {column} = ?1"), &[s(key)])?,
    ]);
    let results = db.batch(stmts).await.map_err(internal("sign-in batch"))?;
    let row = results
        .last()
        .map(|r| r.results::<AccountRow>())
        .transpose()
        .map_err(internal("sign-in account row"))?
        .and_then(|mut rows| rows.pop());
    Ok(row.map(|row| Account { id: row.id, email: row.email, created_at: row.created_at as u64 }))
}

/// The account a browser identity is bound to.
pub async fn identity_account(db: &D1Database, issuer: &str, subject: &str) -> ApiResult<Option<String>> {
    let r = stmt(db, "SELECT account_id AS id FROM identities WHERE issuer = ?1 AND subject = ?2", &[s(issuer), s(subject)])?
        .first::<IdRow>(None)
        .await
        .map_err(internal("find identity"))?;
    Ok(r.map(|r| r.id))
}

/// The account using this (normalized) email.
pub async fn account_by_email(db: &D1Database, email: &str) -> ApiResult<Option<String>> {
    let r = stmt(db, "SELECT id FROM accounts WHERE email = ?1", &[s(email)])?
        .first::<IdRow>(None)
        .await
        .map_err(internal("find account by email"))?;
    Ok(r.map(|r| r.id))
}

/// Whether any browser identity is bound to the account.
pub async fn has_identity(db: &D1Database, account_id: &str) -> ApiResult<bool> {
    let r = stmt(db, "SELECT COUNT(*) AS n FROM identities WHERE account_id = ?1", &[s(account_id)])?
        .first::<Count>(None)
        .await
        .map_err(internal("count identities"))?;
    Ok(r.is_some_and(|c| c.n > 0.0))
}

#[derive(Deserialize)]
struct IdRow {
    id: String,
}

/// Hold a browser identity until the session that asked confirms the link.
/// The email is kept only when the provider verified it.
pub async fn insert_link(
    db: &D1Database,
    id: &str,
    account_id: &str,
    session_hash: &str,
    identity: &Identity,
    now: u64,
    expires_at: u64,
) -> ApiResult<()> {
    let st = stmt(
        db,
        "INSERT INTO identity_links (id, account_id, issuer, subject, email, created_at, expires_at, session_hash) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        &[
            s(id),
            s(account_id),
            s(&identity.issuer),
            s(&identity.subject),
            opt(identity.verified_email()),
            n(now),
            n(expires_at),
            s(session_hash),
        ],
    )?;
    run(st, "insert link").await.map(|_| ())
}

#[derive(Deserialize)]
struct LinkRow {
    issuer: String,
    subject: String,
}

/// A held link that this session of this account made and that hasn't
/// expired: (issuer, subject).
pub async fn pending_link(
    db: &D1Database,
    id: &str,
    account_id: &str,
    session_hash: &str,
    now: u64,
) -> ApiResult<Option<(String, String)>> {
    let r = stmt(
        db,
        "SELECT issuer, subject FROM identity_links \
         WHERE id = ?1 AND account_id = ?2 AND expires_at > ?3 AND session_hash = ?4",
        &[s(id), s(account_id), n(now), s(session_hash)],
    )?
    .first::<LinkRow>(None)
    .await
    .map_err(internal("load link"))?;
    Ok(r.map(|r| (r.issuer, r.subject)))
}

/// Bind a held link's identity to its account and drop the link, in one
/// batch, for the session that made it only. False if the link is gone or
/// expired, or the identity was bound meanwhile.
pub async fn bind_link(db: &D1Database, id: &str, account_id: &str, session_hash: &str, now: u64) -> ApiResult<bool> {
    let stmts = vec![
        stmt(
            db,
            "INSERT INTO identities (issuer, subject, account_id, created_at) \
             SELECT l.issuer, l.subject, l.account_id, ?3 FROM identity_links l \
             WHERE l.id = ?1 AND l.account_id = ?2 AND l.expires_at > ?3 AND l.session_hash = ?4 \
             AND NOT EXISTS (SELECT 1 FROM identities i WHERE i.issuer = l.issuer AND i.subject = l.subject)",
            &[s(id), s(account_id), n(now), s(session_hash)],
        )?,
        stmt(
            db,
            "DELETE FROM identity_links WHERE id = ?1 AND account_id = ?2 AND session_hash = ?3",
            &[s(id), s(account_id), s(session_hash)],
        )?,
    ];
    let results = db.batch(stmts).await.map_err(internal("bind link"))?;
    let bound = results.first().and_then(|r| r.meta().ok().flatten()).and_then(|m| m.changes).unwrap_or(0);
    Ok(bound == 1)
}

pub async fn delete_link(db: &D1Database, id: &str) -> ApiResult<()> {
    let st = stmt(db, "DELETE FROM identity_links WHERE id = ?1", &[s(id)])?;
    run(st, "delete link").await.map(|_| ())
}

#[derive(Deserialize)]
pub struct SessionRow {
    pub account_id: String,
    pub device_id: String,
    pub expires_at: f64,
    pub email: String,
    pub account_created_at: f64,
    pub device_name: String,
    pub platform: String,
    pub device_created_at: f64,
    pub last_seen_at: f64,
    pub revoked_at: Option<f64>,
}

pub async fn session(db: &D1Database, token_hash: &str) -> ApiResult<Option<SessionRow>> {
    stmt(
        db,
        "SELECT s.account_id, s.device_id, s.expires_at, a.email, a.created_at AS account_created_at, \
                d.name AS device_name, d.platform, d.created_at AS device_created_at, d.last_seen_at, d.revoked_at \
         FROM sessions s \
         JOIN accounts a ON a.id = s.account_id \
         JOIN devices d ON d.id = s.device_id AND d.account_id = s.account_id \
         WHERE s.token_hash = ?1",
        &[s(token_hash)],
    )?
    .first::<SessionRow>(None)
    .await
    .map_err(internal("load session"))
}

/// Write `last_seen_at` and slide the session expiry.
pub async fn touch(db: &D1Database, device_id: &str, token_hash: &str, now: u64, expires_at: u64) -> ApiResult<()> {
    let stmts = vec![
        stmt(db, "UPDATE devices SET last_seen_at = ?2 WHERE id = ?1", &[s(device_id), n(now)])?,
        stmt(db, "UPDATE sessions SET expires_at = ?2 WHERE token_hash = ?1", &[s(token_hash), n(expires_at)])?,
    ];
    db.batch(stmts).await.map_err(internal("touch"))?;
    Ok(())
}

pub async fn delete_session(db: &D1Database, token_hash: &str) -> ApiResult<()> {
    let st = stmt(db, "DELETE FROM sessions WHERE token_hash = ?1", &[s(token_hash)])?;
    run(st, "delete session").await.map(|_| ())
}

#[derive(Deserialize)]
struct DeviceRow {
    id: String,
    name: String,
    platform: String,
    created_at: f64,
    last_seen_at: f64,
}

impl From<DeviceRow> for Device {
    fn from(r: DeviceRow) -> Self {
        Device {
            id: r.id,
            name: r.name,
            platform: r.platform,
            created_at: r.created_at as u64,
            last_seen_at: r.last_seen_at as u64,
        }
    }
}

pub async fn active_devices(db: &D1Database, account_id: &str) -> ApiResult<Vec<Device>> {
    let r = stmt(
        db,
        "SELECT id, name, platform, created_at, last_seen_at FROM devices \
         WHERE account_id = ?1 AND revoked_at IS NULL ORDER BY created_at",
        &[s(account_id)],
    )?
    .all()
    .await
    .map_err(internal("list devices"))?;
    let rows = r.results::<DeviceRow>().map_err(internal("device rows"))?;
    Ok(rows.into_iter().map(Device::from).collect())
}

/// Whether the device exists in this account (revoked or not).
pub async fn device_in_account(db: &D1Database, device_id: &str, account_id: &str) -> ApiResult<bool> {
    let r = stmt(
        db,
        "SELECT COUNT(*) AS n FROM devices WHERE id = ?1 AND account_id = ?2",
        &[s(device_id), s(account_id)],
    )?
    .first::<Count>(None)
    .await
    .map_err(internal("find device"))?;
    Ok(r.is_some_and(|c| c.n > 0.0))
}

pub async fn revoke_device(db: &D1Database, device_id: &str, account_id: &str, now: u64) -> ApiResult<()> {
    let stmts = vec![
        stmt(
            db,
            "UPDATE devices SET revoked_at = ?3 WHERE id = ?1 AND account_id = ?2 AND revoked_at IS NULL",
            &[s(device_id), s(account_id), n(now)],
        )?,
        stmt(
            db,
            "DELETE FROM sessions WHERE device_id = ?1 AND account_id = ?2",
            &[s(device_id), s(account_id)],
        )?,
    ];
    db.batch(stmts).await.map_err(internal("revoke device"))?;
    Ok(())
}

/// Delete Account: the account row and everything in D1 that names it, in
/// one batch, children first. Sign-in challenges are keyed by email, not
/// account; they stay until the daily purge, as they feed the rate limits.
/// Deleting an account that is gone already deletes nothing.
pub async fn delete_account(db: &D1Database, account_id: &str) -> ApiResult<()> {
    let stmts = [
        "DELETE FROM identity_links WHERE account_id = ?1",
        "DELETE FROM identities WHERE account_id = ?1",
        "DELETE FROM sessions WHERE account_id = ?1",
        "DELETE FROM devices WHERE account_id = ?1",
        "DELETE FROM accounts WHERE id = ?1",
    ]
    .into_iter()
    .map(|sql| stmt(db, sql, &[s(account_id)]))
    .collect::<ApiResult<Vec<_>>>()?;
    db.batch(stmts).await.map_err(internal("delete account"))?;
    Ok(())
}

/// Daily housekeeping: old challenges, expired sessions and held links.
pub async fn purge_expired(db: &D1Database, challenges_before: u64, now: u64) -> ApiResult<()> {
    let stmts = vec![
        stmt(db, "DELETE FROM auth_challenges WHERE created_at < ?1", &[n(challenges_before)])?,
        stmt(db, "DELETE FROM sessions WHERE expires_at < ?1", &[n(now)])?,
        stmt(db, "DELETE FROM identity_links WHERE expires_at < ?1", &[n(now)])?,
    ];
    db.batch(stmts).await.map_err(internal("purge"))?;
    Ok(())
}
