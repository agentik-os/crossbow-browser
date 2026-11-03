//! Sign-in mail through Resend. Logs never include the code, the key or the address.

use serde::Deserialize;
use worker::wasm_bindgen::JsValue;
use worker::{Env, Fetch, Headers, Method, Request, RequestInit};

use crate::logic::error::ApiError;
use crate::logic::limits::CHALLENGE_TTL_MS;
use crate::rt::{ApiResult, var};

const RESEND_URL: &str = "https://api.resend.com/emails";

#[derive(Deserialize)]
struct Sent {
    id: String,
}

fn unavailable<E: std::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> ApiError {
    move |_error| {
        worker::console_error!("mail: {what} failed");
        ApiError::mail_unavailable()
    }
}

pub async fn send_code(env: &Env, key: &str, to: &str, code: &str, challenge_id: &str) -> ApiResult<()> {
    let from = var(env, "MAIL_FROM").ok_or_else(|| {
        worker::console_error!("mail: MAIL_FROM is not set");
        ApiError::mail_unavailable()
    })?;
    let minutes = CHALLENGE_TTL_MS / 60_000;
    let text = format!(
        "Your Crossbow sign-in code is {code}.\n\nIt expires in {minutes} minutes. \
         If you did not try to sign in, you can ignore this email."
    );
    let html = format!(
        "<p>Your Crossbow sign-in code is</p>\
         <p style=\"font-size:28px;font-weight:600;letter-spacing:6px;font-family:ui-monospace,Menlo,monospace\">{code}</p>\
         <p>It expires in {minutes} minutes. If you did not try to sign in, you can ignore this email.</p>"
    );
    let payload = serde_json::json!({
        "from": from,
        "to": [to],
        "subject": "Your Crossbow sign-in code",
        "text": text,
        "html": html,
    });

    let headers = Headers::new();
    headers.set("authorization", &format!("Bearer {key}")).map_err(unavailable("header"))?;
    headers.set("content-type", "application/json").map_err(unavailable("header"))?;
    headers.set("idempotency-key", challenge_id).map_err(unavailable("header"))?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(JsValue::from_str(&payload.to_string())));
    let req = Request::new_with_init(RESEND_URL, &init).map_err(unavailable("request"))?;
    let mut resp = Fetch::Request(req).send().await.map_err(unavailable("fetch"))?;
    let status = resp.status_code();
    if !(200..300).contains(&status) {
        // Provider error bodies can echo recipient or request data.
        worker::console_error!("mail: resend rejected the message: status {status}");
        return Err(ApiError::mail_unavailable());
    }
    match resp.json::<Sent>().await {
        Ok(sent) => worker::console_log!("mail: resend accepted message id={}", sent.id),
        Err(_) => worker::console_log!("mail: resend accepted message (no id in response)"),
    }
    Ok(())
}
