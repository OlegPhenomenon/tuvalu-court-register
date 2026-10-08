//! C14 global search: cases (number, title, party names), document titles/filenames and intakes.
//! Everything passes through the visibility policy — a restricted case does not appear even when
//! the exact number is typed.

use super::common::{JsonResult, optional, query_json};
use crate::auth::Ctx;
use crate::policy;
use crate::state::AppState;
use axum::extract::Query;
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};

pub fn routes() -> Router<AppState> {
    Router::new().route("/search", get(search))
}

const LIMIT: usize = 20;

#[derive(Deserialize)]
struct SearchQuery {
    q: Option<String>,
}

async fn search(ctx: Ctx, Query(q): Query<SearchQuery>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            let q = optional(&q.q).unwrap_or_default();
            if q.chars().count() < 2 {
                return Ok(json!({ "cases": [], "documents": [], "intakes": [] }));
            }
            let like = format!("%{q}%");

            let cases_sql = format!(
                "SELECT c.id, c.number, c.title, c.status FROM cases c
                 WHERE {}
                   AND (c.number LIKE ?1 OR c.legacy_number LIKE ?1 OR c.title LIKE ?1
                        OR EXISTS (SELECT 1 FROM case_participations cp JOIN parties p ON p.id = cp.party_id
                                   WHERE cp.case_id = c.id AND p.name LIKE ?1))
                 ORDER BY c.registered_date DESC, c.id DESC LIMIT {LIMIT}",
                policy::case_visible_sql(&actor, "c.id")
            );
            let cases: Vec<Value> = query_json(c, &cases_sql, params![like])?
                .iter()
                .map(|r| {
                    json!({
                        "id": r["id"], "number": r["number"], "title": r["title"], "status": r["status"],
                        "link": format!("/cases/{}", r["id"].as_i64().unwrap_or_default()),
                    })
                })
                .collect();

            let docs_sql = format!(
                "SELECT d.id, d.title, d.case_id, d.intake_id, cs.number AS case_number
                 FROM documents d LEFT JOIN cases cs ON cs.id = d.case_id
                 WHERE {}
                   AND (d.title LIKE ?1
                        OR EXISTS (SELECT 1 FROM document_versions v WHERE v.document_id = d.id AND v.filename LIKE ?1))
                 ORDER BY d.id DESC LIMIT {LIMIT}",
                policy::document_visible_sql(&actor, "d")
            );
            let documents: Vec<Value> = query_json(c, &docs_sql, params![like])?
                .iter()
                .map(|r| {
                    let link = match r["case_id"].as_i64() {
                        Some(cid) => format!("/cases/{cid}?tab=documents"),
                        None => format!("/intakes/{}", r["intake_id"].as_i64().unwrap_or_default()),
                    };
                    json!({
                        "id": r["id"], "title": r["title"], "case_id": r["case_id"], "case_number": r["case_number"],
                        "link": link,
                    })
                })
                .collect();

            let intakes_sql = format!(
                "SELECT i.id, i.reference, i.sender_name FROM intakes i
                 WHERE {} AND (i.reference LIKE ?1 OR i.sender_name LIKE ?1 OR i.description LIKE ?1)
                 ORDER BY i.id DESC LIMIT {LIMIT}",
                policy::intake_visible_sql(&actor, "i")
            );
            let intakes: Vec<Value> = query_json(c, &intakes_sql, params![like])?
                .iter()
                .map(|r| {
                    json!({
                        "id": r["id"], "reference": r["reference"], "sender_name": r["sender_name"],
                        "link": format!("/intakes/{}", r["id"].as_i64().unwrap_or_default()),
                    })
                })
                .collect();

            Ok(json!({ "cases": cases, "documents": documents, "intakes": intakes }))
        })
        .await?;
    Ok(Json(v))
}
