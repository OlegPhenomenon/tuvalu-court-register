//! The single access policy used by screens, file downloads, search, report counts, exports and
//! the background worker. Never filter visibility in the UI only.
//!
//! Case access: active assignment, or `case.view_all` on a non-restricted case, or `case.view_restricted`.
//! Document access (on top of case access): administrative / party_material → case access;
//! restricted / judicial_note → uploader or an active `document_grants` row. No role bypasses this.

use crate::auth::Actor;
use crate::error::{AppError, AppResult};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

pub mod perm {
    pub const INTAKE_MANAGE: &str = "intake.manage";
    pub const CASE_REGISTER: &str = "case.register";
    pub const CASE_VIEW_ALL: &str = "case.view_all";
    pub const CASE_VIEW_RESTRICTED: &str = "case.view_restricted";
    pub const CASE_EDIT: &str = "case.edit";
    pub const PARTY_EDIT: &str = "party.edit";
    pub const CASE_ASSIGN_STAFF: &str = "case.assign_staff";
    pub const CASE_ASSIGN_JUDGE: &str = "case.assign_judge";
    pub const CASE_CLOSE: &str = "case.close";
    pub const CASE_REOPEN: &str = "case.reopen";
    pub const HEARING_SCHEDULE: &str = "hearing.schedule";
    pub const HEARING_OVERRIDE_CONFLICT: &str = "hearing.override_conflict";
    pub const HEARING_RECORD_OUTCOME: &str = "hearing.record_outcome";
    pub const HEARING_ADMIN_CORRECT: &str = "hearing.admin_correct";
    pub const TASK_MANAGE: &str = "task.manage";
    pub const DOCUMENT_MANAGE: &str = "document.manage";
    pub const DOCUMENT_GRANT_RESTRICTED: &str = "document.grant_restricted";
    pub const DECISION_DRAFT: &str = "decision.draft";
    pub const DECISION_FINALISE: &str = "decision.finalise";
    pub const DISPATCH_MANAGE: &str = "dispatch.manage";
    pub const DISPATCH_ASSESS_SERVICE: &str = "dispatch.assess_service";
    pub const REPORT_VIEW: &str = "report.view";
    pub const AUDIT_VIEW: &str = "audit.view";
    pub const IMPORT_RUN: &str = "import.run";
    pub const EXPORT_CASE: &str = "export.case";
    pub const ADMIN_USERS: &str = "admin.users";
    pub const ADMIN_SETTINGS: &str = "admin.settings";

    /// Every permission, with a human description (Settings screen).
    pub const ALL: &[(&str, &str)] = &[
        (INTAKE_MANAGE, "Receive incoming documents, request information, mark duplicates, link or return"),
        (CASE_REGISTER, "Register a case from an intake"),
        (CASE_VIEW_ALL, "See all non-restricted cases"),
        (CASE_VIEW_RESTRICTED, "See restricted cases without being assigned"),
        (CASE_EDIT, "Edit case details and participants"),
        (PARTY_EDIT, "Correct contact details of people and organisations on cases you can see"),
        (CASE_ASSIGN_STAFF, "Assign or remove registry staff on a case"),
        (CASE_ASSIGN_JUDGE, "Assign or remove the judge on a case"),
        (CASE_CLOSE, "Close a case with a basis"),
        (CASE_REOPEN, "Reopen a closed case with a reason"),
        (HEARING_SCHEDULE, "Create, confirm, adjourn and cancel hearings"),
        (HEARING_OVERRIDE_CONFLICT, "Confirm a hearing despite a time conflict (with reason)"),
        (HEARING_RECORD_OUTCOME, "Record that a hearing was held, attendance and result"),
        (HEARING_ADMIN_CORRECT, "Correct an erroneously recorded hearing (with reason)"),
        (TASK_MANAGE, "Create, complete, cancel and carry forward tasks"),
        (DOCUMENT_MANAGE, "Upload documents and versions, set visibility"),
        (DOCUMENT_GRANT_RESTRICTED, "Grant or revoke access to restricted documents"),
        (DECISION_DRAFT, "Draft decision documents"),
        (DECISION_FINALISE, "Finalise and amend decisions"),
        (DISPATCH_MANAGE, "Prepare, review and send notices and copy packages; record delivery"),
        (DISPATCH_ASSESS_SERVICE, "Record the legal assessment of service"),
        (REPORT_VIEW, "View reports and export CSV"),
        (AUDIT_VIEW, "View the audit history"),
        (IMPORT_RUN, "Import legacy cases"),
        (EXPORT_CASE, "Export a case package of permitted materials"),
        (ADMIN_USERS, "Manage user accounts, permissions and sessions"),
        (ADMIN_SETTINGS, "Manage rooms, registries, reference lists, templates and settings"),
    ];

    /// Permissions an `admin.users` holder may grant. Judicial and case-access powers are excluded
    /// so a technical administrator cannot escalate himself into the judiciary; those must be
    /// granted from the command line (`tuvalu-court grant`) by the court's appointed authority.
    pub const ADMIN_GRANTABLE: &[&str] = &[
        INTAKE_MANAGE, CASE_REGISTER, CASE_EDIT, PARTY_EDIT, CASE_CLOSE, HEARING_SCHEDULE, TASK_MANAGE, DOCUMENT_MANAGE,
        DISPATCH_MANAGE, REPORT_VIEW, EXPORT_CASE, ADMIN_SETTINGS,
    ];
}

/// A system-only administrator cannot gain case access through an assignment.
pub fn user_assignable(conn: &Connection, user_id: i64) -> AppResult<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM users u JOIN user_permissions p ON p.user_id = u.id
         WHERE u.id = ?1 AND u.active = 1 AND (p.permission LIKE 'case.%'
         OR p.permission LIKE 'hearing.%' OR p.permission LIKE 'task.%'
         OR p.permission LIKE 'document.%' OR p.permission LIKE 'decision.%'
         OR p.permission LIKE 'dispatch.%' OR p.permission = 'export.case'))",
        [user_id], |r| r.get(0),
    )?)
}

/// SQL boolean expression: "actor can see the case whose id is `case_id_expr`".
/// Only integer literals derived from the actor are interpolated.
pub fn case_visible_sql(actor: &Actor, case_id_expr: &str) -> String {
    if actor.has(perm::CASE_VIEW_RESTRICTED) {
        return "1".into();
    }
    let assigned = format!(
        "EXISTS (SELECT 1 FROM case_assignments pa WHERE pa.case_id = {case_id_expr} AND pa.user_id = {} AND pa.end_at IS NULL)",
        actor.user_id
    );
    if actor.has(perm::CASE_VIEW_ALL) {
        format!("({assigned} OR (SELECT pc.restricted FROM cases pc WHERE pc.id = {case_id_expr}) = 0)")
    } else {
        assigned
    }
}

/// SQL boolean expression for documents aliased as `doc_alias` (includes case/intake access).
pub fn document_visible_sql(actor: &Actor, doc_alias: &str) -> String {
    let d = doc_alias;
    let container = format!(
        "(({d}.case_id IS NOT NULL AND {}) OR ({d}.case_id IS NULL AND {}))",
        case_visible_sql(actor, &format!("{d}.case_id")),
        if actor.has(perm::INTAKE_MANAGE) { "1" } else { "0" }
    );
    let scope = format!(
        "({d}.visibility IN ('administrative','party_material') OR {d}.created_by = {uid}
          OR EXISTS (SELECT 1 FROM document_grants pg WHERE pg.document_id = {d}.id AND pg.user_id = {uid} AND pg.revoked_at IS NULL))",
        uid = actor.user_id
    );
    format!("({container} AND {scope})")
}

/// SQL boolean expression for intakes aliased as `i`: unlinked intakes for intake staff,
/// linked intakes follow the case's visibility.
pub fn intake_visible_sql(actor: &Actor, alias: &str) -> String {
    format!(
        "(({a}.case_id IS NULL AND {}) OR ({a}.case_id IS NOT NULL AND {}))",
        if actor.has(perm::INTAKE_MANAGE) { "1" } else { "0" },
        case_visible_sql(actor, &format!("{alias}.case_id")),
        a = alias
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct CaseRef {
    pub id: i64,
    pub number: String,
    pub title: String,
    pub status: String,
    pub restricted: bool,
    pub version: i64,
}

/// Load a case the actor may see, else 404 (never 403: existence is not revealed).
pub fn require_case(conn: &Connection, actor: &Actor, case_id: i64) -> AppResult<CaseRef> {
    let sql = format!(
        "SELECT id, number, title, status, restricted, version FROM cases c WHERE c.id = ?1 AND {}",
        case_visible_sql(actor, "c.id")
    );
    conn.query_row(&sql, [case_id], |r| {
        Ok(CaseRef {
            id: r.get(0)?,
            number: r.get(1)?,
            title: r.get(2)?,
            status: r.get(3)?,
            restricted: r.get(4)?,
            version: r.get(5)?,
        })
    })
    .optional()?
    .ok_or_else(AppError::not_found)
}

pub fn can_view_case(conn: &Connection, actor: &Actor, case_id: i64) -> AppResult<bool> {
    Ok(require_case(conn, actor, case_id).is_ok())
}

/// Case must be visible AND the actor must hold `perm` (403 if visible but not permitted).
pub fn require_case_perm(conn: &Connection, actor: &Actor, case_id: i64, perm: &str) -> AppResult<CaseRef> {
    let c = require_case(conn, actor, case_id)?;
    actor.require(perm)?;
    Ok(c)
}

/// Whether the actor has an active assignment on the case, optionally in a given role.
pub fn is_assigned(conn: &Connection, actor: &Actor, case_id: i64, role: Option<&str>) -> AppResult<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM case_assignments WHERE case_id = ?1 AND user_id = ?2 AND end_at IS NULL
           AND (?3 IS NULL OR role = ?3)",
        rusqlite::params![case_id, actor.user_id, role],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

#[derive(Debug, Clone, Serialize)]
pub struct DocRef {
    pub id: i64,
    pub case_id: Option<i64>,
    pub intake_id: Option<i64>,
    pub title: String,
    pub visibility: String,
    pub created_by: i64,
}

impl DocRef {
    /// Restricted files and judicial notes are logged when their bytes are opened or downloaded.
    pub fn is_sensitive(&self) -> bool {
        matches!(self.visibility.as_str(), "restricted" | "judicial_note")
    }
}

/// Load a document the actor may see, else 404.
pub fn require_document(conn: &Connection, actor: &Actor, document_id: i64) -> AppResult<DocRef> {
    let sql = format!(
        "SELECT d.id, d.case_id, d.intake_id, d.title, d.visibility, d.created_by FROM documents d
         WHERE d.id = ?1 AND {}",
        document_visible_sql(actor, "d")
    );
    conn.query_row(&sql, [document_id], |r| {
        Ok(DocRef {
            id: r.get(0)?,
            case_id: r.get(1)?,
            intake_id: r.get(2)?,
            title: r.get(3)?,
            visibility: r.get(4)?,
            created_by: r.get(5)?,
        })
    })
    .optional()?
    .ok_or_else(AppError::not_found)
}

/// Resolve a document version to its (visible) document, else 404.
pub fn require_version(conn: &Connection, actor: &Actor, version_id: i64) -> AppResult<(DocRef, i64)> {
    let document_id: i64 = conn
        .query_row("SELECT document_id FROM document_versions WHERE id = ?1", [version_id], |r| r.get(0))
        .optional()?
        .ok_or_else(AppError::not_found)?;
    Ok((require_document(conn, actor, document_id)?, version_id))
}

/// All records to which a contact is linked, including representation and service references.
fn party_links_sql(id: &str) -> String {
    format!("SELECT case_id, NULL AS intake_id FROM case_participations WHERE party_id={id} OR representative_party_id={id}
        UNION SELECT case_id, id FROM intakes WHERE sender_party_id={id}
        UNION SELECT COALESCE(d.case_id,(SELECT case_id FROM intakes WHERE id=d.intake_id)), d.intake_id FROM documents d WHERE d.source_party_id={id}
        UNION SELECT COALESCE(dp.case_id,(SELECT case_id FROM intakes WHERE id=dp.intake_id)), dp.intake_id FROM dispatches dp WHERE dp.recipient_party_id={id}")
}

/// Directory visibility follows linked records. Only the creator sees an unlinked contact.
pub fn party_visible_sql(actor: &Actor, id: &str) -> String {
    let links = party_links_sql(id);
    let intake = if actor.has(perm::INTAKE_MANAGE) {
        "1"
    } else {
        "0"
    };
    format!("(EXISTS (SELECT 1 FROM ({links}) pl WHERE (pl.case_id IS NOT NULL AND {case_vis})
        OR (pl.case_id IS NULL AND pl.intake_id IS NOT NULL AND {intake}))
        OR ((SELECT created_by FROM parties WHERE id={id})={uid} AND NOT EXISTS(SELECT 1 FROM ({links}))))",
        case_vis=case_visible_sql(actor,"pl.case_id"), uid=actor.user_id)
}

pub fn require_party(conn: &Connection, actor: &Actor, id: i64) -> AppResult<()> {
    let exists: bool = conn.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM parties p WHERE p.id=?1 AND {})",
            party_visible_sql(actor, "p.id")
        ),
        [id],
        |r| r.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(AppError::not_found())
    }
}

/// A contact correction must be authorised across every linked record.
pub fn party_shared(conn: &Connection, actor: &Actor, id: i64) -> AppResult<bool> {
    let links = party_links_sql("?1");
    Ok(conn.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM ({links}) pl WHERE
         (pl.case_id IS NOT NULL AND NOT ({case_vis})) OR
         (pl.case_id IS NULL AND pl.intake_id IS NOT NULL AND NOT ({intake})))",
            case_vis = case_visible_sql(actor, "pl.case_id"),
            intake = i32::from(actor.has(perm::INTAKE_MANAGE))
        ),
        [id],
        |r| r.get(0),
    )?)
}

/// Writing a contact record needs an explicit write permission; `case.view_all` is read-only.
pub fn can_edit_party(conn: &Connection, actor: &Actor, id: i64) -> AppResult<bool> {
    Ok((actor.has(perm::CASE_EDIT)
        || actor.has(perm::INTAKE_MANAGE)
        || actor.has(perm::PARTY_EDIT))
        && !party_shared(conn, actor, id)?)
}

pub fn require_party_edit(conn: &Connection, actor: &Actor, id: i64) -> AppResult<()> {
    require_party(conn, actor, id)?;
    if party_shared(conn, actor, id)? {
        return Err(AppError::conflict(
            "party_shared",
            "This person is linked to records you cannot access; ask the registry head",
        ));
    }
    if !can_edit_party(conn, actor, id)? {
        return Err(AppError::forbidden(
            "You cannot edit this person's contact record.",
        ));
    }
    Ok(())
}
