//! R04: an import batch must not disclose restricted material it carried.
//! The first test is the reviewer's proposal, unchanged except for the HEAVY lock (the import
//! endpoints share one process-wide permit, so tests in this binary run one at a time).
//! Uses the actual router, ZIP import, access checks and only fictional data.
mod common;
use common::*;
use axum::http::{Method, StatusCode};
use rusqlite::params;
use serde_json::{json, Value};
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;

const PRIVATE_TITLE: &str = "DEMO PRIVATE IMPORT TITLE";
const PRIVATE_NAME: &str = "DEMO_PRIVATE_IMPORTED.pdf";
/// Import shares one process-wide heavy-operation permit; serialise the tests that use it.
static HEAVY: std::sync::LazyLock<tokio::sync::Mutex<()>> = std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

fn zip_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in entries {
        w.start_file(*name, SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}
fn hidden_or_redacted(status: StatusCode, body: &Value, phase: &str) {
    assert!(status.is_success() || status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND,
        "{phase}: unexpected {status} {body}");
    let text = body.to_string();
    assert!(!text.contains(PRIVATE_TITLE), "{phase}: hidden title in import response: {text}");
    assert!(!text.contains(PRIVATE_NAME), "{phase}: hidden filename in import response: {text}");
}

#[tokio::test]
async fn r04_import_run_and_case_access_do_not_disclose_someone_elses_hidden_import() {
    let _lock = HEAVY.lock().await;
    let app = TestApp::demo();
    let clerk = app.persona("olga").await;
    let (cid, number) = register_case(&clerk, "DEMO R04 import visibility").await;
    let head = clerk.switch("elena").await;
    let judge_id = user_id(&clerk, "Viktor").await;
    let (s,b) = head.post(&format!("/api/cases/{cid}/assignments"), json!({
        "user_id":judge_id, "role":"judge", "reason":"DEMO R04 assignment"
    })).await;
    ok(s,&b);
    let conn = clerk.db(&app).open().unwrap();
    conn.execute("INSERT OR IGNORE INTO user_permissions(user_id,permission,granted_at) VALUES(?1,'import.run',?2)",
        params![judge_id,tuvalu_court::time::now_utc()]).unwrap();
    let judge = clerk.switch("viktor").await;
    let data = pdf("DEMO synthetic private note, no real court data");
    let manifest = format!("case_number,filename,title,doc_type,visibility,document_date\n{number},{PRIVATE_NAME},{PRIVATE_TITLE},judicial_note,judicial_note,{}\n", today());
    let bytes = zip_entries(&[("manifest.csv",manifest.as_bytes()),(PRIVATE_NAME,&data)]);
    let (s,preview) = judge.upload("/api/import/files/preview",&[],"DEMO_import.zip",&bytes).await;
    ok(s,&preview);
    assert_eq!(preview["summary"]["create"],1,"{preview}");
    let bid=preview["batch_id"].as_i64().unwrap();
    let (ps,pbody) = head.get(&format!("/api/import/{bid}")).await;

    let (s,committed) = judge.post_idem(&format!("/api/import/{bid}/commit"),"DEMO-r04-commit",json!({})).await;
    ok(s,&committed);
    let did=committed["created"][0]["document_id"].as_i64().unwrap();
    let (s,_)=head.get(&format!("/api/documents/{did}")).await;
    assert!(s==StatusCode::NOT_FOUND || s==StatusCode::FORBIDDEN,
        "Fixture must deny the head access to the private document");
    let (cs,cbody) = head.get(&format!("/api/import/{bid}")).await;
    let (s,own)=judge.get(&format!("/api/import/{bid}")).await;
    ok(s,&own);
    assert!(own.to_string().contains(PRIVATE_TITLE),"Author's own import should remain usable");

    hidden_or_redacted(ps,&pbody,"previewed batch");
    hidden_or_redacted(cs,&cbody,"committed batch");
}

// ---------------------------------------------------------------- additional R04 regressions

const OPEN_TITLE: &str = "DEMO ordinary imported letter";
const OPEN_NAME: &str = "DEMO_ordinary_letter.pdf";

/// Viktor (judge, given import.run) previews a package with one ordinary row and one judicial note
/// in a case Elena (head of registry, import.run + case.view_all) can see.
async fn mixed_package(app: &TestApp) -> (Client, Client, i64) {
    let clerk = app.persona("olga").await;
    let (cid, number) = register_case(&clerk, "DEMO R04 mixed package").await;
    let head = clerk.switch("elena").await;
    let judge_id = user_id(&clerk, "Viktor").await;
    let (s, b) = head
        .post(&format!("/api/cases/{cid}/assignments"), json!({"user_id":judge_id,"role":"judge","reason":"DEMO R04"}))
        .await;
    ok(s, &b);
    clerk.db(app).open().unwrap().execute(
        "INSERT OR IGNORE INTO user_permissions(user_id,permission,granted_at) VALUES(?1,'import.run',?2)",
        params![judge_id, tuvalu_court::time::now_utc()],
    ).unwrap();
    let judge = clerk.switch("viktor").await;
    let manifest = format!(
        "case_number,filename,title,doc_type,visibility,document_date\n\
         {number},{OPEN_NAME},{OPEN_TITLE},correspondence,administrative,{d}\n\
         {number},{PRIVATE_NAME},{PRIVATE_TITLE},judicial_note,judicial_note,{d}\n",
        d = today()
    );
    let open = pdf("DEMO ordinary letter");
    let private = pdf("DEMO private judicial note");
    let bytes = zip_entries(&[("manifest.csv", manifest.as_bytes()), (OPEN_NAME, &open), (PRIVATE_NAME, &private)]);
    let (s, preview) = judge.upload("/api/import/files/preview", &[], "DEMO_mixed.zip", &bytes).await;
    ok(s, &preview);
    assert_eq!(preview["summary"]["create"], 2, "{preview}");
    (judge, head, preview["batch_id"].as_i64().unwrap())
}

fn row(detail: &Value, n: i64) -> &Value {
    detail["preview"]["rows"].as_array().unwrap().iter().find(|r| r["row"] == n).unwrap()
}

#[tokio::test]
async fn r04_other_importer_sees_ordinary_rows_but_cannot_commit_a_package_with_restricted_rows() {
    let _lock = HEAVY.lock().await;
    let app = TestApp::demo();
    let (judge, head, bid) = mixed_package(&app).await;

    let (s, list) = head.get("/api/import").await;
    ok(s, &list);
    assert!(list["batches"].as_array().unwrap().iter().any(|b| b["id"] == bid), "{list}");
    hidden_or_redacted(s, &list, "batch list");

    let (s, d) = head.get(&format!("/api/import/{bid}")).await;
    ok(s, &d);
    hidden_or_redacted(s, &d, "previewed mixed batch");
    assert_eq!(row(&d, 2)["title"], OPEN_TITLE, "ordinary rows stay readable: {d}");
    assert_eq!(row(&d, 3)["restricted"], true, "{d}");
    assert_eq!(d["can_commit"], false, "{d}");

    let docs_before: i64 = head.db(&app).open().unwrap().query_row("SELECT COUNT(*) FROM documents", [], |r| r.get(0)).unwrap();
    let (s, b) = head.post_idem(&format!("/api/import/{bid}/commit"), "DEMO-r04-head", json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
    hidden_or_redacted(s, &b, "refused commit");
    let docs_after: i64 = head.db(&app).open().unwrap().query_row("SELECT COUNT(*) FROM documents", [], |r| r.get(0)).unwrap();
    assert_eq!(docs_before, docs_after, "a refused commit must not create documents");

    let (s, own) = judge.get(&format!("/api/import/{bid}")).await;
    ok(s, &own);
    assert_eq!(own["can_commit"], true);
    assert_eq!(row(&own, 3)["title"], PRIVATE_TITLE);
    let path = format!("/api/import/{bid}/commit");
    let (s, committed) = judge.post_idem(&path, "DEMO-r04-owner", json!({})).await;
    ok(s, &committed);
    assert_eq!(committed["summary"]["created"], 2, "{committed}");
    let (s, again) = judge.post_idem(&path, "DEMO-r04-owner", json!({})).await;
    ok(s, &again);
    assert_eq!(again, committed, "owner replay returns the same result and creates nothing");
    let created = committed["created"].as_array().unwrap();
    let private_doc = created.iter().find(|c| c["row"] == 3).unwrap()["document_id"].as_i64().unwrap();
    let open_doc = created.iter().find(|c| c["row"] == 2).unwrap()["document_id"].as_i64().unwrap();

    let (s, d) = head.get(&format!("/api/import/{bid}")).await;
    ok(s, &d);
    hidden_or_redacted(s, &d, "committed mixed batch");
    assert_eq!(row(&d, 2)["title"], OPEN_TITLE);
    let result = d["result"]["created"].as_array().unwrap();
    assert!(result.iter().any(|c| c["document_id"] == open_doc), "{d}");
    assert!(!d.to_string().contains(&format!("\"document_id\":{private_doc}")), "{d}");
    assert_eq!(d["can_commit"], false);
    assert_eq!(head.get(&format!("/api/documents/{open_doc}")).await.0, StatusCode::OK);
    assert_eq!(head.get(&format!("/api/documents/{private_doc}")).await.0, StatusCode::NOT_FOUND);

    // Access granted by the note's author opens exactly that row, and only while the grant lasts.
    let head_id = user_id(&judge, "Elena").await;
    let (s, g) = judge.post(&format!("/api/documents/{private_doc}/grants"), json!({"user_id":head_id,"reason":"DEMO R04 grant"})).await;
    ok(s, &g);
    let (_, d) = head.get(&format!("/api/import/{bid}")).await;
    assert_eq!(row(&d, 3)["title"], PRIVATE_TITLE, "{d}");
    assert!(d["result"]["created"].as_array().unwrap().iter().any(|c| c["document_id"] == private_doc), "{d}");
    let gid = g["id"].as_i64().or_else(|| g["grant"]["id"].as_i64()).unwrap();
    let (s, r) = judge.clone().raw(Method::DELETE, &format!("/api/documents/{private_doc}/grants/{gid}"), Some(json!({"reason":"DEMO R04 revoke"}))).await;
    ok(s, &r);
    let (s, d) = head.get(&format!("/api/import/{bid}")).await;
    hidden_or_redacted(s, &d, "after revocation");
}

#[tokio::test]
async fn r04_ordinary_package_stays_shared_between_importers() {
    let _lock = HEAVY.lock().await;
    let app = TestApp::demo();
    let clerk = app.persona("olga").await;
    let (_, number) = register_case(&clerk, "DEMO R04 ordinary package").await;
    let head = clerk.switch("elena").await;
    let olga_id = user_id(&clerk, "Olga").await;
    clerk.db(&app).open().unwrap().execute(
        "INSERT OR IGNORE INTO user_permissions(user_id,permission,granted_at) VALUES(?1,'import.run',?2)",
        params![olga_id, tuvalu_court::time::now_utc()],
    ).unwrap();
    let clerk = head.switch("olga").await;
    let manifest = format!("case_number,filename,title,doc_type,visibility,document_date\n{number},{OPEN_NAME},{OPEN_TITLE},correspondence,party_material,{}\n", today());
    let bytes = zip_entries(&[("manifest.csv", manifest.as_bytes()), (OPEN_NAME, &pdf("DEMO ordinary"))]);
    let (s, p) = head.upload("/api/import/files/preview", &[], "DEMO_ordinary.zip", &bytes).await;
    ok(s, &p);
    let bid = p["batch_id"].as_i64().unwrap();
    let (s, d) = clerk.get(&format!("/api/import/{bid}")).await;
    ok(s, &d);
    assert_eq!(row(&d, 2)["title"], OPEN_TITLE);
    assert_eq!(d["can_commit"], true);
    let (s, r) = clerk.post_idem(&format!("/api/import/{bid}/commit"), "DEMO-r04-shared", json!({})).await;
    ok(s, &r);
    assert_eq!(r["summary"]["created"], 1, "{r}");
    let (_, d) = head.get(&format!("/api/import/{bid}")).await;
    assert_eq!(d["result"], r, "the uploader sees the committed result");
}
