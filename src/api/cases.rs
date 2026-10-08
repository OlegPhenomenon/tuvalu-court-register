//! C03 case card, C04 participants, C05 assignments, C13 closing/reopening, case relations,
//! and the server-computed "what happens next" messages.

use super::common::{JsonBody, JsonResult, optional, query_json, query_one_json, reason, require_ref, required};
use super::intake::{NewParty, ParticipantInput};
use crate::audit::{self, Event};
use crate::auth::{Actor, Ctx, IdemKey, idempotent};
use crate::error::{AppError, AppResult};
use crate::policy::{self, perm};
use crate::state::AppState;
use axum::extract::{Path, Query};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/cases", get(list))
        .route("/cases/{id}", get(detail).patch(update))
        .route("/cases/{id}/status", post(change_status))
        .route("/cases/{id}/close", post(close))
        .route("/cases/{id}/reopen", post(reopen))
        .route("/cases/{id}/relations", post(add_relation))
        .route("/cases/{id}/participants", post(participant_add))
        .route("/cases/{id}/participants/{pid}/end", post(participant_end))
        .route("/cases/{id}/assignments", post(assignment_add))
        .route("/cases/{id}/assignments/{aid}/end", post(assignment_end))
}

// ------------------------------------------------------------------ creation (used by intake + import)

pub struct NewCase<'a> {
    pub registry_id: i64,
    pub category: &'a str,
    pub title: &'a str,
    pub summary: Option<String>,
    pub registered_date: Option<String>,
    pub restricted: bool,
    pub responsible_user_id: Option<i64>,
}

pub struct CreatedCase {
    pub id: i64,
    pub number: String,
}

/// Allocate the next number in (registry, year). Runs inside the caller's IMMEDIATE transaction;
/// the counter never goes below the highest existing sequence (imports), numbers are never reused,
/// and UNIQUE(registry_id, year, seq) backs this up at the database level.
pub fn allocate_number(tx: &Transaction, registry_id: i64, year: i32) -> AppResult<(i64, String)> {
    let series: String = tx
        .query_row("SELECT series FROM registries WHERE id = ?1 AND active = 1", [registry_id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| AppError::validation("Choose an active register (number series)."))?;
    tx.execute(
        "INSERT INTO case_number_counters (registry_id, year, last_seq) VALUES (?1, ?2, 0) ON CONFLICT DO NOTHING",
        params![registry_id, year],
    )?;
    let seq: i64 = tx.query_row(
        "UPDATE case_number_counters
            SET last_seq = MAX(last_seq, (SELECT COALESCE(MAX(seq), 0) FROM cases WHERE registry_id = ?1 AND year = ?2)) + 1
          WHERE registry_id = ?1 AND year = ?2
          RETURNING last_seq",
        params![registry_id, year],
        |r| r.get(0),
    )?;
    Ok((seq, format!("{series}-{year}-{seq:04}")))
}

pub fn create_case(tx: &Transaction, actor: &Actor, nc: NewCase) -> AppResult<CreatedCase> {
    let title = required(nc.title, "Title")?;
    require_ref(tx, "case_category", nc.category)?;
    let registered_date = match nc.registered_date.as_deref().map(str::trim) {
        None | Some("") => crate::time::today_local(),
        Some(d) => crate::time::parse_date(d)?,
    };
    if registered_date > crate::time::today_local() {
        return Err(AppError::validation("The registration date cannot be in the future."));
    }
    let year = crate::time::year_of(&registered_date)?;
    let (seq, number) = allocate_number(tx, nc.registry_id, year)?;
    let responsible = nc.responsible_user_id.unwrap_or(actor.user_id);
    let now = crate::time::now_utc();
    tx.execute(
        "INSERT INTO cases (registry_id, year, seq, number, title, category, status, restricted, summary,
                            registered_date, registered_at, registered_by, responsible_user_id, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'registered', ?7, ?8, ?9, ?10, ?11, ?12, ?10)",
        params![nc.registry_id, year, seq, number, title, nc.category, nc.restricted, nc.summary, registered_date, now, actor.user_id, responsible],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO case_status_history (case_id, from_status, to_status, reason, by_user, at, effective_date)
         VALUES (?1, NULL, 'registered', 'Registered', ?2, ?3, ?4)",
        params![id, actor.user_id, now, registered_date],
    )?;
    // The registering clerk keeps access to the case they created; the responsible officer gets it too.
    insert_assignment(tx, actor, id, actor.user_id, "clerk", "Registered the case")?;
    if responsible != actor.user_id {
        insert_assignment(tx, actor, id, responsible, "clerk", "Responsible officer at registration")?;
    }
    Ok(CreatedCase { id, number })
}

fn insert_assignment(tx: &Transaction, actor: &Actor, case_id: i64, user_id: i64, role: &str, why: &str) -> AppResult<i64> {
    let active: bool = tx
        .query_row("SELECT active FROM users WHERE id = ?1", [user_id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| AppError::validation("Unknown user."))?;
    if !active {
        return Err(AppError::validation("This user account is deactivated."));
    }
    if !policy::user_assignable(tx, user_id)? {
        return Err(AppError::validation("This person administers the system and cannot be assigned to cases."));
    }
    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM case_assignments WHERE case_id = ?1 AND user_id = ?2 AND role = ?3 AND end_at IS NULL",
            params![case_id, user_id, role],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    tx.execute(
        "INSERT INTO case_assignments (case_id, user_id, role, reason, assigned_by, start_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![case_id, user_id, role, why, actor.user_id, crate::time::now_utc()],
    )?;
    Ok(tx.last_insert_rowid())
}

pub fn insert_party(tx: &Transaction, actor: &Actor, p: &NewParty) -> AppResult<i64> {
    if !matches!(p.kind.as_str(), "person" | "organisation") {
        return Err(AppError::validation("Party kind must be 'person' or 'organisation'."));
    }
    let name = required(&p.name, "Name")?;
    if let Some(island) = optional(&p.island) {
        require_ref(tx, "origin_island", &island)?;
    }
    tx.execute(
        "INSERT INTO parties (kind, name, contact_email, contact_phone, address, island, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![p.kind, name, optional(&p.contact_email), optional(&p.contact_phone), optional(&p.address), optional(&p.island), actor.user_id, crate::time::now_utc()],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Add a party to a case in a role. Never merges people by name: either an explicit `party_id`
/// or a brand-new party record.
pub fn add_participant(tx: &Transaction, actor: &Actor, case_id: i64, p: &ParticipantInput) -> AppResult<i64> {
    require_ref(tx, "participant_role", &p.role)?;
    let party_id = match (&p.party_id, &p.new_party) {
        (Some(id), None) => tx
            .query_row("SELECT id FROM parties WHERE id = ?1", [id], |r| r.get(0))
            .optional()?
            .ok_or_else(|| AppError::validation("Unknown party."))?,
        (None, Some(np)) => insert_party(tx, actor, np)?,
        _ => return Err(AppError::validation("Give either an existing party or a new party, not both.")),
    };
    if let Some(rep) = p.representative_party_id {
        tx.query_row("SELECT id FROM parties WHERE id = ?1", [rep], |r| r.get::<_, i64>(0))
            .optional()?
            .ok_or_else(|| AppError::validation("Unknown representative."))?;
        if optional(&p.representation_basis).is_none() {
            return Err(AppError::validation("State the basis of representation.").with_details(json!({ "field": "representation_basis" })));
        }
    }
    tx.execute(
        "INSERT INTO case_participations (case_id, party_id, role, representative_party_id, representation_basis, service_contact, added_by, added_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![case_id, party_id, p.role, p.representative_party_id, optional(&p.representation_basis), optional(&p.service_contact), actor.user_id, crate::time::now_utc()],
    )?;
    Ok(tx.last_insert_rowid())
}

// ------------------------------------------------------------------ list & detail

#[derive(Deserialize)]
struct ListQuery {
    q: Option<String>,
    status: Option<String>,
    category: Option<String>,
    responsible: Option<i64>,
    judge: Option<i64>,
    from: Option<String>,
    to: Option<String>,
    ids: Option<String>,
}

async fn list(ctx: Ctx, Query(q): Query<ListQuery>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            let ids: Option<String> = optional(&q.ids).map(|s| {
                s.split(',').filter_map(|x| x.trim().parse::<i64>().ok()).map(|n| n.to_string()).collect::<Vec<_>>().join(",")
            });
            let sql = format!(
                "SELECT c.id, c.number, c.legacy_number, c.title, c.category, c.status, c.restricted, c.registered_date,
                        c.closed_date, c.historical_incomplete, u.display_name AS responsible_name,
                        (SELECT GROUP_CONCAT(p.name, '; ') FROM case_participations cp JOIN parties p ON p.id = cp.party_id
                          WHERE cp.case_id = c.id AND cp.active = 1) AS parties,
                        (SELECT ju.display_name FROM case_assignments a JOIN users ju ON ju.id = a.user_id
                          WHERE a.case_id = c.id AND a.role = 'judge' AND a.end_at IS NULL LIMIT 1) AS judge_name,
                        (SELECT MIN(h.starts_at) FROM hearings h WHERE h.case_id = c.id AND h.status = 'scheduled') AS next_hearing_at,
                        EXISTS (SELECT 1 FROM decisions d WHERE d.case_id = c.id AND d.status = 'finalised') AS has_final_decision
                 FROM cases c LEFT JOIN users u ON u.id = c.responsible_user_id
                 WHERE {vis}
                   AND (?1 IS NULL OR c.number LIKE ?1 OR c.legacy_number LIKE ?1 OR c.title LIKE ?1
                        OR EXISTS (SELECT 1 FROM case_participations cp JOIN parties p ON p.id = cp.party_id
                                   WHERE cp.case_id = c.id AND p.name LIKE ?1))
                   AND (?2 IS NULL OR c.status = ?2)
                   AND (?3 IS NULL OR c.category = ?3)
                   AND (?4 IS NULL OR c.responsible_user_id = ?4)
                   AND (?5 IS NULL OR EXISTS (SELECT 1 FROM case_assignments a WHERE a.case_id = c.id AND a.role = 'judge' AND a.user_id = ?5 AND a.end_at IS NULL))
                   AND (?6 IS NULL OR c.registered_date >= ?6)
                   AND (?7 IS NULL OR c.registered_date <= ?7)
                   {ids}
                 ORDER BY c.registered_date DESC, c.id DESC LIMIT 1000",
                vis = policy::case_visible_sql(&actor, "c.id"),
                ids = ids.map(|s| if s.is_empty() { " AND 0".into() } else { format!(" AND c.id IN ({s})") }).unwrap_or_default()
            );
            let like = optional(&q.q).map(|s| format!("%{s}%"));
            let mut items = query_json(
                c,
                &sql,
                params![
                    like,
                    optional(&q.status),
                    optional(&q.category),
                    q.responsible,
                    q.judge,
                    crate::time::parse_opt_date(q.from.as_deref())?,
                    crate::time::parse_opt_date(q.to.as_deref())?
                ],
            )?;
            for it in &mut items {
                if let Some(t) = it["next_hearing_at"].as_str() {
                    it["next_hearing_local"] = json!(crate::time::utc_to_local(t));
                }
            }
            Ok(json!({ "items": items }))
        })
        .await?;
    Ok(Json(v))
}

/// Full case card (workspace header + summary tab). Other tabs come from their own modules.
pub fn case_json(c: &Connection, actor: &Actor, id: i64) -> AppResult<Value> {
    policy::require_case(c, actor, id)?;
    let mut case = query_one_json(
        c,
        "SELECT c.*, r.series, r.name AS registry_name, ru.display_name AS registered_by_name,
                u.display_name AS responsible_name, cu.display_name AS closed_by_name
         FROM cases c JOIN registries r ON r.id = c.registry_id
         LEFT JOIN users ru ON ru.id = c.registered_by
         LEFT JOIN users u ON u.id = c.responsible_user_id
         LEFT JOIN users cu ON cu.id = c.closed_by
         WHERE c.id = ?1",
        [id],
    )?;
    case["category_label"] = json!(super::common::ref_label(c, "case_category", case["category"].as_str().unwrap_or_default())?);
    let rel_vis_to = policy::case_visible_sql(actor, "r.to_case_id");
    let rel_vis_from = policy::case_visible_sql(actor, "r.from_case_id");
    Ok(json!({
        "case": case,
        "participants": query_json(c,
            "SELECT cp.id, cp.party_id, p.kind, p.name, p.contact_email, p.contact_phone, p.address, cp.role, cp.active,
                    cp.representative_party_id, rp.name AS representative_name, cp.representation_basis, cp.service_contact,
                    cp.added_at, cp.ended_at, cp.end_reason
             FROM case_participations cp JOIN parties p ON p.id = cp.party_id
             LEFT JOIN parties rp ON rp.id = cp.representative_party_id
             WHERE cp.case_id = ?1 ORDER BY cp.active DESC, cp.id", [id])?,
        "assignments": query_json(c,
            "SELECT a.id, a.user_id, u.display_name, u.title, a.role, a.reason, a.start_at, a.end_at, a.end_reason,
                    ab.display_name AS assigned_by_name, eb.display_name AS ended_by_name
             FROM case_assignments a JOIN users u ON u.id = a.user_id
             LEFT JOIN users ab ON ab.id = a.assigned_by LEFT JOIN users eb ON eb.id = a.ended_by
             WHERE a.case_id = ?1 ORDER BY (a.end_at IS NULL) DESC, a.id", [id])?,
        "relations": query_json(c, &format!(
            "SELECT r.id, r.kind, r.note, r.created_at, 'outgoing' AS direction, oc.id AS other_case_id, oc.number AS other_number, oc.title AS other_title
               FROM case_relations r JOIN cases oc ON oc.id = r.to_case_id WHERE r.from_case_id = ?1 AND {rel_vis_to}
             UNION ALL
             SELECT r.id, r.kind, r.note, r.created_at, 'incoming', oc.id, oc.number, oc.title
               FROM case_relations r JOIN cases oc ON oc.id = r.from_case_id WHERE r.to_case_id = ?1 AND {rel_vis_from}"), [id])?,
        "status_history": query_json(c,
            "SELECT h.from_status, h.to_status, h.reason, h.basis, h.at, h.effective_date, u.display_name AS by_name
             FROM case_status_history h LEFT JOIN users u ON u.id = h.by_user WHERE h.case_id = ?1 ORDER BY h.id", [id])?,
        "intakes": query_json(c,
            "SELECT id, reference, received_date, sender_name, description, parent_intake_id FROM intakes WHERE case_id = ?1 ORDER BY id", [id])?,
        "decision_state": query_one_json(c,
            "SELECT EXISTS (SELECT 1 FROM decisions WHERE case_id = ?1 AND status = 'finalised') AS has_final_decision,
                    (SELECT COUNT(*) FROM decisions WHERE case_id = ?1 AND status = 'draft') AS draft_decisions", [id])?,
        "next_actions": next_actions(c, actor, id)?,
        "allowed": allowed(c, actor, id, case["status"].as_str().unwrap_or_default())?,
    }))
}

fn allowed(c: &Connection, actor: &Actor, id: i64, status: &str) -> AppResult<Value> {
    let open = status != "closed";
    let assigned_judge = policy::is_assigned(c, actor, id, Some("judge"))?;
    Ok(json!({
        "edit": open && actor.has(perm::CASE_EDIT),
        "assign_staff": actor.has(perm::CASE_ASSIGN_STAFF),
        "assign_judge": actor.has(perm::CASE_ASSIGN_JUDGE),
        "close": open && actor.has(perm::CASE_CLOSE),
        "reopen": status == "closed" && actor.has(perm::CASE_REOPEN),
        "set_status": open && actor.has(perm::CASE_EDIT),
        "schedule_hearing": open && actor.has(perm::HEARING_SCHEDULE),
        "record_outcome": actor.has(perm::HEARING_RECORD_OUTCOME),
        "manage_documents": actor.has(perm::DOCUMENT_MANAGE),
        "draft_decision": actor.has(perm::DECISION_DRAFT) && (assigned_judge || !actor.is_judge),
        "finalise_decision": actor.has(perm::DECISION_FINALISE),
        "dispatch": actor.has(perm::DISPATCH_MANAGE),
        "manage_tasks": actor.has(perm::TASK_MANAGE),
        "export": actor.has(perm::EXPORT_CASE),
        "grant_restricted": actor.has(perm::DOCUMENT_GRANT_RESTRICTED),
    }))
}

async fn detail(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    Ok(Json(ctx.db.read(move |c| case_json(c, &actor, id)).await?))
}

// ------------------------------------------------------------------ next actions

fn action(code: &str, message: String, link: String) -> Value {
    json!({ "code": code, "message": message, "link": link })
}

/// Plain-language next steps for a case, computed from its records (spec §5: explain the next step,
/// not a status code). Only includes things the actor can see.
pub fn next_actions(c: &Connection, actor: &Actor, case_id: i64) -> AppResult<Vec<Value>> {
    let base = format!("/cases/{case_id}");
    let mut out = Vec::new();
    let status: String = c.query_row("SELECT status FROM cases WHERE id = ?1", [case_id], |r| r.get(0))?;
    if status == "closed" {
        return Ok(out);
    }
    if status == "reopened" {
        out.push(action("decide_after_reopen", "The case was reopened — record the next step (set it active or on hold).".into(), format!("{base}?tab=summary")));
    }
    let has_judge: bool = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM case_assignments WHERE case_id = ?1 AND role = 'judge' AND end_at IS NULL)",
        [case_id],
        |r| r.get(0),
    )?;
    if !has_judge {
        out.push(action("assign_judge", "Assign a judge to this case.".into(), format!("{base}?tab=summary&action=assign-judge")));
    }
    let now = crate::time::now_utc();
    // Hearings that ended but have no outcome.
    for h in query_json(
        c,
        "SELECT id, starts_at FROM hearings WHERE case_id = ?1 AND status = 'scheduled' AND ends_at <= ?2 ORDER BY starts_at",
        params![case_id, now],
    )? {
        let when = crate::time::utc_to_local(h["starts_at"].as_str().unwrap_or_default());
        out.push(action("record_outcome", format!("Record the outcome of the hearing on {when}."), format!("{base}?tab=hearings&hearing={}", h["id"])));
    }
    let has_hearing: bool = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM hearings WHERE case_id = ?1 AND status IN ('scheduled','held'))",
        [case_id],
        |r| r.get(0),
    )?;
    let has_final: bool =
        c.query_row("SELECT EXISTS (SELECT 1 FROM decisions WHERE case_id = ?1 AND status = 'finalised')", [case_id], |r| r.get(0))?;
    if !has_hearing && !has_final && has_judge && matches!(status.as_str(), "registered" | "active") {
        out.push(action("schedule_hearing", "Schedule the first hearing.".into(), format!("{base}?tab=hearings")));
    }
    for d in query_json(c, "SELECT d.id, d.kind, d.recipient_name, d.subject, d.status, h.starts_at AS hearing_at FROM dispatches d LEFT JOIN hearings h ON h.id = d.hearing_id WHERE d.case_id = ?1 AND d.status IN ('draft','failed','sent') ORDER BY d.id", [case_id])? {
        let who = d["recipient_name"].as_str().unwrap_or_default();
        let what = match d["kind"].as_str() {
            Some("notice") => "notice",
            Some("copies") => "copy package",
            _ => "message",
        };
        let link = format!("{base}?tab=dispatch&dispatch={}", d["id"]);
        match d["status"].as_str() {
            Some("draft") => out.push(action("review_dispatch", format!("Check the recipient and contents of the {what} for {who}, then send it."), link.clone())),
            Some("failed") => out.push(action("retry_dispatch", format!("Delivery of the {what} to {who} failed — retry or use another method."), link.clone())),
            Some("sent") => {
                let confirmed: bool = c.query_row(
                    "SELECT EXISTS (SELECT 1 FROM delivery_confirmations WHERE dispatch_id = ?1 AND kind = 'human_handover')",
                    [d["id"].as_i64()],
                    |r| r.get(0),
                )?;
                if !confirmed {
                    let message = if let Some(at) = d["hearing_at"].as_str() {
                        let date = crate::time::human_court_local(&crate::time::utc_to_local(at), true);
                        format!("Confirm that the hearing notice for {date} reached {who}.")
                    } else {
                        format!("Confirm that the {what} “{}” reached {who}.", d["subject"].as_str().unwrap_or_default())
                    };
                    out.push(action("confirm_delivery", message, link));
                }
            }
            _ => {}
        }
    }
    for d in query_json(c, "SELECT title FROM decisions WHERE case_id = ?1 AND status = 'draft'", [case_id])? {
        out.push(action("finalise_decision", format!("Finalise or withdraw the draft decision “{}”.", d["title"].as_str().unwrap_or_default()), format!("{base}?tab=decisions")));
    }
    if has_final {
        // Each party needs each current finalised decision, with its exact bound version.
        for p in query_json(
            c,
            &format!("SELECT DISTINCT p.id AS party_id, p.name, dc.id AS decision_id, dc.title
             FROM decisions dc JOIN case_participations cp ON cp.case_id = dc.case_id
             JOIN parties p ON p.id = cp.party_id
             JOIN document_versions v ON v.id = dc.document_version_id JOIN documents doc ON doc.id = v.document_id
             WHERE dc.case_id = ?1 AND dc.status = 'finalised' AND cp.active = 1
               AND cp.role IN ('claimant','respondent','applicant','defendant') AND {visible}
               AND NOT EXISTS (SELECT 1 FROM dispatches dp JOIN dispatch_items di ON di.dispatch_id = dp.id
                 WHERE dp.case_id = ?1 AND dp.kind = 'copies' AND dp.recipient_party_id = p.id
                   AND dp.status <> 'cancelled' AND di.document_version_id = dc.document_version_id)
             ORDER BY dc.id, p.id", visible = policy::document_visible_sql(actor, "doc")),
            [case_id],
        )? {
            out.push(action("send_decision", format!("Send a copy of the decision “{}” to {}.", p["title"].as_str().unwrap_or_default(), p["name"].as_str().unwrap_or_default()),
                format!("{base}?tab=dispatch&action=copies&decision={}&party={}", p["decision_id"], p["party_id"])));
        }
    }
    for t in query_json(
        c,
        "SELECT t.title, t.due_date, u.display_name AS assignee FROM tasks t LEFT JOIN users u ON u.id = t.assignee_user_id
         WHERE t.case_id = ?1 AND t.status = 'open' ORDER BY t.due_date IS NULL, t.due_date, t.id",
        [case_id],
    )? {
        let who = t["assignee"].as_str().map(|a| format!(" ({a})")).unwrap_or_default();
        let due = t["due_date"].as_str().map(|d| format!(", due {d}")).unwrap_or_default();
        out.push(action("task", format!("Task: {}{who}{due}.", t["title"].as_str().unwrap_or_default()), format!("{base}?tab=tasks")));
    }
    if out.is_empty() && has_final && actor.has(perm::CASE_CLOSE) {
        out.push(action("ready_to_close", "Nothing is left open. The case can be closed with a basis.".into(), format!("{base}?tab=summary")));
    }
    Ok(out)
}

// ------------------------------------------------------------------ updates

#[derive(Deserialize)]
struct UpdateReq {
    version: i64,
    title: Option<String>,
    category: Option<String>,
    summary: Option<String>,
    responsible_user_id: Option<i64>,
    restricted: Option<bool>,
}

async fn update(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<UpdateReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case = policy::require_case_perm(tx, &actor, id, perm::CASE_EDIT)?;
            let before = query_one_json(tx, "SELECT title, category, summary, responsible_user_id, restricted, version FROM cases WHERE id = ?1", [id])?;
            if case.version != req.version {
                return Err(AppError::version_conflict(before));
            }
            if case.status == "closed" {
                return Err(AppError::invalid_transition("Reopen the case before changing it."));
            }
            if let Some(cat) = &req.category {
                require_ref(tx, "case_category", cat)?;
            }
            let title = match &req.title {
                Some(t) => Some(required(t, "Title")?),
                None => None,
            };
            tx.execute(
                "UPDATE cases SET title = COALESCE(?2, title), category = COALESCE(?3, category),
                        summary = CASE WHEN ?4 IS NULL THEN summary ELSE NULLIF(?4, '') END,
                        responsible_user_id = COALESCE(?5, responsible_user_id), restricted = COALESCE(?6, restricted),
                        version = version + 1, updated_at = ?7
                 WHERE id = ?1",
                params![id, title, req.category, req.summary, req.responsible_user_id, req.restricted, crate::time::now_utc()],
            )?;
            if let Some(uid) = req.responsible_user_id {
                insert_assignment(tx, &actor, id, uid, "clerk", "Made responsible officer")?;
            }
            audit::record(
                tx,
                Some(&actor),
                Event::new("case.updated", "case", id, format!("Case {} details changed", case.number))
                    .case(Some(id))
                    .details(json!({ "before": before })),
            )?;
            case_json(tx, &actor, id)
        })
        .await?;
    Ok(Json(v))
}

fn record_status(tx: &Transaction, actor: &Actor, id: i64, from: &str, to: &str, why: Option<&str>, basis: Option<&str>, date: &str) -> AppResult<()> {
    tx.execute(
        "UPDATE cases SET status = ?2, version = version + 1, updated_at = ?3 WHERE id = ?1",
        params![id, to, crate::time::now_utc()],
    )?;
    tx.execute(
        "INSERT INTO case_status_history (case_id, from_status, to_status, reason, basis, by_user, at, effective_date)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![id, from, to, why, basis, actor.user_id, crate::time::now_utc(), date],
    )?;
    Ok(())
}

#[derive(Deserialize)]
struct StatusReq {
    to: String,
    reason: Option<String>,
}

async fn change_status(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<StatusReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case = policy::require_case_perm(tx, &actor, id, perm::CASE_EDIT)?;
            let ok = matches!(
                (case.status.as_str(), req.to.as_str()),
                ("registered", "active") | ("registered", "on_hold") | ("active", "on_hold") | ("on_hold", "active") | ("reopened", "active") | ("reopened", "on_hold")
            );
            if !ok {
                return Err(AppError::invalid_transition(format!("A case that is '{}' cannot be set to '{}'.", case.status, req.to)));
            }
            let why = if req.to == "on_hold" { Some(reason(&req.reason)?) } else { optional(&req.reason) };
            record_status(tx, &actor, id, &case.status, &req.to, why.as_deref(), None, &crate::time::today_local())?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("case.status_changed", "case", id, format!("Case {} is now {}", case.number, req.to.replace('_', " ")))
                    .case(Some(id))
                    .details(json!({ "from": case.status, "to": req.to, "reason": why })),
            )?;
            case_json(tx, &actor, id)
        })
        .await?;
    Ok(Json(v))
}

/// Items that block closing: must be done, cancelled with a reason, or carried forward explicitly.
pub fn open_items(c: &Connection, case_id: i64) -> AppResult<Vec<Value>> {
    let mut items = query_json(
        c,
        "SELECT 'task' AS kind, id, title AS label, status, NULL AS dispatch_kind, NULL AS recipient, NULL AS hearing_starts
         FROM tasks WHERE case_id = ?1 AND status = 'open'
         UNION ALL
         SELECT 'hearing', id, hearing_type || ' at ' || starts_at, status, NULL, NULL, NULL
         FROM hearings WHERE case_id = ?1 AND status IN ('draft','scheduled')
         UNION ALL
         SELECT 'dispatch', d.id, '', d.status, d.kind, d.recipient_name, h.starts_at
         FROM dispatches d LEFT JOIN hearings h ON h.id = d.hearing_id
         WHERE d.case_id = ?1 AND d.status IN ('draft','queued','failed')
         UNION ALL
         SELECT 'decision', id, title, status, NULL, NULL, NULL FROM decisions WHERE case_id = ?1 AND status = 'draft'
         UNION ALL
         SELECT 'unconfirmed_dispatch', d.id, '', d.status, d.kind, d.recipient_name, h.starts_at
         FROM dispatches d LEFT JOIN hearings h ON h.id = d.hearing_id
         WHERE d.case_id = ?1 AND d.status = 'sent'
           AND NOT EXISTS (SELECT 1 FROM delivery_confirmations dc WHERE dc.dispatch_id = d.id AND dc.kind = 'human_handover')",
        [case_id],
    )?;
    for it in &mut items {
        if it["kind"] == "hearing"
            && let Some(l) = it["label"].as_str()
            && let Some((t, at)) = l.split_once(" at ")
        {
            it["label"] = json!(format!("{t} at {}", crate::time::utc_to_local(at)));
        }
        if matches!(it["kind"].as_str(), Some("dispatch") | Some("unconfirmed_dispatch")) {
            let recipient = it["recipient"].as_str().unwrap_or_default();
            let base = match it["dispatch_kind"].as_str() {
                Some("copies") => "Copy package".to_string(),
                Some("information_request") => "Information request".to_string(),
                _ => match it["hearing_starts"].as_str() {
                    Some(starts) => format!(
                        "Hearing notice for {}",
                        crate::time::human_court_local(&crate::time::utc_to_local(starts), true)
                    ),
                    None => "Notice".to_string(),
                },
            };
            it["label"] = json!(format!("{base} → {recipient}"));
        }
        if let Some(o) = it.as_object_mut() {
            o.remove("dispatch_kind");
            o.remove("recipient");
            o.remove("hearing_starts");
        }
    }
    Ok(items)
}

#[derive(Deserialize, Serialize)]
struct Acknowledgement {
    kind: String,
    id: i64,
    #[serde(default)]
    reason: String,
}

#[derive(Deserialize, Serialize)]
struct CloseReq {
    basis: String,
    note: Option<String>,
    closed_date: Option<String>,
    #[serde(default)]
    acknowledge: Vec<Acknowledgement>,
}

async fn close(ctx: Ctx, Path(id): Path<i64>, IdemKey(key): IdemKey, JsonBody(req): JsonBody<CloseReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case = policy::require_case_perm(tx, &actor, id, perm::CASE_CLOSE)?;
            idempotent(tx, &actor, &key, "case.close", &(id, &req), || {
                if !matches!(case.status.as_str(), "registered" | "active" | "on_hold" | "reopened") {
                    return Err(AppError::invalid_transition("This case is already closed."));
                }
                require_ref(tx, "closure_basis", &req.basis)?;
                let mut note = optional(&req.note);
                if req.basis == "other" && note.is_none() {
                    return Err(AppError::validation("Explain the basis for closing.").with_details(json!({ "field": "note" })));
                }
                let mut blockers = open_items(tx, id)?;
                let mut acknowledged = Vec::new();
                for ack in &req.acknowledge {
                    let position = blockers.iter().position(|item| item["kind"] == "unconfirmed_dispatch"
                        && ack.kind == "unconfirmed_dispatch" && item["id"].as_i64() == Some(ack.id))
                        .ok_or_else(|| AppError::validation("Only an unconfirmed dispatch on this case can be acknowledged, once."))?;
                    if ack.reason.trim().is_empty() {
                        continue; // An unexplained dispatch stays in the 409 open-items response.
                    }
                    let why = required(&ack.reason, "Reason")?;
                    let item = blockers.remove(position);
                    let line = format!("Left unconfirmed: {} — {why}", item["label"].as_str().unwrap_or_default());
                    note = Some(match note { Some(n) => format!("{n}\n{line}"), None => line });
                    acknowledged.push(json!({"kind": ack.kind, "id": ack.id, "reason": why}));
                }
                if !blockers.is_empty() {
                    return Err(AppError::conflict(
                        "open_items",
                        "Some actions are still open. Complete them, cancel them with a reason, carry tasks forward, or acknowledge unconfirmed dispatches with a reason.",
                    )
                    .with_details(json!({ "items": blockers })));
                }
                let date = match req.closed_date.as_deref() {
                    Some(d) if !d.trim().is_empty() => crate::time::parse_date(d)?,
                    _ => crate::time::today_local(),
                };
                record_status(tx, &actor, id, &case.status, "closed", note.as_deref(), Some(&req.basis), &date)?;
                tx.execute(
                    "UPDATE cases SET closure_basis = ?2, closure_note = ?3, closed_date = ?4, closed_at = ?5, closed_by = ?6 WHERE id = ?1",
                    params![id, req.basis, note, date, crate::time::now_utc(), actor.user_id],
                )?;
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new("case.closed", "case", id, format!("Case {} closed ({})", case.number, req.basis))
                        .case(Some(id))
                        .details(json!({ "basis": req.basis, "note": note, "closed_date": date, "acknowledge": acknowledged })),
                )?;
                Ok(json!({ "ok": true, "closed_date": date }))
            })
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct ReasonReq {
    reason: Option<String>,
}

async fn reopen(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<ReasonReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case = policy::require_case_perm(tx, &actor, id, perm::CASE_REOPEN)?;
            if case.status != "closed" {
                return Err(AppError::invalid_transition("Only a closed case can be reopened."));
            }
            let why = reason(&req.reason)?;
            record_status(tx, &actor, id, "closed", "reopened", Some(&why), None, &crate::time::today_local())?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("case.reopened", "case", id, format!("Case {} reopened", case.number))
                    .case(Some(id))
                    .details(json!({ "reason": why })),
            )?;
            case_json(tx, &actor, id)
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct RelationReq {
    to_case_id: i64,
    kind: String,
    note: Option<String>,
}

async fn add_relation(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<RelationReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case = policy::require_case_perm(tx, &actor, id, perm::CASE_EDIT)?;
            let other = policy::require_case(tx, &actor, req.to_case_id)?;
            require_ref(tx, "relation_kind", &req.kind)?;
            tx.execute(
                "INSERT INTO case_relations (from_case_id, to_case_id, kind, note, created_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, other.id, req.kind, optional(&req.note), actor.user_id, crate::time::now_utc()],
            )?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("case.related", "case", id, format!("Case {} linked to {} ({})", case.number, other.number, req.kind)).case(Some(id)).details(json!({"related_case_id": other.id})),
            )?;
            case_json(tx, &actor, id)
        })
        .await?;
    Ok(Json(v))
}

async fn participant_add(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<ParticipantInput>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case = policy::require_case_perm(tx, &actor, id, perm::CASE_EDIT)?;
            let pid = add_participant(tx, &actor, id, &req)?;
            let name: String = tx.query_row(
                "SELECT p.name FROM case_participations cp JOIN parties p ON p.id = cp.party_id WHERE cp.id = ?1",
                [pid],
                |r| r.get(0),
            )?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("case.participant_added", "case", id, format!("{name} added to {} as {}", case.number, req.role)).case(Some(id)),
            )?;
            case_json(tx, &actor, id)
        })
        .await?;
    Ok(Json(v))
}

async fn participant_end(ctx: Ctx, Path((id, pid)): Path<(i64, i64)>, JsonBody(req): JsonBody<ReasonReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case = policy::require_case_perm(tx, &actor, id, perm::CASE_EDIT)?;
            let why = reason(&req.reason)?;
            let n = tx.execute(
                "UPDATE case_participations SET active = 0, ended_at = ?3, end_reason = ?4 WHERE id = ?1 AND case_id = ?2 AND active = 1",
                params![pid, id, crate::time::now_utc(), why],
            )?;
            if n == 0 {
                return Err(AppError::not_found());
            }
            audit::record(
                tx,
                Some(&actor),
                Event::new("case.participant_ended", "case", id, format!("A participation in {} ended", case.number))
                    .case(Some(id))
                    .details(json!({ "participation_id": pid, "reason": why })),
            )?;
            case_json(tx, &actor, id)
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct AssignReq {
    user_id: i64,
    role: String,
    reason: Option<String>,
}

fn require_assign_perm(actor: &Actor, role: &str) -> AppResult<()> {
    match role {
        "judge" => actor.require(perm::CASE_ASSIGN_JUDGE),
        "clerk" | "service_officer" | "registry_head" | "other" => actor.require(perm::CASE_ASSIGN_STAFF),
        _ => Err(AppError::validation("Unknown assignment role.")),
    }
}

async fn assignment_add(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<AssignReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case = policy::require_case(tx, &actor, id)?;
            require_assign_perm(&actor, &req.role)?;
            let why = reason(&req.reason)?;
            let (name, is_judge): (String, bool) = tx
                .query_row("SELECT display_name, is_judge FROM users WHERE id = ?1", [req.user_id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .ok_or_else(|| AppError::validation("Unknown user."))?;
            if req.role == "judge" && !is_judge {
                return Err(AppError::validation(format!("{name} is not registered as a judicial officer.")));
            }
            insert_assignment(tx, &actor, id, req.user_id, &req.role, &why)?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("case.assigned", "case", id, format!("{name} assigned to {} as {}", case.number, req.role.replace('_', " ")))
                    .case(Some(id))
                    .details(json!({ "user_id": req.user_id, "role": req.role, "reason": why })),
            )?;
            case_json(tx, &actor, id)
        })
        .await?;
    Ok(Json(v))
}

/// Ending an assignment removes access immediately (all checks are per request, including old file URLs).
async fn assignment_end(ctx: Ctx, Path((id, aid)): Path<(i64, i64)>, JsonBody(req): JsonBody<ReasonReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case = policy::require_case(tx, &actor, id)?;
            let (role, user_id, name): (String, i64, String) = tx
                .query_row(
                    "SELECT a.role, a.user_id, u.display_name FROM case_assignments a JOIN users u ON u.id = a.user_id
                     WHERE a.id = ?1 AND a.case_id = ?2 AND a.end_at IS NULL",
                    params![aid, id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?
                .ok_or_else(AppError::not_found)?;
            require_assign_perm(&actor, &role)?;
            let why = reason(&req.reason)?;
            tx.execute(
                "UPDATE case_assignments SET end_at = ?2, ended_by = ?3, end_reason = ?4 WHERE id = ?1",
                params![aid, crate::time::now_utc(), actor.user_id, why],
            )?;
            // Report residual access so the person ending the assignment sees what remains.
            let remaining = query_json(
                tx,
                "SELECT role FROM case_assignments WHERE case_id = ?1 AND user_id = ?2 AND end_at IS NULL",
                params![id, user_id],
            )?;
            audit::record(
                tx,
                Some(&actor),
                Event::new("case.unassigned", "case", id, format!("{name} no longer assigned to {} as {}", case.number, role.replace('_', " ")))
                    .case(Some(id))
                    .details(json!({ "user_id": user_id, "role": role, "reason": why, "remaining_roles": remaining })),
            )?;
            let mut out = case_json(tx, &actor, id).unwrap_or_else(|_| json!({ "case": null }));
            out["residual_access"] = json!(remaining);
            Ok(out)
        })
        .await?;
    Ok(Json(v))
}
