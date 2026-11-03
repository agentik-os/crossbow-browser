//! Browser sign-in with Clerk: redeem the app's code at the issuer, check
//! the ID token, then sign in, create an account or link the identity.
//! The rules are in `logic::clerk`; this is the I/O around them.

use crossbow_proto::{AuthVerifyResponse, ClerkExchangeRequest, ClerkLinkConfirmRequest, ClerkLinkResponse};
use worker::js_sys::{self, Array, Object, Reflect, Uint8Array};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::wasm_bindgen_futures::JsFuture;
use worker::{Env, Fetch, Headers, Method, Request, RequestInit, Response, Url};

use crate::auth::{self, Caller};
use crate::db::{self, Holder};
use crate::logic::account;
use crate::logic::clerk::{self, Exchange, Identity, IdToken, Jwks, Link, Provider, SignIn, provider_failed};
use crate::logic::error::ApiError;
use crate::logic::tokens::{id_well_formed, make_id};
use crate::logic::validate::{MAX_AUTH_BODY_BYTES, parse_json};
use crate::rt::{ApiResult, internal, json, no_content, now_ms, random, var};

/// The configured Clerk instance, or `503 clerk_unavailable`.
pub fn provider(env: &Env) -> ApiResult<Provider> {
    Provider::from_vars(var(env, "CLERK_ISSUER").as_deref(), var(env, "CLERK_CLIENT_ID").as_deref()).ok_or_else(ApiError::clerk_unavailable)
}

async fn exchange(req: &mut Request) -> ApiResult<Exchange> {
    let raw = req.bytes().await.map_err(|_| ApiError::bad_request("unreadable request body"))?;
    clerk::exchange(parse_json(&raw, MAX_AUTH_BODY_BYTES)?)
}

/// Redeem the code over TLS and check the ID token it returns: its claims,
/// then its signature against the issuer's keys. Clerk's access and refresh
/// tokens are never read. Anything that can't be checked refuses.
async fn redeem(provider: &Provider, x: &Exchange) -> ApiResult<Identity> {
    let url = Url::parse(&provider.token_endpoint()).map_err(internal("token endpoint"))?;
    let headers = Headers::new();
    headers.set("content-type", "application/x-www-form-urlencoded").map_err(internal("headers"))?;
    headers.set("accept", "application/json").map_err(internal("headers"))?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers).with_body(Some(JsValue::from_str(&clerk::token_request(provider, x))));
    let request = Request::new_with_init(url.as_str(), &init).map_err(internal("token request"))?;
    let mut response = Fetch::Request(request).send().await.map_err(|e| {
        worker::console_error!("token endpoint unreachable: {e}");
        provider_failed()
    })?;
    let body = response.bytes().await.map_err(|_| provider_failed())?;
    let jwt = clerk::id_token(response.status_code(), &body)?;
    let token = clerk::parse_id_token(&jwt).map_err(refused)?;
    let identity = clerk::check(&token, provider, &x.nonce, now_ms()).map_err(refused)?;
    if !signature_valid(provider, &token).await? {
        return Err(refused(clerk::Rejection::Signature));
    }
    Ok(identity)
}

fn refused(why: clerk::Rejection) -> ApiError {
    worker::console_error!("browser sign-in refused: {why:?}");
    why.error()
}

/// RS256 over `header.payload`, with the issuer's key for the token's `kid`
/// from its JWKS, through Web Crypto. A missing key is a refusal; a key set
/// that can't be read is a provider failure.
async fn signature_valid(provider: &Provider, token: &IdToken) -> ApiResult<bool> {
    let url = Url::parse(&provider.jwks_uri()).map_err(internal("jwks uri"))?;
    let mut response = Fetch::Url(url).send().await.map_err(|e| {
        worker::console_error!("jwks unreachable: {e}");
        provider_failed()
    })?;
    if response.status_code() != 200 {
        return Err(provider_failed());
    }
    let jwks: Jwks = serde_json::from_slice(&response.bytes().await.map_err(|_| provider_failed())?).map_err(|_| provider_failed())?;
    let Some(key) = clerk::signing_key(&jwks, token.kid.as_deref()) else { return Ok(false) };
    let js = |what: &'static str| move |e: JsValue| {
        worker::console_error!("web crypto {what}: {e:?}");
        ApiError::internal()
    };
    let object = |pairs: &[(&str, JsValue)]| -> ApiResult<Object> {
        let o = Object::new();
        for (k, v) in pairs {
            Reflect::set(&o, &JsValue::from_str(k), v).map_err(js("object"))?;
        }
        Ok(o)
    };
    let subtle = Reflect::get(&Reflect::get(&js_sys::global(), &"crypto".into()).map_err(js("crypto"))?, &"subtle".into()).map_err(js("subtle"))?;
    let method = |name: &str| -> ApiResult<js_sys::Function> {
        Reflect::get(&subtle, &JsValue::from_str(name)).map_err(js("method"))?.dyn_into().map_err(js("method"))
    };
    let jwk = object(&[("kty", "RSA".into()), ("n", key.n.as_str().into()), ("e", key.e.as_str().into()), ("alg", "RS256".into()), ("ext", true.into())])?;
    let algorithm = object(&[("name", "RSASSA-PKCS1-v1_5".into()), ("hash", "SHA-256".into())])?;
    let usages = Array::of1(&"verify".into());
    let args = Array::of5(&"jwk".into(), &jwk, &algorithm, &false.into(), &usages);
    let imported = Reflect::apply(&method("importKey")?, &subtle, &args).map_err(js("importKey"))?;
    let public = JsFuture::from(js_sys::Promise::from(imported)).await.map_err(js("importKey"))?;
    let args = Array::of4(&algorithm, &public, &Uint8Array::from(token.signature.as_slice()), &Uint8Array::from(token.signed.as_bytes()));
    let verified = Reflect::apply(&method("verify")?, &subtle, &args).map_err(js("verify"))?;
    let verified = JsFuture::from(js_sys::Promise::from(verified)).await.map_err(js("verify"))?;
    Ok(verified.as_bool() == Some(true))
}

/// `POST /v1/auth/clerk`: sign in to the identity's account, or create one
/// for a new identity whose verified email no account uses. The session is
/// the email-code path's: `auth::new_session`.
pub async fn sign_in(mut req: Request, env: &Env) -> ApiResult<Response> {
    let provider = provider(env)?;
    let x = exchange(&mut req).await?;
    let identity = redeem(&provider, &x).await?;
    let db = db::db(env)?;
    // A second look only after the account went, or the email or identity
    // was taken, between the decision and the batch.
    for _ in 0..2 {
        let decision = decide(&db, &identity).await?;
        let holder = match &decision {
            SignIn::Account(id) => Holder::Existing(id),
            SignIn::Create { email } => Holder::NewIdentity { email, issuer: &identity.issuer, subject: &identity.subject },
            refused => return Err(refused.error().unwrap_or_else(ApiError::internal)),
        };
        if let Some((session_token, account, device)) = auth::new_session(&db, holder, x.device_name.clone(), x.platform.clone()).await? {
            let display_name = identity.name.clone();
            return json(200, &AuthVerifyResponse { session_token, account, device, display_name }).map_err(internal("clerk response"));
        }
    }
    worker::console_error!("browser sign-in: no account after two decisions");
    Err(ApiError::internal())
}

async fn decide(db: &worker::D1Database, identity: &Identity) -> ApiResult<SignIn> {
    let known = db::identity_account(db, &identity.issuer, &identity.subject).await?;
    let email_owner = match (&known, &identity.email) {
        (None, Some(email)) => db::account_by_email(db, email).await?,
        _ => None,
    };
    Ok(clerk::sign_in(known, identity, email_owner))
}

/// Delete Account's browser step-up: a sign-in redeemed and checked as any,
/// of an identity linked to the caller's account, issued at most 5 minutes
/// ago. None of its refusals is a 401.
pub async fn step_up(env: &Env, request: ClerkExchangeRequest, caller: &Caller) -> ApiResult<()> {
    let provider = provider(env)?;
    let x = clerk::exchange(request)?;
    let identity = redeem(&provider, &x).await.map_err(account::step_up_error)?;
    let db = db::db(env)?;
    let known = db::identity_account(&db, &identity.issuer, &identity.subject).await?;
    account::browser_step_up(known.as_deref(), &caller.account.id, identity.issued_at_ms, now_ms())
}

/// `POST /v1/auth/clerk/link`: a browser sign-in for the caller's account,
/// held until the caller confirms it, naming its email.
pub async fn link(mut req: Request, env: &Env, caller: &Caller) -> ApiResult<Response> {
    let provider = provider(env)?;
    let x = exchange(&mut req).await?;
    let identity = redeem(&provider, &x).await?;
    let db = db::db(env)?;
    let known = db::identity_account(&db, &identity.issuer, &identity.subject).await?;
    match clerk::link(known.as_deref(), &caller.account.id) {
        Link::InUse => Err(ApiError::identity_in_use()),
        Link::Already => json(200, &ClerkLinkResponse { link_id: None, email: identity.verified_email().map(String::from), expires_in: 0 }).map_err(internal("link response")),
        Link::Pending => {
            let link_id = make_id("lnk", &random::<16>()?);
            let now = now_ms();
            db::insert_link(&db, &link_id, &caller.account.id, &caller.token_hash, &identity, now, now + clerk::LINK_TTL_MS).await?;
            json(200, &ClerkLinkResponse { link_id: Some(link_id), email: identity.verified_email().map(String::from), expires_in: clerk::LINK_TTL_MS / 1000 })
                .map_err(internal("link response"))
        }
    }
}

/// `POST /v1/auth/clerk/link/confirm`: bind a held browser sign-in to the
/// caller's account, whose id stays as it is.
pub async fn confirm(mut req: Request, env: &Env, caller: &Caller) -> ApiResult<Response> {
    provider(env)?;
    let raw = req.bytes().await.map_err(|_| ApiError::bad_request("unreadable request body"))?;
    let input: ClerkLinkConfirmRequest = parse_json(&raw, MAX_AUTH_BODY_BYTES)?;
    if !id_well_formed(&input.link_id) {
        return Err(ApiError::not_found());
    }
    let db = db::db(env)?;
    let now = now_ms();
    let (issuer, subject) = db::pending_link(&db, &input.link_id, &caller.account.id, &caller.token_hash, now).await?.ok_or_else(ApiError::not_found)?;
    if let Some(done) = settled(&db, &input.link_id, &issuer, &subject, caller).await? {
        return done;
    }
    if db::bind_link(&db, &input.link_id, &caller.account.id, &caller.token_hash, now).await? {
        return no_content().map_err(internal("confirm response"));
    }
    // Not bound: another request bound the identity first, or the link
    // expired meanwhile.
    settled(&db, &input.link_id, &issuer, &subject, caller).await?.unwrap_or_else(|| Err(ApiError::not_found()))
}

/// The answer when the identity is bound already, to the caller (the link
/// is dropped) or to another account; `None` while it is free.
async fn settled(
    db: &worker::D1Database,
    link_id: &str,
    issuer: &str,
    subject: &str,
    caller: &Caller,
) -> ApiResult<Option<ApiResult<Response>>> {
    let known = db::identity_account(db, issuer, subject).await?;
    Ok(match clerk::link(known.as_deref(), &caller.account.id) {
        Link::Pending => None,
        Link::InUse => Some(Err(ApiError::identity_in_use())),
        Link::Already => {
            db::delete_link(db, link_id).await?;
            Some(no_content().map_err(internal("confirm response")))
        }
    })
}
