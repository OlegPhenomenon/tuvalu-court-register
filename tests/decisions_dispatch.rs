mod common;
use axum::http::StatusCode;
use common::*;
use rusqlite::params;
use serde_json::{Value, json};
use tuvalu_court::db::Db;

async fn posted(c: &Client, path: &str, body: Value) -> Value {
    let (s, b) = c.post(path, body).await;
    ok(s, &b);
    b
}
async fn fetched(c: &Client, path: &str) -> Value {
    let (s, b) = c.get(path).await;
    ok(s, &b);
    b
}
async fn denied(c: &Client, path: &str, body: Value, status: StatusCode, code: &str) {
    let (s, b) = c.post(path, body).await;
    err(s, &b, status, code);
}
async fn assigned(olga: &Client, case_id: i64) -> Client {
    let elena = olga.switch("elena").await;
    let uid = user_id(&elena, "Viktor").await;
    posted(
        &elena,
        &format!("/api/cases/{case_id}/assignments"),
        json!({"user_id":uid,"role":"judge","reason":"Allocated for this hearing"}),
    )
    .await;
    olga.switch("viktor").await
}
async fn draft(c: &Client, case_id: i64, vid: i64) -> Value {
    posted(
        c,
        &format!("/api/cases/{case_id}/decisions"),
        json!({"title":"Decision DEMO","document_version_id":vid}),
    )
    .await
}
async fn notice(c: &Client, case_id: i64, method: &str, address: &str) -> Value {
    posted(c, &format!("/api/cases/{case_id}/dispatches"), json!({"kind":"notice","recipient_name":"Alexei","method":method,"address":address,"subject":"Notice DEMO","body":"Please contact the registry."})).await
}
async fn preview_queue(c: &Client, id: i64, key: &str) -> Value {
    posted(c, &format!("/api/dispatches/{id}/preview"), json!({})).await;
    let (s, b) = c
        .post_idem(&format!("/api/dispatches/{id}/queue"), key, json!({}))
        .await;
    ok(s, &b);
    assert_eq!(b["status"], "queued");
    b
}
fn scalar(db: &Db, sql: &str, id: i64) -> i64 {
    db.open()
        .unwrap()
        .query_row(sql, [id], |r| r.get(0))
        .unwrap()
}

#[tokio::test]
async fn decision_finalisation_amendment_and_replay_are_atomic() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, number) = register_case(&olga, "Decision lifecycle DEMO").await;
    let db = olga.db(&app);
    let viktor = assigned(&olga, cid).await;
    let uid = user_id(&olga, "Viktor").await;
    let (_, vid) = insert_document(
        &db,
        cid,
        "Original ruling",
        "decision",
        "administrative",
        uid,
    );
    let d = draft(&viktor, cid, vid).await;
    let id = d["id"].as_i64().unwrap();
    assert_eq!(d["status"], "draft");
    assert_eq!(d["case_number"], number);
    assert_eq!(d["document_version_id"], vid);
    assert_eq!(d["signed_file_uploaded"], false);
    let path = format!("/api/decisions/{id}/finalise");
    let body = json!({"decision_date":today(),"signed_file_uploaded":true});
    denied(
        &olga,
        &path,
        body.clone(),
        StatusCode::FORBIDDEN,
        "forbidden",
    )
    .await;
    let (s, first) = viktor
        .post_idem(&path, "finalise-original", body.clone())
        .await;
    ok(s, &first);
    assert_eq!(first["status"], "finalised");
    assert_eq!(first["signed_file_uploaded"], true);
    assert!(
        first["finalised_by_name"]
            .as_str()
            .unwrap()
            .starts_with("Viktor")
    );
    assert_eq!(
        first["note"],
        "Finalised in this register. This is not a qualified electronic signature."
    );
    let (s, again) = viktor.post_idem(&path, "finalise-original", body).await;
    ok(s, &again);
    assert_eq!(first, again);
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM audit_events WHERE action='decision.finalised' AND entity_id=?1",
            id
        ),
        1
    );
    let (s, b) = viktor
        .post_idem(
            &path,
            "finalise-original",
            json!({"decision_date":"2026-01-01"}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    denied(
        &viktor,
        &path,
        json!({"decision_date":today()}),
        StatusCode::CONFLICT,
        "invalid_transition",
    )
    .await;
    let (s, b) = viktor
        .patch(
            &format!("/api/decisions/{id}"),
            json!({"version":first["version"],"title":"Changed"}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "invalid_transition");
    denied(
        &viktor,
        &format!("/api/decisions/{id}/withdraw"),
        json!({"reason":"Wrong text"}),
        StatusCode::CONFLICT,
        "invalid_transition",
    )
    .await;
    let (_, new_vid) = insert_document(
        &db,
        cid,
        "Corrected ruling",
        "decision",
        "administrative",
        uid,
    );
    assert!(
        db.open()
            .unwrap()
            .execute(
                "UPDATE decisions SET document_version_id=?2 WHERE id=?1",
                params![id, new_vid]
            )
            .is_err()
    );
    denied(
        &viktor,
        &format!("/api/decisions/{id}/amend"),
        json!({"amendment_basis":" ","document_version_id":new_vid}),
        StatusCode::BAD_REQUEST,
        "validation",
    )
    .await;
    let amended = posted(&viktor, &format!("/api/decisions/{id}/amend"), json!({"amendment_basis":"Correct a transcription error","document_version_id":new_vid,"title":"Corrected decision"})).await;
    let new_id = amended["id"].as_i64().unwrap();
    assert_ne!(id, new_id);
    assert_eq!(amended["amends_decision_id"], id);
    assert_eq!(amended["status"], "draft");
    assert_eq!(
        fetched(&viktor, &format!("/api/decisions/{id}")).await["status"],
        "finalised"
    );
    // Another amendment can exist as a draft, but it cannot supersede the original twice.
    let sibling = posted(
        &viktor,
        &format!("/api/decisions/{id}/amend"),
        json!({"amendment_basis":"Alternative correction","document_version_id":new_vid}),
    )
    .await;
    posted(
        &viktor,
        &format!("/api/decisions/{new_id}/finalise"),
        json!({"decision_date":today()}),
    )
    .await;
    let old = fetched(&viktor, &format!("/api/decisions/{id}")).await;
    assert_eq!(old["status"], "superseded");
    assert_eq!(old["superseded_by_id"], new_id);
    assert_eq!(old["document_version_id"], vid);
    denied(
        &viktor,
        &format!("/api/decisions/{}/finalise", sibling["id"]),
        json!({"decision_date":today()}),
        StatusCode::CONFLICT,
        "invalid_transition",
    )
    .await;
    assert_eq!(
        fetched(&viktor, &format!("/api/decisions/{}", sibling["id"])).await["status"],
        "draft"
    );
    assert_eq!(
        tuvalu_court::audit::verify_chain(&db.open().unwrap())
            .unwrap()
            .1,
        None
    );
}

#[tokio::test]
async fn decision_edits_withdrawal_and_close_blockers() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Withdraw draft DEMO").await;
    let viktor = assigned(&olga, cid).await;
    let db = olga.db(&app);
    let uid = user_id(&olga, "Viktor").await;
    let (_, vid) = insert_document(&db, cid, "Draft ruling", "decision", "administrative", uid);
    let d = draft(&viktor, cid, vid).await;
    let id = d["id"].as_i64().unwrap();
    let (s, b) = viktor
        .patch(
            &format!("/api/decisions/{id}"),
            json!({"version":0,"title":"Revised"}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "version_conflict");
    assert_eq!(b["error"]["details"]["current"], d);
    let (_, vid2) = insert_document(
        &db,
        cid,
        "Revised ruling",
        "decision",
        "administrative",
        uid,
    );
    let (s,b) = viktor.patch(&format!("/api/decisions/{id}"), json!({"version":d["version"],"title":"Revised","decision_date":today(),"document_version_id":vid2})).await;
    ok(s, &b);
    assert_eq!(b["document_version_id"], vid2);
    assert_eq!(b["version"], 2);
    denied(
        &viktor,
        &format!("/api/decisions/{id}/amend"),
        json!({"amendment_basis":"x","document_version_id":vid}),
        StatusCode::CONFLICT,
        "invalid_transition",
    )
    .await;
    denied(
        &olga,
        &format!("/api/cases/{cid}/close"),
        json!({"basis":"decided"}),
        StatusCode::CONFLICT,
        "open_items",
    )
    .await;
    denied(
        &viktor,
        &format!("/api/decisions/{id}/withdraw"),
        json!({}),
        StatusCode::BAD_REQUEST,
        "validation",
    )
    .await;
    let b = posted(
        &viktor,
        &format!("/api/decisions/{id}/withdraw"),
        json!({"reason":"Superseded working draft"}),
    )
    .await;
    assert_eq!(b["status"], "withdrawn");
    assert_eq!(b["status_reason"], "Superseded working draft");
    denied(
        &viktor,
        &format!("/api/decisions/{id}/withdraw"),
        json!({"reason":"Again"}),
        StatusCode::CONFLICT,
        "invalid_transition",
    )
    .await;
    denied(
        &viktor,
        &format!("/api/decisions/{id}/finalise"),
        json!({"decision_date":today()}),
        StatusCode::CONFLICT,
        "invalid_transition",
    )
    .await;
    posted(
        &olga,
        &format!("/api/cases/{cid}/close"),
        json!({"basis":"settled","note":"Settled by parties"}),
    )
    .await;
}

#[tokio::test]
async fn decision_scope_and_document_checks() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Judge scope DEMO").await;
    let db = olga.db(&app);
    let viktor = olga.switch("viktor").await;
    let uid = user_id(&olga, "Viktor").await;
    let oid = user_id(&olga, "Olga").await;
    let (_, vid) = insert_document(&db, cid, "Ruling", "decision", "administrative", uid);
    denied(
        &viktor,
        &format!("/api/cases/{cid}/decisions"),
        json!({"title":"X","document_version_id":vid}),
        StatusCode::FORBIDDEN,
        "forbidden",
    )
    .await;
    let viktor = assigned(&olga, cid).await;
    denied(
        &olga,
        &format!("/api/cases/{cid}/decisions"),
        json!({"title":"X","document_version_id":vid}),
        StatusCode::FORBIDDEN,
        "forbidden",
    )
    .await;
    let (other, _) = register_case(&olga, "Other case DEMO").await;
    assigned(&olga, other).await;
    let (_, other_vid) = insert_document(
        &db,
        other,
        "Other ruling",
        "decision",
        "administrative",
        uid,
    );
    for body in [
        json!({"title":"X","document_version_id":other_vid}),
        json!({"title":"X","document_version_id":vid,"hearing_id":999999}),
        json!({"title":"X","document_version_id":vid,"decision_date":"2026-02-30"}),
        json!({"title":" ","document_version_id":vid}),
    ] {
        denied(
            &viktor,
            &format!("/api/cases/{cid}/decisions"),
            body,
            StatusCode::BAD_REQUEST,
            "validation",
        )
        .await;
    }
    let (_, restricted) =
        insert_document(&db, cid, "Private ruling", "decision", "restricted", oid);
    denied(
        &viktor,
        &format!("/api/cases/{cid}/decisions"),
        json!({"title":"X","document_version_id":restricted}),
        StatusCode::NOT_FOUND,
        "not_found",
    )
    .await;
    db.open()
        .unwrap()
        .execute(
            "UPDATE document_versions SET scan_status='quarantined' WHERE id=?1",
            [vid],
        )
        .unwrap();
    denied(
        &viktor,
        &format!("/api/cases/{cid}/decisions"),
        json!({"title":"X","document_version_id":vid}),
        StatusCode::BAD_REQUEST,
        "validation",
    )
    .await;
    db.open()
        .unwrap()
        .execute(
            "UPDATE document_versions SET scan_status='clean' WHERE id=?1",
            [vid],
        )
        .unwrap();
    let d = draft(&viktor, cid, vid).await;
    let id = d["id"].as_i64().unwrap();
    db.open()
        .unwrap()
        .execute(
            "UPDATE document_versions SET scan_status='quarantined' WHERE id=?1",
            [vid],
        )
        .unwrap();
    denied(
        &viktor,
        &format!("/api/decisions/{id}/finalise"),
        json!({"decision_date":today()}),
        StatusCode::BAD_REQUEST,
        "validation",
    )
    .await;
    assert_eq!(
        fetched(&viktor, &format!("/api/decisions/{id}")).await["status"],
        "draft"
    );
    db.open().unwrap().execute("UPDATE case_assignments SET end_at='2026-01-01T00:00:00Z' WHERE case_id=?1 AND user_id=?2",params![cid,uid]).unwrap();
    denied(
        &viktor,
        &format!("/api/decisions/{id}/finalise"),
        json!({"decision_date":today()}),
        StatusCode::FORBIDDEN,
        "forbidden",
    )
    .await;
    let pavel = olga.switch("pavel").await;
    let (s, b) = pavel.get(&format!("/api/decisions/{id}")).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    assert!(
        fetched(&pavel, "/api/decisions?status=draft").await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let stranger = app.persona("viktor").await;
    let (s, b) = stranger.get(&format!("/api/decisions/{id}")).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn copies_review_queue_mailbox_and_delivery_records() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, number) = register_case(&olga, "Copies DEMO").await;
    let viktor = assigned(&olga, cid).await;
    let db = olga.db(&app);
    let uid = user_id(&olga, "Olga").await;
    let (_, vid) = insert_document(&db, cid, "Copy ruling", "decision", "administrative", uid);
    let d = posted(&olga,&format!("/api/cases/{cid}/dispatches"),json!({"kind":"copies","recipient_name":"Maria","method":"email","address":"maria@example.invalid","body":"For your records.","version_ids":[vid,vid]})).await;
    let id = d["id"].as_i64().unwrap();
    assert_eq!(d["items"].as_array().unwrap().len(), 1);
    assert!(
        d["body"]
            .as_str()
            .unwrap()
            .contains("Copy ruling (version 1, Copy_ruling.pdf)")
    );
    assert_eq!(
        d["state_summary"],
        "Prepared — check recipient and contents"
    );
    denied(
        &olga,
        &format!("/api/dispatches/{id}/queue"),
        json!({}),
        StatusCode::CONFLICT,
        "review_required",
    )
    .await;
    denied(
        &olga,
        &format!("/api/dispatches/{id}/confirm"),
        json!({"kind":"human_handover","note":"Received"}),
        StatusCode::CONFLICT,
        "invalid_transition",
    )
    .await;
    denied(
        &viktor,
        &format!("/api/dispatches/{id}/assess"),
        json!({"assessment":"served","basis":"Received"}),
        StatusCode::CONFLICT,
        "invalid_transition",
    )
    .await;
    let queued = preview_queue(&olga, id, "queue-copies").await;
    assert_eq!(tuvalu_court::outbox::process(&db).unwrap(), 1);
    let sent = fetched(&olga, &format!("/api/dispatches/{id}")).await;
    assert_eq!(sent["status"], "sent");
    assert_eq!(sent["attempts"][0]["attempt_no"], 1);
    assert_eq!(
        sent["state_summary"],
        "Sent — waiting for confirmation of handover"
    );
    let mid = sent["mailbox_ids"][0].as_i64().unwrap();
    assert_eq!(
        sent["attempts"][0]["technical_receipt"],
        format!("local-mailbox:{mid}")
    );
    let mailbox = fetched(&olga, &format!("/api/mailbox?dispatch_id={id}")).await;
    assert_eq!(mailbox["items"].as_array().unwrap().len(), 1);
    let msg = &mailbox["items"][0];
    assert_eq!(msg["case_number"], number);
    assert_eq!(msg["attachments"][0]["document_version_id"], vid);
    assert_eq!(msg["attachments"][0]["sha256"], sent["items"][0]["sha256"]);
    assert!(msg["attachments"][0]["size_bytes"].as_i64().unwrap() > 0);
    assert_eq!(msg["body"], sent["body"]);
    assert_eq!(fetched(&olga, &format!("/api/mailbox/{mid}")).await, *msg);
    let (s, replay) = olga
        .post_idem(
            &format!("/api/dispatches/{id}/queue"),
            "queue-copies",
            json!({}),
        )
        .await;
    ok(s, &replay);
    assert_eq!(replay, queued);
    assert_eq!(tuvalu_court::outbox::process(&db).unwrap(), 0);
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM delivery_attempts WHERE dispatch_id=?1",
            id
        ),
        1
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM audit_events WHERE action='dispatch.queued' AND entity_id=?1",
            id
        ),
        1
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM audit_events WHERE action='dispatch.sent' AND entity_id=?1",
            id
        ),
        1
    );
    denied(
        &olga,
        &format!("/api/dispatches/{id}/assess"),
        json!({"assessment":"served","basis":"Received"}),
        StatusCode::FORBIDDEN,
        "forbidden",
    )
    .await;
    denied(
        &viktor,
        &format!("/api/dispatches/{id}/confirm"),
        json!({"kind":"human_handover","note":"Received"}),
        StatusCode::FORBIDDEN,
        "forbidden",
    )
    .await;
    denied(
        &olga,
        &format!("/api/dispatches/{id}/confirm"),
        json!({"kind":"invalid","note":"Received"}),
        StatusCode::BAD_REQUEST,
        "validation",
    )
    .await;
    denied(
        &viktor,
        &format!("/api/dispatches/{id}/assess"),
        json!({"assessment":"invalid","basis":"Received"}),
        StatusCode::BAD_REQUEST,
        "validation",
    )
    .await;
    posted(
        &olga,
        &format!("/api/dispatches/{id}/confirm"),
        json!({"kind":"technical_ack","note":"Server receipt"}),
    )
    .await;
    let confirmed = posted(&olga,&format!("/api/dispatches/{id}/confirm"),json!({"kind":"human_handover","note":"Maria collected the copies","occurred_date":today()})).await;
    assert!(confirmed["assessments"].as_array().unwrap().is_empty());
    assert_eq!(
        confirmed["state_summary"],
        "Delivered and handover confirmed"
    );
    let assessed = posted(
        &viktor,
        &format!("/api/dispatches/{id}/assess"),
        json!({"assessment":"served","basis":"Recorded handover"}),
    )
    .await;
    assert_eq!(assessed["confirmations"].as_array().unwrap().len(), 2);
    assert_eq!(assessed["assessments"].as_array().unwrap().len(), 1);
    for action in ["queue", "retry", "preview", "cancel", "record-sent"] {
        denied(
            &olga,
            &format!("/api/dispatches/{id}/{action}"),
            json!({"reason":"No longer needed","occurred_date":today(),"note":"Posted"}),
            StatusCode::CONFLICT,
            "invalid_transition",
        )
        .await;
    }
    for persona in ["sergei", "pavel"] {
        let c = olga.switch(persona).await;
        assert!(
            fetched(&c, &format!("/api/mailbox?dispatch_id={id}")).await["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            fetched(&c, &format!("/api/mailbox?dispatch_id={id}")).await["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let (s, b) = c.get(&format!("/api/mailbox/{mid}")).await;
        err(s, &b, StatusCode::NOT_FOUND, "not_found");
        let (s, b) = c.get(&format!("/api/dispatches/{id}")).await;
        err(s, &b, StatusCode::NOT_FOUND, "not_found");
    }
    assert_eq!(
        tuvalu_court::audit::verify_chain(&db.open().unwrap())
            .unwrap()
            .1,
        None
    );
}

#[tokio::test]
async fn package_validation_and_review_invalidation() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Package checks DEMO").await;
    let db = olga.db(&app);
    let uid = user_id(&olga, "Olga").await;
    let (_, note) = insert_document(
        &db,
        cid,
        "Working note",
        "judicial_note",
        "judicial_note",
        uid,
    );
    let (_, restricted) = insert_document(&db, cid, "Medical record", "medical", "restricted", uid);
    let (_, clean) = insert_document(&db, cid, "Evidence", "evidence", "party_material", uid);
    let (_, quarantined) =
        insert_document(&db, cid, "Untrusted", "evidence", "administrative", uid);
    db.open()
        .unwrap()
        .execute(
            "UPDATE document_versions SET scan_status='quarantined' WHERE id=?1",
            [quarantined],
        )
        .unwrap();
    let (other, _) = register_case(&olga, "Other package DEMO").await;
    let (_, wrong_case) = insert_document(
        &db,
        other,
        "Other evidence",
        "evidence",
        "administrative",
        uid,
    );
    let path = format!("/api/cases/{cid}/dispatches");
    for (vid, status) in [
        (note, StatusCode::BAD_REQUEST),
        (restricted, StatusCode::BAD_REQUEST),
        (quarantined, StatusCode::BAD_REQUEST),
        (wrong_case, StatusCode::BAD_REQUEST),
        (999999, StatusCode::NOT_FOUND),
    ] {
        let (s,b) = olga.post(&path,json!({"kind":"copies","recipient_name":"Maria","method":"email","address":"maria@example.invalid","version_ids":[vid]})).await;
        err(
            s,
            &b,
            status,
            if status == StatusCode::NOT_FOUND {
                "not_found"
            } else {
                "validation"
            },
        );
        if vid == note {
            assert_eq!(
                b["error"]["message"],
                "Judicial working notes cannot be sent to participants"
            );
        }
    }
    denied(
        &olga,
        &path,
        json!({"kind":"copies","recipient_name":"Maria","method":"post","version_ids":[]}),
        StatusCode::BAD_REQUEST,
        "validation",
    )
    .await;
    let d = posted(&olga,&path,json!({"kind":"copies","recipient_name":"Maria","method":"email","address":"maria@example.invalid","version_ids":[restricted],"include_restricted":true})).await;
    let id = d["id"].as_i64().unwrap();
    let reviewed = posted(&olga, &format!("/api/dispatches/{id}/preview"), json!({})).await;
    assert!(
        reviewed["reviewed_by_name"]
            .as_str()
            .unwrap()
            .starts_with("Olga")
    );
    assert_eq!(reviewed["version"], 2);
    let (s, b) = olga
        .patch(
            &format!("/api/dispatches/{id}"),
            json!({"version":1,"subject":"Updated"}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "version_conflict");
    let (s,b) = olga.patch(&format!("/api/dispatches/{id}"),json!({"version":2,"version_ids":[clean],"body":"Updated inventory","recipient_name":"Alexei"})).await;
    ok(s, &b);
    assert!(b["reviewed_at"].is_null());
    assert_eq!(b["items"][0]["document_version_id"], clean);
    let body = b["body"].as_str().unwrap();
    assert!(body.contains("Evidence (version 1, Evidence.pdf)"));
    assert!(!body.contains("Medical record"));
    denied(
        &olga,
        &format!("/api/dispatches/{id}/queue"),
        json!({}),
        StatusCode::CONFLICT,
        "review_required",
    )
    .await;
    // The editor sends the complete body back; its inventory must not be appended twice.
    let (s, unchanged) = olga
        .patch(
            &format!("/api/dispatches/{id}"),
            json!({"version": b["version"], "body": b["body"]}),
        )
        .await;
    ok(s, &unchanged);
    assert_eq!(unchanged["body"], b["body"]);
    let (s, invalid) = olga
        .patch(
            &format!("/api/dispatches/{id}"),
            json!({"version": unchanged["version"], "subject": " "}),
        )
        .await;
    err(s, &invalid, StatusCode::BAD_REQUEST, "validation");
    preview_queue(&olga, id, "after-edit").await;
    let (s, b) = olga
        .patch(
            &format!("/api/dispatches/{id}"),
            json!({"version":5,"address":"other@example.invalid"}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "invalid_transition");
}

#[tokio::test]
async fn address_failure_retry_and_cancellation() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Failed delivery DEMO").await;
    let db = olga.db(&app);
    for address in ["x@demo.fail", "invalid-address"] {
        let d = notice(&olga, cid, "email", address).await;
        let id = d["id"].as_i64().unwrap();
        preview_queue(&olga, id, &format!("bad-{id}")).await;
        assert_eq!(tuvalu_court::outbox::process(&db).unwrap(), 1);
        let failed = fetched(&olga, &format!("/api/dispatches/{id}")).await;
        assert_eq!(failed["status"], "failed");
        assert_eq!(
            failed["failure_reason"],
            "Address rejected by the local mail server"
        );
        assert_eq!(failed["attempts"][0]["status"], "failed");
        assert!(!failed["reviewed_at"].is_null());
        denied(
            &olga,
            &format!("/api/dispatches/{id}/confirm"),
            json!({"kind":"human_handover","note":"Received"}),
            StatusCode::CONFLICT,
            "invalid_transition",
        )
        .await;
        let reviewed = posted(&olga, &format!("/api/dispatches/{id}/preview"), json!({})).await;
        assert_eq!(reviewed["status"], "failed");
        posted(&olga, &format!("/api/dispatches/{id}/retry"), json!({})).await;
        assert_eq!(tuvalu_court::outbox::process(&db).unwrap(), 1);
        let failed = fetched(&olga, &format!("/api/dispatches/{id}")).await;
        assert_eq!(failed["attempts"][1]["attempt_no"], 2);
        assert_eq!(failed["mailbox_ids"], json!([]));
        let cancelled = posted(
            &olga,
            &format!("/api/dispatches/{id}/cancel"),
            json!({"reason":"Address must be checked with participant"}),
        )
        .await;
        assert_eq!(cancelled["status"], "cancelled");
        denied(
            &olga,
            &format!("/api/dispatches/{id}/retry"),
            json!({}),
            StatusCode::CONFLICT,
            "invalid_transition",
        )
        .await;
    }
    for queued in [false, true] {
        let d = notice(&olga, cid, "email", "x@example.invalid").await;
        let id = d["id"].as_i64().unwrap();
        if queued {
            preview_queue(&olga, id, "cancel-queued").await;
        }
        denied(
            &olga,
            &format!("/api/dispatches/{id}/cancel"),
            json!({}),
            StatusCode::BAD_REQUEST,
            "validation",
        )
        .await;
        posted(
            &olga,
            &format!("/api/dispatches/{id}/cancel"),
            json!({"reason":"No longer needed"}),
        )
        .await;
        assert_eq!(tuvalu_court::outbox::process(&db).unwrap(), 0);
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM delivery_attempts WHERE dispatch_id=?1",
                id
            ),
            0
        );
    }
}

#[tokio::test]
async fn manual_methods_need_review_and_record_actual_date() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Manual delivery DEMO").await;
    for method in ["post", "hand", "collection", "island_officer"] {
        let d = notice(&olga, cid, method, "Registry counter").await;
        let id = d["id"].as_i64().unwrap();
        let path = format!("/api/dispatches/{id}/record-sent");
        let body = json!({"occurred_date":"2026-10-07","note":"Handed to participant"});
        denied(
            &olga,
            &path,
            body.clone(),
            StatusCode::CONFLICT,
            "review_required",
        )
        .await;
        posted(&olga, &format!("/api/dispatches/{id}/preview"), json!({})).await;
        denied(
            &olga,
            &format!("/api/dispatches/{id}/queue"),
            json!({}),
            StatusCode::CONFLICT,
            "invalid_transition",
        )
        .await;
        denied(
            &olga,
            &path,
            json!({"occurred_date":"2026-02-30","note":"Posted"}),
            StatusCode::BAD_REQUEST,
            "validation",
        )
        .await;
        denied(
            &olga,
            &path,
            json!({"occurred_date":today(),"note":" "}),
            StatusCode::BAD_REQUEST,
            "validation",
        )
        .await;
        let sent = posted(&olga, &path, body).await;
        assert_eq!(sent["status"], "sent");
        assert_eq!(sent["attempts"][0]["technical_receipt"], "manual");
        assert_eq!(sent["attempts"][0]["occurred_date"], "2026-10-07");
        assert_eq!(sent["mailbox_ids"], json!([]));
    }
    let d = notice(&olga, cid, "email", "x@example.invalid").await;
    denied(
        &olga,
        &format!("/api/dispatches/{}/record-sent", d["id"]),
        json!({"occurred_date":today(),"note":"Posted"}),
        StatusCode::CONFLICT,
        "invalid_transition",
    )
    .await;
}

#[tokio::test]
async fn outbox_rechecks_assignment_and_restricted_grants() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Revoked case access DEMO").await;
    let db = olga.db(&app);
    let oid = user_id(&olga, "Olga").await;
    let d = notice(&olga, cid, "email", "x@example.invalid").await;
    let id = d["id"].as_i64().unwrap();
    preview_queue(&olga, id, "revoked-assignment").await;
    let elena = olga.switch("elena").await;
    let aid = scalar(
        &db,
        "SELECT id FROM case_assignments WHERE case_id=?1 AND role='clerk' AND end_at IS NULL",
        cid,
    );
    posted(
        &elena,
        &format!("/api/cases/{cid}/assignments/{aid}/end"),
        json!({"reason":"Reallocated staff"}),
    )
    .await;
    assert_eq!(tuvalu_court::outbox::process(&db).unwrap(), 1);
    let failed = fetched(&elena, &format!("/api/dispatches/{id}")).await;
    assert_eq!(failed["status"], "failed");
    assert_eq!(
        failed["failure_reason"],
        "Permissions or documents changed before sending — review again"
    );
    assert!(failed["reviewed_at"].is_null());
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM mailbox WHERE dispatch_id=?1", id),
        0
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT user_id FROM audit_events WHERE action='dispatch.failed' AND entity_id=?1",
            id
        ),
        oid
    );
    let (s, b) = olga
        .post(&format!("/api/dispatches/{id}/retry"), json!({}))
        .await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    posted(
        &elena,
        &format!("/api/cases/{cid}/assignments"),
        json!({"user_id":oid,"role":"clerk","reason":"Returned to the registry"}),
    )
    .await;
    denied(
        &olga,
        &format!("/api/dispatches/{id}/retry"),
        json!({}),
        StatusCode::CONFLICT,
        "review_required",
    )
    .await;
    posted(&olga, &format!("/api/dispatches/{id}/preview"), json!({})).await;
    posted(&olga, &format!("/api/dispatches/{id}/retry"), json!({})).await;
    assert_eq!(tuvalu_court::outbox::process(&db).unwrap(), 1);
    assert_eq!(
        fetched(&olga, &format!("/api/dispatches/{id}")).await["attempts"][1]["status"],
        "sent"
    );

    let eid = user_id(&olga, "Elena").await;
    let (doc, vid) = insert_document(
        &db,
        cid,
        "Restricted evidence",
        "evidence",
        "restricted",
        eid,
    );
    db.open().unwrap().execute("INSERT INTO document_grants (document_id,user_id,reason,granted_by,granted_at) VALUES (?1,?2,'Prepare copies',?3,?4)",params![doc,oid,eid,tuvalu_court::time::now_utc()]).unwrap();
    let d = posted(&olga,&format!("/api/cases/{cid}/dispatches"),json!({"kind":"copies","recipient_name":"Maria","method":"email","address":"maria@example.invalid","version_ids":[vid],"include_restricted":true})).await;
    let id = d["id"].as_i64().unwrap();
    preview_queue(&olga, id, "revoked-grant").await;
    db.open()
        .unwrap()
        .execute(
            "UPDATE document_grants SET revoked_at=?2 WHERE document_id=?1",
            params![doc, tuvalu_court::time::now_utc()],
        )
        .unwrap();
    assert_eq!(tuvalu_court::outbox::process(&db).unwrap(), 1);
    let failed = fetched(&olga, &format!("/api/dispatches/{id}")).await;
    assert_eq!(failed["status"], "failed");
    assert!(
        failed["failure_reason"]
            .as_str()
            .unwrap()
            .contains("Permissions or documents changed")
    );
    assert_eq!(failed["attempts"][0]["status"], "failed");
    assert_eq!(failed["mailbox_ids"], json!([]));
    denied(
        &olga,
        &format!("/api/dispatches/{id}/preview"),
        json!({}),
        StatusCode::NOT_FOUND,
        "not_found",
    )
    .await;
}

#[tokio::test]
async fn outbox_rechecks_active_user_permission_and_document_state() {
    for change in ["deactivated", "permission", "quarantine", "judicial_note"] {
        let app = TestApp::demo();
        let olga = app.persona("olga").await;
        let (cid, _) = register_case(&olga, "Current permissions DEMO").await;
        let db = olga.db(&app);
        let uid = user_id(&olga, "Olga").await;
        let (doc, vid) = insert_document(&db, cid, "Attachment", "evidence", "administrative", uid);
        let d = posted(&olga,&format!("/api/cases/{cid}/dispatches"),json!({"kind":"copies","recipient_name":"Maria","method":"email","address":"maria@example.invalid","version_ids":[vid]})).await;
        let id = d["id"].as_i64().unwrap();
        preview_queue(&olga, id, "changed-before-delivery").await;
        let elena = olga.switch("elena").await;
        let conn = db.open().unwrap();
        match change {
            "deactivated" => conn.execute("UPDATE users SET active=0 WHERE id=?1",[uid]).unwrap(),
            "permission" => conn.execute("DELETE FROM user_permissions WHERE user_id=?1 AND permission='dispatch.manage'",[uid]).unwrap(),
            "quarantine" => conn.execute("UPDATE document_versions SET scan_status='quarantined' WHERE id=?1",[vid]).unwrap(),
            _ => conn.execute("UPDATE documents SET visibility='judicial_note' WHERE id=?1",[doc]).unwrap(),
        };
        assert_eq!(tuvalu_court::outbox::process(&db).unwrap(), 1);
        let failed = fetched(&elena, &format!("/api/dispatches/{id}")).await;
        assert_eq!(failed["status"], "failed", "{change}");
        assert!(failed["reviewed_at"].is_null());
        assert_eq!(
            scalar(
                &db,
                "SELECT user_id FROM audit_events WHERE action='dispatch.failed' AND entity_id=?1",
                id
            ),
            uid
        );
        assert_eq!(
            scalar(&db, "SELECT COUNT(*) FROM mailbox WHERE dispatch_id=?1", id),
            0
        );
        assert_eq!(tuvalu_court::audit::verify_chain(&conn).unwrap().1, None);
    }
}

#[tokio::test]
async fn notices_use_case_participants_and_hearing_templates() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, number) = register_case(&olga, "Notice templates DEMO").await;
    let db = olga.db(&app);
    let uid = user_id(&olga, "Olga").await;
    let conn = db.open().unwrap();
    let pid: i64 = conn
        .query_row(
            "SELECT party_id FROM case_participations WHERE case_id=?1 ORDER BY id LIMIT 1",
            [cid],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute("INSERT INTO hearings (case_id,hearing_type,status,starts_at,ends_at,status_reason,created_by,created_at) VALUES (?1,'mention','adjourned','2026-11-16T21:00:00Z','2026-11-16T22:00:00Z','Witness unavailable',?2,?3)",params![cid,uid,tuvalu_court::time::now_utc()]).unwrap();
    let prev = conn.last_insert_rowid();
    conn.execute("INSERT INTO hearings (case_id,hearing_type,status,starts_at,ends_at,previous_hearing_id,created_by,created_at) VALUES (?1,'mention','draft','2026-11-18T21:00:00Z','2026-11-18T22:00:00Z',?2,?3,?4)",params![cid,prev,uid,tuvalu_court::time::now_utc()]).unwrap();
    let hid = conn.last_insert_rowid();
    let path = format!("/api/cases/{cid}/dispatches");
    let d = posted(&olga,&path,json!({"kind":"notice","recipient_party_id":pid,"method":"email","template_code":"hearing_rescheduled","hearing_id":hid})).await;
    assert_eq!(d["address"], "alexei@example.invalid");
    assert_eq!(d["recipient_name"], "Alexei Fenwick");
    let body = d["body"].as_str().unwrap();
    for expected in [
        "2026-11-17T09:00",
        "2026-11-19T09:00",
        "Witness unavailable",
        &number,
    ] {
        assert!(body.contains(expected), "{body}");
    }
    assert!(!body.contains("{hearing_local}"));
    for req in [
        json!({"kind":"notice","recipient_party_id":999999,"method":"post","subject":"X","body":"Y"}),
        json!({"kind":"notice","recipient_name":"X","method":"fax","subject":"X","body":"Y"}),
        json!({"kind":"notice","recipient_name":"X","method":"post","template_code":"missing"}),
        json!({"kind":"information_request","recipient_name":"X","method":"post","subject":"X","body":"Y"}),
        json!({"kind":"notice","recipient_name":"X","method":"post","hearing_id":999999,"subject":"X","body":"Y"}),
        json!({"kind":"notice","recipient_name":"X","method":"email","subject":"X","body":"Y"}),
    ] {
        denied(&olga, &path, req, StatusCode::BAD_REQUEST, "validation").await;
    }
    conn.execute(
        "UPDATE case_participations SET active=0 WHERE case_id=?1 AND party_id=?2",
        params![cid, pid],
    )
    .unwrap();
    denied(
        &olga,
        &path,
        json!({"kind":"notice","recipient_party_id":pid,"method":"post","subject":"X","body":"Y"}),
        StatusCode::BAD_REQUEST,
        "validation",
    )
    .await;
    let viktor = assigned(&olga, cid).await;
    denied(
        &viktor,
        &path,
        json!({"kind":"notice","recipient_name":"X","method":"post","subject":"X","body":"Y"}),
        StatusCode::FORBIDDEN,
        "forbidden",
    )
    .await;
    let id = d["id"].as_i64().unwrap();
    for action in [
        "preview",
        "queue",
        "retry",
        "record-sent",
        "confirm",
        "cancel",
    ] {
        denied(
            &viktor,
            &format!("/api/dispatches/{id}/{action}"),
            json!({"reason":"X","note":"X","kind":"human_handover","occurred_date":today()}),
            StatusCode::FORBIDDEN,
            "forbidden",
        )
        .await;
    }
    let (s, b) = viktor
        .patch(
            &format!("/api/dispatches/{id}"),
            json!({"version":1,"subject":"X"}),
        )
        .await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
}

#[tokio::test]
async fn intake_dispatches_follow_intake_access_even_after_linking() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let db = olga.db(&app);
    let iid = new_intake(&olga, "Incoming sender DEMO").await;
    let b = posted(&olga,&format!("/api/intakes/{iid}/request-info"),json!({"missing_items":"Signed application","method":"email","address":"sender@example.invalid"})).await;
    let id = b["dispatch_id"].as_i64().unwrap();
    let d = fetched(&olga, &format!("/api/dispatches/{id}")).await;
    assert!(d["case_id"].is_null());
    assert_eq!(d["intake_id"], iid);
    assert_eq!(d["kind"], "information_request");
    assert_eq!(
        fetched(
            &olga,
            "/api/dispatches?kind=information_request&status=draft"
        )
        .await["items"][0]["id"],
        id
    );
    // intake.manage alone suffices for information requests.
    let oid = user_id(&olga, "Olga").await;
    db.open()
        .unwrap()
        .execute(
            "DELETE FROM user_permissions WHERE user_id=?1 AND permission='dispatch.manage'",
            [oid],
        )
        .unwrap();
    let (s, b) = olga
        .patch(
            &format!("/api/dispatches/{id}"),
            json!({"version":1,"body":"Please send the signed application."}),
        )
        .await;
    ok(s, &b);
    preview_queue(&olga, id, "intake-email").await;
    assert_eq!(tuvalu_court::outbox::process(&db).unwrap(), 1);
    assert_eq!(
        fetched(&olga, &format!("/api/dispatches/{id}")).await["status"],
        "sent"
    );
    let mid = fetched(&olga, &format!("/api/mailbox?dispatch_id={id}")).await["items"][0]["id"]
        .as_i64()
        .unwrap();
    for persona in ["elena", "viktor", "sergei", "pavel"] {
        let c = olga.switch(persona).await;
        assert!(
            fetched(&c, "/api/dispatches?kind=information_request").await["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            fetched(&c, &format!("/api/mailbox?dispatch_id={id}")).await["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let (s, b) = c.get(&format!("/api/dispatches/{id}")).await;
        err(s, &b, StatusCode::NOT_FOUND, "not_found");
    }
    let second = posted(&olga, &format!("/api/intakes/{iid}/request-info"), json!({"missing_items":"Certified copy","method":"email","address":"sender@example.invalid"})).await;
    let second_id = second["dispatch_id"].as_i64().unwrap();
    preview_queue(&olga, second_id, "intake-revoked-after-link").await;
    posted(&olga, &format!("/api/intakes/{iid}/mark-ready"), json!({})).await;
    let refs = fetched(&olga, "/api/ref").await;
    let reg = refs["registries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["series"] == "DEMO-CIV")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let case = posted(&olga,&format!("/api/intakes/{iid}/register"),json!({"registry_id":reg,"category":"civil_contract","title":"Linked information request DEMO"})).await;
    let cid = case["case_id"].as_i64().unwrap();
    // Linked intake follows case visibility, while dispatch/mailbox still need intake.manage.
    let elena = olga.switch("elena").await;
    let (s, b) = elena.get(&format!("/api/mailbox/{mid}")).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    let aid = scalar(
        &db,
        "SELECT id FROM case_assignments WHERE case_id=?1 AND role='clerk' AND end_at IS NULL",
        cid,
    );
    posted(
        &elena,
        &format!("/api/cases/{cid}/assignments/{aid}/end"),
        json!({"reason":"Reallocated"}),
    )
    .await;
    let (s, b) = olga.get(&format!("/api/dispatches/{id}")).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    assert_eq!(tuvalu_court::outbox::process(&db).unwrap(), 1);
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM delivery_attempts WHERE dispatch_id=?1 AND status='failed'",
            second_id
        ),
        1
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM mailbox WHERE dispatch_id=?1",
            second_id
        ),
        0
    );
    assert!(
        fetched(&olga, &format!("/api/mailbox?dispatch_id={id}")).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn concurrent_finalisation_queue_and_workers_do_not_duplicate_history() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "Concurrent operations DEMO").await;
    let viktor = assigned(&olga, cid).await;
    let db = olga.db(&app);
    let uid = user_id(&olga, "Viktor").await;
    let (_, vid) = insert_document(
        &db,
        cid,
        "Concurrent ruling",
        "decision",
        "administrative",
        uid,
    );
    let d = draft(&viktor, cid, vid).await;
    let id = d["id"].as_i64().unwrap();
    let path = format!("/api/decisions/{id}/finalise");
    let body = json!({"decision_date":today()});
    let (a, b) = tokio::join!(
        viktor.post_idem(&path, "concurrent-finalise", body.clone()),
        viktor.post_idem(&path, "concurrent-finalise", body.clone())
    );
    ok(a.0, &a.1);
    ok(b.0, &b.1);
    assert_eq!(a.1, b.1);
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM audit_events WHERE entity_id=?1 AND action='decision.finalised'",
            id
        ),
        1
    );
    let d = notice(&olga, cid, "email", "x@example.invalid").await;
    let id = d["id"].as_i64().unwrap();
    posted(&olga, &format!("/api/dispatches/{id}/preview"), json!({})).await;
    let path = format!("/api/dispatches/{id}/queue");
    let (a, b) = tokio::join!(
        olga.post_idem(&path, "concurrent-queue", json!({})),
        olga.post_idem(&path, "concurrent-queue", json!({}))
    );
    ok(a.0, &a.1);
    ok(b.0, &b.1);
    assert_eq!(a.1, b.1);
    let db1 = db.clone();
    let db2 = db.clone();
    let (a, b) = tokio::join!(
        tokio::task::spawn_blocking(move || tuvalu_court::outbox::process(&db1)),
        tokio::task::spawn_blocking(move || tuvalu_court::outbox::process(&db2))
    );
    assert_eq!(a.unwrap().unwrap() + b.unwrap().unwrap(), 1);
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM delivery_attempts WHERE dispatch_id=?1",
            id
        ),
        1
    );
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM mailbox WHERE dispatch_id=?1", id),
        1
    );
    let foreign = app.persona("olga").await;
    let (s, b) = foreign.get(&format!("/api/dispatches/{id}")).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    assert!(
        fetched(&foreign, &format!("/api/mailbox?dispatch_id={id}")).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
