//! Append-only, hash-chained history. UPDATE/DELETE are blocked by triggers; the chain makes
//! out-of-band edits of the SQLite file detectable (`verify_chain`).

use crate::auth::Actor;
use crate::error::AppResult;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub struct Event<'a> {
    pub action: &'a str,
    pub entity_type: &'a str,
    pub entity_id: Option<i64>,
    pub case_id: Option<i64>,
    pub summary: String,
    pub details: Value,
}

impl<'a> Event<'a> {
    pub fn new(action: &'a str, entity_type: &'a str, entity_id: i64, summary: impl Into<String>) -> Self {
        Self { action, entity_type, entity_id: Some(entity_id), case_id: None, summary: summary.into(), details: Value::Null }
    }
    pub fn case(mut self, case_id: Option<i64>) -> Self {
        self.case_id = case_id;
        self
    }
    pub fn details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
}

const GENESIS: &str = "GENESIS";

fn digest(prev: &str, at: &str, user_id: Option<i64>, ev: &Event, details: &str) -> String {
    let mut h = Sha256::new();
    for part in [
        prev,
        at,
        &user_id.map(|u| u.to_string()).unwrap_or_default(),
        ev.action,
        ev.entity_type,
        &ev.entity_id.map(|u| u.to_string()).unwrap_or_default(),
        &ev.case_id.map(|u| u.to_string()).unwrap_or_default(),
        &ev.summary,
        details,
    ] {
        h.update(part.as_bytes());
        h.update([0x1f]);
    }
    hex::encode(h.finalize())
}

/// Append an event. Call inside the same transaction as the change it describes.
pub fn record(conn: &Connection, actor: Option<&Actor>, ev: Event) -> AppResult<i64> {
    let at = crate::time::now_utc();
    let prev: String = conn
        .query_row("SELECT hash FROM audit_events ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
        .optional()?
        .unwrap_or_else(|| GENESIS.to_string());
    let details = if ev.details.is_null() { "{}".to_string() } else { ev.details.to_string() };
    let user_id = actor.map(|a| a.user_id);
    let hash = digest(&prev, &at, user_id, &ev, &details);
    conn.execute(
        "INSERT INTO audit_events (at, user_id, action, entity_type, entity_id, case_id, summary, details, ip, prev_hash, hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            at,
            user_id,
            ev.action,
            ev.entity_type,
            ev.entity_id,
            ev.case_id,
            ev.summary,
            details,
            actor.and_then(|a| a.ip.clone()),
            prev,
            hash
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Recompute the chain. Returns `(event_count, first_broken_id)`.
pub fn verify_chain(conn: &Connection) -> AppResult<(i64, Option<i64>)> {
    let mut stmt = conn.prepare(
        "SELECT id, at, user_id, action, entity_type, entity_id, case_id, summary, details, prev_hash, hash
         FROM audit_events ORDER BY id",
    )?;
    let mut rows = stmt.query([])?;
    let mut prev = GENESIS.to_string();
    let mut n = 0;
    while let Some(r) = rows.next()? {
        n += 1;
        let id: i64 = r.get(0)?;
        let at: String = r.get(1)?;
        let user_id: Option<i64> = r.get(2)?;
        let action: String = r.get(3)?;
        let entity_type: String = r.get(4)?;
        let ev = Event {
            action: &action,
            entity_type: &entity_type,
            entity_id: r.get(5)?,
            case_id: r.get(6)?,
            summary: r.get(7)?,
            details: Value::Null,
        };
        let details: String = r.get(8)?;
        let prev_hash: String = r.get(9)?;
        let hash: String = r.get(10)?;
        if prev_hash != prev || digest(&prev, &at, user_id, &ev, &details) != hash {
            return Ok((n, Some(id)));
        }
        prev = hash;
    }
    Ok((n, None))
}
