//! Demo-only endpoints: start an isolated sandbox, switch persona, reset own sandbox.
//! All return 404 in production mode.

use super::auth::{clear_session_cookie, me_json, session_cookie};
use super::common::JsonBody;
use crate::audit::{self, Event};
use crate::auth::{self, DbCtx, SANDBOX_COOKIE, SESSION_COOKIE};
use crate::error::{AppError, AppResult};
use crate::seed::PERSONAS;
use crate::state::AppState;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::json;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/demo/start", post(start))
        .route("/demo/personas", get(personas))
        .route("/demo/login", post(login))
        .route("/demo/reset", post(reset))
}

fn demo_only(state: &AppState) -> AppResult<&crate::sandbox::SandboxManager> {
    state.sandboxes.as_deref().ok_or_else(AppError::not_found)
}

fn sandbox_cookie(state: &AppState, id: &str) -> HeaderValue {
    HeaderValue::from_str(&auth::set_cookie(SANDBOX_COOKIE, id, state.cfg.sandbox_ttl_hours * 3600, state.cfg.cookie_secure))
        .expect("valid cookie")
}

async fn start(State(state): State<AppState>, headers: HeaderMap) -> AppResult<Response> {
    let mgr = state.sandboxes.clone().ok_or_else(AppError::not_found)?;
    // CSRF guard (DbCtx is not used here because there is no sandbox yet).
    if headers.get(auth::CSRF_HEADER).and_then(|v| v.to_str().ok()) != Some("1") {
        return Err(AppError::new(axum::http::StatusCode::FORBIDDEN, "csrf", "Missing request header X-TCR."));
    }
    let (id, _db) = tokio::task::spawn_blocking(move || mgr.create())
        .await
        .map_err(|e| AppError::internal(e.to_string()))??;
    let mut res = Json(json!({ "ok": true })).into_response();
    res.headers_mut().append(header::SET_COOKIE, sandbox_cookie(&state, &id));
    res.headers_mut().append(header::SET_COOKIE, clear_session_cookie(&state));
    Ok(res)
}

async fn personas(State(state): State<AppState>) -> AppResult<Json<serde_json::Value>> {
    demo_only(&state)?;
    let list: Vec<_> = PERSONAS
        .iter()
        .map(|p| json!({ "key": p.key, "display": p.display, "title": p.title, "summary": p.summary }))
        .collect();
    Ok(Json(json!(list)))
}

#[derive(Deserialize)]
struct LoginReq {
    persona: String,
}

async fn login(State(state): State<AppState>, dctx: DbCtx, headers: HeaderMap, JsonBody(req): JsonBody<LoginReq>) -> AppResult<Response> {
    demo_only(&state)?;
    let old = auth::cookie_value(&headers, SESSION_COOKIE);
    let hours = state.cfg.session_hours;
    let ip = dctx.ip.clone();
    let st = state.clone();
    let (token, me) = dctx
        .db
        .write(move |tx| {
            let uid: i64 = tx
                .query_row("SELECT id FROM users WHERE persona = ?1 AND active = 1", [&req.persona], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::validation("Unknown or deactivated person."))?;
            if let Some(old) = old {
                tx.execute(
                    "UPDATE sessions SET revoked_at = ?2 WHERE token_hash = ?1 AND revoked_at IS NULL",
                    params![auth::sha256_hex(old.as_bytes()), crate::time::now_utc()],
                )?;
            }
            let token = auth::create_session(tx, uid, true, hours)?;
            let actor = auth::load_actor(tx, uid, ip)?.ok_or_else(AppError::unauthenticated)?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("session.demo_persona", "user", uid, format!("Demo: now acting as {}", actor.display_name)),
            )?;
            Ok((token, me_json(tx, &actor, &st)?))
        })
        .await?;
    let mut res = Json(me).into_response();
    res.headers_mut().append(header::SET_COOKIE, session_cookie(&state, &token));
    Ok(res)
}

async fn reset(State(state): State<AppState>, dctx: DbCtx) -> AppResult<Response> {
    let mgr = state.sandboxes.clone().ok_or_else(AppError::not_found)?;
    let id = dctx.sandbox_id.clone().ok_or_else(AppError::not_found)?;
    tokio::task::spawn_blocking(move || mgr.reset(&id)).await.map_err(|e| AppError::internal(e.to_string()))??;
    let mut res = Json(json!({ "ok": true })).into_response();
    res.headers_mut().append(header::SET_COOKIE, clear_session_cookie(&state));
    Ok(res)
}
