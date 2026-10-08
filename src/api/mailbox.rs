//! Local message viewer. Attachment downloads use the document-version endpoint, which
//! independently checks current document access on every request.
use super::common::{JsonResult, query_json, query_one_json};
use super::dispatch::dispatch_visible_sql;
use crate::auth::{Actor, Ctx};
use crate::error::AppResult;
use crate::state::AppState;
use axum::extract::{Path, Query};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/mailbox", get(list))
        .route("/mailbox/{id}", get(detail))
}

const MAILBOX_SQL: &str = "SELECT m.id, m.dispatch_id, c.number AS case_number, m.attempt_no,
    m.to_address, m.subject, m.body, m.attachments, m.delivered_at
    FROM mailbox m JOIN dispatches d ON d.id = m.dispatch_id
    LEFT JOIN cases c ON c.id = d.case_id LEFT JOIN intakes i ON i.id = d.intake_id";

fn message(c: &rusqlite::Connection, actor: &Actor, mut value: Value) -> AppResult<Value> {
    value["attachments"] = serde_json::from_str(value["attachments"].as_str().unwrap_or("[]"))?;
    super::dispatch::redact_items(c, actor, &mut value, "attachments")?;
    Ok(value)
}

#[derive(Deserialize)]
struct ListQuery {
    dispatch_id: Option<i64>,
}

async fn list(ctx: Ctx, Query(q): Query<ListQuery>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx.db.read(move |c| {
        let sql = format!("{MAILBOX_SQL} WHERE {} AND (?1 IS NULL OR m.dispatch_id = ?1) ORDER BY m.delivered_at DESC, m.id DESC", dispatch_visible_sql(&actor));
        let items = query_json(c, &sql, params![q.dispatch_id])?.into_iter().map(|v| message(c, &actor, v)).collect::<AppResult<Vec<_>>>()?;
        Ok(json!({"items": items}))
    }).await?;
    Ok(Json(v))
}

async fn detail(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            let sql = format!(
                "{MAILBOX_SQL} WHERE m.id = ?1 AND {}",
                dispatch_visible_sql(&actor)
            );
            message(c, &actor, query_one_json(c, &sql, [id])?)
        })
        .await?;
    Ok(Json(v))
}
