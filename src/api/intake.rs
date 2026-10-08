//! C02 Intake and C03 registration. An intake is NOT a case: it is received, possibly supplemented,
//! checked, and then registered (new case number) or linked to an existing case. Intakes are never deleted.

use super::common::{JsonBody, JsonResult, optional, query_json, query_one_json, reason, render_template, require_ref, required};
use crate::audit::{self, Event};
use crate::auth::{Actor, Ctx, IdemKey, idempotent};
use crate::error::{AppError, AppResult};
use crate::policy::{self, perm};
use crate::state::AppState;
use axum::extract::{Path, Query};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{Connection, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/intakes", get(list).post(create))
        .route("/intakes/{id}", get(detail).patch(update))
        .route("/intakes/{id}/supplement", post(supplement))
        .route("/intakes/{id}/request-info", post(request_info))
        .route("/intakes/{id}/mark-ready", post(mark_ready))
        .route("/intakes/{id}/mark-duplicate", post(mark_duplicate))
        .route("/intakes/{id}/return", post(return_intake))
        .route("/intakes/{id}/link", post(link))
        .route("/intakes/{id}/register", post(register))
}

const OPEN_STATES: &[&str] = &["received", "needs_information", "ready_for_registration"];

#[derive(Deserialize)]
struct ListQuery {
    status: Option<String>,
    q: Option<String>,
}

async fn list(ctx: Ctx, Query(q): Query<ListQuery>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            let sql = format!(
                "SELECT i.id, i.reference, i.status, i.sender_name, i.channel, i.origin_island, i.received_date,
                        i.document_date, i.entered_at, i.description, i.is_paper_original, i.parent_intake_id,
                        i.duplicate_of_intake_id, i.case_id, cs.number AS case_number,
                        (SELECT COUNT(*) FROM documents d WHERE d.intake_id = i.id) AS document_count
                 FROM intakes i LEFT JOIN cases cs ON cs.id = i.case_id
                 WHERE {} AND i.parent_intake_id IS NULL AND (?1 IS NULL OR i.status = ?1)
                   AND (?2 IS NULL OR i.reference LIKE ?2 OR i.sender_name LIKE ?2 OR i.description LIKE ?2)
                 ORDER BY i.id DESC LIMIT 500",
                policy::intake_visible_sql(&actor, "i")
            );
            let like = optional(&q.q).map(|s| format!("%{s}%"));
            Ok(json!({ "items": query_json(c, &sql, params![optional(&q.status), like])? }))
        })
        .await?;
    Ok(Json(v))
}

/// Load an intake visible to the actor, else 404.
pub fn require_intake(conn: &Connection, actor: &Actor, id: i64) -> AppResult<Value> {
    let sql = format!("SELECT i.* FROM intakes i WHERE i.id = ?1 AND {}", policy::intake_visible_sql(actor, "i"));
    query_one_json(conn, &sql, [id])
}

fn status_of(v: &Value) -> &str {
    v["status"].as_str().unwrap_or_default()
}

#[derive(Deserialize, Serialize)]
struct IntakeInput {
    sender_name: String,
    sender_party_id: Option<i64>,
    channel: String,
    origin_island: Option<String>,
    document_date: Option<String>,
    received_date: String,
    description: String,
    #[serde(default)]
    is_paper_original: bool,
    paper_location: Option<String>,
}

fn next_reference(tx: &Transaction, received_date: &str) -> AppResult<String> {
    let prefix = crate::db::setting(tx, "intake_reference_prefix", "IN")?;
    let year = crate::time::year_of(received_date)?;
    let stem = format!("{prefix}-{year}-");
    // Parse the entire numeric suffix, rather than comparing padded references as text.
    // The caller holds BEGIN IMMEDIATE, so concurrent allocations cannot pick the same number.
    let mut stmt = tx.prepare("SELECT reference FROM intakes WHERE substr(reference, 1, length(?1)) = ?1")?;
    let refs = stmt.query_map(params![stem], |r| r.get::<_, String>(0))?;
    let mut highest = 0i64;
    for reference in refs {
        let reference = reference?;
        let suffix = &reference[stem.len()..];
        if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
            highest = highest.max(suffix.parse::<i64>().map_err(|_| AppError::validation("Intake sequence is too large."))?);
        }
    }
    let n = highest.checked_add(1).ok_or_else(|| AppError::validation("Intake sequence is exhausted."))?;
    Ok(format!("{prefix}-{year}-{n:04}"))
}

fn insert_intake(tx: &Transaction, actor: &Actor, input: &IntakeInput, parent: Option<i64>) -> AppResult<(i64, String)> {
    let sender = required(&input.sender_name, "Sender")?;
    let description = required(&input.description, "Description")?;
    require_ref(tx, "intake_channel", &input.channel)?;
    if let Some(island) = optional(&input.origin_island) {
        require_ref(tx, "origin_island", &island)?;
    }
    let received = crate::time::parse_date(&input.received_date)?;
    let doc_date = crate::time::parse_opt_date(input.document_date.as_deref())?;
    if received > crate::time::today_local() {
        return Err(AppError::validation("The received date cannot be in the future."));
    }
    if input.is_paper_original && optional(&input.paper_location).is_none() {
        return Err(AppError::validation("Say where the paper original is kept.").with_details(json!({ "field": "paper_location" })));
    }
    if let Some(pid) = input.sender_party_id {
        policy::require_party(tx, actor, pid)?;
    }
    let reference = next_reference(tx, &received)?;
    let now = crate::time::now_utc();
    tx.execute(
        "INSERT INTO intakes (reference, status, sender_party_id, sender_name, channel, origin_island, document_date,
                              received_date, entered_at, description, is_paper_original, paper_location,
                              parent_intake_id, created_by, updated_at)
         VALUES (?1, 'received', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?8)",
        params![
            reference,
            input.sender_party_id,
            sender,
            input.channel,
            optional(&input.origin_island),
            doc_date,
            received,
            now,
            description,
            input.is_paper_original,
            optional(&input.paper_location),
            parent,
            actor.user_id
        ],
    )?;
    Ok((tx.last_insert_rowid(), reference))
}

async fn create(ctx: Ctx, IdemKey(key): IdemKey, JsonBody(input): JsonBody<IntakeInput>) -> JsonResult {
    ctx.actor.require(perm::INTAKE_MANAGE)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            idempotent(tx, &actor, &key, "intake.create", &input, || {
                let (id, reference) = insert_intake(tx, &actor, &input, None)?;
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new("intake.received", "intake", id, format!("Received {reference} from {}", input.sender_name.trim())),
                )?;
                Ok(json!({ "id": id, "reference": reference }))
            })
        })
        .await?;
    Ok(Json(v))
}

/// Duplicate warnings: visible documents elsewhere with the same checksum (never merges anything).
fn checksum_matches(conn: &Connection, actor: &Actor, intake_id: i64) -> AppResult<Vec<Value>> {
    let sql = format!(
        "SELECT DISTINCT v.sha256, d.id AS document_id, d.title, d.intake_id, i2.reference AS intake_reference,
                d.case_id, cs.number AS case_number
         FROM document_versions mine
         JOIN documents md ON md.id = mine.document_id AND md.intake_id = ?1
         JOIN document_versions v ON v.sha256 = mine.sha256 AND v.document_id <> mine.document_id
         JOIN documents d ON d.id = v.document_id
         LEFT JOIN intakes i2 ON i2.id = d.intake_id
         LEFT JOIN cases cs ON cs.id = d.case_id
         WHERE {}",
        policy::document_visible_sql(actor, "d")
    );
    query_json(conn, &sql, [intake_id])
}

async fn detail(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            let mut intake = require_intake(c, &actor, id)?;
            let docs_sql = format!(
                "SELECT d.id, d.title, d.doc_type, d.visibility, d.document_date, d.received_date, d.is_paper_original,
                        d.original_location, d.created_at,
                        (SELECT COUNT(*) FROM document_versions v WHERE v.document_id = d.id) AS version_count
                 FROM documents d WHERE d.intake_id = ?1 AND {} ORDER BY d.id",
                policy::document_visible_sql(&actor, "d")
            );
            let children_sql = format!(
                "SELECT i.id, i.reference, i.status, i.received_date, i.description FROM intakes i
                 WHERE (i.parent_intake_id = ?1 OR i.duplicate_of_intake_id = ?1) AND {} ORDER BY i.id",
                policy::intake_visible_sql(&actor, "i")
            );
            let case = match intake["case_id"].as_i64() {
                Some(cid) if policy::can_view_case(c, &actor, cid)? => {
                    query_one_json(c, "SELECT id, number, title, status FROM cases WHERE id = ?1", [cid])?
                }
                _ => Value::Null,
            };
            let created_by = super::common::user_name(c, intake["created_by"].as_i64())?;
            intake["created_by_name"] = json!(created_by);
            let actions = allowed_actions(&actor, &intake);
            let next_actions: Vec<Value> = query_json(c,
                "SELECT id, recipient_name FROM dispatches WHERE intake_id = ?1 AND kind = 'information_request' AND status = 'draft' ORDER BY id", [id])?
                .iter().map(|d| json!({
                    "code": "review_dispatch",
                    "message": format!("Review and send the information request to {}.", d["recipient_name"].as_str().unwrap_or_default()),
                    "link": format!("/dispatch?dispatch={}", d["id"]),
                })).collect();
            Ok(json!({
                "intake": intake,
                "case": case,
                "documents": query_json(c, &docs_sql, [id])?,
                "messages": query_json(c,
                    "SELECT m.id, m.direction, m.body, m.dispatch_id, m.created_at, u.display_name AS created_by_name
                     FROM intake_messages m LEFT JOIN users u ON u.id = m.created_by WHERE m.intake_id = ?1 ORDER BY m.id", [id])?,
                "related_intakes": query_json(c, &children_sql, [id])?,
                "dispatches": query_json(c,
                    "SELECT id, kind, recipient_name, method, subject, status, prepared_at FROM dispatches WHERE intake_id = ?1 ORDER BY id", [id])?,
                "checksum_matches": checksum_matches(c, &actor, id)?,
                "allowed_actions": actions,
                "next_actions": next_actions,
            }))
        })
        .await?;
    Ok(Json(v))
}

fn allowed_actions(actor: &Actor, intake: &Value) -> Vec<&'static str> {
    let mut out = Vec::new();
    let status = status_of(intake);
    if !actor.has(perm::INTAKE_MANAGE) || !OPEN_STATES.contains(&status) || !intake["parent_intake_id"].is_null() {
        return out;
    }
    out.extend(["edit", "supplement", "request_info", "mark_duplicate", "return", "link"]);
    if status != "ready_for_registration" {
        out.push("mark_ready");
    }
    if status == "ready_for_registration" && actor.has(perm::CASE_REGISTER) {
        out.push("register");
    }
    out
}

/// Load an intake for a state change: visible, permitted, and in one of `from`.
fn load_for_change(tx: &Transaction, actor: &Actor, id: i64, from: &[&str]) -> AppResult<Value> {
    let intake = require_intake(tx, actor, id)?;
    actor.require(perm::INTAKE_MANAGE)?;
    if !intake["parent_intake_id"].is_null() {
        return Err(AppError::invalid_transition("This is a supplement. Act on the original filing instead."));
    }
    if !from.contains(&status_of(&intake)) {
        return Err(AppError::invalid_transition(format!(
            "This filing is '{}', so this action is not available.",
            status_of(&intake)
        )));
    }
    Ok(intake)
}

fn set_status(tx: &Transaction, id: i64, status: &str, reason: Option<&str>) -> AppResult<()> {
    tx.execute(
        "UPDATE intakes SET status = ?2, status_reason = COALESCE(?3, status_reason), updated_at = ?4, version = version + 1 WHERE id = ?1",
        params![id, status, reason, crate::time::now_utc()],
    )?;
    Ok(())
}

fn note(tx: &Transaction, actor: &Actor, intake_id: i64, direction: &str, body: &str, dispatch_id: Option<i64>) -> AppResult<()> {
    tx.execute(
        "INSERT INTO intake_messages (intake_id, direction, body, dispatch_id, created_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![intake_id, direction, body, dispatch_id, actor.user_id, crate::time::now_utc()],
    )?;
    Ok(())
}

#[derive(Deserialize)]
struct UpdateReq {
    version: i64,
    #[serde(flatten)]
    input: IntakeInput,
}

async fn update(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<UpdateReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let current = load_for_change(tx, &actor, id, OPEN_STATES)?;
            if current["version"].as_i64() != Some(req.version) {
                return Err(AppError::version_conflict(current));
            }
            let i = &req.input;
            if let Some(pid)=i.sender_party_id { policy::require_party(tx,&actor,pid)?; }
            require_ref(tx, "intake_channel", &i.channel)?;
            if let Some(island) = optional(&i.origin_island) {
                require_ref(tx, "origin_island", &island)?;
            }
            tx.execute(
                "UPDATE intakes SET sender_name = ?2, sender_party_id = ?3, channel = ?4, origin_island = ?5, document_date = ?6,
                        received_date = ?7, description = ?8, is_paper_original = ?9, paper_location = ?10,
                        updated_at = ?11, version = version + 1
                 WHERE id = ?1",
                params![
                    id,
                    required(&i.sender_name, "Sender")?,
                    i.sender_party_id,
                    i.channel,
                    optional(&i.origin_island),
                    crate::time::parse_opt_date(i.document_date.as_deref())?,
                    crate::time::parse_date(&i.received_date)?,
                    required(&i.description, "Description")?,
                    i.is_paper_original,
                    optional(&i.paper_location),
                    crate::time::now_utc()
                ],
            )?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("intake.updated", "intake", id, "Intake details corrected")
                    .details(json!({ "before": current })),
            )?;
            require_intake(tx, &actor, id)
        })
        .await?;
    Ok(Json(v))
}

/// A supplement is a new intake linked to the original; the original returns to `received`.
async fn supplement(ctx: Ctx, Path(id): Path<i64>, IdemKey(key): IdemKey, JsonBody(input): JsonBody<IntakeInput>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            require_intake(tx, &actor, id)?;
            actor.require(perm::INTAKE_MANAGE)?;
            idempotent(tx, &actor, &key, "intake.supplement", &(id, &input), || {
                let parent = load_for_change(tx, &actor, id, OPEN_STATES)?;
                let (child, reference) = insert_intake(tx, &actor, &input, Some(id))?;
                // The supplement is part of the original package: it stays 'received', is listed under
                // its parent (not in the main list) and follows the parent when it is registered or linked.
                if status_of(&parent) == "needs_information" {
                    set_status(tx, id, "received", Some("Supplementary material received"))?;
                }
                note(
                    tx,
                    &actor,
                    id,
                    "incoming",
                    &format!("Supplement {reference} received: {}", input.description.trim()),
                    None,
                )?;
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new(
                        "intake.supplemented",
                        "intake",
                        id,
                        format!("Supplement {reference} added to {}", parent["reference"].as_str().unwrap_or_default()),
                    )
                    .details(json!({ "supplement_intake_id": child })),
                )?;
                Ok(json!({ "id": child, "reference": reference }))
            })
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize, Serialize)]
struct RequestInfoReq {
    missing_items: String,
    method: Option<String>,
    address: Option<String>,
}

async fn request_info(ctx: Ctx, Path(id): Path<i64>, IdemKey(key): IdemKey, JsonBody(req): JsonBody<RequestInfoReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            require_intake(tx, &actor, id)?;
            actor.require(perm::INTAKE_MANAGE)?;
            idempotent(tx, &actor, &key, "intake.request_info", &(id, &req), || {
                let intake = load_for_change(tx, &actor, id, &["received", "ready_for_registration", "needs_information"])?;
                let missing = required(&req.missing_items, "Missing items")?;
                let method = optional(&req.method).unwrap_or_else(|| "post".into());
                require_ref(tx, "dispatch_method", &method)?;
                let reference = intake["reference"].as_str().unwrap_or_default().to_string();
                let recipient = intake["sender_name"].as_str().unwrap_or_default().to_string();
                let (subject, body) = render_template(
                    tx,
                    "information_request",
                    &[
                        ("recipient", recipient.clone()),
                        ("intake_reference", reference.clone()),
                        ("received_date", intake["received_date"].as_str().unwrap_or_default().to_string()),
                        ("missing_items", missing.clone()),
                    ],
                )?;
                // Prepared message only: a person reviews and sends it from Dispatch.
                tx.execute(
                    "INSERT INTO dispatches (intake_id, kind, template_code, recipient_party_id, recipient_name, method, address,
                                         subject, body, purpose, status, prepared_by, prepared_at)
                 VALUES (?1, 'information_request', 'information_request', ?2, ?3, ?4, ?5, ?6, ?7, 'Request for missing information', 'draft', ?8, ?9)",
                    params![
                        id,
                        intake["sender_party_id"].as_i64(),
                        recipient,
                        method,
                        optional(&req.address),
                        subject,
                        body,
                        actor.user_id,
                        crate::time::now_utc()
                    ],
                )?;
                let dispatch_id = tx.last_insert_rowid();
                tx.execute("UPDATE intakes SET missing_items = ?2 WHERE id = ?1", params![id, missing])?;
                set_status(tx, id, "needs_information", None)?;
                note(tx, &actor, id, "outgoing", &format!("Requested: {missing}"), Some(dispatch_id))?;
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new(
                        "intake.information_requested",
                        "intake",
                        id,
                        format!("Requested missing information for {reference}"),
                    )
                    .details(json!({ "missing_items": missing, "dispatch_id": dispatch_id })),
                )?;
                Ok(json!({ "ok": true, "dispatch_id": dispatch_id }))
            })
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize, Serialize)]
struct NoteReq {
    note: Option<String>,
}

async fn mark_ready(ctx: Ctx, Path(id): Path<i64>, IdemKey(key): IdemKey, JsonBody(req): JsonBody<NoteReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            require_intake(tx, &actor, id)?;
            actor.require(perm::INTAKE_MANAGE)?;
            idempotent(tx, &actor, &key, "intake.mark_ready", &(id, &req), || {
                let intake = load_for_change(tx, &actor, id, &["received", "needs_information"])?;
                let n = optional(&req.note);
                set_status(tx, id, "ready_for_registration", n.as_deref())?;
                if let Some(n) = &n {
                    note(tx, &actor, id, "note", n, None)?;
                }
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new(
                        "intake.ready",
                        "intake",
                        id,
                        format!("{} checked and ready for registration", intake["reference"].as_str().unwrap_or_default()),
                    ),
                )?;
                require_intake(tx, &actor, id)
            })
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize, Serialize)]
struct DuplicateReq {
    duplicate_of_intake_id: i64,
    reason: Option<String>,
}

async fn mark_duplicate(ctx: Ctx, Path(id): Path<i64>, IdemKey(key): IdemKey, JsonBody(req): JsonBody<DuplicateReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            require_intake(tx, &actor, id)?;
            actor.require(perm::INTAKE_MANAGE)?;
            idempotent(tx, &actor, &key, "intake.mark_duplicate", &(id, &req), || {
                let intake = load_for_change(tx, &actor, id, OPEN_STATES)?;
                let why = reason(&req.reason)?;
                if req.duplicate_of_intake_id == id {
                    return Err(AppError::validation("A filing cannot duplicate itself."));
                }
                let original = require_intake(tx, &actor, req.duplicate_of_intake_id)?;
                tx.execute(
                    "UPDATE intakes SET duplicate_of_intake_id = ?2 WHERE id = ?1",
                    params![id, req.duplicate_of_intake_id],
                )?;
                set_status(tx, id, "duplicate", Some(&why))?;
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new(
                        "intake.duplicate",
                        "intake",
                        id,
                        format!(
                            "{} marked as duplicate of {}",
                            intake["reference"].as_str().unwrap_or_default(),
                            original["reference"].as_str().unwrap_or_default()
                        ),
                    )
                    .details(json!({ "reason": why, "original_intake_id": req.duplicate_of_intake_id })),
                )?;
                require_intake(tx, &actor, id)
            })
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize, Serialize)]
struct ReasonReq {
    reason: Option<String>,
}

async fn return_intake(ctx: Ctx, Path(id): Path<i64>, IdemKey(key): IdemKey, JsonBody(req): JsonBody<ReasonReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            require_intake(tx, &actor, id)?;
            actor.require(perm::INTAKE_MANAGE)?;
            idempotent(tx, &actor, &key, "intake.return", &(id, &req), || {
                let intake = load_for_change(tx, &actor, id, OPEN_STATES)?;
                let why = reason(&req.reason)?;
                set_status(tx, id, "returned_or_redirected", Some(&why))?;
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new(
                        "intake.returned",
                        "intake",
                        id,
                        format!("{} returned or redirected", intake["reference"].as_str().unwrap_or_default()),
                    )
                    .details(json!({ "reason": why })),
                )?;
                require_intake(tx, &actor, id)
            })
        })
        .await?;
    Ok(Json(v))
}

/// Attach the intake's documents to a case (they keep their intake link: one history, no copies).
fn attach_documents(tx: &Transaction, intake_id: i64, case_id: i64) -> AppResult<usize> {
    Ok(tx.execute(
        "UPDATE documents SET case_id = ?2 WHERE case_id IS NULL AND (intake_id = ?1 OR intake_id IN (SELECT id FROM intakes WHERE parent_intake_id = ?1))",
        params![intake_id, case_id],
    )?)
}

/// Once a filing is linked, unsent requests are obsolete. The worker and this change share
/// the same immediate transaction, so queued requests cannot be delivered after cancellation.
fn cancel_information_requests(tx: &Transaction, actor: &Actor, intake_id: i64, case_id: i64) -> AppResult<Vec<Value>> {
    let mut requests = query_json(tx,
        "SELECT id, kind, recipient_name, status FROM dispatches WHERE kind = 'information_request'
         AND status IN ('draft','queued') AND (intake_id = ?1 OR intake_id IN (SELECT id FROM intakes WHERE parent_intake_id = ?1)) ORDER BY id",
        [intake_id])?;
    let why = "Not sent: the filing was registered";
    for request in &mut requests {
        let id = request["id"].as_i64().unwrap_or_default();
        tx.execute("UPDATE dispatches SET status = 'cancelled', status_reason = ?2, version = version + 1 WHERE id = ?1", params![id, why])?;
        audit::record(tx, Some(actor), Event::new("dispatch.cancelled", "dispatch", id,
            format!("Information request to {} cancelled", request["recipient_name"].as_str().unwrap_or_default()))
            .case(Some(case_id)).details(json!({"reason": why, "intake_id": intake_id})))?;
        request["status"] = json!("cancelled");
        request["status_reason"] = json!(why);
    }
    tx.execute("UPDATE intakes SET missing_items = NULL WHERE id = ?1 OR parent_intake_id = ?1", [intake_id])?;
    Ok(requests)
}

#[derive(Deserialize, Serialize)]
struct LinkReq {
    case_id: i64,
    note: Option<String>,
}

async fn link(ctx: Ctx, Path(id): Path<i64>, IdemKey(key): IdemKey, JsonBody(req): JsonBody<LinkReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let intake = require_intake(tx, &actor, id)?;
            actor.require(perm::INTAKE_MANAGE)?;
            let case = policy::require_case(tx, &actor, req.case_id)?;
            idempotent(tx, &actor, &key, "intake.link", &(id, &req), || {
                load_for_change(tx, &actor, id, OPEN_STATES)?;
                tx.execute("UPDATE intakes SET case_id = ?2 WHERE id = ?1", params![id, case.id])?;
                set_status(tx, id, "linked_to_case", optional(&req.note).as_deref())?;
                tx.execute(
                    "UPDATE intakes SET case_id = ?2, status = 'linked_to_case' WHERE parent_intake_id = ?1",
                    params![id, case.id],
                )?;
                let docs = attach_documents(tx, id, case.id)?;
                let cancelled_requests = cancel_information_requests(tx, &actor, id, case.id)?;
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new(
                        "intake.linked",
                        "intake",
                        id,
                        format!("{} linked to case {}", intake["reference"].as_str().unwrap_or_default(), case.number),
                    )
                    .case(Some(case.id))
                    .details(json!({ "documents_attached": docs })),
                )?;
                Ok(json!({ "case_id": case.id, "number": case.number, "cancelled_requests": cancelled_requests }))
            })
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize, Serialize)]
pub struct NewParty {
    pub kind: String,
    pub name: String,
    pub contact_email: Option<String>,
    pub contact_phone: Option<String>,
    pub address: Option<String>,
    pub island: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct ParticipantInput {
    pub party_id: Option<i64>,
    pub new_party: Option<NewParty>,
    pub role: String,
    pub representative_party_id: Option<i64>,
    pub representation_basis: Option<String>,
    pub service_contact: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct RegisterReq {
    registry_id: i64,
    category: String,
    title: String,
    summary: Option<String>,
    registered_date: Option<String>,
    #[serde(default)]
    restricted: bool,
    responsible_user_id: Option<i64>,
    #[serde(default)]
    participants: Vec<ParticipantInput>,
    /// Register as a new case related to an earlier one (follow-up application).
    related_case_id: Option<i64>,
}

async fn register(ctx: Ctx, Path(id): Path<i64>, IdemKey(key): IdemKey, JsonBody(req): JsonBody<RegisterReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            // Idempotency check first so a retried request returns the same number even though the
            // intake is already linked.
            let intake = require_intake(tx, &actor, id)?;
            actor.require(perm::INTAKE_MANAGE)?;
            actor.require(perm::CASE_REGISTER)?;
            idempotent(tx, &actor, &key, "intake.register", &(id, &req), || {
                if status_of(&intake) != "ready_for_registration" {
                    return Err(AppError::invalid_transition(match status_of(&intake) {
                        "linked_to_case" => "This filing is already linked to a case.".to_string(),
                        s => format!("Mark the filing as checked (ready for registration) first. It is '{s}'."),
                    }));
                }
                let reference = intake["reference"].as_str().unwrap_or_default().to_string();
                let related = match req.related_case_id {
                    Some(rc) => Some(policy::require_case(tx, &actor, rc)?),
                    None => None,
                };
                let ncase = super::cases::create_case(
                    tx,
                    &actor,
                    super::cases::NewCase {
                        registry_id: req.registry_id,
                        category: &req.category,
                        title: &req.title,
                        summary: optional(&req.summary),
                        registered_date: req.registered_date.clone(),
                        restricted: req.restricted,
                        responsible_user_id: req.responsible_user_id,
                    },
                )?;
                for p in &req.participants {
                    super::cases::add_participant(tx, &actor, ncase.id, p)?;
                }
                tx.execute("UPDATE intakes SET case_id = ?2 WHERE id = ?1", params![id, ncase.id])?;
                set_status(tx, id, "linked_to_case", Some("Registered as a new case"))?;
                tx.execute(
                    "UPDATE intakes SET case_id = ?2, status = 'linked_to_case' WHERE parent_intake_id = ?1",
                    params![id, ncase.id],
                )?;
                let docs = attach_documents(tx, id, ncase.id)?;
                let cancelled_requests = cancel_information_requests(tx, &actor, id, ncase.id)?;
                if let Some(rel) = &related {
                    tx.execute(
                        "INSERT INTO case_relations (from_case_id, to_case_id, kind, note, created_by, created_at)
                         VALUES (?1, ?2, 'follow_up', ?3, ?4, ?5)",
                        params![ncase.id, rel.id, format!("Registered from {reference}"), actor.user_id, crate::time::now_utc()],
                    )?;
                }
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new("case.registered", "case", ncase.id, format!("Case {} registered from {reference}", ncase.number))
                        .case(Some(ncase.id))
                        .details(json!({ "intake_id": id, "documents_attached": docs, "related_case_id": req.related_case_id })),
                )?;
                Ok(json!({ "case_id": ncase.id, "number": ncase.number, "cancelled_requests": cancelled_requests }))
            })
        })
        .await?;
    Ok(Json(v))
}

/// Ensure an intake is in an open state (used by documents module for uploads).
pub fn require_open_intake(conn: &Connection, actor: &Actor, id: i64) -> AppResult<Value> {
    let intake = require_intake(conn, actor, id)?;
    if !OPEN_STATES.contains(&status_of(&intake)) && status_of(&intake) != "linked_to_case" {
        return Err(AppError::invalid_transition("Documents can no longer be added to this filing."));
    }
    Ok(intake)
}
