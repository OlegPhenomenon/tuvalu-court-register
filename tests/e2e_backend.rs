mod common;

use axum::http::StatusCode;
use common::*;
use rusqlite::params;
use serde_json::{Value, json};

async fn assign_judge(olga: &Client, case_id: i64) -> i64 {
    let elena = olga.switch("elena").await;
    let judge = user_id(&elena, "Viktor").await;
    let (s, b) = elena
        .post(&format!("/api/cases/{case_id}/assignments"), json!({"user_id":judge,"role":"judge","reason":"Allocated for the hearing"}))
        .await;
    ok(s, &b);
    judge
}

async fn grant(client: &Client, app: &TestApp, name: &str, permissions: &[&str]) {
    let uid = user_id(client, name).await;
    let c = client.db(app).open().unwrap();
    for permission in permissions {
        c.execute(
            "INSERT OR IGNORE INTO user_permissions (user_id,permission,granted_at) VALUES (?1,?2,?3)",
            params![uid, permission, tuvalu_court::time::now_utc()],
        )
        .unwrap();
    }
}

async fn hearing(client: &Client, case_id: i64, day: &str, room: Option<i64>, confirm: bool) -> Value {
    let (s, b) = client
        .post(
            &format!("/api/cases/{case_id}/hearings"),
            json!({"hearing_type":"hearing","starts_local":format!("{day}T09:00"),
            "ends_local":format!("{day}T10:00"),"room_id":room,"confirm":confirm}),
        )
        .await;
    ok(s, &b);
    b
}

async fn notice(client: &Client, case_id: i64, recipient: &str) -> i64 {
    let (s, b) = client
        .post(
            &format!("/api/cases/{case_id}/dispatches"),
            json!({"kind":"notice","recipient_name":recipient,"method":"hand","subject":"Hearing notice",
            "body":"Please attend the hearing."}),
        )
        .await;
    ok(s, &b);
    b["id"].as_i64().unwrap()
}

async fn send_manual(client: &Client, dispatch: i64) {
    let (s, b) = client.post(&format!("/api/dispatches/{dispatch}/preview"), json!({})).await;
    ok(s, &b);
    let (s, b) = client
        .post(&format!("/api/dispatches/{dispatch}/record-sent"), json!({"occurred_date":today(),"note":"Handed to the service officer"}))
        .await;
    ok(s, &b);
}

fn audit_count(client: &Client, app: &TestApp, action: &str, case_id: i64) -> i64 {
    client
        .db(app)
        .open()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM audit_events WHERE action=?1 AND case_id=?2", params![action, case_id], |r| r.get(0))
        .unwrap()
}

#[tokio::test]
async fn c1_closing_requires_confirmation_or_individual_reason_and_preserves_other_blockers() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Closure acknowledgements").await;
    let first = notice(&olga, cid, "Alexei Fenwick").await;
    let second = notice(&olga, cid, "Maria Calder").await;
    send_manual(&olga, first).await;
    send_manual(&olga, second).await;
    let path = format!("/api/cases/{cid}/close");
    let (s, b) = olga.post(&path, json!({"basis":"decided"})).await;
    err(s, &b, StatusCode::CONFLICT, "open_items");
    let items = b["error"]["details"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|i| i["kind"] == "unconfirmed_dispatch" && i["status"] == "sent" && i["label"].is_string()));
    assert!(items.iter().any(|i| i["label"] == "Notice → Alexei Fenwick"), "items: {items:?}");
    // A technical acknowledgement is not human confirmation.
    let (s, b) = olga.post(&format!("/api/dispatches/{first}/confirm"), json!({"kind":"technical_ack","note":"Local receipt"})).await;
    ok(s, &b);
    let ack = json!({"kind":"unconfirmed_dispatch","id":first,"reason":"Recipient could not be contacted"});
    let (s, b) = olga.post(&path, json!({"basis":"decided","acknowledge":[ack.clone()]})).await;
    err(s, &b, StatusCode::CONFLICT, "open_items");
    assert_eq!(b["error"]["details"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(b["error"]["details"]["items"][0]["id"], second);
    assert_eq!(audit_count(&olga, &app, "case.closed", cid), 0);
    for reason in [json!("  "), json!("")] {
        let (s, b) =
            olga.post(&path, json!({"basis":"decided","acknowledge":[{"kind":"unconfirmed_dispatch","id":first,"reason":reason}]})).await;
        err(s, &b, StatusCode::CONFLICT, "open_items");
        assert_eq!(b["error"]["details"]["items"].as_array().unwrap().len(), 2);
    }
    for acknowledgement in [
        json!({"kind":"task","id":first,"reason":"Cannot do it"}),
        json!({"kind":"unconfirmed_dispatch","id":999999,"reason":"Other case"}),
    ] {
        let (s, b) = olga.post(&path, json!({"basis":"decided","acknowledge":[acknowledgement]})).await;
        err(s, &b, StatusCode::BAD_REQUEST, "validation");
    }
    let (s, b) =
        olga.post(&format!("/api/dispatches/{second}/confirm"), json!({"kind":"human_handover","note":"Maria confirmed receipt"})).await;
    ok(s, &b);
    let (s, task) = olga.post(&format!("/api/cases/{cid}/tasks"), json!({"title":"Finish the record"})).await;
    ok(s, &task);
    let vid = settlement_document(&olga, &app, cid).await;
    let body = json!({"basis":"settled","basis_document_version_id":vid,"note":"Settlement recorded","acknowledge":[ack]});
    let (s, b) = olga.post_idem(&path, "close-with-note", body.clone()).await;
    err(s, &b, StatusCode::CONFLICT, "open_items");
    assert_eq!(b["error"]["details"]["items"][0]["kind"], "task");
    let (s, b) = olga.post(&format!("/api/tasks/{}/cancel", task["id"]), json!({"reason":"Superseded"})).await;
    ok(s, &b);
    let (s, closed) = olga.post_idem(&path, "close-with-note", body.clone()).await;
    ok(s, &closed);
    let (s, replay) = olga.post_idem(&path, "close-with-note", body).await;
    ok(s, &replay);
    assert_eq!(closed, replay);
    assert_eq!(audit_count(&olga, &app, "case.closed", cid), 1);
    let (_, card) = olga.get(&format!("/api/cases/{cid}")).await;
    let note = card["case"]["closure_note"].as_str().unwrap();
    assert!(note.starts_with("Settlement recorded\nLeft unconfirmed: Notice → Alexei Fenwick"));
    assert!(note.ends_with(" — Recipient could not be contacted"));
    let details: String = olga
        .db(&app)
        .open()
        .unwrap()
        .query_row("SELECT details FROM audit_events WHERE action='case.closed' AND case_id=?1", [cid], |r| r.get(0))
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&details).unwrap()["acknowledge"][0]["id"], first);
}

#[tokio::test]
async fn c5_intake_prompts_and_cancellation_are_atomic_and_idempotent() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let intake = new_intake(&olga, "Alexei Fenwick").await;
    let mut ids = Vec::new();
    for address in ["alexei@example.invalid", "alexei@example.fail", "alexei@example.invalid", "alexei@example.invalid"] {
        let (s, b) = olga
            .post(
                &format!("/api/intakes/{intake}/request-info"),
                json!({"missing_items":"Repair agreement","method":"email","address":address}),
            )
            .await;
        ok(s, &b);
        assert_eq!(b.as_object().unwrap().len(), 2);
        let id = b["dispatch_id"].as_i64().unwrap();
        ids.push(id);
        if ids.len() <= 3 {
            let (s, b) = olga.post(&format!("/api/dispatches/{id}/preview"), json!({})).await;
            ok(s, &b);
            let (s, b) = olga.post(&format!("/api/dispatches/{id}/queue"), json!({})).await;
            ok(s, &b);
            if ids.len() <= 2 {
                tuvalu_court::outbox::process(&olga.db(&app)).unwrap();
            }
        }
    }
    let (_, detail) = olga.get(&format!("/api/intakes/{intake}")).await;
    assert_eq!(
        detail["next_actions"],
        json!([{"code":"review_dispatch",
        "message":"Review and send the information request to Alexei Fenwick.","link":format!("/dispatch?dispatch={}", ids[3])}])
    );
    let (s, b) = olga.post(&format!("/api/intakes/{intake}/mark-ready"), json!({})).await;
    ok(s, &b);
    let (_, refs) = olga.get("/api/ref").await;
    let body = json!({"registry_id":refs["registries"][0]["id"],"category":"civil_contract","title":"Information completed"});
    let path = format!("/api/intakes/{intake}/register");
    let (s, registered) = olga.post_idem(&path, "register-info", body.clone()).await;
    ok(s, &registered);
    let (s, replay) = olga.post_idem(&path, "register-info", body).await;
    ok(s, &replay);
    assert_eq!(registered, replay);
    assert_eq!(registered["cancelled_requests"].as_array().unwrap().len(), 2);
    for request in registered["cancelled_requests"].as_array().unwrap() {
        assert_eq!(request["status"], "cancelled");
        assert_eq!(request["status_reason"], "Not sent: the filing was registered");
    }
    assert_eq!(tuvalu_court::outbox::process(&olga.db(&app)).unwrap(), 0);
    let (_, detail) = olga.get(&format!("/api/intakes/{intake}")).await;
    assert!(detail["next_actions"].as_array().unwrap().is_empty());
    assert!(detail["intake"]["missing_items"].is_null());
    assert_eq!(detail["dispatches"][0]["status"], "sent");
    assert_eq!(detail["dispatches"][1]["status"], "failed");
    let cid = registered["case_id"].as_i64().unwrap();
    assert_eq!(audit_count(&olga, &app, "dispatch.cancelled", cid), 2);
    // Linking to an existing case follows the same rules, including replay.
    let other = new_intake(&olga, "Maria Calder").await;
    let (s, request) = olga.post(&format!("/api/intakes/{other}/request-info"), json!({"missing_items":"Signature"})).await;
    ok(s, &request);
    let path = format!("/api/intakes/{other}/link");
    let body = json!({"case_id":cid});
    let (s, linked) = olga.post_idem(&path, "link-info", body.clone()).await;
    ok(s, &linked);
    assert_eq!(linked["cancelled_requests"][0]["id"], request["dispatch_id"]);
    let (s, replay) = olga.post_idem(&path, "link-info", body).await;
    ok(s, &replay);
    assert_eq!(linked, replay);
}

#[tokio::test]
async fn c6_system_only_admin_cannot_gain_access_through_any_assignment_path() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Assignments").await;
    let elena = olga.switch("elena").await;
    let pavel = user_id(&olga, "Pavel").await;
    let (_, refs) = olga.get("/api/ref").await;
    for staff in refs["staff"].as_array().unwrap() {
        assert_eq!(staff["assignable"], staff["id"] != pavel);
    }
    let body = json!({"user_id":pavel,"role":"other","reason":"Technical assistance"});
    let path = format!("/api/cases/{cid}/assignments");
    let (s, b) = olga.post(&path, body.clone()).await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    let (s, b) = elena.post(&path, body).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    assert_eq!(b["error"]["message"], "This person administers the system and cannot be assigned to cases.");
    let (_, card) = olga.get(&format!("/api/cases/{cid}")).await;
    let (s, b) = olga.patch(&format!("/api/cases/{cid}"), json!({"version":card["case"]["version"],"responsible_user_id":pavel})).await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    let admin = olga.switch("pavel").await;
    let (s, b) = admin.get(&format!("/api/cases/{cid}")).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    let intake = new_intake(&olga, "Sender").await;
    let (s, b) = olga.post(&format!("/api/intakes/{intake}/mark-ready"), json!({})).await;
    ok(s, &b);
    let (s, b) = olga.post(&format!("/api/intakes/{intake}/register"),
        json!({"registry_id":refs["registries"][0]["id"],"category":"civil_contract","title":"Blocked registration","responsible_user_id":pavel})).await;
    // Naming another responsible officer at registration is a staff assignment (needs case.assign_staff).
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    let (_, card_after) = olga.get(&format!("/api/cases/{cid}")).await;
    assert_eq!(card["case"], card_after["case"]);
    assert_eq!(audit_count(&olga, &app, "case.assigned", cid), 0);
    assign_judge(&olga, cid).await;
    let other_sandbox = app.persona("elena").await;
    let (s, b) = other_sandbox.post(&path, json!({"user_id":pavel,"role":"other","reason":"Cannot see this case"})).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn c7_only_successful_file_reads_record_sensitive_views() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Sensitive views").await;
    let uid = user_id(&olga, "Olga").await;
    let db = olga.db(&app);
    for visibility in ["restricted", "judicial_note"] {
        let (doc, version) = insert_document(&db, cid, visibility, "medical", visibility, uid);
        for path in [format!("/api/documents/{doc}"), format!("/api/cases/{cid}/documents"), "/api/documents".into()] {
            let (s, b) = olga.get(&path).await;
            ok(s, &b);
        }
        assert_eq!(audit_count(&olga, &app, "document.viewed_restricted", cid), if visibility == "restricted" { 0 } else { 2 });
        for suffix in ["", "?inline=1"] {
            assert_eq!(olga.get_bytes(&format!("/api/document-versions/{version}/download{suffix}")).await.0, StatusCode::OK);
        }
        let elena = olga.switch("elena").await;
        assert_eq!(elena.get(&format!("/api/documents/{doc}")).await.0, StatusCode::NOT_FOUND);
        assert_eq!(elena.get_bytes(&format!("/api/document-versions/{version}/download")).await.0, StatusCode::NOT_FOUND);
        for path in [format!("/api/cases/{cid}/history"), format!("/api/audit?case_id={cid}")] {
            let (s, b) = elena.get(&path).await;
            ok(s, &b);
            assert!(b["events"].as_array().unwrap().iter().all(|ev| ev["action"] != "document.viewed_restricted"));
        }
    }
    assert_eq!(audit_count(&olga, &app, "document.viewed_restricted", cid), 4);
    let (_, broken_version) = insert_document(&db, cid, "Unavailable file", "medical", "restricted", uid);
    let key: String =
        db.open().unwrap().query_row("SELECT storage_key FROM document_versions WHERE id=?1", [broken_version], |r| r.get(0)).unwrap();
    std::fs::remove_file(db.files_dir().join(key)).unwrap();
    assert_eq!(olga.get_bytes(&format!("/api/document-versions/{broken_version}/download")).await.0, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(audit_count(&olga, &app, "document.viewed_restricted", cid), 4, "failed file reads are not views");
    let (_, verified) = olga.switch("elena").await.get("/api/audit/verify").await;
    assert_eq!(verified["intact"], true);
}

#[tokio::test]
async fn c8_mailbox_redacts_missing_and_unknown_versions_without_failing_the_list() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let db = olga.db(&app);
    let c = db.open().unwrap();
    let mut stmt = c.prepare("SELECT attachments FROM mailbox").unwrap();
    for raw in stmt.query_map([], |r| r.get::<_, String>(0)).unwrap() {
        for item in serde_json::from_str::<Value>(&raw.unwrap()).unwrap().as_array().unwrap() {
            assert!(item["document_version_id"].is_i64(), "seed attachments carry version ids");
        }
    }
    let bad_titles: i64 = c.query_row("SELECT COUNT(*) FROM documents WHERE title LIKE '%—%'", [], |r| r.get(0)).unwrap();
    assert_eq!(bad_titles, 0);
    let (s, list) = olga.get("/api/mailbox").await;
    ok(s, &list);
    let id = list["items"][0]["id"].as_i64().unwrap();
    c.execute(
        "UPDATE mailbox SET attachments=?2, body=?3 WHERE id=?1",
        params![
            id,
            json!([{"filename":"legacy-secret.pdf"},{"document_version_id":999999,"filename":"unknown-secret.pdf"}]).to_string(),
            "Cover letter\n- Legacy secret (version 1, legacy-secret.pdf)\n- Unknown secret (version 1, unknown-secret.pdf)"
        ],
    )
    .unwrap();
    for path in ["/api/mailbox".to_string(), format!("/api/mailbox/{id}")] {
        let (s, b) = olga.get(&path).await;
        ok(s, &b);
        let message = if path == "/api/mailbox" { b["items"].as_array().unwrap().iter().find(|m| m["id"] == id).unwrap() } else { &b };
        assert!(message["attachments"].as_array().unwrap().iter().all(|i| i["restricted"] == true && i.get("filename").is_none()));
        assert_eq!(message["body"], "Cover letter\n- Restricted document\n- Restricted document");
    }
}

#[tokio::test]
async fn c10_patch_null_clears_values_and_omission_preserves_them() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Nullable fields").await;
    let judge = assign_judge(&olga, cid).await;
    let (_, refs) = olga.get("/api/ref").await;
    let room = refs["rooms"][0]["id"].as_i64().unwrap();
    let draft = hearing(&olga, cid, "2030-11-17", Some(room), false).await;
    assert_eq!(draft["judge_user_id"], judge);
    let path = format!("/api/hearings/{}", draft["id"]);
    let (s, same) = olga.patch(&path, json!({"version":draft["version"],"notes":"Keep allocation"})).await;
    ok(s, &same);
    assert_eq!(same["room_id"], room);
    assert_eq!(same["judge_user_id"], judge);
    let (s, cleared) = olga.patch(&path, json!({"version":same["version"],"room_id":null,"judge_user_id":null})).await;
    ok(s, &cleared);
    assert!(cleared["room_id"].is_null() && cleared["judge_user_id"].is_null());
    let (s, preserved) = olga.patch(&path, json!({"version":cleared["version"],"notes":"Still no allocation"})).await;
    ok(s, &preserved);
    assert!(preserved["room_id"].is_null() && preserved["judge_user_id"].is_null());
    let (s, task) = olga.post(&format!("/api/cases/{cid}/tasks"), json!({"title":"Check record","assignee_user_id":judge})).await;
    ok(s, &task);
    let path = format!("/api/tasks/{}", task["id"]);
    let (s, same) = olga.patch(&path, json!({"version":task["version"],"title":"Review record"})).await;
    ok(s, &same);
    assert_eq!(same["assignee_user_id"], judge);
    let (s, cleared) = olga.patch(&path, json!({"version":same["version"],"assignee_user_id":null})).await;
    ok(s, &cleared);
    assert!(cleared["assignee_user_id"].is_null());
    let uid = user_id(&olga, "Viktor").await;
    let (_, vid) = insert_document(&olga.db(&app), cid, "Order", "decision", "administrative", uid);
    let viktor = olga.switch("viktor").await;
    let (s, draft) = viktor
        .post(&format!("/api/cases/{cid}/decisions"), json!({"title":"Order","document_version_id":vid,"decision_date":today()}))
        .await;
    ok(s, &draft);
    let path = format!("/api/decisions/{}", draft["id"]);
    let (s, same) = viktor.patch(&path, json!({"version":draft["version"],"title":"Draft order"})).await;
    ok(s, &same);
    assert_eq!(same["decision_date"], today());
    let (s, cleared) = viktor.patch(&path, json!({"version":same["version"],"decision_date":null})).await;
    ok(s, &cleared);
    assert!(cleared["decision_date"].is_null());
    let (s, b) = viktor.patch(&path, json!({"version":same["version"],"decision_date":today()})).await;
    err(s, &b, StatusCode::CONFLICT, "version_conflict");
}

#[tokio::test]
async fn c10_adjourn_requires_a_new_start_and_authorised_override_is_audited() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Adjourn conflicts").await;
    assign_judge(&olga, cid).await;
    let (_, refs) = olga.get("/api/ref").await;
    let room = refs["rooms"][0]["id"].as_i64().unwrap();
    let old = hearing(&olga, cid, "2030-11-17", Some(room), true).await;
    let conflict = hearing(&olga, cid, "2030-11-19", Some(room), true).await;
    let path = format!("/api/hearings/{}/adjourn", old["id"]);
    let mut body = json!({"starts_local":old["starts_local"],"ends_local":"2030-11-17T10:30","reason":"Witness unavailable","authorised_by":"Judge Viktor Hale"});
    let (s, b) = olga.post(&path, body.clone()).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    assert_eq!(b["error"]["message"], "Choose a new date or time");
    body["starts_local"] = conflict["starts_local"].clone();
    body["ends_local"] = conflict["ends_local"].clone();
    let (s, b) = olga.post(&path, body.clone()).await;
    err(s, &b, StatusCode::CONFLICT, "hearing_conflict");
    body["override_reason"] = json!("Urgent joint listing");
    let (s, b) = olga.post(&path, body.clone()).await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    let (_, unchanged) = olga.get(&format!("/api/hearings/{}", old["id"])).await;
    assert_eq!(unchanged, old, "failed adjournment rolls back the freed slot");
    grant(&olga, &app, "Olga", &["hearing.override_conflict"]).await;
    let (s, moved) = olga.post_idem(&path, "adjourn-override", body.clone()).await;
    ok(s, &moved);
    assert_eq!(moved["new"]["conflict_override"], 1);
    assert_eq!(moved["new"]["override_reason"], "Urgent joint listing");
    let (s, replay) = olga.post_idem(&path, "adjourn-override", body).await;
    ok(s, &replay);
    assert_eq!(moved, replay);
    assert_eq!(audit_count(&olga, &app, "hearing.conflict_override", cid), 1);
    assert_eq!(audit_count(&olga, &app, "hearing.adjourned", cid), 1);
    // Outcomes can also book a continuation with an explicit conflict override.
    let viktor = olga.switch("viktor").await;
    grant(&olga, &app, "Viktor", &["hearing.schedule"]).await;
    let mut body = json!({"held":true,"outcome_summary":"First part heard","next_hearing":{
        "starts_local":conflict["starts_local"],"ends_local":conflict["ends_local"],"override_reason":"Continuation heard jointly"}});
    let path = format!("/api/hearings/{}/outcome", moved["new"]["id"]);
    let (s, b) = viktor.post(&path, body.clone()).await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    grant(&olga, &app, "Viktor", &["hearing.override_conflict"]).await;
    body["next_hearing"]["override_reason"] = json!("  ");
    let (s, b) = viktor.post(&path, body.clone()).await;
    err(s, &b, StatusCode::CONFLICT, "hearing_conflict");
    body["next_hearing"]["override_reason"] = json!("Continuation heard jointly");
    let (s, b) = viktor.post(&path, body).await;
    ok(s, &b);
    assert_eq!(b["next_hearing"]["override_reason"], "Continuation heard jointly");
    assert_eq!(audit_count(&olga, &app, "hearing.conflict_override", cid), 2);
}

#[tokio::test]
async fn next_action_links_templates_reports_and_dispatch_wording_match_the_contract() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Correspondence").await;
    let (_, card) = olga.get(&format!("/api/cases/{cid}")).await;
    assert_eq!(card["next_actions"][0]["link"], format!("/cases/{cid}?tab=summary&action=assign-judge"));
    let judge = assign_judge(&olga, cid).await;
    let old = hearing(&olga, cid, "2026-11-17", None, true).await;
    let party = card["participants"][0]["party_id"].as_i64().unwrap();
    let (s, notice) = olga
        .post(
            &format!("/api/cases/{cid}/dispatches"),
            json!({"kind":"notice",
        "hearing_id":old["id"],"template_code":"hearing_notice","recipient_party_id":party,"method":"email"}),
        )
        .await;
    ok(s, &notice);
    assert!(notice["body"].as_str().unwrap().contains("Tuesday 17 November 2026 at 09:00"));
    assert!(!notice["body"].as_str().unwrap().contains("Registry Registry"));
    let dispatch = notice["id"].as_i64().unwrap();
    let (s, b) = olga.post(&format!("/api/dispatches/{dispatch}/preview"), json!({})).await;
    ok(s, &b);
    let (s, b) = olga.post(&format!("/api/dispatches/{dispatch}/queue"), json!({})).await;
    ok(s, &b);
    tuvalu_court::outbox::process(&olga.db(&app)).unwrap();
    let (_, card) = olga.get(&format!("/api/cases/{cid}")).await;
    let action = card["next_actions"].as_array().unwrap().iter().find(|a| a["code"] == "confirm_delivery").unwrap();
    assert_eq!(action["message"], "Confirm that the hearing notice for Tue 17 Nov 2026 reached Alexei Fenwick.");
    assert_eq!(action["link"], format!("/cases/{cid}?tab=dispatch&dispatch={dispatch}"));
    let (_, history) = olga.get(&format!("/api/cases/{cid}/history")).await;
    let events = history["events"].as_array().unwrap();
    assert!(events.iter().any(|e| e["summary"] == "Notice to Alexei Fenwick reviewed"));
    assert!(events.iter().any(|e| e["summary"] == "Notice to Alexei Fenwick delivered to the local mailbox (alexei@example.invalid)"));
    let (s, moved) = olga
        .post(
            &format!("/api/hearings/{}/adjourn", old["id"]),
            json!({
        "starts_local":"2026-11-19T09:00","ends_local":"2026-11-19T10:00","reason":"Witness absent","authorised_by":"Judge"}),
        )
        .await;
    ok(s, &moved);
    let (s, b) = olga
        .post(
            &format!("/api/cases/{cid}/dispatches"),
            json!({"kind":"notice",
        "hearing_id":moved["new"]["id"],"template_code":"hearing_rescheduled","recipient_party_id":party,"method":"hand"}),
        )
        .await;
    ok(s, &b);
    assert!(b["body"].as_str().unwrap().contains("Tuesday 17 November 2026 at 09:00"));
    assert!(b["body"].as_str().unwrap().contains("Thursday 19 November 2026 at 09:00"));
    let (_, vid) = insert_document(&olga.db(&app), cid, "Repair order", "decision", "administrative", judge);
    let viktor = olga.switch("viktor").await;
    let (s, draft) = viktor.post(&format!("/api/cases/{cid}/decisions"), json!({"title":"Repair order","document_version_id":vid})).await;
    ok(s, &draft);
    let (s, b) = viktor.post(&format!("/api/decisions/{}/finalise", draft["id"]), json!({"decision_date":today(),"version":draft["version"],"document_version_id":draft["document_version_id"]})).await;
    ok(s, &b);
    let (_, card) = olga.get(&format!("/api/cases/{cid}")).await;
    let action = card["next_actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["code"] == "send_decision" && a["message"].as_str().unwrap().ends_with("Alexei Fenwick."))
        .unwrap();
    assert_eq!(action["message"], "Send a copy of the decision “Repair order” to Alexei Fenwick.");
    assert_eq!(action["link"], format!("/cases/{cid}?tab=dispatch&action=copies&decision={}&party={party}", draft["id"]));
    let (_, extra_version) = insert_document(&olga.db(&app), cid, "Attachment", "evidence", "party_material", judge);
    let maria = card["participants"][1]["party_id"].as_i64().unwrap();
    let (s, package) = olga
        .post(
            &format!("/api/cases/{cid}/dispatches"),
            json!({"kind":"copies",
        "recipient_party_id":maria,"method":"email","version_ids":[vid,extra_version]}),
        )
        .await;
    ok(s, &package);
    let package_id = package["id"].as_i64().unwrap();
    let (s, b) = olga.post(&format!("/api/dispatches/{package_id}/preview"), json!({})).await;
    ok(s, &b);
    let (s, b) = olga.post(&format!("/api/dispatches/{package_id}/queue"), json!({})).await;
    ok(s, &b);
    tuvalu_court::outbox::process(&olga.db(&app)).unwrap();
    let (_, history) = olga.get(&format!("/api/cases/{cid}/history")).await;
    assert!(
        history["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["summary"] == "Copy package for Maria Calder (2 documents) sent (maria@example.invalid)")
    );
    // Case-closing blockers name the hearing date and recipient instead of the e-mail subject.
    let (s, b) = olga.post(&format!("/api/cases/{cid}/close"), json!({"basis":"decided"})).await;
    err(s, &b, StatusCode::CONFLICT, "open_items");
    let items = b["error"]["details"]["items"].as_array().unwrap();
    assert!(!items.iter().any(|i| i["kind"] == "unconfirmed_dispatch" && i["label"] == "Hearing notice for Tue 17 Nov 2026 → Alexei Fenwick"), "items: {items:?}");
    assert!(items.iter().any(|i| i["kind"] == "dispatch" && i["label"] == "Hearing notice for Thu 19 Nov 2026 → Alexei Fenwick"), "items: {items:?}");
    assert!(items.iter().any(|i| i["kind"] == "unconfirmed_dispatch" && i["label"] == "Copy package → Maria Calder"), "items: {items:?}");
    assert!(items.iter().all(|i| i["recipient"].is_null() && i["dispatch_kind"].is_null() && i["hearing_starts"].is_null()));
    let (_, rows) = olga.get("/api/reports/new_cases/items").await;
    let row = rows["rows"].as_array().unwrap().iter().find(|r| r["id"] == cid).unwrap();
    assert_eq!(row["category"], "civil_contract");
    assert_eq!(row["category_label"], "Civil — contract");
    assert_eq!(row["status_label"], "Registered");
    let (_, rows) = olga.get("/api/reports/without_next_step/items").await;
    assert!(rows["rows"].as_array().unwrap().iter().all(|r| r["id"] != cid));
    for row in rows["rows"].as_array().unwrap() {
        let (_, card) = olga.get(&format!("/api/cases/{}", row["id"])).await;
        assert!(card["next_actions"].as_array().unwrap().iter().any(|a| a["code"] == "plan_next_step"));
    }
}
