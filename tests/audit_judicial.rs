mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};

async fn post(c: &Client, path: &str, body: Value) -> Value {
    let (s, b) = c.post(path, body).await;
    ok(s, &b);
    b
}
async fn get(c: &Client, path: &str) -> Value {
    let (s, b) = c.get(path).await;
    ok(s, &b);
    b
}
async fn setup(app: &TestApp) -> (Client, Client, i64, i64) {
    let o = app.persona("olga").await;
    let (cid, _) = register_case(&o, "Judicial audit DEMO").await;
    let e = o.switch("elena").await;
    let uid = user_id(&e, "Viktor").await;
    post(
        &e,
        &format!("/api/cases/{cid}/assignments"),
        json!({"user_id":uid,"role":"judge","reason":"DEMO allocation"}),
    )
    .await;
    let v = o.switch("viktor").await;
    let (_, vid) = insert_document(
        &o.db(app),
        cid,
        "Ruling DEMO",
        "decision",
        "administrative",
        uid,
    );
    (o, v, cid, vid)
}
async fn draft(v: &Client, cid: i64, vid: i64) -> Value {
    post(
        v,
        &format!("/api/cases/{cid}/decisions"),
        json!({"title":"Ruling DEMO","document_version_id":vid}),
    )
    .await
}
fn review(d: &Value) -> Value {
    json!({"decision_date":today(),"version":d["version"],"document_version_id":d["document_version_id"]})
}
async fn hearing(o: &Client, cid: i64, status: &str, start: &str, end: &str) -> Value {
    post(o,&format!("/api/cases/{cid}/hearings"),json!({"hearing_type":"hearing","confirm":status=="scheduled","starts_local":start,"ends_local":end})).await
}
async fn notice(o: &Client, cid: i64, h: &Value, template: Option<&str>) -> Value {
    post(o,&format!("/api/cases/{cid}/dispatches"),json!({"kind":"notice","hearing_id":h["id"],"template_code":template,"recipient_name":"Recipient DEMO","method":"email","address":"demo@example.invalid","subject":"Notice DEMO","body":if template.is_none(){Some("Hearing invitation DEMO")}else{None}})).await
}
async fn queued(o: &Client, d: &Value) -> Value {
    post(
        o,
        &format!("/api/dispatches/{}/preview", d["id"]),
        json!({}),
    )
    .await;
    post(o, &format!("/api/dispatches/{}/queue", d["id"]), json!({})).await
}

#[tokio::test]
async fn f04_t29_independent_sessions_stale_review_and_double_finalise() {
    let app = TestApp::demo();
    let (o, v, cid, vid) = setup(&app).await;
    let db = o.db(&app);
    let d = draft(&v, cid, vid).await;
    // Registry head is explicitly delegated draft editing; Viktor retains the old review.
    let e = o.switch("elena").await;
    db.open().unwrap().execute("INSERT INTO user_permissions(user_id,permission,granted_at) SELECT id,'decision.draft','2026-10-08T00:00:00Z' FROM users WHERE username='elena'",[]).unwrap();
    let (_, newvid) = insert_document(
        &db,
        cid,
        "Replacement DEMO",
        "decision",
        "administrative",
        user_id(&o, "Olga").await,
    );
    let path = format!("/api/decisions/{}", d["id"]);
    let (s, b) = e
        .patch(
            &path,
            json!({"version":d["version"],"document_version_id":newvid}),
        )
        .await;
    ok(s, &b);
    let (s, b) = v
        .post_idem(&format!("{path}/finalise"), "review", review(&d))
        .await;
    err(s, &b, StatusCode::CONFLICT, "stale_review");
    assert_eq!(b["error"]["details"]["document_version_id"], newvid);
    let fresh = get(&v, &path).await;
    assert_eq!(fresh["status"], "draft");
    let v2 = o.switch("viktor").await;
    let body = review(&fresh);
    let finalise = format!("{path}/finalise");
    let (a, b) = tokio::join!(
        v.post_idem(&finalise, "race-a", body.clone()),
        v2.post_idem(&finalise, "race-b", body.clone())
    );
    assert_eq!([a.0, b.0].iter().filter(|s| s.is_success()).count(), 1);
    assert_eq!(
        [a.0, b.0]
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        1
    );
    let key = if a.0.is_success() { "race-a" } else { "race-b" };
    let client = if a.0.is_success() { &v } else { &v2 };
    let (s, replayed) = client.post_idem(&finalise, key, body).await;
    ok(s, &replayed);
    assert_eq!(replayed["document_version_id"], newvid);
    assert_eq!(db.open().unwrap().query_row("SELECT count(*) FROM audit_events WHERE action='decision.finalised' AND entity_id=?1",[d["id"].as_i64().unwrap()],|r|r.get::<_,i64>(0)).unwrap(),1);
}
#[tokio::test]
async fn f04_t29_hearing_confirm_requires_reviewed_slot() {
    let app = TestApp::demo();
    let (o, _, cid, _) = setup(&app).await;
    let h = hearing(&o, cid, "draft", "2027-03-01T09:00", "2027-03-01T10:00").await;
    let other = o.switch("olga").await;
    let path = format!("/api/hearings/{}", h["id"]);
    let (s,b)=other.patch(&path,json!({"version":h["version"],"starts_local":"2027-03-01T11:00","ends_local":"2027-03-01T12:00"})).await;
    ok(s, &b);
    let (s, b) = o
        .post(&format!("{path}/confirm"), json!({"version":h["version"]}))
        .await;
    err(s, &b, StatusCode::CONFLICT, "version_conflict");
    let fresh = get(&o, &path).await;
    assert_eq!(fresh["status"], "draft");
    post(
        &o,
        &format!("{path}/confirm"),
        json!({"version":fresh["version"]}),
    )
    .await;
}
#[tokio::test]
async fn f07_t10_t11_t13_hearing_changes_supersede_unsent_notices_only() {
    let app = TestApp::demo();
    let (o, _, cid, _) = setup(&app).await;
    let db = o.db(&app);
    for adjourn in [true, false] {
        let h = hearing(
            &o,
            cid,
            "scheduled",
            if adjourn {
                "2027-03-01T09:00"
            } else {
                "2027-03-03T09:00"
            },
            if adjourn {
                "2027-03-01T10:00"
            } else {
                "2027-03-03T10:00"
            },
        )
        .await;
        let sent = notice(&o, cid, &h, None).await;
        queued(&o, &sent).await;
        tuvalu_court::outbox::process(&db).unwrap();
        let d = notice(&o, cid, &h, None).await;
        queued(&o, &d).await;
        let path = format!("/api/hearings/{}", h["id"]);
        if adjourn {
            post(&o,&format!("{path}/adjourn"),json!({"starts_local":"2027-03-02T09:00","ends_local":"2027-03-02T10:00","reason":"DEMO move","authorised_by":"Judge DEMO"})).await;
        } else {
            post(
                &o,
                &format!("{path}/cancel"),
                json!({"reason":"DEMO cancellation"}),
            )
            .await;
        }
        assert_eq!(
            get(&o, &format!("/api/dispatches/{}", d["id"])).await["status"],
            "superseded"
        );
        tuvalu_court::outbox::process(&db).unwrap();
        assert_eq!(
            get(&o, &format!("/api/dispatches/{}", sent["id"])).await["status"],
            "sent"
        );
        assert_eq!(
            db.open()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM mailbox WHERE dispatch_id=?1",
                    [d["id"].as_i64().unwrap()],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
}
#[tokio::test]
async fn f07_t13_worker_checks_hearing_currency_and_allows_cancellation_notice() {
    let app = TestApp::demo();
    let (o, _, cid, _) = setup(&app).await;
    let db = o.db(&app);
    let h = hearing(&o, cid, "scheduled", "2027-04-01T09:00", "2027-04-01T10:00").await;
    let d = notice(&o, cid, &h, None).await;
    queued(&o, &d).await;
    // Simulate a legacy producer bypassing the hearing handler: delivery must still fail closed.
    db.open()
        .unwrap()
        .execute(
            "UPDATE hearings SET status='cancelled',version=version+1 WHERE id=?1",
            [h["id"].as_i64().unwrap()],
        )
        .unwrap();
    tuvalu_court::outbox::process(&db).unwrap();
    assert_eq!(
        get(&o, &format!("/api/dispatches/{}", d["id"])).await["status"],
        "superseded"
    );
    db.open().unwrap().execute("INSERT OR IGNORE INTO message_templates(code,name,subject,body) VALUES('hearing_cancellation','Cancellation DEMO','Cancelled DEMO','Cancelled DEMO')",[]).unwrap();
    let cancel = notice(&o, cid, &h, Some("hearing_cancellation")).await;
    queued(&o, &cancel).await;
    tuvalu_court::outbox::process(&db).unwrap();
    assert_eq!(
        get(&o, &format!("/api/dispatches/{}", cancel["id"])).await["status"],
        "sent"
    );
}
#[tokio::test]
async fn f08_t19_t21_material_kind_and_worker_decision_currency() {
    let app = TestApp::demo();
    let (o, v, cid, vid) = setup(&app).await;
    let db = o.db(&app);
    let d = draft(&v, cid, vid).await;
    let body = json!({"kind":"copies","version_ids":[vid],"recipient_name":"Recipient DEMO","method":"email","address":"demo@example.invalid"});
    let working = post(&o, &format!("/api/cases/{cid}/dispatches"), body.clone()).await;
    assert_eq!(working["items"][0]["material_kind"], "working_document");
    assert!(
        working["body"]
            .as_str()
            .unwrap()
            .contains("DRAFT / working material")
    );
    let mut official = body.clone();
    official["kind"] = json!("decision_copy");
    let (s, b) = o
        .post(&format!("/api/cases/{cid}/dispatches"), official.clone())
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    post(
        &v,
        &format!("/api/decisions/{}/finalise", d["id"]),
        review(&d),
    )
    .await;
    let copy = post(&o, &format!("/api/cases/{cid}/dispatches"), official).await;
    assert_eq!(copy["items"][0]["material_kind"], "decision_copy");
    queued(&o, &copy).await;
    let (_, vid2) = insert_document(
        &db,
        cid,
        "Amendment DEMO",
        "decision",
        "administrative",
        user_id(&o, "Viktor").await,
    );
    let amend = post(
        &v,
        &format!("/api/decisions/{}/amend", d["id"]),
        json!({"amendment_basis":"Correction DEMO","document_version_id":vid2}),
    )
    .await;
    post(
        &v,
        &format!("/api/decisions/{}/finalise", amend["id"]),
        review(&amend),
    )
    .await;
    tuvalu_court::outbox::process(&db).unwrap();
    assert_eq!(
        get(&o, &format!("/api/dispatches/{}", copy["id"])).await["status"],
        "superseded"
    );
}
#[tokio::test]
async fn f18_t09_t28_calendar_overlaps_midnight_and_utc_date_line() {
    let app = TestApp::demo();
    let (o, _, cid, _) = setup(&app).await;
    let h = hearing(&o, cid, "scheduled", "2027-01-01T23:00", "2027-01-02T01:00").await;
    assert_eq!(h["starts_at"], "2027-01-01T11:00:00Z");
    for day in ["2027-01-01", "2027-01-02"] {
        let b = get(
            &o,
            &format!("/api/hearings?from={day}&to={day}&case_id={cid}"),
        )
        .await;
        assert_eq!(b["items"].as_array().unwrap().len(), 1);
    }
    let h2 = hearing(&o, cid, "scheduled", "2027-01-03T00:00", "2027-01-03T01:00").await;
    assert_eq!(h2["starts_at"], "2027-01-02T12:00:00Z");
    let b = get(
        &o,
        &format!("/api/hearings?from=2027-01-03&to=2027-01-03&case_id={cid}"),
    )
    .await;
    assert_eq!(b["items"][0]["id"], h2["id"]);
    let b = get(
        &o,
        &format!("/api/hearings?from=2027-01-02&to=2027-01-02&case_id={cid}"),
    )
    .await;
    assert_eq!(b["items"].as_array().unwrap().len(), 1);
}
#[tokio::test]
async fn f13_t29_t37_judicial_commands_replay_without_duplicate_history() {
    let app = TestApp::demo();
    let (o, v, cid, vid) = setup(&app).await;
    let path = format!("/api/cases/{cid}/decisions");
    let body = json!({"title":"Ruling DEMO","document_version_id":vid});
    let (s, d) = v.post_idem(&path, "draft", body.clone()).await;
    ok(s, &d);
    let (s, again) = v.post_idem(&path, "draft", body.clone()).await;
    ok(s, &again);
    assert_eq!(d, again);
    let mut changed = body;
    changed["title"] = json!("Changed DEMO");
    let (s, b) = v.post_idem(&path, "draft", changed).await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    let path = format!("/api/decisions/{}/withdraw", d["id"]);
    let body = json!({"reason":"DEMO withdrawn"});
    let (s, a) = v.post_idem(&path, "withdraw", body.clone()).await;
    ok(s, &a);
    let (s, b) = v.post_idem(&path, "withdraw", body).await;
    ok(s, &b);
    assert_eq!(a, b);
    let h = hearing(&o, cid, "scheduled", "2027-05-01T09:00", "2027-05-01T10:00").await;
    let path = format!("/api/hearings/{}/outcome", h["id"]);
    let body =
        json!({"held":true,"outcome_summary":"Held DEMO","next_task":{"title":"Follow up DEMO"}});
    let (s, a) = v.post_idem(&path, "outcome", body.clone()).await;
    ok(s, &a);
    let (s, b) = v.post_idem(&path, "outcome", body).await;
    ok(s, &b);
    assert_eq!(a, b);
    let path = format!("/api/cases/{cid}/dispatches");
    let body = json!({"kind":"notice","recipient_name":"Recipient DEMO","method":"hand","subject":"DEMO notice","body":"DEMO text"});
    let (s, a) = o.post_idem(&path, "dispatch", body.clone()).await;
    ok(s, &a);
    let (s, b) = o.post_idem(&path, "dispatch", body).await;
    ok(s, &b);
    assert_eq!(a, b);
    let prefix = format!("/api/dispatches/{}", a["id"]);
    post(&o, &format!("{prefix}/preview"), json!({})).await;
    for (op, body) in [
        (
            "record-sent",
            json!({"occurred_date":today(),"note":"Handover DEMO"}),
        ),
        (
            "confirm",
            json!({"kind":"human_handover","note":"Receipt DEMO"}),
        ),
    ] {
        let path = format!("{prefix}/{op}");
        let (s, a) = o.post_idem(&path, op, body.clone()).await;
        ok(s, &a);
        let (s, b) = o.post_idem(&path, op, body).await;
        ok(s, &b);
        assert_eq!(a, b);
    }
}

#[tokio::test]
async fn f13_t29_t37_amend_queue_and_all_command_body_mismatches() {
    let app = TestApp::demo();
    let (o, v, cid, vid) = setup(&app).await;
    let d = draft(&v, cid, vid).await;
    post(
        &v,
        &format!("/api/decisions/{}/finalise", d["id"]),
        review(&d),
    )
    .await;
    let (_, newvid) = insert_document(
        &o.db(&app),
        cid,
        "Correction DEMO",
        "decision",
        "administrative",
        user_id(&o, "Viktor").await,
    );
    let path = format!("/api/decisions/{}/amend", d["id"]);
    let body = json!({"document_version_id":newvid,"amendment_basis":"Correction DEMO"});
    let (s, a) = v.post_idem(&path, "amend", body.clone()).await;
    ok(s, &a);
    let (s, b) = v.post_idem(&path, "amend", body.clone()).await;
    ok(s, &b);
    assert_eq!(a, b);
    let (s, b) = v
        .post_idem(
            &path,
            "amend",
            json!({"document_version_id":newvid,"amendment_basis":"Different DEMO"}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    let path = format!("/api/decisions/{}/withdraw", a["id"]);
    let body = json!({"reason":"Withdraw DEMO"});
    let (s, a) = v.post_idem(&path, "withdraw-body", body).await;
    ok(s, &a);
    let (s, b) = v
        .post_idem(&path, "withdraw-body", json!({"reason":"Different DEMO"}))
        .await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    let h = hearing(&o, cid, "scheduled", "2027-06-01T09:00", "2027-06-01T10:00").await;
    let path = format!("/api/hearings/{}/outcome", h["id"]);
    let (s, b) = v
        .post_idem(
            &path,
            "outcome-body",
            json!({"held":true,"outcome_summary":"Held DEMO"}),
        )
        .await;
    ok(s, &b);
    let (s, b) = v
        .post_idem(
            &path,
            "outcome-body",
            json!({"held":true,"outcome_summary":"Different DEMO"}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    let path = format!("/api/cases/{cid}/dispatches");
    let body = json!({"kind":"notice","recipient_name":"Recipient DEMO","method":"email","address":"demo@example.invalid","subject":"DEMO","body":"DEMO"});
    let (s, d) = o.post_idem(&path, "dispatch-body", body.clone()).await;
    ok(s, &d);
    let mut changed = body;
    changed["recipient_name"] = json!("Different DEMO");
    let (s, b) = o.post_idem(&path, "dispatch-body", changed).await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    post(
        &o,
        &format!("/api/dispatches/{}/preview", d["id"]),
        json!({}),
    )
    .await;
    let path = format!("/api/dispatches/{}/queue", d["id"]);
    let (s, a) = o.post_idem(&path, "queue-body", json!({})).await;
    ok(s, &a);
    let (s, b) = o.post_idem(&path, "queue-body", json!({})).await;
    ok(s, &b);
    assert_eq!(a, b);
    let (s, b) = o
        .post_idem(&path, "queue-body", json!({"changed":true}))
        .await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    tuvalu_court::outbox::process(&o.db(&app)).unwrap();
    let path = format!("/api/dispatches/{}/confirm", d["id"]);
    let (s, b) = o
        .post_idem(
            &path,
            "confirm-body",
            json!({"kind":"human_handover","note":"Received DEMO"}),
        )
        .await;
    ok(s, &b);
    let (s, b) = o
        .post_idem(
            &path,
            "confirm-body",
            json!({"kind":"human_handover","note":"Different DEMO"}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    let manual=post(&o,&format!("/api/cases/{cid}/dispatches"),json!({"kind":"notice","recipient_name":"Recipient DEMO","method":"hand","subject":"DEMO","body":"DEMO"})).await;
    post(
        &o,
        &format!("/api/dispatches/{}/preview", manual["id"]),
        json!({}),
    )
    .await;
    let path = format!("/api/dispatches/{}/record-sent", manual["id"]);
    let (s, b) = o
        .post_idem(
            &path,
            "manual-body",
            json!({"occurred_date":today(),"note":"Handover DEMO"}),
        )
        .await;
    ok(s, &b);
    let (s, b) = o
        .post_idem(
            &path,
            "manual-body",
            json!({"occurred_date":today(),"note":"Different DEMO"}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f07_t10_t13_delivery_race_and_fresh_rescheduled_preview() {
    let app = TestApp::demo();
    let (o, _, cid, _) = setup(&app).await;
    let db = o.db(&app);
    let h = hearing(&o, cid, "scheduled", "2027-07-01T09:00", "2027-07-01T10:00").await;
    let d = notice(&o, cid, &h, Some("hearing_notice")).await;
    queued(&o, &d).await;
    let worker_db = db.clone();
    let worker = tokio::task::spawn_blocking(move || tuvalu_court::outbox::process(&worker_db));
    let moved=post(&o,&format!("/api/hearings/{}/adjourn",h["id"]),json!({"starts_local":"2027-07-02T09:00","ends_local":"2027-07-02T10:00","reason":"Move DEMO","authorised_by":"Judge DEMO"})).await;
    worker.await.unwrap().unwrap();
    let dispatched = get(&o, &format!("/api/dispatches/{}", d["id"])).await;
    let mails: i64 = db
        .open()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM mailbox WHERE dispatch_id=?1",
            [d["id"].as_i64().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    match dispatched["status"].as_str().unwrap() {
        "sent" => assert_eq!(mails, 1),
        "superseded" => assert_eq!(mails, 0),
        other => panic!("unexpected {other}"),
    };
    let fresh = notice(&o, cid, &moved["new"], Some("hearing_rescheduled")).await;
    assert!(
        fresh["body"]
            .as_str()
            .unwrap()
            .contains("2 July 2027 at 09:00")
    );
    let reviewed = post(
        &o,
        &format!("/api/dispatches/{}/preview", fresh["id"]),
        json!({}),
    )
    .await;
    assert_eq!(reviewed["status"], "draft");
    assert_eq!(reviewed["hearing_version"], moved["new"]["version"]);
    assert_eq!(reviewed["hearing_starts_at"], moved["new"]["starts_at"]);
}

#[tokio::test]
async fn f04_t29_file_identity_is_required_even_with_matching_row_version() {
    let app = TestApp::demo();
    let (_, v, cid, vid) = setup(&app).await;
    let d = draft(&v, cid, vid).await;
    let path = format!("/api/decisions/{}/finalise", d["id"]);
    let (s, b) = v
        .post(
            &path,
            json!({"version":d["version"],"document_version_id":vid+999,"decision_date":today()}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "stale_review");
    for body in [
        json!({"decision_date":today(),"version":d["version"]}),
        json!({"decision_date":today(),"document_version_id":vid}),
    ] {
        let (s, b) = v.post(&path, body).await;
        err(s, &b, StatusCode::BAD_REQUEST, "validation");
    }
    assert_eq!(
        get(&v, &format!("/api/decisions/{}", d["id"])).await["status"],
        "draft"
    );
}

#[tokio::test]
async fn f08_t19_t21_labels_survive_custom_edits_and_mailbox_delivery() {
    let app = TestApp::demo();
    let (o, v, cid, vid) = setup(&app).await;
    draft(&v, cid, vid).await;
    let d=post(&o,&format!("/api/cases/{cid}/dispatches"),json!({"kind":"working_document","version_ids":[vid],"recipient_name":"Recipient DEMO","method":"email","address":"demo@example.invalid","body":"Custom covering letter DEMO"})).await;
    let path = format!("/api/dispatches/{}", d["id"]);
    let (s, edited) = o
        .patch(
            &path,
            json!({"version":d["version"],"body":"Edited covering letter DEMO"}),
        )
        .await;
    ok(s, &edited);
    assert!(
        edited["body"]
            .as_str()
            .unwrap()
            .starts_with("DRAFT / working material")
    );
    queued(&o, &edited).await;
    tuvalu_court::outbox::process(&o.db(&app)).unwrap();
    let mailbox = get(&o, "/api/mailbox").await;
    let mail = mailbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["dispatch_id"] == d["id"])
        .unwrap();
    assert!(
        mail["body"]
            .as_str()
            .unwrap()
            .starts_with("DRAFT / working material")
    );
    assert_eq!(mail["attachments"][0]["material_kind"], "working_document");
}

#[tokio::test]
async fn f07_t13_schema_upgrade_preserves_existing_dispatch_history() {
    let app = TestApp::production();
    let path = app.dir.join("legacy.sqlite");
    let files = app.dir.join("legacy-files");
    let db = tuvalu_court::db::Db::new(path, files, None);
    std::fs::create_dir_all(db.files_dir()).unwrap();
    let conn = db.open().unwrap();
    for sql in [
        include_str!("../src/migrations/0001_init.sql"),
        include_str!("../src/migrations/0002_hearings.sql"),
        include_str!("../src/migrations/0003_documents.sql"),
        include_str!("../src/migrations/0004_dispatch.sql"),
        include_str!("../src/migrations/0005_reports.sql"),
        include_str!("../src/migrations/0006_admin.sql"),
    ] {
        conn.execute_batch(sql).unwrap();
    }
    conn.execute_batch("PRAGMA user_version=6").unwrap();
    tuvalu_court::seed::seed_reference(&db).unwrap();
    tuvalu_court::seed::seed_demo(&db).unwrap();
    let before: i64 = conn
        .query_row("SELECT count(*) FROM dispatches", [], |r| r.get(0))
        .unwrap();
    drop(conn);
    db.init().unwrap();
    let conn = db.open().unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM dispatches", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        before
    );
    assert!(
        !conn
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
}
