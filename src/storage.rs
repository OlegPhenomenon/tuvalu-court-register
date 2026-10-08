//! Private, immutable file store. Types are detected from content (magic bytes), never trusted from
//! the client. Allowed: PDF, DOCX, JPEG, PNG. HTML/SVG/executables are refused. Suspicious content
//! (PDF scripts/launch actions/embedded files, DOCX macros) is stored but quarantined and never served.
//! Files are written to a temp path and renamed into place before the DB row referencing them commits.

use crate::db::Db;
use crate::error::{AppError, AppResult};
use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use rusqlite::Connection;
use serde::Serialize;
use std::io::{Read, Write};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct StoredFile {
    pub storage_key: String,
    pub sha256: String,
    pub size_bytes: i64,
    pub content_type: String,
    pub filename: String,
    pub scan_status: &'static str,
    pub scan_note: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Pdf,
    Docx,
    Jpeg,
    Png,
}

impl Kind {
    fn mime(&self) -> &'static str {
        match self {
            Kind::Pdf => "application/pdf",
            Kind::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Kind::Jpeg => "image/jpeg",
            Kind::Png => "image/png",
        }
    }
    fn extensions(&self) -> &'static [&'static str] {
        match self {
            Kind::Pdf => &["pdf"],
            Kind::Docx => &["docx"],
            Kind::Jpeg => &["jpg", "jpeg"],
            Kind::Png => &["png"],
        }
    }
}

const DOCX_MAX_ENTRIES: usize = 2000;
const DOCX_MAX_UNCOMPRESSED: u64 = 200 * 1024 * 1024;

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Returns (kind, quarantine note).
fn inspect(bytes: &[u8]) -> AppResult<(Kind, Option<String>)> {
    let unsupported = || {
        AppError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_type",
            "Only PDF, DOCX, JPEG and PNG files are accepted.",
        )
    };
    if bytes.starts_with(b"%PDF-") {
        let risky: Vec<&str> = [("/JavaScript", "script"), ("/JS ", "script"), ("/Launch", "launch action"), ("/EmbeddedFile", "embedded file")]
            .iter()
            .filter(|(n, _)| contains(bytes, n.as_bytes()))
            .map(|(_, label)| *label)
            .collect();
        let note = (!risky.is_empty()).then(|| format!("PDF contains active content: {}", risky.join(", ")));
        return Ok((Kind::Pdf, note));
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Ok((Kind::Png, None));
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok((Kind::Jpeg, None));
    }
    if bytes.starts_with(b"PK\x03\x04") {
        // Inspect the central directory only; never extract.
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| unsupported())?;
        if zip.len() > DOCX_MAX_ENTRIES {
            return Ok((Kind::Docx, Some("Archive has too many entries".into())));
        }
        let (mut total, mut has_ct, mut has_doc, mut macros) = (0u64, false, false, false);
        for i in 0..zip.len() {
            let f = zip.by_index_raw(i).map_err(|_| unsupported())?;
            total = total.saturating_add(f.size());
            match f.name() {
                "[Content_Types].xml" => has_ct = true,
                "word/document.xml" => has_doc = true,
                n if n.to_ascii_lowercase().contains("vbaproject") => macros = true,
                _ => {}
            }
        }
        if !(has_ct && has_doc) {
            return Err(unsupported());
        }
        let note = if macros {
            Some("Document contains macros".to_string())
        } else if total > DOCX_MAX_UNCOMPRESSED {
            Some("Document expands to an unreasonable size".to_string())
        } else {
            None
        };
        return Ok((Kind::Docx, note));
    }
    Err(unsupported())
}

/// Keep a safe display filename: basename only, limited charset/length.
pub fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("file");
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')') { c } else { '_' })
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == '.' || c == ' ').to_string();
    let mut out = if trimmed.is_empty() { "file".to_string() } else { trimmed };
    out.truncate(120);
    out
}

/// Total bytes currently stored for this database (document versions + import sources).
pub fn used_bytes(conn: &Connection) -> AppResult<u64> {
    let n: i64 = conn.query_row(
        "SELECT COALESCE((SELECT SUM(size_bytes) FROM document_versions), 0)",
        [],
        |r| r.get(0),
    )?;
    Ok(n.max(0) as u64)
}

fn path_for(db: &Db, storage_key: &str) -> AppResult<PathBuf> {
    // storage_key = "<2 hex>/<64 hex sha>-<32 hex random>" — validated before touching the filesystem.
    let ok = storage_key.len() == 2 + 1 + 64 + 1 + 32
        && storage_key.as_bytes()[2] == b'/'
        && storage_key.as_bytes()[67] == b'-'
        && storage_key
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 2 || i == 67 || b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !ok {
        return Err(AppError::not_found());
    }
    Ok(db.files_dir().join(storage_key))
}

/// Validate and store bytes. Does not insert DB rows; caller records the returned metadata.
pub fn store(db: &Db, bytes: &[u8], filename: &str, max_bytes: u64) -> AppResult<StoredFile> {
    if bytes.is_empty() {
        return Err(AppError::validation("The file is empty."));
    }
    if bytes.len() as u64 > max_bytes {
        return Err(AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "The file is too large."));
    }
    let (kind, note) = inspect(bytes)?;
    let filename = sanitize_filename(filename);
    let ext = filename.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    if !kind.extensions().contains(&ext.as_str()) {
        return Err(AppError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_type",
            format!("The file content is {} but the name ends with '.{ext}'.", kind.mime()),
        ));
    }
    if let Some(quota) = db.quota_bytes() {
        let used = used_bytes(&db.open()?)?;
        if used + bytes.len() as u64 > quota {
            return Err(AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "The demo storage quota is used up."));
        }
    }
    let raw = write_blob(db, bytes)?;
    Ok(StoredFile {
        storage_key: raw.0,
        sha256: raw.1,
        size_bytes: bytes.len() as i64,
        content_type: kind.mime().to_string(),
        filename,
        scan_status: if note.is_some() { "quarantined" } else { "clean" },
        scan_note: note,
    })
}

/// Write opaque bytes (e.g. an import source file) into the store. Returns (storage_key, sha256).
pub fn write_blob(db: &Db, bytes: &[u8]) -> AppResult<(String, String)> {
    let sha = crate::auth::sha256_hex(bytes);
    let key = format!("{}/{}-{}", &sha[..2], sha, hex::encode(crate::auth::random_bytes::<16>()));
    let final_path = path_for(db, &key)?;
    let tmp_dir = db.files_dir().join("tmp");
    std::fs::create_dir_all(&tmp_dir)?;
    std::fs::create_dir_all(final_path.parent().expect("has parent"))?;
    let tmp = tmp_dir.join(hex::encode(crate::auth::random_bytes::<16>()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &final_path)?;
    Ok((key, sha))
}

/// Read stored bytes and verify their checksum.
pub fn read(db: &Db, storage_key: &str, expected_sha256: &str) -> AppResult<Vec<u8>> {
    let mut buf = Vec::new();
    std::fs::File::open(path_for(db, storage_key)?)?.read_to_end(&mut buf)?;
    if crate::auth::sha256_hex(&buf) != expected_sha256 {
        return Err(AppError::internal("Stored file failed its integrity check."));
    }
    Ok(buf)
}

/// Build a download response. Always `attachment` (except PDF/images when `inline`),
/// `nosniff`, `no-store`, and a sandboxing CSP so nothing executes in our origin.
pub fn file_response(bytes: Vec<u8>, filename: &str, content_type: &str, inline: bool) -> Response {
    let disposition = if inline { "inline" } else { "attachment" };
    let safe = sanitize_filename(filename);
    let mut res = Response::new(Body::from(bytes));
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_str(content_type).unwrap_or(HeaderValue::from_static("application/octet-stream")));
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("{disposition}; filename=\"{safe}\"")).unwrap_or(HeaderValue::from_static("attachment")),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store, private"));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("content-security-policy", HeaderValue::from_static("sandbox; default-src 'none'"));
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_html_and_svg() {
        assert!(inspect(b"<html><script>alert(1)</script>").is_err());
        assert!(inspect(b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_err());
        assert!(inspect(b"MZ\x90\x00").is_err());
    }

    #[test]
    fn quarantines_pdf_with_javascript() {
        let (k, note) = inspect(b"%PDF-1.4\n1 0 obj << /OpenAction << /S /JavaScript /JS (x) >> >>").unwrap();
        assert_eq!(k, Kind::Pdf);
        assert!(note.is_some());
    }

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("a<b>.pdf"), "a_b_.pdf");
    }
}
