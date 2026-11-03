//! Small runtime helpers: clock, randomness, responses, configuration.

use serde::Serialize;
use worker::{Env, Response, ResponseBuilder};

use crate::logic::error::ApiError;

pub type ApiResult<T> = std::result::Result<T, ApiError>;

/// Internal headers between the API Worker, `SyncRoom` and `crossbow-ai`.
pub const H_ACCOUNT: &str = "X-Crossbow-Account";
pub const H_DEVICE: &str = "X-Crossbow-Device";
pub const H_SESSION_TAG: &str = "X-Crossbow-Session-Tag";

pub fn now_ms() -> u64 {
    worker::Date::now().as_millis()
}

/// Bytes from the platform CSPRNG (Web Crypto `getRandomValues`).
pub fn random<const N: usize>() -> ApiResult<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::getrandom(&mut buf).map_err(|e| {
        worker::console_error!("getrandom failed: {e}");
        ApiError::internal()
    })?;
    Ok(buf)
}

/// Log the underlying error server-side and return a generic 500.
pub fn internal<E: std::fmt::Display>(context: &str) -> impl FnOnce(E) -> ApiError + '_ {
    move |e| {
        worker::console_error!("{context}: {e}");
        ApiError::internal()
    }
}

fn builder(status: u16) -> ResponseBuilder {
    // Static name and value: this cannot fail in practice.
    ResponseBuilder::new()
        .with_status(status)
        .with_header("cache-control", "no-store")
        .unwrap_or_else(|_| ResponseBuilder::new().with_status(status))
}

pub fn json<T: Serialize>(status: u16, value: &T) -> worker::Result<Response> {
    builder(status).from_json(value)
}

pub fn no_content() -> worker::Result<Response> {
    Ok(builder(204).empty())
}

pub fn error_response(e: &ApiError) -> worker::Result<Response> {
    Ok(builder(e.status)
        .with_header("content-type", "application/json")?
        .fixed(e.body_json().into_bytes()))
}

/// `ENVIRONMENT` var: `dev` or `prod`.
pub fn environment(env: &Env) -> String {
    env.var("ENVIRONMENT").map(|v| v.to_string()).unwrap_or_else(|_| "prod".into())
}

pub fn is_dev(env: &Env) -> bool {
    environment(env) == "dev"
}

/// A secret that is set and non-empty.
pub fn secret(env: &Env, name: &str) -> Option<String> {
    env.secret(name).ok().map(|s| s.to_string()).filter(|s| !s.is_empty())
}

pub fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|s| s.to_string()).filter(|s| !s.is_empty())
}
