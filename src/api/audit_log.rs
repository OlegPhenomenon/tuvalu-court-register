//! C15 history & audit. Every actor who can see a case may read its history; events touching a
//! document the actor cannot see are omitted or replaced with a neutral decision event.
//! The global audit browser and chain verification require `audit.view`.

use super::common::{JsonResult, optional, query_json};
use crate::auth::{Actor, Ctx};
use crate::error::AppResult;
use crate::policy::{self, perm};
use crate::state::AppState;
use axum::extract::{Path, Query};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/cases/{id}/history", get(case_history_handler))
        .route("/audit", get(list))
        .route("/audit/verify", get(verify))
}

/// True when the event concerns a document the actor may not see.
fn touches_hidden_document(
    c: &Connection,
    actor: &Actor,
    entity_type: &str,
    entity_id: Option<i64>,
    action: &str,
) -> AppResult<bool> {
    if !matches!(entity_type, "document" | "document_version" | "decision")
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
        } else if entity_type == "decision" {
            "(SELECT document_id FROM decisions WHERE id=?1)"
        } else {
            "?1"
        }
    );
    Ok(c.query_row(&sql, [doc_id], |r| r.get::<_, i64>(0))? == 0)
}

/// The first later draft edit records the version that was bound at this event's time.
/// Resolve old events without changing the append-only audit trail.
fn decision_event_version(c: &Connection, ev: &Value) -> AppResult<Option<i64>> {
    let earlier: Option<i64> = c
        .query_row(
            "SELECT json_extract(details,'$.before.document_version_id') FROM audit_events
        WHERE entity_type='decision' AND entity_id=?1 AND action='decision.updated' AND id>=?2
          AND json_extract(details,'$.before.document_version_id') IS NOT NULL ORDER BY id LIMIT 1",
            params![ev["entity_id"].as_i64(), ev["id"].as_i64()],
            |r| r.get(0),
        )
        .optional()?;
    if earlier.is_some() {
        return Ok(earlier);
    }
    Ok(c.query_row(
        "SELECT document_version_id FROM decisions WHERE id=?1",
        [ev["entity_id"].as_i64()],
        |r| r.get(0),
    )
    .optional()?)
}

/// Gather hidden-document metadata from arbitrary event snapshots. Audit storage remains intact.
fn hidden_detail_metadata(
    c: &Connection,
    actor: &Actor,
    value: &Value,
    secrets: &mut Vec<String>,
) -> AppResult<()> {
    match value {
        Value::Object(fields) => {
            let mut documents = Vec::new();
            if let Some(id) = fields.get("document_id").and_then(Value::as_i64) {
                documents.push(id);
            }
            let mut versions = Vec::new();
            if let Some(id) = fields.get("document_version_id").and_then(Value::as_i64) {
                versions.push(id);
            }
            for key in ["document_version_ids", "version_ids"] {
                if let Some(ids) = fields.get(key).and_then(Value::as_array) {
                    versions.extend(ids.iter().filter_map(Value::as_i64));
                }
            }
            let mut hidden = false;
            for id in versions {
                if policy::require_version(c, actor, id).is_err() {
                    hidden = true;
                    for row in query_json(
                        c,
                        "SELECT d.title, v.filename, v.sha256 FROM document_versions v JOIN documents d ON d.id=v.document_id WHERE v.id=?1",
                        [id],
                    )? {
                        for key in ["title", "filename", "sha256"] {
                            if let Some(text) = row[key].as_str().filter(|s| !s.is_empty()) {
                                secrets.push(text.to_string());
                            }
                        }
                    }
                }
            }
            for id in documents {
                if policy::require_document(c, actor, id).is_err() {
                    hidden = true;
                    for row in query_json(
                        c,
                        "SELECT d.title, v.filename, v.sha256 FROM documents d LEFT JOIN document_versions v ON v.document_id=d.id WHERE d.id=?1",
                        [id],
                    )? {
                        for key in ["title", "filename", "sha256"] {
                            if let Some(text) = row[key].as_str().filter(|s| !s.is_empty()) {
                                secrets.push(text.to_string());
                            }
                        }
                    }
                }
            }
            if hidden {
                // Snapshot titles can differ from the document's current title.
                for key in ["title", "document_title", "filename", "sha256"] {
                    if let Some(text) = fields
                        .get(key)
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        secrets.push(text.to_string());
                    }
                }
            }
            for child in fields.values() {
                hidden_detail_metadata(c, actor, child, secrets)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                hidden_detail_metadata(c, actor, item, secrets)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Redact metadata wherever nested, including inventory text in sibling before/after bodies.
fn redact_details(c: &Connection, actor: &Actor, details: &mut Value) -> AppResult<()> {
    let mut secrets = Vec::new();
    hidden_detail_metadata(c, actor, details, &mut secrets)?;
    secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    secrets.dedup();
    fn scrub(value: &mut Value, secrets: &[String]) {
        match value {
            Value::Object(fields) => {
                for child in fields.values_mut() {
                    scrub(child, secrets);
                }
            }
            Value::Array(items) => {
                for item in items {
                    scrub(item, secrets);
                }
            }
            Value::String(text) => {
                *text = text
                    .lines()
                    .map(|line| {
                        if line.starts_with("- ")
                            && secrets.iter().any(|secret| line.contains(secret))
                        {
                            "- Restricted document".to_string()
                        } else {
                            let mut line = line.to_string();
                            for secret in secrets {
                                line = line.replace(secret, "Restricted document");
                            }
                            line
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            _ => {}
        }
    }
    if !secrets.is_empty() {
        scrub(details, &secrets);
    }
    Ok(())
}

/// Shape one audit row for the case history, omitting events the actor may not see.
fn event_json(
    c: &Connection,
    actor: &Actor,
    ev: &Value,
    with_details: bool,
) -> AppResult<Option<Value>> {
    let entity_type = ev["entity_type"].as_str().unwrap_or_default();
    let entity_id = ev["entity_id"].as_i64();
    let action = ev["action"].as_str().unwrap_or_default();
    let mut details: Value = serde_json::from_str(ev["details"].as_str().unwrap_or("{}"))?;
    if action == "case.closed" {
        super::cases::redact_closing_basis(c, actor, &mut details)?;
    }
    let hidden_relation = if action == "case.related" {
        if let Some(other) = details["related_case_id"].as_i64() {
            policy::require_case(c, actor, other).is_err()
        } else {
            // Old events have no counterpart id: omit if any related case is hidden.
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
    if entity_type == "party"
        && policy::require_party(c, actor, entity_id.unwrap_or_default()).is_err()
    {
        return Ok(None);
    }
    if entity_type == "intake"
        && super::intake::require_intake(c, actor, entity_id.unwrap_or_default()).is_err()
    {
        return Ok(None);
    }
    if entity_type == "dispatch" {
        let iid: Option<i64> = c
            .query_row(
                "SELECT intake_id FROM dispatches WHERE id=?1",
                [entity_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if let Some(iid) = iid {
            if super::intake::require_intake(c, actor, iid).is_err() {
                return Ok(None);
            }
        }
    }
    let hidden_document = touches_hidden_document(c, actor, entity_type, entity_id, action)?
        || (entity_type == "decision"
            && decision_event_version(c, ev)?
                .is_none_or(|v| policy::require_version(c, actor, v).is_err()));
    if hidden_relation || (hidden_document && entity_type != "decision") {
        return Ok(None);
    }
    let mut out = json!({
        "id": ev["id"],
        "at": ev["at"],
        "at_local": crate::time::utc_to_local(ev["at"].as_str().unwrap_or_default()),
        "user_id": ev["user_id"],
        "user_name": ev["user_name"],
        "action": ev["action"],
        "summary": if hidden_document { json!("Restricted document decision event") } else { ev["summary"].clone() },
    });
    if with_details {
        redact_details(c, actor, &mut details)?;
        out["entity_type"] = ev["entity_type"].clone();
        out["entity_id"] = ev["entity_id"].clone();
        out["case_id"] = ev["case_id"].clone();
        out["ip"] = ev["ip"].clone();
        out["details"] = if hidden_document {
            json!({"document_version_id":decision_event_version(c,ev)?,"restricted":true,"document_title":"Restricted document"})
        } else {
            details
        };
    }
    Ok(Some(out))
}

/// Audit events of one case, oldest first, filtered for the given actor.
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
            OR (a.entity_type = 'intake' AND a.entity_id IN (SELECT id FROM intakes WHERE case_id=?1))
            OR (a.entity_type = 'document' AND a.entity_id IN (SELECT d.id FROM documents d WHERE d.case_id = ?1))
            OR (a.entity_type = 'document_version' AND a.entity_id IN (SELECT v.id FROM document_versions v JOIN documents d ON d.id=v.document_id WHERE d.case_id=?1))
            OR (a.entity_type = 'hearing' AND a.entity_id IN (SELECT h.id FROM hearings h WHERE h.case_id = ?1))
            OR (a.entity_type = 'decision' AND a.entity_id IN (SELECT d.id FROM decisions d WHERE d.case_id = ?1))
            OR (a.entity_type = 'dispatch' AND a.entity_id IN (SELECT d.id FROM dispatches d WHERE d.case_id = ?1 OR d.intake_id IN (SELECT id FROM intakes WHERE case_id=?1)))
            OR (a.entity_type = 'task' AND a.entity_id IN (SELECT t.id FROM tasks t WHERE t.case_id = ?1))
         ORDER BY a.id",
        [case_id],
    )?;
    let mut events = Vec::with_capacity(rows.len());
    for ev in &rows {
        let Some(mut event) = event_json(c, actor, ev, false)? else {
            continue;
        };
        if let Some(ids) = package_ids {
            let entity = ev["entity_type"].as_str().unwrap_or_default();
            let eid = ev["entity_id"].as_i64().unwrap_or_default();
            let details: Value = serde_json::from_str(ev["details"].as_str().unwrap_or("{}"))?;
            let material = match entity {
                "document" => Some((eid, details["version_id"].as_i64())),
                "document_version" => Some((
                    c.query_row(
                        "SELECT document_id FROM document_versions WHERE id=?1",
                        [eid],
                        |r| r.get(0),
                    )?,
                    Some(eid),
                )),
                "decision" => {
                    let Some(vid) = decision_event_version(c, ev)? else {
                        continue;
                    };
                    Some((
                        c.query_row(
                            "SELECT document_id FROM document_versions WHERE id=?1",
                            [vid],
                            |r| r.get(0),
                        )?,
                        Some(vid),
                    ))
                }
                _ => None,
            };
            if let Some((did, vid)) = material {
                if !included_documents.contains(&did) || vid.is_some_and(|v| !ids.contains(&v)) {
                    continue;
                }
                let excluded: bool=c.query_row("SELECT visibility='judicial_note' OR doc_type='judicial_note' FROM documents WHERE id=?1",[did],|r|r.get(0))?;
                if excluded {
                    continue;
                }
                event["summary"] = json!(if entity == "decision" {
                    "Selected decision event"
                } else {
                    "Selected document event"
                });
            }
            // A dispatch event can contain the titles of any of its attachments.
            if entity == "dispatch" {
                let attached = query_json(
                    c,
                    "SELECT document_version_id FROM dispatch_items WHERE dispatch_id=?1",
                    [eid],
                )?;
                if attached
                    .iter()
                    .any(|v| !ids.contains(&v["document_version_id"].as_i64().unwrap_or_default()))
                {
                    continue;
                }
            }
        }
        events.push(event);
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
                WHEN 'dispatch' THEN (SELECT COALESCE(dp.case_id,(SELECT case_id FROM intakes WHERE id=dp.intake_id)) FROM dispatches dp WHERE id=a.entity_id)
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
                if let Some(event) = event_json(c, &actor, ev, true)? {
                    events.push(event);
                }
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
