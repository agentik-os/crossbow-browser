//! HTTP front door. User input, provider bodies and credentials are never logged.

use std::{net::IpAddr, time::Duration};

use futures_util::{
    StreamExt,
    future::{Either, select},
    pin_mut,
};
use serde_json::json;
use url::Url;
use worker::wasm_bindgen::JsValue;
use worker::{
    AbortController, Context, Delay, Env, Fetch, Headers, Method, RateLimiter, Request,
    RequestInit, RequestRedirect, Response, event,
};

use crate::logic::{
    self, ClerkCreate, MAX_BODY_BYTES, MAX_PROVIDER_BYTES, SUCCESS_MESSAGE, Settings, SiteError,
};

const CLERK_ENDPOINT: &str = "https://api.clerk.com/v1/waitlist_entries";
const TURNSTILE_ENDPOINT: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const CSP: &str = "default-src 'self'; script-src 'self' https://challenges.cloudflare.com; \
    style-src 'self' 'unsafe-inline'; \
    font-src 'self'; img-src 'self' data:; \
    connect-src 'self' https://challenges.cloudflare.com; \
    frame-src https://challenges.cloudflare.com; object-src 'none'; \
    base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

type SiteResult<T> = std::result::Result<T, SiteError>;

// No Debug implementation: both provider credentials must remain private.
struct ActiveConfig {
    site_origin: Url,
    site_key: String,
    clerk_secret: String,
    turnstile_secret: String,
    limiter: RateLimiter,
}

fn var(env: &Env, name: &str) -> String {
    env.var(name).map(|v| v.to_string()).unwrap_or_default()
}

fn active_config(env: &Env, request_url: &Url) -> Option<ActiveConfig> {
    let enabled = var(env, "WAITLIST_ENABLED");
    if enabled != "true" {
        return None;
    }
    let environment = var(env, "ENVIRONMENT");
    let site_origin = var(env, "SITE_ORIGIN");
    let site_key = var(env, "TURNSTILE_SITE_KEY");
    let privacy_contact = var(env, "PRIVACY_CONTACT");
    let clerk_secret = env.secret("CLERK_SECRET_KEY").ok()?.to_string();
    let turnstile_secret = env.secret("TURNSTILE_SECRET_KEY").ok()?.to_string();
    let limiter = env.rate_limiter("WAITLIST_RATE_LIMITER").ok()?;
    if !(Settings {
        enabled: &enabled,
        environment: &environment,
        site_origin: &site_origin,
        site_key: &site_key,
        clerk_secret: &clerk_secret,
        turnstile_secret: &turnstile_secret,
        privacy_contact: &privacy_contact,
        has_rate_limiter: true,
    })
    .ready()
    {
        return None;
    }
    let site_origin = logic::parse_site_origin(&site_origin)?;
    if !logic::same_site_url(request_url, &site_origin) {
        return None;
    }
    Some(ActiveConfig {
        site_origin,
        site_key,
        clerk_secret,
        turnstile_secret,
        limiter,
    })
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> worker::Result<Response> {
    let is_https = req.url().is_ok_and(|url| url.scheme() == "https");
    let mut response = match route(req, &env).await {
        Ok(response) => response,
        Err(error) => error_response(error)?,
    };
    let headers = response.headers_mut();
    headers.set("Content-Security-Policy", CSP)?;
    headers.set("X-Content-Type-Options", "nosniff")?;
    headers.set("Referrer-Policy", "no-referrer")?;
    headers.set("X-Frame-Options", "DENY")?;
    headers.set(
        "Permissions-Policy",
        "camera=(), microphone=(), geolocation=(), browsing-topics=()",
    )?;
    if is_https {
        headers.set("Strict-Transport-Security", "max-age=31536000")?;
    }
    Ok(response)
}

async fn route(mut req: Request, env: &Env) -> SiteResult<Response> {
    let url = req.url().map_err(|_| SiteError::INVALID)?;
    let method = req.method();
    if let Some(target) = logic::legacy_redirect(&url) {
        if !matches!(method, Method::Get | Method::Head) {
            return method_not_allowed("GET, HEAD");
        }
        let mut response = Response::empty()
            .map_err(|_| SiteError::UNAVAILABLE)?
            .with_status(308);
        response
            .headers_mut()
            .set("Location", target.as_str())
            .map_err(|_| SiteError::UNAVAILABLE)?;
        response
            .headers_mut()
            .set("Cache-Control", "public, max-age=300")
            .map_err(|_| SiteError::UNAVAILABLE)?;
        return Ok(response);
    }
    match url.path() {
        "/api/config" => {
            if method != Method::Get {
                return method_not_allowed("GET");
            }
            let config = active_config(env, &url);
            let public = logic::public_config(
                config.as_ref().map(|c| c.site_key.as_str()),
                &var(env, "PRIVACY_CONTACT"),
            );
            json_response(
                200,
                serde_json::to_value(public).map_err(|_| SiteError::UNAVAILABLE)?,
            )
        }
        "/api/waitlist" => {
            if method != Method::Post {
                return method_not_allowed("POST");
            }
            let config = active_config(env, &url).ok_or(SiteError::UNAVAILABLE)?;
            submit(&mut req, &url, config).await
        }
        path if path == "/api" || path.starts_with("/api/") => {
            json_response(404, json!({"ok":false,"message":"Not found."}))
        }
        _ => {
            if !matches!(method, Method::Get | Method::Head) {
                return method_not_allowed("GET, HEAD");
            }
            env.assets("ASSETS")
                .map_err(|_| SiteError::UNAVAILABLE)?
                .fetch_request(req)
                .await
                .map_err(|_| SiteError::UNAVAILABLE)
        }
    }
}

async fn submit(req: &mut Request, url: &Url, config: ActiveConfig) -> SiteResult<Response> {
    let origin = header(req, "Origin")?;
    let fetch_site = header(req, "Sec-Fetch-Site")?;
    if !logic::valid_submit_context(
        url,
        origin.as_deref(),
        fetch_site.as_deref(),
        &config.site_origin,
    ) {
        return Err(SiteError::FORBIDDEN);
    }
    logic::validate_content_type(header(req, "Content-Type")?.as_deref())?;
    logic::validate_content_length(header(req, "Content-Length")?.as_deref())?;
    if header(req, "Content-Encoding")?.is_some_and(|e| !e.eq_ignore_ascii_case("identity")) {
        return Err(SiteError::CONTENT_TYPE);
    }

    // Cloudflare supplies this header at the edge. Never trust a body/X-Forwarded-For IP.
    // Missing IP is an unavailable protection, not permission to bypass the limiter.
    let ip: IpAddr = header(req, "CF-Connecting-IP")?
        .ok_or(SiteError::UNAVAILABLE)?
        .parse()
        .map_err(|_| SiteError::UNAVAILABLE)?;
    let allowed = config
        .limiter
        .limit(format!("crossbow-site-v1:ip:{ip}"))
        .await
        .map_err(|_| SiteError::UNAVAILABLE)?;
    if !allowed.success {
        return Err(SiteError::RATE_LIMITED);
    }

    let input = logic::parse_submission(&read_body(req).await?)?;
    let turnstile_request = json!({
        "secret": config.turnstile_secret,
        "response": input.turnstile_token,
        "remoteip": ip.to_string(),
    });
    let (status, body) = provider_request(TURNSTILE_ENDPOINT, &turnstile_request, None).await?;
    let hostname = config
        .site_origin
        .host_str()
        .ok_or(SiteError::UNAVAILABLE)?;
    logic::confirm_turnstile(status, &body, hostname)?;

    // Clerk's SDK converts emailAddress to email_address on the HTTP wire.
    // notify:false avoids sending mail on retries; waitlist creation is duplicate-neutral.
    let clerk_request = serde_json::to_value(ClerkCreate {
        email_address: &input.email,
        notify: false,
    })
    .map_err(|_| SiteError::UNAVAILABLE)?;
    let (status, body) =
        provider_request(CLERK_ENDPOINT, &clerk_request, Some(&config.clerk_secret)).await?;
    logic::confirm_clerk_entry(status, &body, &input.email)?;
    json_response(200, json!({"ok":true,"message":SUCCESS_MESSAGE}))
}

fn header(req: &Request, name: &str) -> SiteResult<Option<String>> {
    req.headers().get(name).map_err(|_| SiteError::INVALID)
}

async fn read_body(req: &mut Request) -> SiteResult<Vec<u8>> {
    let stream = req.stream().map_err(|_| SiteError::INVALID)?;
    let read = async move {
        let mut stream = stream;
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| SiteError::INVALID)?;
            if !logic::append_bounded(&mut bytes, &chunk, MAX_BODY_BYTES) {
                return Err(SiteError::TOO_LARGE);
            }
        }
        Ok(bytes)
    };
    pin_mut!(read);
    match select(read, Delay::from(IO_TIMEOUT)).await {
        Either::Left((result, _)) => result,
        Either::Right(_) => Err(SiteError {
            status: 408,
            message: "The request timed out. Please try again.",
            retry_after: None,
        }),
    }
}

async fn provider_request(
    url: &'static str,
    payload: &serde_json::Value,
    secret: Option<&str>,
) -> SiteResult<(u16, Vec<u8>)> {
    let headers = Headers::new();
    headers
        .set("Content-Type", "application/json")
        .map_err(|_| SiteError::UNAVAILABLE)?;
    headers
        .set("Accept", "application/json")
        .map_err(|_| SiteError::UNAVAILABLE)?;
    if let Some(secret) = secret {
        headers
            .set("Authorization", &format!("Bearer {secret}"))
            .map_err(|_| SiteError::UNAVAILABLE)?;
    }
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_redirect(RequestRedirect::Error)
        .with_body(Some(JsValue::from_str(&payload.to_string())));
    let request = Request::new_with_init(url, &init).map_err(|_| SiteError::UNAVAILABLE)?;
    let controller = AbortController::default();
    let signal = controller.signal();
    let call = async {
        let mut response = Fetch::Request(request)
            .send_with_signal(&signal)
            .await
            .map_err(|_| SiteError::UNAVAILABLE)?;
        let status = response.status_code();
        if !matches!(status, 200 | 201) {
            // Do not parse, return or log provider errors; they can contain submitted data.
            return Ok((status, Vec::new()));
        }
        let mut stream = response.stream().map_err(|_| SiteError::UNAVAILABLE)?;
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| SiteError::UNAVAILABLE)?;
            if !logic::append_bounded(&mut bytes, &chunk, MAX_PROVIDER_BYTES) {
                return Err(SiteError::UNAVAILABLE);
            }
        }
        Ok((status, bytes))
    };
    pin_mut!(call);
    match select(call, Delay::from(IO_TIMEOUT)).await {
        Either::Left((result, _)) => result,
        Either::Right(_) => {
            controller.abort();
            Err(SiteError::UNAVAILABLE)
        }
    }
}

fn json_response(status: u16, value: serde_json::Value) -> SiteResult<Response> {
    let mut response = Response::from_json(&value)
        .map_err(|_| SiteError::UNAVAILABLE)?
        .with_status(status);
    response
        .headers_mut()
        .set("Cache-Control", "no-store")
        .map_err(|_| SiteError::UNAVAILABLE)?;
    Ok(response)
}

fn method_not_allowed(allow: &'static str) -> SiteResult<Response> {
    let mut response = json_response(405, json!({"ok":false,"message":"Method not allowed."}))?;
    response
        .headers_mut()
        .set("Allow", allow)
        .map_err(|_| SiteError::UNAVAILABLE)?;
    Ok(response)
}

fn error_response(error: SiteError) -> worker::Result<Response> {
    let mut response = Response::from_json(&json!({"ok":false,"message":error.message}))?
        .with_status(error.status);
    response.headers_mut().set("Cache-Control", "no-store")?;
    if let Some(seconds) = error.retry_after {
        response
            .headers_mut()
            .set("Retry-After", &seconds.to_string())?;
    }
    Ok(response)
}
