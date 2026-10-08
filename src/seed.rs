//! Reference data (both modes) and the fictional DEMO dataset (demo mode).
//! Reference lists are examples to be confirmed by the court (spec §15); they are editable in Settings.

use crate::db::Db;
use crate::error::AppResult;
use rusqlite::{Transaction, params};

const REF_ITEMS: &[(&str, &str, &str)] = &[
    ("case_category", "civil_contract", "Civil — contract"),
    ("case_category", "civil_property", "Civil — land and property"),
    ("case_category", "civil_other", "Civil — other"),
    ("case_category", "family", "Family"),
    ("case_category", "criminal_summary", "Criminal — summary"),
    ("intake_channel", "counter", "In person at the registry"),
    ("intake_channel", "post", "Post"),
    ("intake_channel", "email", "E-mail"),
    ("intake_channel", "island_officer", "Via island court officer"),
    ("intake_channel", "other", "Other"),
    ("origin_island", "funafuti", "Funafuti"),
    ("origin_island", "nanumea", "Nanumea"),
    ("origin_island", "nanumanga", "Nanumanga"),
    ("origin_island", "niutao", "Niutao"),
    ("origin_island", "nui", "Nui"),
    ("origin_island", "nukufetau", "Nukufetau"),
    ("origin_island", "nukulaelae", "Nukulaelae"),
    ("origin_island", "vaitupu", "Vaitupu"),
    ("origin_island", "niulakita", "Niulakita"),
    ("document_type", "claim", "Claim / application"),
    ("document_type", "supplement", "Supplementary material"),
    ("document_type", "evidence", "Evidence"),
    ("document_type", "response", "Response / defence"),
    ("document_type", "correspondence", "Correspondence"),
    ("document_type", "hearing_record", "Hearing record / minutes"),
    ("document_type", "decision", "Decision"),
    ("document_type", "medical", "Medical record"),
    ("document_type", "judicial_note", "Judicial working note"),
    ("document_type", "notice", "Notice"),
    ("document_type", "other", "Other"),
    ("closure_basis", "decided", "Decided"),
    ("closure_basis", "settled", "Settled or withdrawn by the parties"),
    ("closure_basis", "discontinued", "Discontinued"),
    ("closure_basis", "transferred", "Transferred"),
    ("closure_basis", "struck_out", "Struck out"),
    ("closure_basis", "other", "Other (explain)"),
    ("hearing_type", "mention", "Mention"),
    ("hearing_type", "hearing", "Hearing"),
    ("hearing_type", "case_conference", "Case conference"),
    ("hearing_type", "decision_delivery", "Delivery of decision"),
    ("participant_role", "claimant", "Claimant"),
    ("participant_role", "respondent", "Respondent"),
    ("participant_role", "applicant", "Applicant"),
    ("participant_role", "defendant", "Defendant"),
    ("participant_role", "witness", "Witness"),
    ("participant_role", "interested_party", "Interested party"),
    ("dispatch_method", "email", "E-mail (local mailbox in this installation)"),
    ("dispatch_method", "post", "Post"),
    ("dispatch_method", "hand", "Hand delivery"),
    ("dispatch_method", "collection", "Collected at the registry"),
    ("dispatch_method", "island_officer", "Via island court officer"),
    ("relation_kind", "follow_up", "Follow-up application"),
    ("relation_kind", "related", "Related case"),
    ("relation_kind", "continuation", "Continuation after closure"),
];

const TEMPLATES: &[(&str, &str, &str, &str)] = &[
    (
        "hearing_notice",
        "Notice of hearing",
        "{court}: hearing in {case_number}",
        "Dear {recipient},\n\nA {hearing_type} in case {case_number} ({case_title}) is listed for {hearing_local} in {room}.\n\nPlease contact the registry if you cannot attend.\n\n{court}",
    ),
    (
        "hearing_rescheduled",
        "Hearing rescheduled",
        "{court}: new hearing date in {case_number}",
        "Dear {recipient},\n\nThe hearing in case {case_number} ({case_title}) previously listed for {previous_local} has been moved to {hearing_local} in {room}.\n\nReason: {reason}\n\n{court}",
    ),
    (
        "information_request",
        "Request for missing information",
        "{court}: documents needed for your filing {intake_reference}",
        "Dear {recipient},\n\nThank you for your filing received on {received_date}. Before it can be considered for registration the registry needs:\n\n{missing_items}\n\n{court}",
    ),
    (
        "copy_dispatch",
        "Copies of documents",
        "{court}: documents in {case_number}",
        "Dear {recipient},\n\nPlease find attached copies of the documents listed below in case {case_number}.\n\n{items}\n\n{court}",
    ),
];

/// Insert reference lists, templates and default settings when the database is empty. Idempotent.
pub fn seed_reference(db: &Db) -> AppResult<()> {
    db.write_blocking(|tx| {
        let n: i64 = tx.query_row("SELECT COUNT(*) FROM ref_items", [], |r| r.get(0))?;
        if n > 0 {
            return Ok(());
        }
        for (i, (kind, code, label)) in REF_ITEMS.iter().enumerate() {
            tx.execute(
                "INSERT INTO ref_items (kind, code, label, sort) VALUES (?1, ?2, ?3, ?4)",
                params![kind, code, label, i as i64],
            )?;
        }
        for (code, name, subject, body) in TEMPLATES {
            tx.execute(
                "INSERT INTO message_templates (code, name, subject, body) VALUES (?1, ?2, ?3, ?4)",
                params![code, name, subject, body],
            )?;
        }
        for (k, v) in [
            ("court_name", "Court Registry"),
            ("hearing_buffer_minutes", "0"),
            ("intake_reference_prefix", "IN"),
        ] {
            tx.execute("INSERT OR IGNORE INTO settings (key, value) VALUES (?1, ?2)", params![k, v])?;
        }
        Ok(())
    })
}

pub fn insert_user(
    tx: &Transaction,
    username: &str,
    display: &str,
    title: Option<&str>,
    password_hash: &str,
    is_judge: bool,
    persona: Option<&str>,
    perms: &[&str],
) -> AppResult<i64> {
    let now = crate::time::now_utc();
    tx.execute(
        "INSERT INTO users (username, display_name, title, email, password_hash, is_judge, persona, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![username, display, title, format!("{username}@registry.invalid"), password_hash, is_judge, persona, now],
    )?;
    let id = tx.last_insert_rowid();
    for p in perms {
        tx.execute(
            "INSERT INTO user_permissions (user_id, permission, granted_at) VALUES (?1, ?2, ?3)",
            params![id, p, now],
        )?;
    }
    Ok(id)
}

/// Create a production user (CLI).
pub fn create_user(db: &Db, username: &str, display: &str, password: &str, judge: bool, perms: &[String]) -> AppResult<i64> {
    let hash = crate::auth::hash_password(password)?;
    let perms: Vec<String> = perms.to_vec();
    let (username, display) = (username.to_string(), display.to_string());
    db.write_blocking(move |tx| {
        let refs: Vec<&str> = perms.iter().map(String::as_str).collect();
        let id = insert_user(tx, &username, &display, None, &hash, judge, None, &refs)?;
        tx.execute("UPDATE users SET must_change_password=1 WHERE id=?1", [id])?;
        crate::audit::record(tx, None, crate::audit::Event::new("user.created", "user", id, format!("User '{username}' created via command line")))?;
        Ok(id)
    })
}

pub struct Persona {
    pub key: &'static str,
    pub display: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    pub is_judge: bool,
    pub perms: &'static [&'static str],
}

use crate::policy::perm::*;

pub const PERSONAS: &[Persona] = &[
    Persona {
        key: "olga",
        display: "Olga Marsh",
        title: "Registry clerk",
        summary: "Receives filings, registers cases, schedules hearings, prepares notices and copies.",
        is_judge: false,
        perms: &[INTAKE_MANAGE, CASE_REGISTER, CASE_EDIT, HEARING_SCHEDULE, TASK_MANAGE, DOCUMENT_MANAGE, DISPATCH_MANAGE, CASE_CLOSE, REPORT_VIEW, EXPORT_CASE],
    },
    Persona {
        key: "elena",
        display: "Elena Brooks",
        title: "Head of registry",
        summary: "Assigns staff and the judge, oversees open work, reopens cases, grants access to restricted material.",
        is_judge: false,
        perms: &[CASE_VIEW_ALL, CASE_ASSIGN_STAFF, CASE_ASSIGN_JUDGE, CASE_REOPEN, CASE_CLOSE, REPORT_VIEW, AUDIT_VIEW, IMPORT_RUN, EXPORT_CASE, DOCUMENT_GRANT_RESTRICTED, HEARING_SCHEDULE, HEARING_OVERRIDE_CONFLICT, TASK_MANAGE],
    },
    Persona {
        key: "viktor",
        display: "Viktor Hale",
        title: "Judge",
        summary: "Sees assigned cases, records hearing outcomes, drafts and finalises decisions, keeps private notes.",
        is_judge: true,
        perms: &[DECISION_DRAFT, DECISION_FINALISE, HEARING_RECORD_OUTCOME, HEARING_ADMIN_CORRECT, TASK_MANAGE, DISPATCH_ASSESS_SERVICE, DOCUMENT_MANAGE],
    },
    Persona {
        key: "sergei",
        display: "Sergei Novak",
        title: "Hearings and service officer",
        summary: "Organises hearings and delivers notices and copies. Does not see restricted files or judicial notes.",
        is_judge: false,
        perms: &[DISPATCH_MANAGE, HEARING_SCHEDULE, TASK_MANAGE],
    },
    Persona {
        key: "pavel",
        display: "Pavel Stone",
        title: "Technical administrator",
        summary: "Manages users and settings. Has no access to cases, files or judicial notes.",
        is_judge: false,
        perms: &[ADMIN_USERS, ADMIN_SETTINGS],
    },
];

/// Demo dataset: DEMO court unit, registries, rooms, personas and fictional cases (spec §12).
pub fn seed_demo(db: &Db) -> AppResult<()> {
    db.write_blocking(|tx| {
        tx.execute("UPDATE settings SET value = 'DEMO Magistrates Court Registry' WHERE key = 'court_name'", [])?;
        tx.execute("INSERT INTO court_units (code, name) VALUES ('DEMO-MC', 'DEMO Magistrates Court (fictional)')", [])?;
        let unit = tx.last_insert_rowid();
        for (series, name) in [("DEMO-CIV", "DEMO civil register"), ("DEMO-FAM", "DEMO family register"), ("DEMO-CRM", "DEMO criminal register")] {
            tx.execute("INSERT INTO registries (court_unit_id, series, name) VALUES (?1, ?2, ?3)", params![unit, series, name])?;
        }
        for (name, loc) in [("DEMO Courtroom 1", "Ground floor"), ("DEMO Courtroom 2", "First floor"), ("DEMO Meeting room", "Registry wing")] {
            tx.execute("INSERT INTO rooms (court_unit_id, name, location) VALUES (?1, ?2, ?3)", params![unit, name, loc])?;
        }
        // Demo personas never sign in with a password (persona switch is demo-only); random unusable hash.
        let unusable = format!("!demo-{}", crate::auth::random_token());
        for p in PERSONAS {
            insert_user(tx, p.key, p.display, Some(p.title), &unusable, p.is_judge, Some(p.key), p.perms)?;
        }
        crate::seed_demo::seed_cases(tx, db)?;
        Ok(())
    })
}
