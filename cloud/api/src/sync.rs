//! Sync routes: validate at the edge, then forward to the account's `SyncRoom`.

use crossbow_proto::{DeleteSyncDataResponse, PushRequest};
use worker::wasm_bindgen::JsValue;
use worker::{Env, Headers, Method, Request, RequestInit, Response, Stub};

use crate::auth::Caller;
use crate::logic::error::ApiError;
use crate::logic::pull::parse_pull_query;
use crate::logic::validate::{self, MAX_PUSH_BODY_BYTES, parse_json};
use crate::rt::{ApiResult, H_DEVICE, H_SESSION_TAG, internal};

/// Host is irrelevant for Durable Object fetches; it only names the target.
const ROOM: &str = "https://sync-room";

pub fn room(env: &Env, account_id: &str) -> ApiResult<Stub> {
    env.durable_object("SYNC_ROOM")
        .and_then(|ns| ns.id_from_name(account_id)?.get_stub())
        .map_err(internal("sync room stub"))
}

fn headers(pairs: &[(&str, &str)]) -> ApiResult<Headers> {
    let h = Headers::new();
    for (k, v) in pairs {
        h.set(k, v).map_err(internal("headers"))?;
    }
    Ok(h)
}

async fn call(stub: &Stub, url: &str, method: Method, h: Headers, body: Option<String>) -> ApiResult<Response> {
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(h);
    if let Some(b) = body {
        init.with_body(Some(JsValue::from_str(&b)));
    }
    let req = Request::new_with_init(url, &init).map_err(internal("room request"))?;
    stub.fetch_with_request(req).await.map_err(internal("room fetch"))
}

/// `POST /v1/sync/push`
pub async fn push(mut req: Request, env: &Env, caller: &Caller) -> ApiResult<Response> {
    let raw = req.bytes().await.map_err(|_| ApiError::bad_request("unreadable request body"))?;
    let input: PushRequest = parse_json(&raw, MAX_PUSH_BODY_BYTES)?;
    validate::push(&input)?;
    // Re-serialise the parsed request: the room receives canonical HLC strings.
    let body = serde_json::to_string(&input).map_err(internal("push encode"))?;
    let h = headers(&[("content-type", "application/json"), (H_DEVICE, &caller.device.id)])?;
    call(&room(env, &caller.account.id)?, &format!("{ROOM}/push"), Method::Post, h, Some(body)).await
}

/// `GET /v1/sync/pull?since=&limit=`
pub async fn pull(req: Request, env: &Env, caller: &Caller) -> ApiResult<Response> {
    let url = req.url().map_err(internal("url"))?;
    let q = parse_pull_query(url.query())?;
    let target = format!("{ROOM}/pull?since={}&limit={}", q.since, q.limit);
    call(&room(env, &caller.account.id)?, &target, Method::Get, Headers::new(), None).await
}

/// `DELETE /v1/sync/data`: only the caller's account room. Sessions, devices,
/// the account and everything else in D1 stay.
pub async fn delete_data(env: &Env, caller: &Caller) -> ApiResult<Response> {
    let h = headers(&[(H_DEVICE, &caller.device.id)])?;
    call(&room(env, &caller.account.id)?, &format!("{ROOM}/data"), Method::Delete, h, None).await
}

/// Delete Account's first step: the account room's rows, as Delete Sync
/// Data deletes them, and every socket of the account closed. Anything but
/// a confirmed delete is an error, and nothing in D1 has changed yet.
pub async fn delete_account_room(env: &Env, account_id: &str) -> ApiResult<DeleteSyncDataResponse> {
    let mut res = call(&room(env, account_id)?, &format!("{ROOM}/account"), Method::Delete, Headers::new(), None).await?;
    if res.status_code() != 200 {
        worker::console_error!("account room delete answered {}", res.status_code());
        return Err(ApiError::internal());
    }
    res.json().await.map_err(internal("account room delete"))
}

/// `GET /v1/sync/ws` (WebSocket upgrade)
pub async fn websocket(req: Request, env: &Env, caller: &Caller) -> ApiResult<Response> {
    let upgrade = req.headers().get("upgrade").ok().flatten().unwrap_or_default();
    if !upgrade.eq_ignore_ascii_case("websocket") {
        return Err(ApiError::bad_request("expected a WebSocket upgrade"));
    }
    let h = headers(&[
        ("upgrade", "websocket"),
        (H_DEVICE, &caller.device.id),
        (H_SESSION_TAG, &caller.session_tag()),
    ])?;
    // Keep the client's WebSocket handshake headers (key, version, protocol).
    for (k, v) in req.headers().entries() {
        if k.starts_with("sec-websocket-") {
            h.set(&k, &v).map_err(internal("headers"))?;
        }
    }
    call(&room(env, &caller.account.id)?, &format!("{ROOM}/ws"), Method::Get, h, None).await
}

/// Close the account's sockets that carry `tag` (a session or a device).
pub async fn disconnect(env: &Env, account_id: &str, tag: &str) -> ApiResult<()> {
    let body = serde_json::json!({ "tag": tag }).to_string();
    let h = headers(&[("content-type", "application/json")])?;
    call(&room(env, account_id)?, &format!("{ROOM}/disconnect"), Method::Post, h, Some(body)).await?;
    Ok(())
}
