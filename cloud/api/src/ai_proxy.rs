//! `/v1/ai/*`: authenticated proxy to `crossbow-ai` over the `AI_WORKER` service
//! binding. Client copies of `X-Crossbow-*` and the bearer token never reach the
//! AI Worker; the trusted account and device headers are injected here.

use worker::js_sys::Uint8Array;
use worker::{Env, Headers, Method, Request, RequestInit, Response};

use crate::auth::Caller;
use crate::logic::error::ApiError;
use crate::rt::{ApiResult, H_ACCOUNT, H_DEVICE, internal};

/// AI requests are prompts, not uploads.
const MAX_AI_BODY_BYTES: usize = 256 * 1024;

/// Headers dropped before forwarding (lowercase; Workers lowercases names).
fn dropped(name: &str) -> bool {
    name.starts_with("x-crossbow-")
        || matches!(name, "authorization" | "cookie" | "host" | "content-length")
}

pub async fn forward(mut req: Request, env: &Env, caller: &Caller) -> ApiResult<Response> {
    let url = req.url().map_err(internal("url"))?;
    let target = match url.query() {
        Some(q) => format!("https://crossbow-ai{}?{q}", url.path()),
        None => format!("https://crossbow-ai{}", url.path()),
    };

    let headers = Headers::new();
    for (k, v) in req.headers().entries() {
        if !dropped(&k.to_ascii_lowercase()) {
            headers.append(&k, &v).map_err(internal("ai headers"))?;
        }
    }
    headers.set(H_ACCOUNT, &caller.account.id).map_err(internal("ai headers"))?;
    headers.set(H_DEVICE, &caller.device.id).map_err(internal("ai headers"))?;

    let method = req.method();
    let mut init = RequestInit::new();
    init.with_method(method.clone()).with_headers(headers);
    if matches!(method, Method::Post | Method::Put | Method::Patch) {
        let body = req.bytes().await.map_err(|_| ApiError::bad_request("unreadable request body"))?;
        if body.len() > MAX_AI_BODY_BYTES {
            return Err(ApiError::payload_too_large("AI request body too large"));
        }
        init.with_body(Some(Uint8Array::from(body.as_slice()).into()));
    }
    let fwd = Request::new_with_init(&target, &init).map_err(internal("ai request"))?;
    let service = env.service("AI_WORKER").map_err(internal("AI_WORKER binding"))?;
    // The response (including a streamed body) is passed through unchanged.
    service.fetch_request(fwd).await.map_err(internal("ai fetch"))
}
