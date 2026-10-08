//! C15 history & audit. Every actor who can see a case may read its history; events touching a
//! document the actor cannot see are redacted (the event stays, the content is hidden).
//! The global audit browser and chain verification require `audit.view`.

use super::common::{JsonResult, optional, query_json};
use crate::auth::{Actor, Ctx};
use crate::error::AppResult;
use crate::policy::{self, perm};
use crate::state::AppState;
use axum::extract::{Path, Query};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/cases/{id}/history", get(case_history_handler))
        .route("/audit", get(list))
        .route("/audit/verify", get(verify))
}

const REDACTED: &str = "Activity on a document you do not have access to";

/// True when the event concerns a document the actor may not see.
fn touches_hidden_document(
    c: &Connection,
    actor: &Actor,
    entity_type: &str,
    entity_id: Option<i64>,
    action: &str,
) -> AppResult<bool> {
    if !matches!(entity_type, "document" | "document_version")
        && action != "document.viewed_restricted"
    {
        return Ok(false);
    }
    let Some(doc_id) = entity_id else {
        return Ok(true);
    };
    let sql = format!(
        "SELECT COUNT(*) FROM documents d WHERE d.id = {id_expr} AND {}",
        policy::document_visible_sql(actor, "d"),
        id_expr = if entity_type == "document_version" {
            "(SELECT document_id FROM document_versions WHERE id=?1)"
        } else {
            "?1"
        }
    );
    Ok(c.query_row(&sql, [doc_id], |r| r.get::<_, i64>(0))? == 0)
}

/// Shape one audit row for the case history, redacting document events the actor may not see.
fn event_json(c: &Connection, actor: &Actor, ev: &Value, with_details: bool) -> AppResult<Value> {
    let entity_type = ev["entity_type"].as_str().unwrap_or_default();
    let entity_id = ev["entity_id"].as_i64();
    let action = ev["action"].as_str().unwrap_or_default();
    let details: Value = serde_json::from_str(ev["details"].as_str().unwrap_or("{}"))?;
    let hidden_relation = if action == "case.related" {
        if let Some(other) = details["related_case_id"].as_i64() {
            policy::require_case(c, actor, other).is_err()
        } else {
            // Old events have no counterpart id: redact if any related case is hidden.
            let sql = format!(
                "SELECT COUNT(*) FROM case_relations r WHERE (r.from_case_id=?1 OR r.to_case_id=?1) AND NOT ({})",
                policy::case_visible_sql(
                    actor,
                    "CASE WHEN r.from_case_id=?1 THEN r.to_case_id ELSE r.from_case_id END"
                )
            );
            c.query_row(&sql, [entity_id], |r| r.get::<_, i64>(0))? > 0
        }
    } else {
        false
    };
    let hidden =
        hidden_relation || touches_hidden_document(c, actor, entity_type, entity_id, action)?;
    let mut out = json!({
        "id": ev["id"],
        "at": ev["at"],
        "at_local": crate::time::utc_to_local(ev["at"].as_str().unwrap_or_default()),
        "user_id": ev["user_id"],
        "user_name": ev["user_name"],
        "action": ev["action"],
        "summary": if hidden_relation { "Activity on a case you do not have access to" } else if hidden { REDACTED } else { ev["summary"].as_str().unwrap_or_default() },
    });
    if with_details {
        out["entity_type"] = ev["entity_type"].clone();
        out["entity_id"] = ev["entity_id"].clone();
        out["case_id"] = ev["case_id"].clone();
        out["ip"] = ev["ip"].clone();
        if !hidden {
            out["details"] = serde_json::from_str(ev["details"].as_str().unwrap_or("{}"))?;
        }
    }
    Ok(out)
}

/// Audit events of one case, oldest first, redacted for the given actor.
/// Shared with the case export (chronology section).
pub fn case_history(c: &Connection, actor: &Actor, case_id: i64) -> AppResult<Vec<Value>> {
    history(c, actor, case_id, None)
}

/// Participant chronology excludes private notes and restricted material outside this package.
pub(super) fn package_history(
    c: &Connection,
    actor: &Actor,
    case_id: i64,
    ids: &[i64],
) -> AppResult<Vec<Value>> {
    history(c, actor, case_id, Some(ids))
}

fn history(
    c: &Connection,
    actor: &Actor,
    case_id: i64,
    package_ids: Option<&[i64]>,
) -> AppResult<Vec<Value>> {
    policy::require_case(c, actor, case_id)?;
    let included_documents: std::collections::BTreeSet<i64> = match package_ids {
        Some(ids) => query_json(c, "SELECT DISTINCT document_id FROM document_versions WHERE id IN (SELECT value FROM json_each(?1))", [serde_json::to_string(ids)?])?.iter().filter_map(|v| v["document_id"].as_i64()).collect(),
        None => Default::default(),
    };
    let rows = query_json(
        c,
        "SELECT a.id, a.at, a.user_id, u.display_name AS user_name, a.action, a.entity_type, a.entity_id, a.summary, a.details
         FROM audit_events a LEFT JOIN users u ON u.id = a.user_id
         WHERE a.case_id = ?1
            OR (a.entity_type = 'case' AND a.entity_id = ?1)
            OR (a.entity_type = 'document' AND a.entity_id IN (SELECT d.id FROM documents d WHERE d.case_id = ?1))
            OR (a.entity_type = 'document_version' AND a.entity_id IN (SELECT v.id FROM document_versions v JOIN documents d ON d.id=v.document_id WHERE d.case_id=?1))
            OR (a.entity_type = 'hearing' AND a.entity_id IN (SELECT h.id FROM hearings h WHERE h.case_id = ?1))
            OR (a.entity_type = 'decision' AND a.entity_id IN (SELECT d.id FROM decisions d WHERE d.case_id = ?1))
            OR (a.entity_type = 'dispatch' AND a.entity_id IN (SELECT d.id FROM dispatches d WHERE d.case_id = ?1))
            OR (a.entity_type = 'task' AND a.entity_id IN (SELECT t.id FROM tasks t WHERE t.case_id = ?1))
         ORDER BY a.id",
        [case_id],
    )?;
    let mut events = Vec::with_capacity(rows.len());
    for ev in &rows {
        if let Some(ids) = package_ids {
            let entity = ev["entity_type"].as_str().unwrap_or_default();
            if matches!(entity, "document" | "document_version") {
                let did = if entity == "document_version" {
                    c.query_row(
                        "SELECT document_id FROM document_versions WHERE id=?1",
                        [ev["entity_id"].as_i64()],
                        |r| r.get::<_, i64>(0),
                    )?
                } else {
                    ev["entity_id"].as_i64().unwrap_or_default()
                };
                let (visibility, doc_type): (String, String) = c.query_row(
                    "SELECT visibility, doc_type FROM documents WHERE id=?1",
                    [did],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                if visibility == "judicial_note" || doc_type == "judicial_note" {
                    continue;
                }
                if visibility == "restricted" {
                    let details: Value =
                        serde_json::from_str(ev["details"].as_str().unwrap_or("{}"))?;
                    let version_id = if entity == "document_version" {
                        ev["entity_id"].as_i64()
                    } else {
                        details["version_id"].as_i64()
                    };
                    let included = if let Some(vid) = version_id {
                        ids.contains(&vid)
                    } else {
                        included_documents.contains(&did)
                    };
                    if !included {
                        continue;
                    }
                }
            }
        }
        events.push(event_json(c, actor, ev, false)?);
    }
    Ok(events)
}

async fn case_history_handler(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            policy::require_case(c, &actor, id)?; // any actor who can see the case
            Ok(json!({ "events": case_history(c, &actor, id)? }))
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct AuditQuery {
    case_id: Option<i64>,
    user_id: Option<i64>,
    action: Option<String>,
    from: Option<String>,
    to: Option<String>,
    limit: Option<i64>,
}

/// Audit browser (`audit.view`). Case-scoped events of cases the actor cannot see are excluded.
async fn list(ctx: Ctx, Query(q): Query<AuditQuery>) -> JsonResult {
    ctx.actor.require(perm::AUDIT_VIEW)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            let from_utc = match optional(&q.from) {
                Some(d) => Some(crate::time::local_to_utc(&format!("{}T00:00", crate::time::parse_date(&d)?))?),
                None => None,
            };
            let to_utc = match optional(&q.to) {
                Some(d) => {
                    let d = crate::time::parse_date(&d)?;
                    let next = crate::time::parse_utc(&crate::time::local_to_utc(&format!("{d}T00:00"))?)?
                        .checked_add(time::Duration::days(1))
                        .map(crate::time::fmt_utc)
                        .ok_or_else(|| crate::error::AppError::validation("Invalid date range."))?;
                    Some(next)
                }
                None => None,
            };
            if from_utc.as_ref().zip(to_utc.as_ref()).is_some_and(|(f,t)| f>=t) {
                return Err(crate::error::AppError::validation("Invalid date range."));
            }
            let limit = q.limit.unwrap_or(500).clamp(1, 5000);
            // Old events may omit case_id; resolve their owning case before filtering.
            let scope = "COALESCE(a.case_id, CASE a.entity_type
                WHEN 'case' THEN a.entity_id
                WHEN 'document' THEN (SELECT case_id FROM documents WHERE id=a.entity_id)
                WHEN 'document_version' THEN (SELECT d.case_id FROM document_versions v JOIN documents d ON d.id=v.document_id WHERE v.id=a.entity_id)
                WHEN 'hearing' THEN (SELECT case_id FROM hearings WHERE id=a.entity_id)
                WHEN 'decision' THEN (SELECT case_id FROM decisions WHERE id=a.entity_id)
                WHEN 'dispatch' THEN (SELECT case_id FROM dispatches WHERE id=a.entity_id)
                WHEN 'task' THEN (SELECT case_id FROM tasks WHERE id=a.entity_id)
                WHEN 'intake' THEN (SELECT case_id FROM intakes WHERE id=a.entity_id) END)";
            let vis = policy::case_visible_sql(&actor, scope);
            let sql = format!(
                "SELECT a.id, a.at, a.user_id, u.display_name AS user_name, a.action, a.entity_type, a.entity_id,
                        a.case_id, a.summary, a.details, a.ip
                 FROM audit_events a LEFT JOIN users u ON u.id = a.user_id
                 WHERE ({scope} IS NULL OR {vis})
                   AND (?1 IS NULL OR {scope} = ?1)
                   AND (?2 IS NULL OR a.user_id = ?2)
                   AND (?3 IS NULL OR a.action LIKE ?3 || '%')
                   AND (?4 IS NULL OR a.at >= ?4)
                   AND (?5 IS NULL OR a.at < ?5)
                 ORDER BY a.id DESC LIMIT ?6"
            );
            let rows = query_json(c, &sql, params![q.case_id, q.user_id, optional(&q.action), from_utc, to_utc, limit])?;
            let mut events = Vec::with_capacity(rows.len());
            for ev in &rows {
                events.push(event_json(c, &actor, ev, true)?);
            }
            Ok(json!({ "events": events }))
        })
        .await?;
    Ok(Json(v))
}

async fn verify(ctx: Ctx) -> JsonResult {
    ctx.actor.require(perm::AUDIT_VIEW)?;
    let v = ctx
        .db
        .read(|c| {
            let (events, broken) = crate::audit::verify_chain(c)?;
            Ok(json!({ "events": events, "intact": broken.is_none(), "first_broken_id": broken }))
        })
        .await?;
    Ok(Json(v))
}
