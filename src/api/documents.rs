//! C06 documents & versions and C15 special access (restricted documents, judicial notes).
//! Files live in the private store; the only way to bytes is `/document-versions/{id}/download`,
//! which re-checks the shared policy on every request — an ended assignment or a revoked grant
//! makes an old URL a 404. Restricted documents and judicial notes additionally log
//! `document.viewed_restricted` before bytes are returned.
//!
//! Grant management needs container access (the case or the intake) but NOT read access to the
//! document: the registry head hands out restricted access precisely for documents she cannot
//! open herself, and a judge shares a note only he can read.

use super::common::{JsonBody, JsonResult, optional, query_json, query_one_json, reason, require_ref, required};
use crate::audit::{self, Event};
use crate::auth::{Actor, Ctx, IdemKey, idempotent};
use crate::error::{AppError, AppResult};
use crate::policy::{self, perm};
use crate::state::AppState;
use crate::storage::StoredFile;
use axum::extract::multipart::MultipartError;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

const VISIBILITIES: &[&str] = &["administrative", "party_material", "restricted", "judicial_note"];
const SOURCES: &[&str] = &["court", "party", "external"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/documents", get(list_all))
        .route("/documents/{id}", get(detail).patch(update))
        .route("/documents/{id}/versions", post(add_version))
        .route("/documents/{id}/grants", post(grant_add))
        .route("/documents/{id}/grants/{gid}", delete(grant_revoke))
        .route("/document-versions/{id}/download", get(download))
        .route("/cases/{id}/documents", get(list_for_case).post(upload_to_case))
        .route("/intakes/{id}/documents", post(upload_to_intake))
        .route("/cases/{id}/restricted-documents", get(list_restricted_for_case))
}

// ------------------------------------------------------------------ JSON shapes

/// Shared document columns (list + detail). Includes the case number for the cross-case screen.
const DOC_SELECT: &str = "
    SELECT d.id, d.case_id, cs.number AS case_number, d.intake_id, d.title, d.doc_type,
           COALESCE((SELECT label FROM ref_items r WHERE r.kind = 'document_type' AND r.code = d.doc_type), d.doc_type) AS doc_type_label,
           d.source, d.source_party_id, sp.name AS source_party_name,
           d.document_date, d.received_date, d.visibility, d.is_paper_original, d.original_location, d.legal_hold,
           d.created_by, cu.display_name AS created_by_name, d.created_at, d.version,
           (SELECT COUNT(*) FROM document_versions v WHERE v.document_id = d.id) AS version_count,
           (SELECT MAX(v.id) FROM document_versions v WHERE v.document_id = d.id) AS latest_version_id
    FROM documents d
    LEFT JOIN cases cs ON cs.id = d.case_id
    LEFT JOIN parties sp ON sp.id = d.source_party_id
    LEFT JOIN users cu ON cu.id = d.created_by";

const VERSION_SELECT: &str = "
    SELECT v.id, v.version_no, v.filename, v.content_type, v.size_bytes, v.sha256, v.scan_status, v.scan_note,
           v.note, v.uploaded_by, u.display_name AS uploaded_by_name, v.uploaded_at
    FROM document_versions v LEFT JOIN users u ON u.id = v.uploaded_by";

const GRANT_SELECT: &str = "
    SELECT g.id, g.user_id, u.display_name AS user_name, g.reason,
           gb.display_name AS granted_by_name, g.granted_at, g.revoked_at
    FROM document_grants g
    LEFT JOIN users u ON u.id = g.user_id
    LEFT JOIN users gb ON gb.id = g.granted_by";

/// The actor may hand out access: `document.grant_restricted` for restricted documents, or the
/// author of a judicial note (no permission substitutes for authorship).
fn can_manage_grants(actor: &Actor, doc: &Value) -> bool {
    match doc["visibility"].as_str() {
        Some("restricted") => actor.has(perm::DOCUMENT_GRANT_RESTRICTED),
        Some("judicial_note") => doc["created_by"].as_i64() == Some(actor.user_id),
        _ => false,
    }
}

/// Full document JSON: scalar fields + versions + grants (only when the actor may manage them)
/// + `used_by` decisions/dispatches visible to the actor.
fn detail_json(conn: &Connection, actor: &Actor, doc_id: i64) -> AppResult<Value> {
    let mut doc = query_one_json(conn, &format!("{DOC_SELECT} WHERE d.id = ?1"), [doc_id])?;
    doc["versions"] = json!(query_json(
        conn,
        &format!("{VERSION_SELECT} WHERE v.document_id = ?1 ORDER BY v.version_no"),
        [doc_id]
    )?);
    if can_manage_grants(actor, &doc) {
        doc["grants"] = json!(query_json(
            conn,
            &format!("{GRANT_SELECT} WHERE g.document_id = ?1 ORDER BY g.id"),
            [doc_id]
        )?);
    }
    let dec_vis = policy::case_visible_sql(actor, "dd.case_id");
    let decisions = query_json(
        conn,
        &format!("SELECT dd.id, dd.title, dd.status FROM decisions dd WHERE dd.document_id = ?1 AND {dec_vis} ORDER BY dd.id"),
        [doc_id],
    )?;
    let disp_vis = format!(
        "((dp.case_id IS NOT NULL AND {}) OR (dp.case_id IS NULL AND {}))",
        policy::case_visible_sql(actor, "dp.case_id"),
        if actor.has(perm::INTAKE_MANAGE) { "1" } else { "0" }
    );
    let dispatches = query_json(
        conn,
        &format!(
            "SELECT DISTINCT dp.id, dp.recipient_name, dp.status FROM dispatches dp
             JOIN dispatch_items di ON di.dispatch_id = dp.id
             JOIN document_versions dv ON dv.id = di.document_version_id
             WHERE dv.document_id = ?1 AND {disp_vis} ORDER BY dp.id"
        ),
        [doc_id],
    )?;
    doc["used_by"] = json!({ "decisions": decisions, "dispatches": dispatches });
    Ok(doc)
}

// ------------------------------------------------------------------ lists & detail

async fn list_for_case(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            policy::require_case(c, &actor, id)?;
            let sql = format!(
                "{DOC_SELECT} WHERE d.case_id = ?1 AND {} ORDER BY d.id",
                policy::document_visible_sql(&actor, "d")
            );
            Ok(json!({ "items": query_json(c, &sql, [id])? }))
        })
        .await?;
    Ok(Json(v))
}

/// Grant management metadata only: no versions, file access or view audit.
async fn list_restricted_for_case(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            policy::require_case(c, &actor, id)?;
            actor.require(perm::DOCUMENT_GRANT_RESTRICTED)?;
            let mut items = query_json(
                c,
                "SELECT d.id, d.title, d.doc_type, u.display_name AS created_by_name, d.created_at
             FROM documents d LEFT JOIN users u ON u.id = d.created_by
             WHERE d.case_id = ?1 AND d.visibility = 'restricted' ORDER BY d.id",
                [id],
            )?;
            for doc in &mut items {
                doc["grants"] = json!(query_json(
                    c,
                    &format!("{GRANT_SELECT} WHERE g.document_id = ?1 ORDER BY g.id"),
                    [doc["id"].as_i64().unwrap_or_default()]
                )?);
            }
            Ok(json!({ "items": items }))
        })
        .await?;
    Ok(Json(v))
}

fn document_label(id: i64, visibility: &str, title: &str) -> String {
    match visibility {
        "restricted" => format!("Restricted document #{id}"),
        "judicial_note" => format!("Judicial note #{id}"),
        _ => format!("Document '{title}'"),
    }
}

#[derive(Deserialize)]
struct ListQuery {
    q: Option<String>,
    case_id: Option<i64>,
    doc_type: Option<String>,
    visibility: Option<String>,
}

/// Cross-case Documents screen. Only documents the actor may see, newest first, max 500.
async fn list_all(ctx: Ctx, Query(q): Query<ListQuery>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .read(move |c| {
            let sql = format!(
                "{DOC_SELECT} WHERE {}
                   AND (?1 IS NULL OR d.title LIKE ?1
                        OR EXISTS (SELECT 1 FROM document_versions v WHERE v.document_id = d.id AND v.filename LIKE ?1))
                   AND (?2 IS NULL OR d.case_id = ?2)
                   AND (?3 IS NULL OR d.doc_type = ?3)
                   AND (?4 IS NULL OR d.visibility = ?4)
                 ORDER BY d.id DESC LIMIT 500",
                policy::document_visible_sql(&actor, "d")
            );
            let like = optional(&q.q).map(|s| format!("%{s}%"));
            Ok(json!({ "items": query_json(c, &sql, params![like, q.case_id, optional(&q.doc_type), optional(&q.visibility)])? }))
        })
        .await?;
    Ok(Json(v))
}

async fn detail(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    let actor = ctx.actor;
    Ok(Json(ctx.db.read(move |c| {
        policy::require_document(c, &actor, id)?;
        detail_json(c, &actor, id)
    }).await?))
}

// ------------------------------------------------------------------ uploads

fn mp_err(e: MultipartError) -> AppError {
    match e.status() {
        StatusCode::PAYLOAD_TOO_LARGE => AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "The file is too large."),
        _ => AppError::validation(e.body_text()),
    }
}

/// Read a multipart file part with a hard cap; anything larger is refused with 413.
async fn read_file_field(field: &mut axum::extract::multipart::Field<'_>, max: u64) -> AppResult<Vec<u8>> {
    let mut buf = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(mp_err)? {
        if buf.len() as u64 + chunk.len() as u64 > max {
            return Err(AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "The file is too large."));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

#[derive(Default, Clone)]
struct UploadForm {
    title: String,
    doc_type: String,
    source: String,
    source_party_id: Option<i64>,
    document_date: Option<String>,
    received_date: Option<String>,
    visibility: String,
    is_paper_original: bool,
    original_location: Option<String>,
    note: Option<String>,
}

struct ParsedUpload {
    form: UploadForm,
    filename: String,
    bytes: Vec<u8>,
}

async fn parse_upload(mp: &mut Multipart, max: u64) -> AppResult<ParsedUpload> {
    let mut form = UploadForm::default();
    let mut filename = String::new();
    let mut bytes = None;
    while let Some(mut field) = mp.next_field().await.map_err(mp_err)? {
        match field.name() {
            Some("file") => {
                filename = field.file_name().unwrap_or("").to_string();
                bytes = Some(read_file_field(&mut field, max).await?);
            }
            Some(name) => {
                let name = name.to_string();
                let text = field.text().await.map_err(mp_err)?;
                match name.as_str() {
                    "title" => form.title = text,
                    "doc_type" => form.doc_type = text,
                    "source" => form.source = text,
                    "source_party_id" => {
                        form.source_party_id = match text.trim() {
                            "" => None,
                            v => Some(
                                v.parse::<i64>()
                                    .map_err(|_| AppError::validation("The source party is not a number."))?,
                            ),
                        };
                    }
                    "document_date" => form.document_date = Some(text),
                    "received_date" => form.received_date = Some(text),
                    "visibility" => form.visibility = text,
                    "is_paper_original" => form.is_paper_original = matches!(text.trim(), "true" | "1" | "on" | "yes"),
                    "original_location" => form.original_location = Some(text),
                    "note" => form.note = Some(text),
                    _ => {}
                }
            }
            None => {
                let _ = field.text().await;
            }
        }
    }
    let bytes = bytes.ok_or_else(|| AppError::validation("Attach a file."))?;
    Ok(ParsedUpload { form, filename, bytes })
}

fn insert_version(conn: &Connection, doc_id: i64, version_no: i64, f: &StoredFile, note: Option<&str>, actor: &Actor) -> AppResult<i64> {
    conn.execute(
        "INSERT INTO document_versions (document_id, version_no, filename, content_type, size_bytes, sha256, storage_key,
                                        scan_status, scan_note, note, uploaded_by, uploaded_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            doc_id,
            version_no,
            f.filename,
            f.content_type,
            f.size_bytes,
            f.sha256,
            f.storage_key,
            f.scan_status,
            f.scan_note,
            note,
            actor.user_id,
            crate::time::now_utc()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Validate the entire form before storing bytes; repeat inside the write for reference checks.
fn validate_upload(conn: &Connection, actor: &Actor, f: &UploadForm) -> AppResult<UploadForm> {
    let title = required(&f.title, "Title")?;
    let mut doc_type = required(&f.doc_type, "Document type")?;
    let mut visibility = required(&f.visibility, "Visibility")?;
    if !VISIBILITIES.contains(&visibility.as_str()) {
        return Err(AppError::validation("Unknown visibility.").with_details(json!({ "field": "visibility" })));
    }
    // A judicial working note is private to its author from the start; only a judge files one.
    // Choosing either the type or the visibility makes the whole document a judicial note.
    if visibility == "judicial_note" || doc_type == "judicial_note" {
        if !actor.is_judge {
            return Err(AppError::forbidden("Only a judge can file a judicial working note."));
        }
        visibility = "judicial_note".to_string();
        doc_type = "judicial_note".to_string();
    }
    require_ref(conn, "document_type", &doc_type)?;
    if !SOURCES.contains(&f.source.as_str()) {
        return Err(AppError::validation("Source must be 'court', 'party' or 'external'.").with_details(json!({ "field": "source" })));
    }
    if let Some(pid) = f.source_party_id {
        conn.query_row("SELECT id FROM parties WHERE id = ?1", [pid], |r| r.get::<_, i64>(0))
            .optional()?
            .ok_or_else(|| AppError::validation("Unknown source party.").with_details(json!({ "field": "source_party_id" })))?;
    }
    let document_date = crate::time::parse_opt_date(f.document_date.as_deref())?;
    let received_date = crate::time::parse_opt_date(f.received_date.as_deref())?;
    let original_location = optional(&f.original_location);
    if f.is_paper_original && original_location.is_none() {
        return Err(AppError::validation("Say where the paper original is kept.").with_details(json!({ "field": "original_location" })));
    }
    Ok(UploadForm {
        title,
        doc_type,
        visibility,
        document_date,
        received_date,
        original_location,
        ..f.clone()
    })
}

fn insert_document(
    conn: &Connection,
    actor: &Actor,
    case_id: Option<i64>,
    intake_id: Option<i64>,
    f: &UploadForm,
    file: &StoredFile,
) -> AppResult<i64> {
    let f = validate_upload(conn, actor, f)?;
    // A scan of a paper original stays a scan: is_paper_original records what the clerk received
    // and original_location says where it is kept; the uploaded bytes never pretend to be it.
    conn.execute(
        "INSERT INTO documents (case_id, intake_id, title, doc_type, source, source_party_id, document_date, received_date,
                                visibility, is_paper_original, original_location, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            case_id,
            intake_id,
            f.title,
            f.doc_type,
            f.source,
            f.source_party_id,
            f.document_date,
            f.received_date,
            f.visibility,
            f.is_paper_original,
            f.original_location,
            actor.user_id,
            crate::time::now_utc()
        ],
    )?;
    let doc_id = conn.last_insert_rowid();
    insert_version(conn, doc_id, 1, file, optional(&f.note).as_deref(), actor)?;
    Ok(doc_id)
}

/// Request value recorded for `document.upload` idempotency: the text fields plus the checksum.
fn upload_request_value(case_id: Option<i64>, intake_id: Option<i64>, p: &ParsedUpload) -> Value {
    json!({
        "case_id": case_id,
        "intake_id": intake_id,
        "title": p.form.title.trim(),
        "doc_type": p.form.doc_type.trim(),
        "source": p.form.source.trim(),
        "source_party_id": p.form.source_party_id,
        "document_date": p.form.document_date,
        "received_date": p.form.received_date,
        "visibility": p.form.visibility.trim(),
        "is_paper_original": p.form.is_paper_original,
        "original_location": p.form.original_location,
        "note": p.form.note,
        "filename": crate::storage::sanitize_filename(&p.filename),
        "sha256": crate::auth::sha256_hex(&p.bytes),
    })
}

fn check_upload_target_case(conn: &Connection, actor: &Actor, case_id: i64) -> AppResult<()> {
    let case = policy::require_case_perm(conn, actor, case_id, perm::DOCUMENT_MANAGE)?;
    if case.status == "closed" {
        return Err(AppError::invalid_transition("Reopen the case before adding documents."));
    }
    Ok(())
}

fn check_upload_target_intake(conn: &Connection, actor: &Actor, intake_id: i64) -> AppResult<Option<i64>> {
    let intake = super::intake::require_open_intake(conn, actor, intake_id)?;
    actor.require(perm::INTAKE_MANAGE)?;
    let case_id = intake["case_id"].as_i64();
    check_open_case(conn, actor, case_id)?;
    Ok(case_id)
}

fn check_open_case(conn: &Connection, actor: &Actor, case_id: Option<i64>) -> AppResult<()> {
    if let Some(id) = case_id {
        if policy::require_case(conn, actor, id)?.status == "closed" {
            return Err(AppError::invalid_transition("Reopen the case before adding documents."));
        }
    }
    Ok(())
}

/// Reuse the central idempotency comparison without executing a new operation.
fn upload_replay(conn: &Connection, actor: &Actor, key: &Option<String>, op: &str, request: &Value) -> AppResult<Option<Value>> {
    let exists = match key {
        Some(key) => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM operation_keys WHERE user_id = ?1 AND key = ?2)",
            params![actor.user_id, key],
            |r| r.get::<_, bool>(0),
        )?,
        None => false,
    };
    if exists {
        return idempotent::<Value, _>(conn, actor, key, op, request, || Err(AppError::internal("Missing upload replay"))).map(Some);
    }
    Ok(None)
}

async fn store_upload(db: crate::db::Db, bytes: Vec<u8>, filename: String, max: u64) -> AppResult<StoredFile> {
    tokio::task::spawn_blocking(move || crate::storage::store(&db, &bytes, &filename, max))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
}

/// A concurrent replay can win after preflight; discard our blob unless this write used it.
async fn finish_upload(db: crate::db::Db, storage_key: String, result: AppResult<(Value, bool)>) -> JsonResult {
    if !matches!(&result, Ok((_, true))) {
        tokio::task::spawn_blocking(move || crate::storage::discard(&db, &storage_key))
            .await
            .map_err(|e| AppError::internal(e.to_string()))?;
    }
    Ok(Json(result?.0))
}

async fn upload_to_case(
    ctx: Ctx,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    IdemKey(key): IdemKey,
    mut mp: Multipart,
) -> JsonResult {
    let parsed = parse_upload(&mut mp, state.cfg.upload_max_bytes).await?;
    upload_document(ctx, state.cfg.upload_max_bytes, Some(id), None, key, parsed).await
}

async fn upload_to_intake(
    ctx: Ctx,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    IdemKey(key): IdemKey,
    mut mp: Multipart,
) -> JsonResult {
    let parsed = parse_upload(&mut mp, state.cfg.upload_max_bytes).await?;
    upload_document(ctx, state.cfg.upload_max_bytes, None, Some(id), key, parsed).await
}

async fn upload_document(
    ctx: Ctx,
    max: u64,
    case_id: Option<i64>,
    intake_id: Option<i64>,
    key: Option<String>,
    parsed: ParsedUpload,
) -> JsonResult {
    let actor = ctx.actor.clone();
    let preflight_key = key.clone();
    let (mut parsed, case_id, request, replay) = ctx
        .db
        .read(move |c| {
            let case_id = if let Some(id) = intake_id {
                check_upload_target_intake(c, &actor, id)?
            } else {
                check_upload_target_case(c, &actor, case_id.unwrap_or_default())?;
                case_id
            };
            validate_upload(c, &actor, &parsed.form)?;
            let request = upload_request_value(case_id, intake_id, &parsed);
            let replay = upload_replay(c, &actor, &preflight_key, "document.upload", &request)?;
            Ok((parsed, case_id, request, replay))
        })
        .await?;
    if let Some(value) = replay {
        return Ok(Json(value));
    }
    let file = store_upload(ctx.db.clone(), std::mem::take(&mut parsed.bytes), parsed.filename, max).await?;
    let storage_key = file.storage_key.clone();
    let actor = ctx.actor;
    let result = ctx
        .db
        .write(move |tx| {
            if let Some(id) = intake_id {
                check_upload_target_intake(tx, &actor, id)?;
            } else {
                check_upload_target_case(tx, &actor, case_id.unwrap_or_default())?;
            }
            let mut used = false;
            let value = idempotent(tx, &actor, &key, "document.upload", &request, || {
                let doc_id = insert_document(tx, &actor, case_id, intake_id, &parsed.form, &file)?;
                used = true;
                let doc = policy::require_document(tx, &actor, doc_id)?;
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new(
                        "document.uploaded",
                        "document",
                        doc_id,
                        format!("{} added", document_label(doc_id, &doc.visibility, &doc.title)),
                    )
                    .case(case_id)
                    .details(json!({ "filename": file.filename, "sha256": file.sha256,
                    "scan_status": file.scan_status, "intake_id": intake_id })),
                )?;
                detail_json(tx, &actor, doc_id)
            })?;
            Ok((value, used))
        })
        .await;
    finish_upload(ctx.db, storage_key, result).await
}

// ------------------------------------------------------------------ new versions

/// Versions are refused once a finalised (or superseded) decision binds this document:
/// the decision is anchored to its exact revision and can only be corrected by an amendment.
fn check_version_target(conn: &Connection, actor: &Actor, doc_id: i64) -> AppResult<policy::DocRef> {
    let doc = policy::require_document(conn, actor, doc_id)?;
    require_document_editor(actor, &doc)?;
    check_open_case(conn, actor, doc.case_id)?;
    let frozen: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM decisions WHERE document_id = ?1 AND status IN ('finalised','superseded'))",
        [doc.id],
        |r| r.get(0),
    )?;
    if frozen {
        return Err(AppError::invalid_transition(
            "A finalised decision uses this document; record an amendment instead.",
        ));
    }
    Ok(doc)
}

async fn add_version(ctx: Ctx, State(state): State<AppState>, Path(id): Path<i64>, IdemKey(key): IdemKey, mut mp: Multipart) -> JsonResult {
    let mut note = String::new();
    let mut filename = String::new();
    let mut bytes = None;
    while let Some(mut field) = mp.next_field().await.map_err(mp_err)? {
        match field.name() {
            Some("file") => {
                filename = field.file_name().unwrap_or("").to_string();
                bytes = Some(read_file_field(&mut field, state.cfg.upload_max_bytes).await?);
            }
            Some("note") => note = field.text().await.map_err(mp_err)?,
            _ => {
                let _ = field.text().await;
            }
        }
    }
    let bytes = bytes.ok_or_else(|| AppError::validation("Attach a file."))?;
    let note = required(&note, "Note")?;
    let actor = ctx.actor.clone();
    let preflight_key = key.clone();
    let (bytes, filename, request, replay) = ctx
        .db
        .read(move |c| {
            check_version_target(c, &actor, id)?;
            let request = json!({ "document_id": id, "note": note, "filename": crate::storage::sanitize_filename(&filename),
            "sha256": crate::auth::sha256_hex(&bytes) });
            let replay = upload_replay(c, &actor, &preflight_key, "document.version", &request)?;
            Ok((bytes, filename, request, replay))
        })
        .await?;
    if let Some(value) = replay {
        return Ok(Json(value));
    }
    let file = store_upload(ctx.db.clone(), bytes, filename, state.cfg.upload_max_bytes).await?;
    let storage_key = file.storage_key.clone();
    let actor = ctx.actor;
    let result = ctx
        .db
        .write(move |tx| {
            let doc = check_version_target(tx, &actor, id)?;
            let mut used = false;
            let value = idempotent(tx, &actor, &key, "document.version", &request, || {
                let next: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(version_no), 0) + 1 FROM document_versions WHERE document_id = ?1",
                    [id],
                    |r| r.get(0),
                )?;
                let note = request["note"].as_str().unwrap_or_default();
                insert_version(tx, id, next, &file, Some(note), &actor)?;
                used = true;
                audit::record(
                    tx,
                    Some(&actor),
                    Event::new(
                        "document.version_added",
                        "document",
                        id,
                        format!("Version {next} added to a document"),
                    )
                    .case(doc.case_id)
                    .details(json!({ "version_no": next, "filename": file.filename, "sha256": file.sha256, "note": note })),
                )?;
                detail_json(tx, &actor, id)
            })?;
            Ok((value, used))
        })
        .await;
    finish_upload(ctx.db, storage_key, result).await
}

fn require_document_editor(actor: &Actor, doc: &policy::DocRef) -> AppResult<()> {
    if doc.visibility == "judicial_note" && doc.created_by != actor.user_id {
        return Err(AppError::forbidden("Only the author can change a judicial working note."));
    }
    actor.require(perm::DOCUMENT_MANAGE)
}

// ------------------------------------------------------------------ patch

#[derive(Deserialize)]
struct UpdateReq {
    version: i64,
    title: Option<String>,
    visibility: Option<String>,
    original_location: Option<Option<String>>,
    legal_hold: Option<bool>,
}

async fn update(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<UpdateReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let doc = policy::require_document(tx, &actor, id)?;
            require_document_editor(&actor, &doc)?;
            let before = query_one_json(tx, "SELECT * FROM documents WHERE id = ?1", [id])?;
            if before["version"].as_i64() != Some(req.version) {
                return Err(AppError::version_conflict(before));
            }
            let mut new_vis: Option<String> = None;
            if let Some(vis) = optional(&req.visibility) {
                if !VISIBILITIES.contains(&vis.as_str()) {
                    return Err(AppError::validation("Unknown visibility.").with_details(json!({ "field": "visibility" })));
                }
                if vis != doc.visibility {
                    // A judicial note's visibility can never move in either direction.
                    if doc.visibility == "judicial_note" || vis == "judicial_note" {
                        return Err(AppError::validation("A judicial working note cannot change its visibility."));
                    }
                    if (doc.visibility == "restricted" || vis == "restricted")
                        && !(actor.has(perm::DOCUMENT_GRANT_RESTRICTED) || doc.created_by == actor.user_id)
                    {
                        return Err(AppError::forbidden(
                            "Restricting a document or opening a restricted one needs the 'document.grant_restricted' permission.",
                        ));
                    }
                }
                new_vis = Some(vis);
            }
            let title = match &req.title {
                Some(t) => Some(required(t, "Title")?),
                None => None,
            };
            let (loc_set, loc_val): (bool, Option<String>) = match &req.original_location {
                None => (false, None),
                Some(v) => (true, optional(v)),
            };
            tx.execute(
                "UPDATE documents SET title = COALESCE(?2, title), visibility = COALESCE(?3, visibility),
                        original_location = CASE WHEN ?4 THEN ?5 ELSE original_location END,
                        legal_hold = COALESCE(?6, legal_hold), version = version + 1
                 WHERE id = ?1",
                params![id, title, new_vis, loc_set, loc_val, req.legal_hold],
            )?;
            let audit_visibility = if doc.is_sensitive() {
                doc.visibility.as_str()
            } else {
                new_vis.as_deref().unwrap_or(&doc.visibility)
            };
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "document.updated",
                    "document",
                    id,
                    format!("{} details changed", document_label(id, audit_visibility, &doc.title)),
                )
                .case(doc.case_id)
                .details(json!({ "before": before })),
            )?;
            detail_json(tx, &actor, id)
        })
        .await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ grants (restricted docs & judicial notes)

/// Load a document for grant management: it must exist and the actor must reach its container
/// (case or intake). Document visibility itself is deliberately NOT required.
fn load_for_grant(conn: &Connection, actor: &Actor, id: i64) -> AppResult<Value> {
    let container = format!(
        "((d.case_id IS NOT NULL AND {}) OR (d.case_id IS NULL AND {}))",
        policy::case_visible_sql(actor, "d.case_id"),
        if actor.has(perm::INTAKE_MANAGE) { "1" } else { "0" }
    );
    query_one_json(conn, &format!("SELECT d.* FROM documents d WHERE d.id = ?1 AND {container}"), [id])
}

fn authorize_grant(actor: &Actor, doc: &Value) -> AppResult<()> {
    if can_manage_grants(actor, doc) {
        Ok(())
    } else {
        Err(AppError::not_found())
    }
}

fn grant_json(conn: &Connection, gid: i64) -> AppResult<Value> {
    query_one_json(conn, &format!("{GRANT_SELECT} WHERE g.id = ?1"), [gid])
}

#[derive(Deserialize)]
struct GrantReq {
    user_id: i64,
    reason: Option<String>,
}

async fn grant_add(ctx: Ctx, Path(id): Path<i64>, JsonBody(req): JsonBody<GrantReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let doc = load_for_grant(tx, &actor, id)?;
            authorize_grant(&actor, &doc)?;
            let why = reason(&req.reason)?;
            let target = crate::auth::load_actor(tx, req.user_id, None)?
                .ok_or_else(|| AppError::validation("Unknown user.").with_details(json!({ "field": "user_id" })))?;
            if doc["visibility"] == "restricted" && target.user_id == actor.user_id {
                return Err(AppError::forbidden("You cannot grant yourself access to a restricted document."));
            }
            // The grant widens the visibility scope only; the person must already reach the container.
            let reaches_container = match doc["case_id"].as_i64() {
                Some(cid) => policy::can_view_case(tx, &target, cid)?,
                None => target.has(perm::INTAKE_MANAGE),
            };
            if !reaches_container {
                return Err(AppError::validation("This person has no access to the case; a grant cannot help.")
                    .with_details(json!({ "field": "user_id" })));
            }
            if let Some(existing) = query_json(
                tx,
                "SELECT id FROM document_grants WHERE document_id = ?1 AND user_id = ?2 AND revoked_at IS NULL",
                params![id, req.user_id],
            )?
            .into_iter()
            .next()
            {
                // Already has access — return the existing grant instead of stacking rows.
                let g = grant_json(tx, existing["id"].as_i64().unwrap_or_default())?;
                return Ok(json!({ "ok": true, "grant": g, "already": true }));
            }
            tx.execute(
                "INSERT INTO document_grants (document_id, user_id, reason, granted_by, granted_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, req.user_id, why, actor.user_id, crate::time::now_utc()],
            )?;
            let gid = tx.last_insert_rowid();
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "document.granted",
                    "document",
                    id,
                    format!(
                        "{} shared with {}",
                        document_label(
                            id,
                            doc["visibility"].as_str().unwrap_or_default(),
                            doc["title"].as_str().unwrap_or_default()
                        ),
                        target.display_name
                    ),
                )
                .case(doc["case_id"].as_i64())
                .details(json!({ "grant_id": gid, "user_id": req.user_id, "reason": why })),
            )?;
            let g = grant_json(tx, gid)?;
            Ok(json!({ "ok": true, "grant": g }))
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct RevokeReq {
    reason: Option<String>,
}

/// Revoking keeps the grant row (with revoked_at/revoked_by) — access history is never erased.
async fn grant_revoke(ctx: Ctx, Path((id, gid)): Path<(i64, i64)>, JsonBody(req): JsonBody<RevokeReq>) -> JsonResult {
    let actor = ctx.actor;
    let v = ctx
        .db
        .write(move |tx| {
            let doc = load_for_grant(tx, &actor, id)?;
            authorize_grant(&actor, &doc)?;
            let why = reason(&req.reason)?;
            let grant = query_json(
                tx,
                "SELECT * FROM document_grants WHERE id = ?1 AND document_id = ?2 AND revoked_at IS NULL",
                params![gid, id],
            )?
            .into_iter()
            .next()
            .ok_or_else(AppError::not_found)?;
            tx.execute(
                "UPDATE document_grants SET revoked_at = ?2, revoked_by = ?3 WHERE id = ?1",
                params![gid, crate::time::now_utc(), actor.user_id],
            )?;
            let name: Option<String> = tx
                .query_row("SELECT display_name FROM users WHERE id = ?1", [grant["user_id"].as_i64()], |r| {
                    r.get(0)
                })
                .optional()?;
            audit::record(
                tx,
                Some(&actor),
                Event::new(
                    "document.grant_revoked",
                    "document",
                    id,
                    format!(
                        "{} access revoked for {}",
                        document_label(
                            id,
                            doc["visibility"].as_str().unwrap_or_default(),
                            doc["title"].as_str().unwrap_or_default()
                        ),
                        name.unwrap_or_default()
                    ),
                )
                .case(doc["case_id"].as_i64())
                .details(json!({ "grant_id": gid, "user_id": grant["user_id"], "reason": why })),
            )?;
            let g = grant_json(tx, gid)?;
            Ok(json!({ "ok": true, "grant": g }))
        })
        .await?;
    Ok(Json(v))
}

// ------------------------------------------------------------------ download

#[derive(Deserialize)]
struct DownloadQuery {
    inline: Option<String>,
}

/// The ONLY way to file bytes. Access is re-checked on every request, so an ended assignment or
/// a revoked grant turns a saved URL into a 404. Quarantined files are never served. Viewing a
/// restricted document or a judicial note is audited before the bytes leave the store.
async fn download(ctx: Ctx, Path(id): Path<i64>, Query(q): Query<DownloadQuery>) -> AppResult<Response> {
    let actor = ctx.actor;
    let db = ctx.db;
    let actor_r = actor.clone();
    let (doc, v) = db
        .read(move |c| {
            let (doc, vid) = policy::require_version(c, &actor_r, id)?;
            let v = query_one_json(
                c,
                "SELECT id, version_no, filename, content_type, size_bytes, sha256, storage_key, scan_status
                 FROM document_versions WHERE id = ?1",
                [vid],
            )?;
            Ok((doc, v))
        })
        .await?;
    if v["scan_status"].as_str() == Some("quarantined") {
        return Err(AppError::conflict(
            "quarantined",
            "This file failed the safety check and cannot be opened.",
        ));
    }
    let storage_key = v["storage_key"].as_str().unwrap_or_default().to_string();
    let sha256 = v["sha256"].as_str().unwrap_or_default().to_string();
    let file_db = db.clone();
    let bytes = tokio::task::spawn_blocking(move || crate::storage::read(&file_db, &storage_key, &sha256))
        .await
        .map_err(|e| AppError::internal(e.to_string()))??;
    if doc.is_sensitive() {
        let actor_w = actor.clone();
        let doc_w = doc.clone();
        db.write(move |tx| {
            audit::record(
                tx,
                Some(&actor_w),
                Event::new(
                    "document.viewed_restricted",
                    "document",
                    doc_w.id,
                    format!("{} opened", document_label(doc_w.id, &doc_w.visibility, &doc_w.title)),
                )
                .case(doc_w.case_id)
                    .details(json!({"version_id": id})),
            )?;
            Ok(())
        })
        .await?;
    }
    let filename = v["filename"].as_str().unwrap_or("file").to_string();
    let content_type = v["content_type"].as_str().unwrap_or("application/octet-stream").to_string();
    // Inline display is honoured only for types a browser can show without scripting risk.
    let inline = q.inline.as_deref() == Some("1") && matches!(content_type.as_str(), "application/pdf" | "image/png" | "image/jpeg");
    Ok(crate::storage::file_response(bytes, &filename, &content_type, inline))
}
