mod common;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use common::*;
use rusqlite::params;
use serde_json::{Value, json};
use std::io::{Cursor, Read, Write};
use zip::write::SimpleFileOptions;
const HEADER: &str = "number,category,title,registered_date,status,responsible_username,closed_date,closure_basis,parties\n";
fn zip_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, b) in entries {
        z.start_file(
            *name,
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        z.write_all(b).unwrap();
    }
    z.finish().unwrap().into_inner()
}
async fn package(client: &Client, id: i64, body: Value) -> (StatusCode, Value, Vec<u8>) {
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/cases/{id}/export"))
        .header("host", "localhost")
        .header("x-tcr", "1")
        .header(
            "cookie",
            format!(
                "tcr_sandbox={}; tcr_session={}",
                client.sandbox.as_ref().unwrap(),
                client.session.as_ref().unwrap()
            ),
        )
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (s, b, _, raw) = client.clone().send(req).await;
    (s, b, raw)
}
#[tokio::test]
async fn csv_preview_commit_history_idempotency_and_numbering() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (_, existing) = register_case(&olga, "Existing case").await;
    let elena = olga.switch("elena").await;
    let db = olga.db(&app);
    let c = db.open().unwrap();
    let username: String = c
        .query_row("SELECT username FROM users WHERE persona='olga'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let year = &today()[..4];
    let number = format!("DEMO-CIV-{year}-0040");
    let csv = format!(
        "{HEADER}{existing},civil_contract,Existing,2000-01-01,registered,,,,\nDUP,civil_contract,Duplicate,2000-01-01,registered,,,,\nDUP,civil_contract,Duplicate,2000-01-01,registered,,,,\nBAD,civil_contract,Bad date,nonsense,registered,,,,\n{number},civil_contract,Historical,2001-04-03,closed,{username},,,Legacy Person (claimant)\n"
    );
    let reg: i64 = c
        .query_row(
            "SELECT id FROM registries WHERE series='DEMO-CIV'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let regstr = reg.to_string();
    let (s, preview) = elena
        .upload(
            "/api/import/cases/preview",
            &[("registry_id", &regstr)],
            "legacy.csv",
            csv.as_bytes(),
        )
        .await;
    ok(s, &preview);
    assert_eq!(
        preview["summary"],
        json!({"create":1,"skip_existing":1,"error":3})
    );
    assert!(
        preview["rows"][1]["problems"]
            .to_string()
            .contains("Duplicate")
    );
    assert!(
        preview["rows"][3]["problems"]
            .to_string()
            .contains("Invalid date")
    );
    assert_eq!(
        preview["rows"][4]["missing"],
        json!(["closed_date", "closure_basis"])
    );
    let id = preview["batch_id"].as_i64().unwrap();
    let key: String = c
        .query_row(
            "SELECT storage_key FROM import_batches WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        std::fs::read(db.files_dir().join(key)).unwrap(),
        csv.as_bytes()
    );
    let path = format!("/api/import/{id}/commit");
    let (s, result) = elena.post_idem(&path, "commit-legacy", json!({})).await;
    ok(s, &result);
    let cid = result["created"][0]["case_id"].as_i64().unwrap();
    let (date,incomplete,closed,basis):(String,i64,Option<String>,Option<String>)=c.query_row("SELECT registered_date,historical_incomplete,closed_date,closure_basis FROM cases WHERE id=?1",[cid],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    assert_eq!(date, "2001-04-03");
    assert_eq!(incomplete, 1);
    assert_eq!(closed, None);
    assert_eq!(basis, None);
    let (_, replayed) = elena.post_idem(&path, "commit-legacy", json!({})).await;
    assert_eq!(result, replayed);
    assert_eq!(elena.post(&path, json!({})).await.0, StatusCode::CONFLICT);
    assert_eq!(
        elena.post_idem(&path, "other-key", json!({})).await.0,
        StatusCode::CONFLICT
    );
    let (_, detail) = elena.get(&format!("/api/import/{id}")).await;
    assert_eq!(detail["result"], result);
    let (_, list) = elena.get("/api/import").await;
    assert!(
        list["batches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["id"] == id)
    );
    let clean = format!(
        "{HEADER}{number},civil_contract,Historical,2001-04-03,closed,{username},,,Legacy Person (claimant)\n{existing},civil_contract,Existing,2000-01-01,registered,,,,\n"
    );
    let (s, p) = elena
        .upload(
            "/api/import/cases/preview",
            &[],
            "again.csv",
            clean.as_bytes(),
        )
        .await;
    ok(s, &p);
    assert_eq!(
        p["summary"],
        json!({"create":0,"skip_existing":2,"error":0})
    );
    let (_, next) = register_case(&olga, "Following import").await;
    assert_eq!(next, format!("DEMO-CIV-{year}-0041"));
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM case_assignments WHERE case_id=?1 AND role='clerk'",
            [cid],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM parties WHERE name='Legacy Person'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    for persona in ["olga", "sergei", "pavel"] {
        let client = olga.switch(persona).await;
        assert_eq!(client.get("/api/import").await.0, StatusCode::FORBIDDEN);
        assert_eq!(
            client
                .upload("/api/import/cases/preview", &[], "x.csv", clean.as_bytes())
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(client.post(&path, json!({})).await.0, StatusCode::FORBIDDEN);
    }
    let other = app.persona("elena").await;
    assert_eq!(
        other.get(&format!("/api/import/{id}")).await.0,
        StatusCode::NOT_FOUND
    );
}
#[tokio::test]
async fn legacy_numbers_dates_and_validation() {
    let app = TestApp::demo();
    let client = app.persona("elena").await;
    let db = client.db(&app);
    let c = db.open().unwrap();
    let reg: i64 = c
        .query_row(
            "SELECT id FROM registries WHERE series='DEMO-CIV'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let regstr = reg.to_string();
    let csv = format!(
        "{HEADER}OLD-17,civil_contract,Archived,1999-02-03,closed,,1999-03-04,settled,Same Name (claimant); Same Name (respondent)\nBAD-X,unknown,Invalid,2000-01-01,unknown,unknown,,,\n"
    );
    let (_, p) = client
        .upload(
            "/api/import/cases/preview",
            &[],
            "legacy.csv",
            csv.as_bytes(),
        )
        .await;
    assert_eq!(p["rows"][0]["action"], "error");
    let (s, p) = client
        .upload(
            "/api/import/cases/preview",
            &[("registry_id", &regstr)],
            "legacy.csv",
            csv.as_bytes(),
        )
        .await;
    ok(s, &p);
    assert_eq!(p["rows"][0]["legacy_number"], "OLD-17");
    assert!(p["rows"][0]["target_number"].is_null());
    assert!(p["rows"][1]["problems"].as_array().unwrap().len() >= 3);
    let id = p["batch_id"].as_i64().unwrap();
    let (s, r) = client
        .post(&format!("/api/import/{id}/commit"), json!({}))
        .await;
    ok(s, &r);
    let cid = r["created"][0]["case_id"].as_i64().unwrap();
    assert_eq!(r["created"][0]["number"], "DEMO-CIV-1999-0001");
    let events: Vec<(String, String)> = c
        .prepare(
            "SELECT to_status,effective_date FROM case_status_history WHERE case_id=?1 ORDER BY id",
        )
        .unwrap()
        .query_map([cid], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        events,
        vec![
            ("registered".into(), "1999-02-03".into()),
            ("closed".into(), "1999-03-04".into())
        ]
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM parties WHERE name='Same Name'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    let (_, p) = client
        .upload(
            "/api/import/cases/preview",
            &[],
            "again.csv",
            format!(
                "{HEADER}OLD-17,civil_contract,Archived,1999-02-03,closed,,1999-03-04,settled,\n"
            )
            .as_bytes(),
        )
        .await;
    // Existing legacy numbers can be skipped without choosing a new registry.
    assert_eq!(p["rows"][0]["action"], "skip_existing");
    assert_eq!(
        client
            .upload("/api/import/cases/preview", &[], "bad.csv", b"bad headers")
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        client
            .upload("/api/import/cases/preview", &[], "bad.csv", &[255, 254])
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        client
            .upload(
                "/api/import/cases/preview",
                &[],
                "huge.csv",
                &vec![b'a'; 5 * 1024 * 1024 + 1]
            )
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}
#[tokio::test]
async fn zip_import_safety_policy_and_commit() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, number) = register_case(&olga, "File import").await;
    let elena = olga.switch("elena").await;
    let bytes = pdf("Imported");
    let manifest = format!(
        "case_number,filename,title,doc_type,visibility,document_date\n{number},folder/material.pdf,Imported file,evidence,party_material,2000-02-03\n"
    );
    let z = zip_entries(&[
        ("manifest.csv", manifest.as_bytes()),
        ("folder/material.pdf", &bytes),
    ]);
    let (s, p) = elena
        .upload("/api/import/files/preview", &[], "files.zip", &z)
        .await;
    ok(s, &p);
    assert_eq!(p["summary"]["create"], 1);
    let id = p["batch_id"].as_i64().unwrap();
    let path = format!("/api/import/{id}/commit");
    let (s, r) = elena.post_idem(&path, "files-import", json!({})).await;
    ok(s, &r);
    let (_, retry) = elena.post_idem(&path, "files-import", json!({})).await;
    assert_eq!(r, retry);
    assert_eq!(elena.post(&path, json!({})).await.0, StatusCode::CONFLICT);
    let db = elena.db(&app);
    let c = db.open().unwrap();
    let vid = r["created"][0]["version_id"].as_i64().unwrap();
    let (sha, key): (String, String) = c
        .query_row(
            "SELECT sha256,storage_key FROM document_versions WHERE id=?1",
            [vid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(std::fs::read(db.files_dir().join(key)).unwrap(), bytes);
    assert_eq!(sha, tuvalu_court::auth::sha256_hex(&bytes));
    c.execute("UPDATE cases SET restricted=1 WHERE id=?1", [cid])
        .unwrap();
    let (_, p) = elena
        .upload("/api/import/files/preview", &[], "hidden.zip", &z)
        .await;
    assert_eq!(p["summary"]["error"], 1);
    for name in [
        "../evil.pdf",
        "/evil.pdf",
        "folder\\evil.pdf",
        "folder/../evil.pdf",
    ] {
        let bad = zip_entries(&[("manifest.csv", manifest.as_bytes()), (name, &bytes)]);
        assert_eq!(
            elena
                .upload("/api/import/files/preview", &[], "bad.zip", &bad)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    let big = vec![0; 20 * 1024 * 1024 + 1];
    let bad = zip_entries(&[("manifest.csv", manifest.as_bytes()), ("huge.pdf", &big)]);
    assert_eq!(
        elena
            .upload("/api/import/files/preview", &[], "big.zip", &bad)
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..501 {
        writer
            .start_file(format!("{i}.pdf"), SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"x").unwrap();
    }
    let bad = writer.finish().unwrap().into_inner();
    assert_eq!(
        elena
            .upload("/api/import/files/preview", &[], "many.zip", &bad)
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            "bomb.pdf",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
    writer.write_all(&vec![0; 1024 * 1024]).unwrap();
    let bad = writer.finish().unwrap().into_inner();
    assert_eq!(
        elena
            .upload("/api/import/files/preview", &[], "ratio.zip", &bad)
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let unsupported = zip_entries(&[
        ("manifest.csv", manifest.as_bytes()),
        ("folder/material.pdf", b"<html>bad</html>"),
    ]);
    let (_, p) = elena
        .upload(
            "/api/import/files/preview",
            &[],
            "unsupported.zip",
            &unsupported,
        )
        .await;
    assert_eq!(p["summary"]["error"], 1);
}
#[tokio::test]
async fn export_only_permitted_versions_and_checksums() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Exportable").await;
    let db = olga.db(&app);
    let uid = user_id(&olga, "Olga").await;
    let (public, pubver) = insert_document(&db, cid, "Public", "evidence", "party_material", uid);
    let (_, restricted) = insert_document(&db, cid, "Restricted", "medical", "restricted", uid);
    let (_, note) = insert_document(
        &db,
        cid,
        "Judge private",
        "judicial_note",
        "judicial_note",
        uid,
    );
    let (s, _, bytes) = package(&olga, cid, json!({"purpose":"Copies for participant"})).await;
    assert_eq!(s, StatusCode::OK);
    let mut z = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
    let manifest: Value = serde_json::from_reader(z.by_name("manifest.json").unwrap()).unwrap();
    assert_eq!(manifest["format"], "tcr-case-export/1");
    assert_eq!(
        manifest["note"],
        "Participant/user package of permitted materials. Not a backup."
    );
    assert_eq!(manifest["files"].as_array().unwrap().len(), 1);
    assert_eq!(manifest["files"][0]["document_title"], "Public");
    for f in manifest["files"].as_array().unwrap() {
        let mut data = vec![];
        z.by_name(f["path"].as_str().unwrap())
            .unwrap()
            .read_to_end(&mut data)
            .unwrap();
        assert_eq!(f["sha256"], tuvalu_court::auth::sha256_hex(&data));
        assert_eq!(f["size_bytes"], data.len());
    }
    let c = db.open().unwrap();
    let stored: String = c
        .query_row(
            "SELECT sha256 FROM export_batches WHERE case_id=?1 ORDER BY id DESC LIMIT 1",
            [cid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, tuvalu_court::auth::sha256_hex(&bytes));
    let (s, _, bytes) = package(
        &olga,
        cid,
        json!({"purpose":"Authorised restricted copy","version_ids":[restricted]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let m: Value = serde_json::from_reader(z.by_name("manifest.json").unwrap()).unwrap();
    assert_eq!(m["files"][0]["visibility"], "restricted");
    assert_eq!(
        package(&olga, cid, json!({"purpose":"Notes","version_ids":[note]}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let elena = olga.switch("elena").await;
    assert_eq!(
        package(
            &elena,
            cid,
            json!({"purpose":"Restricted","version_ids":[restricted]})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        package(&olga, cid, json!({"purpose":""})).await.0,
        StatusCode::BAD_REQUEST
    );
    c.execute(
        "UPDATE document_versions SET scan_status='quarantined' WHERE id=?1",
        [pubver],
    )
    .unwrap();
    assert_eq!(
        package(
            &olga,
            cid,
            json!({"purpose":"Quarantine","version_ids":[pubver]})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    c.execute(
        "UPDATE document_versions SET scan_status='clean' WHERE document_id=?1",
        [public],
    )
    .unwrap();
    let (other, _) = register_case(&olga, "Other case").await;
    let (_, version) = insert_document(&db, other, "Other", "evidence", "party_material", uid);
    assert_eq!(
        package(
            &olga,
            cid,
            json!({"purpose":"Other","version_ids":[version]})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let pavel = olga.switch("pavel").await;
    assert_eq!(
        package(&pavel, cid, json!({"purpose":"Backup"})).await.0,
        StatusCode::NOT_FOUND
    );
    let sergei = olga.switch("sergei").await;
    let sid = user_id(&elena, "Sergei").await;
    c.execute("INSERT INTO case_assignments(case_id,user_id,role,reason,start_at) VALUES(?1,?2,'service_officer','Deliver',?3)",params![cid,sid,tuvalu_court::time::now_utc()]).unwrap();
    assert_eq!(
        package(&sergei, cid, json!({"purpose":"Copies"})).await.0,
        StatusCode::FORBIDDEN
    );
    let other = app.persona("olga").await;
    assert_eq!(
        package(&other, cid, json!({"purpose":"Other sandbox"}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn commits_serialize_and_recheck_case_access() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, number) = register_case(&olga, "Import access").await;
    let elena = olga.switch("elena").await;
    let manifest = format!(
        "case_number,filename,title,doc_type,visibility,document_date\n{number},a.pdf,Imported,evidence,administrative,\n"
    );
    let bytes = pdf("Retry");
    let z = zip_entries(&[("manifest.csv", manifest.as_bytes()), ("a.pdf", &bytes)]);
    let (s, p) = elena
        .upload("/api/import/files/preview", &[], "files.zip", &z)
        .await;
    ok(s, &p);
    let id = p["batch_id"].as_i64().unwrap();
    let path = format!("/api/import/{id}/commit");
    let (a, b) = tokio::join!(
        elena.post_idem(&path, "same-import", json!({})),
        elena.post_idem(&path, "same-import", json!({}))
    );
    ok(a.0, &a.1);
    ok(b.0, &b.1);
    assert_eq!(a.1, b.1);
    let db = elena.db(&app);
    let c = db.open().unwrap();
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM documents WHERE case_id=?1 AND title='Imported'",
            [cid],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    c.execute("UPDATE cases SET restricted=1 WHERE id=?1", [cid])
        .unwrap();
    assert_eq!(
        elena.post_idem(&path, "same-import", json!({})).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        elena.get(&format!("/api/import/{id}")).await.0,
        StatusCode::NOT_FOUND
    );
    let (_, list) = elena.get("/api/import").await;
    assert!(
        list["batches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["id"] != id)
    );
    c.execute("UPDATE cases SET restricted=0 WHERE id=?1", [cid])
        .unwrap();
    let (_, p) = elena
        .upload("/api/import/files/preview", &[], "files.zip", &z)
        .await;
    let fresh = p["batch_id"].as_i64().unwrap();
    c.execute("UPDATE cases SET restricted=1 WHERE id=?1", [cid])
        .unwrap();
    assert_eq!(
        elena
            .post(&format!("/api/import/{fresh}/commit"), json!({}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let status: String = c
        .query_row(
            "SELECT status FROM import_batches WHERE id=?1",
            [fresh],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "previewed");
}

#[tokio::test]
async fn export_manifest_preserves_versions_times_relations_and_redacted_history() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Manifest").await;
    let (visible, visible_number) = register_case(&olga, "Related visible").await;
    let (hidden, hidden_number) = register_case(&olga, "Related hidden").await;
    let db = olga.db(&app);
    let c = db.open().unwrap();
    let uid = user_id(&olga, "Olga").await;
    let now = tuvalu_court::time::now_utc();
    c.execute("UPDATE cases SET restricted=1 WHERE id=?1", [hidden])
        .unwrap();
    let (doc, v1) = insert_document(
        &db,
        cid,
        "Clean versions",
        "evidence",
        "administrative",
        uid,
    );
    let bytes = pdf("Second version");
    let (key, sha) = tuvalu_court::storage::write_blob(&db, &bytes).unwrap();
    c.execute("INSERT INTO document_versions(document_id,version_no,filename,content_type,size_bytes,sha256,storage_key,scan_status,uploaded_by,uploaded_at) VALUES(?1,2,'second.pdf','application/pdf',?2,?3,?4,'clean',?5,?6)",params![doc,bytes.len() as i64,sha,key,uid,now]).unwrap();
    let v2 = c.last_insert_rowid();
    let bytes = pdf("Quarantined version");
    let (key, sha) = tuvalu_court::storage::write_blob(&db, &bytes).unwrap();
    c.execute("INSERT INTO document_versions(document_id,version_no,filename,content_type,size_bytes,sha256,storage_key,scan_status,uploaded_by,uploaded_at) VALUES(?1,3,'third.pdf','application/pdf',?2,?3,?4,'quarantined',?5,?6)",params![doc,bytes.len() as i64,sha,key,uid,now]).unwrap();
    let (note, _) = insert_document(
        &db,
        cid,
        "Private text",
        "judicial_note",
        "judicial_note",
        uid,
    );
    for other in [visible, hidden] {
        c.execute("INSERT INTO case_relations(from_case_id,to_case_id,kind,created_at) VALUES(?1,?2,'related',?3)",params![cid,other,now]).unwrap();
    }
    let local = format!("{}T09:00", today());
    let start = tuvalu_court::time::local_to_utc(&local).unwrap();
    let end = tuvalu_court::time::add_minutes(&start, 60).unwrap();
    c.execute("INSERT INTO hearings(case_id,hearing_type,status,starts_at,ends_at,created_at) VALUES(?1,'mention','held',?2,?3,?4)",params![cid,start,end,now]).unwrap();
    for status in ["draft", "finalised", "superseded", "withdrawn"] {
        c.execute("INSERT INTO decisions(case_id,title,status,document_id,document_version_id,author_user_id,created_at) VALUES(?1,?2,?2,?3,?4,?5,?6)",params![cid,status,doc,v1,uid,now]).unwrap();
    }
    let actor = tuvalu_court::auth::load_actor(&c, uid, None)
        .unwrap()
        .unwrap();
    db.write_blocking(|tx| {
        tuvalu_court::audit::record(
            tx,
            Some(&actor),
            tuvalu_court::audit::Event::new("document.created", "document", note, "Private text")
                .case(Some(cid)),
        )?;
        Ok(())
    })
    .unwrap();
    let elena = olga.switch("elena").await;
    let (s, _, raw) = package(&elena, cid, json!({"purpose":"Participant copy"})).await;
    assert_eq!(s, StatusCode::OK);
    let mut z = zip::ZipArchive::new(Cursor::new(raw)).unwrap();
    let m: Value = serde_json::from_reader(z.by_name("manifest.json").unwrap()).unwrap();
    assert_eq!(m["files"].as_array().unwrap().len(), 1);
    assert_eq!(m["files"][0]["version_no"], 2);
    assert!(
        m["files"][0]["path"]
            .as_str()
            .unwrap()
            .starts_with(&format!("files/{v2}-"))
    );
    assert_eq!(m["hearings"][0]["starts_local"], local);
    assert_eq!(m["hearings"][0]["starts_at"], start);
    assert_eq!(m["decisions"].as_array().unwrap().len(), 2);
    assert!(
        m["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["document_version_id"] == v1)
    );
    assert_eq!(m["relations"].as_array().unwrap().len(), 1);
    assert_eq!(m["relations"][0]["number"], visible_number);
    assert!(!m.to_string().contains(&hidden_number));
    assert!(!m.to_string().contains("Private text"));
    assert!(
        m["chronology"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["summary"] == "Activity on a document you do not have access to")
    );
    assert_eq!(
        package(
            &elena,
            cid,
            json!({"purpose":"Duplicates","version_ids":[v1,v1]})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn zip_symlinks_duplicates_and_declared_expansion_limits_are_rejected() {
    let app = TestApp::demo();
    let elena = app.persona("elena").await;
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .add_symlink("link.pdf", "target.pdf", SimpleFileOptions::default())
        .unwrap();
    let bad = writer.finish().unwrap().into_inner();
    assert_eq!(
        elena
            .upload("/api/import/files/preview", &[], "symlink.zip", &bad)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut duplicate = zip_entries(&[("a.pdf", b"a"), ("b.pdf", b"b")]);
    for i in 0..duplicate.len() - 5 {
        if &duplicate[i..i + 5] == b"b.pdf" {
            duplicate[i] = b'a';
        }
    }
    assert_eq!(
        elena
            .upload(
                "/api/import/files/preview",
                &[],
                "duplicate.zip",
                &duplicate
            )
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut expanded = zip_entries(&[
        ("a.pdf", b"a"),
        ("b.pdf", b"b"),
        ("c.pdf", b"c"),
        ("d.pdf", b"d"),
        ("e.pdf", b"e"),
        ("f.pdf", b"f"),
    ]);
    // The central directory is examined before any contents are decompressed. Forge plausible
    // per-entry metadata whose aggregate exceeds 100 MiB, without a large test allocation.
    for i in 0..expanded.len() - 46 {
        if &expanded[i..i + 4] == b"PK\x01\x02" {
            expanded[i + 20..i + 24].copy_from_slice(&(1024 * 1024u32).to_le_bytes());
            expanded[i + 24..i + 28].copy_from_slice(&(20 * 1024 * 1024u32).to_le_bytes());
        }
    }
    assert_eq!(
        elena
            .upload("/api/import/files/preview", &[], "expansion.zip", &expanded)
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}
