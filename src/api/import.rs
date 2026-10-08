//! Legacy sources are immutable; preview validates and commit rechecks inside one transaction.
use super::common::{JsonResult, query_json, query_one_json, require_ref};
use crate::{
    audit::{self, Event},
    auth::{Actor, Ctx, IdemKey, idempotent},
    db::Db,
    error::{AppError, AppResult},
    policy::{self, perm},
    state::AppState,
    storage,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, Path},
    http::StatusCode,
    routing::{get, post},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
    sync::{Arc, LazyLock},
};

const MB: usize = 1024 * 1024;
static HEAVY_OPERATION: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(1)));

pub(super) fn heavy_operation() -> AppResult<tokio::sync::OwnedSemaphorePermit> {
    HEAVY_OPERATION
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::new(StatusCode::SERVICE_UNAVAILABLE, "busy", "busy, try again"))
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/import", get(list))
        .route("/import/{id}", get(detail))
        .route("/import/{id}/commit", post(commit))
        .route(
            "/import/cases/preview",
            post(preview_cases).layer(DefaultBodyLimit::max(5 * MB + MB)),
        )
        .route(
            "/import/files/preview",
            post(preview_files).layer(DefaultBodyLimit::max(21 * MB)),
        )
}
fn invalid(s: impl Into<String>) -> AppError {
    AppError::validation(s)
}
fn too_large() -> AppError {
    AppError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "too_large",
        "The import exceeds its size limit.",
    )
}

async fn upload(mut mp: Multipart, limit: usize) -> AppResult<(String, Vec<u8>, Option<i64>)> {
    let (mut file, mut registry) = (None, None);
    while let Some(mut part) = mp.next_field().await.map_err(|e| invalid(e.to_string()))? {
        match part.name() {
            Some("file") => {
                if file.is_some() {
                    return Err(invalid("Supply one file."));
                }
                let name = storage::sanitize_filename(part.file_name().unwrap_or("source"));
                let mut bytes = Vec::new();
                while let Some(chunk) = part.chunk().await.map_err(|_| too_large())? {
                    if bytes.len() + chunk.len() > limit {
                        return Err(too_large());
                    }
                    bytes.extend_from_slice(&chunk);
                }
                file = Some((name, bytes));
            }
            Some("registry_id") => {
                if registry.is_some() {
                    return Err(invalid("Supply one registry."));
                }
                registry = Some(
                    part.text()
                        .await
                        .map_err(|e| invalid(e.to_string()))?
                        .parse::<i64>()
                        .map_err(|_| invalid("Invalid registry."))?,
                );
            }
            _ => return Err(invalid("Unknown import field.")),
        }
    }
    let (name, bytes) = file.ok_or_else(|| invalid("A file is required."))?;
    Ok((name, bytes, registry))
}

#[derive(Clone, Deserialize, Serialize)]
struct CaseRow {
    number: String,
    category: String,
    title: String,
    registered_date: String,
    status: String,
    responsible_username: String,
    closed_date: String,
    closure_basis: String,
    parties: String,
}
#[derive(Clone, Deserialize, Serialize)]
struct FileRow {
    case_number: String,
    filename: String,
    title: String,
    doc_type: String,
    visibility: String,
    document_date: String,
}
fn csv_rows<T: for<'a> Deserialize<'a>>(bytes: &[u8], headers: &[&str]) -> AppResult<Vec<T>> {
    if bytes.len() > 5 * MB {
        return Err(too_large());
    }
    std::str::from_utf8(bytes).map_err(|_| invalid("The CSV must be UTF-8."))?;
    let mut r = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(bytes);
    let h = r.headers().map_err(|e| invalid(e.to_string()))?;
    if h.len() != headers.len()
        || headers
            .iter()
            .any(|name| h.iter().filter(|v| v == name).count() != 1)
    {
        return Err(invalid(format!(
            "Required CSV columns: {}",
            headers.join(", ")
        )));
    }
    r.deserialize()
        .map(|v| v.map_err(|e| invalid(e.to_string())))
        .collect()
}
fn cases_rows(bytes: &[u8]) -> AppResult<Vec<CaseRow>> {
    csv_rows(
        bytes,
        &[
            "number",
            "category",
            "title",
            "registered_date",
            "status",
            "responsible_username",
            "closed_date",
            "closure_basis",
            "parties",
        ],
    )
}
fn series_number(c: &Connection, n: &str) -> AppResult<Option<(i64, i32, i64)>> {
    let Some((prefix, seq)) = n.rsplit_once('-') else {
        return Ok(None);
    };
    let Some((series, year)) = prefix.rsplit_once('-') else {
        return Ok(None);
    };
    if year.len() != 4
        || seq.len() != 4
        || !year.bytes().all(|b| b.is_ascii_digit())
        || !seq.bytes().all(|b| b.is_ascii_digit())
    {
        return Ok(None);
    }
    let year: i32 = year.parse().map_err(|_| invalid("Invalid number year."))?;
    let seq: i64 = seq
        .parse()
        .map_err(|_| invalid("Invalid number sequence."))?;
    if seq == 0 || crate::time::parse_date(&format!("{year:04}-01-01")).is_err() {
        return Ok(None);
    }
    Ok(
        c.query_row("SELECT id FROM registries WHERE series=?1", [series], |r| {
            r.get(0)
        })
        .optional()?
        .map(|id| (id, year, seq)),
    )
}
fn parties(c: &Connection, input: &str) -> AppResult<Vec<(String, String)>> {
    let mut out = vec![];
    for p in input.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (name, role) = p
            .rsplit_once(" (")
            .ok_or_else(|| invalid("Use Name (role) for each party."))?;
        let role = role
            .strip_suffix(')')
            .ok_or_else(|| invalid("Invalid party role."))?;
        if name.trim().is_empty() {
            return Err(invalid("Party name is required."));
        }
        require_ref(c, "participant_role", role)?;
        out.push((name.trim().to_string(), role.to_string()));
    }
    Ok(out)
}
fn responsible(c: &Connection, actor: &Actor, name: &str) -> AppResult<Option<i64>> {
    if name.is_empty() {
        return Ok(None);
    }
    actor.require(perm::CASE_ASSIGN_STAFF)?;
    let uid = c.query_row(
        "SELECT id FROM users WHERE username=?1 AND active=1",
        [name],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| invalid("Unknown responsible user."))?;
    if !policy::user_assignable(c,uid)? { return Err(invalid("This user is not eligible for case work.")); }
    Ok(Some(uid))
}
fn case_preview(
    c: &Connection,
    actor: &Actor,
    input: &[CaseRow],
    registry: Option<i64>,
) -> AppResult<Value> {
    let mut rows = vec![];
    let mut counts = BTreeMap::new();
    for r in input {
        *counts.entry(&r.number).or_insert(0usize) += 1;
    }
    for (i, r) in input.iter().enumerate() {
        let mut problems = vec![];
        let mut missing = vec![];
        let mut check = |result: AppResult<()>| {
            if let Err(e) = result {
                problems.push(e.message);
            }
        };
        check(crate::time::parse_date(&r.registered_date).map(|_| ()));
        check(require_ref(c, "case_category", &r.category));
        check(responsible(c, actor, &r.responsible_username).map(|_| ()));
        if !matches!(
            r.status.as_str(),
            "registered" | "active" | "on_hold" | "closed" | "reopened"
        ) {
            problems.push("Unknown status.".into());
        }
        if r.number.is_empty() || r.title.is_empty() {
            problems.push("Number and title are required.".into());
        }
        if !r.closed_date.is_empty() {
            if crate::time::parse_date(&r.closed_date).is_err() {
                problems.push("Invalid closed date.".into());
            } else if r.closed_date < r.registered_date {
                problems.push("Closed date precedes registration.".into());
            }
        }
        if !r.closure_basis.is_empty() {
            if let Err(e) = require_ref(c, "closure_basis", &r.closure_basis) {
                problems.push(e.message);
            }
        }
        if let Err(e) = parties(c, &r.parties) {
            problems.push(e.message);
        }
        for (key, value) in [
            ("responsible_username", &r.responsible_username),
            ("parties", &r.parties),
        ] {
            if value.is_empty() {
                missing.push(key);
            }
        }
        if r.status == "closed" {
            if r.closed_date.is_empty() {
                problems.push("Closed date is required for a closed case.".into());
            }
            for (key, value) in [
                ("closed_date", &r.closed_date),
                ("closure_basis", &r.closure_basis),
            ] {
                if value.is_empty() {
                    missing.push(key);
                }
            }
        }
        let parsed = series_number(c, &r.number)?;
        let existing: Option<i64> = c
            .query_row(
                "SELECT id FROM cases WHERE number=?1 OR legacy_number=?1",
                [&r.number],
                |r| r.get(0),
            )
            .optional()?;
        if parsed.is_none() && existing.is_none() {
            let valid = match registry {
                Some(id) => {
                    c.query_row(
                        "SELECT COUNT(*) FROM registries WHERE id=?1 AND active=1",
                        [id],
                        |r| r.get::<_, i64>(0),
                    )? == 1
                }
                None => false,
            };
            if !valid {
                problems.push("Choose a registry for legacy numbers.".into());
            }
        }
        if let Some(id) = existing {
            if policy::require_case(c, actor, id).is_err() {
                problems.push("Number cannot be imported.".into());
            }
        }
        if counts[&r.number] > 1 {
            problems.push("Duplicate number in this file.".into());
        }
        let action = if !problems.is_empty() {
            "error"
        } else if existing.is_some() {
            "skip_existing"
        } else {
            "create"
        };
        rows.push(json!({"row":i+2,"number":r.number,"action":action,"target_number": if parsed.is_some() { Some(&r.number) } else { None },"legacy_number":if parsed.is_none() { Some(&r.number) } else { None },"problems":problems,"missing":missing}));
    }
    Ok(preview_value(rows))
}
fn preview_value(rows: Vec<Value>) -> Value {
    let count = |a: &str| rows.iter().filter(|r| r["action"] == a).count();
    json!({"summary":{"create":count("create"),"skip_existing":count("skip_existing"),"error":count("error")},"rows":rows})
}

// No archive path is ever used as a filesystem path.
fn safe_name(n: &str) -> bool {
    !n.is_empty()
        && !n.starts_with('/')
        && !n.contains('\\')
        && !n.contains(':')
        && n.split('/').all(|p| !matches!(p, "" | "." | ".."))
}
// zip 8 coalesces duplicate central-directory names. Count the original entries before
// constructing ZipArchive, both to bound its allocation and to detect that coalescing.
fn archive_entries(bytes: &[u8]) -> AppResult<usize> {
    let start = bytes.len().saturating_sub(65535 + 22);
    let end = (start..bytes.len().saturating_sub(21))
        .rev()
        .find(|&i| {
            bytes.get(i..i + 4) == Some(b"PK\x05\x06")
                && i + 22 + u16::from_le_bytes([bytes[i + 20], bytes[i + 21]]) as usize
                    == bytes.len()
        })
        .ok_or_else(|| invalid("Invalid ZIP directory."))?;
    let mut count = u16::from_le_bytes([bytes[end + 10], bytes[end + 11]]) as u64;
    if count == 65535 {
        let locator = end
            .checked_sub(20)
            .ok_or_else(|| invalid("Invalid ZIP64 directory."))?;
        if bytes.get(locator..locator + 4) != Some(b"PK\x06\x07") {
            return Err(invalid("Invalid ZIP64 locator."));
        }
        let offset = u64::from_le_bytes(
            bytes[locator + 8..locator + 16]
                .try_into()
                .map_err(|_| invalid("Invalid ZIP64 directory."))?,
        );
        let offset = usize::try_from(offset).map_err(|_| invalid("Invalid ZIP64 offset."))?;
        let header = bytes
            .get(
                offset
                    ..offset
                        .checked_add(56)
                        .ok_or_else(|| invalid("Invalid ZIP64 offset."))?,
            )
            .ok_or_else(|| invalid("Invalid ZIP64 directory."))?;
        if &header[..4] != b"PK\x06\x06" {
            return Err(invalid("Invalid ZIP64 directory."));
        }
        count = u64::from_le_bytes(
            header[32..40]
                .try_into()
                .map_err(|_| invalid("Invalid ZIP64 count."))?,
        );
    }
    if count > 500 {
        return Err(too_large());
    }
    Ok(count as usize)
}
fn unpack(bytes: &[u8]) -> AppResult<BTreeMap<String, Vec<u8>>> {
    let count = archive_entries(bytes)?;
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| invalid("Invalid ZIP."))?;
    if zip.len() != count {
        return Err(invalid("Duplicate or invalid archive entries."));
    }
    let mut offset = usize::try_from(zip.central_directory_start())
        .map_err(|_| invalid("Invalid ZIP directory."))?;
    let mut actual = 0;
    while bytes.get(offset..offset.saturating_add(4)) == Some(b"PK\x01\x02") {
        let h = bytes
            .get(offset..offset.saturating_add(46))
            .ok_or_else(|| invalid("Truncated ZIP directory."))?;
        actual += 1;
        if actual > 500 {
            return Err(too_large());
        }
        offset = offset
            .checked_add(
                46 + [28, 30, 32]
                    .iter()
                    .map(|&i| u16::from_le_bytes([h[i], h[i + 1]]) as usize)
                    .sum::<usize>(),
            )
            .ok_or_else(|| invalid("Invalid ZIP directory."))?;
    }
    if actual != count {
        return Err(invalid("Invalid ZIP entry count."));
    }
    if zip.len() > 500 {
        return Err(too_large());
    }
    let (mut total, mut names, mut files) = (0u64, BTreeSet::new(), BTreeMap::new());
    for i in 0..zip.len() {
        let f = zip
            .by_index_raw(i)
            .map_err(|_| invalid("Invalid ZIP entry."))?;
        let n = f.name().to_string();
        let is_dir = f.is_dir();
        let path = if is_dir {
            n.strip_suffix('/').unwrap_or(&n)
        } else {
            &n
        };
        if !safe_name(path)
            || f.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
            || !names.insert(path.to_string())
        {
            return Err(invalid("Unsafe or duplicate archive path."));
        }
        total = total.checked_add(f.size()).ok_or_else(too_large)?;
        if f.size() > 15 * MB as u64
            || total > 40 * MB as u64
            || f.size() > f.compressed_size().saturating_mul(100)
        {
            return Err(too_large());
        }
        if is_dir {
            if f.size() != 0 {
                return Err(invalid("Invalid directory entry."));
            }
            continue;
        }
    }
    // Decompression begins only after every entry passes the metadata limits.
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|_| invalid("Invalid ZIP entry."))?;
        if f.is_dir() {
            continue;
        }
        let n = f.name().to_string();
        let expected = f.size();
        let mut data = Vec::with_capacity(expected as usize);
        (&mut f)
            .take(15 * MB as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|_| invalid("Invalid ZIP entry."))?;
        if data.len() as u64 != expected {
            return Err(invalid("Invalid archive size."));
        }
        files.insert(n, data);
    }
    // A file cannot also serve as another entry's directory.
    for name in files.keys() {
        for prefix in name.match_indices('/').map(|(i, _)| &name[..i]) {
            if files.contains_key(prefix) {
                return Err(invalid("Invalid archive directory."));
            }
        }
    }
    Ok(files)
}
fn validate_file(bytes: &[u8], filename: &str) -> AppResult<()> {
    if bytes.is_empty() {
        return Err(invalid("The file is empty."));
    }
    let safe = storage::sanitize_filename(filename);
    let ext = safe.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let valid = match ext.as_str() {
        "pdf" => bytes.starts_with(b"%PDF-"),
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" | "jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "docx" => {
            if !bytes.starts_with(b"PK\x03\x04") {
                return Err(invalid("Invalid DOCX content."));
            }
            if let Ok(mut z) = zip::ZipArchive::new(Cursor::new(bytes)) {
                let ct = z.by_name("[Content_Types].xml").is_ok();
                ct && z.by_name("word/document.xml").is_ok()
            } else {
                false
            }
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(invalid(
            "File content and extension must be PDF, DOCX, JPEG or PNG.",
        ))
    }
}
fn file_rows(files: &BTreeMap<String, Vec<u8>>) -> AppResult<Vec<FileRow>> {
    csv_rows(
        files
            .get("manifest.csv")
            .ok_or_else(|| invalid("manifest.csv is required."))?,
        &[
            "case_number",
            "filename",
            "title",
            "doc_type",
            "visibility",
            "document_date",
        ],
    )
}
fn matching_case(c: &Connection, actor: &Actor, number: &str) -> AppResult<i64> {
    let sql = format!(
        "SELECT c.id FROM cases c WHERE (c.number=?1 OR c.legacy_number=?1) AND {}",
        policy::case_visible_sql(actor, "c.id")
    );
    c.query_row(&sql, [number], |r| r.get(0))
        .optional()?
        .ok_or_else(AppError::not_found)
}
fn files_preview(
    c: &Connection,
    actor: &Actor,
    input: &[FileRow],
    files: &BTreeMap<String, Vec<u8>>,
) -> AppResult<Value> {
    let mut rows = vec![];
    let mut counts = BTreeMap::new();
    for row in input {
        *counts.entry(&row.filename).or_insert(0usize) += 1;
    }
    for (i, r) in input.iter().enumerate() {
        let mut problems = vec![];
        if matching_case(c, actor, &r.case_number).is_err() {
            problems.push("Matching case is unavailable.".into());
        }
        if let Err(e) = require_ref(c, "document_type", &r.doc_type) {
            problems.push(e.message);
        }
        if !matches!(
            r.visibility.as_str(),
            "administrative" | "party_material" | "restricted" | "judicial_note"
        ) {
            problems.push("Unknown visibility.".into());
        }
        if r.visibility == "judicial_note" || r.doc_type == "judicial_note" {
            if !actor.is_judge {
                problems.push("Only judges can import judicial notes.".into());
            }
            if (r.visibility == "judicial_note") != (r.doc_type == "judicial_note") {
                problems.push("Judicial-note type and visibility must agree.".into());
            }
        }
        if r.title.is_empty() {
            problems.push("Title is required.".into());
        }
        if !r.document_date.is_empty() && crate::time::parse_date(&r.document_date).is_err() {
            problems.push("Invalid document date.".into());
        }
        match files.get(&r.filename) {
            Some(b) if r.filename != "manifest.csv" => {
                if let Err(e) = validate_file(b, &r.filename) {
                    problems.push(e.message);
                }
            }
            _ => problems.push("File is missing.".into()),
        }
        if counts[&r.filename] > 1 {
            problems.push("Duplicate file in manifest.".into());
        }
        let existing = if problems.is_empty() {
            let cid = matching_case(c, actor, &r.case_number)?;
            let sha = crate::auth::sha256_hex(&files[&r.filename]);
            c.query_row("SELECT EXISTS(SELECT 1 FROM document_versions v JOIN documents d ON d.id=v.document_id WHERE d.case_id=?1 AND v.sha256=?2)", params![cid,sha], |r| r.get::<_, bool>(0))?
        } else {
            false
        };
        rows.push(json!({"row":i+2,"case_number":r.case_number,"filename":r.filename,"title":r.title,"action":if !problems.is_empty(){"error"}else if existing{"skip_existing"}else{"create"},"problems":problems,"missing":if r.document_date.is_empty(){vec!["document_date"]}else{vec![]}}));
    }
    Ok(preview_value(rows))
}
async fn preview_cases(ctx: Ctx, mp: Multipart) -> JsonResult {
    preview(ctx, mp, false).await
}
async fn preview_files(ctx: Ctx, mp: Multipart) -> JsonResult {
    preview(ctx, mp, true).await
}
async fn preview(ctx: Ctx, mp: Multipart, is_zip: bool) -> JsonResult {
    ctx.actor.require(perm::IMPORT_RUN)?;
    let permit = Arc::new(heavy_operation()?);
    let _request_permit = permit.clone();
    let (filename, bytes, registry) = upload(mp, if is_zip { 20 * MB } else { 5 * MB }).await?;
    let actor = ctx.actor;
    let db = ctx.db.clone();
    let written = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let keys = written.clone();
    let result=ctx.db.write(move |tx| {
        let _permit = permit;
        let mut v=if is_zip { let files=unpack(&bytes)?; files_preview(tx,&actor,&file_rows(&files)?,&files)? } else { case_preview(tx,&actor,&cases_rows(&bytes)?,registry)? };
        if let Some(quota) = db.quota_bytes() {
            if storage::used_bytes(tx)?.saturating_add(bytes.len() as u64) > quota { return Err(too_large()); }
        }
        let (key,sha)=storage::write_blob(&db,&bytes)?;
        keys.lock().push(key.clone());
        tx.execute("INSERT INTO import_batches(kind,filename,source_sha256,storage_key,status,preview_json,created_by,created_at,source_options)
                 VALUES(?1,?2,?3,?4,'previewed',?5,?6,?7,?8)",params![if is_zip{"files_zip"}else{"cases_csv"},filename,sha,key,v.to_string(),actor.user_id,crate::time::now_utc(),json!({"registry_id":registry,"size_bytes":bytes.len()}).to_string()])?;
        let id=tx.last_insert_rowid();
        audit::record(tx,Some(&actor),Event::new("import.previewed","import_batch",id,"Legacy import previewed"))?;
        v["batch_id"]=json!(id); Ok(v)
    }).await;
    if result.is_err() {
        for key in written.lock().iter() {
            storage::discard(&ctx.db, key);
        }
    }
    Ok(Json(result?))
}
async fn list(ctx: Ctx) -> JsonResult {
    ctx.actor.require(perm::IMPORT_RUN)?;
    let v=ctx.db.read(move |c| Ok(json!({"batches":query_json(c,&format!("SELECT id,kind,filename,status,created_by,created_at,committed_by,committed_at FROM import_batches
                 b WHERE {} ORDER BY id DESC",batch_visible_sql(&ctx.actor)),[])?}))).await?;
    Ok(Json(v))
}
fn batch_visible_sql(actor: &Actor) -> String {
    format!("NOT EXISTS (
        SELECT 1 FROM cases c WHERE NOT ({}) AND (
            c.import_batch_id=b.id OR EXISTS (
                SELECT 1 FROM json_each(b.preview_json, '$.rows') row
                WHERE json_extract(row.value,'$.action') IN ('create','skip_existing')
                  AND COALESCE(json_extract(row.value,'$.number'),json_extract(row.value,'$.case_number'))
                    IN (c.number,c.legacy_number))))", policy::case_visible_sql(actor,"c.id"))
}
fn batch(c: &Connection, actor: &Actor, id: i64) -> AppResult<Value> {
    query_one_json(
        c,
        &format!(
            "SELECT * FROM import_batches b WHERE b.id=?1 AND {}",
            batch_visible_sql(actor)
        ),
        [id],
    )
}
async fn detail(ctx: Ctx, Path(id): Path<i64>) -> JsonResult {
    ctx.actor.require(perm::IMPORT_RUN)?;
    let v=ctx.db.read(move |c| { let b=batch(c,&ctx.actor,id)?; Ok(json!({"batch_id":id,"kind":b["kind"],"filename":b["filename"],"status":b["status"],"preview":serde_json::from_str::<Value>(b["preview_json"].as_str().unwrap_or("null"))?,"result":b["result_json"].as_str().map(serde_json::from_str::<Value>).transpose()?})) }).await?;
    Ok(Json(v))
}
fn text<'a>(b: &'a Value, key: &str) -> &'a str {
    b[key].as_str().unwrap_or("")
}
async fn commit(ctx: Ctx, Path(id): Path<i64>, IdemKey(key): IdemKey) -> JsonResult {
    ctx.actor.require(perm::IMPORT_RUN)?;
    let permit = Arc::new(heavy_operation()?);
    let _request_permit = permit.clone();
    let written = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let keys = written.clone();
    let actor = ctx.actor;
    let db = ctx.db.clone();
    let value = ctx
        .db
        .write(move |tx| {
            let _permit = permit;
            // Re-authorize the batch and its cases even when replaying a stored result.
            let b = batch(tx, &actor, id)?;
            idempotent(tx, &actor, &key, "import.commit", &id, || {
                commit_rows(tx, &actor, &db, id, &b, &mut keys.lock())
            })
        })
        .await;
    if value.is_err() {
        for key in written.lock().iter() {
            storage::discard(&ctx.db, key);
        }
    }
    let value = value?;
    // The import rows and idempotency response commit before any scanner network I/O.
    // Replays also finish any pending versions from an interrupted request.
    for row in value["created"].as_array().into_iter().flatten() {
        if let Some(version) = row["version_id"].as_i64() {
            let key = ctx
                .db
                .read(move |c| {
                    Ok(c.query_row(
                        "SELECT storage_key FROM document_versions WHERE id=?1",
                        [version],
                        |r| r.get::<_, String>(0),
                    )?)
                })
                .await?;
            crate::scan::finish(ctx.db.clone(), key).await?;
        }
    }
    Ok(Json(value))
}

fn commit_rows(
    tx: &Transaction,
    actor: &Actor,
    db: &Db,
    id: i64,
    b: &Value,
    written: &mut Vec<String>,
) -> AppResult<Value> {
    if b["status"] != "previewed" {
        return Err(AppError::invalid_transition(
            "This batch has already been committed.",
        ));
    }
    let bytes = storage::read(db, text(b, "storage_key"), text(b, "source_sha256"))?;
    let old: Value = serde_json::from_str(text(b, "preview_json"))?;
    let mut created = vec![];
    let mut skipped = old["summary"]["skip_existing"].as_u64().unwrap_or_default();
    let now = crate::time::now_utc();
    if b["kind"] == "cases_csv" {
        let options: Value = serde_json::from_str(text(b, "source_options"))?;
        let registry = options["registry_id"].as_i64();
        let input = cases_rows(&bytes)?;
        let current = case_preview(tx, actor, &input, registry)?;
        // Reserve every retained series number before allocating legacy numbers, regardless
        // of source row order, so a generated number cannot collide with a later row.
        for (i, r) in input.iter().enumerate() {
            if old["rows"][i]["action"] == "create" {
                if current["rows"][i]["action"] != "create" {
                    return Err(AppError::conflict(
                        "import_changed",
                        "An import row changed after preview. Preview the source again.",
                    ));
                }
                if let Some((reg, y, seq)) = series_number(tx, &r.number)? {
                    tx.execute("INSERT INTO case_number_counters(registry_id,year,last_seq) VALUES(?1,?2,?3) ON CONFLICT(registry_id,year)
                 DO UPDATE SET last_seq=MAX(last_seq,excluded.last_seq)",params![reg,y,seq])?;
                }
            }
        }
        for (i, r) in input.iter().enumerate() {
            if old["rows"][i]["action"] != "create" {
                continue;
            }
            if current["rows"][i]["action"] != "create" {
                return Err(AppError::conflict(
                    "import_changed",
                    "An import row changed after preview. Preview the source again.",
                ));
            }
            let (registry, year, seq, number, legacy) =
                if let Some((reg, y, s)) = series_number(tx, &r.number)? {
                    (reg, y, s, r.number.clone(), None)
                } else {
                    let reg = registry.ok_or_else(|| invalid("Registry required."))?;
                    let y = crate::time::year_of(&r.registered_date)?;
                    let (s, n) = super::cases::allocate_number(tx, reg, y)?;
                    (reg, y, s, n, Some(r.number.clone()))
                };
            let user = responsible(tx, actor, &r.responsible_username)?;
            tx.execute("INSERT INTO cases(registry_id,year,seq,number,legacy_number,title,category,status,registered_date,registered_at,registered_by,responsible_user_id,closed_date,closure_basis,import_batch_id,historical_incomplete,updated_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?10)",params![registry,year,seq,number,legacy,r.title,r.category,r.status,r.registered_date,now,actor.user_id,user,(!r.closed_date.is_empty()).then_some(&r.closed_date),(!r.closure_basis.is_empty()).then_some(&r.closure_basis),id,!old["rows"][i]["missing"].as_array().is_some_and(|a|a.is_empty())])?;
            let cid = tx.last_insert_rowid();
            tx.execute("INSERT INTO case_status_history(case_id,to_status,by_user,at,effective_date) VALUES(?1,'registered',?2,?3,?4)",params![cid,actor.user_id,now,r.registered_date])?;
            if r.status == "closed" && !r.closed_date.is_empty() {
                tx.execute("INSERT INTO case_status_history(case_id,from_status,to_status,basis,by_user,at,effective_date)
                 VALUES(?1,'registered','closed',?2,?3,?4,?5)",params![cid,(!r.closure_basis.is_empty()).then_some(&r.closure_basis),actor.user_id,now,r.closed_date])?;
            }
            if let Some(uid) = user {
                tx.execute("INSERT INTO case_assignments(case_id,user_id,role,reason,assigned_by,start_at) VALUES(?1,?2,'clerk','Responsible user recorded in legacy source',?3,?4)",params![cid,uid,actor.user_id,now])?;
            }
            for (name, role) in parties(tx, &r.parties)? {
                tx.execute("INSERT INTO parties(kind,name,created_by,created_at) VALUES('person',?1,?2,?3)",params![name,actor.user_id,now])?;
                tx.execute("INSERT INTO case_participations(case_id,party_id,role,added_by,added_at) VALUES(?1,?2,?3,?4,?5)",params![cid,tx.last_insert_rowid(),role,actor.user_id,now])?;
            }
            tx.execute("INSERT INTO case_number_counters(registry_id,year,last_seq) VALUES(?1,?2,?3) ON CONFLICT(registry_id,year)
                 DO UPDATE SET last_seq=MAX(last_seq,excluded.last_seq)",params![registry,year,seq])?;
            audit::record(
                tx,
                Some(actor),
                Event::new(
                    "case.imported",
                    "case",
                    cid,
                    format!("Legacy case {number} imported"),
                )
                .case(Some(cid))
                .details(json!({"batch_id":id})),
            )?;
            created.push(json!({"case_id":cid,"number":number,"legacy_number":legacy}));
        }
    } else {
        let files = unpack(&bytes)?;
        let input = file_rows(&files)?;
        let current = files_preview(tx, actor, &input, &files)?;
        {
            let mut added_bytes = 0u64;
            let used = storage::used_bytes(tx)?;
            for (i, r) in input.iter().enumerate() {
                if old["rows"][i]["action"] != "create" {
                    continue;
                }
                if current["rows"][i]["action"] == "skip_existing" {
                    skipped += 1;
                    continue;
                }
                if current["rows"][i]["action"] != "create" {
                    return Err(AppError::conflict(
                        "import_changed",
                        "An import row changed after preview.",
                    ));
                }
                let cid = matching_case(tx, actor, &r.case_number)?;
                policy::require_case(tx, actor, cid)?;
                let data = files
                    .get(&r.filename)
                    .ok_or_else(|| invalid("Missing file."))?;
                let existing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM document_versions v JOIN documents d ON d.id=v.document_id WHERE d.case_id=?1 AND v.sha256=?2)", params![cid,crate::auth::sha256_hex(data)], |r| r.get(0))?;
                if existing {
                    skipped += 1;
                    continue;
                }
                added_bytes += data.len() as u64;
                if db.quota_bytes().is_some_and(|q| used + added_bytes > q) {
                    return Err(too_large());
                }
                let f = storage::prepare_upload(db, data, &r.filename, 15 * MB as u64)?;
                written.push(f.storage_key.clone());
                tx.execute("INSERT INTO documents(case_id,title,doc_type,source,visibility,document_date,created_by,created_at)
                 VALUES(?1,?2,?3,'external',?4,?5,?6,?7)",params![cid,r.title,r.doc_type,r.visibility,(!r.document_date.is_empty()).then_some(&r.document_date),actor.user_id,now])?;
                let did = tx.last_insert_rowid();
                tx.execute("INSERT INTO document_versions(document_id,version_no,filename,content_type,size_bytes,sha256,storage_key,scan_status,scan_note,uploaded_by,uploaded_at)
                 VALUES(?1,1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![did,f.filename,f.content_type,f.size_bytes,f.sha256,f.storage_key,f.scan_status,f.scan_note,actor.user_id,now])?;
                let vid = tx.last_insert_rowid();
                audit::record(
                    tx,
                    Some(actor),
                    Event::new(
                        "document.imported",
                        "document",
                        did,
                        "Legacy document imported",
                    )
                    .case(Some(cid))
                    .details(json!({"batch_id":id})),
                )?;
                created.push(json!({"case_id":cid,"document_id":did,"version_id":vid}));
            }
        }
    }
    let result = json!({"batch_id":id,"status":"committed","created":created,"summary":{"created":created.len(),"skip_existing":skipped,"error":old["summary"]["error"]}});
    tx.execute("UPDATE import_batches SET status='committed',result_json=?2,committed_by=?3,committed_at=?4 WHERE id=?1",params![id,result.to_string(),actor.user_id,now])?;
    audit::record(
        tx,
        Some(actor),
        Event::new(
            "import.committed",
            "import_batch",
            id,
            "Legacy import committed",
        )
        .details(json!({"created":created.len()})),
    )?;
    Ok(result)
}
