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

// Retain the conservative active-name check, including names in displayed text.
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

fn pdf_active_name(name: &[u8]) -> bool {
    [
        b"JavaScript".as_slice(),
        b"JS",
        b"OpenAction",
        b"AA",
        b"Launch",
        b"EmbeddedFile",
        b"EmbeddedFiles",
        b"RichMedia",
        b"XFA",
        b"SubmitForm",
        b"ImportData",
    ]
    .contains(&name)
}

fn pdf_risky(bytes: &[u8]) -> bool {
    let names = decode_pdf_names(bytes);
    let mut at = 0;
    let mut atom_start = 0;
    let mut name_chain = None;
    while at < names.len() {
        if names[at] == b'/' {
            // A name starts after a delimiter, a preceding name, or a PDF scalar.
            // Arbitrary binary/text substrings such as DEMO-profile/AA are not names.
            let atom = &names[atom_start..at];
            let boundary = name_chain.unwrap_or_else(|| {
                atom.is_empty()
                    || [
                        b"true".as_slice(),
                        b"false",
                        b"null",
                        b"R",
                        b"obj",
                        b"endobj",
                    ]
                    .contains(&atom)
                    || (atom.iter().any(u8::is_ascii_digit)
                        && atom
                            .iter()
                            .all(|b| b.is_ascii_digit() || b"+-.".contains(b)))
            });
            let start = at + 1;
            at = start;
            while at < names.len() && !pdf_delimiter(names[at]) {
                at += 1;
            }
            // AA requires an action dictionary/reference; JS requires a string/stream.
            // Neither accepts a name value. Slash chains in XML thumbnails can contain
            // these short byte sequences without representing either action key.
            let name_value = names.get(at) == Some(&b'/')
                && [b"AA".as_slice(), b"JS"].contains(&&names[start..at]);
            if boundary && !name_value && pdf_active_name(&names[start..at]) {
                return true;
            }
            name_chain = Some(boundary);
        } else {
            if pdf_delimiter(names[at]) {
                atom_start = at + 1;
            }
            name_chain = None;
            at += 1;
        }
    }
    false
}

// PDF syntax must be tokenised before locating stream dictionaries: comments and
// strings may contain arbitrary structural words such as endobj and stream.
#[derive(Debug)]
enum PdfValue {
    Name(Vec<u8>),
    Atom(Vec<u8>),
    Dict(Vec<(Vec<u8>, PdfValue)>),
    Array(Vec<PdfValue>),
    Scalar,
}

struct PdfTokens<'a> {
    bytes: &'a [u8],
    at: usize,
    risky: bool,
}

fn pdf_space(b: u8) -> bool {
    matches!(b, 0 | b'\t' | b'\n' | 12 | b'\r' | b' ')
}
fn pdf_delimiter(b: u8) -> bool {
    pdf_space(b) || b"()<>[]{}/%".contains(&b)
}
impl<'a> PdfTokens<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            at: 0,
            risky: false,
        }
    }
    fn space(&mut self) {
        loop {
            while self.bytes.get(self.at).is_some_and(|b| pdf_space(*b)) {
                self.at += 1;
            }
            if self.bytes.get(self.at) != Some(&b'%') {
                break;
            }
            while self
                .bytes
                .get(self.at)
                .is_some_and(|b| !matches!(b, b'\r' | b'\n'))
            {
                self.at += 1;
            }
        }
    }
    fn starts(&self, token: &[u8]) -> bool {
        self.bytes.get(self.at..self.at + token.len()) == Some(token)
    }
    fn value(&mut self, depth: usize) -> Result<PdfValue, &'static str> {
        self.space();
        if depth > 64 {
            return Err("PDF syntax limit exceeded");
        }
        let Some(&first) = self.bytes.get(self.at) else {
            return Err("Incomplete PDF value");
        };
        if self.starts(b"<<") {
            self.at += 2;
            let mut entries = Vec::new();
            let mut keys = std::collections::BTreeSet::new();
            loop {
                self.space();
                if self.starts(b">>") {
                    self.at += 2;
                    break;
                }
                let PdfValue::Name(key) = self.value(depth + 1)? else {
                    return Err("Unparsable PDF dictionary key");
                };
                if !keys.insert(key.clone()) {
                    return Err("Duplicate PDF dictionary key");
                }
                let value = self.value(depth + 1)?;
                // Indirect reference lookahead reads only its numeric generation and R;
                // it must not parse (and allocate) the next dictionary speculatively.
                if matches!(&value, PdfValue::Atom(n) if n.iter().all(u8::is_ascii_digit)) {
                    let saved = self.at;
                    self.space();
                    let generation = self.at;
                    while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                        self.at += 1;
                    }
                    let numeric = self.at > generation
                        && self.bytes.get(self.at).is_none_or(|b| pdf_delimiter(*b));
                    self.space();
                    if numeric
                        && self.starts(b"R")
                        && self
                            .bytes
                            .get(self.at + 1)
                            .is_none_or(|b| pdf_delimiter(*b))
                    {
                        self.at += 1;
                        entries.push((key, PdfValue::Scalar));
                        continue;
                    }
                    self.at = saved;
                }
                entries.push((key, value));
            }
            return Ok(PdfValue::Dict(entries));
        }
        self.at += 1;
        match first {
            b'/' => {
                let mut name = Vec::new();
                while let Some(&b) = self.bytes.get(self.at).filter(|b| !pdf_delimiter(**b)) {
                    if b == b'#' {
                        let Some(hex) = self.bytes.get(self.at + 1..self.at + 3) else {
                            return Err("Invalid PDF name escape");
                        };
                        let a = (hex[0] as char)
                            .to_digit(16)
                            .ok_or("Invalid PDF name escape")?;
                        let b = (hex[1] as char)
                            .to_digit(16)
                            .ok_or("Invalid PDF name escape")?;
                        name.push((a * 16 + b) as u8);
                        self.at += 3;
                    } else {
                        name.push(b);
                        self.at += 1;
                    }
                }
                self.risky |= pdf_active_name(&name);
                Ok(PdfValue::Name(name))
            }
            b'(' => {
                let mut nesting = 1usize;
                while nesting > 0 {
                    let b = *self.bytes.get(self.at).ok_or("Unterminated PDF string")?;
                    self.at += 1;
                    match b {
                        b'\\' => {
                            let escaped = *self
                                .bytes
                                .get(self.at)
                                .ok_or("Unterminated PDF string escape")?;
                            self.at += 1;
                            if escaped == b'\r' && self.bytes.get(self.at) == Some(&b'\n') {
                                self.at += 1;
                            }
                        }
                        b'(' => {
                            nesting += 1;
                            if nesting > 64 {
                                return Err("PDF string nesting limit exceeded");
                            }
                        }
                        b')' => nesting -= 1,
                        _ => {}
                    }
                }
                Ok(PdfValue::Scalar)
            }
            b'<' => {
                loop {
                    let b = *self
                        .bytes
                        .get(self.at)
                        .ok_or("Unterminated PDF hex string")?;
                    self.at += 1;
                    if b == b'>' {
                        break;
                    }
                    if !pdf_space(b) && !b.is_ascii_hexdigit() {
                        return Err("Invalid PDF hex string");
                    }
                }
                Ok(PdfValue::Scalar)
            }
            b'[' => {
                let mut values = Vec::new();
                loop {
                    self.space();
                    if self.bytes.get(self.at) == Some(&b']') {
                        self.at += 1;
                        break;
                    }
                    values.push(self.value(depth + 1)?);
                }
                Ok(PdfValue::Array(values))
            }
            b'>' | b')' | b']' | b'{' | b'}' => Err("Unexpected PDF delimiter"),
            _ => {
                let start = self.at - 1;
                while self.bytes.get(self.at).is_some_and(|b| !pdf_delimiter(*b)) {
                    self.at += 1;
                }
                Ok(PdfValue::Atom(self.bytes[start..self.at].to_vec()))
            }
        }
    }
}

fn pdf_field<'a>(dict: &'a [(Vec<u8>, PdfValue)], key: &[u8]) -> Option<&'a PdfValue> {
    dict.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}
fn pdf_name_is(value: Option<&PdfValue>, expected: &[u8]) -> bool {
    matches!(value, Some(PdfValue::Name(name)) if name == expected)
}

// Predictors transform inflated bytes before inspection. Only the bounded, common
// 8-bit TIFF and PNG layouts are supported for non-image streams; others fail closed.
fn pdf_predictor(bytes: Vec<u8>, params: Option<&PdfValue>) -> Result<Vec<u8>, &'static str> {
    let dict = match params {
        None => return Ok(bytes),
        Some(PdfValue::Atom(a)) if a == b"null" => return Ok(bytes),
        Some(PdfValue::Dict(dict)) => dict,
        _ => return Err("PDF decode parameters cannot be inspected"),
    };
    let integer = |key: &[u8], default: usize| -> Result<usize, &'static str> {
        match pdf_field(dict, key) {
            None => Ok(default),
            Some(PdfValue::Atom(n)) => std::str::from_utf8(n)
                .ok()
                .and_then(|n| n.parse().ok())
                .ok_or("Invalid PDF predictor parameter"),
            _ => Err("PDF predictor parameter cannot be inspected"),
        }
    };
    let predictor = integer(b"Predictor", 1)?;
    if predictor == 1 {
        return Ok(bytes);
    }
    let colors = integer(b"Colors", 1)?;
    let columns = integer(b"Columns", 1)?;
    if integer(b"BitsPerComponent", 8)? != 8 || colors == 0 || columns == 0 {
        return Err("PDF predictor layout cannot be inspected");
    }
    let row = colors
        .checked_mul(columns)
        .filter(|n| *n <= 8 * 1024 * 1024)
        .ok_or("PDF predictor row exceeds limits")?;
    if predictor == 2 {
        if !bytes.len().is_multiple_of(row) {
            return Err("Invalid TIFF predictor data");
        }
        let mut decoded = bytes;
        for line in decoded.chunks_mut(row) {
            for i in colors..row {
                line[i] = line[i].wrapping_add(line[i - colors]);
            }
        }
        return Ok(decoded);
    }
    if !(10..=15).contains(&predictor) || !bytes.len().is_multiple_of(row + 1) {
        return Err("PDF predictor cannot be inspected");
    }
    let mut decoded = Vec::<u8>::with_capacity(bytes.len());
    for line in bytes.chunks_exact(row + 1) {
        let filter = line[0];
        if filter > 4 {
            return Err("Invalid PNG predictor data");
        }
        let start = decoded.len();
        for (i, byte) in line[1..].iter().enumerate() {
            let left = if i >= colors {
                decoded[start + i - colors]
            } else {
                0
            };
            let up = if start >= row {
                decoded[start - row + i]
            } else {
                0
            };
            let upper_left = if start >= row && i >= colors {
                decoded[start - row + i - colors]
            } else {
                0
            };
            let prediction = match filter {
                0 => 0,
                1 => left,
                2 => up,
                3 => ((left as u16 + up as u16) / 2) as u8,
                4 => {
                    let p = left as i16 + up as i16 - upper_left as i16;
                    let a = (p - left as i16).abs();
                    let b = (p - up as i16).abs();
                    let c = (p - upper_left as i16).abs();
                    if a <= b && a <= c {
                        left
                    } else if b <= c {
                        up
                    } else {
                        upper_left
                    }
                }
                _ => unreachable!(),
            };
            decoded.push(byte.wrapping_add(prediction));
        }
    }
    Ok(decoded)
}

fn check_pdf(bytes: &[u8], expanded: &mut usize, depth: usize) -> Result<(), &'static str> {
    if depth > 8 {
        return Err("PDF compressed nesting limit exceeded");
    }
    let mut tokens = PdfTokens::new(bytes);
    let mut previous = None;
    loop {
        tokens.space();
        if tokens.at == bytes.len() {
            break;
        }
        let start_value = tokens.at;
        let value = tokens.value(0)?;
        // Inspect object syntax, including displayed strings, but never compressed bytes.
        if tokens.risky || pdf_risky(&bytes[start_value..tokens.at]) {
            return Err("PDF contains active content");
        }
        if matches!(&value, PdfValue::Atom(word) if word == b"stream") {
            let Some(PdfValue::Dict(dict)) = previous.take() else {
                return Err("PDF stream dictionary missing");
            };
            if tokens.starts(b"\r\n") {
                tokens.at += 2;
            } else if tokens.starts(b"\n") || tokens.starts(b"\r") {
                tokens.at += 1;
            } else {
                return Err("PDF stream requires a line ending");
            }
            let start = tokens.at;
            // Direct lengths prevent literal endstream bytes truncating compressed data.
            let length = match pdf_field(&dict, b"Length") {
                Some(PdfValue::Atom(n)) => Some(
                    std::str::from_utf8(n)
                        .ok()
                        .and_then(|s| s.parse::<usize>().ok())
                        .ok_or("Invalid PDF stream length")?,
                ),
                _ => None,
            };
            let end = if let Some(length) = length {
                start
                    .checked_add(length)
                    .filter(|end| *end <= bytes.len())
                    .ok_or("Invalid PDF stream length")?
            } else {
                let offset = bytes[start..]
                    .windows(9)
                    .enumerate()
                    .find(|(i, w)| {
                        *w == b"endstream"
                            && (*i == 0 || pdf_space(bytes[start + i - 1]))
                            && bytes.get(start + i + 9).is_none_or(|b| pdf_delimiter(*b))
                    })
                    .map(|(i, _)| i)
                    .ok_or("PDF stream cannot be checked")?;
                // The zlib decoder stops at its end marker. Retain separator bytes:
                // trimming CR/LF can remove checksum bytes from an indirect-length stream.
                start + offset
            };
            tokens.at = end;
            tokens.space();
            if !matches!(tokens.value(0)?, PdfValue::Atom(word) if word == b"endstream") {
                return Err("PDF stream terminator missing");
            }
            let filters = match pdf_field(&dict, b"Filter") {
                None => Vec::new(),
                Some(PdfValue::Name(name)) => vec![name.as_slice()],
                Some(PdfValue::Array(values)) => values
                    .iter()
                    .map(|v| match v {
                        PdfValue::Name(name) => Ok(name.as_slice()),
                        _ => Err("Invalid PDF stream filter"),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                _ => return Err("Invalid PDF stream filter"),
            };
            let object = pdf_name_is(pdf_field(&dict, b"Type"), b"ObjStm");
            let image = !object
                && (pdf_name_is(pdf_field(&dict, b"Subtype"), b"Image")
                    || filters.iter().any(|f| {
                        [
                            b"DCTDecode".as_slice(),
                            b"DCT",
                            b"JPXDecode",
                            b"CCITTFaxDecode",
                            b"CCF",
                            b"JBIG2Decode",
                        ]
                        .contains(f)
                    }));
            let params = pdf_field(&dict, b"DecodeParms");
            if filters.iter().any(|f| *f != b"FlateDecode" && *f != b"Fl") {
                if !image {
                    return Err("PDF non-image stream has an unsupported filter");
                }
            } else {
                let mut decoded = bytes[start..end].to_vec();
                for (i, _) in filters.iter().enumerate() {
                    let limit = (32 * 1024 * 1024_usize)
                        .saturating_sub(*expanded)
                        .min(8 * 1024 * 1024);
                    decoded =
                        miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&decoded, limit)
                            .map_err(
                                |_| "PDF compressed stream could not be checked within limits",
                            )?;
                    *expanded += decoded.len();
                    if !image {
                        let params = match params {
                            Some(PdfValue::Array(values)) => {
                                if values.len() != filters.len() {
                                    return Err("PDF decode parameters do not match filters");
                                }
                                values.get(i)
                            }
                            other => other,
                        };
                        decoded = pdf_predictor(decoded, params)?;
                    }
                }
                // DecodeParms on an unfiltered non-image stream is ambiguous.
                if filters.is_empty() && !image && params.is_some() {
                    decoded = pdf_predictor(decoded, params)?;
                }
                if object {
                    check_pdf(&decoded, expanded, depth + 1)?;
                } else if pdf_risky(&decoded) {
                    return Err("PDF contains active content");
                }
            }
            previous = None;
        } else {
            previous = Some(value);
        }
    }
    Ok(())
}
fn pdf_note(bytes: &[u8]) -> Option<String> {
    check_pdf(bytes, &mut 0, 0).err().map(str::to_string)
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
