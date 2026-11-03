//! Email-code sign-in, sessions, devices and Delete Account. Browser sign-in
//! (`clerk`) ends in the same `new_session`.

use crossbow_proto::{
    Account, AuthStartRequest, AuthStartResponse, AuthVerifyRequest, AuthVerifyResponse, DEV_KEY_HEADER,
    DeleteAccountRequest, DeleteAccountResponse, Device, MeResponse, StepUp,
};
use worker::{D1Database, Env, Request, Response};

use crate::db::{self, Holder, NewSignIn};
use crate::logic::account;
use crate::logic::error::ApiError;
use crate::logic::limits::{
    CHALLENGE_RETENTION_MS, CHALLENGE_TTL_MS, CodeDelivery, MAX_VERIFY_ATTEMPTS, SESSION_TTL_MS,
    challenge_state, check_start_limits, code_delivery, session_valid, should_touch, window_start,
};
use crate::logic::tokens::{
    bearer_token, code_hash, code_well_formed, ct_eq, id_well_formed, make_code, make_id, make_token, token_hash,
};
use crate::logic::validate::{self, MAX_AUTH_BODY_BYTES, parse_json};
use crate::rt::{ApiResult, internal, is_dev, json, no_content, now_ms, random, secret};
use crate::{clerk, mail, sync};

/// The authenticated caller of a request.
pub struct Caller {
    pub account: Account,
    pub device: Device,
    pub token_hash: String,
}

impl Caller {
    /// WebSocket tag that identifies this session inside `SyncRoom`.
    pub fn session_tag(&self) -> String {
        session_tag(&self.token_hash)
    }
}

pub fn session_tag(token_hash: &str) -> String {
    format!("s:{token_hash}")
}

pub fn device_tag(device_id: &str) -> String {
    format!("d:{device_id}")
}

fn pepper(env: &Env) -> ApiResult<String> {
    secret(env, "AUTH_PEPPER").ok_or_else(|| {
        worker::console_error!("AUTH_PEPPER is not set");
        ApiError::internal()
    })
}

async fn body(req: &mut Request) -> ApiResult<Vec<u8>> {
    req.bytes().await.map_err(|_| ApiError::bad_request("unreadable request body"))
}

/// `Authorization: Bearer <token>` → caller, or `401 unauthorized`.
pub async fn authenticate(req: &Request, env: &Env) -> ApiResult<Caller> {
    let header = req.headers().get("authorization").ok().flatten();
    let token = bearer_token(header.as_deref()).ok_or_else(ApiError::unauthorized)?;
    let th = token_hash(token);
    let db = db::db(env)?;
    let row = db::session(&db, &th).await?.ok_or_else(ApiError::unauthorized)?;
    let now = now_ms();
    if !session_valid(row.expires_at as u64, row.revoked_at.map(|v| v as u64), now) {
        if (row.expires_at as u64) <= now {
            let _ = db::delete_session(&db, &th).await;
        }
        return Err(ApiError::unauthorized());
    }
    let mut last_seen_at = row.last_seen_at as u64;
    if should_touch(last_seen_at, now) {
        db::touch(&db, &row.device_id, &th, now, now + SESSION_TTL_MS).await?;
        last_seen_at = now;
    }
    Ok(Caller {
        account: Account {
            id: row.account_id,
            email: row.email,
            created_at: row.account_created_at as u64,
        },
        device: Device {
            id: row.device_id,
            name: row.device_name,
            platform: row.platform,
            created_at: row.device_created_at as u64,
            last_seen_at,
        },
        token_hash: th,
    })
}

/// `POST /v1/auth/start`
pub async fn start(mut req: Request, env: &Env) -> ApiResult<Response> {
    let input: AuthStartRequest = parse_json(&body(&mut req).await?, MAX_AUTH_BODY_BYTES)?;
    let email = validate::auth_start(&input)?;
    let ip = req.headers().get("cf-connecting-ip").ok().flatten().filter(|s| !s.is_empty());
    let now = now_ms();
    let db = db::db(env)?;

    let (email_starts, ip_starts) = db::count_starts(&db, &email, ip.as_deref(), window_start(now)).await?;
    check_start_limits(email_starts, ip_starts)?;

    let pepper = pepper(env)?;
    let resend_key = secret(env, "RESEND_API_KEY");
    let presented = req.headers().get(DEV_KEY_HEADER).ok().flatten();
    let delivery =
        code_delivery(is_dev(env), resend_key.is_some(), secret(env, "DEV_CODE_KEY").as_deref(), presented.as_deref());
    if delivery == CodeDelivery::Unavailable {
        worker::console_error!("RESEND_API_KEY is not set outside dev");
        return Err(ApiError::mail_unavailable());
    }

    let challenge_id = make_id("chl", &random::<16>()?);
    let pool = random::<64>()?;
    let mut i = 0usize;
    let code = make_code(|| {
        let at = (i % 16) * 4;
        i += 1;
        u32::from_le_bytes([pool[at], pool[at + 1], pool[at + 2], pool[at + 3]])
    });
    let hash = code_hash(&pepper, &challenge_id, &code);
    db::insert_challenge(&db, &challenge_id, &email, &hash, ip.as_deref(), now, now + CHALLENGE_TTL_MS)
        .await?;
    // Old rows only fed the rate limits; dropping them is best-effort.
    let _ = db::purge_challenges_for(&db, &email, now.saturating_sub(CHALLENGE_RETENTION_MS)).await;

    let dev_code = match (delivery, resend_key.as_deref()) {
        (CodeDelivery::Reveal, _) => Some(code),
        (CodeDelivery::Mail, Some(key)) => {
            mail::send_code(env, key, &email, &code, &challenge_id).await?;
            None
        }
        // Dev without mail, to a caller without the dev key: the same answer
        // as a mailed code, and the code goes nowhere.
        _ => None,
    };
    json(200, &AuthStartResponse { challenge_id, expires_in: CHALLENGE_TTL_MS / 1000, dev_code })
        .map_err(internal("start response"))
}

/// `POST /v1/auth/verify`
pub async fn verify(mut req: Request, env: &Env) -> ApiResult<Response> {
    let input: AuthVerifyRequest = parse_json(&body(&mut req).await?, MAX_AUTH_BODY_BYTES)?;
    let v = validate::auth_verify(input)?;
    let db = db::db(env)?;
    let row = db::challenge(&db, &v.challenge_id).await?.ok_or_else(ApiError::invalid_code)?;
    let now = now_ms();
    let state = challenge_state(
        row.attempts as u64,
        row.expires_at as u64,
        row.consumed_at.map(|t| t as u64),
        now,
    );
    if let Some(e) = state.error() {
        return Err(e);
    }

    let expected = code_hash(&pepper(env)?, &v.challenge_id, &v.code);
    if v.code.is_empty() || !ct_eq(&expected, &row.code_hash) {
        db::record_wrong_attempt(&db, &v.challenge_id).await?;
        return Err(ApiError::invalid_code());
    }
    if !db::consume_challenge(&db, &v.challenge_id, now, MAX_VERIFY_ATTEMPTS).await? {
        return Err(ApiError::invalid_code());
    }

    let (session_token, account, device) = new_session(&db, Holder::Email(&row.email), v.device_name, v.platform)
        .await?
        .ok_or_else(|| {
            worker::console_error!("sign-in batch returned no account row");
            ApiError::internal()
        })?;
    json(200, &AuthVerifyResponse { session_token, account, device, display_name: None }).map_err(internal("verify response"))
}

/// A new device and session for the holder's account, as every sign-in
/// makes them: (session token, account, device). `None` when that account
/// isn't there to sign in to.
pub async fn new_session(
    db: &D1Database,
    holder: Holder<'_>,
    device_name: String,
    platform: String,
) -> ApiResult<Option<(String, Account, Device)>> {
    let session_token = make_token(&random::<32>()?);
    let th = token_hash(&session_token);
    let device_id = make_id("dev", &random::<16>()?);
    let new_account_id = make_id("acc", &random::<16>()?);
    let now = now_ms();
    let account = db::sign_in(
        db,
        NewSignIn {
            holder,
            new_account_id: &new_account_id,
            device_id: &device_id,
            device_name: &device_name,
            platform: &platform,
            token_hash: &th,
            now,
            session_expires_at: now + SESSION_TTL_MS,
        },
    )
    .await?;
    Ok(account.map(|account| {
        let device = Device { id: device_id, name: device_name, platform, created_at: now, last_seen_at: now };
        (session_token, account, device)
    }))
}

/// `POST /v1/auth/logout`
pub async fn logout(env: &Env, caller: Caller) -> ApiResult<Response> {
    let db = db::db(env)?;
    db::delete_session(&db, &caller.token_hash).await?;
    // Close this session's sockets; failure here must not fail the logout.
    let _ = sync::disconnect(env, &caller.account.id, &caller.session_tag()).await;
    no_content().map_err(internal("logout response"))
}

/// `GET /v1/me`
pub async fn me(env: &Env, caller: Caller) -> ApiResult<Response> {
    let db = db::db(env)?;
    let devices = db::active_devices(&db, &caller.account.id).await?;
    let browser_sign_in = db::has_identity(&db, &caller.account.id).await?;
    json(200, &MeResponse { account: caller.account, device: caller.device, devices, browser_sign_in })
        .map_err(internal("me response"))
}

/// `DELETE /v1/devices/{id}`
pub async fn revoke(env: &Env, caller: Caller, device_id: &str) -> ApiResult<Response> {
    if !id_well_formed(device_id) {
        return Err(ApiError::not_found());
    }
    let db = db::db(env)?;
    if !db::device_in_account(&db, device_id, &caller.account.id).await? {
        return Err(ApiError::not_found());
    }
    db::revoke_device(&db, device_id, &caller.account.id, now_ms()).await?;
    let _ = sync::disconnect(env, &caller.account.id, &device_tag(device_id)).await;
    no_content().map_err(internal("revoke response"))
}

/// `DELETE /v1/account`: the caller's account, once the user typed its
/// email and stepped up within 5 minutes with an email code or a linked
/// browser sign-in. The account room goes first (its rows, and every socket
/// closed), then the account, its devices, sessions, identities and held
/// links in one D1 batch, so a failure part way leaves a session to retry
/// with. The Clerk user, if any, is the provider's and stays.
pub async fn delete_account(mut req: Request, env: &Env, caller: Caller) -> ApiResult<Response> {
    let input: DeleteAccountRequest = parse_json(&body(&mut req).await?, MAX_AUTH_BODY_BYTES)?;
    account::confirmation(&input.confirm, &caller.account.email)?;
    let db = db::db(env)?;
    match input.step_up {
        StepUp::EmailCode { challenge_id, code } => email_step_up(env, &db, &caller, &challenge_id, &code).await?,
        StepUp::Browser(exchange) => clerk::step_up(env, exchange, &caller).await?,
    }
    let room = sync::delete_account_room(env, &caller.account.id).await?;
    db::delete_account(&db, &caller.account.id).await?;
    json(200, &DeleteAccountResponse { deleted_rows: room.deleted_rows }).map_err(internal("delete account response"))
}

/// A code from `/v1/auth/start` for the caller's email, at most 5 minutes
/// old, used up here. A wrong code counts as an attempt, as at sign-in.
async fn email_step_up(env: &Env, db: &D1Database, caller: &Caller, challenge_id: &str, code: &str) -> ApiResult<()> {
    if !id_well_formed(challenge_id) {
        return Err(account::wrong_code());
    }
    let row = db::challenge(db, challenge_id).await?.ok_or_else(account::wrong_code)?;
    let now = now_ms();
    let state = challenge_state(row.attempts as u64, row.expires_at as u64, row.consumed_at.map(|t| t as u64), now);
    account::email_step_up(state, &row.email, row.created_at as u64, &caller.account.email, now)?;
    let code = code.trim();
    let expected = code_hash(&pepper(env)?, challenge_id, code);
    if !code_well_formed(code) || !ct_eq(&expected, &row.code_hash) {
        db::record_wrong_attempt(db, challenge_id).await?;
        return Err(account::wrong_code());
    }
    if !db::consume_challenge(db, challenge_id, now, MAX_VERIFY_ATTEMPTS).await? {
        return Err(account::wrong_code());
    }
    Ok(())
}
