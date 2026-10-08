//! C10 Tasks: registry work with its own state (`open → done|cancelled|carried_forward`).
//! A task never decides the case outcome; open tasks only block a silent closing until they are
//! done, cancelled with a reason, or carried forward explicitly. Due dates are entered by a
//! human — the system never computes legal deadlines.

use super::common::{JsonBody, JsonResult, optional, query_json, query_one_json, reason, required};
use crate::audit::{self, Event};
use crate::auth::{Actor, Ctx};
use crate::error::{AppError, AppResult};
use crate::policy::{self, perm};
use crate::state::AppState;
use axum::extract::{Path, Query};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/tasks", get(list))
        .route("/tasks/{id}", get(detail).patch(update))
        .route("/tasks/{id}/complete", post(complete))
        .route("/tasks/{id}/cancel", post(cancel))
        .route("/tasks/{id}/carry-forward", post(carry_forward))
        .route("/cases/{id}/tasks", get(list_for_case).post(create))
}

const COLS: &str = "t.id, t.case_id, cs.number AS case_number, t.intake_id, t.hearing_id, t.kind, t.title,
    t.description, t.assignee_user_id, au.display_name AS assignee_name, t.due_date, t.status, t.result,
    t.status_reason, cu.display_name AS created_by_name, t.created_at, cl.display_name AS closed_by_name,
    t.closed_at, t.version";
const JOINS: &str = "LEFT JOIN cases cs ON cs.id = t.case_id
    LEFT JOIN users au ON au.id = t.assignee_user_id
    LEFT JOIN users cu ON cu.id = t.created_by
    LEFT JOIN users cl ON cl.id = t.closed_by";

/// Full task JSON (also used by the hearings module for generated follow-up tasks).
pub fn task_json(conn: &Connection, id: i64) -> AppResult<Value> {
    query_one_json(conn, &format!("SELECT {COLS} FROM tasks t {JOINS} WHERE t.id = ?1"), [id])
}

/// Tasks follow case access; intake tasks require intake.manage.
fn task_visible_sql(actor: &Actor, t: &str) -> String {
    let case = policy::case_visible_sql(actor, &format!("{t}.case_id"));
    let intake = if actor.has(perm::INTAKE_MANAGE) { "1" } else { "0" };
    format!(
        "(({t}.case_id IS NOT NULL AND {case})
          OR ({t}.case_id IS NULL AND {t}.intake_id IS NOT NULL AND {intake}))"
    )
}

/// Load a task the actor may see, else 404.
fn require_task(conn: &Connection, actor: &Actor, id: i64) -> AppResult<Value> {
    query_one_json(
        conn,
        &format!("SELECT t.id FROM tasks t WHERE t.id = ?1 AND {}", task_visible_sql(actor, "t")),
        [id],
    )?;
    task_json(conn, id)
}

/// task.manage or being the assignee — required for state changes.
fn require_touch(actor: &Actor, task: &Value) -> AppResult<()> {
    if actor.has(perm::TASK_MANAGE) || task["assignee_user_id"].as_i64() == Some(actor.user_id) {
        Ok(())
    } else {
        Err(AppError::forbidden("Only the assignee or a task manager can change this task."))
    }
}

/// New task fields shared by the endpoint and by hearings (renotify / follow-up tasks).
pub struct NewTask {
    pub case_id: Option<i64>,
    pub intake_id: Option<i64>,
    pub hearing_id: Option<i64>,
    pub kind: String,
    pub title: String,
    pub description: Option<String>,
    pub assignee_user_id: Option<i64>,
    pub due_date: Option<String>,
}

/// The assignee must be a real, active user who currently has access to the case.
fn check_assignee(tx: &Connection, assignee_user_id: i64, case_id: Option<i64>) -> AppResult<()> {
    let assignee = crate::auth::load_actor(tx, assignee_user_id, None)?
        .ok_or_else(|| AppError::validation("Unknown assignee.").with_details(json!({ "field": "assignee_user_id" })))?;
    if let Some(cid) = case_id
        && !policy::can_view_case(tx, &assignee, cid)?
    {
        return Err(AppError::validation(format!(
            "{} has no access to this case — assign them to the case first.",
            assignee.display_name
        ))
        .with_details(json!({ "field": "assignee_user_id" })));
    }
    Ok(())
}

/// Insert an open task. Assignee access is enforced at this boundary for every caller.
pub fn insert_task(tx: &Connection, actor: &Actor, t: &NewTask) -> AppResult<i64> {
    let title = required(&t.title, "Title")?;
    let due = crate::time::parse_opt_date(t.due_date.as_deref())?;
    if let Some(uid) = t.assignee_user_id {
        check_assignee(tx, uid, t.case_id)?;
    }
    tx.execute(
        "INSERT INTO tasks (case_id, intake_id, hearing_id, kind, title, description, assignee_user_id, due_date, status, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', ?9, ?10)",
        params![t.case_id, t.intake_id, t.hearing_id, t.kind, title, optional(&t.description), t.assignee_user_id, due, actor.user_id, crate::time::now_utc()],
    )?;
    let id = tx.last_insert_rowid();
    audit::record(
        tx,
        Some(actor),
        Event::new("task.created", "task", id, format!("Task created: {title}"))
            .case(t.case_id)
            .details(json!({ "kind": t.kind, "hearing_id": t.hearing_id })),
    )?;
    Ok(id)
}

// ------------------------------------------------------------------ lists

#[derive(Deserialize)]
struct ListQuery {
    mine: Option<i64>,
    status: Option<String>,
}

async fn list(ctx: Ctx, Query(q): Query<ListQuery>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            let sql = format!(
                "SELECT {COLS} FROM tasks t {JOINS}
                 WHERE {vis} AND (?1 IS NULL OR t.status = ?1) AND (?2 IS NULL OR t.assignee_user_id = ?2)
                 ORDER BY (t.status = 'open') DESC, t.due_date IS NULL, t.due_date, t.id",
                vis = task_visible_sql(&actor, "t")
            );
            let assignee = if q.mine == Some(1) { Some(actor.user_id) } else { None };
            Ok(json!({ "items": query_json(c, &sql, params![optional(&q.status), assignee])? }))
        })
        .await?;
    Ok(Json(v))
}

async fn list_for_case(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            policy::require_case(c, &actor, id)?;
            Ok(json!({ "items": query_json(
                c,
                &format!("SELECT {COLS} FROM tasks t {JOINS} WHERE t.case_id = ?1
                          ORDER BY (t.status = 'open') DESC, t.due_date IS NULL, t.due_date, t.id"),
                [id],
            )? }))
        })
        .await?;
    Ok(Json(v))
}

async fn detail(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx.db.read(move |c| require_task(c, &actor, id)).await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ create & edit

#[derive(Deserialize)]
struct CreateReq {
    title: String,
    description: Option<String>,
    assignee_user_id: Option<i64>,
    due_date: Option<String>,
    hearing_id: Option<i64>,
}

async fn create(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<CreateReq>) -> JsonResult {
    let actor = ctx.actor;
    let v =
        ctx.db
            .write(move |tx| {
                policy::require_case_perm(tx, &actor, id, perm::TASK_MANAGE)?;
                if let Some(hid) = req.hearing_id {
                    let belongs: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM hearings WHERE id = ?1 AND case_id = ?2)",
                        params![hid, id],
                        |r| r.get(0),
                    )?;
                    if !belongs {
                        return Err(AppError::validation("That hearing does not belong to this case.")
                            .with_details(json!({ "field": "hearing_id" })));
                    }
                }
                let tid = insert_task(
                    tx,
                    &actor,
                    &NewTask {
                        case_id: Some(id),
                        intake_id: None,
                        hearing_id: req.hearing_id,
                        kind: "general".into(),
                        title: req.title.clone(),
                        description: optional(&req.description),
                        assignee_user_id: req.assignee_user_id,
                        due_date: req.due_date.clone(),
                    },
                )?;
                task_json(tx, tid)
            })
            .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct UpdateReq {
    version: i64,
    title: Option<String>,
    description: Option<String>,
    assignee_user_id: Option<i64>,
    due_date: Option<String>,
}

async fn update(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<UpdateReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let current = require_task(tx, &actor, id)?;
            actor.require(perm::TASK_MANAGE)?;
            if current["status"].as_str() != Some("open") {
                return Err(AppError::invalid_transition("Only an open task can be edited."));
            }
            if current["version"].as_i64() != Some(req.version) {
                return Err(AppError::version_conflict(current));
            }
            let title = req.title.as_deref().map(|t| required(t, "Title")).transpose()?;
            if let Some(uid) = req.assignee_user_id {
                check_assignee(tx, uid, current["case_id"].as_i64())?;
            }
            tx.execute(
                "UPDATE tasks SET title = COALESCE(?2, title),
                        description = CASE WHEN ?3 IS NULL THEN description ELSE NULLIF(?3, '') END,
                        assignee_user_id = COALESCE(?4, assignee_user_id),
                        due_date = CASE WHEN ?6 THEN ?5 ELSE due_date END, version = version + 1
                 WHERE id = ?1",
                params![
                    id,
                    title,
                    req.description.as_deref().map(str::trim),
                    req.assignee_user_id,
                    crate::time::parse_opt_date(req.due_date.as_deref())?,
                    req.due_date.is_some()
                ],
            )?;
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "task.updated",
                    "task",
                    id,
                    format!("Task updated: {}", current["title"].as_str().unwrap_or_default()),
                )
                .case(current["case_id"].as_i64())
                .details(json!({ "before": current })),
            )?;
            task_json(tx, id)
        })
        .await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ transitions

/// Open task the actor may see and touch, else 404/403/409.
fn load_open(tx: &Connection, actor: &Actor, id: i64) -> AppResult<Value> {
    let t = require_task(tx, actor, id)?;
    require_touch(actor, &t)?;
    if t["status"].as_str() != Some("open") {
        return Err(AppError::invalid_transition(format!(
            "This task is already {}.",
            t["status"].as_str().unwrap_or_default().replace('_', " ")
        )));
    }
    Ok(t)
}

fn close_task(tx: &Connection, actor: &Actor, id: i64, status: &str, result: Option<&str>, why: Option<&str>) -> AppResult<()> {
    tx.execute(
        "UPDATE tasks SET status = ?2, result = ?3, status_reason = ?4, closed_by = ?5, closed_at = ?6, version = version + 1 WHERE id = ?1",
        params![id, status, result, why, actor.user_id, crate::time::now_utc()],
    )?;
    Ok(())
}

#[derive(Deserialize)]
struct ResultReq {
    result: Option<String>,
}

async fn complete(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<ResultReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let t = load_open(tx, &actor, id)?;
            let result = required(req.result.as_deref().unwrap_or(""), "Result")?;
            close_task(tx, &actor, id, "done", Some(&result), None)?;
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "task.completed",
                    "task",
                    id,
                    format!("Task done: {}", t["title"].as_str().unwrap_or_default()),
                )
                .case(t["case_id"].as_i64())
                .details(json!({ "result": result })),
            )?;
            task_json(tx, id)
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct ReasonReq {
    reason: Option<String>,
}

async fn cancel(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<ReasonReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let t = load_open(tx, &actor, id)?;
            let why = reason(&req.reason)?;
            close_task(tx, &actor, id, "cancelled", None, Some(&why))?;
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "task.cancelled",
                    "task",
                    id,
                    format!("Task cancelled: {}", t["title"].as_str().unwrap_or_default()),
                )
                .case(t["case_id"].as_i64())
                .details(json!({ "reason": why })),
            )?;
            task_json(tx, id)
        })
        .await?;
    Ok(Json(v))
}

/// Carried forward: the task stays on record as deliberately left for later work (reason required).
async fn carry_forward(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<ReasonReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let t = load_open(tx, &actor, id)?;
            let why = reason(&req.reason)?;
            close_task(tx, &actor, id, "carried_forward", None, Some(&why))?;
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "task.carried_forward",
                    "task",
                    id,
                    format!("Task carried forward: {}", t["title"].as_str().unwrap_or_default()),
                )
                .case(t["case_id"].as_i64())
                .details(json!({ "reason": why })),
            )?;
            task_json(tx, id)
        })
        .await?;
    Ok(Json(v))
}
