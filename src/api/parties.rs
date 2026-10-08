//! C04 parties (people and organisations). Internal only — there is no public person search.
//! A party record is never merged with another because the names match.

use super::common::{
    JsonBody, JsonResult, optional, query_json, query_one_json, require_ref, required,
};
use super::intake::NewParty;
use crate::audit::{self, Event};
use crate::auth::{Actor, Ctx, IdemKey, idempotent};
use crate::error::{AppError, AppResult};
use crate::policy::{self, perm};
use crate::state::AppState;
use axum::extract::{Path, Query};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/parties", get(list).post(create))
        .route("/parties/{id}", get(detail).patch(update))
        .route(
            "/cases/{id}/participants/{pid}",
            axum::routing::patch(update_participation),
        )
}

/// Party data is needed by people who register cases or edit participants.
fn require_party_access(actor: &Actor) -> AppResult<()> {
    if [
        perm::INTAKE_MANAGE,
        perm::CASE_REGISTER,
        perm::CASE_EDIT,
        perm::DISPATCH_MANAGE,
    ]
    .iter()
    .any(|p| actor.has(p))
    {
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
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            let like = optional(&q.q).map(|s| format!("%{s}%"));
            Ok(json!({ "items": query_json(c,
                &format!("SELECT id, kind, name, contact_email, contact_phone, address, island, created_at, version FROM parties p
                 WHERE {} AND (?1 IS NULL OR name LIKE ?1 OR contact_email LIKE ?1) ORDER BY name, id LIMIT 50", policy::party_visible_sql(&actor,"p.id")), [like])? }))
        })
        .await?;
    Ok(Json(v))
}

async fn create(ctx: Ctx, IdemKey(key): IdemKey, JsonBody(req): JsonBody<NewParty>) -> JsonResult {
    require_party_access(&ctx.actor)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            if let Some(key)=&key {
                let stored: Option<String>=tx.query_row("SELECT result_json FROM operation_keys WHERE user_id=?1 AND key=?2 AND operation='party.create'",params![actor.user_id,key],|r|r.get(0)).optional()?;
                if let Some(stored)=stored {
                    let result:serde_json::Value=serde_json::from_str(&stored)?;
                    policy::require_party(tx,&actor,result["id"].as_i64().unwrap_or_default())?;
                    if let Some(records)=result["same_name_records"].as_array() { for party in records { policy::require_party(tx,&actor,party["id"].as_i64().unwrap_or_default())?; } }
                }
            }
            idempotent(tx, &actor, &key, "party.create", &req, || {
            // Same-name records are allowed and reported, never merged.
            let same_name = query_json(tx, &format!("SELECT id, kind, name, contact_email, island FROM parties p WHERE name = ?1 COLLATE NOCASE AND {}", policy::party_visible_sql(&actor,"p.id")), [req.name.trim()])?;
            let id = super::cases::insert_party(tx, &actor, &req)?;
            audit::record(tx, Some(&actor), Event::new("party.created", "party", id, format!("Party record '{}' created", req.name.trim())))?;
            Ok(json!({ "id": id, "same_name_records": same_name }))
            })
        })
        .await?;
    Ok(Json(v))
}

async fn detail(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            policy::require_party(c, &actor, id)?;
            let party = query_one_json(c, "SELECT * FROM parties WHERE id = ?1", [id])?;
            let sql = format!(
                "SELECT cs.id, cs.number, cs.title, cs.status, cp.role, cp.active FROM case_participations cp
                 JOIN cases cs ON cs.id = cp.case_id WHERE cp.party_id = ?1 AND {} ORDER BY cs.id DESC",
                policy::case_visible_sql(&actor, "cs.id")
            );
            Ok(json!({ "party": party, "editable": policy::can_edit_party(c,&actor,id)?, "cases": query_json(c, &sql, [id])? }))
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
    #[serde(default, deserialize_with = "super::common::nullable")]
    notes: Option<Option<String>>,
}

async fn update(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<UpdateReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            policy::require_party_edit(tx,&actor,id)?;
            let before = query_one_json(tx, "SELECT * FROM parties WHERE id = ?1", [id])?;
            if before["version"].as_i64() != Some(req.version) {
                return Err(AppError::version_conflict(before));
            }
            tx.execute(
                "UPDATE parties SET name = ?2, contact_email = ?3, contact_phone = ?4, address = ?5, island = ?6, notes = CASE WHEN ?8 THEN ?7 ELSE notes END,
                        version = version + 1 WHERE id = ?1",
                params![id, required(&req.name, "Name")?, optional(&req.contact_email), optional(&req.contact_phone), optional(&req.address), optional(&req.island), optional(&req.notes.clone().flatten()), req.notes.is_some()],
            )?;
            let after = query_one_json(tx, "SELECT * FROM parties WHERE id = ?1", [id])?;
            let fields: Vec<_> = ["name","contact_email","contact_phone","address","island","notes"].into_iter().filter(|f| before[*f]!=after[*f]).map(|f|json!({"from_field":f,"to_field":f})).collect();
            audit::record(tx, Some(&actor), Event::new("party.updated", "party", id, "Party contact fields changed").details(json!({ "changes": fields })))?;
            Ok(after)
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize, Serialize)]
struct ParticipationUpdate {
    version: i64,
    role: String,
    representative_party_id: Option<i64>,
    representation_basis: Option<String>,
    service_contact: Option<String>,
}

async fn update_participation(
    ctx: Ctx,
    Path((id, pid)): Path<(i64, i64)>,
    IdemKey(key): IdemKey,
    JsonBody(req): JsonBody<ParticipationUpdate>,
) -> JsonResult {
    let actor = ctx.actor;
    let v=ctx.db.write(move |tx| {
        policy::require_case_perm(tx,&actor,id,perm::CASE_EDIT)?;
        idempotent(tx,&actor,&key,"participant.update",&json!({"case_id":id,"participation_id":pid,"body":req}),|| {
        let before=query_one_json(tx,"SELECT * FROM case_participations WHERE id=?1 AND case_id=?2 AND active=1",params![pid,id])?;
        if before["version"].as_i64()!=Some(req.version) { return Err(AppError::version_conflict(before)); }
        require_ref(tx,"participant_role",&req.role)?;
        if let Some(rep)=req.representative_party_id {
            policy::require_party(tx,&actor,rep)?;
            if optional(&req.representation_basis).is_none() { return Err(AppError::validation("State the basis of representation.")); }
        }
        tx.execute("UPDATE case_participations SET role=?3,representative_party_id=?4,representation_basis=?5,service_contact=?6,version=version+1 WHERE id=?1 AND case_id=?2",
            params![pid,id,req.role,req.representative_party_id,if req.representative_party_id.is_some() {optional(&req.representation_basis)} else {None},optional(&req.service_contact)])?;
        let after=query_one_json(tx,"SELECT * FROM case_participations WHERE id=?1",[pid])?;
        let fields:Vec<_>=["role","representative_party_id","representation_basis","service_contact"].into_iter().filter(|f|before[*f]!=after[*f]).map(|f|json!({"from_field":f,"to_field":f})).collect();
        audit::record(tx,Some(&actor),Event::new("case.participant_updated","case",id,"Participant role, representation or service contact changed").case(Some(id)).details(json!({"participation_id":pid,"changes":fields})))?;
        Ok(after)
        })
    }).await?;
    Ok(Json(v))
}
