mod common;
use axum::http::StatusCode;
use common::*;
use rusqlite::params;
use serde_json::{Value, json};

fn intake_body(date: &str) -> Value {
    json!({"sender_name":"DEMO Workflow sender", "channel":"counter", "received_date":date,
        "description":"DEMO filing"})
}

async fn replay(c: &Client, path: &str, key: &str, body: Value) -> Value {
    let (s, first) = c.post_idem(path, key, body.clone()).await;
    ok(s, &first);
    // Discarding the first response models a committed command whose response was lost.
    let (s, retry) = c.post_idem(path, key, body.clone()).await;
    ok(s, &retry);
    assert_eq!(first, retry, "lost-response retry for {path}");
    let mut changed = body;
    for field in ["title", "note", "reason", "result", "sender_name", "missing_items"] {
        if changed.get(field).is_some() {
            changed[field] = json!("DEMO different request");
            break;
        }
    }
    let (s, b) = c.post_idem(path, key, changed).await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    first
}

fn event_count(c: &Client, app: &TestApp, event: &str, entity: i64) -> i64 {
    c.db(app)
        .open()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM audit_events WHERE action = ?1 AND entity_id = ?2",
            params![event, entity],
            |r| r.get(0),
        )
        .unwrap()
}

fn basis(c: &Client, app: &TestApp, cid: i64, uid: i64) -> i64 {
    let db = c.db(app);
    let (doc, vid) = insert_document(&db, cid, "DEMO settlement", "claim", "administrative", uid);
    db.open()
        .unwrap()
        .execute("UPDATE documents SET document_date = ?2 WHERE id = ?1", params![doc, today()])
        .unwrap();
    vid
}

#[tokio::test]
async fn f11_t24_t26_closure_requires_checkable_basis_and_ordered_dates() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO closure validation").await;
    let path = format!("/api/cases/{cid}/close");
    let (s, b) = c.post(&path, json!({"basis":"settled"})).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let uid = user_id(&c, "Olga").await;
    let vid = basis(&c, &app, cid, uid);
    for date in ["2020-01-01", "2099-01-01"] {
        let (s, b) = c
            .post(&path, json!({"basis":"settled", "basis_document_version_id":vid, "closed_date":date}))
            .await;
        err(s, &b, StatusCode::BAD_REQUEST, "validation");
    }
    let b = replay(
        &c,
        &path,
        "close-basis",
        json!({"basis":"settled", "note":"DEMO settlement", "basis_document_version_id":vid, "closed_date":today()}),
    )
    .await;
    assert_eq!(b["closed_date"], today());
    assert_eq!(event_count(&c, &app, "case.closed", cid), 1);
    let (_, detail) = c.get(&format!("/api/cases/{cid}")).await;
    assert_eq!(detail["case"]["basis_document_version_id"], vid);
    let (_, hs) = c.get(&format!("/api/hearings?from={}&to={}", today(), today())).await;
    assert!(!hs["items"].as_array().unwrap().iter().any(|h| h["case_id"] == cid));
}

#[tokio::test]
async fn f11_t16_t17_t24_t27_basis_must_belong_be_visible_clean_and_nonjudicial() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO basis access").await;
    let (other, _) = register_case(&c, "DEMO other").await;
    let uid = user_id(&c, "Olga").await;
    let judge = user_id(&c, "Viktor").await;
    let db = c.db(&app);
    let conn = db.open().unwrap();
    let foreign = basis(&c, &app, other, uid);
    let (_, hidden) = insert_document(&db, cid, "DEMO hidden", "claim", "restricted", judge);
    let (_, note) = insert_document(&db, cid, "DEMO note", "claim", "judicial_note", uid);
    let (doc, future) = insert_document(&db, cid, "DEMO future basis", "claim", "administrative", uid);
    conn.execute("UPDATE documents SET document_date='2099-01-01' WHERE id=?1", [doc]).unwrap();
    let (_, quarantined) = insert_document(&db, cid, "DEMO quarantined", "claim", "administrative", uid);
    // Versions are immutable; a new quarantined version can be inserted.
    conn.execute("INSERT INTO document_versions(document_id,version_no,filename,content_type,size_bytes,sha256,storage_key,scan_status,uploaded_by,uploaded_at)
      SELECT document_id,2,filename,content_type,size_bytes,sha256,storage_key || '-quarantined','quarantined',uploaded_by,uploaded_at FROM document_versions WHERE id=?1", [quarantined]).unwrap();
    let bad = conn.last_insert_rowid();
    let path = format!("/api/cases/{cid}/close");
    for (vid, status, code) in [
        (foreign, StatusCode::BAD_REQUEST, "validation"),
        (hidden, StatusCode::NOT_FOUND, "not_found"),
        (note, StatusCode::BAD_REQUEST, "validation"),
        (future, StatusCode::BAD_REQUEST, "validation"),
        (bad, StatusCode::BAD_REQUEST, "validation"),
    ] {
        let (s, b) = c.post(&path, json!({"basis":"settled","basis_document_version_id":vid})).await;
        err(s, &b, status, code);
    }
    let elena = c.switch("elena").await;
    conn.execute(
        "DELETE FROM user_permissions WHERE user_id=?1 AND permission='case.close'",
        [user_id(&elena, "Elena").await],
    )
    .unwrap();
    let vid = basis(&c, &app, cid, uid);
    let (s, b) = elena.post(&path, json!({"basis":"settled","basis_document_version_id":vid})).await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    let stranger = app.persona("olga").await;
    let (s, b) = stranger.post(&path, json!({"basis":"settled","basis_document_version_id":vid})).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn f11_t18_t24_closure_references_held_outcome_or_finalised_decision() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO hearing basis").await;
    let uid = user_id(&c, "Olga").await;
    let conn = c.db(&app).open().unwrap();
    let date = today();
    let start = tuvalu_court::time::local_to_utc(&format!("{date}T00:00")).unwrap();
    let end = tuvalu_court::time::add_minutes(&start, 1).unwrap();
    conn.execute(
        "INSERT INTO hearings(case_id,hearing_type,status,starts_at,ends_at,outcome_summary,created_by,created_at)
      VALUES (?1,'directions','held',?2,?3,'DEMO claim withdrawn',?4,?2)",
        params![cid, start, end, uid],
    )
    .unwrap();
    let hid = conn.last_insert_rowid();
    let path = format!("/api/cases/{cid}/close");
    let (s, b) = c.post(&path, json!({"basis":"settled"})).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = c.post(&path, json!({"basis":"settled","basis_hearing_id":hid})).await;
    ok(s, &b);
    assert_eq!(event_count(&c, &app, "case.closed", cid), 1);
    let (cid, _) = register_case(&c, "DEMO decision basis").await;
    let vid = basis(&c, &app, cid, uid);
    conn.execute(
        "INSERT INTO decisions(case_id,title,decision_date,status,document_id,document_version_id,author_user_id,created_at)
      SELECT ?1,'DEMO decision',?2,'draft',document_id,id,?3,uploaded_at FROM document_versions WHERE id=?4",
        params![cid, date, uid, vid],
    )
    .unwrap();
    let did = conn.last_insert_rowid();
    let path = format!("/api/cases/{cid}/close");
    let (s, b) = c.post(&path, json!({"basis":"decided","basis_decision_id":did})).await;
    assert!(!s.is_success(), "draft basis must be rejected: {b}");
    conn.execute("UPDATE decisions SET status='finalised' WHERE id=?1", [did]).unwrap();
    let (s, b) = c.post(&path, json!({"basis":"decided","basis_decision_id":did})).await;
    ok(s, &b);
}

#[tokio::test]
async fn f15_t26_future_hearing_and_other_planned_work_agree_with_report_and_summary() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO future hearing only").await;
    let elena = c.switch("elena").await;
    let uid = user_id(&c, "Olga").await;
    let judge = user_id(&c, "Viktor").await;
    let (s, b) = elena
        .post(
            &format!("/api/cases/{cid}/assignments"),
            json!({"user_id":judge,"role":"judge","reason":"DEMO allocate"}),
        )
        .await;
    ok(s, &b);
    let conn = c.db(&app).open().unwrap();
    conn.execute(
        "INSERT INTO hearings(case_id,hearing_type,status,starts_at,ends_at,created_by,created_at)
      VALUES (?1,'directions','scheduled','2099-01-01T00:00:00Z','2099-01-01T01:00:00Z',?2,?3)",
        params![cid, uid, tuvalu_court::time::now_utc()],
    )
    .unwrap();
    let (_, report) = c.get("/api/reports/without_next_step/items").await;
    assert!(
        !report["rows"].as_array().unwrap().iter().any(|r| r["id"] == cid),
        "future hearing already supplies a next step"
    );
    let (_, detail) = c.get(&format!("/api/cases/{cid}")).await;
    assert!(detail["next_actions"].as_array().unwrap().iter().any(|a| a["code"] == "scheduled_hearing"));
    conn.execute("UPDATE hearings SET status='cancelled' WHERE case_id=?1", [cid]).unwrap();
    let (_, report) = c.get("/api/reports/without_next_step/items").await;
    assert!(
        report["rows"].as_array().unwrap().iter().any(|r| r["id"] == cid),
        "suggestion to schedule is not an existing plan"
    );
    let (_, summary) = c.get("/api/reports/summary").await;
    let metric = summary["metrics"].as_array().unwrap().iter().find(|m| m["key"] == "without_next_step").unwrap();
    assert_eq!(metric["count"].as_u64().unwrap() as usize, report["rows"].as_array().unwrap().len());
    let task = replay(&c, &format!("/api/cases/{cid}/tasks"), "planned-task", json!({"title":"DEMO planned work"})).await;
    let (_, report) = c.get("/api/reports/without_next_step/items").await;
    assert!(!report["rows"].as_array().unwrap().iter().any(|r| r["id"] == cid));
    let (s, b) = c.post(&format!("/api/tasks/{}/complete", task["id"]), json!({"result":"DEMO done"})).await;
    ok(s, &b);
}

#[tokio::test]
async fn f17_t03_t04_intake_numbers_cross_9999_and_remain_unique_under_concurrency() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let date = today();
    let year = &date[..4];
    let id = new_intake(&c, "DEMO counter boundary").await;
    let conn = c.db(&app).open().unwrap();
    conn.execute("UPDATE intakes SET reference=?2 WHERE id=?1", params![id, format!("IN-{year}-9998")])
        .unwrap();
    for n in [9999, 10000, 10001] {
        let key = format!("boundary-{n}");
        let b = replay(&c, "/api/intakes", &key, intake_body(&date)).await;
        assert_eq!(b["reference"], format!("IN-{year}-{n:04}"));
    }
    let (a, b) = tokio::join!(
        c.post_idem("/api/intakes", "parallel-a", intake_body(&date)),
        c.post_idem("/api/intakes", "parallel-b", intake_body(&date))
    );
    ok(a.0, &a.1);
    ok(b.0, &b.1);
    assert_ne!(a.1["reference"], b.1["reference"]);
    let old = replay(&c, "/api/intakes", "older-year", intake_body("2000-01-01")).await;
    assert_eq!(old["reference"], "IN-2000-0001");
}

#[tokio::test]
async fn f13_t04_t18_t29_task_create_complete_reassign_replay_once() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO task retries").await;
    let path = format!("/api/cases/{cid}/tasks");
    let t = replay(&c, &path, "task-create", json!({"title":"DEMO task"})).await;
    let id = t["id"].as_i64().unwrap();
    assert_eq!(event_count(&c, &app, "task.created", id), 1);
    let other = c.post_idem(&path, "task-create-new", json!({"title":"DEMO task"})).await;
    ok(other.0, &other.1);
    assert_ne!(other.1["id"], id);
    let body = json!({"version":t["version"],"assignee_user_id":user_id(&c,"Olga").await});
    let path = format!("/api/tasks/{id}");
    let (s, first) = c.patch_idem(&path, "task-reassign", body.clone()).await;
    ok(s, &first);
    let (s, retry) = c.patch_idem(&path, "task-reassign", body.clone()).await;
    ok(s, &retry);
    assert_eq!(first, retry);
    let (s, b) = c
        .patch_idem(&path, "task-reassign", json!({"version":t["version"],"assignee_user_id":null}))
        .await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    assert_eq!(event_count(&c, &app, "task.updated", id), 1);
    replay(&c, &format!("/api/tasks/{id}/complete"), "task-complete", json!({"result":"DEMO filed"})).await;
    assert_eq!(event_count(&c, &app, "task.completed", id), 1);
}

#[tokio::test]
async fn f13_t01_t02_t04_intake_committing_commands_replay_after_transition() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let i = replay(&c, "/api/intakes", "intake-create", intake_body(&today())).await;
    let id = i["id"].as_i64().unwrap();
    replay(
        &c,
        &format!("/api/intakes/{id}/request-info"),
        "intake-info",
        json!({"missing_items":"DEMO missing attachment"}),
    )
    .await;
    replay(&c, &format!("/api/intakes/{id}/supplement"), "intake-supplement", intake_body(&today())).await;
    replay(&c, &format!("/api/intakes/{id}/mark-ready"), "intake-ready", json!({"note":"DEMO checked"})).await;
    assert_eq!(event_count(&c, &app, "intake.ready", id), 1);
    replay(
        &c,
        &format!("/api/intakes/{id}/return"),
        "intake-return",
        json!({"reason":"DEMO wrong registry"}),
    )
    .await;
    assert_eq!(event_count(&c, &app, "intake.returned", id), 1);
    let dup = new_intake(&c, "DEMO duplicate").await;
    replay(
        &c,
        &format!("/api/intakes/{dup}/mark-duplicate"),
        "intake-duplicate",
        json!({"duplicate_of_intake_id":id,"reason":"DEMO duplicate filing"}),
    )
    .await;
    assert_eq!(event_count(&c, &app, "intake.duplicate", dup), 1);
}

#[tokio::test]
async fn f13_t24_t25_case_close_reopen_and_relation_retries_preserve_history() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO reopen retry").await;
    let vid = basis(&c, &app, cid, user_id(&c, "Olga").await);
    replay(
        &c,
        &format!("/api/cases/{cid}/close"),
        "case-close",
        json!({"basis":"settled","note":"DEMO settled","basis_document_version_id":vid}),
    )
    .await;
    let e = c.switch("elena").await;
    replay(
        &e,
        &format!("/api/cases/{cid}/reopen"),
        "case-reopen",
        json!({"reason":"DEMO settlement failed"}),
    )
    .await;
    assert_eq!(event_count(&c, &app, "case.reopened", cid), 1);
    let (other, _) = register_case(&c, "DEMO separate related case").await;
    replay(
        &c,
        &format!("/api/cases/{cid}/relations"),
        "case-relation",
        json!({"to_case_id":other,"kind":"follow_up","note":"DEMO follow-up"}),
    )
    .await;
    assert_eq!(event_count(&c, &app, "case.related", cid), 1);
    let (_, d) = c.get(&format!("/api/cases/{cid}")).await;
    assert!(d["status_history"].as_array().unwrap().iter().any(|h| h["to_status"] == "closed"));
    assert!(d["status_history"].as_array().unwrap().iter().any(|h| h["to_status"] == "reopened"));
}

#[tokio::test]
async fn f13_t04_t15_document_upload_and_version_replay_bind_metadata_and_bytes() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO file retries").await;
    let path = format!("/api/cases/{cid}/documents");
    let fields = [
        ("title", "DEMO application"),
        ("doc_type", "claim"),
        ("source", "party"),
        ("visibility", "party_material"),
    ];
    let bytes = pdf("DEMO version one");
    let (s, first) = c.upload_idem(&path, "file-upload", &fields, "DEMO.pdf", &bytes).await;
    ok(s, &first);
    let (s, retry) = c.upload_idem(&path, "file-upload", &fields, "DEMO.pdf", &bytes).await;
    ok(s, &retry);
    assert_eq!(first, retry);
    let (s, b) = c.upload_idem(&path, "file-upload", &fields, "DEMO.pdf", &pdf("DEMO different")).await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    let id = first["id"].as_i64().unwrap();
    assert_eq!(event_count(&c, &app, "document.uploaded", id), 1);
    let path = format!("/api/documents/{id}/versions");
    let bytes = pdf("DEMO version two");
    let (s, first) = c.upload_idem(&path, "file-version", &[("note", "DEMO correction")], "DEMO.pdf", &bytes).await;
    ok(s, &first);
    let (s, retry) = c.upload_idem(&path, "file-version", &[("note", "DEMO correction")], "DEMO.pdf", &bytes).await;
    ok(s, &retry);
    assert_eq!(first, retry);
    let (s, b) = c
        .upload_idem(&path, "file-version", &[("note", "DEMO different note")], "DEMO.pdf", &bytes)
        .await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    assert_eq!(event_count(&c, &app, "document.version_added", id), 1);
    assert_eq!(first["versions"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn f13_t04_t15_document_replay_survives_case_closing() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO upload then close").await;
    let path = format!("/api/cases/{cid}/documents");
    let fields = [
        ("title", "DEMO basis"),
        ("doc_type", "claim"),
        ("source", "party"),
        ("visibility", "party_material"),
    ];
    let bytes = pdf("DEMO basis");
    let (s, doc) = c.upload_idem(&path, "closed-upload", &fields, "DEMO.pdf", &bytes).await;
    ok(s, &doc);
    let vpath = format!("/api/documents/{}/versions", doc["id"]);
    let (s, version) = c
        .upload_idem(&vpath, "closed-version", &[("note", "DEMO correction")], "DEMO.pdf", &bytes)
        .await;
    ok(s, &version);
    c.db(&app)
        .open()
        .unwrap()
        .execute("UPDATE cases SET status='closed' WHERE id=?1", [cid])
        .unwrap();
    let (s, retry) = c.upload_idem(&path, "closed-upload", &fields, "DEMO.pdf", &bytes).await;
    ok(s, &retry);
    // A replay re-renders the same document as it is now (the later version included), without
    // creating anything.
    assert_eq!(retry["id"], doc["id"]);
    assert_eq!(retry["version_count"], 2);
    let (s, retry) = c
        .upload_idem(&vpath, "closed-version", &[("note", "DEMO correction")], "DEMO.pdf", &bytes)
        .await;
    ok(s, &retry);
    assert_eq!(retry["id"], version["id"]);
    assert_eq!(retry["versions"], version["versions"]);
    assert_eq!(retry["version_count"], 2);
}

#[tokio::test]
async fn f13_t02_t04_intake_register_link_and_upload_retries_survive_linking() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let id = new_intake(&c, "DEMO linked upload").await;
    let fields = [
        ("title", "DEMO application"),
        ("doc_type", "claim"),
        ("source", "party"),
        ("visibility", "party_material"),
    ];
    let path = format!("/api/intakes/{id}/documents");
    let bytes = pdf("DEMO original filing");
    let (s, doc) = c.upload_idem(&path, "intake-upload", &fields, "DEMO.pdf", &bytes).await;
    ok(s, &doc);
    let (s, _) = c.post(&format!("/api/intakes/{id}/mark-ready"), json!({})).await;
    assert!(s.is_success());
    let (_, refs) = c.get("/api/ref").await;
    let rid = refs["registries"].as_array().unwrap().iter().find(|r| r["series"] == "DEMO-CIV").unwrap()["id"].clone();
    let case = replay(
        &c,
        &format!("/api/intakes/{id}/register"),
        "intake-register",
        json!({"title":"DEMO registered retry","registry_id":rid,"category":"civil_contract"}),
    )
    .await;
    let cid = case["case_id"].as_i64().unwrap();
    assert_eq!(event_count(&c, &app, "case.registered", cid), 1);
    let (s, retry) = c.upload_idem(&path, "intake-upload", &fields, "DEMO.pdf", &bytes).await;
    ok(s, &retry);
    // Same record, now shown under the registered case.
    assert_eq!(retry["id"], doc["id"]);
    assert_eq!(retry["versions"], doc["versions"]);
    assert_eq!(retry["case_id"], cid);
    assert_eq!(event_count(&c, &app, "document.uploaded", doc["id"].as_i64().unwrap()), 1);
    let id = new_intake(&c, "DEMO link retry").await;
    replay(&c, &format!("/api/intakes/{id}/link"), "intake-link", json!({"case_id":cid,"note":"DEMO link"})).await;
    assert_eq!(event_count(&c, &app, "intake.linked", id), 1);
}

#[tokio::test]
async fn f15_t26_t27_unsent_dispatch_and_visible_draft_decision_supply_planned_work() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO planned actions").await;
    let path = format!("/api/cases/{cid}/dispatches");
    let (s, d) = c
        .post(
            &path,
            json!({"kind":"notice","recipient_name":"DEMO recipient","method":"post","subject":"DEMO notice","body":"DEMO awaiting action"}),
        )
        .await;
    ok(s, &d);
    let conn = c.db(&app).open().unwrap();
    for status in ["draft", "queued", "failed"] {
        conn.execute("UPDATE dispatches SET status=?2 WHERE id=?1", params![d["id"].as_i64(), status])
            .unwrap();
        let (_, items) = c.get("/api/reports/without_next_step/items").await;
        assert!(!items["rows"].as_array().unwrap().iter().any(|r| r["id"] == cid));
        let (_, detail) = c.get(&format!("/api/cases/{cid}")).await;
        assert!(!detail["next_actions"].as_array().unwrap().iter().any(|a| a["code"] == "plan_next_step"));
    }
    conn.execute("UPDATE dispatches SET status='cancelled' WHERE id=?1", [d["id"].as_i64()])
        .unwrap();
    let uid = user_id(&c, "Olga").await;
    let (doc, vid) = insert_document(&c.db(&app), cid, "DEMO draft", "decision", "administrative", uid);
    conn.execute("INSERT INTO decisions(case_id,title,decision_date,status,document_id,document_version_id,author_user_id,created_at) VALUES (?1,'DEMO draft',?2,'draft',?3,?4,?5,?6)",params![cid,today(),doc,vid,uid,tuvalu_court::time::now_utc()]).unwrap();
    let (_, items) = c.get("/api/reports/without_next_step/items").await;
    assert!(!items["rows"].as_array().unwrap().iter().any(|r| r["id"] == cid));
    let stranger = app.persona("olga").await;
    let (_, items) = stranger.get("/api/reports/without_next_step/items").await;
    assert!(!items["rows"].as_array().unwrap().iter().any(|r| r["id"] == cid));
    conn.execute("UPDATE cases SET status='closed' WHERE id=?1", [cid]).unwrap();
    let (_, items) = c.get("/api/reports/without_next_step/items").await;
    assert!(!items["rows"].as_array().unwrap().iter().any(|r| r["id"] == cid));
}

#[tokio::test]
async fn f11_t24_t29_closing_picker_and_stale_version_use_same_evidence_policy() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO closure picker").await;
    let uid = user_id(&c, "Olga").await;
    let vid = basis(&c, &app, cid, uid);
    let (_, note) = insert_document(&c.db(&app), cid, "DEMO private note", "judicial_note", "judicial_note", uid);
    let (_, hidden) = insert_document(&c.db(&app), cid, "DEMO hidden basis", "claim", "restricted", user_id(&c, "Viktor").await);
    let (s, items) = c.get(&format!("/api/cases/{cid}/closing-bases")).await;
    ok(s, &items);
    assert!(items["items"].as_array().unwrap().iter().any(|i| i["id"] == vid && i["kind"] == "document"));
    assert!(!items["items"].as_array().unwrap().iter().any(|i| i["id"] == note || i["id"] == hidden));
    let (s, b) = c
        .post(
            &format!("/api/cases/{cid}/close"),
            json!({"basis":"settled","basis_document_version_id":vid,"version":0}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "version_conflict");
    let (s, b) = c
        .post_idem(
            &format!("/api/cases/{cid}/close"),
            "closing-version",
            json!({"basis":"settled","basis_document_version_id":vid,"version":1}),
        )
        .await;
    ok(s, &b);
    let e = c.switch("elena").await;
    let (s, b) = e.post(&format!("/api/cases/{cid}/reopen"), json!({"reason":"DEMO reopen","version":1})).await;
    err(s, &b, StatusCode::CONFLICT, "version_conflict");
}

#[tokio::test]
async fn f11_t16_t24_t27_restricted_closure_evidence_is_redacted_for_other_case_viewers() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO restricted basis").await;
    let (_, vid) = insert_document(
        &c.db(&app),
        cid,
        "DEMO restricted settlement",
        "correspondence",
        "restricted",
        user_id(&c, "Olga").await,
    );
    let (s, b) = c
        .post(&format!("/api/cases/{cid}/close"), json!({"basis":"settled","basis_document_version_id":vid}))
        .await;
    ok(s, &b);
    let e = c.switch("elena").await;
    let (_, card) = e.get(&format!("/api/cases/{cid}")).await;
    assert!(card["case"]["basis_document_version_id"].is_null());
    let (_, audit) = e.get(&format!("/api/audit?case_id={cid}&action=case.closed")).await;
    assert_eq!(audit["events"].as_array().unwrap().len(), 1);
    assert!(audit["events"][0]["details"]["basis_document_version_id"].is_null());
    let (_, own) = c.get(&format!("/api/cases/{cid}")).await;
    assert_eq!(own["case"]["basis_document_version_id"], vid);
}

async fn f15_assert_step_views(c: &Client, cid: i64, has_step: bool, action: Option<&str>) {
    let (s, report) = c.get("/api/reports/without_next_step/items").await;
    ok(s, &report);
    let rows = report["rows"].as_array().unwrap();
    assert_eq!(rows.iter().any(|r| r["id"] == cid), !has_step, "{report}");
    let (s, summary) = c.get("/api/reports/summary").await;
    ok(s, &summary);
    let metric = summary["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["key"] == "without_next_step")
        .unwrap();
    assert_eq!(metric["count"].as_u64().unwrap() as usize, rows.len());
    let (s, detail) = c.get(&format!("/api/cases/{cid}")).await;
    ok(s, &detail);
    let actions = detail["next_actions"].as_array().unwrap();
    assert_eq!(actions.iter().any(|a| a["code"] == "plan_next_step"), !has_step, "{detail}");
    if let Some(code) = action {
        assert!(actions.iter().any(|a| a["code"] == code), "{detail}");
    }
    let (s, queue) = c.get("/api/queue").await;
    ok(s, &queue);
    let case_items: Vec<_> = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["case_number"] == detail["case"]["number"])
        .collect();
    assert_eq!(
        case_items
            .iter()
            .any(|i| i["message"].as_str().unwrap_or_default().starts_with("No next step is recorded.")),
        !has_step
    );
}

async fn f15_hearing_step(status: &str, start: &str, end: &str, action: &str) {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO pending hearing").await;
    let conn = c.db(&app).open().unwrap();
    conn.execute(
        "INSERT INTO hearings(case_id,hearing_type,status,starts_at,ends_at,created_by,created_at)
        VALUES(?1,'directions',?2,?3,?4,?5,?6)",
        params![cid, status, start, end, user_id(&c, "Olga").await, tuvalu_court::time::now_utc()],
    )
    .unwrap();
    f15_assert_step_views(&c, cid, true, Some(action)).await;
    conn.execute("UPDATE hearings SET status='cancelled' WHERE case_id=?1", [cid]).unwrap();
    f15_assert_step_views(&c, cid, false, None).await;
}

#[tokio::test]
async fn f15_ended_hearing_awaiting_outcome_agrees_across_views() {
    f15_hearing_step("scheduled", "2000-01-01T00:00:00Z", "2000-01-01T01:00:00Z", "record_outcome").await;
}

#[tokio::test]
async fn f15_draft_hearing_awaiting_confirmation_agrees_across_views() {
    f15_hearing_step("draft", "2099-01-01T00:00:00Z", "2099-01-01T01:00:00Z", "confirm_hearing").await;
}

#[tokio::test]
async fn f15_reopened_case_awaiting_status_agrees_across_views() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO reopened workflow").await;
    let vid = basis(&c, &app, cid, user_id(&c, "Olga").await);
    let (s, b) = c
        .post(
            &format!("/api/cases/{cid}/close"),
            json!({"basis":"settled","basis_document_version_id":vid}),
        )
        .await;
    ok(s, &b);
    let e = c.switch("elena").await;
    let (s, b) = e.post(&format!("/api/cases/{cid}/reopen"), json!({"reason":"DEMO new filing"})).await;
    ok(s, &b);
    f15_assert_step_views(&c, cid, true, Some("decide_after_reopen")).await;
}

#[tokio::test]
async fn f15_sent_dispatch_pending_handover_or_assessment_agrees_across_views() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO pending service").await;
    let uid = user_id(&c, "Olga").await;
    let conn = c.db(&app).open().unwrap();
    conn.execute(
        "INSERT INTO dispatches(case_id,kind,recipient_name,method,subject,body,status,prepared_by,prepared_at,sent_at)
        VALUES(?1,'notice','DEMO recipient','hand','DEMO subject','DEMO message','sent',?2,?3,?3)",
        params![cid, uid, tuvalu_court::time::now_utc()],
    )
    .unwrap();
    let did = conn.last_insert_rowid();
    f15_assert_step_views(&c, cid, true, Some("confirm_delivery")).await;
    conn.execute(
        "INSERT INTO delivery_confirmations(dispatch_id,kind,note,recorded_by,recorded_at)
        VALUES(?1,'human_handover','DEMO received',?2,?3)",
        params![did, uid, tuvalu_court::time::now_utc()],
    )
    .unwrap();
    f15_assert_step_views(&c, cid, true, Some("assess_service")).await;
    conn.execute(
        "INSERT INTO service_assessments(dispatch_id,assessment,basis,assessed_by,assessed_at)
        VALUES(?1,'served','DEMO receipt',?2,?3)",
        params![did, uid, tuvalu_court::time::now_utc()],
    )
    .unwrap();
    f15_assert_step_views(&c, cid, false, None).await;
    conn.execute("DELETE FROM delivery_confirmations WHERE dispatch_id=?1", [did]).unwrap();
    f15_assert_step_views(&c, cid, true, Some("confirm_delivery")).await;
}

#[tokio::test]
async fn f15_hidden_draft_decision_still_counts_as_recorded_work() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO restricted draft work").await;
    let judge = user_id(&c, "Viktor").await;
    let (doc, vid) = insert_document(&c.db(&app), cid, "DEMO private draft title", "decision", "restricted", judge);
    c.db(&app)
        .open()
        .unwrap()
        .execute(
            "INSERT INTO decisions(case_id,title,status,document_id,document_version_id,author_user_id,created_at)
        VALUES(?1,'DEMO private draft title','draft',?2,?3,?4,?5)",
            params![cid, doc, vid, judge, tuvalu_court::time::now_utc()],
        )
        .unwrap();
    f15_assert_step_views(&c, cid, true, Some("finalise_decision")).await;
    let (_, detail) = c.get(&format!("/api/cases/{cid}")).await;
    assert!(!detail["next_actions"].to_string().contains("DEMO private draft title"));
}

#[tokio::test]
async fn f11_reopen_rejects_backdated_close_and_preserves_open_as_of() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO closure chronology").await;
    let conn = c.db(&app).open().unwrap();
    conn.execute("UPDATE cases SET registered_date='2000-01-01' WHERE id=?1", [cid]).unwrap();
    conn.execute("UPDATE case_status_history SET effective_date='2000-01-01' WHERE case_id=?1", [cid])
        .unwrap();
    let vid = basis(&c, &app, cid, user_id(&c, "Olga").await);
    conn.execute(
        "UPDATE documents SET document_date='2000-01-01' WHERE id=(SELECT document_id FROM document_versions WHERE id=?1)",
        [vid],
    )
    .unwrap();
    let close = format!("/api/cases/{cid}/close");
    let (s, b) = c
        .post(
            &close,
            json!({"basis":"settled","basis_document_version_id":vid,"closed_date":"2000-02-01"}),
        )
        .await;
    ok(s, &b);
    let e = c.switch("elena").await;
    let (s, b) = e.post(&format!("/api/cases/{cid}/reopen"), json!({"reason":"DEMO reopen"})).await;
    ok(s, &b);
    let (s, b) = c
        .post(
            &close,
            json!({"basis":"settled","basis_document_version_id":vid,"closed_date":"2000-03-01"}),
        )
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    assert_eq!(b["error"]["details"]["field"], "closed_date");
    assert_eq!(event_count(&c, &app, "case.closed", cid), 1);
    let report = format!("/api/reports/open_as_of/items?as_of={}", today());
    let (s, b) = c.get(&report).await;
    ok(s, &b);
    assert!(
        b["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == cid && r["state_as_of"] == "reopened")
    );
    let (s, b) = c
        .post(&close, json!({"basis":"settled","basis_document_version_id":vid,"closed_date":today()}))
        .await;
    ok(s, &b);
    let (s, b) = c.get(&report).await;
    ok(s, &b);
    assert!(!b["rows"].as_array().unwrap().iter().any(|r| r["id"] == cid));
}

#[tokio::test]
async fn f11_import_future_closed_date_rejected_at_preview_and_commit() {
    let app = TestApp::demo();
    let c = app.persona("elena").await;
    let csv = b"number,category,title,registered_date,status,responsible_username,closed_date,closure_basis,parties\nDEMO-CIV-2000-0999,civil_contract,DEMO future closure,2000-01-01,closed,,2099-01-01,settled,\n";
    let (s, preview) = c.upload("/api/import/cases/preview", &[], "future.csv", csv).await;
    ok(s, &preview);
    let id = preview["batch_id"].as_i64().unwrap();
    // Model a batch previewed by the old server before this validation was added.
    let mut old = preview.clone();
    old["rows"][0]["action"] = json!("create");
    old["rows"][0]["problems"] = json!([]);
    old["summary"] = json!({"create":1,"skip_existing":0,"error":0});
    let conn = c.db(&app).open().unwrap();
    conn.execute("UPDATE import_batches SET preview_json=?2 WHERE id=?1", params![id, old.to_string()])
        .unwrap();
    let (s, b) = c.post(&format!("/api/import/{id}/commit"), json!({})).await;
    err(s, &b, StatusCode::CONFLICT, "import_changed");
    assert_eq!(preview["rows"][0]["action"], "error");
    assert!(
        preview["rows"][0]["problems"]
            .to_string()
            .contains("Closed date cannot be in the future.")
    );
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM cases WHERE import_batch_id=?1", [id], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn e2e_d2_closure_evidence_is_linked_versioned_and_redacted_in_each_status_entry() {
    let app = TestApp::demo();
    let o = app.persona("olga").await;
    let (cid,_) = register_case(&o,"DEMO evidence display").await;
    let e = o.switch("elena").await;
    let uid = user_id(&o,"Olga").await;
    let vid = basis(&o,&app,cid,uid);
    let close = format!("/api/cases/{cid}/close");
    let (s,b) = o.post(&close,json!({"basis":"settled","basis_document_version_id":vid})).await;
    ok(s,&b);
    let (_,detail) = o.get(&format!("/api/cases/{cid}")).await;
    let evidence = &detail["case"]["closure_evidence"];
    assert_eq!(evidence["label"],"DEMO settlement · v1");
    assert_eq!(evidence["link"],format!("/api/document-versions/{vid}/download"));
    assert_eq!(detail["status_history"].as_array().unwrap().last().unwrap()["closure_evidence"],*evidence);
    let (s,b) = e.post(&format!("/api/cases/{cid}/reopen"),json!({"reason":"DEMO new material"})).await;
    ok(s,&b);
    let db = o.db(&app);
    let conn = db.open().unwrap();
    let (_,secret) = insert_document(&db,cid,"SECRET closure title","decision","restricted",uid);
    conn.execute("INSERT INTO decisions(case_id,title,decision_date,status,document_id,document_version_id,author_user_id,created_at)
        SELECT ?1,'SECRET decision',?2,'finalised',document_id,id,?3,uploaded_at FROM document_versions WHERE id=?4",
        params![cid,today(),uid,secret]).unwrap();
    let did = conn.last_insert_rowid();
    let (s,b) = o.post(&close,json!({"basis":"decided","basis_decision_id":did})).await;
    ok(s,&b);
    let (_,detail) = o.get(&format!("/api/cases/{cid}")).await;
    assert_eq!(detail["case"]["closure_evidence"]["label"],"SECRET decision — SECRET closure title · v1");
    assert_eq!(detail["case"]["closure_evidence"]["link"],format!("/cases/{cid}?tab=decisions#decision-{did}"));
    let (_,hidden) = e.get(&format!("/api/cases/{cid}")).await;
    assert!(!hidden.to_string().contains("SECRET"));
    assert_eq!(hidden["case"]["closure_evidence"],json!({"label":"Restricted document","link":null}));
    assert_eq!(hidden["status_history"].as_array().unwrap().last().unwrap()["closure_evidence"],hidden["case"]["closure_evidence"]);
    assert_eq!(hidden["status_history"][1]["closure_evidence"],*evidence);
    let (s,b) = e.post(&format!("/api/cases/{cid}/reopen"),json!({"reason":"DEMO outcome"})).await;
    ok(s,&b);
    let start = tuvalu_court::time::local_to_utc(&format!("{}T00:00",today())).unwrap();
    conn.execute("INSERT INTO hearings(case_id,hearing_type,status,starts_at,ends_at,outcome_summary,created_by,created_at)
        VALUES(?1,'directions','held',?2,?3,'DEMO settlement recorded',?4,?2)",params![cid,start,tuvalu_court::time::add_minutes(&start,1).unwrap(),uid]).unwrap();
    let hid = conn.last_insert_rowid();
    let (s,b) = o.post(&close,json!({"basis":"settled","basis_hearing_id":hid})).await;
    ok(s,&b);
    let (_,detail) = e.get(&format!("/api/cases/{cid}")).await;
    assert_eq!(detail["case"]["closure_evidence"]["link"],format!("/cases/{cid}?tab=hearings&hearing={hid}"));
    assert!(detail["case"]["closure_evidence"]["label"].as_str().unwrap().contains("DEMO settlement recorded"));
    assert_eq!(detail["status_history"].as_array().unwrap().iter().filter(|h|!h["closure_evidence"].is_null()).count(),3);
}

#[tokio::test]
async fn e2e_d4_grant_candidates_share_endpoint_eligibility_and_exclude_pavel() {
    let app = TestApp::demo();
    let o = app.persona("olga").await;
    let (cid,_) = register_case(&o,"DEMO grant candidates").await;
    let e = o.switch("elena").await;
    let uid = user_id(&o,"Olga").await;
    let (doc,_) = insert_document(&o.db(&app),cid,"DEMO restricted","claim","restricted",uid);
    let pavel = user_id(&o,"Pavel").await;
    let sergei = user_id(&o,"Sergei").await;
    let conn = o.db(&app).open().unwrap();
    // Even a tech admin with delegated task permission and stale case assignment is ineligible.
    conn.execute("INSERT INTO user_permissions(user_id,permission,granted_at) VALUES(?1,'task.manage',?2)",params![pavel,tuvalu_court::time::now_utc()]).unwrap();
    conn.execute("INSERT INTO case_assignments(case_id,user_id,role,reason,assigned_by,start_at) VALUES(?1,?2,'other','DEMO stale admin assignment',?3,?4)",params![cid,pavel,uid,tuvalu_court::time::now_utc()]).unwrap();
    let path = format!("/api/documents/{doc}/grants");
    let (s,b) = e.get(&format!("{path}/candidates")).await;
    ok(s,&b);
    let items = b["items"].as_array().unwrap();
    assert!(!items.is_empty());
    assert!(!items.iter().any(|u|u["id"]==pavel || u["id"]==sergei || u["id"]==user_id_value(&conn,"elena")));
    for u in items {
        let (s,b) = e.post(&path,json!({"user_id":u["id"],"reason":"DEMO grant"})).await;
        ok(s,&b);
    }
    let (_,remaining) = e.get(&format!("{path}/candidates")).await;
    assert!(remaining["items"].as_array().unwrap().is_empty());
    let (s,b) = e.post(&path,json!({"user_id":pavel,"reason":"DEMO rejected admin grant"})).await;
    err(s,&b,StatusCode::BAD_REQUEST,"validation");
    // Fresh endpoint results reflect deactivation and ended assignments.
    let (s,b) = e.post(&format!("/api/cases/{cid}/assignments"),json!({"user_id":sergei,"role":"service_officer","reason":"DEMO access"})).await;
    ok(s,&b);
    let (_,b) = e.get(&format!("{path}/candidates")).await;
    assert!(b["items"].as_array().unwrap().iter().any(|u|u["id"]==sergei));
    conn.execute("UPDATE users SET active=0 WHERE id=?1",[sergei]).unwrap();
    let (_,b) = e.get(&format!("{path}/candidates")).await;
    assert!(!b["items"].as_array().unwrap().iter().any(|u|u["id"]==sergei));
    conn.execute("UPDATE users SET active=1 WHERE id=?1",[sergei]).unwrap();
    conn.execute("UPDATE case_assignments SET end_at=?2 WHERE case_id=?1 AND user_id=?3",params![cid,tuvalu_court::time::now_utc(),sergei]).unwrap();
    let (_,b) = e.get(&format!("{path}/candidates")).await;
    assert!(!b["items"].as_array().unwrap().iter().any(|u|u["id"]==sergei));
}

fn user_id_value(conn: &rusqlite::Connection, username: &str) -> i64 {
    conn.query_row("SELECT id FROM users WHERE username=?1",[username],|r|r.get(0)).unwrap()
}

#[tokio::test]
async fn e2e_d5_replaced_notices_are_history_without_next_steps_or_closing_blockers() {
    for status in ["adjourned","cancelled"] {
        let app = TestApp::demo();
        let o = app.persona("olga").await;
        let (cid,_) = register_case(&o,"DEMO stale notice").await;
        let uid = user_id(&o,"Olga").await;
        let db = o.db(&app);
        let conn = db.open().unwrap();
        conn.execute("INSERT INTO hearings(case_id,hearing_type,status,starts_at,ends_at,created_by,created_at)
            VALUES(?1,'hearing',?2,'2099-01-01T00:00:00Z','2099-01-01T01:00:00Z',?3,?4)",params![cid,status,uid,tuvalu_court::time::now_utc()]).unwrap();
        let hid = conn.last_insert_rowid();
        conn.execute("INSERT INTO dispatches(case_id,hearing_id,kind,recipient_name,method,address,subject,body,status,prepared_by,prepared_at,sent_at)
            VALUES(?1,?2,'notice','DEMO recipient','email','demo@example.invalid','Old invitation','DEMO','sent',?3,?4,?4)",params![cid,hid,uid,tuvalu_court::time::now_utc()]).unwrap();
        let did = conn.last_insert_rowid();
        let (_,detail) = o.get(&format!("/api/cases/{cid}")).await;
        assert!(detail["next_actions"].as_array().unwrap().iter().any(|a|a["code"]=="plan_next_step"));
        assert!(!detail["next_actions"].as_array().unwrap().iter().any(|a|a["code"]=="confirm_delivery" || a["code"]=="assess_service"));
        let (_,report) = o.get("/api/reports/without_next_step/items").await;
        assert!(report["rows"].as_array().unwrap().iter().any(|r|r["id"]==cid));
        assert!(tuvalu_court::api::cases::open_items(&conn,cid).unwrap().is_empty());
        let (_,history) = o.get(&format!("/api/cases/{cid}/dispatches")).await;
        assert!(history["items"].as_array().unwrap().iter().any(|d|d["id"]==did && d["status"]=="sent"));
        // A cancellation message about the old appointment is still active correspondence.
        conn.execute("UPDATE dispatches SET notice_purpose='cancellation' WHERE id=?1",[did]).unwrap();
        let (_,detail) = o.get(&format!("/api/cases/{cid}")).await;
        assert!(detail["next_actions"].as_array().unwrap().iter().any(|a|a["code"]=="confirm_delivery"));
        assert_eq!(tuvalu_court::api::cases::open_items(&conn,cid).unwrap().len(),1);
        conn.execute("UPDATE dispatches SET notice_purpose='invitation' WHERE id=?1",[did]).unwrap();
        let vid = basis(&o,&app,cid,uid);
        let (s,b) = o.post(&format!("/api/cases/{cid}/close"),json!({"basis":"settled","basis_document_version_id":vid})).await;
        ok(s,&b);
    }
}

#[tokio::test]
async fn e2e_d2_existing_closures_and_d5_legacy_renotify_bindings_survive_migration() {
    let app = TestApp::demo();
    let o = app.persona("olga").await;
    let e = o.switch("elena").await;
    let (cid,_) = register_case(&o,"DEMO migration evidence").await;
    let uid = user_id(&o,"Olga").await;
    let db = o.db(&app);
    let mut versions = Vec::new();
    for _ in 0..2 {
        let vid = basis(&o,&app,cid,uid);
        versions.push(vid);
        let (s,b) = o.post(&format!("/api/cases/{cid}/close"),json!({"basis":"settled","basis_document_version_id":vid})).await;
        ok(s,&b);
        let (s,b) = e.post(&format!("/api/cases/{cid}/reopen"),json!({"reason":"DEMO reopening"})).await;
        ok(s,&b);
    }
    let (_,detail) = o.get(&format!("/api/cases/{cid}")).await;
    let party = detail["participants"][0]["party_id"].as_i64().unwrap();
    let (s,h) = o.post(&format!("/api/cases/{cid}/hearings"),json!({"hearing_type":"hearing","confirm":true,
        "starts_local":"2027-09-01T09:00","ends_local":"2027-09-01T10:00","participants":[{"party_id":party,"role":"claimant"}]})).await;
    ok(s,&h);
    let (s,moved) = o.post(&format!("/api/hearings/{}/adjourn",h["id"]),json!({"starts_local":"2027-09-02T09:00","ends_local":"2027-09-02T10:00","reason":"DEMO migration","authorised_by":"Judge DEMO"})).await;
    ok(s,&moved);
    let conn = db.open().unwrap();
    // Model a v12 database with an imported closure preceding its native audit records.
    conn.execute("INSERT INTO case_status_history(id,case_id,to_status,at,effective_date) VALUES(-1,?1,'closed','2000-01-01T00:00:00Z','2000-01-01')",[cid]).unwrap();
    conn.execute_batch("ALTER TABLE case_status_history DROP COLUMN basis_document_version_id;
        ALTER TABLE case_status_history DROP COLUMN basis_decision_id;
        ALTER TABLE case_status_history DROP COLUMN basis_hearing_id;
        ALTER TABLE tasks DROP COLUMN renotify_party_id;
        ALTER TABLE hearings DROP COLUMN record_version_id;
        PRAGMA user_version=12;").unwrap();
    drop(conn);
    db.init().unwrap();
    let (_,detail) = o.get(&format!("/api/cases/{cid}")).await;
    let closures:Vec<_> = detail["status_history"].as_array().unwrap().iter().filter(|h|h["to_status"]=="closed").collect();
    assert!(closures[0]["closure_evidence"].is_null());
    for (h,vid) in closures[1..].iter().zip(versions) {
        assert_eq!(h["closure_evidence"]["link"],format!("/api/document-versions/{vid}/download"));
    }
    assert_eq!(db.open().unwrap().query_row("SELECT renotify_party_id FROM tasks WHERE id=?1",[moved["tasks"][0]["id"].as_i64()],|r|r.get::<_,i64>(0)).unwrap(),party);
    let conn = db.open().unwrap();
    let missing_seed_evidence:i64 = conn.query_row("SELECT COUNT(*) FROM case_status_history h JOIN cases c ON c.id=h.case_id
        WHERE h.to_status='closed' AND c.basis_decision_id IS NOT NULL AND h.basis_decision_id IS NULL",[],|r|r.get(0)).unwrap();
    assert_eq!(missing_seed_evidence,0);
}
