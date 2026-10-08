//! C01 administration (users, permissions, sessions) and C18 self-sufficient operation
//! (court units, number-series registries, rooms, reference lists, message templates, settings).
//!
//! `admin.users` manages accounts; `admin.settings` manages reference data. A technical
//! administrator NEVER gains case access through these screens: judicial and case-visibility
//! permissions are outside `policy::perm::ADMIN_GRANTABLE` and must be granted from the
//! command line by the court authority (`tuvalu-court grant`).

use super::common::{JsonBody, JsonResult, optional, query_json, query_one_json, reason, required};
use crate::audit::{self, Event};
use crate::auth::{self, Ctx};
use crate::error::{AppError, AppResult};
use crate::policy::perm;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/permissions", get(permission_list))
        .route("/admin/users", get(user_list).post(user_create))
        .route("/admin/users/{id}", patch(user_update))
        .route("/admin/users/{id}/permissions", put(user_permissions))
        .route("/admin/users/{id}/deactivate", post(user_deactivate))
        .route("/admin/users/{id}/reactivate", post(user_reactivate))
        .route(
            "/admin/users/{id}/revoke-sessions",
            post(user_revoke_sessions),
        )
        .route(
            "/admin/users/{id}/reset-password",
            post(user_reset_password),
        )
        .route("/admin/users/{id}/reset-mfa", post(user_reset_mfa))
        .route(
            "/admin/court-units",
            get(court_unit_list).post(court_unit_create),
        )
        .route("/admin/court-units/{id}", patch(court_unit_update))
        .route(
            "/admin/registries",
            get(registry_list).post(registry_create),
        )
        .route("/admin/registries/{id}", patch(registry_update))
        .route("/admin/rooms", get(room_list).post(room_create))
        .route("/admin/rooms/{id}", patch(room_update))
        .route("/admin/ref-items", get(ref_item_list).post(ref_item_create))
        .route("/admin/ref-items/{id}", patch(ref_item_update))
        .route("/admin/templates", get(template_list).post(template_create))
        .route("/admin/templates/{id}", patch(template_update))
        .route("/admin/settings", get(settings_get).put(settings_put))
}

const GRANT_MESSAGE: &str =
    "This permission must be granted by the court authority on the server (tuvalu-court grant)";

const REF_KINDS: &[&str] = &[
    "case_category",
    "intake_channel",
    "origin_island",
    "document_type",
    "closure_basis",
    "hearing_type",
    "participant_role",
    "dispatch_method",
    "relation_kind",
];

/// Placeholders allowed in message templates (checked on create/update).
const TEMPLATE_VARS: &[&str] = &[
    "court",
    "recipient",
    "case_number",
    "case_title",
    "hearing_local",
    "hearing_type",
    "room",
    "previous_local",
    "reason",
    "intake_reference",
    "received_date",
    "missing_items",
    "items",
];

fn valid_username(s: &str) -> bool {
    (3..=32).contains(&s.len())
        && s.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

fn valid_series(s: &str) -> bool {
    (2..=20).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
}

fn valid_code(s: &str) -> bool {
    (2..=40).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn valid_prefix(s: &str) -> bool {
    (1..=6).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_uppercase())
}

/// One-time sign-in secret: 16 characters, URL-safe.
fn temporary_password() -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    crate::auth::random_bytes::<16>()
        .iter()
        .map(|b| ALPHABET[(b & 63) as usize] as char)
        .collect()
}

fn permission_known(p: &str) -> bool {
    perm::ALL.iter().any(|(key, _)| *key == p)
}

/// `{name}` placeholders found in a template text.
fn placeholders(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        rest = &rest[open + 1..];
        match rest.find('}') {
            Some(close) => {
                out.push(rest[..close].to_string());
                rest = &rest[close + 1..];
            }
            None => break,
        }
    }
    out
}

fn check_placeholders(subject: &str, body: &str) -> AppResult<()> {
    for ph in placeholders(subject).into_iter().chain(placeholders(body)) {
        if !TEMPLATE_VARS.contains(&ph.as_str()) {
            return Err(AppError::validation(format!(
                "Unknown placeholder '{{{ph}}}' in the template."
            ))
            .with_details(json!({ "field": "body", "placeholder": ph })));
        }
    }
    Ok(())
}

fn user_exists(conn: &Connection, id: i64) -> AppResult<()> {
    conn.query_row("SELECT id FROM users WHERE id = ?1", [id], |r| {
        r.get::<_, i64>(0)
    })
    .optional()?
    .map(|_| ())
    .ok_or_else(AppError::not_found)
}

fn user_json(conn: &Connection, id: i64) -> AppResult<Value> {
    let mut u = query_one_json(
        conn,
        "SELECT u.id, u.username, u.display_name, u.title, u.email, u.is_judge, u.active, u.persona,
                u.deactivated_at, u.must_change_password,
                u.totp_secret IS NOT NULL AS mfa_enrolled,
                (SELECT MAX(s.last_seen_at) FROM sessions s WHERE s.user_id = u.id) AS last_seen_at,
                (SELECT COUNT(*) FROM sessions s WHERE s.user_id = u.id AND s.revoked_at IS NULL
                   AND s.expires_at > ?2) AS active_sessions
         FROM users u WHERE u.id = ?1",
        params![id, crate::time::now_utc()],
    )?;
    let perms = query_json(
        conn,
        "SELECT permission FROM user_permissions WHERE user_id = ?1 ORDER BY permission",
        [id],
    )?
    .into_iter()
    .map(|r| r["permission"].clone())
    .collect::<Vec<_>>();
    u["permissions"] = json!(perms);
    Ok(u)
}

fn user_permissions_of(conn: &Connection, id: i64) -> AppResult<Vec<String>> {
    let mut stmt = conn.prepare("SELECT permission FROM user_permissions WHERE user_id = ?1")?;
    Ok(stmt
        .query_map([id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?)
}

// ------------------------------------------------------------------ permissions & users

async fn permission_list(ctx: Ctx) -> JsonResult {
    ctx.actor.require(perm::ADMIN_USERS)?;
    let items: Vec<Value> = perm::ALL
        .iter()
        .map(|(key, description)| {
            json!({ "key": key, "description": description, "admin_grantable": perm::ADMIN_GRANTABLE.contains(key) })
        })
        .collect();
    Ok(Json(json!(items)))
}

async fn user_list(ctx: Ctx) -> JsonResult {
    ctx.actor.require(perm::ADMIN_USERS)?;
    let v = ctx
        .db
        .read(|c| {
            let ids = query_json(c, "SELECT id FROM users ORDER BY username", [])?
                .into_iter()
                .filter_map(|r| r["id"].as_i64())
                .collect::<Vec<_>>();
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                out.push(user_json(c, id)?);
            }
            Ok(json!(out))
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct NewUser {
    username: String,
    display_name: String,
    title: Option<String>,
    email: Option<String>,
    #[serde(default)]
    permissions: Vec<String>,
}

async fn user_create(
    State(state): State<AppState>,
    ctx: Ctx,
    JsonBody(req): JsonBody<NewUser>,
) -> JsonResult {
    ctx.actor.require(perm::ADMIN_USERS)?;
    let actor = ctx.actor;
    let demo = state.is_demo();
    let v = ctx
        .db
        .write(move |tx| {
            let username = req.username.trim().to_string();
            if !valid_username(&username) {
                return Err(AppError::validation(
                    "Usernames are 3–32 characters: lowercase letters, digits, '.', '_' or '-'.",
                )
                .with_details(json!({ "field": "username" })));
            }
            let display = required(&req.display_name, "Display name")?;
            for p in &req.permissions {
                if !permission_known(p) {
                    return Err(AppError::validation(format!("Unknown permission '{p}'.")));
                }
                if !perm::ADMIN_GRANTABLE.contains(&p.as_str()) {
                    return Err(AppError::forbidden(GRANT_MESSAGE));
                }
            }
            let now = crate::time::now_utc();
            // is_judge is never set here: judicial status is assigned by the court authority.
            let (hash, temp): (String, Option<String>) = if demo {
                // Demo people sign in through the persona switcher, never with a password.
                (format!("!demo-{}", auth::random_token()), None)
            } else {
                let t = temporary_password();
                (auth::hash_password(&t)?, Some(t))
            };
            tx.execute(
                "INSERT INTO users (username, display_name, title, email, password_hash, must_change_password, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![username, display, optional(&req.title), optional(&req.email), hash, (!demo) as i64, now],
            )?;
            let id = tx.last_insert_rowid();
            for p in &req.permissions {
                tx.execute(
                    "INSERT INTO user_permissions (user_id, permission, granted_by, granted_at) VALUES (?1, ?2, ?3, ?4)",
                    params![id, p, actor.user_id, now],
                )?;
            }
            audit::record(
                tx,
                Some(&actor),
                Event::new("user.created", "user", id, format!("User '{username}' created"))
                    .details(json!({ "permissions": req.permissions })),
            )?;
            if demo {
                Ok(json!({
                    "id": id,
                    "temporary_password": Value::Null,
                    "note": "Demo: new people cannot sign in; use the persona switcher."
                }))
            } else {
                Ok(json!({ "id": id, "temporary_password": temp }))
            }
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct UserPatch {
    display_name: Option<String>,
    title: Option<String>,
    email: Option<String>,
}

async fn user_update(
    ctx: Ctx,
    Path(id): Path<i64>,
    JsonBody(req): JsonBody<UserPatch>,
) -> JsonResult {
    ctx.actor.require(perm::ADMIN_USERS)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let before = user_json(tx, id)?;
            let display = match &req.display_name {
                Some(d) => Some(required(d, "Display name")?),
                None => None,
            };
            tx.execute(
                "UPDATE users SET display_name = COALESCE(?2, display_name),
                        title = CASE WHEN ?3 THEN ?4 ELSE title END,
                        email = CASE WHEN ?5 THEN ?6 ELSE email END
                 WHERE id = ?1",
                params![
                    id,
                    display,
                    req.title.is_some(),
                    optional(&req.title),
                    req.email.is_some(),
                    optional(&req.email)
                ],
            )?;
            let after = user_json(tx, id)?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("user.updated", "user", id, "User details changed")
                    .details(json!({ "before": before, "after": after })),
            )?;
            Ok(after)
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct PermissionsReq {
    #[serde(default)]
    permissions: Vec<String>,
}

/// Replace the admin-grantable slice of a user's permissions. Permissions outside
/// ADMIN_GRANTABLE that the user already holds are preserved untouched.
async fn user_permissions(
    ctx: Ctx,
    Path(id): Path<i64>,
    JsonBody(req): JsonBody<PermissionsReq>,
) -> JsonResult {
    ctx.actor.require(perm::ADMIN_USERS)?;
    if ctx.actor.user_id == id {
        return Err(AppError::forbidden(
            "You cannot change your own permissions.",
        ));
    }
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            user_exists(tx, id)?;
            for p in &req.permissions {
                if !permission_known(p) {
                    return Err(AppError::validation(format!("Unknown permission '{p}'.")));
                }
            }
            let existing = user_permissions_of(tx, id)?;
            for p in &req.permissions {
                if !existing.iter().any(|e| e == p) && !perm::ADMIN_GRANTABLE.contains(&p.as_str()) {
                    return Err(AppError::forbidden(GRANT_MESSAGE));
                }
            }
            let mut next: Vec<String> = existing
                .iter()
                .filter(|p| !perm::ADMIN_GRANTABLE.contains(&p.as_str()))
                .cloned()
                .collect();
            for p in &req.permissions {
                if !next.iter().any(|n| n == p) {
                    next.push(p.clone());
                }
            }
            next.sort();
            let now = crate::time::now_utc();
            tx.execute("DELETE FROM user_permissions WHERE user_id = ?1", [id])?;
            for p in &next {
                tx.execute(
                    "INSERT INTO user_permissions (user_id, permission, granted_by, granted_at) VALUES (?1, ?2, ?3, ?4)",
                    params![id, p, actor.user_id, now],
                )?;
            }
            let mut before = existing;
            before.sort();
            audit::record(
                tx,
                Some(&actor),
                Event::new("user.permissions_changed", "user", id, "User permissions changed")
                    .details(json!({ "before": before, "after": next })),
            )?;
            user_json(tx, id)
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct ReasonReq {
    reason: Option<String>,
}

/// Deactivating ends every session and every active assignment, so the person's next
/// request is refused and previously issued file links stop working.
async fn user_deactivate(
    ctx: Ctx,
    Path(id): Path<i64>,
    JsonBody(req): JsonBody<ReasonReq>,
) -> JsonResult {
    ctx.actor.require(perm::ADMIN_USERS)?;
    if ctx.actor.user_id == id {
        return Err(AppError::forbidden(
            "You cannot deactivate your own account.",
        ));
    }
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let before = user_json(tx, id)?;
            if before["active"].as_i64() == Some(0) {
                return Err(AppError::invalid_transition("This account is already deactivated."));
            }
            let why = reason(&req.reason)?;
            let now = crate::time::now_utc();
            tx.execute("UPDATE users SET active = 0, deactivated_at = ?2 WHERE id = ?1", params![id, now])?;
            let revoked = auth::revoke_user_sessions(tx, id)?;
            let ended = tx.execute(
                "UPDATE case_assignments SET end_at = ?2, ended_by = ?3, end_reason = ?4 WHERE user_id = ?1 AND end_at IS NULL",
                params![id, now, actor.user_id, format!("Account deactivated: {why}")],
            )?;
            let after = user_json(tx, id)?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("user.deactivated", "user", id, "User account deactivated").details(
                    json!({ "reason": why, "sessions_revoked": revoked, "assignments_ended": ended, "before": before }),
                ),
            )?;
            Ok(after)
        })
        .await?;
    Ok(Json(v))
}

/// Reactivation restores the account but NOT ended assignments: someone with the
/// assignment permission must re-assign the person to each case explicitly.
async fn user_reactivate(
    ctx: Ctx,
    Path(id): Path<i64>,
    JsonBody(req): JsonBody<ReasonReq>,
) -> JsonResult {
    ctx.actor.require(perm::ADMIN_USERS)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let before = user_json(tx, id)?;
            if before["active"].as_i64() == Some(1) {
                return Err(AppError::invalid_transition(
                    "This account is already active.",
                ));
            }
            let why = reason(&req.reason)?;
            tx.execute(
                "UPDATE users SET active = 1, deactivated_at = NULL WHERE id = ?1",
                [id],
            )?;
            let after = user_json(tx, id)?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("user.reactivated", "user", id, "User account reactivated")
                    .details(json!({ "reason": why, "before": before })),
            )?;
            Ok(after)
        })
        .await?;
    Ok(Json(v))
}

async fn user_revoke_sessions(
    ctx: Ctx,
    Path(id): Path<i64>,
    JsonBody(req): JsonBody<ReasonReq>,
) -> JsonResult {
    ctx.actor.require(perm::ADMIN_USERS)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            user_exists(tx, id)?;
            let why = reason(&req.reason)?;
            let revoked = auth::revoke_user_sessions(tx, id)?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("user.sessions_revoked", "user", id, "All sessions revoked")
                    .details(json!({ "reason": why, "sessions_revoked": revoked })),
            )?;
            Ok(json!({ "ok": true, "sessions_revoked": revoked }))
        })
        .await?;
    Ok(Json(v))
}

/// Production only: a new one-time password is shown once; the second factor (TOTP) is kept.
async fn user_reset_password(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
    JsonBody(req): JsonBody<ReasonReq>,
) -> JsonResult {
    ctx.actor.require(perm::ADMIN_USERS)?;
    if state.is_demo() {
        return Err(AppError::conflict(
            "not_available_in_demo",
            "Demo people have no passwords; use the persona switcher.",
        ));
    }
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            user_exists(tx, id)?;
            let why = reason(&req.reason)?;
            let temp = temporary_password();
            let hash = auth::hash_password(&temp)?;
            tx.execute(
                "UPDATE users SET password_hash = ?2, must_change_password = 1 WHERE id = ?1",
                params![id, hash],
            )?;
            let revoked = auth::revoke_user_sessions(tx, id)?;
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "user.password_reset",
                    "user",
                    id,
                    "Password reset by an administrator",
                )
                .details(json!({ "reason": why, "sessions_revoked": revoked })),
            )?;
            Ok(json!({ "id": id, "temporary_password": temp }))
        })
        .await?;
    Ok(Json(v))
}

/// Clears the second factor: the person must enrol a new sign-in code at next login.
async fn user_reset_mfa(
    ctx: Ctx,
    Path(id): Path<i64>,
    JsonBody(req): JsonBody<ReasonReq>,
) -> JsonResult {
    ctx.actor.require(perm::ADMIN_USERS)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            user_exists(tx, id)?;
            let why = reason(&req.reason)?;
            tx.execute(
                "UPDATE users SET totp_secret = NULL, totp_pending = NULL, totp_last_step = NULL WHERE id = ?1",
                [id],
            )?;
            let revoked = auth::revoke_user_sessions(tx, id)?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("user.mfa_reset", "user", id, "Sign-in code reset by an administrator")
                    .details(json!({ "reason": why, "sessions_revoked": revoked })),
            )?;
            Ok(json!({ "ok": true }))
        })
        .await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ court units

async fn court_unit_list(ctx: Ctx) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let v = ctx
        .db
        .read(|c| {
            Ok(json!(query_json(
                c,
                "SELECT id, code, name, active FROM court_units ORDER BY id",
                []
            )?))
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct CourtUnitReq {
    code: String,
    name: String,
}

async fn court_unit_create(ctx: Ctx, JsonBody(req): JsonBody<CourtUnitReq>) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let code = required(&req.code, "Code")?.to_uppercase();
            if !valid_series(&code) {
                return Err(AppError::validation(
                    "Court unit codes are 2–20 characters: A–Z, digits and '-'.",
                )
                .with_details(json!({ "field": "code" })));
            }
            tx.execute(
                "INSERT INTO court_units (code, name) VALUES (?1, ?2)",
                params![code, required(&req.name, "Name")?],
            )?;
            let id = tx.last_insert_rowid();
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "court_unit.created",
                    "court_unit",
                    id,
                    format!("Court unit '{code}' created"),
                ),
            )?;
            query_one_json(
                tx,
                "SELECT id, code, name, active FROM court_units WHERE id = ?1",
                [id],
            )
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct CourtUnitPatch {
    name: Option<String>,
    active: Option<bool>,
}

async fn court_unit_update(
    ctx: Ctx,
    Path(id): Path<i64>,
    JsonBody(req): JsonBody<CourtUnitPatch>,
) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let before = query_one_json(tx, "SELECT id, code, name, active FROM court_units WHERE id = ?1", [id])?;
            let name = match &req.name {
                Some(n) => Some(required(n, "Name")?),
                None => None,
            };
            tx.execute(
                "UPDATE court_units SET name = COALESCE(?2, name), active = COALESCE(?3, active) WHERE id = ?1",
                params![id, name, req.active.map(|a| a as i64)],
            )?;
            let after = query_one_json(tx, "SELECT id, code, name, active FROM court_units WHERE id = ?1", [id])?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("court_unit.updated", "court_unit", id, "Court unit changed")
                    .details(json!({ "before": before, "after": after })),
            )?;
            Ok(after)
        })
        .await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ registries (number series)

const REGISTRY_SQL: &str =
    "SELECT r.id, r.court_unit_id, cu.name AS court_unit_name, r.series, r.name, r.active,
        (SELECT COUNT(*) FROM cases cs WHERE cs.registry_id = r.id) AS cases
     FROM registries r LEFT JOIN court_units cu ON cu.id = r.court_unit_id";

async fn registry_list(ctx: Ctx) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let v = ctx
        .db
        .read(|c| {
            Ok(json!(query_json(
                c,
                &format!("{REGISTRY_SQL} ORDER BY r.series"),
                []
            )?))
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct RegistryReq {
    court_unit_id: i64,
    series: String,
    name: String,
}

async fn registry_create(ctx: Ctx, JsonBody(req): JsonBody<RegistryReq>) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            tx.query_row(
                "SELECT id FROM court_units WHERE id = ?1",
                [req.court_unit_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .ok_or_else(|| AppError::validation("Unknown court unit."))?;
            let series = required(&req.series, "Series")?;
            if !valid_series(&series) {
                return Err(AppError::validation(
                    "Series are 2–20 characters: A–Z, digits and '-'.",
                )
                .with_details(json!({ "field": "series" })));
            }
            tx.execute(
                "INSERT INTO registries (court_unit_id, series, name) VALUES (?1, ?2, ?3)",
                params![req.court_unit_id, series, required(&req.name, "Name")?],
            )?;
            let id = tx.last_insert_rowid();
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "registry.created",
                    "registry",
                    id,
                    format!("Registry '{series}' created"),
                ),
            )?;
            query_one_json(tx, &format!("{REGISTRY_SQL} WHERE r.id = ?1"), [id])
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct RegistryPatch {
    name: Option<String>,
    active: Option<bool>,
    series: Option<String>,
}

/// The series may change only while no case has been numbered from this registry:
/// issued numbers are never rewritten.
async fn registry_update(
    ctx: Ctx,
    Path(id): Path<i64>,
    JsonBody(req): JsonBody<RegistryPatch>,
) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let before = query_one_json(tx, &format!("{REGISTRY_SQL} WHERE r.id = ?1"), [id])?;
            let name = match &req.name {
                Some(n) => Some(required(n, "Name")?),
                None => None,
            };
            let mut new_series: Option<String> = None;
            if let Some(s) = &req.series {
                let s = required(s, "Series")?;
                if !valid_series(&s) {
                    return Err(AppError::validation("Series are 2–20 characters: A–Z, digits and '-'.").with_details(json!({ "field": "series" })));
                }
                if s != before["series"].as_str().unwrap_or_default() {
                    if before["cases"].as_i64().unwrap_or(0) > 0 {
                        return Err(AppError::conflict(
                            "in_use",
                            "Cases already use this number series. Create a new registry instead.",
                        ));
                    }
                    new_series = Some(s);
                }
            }
            tx.execute(
                "UPDATE registries SET name = COALESCE(?2, name), active = COALESCE(?3, active), series = COALESCE(?4, series) WHERE id = ?1",
                params![id, name, req.active.map(|a| a as i64), new_series],
            )?;
            let after = query_one_json(tx, &format!("{REGISTRY_SQL} WHERE r.id = ?1"), [id])?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("registry.updated", "registry", id, "Registry changed")
                    .details(json!({ "before": before, "after": after })),
            )?;
            Ok(after)
        })
        .await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ rooms

async fn room_list(ctx: Ctx) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let v = ctx
        .db
        .read(|c| {
            Ok(json!(query_json(
                c,
                "SELECT r.id, r.name, r.location, r.court_unit_id, cu.name AS court_unit_name, r.active
                 FROM rooms r LEFT JOIN court_units cu ON cu.id = r.court_unit_id ORDER BY r.name",
                [],
            )?))
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct RoomReq {
    name: String,
    location: Option<String>,
    court_unit_id: Option<i64>,
}

async fn room_create(ctx: Ctx, JsonBody(req): JsonBody<RoomReq>) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            if let Some(unit) = req.court_unit_id {
                tx.query_row("SELECT id FROM court_units WHERE id = ?1", [unit], |r| {
                    r.get::<_, i64>(0)
                })
                .optional()?
                .ok_or_else(|| AppError::validation("Unknown court unit."))?;
            }
            tx.execute(
                "INSERT INTO rooms (name, location, court_unit_id) VALUES (?1, ?2, ?3)",
                params![
                    required(&req.name, "Name")?,
                    optional(&req.location),
                    req.court_unit_id
                ],
            )?;
            let id = tx.last_insert_rowid();
            audit::record(
                tx,
                Some(&actor),
                Event::new("room.created", "room", id, "Room created"),
            )?;
            query_one_json(
                tx,
                "SELECT id, name, location, court_unit_id, active FROM rooms WHERE id = ?1",
                [id],
            )
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct RoomPatch {
    name: Option<String>,
    location: Option<String>,
    active: Option<bool>,
}

/// Rooms are deactivated, never deleted: hearing history keeps referring to them.
async fn room_update(
    ctx: Ctx,
    Path(id): Path<i64>,
    JsonBody(req): JsonBody<RoomPatch>,
) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let before = query_one_json(tx, "SELECT id, name, location, court_unit_id, active FROM rooms WHERE id = ?1", [id])?;
            let name = match &req.name {
                Some(n) => Some(required(n, "Name")?),
                None => None,
            };
            tx.execute(
                "UPDATE rooms SET name = COALESCE(?2, name), location = CASE WHEN ?3 THEN ?4 ELSE location END,
                        active = COALESCE(?5, active)
                 WHERE id = ?1",
                params![id, name, req.location.is_some(), optional(&req.location), req.active.map(|a| a as i64)],
            )?;
            let after = query_one_json(tx, "SELECT id, name, location, court_unit_id, active FROM rooms WHERE id = ?1", [id])?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("room.updated", "room", id, "Room changed").details(json!({ "before": before, "after": after })),
            )?;
            Ok(after)
        })
        .await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ reference lists

#[derive(Deserialize)]
struct RefItemQuery {
    kind: Option<String>,
}

async fn ref_item_list(ctx: Ctx, Query(q): Query<RefItemQuery>) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let v = ctx
        .db
        .read(move |c| {
            Ok(json!(query_json(
                c,
                "SELECT id, kind, code, label, active, sort FROM ref_items WHERE ?1 IS NULL OR kind = ?1 ORDER BY kind, sort, id",
                [optional(&q.kind)],
            )?))
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct RefItemReq {
    kind: String,
    code: String,
    label: String,
    sort: Option<i64>,
}

async fn ref_item_create(ctx: Ctx, JsonBody(req): JsonBody<RefItemReq>) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            if !REF_KINDS.contains(&req.kind.as_str()) {
                return Err(
                    AppError::validation(format!("Unknown list '{}'.", req.kind))
                        .with_details(json!({ "field": "kind" })),
                );
            }
            let code = required(&req.code, "Code")?;
            if !valid_code(&code) {
                return Err(AppError::validation(
                    "Codes are 2–40 characters: lowercase letters, digits and '_'.",
                )
                .with_details(json!({ "field": "code" })));
            }
            tx.execute(
                "INSERT INTO ref_items (kind, code, label, sort) VALUES (?1, ?2, ?3, ?4)",
                params![
                    req.kind,
                    code,
                    required(&req.label, "Label")?,
                    req.sort.unwrap_or(0)
                ],
            )?;
            let id = tx.last_insert_rowid();
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "ref_item.created",
                    "ref_item",
                    id,
                    format!("{} '{code}' added", req.kind),
                ),
            )?;
            query_one_json(
                tx,
                "SELECT id, kind, code, label, active, sort FROM ref_items WHERE id = ?1",
                [id],
            )
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct RefItemPatch {
    code: Option<String>,
    label: Option<String>,
    active: Option<bool>,
    sort: Option<i64>,
}

/// Codes are immutable once issued: records elsewhere may already store them.
async fn ref_item_update(
    ctx: Ctx,
    Path(id): Path<i64>,
    JsonBody(req): JsonBody<RefItemPatch>,
) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let before = query_one_json(tx, "SELECT id, kind, code, label, active, sort FROM ref_items WHERE id = ?1", [id])?;
            if let Some(code) = &req.code
                && code != before["code"].as_str().unwrap_or_default()
            {
                return Err(AppError::validation("The code cannot be changed once issued.").with_details(json!({ "field": "code" })));
            }
            let label = match &req.label {
                Some(l) => Some(required(l, "Label")?),
                None => None,
            };
            tx.execute(
                "UPDATE ref_items SET label = COALESCE(?2, label), active = COALESCE(?3, active), sort = COALESCE(?4, sort) WHERE id = ?1",
                params![id, label, req.active.map(|a| a as i64), req.sort],
            )?;
            let after = query_one_json(tx, "SELECT id, kind, code, label, active, sort FROM ref_items WHERE id = ?1", [id])?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("ref_item.updated", "ref_item", id, "Reference item changed")
                    .details(json!({ "before": before, "after": after })),
            )?;
            Ok(after)
        })
        .await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ message templates

async fn template_list(ctx: Ctx) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let v = ctx
        .db
        .read(|c| {
            Ok(json!(query_json(
                c,
                "SELECT id, code, name, subject, body, active FROM message_templates ORDER BY name",
                [],
            )?))
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct TemplateReq {
    code: String,
    name: String,
    subject: String,
    body: String,
}

async fn template_create(ctx: Ctx, JsonBody(req): JsonBody<TemplateReq>) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let code = required(&req.code, "Code")?;
            if !valid_code(&code) {
                return Err(AppError::validation(
                    "Codes are 2–40 characters: lowercase letters, digits and '_'.",
                )
                .with_details(json!({ "field": "code" })));
            }
            let subject = required(&req.subject, "Subject")?;
            let body = required(&req.body, "Body")?;
            check_placeholders(&subject, &body)?;
            tx.execute(
                "INSERT INTO message_templates (code, name, subject, body) VALUES (?1, ?2, ?3, ?4)",
                params![code, required(&req.name, "Name")?, subject, body],
            )?;
            let id = tx.last_insert_rowid();
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "template.created",
                    "template",
                    id,
                    format!("Template '{code}' created"),
                ),
            )?;
            query_one_json(
                tx,
                "SELECT id, code, name, subject, body, active FROM message_templates WHERE id = ?1",
                [id],
            )
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct TemplatePatch {
    name: Option<String>,
    subject: Option<String>,
    body: Option<String>,
    active: Option<bool>,
}

async fn template_update(
    ctx: Ctx,
    Path(id): Path<i64>,
    JsonBody(req): JsonBody<TemplatePatch>,
) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let before = query_one_json(tx, "SELECT id, code, name, subject, body, active FROM message_templates WHERE id = ?1", [id])?;
            let subject = match &req.subject {
                Some(s) => Some(required(s, "Subject")?),
                None => None,
            };
            let body = match &req.body {
                Some(b) => Some(required(b, "Body")?),
                None => None,
            };
            check_placeholders(
                subject.as_deref().unwrap_or_else(|| before["subject"].as_str().unwrap_or_default()),
                body.as_deref().unwrap_or_else(|| before["body"].as_str().unwrap_or_default()),
            )?;
            let name = match &req.name {
                Some(n) => Some(required(n, "Name")?),
                None => None,
            };
            tx.execute(
                "UPDATE message_templates SET name = COALESCE(?2, name), subject = COALESCE(?3, subject),
                        body = COALESCE(?4, body), active = COALESCE(?5, active) WHERE id = ?1",
                params![id, name, subject, body, req.active.map(|a| a as i64)],
            )?;
            let after = query_one_json(tx, "SELECT id, code, name, subject, body, active FROM message_templates WHERE id = ?1", [id])?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("template.updated", "template", id, "Template changed")
                    .details(json!({ "before": before, "after": after })),
            )?;
            Ok(after)
        })
        .await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ settings

fn settings_json(conn: &Connection, demo: bool) -> AppResult<Value> {
    Ok(json!({
        "court_name": crate::db::setting(conn, "court_name", "Court Registry")?,
        "hearing_buffer_minutes": crate::db::setting(conn, "hearing_buffer_minutes", "0")?.parse::<i64>().unwrap_or(0),
        "intake_reference_prefix": crate::db::setting(conn, "intake_reference_prefix", "IN")?,
        "timezone": "Pacific/Funafuti (UTC+12, fixed)",
        "mode": if demo { "demo" } else { "production" },
    }))
}

async fn settings_get(State(state): State<AppState>, ctx: Ctx) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let demo = state.is_demo();
    let v = ctx.db.read(move |c| settings_json(c, demo)).await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct SettingsReq {
    court_name: Option<String>,
    hearing_buffer_minutes: Option<i64>,
    intake_reference_prefix: Option<String>,
}

async fn settings_put(
    State(state): State<AppState>,
    ctx: Ctx,
    JsonBody(req): JsonBody<SettingsReq>,
) -> JsonResult {
    ctx.actor.require(perm::ADMIN_SETTINGS)?;
    let demo = state.is_demo();
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let before = settings_json(tx, demo)?;
            if let Some(name) = &req.court_name {
                let name = required(name, "Court name")?;
                tx.execute(
                    "INSERT INTO settings (key, value) VALUES ('court_name', ?1)
                            ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![name],
                )?;
            }
            if let Some(buf) = req.hearing_buffer_minutes {
                if !(0..=240).contains(&buf) {
                    return Err(AppError::validation(
                        "The hearing buffer must be between 0 and 240 minutes.",
                    )
                    .with_details(json!({ "field": "hearing_buffer_minutes" })));
                }
                tx.execute(
                    "INSERT INTO settings (key, value) VALUES ('hearing_buffer_minutes', ?1)
                            ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![buf.to_string()],
                )?;
            }
            if let Some(prefix) = &req.intake_reference_prefix {
                let prefix = required(prefix, "Intake reference prefix")?;
                if !valid_prefix(&prefix) {
                    return Err(AppError::validation(
                        "The intake reference prefix is 1–6 capital letters.",
                    )
                    .with_details(json!({ "field": "intake_reference_prefix" })));
                }
                tx.execute(
                    "INSERT INTO settings (key, value) VALUES ('intake_reference_prefix', ?1)
                            ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![prefix],
                )?;
            }
            let after = settings_json(tx, demo)?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("settings.updated", "setting", 0, "Settings changed")
                    .details(json!({ "before": before, "after": after })),
            )?;
            Ok(after)
        })
        .await?;
    Ok(Json(v))
}
