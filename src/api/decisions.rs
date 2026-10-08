//! C11 Decisions: a draft binds to one exact document version, an authorised person finalises it,
//! and a correction is a NEW linked draft that supersedes the earlier finalised text on finalisation
//! — a finalised record is never edited (enforced by the `decisions_finalised_frozen` trigger).

use super::common::{JsonBody, JsonResult, optional, query_json, query_one_json, reason, required};
use crate::audit::{self, Event};
use crate::auth::{Actor, Ctx, IdemKey, idempotent};
use crate::error::{AppError, AppResult};
use crate::policy::{self, perm};
use crate::state::AppState;
use axum::extract::{Path, Query};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/cases/{id}/decisions", get(list_for_case).post(create))
        .route("/decisions", get(list_all))
        .route("/decisions/{id}", get(detail).patch(update))
        .route("/decisions/{id}/finalise", post(finalise))
        .route("/decisions/{id}/withdraw", post(withdraw))
        .route("/decisions/{id}/amend", post(amend))
}

const FINALISED_NOTE: &str =
    "Finalised in this register. This is not a qualified electronic signature.";

const DECISION_SQL: &str = "\
SELECT d.id, d.case_id, c.number AS case_number, d.title, d.decision_date, d.status, d.status_reason,
       d.document_id, doc.title AS document_title, d.document_version_id, v.version_no, v.filename, v.sha256,
       d.hearing_id, au.display_name AS author_name, fu.display_name AS finalised_by_name, d.finalised_at,
       d.amends_decision_id, d.amendment_basis, d.superseded_by_id, d.signed_file_uploaded, d.created_at, d.version
  FROM decisions d
  JOIN cases c ON c.id = d.case_id
  JOIN documents doc ON doc.id = d.document_id
  JOIN document_versions v ON v.id = d.document_version_id
  LEFT JOIN users au ON au.id = d.author_user_id
  LEFT JOIN users fu ON fu.id = d.finalised_by";

/// Display shaping shared by detail and list responses.
fn decorate(mut d: Value) -> Value {
    d["signed_file_uploaded"] = json!(d["signed_file_uploaded"].as_i64() == Some(1));
    if matches!(d["status"].as_str(), Some("finalised") | Some("superseded")) {
        d["note"] = json!(FINALISED_NOTE);
    }
    d
}

/// Full decision JSON, or 404 when the actor may not see the case.
fn decision_json(conn: &Connection, actor: &Actor, id: i64) -> AppResult<Value> {
    let sql = format!(
        "{DECISION_SQL} WHERE d.id = ?1 AND {}",
        policy::case_visible_sql(actor, "d.case_id")
    );
    Ok(decorate(query_one_json(conn, &sql, [id])?))
}

/// Resolve the container before applying the assigned-judge requirement.
fn decision_case_id(conn: &Connection, id: i64) -> AppResult<i64> {
    conn.query_row("SELECT case_id FROM decisions WHERE id = ?1", [id], |r| {
        r.get(0)
    })
    .optional()?
    .ok_or_else(AppError::not_found)
}

/// Visible cases still require a judge assignment for judicial actors.
fn require_judge_scope(conn: &Connection, actor: &Actor, case_id: i64) -> AppResult<()> {
    policy::require_case(conn, actor, case_id)?;
    if actor.is_judge && !policy::is_assigned(conn, actor, case_id, Some("judge"))? {
        return Err(AppError::forbidden(
            "Only the judge assigned to this case can do this.",
        ));
    }
    Ok(())
}

/// A document version usable for a decision: visible to the actor, part of this case, clean.
fn require_case_version(
    conn: &Connection,
    actor: &Actor,
    case_id: i64,
    version_id: i64,
) -> AppResult<i64> {
    let (doc, _) = policy::require_version(conn, actor, version_id)?;
    if doc.case_id != Some(case_id) {
        return Err(AppError::validation(
            "That document does not belong to this case.",
        ));
    }
    let doc_type: String = conn.query_row(
        "SELECT doc_type FROM documents WHERE id = ?1",
        [doc.id],
        |r| r.get(0),
    )?;
    if doc.visibility == "judicial_note" || doc_type == "judicial_note" {
        return Err(AppError::validation(
            "Judicial working notes cannot be used for decisions.",
        ));
    }
    let clean: bool = conn.query_row(
        "SELECT scan_status = 'clean' FROM document_versions WHERE id = ?1",
        [version_id],
        |r| r.get(0),
    )?;
    if !clean {
        return Err(AppError::validation(
            "This file is quarantined and cannot be used.",
        ));
    }
    Ok(doc.id)
}

fn check_hearing_belongs(
    conn: &Connection,
    case_id: i64,
    hearing_id: Option<i64>,
) -> AppResult<()> {
    if let Some(hid) = hearing_id {
        let belongs: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM hearings WHERE id = ?1 AND case_id = ?2)",
            params![hid, case_id],
            |r| r.get(0),
        )?;
        if !belongs {
            return Err(AppError::validation(
                "That hearing does not belong to this case.",
            ));
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ lists & detail

async fn list_for_case(ctx: Ctx, Path(case_id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            policy::require_case(c, &actor, case_id)?;
            let sql = format!(
                "{DECISION_SQL} WHERE d.case_id = ?1 AND {} ORDER BY d.id",
                policy::case_visible_sql(&actor, "d.case_id")
            );
            let items: Vec<Value> = query_json(c, &sql, [case_id])?
                .into_iter()
                .map(decorate)
                .collect();
            Ok(json!({ "items": items }))
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct ListQuery {
    status: Option<String>,
}

/// Cross-case decision list (Decisions screen): only cases the actor may see.
async fn list_all(ctx: Ctx, Query(q): Query<ListQuery>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            let sql = format!(
                "{DECISION_SQL} WHERE {} AND (?1 IS NULL OR d.status = ?1) ORDER BY d.id DESC LIMIT 500",
                policy::case_visible_sql(&actor, "d.case_id")
            );
            let items: Vec<Value> = query_json(c, &sql, params![optional(&q.status)])?.into_iter().map(decorate).collect();
            Ok(json!({ "items": items }))
        })
        .await?;
    Ok(Json(v))
}

async fn detail(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx.db.read(move |c| decision_json(c, &actor, id)).await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ create & edit (draft only)

#[derive(Deserialize, Serialize)]
struct CreateReq {
    title: String,
    decision_date: Option<String>,
    document_version_id: i64,
    hearing_id: Option<i64>,
}

async fn create(
    ctx: Ctx,
    Path(case_id): Path<i64>,
    IdemKey(key): IdemKey,
    JsonBody(req): JsonBody<CreateReq>,
) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            require_judge_scope(tx, &actor, case_id)?;
            policy::require_case_perm(tx, &actor, case_id, perm::DECISION_DRAFT)?;
            idempotent(tx, &actor, &key, "decision.create", &(case_id, &req), || {
                let document_id = require_case_version(tx, &actor, case_id, req.document_version_id)?;
                check_hearing_belongs(tx, case_id, req.hearing_id)?;
                let title = required(&req.title, "Title")?;
                tx.execute(
                    "INSERT INTO decisions (case_id, title, decision_date, status, document_id, document_version_id,
                                            hearing_id, author_user_id, created_at)
                     VALUES (?1, ?2, ?3, 'draft', ?4, ?5, ?6, ?7, ?8)",
                    params![
                        case_id,
                        title,
                        crate::time::parse_opt_date(req.decision_date.as_deref())?,
                        document_id,
                        req.document_version_id,
                        req.hearing_id,
                        actor.user_id,
                        crate::time::now_utc()
                    ],
                )?;
                let id = tx.last_insert_rowid();
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new("decision.drafted", "decision", id, format!("Draft decision “{title}” prepared")).case(Some(case_id)),
                )?;
                decision_json(tx, &actor, id)
            })
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct UpdateReq {
    version: i64,
    title: Option<String>,
    #[serde(default, deserialize_with = "super::common::nullable")]
    decision_date: Option<Option<String>>,
    document_version_id: Option<i64>,
}

async fn update(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<UpdateReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case_id = decision_case_id(tx, id)?;
            require_judge_scope(tx, &actor, case_id)?;
            let current = decision_json(tx, &actor, id)?;
            actor.require(perm::DECISION_DRAFT)?;
            if current["status"].as_str() != Some("draft") {
                return Err(AppError::invalid_transition("Only a draft decision can be edited."));
            }
            if current["version"].as_i64() != Some(req.version) {
                return Err(AppError::version_conflict(current));
            }
            let title = match &req.title {
                Some(t) => Some(required(t, "Title")?),
                None => None,
            };
            let new_doc = match req.document_version_id {
                Some(vid) => Some(require_case_version(tx, &actor, case_id, vid)?),
                None => None,
            };
            tx.execute(
                "UPDATE decisions SET title = COALESCE(?2, title), decision_date = CASE WHEN ?6 THEN ?3 ELSE decision_date END,
                        document_id = COALESCE(?4, document_id), document_version_id = COALESCE(?5, document_version_id),
                        version = version + 1
                 WHERE id = ?1",
                params![
                    id,
                    title,
                    crate::time::parse_opt_date(req.decision_date.as_ref().and_then(|d| d.as_deref()))?,
                    new_doc,
                    req.document_version_id,
                    req.decision_date.is_some()
                ],
            )?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("decision.updated", "decision", id, "Draft decision edited".to_string())
                    .case(Some(case_id))
                    .details(json!({ "before": current })),
            )?;
            decision_json(tx, &actor, id)
        })
        .await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ state changes

#[derive(Deserialize, Serialize)]
struct FinaliseReq {
    version: i64,
    document_version_id: i64,
    decision_date: String,
    signed_file_uploaded: Option<bool>,
}

async fn finalise(
    ctx: Ctx,
    Path(id): Path<i64>,
    IdemKey(key): IdemKey,
    JsonBody(req): JsonBody<FinaliseReq>,
) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case_id = decision_case_id(tx, id)?;
            require_judge_scope(tx, &actor, case_id)?;
            let current = decision_json(tx, &actor, id)?;
            actor.require(perm::DECISION_FINALISE)?;
            idempotent(tx, &actor, &key, "decision.finalise", &(id, &req), || {
                if current["status"].as_str() != Some("draft") {
                    return Err(AppError::invalid_transition("Only a draft decision can be finalised."));
                }
                if current["version"].as_i64() != Some(req.version)
                    || current["document_version_id"].as_i64() != Some(req.document_version_id)
                {
                    return Err(AppError::conflict("stale_review", "The draft changed. Reload and review the current file before finalising.")
                        .with_details(json!({"version": current["version"], "document_version_id": current["document_version_id"]})));
                }
                require_case_version(tx, &actor, case_id, req.document_version_id)?;
                if let Some(old_id) = current["amends_decision_id"].as_i64() {
                    let old = decision_json(tx, &actor, old_id)?;
                    if old["status"] != "finalised" {
                        return Err(AppError::invalid_transition("The original decision has already been superseded."));
                    }
                }
                let date = crate::time::parse_date(&req.decision_date)?;
                let now = crate::time::now_utc();
                tx.execute(
                    "UPDATE decisions SET status = 'finalised', decision_date = ?2,
                            signed_file_uploaded = COALESCE(?3, signed_file_uploaded),
                            finalised_by = ?4, finalised_at = ?5, version = version + 1
                     WHERE id = ?1",
                    params![id, date, req.signed_file_uploaded, actor.user_id, now],
                )?;
                // A finalised amendment supersedes the decision it corrects, in the same transaction.
                if let Some(old_id) = current["amends_decision_id"].as_i64() {
                    tx.execute(
                        "UPDATE decisions SET status = 'superseded', superseded_by_id = ?2, version = version + 1 WHERE id = ?1",
                        params![old_id, id],
                    )?;
                    audit::record(
                        tx,
                        Some(&actor),
                        Event::new("decision.superseded", "decision", old_id, format!("Superseded by decision #{id}"))
                            .case(Some(case_id))
                            .details(json!({ "superseded_by_id": id })),
                    )?;
                }
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new(
                        "decision.finalised",
                        "decision",
                        id,
                        format!("Decision “{}” finalised", current["title"].as_str().unwrap_or_default()),
                    )
                    .case(Some(case_id))
                    .details(json!({ "decision_date": date, "signed_file_uploaded": req.signed_file_uploaded.unwrap_or(false) })),
                )?;
                decision_json(tx, &actor, id)
            })
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize, Serialize)]
struct WithdrawReq {
    reason: Option<String>,
}

async fn withdraw(
    ctx: Ctx,
    Path(id): Path<i64>,
    IdemKey(key): IdemKey,
    JsonBody(req): JsonBody<WithdrawReq>,
) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case_id = decision_case_id(tx, id)?;
            require_judge_scope(tx, &actor, case_id)?;
            let current = decision_json(tx, &actor, id)?;
            actor.require(perm::DECISION_DRAFT)?;
            idempotent(tx, &actor, &key, "decision.withdraw", &(id, &req), || {
                if current["status"].as_str() != Some("draft") {
                    return Err(AppError::invalid_transition("Only a draft decision can be withdrawn."));
                }
                let why = reason(&req.reason)?;
                tx.execute(
                    "UPDATE decisions SET status = 'withdrawn', status_reason = ?2, version = version + 1 WHERE id = ?1",
                    params![id, why],
                )?;
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new("decision.withdrawn", "decision", id, format!("Draft decision “{}” withdrawn", current["title"].as_str().unwrap_or_default()))
                        .case(Some(case_id))
                        .details(json!({ "reason": why })),
                )?;
                decision_json(tx, &actor, id)
            })
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize, Serialize)]
struct AmendReq {
    amendment_basis: String,
    document_version_id: i64,
    title: Option<String>,
    decision_date: Option<String>,
}

/// Correct a finalised decision by drafting a linked amendment; the original stays finalised
/// until the amendment itself is finalised (§4 "исправили ошибку").
async fn amend(ctx: Ctx, Path(id): Path<i64>, IdemKey(key): IdemKey, JsonBody(req): JsonBody<AmendReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case_id = decision_case_id(tx, id)?;
            require_judge_scope(tx, &actor, case_id)?;
            let old = decision_json(tx, &actor, id)?;
            actor.require(perm::DECISION_FINALISE)?;
            idempotent(tx, &actor, &key, "decision.amend", &(id, &req), || {
                if old["status"].as_str() != Some("finalised") {
                    return Err(AppError::invalid_transition("Only a finalised decision can be amended."));
                }
                let basis = required(&req.amendment_basis, "Amendment basis")?;
                let document_id = require_case_version(tx, &actor, case_id, req.document_version_id)?;
                let title = match &req.title {
                    Some(t) => required(t, "Title")?,
                    None => old["title"].as_str().unwrap_or_default().to_string(),
                };
                tx.execute(
                    "INSERT INTO decisions (case_id, title, decision_date, status, document_id, document_version_id,
                                            hearing_id, author_user_id, amends_decision_id, amendment_basis, created_at)
                     VALUES (?1, ?2, ?3, 'draft', ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        case_id,
                        title,
                        crate::time::parse_opt_date(req.decision_date.as_deref())?,
                        document_id,
                        req.document_version_id,
                        old["hearing_id"].as_i64(),
                        actor.user_id,
                        id,
                        basis,
                        crate::time::now_utc()
                    ],
                )?;
                let new_id = tx.last_insert_rowid();
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new("decision.amended", "decision", new_id, format!("Amendment drafted for decision #{id}"))
                        .case(Some(case_id))
                        .details(json!({ "amends_decision_id": id, "amendment_basis": basis })),
                )?;
                decision_json(tx, &actor, new_id)
            })
        })
        .await?;
    Ok(Json(v))
}
