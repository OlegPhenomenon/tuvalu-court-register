//! C09/C12 Dispatch: prepared notices and per-recipient copy packages. Nothing leaves the server —
//! method `email` is delivered to the local mailbox by the outbox worker; post/hand/collection/
//! island_officer are recorded by hand via record-sent. A human preview is mandatory before sending;
//! technical attempts, handover confirmations and the legal assessment of service are separate records.

use super::common::{
    JsonBody, JsonResult, optional, query_json, query_one_json, reason, ref_label, render_template,
    require_ref, required,
};
use crate::audit::{self, Event};
use crate::auth::{Actor, Ctx, IdemKey, idempotent};
use crate::error::{AppError, AppResult};
use crate::policy::{self, perm};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/cases/{id}/dispatches", get(list_for_case).post(create))
        .route("/dispatches", get(list_all))
        .route("/dispatches/{id}", get(detail).patch(update))
        .route("/dispatches/{id}/preview", post(preview))
        .route("/dispatches/{id}/queue", post(queue))
        .route("/dispatches/{id}/record-sent", post(record_sent))
        .route("/dispatches/{id}/retry", post(retry))
        .route("/dispatches/{id}/confirm", post(confirm))
        .route("/dispatches/{id}/assess", post(assess))
        .route("/dispatches/{id}/cancel", post(cancel))
}

/// SQL boolean: actor may see dispatch `d`. Requires `LEFT JOIN intakes i ON i.id = d.intake_id`
/// in the query — intake dispatches (information requests) follow intake visibility.
pub fn dispatch_visible_sql(actor: &Actor) -> String {
    format!(
        "((d.case_id IS NOT NULL AND {}) OR (d.case_id IS NULL AND d.intake_id IS NOT NULL AND {}))",
        policy::case_visible_sql(actor, "d.case_id"),
        if actor.has(perm::INTAKE_MANAGE) {
            policy::intake_visible_sql(actor, "i")
        } else {
            "0".into()
        }
    )
}

const DISPATCH_SQL: &str = "\
SELECT d.id, d.case_id, c.number AS case_number, d.intake_id, i.reference AS intake_reference,
       d.hearing_id, d.hearing_version, d.hearing_starts_at, d.notice_purpose, d.kind, d.template_code, d.recipient_party_id, d.recipient_name, d.method, d.address,
       d.subject, d.body, d.purpose, d.status,
       ru.display_name AS reviewed_by_name, d.reviewed_at,
       pu.display_name AS prepared_by_name, d.prepared_at,
       qu.display_name AS queued_by_name, d.queued_at, d.sent_at,
       d.failure_reason, d.status_reason, d.version
  FROM dispatches d
  LEFT JOIN cases c ON c.id = d.case_id
  LEFT JOIN intakes i ON i.id = d.intake_id
  LEFT JOIN users ru ON ru.id = d.reviewed_by
  LEFT JOIN users pu ON pu.id = d.prepared_by
  LEFT JOIN users qu ON qu.id = d.queued_by";

/// Plain-language line for the card (spec §5: explain the next step, not a bare status code).
fn state_summary(d: &Value) -> String {
    match d["status"].as_str().unwrap_or_default() {
        "draft" if d["reviewed_at"].is_null() => "Prepared — check recipient and contents".into(),
        "draft" => "Reviewed — ready to send".into(),
        "queued" if d["failure_reason"].as_str() == Some("Mail transport not configured") => "Queued — mail transport not configured".into(),
        "queued" => "Queued for delivery".into(),
        "sent" => {
            let handed = d["confirmations"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .any(|c| c["kind"].as_str() == Some("human_handover"))
                })
                .unwrap_or(false);
            if handed {
                "Delivered and handover confirmed".into()
            } else {
                "Sent — waiting for confirmation of handover".into()
            }
        }
        "failed" => format!(
            "Delivery failed: {}",
            d["failure_reason"].as_str().unwrap_or("unknown reason")
        ),
        "superseded" => d["status_reason"].as_str().unwrap_or("Superseded — prepare a new dispatch").into(),
        "cancelled" => match d["status_reason"].as_str() {
            Some(r) => format!("Cancelled: {r}"),
            None => "Cancelled".into(),
        },
        s => s.to_string(),
    }
}

/// Full dispatch JSON (composition + attempts + confirmations + assessments), 404 if invisible.
fn dispatch_json(conn: &Connection, actor: &Actor, id: i64) -> AppResult<Value> {
    let sql = format!(
        "{DISPATCH_SQL} WHERE d.id = ?1 AND {}",
        dispatch_visible_sql(actor)
    );
    let mut d = query_one_json(conn, &sql, [id])?;
    d["items"] = json!(query_json(
        conn,
        "SELECT di.document_version_id, v.document_id, doc.title AS document_title, v.version_no,
                v.filename, v.sha256, doc.visibility, di.material_kind, di.decision_id
         FROM dispatch_items di
         JOIN document_versions v ON v.id = di.document_version_id
         JOIN documents doc ON doc.id = v.document_id
         WHERE di.dispatch_id = ?1 ORDER BY di.id",
        [id]
    )?);
    redact_items(conn, actor, &mut d, "items")?;
    d["attempts"] = json!(query_json(
        conn,
        "SELECT attempt_no, status, technical_receipt, detail, occurred_date, at
         FROM delivery_attempts WHERE dispatch_id = ?1 ORDER BY attempt_no",
        [id]
    )?);
    d["confirmations"] = json!(query_json(
        conn,
        "SELECT dc.kind, dc.note, dc.occurred_date, u.display_name AS recorded_by_name, dc.recorded_at
         FROM delivery_confirmations dc LEFT JOIN users u ON u.id = dc.recorded_by
         WHERE dc.dispatch_id = ?1 ORDER BY dc.id",
        [id]
    )?);
    d["assessments"] = json!(query_json(
        conn,
        "SELECT sa.assessment, sa.basis, u.display_name AS assessed_by_name, sa.assessed_at
         FROM service_assessments sa LEFT JOIN users u ON u.id = sa.assessed_by
         WHERE sa.dispatch_id = ?1 ORDER BY sa.id",
        [id]
    )?);
    d["mailbox_ids"] = json!(
        query_json(
            conn,
            "SELECT id FROM mailbox WHERE dispatch_id = ?1 ORDER BY id",
            [id]
        )?
        .iter()
        .filter_map(|r| r["id"].as_i64())
        .collect::<Vec<i64>>()
    );
    d["state_summary"] = json!(state_summary(&d));
    Ok(d)
}

/// Redact both structured metadata and the inventory embedded in the body.
/// Mailbox attachments use the same document-version ids and policy.
pub(super) fn redact_items(
    conn: &Connection,
    actor: &Actor,
    value: &mut Value,
    field: &str,
) -> AppResult<()> {
    let mut body = value["body"].as_str().unwrap_or_default().to_string();
    if let Some(items) = value[field].as_array_mut() {
        for item in items {
            let vid = item["document_version_id"].as_i64();
            let sql = format!(
                "SELECT COUNT(*) FROM document_versions v JOIN documents doc ON doc.id=v.document_id WHERE v.id=?1 AND {}",
                policy::document_visible_sql(actor, "doc")
            );
            let visible: i64 = conn.query_row(&sql, [vid], |r| r.get(0))?;
            if visible == 0 {
                // Version metadata is immutable; the document title may have changed since send.
                let metadata = query_json(conn,
                    "SELECT version_no, filename FROM document_versions WHERE id=?1", [vid])?.into_iter().next();
                let suffix = metadata.as_ref().map(|m| format!(" (version {}, {})", m["version_no"], m["filename"].as_str().unwrap_or_default()));
                // Legacy/malformed attachments may have no version at all. Fail closed for
                // their inventory text, without aborting the mailbox list or leaking filenames.
                body = body.lines().map(|line| {
                    if line.starts_with("- ") && suffix.as_ref().is_none_or(|s| line.ends_with(s)) {
                        "- Restricted document"
                    } else { line }
                }).collect::<Vec<_>>().join("\n");
                *item = json!({"document_version_id":vid,"restricted":true,"document_title":"Restricted document"});
            }
        }
    }
    value["body"] = json!(body);
    Ok(())
}

/// Intake information requests are managed with intake.manage; case dispatches use dispatch permissions.
fn require_dispatch_perm(actor: &Actor, d: &Value, permission: &str) -> AppResult<()> {
    if d["case_id"].is_null() {
        actor.require(perm::INTAKE_MANAGE)?;
        if permission == perm::DISPATCH_MANAGE {
            return Ok(());
        }
    }
    actor.require(permission)
}

fn current_items(conn: &Connection, id: i64) -> AppResult<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT document_version_id FROM dispatch_items WHERE dispatch_id = ?1 ORDER BY id",
    )?;
    Ok(stmt
        .query_map([id], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?)
}

fn validate_composition(conn: &Connection, actor: &Actor, d: &Value) -> AppResult<()> {
    check_items(
        conn,
        actor,
        d["case_id"].as_i64(),
        d["intake_id"].as_i64(),
        &current_items(conn, d["id"].as_i64().unwrap())?,
        true,
    )?;
    require_current_material(conn, actor, d["id"].as_i64().unwrap())?;
    if let Some(why) = stale_reason(conn, d["id"].as_i64().unwrap())? {
        return Err(AppError::conflict("stale_review", why));
    }
    Ok(())
}

fn review_required() -> AppError {
    AppError::conflict(
        "review_required",
        "Check the recipient and contents with Preview before sending.",
    )
}

// ------------------------------------------------------------------ lists & detail

async fn list_for_case(ctx: Ctx, Path(case_id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            policy::require_case(c, &actor, case_id)?;
            let sql = format!(
                "SELECT d.id FROM dispatches d LEFT JOIN intakes i ON i.id = d.intake_id
                 WHERE d.case_id = ?1 AND {} ORDER BY d.id",
                dispatch_visible_sql(&actor)
            );
            let ids: Vec<i64> = query_json(c, &sql, [case_id])?
                .iter()
                .filter_map(|r| r["id"].as_i64())
                .collect();
            let mut items = Vec::with_capacity(ids.len());
            for id in ids {
                items.push(dispatch_json(c, &actor, id)?);
            }
            Ok(json!({ "items": items }))
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct ListQuery {
    status: Option<String>,
    kind: Option<String>,
}

/// All dispatches the actor may see (case dispatches via case access; intake information
/// requests only for `intake.manage` holders — intake_visible_sql handles both).
async fn list_all(ctx: Ctx, Query(q): Query<ListQuery>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            let sql = format!(
                "SELECT d.id FROM dispatches d LEFT JOIN intakes i ON i.id = d.intake_id
                 WHERE {} AND (?1 IS NULL OR d.status = ?1) AND (?2 IS NULL OR d.kind = ?2)
                 ORDER BY d.id DESC LIMIT 500",
                dispatch_visible_sql(&actor)
            );
            let ids: Vec<i64> =
                query_json(c, &sql, params![optional(&q.status), optional(&q.kind)])?
                    .iter()
                    .filter_map(|r| r["id"].as_i64())
                    .collect();
            let mut items = Vec::with_capacity(ids.len());
            for id in ids {
                items.push(dispatch_json(c, &actor, id)?);
            }
            Ok(json!({ "items": items }))
        })
        .await?;
    Ok(Json(v))
}

async fn detail(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx.db.read(move |c| dispatch_json(c, &actor, id)).await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ create & edit

/// Validate the package contents against the actor's rights RIGHT NOW. Every version must be
/// visible, belong to the dispatch's case (or intake), and be clean; judicial working notes are
/// never sendable, restricted ones only with an explicit `include_restricted`. Returns the
/// deduplicated version ids and the plain-text item lines used in the rendered body.
fn check_items(
    conn: &Connection,
    actor: &Actor,
    case_id: Option<i64>,
    intake_id: Option<i64>,
    version_ids: &[i64],
    include_restricted: bool,
) -> AppResult<(Vec<i64>, String)> {
    let mut ids = Vec::new();
    let mut seen = BTreeSet::new();
    let mut lines = Vec::new();
    for &vid in version_ids {
        if !seen.insert(vid) {
            continue;
        }
        let (doc, _) = policy::require_version(conn, actor, vid)?; // invisible → 404
        let belongs = match (case_id, intake_id) {
            (Some(cid), _) => doc.case_id == Some(cid),
            (None, Some(iid)) => doc.intake_id == Some(iid),
            _ => false,
        };
        if !belongs {
            return Err(AppError::validation(
                "A selected document does not belong to this case.",
            ));
        }
        let doc_type: String = conn.query_row(
            "SELECT doc_type FROM documents WHERE id = ?1",
            [doc.id],
            |r| r.get(0),
        )?;
        if doc.visibility == "judicial_note" || doc_type == "judicial_note" {
            return Err(AppError::validation(
                "Judicial working notes cannot be sent to participants",
            ));
        }
        if doc.visibility == "restricted" && !include_restricted {
            return Err(AppError::validation(format!(
                "'{}' is restricted — include it only with an explicit restricted-content choice.",
                doc.title
            )));
        }
        let (vno, filename, clean): (i64, String, bool) = conn.query_row(
            "SELECT version_no, filename, scan_status = 'clean' FROM document_versions WHERE id = ?1",
            [vid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if !clean {
            return Err(AppError::validation(format!(
                "'{}' is quarantined and cannot be sent.",
                doc.title
            )));
        }
        ids.push(vid);
        lines.push(format!("- {} (version {vno}, {filename})", doc.title));
    }
    Ok((ids, lines.join("\n")))
}

/// Copy packages always include the exact selected inventory, even with custom text or an
/// administrator-edited template. Editing the complete existing body does not duplicate it.
fn copy_body(custom: &Option<String>, rendered: String, items: &str) -> AppResult<String> {
    let mut body = match custom {
        Some(text) => required(text, "Body")?,
        None => rendered,
    };
    if !items.is_empty() && !body.contains(items) {
        body.push_str("\n\n");
        body.push_str(items);
    }
    required(&body, "Body")
}

fn insert_items(tx: &Transaction, dispatch_id: i64, version_ids: &[i64], working: bool, require_decision: bool) -> AppResult<()> {
    for vid in version_ids {
        let decision: Option<i64> = if working { None } else {
            tx.query_row("SELECT id FROM decisions WHERE document_version_id=?1 AND status='finalised' ORDER BY id DESC LIMIT 1", [vid], |r| r.get(0)).optional()?
        };
        if require_decision && decision.is_none() {
            return Err(AppError::validation("Decision copies require the exact document version of a finalised decision."));
        }
        tx.execute(
            "INSERT INTO dispatch_items (dispatch_id, document_version_id, material_kind, decision_id) VALUES (?1, ?2, ?3, ?4)",
            params![dispatch_id, vid, if decision.is_some() { "decision_copy" } else { "working_document" }, decision],
        )?;
    }
    Ok(())
}

/// Verify decision-copy authority separately from ordinary file access at every sending step.
pub(crate) fn require_current_material(conn: &Connection, actor: &Actor, id: i64) -> AppResult<()> {
    let copies: i64 = conn.query_row("SELECT count(*) FROM dispatch_items WHERE dispatch_id=?1 AND material_kind='decision_copy'", [id], |r| r.get(0))?;
    if copies > 0 { actor.require(perm::DISPATCH_MANAGE)?; }
    Ok(())
}

/// Business currency is checked under the same write lock as delivery. Unknown legacy
/// bindings fail closed. Copies and cancellation notices are independent of hearing state.
pub(crate) fn stale_reason(conn: &Connection, id: i64) -> AppResult<Option<&'static str>> {
    let stale_hearing: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM dispatches d LEFT JOIN hearings h ON h.id=d.hearing_id
         WHERE d.id=?1 AND d.kind='notice' AND d.hearing_id IS NOT NULL AND d.notice_purpose='invitation'
         AND (h.id IS NULL OR h.status <> 'scheduled' OR d.hearing_version IS NOT h.version OR d.hearing_starts_at IS NOT h.starts_at))", [id], |r| r.get(0))?;
    if stale_hearing { return Ok(Some("Superseded — hearing changed; prepare a new notice")); }
    let stale_decision: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM dispatch_items di LEFT JOIN decisions d ON d.id=di.decision_id
         WHERE di.dispatch_id=?1 AND di.material_kind='decision_copy'
         AND (d.id IS NULL OR d.status <> 'finalised' OR d.document_version_id <> di.document_version_id))", [id], |r| r.get(0))?;
    Ok(stale_decision.then_some("Superseded — decision changed; prepare a new copy"))
}

pub(crate) fn supersede(conn: &Connection, actor: Option<&Actor>, id: i64, why: &str) -> AppResult<()> {
    let case_id: Option<i64> = conn.query_row("SELECT case_id FROM dispatches WHERE id=?1", [id], |r| r.get(0))?;
    let changed = conn.execute("UPDATE dispatches SET status='superseded',status_reason=?2,version=version+1 WHERE id=?1 AND status IN ('draft','queued','failed')", params![id, why])?;
    if changed > 0 {
        audit::record(conn, actor, Event::new("dispatch.superseded", "dispatch", id, why).case(case_id).details(json!({"reason":why})))?;
    }
    Ok(())
}

pub(super) fn supersede_hearing_notices(conn: &Connection, actor: &Actor, hearing_id: i64) -> AppResult<()> {
    let ids = query_json(conn, "SELECT id FROM dispatches WHERE hearing_id=?1 AND kind='notice' AND notice_purpose='invitation' AND status IN ('draft','queued','failed')", [hearing_id])?;
    for d in ids { supersede(conn, Some(actor), d["id"].as_i64().unwrap(), "Superseded — hearing changed; prepare a new notice")?; }
    Ok(())
}

/// Labels are mandatory even when the preparer writes custom correspondence.
fn label_material(conn: &Connection, id: i64) -> AppResult<()> {
    let has_items: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM dispatch_items WHERE dispatch_id=?1)", [id], |r| r.get(0))?;
    if !has_items { return Ok(()); }
    let working: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM dispatch_items WHERE dispatch_id=?1 AND material_kind='working_document')", [id], |r| r.get(0))?;
    let label = if working { "DRAFT / working material" } else { "Copy of finalised decision" };
    conn.execute("UPDATE dispatches SET subject=CASE WHEN subject LIKE ?2 || '%' THEN subject ELSE ?2 || ': ' || subject END,
      body=CASE WHEN body LIKE ?2 || '%' THEN body ELSE ?2 || char(10) || char(10) || body END WHERE id=?1", params![id, label])?;
    Ok(())
}

#[derive(Deserialize, Serialize)]
struct CreateReq {
    kind: String, // 'notice' | 'copies'
    recipient_party_id: Option<i64>,
    recipient_name: Option<String>,
    method: String,
    address: Option<String>,
    template_code: Option<String>,
    hearing_id: Option<i64>,
    subject: Option<String>,
    body: Option<String>,
    purpose: Option<String>,
    version_ids: Option<Vec<i64>>,
    include_restricted: Option<bool>,
}

/// Hearing-scoped template variables; the hearing must belong to this case.
/// `previous_local`/`reason` come from the hearing this one was adjourned from.
fn hearing_vars(
    conn: &Connection,
    case_id: i64,
    hearing_id: Option<i64>,
) -> AppResult<[(String, String); 5]> {
    let mut vars = [
        ("hearing_local".to_string(), String::new()),
        ("hearing_type".to_string(), String::new()),
        ("room".to_string(), String::new()),
        ("previous_local".to_string(), String::new()),
        ("reason".to_string(), String::new()),
    ];
    let Some(hid) = hearing_id else {
        return Ok(vars);
    };
    let h: Option<(String, String, Option<String>, Option<i64>)> = conn
        .query_row(
            "SELECT h.hearing_type, h.starts_at, r.name, h.previous_hearing_id
             FROM hearings h LEFT JOIN rooms r ON r.id = h.room_id WHERE h.id = ?1 AND h.case_id = ?2",
            params![hid, case_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((htype, starts, room_name, prev)) = h else {
        return Err(AppError::validation(
            "That hearing does not belong to this case.",
        ));
    };
    vars[0].1 = crate::time::utc_to_local(&starts);
    vars[1].1 = ref_label(conn, "hearing_type", &htype)?;
    vars[2].1 = room_name.unwrap_or_default();
    if let Some(prev_id) = prev {
        let p: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT starts_at, status_reason FROM hearings WHERE id = ?1",
                [prev_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((pstarts, preason)) = p {
            vars[3].1 = crate::time::utc_to_local(&pstarts);
            vars[4].1 = preason.unwrap_or_default();
        }
    }
    Ok(vars)
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
            let case = policy::require_case_perm(tx, &actor, case_id, perm::DISPATCH_MANAGE)?;
            idempotent(tx, &actor, &key, "dispatch.create", &(case_id, &req), || {
                if !matches!(req.kind.as_str(), "notice" | "copies" | "decision_copy" | "working_document") {
                    return Err(AppError::validation("Kind must be notice, copies, decision_copy or working_document."));
                }
                let copies = matches!(req.kind.as_str(), "copies" | "decision_copy" | "working_document");
                let kind = if copies { "copies" } else { "notice" };
                let notice_purpose = if req.template_code.as_deref() == Some("hearing_cancellation") { "cancellation" } else { "invitation" };
                require_ref(tx, "dispatch_method", &req.method)?;
                // One dispatch = one recipient: an active participant (default address = its service
                // contact, then the party's e-mail, then the party's address) or a free-form name.
                let mut recipient_name = optional(&req.recipient_name);
                let mut address = optional(&req.address);
                if let Some(pid) = req.recipient_party_id {
                    let party: (String, Option<String>, Option<String>, Option<String>) = tx
                        .query_row(
                            "SELECT p.name, cp.service_contact, p.contact_email, p.address FROM case_participations cp
                             JOIN parties p ON p.id = cp.party_id
                             WHERE cp.case_id = ?1 AND cp.party_id = ?2 AND cp.active = 1",
                            params![case_id, pid],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                        )
                        .optional()?
                        .ok_or_else(|| AppError::validation("The recipient is not an active participant of this case."))?;
                    if recipient_name.is_none() {
                        recipient_name = Some(party.0);
                    }
                    if address.is_none() {
                        address = [party.1, party.2, party.3]
                            .into_iter()
                            .flatten()
                            .find(|a| !a.trim().is_empty());
                    }
                }
                let recipient_name = recipient_name
                    .ok_or_else(|| AppError::validation("Recipient name is required.").with_details(json!({ "field": "recipient_name" })))?;
                if req.method == "email" && address.is_none() {
                    return Err(AppError::validation("An e-mail address is required for this delivery method.").with_details(json!({ "field": "address" })));
                }
                let hv = hearing_vars(tx, case_id, req.hearing_id)?;
                let version_ids = req.version_ids.clone().unwrap_or_default();
                let include_restricted = req.include_restricted.unwrap_or(false);
                let (version_ids, items_text) = check_items(tx, &actor, Some(case_id), None, &version_ids, include_restricted)?;
                if copies && version_ids.is_empty() {
                    return Err(AppError::validation("Choose at least one document version to send."));
                }
                // The template supplies defaults. A custom copy body must retain the inventory;
                // the mandatory preview shows the final composition before anything is sent.
                let (mut subject, mut body) = if copies {
                    render_template(
                        tx,
                        "copy_dispatch",
                        &[
                            ("recipient", recipient_name.clone()),
                            ("case_number", case.number.clone()),
                            ("case_title", case.title.clone()),
                            ("items", items_text.clone()),
                        ],
                    )?
                } else if let Some(code) = optional(&req.template_code) {
                    let vars: Vec<(&str, String)> = [
                        ("recipient", recipient_name.clone()),
                        ("case_number", case.number.clone()),
                        ("case_title", case.title.clone()),
                    ]
                    .into_iter()
                    .chain(hv.iter().map(|(k, v)| (k.as_str(), v.clone())))
                    .collect();
                    render_template(tx, &code, &vars)?
                } else {
                    (String::new(), String::new())
                };
                if let Some(s) = optional(&req.subject) {
                    subject = s;
                }
                if !copies && let Some(b) = optional(&req.body) {
                    body = b;
                }
                if copies {
                    body = copy_body(&req.body, body, &items_text)?;
                }
                let subject = required(&subject, "Subject")?;
                let body = required(&body, "Body")?;
                let template_code = if copies { Some("copy_dispatch".to_string()) } else { optional(&req.template_code) };
                tx.execute(
                    "INSERT INTO dispatches (case_id, hearing_id, kind, template_code, recipient_party_id, recipient_name,
                                             method, address, subject, body, purpose, status, prepared_by, prepared_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'draft', ?12, ?13)",
                    params![
                        case_id,
                        req.hearing_id,
                        kind,
                        template_code,
                        req.recipient_party_id,
                        recipient_name,
                        req.method,
                        address,
                        subject,
                        body,
                        optional(&req.purpose),
                        actor.user_id,
                        crate::time::now_utc()
                    ],
                )?;
                let id = tx.last_insert_rowid();
                insert_items(tx, id, &version_ids, req.kind == "working_document", req.kind == "decision_copy")?;
                label_material(tx, id)?;
                tx.execute("UPDATE dispatches SET notice_purpose=?2, hearing_version=(SELECT version FROM hearings WHERE id=hearing_id), hearing_starts_at=(SELECT starts_at FROM hearings WHERE id=hearing_id) WHERE id=?1", params![id, notice_purpose])?;
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new(
                        "dispatch.prepared",
                        "dispatch",
                        id,
                        super::common::dispatch_activity(tx, id, "prepared")?,
                    )
                    .case(Some(case_id)),
                )?;
                dispatch_json(tx, &actor, id)
            })
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct PatchReq {
    version: i64,
    recipient_name: Option<String>,
    method: Option<String>,
    address: Option<String>,
    subject: Option<String>,
    body: Option<String>,
    version_ids: Option<Vec<i64>>,
    include_restricted: Option<bool>,
}

/// Draft edits clear the recorded review — a send is only possible after previewing the
/// composition as it stands now.
async fn update(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<PatchReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let d = dispatch_json(tx, &actor, id)?;
            require_dispatch_perm(&actor, &d, perm::DISPATCH_MANAGE)?;
            if d["status"].as_str() != Some("draft") {
                return Err(AppError::invalid_transition("Only a draft dispatch can be edited."));
            }
            if d["version"].as_i64() != Some(req.version) {
                return Err(AppError::version_conflict(d));
            }
            if let Some(m) = &req.method {
                require_ref(tx, "dispatch_method", m)?;
            }
            let subject = req.subject.as_deref().map(|s| required(s, "Subject")).transpose()?;
            let mut body = req.body.as_deref().map(|s| required(s, "Body")).transpose()?;
            let old_items = d["items"].as_array().map(|items| items.iter().map(|item| {
                if item["restricted"] == true { "- Restricted document".to_string() } else {
                    format!("- {} (version {}, {})", item["document_title"].as_str().unwrap_or_default(), item["version_no"], item["filename"].as_str().unwrap_or_default())
                }
            }).collect::<Vec<_>>().join("\n"));
            if let Some(vids) = &req.version_ids {
                let (ids, _) = check_items(tx, &actor, d["case_id"].as_i64(), d["intake_id"].as_i64(), vids, req.include_restricted.unwrap_or(false))?;
                if d["kind"] == "copies" && ids.is_empty() {
                    return Err(AppError::validation("Choose at least one document version to send."));
                }
                tx.execute("DELETE FROM dispatch_items WHERE dispatch_id = ?1", [id])?;
                let decision_only = d["items"].as_array().is_some_and(|items| !items.is_empty() && items.iter().all(|i| i["material_kind"] == "decision_copy"));
                insert_items(tx, id, &ids, !decision_only, decision_only)?;
            }
            if d["kind"] == "copies" && (req.version_ids.is_some() || req.body.is_some() || req.recipient_name.is_some()) {
                let (_, items_text) = check_items(tx, &actor, d["case_id"].as_i64(), None, &current_items(tx, id)?, true)?;
                let title: String = tx.query_row("SELECT title FROM cases WHERE id = ?1", [d["case_id"].as_i64()], |r| r.get(0))?;
                let (_, rendered) = render_template(tx, "copy_dispatch", &[
                    ("recipient", optional(&req.recipient_name).unwrap_or_else(|| d["recipient_name"].as_str().unwrap_or_default().into())),
                    ("case_number", d["case_number"].as_str().unwrap_or_default().into()),
                    ("case_title", title), ("items", items_text.clone()),
                ])?;
                let custom = req.body.clone().or_else(|| {
                    let stored = d["body"].as_str().unwrap_or_default();
                    let old = old_items.as_deref().unwrap_or_default();
                    Some(if old.is_empty() { stored.to_string() } else { stored.replace(old, &items_text) })
                });
                body = Some(copy_body(&custom, rendered, &items_text)?);
            }
            let recipient_name = match &req.recipient_name {
                Some(n) => Some(required(n, "Recipient name")?),
                None => None,
            };
            tx.execute(
                "UPDATE dispatches SET recipient_name = COALESCE(?2, recipient_name), method = COALESCE(?3, method),
                        address = COALESCE(?4, address), subject = COALESCE(?5, subject), body = COALESCE(?6, body),
                        reviewed_by = NULL, reviewed_at = NULL, version = version + 1
                 WHERE id = ?1",
                params![id, recipient_name, req.method, optional(&req.address), subject, body],
            )?;
            label_material(tx, id)?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("dispatch.updated", "dispatch", id, "Draft dispatch edited".to_string())
                    .case(d["case_id"].as_i64())
                    .details(json!({ "before": d })),
            )?;
            dispatch_json(tx, &actor, id)
        })
        .await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ review, send, confirm

/// The mandatory human review: returns the exact composition and records who checked it.
async fn preview(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let d = dispatch_json(tx, &actor, id)?;
            require_dispatch_perm(&actor, &d, perm::DISPATCH_MANAGE)?;
            if !matches!(d["status"].as_str(), Some("draft") | Some("failed")) {
                return Err(AppError::invalid_transition("Only a draft or failed dispatch can be reviewed."));
            }
            validate_composition(tx, &actor, &d)?;
            tx.execute(
                "UPDATE dispatches SET reviewed_by = ?2, reviewed_at = ?3, version = version + 1,
                 hearing_version=(SELECT version FROM hearings WHERE id=hearing_id),
                 hearing_starts_at=(SELECT starts_at FROM hearings WHERE id=hearing_id) WHERE id = ?1",
                params![id, actor.user_id, crate::time::now_utc()],
            )?;
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "dispatch.reviewed",
                    "dispatch",
                    id,
                    super::common::dispatch_activity(tx, id, "reviewed")?,
                )
                .case(d["case_id"].as_i64()),
            )?;
            dispatch_json(tx, &actor, id)
        })
        .await?;
    Ok(Json(v))
}

/// Queue an e-mail dispatch for the outbox worker (local mailbox). Manual methods are sent via
/// record-sent instead.
async fn queue(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
    IdemKey(key): IdemKey,
    JsonBody(body): JsonBody<Value>,
) -> JsonResult {
    let actor = ctx.actor;
    let db = ctx.db.clone();
    let v = ctx
        .db
        .write(move |tx| {
            let d = dispatch_json(tx, &actor, id)?;
            require_dispatch_perm(&actor, &d, perm::DISPATCH_MANAGE)?;
            idempotent(tx, &actor, &key, "dispatch.queue", &(id, &body), || {
                if d["status"].as_str() != Some("draft") {
                    return Err(AppError::invalid_transition("This dispatch is not a draft."));
                }
                if d["reviewed_at"].is_null() {
                    return Err(review_required());
                }
                if d["method"].as_str() != Some("email") {
                    return Err(AppError::invalid_transition(
                        "This dispatch does not go by e-mail — record the handover with record-sent instead.",
                    ));
                }
                validate_composition(tx, &actor, &d)?;
                tx.execute(
                    "UPDATE dispatches SET status = 'queued', queued_by = ?2, queued_at = ?3, failure_reason = NULL,
                            version = version + 1 WHERE id = ?1",
                    params![id, actor.user_id, crate::time::now_utc()],
                )?;
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new("dispatch.queued", "dispatch", id, super::common::dispatch_activity(tx, id, "queued for delivery")?)
                        .case(d["case_id"].as_i64()),
                )?;
                dispatch_json(tx, &actor, id)
            })
        })
        .await?;
    state.kick_outbox(&db);
    Ok(Json(v))
}

#[derive(Deserialize, Serialize)]
struct RecordSentReq {
    occurred_date: String,
    note: Option<String>,
}

/// Manual methods (post, hand, collection, island_officer): record that the reviewed draft was
/// physically sent/handed over — a `sent` attempt with technical receipt "manual".
async fn record_sent(
    ctx: Ctx,
    Path(id): Path<i64>,
    IdemKey(key): IdemKey,
    JsonBody(req): JsonBody<RecordSentReq>,
) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let d = dispatch_json(tx, &actor, id)?;
            require_dispatch_perm(&actor, &d, perm::DISPATCH_MANAGE)?;
            idempotent(tx, &actor, &key, "dispatch.record_sent", &(id, &req), || {
                if d["method"].as_str() == Some("email") {
                    return Err(AppError::invalid_transition("E-mail dispatches go through the outbox — use queue."));
                }
                if d["status"].as_str() != Some("draft") {
                    return Err(AppError::invalid_transition("This dispatch is not a draft."));
                }
                if d["reviewed_at"].is_null() {
                    return Err(review_required());
                }
                validate_composition(tx, &actor, &d)?;
                let date = crate::time::parse_date(&req.occurred_date)?;
                let note = reason(&req.note)?;
                let attempt_no: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(attempt_no), 0) + 1 FROM delivery_attempts WHERE dispatch_id = ?1",
                    [id],
                    |r| r.get(0),
                )?;
                tx.execute(
                    "INSERT INTO delivery_attempts (dispatch_id, attempt_no, status, technical_receipt, detail, occurred_date, at)
                     VALUES (?1, ?2, 'sent', 'manual', ?3, ?4, ?5)",
                    params![id, attempt_no, note, date, crate::time::now_utc()],
                )?;
                tx.execute(
                    "UPDATE dispatches SET status = 'sent', sent_at = ?2, version = version + 1 WHERE id = ?1",
                    params![id, crate::time::now_utc()],
                )?;
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new("dispatch.sent", "dispatch", id, super::common::dispatch_activity(tx, id, "sent")?)
                        .case(d["case_id"].as_i64())
                        .details(json!({ "method": d["method"], "manual": true, "occurred_date": date, "attempt_no": attempt_no })),
                )?;
                dispatch_json(tx, &actor, id)
            })
        })
        .await?;
    Ok(Json(v))
}

/// A failed delivery goes back to the queue; the worker writes attempt n+1. The earlier review
/// still stands — any edit would have cleared `reviewed_at`.
async fn retry(State(state): State<AppState>, ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let db = ctx.db.clone();
    let v = ctx
        .db
        .write(move |tx| {
            let d = dispatch_json(tx, &actor, id)?;
            require_dispatch_perm(&actor, &d, perm::DISPATCH_MANAGE)?;
            if d["status"].as_str() != Some("failed") {
                return Err(AppError::invalid_transition("Only a failed dispatch can be retried."));
            }
            if d["method"].as_str() != Some("email") {
                return Err(AppError::invalid_transition("This dispatch does not go by e-mail — record a fresh delivery with record-sent."));
            }
            if d["reviewed_at"].is_null() {
                return Err(review_required());
            }
            validate_composition(tx, &actor, &d)?;
            tx.execute(
                "UPDATE dispatches SET status = 'queued', queued_by = ?2, queued_at = ?3, failure_reason = NULL,
                        version = version + 1 WHERE id = ?1",
                params![id, actor.user_id, crate::time::now_utc()],
            )?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("dispatch.retried", "dispatch", id, "Queued again after a failed delivery".to_string()).case(d["case_id"].as_i64()),
            )?;
            dispatch_json(tx, &actor, id)
        })
        .await?;
    state.kick_outbox(&db);
    Ok(Json(v))
}

#[derive(Deserialize, Serialize)]
struct ConfirmReq {
    kind: String, // 'technical_ack' | 'human_handover'
    note: String,
    occurred_date: Option<String>,
}

/// Human confirmation of handover/receipt — a separate record from the technical attempt.
async fn confirm(ctx: Ctx, Path(id): Path<i64>, IdemKey(key): IdemKey, JsonBody(req): JsonBody<ConfirmReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let d = dispatch_json(tx, &actor, id)?;
            require_dispatch_perm(&actor, &d, perm::DISPATCH_MANAGE)?;
            idempotent(tx, &actor, &key, "dispatch.confirm", &(id, &req), || {
                if d["status"].as_str() != Some("sent") {
                    return Err(AppError::invalid_transition("Only a sent dispatch can be confirmed."));
                }
                if !matches!(req.kind.as_str(), "technical_ack" | "human_handover") {
                    return Err(AppError::validation("Kind must be 'technical_ack' or 'human_handover'."));
                }
                let note = required(&req.note, "Note")?;
                tx.execute(
                    "INSERT INTO delivery_confirmations (dispatch_id, kind, note, occurred_date, recorded_by, recorded_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, req.kind, note, crate::time::parse_opt_date(req.occurred_date.as_deref())?, actor.user_id, crate::time::now_utc()],
                )?;
                let recipient = d["recipient_name"].as_str().unwrap_or_default();
                let summary = match req.kind.as_str() {
                    "human_handover" => format!("Handover to {recipient} confirmed by a person"),
                    _ => format!("Technical delivery acknowledgement recorded for {recipient}"),
                };
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new("dispatch.confirmed", "dispatch", id, summary)
                        .case(d["case_id"].as_i64())
                        .details(json!({ "kind": req.kind })),
                )?;
                dispatch_json(tx, &actor, id)
            })
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct AssessReq {
    assessment: String, // 'served' | 'not_served' | 'undetermined'
    basis: String,
}

/// The legal assessment of service, recorded separately by an authorised human. The register
/// never computes it (and never derives deadlines from it).
async fn assess(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<AssessReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let d = dispatch_json(tx, &actor, id)?;
            require_dispatch_perm(&actor, &d, perm::DISPATCH_ASSESS_SERVICE)?;
            if d["status"].as_str() != Some("sent") {
                return Err(AppError::invalid_transition("Service can only be assessed after the dispatch was sent."));
            }
            if !matches!(req.assessment.as_str(), "served" | "not_served" | "undetermined") {
                return Err(AppError::validation("Assessment must be 'served', 'not_served' or 'undetermined'."));
            }
            let basis = required(&req.basis, "Basis")?;
            tx.execute(
                "INSERT INTO service_assessments (dispatch_id, assessment, basis, assessed_by, assessed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, req.assessment, basis, actor.user_id, crate::time::now_utc()],
            )?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("dispatch.assessed", "dispatch", id, format!("Service assessed as '{}'", req.assessment.replace('_', " ")))
                    .case(d["case_id"].as_i64())
                    .details(json!({ "assessment": req.assessment, "basis": basis })),
            )?;
            dispatch_json(tx, &actor, id)
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct CancelReq {
    reason: Option<String>,
}

async fn cancel(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<CancelReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let d = dispatch_json(tx, &actor, id)?;
            require_dispatch_perm(&actor, &d, perm::DISPATCH_MANAGE)?;
            if !matches!(d["status"].as_str(), Some("draft") | Some("queued") | Some("failed")) {
                return Err(AppError::invalid_transition("This dispatch can no longer be cancelled."));
            }
            let why = reason(&req.reason)?;
            tx.execute(
                "UPDATE dispatches SET status = 'cancelled', status_reason = ?2, version = version + 1 WHERE id = ?1",
                params![id, why],
            )?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("dispatch.cancelled", "dispatch", id, super::common::dispatch_activity(tx, id, "cancelled")?)
                    .case(d["case_id"].as_i64())
                    .details(json!({ "reason": why })),
            )?;
            dispatch_json(tx, &actor, id)
        })
        .await?;
    Ok(Json(v))
}
