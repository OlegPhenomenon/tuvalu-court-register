//! C14 reports: period summary, drill-down to the exact records behind every number, and CSV.
//! Every count is computed over cases visible to the actor (`policy::case_visible_sql`).
//! Closure/reopen metrics use the real event dates (`case_status_history.effective_date`),
//! not today's status; case age is never presented as a legal breach.

use super::common::{JsonResult, optional, query_json};
use crate::auth::{Actor, Ctx};
use crate::error::{AppError, AppResult};
use crate::policy::{self, perm};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Path, Query};
use axum::http::{HeaderValue, header};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/reports/summary", get(summary))
        .route("/reports/{key}/items", get(items))
        .route("/reports/{key}/csv", get(csv_download))
}

const LABELS: &[(&str, &str)] = &[
    ("new_cases", "Cases registered in the period"),
    ("closed_cases", "Cases closed in the period"),
    ("reopened_cases", "Cases reopened in the period"),
    ("open_as_of", "Cases open as of the date"),
    ("without_next_step", "Open cases with no next step"),
    (
        "upcoming_hearings",
        "Hearings scheduled in the next 14 days",
    ),
    ("undelivered_notices", "Dispatches not yet delivered"),
];

fn known(key: &str) -> bool {
    LABELS.iter().any(|(k, _)| *k == key)
}

fn label(key: &str) -> &'static str {
    LABELS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, l)| *l)
        .unwrap_or("Report")
}

#[derive(Deserialize)]
struct PeriodQuery {
    from: Option<String>,
    to: Option<String>,
    as_of: Option<String>,
}

struct Period {
    from: String,
    to: String,
    as_of: String,
    /// `as_of` 00:00 court-local and 14 days later, as UTC bounds for hearing instants.
    hearings_from: String,
    hearings_to: String,
}

fn period(q: &PeriodQuery) -> AppResult<Period> {
    let today = crate::time::today_local();
    let from = match optional(&q.from) {
        Some(d) => crate::time::parse_date(&d)?,
        None => format!("{}-01", &today[..7]),
    };
    let to = match optional(&q.to) {
        Some(d) => crate::time::parse_date(&d)?,
        None => today.clone(),
    };
    let as_of = match optional(&q.as_of) {
        Some(d) => crate::time::parse_date(&d)?,
        None => today,
    };
    if from > to {
        return Err(AppError::validation(
            "The start of the period must not be after its end.",
        ));
    }
    let hearings_from = crate::time::local_to_utc(&format!("{as_of}T00:00"))?;
    let end = crate::time::parse_utc(&hearings_from)?
        .checked_add(time::Duration::days(14))
        .map(crate::time::fmt_utc)
        .ok_or_else(|| AppError::validation("Invalid date."))?;
    Ok(Period {
        from,
        to,
        as_of,
        hearings_from,
        hearings_to: end,
    })
}

fn col(key: &str, header: &str) -> Value {
    json!({ "key": key, "header": header })
}

fn case_link(id: &Value) -> Value {
    json!(format!("/cases/{}", id.as_i64().unwrap_or_default()))
}

/// Turn raw case rows into report rows (fixed columns + link).
fn case_rows(rows: &[Value], extra: &[(&str, &str)]) -> (Vec<Value>, Vec<Value>) {
    let mut cols = vec![
        col("number", "Number"),
        col("title", "Title"),
        col("category", "Category"),
        col("status", "Status"),
        col("registered_date", "Registered"),
    ];
    for (k, h) in extra {
        cols.push(col(k, h));
    }
    let out = rows
        .iter()
        .map(|r| {
            let mut o = json!({
                "id": r["id"],
                "number": r["number"],
                "title": r["title"],
                "category": r["category"],
                "status": r["status"],
                "registered_date": r["registered_date"],
                "link": case_link(&r["id"]),
            });
            for (k, _) in extra {
                o[*k] = r[*k].clone();
            }
            o
        })
        .collect();
    (cols, out)
}

/// The records behind a metric. Returns `(columns, rows)`; the count is `rows.len()`.
fn fetch_items(c: &Connection, actor: &Actor, key: &str, p: &Period) -> AppResult<(Vec<Value>, Vec<Value>)> {
    let (mut columns, mut rows) = raw_items(c, actor, key, p)?;
    for row in &mut rows {
        for (field, kind) in [("category", "case_category"), ("closure_basis", "closure_basis"), ("hearing_type", "hearing_type"), ("method", "dispatch_method")] {
            if let Some(code) = row[field].as_str() {
                row[format!("{field}_label")] = json!(super::common::ref_label(c, kind, code)?);
            }
        }
        if let Some(code) = row["kind"].as_str() {
            let label = match code {
                "notice" => "Notice",
                "copies" => "Copy package",
                "information_request" => "Information request",
                other => other,
            };
            row["kind_label"] = json!(label);
        }
        for field in ["status", "state_as_of"] {
            if let Some(code) = row[field].as_str() {
                let label = code.replace('_', " ");
                row[format!("{field}_label")] = json!(format!("{}{}", label[..1].to_uppercase(), &label[1..]));
            }
        }
    }
    for column in &mut columns {
        if let Some(key) = column["key"].as_str()
            && matches!(key, "category" | "status" | "closure_basis" | "state_as_of" | "hearing_type" | "kind" | "method") {
            column["key"] = json!(format!("{key}_label"));
        }
    }
    Ok((columns, rows))
}

fn raw_items(
    c: &Connection,
    actor: &Actor,
    key: &str,
    p: &Period,
) -> AppResult<(Vec<Value>, Vec<Value>)> {
    let vis = || policy::case_visible_sql(actor, "c.id");
    match key {
        "new_cases" => {
            let sql = format!(
                "SELECT c.id, c.number, c.title, c.category, c.status, c.registered_date
                 FROM cases c WHERE c.registered_date BETWEEN ?1 AND ?2 AND {} ORDER BY c.registered_date, c.number",
                vis()
            );
            Ok(case_rows(&query_json(c, &sql, params![p.from, p.to])?, &[]))
        }
        "closed_cases" => {
            let sql = format!(
                "SELECT c.id, c.number, c.title, c.category, c.status, c.registered_date,
                        MAX(h.effective_date) AS event_date, h.basis AS closure_basis
                 FROM case_status_history h JOIN cases c ON c.id = h.case_id
                 WHERE h.to_status = 'closed' AND h.effective_date BETWEEN ?1 AND ?2 AND {}
                 GROUP BY c.id ORDER BY event_date, c.number",
                vis()
            );
            Ok(case_rows(
                &query_json(c, &sql, params![p.from, p.to])?,
                &[
                    ("event_date", "Closed on"),
                    ("closure_basis", "Closure basis"),
                ],
            ))
        }
        "reopened_cases" => {
            let sql = format!(
                "SELECT c.id, c.number, c.title, c.category, c.status, c.registered_date,
                        MAX(h.effective_date) AS event_date
                 FROM case_status_history h JOIN cases c ON c.id = h.case_id
                 WHERE h.to_status = 'reopened' AND h.effective_date BETWEEN ?1 AND ?2 AND {}
                 GROUP BY c.id ORDER BY event_date, c.number",
                vis()
            );
            Ok(case_rows(
                &query_json(c, &sql, params![p.from, p.to])?,
                &[("event_date", "Reopened on")],
            ))
        }
        "open_as_of" => {
            let state = "(SELECT h.to_status FROM case_status_history h WHERE h.case_id = c.id AND h.effective_date <= ?1
                          ORDER BY h.effective_date DESC, h.id DESC LIMIT 1)";
            let sql = format!(
                "SELECT * FROM (
                   SELECT c.id, c.number, c.title, c.category, c.status, c.registered_date, {state} AS state_as_of
                   FROM cases c WHERE {vis}
                 ) WHERE state_as_of IS NOT NULL AND state_as_of <> 'closed' ORDER BY number",
                vis = vis()
            );
            Ok(case_rows(
                &query_json(c, &sql, params![p.as_of])?,
                &[("state_as_of", "State on the date")],
            ))
        }
        "without_next_step" => {
            let sql = format!(
                "SELECT c.id, c.number, c.title, c.category, c.status, c.registered_date
                 FROM cases c
                 WHERE c.status <> 'closed' AND {}
                 ORDER BY c.registered_date, c.number",
                vis()
            );
            let mut rows = Vec::new();
            for row in query_json(c, &sql, [])? {
                if !super::cases::has_next_step(c, actor, row["id"].as_i64().unwrap_or_default())? {
                    rows.push(row);
                }
            }
            Ok(case_rows(&rows, &[]))
        }
        "upcoming_hearings" => {
            let sql = format!(
                "SELECT h.id, h.starts_at, h.ends_at, h.hearing_type, h.status, r.name AS room,
                        ju.display_name AS judge, c.id AS case_id, c.number AS case_number, c.title AS case_title
                 FROM hearings h JOIN cases c ON c.id = h.case_id
                 LEFT JOIN rooms r ON r.id = h.room_id LEFT JOIN users ju ON ju.id = h.judge_user_id
                 WHERE h.status = 'scheduled' AND h.starts_at >= ?1 AND h.starts_at < ?2 AND {}
                 ORDER BY h.starts_at",
                policy::case_visible_sql(actor, "c.id")
            );
            let rows = query_json(c, &sql, params![p.hearings_from, p.hearings_to])?;
            let cols = vec![
                col("starts_local", "Starts"),
                col("ends_local", "Ends"),
                col("case_number", "Case"),
                col("case_title", "Case title"),
                col("hearing_type", "Type"),
                col("room", "Room"),
                col("judge", "Judge"),
            ];
            let out = rows
                .iter()
                .map(|r| {
                    json!({
                        "id": r["id"], "case_id": r["case_id"], "starts_at": r["starts_at"], "ends_at": r["ends_at"],
                        "starts_local": crate::time::utc_to_local(r["starts_at"].as_str().unwrap_or_default()),
                        "ends_local": crate::time::utc_to_local(r["ends_at"].as_str().unwrap_or_default()),
                        "case_number": r["case_number"],
                        "case_title": r["case_title"],
                        "hearing_type": r["hearing_type"], "status": r["status"],
                        "room": r["room"],
                        "judge": r["judge"],
                        "link": format!("/cases/{}?tab=hearings&hearing={}", r["case_id"].as_i64().unwrap_or_default(), r["id"]),
                    })
                })
                .collect();
            Ok((cols, out))
        }
        "undelivered_notices" => {
            let sql = format!(
                "SELECT d.id, d.kind, d.recipient_name, d.method, d.subject, d.status, d.prepared_at,
                        d.case_id, d.intake_id, c.number AS case_number
                 FROM dispatches d LEFT JOIN cases c ON c.id = d.case_id
                 WHERE (d.status IN ('draft','queued','failed')
                        OR (d.status = 'sent' AND NOT EXISTS (SELECT 1 FROM delivery_confirmations dc
                              WHERE dc.dispatch_id = d.id AND dc.kind = 'human_handover')))
                   AND d.case_id IS NOT NULL AND {}
                 ORDER BY d.id",
                vis()
            );
            let rows = query_json(c, &sql, [])?;
            let cols = vec![
                col("case_number", "Case"),
                col("kind", "Kind"),
                col("recipient_name", "Recipient"),
                col("method", "Method"),
                col("subject", "Subject"),
                col("status", "Status"),
                col("prepared_at", "Prepared"),
            ];
            let out = rows
                .iter()
                .map(|r| {
                    let link = match (r["case_id"].as_i64(), r["intake_id"].as_i64()) {
                        (Some(cid), _) => format!("/cases/{cid}?tab=dispatch&dispatch={}", r["id"]),
                        (None, Some(iid)) => format!("/intakes/{iid}"),
                        _ => "/".to_string(),
                    };
                    json!({
                        "id": r["id"], "case_id": r["case_id"],
                        "case_number": r["case_number"],
                        "kind": r["kind"],
                        "recipient_name": r["recipient_name"],
                        "method": r["method"],
                        "subject": r["subject"],
                        "status": r["status"],
                        "prepared_at": r["prepared_at"],
                        "link": link,
                    })
                })
                .collect();
            Ok((cols, out))
        }
        _ => Err(AppError::not_found()),
    }
}

/// Per-person open workload, counted only over cases the actor can see.
fn workload(c: &Connection, actor: &Actor) -> AppResult<Vec<Value>> {
    let case_vis = policy::case_visible_sql(actor, "c2.id");
    let task_vis = policy::case_visible_sql(actor, "t.case_id");
    query_json(
        c,
        &format!(
            "SELECT u.id AS user_id, u.display_name,
                    (SELECT COUNT(DISTINCT a.case_id) FROM case_assignments a JOIN cases c2 ON c2.id = a.case_id
                      WHERE a.user_id = u.id AND a.end_at IS NULL AND c2.status <> 'closed' AND {case_vis}) AS open_cases,
                    (SELECT COUNT(*) FROM tasks t WHERE t.assignee_user_id = u.id AND t.status = 'open'
                      AND t.case_id IS NOT NULL AND {task_vis}) AS open_tasks
             FROM users u WHERE u.active = 1 ORDER BY u.display_name"
        ),
        [],
    )
}

async fn summary(ctx: Ctx, Query(q): Query<PeriodQuery>) -> JsonResult {
    ctx.actor.require(perm::REPORT_VIEW)?;
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            let p = period(&q)?;
            let mut metrics = Vec::new();
            for (key, _) in LABELS {
                let (_, rows) = fetch_items(c, &actor, key, &p)?;
                metrics.push(json!({
                    "key": key,
                    "label": label(key),
                    "count": rows.len(),
                    "drilldown": format!("/api/reports/{key}/items?from={}&to={}&as_of={}", p.from, p.to, p.as_of),
                }));
            }
            Ok(json!({
                "period": { "from": p.from, "to": p.to, "as_of": p.as_of },
                "metrics": metrics,
                "workload": workload(c, &actor)?,
            }))
        })
        .await?;
    Ok(Json(v))
}

async fn items(ctx: Ctx, Path(key): Path<String>, Query(q): Query<PeriodQuery>) -> JsonResult {
    ctx.actor.require(perm::REPORT_VIEW)?;
    if !known(&key) {
        return Err(AppError::not_found());
    }
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            let p = period(&q)?;
            let (columns, rows) = fetch_items(c, &actor, &key, &p)?;
            Ok(json!({ "columns": columns, "rows": rows }))
        })
        .await?;
    Ok(Json(v))
}

/// Spreadsheet formula-injection guard: a cell whose first character could start a
/// formula (`=`, `+`, `-`, `@`) or inject a break (tab/CR) is prefixed with `'`.
fn csv_cell(value: &Value) -> String {
    let s = match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    match s.chars().next() {
        Some('=' | '+' | '-' | '@' | '\t' | '\r') => format!("'{s}"),
        _ => s,
    }
}

fn csv_response(key: &str, as_of: &str, columns: &[Value], rows: &[Value]) -> AppResult<Response> {
    let mut w = csv::Writer::from_writer(Vec::new());
    w.write_record(
        columns
            .iter()
            .map(|c| c["header"].as_str().unwrap_or_default()),
    )
    .map_err(|e| AppError::internal(format!("csv: {e}")))?;
    for r in rows {
        w.write_record(
            columns
                .iter()
                .map(|c| csv_cell(&r[c["key"].as_str().unwrap_or_default()])),
        )
        .map_err(|e| AppError::internal(format!("csv: {e}")))?;
    }
    let bytes = w
        .into_inner()
        .map_err(|e| AppError::internal(format!("csv: {e}")))?;
    let mut res = Response::new(Body::from(bytes));
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    let filename = crate::storage::sanitize_filename(&format!("{key}-{as_of}.csv"));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, private"),
    );
    Ok(res)
}

async fn csv_download(
    ctx: Ctx,
    Path(key): Path<String>,
    Query(q): Query<PeriodQuery>,
) -> AppResult<Response> {
    ctx.actor.require(perm::REPORT_VIEW)?;
    if !known(&key) {
        return Err(AppError::not_found());
    }
    let actor = ctx.actor;
    let res = ctx
        .db
        .read(move |c| {
            let p = period(&q)?;
            let (columns, rows) = fetch_items(c, &actor, &key, &p)?;
            csv_response(&key, &p.as_of, &columns, &rows)
        })
        .await?;
    Ok(res)
}
