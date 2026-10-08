mod common;
use axum::http::StatusCode;
use common::*;
use rusqlite::params;
use serde_json::{Value, json};
fn metric(v: &Value, k: &str) -> i64 {
    v["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["key"] == k)
        .unwrap()["count"]
        .as_i64()
        .unwrap()
}
#[tokio::test]
async fn reports_dates_visibility_drilldown_and_csv() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (visible, _) = register_case(&olga, "=HYPERLINK(\"evil\")").await;
    let (hidden, _) = register_case(&olga, "Secret case").await;
    let db = olga.db(&app);
    let c = db.open().unwrap();
    c.execute("UPDATE cases SET restricted=1 WHERE id=?1", [hidden])
        .unwrap();
    for id in [visible, hidden] {
        c.execute(
            "UPDATE cases SET registered_date='2000-01-10' WHERE id=?1",
            [id],
        )
        .unwrap();
        c.execute(
            "UPDATE case_status_history SET effective_date='2000-01-10' WHERE case_id=?1",
            [id],
        )
        .unwrap();
    }
    let elena = olga.switch("elena").await;
    let path = "/api/reports/summary?from=2000-01-01&to=2000-01-31&as_of=2000-01-31";
    let (s, summary) = elena.get(path).await;
    ok(s, &summary);
    assert_eq!(metric(&summary, "new_cases"), 1);
    assert_eq!(metric(&summary, "open_as_of"), 1);
    for m in summary["metrics"].as_array().unwrap() {
        let (s, items) = elena.get(m["drilldown"].as_str().unwrap()).await;
        ok(s, &items);
        assert_eq!(items["rows"].as_array().unwrap().len() as i64, m["count"]);
    }
    let (s, h, bytes) = elena
        .get_bytes("/api/reports/new_cases/csv?from=2000-01-01&to=2000-01-31&as_of=2000-01-31")
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h["content-type"], "text/csv; charset=utf-8");
    assert!(
        h["content-disposition"]
            .to_str()
            .unwrap()
            .contains("new_cases-2000-01-31.csv")
    );
    let csv = String::from_utf8(bytes).unwrap();
    assert!(csv.contains("'=HYPERLINK"));
    assert!(!csv.contains("Secret case"));
    let (_, prior) = elena
        .get("/api/reports/summary?from=1900-01-01&to=1900-01-01&as_of=1900-01-01")
        .await;
    assert_eq!(metric(&prior, "open_as_of"), 0);
    let vid = settlement_document(&olga, &app, visible).await;
    let (s, b) = olga
        .post(
            &format!("/api/cases/{visible}/close"),
            json!({"basis":"settled","note":"Settlement","basis_document_version_id":vid}),
        )
        .await;
    ok(s, &b);
    let (_, old) = elena.get(path).await;
    assert_eq!(metric(&old, "closed_cases"), 0);
    let (_, current) = elena
        .get(&format!("/api/reports/summary?from={0}&to={0}", today()))
        .await;
    assert!(metric(&current, "closed_cases") >= 1);
    let (s, b) = elena
        .post(
            &format!("/api/cases/{visible}/reopen"),
            json!({"reason":"Legacy settlement failed"}),
        )
        .await;
    ok(s, &b);
    let (_, current) = elena
        .get(&format!("/api/reports/summary?from={0}&to={0}", today()))
        .await;
    assert!(metric(&current, "reopened_cases") >= 1);
    assert!(metric(&current, "closed_cases") >= 1);
    for persona in ["pavel", "sergei"] {
        let client = olga.switch(persona).await;
        for p in [
            "/api/reports/summary",
            "/api/reports/new_cases/items",
            "/api/reports/new_cases/csv",
        ] {
            assert_eq!(client.get(p).await.0, StatusCode::FORBIDDEN);
        }
    }
    assert_eq!(
        elena.get("/api/reports/summary?from=bad").await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        elena
            .get("/api/reports/summary?from=2026-02-01&to=2026-01-01")
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        elena.get("/api/reports/missing/items").await.0,
        StatusCode::NOT_FOUND
    );
}
#[tokio::test]
async fn hearings_dispatch_and_workload_are_case_filtered() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Visible workload").await;
    let (hidden, _) = register_case(&olga, "Hidden workload").await;
    let db = olga.db(&app);
    let c = db.open().unwrap();
    let uid = user_id(&olga, "Olga").await;
    c.execute("UPDATE cases SET restricted=1 WHERE id=?1", [hidden])
        .unwrap();
    let start = tuvalu_court::time::local_to_utc(&format!("{}T09:00", today())).unwrap();
    let end = tuvalu_court::time::add_minutes(&start, 60).unwrap();
    let now = tuvalu_court::time::now_utc();
    for case in [cid, hidden] {
        c.execute("INSERT INTO hearings(case_id,hearing_type,status,starts_at,ends_at,created_at) VALUES(?1,'mention','scheduled',?2,?3,?4)",params![case,start,end,now]).unwrap();
        c.execute("INSERT INTO tasks(case_id,title,assignee_user_id,status,created_at) VALUES(?1,'Next step',?2,'open',?3)",params![case,uid,now]).unwrap();
        c.execute("INSERT INTO dispatches(case_id,kind,recipient_name,method,subject,body,purpose,status,prepared_by,prepared_at) VALUES(?1,'notice','Recipient','email','Notice','Body','Notice','sent',?2,?3)",params![case,uid,now]).unwrap();
    }
    let elena = olga.switch("elena").await;
    let (_, before) = elena.get("/api/reports/summary").await;
    let (_, hearing) = elena.get("/api/reports/upcoming_hearings/items").await;
    assert!(
        hearing["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["case_title"] == "Visible workload")
    );
    assert!(
        hearing["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["case_title"] != "Hidden workload")
    );
    let did: i64 = c
        .query_row("SELECT id FROM dispatches WHERE case_id=?1", [cid], |r| {
            r.get(0)
        })
        .unwrap();
    // Drill-down rows keep the codes and add human labels for kind and method.
    let (_, undelivered) = elena.get("/api/reports/undelivered_notices/items").await;
    let row = undelivered["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == did)
        .unwrap();
    assert_eq!(row["kind"], "notice");
    assert_eq!(row["kind_label"], "Notice");
    assert_eq!(row["method"], "email");
    assert_eq!(row["method_label"], "E-mail (local mailbox in this installation)");
    assert_eq!(row["status_label"], "Sent");
    let keys: Vec<&str> = undelivered["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["key"].as_str().unwrap())
        .collect();
    assert!(keys.contains(&"kind_label") && keys.contains(&"method_label") && keys.contains(&"status_label"));
    c.execute("INSERT INTO delivery_confirmations(dispatch_id,kind,note,recorded_by,recorded_at) VALUES(?1,'technical_ack','Technical delivery',?2,?3)",params![did,uid,now]).unwrap();
    let (_, technical) = elena.get("/api/reports/summary").await;
    assert_eq!(
        metric(&before, "undelivered_notices"),
        metric(&technical, "undelivered_notices")
    );
    c.execute("INSERT INTO delivery_confirmations(dispatch_id,kind,note,recorded_by,recorded_at) VALUES(?1,'human_handover','Handed over',?2,?3)",params![did,uid,now]).unwrap();
    let (_, after) = elena.get("/api/reports/summary").await;
    assert_eq!(
        metric(&after, "undelivered_notices"),
        metric(&before, "undelivered_notices") - 1
    );
    let baseline = after["workload"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["user_id"] == uid)
        .unwrap()["open_tasks"]
        .as_i64()
        .unwrap();
    c.execute("INSERT INTO tasks(title,assignee_user_id,status,created_at) VALUES('Unscoped',?1,'open',?2)",params![uid,now]).unwrap();
    let (_, after) = elena.get("/api/reports/summary").await;
    assert_eq!(
        after["workload"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["user_id"] == uid)
            .unwrap()["open_tasks"],
        baseline
    );
}
#[tokio::test]
async fn search_and_history_omit_sensitive_materials() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Visible search").await;
    let (hidden, number) = register_case(&olga, "Secret search").await;
    let db = olga.db(&app);
    let c = db.open().unwrap();
    c.execute("UPDATE cases SET restricted=1 WHERE id=?1", [hidden])
        .unwrap();
    let uid = user_id(&olga, "Olga").await;
    let (doc, _) = insert_document(&db, cid, "Sensitive-needle", "medical", "restricted", uid);
    insert_document(
        &db,
        cid,
        "Judicial-needle",
        "judicial_note",
        "judicial_note",
        uid,
    );
    insert_document(
        &db,
        cid,
        "Visible-needle",
        "evidence",
        "party_material",
        uid,
    );
    let actor = tuvalu_court::auth::load_actor(&c, uid, None)
        .unwrap()
        .unwrap();
    db.write_blocking(|tx| {
        tuvalu_court::audit::record(
            tx,
            Some(&actor),
            tuvalu_court::audit::Event::new(
                "document.viewed_restricted",
                "document",
                doc,
                "Sensitive-needle",
            )
            .case(Some(cid))
            .details(json!({"secret":"Sensitive-needle"})),
        )?;
        tuvalu_court::audit::record(
            tx,
            Some(&actor),
            tuvalu_court::audit::Event::new("case.updated", "case", hidden, "Secret search"),
        )?;
        Ok(())
    })
    .unwrap();
    let elena = olga.switch("elena").await;
    let (_, b) = elena.get(&format!("/api/search?q={number}")).await;
    assert!(b["cases"].as_array().unwrap().is_empty());
    let (_, b) = elena.get("/api/search?q=Fenwick").await;
    assert!(
        b["cases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == cid)
    );
    assert!(
        b["cases"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["id"] != hidden)
    );
    let (_, b) = elena.get("/api/search?q=needle").await;
    assert_eq!(b["documents"].as_array().unwrap().len(), 1);
    let (_, b) = elena.get("/api/search?q=x").await;
    assert_eq!(b, json!({"cases":[],"documents":[],"intakes":[]}));
    let (s, h) = elena.get(&format!("/api/cases/{cid}/history")).await;
    ok(s, &h);
    assert!(h["events"].as_array().unwrap().iter().all(|r| r["action"] != "document.viewed_restricted"));
    let (_, a) = elena.get("/api/audit").await;
    assert!(!a.to_string().contains("Secret search"));
    assert!(!a.to_string().contains("Sensitive-needle"));
    let (_, verified) = elena.get("/api/audit/verify").await;
    assert_eq!(verified["intact"], true);
    let pavel = olga.switch("pavel").await;
    assert_eq!(pavel.get("/api/audit").await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        pavel.get("/api/audit/verify").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        pavel.get(&format!("/api/cases/{cid}/history")).await.0,
        StatusCode::NOT_FOUND
    );
    let other = app.persona("elena").await;
    assert_eq!(
        other.get(&format!("/api/cases/{cid}/history")).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn hidden_counterpart_case_is_omitted_from_history_and_audit() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (visible, _) = register_case(&olga, "Visible relation").await;
    let (hidden, number) = register_case(&olga, "Secret counterpart").await;
    let (s, b) = olga
        .post(
            &format!("/api/cases/{visible}/relations"),
            json!({"to_case_id":hidden,"kind":"related"}),
        )
        .await;
    ok(s, &b);
    let db = olga.db(&app);
    let c = db.open().unwrap();
    let details: String = c
        .query_row(
            "SELECT details FROM audit_events WHERE action='case.related' AND case_id=?1",
            [visible],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&details).unwrap()["related_case_id"],
        hidden
    );
    c.execute("UPDATE cases SET restricted=1 WHERE id=?1", [hidden])
        .unwrap();
    let elena = olga.switch("elena").await;
    for path in [
        format!("/api/cases/{visible}/history"),
        format!("/api/audit?case_id={visible}"),
    ] {
        let (s, b) = elena.get(&path).await;
        ok(s, &b);
        assert!(b["events"].as_array().unwrap().iter().all(|e| e["action"] != "case.related"));
        assert!(!b.to_string().contains(&number));
    }
    let (s, b) = olga.get(&format!("/api/cases/{visible}/history")).await;
    ok(s, &b);
    assert!(b.to_string().contains(&number));
    // Legacy events without details also fail closed when a counterpart is hidden.
    db.write_blocking(|tx| {
        tuvalu_court::audit::record(
            tx,
            None,
            tuvalu_court::audit::Event::new(
                "case.related",
                "case",
                visible,
                format!("Linked to {number}"),
            )
            .case(Some(visible)),
        )?;
        Ok(())
    })
    .unwrap();
    let (_, b) = elena.get(&format!("/api/cases/{visible}/history")).await;
    assert!(!b.to_string().contains(&number));
}
