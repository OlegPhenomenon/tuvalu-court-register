//! C07 hearings, C08 adjournment/cancellation, C10 outcomes; §8 calendar and booking rules.
//!
//! A draft occupies no slot — confirmation is the booking step. The app-level conflict check
//! (with the `hearing_buffer_minutes` setting) runs inside the same write transaction; the
//! `hearings_no_overlap_*` triggers remain the database backstop for plain `[start, end)` overlap.
//! Confirmed hearings never move: rescheduling is an adjournment that links a new hearing.

use super::common::{JsonBody, JsonResult, optional, query_json, query_one_json, reason, ref_label, require_ref, required};
use super::tasks::{self, NewTask};
use crate::audit::{self, Event};
use crate::auth::{Actor, Ctx, IdemKey, idempotent};
use crate::error::{AppError, AppResult};
use crate::policy::{self, perm};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/hearings", get(calendar))
        .route("/hearings/{id}", get(detail).patch(update))
        .route("/hearings/{id}/confirm", post(confirm))
        .route("/hearings/{id}/adjourn", post(adjourn))
        .route("/hearings/{id}/cancel", post(cancel))
        .route("/hearings/{id}/outcome", post(outcome))
        .route("/hearings/{id}/correct", post(correct))
        .route("/cases/{id}/hearings", get(list_for_case).post(create))
}

// ------------------------------------------------------------------ representation

/// Full hearing JSON, including `starts_local`/`ends_local` and the participant list.
fn hearing_json(conn: &Connection, id: i64) -> AppResult<Value> {
    let mut h = query_one_json(
        conn,
        "SELECT h.id, h.case_id, cs.number AS case_number, h.hearing_type, h.status, h.starts_at, h.ends_at,
                h.room_id, r.name AS room_name, h.judge_user_id, ju.display_name AS judge_name, h.notes,
                h.previous_hearing_id, h.adjourned_to_id, h.status_reason, h.status_authorised_by,
                h.conflict_override, h.override_reason, ou.display_name AS override_by_name,
                h.outcome_summary, h.next_step, oc.display_name AS outcome_recorded_by_name, h.outcome_recorded_at,
                h.created_at, h.version
         FROM hearings h
         JOIN cases cs ON cs.id = h.case_id
         LEFT JOIN rooms r ON r.id = h.room_id
         LEFT JOIN users ju ON ju.id = h.judge_user_id
         LEFT JOIN users ou ON ou.id = h.override_by
         LEFT JOIN users oc ON oc.id = h.outcome_recorded_by
         WHERE h.id = ?1",
        [id],
    )?;
    h["hearing_type_label"] = json!(ref_label(conn, "hearing_type", h["hearing_type"].as_str().unwrap_or_default())?);
    if let Some(s) = h["starts_at"].as_str() {
        h["starts_local"] = json!(crate::time::utc_to_local(s));
    }
    if let Some(s) = h["ends_at"].as_str() {
        h["ends_local"] = json!(crate::time::utc_to_local(s));
    }
    h["participants"] = json!(query_json(
        conn,
        "SELECT hp.id, hp.party_id, p.name AS party_name, hp.user_id, u.display_name AS user_name,
                hp.role, hp.required, hp.attended
         FROM hearing_participants hp
         LEFT JOIN parties p ON p.id = hp.party_id
         LEFT JOIN users u ON u.id = hp.user_id
         WHERE hp.hearing_id = ?1 ORDER BY hp.id",
        [id],
    )?);
    Ok(h)
}

/// Load a hearing whose case the actor may see, else 404 (never leak existence).
fn require_hearing(conn: &Connection, actor: &Actor, id: i64) -> AppResult<Value> {
    let row = query_one_json(conn, "SELECT h.id, h.case_id FROM hearings h WHERE h.id = ?1", [id])?;
    policy::require_case(conn, actor, row["case_id"].as_i64().unwrap_or_default())?;
    hearing_json(conn, id)
}

/// '2026-11-16T21:00:00Z' → '17 Nov 2026 09:00' in court time (audit summaries, task titles).
fn human_local(utc: &str) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let local = crate::time::utc_to_local(utc);
    if local.len() == 16
        && let Ok(month) = local[5..7].parse::<usize>()
        && (1..=12).contains(&month)
    {
        return format!("{} {} {} {}", &local[8..10], MONTHS[month - 1], &local[0..4], &local[11..16]);
    }
    local
}

// ------------------------------------------------------------------ reads

#[derive(Deserialize)]
struct CalendarQuery {
    from: Option<String>,
    to: Option<String>,
    #[serde(default, deserialize_with = "optional_query_id")]
    judge: Option<i64>,
    #[serde(default, deserialize_with = "optional_query_id")]
    room: Option<i64>,
    #[serde(default, deserialize_with = "optional_query_id")]
    case_id: Option<i64>,
}

fn optional_query_id<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.trim().is_empty() {
        Ok(None)
    } else {
        value.parse().map(Some).map_err(serde::de::Error::custom)
    }
}

/// Calendar feed: court-local day range — `from` inclusive, `to` covering its whole day
/// (the upper bound is the exclusive end of the `to` date).
async fn calendar(ctx: Ctx, Query(q): Query<CalendarQuery>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            let day_start = |d: Option<&String>| -> AppResult<Option<String>> {
                match d.map(|s| s.trim()).filter(|s| !s.is_empty()) {
                    Some(d) => Ok(Some(crate::time::local_to_utc(&format!("{}T00:00", crate::time::parse_date(d)?))?)),
                    None => Ok(None),
                }
            };
            let from_utc = day_start(q.from.as_ref())?;
            let to_utc = match day_start(q.to.as_ref())? {
                Some(t) => Some(crate::time::add_minutes(&t, 24 * 60)?),
                None => None,
            };
            let sql = format!(
                "SELECT h.id FROM hearings h
                 WHERE {vis}
                   AND (?1 IS NULL OR h.starts_at >= ?1) AND (?2 IS NULL OR h.starts_at < ?2)
                   AND (?3 IS NULL OR h.judge_user_id = ?3) AND (?4 IS NULL OR h.room_id = ?4)
                   AND (?5 IS NULL OR h.case_id = ?5)
                 ORDER BY h.starts_at, h.id",
                vis = policy::case_visible_sql(&actor, "h.case_id")
            );
            let mut items = Vec::new();
            for row in query_json(c, &sql, params![from_utc, to_utc, q.judge, q.room, q.case_id])? {
                items.push(hearing_json(c, row["id"].as_i64().unwrap_or_default())?);
            }
            Ok(json!({ "items": items }))
        })
        .await?;
    Ok(Json(v))
}

/// All hearings of a case including the adjournment chain, newest first.
async fn list_for_case(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            policy::require_case(c, &actor, id)?;
            let mut items = Vec::new();
            for row in query_json(
                c,
                "SELECT id FROM hearings WHERE case_id = ?1 ORDER BY starts_at DESC, id DESC",
                [id],
            )? {
                items.push(hearing_json(c, row["id"].as_i64().unwrap_or_default())?);
            }
            Ok(json!({ "items": items }))
        })
        .await?;
    Ok(Json(v))
}

async fn detail(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx.db.read(move |c| require_hearing(c, &actor, id)).await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ slot validation & booking

/// A validated booking slot (times already in UTC text).
struct Slot {
    hearing_type: String,
    starts_at: String,
    ends_at: String,
    room_id: Option<i64>,
    judge_user_id: Option<i64>,
}

fn active_judge(tx: &Connection, case_id: i64) -> AppResult<Option<i64>> {
    Ok(tx
        .query_row(
            "SELECT ca.user_id FROM case_assignments ca JOIN users u ON u.id = ca.user_id
             WHERE ca.case_id = ?1 AND ca.role = 'judge' AND ca.end_at IS NULL AND u.active = 1 AND u.is_judge = 1
             ORDER BY ca.id DESC LIMIT 1",
            [case_id],
            |r| r.get(0),
        )
        .optional()?)
}

fn service_officer(tx: &Connection, case_id: i64) -> AppResult<Option<i64>> {
    Ok(tx
        .query_row(
            "SELECT ca.user_id FROM case_assignments ca JOIN users u ON u.id = ca.user_id
             WHERE ca.case_id = ?1 AND ca.role = 'service_officer' AND ca.end_at IS NULL AND u.active = 1
             ORDER BY ca.id DESC LIMIT 1",
            [case_id],
            |r| r.get(0),
        )
        .optional()?)
}

/// A chosen judge must be an assigned judge of the case (the system never picks one).
fn require_case_judge(tx: &Connection, case_id: i64, user_id: i64) -> AppResult<()> {
    let ok: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM case_assignments ca JOIN users u ON u.id = ca.user_id
          WHERE ca.case_id = ?1 AND ca.user_id = ?2 AND ca.role = 'judge' AND ca.end_at IS NULL AND u.active = 1 AND u.is_judge = 1)",
        params![case_id, user_id],
        |r| r.get(0),
    )?;
    if !ok {
        let name = super::common::user_name(tx, Some(user_id))?.unwrap_or_else(|| "This user".into());
        return Err(AppError::validation(format!("{name} is not an assigned judge of this case."))
            .with_details(json!({ "field": "judge_user_id" })));
    }
    Ok(())
}

/// Validate hearing type, times and room, and resolve the judge (explicit → must be an assigned
/// judge of the case; absent → `judge_default`, else the case's active judge).
fn prepare_slot(
    tx: &Connection,
    case_id: i64,
    hearing_type: &str,
    starts_local: &str,
    ends_local: &str,
    room_id: Option<i64>,
    judge_given: Option<i64>,
    judge_default: Option<i64>,
) -> AppResult<Slot> {
    require_ref(tx, "hearing_type", hearing_type)?;
    let starts_at = crate::time::local_to_utc(starts_local)?;
    let ends_at = crate::time::local_to_utc(ends_local)?;
    if ends_at <= starts_at {
        return Err(AppError::validation("The end time must be after the start time.").with_details(json!({ "field": "ends_local" })));
    }
    let room_id = match room_id {
        Some(r) => {
            let ok: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM rooms WHERE id = ?1 AND active = 1)", [r], |r0| {
                r0.get(0)
            })?;
            if !ok {
                return Err(AppError::validation("Unknown or inactive room.").with_details(json!({ "field": "room_id" })));
            }
            Some(r)
        }
        None => None,
    };
    let judge_user_id = match judge_given.or(judge_default) {
        Some(j) => Some(j),
        None => active_judge(tx, case_id)?,
    };
    if let Some(judge) = judge_user_id {
        require_case_judge(tx, case_id, judge)?;
    }
    Ok(Slot {
        hearing_type: hearing_type.trim().to_string(),
        starts_at,
        ends_at,
        room_id,
        judge_user_id,
    })
}

fn require_open_case(conn: &Connection, actor: &Actor, case_id: i64) -> AppResult<()> {
    if policy::require_case(conn, actor, case_id)?.status == "closed" {
        return Err(AppError::invalid_transition(
            "This case is closed. Reopen it to schedule a hearing.",
        ));
    }
    Ok(())
}

/// App-level double-booking check inside the write transaction: another `scheduled` hearing with
/// the same room or judge overlaps when `existing.starts_at < new.ends_at + buffer` and
/// `new.starts_at < existing.ends_at + buffer` (buffer = `hearing_buffer_minutes`, default 0).
/// Returns the accepted override reason; errors are 409 `hearing_conflict` (visible conflicts are
/// listed, invisible ones counted) or 403 when the actor overrides without `hearing.override_conflict`.
fn conflict_gate(
    conn: &Connection,
    actor: &Actor,
    exclude: Option<i64>,
    slot: &Slot,
    override_reason: Option<String>,
) -> AppResult<Option<String>> {
    let buffer: i64 = crate::db::setting(conn, "hearing_buffer_minutes", "0")?
        .trim()
        .parse()
        .unwrap_or(0)
        .max(0);
    let new_end_buf = crate::time::add_minutes(&slot.ends_at, buffer)?;
    let new_start_buf = crate::time::add_minutes(&slot.starts_at, -buffer)?;
    let candidates = query_json(
        conn,
        "SELECT h.id, h.case_id, cs.number AS case_number, h.starts_at, h.ends_at,
                r.name AS room_name, ju.display_name AS judge_name
         FROM hearings h JOIN cases cs ON cs.id = h.case_id
         LEFT JOIN rooms r ON r.id = h.room_id LEFT JOIN users ju ON ju.id = h.judge_user_id
         WHERE h.status = 'scheduled' AND (?1 IS NULL OR h.id <> ?1)
           AND ((?2 IS NOT NULL AND h.room_id = ?2) OR (?3 IS NOT NULL AND h.judge_user_id = ?3))
           AND h.starts_at < ?4 AND h.ends_at > ?5
         ORDER BY h.starts_at, h.id",
        params![exclude, slot.room_id, slot.judge_user_id, new_end_buf, new_start_buf],
    )?;
    let mut visible = Vec::new();
    let mut hidden = 0i64;
    for h in candidates {
        let (hs, he) = (
            h["starts_at"].as_str().unwrap_or_default(),
            h["ends_at"].as_str().unwrap_or_default(),
        );
        if policy::can_view_case(conn, actor, h["case_id"].as_i64().unwrap_or_default())? {
            visible.push(json!({
                "hearing_id": h["id"], "case_id": h["case_id"], "case_number": h["case_number"],
                "starts_local": crate::time::utc_to_local(hs), "ends_local": crate::time::utc_to_local(he),
                "room_name": h["room_name"], "judge_name": h["judge_name"],
            }));
        } else {
            hidden += 1;
        }
    }
    if visible.is_empty() && hidden == 0 {
        return Ok(None);
    }
    match override_reason {
        Some(r) => {
            actor.require(perm::HEARING_OVERRIDE_CONFLICT)?;
            Ok(Some(r))
        }
        None => Err(AppError::conflict(
            "hearing_conflict",
            "The judge or the room is already booked for an overlapping time.",
        )
        .with_details(json!({ "conflicts": visible, "hidden_conflicts": hidden }))),
    }
}

fn insert_hearing(
    tx: &Connection,
    actor: &Actor,
    case_id: i64,
    slot: &Slot,
    status: &str,
    previous: Option<i64>,
    over: Option<&str>,
    notes: Option<String>,
) -> AppResult<i64> {
    require_open_case(tx, actor, case_id)?;
    tx.execute(
        "INSERT INTO hearings (case_id, hearing_type, status, starts_at, ends_at, room_id, judge_user_id, notes,
                               previous_hearing_id, conflict_override, override_reason, override_by, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            case_id,
            slot.hearing_type,
            status,
            slot.starts_at,
            slot.ends_at,
            slot.room_id,
            slot.judge_user_id,
            notes,
            previous,
            over.is_some() as i64,
            over,
            over.map(|_| actor.user_id),
            actor.user_id,
            crate::time::now_utc()
        ],
    )?;
    let id = tx.last_insert_rowid();
    audit::record(
        tx,
        Some(actor),
        Event::new(
            "hearing.created",
            "hearing",
            id,
            format!(
                "Hearing on {} {}",
                human_local(&slot.starts_at),
                if status == "scheduled" { "scheduled" } else { "saved as draft" }
            ),
        )
        .case(Some(case_id))
        .details(json!({ "status": status, "hearing_type": slot.hearing_type, "previous_hearing_id": previous })),
    )?;
    Ok(id)
}

#[derive(Deserialize, Serialize)]
struct ParticipantIn {
    party_id: Option<i64>,
    user_id: Option<i64>,
    role: String,
    required: Option<bool>,
}

fn insert_participants(tx: &Connection, case_id: i64, hearing_id: i64, parts: &[ParticipantIn]) -> AppResult<()> {
    for p in parts {
        let role = required(&p.role, "Role")?;
        match (p.party_id, p.user_id) {
            (Some(pid), None) => {
                let belongs: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM case_participations WHERE case_id = ?1 AND party_id = ?2 AND active = 1)",
                    params![case_id, pid],
                    |r| r.get(0),
                )?;
                if !belongs {
                    return Err(AppError::validation("That participant party does not belong to this case."));
                }
                require_ref(tx, "participant_role", &role)?;
            }
            (None, Some(uid)) => {
                tx.query_row("SELECT id FROM users WHERE id = ?1 AND active = 1", [uid], |r| r.get::<_, i64>(0))
                    .optional()?
                    .ok_or_else(|| AppError::validation("Unknown participant user."))?;
            }
            _ => {
                return Err(AppError::validation("Each participant is either a party or a staff user."));
            }
        }
        tx.execute(
            "INSERT INTO hearing_participants (hearing_id, party_id, user_id, role, required) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![hearing_id, p.party_id, p.user_id, role, p.required.unwrap_or(true) as i64],
        )?;
    }
    Ok(())
}

/// An adjourned hearing's new appointment inherits the participant list (attendance starts blank).
fn copy_participants(tx: &Connection, from_hearing: i64, to_hearing: i64) -> AppResult<()> {
    tx.execute(
        "INSERT INTO hearing_participants (hearing_id, party_id, user_id, role, required)
         SELECT ?2, party_id, user_id, role, required FROM hearing_participants WHERE hearing_id = ?1",
        params![from_hearing, to_hearing],
    )?;
    Ok(())
}

/// Names of required participants (party or staff user), for re-notification tasks.
fn required_participant_names(tx: &Connection, hearing_id: i64) -> AppResult<Vec<String>> {
    let rows = query_json(
        tx,
        "SELECT COALESCE(p.name, u.display_name) AS name FROM hearing_participants hp
         LEFT JOIN parties p ON p.id = hp.party_id LEFT JOIN users u ON u.id = hp.user_id
         WHERE hp.hearing_id = ?1 AND hp.required = 1 ORDER BY hp.id",
        [hearing_id],
    )?;
    Ok(rows.iter().filter_map(|r| r["name"].as_str().map(str::to_string)).collect())
}

// ------------------------------------------------------------------ create & edit

#[derive(Deserialize, Serialize)]
struct CreateReq {
    hearing_type: String,
    starts_local: String,
    ends_local: String,
    room_id: Option<i64>,
    judge_user_id: Option<i64>,
    notes: Option<String>,
    #[serde(default)]
    participants: Vec<ParticipantIn>,
    confirm: bool,
    override_reason: Option<String>,
}

async fn create(ctx: Ctx, Path(case_id): Path<i64>, IdemKey(key): IdemKey, JsonBody(req): JsonBody<CreateReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case = policy::require_case_perm(tx, &actor, case_id, perm::HEARING_SCHEDULE)?;
            idempotent(tx, &actor, &key, "hearing.create", &(case_id, &req), || {
                if case.status == "closed" {
                    return Err(AppError::invalid_transition(
                        "This case is closed. Reopen it to schedule a hearing.",
                    ));
                }
                let slot = prepare_slot(
                    tx,
                    case_id,
                    &req.hearing_type,
                    &req.starts_local,
                    &req.ends_local,
                    req.room_id,
                    req.judge_user_id,
                    None,
                )?;
                // Drafts occupy no slot; the booking check runs only when confirming.
                let over = if req.confirm {
                    conflict_gate(tx, &actor, None, &slot, optional(&req.override_reason))?
                } else {
                    None
                };
                let id = insert_hearing(
                    tx,
                    &actor,
                    case_id,
                    &slot,
                    if req.confirm { "scheduled" } else { "draft" },
                    None,
                    over.as_deref(),
                    optional(&req.notes),
                )?;
                insert_participants(tx, case_id, id, &req.participants)?;
                let when = human_local(&slot.starts_at);
                if let Some(r) = &over {
                    audit::record(
                        tx,
                        Some(&actor),
                        Event::new(
                            "hearing.conflict_override",
                            "hearing",
                            id,
                            format!("Conflict overridden for the hearing on {when}"),
                        )
                        .case(Some(case_id))
                        .details(json!({ "reason": r })),
                    )?;
                }
                hearing_json(tx, id)
            })
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct UpdateReq {
    version: i64,
    hearing_type: Option<String>,
    starts_local: Option<String>,
    ends_local: Option<String>,
    #[serde(default, deserialize_with = "super::common::nullable")]
    room_id: Option<Option<i64>>,
    #[serde(default, deserialize_with = "super::common::nullable")]
    judge_user_id: Option<Option<i64>>,
    notes: Option<String>,
}

/// Edit a draft (a confirmed hearing never moves — adjourn instead).
async fn update(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<UpdateReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let h = require_hearing(tx, &actor, id)?;
            actor.require(perm::HEARING_SCHEDULE)?;
            if h["status"].as_str() != Some("draft") {
                return Err(AppError::invalid_transition(
                    "Only a draft hearing can be edited. Adjourn a confirmed hearing instead.",
                ));
            }
            if h["version"].as_i64() != Some(req.version) {
                return Err(AppError::version_conflict(h));
            }
            let case_id = h["case_id"].as_i64().unwrap_or_default();
            let htype = req
                .hearing_type
                .clone()
                .unwrap_or_else(|| h["hearing_type"].as_str().unwrap_or_default().to_string());
            let starts = req
                .starts_local
                .clone()
                .unwrap_or_else(|| h["starts_local"].as_str().unwrap_or_default().to_string());
            let ends = req
                .ends_local
                .clone()
                .unwrap_or_else(|| h["ends_local"].as_str().unwrap_or_default().to_string());
            let judge = req.judge_user_id.unwrap_or(h["judge_user_id"].as_i64());
            let mut slot = prepare_slot(
                tx,
                case_id,
                &htype,
                &starts,
                &ends,
                req.room_id.unwrap_or(h["room_id"].as_i64()),
                judge,
                None,
            )?;
            // Unlike creation, PATCH never chooses a judge when the field is cleared or omitted.
            slot.judge_user_id = judge;
            tx.execute(
                "UPDATE hearings SET hearing_type = ?2, starts_at = ?3, ends_at = ?4, room_id = ?5, judge_user_id = ?6,
                        notes = CASE WHEN ?7 IS NULL THEN notes ELSE NULLIF(?7, '') END, version = version + 1
                 WHERE id = ?1",
                params![
                    id,
                    slot.hearing_type,
                    slot.starts_at,
                    slot.ends_at,
                    slot.room_id,
                    slot.judge_user_id,
                    req.notes.as_deref().map(str::trim)
                ],
            )?;
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "hearing.updated",
                    "hearing",
                    id,
                    format!("Draft hearing on {} changed", human_local(&slot.starts_at)),
                )
                .case(Some(case_id))
                .details(json!({ "before": h })),
            )?;
            hearing_json(tx, id)
        })
        .await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ confirm

#[derive(Deserialize)]
struct ConfirmReq {
    override_reason: Option<String>,
}

async fn confirm(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<ConfirmReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let h = require_hearing(tx, &actor, id)?;
            actor.require(perm::HEARING_SCHEDULE)?;
            if h["status"].as_str() != Some("draft") {
                return Err(AppError::invalid_transition("Only a draft hearing can be confirmed."));
            }
            let case_id = h["case_id"].as_i64().unwrap_or_default();
            require_open_case(tx, &actor, case_id)?;
            let slot = prepare_slot(
                tx,
                case_id,
                h["hearing_type"].as_str().unwrap_or_default(),
                h["starts_local"].as_str().unwrap_or_default(),
                h["ends_local"].as_str().unwrap_or_default(),
                h["room_id"].as_i64(),
                h["judge_user_id"].as_i64(),
                None,
            )?;
            let over = conflict_gate(tx, &actor, Some(id), &slot, optional(&req.override_reason))?;
            let had_over = over.is_some();
            let over_by = over.as_ref().map(|_| actor.user_id);
            tx.execute(
                "UPDATE hearings SET status = 'scheduled', conflict_override = ?2, override_reason = ?3, override_by = ?4,
                        judge_user_id = ?5, version = version + 1
                 WHERE id = ?1",
                params![id, had_over as i64, over, over_by, slot.judge_user_id],
            )?;
            let when = human_local(&slot.starts_at);
            if had_over {
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new(
                        "hearing.conflict_override",
                        "hearing",
                        id,
                        format!("Conflict overridden for the hearing on {when}"),
                    )
                    .case(h["case_id"].as_i64())
                    .details(json!({ "reason": over })),
                )?;
            }
            audit::record(
                tx,
                Some(&actor),
                Event::new("hearing.confirmed", "hearing", id, format!("Hearing on {when} confirmed")).case(h["case_id"].as_i64()),
            )?;
            hearing_json(tx, id)
        })
        .await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ adjourn & cancel

#[derive(Deserialize, Serialize)]
struct AdjournReq {
    starts_local: String,
    ends_local: String,
    room_id: Option<i64>,
    judge_user_id: Option<i64>,
    reason: String,
    authorised_by: String,
    override_reason: Option<String>,
}

/// Adjourn: the old hearing stays as 'adjourned' with reason + authoriser, a new linked hearing is
/// booked, and one re-notification task is created per required participant — all in one transaction.
async fn adjourn(ctx: Ctx, Path(id): Path<i64>, IdemKey(key): IdemKey, JsonBody(req): JsonBody<AdjournReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let h = require_hearing(tx, &actor, id)?;
            actor.require(perm::HEARING_SCHEDULE)?;
            let case_id = h["case_id"].as_i64().unwrap_or_default();
            idempotent(tx, &actor, &key, "hearing.adjourn", &(id, &req), || {
                if h["status"].as_str() != Some("scheduled") {
                    return Err(AppError::invalid_transition(match h["status"].as_str() {
                        Some("held") => "A hearing that was held cannot be adjourned.".to_string(),
                        Some(s) => format!("This hearing is '{s}', so it cannot be adjourned."),
                        None => "This hearing cannot be adjourned.".to_string(),
                    }));
                }
                let why = required(&req.reason, "Reason")?;
                let authorised = required(&req.authorised_by, "Authorised by")?;
                let slot = prepare_slot(
                    tx,
                    case_id,
                    h["hearing_type"].as_str().unwrap_or_default(),
                    &req.starts_local,
                    &req.ends_local,
                    req.room_id.or(h["room_id"].as_i64()),
                    req.judge_user_id,
                    h["judge_user_id"].as_i64(),
                )?;
                if slot.starts_at == h["starts_at"].as_str().unwrap_or_default() {
                    return Err(AppError::validation("Choose a new date or time"));
                }
                // Free the old slot first so the new booking cannot clash with it.
                tx.execute(
                    "UPDATE hearings SET status = 'adjourned', status_reason = ?2, status_authorised_by = ?3, version = version + 1 WHERE id = ?1",
                    params![id, why, authorised],
                )?;
                let over = conflict_gate(tx, &actor, None, &slot, optional(&req.override_reason))?;
                let new_id = insert_hearing(tx, &actor, case_id, &slot, "scheduled", Some(id), over.as_deref(), h["notes"].as_str().map(str::to_string))?;
                if let Some(r) = &over {
                    audit::record(tx, Some(&actor), Event::new("hearing.conflict_override", "hearing", new_id,
                        format!("Conflict overridden for the hearing on {}", human_local(&slot.starts_at)))
                        .case(Some(case_id)).details(json!({"reason": r})))?;
                }
                copy_participants(tx, id, new_id)?;
                tx.execute("UPDATE hearings SET adjourned_to_id = ?2 WHERE id = ?1", params![id, new_id])?;
                let assignee = service_officer(tx, case_id)?.unwrap_or(actor.user_id);
                let new_when = human_local(&slot.starts_at);
                let mut made = Vec::new();
                for name in required_participant_names(tx, id)? {
                    let tid = tasks::insert_task(
                        tx,
                        &actor,
                        &NewTask {
                            case_id: Some(case_id),
                            intake_id: None,
                            hearing_id: Some(new_id),
                            kind: "renotify".into(),
                            title: format!("Notify {name} of the new hearing date ({new_when})"),
                            description: None,
                            assignee_user_id: Some(assignee),
                            due_date: None,
                        },
                    )?;
                    made.push(tasks::task_json(tx, tid)?);
                }
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new("hearing.adjourned", "hearing", id, format!("Hearing on {} adjourned to {new_when}", human_local(h["starts_at"].as_str().unwrap_or_default())))
                        .case(Some(case_id))
                        .details(json!({ "reason": why, "authorised_by": authorised, "new_hearing_id": new_id, "tasks": made.iter().map(|t| t["id"].clone()).collect::<Vec<_>>() })),
                )?;
                Ok(json!({ "old": hearing_json(tx, id)?, "new": hearing_json(tx, new_id)?, "tasks": made }))
            })
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct ReasonReq {
    reason: Option<String>,
}

/// Cancel a draft or scheduled hearing; the date, participants and notices stay on record.
async fn cancel(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<ReasonReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let h = require_hearing(tx, &actor, id)?;
            actor.require(perm::HEARING_SCHEDULE)?;
            if !matches!(h["status"].as_str(), Some("draft") | Some("scheduled")) {
                return Err(AppError::invalid_transition(format!(
                    "This hearing is '{}', so it cannot be cancelled.",
                    h["status"].as_str().unwrap_or_default()
                )));
            }
            let why = reason(&req.reason)?;
            tx.execute(
                "UPDATE hearings SET status = 'cancelled', status_reason = ?2, version = version + 1 WHERE id = ?1",
                params![id, why],
            )?;
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "hearing.cancelled",
                    "hearing",
                    id,
                    format!("Hearing on {} cancelled", human_local(h["starts_at"].as_str().unwrap_or_default())),
                )
                .case(h["case_id"].as_i64())
                .details(json!({ "reason": why })),
            )?;
            hearing_json(tx, id)
        })
        .await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ outcome & correction

#[derive(Deserialize)]
struct Attendance {
    participant_id: i64,
    attended: bool,
}

#[derive(Deserialize)]
struct NextTaskIn {
    title: String,
    assignee_user_id: Option<i64>,
    due_date: Option<String>,
}

#[derive(Deserialize)]
struct NextHearingIn {
    starts_local: String,
    ends_local: String,
    room_id: Option<i64>,
    hearing_type: Option<String>,
    override_reason: Option<String>,
}

#[derive(Deserialize)]
struct OutcomeReq {
    held: bool,
    reason: Option<String>,
    #[serde(default)]
    attendance: Vec<Attendance>,
    outcome_summary: Option<String>,
    next_step: Option<String>,
    next_task: Option<NextTaskIn>,
    next_hearing: Option<NextHearingIn>,
}

/// Record the outcome: held (summary, attendance, next step) or not held (mandatory reason, kept
/// on record). A held hearing never changes the case status. In production an outcome can only be
/// recorded once the hearing has started; demo mode allows it so visitors can finish the walkthrough.
async fn outcome(
    ctx: Ctx,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    JsonBody(req): JsonBody<OutcomeReq>,
) -> JsonResult {
    let demo = state.is_demo();
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let h = require_hearing(tx, &actor, id)?;
            actor.require(perm::HEARING_RECORD_OUTCOME)?;
            if h["status"].as_str() != Some("scheduled") {
                return Err(AppError::invalid_transition(
                    "Only a scheduled hearing can have an outcome recorded.",
                ));
            }
            let now = crate::time::now_utc();
            let future = h["starts_at"].as_str().unwrap_or_default() > now.as_str();
            if future && !demo {
                return Err(AppError::invalid_transition("The hearing has not taken place yet."));
            }
            let case_id = h["case_id"].as_i64().unwrap_or_default();
            let when = human_local(h["starts_at"].as_str().unwrap_or_default());
            if req.held {
                let summary = required(req.outcome_summary.as_deref().unwrap_or(""), "Outcome summary")?;
                tx.execute(
                    "UPDATE hearings SET status = 'held', outcome_summary = ?2, next_step = ?3,
                            outcome_recorded_by = ?4, outcome_recorded_at = ?5, version = version + 1
                     WHERE id = ?1",
                    params![id, summary, optional(&req.next_step), actor.user_id, now],
                )?;
            } else {
                let why = reason(&req.reason)?;
                tx.execute(
                    "UPDATE hearings SET status = 'cancelled', status_reason = ?2, outcome_summary = ?3, next_step = ?4,
                            outcome_recorded_by = ?5, outcome_recorded_at = ?6, version = version + 1
                     WHERE id = ?1",
                    params![
                        id,
                        why,
                        optional(&req.outcome_summary),
                        optional(&req.next_step),
                        actor.user_id,
                        now
                    ],
                )?;
            }
            for a in &req.attendance {
                let n = tx.execute(
                    "UPDATE hearing_participants SET attended = ?3 WHERE id = ?1 AND hearing_id = ?2",
                    params![a.participant_id, id, a.attended as i64],
                )?;
                if n == 0 {
                    return Err(AppError::validation("A participant id does not belong to this hearing."));
                }
            }
            let mut task_v = Value::Null;
            if let Some(nt) = &req.next_task {
                let tid = tasks::insert_task(
                    tx,
                    &actor,
                    &NewTask {
                        case_id: Some(case_id),
                        intake_id: None,
                        hearing_id: Some(id),
                        kind: "follow_up".into(),
                        title: nt.title.clone(),
                        description: None,
                        assignee_user_id: nt.assignee_user_id,
                        due_date: nt.due_date.clone(),
                    },
                )?;
                task_v = tasks::task_json(tx, tid)?;
            }
            let mut next_v = Value::Null;
            if let Some(nh) = &req.next_hearing {
                // A continuation of the same case: type/room/judge/participants carry over by default.
                let htype = nh
                    .hearing_type
                    .clone()
                    .unwrap_or_else(|| h["hearing_type"].as_str().unwrap_or_default().to_string());
                let slot = prepare_slot(
                    tx,
                    case_id,
                    &htype,
                    &nh.starts_local,
                    &nh.ends_local,
                    nh.room_id.or(h["room_id"].as_i64()),
                    None,
                    h["judge_user_id"].as_i64(),
                )?;
                let scheduled = actor.has(perm::HEARING_SCHEDULE);
                let over = if scheduled { conflict_gate(tx, &actor, None, &slot, optional(&nh.override_reason))? } else { None };
                let nid = insert_hearing(tx, &actor, case_id, &slot, if scheduled { "scheduled" } else { "draft" }, Some(id), over.as_deref(), None)?;
                if let Some(r) = &over {
                    audit::record(tx, Some(&actor), Event::new("hearing.conflict_override", "hearing", nid,
                        format!("Conflict overridden for the hearing on {}", human_local(&slot.starts_at)))
                        .case(Some(case_id)).details(json!({"reason": r})))?;
                }
                copy_participants(tx, id, nid)?;
                next_v = hearing_json(tx, nid)?;
            }
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "hearing.outcome_recorded",
                    "hearing",
                    id,
                    if req.held {
                        format!("Hearing on {when} held — outcome recorded")
                    } else {
                        format!("Hearing on {when} did not take place")
                    },
                )
                .case(Some(case_id))
                .details(
                    json!({ "held": req.held, "reason": optional(&req.reason), "task_id": task_v["id"], "next_hearing_id": next_v["id"] }),
                ),
            )?;
            let mut out = json!({ "hearing": hearing_json(tx, id)?, "task": task_v, "next_hearing": next_v });
            if out["next_hearing"]["status"] == "draft" {
                out["next_hearing_note"] = json!("Next hearing saved as a draft for a scheduler to confirm.");
            }
            if future && demo {
                out["demo_note"] = json!("Recorded ahead of the hearing time (demo only)");
            }
            Ok(out)
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct CorrectReq {
    reason: Option<String>,
    status: String,
}

/// Correct an erroneously held hearing back to scheduled or cancelled. Nothing is cleared;
/// the audit event carries the full before-state.
async fn correct(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<CorrectReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let h = require_hearing(tx, &actor, id)?;
            actor.require(perm::HEARING_ADMIN_CORRECT)?;
            if h["status"].as_str() != Some("held") {
                return Err(AppError::invalid_transition("Only a hearing recorded as held can be corrected."));
            }
            if !matches!(req.status.as_str(), "scheduled" | "cancelled") {
                return Err(AppError::validation("Correction status must be 'scheduled' or 'cancelled'.")
                    .with_details(json!({ "field": "status" })));
            }
            let why = reason(&req.reason)?;
            if req.status == "scheduled" {
                let case_id = h["case_id"].as_i64().unwrap_or_default();
                require_open_case(tx, &actor, case_id)?;
                let slot = Slot {
                    hearing_type: h["hearing_type"].as_str().unwrap_or_default().to_string(),
                    starts_at: h["starts_at"].as_str().unwrap_or_default().to_string(),
                    ends_at: h["ends_at"].as_str().unwrap_or_default().to_string(),
                    room_id: h["room_id"].as_i64(),
                    judge_user_id: h["judge_user_id"].as_i64(),
                };
                conflict_gate(tx, &actor, Some(id), &slot, None)?;
            }
            tx.execute(
                "UPDATE hearings SET status = ?2, status_reason = ?3, version = version + 1 WHERE id = ?1",
                params![id, req.status, why],
            )?;
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "hearing.corrected",
                    "hearing",
                    id,
                    format!(
                        "Hearing on {} corrected to {}",
                        human_local(h["starts_at"].as_str().unwrap_or_default()),
                        req.status
                    ),
                )
                .case(h["case_id"].as_i64())
                .details(json!({ "before": h, "reason": why })),
            )?;
            hearing_json(tx, id)
        })
        .await?;
    Ok(Json(v))
}
