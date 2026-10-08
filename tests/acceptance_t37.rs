mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};

async fn get(c: &Client, path: &str) -> Value {
    let (status, body) = c.get(path).await;
    assert_eq!(status, StatusCode::OK, "GET {path}: {body}");
    body
}

async fn post(c: &Client, path: &str, key: &str, body: Value) -> Value {
    let (status, result) = c.post_idem(path, key, body).await;
    assert_eq!(status, StatusCode::OK, "POST {path}: {result}");
    result
}

fn metric(summary: &Value, key: &str) -> i64 {
    summary["metrics"].as_array().unwrap().iter().find(|m| m["key"] == key).unwrap()["count"].as_i64().unwrap()
}

async fn preview_queue(c: &Client, dispatch: &Value, key: &str) {
    let id = dispatch["id"].as_i64().unwrap();
    let reviewed = post(c, &format!("/api/dispatches/{id}/preview"), &format!("{key}-preview"), json!({})).await;
    assert_eq!(reviewed["id"], id);
    assert_eq!(reviewed["status"], "draft");
    assert!(reviewed["reviewed_at"].is_string());
    assert_eq!(reviewed["body"], dispatch["body"]);
    assert_eq!(reviewed["items"], dispatch["items"]);
    let queued = post(c, &format!("/api/dispatches/{id}/queue"), key, json!({})).await;
    assert_eq!(queued["id"], id);
    assert_eq!(queued["status"], "queued");
}

async fn mailbox_item(c: &Client, dispatch: &Value, number: &str) -> Value {
    let id = dispatch["id"].as_i64().unwrap();
    let sent = get(c, &format!("/api/dispatches/{id}")).await;
    assert_eq!(sent["status"], "sent");
    let mailbox = get(c, &format!("/api/mailbox?dispatch_id={id}")).await;
    let items = mailbox["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "mailbox for dispatch {id}: {mailbox}");
    let mail = &items[0];
    assert_eq!(mail["dispatch_id"], id);
    assert_eq!(mail["case_number"], number);
    assert_eq!(mail["body"], dispatch["body"]);
    assert_eq!(sent["mailbox_ids"], json!([mail["id"]]));
    assert_eq!(get(c, &format!("/api/mailbox/{}", mail["id"])).await, *mail);
    mail.clone()
}

async fn confirm_receipt(c: &Client, dispatch: &Value, key: &str, date: &str) {
    let id = dispatch["id"].as_i64().unwrap();
    let confirmed = post(
        c,
        &format!("/api/dispatches/{id}/confirm"),
        key,
        json!({"kind":"human_handover","note":"DEMO recipient confirmed receipt", "occurred_date":date}),
    )
    .await;
    assert_eq!(confirmed["status"], "sent");
    assert!(
        confirmed["confirmations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "human_handover" && r["occurred_date"] == date)
    );
}

#[tokio::test]
async fn t37_new_intake_through_full_chain_changes_reports() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let elena = olga.switch("elena").await;
    let refs = get(&olga, "/api/ref").await;
    let date = refs["today"].as_str().unwrap();
    let registry = refs["registries"].as_array().unwrap().iter().find(|r| r["series"] == "DEMO-CIV").unwrap()["id"]
        .as_i64()
        .unwrap();
    let room = refs["rooms"].as_array().unwrap().iter().find(|r| r["name"] == "DEMO Courtroom 1").unwrap()["id"]
        .as_i64()
        .unwrap();
    let staff_id = |name: &str| {
        refs["staff"].as_array().unwrap().iter().find(|u| u["display_name"] == name).unwrap()["id"]
            .as_i64()
            .unwrap()
    };
    let (olga_id, viktor_id, sergei_id) = (staff_id("Olga Marsh"), staff_id("Viktor Hale"), staff_id("Sergei Novak"));
    let report_path = format!("/api/reports/summary?from={date}&to={date}&as_of={date}");
    let before = get(&elena, &report_path).await;

    // A fresh filing is incomplete and has no case or allocated case number yet.
    let intake = post(
        &olga,
        "/api/intakes",
        "t37-intake",
        json!({
            "sender_name":"DEMO Nalia Reef", "channel":"counter", "origin_island":"funafuti",
            "received_date":date, "description":"DEMO boat repair claim; agreement copy missing",
            "is_paper_original":true, "paper_location":"DEMO T37 cabinet, folder 1"
        }),
    )
    .await;
    let intake_id = intake["id"].as_i64().unwrap();
    let reference = intake["reference"].as_str().unwrap();
    assert!(!reference.is_empty());
    let intake_path = format!("/api/intakes/{intake_id}");
    let received = get(&olga, &intake_path).await;
    assert_eq!(received["intake"]["status"], "received");
    assert!(received["intake"]["case_id"].is_null());
    assert!(received["case"].is_null());
    let requested = post(
        &olga,
        &format!("{intake_path}/request-info"),
        "t37-request-info",
        json!({
            "missing_items":"DEMO signed repair agreement copy", "method":"email", "address":"nalia.t37@example.invalid"
        }),
    )
    .await;
    assert_eq!(requested["ok"], true);
    let incomplete = get(&olga, &intake_path).await;
    assert_eq!(incomplete["intake"]["status"], "needs_information");
    assert_eq!(incomplete["dispatches"][0]["id"], requested["dispatch_id"]);
    assert_eq!(incomplete["dispatches"][0]["status"], "draft");
    let supplement = post(
        &olga,
        &format!("{intake_path}/supplement"),
        "t37-supplement",
        json!({
            "sender_name":"DEMO Nalia Reef", "channel":"post", "received_date":date,
            "description":"DEMO signed repair agreement supplied"
        }),
    )
    .await;
    let supplement_id = supplement["id"].as_i64().unwrap();
    assert_ne!(supplement_id, intake_id);
    let supplemented = get(&olga, &intake_path).await;
    assert_eq!(supplemented["intake"]["status"], "received");
    assert_eq!(supplemented["related_intakes"][0]["id"], supplement_id);
    let evidence_bytes = pdf("DEMO T37 signed boat repair agreement");
    let (status, evidence) = olga
        .upload_idem(
            &format!("/api/intakes/{supplement_id}/documents"),
            "t37-evidence",
            &[
                ("title", "DEMO T37 repair agreement"),
                ("doc_type", "evidence"),
                ("source", "party"),
                ("visibility", "party_material"),
            ],
            "DEMO_T37_agreement.pdf",
            &evidence_bytes,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "supplement upload: {evidence}");
    let evidence_id = evidence["id"].as_i64().unwrap();
    let evidence_version = evidence["versions"][0]["id"].as_i64().unwrap();
    assert_eq!(evidence["intake_id"], supplement_id);
    assert_eq!(evidence["versions"][0]["scan_status"], "clean");
    let ready = post(&olga, &format!("{intake_path}/mark-ready"), "t37-ready", json!({"note":"DEMO package checked"})).await;
    assert_eq!(ready["status"], "ready_for_registration");
    let registered = post(&olga, &format!("{intake_path}/register"), "t37-register", json!({
        "registry_id":registry, "category":"civil_contract", "title":"DEMO T37 Reef v Lagoon boat repair",
        "participants":[
            {"new_party":{"kind":"person","name":"DEMO Nalia Reef","contact_email":"nalia.t37@example.invalid"},"role":"claimant","service_contact":"nalia.t37@example.invalid"},
            {"new_party":{"kind":"person","name":"DEMO Tomas Lagoon","contact_email":"tomas.t37@example.invalid"},"role":"respondent","service_contact":"tomas.t37@example.invalid"}
        ]
    })).await;
    let case_id = registered["case_id"].as_i64().unwrap();
    let number = registered["number"].as_str().unwrap();
    assert!(number.starts_with(&format!("DEMO-CIV-{}-", &date[..4])));
    assert!(number.rsplit('-').next().unwrap().parse::<i64>().unwrap() > 0);
    let case_path = format!("/api/cases/{case_id}");
    for id in [intake_id, supplement_id] {
        let linked = get(&olga, &format!("/api/intakes/{id}")).await;
        assert_eq!(linked["intake"]["status"], "linked_to_case");
        assert_eq!(linked["intake"]["case_id"], case_id);
        assert_eq!(linked["case"]["number"], number);
    }
    let attached = get(&olga, &format!("/api/documents/{evidence_id}")).await;
    assert_eq!(attached["case_id"], case_id);
    assert_eq!(attached["versions"][0]["id"], evidence_version);
    let card = get(&olga, &case_path).await;
    assert_eq!(card["case"]["number"], number);
    assert_eq!(card["case"]["status"], "registered");
    assert_eq!(card["case"]["registered_date"], date);
    assert_eq!(card["intakes"].as_array().unwrap().len(), 2);
    let parties = card["participants"].as_array().unwrap();
    assert_eq!(parties.len(), 2);
    let claimant = parties.iter().find(|p| p["role"] == "claimant").unwrap()["party_id"].as_i64().unwrap();
    let respondent = parties.iter().find(|p| p["role"] == "respondent").unwrap()["party_id"].as_i64().unwrap();
    let after_registration = get(&elena, &report_path).await;
    assert_eq!(metric(&after_registration, "new_cases"), metric(&before, "new_cases") + 1);
    assert_eq!(metric(&after_registration, "open_as_of"), metric(&before, "open_as_of") + 1);
    assert_eq!(metric(&after_registration, "closed_cases"), metric(&before, "closed_cases"));

    post(
        &elena,
        &format!("{case_path}/assignments"),
        "t37-judge",
        json!({"user_id":viktor_id,"role":"judge","reason":"DEMO T37 allocation"}),
    )
    .await;
    post(
        &elena,
        &format!("{case_path}/assignments"),
        "t37-service-officer",
        json!({"user_id":sergei_id,"role":"service_officer","reason":"DEMO T37 notices and decision copies"}),
    )
    .await;
    let assigned = get(&elena, &case_path).await;
    assert_eq!(assigned["case"]["responsible_user_id"], olga_id);
    for (user, role) in [(olga_id, "clerk"), (viktor_id, "judge"), (sergei_id, "service_officer")] {
        assert!(
            assigned["assignments"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["user_id"] == user && a["role"] == role && a["end_at"].is_null())
        );
    }
    let sergei = olga.switch("sergei").await;
    let viktor = olga.switch("viktor").await;
    let hearing = post(
        &olga,
        &format!("{case_path}/hearings"),
        "t37-hearing",
        json!({
            "hearing_type":"hearing", "confirm":true, "starts_local":format!("{date}T08:00"), "ends_local":format!("{date}T09:00"), "room_id":room,
            "participants":[{"party_id":claimant,"role":"claimant"},{"party_id":respondent,"role":"respondent"}]
        }),
    )
    .await;
    let hearing_id = hearing["id"].as_i64().unwrap();
    assert_eq!(hearing["status"], "scheduled");
    assert_eq!(hearing["starts_local"], format!("{date}T08:00"));
    assert_eq!(hearing["ends_local"], format!("{date}T09:00"));
    assert_eq!(hearing["room_id"], room);
    assert_eq!(hearing["judge_user_id"], viktor_id);
    assert_eq!(hearing["conflict_override"], 0);
    let notice_body = json!({"kind":"notice","hearing_id":hearing_id,"template_code":"hearing_notice","recipient_party_id":claimant,"method":"email"});
    let notice = post(&sergei, &format!("{case_path}/dispatches"), "t37-notice", notice_body.clone()).await;
    assert_eq!(notice["status"], "draft");
    assert_eq!(notice["hearing_id"], hearing_id);
    assert!(notice["body"].as_str().unwrap().contains(&tuvalu_court::time::human_court_local(&format!("{date}T08:00"),false)), "notice: {notice}");
    preview_queue(&sergei, &notice, "t37-notice-queue").await;
    // The harness drops the background receiver: this is the existing worker trigger,
    // not a fixture write. All domain commands and observations use HTTP.
    tuvalu_court::outbox::process(&olga.db(&app)).unwrap();
    let first_mail = mailbox_item(&sergei, &notice, number).await;
    assert_eq!(first_mail["to_address"], "nalia.t37@example.invalid");
    confirm_receipt(&sergei, &notice, "t37-first-receipt", date).await;

    let stale = post(&sergei, &format!("{case_path}/dispatches"), "t37-stale-notice", notice_body).await;
    preview_queue(&sergei, &stale, "t37-stale-queue").await;
    let stale_id = stale["id"].as_i64().unwrap();
    assert!(get(&sergei, &format!("/api/mailbox?dispatch_id={stale_id}")).await["items"].as_array().unwrap().is_empty());
    let moved = post(
        &olga,
        &format!("/api/hearings/{hearing_id}/adjourn"),
        "t37-adjourn",
        json!({
            "starts_local":format!("{date}T10:00"), "ends_local":format!("{date}T11:00"), "room_id":room,
            "reason":"DEMO witness unavailable on the original date", "authorised_by":"Viktor Hale"
        }),
    )
    .await;
    let new_hearing = &moved["new"];
    let new_hearing_id = new_hearing["id"].as_i64().unwrap();
    assert_ne!(new_hearing_id, hearing_id);
    assert_eq!(moved["old"]["status"], "adjourned");
    assert_eq!(moved["old"]["adjourned_to_id"], new_hearing_id);
    assert_eq!(new_hearing["previous_hearing_id"], hearing_id);
    assert_eq!(new_hearing["status"], "scheduled");
    assert_eq!(new_hearing["starts_local"], format!("{date}T10:00"));
    assert_eq!(new_hearing["ends_local"], format!("{date}T11:00"));
    assert_eq!(new_hearing["room_id"], room);
    assert_eq!(new_hearing["conflict_override"], 0);
    assert_eq!(get(&sergei, &format!("/api/dispatches/{stale_id}")).await["status"], "superseded");
    tuvalu_court::outbox::process(&olga.db(&app)).unwrap();
    assert!(get(&sergei, &format!("/api/mailbox?dispatch_id={stale_id}")).await["items"].as_array().unwrap().is_empty());
    assert_eq!(mailbox_item(&sergei, &notice, number).await, first_mail);
    let fresh = post(
        &sergei,
        &format!("{case_path}/dispatches"),
        "t37-new-notice",
        json!({
            "kind":"notice", "hearing_id":new_hearing_id, "template_code":"hearing_rescheduled", "recipient_party_id":claimant, "method":"email"
        }),
    )
    .await;
    assert_eq!(fresh["status"], "draft");
    assert_eq!(fresh["hearing_id"], new_hearing_id);
    assert!(fresh["body"].as_str().unwrap().contains(&tuvalu_court::time::human_court_local(&format!("{date}T10:00"),false)), "new notice: {fresh}");
    preview_queue(&sergei, &fresh, "t37-new-notice-queue").await;
    tuvalu_court::outbox::process(&olga.db(&app)).unwrap();
    mailbox_item(&sergei, &fresh, number).await;
    confirm_receipt(&sergei, &fresh, "t37-new-receipt", date).await;
    let tasks = moved["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    for task in tasks {
        assert_eq!(task["kind"], "renotify");
        assert_eq!(task["hearing_id"], new_hearing_id);
        assert_eq!(task["assignee_user_id"], sergei_id);
        let id = task["id"].as_i64().unwrap();
        let current = get(&sergei, &format!("/api/tasks/{id}")).await;
        if current["status"] == "done" {
            assert_eq!(current["result"],format!("completed automatically: notice #{} queued for the new hearing",fresh["id"]));
            continue;
        }
        let completed = post(
            &sergei,
            &format!("/api/tasks/{id}/complete"),
            &format!("t37-task-{id}"),
            json!({
                "version":task["version"], "result":"DEMO new hearing date communicated to participant"
            }),
        )
        .await;
        assert_eq!(completed["status"], "done");
    }

    let attendance: Vec<Value> = new_hearing["participants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| json!({"participant_id":p["id"],"attended":true}))
        .collect();
    let outcome = post(
        &viktor,
        &format!("/api/hearings/{new_hearing_id}/outcome"),
        "t37-outcome",
        json!({
            "held":true, "attendance":attendance, "outcome_summary":"DEMO parties heard; claim allowed", "next_step":"DEMO written decision and copies"
        }),
    )
    .await;
    assert_eq!(outcome["hearing"]["status"], "held");
    assert_eq!(outcome["hearing"]["outcome_summary"], "DEMO parties heard; claim allowed");
    assert_eq!(outcome["hearing"]["outcome_recorded_by_name"], "Viktor Hale");
    assert!(outcome["hearing"]["participants"].as_array().unwrap().iter().all(|p| p["attended"] == 1));
    if new_hearing["starts_at"].as_str().unwrap() > tuvalu_court::time::now_utc().as_str() {
        assert_eq!(outcome["demo_note"], "Recorded ahead of the hearing time (demo only)");
    }
    let decision_bytes = pdf("DEMO T37 final decision: boat repair claim allowed");
    let (status, document) = viktor
        .upload_idem(
            &format!("{case_path}/documents"),
            "t37-decision-document",
            &[
                ("title", "DEMO T37 boat repair decision"),
                ("doc_type", "decision"),
                ("source", "court"),
                ("visibility", "party_material"),
                ("document_date", date),
            ],
            "DEMO_T37_decision.pdf",
            &decision_bytes,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "decision upload: {document}");
    let document_id = document["id"].as_i64().unwrap();
    let document_version_id = document["versions"][0]["id"].as_i64().unwrap();
    assert_eq!(document["case_id"], case_id);
    assert_eq!(document["versions"][0]["scan_status"], "clean");
    let draft = post(
        &viktor,
        &format!("{case_path}/decisions"),
        "t37-draft",
        json!({
            "title":"DEMO T37 boat repair decision", "hearing_id":new_hearing_id, "document_version_id":document_version_id
        }),
    )
    .await;
    let decision_id = draft["id"].as_i64().unwrap();
    assert_eq!(draft["status"], "draft");
    assert_eq!(draft["document_id"], document_id);
    assert_eq!(draft["document_version_id"], document_version_id);
    assert_eq!(draft["hearing_id"], new_hearing_id);
    let reviewed = get(&viktor, &format!("/api/decisions/{decision_id}")).await;
    let finalised = post(
        &viktor,
        &format!("/api/decisions/{decision_id}/finalise"),
        "t37-finalise",
        json!({
            "decision_date":date, "version":reviewed["version"], "document_version_id":reviewed["document_version_id"], "signed_file_uploaded":true
        }),
    )
    .await;
    assert_eq!(finalised["status"], "finalised");
    assert_eq!(finalised["document_version_id"], document_version_id);
    assert_eq!(finalised["finalised_by_name"], "Viktor Hale");
    assert_eq!(finalised["decision_date"], date);
    let copy = post(
        &sergei,
        &format!("{case_path}/dispatches"),
        "t37-decision-copy",
        json!({
            "kind":"decision_copy", "version_ids":[finalised["document_version_id"]], "recipient_party_id":respondent, "method":"email"
        }),
    )
    .await;
    assert_eq!(copy["status"], "draft");
    assert_eq!(copy["recipient_party_id"], respondent);
    assert_eq!(copy["items"].as_array().unwrap().len(), 1);
    assert_eq!(copy["items"][0]["material_kind"], "decision_copy");
    assert_eq!(copy["items"][0]["document_version_id"], document_version_id);
    preview_queue(&sergei, &copy, "t37-copy-queue").await;
    tuvalu_court::outbox::process(&olga.db(&app)).unwrap();
    let copy_mail = mailbox_item(&sergei, &copy, number).await;
    assert_eq!(copy_mail["to_address"], "tomas.t37@example.invalid");
    assert_eq!(copy_mail["attachments"].as_array().unwrap().len(), 1);
    assert_eq!(copy_mail["attachments"][0]["document_version_id"], finalised["document_version_id"]);
    assert_eq!(copy_mail["attachments"][0]["material_kind"], "decision_copy");
    assert_eq!(copy_mail["attachments"][0]["sha256"], document["versions"][0]["sha256"]);
    confirm_receipt(&sergei, &copy, "t37-copy-receipt", date).await;
    let closed = post(
        &olga,
        &format!("{case_path}/close"),
        "t37-close",
        json!({
            "basis":"decided", "basis_decision_id":decision_id, "closed_date":date, "note":"DEMO decision issued and registry work complete"
        }),
    )
    .await;
    assert_eq!(closed["ok"], true);
    assert_eq!(closed["closed_date"], date);
    let closed_card = get(&olga, &case_path).await;
    assert_eq!(closed_card["case"]["status"], "closed");
    assert_eq!(closed_card["case"]["basis_decision_id"], decision_id);
    assert_eq!(closed_card["case"]["closure_basis"], "decided");
    assert_eq!(closed_card["case"]["closed_date"], date);
    assert!(closed_card["next_actions"].as_array().unwrap().is_empty());
    assert_eq!(
        closed_card["status_history"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["to_status"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["registered", "closed"]
    );
    for (version, bytes) in [(evidence_version, &evidence_bytes), (document_version_id, &decision_bytes)] {
        let (status, _, downloaded) = olga.get_bytes(&format!("/api/document-versions/{version}/download")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&downloaded, bytes);
    }
    assert_eq!(get(&sergei, &format!("/api/dispatches/{stale_id}")).await["status"], "superseded");
    assert!(get(&sergei, &format!("/api/mailbox?dispatch_id={stale_id}")).await["items"].as_array().unwrap().is_empty());

    let after = get(&elena, &report_path).await;
    assert_eq!(metric(&after, "new_cases"), metric(&before, "new_cases") + 1);
    assert_eq!(metric(&after, "closed_cases"), metric(&before, "closed_cases") + 1);
    assert_eq!(metric(&after, "open_as_of"), metric(&before, "open_as_of"));
    let closed_metric = after["metrics"].as_array().unwrap().iter().find(|m| m["key"] == "closed_cases").unwrap();
    let drilldown = get(&elena, closed_metric["drilldown"].as_str().unwrap()).await;
    let row = drilldown["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == case_id)
        .expect("new case in closed drill-down");
    assert_eq!(row["number"], number);
    assert_eq!(row["event_date"], date);
    assert_eq!(row["closure_basis"], "decided");

    // History must carry pre-registration intake events into this case and preserve
    // the complete chain, including repeated review/queue/delivery steps, in order.
    let history = get(&olga, &format!("{case_path}/history")).await;
    let events = history["events"].as_array().unwrap();
    assert!(events.windows(2).all(|w| w[0]["id"].as_i64().unwrap() < w[1]["id"].as_i64().unwrap()));
    let expected = [
        ("intake.received", "Olga Marsh"),
        ("intake.information_requested", "Olga Marsh"),
        ("intake.supplemented", "Olga Marsh"),
        ("document.uploaded", "Olga Marsh"),
        ("intake.ready", "Olga Marsh"),
        ("case.registered", "Olga Marsh"),
        ("case.assigned", "Elena Brooks"),
        ("case.assigned", "Elena Brooks"),
        ("hearing.created", "Olga Marsh"),
        ("dispatch.prepared", "Sergei Novak"),
        ("dispatch.reviewed", "Sergei Novak"),
        ("dispatch.queued", "Sergei Novak"),
        ("dispatch.sent", "Sergei Novak"),
        ("dispatch.confirmed", "Sergei Novak"),
        ("dispatch.prepared", "Sergei Novak"),
        ("dispatch.reviewed", "Sergei Novak"),
        ("dispatch.queued", "Sergei Novak"),
        ("hearing.created", "Olga Marsh"),
        ("task.created", "Olga Marsh"),
        ("task.created", "Olga Marsh"),
        ("hearing.adjourned", "Olga Marsh"),
        ("dispatch.superseded", "Olga Marsh"),
        ("dispatch.prepared", "Sergei Novak"),
        ("dispatch.reviewed", "Sergei Novak"),
        ("dispatch.queued", "Sergei Novak"),
        ("task.completed", "Sergei Novak"),
        ("dispatch.sent", "Sergei Novak"),
        ("dispatch.confirmed", "Sergei Novak"),
        ("task.completed", "Sergei Novak"),
        ("hearing.outcome_recorded", "Viktor Hale"),
        ("document.uploaded", "Viktor Hale"),
        ("decision.drafted", "Viktor Hale"),
        ("decision.finalised", "Viktor Hale"),
        ("dispatch.prepared", "Sergei Novak"),
        ("dispatch.reviewed", "Sergei Novak"),
        ("dispatch.queued", "Sergei Novak"),
        ("dispatch.sent", "Sergei Novak"),
        ("dispatch.confirmed", "Sergei Novak"),
        ("case.closed", "Olga Marsh"),
    ];
    let selected: Vec<_> = events.iter().filter(|e| expected.iter().any(|(action, _)| e["action"] == *action)).collect();
    let actual: Vec<_> = selected.iter().map(|e| (e["action"].as_str().unwrap(), e["user_name"].as_str().unwrap())).collect();
    assert_eq!(actual, expected, "case history: {history}");
    for event in &selected[..3] {
        assert!(event["summary"].as_str().unwrap().contains(reference), "pre-registration event linked to intake: {event}");
    }
}
