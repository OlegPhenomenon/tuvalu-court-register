//! Durable outbox claims. Short transactions surround attachment preparation and
//! SMTP; network I/O never holds SQLite's writer lock.
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
    version: i64,
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
    crate::api::dispatch::require_current_material(conn, actor, d.id)?;
    let mut stmt = conn.prepare(
        "SELECT v.id, v.document_id, v.filename, v.sha256, v.size_bytes, v.scan_status, doc.doc_type, di.material_kind
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
            r.get::<_, String>(7)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (version_id, doc_id, filename, sha256, size_bytes, scan, doc_type, material_kind) = row?;
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
        out.push(json!({"filename": filename, "sha256": sha256, "size_bytes": size_bytes, "document_version_id": version_id, "material_kind": material_kind}));
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

struct Claim {
    dispatch: Dispatch,
    token: String,
    attempt: i64,
    message_id: String,
    items: Vec<Value>,
}

fn dispatch(conn: &Connection, id: i64) -> AppResult<Option<Dispatch>> {
    Ok(conn
        .query_row(
            "SELECT case_id,intake_id,queued_by,address,subject,body,method,
         reviewed_by IS NOT NULL AND reviewed_at IS NOT NULL,kind,version
         FROM dispatches WHERE id=?1",
            [id],
            |r| {
                Ok(Dispatch {
                    id,
                    case_id: r.get(0)?,
                    intake_id: r.get(1)?,
                    queued_by: r.get(2)?,
                    address: r.get(3)?,
                    subject: r.get(4)?,
                    body: r.get(5)?,
                    method: r.get(6)?,
                    reviewed: r.get(7)?,
                    kind: r.get(8)?,
                    version: r.get(9)?,
                })
            },
        )
        .optional()?)
}
fn actor(conn: &Connection, d: &Dispatch) -> AppResult<Option<Actor>> {
    d.queued_by
        .map(|uid| auth::load_actor(conn, uid, None))
        .transpose()
        .map(Option::flatten)
}
fn checked_items(conn: &Connection, d: &Dispatch) -> AppResult<Vec<Value>> {
    let actor = actor(conn, d)?.ok_or_else(|| AppError::forbidden(CHANGED))?;
    attachments(conn, &actor, d)
}

/// Finish only the named claim. A delayed worker can never overwrite a recovered attempt.
fn finish(conn: &Connection, claim: &Claim, result: Result<&str, String>) -> AppResult<bool> {
    let d = &claim.dispatch;
    let live: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM delivery_attempts WHERE claim=?1 AND status='in_flight')",
        [&claim.token],
        |r| r.get(0),
    )?;
    if !live {
        return Ok(false);
    }
    let current: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM dispatches WHERE id=?1 AND status='queued' AND version=?2)",
        params![d.id, d.version],
        |r| r.get(0),
    )?;
    let now = crate::time::now_utc();
    let active = actor(conn, d)?;
    let attribution = audit_actor(conn, d.queued_by, &active)?;
    match result {
        Err(why) => {
            conn.execute("UPDATE delivery_attempts SET status='failed',detail=?2,at=?3 WHERE claim=?1 AND status='in_flight'",params![claim.token,why,now])?;
            if current {
                conn.execute(
                    "UPDATE dispatches SET status='failed',failure_reason=?2,version=version+1,
                    reviewed_by=CASE WHEN ?3 THEN NULL ELSE reviewed_by END,
                    reviewed_at=CASE WHEN ?3 THEN NULL ELSE reviewed_at END WHERE id=?1",
                    params![d.id, why, why == CHANGED || why.starts_with("Attachment ")],
                )?;
                if why.starts_with("SMTP ") {
                    crate::mail::failed(conn, d.id, claim.attempt)?;
                } else {
                    conn.execute("DELETE FROM mail_retries WHERE dispatch_id=?1", [d.id])?;
                }
            }
            audit::record(conn,attribution.as_ref(),Event::new("dispatch.failed","dispatch",d.id,&why).case(d.case_id)
                .details(json!({"attempt_no":claim.attempt,"failure_reason":why,"claim":claim.token})))?;
        }
        Ok(transport) => {
            // Acceptance is historical evidence even if the dispatch was cancelled while
            // SMTP was running; do not overwrite the intervening business state.
            conn.execute("INSERT INTO mailbox(dispatch_id,attempt_no,to_address,subject,body,attachments,delivered_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![d.id,claim.attempt,d.address.as_deref().unwrap_or_default().trim(),d.subject,d.body,serde_json::to_string(&claim.items)?,now])?;
            let mailbox = conn.last_insert_rowid();
            crate::mail::sent(conn, d.id, mailbox, transport)?;
            let receipt = if transport == "sent via SMTP" {
                format!("sent via SMTP; mailbox:{mailbox}")
            } else {
                format!("local-mailbox:{mailbox}")
            };
            conn.execute("UPDATE delivery_attempts SET status='sent',technical_receipt=?2,at=?3 WHERE claim=?1 AND status='in_flight'",params![claim.token,receipt,now])?;
            if current {
                conn.execute("UPDATE dispatches SET status='sent',sent_at=?2,failure_reason=NULL,version=version+1 WHERE id=?1",params![d.id,now])?;
            }
            let label = if transport == "sent via SMTP" {
                "sent via SMTP"
            } else if d.kind == "copies" {
                "sent"
            } else {
                "delivered to the local mailbox"
            };
            audit::record(conn,attribution.as_ref(),Event::new("dispatch.sent","dispatch",d.id,format!("{} ({})",crate::api::common::dispatch_activity(conn,d.id,label)?,d.address.as_deref().unwrap_or_default().trim())).case(d.case_id)
                .details(json!({"attempt_no":claim.attempt,"mailbox_id":mailbox,"claim":claim.token,"message_id":claim.message_id})))?;
        }
    }
    Ok(true)
}

/// Recheck claim ownership, review, permissions, exact versions and F07/F08 currency
/// immediately before sending. This transaction commits before the first network call.
fn current(conn: &Connection, claim: &Claim) -> AppResult<bool> {
    let d = &claim.dispatch;
    let live: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM delivery_attempts a JOIN dispatches d ON d.id=a.dispatch_id
        WHERE a.claim=?1 AND a.status='in_flight' AND d.status='queued' AND d.version=a.dispatch_version)",[&claim.token],|r|r.get(0))?;
    if !live {
        finish(
            conn,
            claim,
            Err("Dispatch changed before sending; review again".into()),
        )?;
        return Ok(false);
    }
    if let Some(why) = crate::api::dispatch::stale_reason(conn, d.id)? {
        let active = actor(conn, d)?;
        let attribution = audit_actor(conn, d.queued_by, &active)?;
        crate::api::dispatch::supersede(conn, attribution.as_ref(), d.id, why)?;
        finish(conn, claim, Err(why.into()))?;
        return Ok(false);
    }
    let latest = dispatch(conn, d.id)?.ok_or_else(AppError::not_found)?;
    match checked_items(conn, &latest) {
        Ok(items) if items == claim.items => Ok(true),
        Ok(_) => {
            finish(conn, claim, Err(CHANGED.into()))?;
            Ok(false)
        }
        Err(e) if matches!(e.status.as_u16(), 400 | 403 | 404 | 409) => {
            finish(conn, claim, Err(CHANGED.into()))?;
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// A crashed attempt is ambiguous, never silently sent. A live send is bounded by
/// one SMTP timeout, so recovery waits twice that timeout before releasing its claim.
fn recover(db: &Db) -> AppResult<()> {
    let timeout = db.config().map_or(20, |c| c.smtp_timeout_secs);
    let cutoff =
        crate::time::fmt_utc(crate::time::now() - time::Duration::seconds((timeout * 2) as i64));
    db.write_blocking(|tx| {
        let mut stmt = tx.prepare("SELECT dispatch_id,claim,attempt_no,message_id,attachments_json,dispatch_version FROM delivery_attempts WHERE status='in_flight' AND at<?1")?;
        let rows = stmt.query_map([&cutoff],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,i64>(5)?)))?.collect::<Result<Vec<_>,_>>()?;
        drop(stmt);
        for (id,token,attempt,message_id,items,version) in rows {
            let Some(mut dispatch) = dispatch(tx,id)? else { continue; };
            dispatch.version = version;
            let claim = Claim { dispatch, token, attempt, message_id, items:serde_json::from_str(&items)? };
            // If business bindings changed during the interruption, retain superseded history.
            if let Some(why) = crate::api::dispatch::stale_reason(tx,id)? {
                let active = actor(tx,&claim.dispatch)?;
                let attribution = audit_actor(tx,claim.dispatch.queued_by,&active)?;
                crate::api::dispatch::supersede(tx,attribution.as_ref(),id,why)?;
            }
            finish(tx,&claim,Err("SMTP interrupted; delivery is ambiguous; retry uses the same Message-ID".into()))?;
        }
        Ok(())
    })
}

/// Deliver oldest first. Claim commits before attachment reads/MIME building, final
/// validation commits before SMTP, and completion is a short claim-keyed transaction.
pub fn process(db: &Db) -> AppResult<usize> {
    recover(db)?;
    crate::mail::retry_due(db)?;
    let conn = db.open()?;
    let mut stmt =
        conn.prepare("SELECT id FROM dispatches WHERE status='queued' ORDER BY queued_at,id")?;
    let ids = stmt
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    drop(conn);
    let mut processed = 0;
    for id in ids {
        let (claim, handled) = db.write_blocking(|tx| {
            let eligible: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM dispatches d WHERE id=?1 AND status='queued'
                AND NOT EXISTS(SELECT 1 FROM delivery_attempts a WHERE a.dispatch_id=d.id AND a.status='in_flight'))",[id],|r|r.get(0))?;
            if !eligible { return Ok((None,false)); }
            let d = dispatch(tx,id)?.ok_or_else(AppError::not_found)?;
            let active = actor(tx,&d)?;
            let attribution = audit_actor(tx,d.queued_by,&active)?;
            if let Some(why) = crate::api::dispatch::stale_reason(tx,id)? {
                crate::api::dispatch::supersede(tx,attribution.as_ref(),id,why)?;
                return Ok((None,true));
            }
            let (items,mut failure) = match checked_items(tx,&d) {
                Ok(items) => (items,None),
                Err(e) if matches!(e.status.as_u16(),400|403|404|409) => (Vec::new(),Some(CHANGED.to_string())),
                Err(e) => return Err(e),
            };
            let address = d.address.as_deref().unwrap_or_default().trim();
            if failure.is_none() && (!address.contains('@') || address.rsplit_once('@').is_some_and(|(_,domain)|domain.to_ascii_lowercase().ends_with(".fail"))) {
                failure = Some(REJECTED.into());
            }
            if failure.is_none() && !crate::mail::configured(db) {
                tx.execute("UPDATE dispatches SET failure_reason='Mail transport not configured' WHERE id=?1 AND failure_reason IS NOT 'Mail transport not configured'",[id])?;
                return Ok((None,false));
            }
            let attempt = tx.query_row("SELECT COALESCE(MAX(attempt_no),0)+1 FROM delivery_attempts WHERE dispatch_id=?1",[id],|r|r.get(0))?;
            let installation = crate::db::setting(tx,"installation_id","court")?;
            let message_id: Option<String> = tx.query_row("SELECT message_id FROM delivery_attempts WHERE dispatch_id=?1 AND message_id IS NOT NULL ORDER BY attempt_no LIMIT 1",[id],|r|r.get(0)).optional()?;
            let claim = Claim { dispatch:d, token:auth::random_token(), attempt, message_id:message_id.unwrap_or_else(||format!("<dispatch-{id}-{installation}@tuvalu-court.invalid>")), items };
            tx.execute("INSERT INTO delivery_attempts(dispatch_id,attempt_no,status,at,claim,message_id,attachments_json,dispatch_version) VALUES(?1,?2,'in_flight',?3,?4,?5,?6,?7)",
                params![id,attempt,crate::time::now_utc(),claim.token,claim.message_id,serde_json::to_string(&claim.items)?,claim.dispatch.version])?;
            if let Some(why) = failure {
                finish(tx,&claim,Err(why))?;
                return Ok((None,true));
            }
            audit::record(tx,attribution.as_ref(),Event::new("dispatch.delivery_started","dispatch",id,"Delivery attempt claimed").case(claim.dispatch.case_id)
                .details(json!({"attempt_no":attempt,"claim":claim.token,"message_id":claim.message_id})))?;
            Ok((Some(claim),false))
        })?;
        if handled {
            processed += 1;
        }
        let Some(claim) = claim else {
            continue;
        };
        let d = &claim.dispatch;
        let prepared = crate::mail::prepare(
            db,
            &db.open()?,
            &claim.message_id,
            d.address.as_deref().unwrap_or_default().trim(),
            &d.subject,
            &d.body,
            &claim.items,
        );
        let result = match prepared {
            Ok(prepared) => {
                if !db.write_blocking(|tx| current(tx, &claim))? {
                    processed += 1;
                    continue;
                }
                prepared.send().map_err(|e| e.message)
            }
            Err(e) if e.status.as_u16() == 400 => Err(e.message),
            Err(e) => return Err(e),
        };
        if db.write_blocking(|tx| finish(tx, &claim, result))? {
            processed += 1;
        }
    }
    Ok(processed)
}
