//! Work queue: what the current user should do next, from records they can see.

use super::common::{JsonResult, query_json};
use crate::auth::Ctx;
use crate::policy::{self, perm};
use crate::state::AppState;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

pub fn routes() -> Router<AppState> {
    Router::new().route("/queue", get(queue))
}

async fn queue(ctx: Ctx) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            let mut items: Vec<Value> = Vec::new();
            if actor.has(perm::INTAKE_MANAGE) {
                for i in query_json(
                    c,
                    "SELECT id, reference, status, sender_name, received_date FROM intakes
                     WHERE case_id IS NULL AND parent_intake_id IS NULL
                       AND status IN ('received','needs_information','ready_for_registration') ORDER BY received_date, id",
                    [],
                )? {
                    let (msg, prio) = match i["status"].as_str() {
                        Some("received") => ("Check the filing and mark it ready, or request missing information.", 2),
                        Some("needs_information") => ("Waiting for missing information from the sender.", 3),
                        _ => ("Checked — register it as a case or link it to an existing case.", 1),
                    };
                    items.push(json!({
                        "kind": "intake",
                        "title": format!("{} — {}", i["reference"].as_str().unwrap_or_default(), i["sender_name"].as_str().unwrap_or_default()),
                        "message": msg,
                        "link": format!("/intakes/{}", i["id"]),
                        "due_date": Value::Null,
                        "priority": prio,
                    }));
                }
            }
            for t in query_json(
                c,
                &format!(
                    "SELECT t.id, t.title, t.due_date, t.case_id, t.intake_id, cs.number FROM tasks t LEFT JOIN cases cs ON cs.id = t.case_id
                     WHERE t.status = 'open' AND t.assignee_user_id = ?1 AND (t.case_id IS NULL OR {})
                     ORDER BY t.due_date IS NULL, t.due_date, t.id",
                    policy::case_visible_sql(&actor, "t.case_id")
                ),
                [actor.user_id],
            )? {
                let link = match (t["case_id"].as_i64(), t["intake_id"].as_i64()) {
                    (Some(cid), _) => format!("/cases/{cid}?tab=tasks"),
                    (None, Some(iid)) => format!("/intakes/{iid}"),
                    _ => "/".into(),
                };
                items.push(json!({
                    "kind": "task",
                    "title": t["title"],
                    "message": "Task assigned to you.",
                    "link": link,
                    "case_number": t["number"],
                    "due_date": t["due_date"],
                    "priority": 2,
                }));
            }
            // Next steps on cases where the user is actively assigned.
            for cs in query_json(
                c,
                "SELECT DISTINCT cs.id, cs.number, cs.title FROM case_assignments a JOIN cases cs ON cs.id = a.case_id
                 WHERE a.user_id = ?1 AND a.end_at IS NULL AND cs.status <> 'closed' ORDER BY cs.id LIMIT 200",
                [actor.user_id],
            )? {
                let cid = cs["id"].as_i64().unwrap_or_default();
                for a in super::cases::next_actions(c, &actor, cid)? {
                    if a["code"] == "task" {
                        continue; // tasks are listed individually above
                    }
                    items.push(json!({
                        "kind": "case",
                        "title": format!("{} — {}", cs["number"].as_str().unwrap_or_default(), cs["title"].as_str().unwrap_or_default()),
                        "message": a["message"],
                        "link": a["link"],
                        "case_number": cs["number"],
                        "priority": 2,
                    }));
                }
            }
            let today = crate::time::today_local();
            let from = crate::time::local_to_utc(&format!("{today}T00:00"))?;
            let to = crate::time::add_minutes(&from, 24 * 60)?;
            for h in query_json(
                c,
                &format!(
                    "SELECT h.id, h.case_id, h.starts_at, cs.number, r.name AS room FROM hearings h JOIN cases cs ON cs.id = h.case_id
                     LEFT JOIN rooms r ON r.id = h.room_id
                     WHERE h.status = 'scheduled' AND h.starts_at >= ?1 AND h.starts_at < ?2 AND {}
                       AND (h.judge_user_id = ?3 OR EXISTS (SELECT 1 FROM case_assignments a WHERE a.case_id = h.case_id AND a.user_id = ?3 AND a.end_at IS NULL))
                     ORDER BY h.starts_at",
                    policy::case_visible_sql(&actor, "h.case_id")
                ),
                rusqlite::params![from, to, actor.user_id],
            )? {
                items.push(json!({
                    "kind": "hearing",
                    "title": format!("{} at {}", h["number"].as_str().unwrap_or_default(), crate::time::utc_to_local(h["starts_at"].as_str().unwrap_or_default())),
                    "message": format!("Hearing today{}.", h["room"].as_str().map(|r| format!(" in {r}")).unwrap_or_default()),
                    "link": format!("/cases/{}?tab=hearings", h["case_id"]),
                    "case_number": h["number"],
                    "priority": 1,
                }));
            }
            Ok(json!({ "items": items }))
        })
        .await?;
    Ok(Json(v))
}
