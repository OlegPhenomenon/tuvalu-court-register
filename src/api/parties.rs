//! C04 parties (people and organisations). Internal only — there is no public person search.
//! A party record is never merged with another because the names match.

use super::common::{JsonBody, JsonResult, optional, query_json, query_one_json, required};
use super::intake::NewParty;
use crate::audit::{self, Event};
use crate::auth::{Actor, Ctx};
use crate::error::{AppError, AppResult};
use crate::policy::{self, perm};
use crate::state::AppState;
use axum::extract::{Path, Query};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::params;
use serde::Deserialize;
use serde_json::json;

pub fn routes() -> Router<AppState> {
    Router::new().route("/parties", get(list).post(create)).route("/parties/{id}", get(detail).patch(update))
}

/// Party data is needed by people who register cases or edit participants.
fn require_party_access(actor: &Actor) -> AppResult<()> {
    if [perm::INTAKE_MANAGE, perm::CASE_REGISTER, perm::CASE_EDIT, perm::DISPATCH_MANAGE].iter().any(|p| actor.has(p)) {
        Ok(())
    } else {
        Err(AppError::forbidden("You do not work with party records."))
    }
}

#[derive(Deserialize)]
struct ListQuery {
    q: Option<String>,
}

async fn list(ctx: Ctx, Query(q): Query<ListQuery>) -> JsonResult {
    require_party_access(&ctx.actor)?;
    let v = ctx
        .db
        .read(move |c| {
            let like = optional(&q.q).map(|s| format!("%{s}%"));
            Ok(json!({ "items": query_json(c,
                "SELECT id, kind, name, contact_email, contact_phone, address, island, created_at FROM parties
                 WHERE ?1 IS NULL OR name LIKE ?1 OR contact_email LIKE ?1 ORDER BY name, id LIMIT 50", [like])? }))
        })
        .await?;
    Ok(Json(v))
}

async fn create(ctx: Ctx, JsonBody(req): JsonBody<NewParty>) -> JsonResult {
    require_party_access(&ctx.actor)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            // Same-name records are allowed and reported, never merged.
            let same_name = query_json(tx, "SELECT id, kind, name, contact_email, island FROM parties WHERE name = ?1 COLLATE NOCASE", [req.name.trim()])?;
            let id = super::cases::insert_party(tx, &actor, &req)?;
            audit::record(tx, Some(&actor), Event::new("party.created", "party", id, format!("Party record '{}' created", req.name.trim())))?;
            Ok(json!({ "id": id, "same_name_records": same_name }))
        })
        .await?;
    Ok(Json(v))
}

async fn detail(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    require_party_access(&ctx.actor)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            let party = query_one_json(c, "SELECT * FROM parties WHERE id = ?1", [id])?;
            let sql = format!(
                "SELECT cs.id, cs.number, cs.title, cs.status, cp.role, cp.active FROM case_participations cp
                 JOIN cases cs ON cs.id = cp.case_id WHERE cp.party_id = ?1 AND {} ORDER BY cs.id DESC",
                policy::case_visible_sql(&actor, "cs.id")
            );
            Ok(json!({ "party": party, "cases": query_json(c, &sql, [id])? }))
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct UpdateReq {
    version: i64,
    name: String,
    contact_email: Option<String>,
    contact_phone: Option<String>,
    address: Option<String>,
    island: Option<String>,
    notes: Option<String>,
}

async fn update(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<UpdateReq>) -> JsonResult {
    require_party_access(&ctx.actor)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let before = query_one_json(tx, "SELECT * FROM parties WHERE id = ?1", [id])?;
            if before["version"].as_i64() != Some(req.version) {
                return Err(AppError::version_conflict(before));
            }
            tx.execute(
                "UPDATE parties SET name = ?2, contact_email = ?3, contact_phone = ?4, address = ?5, island = ?6, notes = ?7,
                        version = version + 1 WHERE id = ?1",
                params![id, required(&req.name, "Name")?, optional(&req.contact_email), optional(&req.contact_phone), optional(&req.address), optional(&req.island), optional(&req.notes)],
            )?;
            audit::record(tx, Some(&actor), Event::new("party.updated", "party", id, "Party details changed").details(json!({ "before": before })))?;
            query_one_json(tx, "SELECT * FROM parties WHERE id = ?1", [id])
        })
        .await?;
    Ok(Json(v))
}
