//! A participant package contains only permitted materials; it is never a technical backup.
use super::common::{JsonBody, query_json, query_one_json, required};
use crate::{
    audit::{self, Event},
    auth::{Actor, Ctx},
    db::Db,
    error::{AppError, AppResult},
    policy::{self, perm},
    state::AppState,
    storage,
};
use axum::{Router, extract::Path, http::StatusCode, response::Response, routing::post};
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::BTreeSet,
    io::{Cursor, Seek, SeekFrom, Write},
};
use zip::{ZipWriter, write::SimpleFileOptions};
const CAP: usize = 40 * 1024 * 1024;
pub fn routes() -> Router<AppState> {
    Router::new().route("/cases/{id}/export", post(export))
}
#[derive(Deserialize)]
struct ExportReq {
    purpose: String,
    version_ids: Option<Vec<i64>>,
}
fn large() -> AppError {
    AppError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "too_large",
        "The case package exceeds 40 MB.",
    )
}
struct LimitedZip(Cursor<Vec<u8>>);
impl Write for LimitedZip {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.0.position().saturating_add(b.len() as u64) > CAP as u64 {
            return Err(std::io::Error::other("Package too large"));
        }
        self.0.write(b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}
impl Seek for LimitedZip {
    fn seek(&mut self, p: SeekFrom) -> std::io::Result<u64> {
        self.0.seek(p)
    }
}
async fn export(
    ctx: Ctx,
    Path(id): Path<i64>,
    JsonBody(req): JsonBody<ExportReq>,
) -> AppResult<Response> {
    let permit = std::sync::Arc::new(super::import::heavy_operation()?);
    let _request_permit = permit.clone();
    let actor = ctx.actor;
    let db = ctx.db.clone();
    ctx.db
        .write(move |tx| {
            let _permit = permit;
            build_package(tx, &actor, &db, id, req)
        })
        .await
}

fn build_package(
    tx: &Connection,
    actor: &Actor,
    db: &Db,
    id: i64,
    req: ExportReq,
) -> AppResult<Response> {
    let case = policy::require_case_perm(tx, actor, id, perm::EXPORT_CASE)?;
    let purpose = required(&req.purpose, "Purpose")?;
    let ids = match req.version_ids {
        Some(ids) => {
            if ids.len() > 5000 || ids.iter().copied().collect::<BTreeSet<_>>().len() != ids.len() {
                return Err(AppError::validation(
                    "Supply unique document version ids (at most 5000).",
                ));
            }
            ids
        }
        None => {
            let sql = format!(
                "SELECT (SELECT v.id FROM document_versions v
                    WHERE v.document_id=d.id AND v.scan_status='clean'
                    ORDER BY v.version_no DESC LIMIT 1) AS id
                 FROM documents d WHERE d.case_id=?1
                    AND d.visibility IN ('party_material','administrative') AND {}
                 ORDER BY d.id",
                policy::document_visible_sql(actor, "d")
            );
            query_json(tx, &sql, [id])?
                .iter()
                .filter_map(|r| r["id"].as_i64())
                .collect()
        }
    };
    let mut zip = ZipWriter::new(LimitedZip(Cursor::new(Vec::new())));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let (mut total, mut files) = (0usize, vec![]);
    for vid in &ids {
        let (doc, _) = policy::require_version(tx, actor, *vid)?;
        let doc_type: String = tx.query_row(
            "SELECT doc_type FROM documents WHERE id=?1",
            [doc.id],
            |r| r.get(0),
        )?;
        if doc.case_id != Some(id)
            || doc.visibility == "judicial_note"
            || doc_type == "judicial_note"
        {
            return Err(AppError::not_found());
        }
        let v = query_one_json(
            tx,
            "SELECT * FROM document_versions WHERE id=?1 AND scan_status='clean'",
            [*vid],
        )?;
        let size = v["size_bytes"]
            .as_u64()
            .ok_or_else(|| AppError::internal("Invalid file size."))?;
        if size > CAP as u64 || total as u64 + size > CAP as u64 {
            return Err(large());
        }
        let bytes = storage::read(
            db,
            v["storage_key"].as_str().unwrap_or(""),
            v["sha256"].as_str().unwrap_or(""),
        )?;
        if bytes.len() as u64 != size {
            return Err(AppError::internal("Stored file size mismatch."));
        }
        total += bytes.len();
        let path = format!(
            "files/{vid}-{}",
            storage::sanitize_filename(v["filename"].as_str().unwrap_or("file"))
        );
        zip.start_file(&path, options).map_err(|_| large())?;
        zip.write_all(&bytes).map_err(|_| large())?;
        files.push(json!({"path":path,"document_title":doc.title,"version_no":v["version_no"],"sha256":v["sha256"],"size_bytes":size,"visibility":doc.visibility}));
        if doc.is_sensitive() {
            audit::record(
                tx,
                Some(actor),
                Event::new(
                    "document.viewed_restricted",
                    "document",
                    doc.id,
                    "Restricted document included in a case package",
                )
                .case(Some(id))
                .details(json!({"version_id":vid,"purpose":purpose})),
            )?;
        }
    }
    let mut hearings=query_json(tx,"SELECT id,hearing_type,status,starts_at,ends_at,room_id,judge_user_id,previous_hearing_id,adjourned_to_id,outcome_summary,next_step
                 FROM hearings WHERE case_id=?1 ORDER BY starts_at,id",[id])?;
    for h in &mut hearings {
        h["starts_local"] = json!(crate::time::utc_to_local(
            h["starts_at"].as_str().unwrap_or("")
        ));
        h["ends_local"] = json!(crate::time::utc_to_local(
            h["ends_at"].as_str().unwrap_or("")
        ));
    }
    let decisions=query_json(tx,&format!("SELECT x.id,x.title,x.decision_date,x.status,x.document_id,x.document_version_id,x.finalised_at,x.amends_decision_id,x.superseded_by_id
                 FROM decisions x JOIN documents d ON d.id=x.document_id WHERE x.case_id=?1
                 AND d.case_id=x.case_id AND x.status IN ('finalised','superseded') AND d.visibility<>'judicial_note'
                 AND {} ORDER BY x.id",policy::document_visible_sql(actor,"d")),[id])?;
    let relations=query_json(tx,&format!("SELECT r.kind,r.note,c.id AS case_id,c.number,c.title FROM case_relations r JOIN cases c ON
                 c.id=CASE WHEN r.from_case_id=?1 THEN r.to_case_id ELSE r.from_case_id END
                 WHERE (r.from_case_id=?1 OR r.to_case_id=?1) AND {} ORDER BY r.id",policy::case_visible_sql(actor,"c.id")),[id])?;
    let at = crate::time::now_utc();
    let manifest = json!({"format":"tcr-case-export/1","note":"Participant/user package of permitted materials. Not a backup.","exported_at":at,"exported_by":{"user_id":actor.user_id,"display_name":actor.display_name},"purpose":purpose,
            "case":query_one_json(tx,"SELECT number,title,category,status,registered_date,closed_date,closure_basis FROM cases WHERE id=?1",[id])?,
            "participants":query_json(tx,"SELECT cp.id,cp.role,cp.active,cp.service_contact,p.name,p.kind,cp.representative_party_id,cp.representation_basis
                 FROM case_participations cp JOIN parties p ON p.id=cp.party_id WHERE cp.case_id=?1
                 ORDER BY cp.id",[id])?,
            "hearings":hearings,"decisions":decisions,"relations":relations,"chronology":super::audit_log::package_history(tx,actor,id,&ids)?,"files":files});
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    if manifest_bytes.len() + total > CAP {
        return Err(large());
    }
    zip.start_file("manifest.json", options)
        .map_err(|_| large())?;
    zip.write_all(&manifest_bytes).map_err(|_| large())?;
    let bytes = zip.finish().map_err(|_| large())?.0.into_inner();
    let sha = crate::auth::sha256_hex(&bytes);
    tx.execute("INSERT INTO export_batches(kind,case_id,purpose,manifest_json,sha256,created_by,created_at)
                 VALUES('case_package',?1,?2,?3,?4,?5,?6)",params![id,purpose,manifest.to_string(),sha,actor.user_id,at])?;
    let bid = tx.last_insert_rowid();
    audit::record(
        tx,
        Some(actor),
        Event::new("case.exported", "case", id, "Case package exported")
            .case(Some(id))
            .details(
                json!({"export_batch_id":bid,"purpose":purpose,"sha256":sha,"version_ids":ids}),
            ),
    )?;
    Ok(storage::file_response(
        bytes,
        &format!("{}-export.zip", case.number),
        "application/zip",
        false,
    ))
}
