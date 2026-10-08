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
        .route("/cases/{id}/closing-bases", get(closing_bases))
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

/// Staff responsibility and manual assignments share eligibility checks.
pub(super) fn require_assignment_user(c: &Connection, user_id: i64, role: &str) -> AppResult<()> {
    let (active, is_judge): (bool, bool) = c
        .query_row(
            "SELECT active, is_judge FROM users WHERE id = ?1",
            [user_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| AppError::validation("Unknown user."))?;
    if !active {
        return Err(AppError::validation("This user account is deactivated."));
    }
    if !policy::user_assignable(c, user_id)? {
        return Err(AppError::validation(
            "This person administers the system and cannot be assigned to cases.",
        ));
    }
    if is_judge && role != "judge" {
        return Err(AppError::validation(
            "Assign judicial officers through the judge-assignment action.",
        ));
    }
    if role == "judge" && !is_judge {
        return Err(AppError::validation(
            "This person is not registered as a judicial officer.",
        ));
    }
    Ok(())
}

fn insert_assignment(
    tx: &Transaction,
    actor: &Actor,
    case_id: i64,
    user_id: i64,
    role: &str,
    why: &str,
) -> AppResult<i64> {
    require_assignment_user(tx, user_id, role)?;
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
        (Some(id), None) => { policy::require_party(tx, actor, *id)?; *id },
        (None, Some(np)) => insert_party(tx, actor, np)?,
        _ => return Err(AppError::validation("Give either an existing party or a new party, not both.")),
    };
    if let Some(rep) = p.representative_party_id {
        policy::require_party(tx, actor, rep)?;
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
    redact_closing_basis(c, actor, &mut case)?;
    let rel_vis_to = policy::case_visible_sql(actor, "r.to_case_id");
    let rel_vis_from = policy::case_visible_sql(actor, "r.from_case_id");
    Ok(json!({
        "case": case,
        "participants": query_json(c,
            "SELECT cp.id, cp.party_id, p.kind, p.name, p.contact_email, p.contact_phone, p.address, cp.role, cp.active,
                    cp.representative_party_id, rp.name AS representative_name, cp.representation_basis, cp.service_contact,
                    cp.added_at, cp.ended_at, cp.end_reason, cp.version
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

/// Existing planned work, independent of whether a person needs to act immediately.
pub fn has_next_step(c: &Connection, actor: &Actor, case_id: i64) -> AppResult<bool> {
    policy::require_case(c, actor, case_id)?;
    let sql = format!(
        "SELECT status <> 'closed' AND (
        EXISTS(SELECT 1 FROM hearings WHERE case_id=?1 AND status='scheduled' AND ends_at > ?2)
        OR EXISTS(SELECT 1 FROM tasks WHERE case_id=?1 AND status='open')
        OR EXISTS(SELECT 1 FROM dispatches WHERE case_id=?1 AND status IN ('draft','queued','failed'))
        OR EXISTS(SELECT 1 FROM decisions dc JOIN documents doc ON doc.id=dc.document_id
            WHERE dc.case_id=?1 AND dc.status='draft' AND {})) FROM cases WHERE id=?1",
        policy::document_visible_sql(actor, "doc")
    );
    Ok(c.query_row(&sql, params![case_id, crate::time::now_utc()], |r| r.get(0))?)
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
        out.push(action(
            "decide_after_reopen",
            "The case was reopened — record the next step (set it active or on hold).".into(),
            format!("{base}?tab=summary"),
        ));
    }
    let has_judge: bool = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM case_assignments WHERE case_id = ?1 AND role = 'judge' AND end_at IS NULL)",
        [case_id],
        |r| r.get(0),
    )?;
    if !has_judge {
        out.push(action(
            "assign_judge",
            "Assign a judge to this case.".into(),
            format!("{base}?tab=summary&action=assign-judge"),
        ));
    }
    let now = crate::time::now_utc();
    for h in query_json(
        c,
        "SELECT id, starts_at FROM hearings WHERE case_id=?1 AND status='scheduled' AND ends_at > ?2 ORDER BY starts_at",
        params![case_id, now],
    )? {
        let when = crate::time::utc_to_local(h["starts_at"].as_str().unwrap_or_default());
        out.push(action(
            "scheduled_hearing",
            format!("Next hearing: {when} (court time)."),
            format!("{base}?tab=hearings&hearing={}", h["id"]),
        ));
    }
    if !has_next_step(c, actor, case_id)? {
        out.push(action(
            "plan_next_step",
            "No next step is recorded. Schedule a hearing or record the work still needed.".into(),
            format!("{base}?tab=summary"),
        ));
    }
    // Hearings that ended but have no outcome.
    for h in query_json(
        c,
        "SELECT id, starts_at FROM hearings WHERE case_id = ?1 AND status = 'scheduled' AND ends_at <= ?2 ORDER BY starts_at",
        params![case_id, now],
    )? {
        let when = crate::time::utc_to_local(h["starts_at"].as_str().unwrap_or_default());
        out.push(action(
            "record_outcome",
            format!("Record the outcome of the hearing on {when}."),
            format!("{base}?tab=hearings&hearing={}", h["id"]),
        ));
    }
    let has_hearing: bool = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM hearings WHERE case_id = ?1 AND status IN ('scheduled','held'))",
        [case_id],
        |r| r.get(0),
    )?;
    let has_final: bool = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM decisions WHERE case_id = ?1 AND status = 'finalised')",
        [case_id],
        |r| r.get(0),
    )?;
    if !has_hearing && !has_final && has_judge && matches!(status.as_str(), "registered" | "active") {
        out.push(action("schedule_hearing", "Schedule the first hearing.".into(), format!("{base}?tab=hearings")));
    }
    for d in query_json(
        c,
        "SELECT d.id, d.kind, d.recipient_name, d.subject, d.status, h.starts_at AS hearing_at FROM dispatches d LEFT JOIN hearings h ON h.id = d.hearing_id WHERE d.case_id = ?1 AND d.status IN ('draft','queued','failed','sent') ORDER BY d.id",
        [case_id],
    )? {
        let who = d["recipient_name"].as_str().unwrap_or_default();
        let what = match d["kind"].as_str() {
            Some("notice") => "notice",
            Some("copies") => "copy package",
            _ => "message",
        };
        let link = format!("{base}?tab=dispatch&dispatch={}", d["id"]);
        match d["status"].as_str() {
            Some("queued") => out.push(action("queued_dispatch", format!("The {what} to {who} is queued for sending."), link.clone())),
            Some("draft") => out.push(action(
                "review_dispatch",
                format!("Check the recipient and contents of the {what} for {who}, then send it."),
                link.clone(),
            )),
            Some("failed") => out.push(action(
                "retry_dispatch",
                format!("Delivery of the {what} to {who} failed — retry or use another method."),
                link.clone(),
            )),
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
    for mut d in query_json(c, "SELECT title, document_version_id FROM decisions WHERE case_id = ?1 AND status = 'draft'", [case_id])? {
        if policy::require_version(c, actor, d["document_version_id"].as_i64().unwrap_or_default()).is_err() {
            d["title"] = json!("Restricted document");
        }
        out.push(action(
            "finalise_decision",
            format!("Finalise or withdraw the draft decision “{}”.", d["title"].as_str().unwrap_or_default()),
            format!("{base}?tab=decisions"),
        ));
    }
    if has_final {
        // Each party needs each current finalised decision, with its exact bound version.
        for p in query_json(
            c,
            &format!(
                "SELECT DISTINCT p.id AS party_id, p.name, dc.id AS decision_id, dc.title
             FROM decisions dc JOIN case_participations cp ON cp.case_id = dc.case_id
             JOIN parties p ON p.id = cp.party_id
             JOIN document_versions v ON v.id = dc.document_version_id JOIN documents doc ON doc.id = v.document_id
             WHERE dc.case_id = ?1 AND dc.status = 'finalised' AND cp.active = 1
               AND cp.role IN ('claimant','respondent','applicant','defendant') AND {visible}
               AND NOT EXISTS (SELECT 1 FROM dispatches dp JOIN dispatch_items di ON di.dispatch_id = dp.id
                 WHERE dp.case_id = ?1 AND dp.kind = 'copies' AND dp.recipient_party_id = p.id
                   AND dp.status <> 'cancelled' AND di.document_version_id = dc.document_version_id)
             ORDER BY dc.id, p.id",
                visible = policy::document_visible_sql(actor, "doc")
            ),
            [case_id],
        )? {
            out.push(action(
                "send_decision",
                format!(
                    "Send a copy of the decision “{}” to {}.",
                    p["title"].as_str().unwrap_or_default(),
                    p["name"].as_str().unwrap_or_default()
                ),
                format!("{base}?tab=dispatch&action=copies&decision={}&party={}", p["decision_id"], p["party_id"]),
            ));
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
        out.push(action(
            "task",
            format!("Task: {}{who}{due}.", t["title"].as_str().unwrap_or_default()),
            format!("{base}?tab=tasks"),
        ));
    }
    if out.iter().all(|a| a["code"] == "plan_next_step") && has_final && actor.has(perm::CASE_CLOSE) {
        out.push(action(
            "ready_to_close",
            "Nothing is left open. The case can be closed with a basis.".into(),
            format!("{base}?tab=summary"),
        ));
    }
    Ok(out)
}

// ------------------------------------------------------------------ updates

#[derive(Deserialize, Serialize)]
struct UpdateReq {
    version: i64,
    title: Option<String>,
    category: Option<String>,
    summary: Option<String>,
    responsible_user_id: Option<i64>,
    assignment_reason: Option<String>,
    restricted: Option<bool>,
}

async fn update(
    ctx: Ctx,
    Path(id): Path<i64>,
    IdemKey(key): IdemKey,
    JsonBody(req): JsonBody<UpdateReq>,
) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case = policy::require_case(tx, &actor, id)?;
            if req.responsible_user_id.is_some() { require_assign_perm(&actor,"clerk")?; }
            if req.title.is_some() || req.category.is_some() || req.summary.is_some() || req.restricted.is_some() || req.responsible_user_id.is_none() { actor.require(perm::CASE_EDIT)?; }
            idempotent(tx, &actor, &key, "case.update", &(id, &req), || {
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
            let mut remaining = None;
            if let Some(uid) = req.responsible_user_id {
                let why = reason(&req.assignment_reason)?;
                require_assignment_user(tx, uid, "clerk")?;
                if before["responsible_user_id"].as_i64() != Some(uid) {
                    if let Some(previous) = before["responsible_user_id"].as_i64() {
                        let ended = tx.execute(
                            "UPDATE case_assignments SET end_at=?3, ended_by=?4, end_reason=?5
                             WHERE case_id=?1 AND user_id=?2 AND role='clerk' AND end_at IS NULL",
                            params![id, previous, crate::time::now_utc(), actor.user_id, why],
                        )?;
                        let roles = query_json(tx,
                            "SELECT role FROM case_assignments WHERE case_id=?1 AND user_id=?2 AND end_at IS NULL",
                            params![id, previous])?;
                        if ended > 0 {
                            audit::record(tx, Some(&actor), Event::new("case.unassigned", "case", id, "Previous responsible officer assignment ended")
                                .case(Some(id)).details(json!({"user_id":previous,"role":"clerk","reason":why,"remaining_roles":roles})))?;
                        }
                        remaining = Some(roles);
                    }
                    insert_assignment(tx, &actor, id, uid, "clerk", &why)?;
                    audit::record(tx,Some(&actor),Event::new("case.assigned","case",id,"Responsible officer assigned").case(Some(id)).details(json!({"user_id":uid,"role":"clerk","reason":why})))?;
                }
            }
            audit::record(
                tx,
                Some(&actor),
                Event::new("case.updated", "case", id, format!("Case {} details changed", case.number))
                    .case(Some(id))
                    .details(json!({ "before": before })),
            )?;
            let visible: bool = tx.query_row(
                &format!("SELECT {}", policy::case_visible_sql(&actor, &id.to_string())),
                [], |r| r.get(0),
            )?;
            let mut out = if remaining.is_some() && !visible { json!({"case":null}) } else { case_json(tx, &actor, id)? };
            if let Some(roles) = remaining {
                out["residual_access"] = json!(roles);
            }
            Ok(out)
            })
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
    version: Option<i64>,
    basis_document_version_id: Option<i64>,
    basis_decision_id: Option<i64>,
    basis_hearing_id: Option<i64>,
    basis: String,
    note: Option<String>,
    closed_date: Option<String>,
    #[serde(default)]
    acknowledge: Vec<Acknowledgement>,
}

/// New closure references inherit their evidence's document visibility, including audit details.
pub(crate) fn redact_closing_basis(c: &Connection, actor: &Actor, value: &mut Value) -> AppResult<()> {
    for (field, target) in [
        ("basis_document_version_id", "SELECT document_id FROM document_versions WHERE id=?1"),
        ("basis_decision_id", "SELECT document_id FROM decisions WHERE id=?1"),
    ] {
        if let Some(id) = value[field].as_i64() {
            let visible: bool = c.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM documents d WHERE d.id=({target}) AND {})",
                    policy::document_visible_sql(actor, "d")
                ),
                [id],
                |r| r.get(0),
            )?;
            if !visible {
                value[field] = Value::Null;
            }
        }
    }
    Ok(())
}

/// Only evidence that the actor can inspect is offered by the closing dialog.
async fn closing_bases(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let value = ctx.db.read(move |c| {
        policy::require_case_perm(c, &actor, id, perm::CASE_CLOSE)?;
        let mut items = Vec::new();
        let held: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM hearings WHERE case_id=?1 AND status='held')",[id],|r|r.get(0))?;
        if !held {
            for row in query_json(c, &format!("SELECT v.id, v.version_no, d.title,
                COALESCE(d.document_date, d.received_date, date(d.created_at, '+12 hours')) AS date
                FROM documents d JOIN document_versions v ON v.document_id=d.id
                WHERE d.case_id=?1 AND d.visibility <> 'judicial_note' AND v.scan_status='clean' AND {}
                ORDER BY d.id, v.version_no DESC",policy::document_visible_sql(&actor,"d")),[id])? {
                items.push(json!({"kind":"document", "id":row["id"], "date":row["date"],
                    "label":format!("{} — version {}",row["title"].as_str().unwrap_or_default(),row["version_no"])}));
            }
        }
        for row in query_json(c, &format!("SELECT dc.id, dc.title, dc.decision_date AS date
            FROM decisions dc JOIN document_versions v ON v.id=dc.document_version_id JOIN documents d ON d.id=v.document_id
            WHERE dc.case_id=?1 AND dc.status='finalised' AND d.case_id=?1 AND dc.decision_date IS NOT NULL
                AND d.visibility <> 'judicial_note' AND v.scan_status='clean' AND {} ORDER BY dc.id",
                policy::document_visible_sql(&actor,"d")),[id])? {
            items.push(json!({"kind":"decision","id":row["id"],"date":row["date"],"label":row["title"]}));
        }
        for row in query_json(c,"SELECT id, ends_at, outcome_summary FROM hearings WHERE case_id=?1 AND status='held' AND trim(COALESCE(outcome_summary,'')) <> '' ORDER BY starts_at DESC",[id])? {
            let date = crate::time::utc_to_local(row["ends_at"].as_str().unwrap_or_default())[..10].to_string();
            items.push(json!({"kind":"hearing","id":row["id"],"date":date,
                "label":format!("Hearing on {date} — {}",row["outcome_summary"].as_str().unwrap_or_default())}));
        }
        Ok(json!({"items":items}))
    }).await?;
    Ok(Json(value))
}

/// Check exact evidence and chronology without inferring a judicial outcome.
fn validate_closing_basis(tx: &Connection, actor: &Actor, id: i64, req: &CloseReq, date: &str) -> AppResult<()> {
    let registered: String = tx.query_row("SELECT registered_date FROM cases WHERE id = ?1", [id], |r| r.get(0))?;
    if date < registered.as_str() || date > crate::time::today_local().as_str() {
        return Err(AppError::validation("Closed date must be on or after registration and no later than today.").with_details(json!({"field":"closed_date"})));
    }
    let count =
        usize::from(req.basis_document_version_id.is_some()) + usize::from(req.basis_decision_id.is_some()) + usize::from(req.basis_hearing_id.is_some());
    if count != 1 {
        return Err(
            AppError::validation("Choose one basis document, finalised decision or recorded hearing outcome.")
                .with_details(json!({"field":"basis_document_version_id"})),
        );
    }
    let evidence_date = if let Some(vid) = req.basis_document_version_id {
        let (doc, _) = policy::require_version(tx, actor, vid)?;
        if doc.case_id != Some(id) || doc.visibility == "judicial_note" {
            return Err(AppError::validation("The basis must be a document of this case and cannot be a judicial note.")
                .with_details(json!({"field":"basis_document_version_id"})));
        }
        let (scan, document_date, received, created): (String, Option<String>, Option<String>, String) = tx.query_row(
            "SELECT v.scan_status, d.document_date, d.received_date, d.created_at FROM document_versions v JOIN documents d ON d.id=v.document_id WHERE v.id=?1",
            [vid], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        if scan != "clean" {
            return Err(AppError::validation("A quarantined file cannot be the basis for closing.").with_details(json!({"field":"basis_document_version_id"})));
        }
        let held: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM hearings WHERE case_id=?1 AND status='held')", [id], |r| r.get(0))?;
        if held || req.basis == "decided" {
            return Err(
                AppError::validation("Closing after a hearing or decision requires its recorded outcome or finalised decision.")
                    .with_details(json!({"field":"basis_hearing_id"})),
            );
        }
        document_date
            .or(received)
            .unwrap_or_else(|| crate::time::utc_to_local(&created)[..10].to_string())
    } else if let Some(did) = req.basis_decision_id {
        let decision = query_one_json(tx, "SELECT * FROM decisions WHERE id=?1 AND case_id=?2", params![did, id])?;
        let (doc, _) = policy::require_version(tx, actor, decision["document_version_id"].as_i64().unwrap_or_default())?;
        let scan: String = tx.query_row(
            "SELECT scan_status FROM document_versions WHERE id=?1",
            [decision["document_version_id"].as_i64()],
            |r| r.get(0),
        )?;
        if decision["status"] != "finalised" || doc.case_id != Some(id) || doc.visibility == "judicial_note" || scan != "clean" {
            return Err(
                AppError::validation("Choose a finalised decision with a safe document of this case.").with_details(json!({"field":"basis_decision_id"}))
            );
        }
        decision["decision_date"]
            .as_str()
            .ok_or_else(|| AppError::validation("The basis decision needs a decision date."))?
            .to_string()
    } else {
        let hearing = query_one_json(tx, "SELECT * FROM hearings WHERE id=?1 AND case_id=?2", params![req.basis_hearing_id, id])?;
        if hearing["status"] != "held" || hearing["outcome_summary"].as_str().unwrap_or_default().trim().is_empty() {
            return Err(AppError::validation("Choose a held hearing with a recorded outcome.").with_details(json!({"field":"basis_hearing_id"})));
        }
        crate::time::utc_to_local(hearing["ends_at"].as_str().unwrap_or_default())[..10].to_string()
    };
    if date < evidence_date.as_str() {
        return Err(
            AppError::validation("Closed date cannot precede the basis document, decision or hearing outcome.").with_details(json!({"field":"closed_date"})),
        );
    }
    Ok(())
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
                if let Some(version) = req.version && version != case.version {
                    return Err(AppError::version_conflict(case_json(tx, &actor, id)?));
                }
                require_ref(tx, "closure_basis", &req.basis)?;
                let mut note = optional(&req.note);
                if req.basis == "other" && note.is_none() {
                    return Err(AppError::validation("Explain the basis for closing.").with_details(json!({ "field": "note" })));
                }
                let mut blockers = open_items(tx, id)?;
                for blocker in &mut blockers {
                    if blocker["kind"]=="decision" {
                        let vid:i64=tx.query_row("SELECT document_version_id FROM decisions WHERE id=?1",[blocker["id"].as_i64()],|r|r.get(0))?;
                        if policy::require_version(tx,&actor,vid).is_err() { blocker["label"]=json!("Restricted document"); }
                    }
                }
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
                validate_closing_basis(tx, &actor, id, &req, &date)?;
                record_status(tx, &actor, id, &case.status, "closed", note.as_deref(), Some(&req.basis), &date)?;
                tx.execute(
                    "UPDATE cases SET closure_basis = ?2, closure_note = ?3, closed_date = ?4, closed_at = ?5, closed_by = ?6,
                            basis_document_version_id = ?7, basis_decision_id = ?8, basis_hearing_id = ?9 WHERE id = ?1",
                    params![id, req.basis, note, date, crate::time::now_utc(), actor.user_id, req.basis_document_version_id, req.basis_decision_id, req.basis_hearing_id],
                )?;
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new("case.closed", "case", id, format!("Case {} closed ({})", case.number, req.basis))
                        .case(Some(id))
                        .details(json!({ "basis": req.basis, "note": note, "closed_date": date, "acknowledge": acknowledged, "basis_document_version_id": req.basis_document_version_id, "basis_decision_id": req.basis_decision_id, "basis_hearing_id": req.basis_hearing_id })),
                )?;
                Ok(json!({ "ok": true, "closed_date": date }))
            })
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize, Serialize)]
struct ReasonReq {
    version: Option<i64>,
    reason: Option<String>,
}

async fn reopen(ctx: Ctx, Path(id): Path<i64>, IdemKey(key): IdemKey, JsonBody(req): JsonBody<ReasonReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case = policy::require_case_perm(tx, &actor, id, perm::CASE_REOPEN)?;
            idempotent(tx, &actor, &key, "case.reopen", &(id, &req), || {
                if let Some(version) = req.version
                    && version != case.version
                {
                    return Err(AppError::version_conflict(case_json(tx, &actor, id)?));
                }
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
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize, Serialize)]
struct RelationReq {
    to_case_id: i64,
    kind: String,
    note: Option<String>,
}

async fn add_relation(ctx: Ctx, Path(id): Path<i64>, IdemKey(key): IdemKey, JsonBody(req): JsonBody<RelationReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case = policy::require_case_perm(tx, &actor, id, perm::CASE_EDIT)?;
            let other = policy::require_case(tx, &actor, req.to_case_id)?;
            idempotent(tx, &actor, &key, "case.relation", &(id, &req), || {
                require_ref(tx, "relation_kind", &req.kind)?;
                tx.execute(
                    "INSERT INTO case_relations (from_case_id, to_case_id, kind, note, created_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, other.id, req.kind, optional(&req.note), actor.user_id, crate::time::now_utc()],
                )?;
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new(
                        "case.related",
                        "case",
                        id,
                        format!("Case {} linked to {} ({})", case.number, other.number, req.kind),
                    )
                    .case(Some(id))
                    .details(json!({"related_case_id": other.id})),
                )?;
                case_json(tx, &actor, id)
            })
        })
        .await?;
    Ok(Json(v))
}

async fn participant_add(ctx: Ctx, Path(id): Path<i64>, IdemKey(key): IdemKey, JsonBody(req): JsonBody<ParticipantInput>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case = policy::require_case_perm(tx, &actor, id, perm::CASE_EDIT)?;
            idempotent(tx,&actor,&key,"participant.add",&json!({"case_id":id,"body":req}),|| {
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
        })
        .await?;
    Ok(Json(v))
}

async fn participant_end(ctx: Ctx, Path((id, pid)): Path<(i64, i64)>, IdemKey(key): IdemKey, JsonBody(req): JsonBody<ReasonReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let case = policy::require_case_perm(tx, &actor, id, perm::CASE_EDIT)?;
            idempotent(tx,&actor,&key,"participant.end",&json!({"case_id":id,"participation_id":pid,"body":req}),|| {
            let why = reason(&req.reason)?;
            let n = tx.execute(
                "UPDATE case_participations SET version=version+1, active = 0, ended_at = ?3, end_reason = ?4 WHERE id = ?1 AND case_id = ?2 AND active = 1",
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
