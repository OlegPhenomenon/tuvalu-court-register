//! Fictional DEMO cases (spec §12) and a tiny PDF generator for DEMO-marked sample files.
use crate::audit::{self, Event};
use crate::auth::Actor;
use crate::db::Db;
use crate::error::{AppError, AppResult};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};

/// Build a minimal, valid single-page PDF (Helvetica text) with a "DEMO" watermark line.
/// `lines` are escaped; non-ASCII characters are replaced with '?'.
pub fn demo_pdf(title: &str, lines: &[&str]) -> Vec<u8> {
    fn esc(s: &str) -> String {
        s.chars()
            .map(|c| match c {
                '(' => "\\(".to_string(),
                ')' => "\\)".to_string(),
                '\\' => "\\\\".to_string(),
                c if c.is_ascii() && !c.is_ascii_control() => c.to_string(),
                _ => "?".to_string(),
            })
            .collect()
    }
    let mut content = String::from(
        "BT /F1 22 Tf 1 0 0 rg 72 770 Td (DEMO - FICTIONAL - NOT A COURT RECORD) Tj ET\n",
    );
    content.push_str(&format!(
        "BT /F1 16 Tf 0 0 0 rg 72 730 Td ({}) Tj ET\n",
        esc(title)
    ));
    let mut y = 700;
    for line in lines {
        for chunk in line.as_bytes().chunks(90) {
            let s = String::from_utf8_lossy(chunk);
            content.push_str(&format!("BT /F1 11 Tf 72 {y} Td ({}) Tj ET\n", esc(&s)));
            y -= 16;
            if y < 60 {
                break;
            }
        }
    }
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        format!("<< /Length {} >>\nstream\n{}endstream", content.len(), content),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", i + 1, obj).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

// --------------------------------------------------------------------------- seed helpers

/// Court-local calendar date `delta` days from today (negative = past).
fn ldate(delta: i64) -> String {
    crate::time::utc_to_local_date(&crate::time::fmt_utc(
        crate::time::now() + time::Duration::days(delta),
    ))
}

/// UTC instant `delta` days from now.
fn ts(delta: i64) -> String {
    crate::time::fmt_utc(crate::time::now() + time::Duration::days(delta))
}

/// UTC instant for a court-local `HH:MM` on the date `delta` days from today.
fn local_time(delta: i64, hm: &str) -> AppResult<String> {
    crate::time::local_to_utc(&format!("{}T{hm}", ldate(delta)))
}

fn user(tx: &Transaction, persona: &str) -> AppResult<i64> {
    Ok(
        tx.query_row("SELECT id FROM users WHERE persona = ?1", [persona], |r| {
            r.get(0)
        })?,
    )
}

fn actor(tx: &Transaction, user_id: i64) -> AppResult<Actor> {
    crate::auth::load_actor(tx, user_id, None)?
        .ok_or_else(|| AppError::internal("missing demo persona"))
}

fn registry(tx: &Transaction, series: &str) -> AppResult<i64> {
    Ok(tx.query_row(
        "SELECT id FROM registries WHERE series = ?1",
        [series],
        |r| r.get(0),
    )?)
}

fn room(tx: &Transaction, name: &str) -> AppResult<i64> {
    Ok(tx.query_row("SELECT id FROM rooms WHERE name = ?1", [name], |r| r.get(0))?)
}

#[allow(clippy::too_many_arguments)]
fn new_case(
    tx: &Transaction,
    a: &Actor,
    registry_id: i64,
    category: &str,
    title: &str,
    summary: &str,
    registered_days_ago: i64,
    restricted: bool,
    responsible: i64,
) -> AppResult<(i64, String)> {
    let reg_date = ldate(-registered_days_ago);
    let year = crate::time::year_of(&reg_date)?;
    let (seq, number) = crate::api::cases::allocate_number(tx, registry_id, year)?;
    let at = ts(-registered_days_ago);
    tx.execute(
        "INSERT INTO cases (registry_id, year, seq, number, title, category, status, restricted, summary,
                            registered_date, registered_at, registered_by, responsible_user_id, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'registered', ?7, ?8, ?9, ?10, ?11, ?12, ?10)",
        params![registry_id, year, seq, number, title, category, restricted as i64, summary, reg_date, at, a.user_id, responsible],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO case_status_history (case_id, from_status, to_status, reason, by_user, at, effective_date)
         VALUES (?1, NULL, 'registered', 'Registered', ?2, ?3, ?4)",
        params![id, a.user_id, at, reg_date],
    )?;
    audit::record(
        tx,
        Some(a),
        Event::new(
            "case.registered",
            "case",
            id,
            format!("Case {number} registered"),
        )
        .case(Some(id)),
    )?;
    Ok((id, number))
}

#[allow(clippy::too_many_arguments)]
fn set_status(
    tx: &Transaction,
    a: &Actor,
    case_id: i64,
    from: &str,
    to: &str,
    why: Option<&str>,
    basis: Option<&str>,
    days_ago: i64,
) -> AppResult<()> {
    tx.execute(
        "UPDATE cases SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![case_id, to, ts(-days_ago)],
    )?;
    tx.execute(
        "INSERT INTO case_status_history (case_id, from_status, to_status, reason, basis, by_user, at, effective_date)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![case_id, from, to, why, basis, a.user_id, ts(-days_ago), ldate(-days_ago)],
    )?;
    if !matches!(to, "closed" | "reopened") {
        audit::record(
            tx,
            Some(a),
            Event::new(
                "case.status_changed",
                "case",
                case_id,
                format!("Case status changed from {from} to {to}"),
            )
            .case(Some(case_id))
            .details(json!({ "from": from, "to": to, "reason": why })),
        )?;
    }
    Ok(())
}

fn assign(
    tx: &Transaction,
    a: &Actor,
    case_id: i64,
    user_id: i64,
    role: &str,
    why: &str,
    days_ago: i64,
) -> AppResult<()> {
    tx.execute(
        "INSERT INTO case_assignments (case_id, user_id, role, reason, assigned_by, start_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![case_id, user_id, role, why, a.user_id, ts(-days_ago)],
    )?;
    let name: String = tx.query_row(
        "SELECT display_name FROM users WHERE id = ?1",
        [user_id],
        |r| r.get(0),
    )?;
    audit::record(
        tx,
        Some(a),
        Event::new(
            "case.assigned",
            "case",
            case_id,
            format!("{name} assigned as {}", role.replace('_', " ")),
        )
        .case(Some(case_id))
        .details(json!({ "user_id": user_id, "role": role, "reason": why })),
    )?;
    Ok(())
}

fn party(
    tx: &Transaction,
    a: &Actor,
    kind: &str,
    name: &str,
    email: Option<&str>,
    island: Option<&str>,
) -> AppResult<i64> {
    tx.execute(
        "INSERT INTO parties (kind, name, contact_email, island, created_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![kind, name, email, island, a.user_id, ts(0)],
    )?;
    Ok(tx.last_insert_rowid())
}

fn participate(
    tx: &Transaction,
    a: &Actor,
    case_id: i64,
    party_id: i64,
    role: &str,
    service_contact: Option<&str>,
) -> AppResult<()> {
    tx.execute(
        "INSERT INTO case_participations (case_id, party_id, role, service_contact, added_by, added_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![case_id, party_id, role, service_contact, a.user_id, ts(0)],
    )?;
    Ok(())
}

#[derive(Clone)]
struct Stored {
    version_id: i64,
    filename: String,
    sha256: String,
    size_bytes: i64,
}

/// Insert a document + version 1 whose bytes are a generated DEMO PDF on disk.
#[allow(clippy::too_many_arguments)]
fn document(
    tx: &Transaction,
    db: &Db,
    case_id: i64,
    title: &str,
    doc_type: &str,
    source: &str,
    visibility: &str,
    source_party: Option<i64>,
    a: &Actor,
    days_ago: i64,
    lines: &[&str],
) -> AppResult<(i64, Stored)> {
    let bytes = demo_pdf(title, lines);
    let (key, sha) = crate::storage::write_blob(db, &bytes)?;
    let at = ts(-days_ago);
    tx.execute(
        "INSERT INTO documents (case_id, title, doc_type, source, source_party_id, document_date, received_date,
                                visibility, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8, ?9)",
        params![case_id, title, doc_type, source, source_party, ldate(-days_ago), visibility, a.user_id, at],
    )?;
    let document_id = tx.last_insert_rowid();
    let filename = crate::storage::sanitize_filename(&format!("{title}.pdf"));
    tx.execute(
        "INSERT INTO document_versions (document_id, version_no, filename, content_type, size_bytes, sha256, storage_key,
                                        scan_status, uploaded_by, uploaded_at)
         VALUES (?1, 1, ?2, 'application/pdf', ?3, ?4, ?5, 'clean', ?6, ?7)",
        params![document_id, filename, bytes.len() as i64, sha, key, a.user_id, at],
    )?;
    let version_id = tx.last_insert_rowid();
    audit::record(
        tx,
        Some(a),
        Event::new(
            "document.uploaded",
            "document",
            document_id,
            match visibility {
                "restricted" => format!("Restricted document #{document_id} filed"),
                "judicial_note" => format!("Judicial note #{document_id} filed"),
                _ => format!("Document '{title}' filed"),
            },
        )
        .case(Some(case_id))
        .details(json!({ "visibility": visibility, "doc_type": doc_type })),
    )?;
    Ok((
        document_id,
        Stored {
            version_id,
            filename,
            sha256: sha,
            size_bytes: bytes.len() as i64,
        },
    ))
}

#[allow(clippy::too_many_arguments)]
fn hearing(
    tx: &Transaction,
    a: &Actor,
    case_id: i64,
    htype: &str,
    status: &str,
    starts_at: &str,
    ends_at: &str,
    room_id: i64,
    judge: i64,
    notes: Option<&str>,
) -> AppResult<i64> {
    tx.execute(
        "INSERT INTO hearings (case_id, hearing_type, status, starts_at, ends_at, room_id, judge_user_id, notes, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![case_id, htype, status, starts_at, ends_at, room_id, judge, notes, a.user_id, ts(0)],
    )?;
    Ok(tx.last_insert_rowid())
}

fn hparticipant(
    tx: &Transaction,
    hearing_id: i64,
    party: Option<i64>,
    user: Option<i64>,
    role: &str,
    attended: Option<bool>,
) -> AppResult<()> {
    tx.execute(
        "INSERT INTO hearing_participants (hearing_id, party_id, user_id, role, required, attended) VALUES (?1, ?2, ?3, ?4, 1, ?5)",
        params![hearing_id, party, user, role, attended.map(|b| b as i64)],
    )?;
    Ok(())
}

fn task(
    tx: &Transaction,
    a: &Actor,
    case_id: i64,
    kind: &str,
    title: &str,
    assignee: i64,
    due_date: Option<&str>,
) -> AppResult<i64> {
    tx.execute(
        "INSERT INTO tasks (case_id, kind, title, assignee_user_id, due_date, status, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7)",
        params![case_id, kind, title, assignee, due_date, a.user_id, ts(0)],
    )?;
    let id = tx.last_insert_rowid();
    audit::record(
        tx,
        Some(a),
        Event::new("task.created", "task", id, format!("Task: {title}")).case(Some(case_id)),
    )?;
    Ok(id)
}

/// A notice / copy package / information request. `sent_days_ago` set means it already went out
/// (attempt + local mailbox row); `confirmed_days_ago` adds a human handover record.
struct Out<'a> {
    case_id: Option<i64>,
    intake_id: Option<i64>,
    hearing_id: Option<i64>,
    kind: &'a str,
    template: Option<&'a str>,
    party: Option<i64>,
    recipient: &'a str,
    method: &'a str,
    address: Option<&'a str>,
    subject: &'a str,
    body: &'a str,
    sent_days_ago: Option<i64>,
    confirmed_days_ago: Option<i64>,
    items: Vec<Stored>,
}

#[allow(clippy::too_many_arguments)]
fn dispatch(tx: &Transaction, a: &Actor, d: Out) -> AppResult<i64> {
    let prepared = ts(-(d.sent_days_ago.unwrap_or(0) + 1));
    let (status, sent_at) = match d.sent_days_ago {
        Some(n) => ("sent", Some(ts(-n))),
        None => ("draft", None),
    };
    tx.execute(
        "INSERT INTO dispatches (case_id, intake_id, hearing_id, kind, template_code, recipient_party_id, recipient_name,
                                 method, address, subject, body, status,
                                 reviewed_by, reviewed_at, prepared_by, prepared_at, queued_by, queued_at, sent_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                 ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
        params![
            d.case_id, d.intake_id, d.hearing_id, d.kind, d.template, d.party, d.recipient,
            d.method, d.address, d.subject, d.body, status,
            sent_at.is_some().then_some(a.user_id),          // reviewed_by
            sent_at.clone(),                                 // reviewed_at
            a.user_id,                                       // prepared_by (always)
            prepared,                                        // prepared_at
            sent_at.is_some().then_some(a.user_id),          // queued_by
            sent_at.clone(),                                 // queued_at
            sent_at                                          // sent_at
        ],
    )?;
    let id = tx.last_insert_rowid();
    audit::record(
        tx,
        Some(a),
        Event::new(
            "dispatch.prepared",
            "dispatch",
            id,
            format!("{} for {}", d.kind.replace('_', " "), d.recipient),
        )
        .case(d.case_id),
    )?;
    for item in &d.items {
        tx.execute(
            "INSERT INTO dispatch_items (dispatch_id, document_version_id) VALUES (?1, ?2)",
            params![id, item.version_id],
        )?;
    }
    if let Some(n) = d.sent_days_ago {
        let attachments = json!(d
            .items
            .iter()
            .map(|i| json!({ "filename": i.filename, "sha256": i.sha256, "size_bytes": i.size_bytes, "document_version_id": i.version_id }))
            .collect::<Vec<Value>>());
        let receipt = if d.method == "email" {
            tx.execute(
                "INSERT INTO mailbox (dispatch_id, attempt_no, to_address, subject, body, attachments, delivered_at)
                 VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6)",
                params![id, d.address.unwrap_or_default(), d.subject, d.body, attachments.to_string(), ts(-n)],
            )?;
            format!("local-mailbox:{}", tx.last_insert_rowid())
        } else { "manual".to_string() };
        tx.execute(
            "INSERT INTO delivery_attempts (dispatch_id, attempt_no, status, technical_receipt, at) VALUES (?1, 1, 'sent', ?2, ?3)",
            params![id, receipt, ts(-n)],
        )?;
        audit::record(
            tx,
            Some(a),
            Event::new(
                "dispatch.sent",
                "dispatch",
                id,
                format!("{} ({})", crate::api::common::dispatch_activity(tx, id, if d.method == "email" && d.kind != "copies" { "delivered to the local mailbox" } else { "sent" })?, d.address.unwrap_or(d.recipient)),
            )
            .case(d.case_id),
        )?;
    }
    if let Some(n) = d.confirmed_days_ago {
        tx.execute(
            "INSERT INTO delivery_confirmations (dispatch_id, kind, note, occurred_date, recorded_by, recorded_at)
             VALUES (?1, 'human_handover', ?2, ?3, ?4, ?5)",
            params![id, format!("Confirmed that it reached {}", d.recipient), ldate(-n), a.user_id, ts(-n)],
        )?;
        audit::record(
            tx,
            Some(a),
            Event::new(
                "dispatch.delivered",
                "dispatch",
                id,
                format!("Delivery to {} confirmed", d.recipient),
            )
            .case(d.case_id),
        )?;
    }
    Ok(id)
}

fn next_intake_ref(tx: &Transaction, received_date: &str) -> AppResult<String> {
    let prefix = crate::db::setting(tx, "intake_reference_prefix", "IN")?;
    let year = crate::time::year_of(received_date)?;
    let max: Option<String> = tx
        .query_row(
            "SELECT MAX(reference) FROM intakes WHERE reference LIKE ?1",
            [format!("{prefix}-{year}-%")],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let n = max
        .and_then(|m| m.rsplit('-').next().and_then(|s| s.parse::<i64>().ok()))
        .unwrap_or(0)
        + 1;
    Ok(format!("{prefix}-{year}-{n:04}"))
}

#[allow(clippy::too_many_arguments)]
fn intake(
    tx: &Transaction,
    a: &Actor,
    sender_party: Option<i64>,
    sender: &str,
    channel: &str,
    island: &str,
    desc: &str,
    received_days_ago: i64,
) -> AppResult<(i64, String)> {
    let received = ldate(-received_days_ago);
    let reference = next_intake_ref(tx, &received)?;
    tx.execute(
        "INSERT INTO intakes (reference, status, sender_party_id, sender_name, channel, origin_island, document_date,
                              received_date, entered_at, description, is_paper_original, paper_location, created_by, updated_at)
         VALUES (?1, 'received', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, 'Registry cabinet B, shelf 2 (DEMO)', ?10, ?8)",
        params![reference, sender_party, sender, channel, island, ldate(-(received_days_ago + 2)), received, ts(-received_days_ago), desc, a.user_id],
    )?;
    let id = tx.last_insert_rowid();
    audit::record(
        tx,
        Some(a),
        Event::new(
            "intake.received",
            "intake",
            id,
            format!("Received {reference} from {sender}"),
        ),
    )?;
    Ok((id, reference))
}

// --------------------------------------------------------------------------- the dataset (spec §12)

/// Fictional demo dataset. Called once when the demo template is built; the personas,
/// DEMO registries and DEMO rooms already exist. None of these is the visitor's own
/// walkthrough case — a visitor registers a new case from a fresh intake.
pub fn seed_cases(tx: &Transaction, db: &Db) -> AppResult<()> {
    let (olga, elena, viktor, sergei) = (
        user(tx, "olga")?,
        user(tx, "elena")?,
        user(tx, "viktor")?,
        user(tx, "sergei")?,
    );
    let (a_olga, a_elena, a_viktor) = (actor(tx, olga)?, actor(tx, elena)?, actor(tx, viktor)?);
    let (civ, fam, crm) = (
        registry(tx, "DEMO-CIV")?,
        registry(tx, "DEMO-FAM")?,
        registry(tx, "DEMO-CRM")?,
    );
    let (room1, room2) = (room(tx, "DEMO Courtroom 1")?, room(tx, "DEMO Courtroom 2")?);

    // 1. Incomplete intake from Nukufetau — the request for missing items is already out.
    let tavita = party(
        tx,
        &a_olga,
        "person",
        "Jonah Whitlock (DEMO)",
        Some("jonah.whitlock@example.invalid"),
        Some("nukufetau"),
    )?;
    let (in1, in1_ref) = intake(
        tx,
        &a_olga,
        Some(tavita),
        "Jonah Whitlock (DEMO)",
        "post",
        "nukufetau",
        "Statement about damage to a family water tank (fictional DEMO filing)",
        3,
    )?;
    let missing = "A signed claim form and a copy of the repair invoice".to_string();
    let (subject, body) = crate::api::common::render_template(
        tx,
        "information_request",
        &[
            ("recipient", "Jonah Whitlock (DEMO)".to_string()),
            ("intake_reference", in1_ref.clone()),
            ("received_date", ldate(-3)),
            ("missing_items", missing.clone()),
        ],
    )?;
    let d1 = dispatch(
        tx,
        &a_olga,
        Out {
            case_id: None,
            intake_id: Some(in1),
            hearing_id: None,
            kind: "information_request",
            template: Some("information_request"),
            party: Some(tavita),
            recipient: "Jonah Whitlock (DEMO)",
            method: "email",
            address: Some("jonah.whitlock@example.invalid"),
            subject: &subject,
            body: &body,
            sent_days_ago: Some(2),
            confirmed_days_ago: None,
            items: vec![],
        },
    )?;
    tx.execute(
        "UPDATE intakes SET status = 'needs_information', missing_items = ?2, status_reason = 'Waiting for the sender', updated_at = ?3 WHERE id = ?1",
        params![in1, missing, ts(-2)],
    )?;
    tx.execute(
        "INSERT INTO intake_messages (intake_id, direction, body, dispatch_id, created_by, created_at) VALUES (?1, 'outgoing', ?2, ?3, ?4, ?5)",
        params![in1, format!("Requested: {missing}"), d1, a_olga.user_id, ts(-2)],
    )?;
    audit::record(
        tx,
        Some(&a_olga),
        Event::new(
            "intake.information_requested",
            "intake",
            in1,
            format!("Requested missing information for {in1_ref}"),
        )
        .details(json!({ "missing_items": missing, "dispatch_id": d1 })),
    )?;

    // 2. Active civil case: judge assigned, hearing in 10 days, notices on their way.
    let tomasi = party(
        tx,
        &a_olga,
        "person",
        "Edwin Marlowe (DEMO)",
        Some("edwin.marlowe@example.invalid"),
        Some("funafuti"),
    )?;
    let selima = party(
        tx,
        &a_olga,
        "person",
        "Clara Bennett (DEMO)",
        Some("clara.bennett@example.invalid"),
        Some("funafuti"),
    )?;
    let (case2, num2) = new_case(
        tx,
        &a_olga,
        civ,
        "civil_contract",
        "DEMO — Unpaid fishing boat repair",
        "Fictional DEMO dispute about an unpaid repair invoice",
        30,
        false,
        olga,
    )?;
    set_status(tx, &a_olga, case2, "registered", "active", None, None, 29)?;
    participate(
        tx,
        &a_olga,
        case2,
        tomasi,
        "claimant",
        Some("edwin.marlowe@example.invalid"),
    )?;
    participate(
        tx,
        &a_olga,
        case2,
        selima,
        "respondent",
        Some("clara.bennett@example.invalid"),
    )?;
    assign(tx, &a_olga, case2, olga, "clerk", "Registered the case", 30)?;
    assign(
        tx,
        &a_elena,
        case2,
        viktor,
        "judge",
        "Allocated by the registry head",
        28,
    )?;
    assign(
        tx,
        &a_elena,
        case2,
        sergei,
        "service_officer",
        "Will deliver notices",
        28,
    )?;
    let h2 = hearing(
        tx,
        &a_olga,
        case2,
        "hearing",
        "scheduled",
        &local_time(10, "10:00")?,
        &local_time(10, "11:00")?,
        room1,
        viktor,
        None,
    )?;
    hparticipant(tx, h2, Some(tomasi), None, "claimant", None)?;
    hparticipant(tx, h2, Some(selima), None, "respondent", None)?;
    hparticipant(tx, h2, None, Some(viktor), "judge", None)?;
    audit::record(
        tx,
        Some(&a_olga),
        Event::new(
            "hearing.scheduled",
            "hearing",
            h2,
            format!("Hearing scheduled for {}", ldate(10)),
        )
        .case(Some(case2)),
    )?;
    for (party_id, name, method, confirmed) in [
        (tomasi, "Edwin Marlowe (DEMO)", "email", true),
        (selima, "Clara Bennett (DEMO)", "post", false),
    ] {
        let (subject, body) = crate::api::common::render_template(
            tx,
            "hearing_notice",
            &[
                ("recipient", name.to_string()),
                ("case_number", num2.clone()),
                (
                    "case_title",
                    "DEMO — Unpaid fishing boat repair".to_string(),
                ),
                ("hearing_type", "hearing".to_string()),
                ("hearing_local", format!("{} 10:00", ldate(10))),
                ("room", "DEMO Courtroom 1".to_string()),
            ],
        )?;
        dispatch(
            tx,
            &a_olga,
            Out {
                case_id: Some(case2),
                intake_id: None,
                hearing_id: Some(h2),
                kind: "notice",
                template: Some("hearing_notice"),
                party: Some(party_id),
                recipient: name,
                method,
                address: Some(if method == "email" { "edwin.marlowe@example.invalid" } else { "Funafuti, Tuvalu (DEMO)" }),
                subject: &subject,
                body: &body,
                sent_days_ago: Some(3),
                confirmed_days_ago: if confirmed { Some(2) } else { None },
                items: vec![],
            },
        )?;
    }
    task(
        tx,
        &a_olga,
        case2,
        "general",
        "Confirm the hearing notice reached Clara Bennett (DEMO) and record it",
        sergei,
        Some(&ldate(9)),
    )?;

    // 3. Civil case whose 10 Nov 2026 hearing was adjourned to 12 Nov 2026.
    let paleki = party(
        tx,
        &a_olga,
        "person",
        "Felix Rowan (DEMO)",
        Some("felix.rowan@example.invalid"),
        Some("funafuti"),
    )?;
    let moea = party(
        tx,
        &a_olga,
        "person",
        "Ada Pemberton (DEMO)",
        Some("ada.pemberton@example.invalid"),
        Some("vaitupu"),
    )?;
    let (case3, _num3) = new_case(
        tx,
        &a_olga,
        civ,
        "civil_property",
        "DEMO — Boundary fence contribution",
        "Fictional DEMO dispute about sharing the cost of a boundary fence",
        45,
        false,
        olga,
    )?;
    set_status(tx, &a_olga, case3, "registered", "active", None, None, 44)?;
    participate(
        tx,
        &a_olga,
        case3,
        paleki,
        "claimant",
        Some("felix.rowan@example.invalid"),
    )?;
    participate(
        tx,
        &a_olga,
        case3,
        moea,
        "respondent",
        Some("ada.pemberton@example.invalid"),
    )?;
    assign(tx, &a_olga, case3, olga, "clerk", "Registered the case", 45)?;
    assign(
        tx,
        &a_elena,
        case3,
        viktor,
        "judge",
        "Allocated by the registry head",
        43,
    )?;
    assign(
        tx,
        &a_elena,
        case3,
        sergei,
        "service_officer",
        "Will deliver notices",
        43,
    )?;
    let old_h = hearing(
        tx,
        &a_olga,
        case3,
        "hearing",
        "adjourned",
        &crate::time::local_to_utc("2026-11-10T14:00")?,
        &crate::time::local_to_utc("2026-11-10T15:00")?,
        room2,
        viktor,
        None,
    )?;
    tx.execute(
        "UPDATE hearings SET status_reason = 'Respondent requested time to obtain counsel', status_authorised_by = 'Judge Viktor Hale' WHERE id = ?1",
        [old_h],
    )?;
    let new_h = hearing(
        tx,
        &a_olga,
        case3,
        "hearing",
        "scheduled",
        &crate::time::local_to_utc("2026-11-12T14:00")?,
        &crate::time::local_to_utc("2026-11-12T15:00")?,
        room2,
        viktor,
        Some("Adjourned from 10 Nov 2026"),
    )?;
    tx.execute(
        "UPDATE hearings SET previous_hearing_id = ?2 WHERE id = ?1",
        params![new_h, old_h],
    )?;
    tx.execute(
        "UPDATE hearings SET adjourned_to_id = ?2 WHERE id = ?1",
        params![old_h, new_h],
    )?;
    for h in [old_h, new_h] {
        hparticipant(tx, h, Some(paleki), None, "claimant", None)?;
        hparticipant(tx, h, Some(moea), None, "respondent", None)?;
        hparticipant(tx, h, None, Some(viktor), "judge", None)?;
    }
    audit::record(
        tx,
        Some(&a_olga),
        Event::new(
            "hearing.adjourned",
            "hearing",
            old_h,
            "Hearing of 10 Nov 2026 adjourned to 12 Nov 2026",
        )
        .case(Some(case3))
        .details(json!({
            "reason": "Respondent requested time to obtain counsel",
            "authorised_by": "Judge Viktor Hale",
            "new_hearing_id": new_h,
        })),
    )?;
    for name in ["Felix Rowan (DEMO)", "Ada Pemberton (DEMO)"] {
        task(
            tx,
            &a_olga,
            case3,
            "renotify",
            &format!("Send the new hearing date (12 Nov 2026, 14:00) to {name}"),
            sergei,
            Some("2026-11-11"),
        )?;
    }
    // 4. Closed family case: hearing held, decision finalised, copies delivered.
    let ana = party(
        tx,
        &a_olga,
        "person",
        "Nina Hartley (DEMO)",
        Some("nina.hartley@example.invalid"),
        Some("funafuti"),
    )?;
    let keli = party(
        tx,
        &a_olga,
        "person",
        "Owen Hartley (DEMO)",
        Some("owen.hartley@example.invalid"),
        Some("funafuti"),
    )?;
    let (case4, num4) = new_case(
        tx,
        &a_olga,
        fam,
        "family",
        "DEMO — Care arrangements for two children",
        "Fictional DEMO family matter about care arrangements",
        90,
        false,
        olga,
    )?;
    set_status(tx, &a_olga, case4, "registered", "active", None, None, 89)?;
    participate(
        tx,
        &a_olga,
        case4,
        ana,
        "applicant",
        Some("nina.hartley@example.invalid"),
    )?;
    participate(
        tx,
        &a_olga,
        case4,
        keli,
        "respondent",
        Some("owen.hartley@example.invalid"),
    )?;
    assign(tx, &a_olga, case4, olga, "clerk", "Registered the case", 90)?;
    assign(
        tx,
        &a_elena,
        case4,
        viktor,
        "judge",
        "Allocated by the registry head",
        88,
    )?;
    assign(
        tx,
        &a_elena,
        case4,
        sergei,
        "service_officer",
        "Will deliver copies",
        88,
    )?;
    let h4 = hearing(
        tx,
        &a_olga,
        case4,
        "hearing",
        "held",
        &local_time(-20, "09:00")?,
        &local_time(-20, "10:00")?,
        room1,
        viktor,
        None,
    )?;
    hparticipant(tx, h4, Some(ana), None, "applicant", Some(true))?;
    hparticipant(tx, h4, Some(keli), None, "respondent", Some(true))?;
    hparticipant(tx, h4, None, Some(viktor), "judge", Some(true))?;
    tx.execute(
        "UPDATE hearings SET outcome_summary = 'Heard both parties; written order to follow.', next_step = 'Judge issues the order',
                outcome_recorded_by = ?2, outcome_recorded_at = ?3 WHERE id = ?1",
        params![h4, viktor, ts(-20)],
    )?;
    audit::record(
        tx,
        Some(&a_viktor),
        Event::new(
            "hearing.outcome_recorded",
            "hearing",
            h4,
            "Hearing held; outcome recorded",
        )
        .case(Some(case4)),
    )?;
    let (dec_doc, dec_v) = document(
        tx,
        db,
        case4,
        "DEMO - Care arrangements order",
        "decision",
        "court",
        "administrative",
        None,
        &a_viktor,
        18,
        &[
            "DEMO - FICTIONAL ORDER - NOT A COURT RECORD",
            "Order about care arrangements for two children.",
            "Made by Judge Viktor Hale in the DEMO Magistrates Court.",
        ],
    )?;
    tx.execute(
        "INSERT INTO decisions (case_id, title, decision_date, status, document_id, document_version_id, hearing_id,
                                author_user_id, finalised_by, finalised_at, created_at)
         VALUES (?1, 'DEMO - Care arrangements order', ?2, 'finalised', ?3, ?4, ?5, ?6, ?6, ?7, ?7)",
        params![case4, ldate(-18), dec_doc, dec_v.version_id, h4, viktor, ts(-18)],
    )?;
    let dec_id = tx.last_insert_rowid();
    audit::record(
        tx,
        Some(&a_viktor),
        Event::new(
            "decision.finalised",
            "decision",
            dec_id,
            "Decision finalised",
        )
        .case(Some(case4)),
    )?;
    for (party_id, name, method) in [
        (ana, "Nina Hartley (DEMO)", "email"),
        (keli, "Owen Hartley (DEMO)", "post"),
    ] {
        let (subject, body) = crate::api::common::render_template(
            tx,
            "copy_dispatch",
            &[
                ("recipient", name.to_string()),
                ("case_number", num4.clone()),
                (
                    "items",
                    "DEMO - Care arrangements order (version 1)".to_string(),
                ),
            ],
        )?;
        dispatch(
            tx,
            &a_olga,
            Out {
                case_id: Some(case4),
                intake_id: None,
                hearing_id: None,
                kind: "copies",
                template: Some("copy_dispatch"),
                party: Some(party_id),
                recipient: name,
                method,
                address: Some(if method == "email" { "nina.hartley@example.invalid" } else { "Funafuti, Tuvalu (DEMO)" }),
                subject: &subject,
                body: &body,
                sent_days_ago: Some(17),
                confirmed_days_ago: Some(16),
                items: vec![dec_v.clone()],
            },
        )?;
    }
    // The order went out as the decision's copy, exactly as the dispatch workflow records it.
    tx.execute(
        "UPDATE dispatch_items SET material_kind = 'decision_copy', decision_id = ?1
         WHERE document_version_id = ?2 AND dispatch_id IN (SELECT id FROM dispatches WHERE case_id = ?3 AND kind = 'copies')",
        params![dec_id, dec_v.version_id, case4],
    )?;
    set_status(
        tx,
        &a_olga,
        case4,
        "active",
        "closed",
        Some("Decision made and copies delivered"),
        Some("decided"),
        15,
    )?;
    tx.execute(
        "UPDATE cases SET closure_basis = 'decided', closure_note = 'Decision made and copies delivered to both parties',
                closed_date = ?2, closed_at = ?3, closed_by = ?4, basis_decision_id = ?5 WHERE id = ?1",
        params![case4, ldate(-15), ts(-15), olga, dec_id],
    )?;
    tx.execute("UPDATE case_status_history SET basis_decision_id=?2 WHERE case_id=?1 AND to_status='closed'",params![case4,dec_id])?;
    audit::record(
        tx,
        Some(&a_olga),
        Event::new(
            "case.closed",
            "case",
            case4,
            format!("Case {num4} closed (decided)"),
        )
        .case(Some(case4)),
    )?;

    // 5. Case closed as settled, reopened two days ago after a follow-up filing.
    let malia = party(
        tx,
        &a_olga,
        "person",
        "Iris Calloway (DEMO)",
        Some("iris.calloway@example.invalid"),
        Some("funafuti"),
    )?;
    let traders = party(
        tx,
        &a_olga,
        "organisation",
        "Island Traders Ltd (DEMO)",
        Some("office@traders.invalid"),
        Some("funafuti"),
    )?;
    let (case5, num5) = new_case(
        tx,
        &a_olga,
        civ,
        "civil_contract",
        "DEMO — Refund for faulty solar panels",
        "Fictional DEMO claim about a refund for faulty equipment",
        60,
        false,
        olga,
    )?;
    set_status(tx, &a_olga, case5, "registered", "active", None, None, 59)?;
    participate(
        tx,
        &a_olga,
        case5,
        malia,
        "claimant",
        Some("iris.calloway@example.invalid"),
    )?;
    participate(
        tx,
        &a_olga,
        case5,
        traders,
        "respondent",
        Some("office@traders.invalid"),
    )?;
    assign(tx, &a_olga, case5, olga, "clerk", "Registered the case", 60)?;
    assign(
        tx,
        &a_elena,
        case5,
        viktor,
        "judge",
        "Allocated by the registry head",
        58,
    )?;
    let (_, settlement_basis) = document(tx, db, case5, "DEMO - Settlement agreement", "correspondence",
        "party", "party_material", None, &a_olga, 30,
        &["DEMO - FICTIONAL SETTLEMENT", "The fictional parties agreed to settle the refund claim."])?;
    set_status(
        tx,
        &a_olga,
        case5,
        "active",
        "closed",
        Some("Parties settled"),
        Some("settled"),
        30,
    )?;
    tx.execute(
        "UPDATE cases SET closure_basis = 'settled', closure_note = 'Parties settled',
                closed_date = ?2, closed_at = ?3, closed_by = ?4, basis_document_version_id = ?5 WHERE id = ?1",
        params![case5, ldate(-30), ts(-30), olga, settlement_basis.version_id],
    )?;
    tx.execute("UPDATE case_status_history SET basis_document_version_id=?2 WHERE case_id=?1 AND to_status='closed'",params![case5,settlement_basis.version_id])?;
    audit::record(
        tx,
        Some(&a_olga),
        Event::new(
            "case.closed",
            "case",
            case5,
            format!("Case {num5} closed (settled)"),
        )
        .case(Some(case5)),
    )?;
    let (in5, in5_ref) = intake(
        tx,
        &a_olga,
        Some(malia),
        "Iris Calloway (DEMO)",
        "counter",
        "funafuti",
        "Follow-up application: the settlement was not honoured (fictional DEMO filing)",
        2,
    )?;
    tx.execute(
        "UPDATE intakes SET status = 'linked_to_case', case_id = ?2, status_reason = 'Linked to the earlier case', updated_at = ?3 WHERE id = ?1",
        params![in5, case5, ts(-2)],
    )?;
    audit::record(
        tx,
        Some(&a_olga),
        Event::new(
            "intake.linked",
            "intake",
            in5,
            format!("{in5_ref} linked to case {num5}"),
        )
        .case(Some(case5)),
    )?;
    set_status(
        tx,
        &a_elena,
        case5,
        "closed",
        "reopened",
        Some("Settlement not honoured; the claimant filed a follow-up application"),
        None,
        2,
    )?;
    audit::record(
        tx,
        Some(&a_elena),
        Event::new("case.reopened", "case", case5, format!("Case {num5} reopened"))
            .case(Some(case5))
            .details(json!({ "reason": "Settlement not honoured; the claimant filed a follow-up application", "intake_id": in5 })),
    )?;

    // 6. Restricted family case: only Olga and Viktor are assigned; files are protected.
    let sina = party(
        tx,
        &a_olga,
        "person",
        "Rosa Ingram (DEMO)",
        Some("rosa.ingram@example.invalid"),
        Some("funafuti"),
    )?;
    let teo = party(
        tx,
        &a_olga,
        "person",
        "Leo Ingram (DEMO)",
        Some("leo.ingram@example.invalid"),
        Some("funafuti"),
    )?;
    let (case6, _num6) = new_case(
        tx,
        &a_olga,
        fam,
        "family",
        "DEMO — Guardianship assessment",
        "Fictional DEMO restricted family matter",
        25,
        true,
        olga,
    )?;
    set_status(tx, &a_olga, case6, "registered", "active", None, None, 24)?;
    participate(
        tx,
        &a_olga,
        case6,
        sina,
        "applicant",
        Some("rosa.ingram@example.invalid"),
    )?;
    participate(
        tx,
        &a_olga,
        case6,
        teo,
        "respondent",
        Some("leo.ingram@example.invalid"),
    )?;
    assign(tx, &a_olga, case6, olga, "clerk", "Registered the case", 25)?;
    assign(
        tx,
        &a_elena,
        case6,
        viktor,
        "judge",
        "Allocated by the registry head",
        23,
    )?;
    assign(
        tx,
        &a_elena,
        case6,
        elena,
        "registry_head",
        "Oversees restricted document access",
        21,
    )?;
    let (med, _med_v) = document(
        tx,
        db,
        case6,
        "DEMO medical report",
        "medical",
        "external",
        "restricted",
        None,
        &a_olga,
        20,
        &[
            "DEMO - FICTIONAL MEDICAL REPORT - NOT A REAL RECORD",
            "Assessment summary for the guardianship application.",
        ],
    )?;
    tx.execute(
        "INSERT INTO document_grants (document_id, user_id, reason, granted_by, granted_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![med, viktor, "Needed to assess the application", elena, ts(-19)],
    )?;
    audit::record(
        tx,
        Some(&a_elena),
        Event::new(
            "document.granted",
            "document",
            med,
            "Restricted document access granted",
        )
        .case(Some(case6))
        .details(json!({ "user_id": viktor, "reason": "Needed to assess the application" })),
    )?;
    document(
        tx,
        db,
        case6,
        "DEMO statement of the applicant",
        "evidence",
        "party",
        "party_material",
        Some(sina),
        &a_olga,
        22,
        &[
            "DEMO - FICTIONAL STATEMENT",
            "Statement of the applicant about the child's care.",
        ],
    )?;
    document(
        tx,
        db,
        case6,
        "DEMO judicial note",
        "judicial_note",
        "court",
        "judicial_note",
        None,
        &a_viktor,
        15,
        &[
            "DEMO - FICTIONAL JUDICIAL NOTE",
            "Private working note of Judge Viktor Hale. Not shared.",
        ],
    )?;

    // A DEMO-CRM case that still needs a judge ("Assign a judge" next step).
    let junior = party(
        tx,
        &a_olga,
        "person",
        "Sam Ashdown (DEMO)",
        Some("sam.ashdown@example.invalid"),
        Some("funafuti"),
    )?;
    let police = party(
        tx,
        &a_olga,
        "organisation",
        "DEMO Police Prosecutions (fictional)",
        None,
        Some("funafuti"),
    )?;
    let (case7, _num7) = new_case(
        tx,
        &a_olga,
        crm,
        "criminal_summary",
        "DEMO — Theft of fishing gear",
        "Fictional DEMO criminal summary matter",
        2,
        false,
        olga,
    )?;
    participate(
        tx,
        &a_olga,
        case7,
        police,
        "applicant",
        Some("Funafuti police station (DEMO)"),
    )?;
    participate(
        tx,
        &a_olga,
        case7,
        junior,
        "defendant",
        Some("Funafuti (DEMO)"),
    )?;
    assign(tx, &a_olga, case7, olga, "clerk", "Registered the case", 2)?;

    Ok(())
}
