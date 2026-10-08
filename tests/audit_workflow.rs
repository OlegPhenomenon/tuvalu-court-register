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
    assert_eq!(doc, retry);
    let (s, retry) = c
        .upload_idem(&vpath, "closed-version", &[("note", "DEMO correction")], "DEMO.pdf", &bytes)
        .await;
    ok(s, &retry);
    assert_eq!(version, retry);
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
    assert_eq!(doc, retry);
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
