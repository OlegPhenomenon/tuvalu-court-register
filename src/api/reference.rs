//! Reference lists for forms: categories, channels, islands, rooms, registries, staff.

use super::common::{JsonResult, query_json};
use crate::auth::Ctx;
use crate::state::AppState;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Map, Value, json};

pub fn routes() -> Router<AppState> {
    Router::new().route("/ref", get(reference))
}

async fn reference(ctx: Ctx) -> JsonResult {
    let v = ctx
        .db
        .read(|c| {
            let mut lists = Map::new();
            for item in query_json(c, "SELECT kind, code, label FROM ref_items WHERE active = 1 ORDER BY kind, sort, label", [])? {
                let kind = item["kind"].as_str().unwrap_or_default().to_string();
                let entry = lists.entry(kind).or_insert_with(|| json!([]));
                entry.as_array_mut().expect("array").push(json!({ "code": item["code"], "label": item["label"] }));
            }
            Ok(json!({
                "lists": Value::Object(lists),
                "rooms": query_json(c, "SELECT id, name, location FROM rooms WHERE active = 1 ORDER BY name", [])?,
                "registries": query_json(c, "SELECT id, series, name FROM registries WHERE active = 1 ORDER BY series", [])?,
                "staff": query_json(c, "SELECT id, display_name, title, is_judge FROM users WHERE active = 1 ORDER BY display_name", [])?,
                "templates": query_json(c, "SELECT code, name FROM message_templates WHERE active = 1 ORDER BY name", [])?,
                "court_name": crate::db::setting(c, "court_name", "Court Registry")?,
                "court_timezone": crate::time::COURT_TZ_NAME,
                "today": crate::time::today_local(),
            }))
        })
        .await?;
    Ok(Json(v))
}
