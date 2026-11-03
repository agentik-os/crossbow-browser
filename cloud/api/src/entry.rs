//! HTTP front door and the daily housekeeping cron.

use crossbow_proto::{HealthResponse, PROTOCOL_VERSION};
use worker::{Context, Env, Request, Response, ScheduleContext, ScheduledEvent, event};

use crate::logic::error::ApiError;
use crate::logic::limits::CHALLENGE_RETENTION_MS;
use crate::logic::route::Route;
use crate::rt::{ApiResult, error_response, internal, json, now_ms};
use crate::{ai_proxy, auth, clerk, db, sync};

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> worker::Result<Response> {
    match route(req, &env).await {
        Ok(resp) => Ok(resp),
        Err(e) => error_response(&e),
    }
}

async fn route(req: Request, env: &Env) -> ApiResult<Response> {
    let path = req.path();
    let method = req.method();
    let Some(route) = Route::parse(method.as_ref(), &path) else { return Err(ApiError::not_found()) };
    if !route.needs_session() {
        return match route {
            Route::Health => json(
                200,
                &HealthResponse {
                    ok: true,
                    protocol: PROTOCOL_VERSION,
                    service: "crossbow-api".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    clerk: clerk::provider(env).ok().map(|p| p.public()),
                },
            )
            .map_err(internal("health")),
            Route::AuthStart => auth::start(req, env).await,
            Route::AuthVerify => auth::verify(req, env).await,
            Route::AuthClerk => clerk::sign_in(req, env).await,
            _ => Err(ApiError::not_found()),
        };
    }
    let caller = auth::authenticate(&req, env).await?;
    match route {
        Route::Logout => auth::logout(env, caller).await,
        Route::Me => auth::me(env, caller).await,
        Route::ClerkLink => clerk::link(req, env, &caller).await,
        Route::ClerkLinkConfirm => clerk::confirm(req, env, &caller).await,
        Route::RevokeDevice(id) => {
            let id = id.to_string();
            auth::revoke(env, caller, &id).await
        }
        Route::SyncPush => sync::push(req, env, &caller).await,
        Route::SyncPull => sync::pull(req, env, &caller).await,
        Route::SyncSocket => sync::websocket(req, env, &caller).await,
        Route::SyncDelete => sync::delete_data(env, &caller).await,
        Route::DeleteAccount => auth::delete_account(req, env, caller).await,
        Route::Ai => ai_proxy::forward(req, env, &caller).await,
        Route::Health | Route::AuthStart | Route::AuthVerify | Route::AuthClerk => Err(ApiError::not_found()),
    }
}

#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let now = now_ms();
    let result = match db::db(&env) {
        Ok(d) => db::purge_expired(&d, now.saturating_sub(CHALLENGE_RETENTION_MS), now).await,
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        worker::console_error!("housekeeping failed: {}", e.message);
    }
}
