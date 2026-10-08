mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::*;
use serde_json::{Value, json};

fn png_bytes() -> Vec<u8> {
    vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 1]
}

/// Multipart upload with an Idempotency-Key header (the harness `upload` has no key support).
async fn upload_idem(c: &Client, path: &str, key: &str, fields: &[(&str, &str)], filename: &str, bytes: &[u8]) -> (StatusCode, Value) {
    let boundary = "----tcrtestboundary";
    let mut body = Vec::new();
    for (k, v) in fields {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes());
    }
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let req = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::HOST, "localhost")
        .header(
            header::COOKIE,
            format!(
                "tcr_sandbox={}; tcr_session={}",
                c.sandbox.clone().unwrap(),
                c.session.clone().unwrap()
            ),
        )
        .header("x-tcr", "1")
        .header("idempotency-key", key)
        .header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
        .body(Body::from(body))
        .unwrap();
    let (s, v, _, _) = c.clone().send(req).await;
    (s, v)
}

/// DELETE with a JSON body (the harness `delete` sends none).
async fn delete_json(c: &Client, path: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::DELETE)
        .uri(path)
        .header(header::HOST, "localhost")
        .header(
            header::COOKIE,
            format!(
                "tcr_sandbox={}; tcr_session={}",
                c.sandbox.clone().unwrap(),
                c.session.clone().unwrap()
            ),
        )
        .header("x-tcr", "1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (s, v, _, _) = c.clone().send(req).await;
    (s, v)
}

fn doc_fields<'a>(title: &'a str, doc_type: &'a str, visibility: &'a str) -> Vec<(&'static str, &'a str)> {
    vec![
        ("title", title),
        ("doc_type", doc_type),
        ("source", "party"),
        ("visibility", visibility),
        ("is_paper_original", "false"),
    ]
}

async fn upload_to_case(c: &Client, case_id: i64, title: &str, visibility: &str, filename: &str, bytes: &[u8]) -> (StatusCode, Value) {
    c.upload(
        &format!("/api/cases/{case_id}/documents"),
        &doc_fields(title, "evidence", visibility),
        filename,
        bytes,
    )
    .await
}

async fn new_version(c: &Client, doc_id: i64, note: &str, filename: &str, bytes: &[u8]) -> (StatusCode, Value) {
    c.upload(&format!("/api/documents/{doc_id}/versions"), &[("note", note)], filename, bytes)
        .await
}

/// Assign a persona to a case through Elena's permissions.
async fn assign(elena: &Client, case_id: i64, uid: i64, role: &str) {
    let (s, b) = elena
        .post(
            &format!("/api/cases/{case_id}/assignments"),
            json!({"user_id": uid, "role": role, "reason": "Assigned for the test"}),
        )
        .await;
    ok(s, &b);
}

#[tokio::test]
async fn upload_list_detail_and_download() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, number) = register_case(&olga, "Document case").await;

    let bytes = pdf("Statement of claim text");
    let (s, b) = upload_to_case(&olga, case_id, "Statement of claim", "party_material", "claim.pdf", &bytes).await;
    ok(s, &b);
    let doc_id = b["id"].as_i64().unwrap();
    let v1 = b["versions"][0]["id"].as_i64().unwrap();
    assert_eq!(b["case_id"], case_id);
    assert_eq!(b["case_number"], number);
    assert_eq!(b["doc_type"], "evidence");
    assert_eq!(b["doc_type_label"], "Evidence");
    assert_eq!(b["visibility"], "party_material");
    assert_eq!(b["versions"][0]["version_no"], 1);
    assert_eq!(b["versions"][0]["scan_status"], "clean");
    assert_eq!(b["versions"][0]["uploaded_by_name"], "Olga Marsh");
    assert!(b["grants"].is_null());
    assert_eq!(b["used_by"]["decisions"].as_array().unwrap().len(), 0);

    // Listed on the case and on the cross-case screen.
    let (_, list) = olga.get(&format!("/api/cases/{case_id}/documents")).await;
    assert!(list["items"].as_array().unwrap().iter().any(|d| d["id"] == doc_id));
    let (_, all) = olga.get("/api/documents?q=Statement").await;
    assert!(
        all["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["id"] == doc_id && d["case_number"] == number)
    );

    // Detail shows the same data.
    let (s, d) = olga.get(&format!("/api/documents/{doc_id}")).await;
    ok(s, &d);
    assert_eq!(d["title"], "Statement of claim");

    // Download returns the exact bytes with safe headers.
    let (s, h, body) = olga.get_bytes(&format!("/api/document-versions/{v1}/download")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    assert!(h.get(header::CACHE_CONTROL).unwrap().to_str().unwrap().contains("no-store"));
    assert!(
        h.get(header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("attachment")
    );
    assert_eq!(body, bytes);
    assert_eq!(h.get(header::CONTENT_TYPE).unwrap(), "application/pdf");

    // inline=1 is honoured for PDF.
    let (s, h, _) = olga.get_bytes(&format!("/api/document-versions/{v1}/download?inline=1")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h.get(header::CONTENT_DISPOSITION).unwrap().to_str().unwrap().starts_with("inline"));
}

#[tokio::test]
async fn upload_rejects_dangerous_and_mislabelled_files() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _) = register_case(&olga, "Bad files").await;

    for (name, bytes) in [
        ("page.html", b"<html><script>alert(1)</script></html>".as_slice()),
        ("pic.svg", b"<svg xmlns='http://www.w3.org/2000/svg'><script/></svg>".as_slice()),
        ("tool.exe", b"MZ\x90\x00\x03\x00\x00\x00".as_slice()),
    ] {
        let (s, b) = upload_to_case(&olga, case_id, name, "party_material", name, bytes).await;
        err(s, &b, StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_type");
    }
    // PNG bytes disguised as .pdf.
    let (s, b) = upload_to_case(&olga, case_id, "fake", "party_material", "fake.pdf", &png_bytes()).await;
    err(s, &b, StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_type");
}

#[tokio::test]
async fn pdf_with_active_content_is_quarantined() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _) = register_case(&olga, "Quarantine").await;

    let bytes = pdf("innocent text /JavaScript (app.alert('x'))");
    let (s, b) = upload_to_case(&olga, case_id, "Scripted PDF", "party_material", "scripted.pdf", &bytes).await;
    ok(s, &b);
    let doc_id = b["id"].as_i64().unwrap();
    let v1 = b["versions"][0]["id"].as_i64().unwrap();
    assert_eq!(b["versions"][0]["scan_status"], "quarantined");

    let (s, _, bytes) = olga.get_bytes(&format!("/api/document-versions/{v1}/download")).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let b: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(b["error"]["code"], "quarantined");
    // Still listed, marked, never served.
    let (_, d) = olga.get(&format!("/api/documents/{doc_id}")).await;
    assert_eq!(d["versions"][0]["scan_status"], "quarantined");
}

#[tokio::test]
async fn versions_accumulate_and_stay_downloadable() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _) = register_case(&olga, "Versioned").await;

    let (s, b) = upload_to_case(&olga, case_id, "Report", "administrative", "report.pdf", &pdf("version one")).await;
    ok(s, &b);
    let doc_id = b["id"].as_i64().unwrap();
    let v1 = b["versions"][0]["id"].as_i64().unwrap();
    let sha1 = b["versions"][0]["sha256"].as_str().unwrap().to_string();

    // The "what changed" note is mandatory.
    let (s, b) = new_version(&olga, doc_id, "", "report.pdf", &pdf("version two")).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");

    let (s, b) = new_version(&olga, doc_id, "Corrected the case number", "report_v2.pdf", &pdf("version two")).await;
    ok(s, &b);
    let v2 = b["versions"][1]["id"].as_i64().unwrap();
    assert_eq!(b["versions"].as_array().unwrap().len(), 2);
    assert_eq!(b["versions"][1]["version_no"], 2);
    assert_eq!(b["versions"][1]["note"], "Corrected the case number");
    let sha2 = b["versions"][1]["sha256"].as_str().unwrap().to_string();
    assert_ne!(sha1, sha2);

    let db = olga.db(&app);
    let conn = db.open().unwrap();
    let audit_case: i64 = conn
        .query_row(
            "SELECT case_id FROM audit_events WHERE action = 'document.version_added' AND entity_id = ?1",
            [doc_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(audit_case, case_id);

    // Both versions remain downloadable, with their own bytes.
    let (s, _, body) = olga.get_bytes(&format!("/api/document-versions/{v1}/download")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body, pdf("version one"));
    let (s, _, body) = olga.get_bytes(&format!("/api/document-versions/{v2}/download")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body, pdf("version two"));
}

#[tokio::test]
async fn restricted_document_grant_revoke_and_view_audit() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _) = register_case(&olga, "Restricted doc").await;
    let sergei = olga.switch("sergei").await;
    let elena = olga.switch("elena").await;
    let sergei_id = user_id(&elena, "Sergei").await;
    let pavel_id = user_id(&elena, "Pavel").await;
    assign(&elena, case_id, sergei_id, "service_officer").await;

    let (s, b) = upload_to_case(
        &olga,
        case_id,
        "Medical report",
        "restricted",
        "medical.pdf",
        &pdf("medical findings"),
    )
    .await;
    ok(s, &b);
    let doc_id = b["id"].as_i64().unwrap();
    let v1 = b["versions"][0]["id"].as_i64().unwrap();

    // Sergei sees the case but not the restricted document.
    let (_, list) = sergei.get(&format!("/api/cases/{case_id}/documents")).await;
    assert!(list["items"].as_array().unwrap().iter().all(|d| d["id"] != doc_id));
    let (s, _) = sergei.get(&format!("/api/documents/{doc_id}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = sergei.get_bytes(&format!("/api/document-versions/{v1}/download")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let pavel = olga.switch("pavel").await;
    let elena_id = user_id(&elena, "Elena").await;
    let (s, b) = elena.get(&format!("/api/documents/{doc_id}")).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    let db = olga.db(&app);
    let conn = db.open().unwrap();
    let audit_count = || {
        conn.query_row("SELECT COUNT(*) FROM audit_events", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    let before = audit_count();
    let (s, list) = elena.get(&format!("/api/cases/{case_id}/restricted-documents")).await;
    ok(s, &list);
    assert_eq!(audit_count(), before, "grant management list must not audit a view");
    let item = list["items"].as_array().unwrap().iter().find(|d| d["id"] == doc_id).unwrap();
    assert_eq!(item["title"], "Medical report");
    let mut keys = item.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>();
    keys.sort();
    assert_eq!(keys, ["created_at", "created_by_name", "doc_type", "grants", "id", "title"]);
    assert!(item["grants"].as_array().unwrap().is_empty());
    let (s, b) = elena
        .post(
            &format!("/api/documents/{doc_id}/grants"),
            json!({ "user_id": elena_id, "reason": "Self grant" }),
        )
        .await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    for c in [&olga, &sergei, &pavel] {
        let (s, b) = c
            .post(
                &format!("/api/documents/{doc_id}/grants"),
                json!({ "user_id": sergei_id, "reason": "Denied" }),
            )
            .await;
        err(s, &b, StatusCode::NOT_FOUND, "not_found");
    }
    let (s, b) = olga.get(&format!("/api/cases/{case_id}/restricted-documents")).await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    let (s, b) = pavel.get(&format!("/api/cases/{case_id}/restricted-documents")).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");

    // A grant cannot help someone with no case access at all.
    let (s, b) = elena
        .post(
            &format!("/api/documents/{doc_id}/grants"),
            json!({"user_id": pavel_id, "reason": "Try"}),
        )
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");

    // Elena grants Sergei access with a reason.
    let (s, b) = elena
        .post(
            &format!("/api/documents/{doc_id}/grants"),
            json!({"user_id": sergei_id, "reason": "Handles service on this case"}),
        )
        .await;
    ok(s, &b);
    let gid = b["grant"]["id"].as_i64().unwrap();
    assert_eq!(b["grant"]["user_name"], "Sergei Novak");

    let (_, managed) = elena.get(&format!("/api/cases/{case_id}/restricted-documents")).await;
    assert_eq!(
        managed["items"].as_array().unwrap().iter().find(|d| d["id"] == doc_id).unwrap()["grants"][0]["id"],
        gid
    );
    for c in [&olga, &sergei, &pavel] {
        let (s, b) = delete_json(c, &format!("/api/documents/{doc_id}/grants/{gid}"), json!({ "reason": "Denied" })).await;
        err(s, &b, StatusCode::NOT_FOUND, "not_found");
    }

    // Sergei can now see and download it; the view is audited.
    let (s, d) = sergei.get(&format!("/api/documents/{doc_id}")).await;
    ok(s, &d);
    let (s, _, body) = sergei.get_bytes(&format!("/api/document-versions/{v1}/download")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body, pdf("medical findings"));

    let db = olga.db(&app);
    let conn = db.open().unwrap();
    let views: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_events WHERE action = 'document.viewed_restricted' AND entity_id = ?1",
            [doc_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(views, 1, "only the download is audited");
    let detail_views: i64 = conn.query_row("SELECT COUNT(*) FROM audit_events WHERE action = 'document.viewed_restricted' AND entity_id = ?1 AND json_extract(details, '$.via') = 'detail'", [doc_id], |r| r.get(0)).unwrap();
    assert_eq!(detail_views, 0);

    // Revoking (with a reason) removes access again; the row is kept.
    let (s, b) = delete_json(
        &elena,
        &format!("/api/documents/{doc_id}/grants/{gid}"),
        json!({"reason": "No longer needed"}),
    )
    .await;
    ok(s, &b);
    assert!(b["grant"]["revoked_at"].is_string());
    let (s, _) = sergei.get(&format!("/api/documents/{doc_id}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = sergei.get_bytes(&format!("/api/document-versions/{v1}/download")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn judicial_note_is_private_to_the_author_until_shared() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _) = register_case(&olga, "Judge note").await;
    let elena = olga.switch("elena").await;
    let viktor = olga.switch("viktor").await;
    let pavel = olga.switch("pavel").await;
    let viktor_id = user_id(&elena, "Viktor").await;
    let olga_id = user_id(&elena, "Olga").await;
    assign(&elena, case_id, viktor_id, "judge").await;

    // Only a judge can file a judicial note.
    let (s, b) = upload_to_case(&olga, case_id, "Fake note", "judicial_note", "n.pdf", &pdf("x")).await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");

    // Viktor uploads his working note; doc_type is forced to judicial_note.
    let (s, b) = upload_to_case(
        &viktor,
        case_id,
        "Working note",
        "judicial_note",
        "note.pdf",
        &pdf("private reasoning"),
    )
    .await;
    ok(s, &b);
    let doc_id = b["id"].as_i64().unwrap();
    let v1 = b["versions"][0]["id"].as_i64().unwrap();
    assert_eq!(b["doc_type"], "judicial_note");
    assert_eq!(b["visibility"], "judicial_note");

    // Not even view_all / grant_restricted / admin powers can read it.
    for c in [&elena, &olga, &pavel] {
        let (s, _) = c.get(&format!("/api/documents/{doc_id}")).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _, _) = c.get_bytes(&format!("/api/document-versions/{v1}/download")).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }
    let (_, list) = elena.get(&format!("/api/cases/{case_id}/documents")).await;
    assert!(list["items"].as_array().unwrap().iter().all(|d| d["id"] != doc_id));

    // Elena cannot grant access to a note — only the author can share it.
    let (s, _) = elena
        .post(
            &format!("/api/documents/{doc_id}/grants"),
            json!({"user_id": olga_id, "reason": "She asked"}),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (s, b) = delete_json(
        &elena,
        &format!("/api/documents/{doc_id}/grants/999999"),
        json!({"reason": "Denied"}),
    )
    .await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");

    // Viktor shares the note with Olga explicitly.
    let (s, b) = viktor
        .post(
            &format!("/api/documents/{doc_id}/grants"),
            json!({"user_id": olga_id, "reason": "Clerk needs the context"}),
        )
        .await;
    ok(s, &b);
    let (s, d) = olga.get(&format!("/api/documents/{doc_id}")).await;
    ok(s, &d);
    let (s, _, _) = olga.get_bytes(&format!("/api/document-versions/{v1}/download")).await;
    assert_eq!(s, StatusCode::OK);

    // Sharing gives read-only access, including for a clerk with document.manage.
    let (s, b) = new_version(&olga, doc_id, "Changed", "note2.pdf", &pdf("changed")).await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    for change in [
        json!({"title": "Renamed"}),
        json!({"legal_hold": true}),
        json!({"visibility": "administrative"}),
    ] {
        let mut change = change;
        change["version"] = d["version"].clone();
        let (s, b) = olga.patch(&format!("/api/documents/{doc_id}"), change).await;
        err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    }
    let (s, b) = olga
        .post(
            &format!("/api/documents/{doc_id}/grants"),
            json!({ "user_id": olga_id, "reason": "Denied" }),
        )
        .await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    let (s, b) = delete_json(
        &olga,
        &format!("/api/documents/{doc_id}/grants/{}", b_grant_id(&viktor, doc_id).await),
        json!({ "reason": "Denied" }),
    )
    .await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    let (_, list) = elena.get(&format!("/api/cases/{case_id}/restricted-documents")).await;
    assert!(list["items"].as_array().unwrap().iter().all(|d| d["id"] != doc_id));
    let db = olga.db(&app);
    let conn = db.open().unwrap();
    let summaries: Vec<String> = conn
        .prepare("SELECT summary FROM audit_events WHERE entity_type = 'document' AND entity_id = ?1")
        .unwrap()
        .query_map([doc_id], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(summaries.iter().all(|s| !s.contains("Working note")));
    assert!(
        summaries
            .iter()
            .any(|s| s == &format!("Judicial note #{doc_id} shared with Olga Marsh"))
    );

    // The author retains editing and version access.
    let (s, b) = viktor
        .patch(
            &format!("/api/documents/{doc_id}"),
            json!({"version": d["version"], "legal_hold": true}),
        )
        .await;
    ok(s, &b);
    let (s, b) = new_version(&viktor, doc_id, "Clarified reasoning", "note2.pdf", &pdf("clarified reasoning")).await;
    ok(s, &b);
    assert_eq!(b["versions"].as_array().unwrap().len(), 2);

    // The author sees the grant list in the detail.
    let (_, d) = viktor.get(&format!("/api/documents/{doc_id}")).await;
    let grants = d["grants"].as_array().unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0]["user_name"], "Olga Marsh");
}

#[tokio::test]
async fn ended_assignment_breaks_old_download_urls() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _) = register_case(&olga, "Rotating staff").await;
    let sergei = olga.switch("sergei").await;
    let elena = olga.switch("elena").await;
    let sergei_id = user_id(&elena, "Sergei").await;
    assign(&elena, case_id, sergei_id, "service_officer").await;

    let (s, b) = upload_to_case(&olga, case_id, "Ordinary filing", "party_material", "filing.pdf", &pdf("filing")).await;
    ok(s, &b);
    let v1 = b["versions"][0]["id"].as_i64().unwrap();
    let doc_id = b["id"].as_i64().unwrap();

    // Sergei can download while assigned.
    let (s, _, _) = sergei.get_bytes(&format!("/api/document-versions/{v1}/download")).await;
    assert_eq!(s, StatusCode::OK);

    // End the assignment → the same URL is a 404, and the document is gone from his world.
    let (_, card) = elena.get(&format!("/api/cases/{case_id}")).await;
    let aid = card["assignments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["user_id"] == sergei_id && a["end_at"].is_null())
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let (s, _) = elena
        .post(
            &format!("/api/cases/{case_id}/assignments/{aid}/end"),
            json!({"reason": "Moved to another island"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = sergei.get_bytes(&format!("/api/document-versions/{v1}/download")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = sergei.get(&format!("/api/documents/{doc_id}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tech_admin_sees_nothing() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _) = register_case(&olga, "Hidden from Pavel").await;
    let (s, b) = upload_to_case(&olga, case_id, "Filing", "party_material", "f.pdf", &pdf("f")).await;
    ok(s, &b);
    let doc_id = b["id"].as_i64().unwrap();
    let v1 = b["versions"][0]["id"].as_i64().unwrap();

    let pavel = olga.switch("pavel").await;
    for path in [format!("/api/documents/{doc_id}"), format!("/api/cases/{case_id}/documents")] {
        let (s, _) = pavel.get(&path).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{path}");
    }
    let (s, _, _) = pavel.get_bytes(&format!("/api/document-versions/{v1}/download")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, all) = pavel.get("/api/documents").await;
    assert!(all["items"].as_array().unwrap().iter().all(|d| d["id"] != doc_id));
    let (s, _) = pavel
        .upload(
            &format!("/api/cases/{case_id}/documents"),
            &doc_fields("x", "evidence", "party_material"),
            "x.pdf",
            &pdf("x"),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn intake_upload_and_checksum_warning() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let i1 = new_intake(&olga, "Sender One").await;
    let i2 = new_intake(&olga, "Sender Two").await;

    let bytes = pdf("identical attachment");
    let fields = doc_fields("Attachment", "claim", "administrative");
    let (s, b) = olga
        .upload(&format!("/api/intakes/{i1}/documents"), &fields, "att.pdf", &bytes)
        .await;
    ok(s, &b);
    assert_eq!(b["intake_id"], i1);
    assert!(b["case_id"].is_null());
    let (s, b) = olga
        .upload(&format!("/api/intakes/{i2}/documents"), &fields, "att.pdf", &bytes)
        .await;
    ok(s, &b);
    let doc2 = b["id"].as_i64().unwrap();

    // The second intake's detail warns that the same file exists elsewhere.
    let (_, d) = olga.get(&format!("/api/intakes/{i2}")).await;
    let matches = d["checksum_matches"].as_array().unwrap();
    assert!(matches.iter().any(|m| m["intake_id"] == i1), "checksum_matches: {matches:?}");

    // The intake document is also on the cross-case screen and is downloadable.
    let (_, all) = olga.get("/api/documents?visibility=administrative").await;
    assert!(all["items"].as_array().unwrap().iter().any(|d| d["id"] == doc2));
    let (s, d2) = olga.get(&format!("/api/documents/{doc2}")).await;
    ok(s, &d2);
    let v = d2["versions"][0]["id"].as_i64().unwrap();
    let (s, _, body) = olga.get_bytes(&format!("/api/document-versions/{v}/download")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body, bytes);
}

#[tokio::test]
async fn finalised_decision_freezes_the_document() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _) = register_case(&olga, "Decided").await;
    let (s, b) = upload_to_case(&olga, case_id, "Decision draft", "administrative", "d.pdf", &pdf("decision text")).await;
    ok(s, &b);
    let doc_id = b["id"].as_i64().unwrap();
    let v1 = b["versions"][0]["id"].as_i64().unwrap();
    let olga_id = user_id(&olga, "Olga").await;

    // A finalised decision binds to this document.
    let db = olga.db(&app);
    let conn = db.open().unwrap();
    conn.execute(
        "INSERT INTO decisions (case_id, title, status, document_id, document_version_id, author_user_id, created_at)
         VALUES (?1, 'Decision on the merits', 'finalised', ?2, ?3, ?4, ?5)",
        rusqlite::params![case_id, doc_id, v1, olga_id, tuvalu_court::time::now_utc()],
    )
    .unwrap();

    let (s, b) = new_version(&olga, doc_id, "Trying to change it", "d2.pdf", &pdf("other text")).await;
    err(s, &b, StatusCode::CONFLICT, "invalid_transition");

    // used_by reports the binding decision.
    let (_, d) = olga.get(&format!("/api/documents/{doc_id}")).await;
    assert_eq!(d["used_by"]["decisions"].as_array().unwrap().len(), 1);
    assert_eq!(d["used_by"]["decisions"][0]["status"], "finalised");
}

#[tokio::test]
async fn idempotent_upload_replay() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _) = register_case(&olga, "Idempotent").await;

    let fields = doc_fields("Repeatable", "evidence", "administrative");
    let bytes = pdf("same bytes");
    let (s, first) = upload_idem(&olga, &format!("/api/cases/{case_id}/documents"), "up-1", &fields, "r.pdf", &bytes).await;
    ok(s, &first);
    let before_replay = stored_blobs(&olga.db(&app));
    let (s, again) = upload_idem(&olga, &format!("/api/cases/{case_id}/documents"), "up-1", &fields, "r.pdf", &bytes).await;
    ok(s, &again);
    assert_eq!(first["id"], again["id"]);
    assert_eq!(stored_blobs(&olga.db(&app)), before_replay);

    let (_, d) = olga.get(&format!("/api/documents/{}", first["id"].as_i64().unwrap())).await;
    assert_eq!(d["versions"].as_array().unwrap().len(), 1);

    // Same key, different content → conflict, not a second document.
    let other = pdf("different bytes");
    let (s, b) = upload_idem(&olga, &format!("/api/cases/{case_id}/documents"), "up-1", &fields, "r.pdf", &other).await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    assert_eq!(stored_blobs(&olga.db(&app)), before_replay);
}

#[tokio::test]
async fn patch_rules_and_version_locking() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _) = register_case(&olga, "Patchy").await;
    let sergei = olga.switch("sergei").await;
    let elena = olga.switch("elena").await;
    let viktor = olga.switch("viktor").await;
    let sergei_id = user_id(&elena, "Sergei").await;
    let viktor_id = user_id(&elena, "Viktor").await;
    assign(&elena, case_id, sergei_id, "service_officer").await;
    assign(&elena, case_id, viktor_id, "judge").await;

    let (s, b) = upload_to_case(&olga, case_id, "Editable", "party_material", "e.pdf", &pdf("e")).await;
    ok(s, &b);
    let doc_id = b["id"].as_i64().unwrap();
    let ver = b["version"].as_i64().unwrap();

    // Optimistic locking.
    let (s, b) = olga
        .patch(&format!("/api/documents/{doc_id}"), json!({"version": ver + 9, "title": "No"}))
        .await;
    err(s, &b, StatusCode::CONFLICT, "version_conflict");

    // Sergei can see the document but lacks document.manage.
    let (s, b) = sergei
        .patch(&format!("/api/documents/{doc_id}"), json!({"version": ver, "title": "No"}))
        .await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");

    // The uploader may restrict and un-restrict it.
    let (s, b) = olga
        .patch(
            &format!("/api/documents/{doc_id}"),
            json!({"version": ver, "title": "Editable (restricted)", "visibility": "restricted"}),
        )
        .await;
    ok(s, &b);
    let ver = b["version"].as_i64().unwrap();
    assert_eq!(b["visibility"], "restricted");
    let (s, b) = olga
        .patch(
            &format!("/api/documents/{doc_id}"),
            json!({"version": ver, "visibility": "party_material", "legal_hold": true}),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["visibility"], "party_material");
    assert_eq!(b["legal_hold"], 1);

    let db = olga.db(&app);
    let conn = db.open().unwrap();
    let summaries: Vec<String> = conn
        .prepare("SELECT summary FROM audit_events WHERE action = 'document.updated' AND entity_id = ?1")
        .unwrap()
        .query_map([doc_id], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(summaries, vec![format!("Restricted document #{doc_id} details changed"); 2]);

    // A judicial note's visibility never changes, in either direction.
    let (s, b) = upload_to_case(&viktor, case_id, "Note", "judicial_note", "n.pdf", &pdf("n")).await;
    ok(s, &b);
    let note_id = b["id"].as_i64().unwrap();
    let note_ver = b["version"].as_i64().unwrap();
    let (s, b) = viktor
        .patch(
            &format!("/api/documents/{note_id}"),
            json!({"version": note_ver, "visibility": "administrative"}),
        )
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = olga
        .patch(
            &format!("/api/documents/{doc_id}"),
            json!({"version": b_version(&olga, doc_id).await, "visibility": "judicial_note"}),
        )
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
}

async fn b_version(c: &Client, doc_id: i64) -> i64 {
    let (_, d) = c.get(&format!("/api/documents/{doc_id}")).await;
    d["version"].as_i64().unwrap()
}

async fn b_grant_id(c: &Client, doc_id: i64) -> i64 {
    let (_, d) = c.get(&format!("/api/documents/{doc_id}")).await;
    d["grants"][0]["id"].as_i64().unwrap()
}

fn stored_blobs(db: &tuvalu_court::db::Db) -> Vec<std::path::PathBuf> {
    fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    collect(db.files_dir(), &mut paths);
    paths.sort();
    paths
}

#[tokio::test]
async fn uploads_validate_before_storage_and_discard_after_failed_writes() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _) = register_case(&olga, "Upload cleanup").await;
    let intake = new_intake(&olga, "Cleanup intake").await;
    let db = olga.db(&app);
    let baseline = stored_blobs(&db);
    for (field, value, status) in [
        ("title", "", StatusCode::BAD_REQUEST),
        ("doc_type", "missing", StatusCode::BAD_REQUEST),
        ("visibility", "missing", StatusCode::BAD_REQUEST),
        ("source", "missing", StatusCode::BAD_REQUEST),
        ("source_party_id", "999999", StatusCode::BAD_REQUEST),
        ("document_date", "2026-02-30", StatusCode::BAD_REQUEST),
        ("received_date", "invalid", StatusCode::BAD_REQUEST),
        ("is_paper_original", "true", StatusCode::BAD_REQUEST),
        ("visibility", "judicial_note", StatusCode::FORBIDDEN),
    ] {
        let mut fields = doc_fields("Valid", "evidence", "party_material");
        if let Some(entry) = fields.iter_mut().find(|(name, _)| *name == field) {
            entry.1 = value;
        } else {
            fields.push((field, value));
        }
        for path in [
            format!("/api/cases/{case_id}/documents"),
            format!("/api/intakes/{intake}/documents"),
        ] {
            let (s, b) = olga.upload(&path, &fields, "valid.pdf", &pdf("valid")).await;
            err(
                s,
                &b,
                status,
                if status == StatusCode::FORBIDDEN {
                    "forbidden"
                } else {
                    "validation"
                },
            );
            assert_eq!(stored_blobs(&db), baseline, "{field} on {path}");
        }
    }
    let conn = db.open().unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_upload BEFORE INSERT ON document_versions BEGIN SELECT RAISE(ABORT, 'forced write failure'); END;",
    )
    .unwrap();
    let (s, _) = upload_to_case(&olga, case_id, "Fails write", "administrative", "valid.pdf", &pdf("valid")).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(stored_blobs(&db), baseline);
    conn.execute_batch("DROP TRIGGER reject_upload").unwrap();
    let (s, d) = upload_to_case(&olga, case_id, "Versioned", "administrative", "valid.pdf", &pdf("valid")).await;
    ok(s, &d);
    let doc_id = d["id"].as_i64().unwrap();
    let baseline = stored_blobs(&db);
    let (s, b) = new_version(&olga, doc_id, "", "valid.pdf", &pdf("valid")).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    assert_eq!(stored_blobs(&db), baseline);
    conn.execute_batch(
        "CREATE TRIGGER reject_upload BEFORE INSERT ON document_versions BEGIN SELECT RAISE(ABORT, 'forced write failure'); END;",
    )
    .unwrap();
    let (s, _) = new_version(&olga, doc_id, "New version", "v2.pdf", &pdf("v2")).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(stored_blobs(&db), baseline);
}

#[tokio::test]
async fn intake_and_version_replays_and_concurrent_uploads_leave_no_orphans() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let intake = new_intake(&olga, "Replay intake").await;
    let path = format!("/api/intakes/{intake}/documents");
    let fields = doc_fields("Repeatable", "evidence", "administrative");
    let bytes = pdf("same bytes");
    let before = stored_blobs(&olga.db(&app)).len();
    let (first, second) = tokio::join!(
        upload_idem(&olga, &path, "intake-replay", &fields, "r.pdf", &bytes),
        upload_idem(&olga, &path, "intake-replay", &fields, "r.pdf", &bytes)
    );
    ok(first.0, &first.1);
    ok(second.0, &second.1);
    assert_eq!(first.1, second.1);
    assert_eq!(stored_blobs(&olga.db(&app)).len(), before + 1);
    let version_path = format!("/api/documents/{}/versions", first.1["id"].as_i64().unwrap());
    for path in [&path, &version_path] {
        let (fields, key) = if path == &version_path {
            (vec![("note", "Update")], "version-replay")
        } else {
            (fields.clone(), "intake-replay")
        };
        let (s, first) = upload_idem(&olga, path, key, &fields, "r.pdf", &bytes).await;
        ok(s, &first);
        let baseline = stored_blobs(&olga.db(&app));
        let (s, replay) = upload_idem(&olga, path, key, &fields, "r.pdf", &bytes).await;
        ok(s, &replay);
        assert_eq!(first, replay);
        assert_eq!(stored_blobs(&olga.db(&app)), baseline);
        let (s, b) = upload_idem(&olga, path, key, &fields, "r.pdf", &pdf("changed")).await;
        err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
        assert_eq!(stored_blobs(&olga.db(&app)), baseline);
    }
}

#[tokio::test]
async fn oversized_and_closed_case_uploads_are_rejected() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _) = register_case(&olga, "Closed uploads").await;
    let db = olga.db(&app);
    let baseline = stored_blobs(&db);
    let large = vec![b'x'; app.state.cfg.upload_max_bytes as usize + 1];
    let (s, b) = upload_to_case(&olga, case_id, "Large", "party_material", "large.pdf", &large).await;
    err(s, &b, StatusCode::PAYLOAD_TOO_LARGE, "too_large");
    assert_eq!(stored_blobs(&db), baseline);
    let (s, d) = upload_to_case(&olga, case_id, "Old", "administrative", "old.pdf", &pdf("old")).await;
    ok(s, &d);
    let doc_id = d["id"].as_i64().unwrap();
    let conn = db.open().unwrap();
    let intake: i64 = conn
        .query_row("SELECT id FROM intakes WHERE case_id = ?1", [case_id], |r| r.get(0))
        .unwrap();
    let (s, b) = olga
        .post(&format!("/api/cases/{case_id}/close"), json!({ "basis": "settled", "basis_document_version_id": d["versions"][0]["id"] }))
        .await;
    ok(s, &b);
    let baseline = stored_blobs(&db);
    for path in [
        format!("/api/cases/{case_id}/documents"),
        format!("/api/intakes/{intake}/documents"),
    ] {
        let (s, b) = olga
            .upload(&path, &doc_fields("New", "evidence", "party_material"), "new.pdf", &pdf("new"))
            .await;
        err(s, &b, StatusCode::CONFLICT, "invalid_transition");
    }
    let (s, b) = new_version(&olga, doc_id, "New version", "new.pdf", &pdf("new")).await;
    err(s, &b, StatusCode::CONFLICT, "invalid_transition");
    assert_eq!(stored_blobs(&db), baseline);
}
