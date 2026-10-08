//! Sign-in (production), second factor (TOTP), sign-out, current user.

use super::common::{JsonBody, JsonResult};
use crate::auth::{self, Actor, Ctx, DbCtx, SESSION_COOKIE};
use crate::audit::{self, Event};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/mode", get(mode))
        .route("/auth/me", get(me))
        .route("/auth/login", post(login))
        .route("/auth/totp", post(totp))
        .route("/auth/totp/setup", post(totp_setup))
        .route("/auth/totp/enable", post(totp_enable))
        .route("/auth/password", post(change_password))
        .route("/auth/logout", post(logout))
}

/// The `/auth/me` payload, also returned after persona switch / second factor.
pub fn me_json(conn: &Connection, actor: &Actor, state: &AppState) -> AppResult<Value> {
    let (title, persona, enrolled): (Option<String>, Option<String>, bool) = conn.query_row(
        "SELECT title, persona, totp_secret IS NOT NULL FROM users WHERE id = ?1",
        [actor.user_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(json!({
        "user": {
            "id": actor.user_id,
            "username": actor.username,
            "display_name": actor.display_name,
            "title": title,
            "is_judge": actor.is_judge,
            "persona": persona,
            "perms": actor.perms,
        },
        "mode": if state.is_demo() { "demo" } else { "production" },
        "court_name": crate::db::setting(conn, "court_name", "Court Registry")?,
        "court_timezone": crate::time::COURT_TZ_NAME,
        "mfa_enrolled": enrolled,
    }))
}

pub fn session_cookie(state: &AppState, token: &str) -> HeaderValue {
    HeaderValue::from_str(&auth::set_cookie(SESSION_COOKIE, token, state.cfg.session_hours * 3600, state.cfg.cookie_secure))
        .expect("valid cookie")
}

pub fn clear_session_cookie(state: &AppState) -> HeaderValue {
    HeaderValue::from_str(&auth::set_cookie(SESSION_COOKIE, "", 0, state.cfg.cookie_secure)).expect("valid cookie")
}

fn with_cookie(body: Value, cookie: HeaderValue) -> Response {
    let mut res = Json(body).into_response();
    res.headers_mut().append(header::SET_COOKIE, cookie);
    res
}

async fn mode(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "mode": if state.is_demo() { "demo" } else { "production" } }))
}

async fn me(State(state): State<AppState>, ctx: Ctx) -> JsonResult {
    let actor = ctx.actor.clone();
    Ok(Json(ctx.db.read(move |c| me_json(c, &actor, &state)).await?))
}

#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}

async fn login(State(state): State<AppState>, dctx: DbCtx, JsonBody(req): JsonBody<LoginReq>) -> AppResult<Response> {
    if state.is_demo() {
        return Err(AppError::not_found());
    }
    let ip = dctx.ip.clone();
    let hours = state.cfg.session_hours;
    let username = req.username.trim().to_lowercase();
    // Check lockout first (short read), then hash outside the write lock.
    let row = {
        let (u, ip) = (username.clone(), ip.clone());
        dctx.db
            .read(move |c| {
                auth::check_login_allowed(c, &u, ip.as_deref())?;
                Ok(c.query_row(
                    "SELECT id, password_hash, totp_secret IS NOT NULL FROM users WHERE username = ?1 AND active = 1",
                    [&u],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?)),
                )
                .optional()?)
            })
            .await?
    };
    let verified = match &row {
        Some((_, hash, _)) => auth::verify_password(&req.password, hash),
        None => {
            // Equalise timing for unknown users.
            static DUMMY: std::sync::LazyLock<String> =
                std::sync::LazyLock::new(|| auth::hash_password("timing-equaliser").unwrap_or_default());
            let _ = auth::verify_password(&req.password, &DUMMY);
            false
        }
    };
    let result = dctx
        .db
        .write(move |tx| {
            auth::record_login_attempt(tx, &username, ip.as_deref(), verified)?;
            let Some((uid, _, enrolled)) = row.filter(|_| verified) else {
                return Ok(None);
            };
            let token = auth::create_session(tx, uid, false, hours)?;
            audit::record(tx, None, Event::new("session.password_ok", "user", uid, "Password accepted; second factor pending"))?;
            Ok(Some((token, enrolled)))
        })
        .await?;
    match result {
        Some((token, enrolled)) => Ok(with_cookie(
            json!({ "mfa_required": enrolled, "enroll_required": !enrolled }),
            session_cookie(&state, &token),
        )),
        None => Err(AppError::new(axum::http::StatusCode::UNAUTHORIZED, "bad_credentials", "Wrong username or password.")),
    }
}

/// A session that exists but may still lack the second factor.
fn pending_session(conn: &Connection, token: &str) -> AppResult<(i64, bool)> {
    conn.query_row(
        "SELECT s.user_id, s.mfa_ok FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = ?1 AND s.revoked_at IS NULL AND s.expires_at > ?2 AND u.active = 1",
        params![auth::sha256_hex(token.as_bytes()), crate::time::now_utc()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or_else(AppError::unauthenticated)
}

/// Replace the session token after a privilege change (session fixation defence).
fn rotate(conn: &Connection, old: &str, user_id: i64, hours: i64) -> AppResult<String> {
    conn.execute(
        "UPDATE sessions SET revoked_at = ?2 WHERE token_hash = ?1",
        params![auth::sha256_hex(old.as_bytes()), crate::time::now_utc()],
    )?;
    auth::create_session(conn, user_id, true, hours)
}

fn token(headers: &HeaderMap) -> AppResult<String> {
    auth::cookie_value(headers, SESSION_COOKIE).ok_or_else(AppError::unauthenticated)
}

#[derive(Deserialize)]
struct CodeReq {
    code: String,
}

async fn totp(State(state): State<AppState>, dctx: DbCtx, headers: HeaderMap, JsonBody(req): JsonBody<CodeReq>) -> AppResult<Response> {
    let old = token(&headers)?;
    let hours = state.cfg.session_hours;
    let ip = dctx.ip.clone();
    let st = state.clone();
    let (new_token, me) = dctx
        .db
        .write(move |tx| {
            let (uid, _) = pending_session(tx, &old)?;
            let (username, secret, last): (String, Option<String>, Option<i64>) = tx.query_row(
                "SELECT username, totp_secret, totp_last_step FROM users WHERE id = ?1",
                [uid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            auth::check_login_allowed(tx, &username, ip.as_deref())?;
            let Some(secret) = secret else {
                return Err(AppError::conflict("enroll_required", "Set up your sign-in code first."));
            };
            let Some(step) = auth::verify_totp(&secret, &req.code, last) else {
                auth::record_login_attempt(tx, &username, ip.as_deref(), false)?;
                return Ok(None);
            };
            tx.execute("UPDATE users SET totp_last_step = ?2 WHERE id = ?1", params![uid, step])?;
            auth::record_login_attempt(tx, &username, ip.as_deref(), true)?;
            let new_token = rotate(tx, &old, uid, hours)?;
            let actor = auth::load_actor(tx, uid, ip.clone())?.ok_or_else(AppError::unauthenticated)?;
            audit::record(tx, Some(&actor), Event::new("session.signed_in", "user", uid, "Signed in with password and code"))?;
            Ok(Some((new_token, me_json(tx, &actor, &st)?)))
        })
        .await?
        .ok_or_else(|| AppError::new(axum::http::StatusCode::UNAUTHORIZED, "bad_code", "The code is not valid."))?;
    Ok(with_cookie(me, session_cookie(&state, &new_token)))
}

async fn totp_setup(State(state): State<AppState>, dctx: DbCtx, headers: HeaderMap) -> JsonResult {
    let old = token(&headers)?;
    let court = state.is_demo();
    let v = dctx
        .db
        .write(move |tx| {
            let (uid, _) = pending_session(tx, &old)?;
            let (username, enrolled): (String, bool) =
                tx.query_row("SELECT username, totp_secret IS NOT NULL FROM users WHERE id = ?1", [uid], |r| Ok((r.get(0)?, r.get(1)?)))?;
            if enrolled {
                return Err(AppError::conflict("already_enrolled", "A sign-in code is already set up. Ask an administrator to reset it."));
            }
            let secret = auth::base32_encode(&auth::random_bytes::<20>());
            tx.execute("UPDATE users SET totp_pending = ?2 WHERE id = ?1", params![uid, secret])?;
            let issuer = if court { "TuvaluCourtDEMO" } else { "TuvaluCourtRegister" };
            Ok(json!({
                "secret": secret,
                "otpauth_uri": format!("otpauth://totp/{issuer}:{username}?secret={secret}&issuer={issuer}&algorithm=SHA1&digits=6&period=30"),
            }))
        })
        .await?;
    Ok(Json(v))
}

async fn totp_enable(State(state): State<AppState>, dctx: DbCtx, headers: HeaderMap, JsonBody(req): JsonBody<CodeReq>) -> AppResult<Response> {
    let old = token(&headers)?;
    let hours = state.cfg.session_hours;
    let ip = dctx.ip.clone();
    let st = state.clone();
    let (new_token, me) = dctx
        .db
        .write(move |tx| {
            let (uid, _) = pending_session(tx, &old)?;
            let pending: Option<String> = tx.query_row("SELECT totp_pending FROM users WHERE id = ?1", [uid], |r| r.get(0))?;
            let pending = pending.ok_or_else(|| AppError::conflict("no_pending", "Start the set-up first."))?;
            let Some(step) = auth::verify_totp(&pending, &req.code, None) else { return Ok(None) };
            tx.execute(
                "UPDATE users SET totp_secret = totp_pending, totp_pending = NULL, totp_last_step = ?2 WHERE id = ?1",
                params![uid, step],
            )?;
            let new_token = rotate(tx, &old, uid, hours)?;
            let actor = auth::load_actor(tx, uid, ip.clone())?.ok_or_else(AppError::unauthenticated)?;
            audit::record(tx, Some(&actor), Event::new("user.totp_enrolled", "user", uid, "Sign-in code set up"))?;
            Ok(Some((new_token, me_json(tx, &actor, &st)?)))
        })
        .await?
        .ok_or_else(|| AppError::new(axum::http::StatusCode::UNAUTHORIZED, "bad_code", "The code is not valid."))?;
    Ok(with_cookie(me, session_cookie(&state, &new_token)))
}

#[derive(Deserialize)]
struct PasswordReq {
    current: String,
    new: String,
}

async fn change_password(State(state): State<AppState>, ctx: Ctx, headers: HeaderMap, JsonBody(req): JsonBody<PasswordReq>) -> AppResult<Response> {
    if state.is_demo() {
        return Err(AppError::forbidden("Demo people have no passwords."));
    }
    if req.new.chars().count() < 12 {
        return Err(AppError::validation("The new password must be at least 12 characters."));
    }
    let old = token(&headers)?;
    let actor = ctx.actor.clone();
    let current_hash: String =
        ctx.db.read(move |c| Ok(c.query_row("SELECT password_hash FROM users WHERE id = ?1", [actor.user_id], |r| r.get(0))?)).await?;
    if !auth::verify_password(&req.current, &current_hash) {
        return Err(AppError::new(axum::http::StatusCode::UNAUTHORIZED, "bad_credentials", "The current password is wrong."));
    }
    let new_hash = auth::hash_password(&req.new)?;
    let hours = state.cfg.session_hours;
    let actor = ctx.actor.clone();
    let token = ctx
        .db
        .write(move |tx| {
            tx.execute("UPDATE users SET password_hash = ?2, must_change_password = 0 WHERE id = ?1", params![actor.user_id, new_hash])?;
            auth::revoke_user_sessions(tx, actor.user_id)?;
            let t = rotate(tx, &old, actor.user_id, hours)?;
            audit::record(tx, Some(&actor), Event::new("user.password_changed", "user", actor.user_id, "Password changed; other sessions ended"))?;
            Ok(t)
        })
        .await?;
    Ok(with_cookie(json!({ "ok": true }), session_cookie(&state, &token)))
}

async fn logout(State(state): State<AppState>, dctx: DbCtx, headers: HeaderMap) -> AppResult<Response> {
    if let Some(t) = auth::cookie_value(&headers, SESSION_COOKIE) {
        dctx.db
            .write(move |tx| {
                tx.execute(
                    "UPDATE sessions SET revoked_at = ?2 WHERE token_hash = ?1 AND revoked_at IS NULL",
                    params![auth::sha256_hex(t.as_bytes()), crate::time::now_utc()],
                )?;
                Ok(())
            })
            .await?;
    }
    Ok(with_cookie(json!({ "ok": true }), clear_session_cookie(&state)))
}
