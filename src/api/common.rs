//! Small helpers shared by API modules.

use crate::error::{AppError, AppResult};
use axum::extract::{FromRequest, Request};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// JSON body extractor whose rejections are rendered as our JSON `validation` error.
pub struct JsonBody<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for JsonBody<T> {
    type Rejection = AppError;
    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(v)) => Ok(JsonBody(v)),
            Err(e) => Err(AppError::validation(e.body_text())),
        }
    }
}

pub type JsonResult = AppResult<axum::Json<Value>>;

pub fn ok() -> JsonResult {
    Ok(axum::Json(json!({ "ok": true })))
}

/// Trimmed non-empty text or a validation error naming the field.
pub fn required(value: &str, field: &str) -> AppResult<String> {
    let v = value.trim();
    if v.is_empty() {
        return Err(AppError::validation(format!("{field} is required.")).with_details(json!({ "field": field })));
    }
    if v.chars().count() > 20_000 {
        return Err(AppError::validation(format!("{field} is too long.")).with_details(json!({ "field": field })));
    }
    Ok(v.to_string())
}

/// Optional text: trimmed, empty → None.
pub fn optional(value: &Option<String>) -> Option<String> {
    value.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Mandatory reason for state changes that must be explained.
pub fn reason(value: &Option<String>) -> AppResult<String> {
    required(value.as_deref().unwrap_or(""), "Reason")
}

/// Check a code exists in a reference list (`ref_items`).
pub fn require_ref(conn: &rusqlite::Connection, kind: &str, code: &str) -> AppResult<()> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ref_items WHERE kind = ?1 AND code = ?2",
        [kind, code],
        |r| r.get(0),
    )?;
    if n == 0 {
        return Err(AppError::validation(format!("Unknown {kind} '{code}'.")).with_details(json!({ "field": kind })));
    }
    Ok(())
}

/// Label of a reference code (falls back to the code).
pub fn ref_label(conn: &rusqlite::Connection, kind: &str, code: &str) -> AppResult<String> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row("SELECT label FROM ref_items WHERE kind = ?1 AND code = ?2", [kind, code], |r| r.get(0))
        .optional()?
        .unwrap_or_else(|| code.to_string()))
}

/// Display name of a user id.
pub fn user_name(conn: &rusqlite::Connection, user_id: Option<i64>) -> AppResult<Option<String>> {
    use rusqlite::OptionalExtension;
    match user_id {
        None => Ok(None),
        Some(id) => Ok(conn.query_row("SELECT display_name FROM users WHERE id = ?1", [id], |r| r.get(0)).optional()?),
    }
}

/// Convert a rusqlite row into a JSON object using column names (generic list endpoints).
pub fn row_to_json(row: &rusqlite::Row) -> rusqlite::Result<Value> {
    use rusqlite::types::ValueRef;
    let stmt = row.as_ref();
    let mut map = serde_json::Map::with_capacity(stmt.column_count());
    for i in 0..stmt.column_count() {
        let name = stmt.column_name(i)?.to_string();
        let v = match row.get_ref(i)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(n) => json!(n),
            ValueRef::Real(f) => json!(f),
            ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
            ValueRef::Blob(_) => Value::Null,
        };
        map.insert(name, v);
    }
    Ok(Value::Object(map))
}

/// Run a query and return all rows as JSON objects.
pub fn query_json(conn: &rusqlite::Connection, sql: &str, params: impl rusqlite::Params) -> AppResult<Vec<Value>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params, row_to_json)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// First row as JSON or 404.
pub fn query_one_json(conn: &rusqlite::Connection, sql: &str, params: impl rusqlite::Params) -> AppResult<Value> {
    query_json(conn, sql, params)?.into_iter().next().ok_or_else(AppError::not_found)
}

/// Render a message template; `{court}` is filled from settings. Unknown placeholders stay visible
/// so the reviewer notices them before sending.
pub fn render_template(conn: &rusqlite::Connection, code: &str, vars: &[(&str, String)]) -> AppResult<(String, String)> {
    use rusqlite::OptionalExtension;
    let (subject, body): (String, String) = conn
        .query_row("SELECT subject, body FROM message_templates WHERE code = ?1 AND active = 1", [code], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?
        .ok_or_else(|| AppError::validation(format!("Message template '{code}' is missing or inactive.")))?;
    let court = crate::db::setting(conn, "court_name", "Court Registry")?;
    let fill = |s: &str| {
        let mut out = s.replace("{court}", &court);
        for (k, v) in vars {
            out = out.replace(&format!("{{{k}}}"), v);
        }
        out
    };
    Ok((fill(&subject), fill(&body)))
}
