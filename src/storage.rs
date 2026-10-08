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

fn pdf_active_name(name: &[u8]) -> bool {
    [
        b"JavaScript".as_slice(),
        b"JS",
        b"EmbeddedFile",
        b"EmbeddedFiles",
        b"RichMedia",
        b"XFA",
    ]
    .contains(&name)
}

fn pdf_risky(bytes: &[u8]) -> bool {
    let mut scan = PdfNameScan {
        numeric: true,
        ..Default::default()
    };
    scan.feed(bytes);
    scan.finish();
    scan.risky
}

// PDF syntax must be tokenised before locating stream dictionaries: comments and
// strings may contain arbitrary structural words such as endobj and stream.
#[derive(Debug)]
enum PdfValue {
    Name(Vec<u8>),
    Atom(Vec<u8>),
    Dict(Vec<(Vec<u8>, PdfValue)>),
    Array(Vec<PdfValue>),
    LimitedArray(Vec<PdfValue>),
    String(Vec<u8>),
    Ref(u32, u32),
    Scalar,
}

struct PdfTokens<'a> {
    bytes: &'a [u8],
    at: usize,
    risky: bool,
    allocated: usize,
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
            allocated: 0,
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
        self.allocated += std::mem::size_of::<PdfValue>();
        if self.allocated > PDF_VALUE_CAP {
            return Err("PDF object syntax exceeds memory limit");
        }
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
                if !keys.insert(key.clone()) && pdf_security_key(&key) {
                    return Err("Duplicate PDF dictionary key");
                }
                let value = self.value(depth + 1)?;
                if key == b"Encrypt" && !matches!(&value, PdfValue::Atom(n) if n == b"null") {
                    return Err("PDF encryption cannot be inspected");
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
                self.allocated += name.len();
                if self.allocated > PDF_VALUE_CAP {
                    return Err("PDF object syntax exceeds memory limit");
                }
                self.risky |= pdf_active_name(&name);
                Ok(PdfValue::Name(name))
            }
            b'(' => {
                let mut nesting = 1usize;
                let mut text = Vec::new();
                while nesting > 0 {
                    let mut b = *self.bytes.get(self.at).ok_or("Unterminated PDF string")?;
                    self.at += 1;
                    match b {
                        b'\\' => {
                            b = *self
                                .bytes
                                .get(self.at)
                                .ok_or("Unterminated PDF string escape")?;
                            self.at += 1;
                            b = match b {
                                b'n' => b'\n',
                                b'r' => b'\r',
                                b't' => b'\t',
                                b'b' => 8,
                                b'f' => 12,
                                b'\r' | b'\n' => {
                                    if b == b'\r' && self.bytes.get(self.at) == Some(&b'\n') {
                                        self.at += 1;
                                    }
                                    continue;
                                }
                                b'0'..=b'7' => {
                                    let mut n = b - b'0';
                                    for _ in 0..2 {
                                        if let Some(&digit @ b'0'..=b'7') = self.bytes.get(self.at)
                                        {
                                            n = n.wrapping_mul(8).wrapping_add(digit - b'0');
                                            self.at += 1;
                                        } else {
                                            break;
                                        }
                                    }
                                    n
                                }
                                other => other,
                            };
                        }
                        b'(' => {
                            nesting += 1;
                            if nesting > 64 {
                                return Err("PDF string nesting limit exceeded");
                            }
                        }
                        b')' => {
                            nesting -= 1;
                            if nesting == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    if text.len() <= 4096 {
                        text.push(b);
                    }
                }
                if text.len() > 4096 {
                    Ok(PdfValue::Scalar)
                } else {
                    self.allocated += text.len();
                    Ok(PdfValue::String(text))
                }
            }
            b'<' => {
                let mut text = Vec::new();
                let mut high = None;
                loop {
                    let b = *self
                        .bytes
                        .get(self.at)
                        .ok_or("Unterminated PDF hex string")?;
                    self.at += 1;
                    if b == b'>' {
                        break;
                    }
                    if pdf_space(b) {
                        continue;
                    }
                    let digit = (b as char).to_digit(16).ok_or("Invalid PDF hex string")? as u8;
                    if let Some(h) = high.take() {
                        if text.len() <= 4096 {
                            text.push(h * 16 + digit);
                        }
                    } else {
                        high = Some(digit);
                    }
                }
                if let Some(h) = high {
                    text.push(h * 16);
                }
                if text.len() > 4096 {
                    Ok(PdfValue::Scalar)
                } else {
                    self.allocated += text.len();
                    Ok(PdfValue::String(text))
                }
            }
            b'[' => {
                let mut values = Vec::new();
                let mut limited = false;
                loop {
                    self.space();
                    if self.bytes.get(self.at) == Some(&b']') {
                        self.at += 1;
                        break;
                    }
                    let allocated = self.allocated;
                    let value = self.value(depth + 1)?;
                    // Large coordinate/xref arrays do not need hundreds of thousands
                    // of retained scalars. Keep structural values for action inspection.
                    if values.len() < 1024
                        || matches!(
                            value,
                            PdfValue::Dict(_) | PdfValue::Array(_) | PdfValue::LimitedArray(_)
                        )
                    {
                        values.push(value);
                    } else {
                        self.allocated = allocated;
                        limited = true;
                    }
                }
                if limited {
                    Ok(PdfValue::LimitedArray(values))
                } else {
                    Ok(PdfValue::Array(values))
                }
            }
            b'>' | b')' | b']' | b'{' | b'}' => Err("Unexpected PDF delimiter"),
            _ => {
                let start = self.at - 1;
                while self.bytes.get(self.at).is_some_and(|b| !pdf_delimiter(*b)) {
                    self.at += 1;
                }
                let word = &self.bytes[start..self.at];
                if word.len() > 4096 {
                    return Err("PDF token exceeds memory limit");
                }
                self.allocated += word.len();
                if let Ok(number) = std::str::from_utf8(word).unwrap_or("").parse::<u32>() {
                    let saved = self.at;
                    self.space();
                    let generation = self.at;
                    while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                        self.at += 1;
                    }
                    let numeric = self.at > generation
                        && self.bytes.get(self.at).is_none_or(|b| pdf_delimiter(*b));
                    let gen_number = std::str::from_utf8(&self.bytes[generation..self.at])
                        .unwrap_or("")
                        .parse::<u32>();
                    self.space();
                    if numeric
                        && self.starts(b"R")
                        && self
                            .bytes
                            .get(self.at + 1)
                            .is_none_or(|b| pdf_delimiter(*b))
                    {
                        self.at += 1;
                        return Ok(PdfValue::Ref(
                            number,
                            gen_number.map_err(|_| "Invalid PDF reference")?,
                        ));
                    }
                    self.at = saved;
                }
                Ok(PdfValue::Atom(word.to_vec()))
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

const PDF_OBJECT_CAP: usize = 32 * 1024 * 1024;
const PDF_VALUE_CAP: usize = 8 * 1024 * 1024;
const PDF_CHUNK: usize = 8192;

type PdfObjects = std::collections::BTreeMap<(u32, u32), PdfValue>;

fn pdf_security_key(key: &[u8]) -> bool {
    [
        b"Type".as_slice(),
        b"S",
        b"Filter",
        b"DecodeParms",
        b"Length",
        b"JS",
        b"JavaScript",
        b"AA",
        b"OpenAction",
        b"Names",
        b"EmbeddedFiles",
        b"URI",
        b"F",
        b"Launch",
        b"Subtype",
        b"Next",
        b"UF",
        b"EF",
        b"N",
        b"First",
        b"Predictor",
        b"Colors",
        b"Columns",
        b"BitsPerComponent",
        b"EarlyChange",
        b"DOS",
        b"Mac",
        b"Unix",
        b"FS",
        b"Encrypt",
    ]
    .contains(&key)
}
fn pdf_integer(value: Option<&PdfValue>, default: usize) -> Result<usize, &'static str> {
    match value {
        None => Ok(default),
        Some(PdfValue::Atom(n)) => std::str::from_utf8(n)
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or("Invalid PDF integer parameter"),
        _ => Err("PDF parameter cannot be inspected"),
    }
}

#[derive(Debug)]
struct PdfDecodeError(&'static str);
impl std::fmt::Display for PdfDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for PdfDecodeError {}
fn pdf_io(reason: &'static str) -> std::io::Error {
    std::io::Error::other(PdfDecodeError(reason))
}
fn pdf_read_error(error: std::io::Error) -> &'static str {
    error
        .get_ref()
        .and_then(|e| e.downcast_ref::<PdfDecodeError>())
        .map_or("PDF encoded stream is malformed", |e| e.0)
}

// Every filter stage shares a file-wide work budget; chains cannot multiply it.
struct PdfBudget<'a> {
    inner: Box<dyn Read + 'a>,
    left: std::rc::Rc<std::cell::Cell<usize>>,
}
impl Read for PdfBudget<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(out)?;
        let remaining = self
            .left
            .get()
            .checked_sub(n)
            .ok_or_else(|| pdf_io("PDF decoded bytes exceed file work limit"))?;
        self.left.set(remaining);
        Ok(n)
    }
}

struct PdfFlate<'a> {
    inner: Box<dyn Read + 'a>,
    state: Box<miniz_oxide::inflate::stream::InflateState>,
    input: [u8; PDF_CHUNK],
    at: usize,
    len: usize,
    done: bool,
}
impl Read for PdfFlate<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() || self.done {
            return Ok(0);
        }
        loop {
            if self.at == self.len {
                self.len = self.inner.read(&mut self.input)?;
                self.at = 0;
            }
            let result = miniz_oxide::inflate::stream::inflate(
                &mut self.state,
                &self.input[self.at..self.len],
                out,
                miniz_oxide::MZFlush::None,
            );
            self.at += result.bytes_consumed;
            match result.status {
                Ok(miniz_oxide::MZStatus::StreamEnd) => self.done = true,
                Ok(_) => {}
                Err(_) => return Err(pdf_io("PDF Flate stream is malformed")),
            }
            if result.bytes_written > 0 || self.done {
                return Ok(result.bytes_written);
            }
            if result.bytes_consumed == 0 {
                return Err(pdf_io("PDF Flate stream is malformed"));
            }
        }
    }
}

// Byte-oriented encodings use a buffered upstream reader, fixed output groups and
// a fixed 4096-entry LZW dictionary. No stage retains the complete decoded stream.
struct PdfCodes<'a> {
    inner: std::io::BufReader<Box<dyn Read + 'a>>,
    kind: u8,
    pending: Vec<u8>,
    at: usize,
    done: bool,
    prefix: [u16; 4096],
    suffix: [u8; 4096],
    previous: Option<u16>,
    next: usize,
    width: usize,
    early: usize,
    bits: u32,
    bit_count: usize,
}
impl<'a> PdfCodes<'a> {
    fn new(inner: Box<dyn Read + 'a>, kind: u8, early: usize) -> Self {
        Self {
            inner: std::io::BufReader::new(inner),
            kind,
            pending: Vec::with_capacity(4096),
            at: 0,
            done: false,
            prefix: [0; 4096],
            suffix: [0; 4096],
            previous: None,
            next: 258,
            width: 9,
            early,
            bits: 0,
            bit_count: 0,
        }
    }
    fn byte(&mut self) -> std::io::Result<u8> {
        let mut b = [0];
        self.inner.read_exact(&mut b)?;
        Ok(b[0])
    }
    fn nonspace(&mut self) -> std::io::Result<u8> {
        loop {
            let b = self.byte()?;
            if !pdf_space(b) {
                return Ok(b);
            }
        }
    }
    fn sequence(&self, mut code: u16, out: &mut Vec<u8>) -> std::io::Result<()> {
        let start = out.len();
        while code >= 258 {
            if code as usize >= self.next || out.len() - start >= 4096 {
                return Err(pdf_io("PDF LZW stream is malformed"));
            }
            out.push(self.suffix[code as usize]);
            code = self.prefix[code as usize];
        }
        if code > 255 {
            return Err(pdf_io("PDF LZW stream is malformed"));
        }
        out.push(code as u8);
        out[start..].reverse();
        Ok(())
    }
    fn group(&mut self) -> std::io::Result<()> {
        self.pending.clear();
        self.at = 0;
        match self.kind {
            0 => {
                // ASCIIHex, including an odd final nibble.
                let b = self.nonspace()?;
                if b == b'>' {
                    self.done = true;
                    return Ok(());
                }
                let high = (b as char)
                    .to_digit(16)
                    .ok_or_else(|| pdf_io("PDF ASCIIHex stream is malformed"))?;
                let b = self.nonspace()?;
                let low = if b == b'>' {
                    self.done = true;
                    0
                } else {
                    (b as char)
                        .to_digit(16)
                        .ok_or_else(|| pdf_io("PDF ASCIIHex stream is malformed"))?
                };
                self.pending.push((high * 16 + low) as u8);
            }
            1 => {
                // ASCII85, z abbreviation and padded final groups.
                let mut value = 0u64;
                let mut count = 0;
                while count < 5 {
                    let b = self.nonspace()?;
                    if b == b'~' {
                        if self.byte()? != b'>' || count == 1 {
                            return Err(pdf_io("PDF ASCII85 stream is malformed"));
                        }
                        self.done = true;
                        break;
                    }
                    if b == b'z' && count == 0 {
                        self.pending.extend_from_slice(&[0; 4]);
                        return Ok(());
                    }
                    if !(b'!'..=b'u').contains(&b) {
                        return Err(pdf_io("PDF ASCII85 stream is malformed"));
                    }
                    value = value * 85 + (b - b'!') as u64;
                    count += 1;
                }
                if count > 0 {
                    for _ in count..5 {
                        value = value * 85 + 84;
                    }
                    let value = u32::try_from(value)
                        .map_err(|_| pdf_io("PDF ASCII85 stream is malformed"))?;
                    self.pending.extend_from_slice(
                        &value.to_be_bytes()[..if count == 5 { 4 } else { count - 1 }],
                    );
                }
            }
            2 => {
                let control = self.byte()?;
                match control {
                    128 => self.done = true,
                    0..=127 => {
                        self.pending.resize(control as usize + 1, 0);
                        self.inner.read_exact(&mut self.pending)?;
                    }
                    _ => {
                        let byte = self.byte()?;
                        self.pending.resize(257 - control as usize, byte);
                    }
                }
            }
            _ => {
                while self.bit_count < self.width {
                    self.bits = (self.bits << 8) | self.byte()? as u32;
                    self.bit_count += 8;
                }
                self.bit_count -= self.width;
                let code = ((self.bits >> self.bit_count) & ((1 << self.width) - 1)) as u16;
                self.bits &= (1 << self.bit_count) - 1;
                match code {
                    256 => {
                        self.previous = None;
                        self.next = 258;
                        self.width = 9;
                    }
                    257 => self.done = true,
                    _ => {
                        let mut sequence = std::mem::take(&mut self.pending);
                        sequence.clear();
                        if code as usize == self.next {
                            let previous = self
                                .previous
                                .ok_or_else(|| pdf_io("PDF LZW stream is malformed"))?;
                            self.sequence(previous, &mut sequence)?;
                            sequence.push(sequence[0]);
                        } else {
                            self.sequence(code, &mut sequence)?;
                        }
                        if let Some(previous) = self.previous {
                            if self.next < 4096 {
                                self.prefix[self.next] = previous;
                                self.suffix[self.next] = sequence[0];
                                self.next += 1;
                                if self.width < 12 && self.next + self.early == 1 << self.width {
                                    self.width += 1;
                                }
                            }
                        }
                        self.previous = Some(code);
                        self.pending = sequence;
                    }
                }
            }
        }
        Ok(())
    }
}
impl Read for PdfCodes<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let mut written = 0;
        while written < out.len() {
            while self.at == self.pending.len() && !self.done {
                self.group()?;
            }
            let n = (out.len() - written).min(self.pending.len() - self.at);
            if n == 0 {
                break;
            }
            out[written..written + n].copy_from_slice(&self.pending[self.at..self.at + n]);
            self.at += n;
            written += n;
        }
        Ok(written)
    }
}

struct PdfPredictor<'a> {
    inner: Box<dyn Read + 'a>,
    row: Vec<u8>,
    previous: Vec<u8>,
    colors: usize,
    png: bool,
    at: usize,
}
impl Read for PdfPredictor<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.at == self.row.len() {
            let mut first = [0];
            if self.inner.read(&mut first)? == 0 {
                return Ok(0);
            }
            let filter = if self.png {
                self.inner.read_exact(&mut self.row)?;
                first[0]
            } else {
                self.row[0] = first[0];
                self.inner.read_exact(&mut self.row[1..])?;
                1
            };
            if filter > 4 {
                return Err(pdf_io("Invalid PNG predictor data"));
            }
            for i in 0..self.row.len() {
                let left = if i >= self.colors {
                    self.row[i - self.colors]
                } else {
                    0
                };
                let up = self.previous[i];
                let upper_left = if i >= self.colors {
                    self.previous[i - self.colors]
                } else {
                    0
                };
                let prediction = match filter {
                    0 => 0,
                    1 => left,
                    2 => up,
                    3 => ((left as u16 + up as u16) / 2) as u8,
                    _ => {
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
                };
                self.row[i] = self.row[i].wrapping_add(prediction);
            }
            self.previous.copy_from_slice(&self.row);
            self.at = 0;
        }
        let n = out.len().min(self.row.len() - self.at);
        out[..n].copy_from_slice(&self.row[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}
fn pdf_predictor<'a>(
    inner: Box<dyn Read + 'a>,
    params: Option<&PdfValue>,
) -> Result<Box<dyn Read + 'a>, &'static str> {
    let dict = match params {
        None => return Ok(inner),
        Some(PdfValue::Atom(n)) if n == b"null" => return Ok(inner),
        Some(PdfValue::Dict(d)) => d,
        _ => return Err("PDF decode parameters cannot be inspected"),
    };
    let predictor = pdf_integer(pdf_field(dict, b"Predictor"), 1)?;
    if predictor == 1 {
        return Ok(inner);
    }
    let colors = pdf_integer(pdf_field(dict, b"Colors"), 1)?;
    let columns = pdf_integer(pdf_field(dict, b"Columns"), 1)?;
    if pdf_integer(pdf_field(dict, b"BitsPerComponent"), 8)? != 8 || colors == 0 || columns == 0 {
        return Err("PDF predictor layout cannot be inspected");
    }
    let row = colors
        .checked_mul(columns)
        .filter(|n| *n <= 1024 * 1024)
        .ok_or("PDF predictor row exceeds limits")?;
    if predictor != 2 && !(10..=15).contains(&predictor) {
        return Err("PDF predictor cannot be inspected");
    }
    Ok(Box::new(PdfPredictor {
        inner,
        row: vec![0; row],
        previous: vec![0; row],
        colors,
        png: predictor != 2,
        at: row,
    }))
}

// A two-byte escape overlap and bounded lexical state span chunk boundaries.
// Long binary atoms/names never grow the scan's memory or manufacture a boundary.
#[derive(Default)]
struct PdfNameScan {
    escape: Vec<u8>,
    atom: Vec<u8>,
    atom_long: bool,
    numeric: bool,
    name: Option<(Vec<u8>, bool, bool)>,
    chain: Option<bool>,
    risky: bool,
    actions: Vec<(Vec<u8>, Vec<u8>)>,
    too_many_actions: bool,
    open_tail: usize,
    capture_actions: bool,
    xml_names: bool,
}
impl PdfNameScan {
    fn decoded(&mut self, b: u8) {
        for (_, tail) in &mut self.actions[self.open_tail..] {
            tail.push(b);
        }
        // Captures are ordered by age, so full tails always form a prefix.
        while self.actions.get(self.open_tail)
            .is_some_and(|(_, tail)| tail.len() == 4096)
        {
            self.open_tail += 1;
        }
        if let Some((name, boundary, long)) = self.name.as_mut() {
            if !pdf_delimiter(b) {
                if name.len() < 32 {
                    name.push(b);
                } else {
                    *long = true;
                }
                return;
            }
            if *boundary
                && !*long
                && pdf_active_name(name)
                && !(self.xml_names && b == b'/' && name == b"JS")
            {
                self.risky = true;
            }
            // Retain only a small lookahead for action-shaped values in payloads.
            // Resolve these after the entire file's object table has been read.
            if self.capture_actions
                && *boundary
                && !*long
                && (name == b"S" || ((name == b"AA" || name == b"OpenAction") && b != b'/'))
            {
                if self.actions.len() < 64 {
                    self.actions.push((name.clone(), vec![b]));
                } else {
                    self.too_many_actions = true;
                }
            }
            self.chain = Some(*boundary);
            self.name = None;
        }
        if b == b'/' {
            let boundary = self.chain.unwrap_or_else(|| {
                self.atom.is_empty()
                    || (!self.atom_long
                        && [
                            b"true".as_slice(),
                            b"false",
                            b"null",
                            b"R",
                            b"obj",
                            b"endobj",
                        ]
                        .contains(&self.atom.as_slice()))
                    || (self.numeric && self.atom.iter().any(u8::is_ascii_digit))
            });
            self.name = Some((Vec::with_capacity(32), boundary, false));
        } else {
            self.chain = None;
            if pdf_delimiter(b) {
                self.atom.clear();
                self.atom_long = false;
                self.numeric = true;
            } else {
                self.numeric &= b.is_ascii_digit() || b"+-.".contains(&b);
                if self.atom.len() < 16 {
                    self.atom.push(b);
                } else {
                    self.atom_long = true;
                }
            }
        }
    }
    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.risky || self.too_many_actions {
                break;
            }
            if !self.escape.is_empty() {
                self.escape.push(b);
                if self.escape.len() == 3 {
                    let a = (self.escape[1] as char).to_digit(16);
                    let c = (self.escape[2] as char).to_digit(16);
                    if let (Some(a), Some(c)) = (a, c) {
                        self.decoded((a * 16 + c) as u8);
                    } else {
                        for i in 0..3 {
                            self.decoded(self.escape[i]);
                        }
                    }
                    self.escape.clear();
                }
            } else if b == b'#' {
                self.escape.push(b);
            } else {
                self.decoded(b);
            }
        }
    }
    fn finish(&mut self) {
        for i in 0..self.escape.len() {
            self.decoded(self.escape[i]);
        }
        self.escape.clear();
        self.decoded(b' ');
    }
}

// Separate from decoded-byte limits: every traversal node and reference hop shares
// this file-wide budget. Completed actions are memoised by reference and context;
// active references remain distinct so cycles still fail closed.
const PDF_ACTION_WORK: usize = 100_000;
struct PdfActionWork {
    left: usize,
    complete: std::collections::BTreeSet<(u32, u32, bool)>,
    active: std::collections::BTreeSet<(u32, u32, bool)>,
}
impl PdfActionWork {
    fn step(&mut self) -> Result<(), &'static str> {
        self.left = self.left.checked_sub(1)
            .ok_or("PDF action/reference work budget exceeded")?;
        Ok(())
    }
}

struct PdfCheck {
    objects: PdfObjects,
    roots: Vec<PdfValue>,
    work: std::rc::Rc<std::cell::Cell<usize>>,
    allocated: usize,
}
impl PdfCheck {
    fn parse(&mut self, bytes: &[u8], depth: usize) -> Result<(), &'static str> {
        if depth > 8 {
            return Err("PDF compressed nesting limit exceeded");
        }
        let mut tokens = PdfTokens::new(bytes);
        tokens.allocated = self.allocated;
        let mut previous = None;
        let mut numbers = Vec::new();
        let mut object_id = None;
        loop {
            tokens.space();
            if tokens.at == bytes.len() {
                break;
            }
            let start_value = tokens.at;
            tokens.allocated = 0;
            let value = tokens.value(0)?;
            if tokens.risky || pdf_risky(&bytes[start_value..tokens.at]) {
                return Err("PDF contains active content");
            }
            if matches!(&value, PdfValue::Atom(w) if w == b"obj") {
                if numbers.len() != 2 {
                    return Err("Invalid PDF object header");
                }
                object_id = Some((numbers[0], numbers[1]));
                previous = None;
                numbers.clear();
                continue;
            }
            if matches!(&value, PdfValue::Atom(w) if w == b"endobj") {
                if let Some(v) = previous.take() {
                    self.retain(object_id.take(), v)?;
                }
                numbers.clear();
                continue;
            }
            if matches!(&value, PdfValue::Atom(w) if w == b"stream") {
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
                let end = match pdf_field(&dict, b"Length") {
                    Some(PdfValue::Atom(_)) => start
                        .checked_add(pdf_integer(pdf_field(&dict, b"Length"), 0)?)
                        .filter(|n| *n <= bytes.len())
                        .ok_or("Invalid PDF stream length")?,
                    _ => {
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
                        start + offset
                    }
                };
                tokens.at = end;
                tokens.space();
                if !matches!(tokens.value(0)?, PdfValue::Atom(w) if w == b"endstream") {
                    return Err("PDF stream terminator missing");
                }
                self.stream(&bytes[start..end], &dict, depth)?;
                self.retain(object_id.take(), PdfValue::Dict(dict))?;
                numbers.clear();
            } else {
                if let PdfValue::Atom(w) = &value {
                    if let Ok(n) = std::str::from_utf8(w).unwrap_or("").parse::<u32>() {
                        if numbers.len() == 2 {
                            numbers.remove(0);
                        }
                        numbers.push(n);
                    } else {
                        numbers.clear();
                    }
                } else {
                    numbers.clear();
                }
                if let Some(v) = previous.replace(value) {
                    self.retain(None, v)?;
                }
            }
        }
        if let Some(v) = previous {
            self.retain(object_id, v)?;
        }
        Ok(())
    }
    fn retain(&mut self, id: Option<(u32, u32)>, value: PdfValue) -> Result<(), &'static str> {
        fn heap_size(value: &PdfValue) -> usize {
            match value {
                PdfValue::Name(n) | PdfValue::Atom(n) | PdfValue::String(n) => n.capacity(),
                PdfValue::Dict(d) => {
                    d.capacity() * std::mem::size_of::<(Vec<u8>, PdfValue)>()
                        + d.iter()
                            .map(|(k, v)| k.capacity() + heap_size(v))
                            .sum::<usize>()
                }
                PdfValue::Array(a) | PdfValue::LimitedArray(a) => {
                    a.capacity() * std::mem::size_of::<PdfValue>()
                        + a.iter().map(heap_size).sum::<usize>()
                }
                _ => 0,
            }
        }
        if id.is_some()
            || matches!(
                value,
                PdfValue::Dict(_) | PdfValue::Array(_) | PdfValue::LimitedArray(_)
            )
        {
            self.allocated = self
                .allocated
                .checked_add(std::mem::size_of::<PdfValue>() + 64 + heap_size(&value))
                .filter(|n| *n <= PDF_OBJECT_CAP)
                .ok_or("PDF object table exceeds memory limit")?;
            if let Some(id) = id {
                // Inspect superseded revisions too: a reader may select another xref.
                if let Some(old) = self.objects.insert(id, value) {
                    self.roots.push(old);
                }
            } else {
                self.roots.push(value);
            }
        }
        Ok(())
    }
    fn stream(
        &mut self,
        bytes: &[u8],
        dict: &[(Vec<u8>, PdfValue)],
        depth: usize,
    ) -> Result<(), &'static str> {
        let filters = match pdf_field(dict, b"Filter") {
            None => Vec::new(),
            Some(PdfValue::Name(n)) => vec![n.as_slice()],
            Some(PdfValue::Array(a)) => a
                .iter()
                .map(|v| match v {
                    PdfValue::Name(n) => Ok(n.as_slice()),
                    _ => Err("Invalid PDF stream filter"),
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err("Invalid PDF stream filter"),
        };
        if filters.len() > 8 {
            return Err("PDF filter chain exceeds limits");
        }
        if matches!(
            pdf_field(dict, b"Type"),
            Some(PdfValue::Ref(_, _) | PdfValue::Scalar)
        ) {
            return Err("PDF stream type cannot be inspected");
        }
        let object = pdf_name_is(pdf_field(dict, b"Type"), b"ObjStm")
            || (pdf_field(dict, b"N").is_some() && pdf_field(dict, b"First").is_some());
        let image = !object
            && pdf_name_is(pdf_field(dict, b"Subtype"), b"Image")
            && (pdf_field(dict, b"Type").is_none()
                || pdf_name_is(pdf_field(dict, b"Type"), b"XObject"));
        let params = pdf_field(dict, b"DecodeParms");
        if filters.len() > 1 && matches!(params, Some(PdfValue::Dict(_))) {
            return Err("PDF decode parameters do not match filters");
        }
        if let Some(PdfValue::Array(a)) = params {
            if a.len() != filters.len() {
                return Err("PDF decode parameters do not match filters");
            }
        }
        // Image codecs are opaque only on image XObjects, never on arbitrary streams.
        if image
            && filters.iter().any(|f| {
                [
                    b"ASCIIHexDecode".as_slice(),
                    b"AHx",
                    b"ASCII85Decode",
                    b"A85",
                    b"RunLengthDecode",
                    b"RL",
                    b"DCTDecode",
                    b"DCT",
                    b"JPXDecode",
                    b"CCITTFaxDecode",
                    b"CCF",
                    b"JBIG2Decode",
                ]
                .contains(f)
            })
        {
            return Ok(());
        }
        let mut reader: Box<dyn Read + '_> = Box::new(bytes);
        for (i, filter) in filters.iter().enumerate() {
            let param = match params {
                Some(PdfValue::Array(a)) => a.get(i),
                other => other,
            };
            reader = match *filter {
                b"FlateDecode" | b"Fl" => Box::new(PdfFlate {
                    inner: reader,
                    state: miniz_oxide::inflate::stream::InflateState::new_boxed(
                        miniz_oxide::DataFormat::Zlib,
                    ),
                    input: [0; PDF_CHUNK],
                    at: 0,
                    len: 0,
                    done: false,
                }),
                b"ASCIIHexDecode" | b"AHx" => Box::new(PdfCodes::new(reader, 0, 1)),
                b"ASCII85Decode" | b"A85" => Box::new(PdfCodes::new(reader, 1, 1)),
                b"RunLengthDecode" | b"RL" => Box::new(PdfCodes::new(reader, 2, 1)),
                b"LZWDecode" | b"LZW" => {
                    let early = match param {
                        Some(PdfValue::Dict(d)) => pdf_integer(pdf_field(d, b"EarlyChange"), 1)?,
                        None => 1,
                        Some(PdfValue::Atom(n)) if n == b"null" => 1,
                        _ => return Err("PDF decode parameters cannot be inspected"),
                    };
                    if early > 1 {
                        return Err("Invalid PDF LZW EarlyChange");
                    }
                    Box::new(PdfCodes::new(reader, 3, early))
                }
                _ => return Err("PDF non-image stream has an unsupported filter"),
            };
            reader = Box::new(PdfBudget {
                inner: reader,
                left: self.work.clone(),
            });
            if !image {
                reader = pdf_predictor(reader, param)?;
            }
        }
        if filters.is_empty() {
            reader = Box::new(PdfBudget {
                inner: reader,
                left: self.work.clone(),
            });
            if !image {
                reader = pdf_predictor(reader, params)?;
            }
        }
        let mut chunk = [0; PDF_CHUNK];
        let mut decoded = Vec::new();
        let mut scan = PdfNameScan {
            numeric: true,
            capture_actions: true,
            xml_names: pdf_name_is(pdf_field(dict, b"Type"), b"Metadata")
                && pdf_name_is(pdf_field(dict, b"Subtype"), b"XML"),
            ..Default::default()
        };
        loop {
            let n = reader.read(&mut chunk).map_err(pdf_read_error)?;
            if n == 0 {
                break;
            }
            if object {
                if decoded.len() + n > PDF_OBJECT_CAP {
                    return Err("PDF object stream exceeds memory limit");
                }
                decoded.extend_from_slice(&chunk[..n]);
            } else {
                scan.feed(&chunk[..n]);
                if scan.risky {
                    return Err("PDF contains active content");
                }
                if scan.too_many_actions {
                    return Err("PDF payload actions exceed inspection limit");
                }
            }
        }
        if object {
            if pdf_risky(&decoded) {
                return Err("PDF contains active content");
            }
            self.object_stream(&decoded, dict, depth + 1)?;
        } else {
            scan.finish();
            if scan.risky {
                return Err("PDF contains active content");
            }
            if scan.too_many_actions {
                return Err("PDF payload actions exceed inspection limit");
            }
            for (key, tail) in scan.actions {
                let mut tokens = PdfTokens::new(&tail);
                let value = match tokens.value(0) {
                    Ok(value) => value,
                    Err(_) if key == b"S" => continue, // ordinary graphics names/operators
                    Err(_) => return Err("PDF payload action cannot be inspected"),
                };
                // /S occurs in ordinary graphics syntax too; only action subtypes matter.
                if key == b"S"
                    && !matches!(&value, PdfValue::Name(n) if [b"JavaScript".as_slice(),b"Launch",b"SubmitForm",b"ImportData",b"Rendition",b"GoToE",b"GoToR"].contains(&n.as_slice()))
                {
                    continue;
                }
                self.retain(None, PdfValue::Dict(vec![(key, value)]))?;
            }
        }
        Ok(())
    }
    fn object_stream(
        &mut self,
        bytes: &[u8],
        dict: &[(Vec<u8>, PdfValue)],
        depth: usize,
    ) -> Result<(), &'static str> {
        if depth > 8 {
            return Err("PDF compressed nesting limit exceeded");
        }
        if pdf_field(dict, b"N").is_none() || pdf_field(dict, b"First").is_none() {
            return Err("Invalid PDF object stream header");
        }
        let count = pdf_integer(pdf_field(dict, b"N"), 0)?;
        let first = pdf_integer(pdf_field(dict, b"First"), 0)?;
        if count > 100_000 || first > bytes.len() {
            return Err("Invalid PDF object stream header");
        }
        let mut header = PdfTokens::new(&bytes[..first]);
        let mut entries = Vec::new();
        for _ in 0..count {
            let id = pdf_integer(Some(&header.value(0)?), 0)?;
            let offset = pdf_integer(Some(&header.value(0)?), 0)?;
            let id = u32::try_from(id).map_err(|_| "Invalid PDF object stream header")?;
            let start = first
                .checked_add(offset)
                .filter(|s| *s < bytes.len())
                .ok_or("Invalid PDF object stream offset")?;
            if entries.last().is_some_and(|&(_, s)| s >= start) {
                return Err("Invalid PDF object stream offset");
            }
            entries.push((id, start));
        }
        header.space();
        if header.at != first {
            return Err("Invalid PDF object stream header");
        }
        for (i, &(id, start)) in entries.iter().enumerate() {
            let end = entries.get(i + 1).map_or(bytes.len(), |&(_, s)| s);
            let mut tokens = PdfTokens::new(&bytes[start..end]);
            let value = tokens.value(0)?;
            if tokens.risky || pdf_risky(&bytes[start..start + tokens.at]) {
                return Err("PDF contains active content");
            }
            tokens.space();
            if tokens.at != end - start {
                return Err("Invalid PDF object stream content");
            }
            self.retain(Some((id, 0)), value)?;
        }
        Ok(())
    }
    fn resolve<'a>(
        &'a self,
        mut value: &'a PdfValue,
        work: &mut PdfActionWork,
    ) -> Result<&'a PdfValue, &'static str> {
        for _ in 0..64 {
            if let PdfValue::Ref(id, generation) = value {
                work.step()?;
                value = self
                    .objects
                    .get(&(*id, *generation))
                    .ok_or("PDF action reference cannot be resolved")?;
            } else {
                return Ok(value);
            }
        }
        Err("PDF action reference cycle or depth limit")
    }
    fn action(
        &self,
        value: &PdfValue,
        additional: bool,
        depth: usize,
        work: &mut PdfActionWork,
    ) -> Result<(), &'static str> {
        if depth > 64 {
            return Err("PDF action reference cycle or depth limit");
        }
        work.step()?;
        if let PdfValue::Ref(id, generation) = value {
            let key = (*id, *generation, additional);
            if work.complete.contains(&key) {
                return Ok(());
            }
            if !work.active.insert(key) {
                return Err("PDF action reference cycle or depth limit");
            }
            let target = self.resolve(value, work)?;
            self.action(target, additional, depth, work)?;
            work.active.remove(&key);
            work.complete.insert(key);
            return Ok(());
        }
        match value {
            PdfValue::Array(_) | PdfValue::String(_) | PdfValue::Name(_) if !additional => Ok(()), // destinations
            PdfValue::Atom(n) if n == b"null" => Ok(()),
            PdfValue::Dict(dict) if additional => {
                for (_, v) in dict {
                    self.action(v, false, depth + 1, work)?;
                }
                Ok(())
            }
            PdfValue::Dict(dict) => {
                let s = pdf_field(dict, b"S").ok_or("PDF action subtype cannot be inspected")?;
                let PdfValue::Name(s) = self.resolve(s, work)? else {
                    return Err("PDF action subtype cannot be inspected");
                };
                if [
                    b"JavaScript".as_slice(),
                    b"Launch",
                    b"SubmitForm",
                    b"ImportData",
                    b"Rendition",
                    b"GoToE",
                ]
                .contains(&s.as_slice())
                {
                    return Err("PDF contains risky action");
                }
                if s == b"GoToR" {
                    let file = pdf_field(dict, b"F")
                        .ok_or("PDF remote action file cannot be inspected")?;
                    self.remote_file(file, work)?;
                }
                if let Some(next) = pdf_field(dict, b"Next") {
                    match self.resolve(next, work)? {
                        PdfValue::Array(values) => {
                            for v in values {
                                self.action(v, false, depth + 1, work)?;
                            }
                        }
                        _ => self.action(next, false, depth + 1, work)?,
                    }
                }
                if ![
                    b"GoTo".as_slice(),
                    b"GoToR",
                    b"URI",
                    b"Named",
                    b"Thread",
                    b"ResetForm",
                    b"Hide",
                    b"SetOCGState",
                    b"Trans",
                    b"Sound",
                    b"Movie",
                ]
                .contains(&s.as_slice())
                {
                    return Err("PDF action subtype cannot be inspected");
                }
                Ok(())
            }
            _ => Err("PDF action cannot be inspected"),
        }
    }
    fn remote_file(&self, file: &PdfValue, work: &mut PdfActionWork) -> Result<(), &'static str> {
        let file = self.resolve(file, work)?;
        let names: Vec<&PdfValue> = match file {
            PdfValue::String(_) => vec![file],
            PdfValue::Dict(d) => [b"F".as_slice(), b"UF", b"DOS", b"Mac", b"Unix"]
                .iter()
                .filter_map(|k| pdf_field(d, k))
                .collect(),
            _ => return Err("PDF remote action file cannot be inspected"),
        };
        if names.is_empty() {
            return Err("PDF remote action file cannot be inspected");
        }
        for name in names {
            let PdfValue::String(name) = self.resolve(name, work)? else {
                return Err("PDF remote action file cannot be inspected");
            };
            let mut normalized = Vec::with_capacity(name.len());
            let mut at = 0;
            while at < name.len() {
                if name[at] == b'%' && at + 2 < name.len() {
                    if let (Some(a), Some(b)) = (
                        (name[at + 1] as char).to_digit(16),
                        (name[at + 2] as char).to_digit(16),
                    ) {
                        normalized.push((a * 16 + b) as u8);
                        at += 3;
                        continue;
                    }
                }
                normalized.push(name[at]);
                at += 1;
            }
            let name = String::from_utf8_lossy(&normalized)
                .to_ascii_lowercase()
                .replace('\0', "");
            let path = name.split(['?', '#']).next().unwrap_or("");
            let basename = path.rsplit(['/', '\\']).next().unwrap_or("");
            let basename = basename
                .split(':')
                .next()
                .unwrap_or("")
                .trim_matches([' ', '.', '"']);
            let extension = basename.rsplit('.').next().unwrap_or("");
            if [
                "exe",
                "com",
                "bat",
                "cmd",
                "msi",
                "msp",
                "scr",
                "pif",
                "ps1",
                "psm1",
                "vbs",
                "vbe",
                "js",
                "jse",
                "wsf",
                "wsh",
                "hta",
                "cpl",
                "dll",
                "lnk",
                "app",
                "sh",
                "bash",
                "command",
                "jar",
                "py",
                "pyw",
                "pl",
                "rb",
                "desktop",
                "url",
                "scf",
                "reg",
                "gadget",
                "application",
            ]
            .contains(&extension)
            {
                return Err("PDF contains executable remote action");
            }
        }
        Ok(())
    }
    fn actions(
        &self,
        value: &PdfValue,
        depth: usize,
        work: &mut PdfActionWork,
    ) -> Result<(), &'static str> {
        if depth > 64 {
            return Err("PDF action nesting limit exceeded");
        }
        work.step()?;
        match value {
            PdfValue::Dict(d) => {
                if pdf_field(d, b"EF").is_some() {
                    return Err("PDF contains attachments");
                }
                if let Some(subtype) = pdf_field(d, b"Subtype") {
                    if pdf_name_is(Some(self.resolve(subtype, work)?), b"FileAttachment") {
                        return Err("PDF contains attachments");
                    }
                }
                if pdf_field(d, b"S").is_some() {
                    // /S is also used by non-action dictionaries (e.g. transparency).
                    let s = self.resolve(pdf_field(d, b"S").unwrap(), work)?;
                    if let PdfValue::Name(n) = s {
                        if [
                            b"JavaScript".as_slice(),
                            b"Launch",
                            b"SubmitForm",
                            b"ImportData",
                            b"Rendition",
                            b"GoToE",
                            b"GoToR",
                            b"GoTo",
                            b"URI",
                            b"Named",
                            b"Thread",
                            b"ResetForm",
                            b"Hide",
                            b"SetOCGState",
                            b"Trans",
                            b"Sound",
                            b"Movie",
                        ]
                        .contains(&n.as_slice())
                        {
                            self.action(value, false, depth + 1, work)?;
                        }
                    }
                }
                let annotation = pdf_name_is(pdf_field(d, b"Type"), b"Annot")
                    || matches!(pdf_field(d, b"Subtype"), Some(PdfValue::Name(n)) if [
                        b"Text".as_slice(), b"Link", b"FreeText", b"Line", b"Square", b"Circle",
                        b"Polygon", b"PolyLine", b"Highlight", b"Underline", b"Squiggly", b"StrikeOut",
                        b"Stamp", b"Caret", b"Ink", b"Popup", b"FileAttachment", b"Sound", b"Movie",
                        b"Widget", b"Screen", b"PrinterMark", b"TrapNet", b"Watermark", b"3D", b"Redact",
                    ].contains(&n.as_slice()));
                let outline = pdf_field(d, b"Title").is_some();
                let mut seen_action = false;
                for (key, v) in d {
                    if key == b"A" && (annotation || outline) {
                        if seen_action {
                            return Err("Duplicate PDF dictionary key");
                        }
                        seen_action = true;
                    }
                    if key == b"OpenAction" || (key == b"A" && (annotation || outline)) {
                        self.action(v, false, depth + 1, work)?;
                    } else if key == b"AA" {
                        self.action(v, true, depth + 1, work)?;
                    }
                    self.actions(v, depth + 1, work)?;
                }
            }
            PdfValue::Array(a) | PdfValue::LimitedArray(a) => {
                for v in a {
                    self.actions(v, depth + 1, work)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
fn pdf_note_with_limit(bytes: &[u8], upload_limit: usize) -> Option<String> {
    let mut check = PdfCheck {
        objects: PdfObjects::new(),
        roots: Vec::new(),
        work: std::rc::Rc::new(std::cell::Cell::new(upload_limit.saturating_mul(64))),
        allocated: 0,
    };
    let result = check.parse(bytes, 0).and_then(|()| {
        let mut work = PdfActionWork {
            left: PDF_ACTION_WORK,
            complete: std::collections::BTreeSet::new(),
            active: std::collections::BTreeSet::new(),
        };
        for value in check.objects.values().chain(check.roots.iter()) {
            check.actions(value, 0, &mut work)?;
        }
        Ok(())
    });
    result.err().map(str::to_string)
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
#[cfg(test)]
fn inspect(bytes: &[u8]) -> AppResult<(Kind, Option<String>)> {
    inspect_with_limit(bytes, 15 * 1024 * 1024)
}
fn inspect_with_limit(bytes: &[u8], upload_limit: usize) -> AppResult<(Kind, Option<String>)> {
    let unsupported = || {
        AppError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_type",
            "Only PDF, DOCX, JPEG and PNG files are accepted.",
        )
    };
    if bytes.starts_with(b"%PDF-") {
        return Ok((Kind::Pdf, pdf_note_with_limit(bytes, upload_limit)));
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

// Per-database debug hook lets integration tests check the actual inspection boundary.
#[cfg(debug_assertions)]
type InspectionHooks = std::collections::BTreeMap<PathBuf, fn(&Db)>;
#[cfg(debug_assertions)]
static INSPECTION_HOOKS: std::sync::LazyLock<parking_lot::Mutex<InspectionHooks>> =
    std::sync::LazyLock::new(|| parking_lot::Mutex::new(InspectionHooks::new()));

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn set_inspection_hook(db: &Db, hook: Option<fn(&Db)>) {
    let mut hooks = INSPECTION_HOOKS.lock();
    if let Some(hook) = hook {
        hooks.insert(db.path().to_path_buf(), hook);
    } else {
        hooks.remove(db.path());
    }
}

/// A format/scanner verdict for immutable bytes, obtained before taking a DB writer lock.
pub(crate) struct UploadInspection {
    content_type: String,
    filename: String,
    scan_status: &'static str,
    scan_note: Option<String>,
}

pub(crate) fn inspect_upload(
    db: &Db,
    bytes: &[u8],
    filename: &str,
    max_bytes: u64,
) -> AppResult<UploadInspection> {
    inspect_upload_inner(db, bytes, filename, max_bytes, false)
}

fn inspect_upload_inner(
    db: &Db,
    bytes: &[u8],
    filename: &str,
    max_bytes: u64,
    scan_now: bool,
) -> AppResult<UploadInspection> {
    if bytes.is_empty() {
        return Err(AppError::validation("The file is empty."));
    }
    if bytes.len() as u64 > max_bytes {
        return Err(AppError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "The file is too large.",
        ));
    }
    #[cfg(debug_assertions)]
    {
        let hook = INSPECTION_HOOKS.lock().get(db.path()).copied();
        if let Some(hook) = hook {
            hook(db);
        }
    }
    let (kind, note) = inspect_with_limit(bytes, usize::try_from(max_bytes).unwrap_or(usize::MAX))?;
    let filename = sanitize_filename(filename);
    let ext = filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !kind.extensions().contains(&ext.as_str()) {
        return Err(AppError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_type",
            format!(
                "The file content is {} but the name ends with '.{ext}'.",
                kind.mime()
            ),
        ));
    }
    let (scan_status, scan_note) = if let Some(note) = note {
        ("quarantined", Some(note))
    } else if !scan_now
        && db
            .config()
            .is_some_and(|c| c.mode == crate::config::Mode::Production && c.clamd.is_some())
    {
        (
            "pending_scan",
            Some("Awaiting ClamAV verdict; file cannot be opened".into()),
        )
    } else {
        match crate::scan::verdict(db.config(), bytes) {
            Ok(note) => ("clean", note),
            Err(note) => ("quarantined", Some(note)),
        }
    };
    Ok(UploadInspection {
        content_type: kind.mime().to_string(),
        filename,
        scan_status,
        scan_note,
    })
}

fn store_inner(
    db: &Db,
    bytes: &[u8],
    filename: &str,
    max_bytes: u64,
    scan_now: bool,
) -> AppResult<StoredFile> {
    let inspection = inspect_upload_inner(db, bytes, filename, max_bytes, scan_now)?;
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
    store_inspected_upload(db, bytes, inspection)
}

// The caller must use the same immutable bytes and check quota inside its transaction.
pub(crate) fn store_inspected_upload(
    db: &Db,
    bytes: &[u8],
    inspection: UploadInspection,
) -> AppResult<StoredFile> {
    let raw = write_blob(db, bytes)?;
    Ok(StoredFile {
        storage_key: raw.0,
        sha256: raw.1,
        size_bytes: bytes.len() as i64,
        content_type: inspection.content_type,
        filename: inspection.filename,
        scan_status: inspection.scan_status,
        scan_note: inspection.scan_note,
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
