//! The local outbox. Every claim, permission check, attempt and mailbox entry shares one
//! immediate transaction. Production delivery delegates to the SMTP transport.
use crate::audit::{self, Event};
use crate::auth::{self, Actor};
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::policy::{self, perm};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const CHANGED: &str = "Permissions or documents changed before sending — review again";
const REJECTED: &str = "Address rejected by the local mail server";

struct Dispatch {
    id: i64,
    case_id: Option<i64>,
    intake_id: Option<i64>,
    queued_by: Option<i64>,
    address: Option<String>,
    subject: String,
    body: String,
    method: String,
    kind: String,
    reviewed: bool,
}

/// Only access/validation failures become delivery failures. Database failures roll back and
/// leave the dispatch queued so the worker can safely try again.
fn attachments(conn: &Connection, actor: &Actor, d: &Dispatch) -> AppResult<Vec<Value>> {
    if !d.reviewed || d.method != "email" {
        return Err(AppError::validation(CHANGED));
    }
    if let Some(cid) = d.case_id {
        policy::require_case_perm(conn, actor, cid, perm::DISPATCH_MANAGE)?;
    } else if let Some(iid) = d.intake_id {
        actor.require(perm::INTAKE_MANAGE)?;
        let sql = format!(
            "SELECT id FROM intakes i WHERE id = ?1 AND {}",
            policy::intake_visible_sql(actor, "i")
        );
        conn.query_row(&sql, [iid], |r| r.get::<_, i64>(0))?;
    } else {
        return Err(AppError::not_found());
    }
    let mut stmt = conn.prepare(
        "SELECT v.id, v.document_id, v.filename, v.sha256, v.size_bytes, v.scan_status, doc.doc_type
         FROM dispatch_items di JOIN document_versions v ON v.id = di.document_version_id
         JOIN documents doc ON doc.id = v.document_id WHERE di.dispatch_id = ?1 ORDER BY di.id",
    )?;
    let rows = stmt.query_map([d.id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, String>(6)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (version_id, doc_id, filename, sha256, size_bytes, scan, doc_type) = row?;
        let doc = policy::require_document(conn, actor, doc_id)?;
        let belongs = match d.case_id {
            Some(cid) => doc.case_id == Some(cid),
            None => doc.intake_id == d.intake_id,
        };
        if !belongs
            || doc.visibility == "judicial_note"
            || doc_type == "judicial_note"
            || scan != "clean"
        {
            return Err(AppError::validation(CHANGED));
        }
        out.push(json!({"filename": filename, "sha256": sha256, "size_bytes": size_bytes, "document_version_id": version_id}));
    }
    Ok(out)
}

/// Retain attribution even when the queued user has been deactivated. This actor is used
/// exclusively for audit; permission checks always use auth::load_actor's active-user result.
fn audit_actor(
    conn: &Connection,
    user_id: Option<i64>,
    active: &Option<Actor>,
) -> AppResult<Option<Actor>> {
    if let Some(actor) = active {
        return Ok(Some(actor.clone()));
    }
    let Some(id) = user_id else { return Ok(None) };
    Ok(conn
        .query_row(
            "SELECT username, display_name, is_judge FROM users WHERE id = ?1",
            [id],
            |r| {
                Ok(Actor {
                    user_id: id,
                    username: r.get(0)?,
                    display_name: r.get(1)?,
                    is_judge: r.get(2)?,
                    perms: BTreeSet::new(),
                    ip: None,
                })
            },
        )
        .optional()?)
}

/// Deliver queued dispatches, oldest first. Concurrent workers re-check the claim under
/// BEGIN IMMEDIATE; a dispatch already handled or cancelled is skipped.
pub fn process(db: &Db) -> AppResult<usize> {
    crate::mail::retry_due(db)?;
    let conn = db.open()?;
    let mut stmt =
        conn.prepare("SELECT id FROM dispatches WHERE status = 'queued' ORDER BY queued_at, id")?;
    let ids = stmt
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    drop(conn);
    let mut processed = 0;
    for id in ids {
        processed += db.write_blocking(|tx| {
            let d = tx.query_row(
                "SELECT case_id, intake_id, queued_by, address, subject, body, method,
                        reviewed_by IS NOT NULL AND reviewed_at IS NOT NULL, kind
                 FROM dispatches WHERE id = ?1 AND status = 'queued'", [id], |r| {
                    Ok(Dispatch { id, case_id: r.get(0)?, intake_id: r.get(1)?, queued_by: r.get(2)?, address: r.get(3)?,
                        subject: r.get(4)?, body: r.get(5)?, method: r.get(6)?, reviewed: r.get(7)?, kind: r.get(8)? })
                },
            ).optional()?;
            let Some(d) = d else { return Ok(0) };
            let actor = match d.queued_by {
                Some(uid) => auth::load_actor(tx, uid, None)?,
                None => None,
            };
            let checked = match &actor {
                Some(a) => attachments(tx, a, &d),
                None => Err(AppError::forbidden(CHANGED)),
            };
            let mut failure = None;
            let items = match checked {
                Ok(items) => items,
                Err(e) if matches!(e.status.as_u16(), 400 | 403 | 404) => {
                    failure = Some(CHANGED.to_string());
                    Vec::new()
                }
                Err(e) => return Err(e),
            };
            let address = d.address.as_deref().unwrap_or_default().trim();
            if failure.is_none() && (!address.contains('@') || address.rsplit_once('@').is_some_and(|(_, domain)| domain.to_ascii_lowercase().ends_with(".fail"))) {
                failure = Some(REJECTED.to_string());
            }
            let mut transport = "local mailbox (DEMO)";
            if failure.is_none() {
                match crate::mail::deliver(db, tx, id, address, &d.subject, &d.body, &items) {
                    Ok(Some(label)) => transport = label,
                    Ok(None) => return Ok(0),
                    Err(e) if e.status.as_u16() == 400 => {
                        failure = Some(e.message.clone());
                    }
                    Err(e) => return Err(e),
                }
            }
            let attempt_no: i64 = tx.query_row("SELECT COALESCE(MAX(attempt_no), 0) + 1 FROM delivery_attempts WHERE dispatch_id = ?1", [id], |r| r.get(0))?;
            let now = crate::time::now_utc();
            let attribution = audit_actor(tx, d.queued_by, &actor)?;
            if let Some(why) = failure {
                if why.starts_with("SMTP ") { crate::mail::failed(tx, id, attempt_no)?; }
                tx.execute("INSERT INTO delivery_attempts (dispatch_id, attempt_no, status, detail, at) VALUES (?1, ?2, 'failed', ?3, ?4)", params![id, attempt_no, why, now])?;
                tx.execute(
                    "UPDATE dispatches SET status = 'failed', failure_reason = ?2, version = version + 1,
                     reviewed_by = CASE WHEN ?3 THEN NULL ELSE reviewed_by END,
                     reviewed_at = CASE WHEN ?3 THEN NULL ELSE reviewed_at END WHERE id = ?1",
                    params![id, why, why == CHANGED],
                )?;
                audit::record(tx, attribution.as_ref(), Event::new("dispatch.failed", "dispatch", id, &why).case(d.case_id)
                    .details(json!({"attempt_no": attempt_no, "failure_reason": why})))?;
            } else {
                tx.execute("INSERT INTO mailbox (dispatch_id, attempt_no, to_address, subject, body, attachments, delivered_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![id, attempt_no, address, d.subject, d.body, serde_json::to_string(&items)?, now])?;
                let mailbox_id = tx.last_insert_rowid();
                crate::mail::sent(tx, id, mailbox_id, transport)?;
                tx.execute("INSERT INTO delivery_attempts (dispatch_id, attempt_no, status, technical_receipt, at) VALUES (?1, ?2, 'sent', ?3, ?4)",
                    params![id, attempt_no, if transport == "sent via SMTP" { format!("sent via SMTP; mailbox:{mailbox_id}") } else { format!("local-mailbox:{mailbox_id}") }, now])?;
                tx.execute("UPDATE dispatches SET status = 'sent', sent_at = ?2, failure_reason = NULL, version = version + 1 WHERE id = ?1", params![id, now])?;
                audit::record(tx, attribution.as_ref(), Event::new("dispatch.sent", "dispatch", id, format!("{} ({address})", crate::api::common::dispatch_activity(tx, id, if transport == "sent via SMTP" { "sent via SMTP" } else if d.kind == "copies" { "sent" } else { "delivered to the local mailbox" })?)).case(d.case_id)
                    .details(json!({"attempt_no": attempt_no, "mailbox_id": mailbox_id})))?;
            }
            Ok(1)
        })?;
    }
    Ok(processed)
}
