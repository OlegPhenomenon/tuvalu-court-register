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

fn decode_pdf_names(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(a), Some(b)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((a * 16 + b) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn pdf_risky(bytes: &[u8]) -> bool {
    let names = decode_pdf_names(bytes);
    [
        "/JavaScript",
        "/JS",
        "/OpenAction",
        "/AA",
        "/Launch",
        "/EmbeddedFile",
        "/RichMedia",
        "/XFA",
        "/SubmitForm",
        "/ImportData",
    ]
    .iter()
    .any(|key| contains(&names, key.as_bytes()))
}

fn pdf_note(bytes: &[u8]) -> Option<String> {
    if pdf_risky(bytes) {
        return Some("PDF contains active content".into());
    }
    let mut start = 0;
    let mut expanded = 0;
    let mut streams = 0;
    while let Some(offset) = bytes[start..].windows(6).position(|w| w == b"stream") {
        let at = start + offset;
        let dict_start = bytes[..at].windows(6).rposition(|w| w == b"endobj").map(|p| p + 6).unwrap_or(start);
        let dict = decode_pdf_names(&bytes[dict_start..at]);
        let content = at + 6;
        let content = if bytes.get(content..content + 2) == Some(b"\r\n") {
            content + 2
        } else if bytes.get(content) == Some(&b'\n') || bytes.get(content) == Some(&b'\r') {
            content + 1
        } else {
            start = content;
            continue;
        };
        let Some(end) = bytes[content..].windows(9).position(|w| w == b"endstream") else {
            return Some("PDF stream cannot be checked".into());
        };
        let end = content + end;
        streams += 1;
        if streams > 256 {
            return Some("PDF stream limit exceeded".into());
        }
        if contains(&dict, b"/FlateDecode") {
            let limit = (32 * 1024 * 1024_usize).saturating_sub(expanded).min(8 * 1024 * 1024);
            match miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&bytes[content..end], limit) {
                Ok(decoded) => {
                    expanded += decoded.len();
                    if pdf_risky(&decoded) {
                        return Some("PDF stream contains active content".into());
                    }
                }
                Err(_) => return Some("PDF compressed stream could not be checked within limits".into()),
            }
        } else if contains(&dict, b"/ObjStm") && contains(&dict, b"/Filter") {
            return Some("PDF object stream has an unsupported filter".into());
        }
        start = end + 9;
    }
    None
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for b in bytes {
        crc ^= *b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

fn valid_png(bytes: &[u8]) -> bool {
    let mut at = 8;
    let mut first = true;
    let mut image = false;
    while at + 12 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let Some(end) = at.checked_add(12).and_then(|p| p.checked_add(size)).filter(|p| *p <= bytes.len()) else {
            return false;
        };
        let kind = &bytes[at + 4..at + 8];
        if !kind.iter().all(u8::is_ascii_alphabetic) {
            return false;
        }
        if crc32(&bytes[at + 4..end - 4]) != u32::from_be_bytes(bytes[end - 4..end].try_into().unwrap()) {
            return false;
        }
        if first {
            if kind != b"IHDR" || size != 13 {
                return false;
            }
            let header = &bytes[at + 8..at + 21];
            if header[..4] == [0; 4] || header[4..8] == [0; 4] || header[10] != 0 || header[11] != 0 || header[12] > 1 {
                return false;
            }
            let depth = header[8];
            let allowed = match header[9] {
                0 => [1, 2, 4, 8, 16].contains(&depth),
                2 | 4 | 6 => [8, 16].contains(&depth),
                3 => [1, 2, 4, 8].contains(&depth),
                _ => false,
            };
            if !allowed {
                return false;
            }
        } else if kind == b"IHDR" {
            return false;
        }
        if kind == b"IDAT" {
            image = true;
        }
        if kind == b"IEND" {
            return size == 0 && image && end == bytes.len();
        }
        first = false;
        at = end;
    }
    false
}

fn valid_jpeg(bytes: &[u8]) -> bool {
    let mut at = 2;
    let mut frame = false;
    let mut scan = false;
    while at < bytes.len() {
        if bytes[at] != 0xff {
            return false;
        }
        while bytes.get(at) == Some(&0xff) {
            at += 1;
        }
        let Some(&marker) = bytes.get(at) else {
            return false;
        };
        at += 1;
        if marker == 0xd9 {
            return frame && scan && at == bytes.len();
        }
        if marker == 0 || marker == 0xd8 || (0xd0..=0xd7).contains(&marker) {
            return false;
        }
        if marker == 1 {
            continue;
        }
        if at + 2 > bytes.len() {
            return false;
        }
        let size = u16::from_be_bytes([bytes[at], bytes[at + 1]]) as usize;
        if size < 2 || at + size > bytes.len() {
            return false;
        }
        if matches!(marker,0xc0..=0xc3|0xc5..=0xc7|0xc9..=0xcb|0xcd..=0xcf) {
            if size < 8 || bytes[at + 3..at + 5] == [0, 0] || bytes[at + 5..at + 7] == [0, 0] || size != 8 + 3 * bytes[at + 7] as usize {
                return false;
            }
            frame = true;
        }
        if marker == 0xda {
            if !frame || size < 6 || size != 6 + 2 * bytes[at + 2] as usize {
                return false;
            }
            scan = true;
            at += size;
            loop {
                let Some(offset) = bytes[at..].iter().position(|b| *b == 0xff) else {
                    return false;
                };
                at += offset;
                let Some(&next) = bytes.get(at + 1) else {
                    return false;
                };
                if next == 0 || (0xd0..=0xd7).contains(&next) {
                    at += 2;
                    continue;
                }
                break;
            }
        } else {
            at += size;
        }
    }
    false
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
        return Ok((Kind::Pdf, pdf_note(bytes)));
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Ok((Kind::Png, (!valid_png(bytes)).then(|| "Malformed PNG structure or CRC".into())));
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok((Kind::Jpeg, (!valid_jpeg(bytes)).then(|| "Malformed JPEG segment structure".into())));
    }
    if bytes.starts_with(b"PK\x03\x04") {
        // Inspect archive metadata and bounded XML bodies; never extract to the filesystem.
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| unsupported())?;
        if zip.len() > DOCX_MAX_ENTRIES {
            return Ok((Kind::Docx, Some("Archive has too many entries".into())));
        }
        let (mut total, mut has_ct, mut has_doc, mut macros) = (0u64, false, false, false);
        for i in 0..zip.len() {
            let mut f = zip.by_index(i).map_err(|_| unsupported())?;
            total = total.saturating_add(f.size());
            match f.name() {
                "[Content_Types].xml" => has_ct = true,
                "word/document.xml" => has_doc = true,
                n if ["vbaproject", "activex", "embeddings/", "oleobject"]
                    .iter()
                    .any(|key| n.to_ascii_lowercase().contains(key)) =>
                {
                    macros = true
                }
                _ => {}
            }
            if total <= DOCX_MAX_UNCOMPRESSED
                && (f.name().to_ascii_lowercase().ends_with(".xml") || f.name().to_ascii_lowercase().ends_with(".rels"))
            {
                let mut xml = Vec::new();
                (&mut f)
                    .take(4 * 1024 * 1024 + 1)
                    .read_to_end(&mut xml)
                    .map_err(|_| unsupported())?;
                let lower: Vec<u8> = xml.iter().map(u8::to_ascii_lowercase).collect();
                if xml.len() > 4 * 1024 * 1024
                    || ["macroenabled", "activex", "oleobject", "vbaproject"]
                        .iter()
                        .any(|key| contains(&lower, key.as_bytes()))
                {
                    macros = true;
                }
            }
        }
        if !(has_ct && has_doc) {
            return Err(unsupported());
        }
        let note = if macros {
            Some("Document contains macros, ActiveX or embedded OLE content".to_string())
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
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == '.' || c == ' ').to_string();
    let mut out = if trimmed.is_empty() { "file".to_string() } else { trimmed };
    out.truncate(120);
    out
}

/// Total bytes currently stored for this database (document versions + import sources).
pub fn used_bytes(conn: &Connection) -> AppResult<u64> {
    let n: i64 = conn.query_row("SELECT COALESCE((SELECT SUM(size_bytes) FROM document_versions), 0)", [], |r| {
        r.get(0)
    })?;
    let mut used = n.max(0) as u64;
    let mut stmt = conn.prepare("SELECT storage_key, json_extract(source_options, '$.size_bytes') FROM import_batches")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?)))?;
    for row in rows {
        let (key, size) = row?;
        let size = match size {
            Some(size) => size.max(0) as u64,
            // Older batches predate source sizes in the immutable source options.
            None => {
                let parent = conn
                    .path()
                    .and_then(|p| std::path::Path::new(p).parent())
                    .ok_or_else(|| AppError::internal("Missing database path."))?;
                let db = Db::new(parent.join("court.sqlite"), parent.join("files"), None);
                std::fs::metadata(path_for(&db, &key)?)?.len()
            }
        };
        used = used.saturating_add(size);
    }
    Ok(used)
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

/// Remove an unreferenced blob after a failed write or an idempotent replay.
pub fn discard(db: &Db, storage_key: &str) {
    if let Ok(path) = path_for(db, storage_key) {
        let _ = std::fs::remove_file(path);
    }
}

/// Validate and store bytes. Does not insert DB rows; caller records the returned metadata.
pub fn store(db: &Db, bytes: &[u8], filename: &str, max_bytes: u64) -> AppResult<StoredFile> {
    store_inner(db, bytes, filename, max_bytes, true)
}

/// HTTP uploads commit a pending row before contacting the scanner.
pub fn prepare_upload(db: &Db, bytes: &[u8], filename: &str, max_bytes: u64) -> AppResult<StoredFile> {
    store_inner(db, bytes, filename, max_bytes, false)
}

fn store_inner(db: &Db, bytes: &[u8], filename: &str, max_bytes: u64, scan_now: bool) -> AppResult<StoredFile> {
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
            return Err(AppError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                "The demo storage quota is used up.",
            ));
        }
    }
    let (scan_status, scan_note) = if let Some(note) = note {
        ("quarantined", Some(note))
    } else if !scan_now
        && db
            .config()
            .is_some_and(|c| c.mode == crate::config::Mode::Production && c.clamd.is_some())
    {
        ("pending_scan", Some("Awaiting ClamAV verdict; file cannot be opened".into()))
    } else {
        match crate::scan::verdict(db.config(), bytes) {
            Ok(note) => ("clean", note),
            Err(note) => ("quarantined", Some(note)),
        }
    };
    let raw = write_blob(db, bytes)?;
    Ok(StoredFile {
        storage_key: raw.0,
        sha256: raw.1,
        size_bytes: bytes.len() as i64,
        content_type: kind.mime().to_string(),
        filename,
        scan_status,
        scan_note,
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
    let conn = db.open()?;
    let blocked: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM document_versions WHERE storage_key=?1 AND scan_status!='clean')",
        [storage_key],
        |r| r.get(0),
    )?;
    if blocked {
        return Err(AppError::conflict(
            "quarantined",
            "This file is pending a check or quarantined and cannot be opened.",
        ));
    }
    read_for_scan(db, storage_key, expected_sha256)
}

/// Scanner-only integrity read; public file access must use `read`, which checks the verdict.
pub(crate) fn read_for_scan(db: &Db, storage_key: &str, expected_sha256: &str) -> AppResult<Vec<u8>> {
    let mut file = std::fs::File::open(path_for(db, storage_key)?)?;
    let mut buf = Vec::with_capacity(file.metadata()?.len() as usize);
    file.read_to_end(&mut buf)?;
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
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
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
