//! Fictional DEMO cases (spec §12) and a tiny PDF generator for DEMO-marked sample files.
use crate::db::Db;
use crate::error::AppResult;
use rusqlite::Transaction;

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
    let mut content = String::from("BT /F1 22 Tf 1 0 0 rg 72 770 Td (DEMO - FICTIONAL - NOT A COURT RECORD) Tj ET\n");
    content.push_str(&format!("BT /F1 16 Tf 0 0 0 rg 72 730 Td ({}) Tj ET\n", esc(title)));
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
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    out
}

pub fn seed_cases(_tx: &Transaction, _db: &Db) -> AppResult<()> {
    Ok(())
}
