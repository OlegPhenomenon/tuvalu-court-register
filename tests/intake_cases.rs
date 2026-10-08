mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;

#[tokio::test]
async fn intake_is_not_a_case_until_registered() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let intake = new_intake(&olga, "Alexei Fenwick").await;

    // Registration requires the explicit "checked" step.
    let (_, refs) = olga.get("/api/ref").await;
    let reg = refs["registries"][0]["id"].as_i64().unwrap();
    let (s, b) = olga
        .post(&format!("/api/intakes/{intake}/register"), json!({"registry_id": reg, "category": "civil_contract", "title": "X"}))
        .await;
    err(s, &b, StatusCode::CONFLICT, "invalid_transition");

    // Request info → needs_information, a draft message is prepared (not sent).
    let (s, b) = olga.post(&format!("/api/intakes/{intake}/request-info"), json!({"missing_items": "Copy of the repair agreement"})).await;
    ok(s, &b);
    let (_, d) = olga.get(&format!("/api/intakes/{intake}")).await;
    assert_eq!(d["intake"]["status"], "needs_information");
    assert_eq!(d["dispatches"][0]["status"], "draft");

    // Supplement returns it to received and is linked to the original.
    let (s, b) = olga
        .post(
            &format!("/api/intakes/{intake}/supplement"),
            json!({"sender_name": "Alexei Fenwick", "channel": "post", "received_date": today(), "description": "Repair agreement copy"}),
        )
        .await;
    ok(s, &b);
    let supplement = b["id"].as_i64().unwrap();
    let (_, d) = olga.get(&format!("/api/intakes/{intake}")).await;
    assert_eq!(d["intake"]["status"], "received");
    assert_eq!(d["related_intakes"][0]["id"], supplement);
    // A supplement cannot be registered on its own.
    let (s, b) = olga.post(&format!("/api/intakes/{supplement}/mark-ready"), json!({})).await;
    err(s, &b, StatusCode::CONFLICT, "invalid_transition");
}

#[tokio::test]
async fn registration_numbers_are_unique_and_idempotent() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (_, refs) = olga.get("/api/ref").await;
    let reg = refs["registries"].as_array().unwrap().iter().find(|r| r["series"] == "DEMO-CIV").unwrap()["id"].as_i64().unwrap();

    // Several intakes registered concurrently → distinct, consecutive numbers.
    let mut intakes = vec![];
    for i in 0..6 {
        let id = new_intake(&olga, &format!("Sender {i}")).await;
        let (s, b) = olga.post(&format!("/api/intakes/{id}/mark-ready"), json!({})).await;
        ok(s, &b);
        intakes.push(id);
    }
    let futs = intakes.iter().map(|id| {
        let olga = olga.clone();
        let id = *id;
        async move {
            olga.post_idem(
                &format!("/api/intakes/{id}/register"),
                &format!("key-{id}"),
                json!({"registry_id": reg, "category": "civil_contract", "title": format!("Case {id}")}),
            )
            .await
        }
    });
    let results = futures_join_all(futs).await;
    let mut numbers: Vec<String> = results
        .iter()
        .map(|(s, b)| {
            ok(*s, b);
            b["number"].as_str().unwrap().to_string()
        })
        .collect();
    numbers.sort();
    numbers.dedup();
    assert_eq!(numbers.len(), 6, "numbers must be unique: {numbers:?}");

    // Replaying the same request with the same key returns the same number, no new case.
    let id = intakes[0];
    let body = json!({"registry_id": reg, "category": "civil_contract", "title": format!("Case {id}")});
    let (s1, first) = olga.post_idem(&format!("/api/intakes/{id}/register"), &format!("key-{id}"), body.clone()).await;
    ok(s1, &first);
    let (_, list) = olga.get("/api/cases").await;
    let before = list["items"].as_array().unwrap().len();
    let (s2, again) = olga.post_idem(&format!("/api/intakes/{id}/register"), &format!("key-{id}"), body).await;
    ok(s2, &again);
    assert_eq!(first["number"], again["number"]);
    let (_, list) = olga.get("/api/cases").await;
    assert_eq!(list["items"].as_array().unwrap().len(), before);

    // Same key with a different body is refused.
    let (s, b) = olga
        .post_idem(&format!("/api/intakes/{id}/register"), &format!("key-{id}"), json!({"registry_id": reg, "category": "family", "title": "Other"}))
        .await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
}

async fn futures_join_all<F: Future + Send + 'static>(futs: impl Iterator<Item = F>) -> Vec<F::Output>
where
    F::Output: Send + 'static,
{
    let handles: Vec<_> = futs.map(tokio::spawn).collect();
    let mut out = vec![];
    for h in handles {
        out.push(h.await.unwrap());
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_registration_on_multi_thread_runtime() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (_, refs) = olga.get("/api/ref").await;
    let reg = refs["registries"][0]["id"].as_i64().unwrap();
    let mut ids = vec![];
    for i in 0..8 {
        let id = new_intake(&olga, &format!("P{i}")).await;
        olga.post(&format!("/api/intakes/{id}/mark-ready"), json!({})).await;
        ids.push(id);
    }
    let results = futures_join_all(ids.into_iter().map(|id| {
        let olga = olga.clone();
        async move {
            olga.post(&format!("/api/intakes/{id}/register"), json!({"registry_id": reg, "category": "civil_contract", "title": "Parallel"}))
                .await
        }
    }))
    .await;
    let mut nums: Vec<String> = results.into_iter().map(|(s, b)| {
        ok(s, &b);
        b["number"].as_str().unwrap().to_string()
    }).collect();
    nums.sort();
    nums.dedup();
    assert_eq!(nums.len(), 8);
}

#[tokio::test]
async fn case_visibility_follows_assignments() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, number) = register_case(&olga, "Boat repair dispute").await;

    // Olga registered it → she keeps access.
    let (s, b) = olga.get(&format!("/api/cases/{case_id}")).await;
    ok(s, &b);
    assert_eq!(b["case"]["number"], number);

    // Sergei is not assigned → 404, and the case is not in his list.
    let sergei = olga.switch("sergei").await;
    let (s, b) = sergei.get(&format!("/api/cases/{case_id}")).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    let (_, list) = sergei.get("/api/cases").await;
    assert!(list["items"].as_array().unwrap().iter().all(|c| c["id"] != case_id));

    // Pavel (technical admin) has no case access at all.
    let pavel = olga.switch("pavel").await;
    let (s, _) = pavel.get(&format!("/api/cases/{case_id}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Elena (view_all) sees it; she assigns Sergei and Viktor (judge).
    let elena = olga.switch("elena").await;
    let sergei_id = user_id(&elena, "Sergei").await;
    let viktor_id = user_id(&elena, "Viktor").await;
    let (s, b) = elena.post(&format!("/api/cases/{case_id}/assignments"), json!({"user_id": sergei_id, "role": "service_officer"})).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation"); // reason is mandatory
    let (s, b) = elena
        .post(&format!("/api/cases/{case_id}/assignments"), json!({"user_id": sergei_id, "role": "service_officer", "reason": "Will deliver notices"}))
        .await;
    ok(s, &b);
    let (s, b) = elena
        .post(&format!("/api/cases/{case_id}/assignments"), json!({"user_id": sergei_id, "role": "judge", "reason": "x"}))
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation"); // Sergei is not a judicial officer
    let (s, b) = elena
        .post(&format!("/api/cases/{case_id}/assignments"), json!({"user_id": viktor_id, "role": "judge", "reason": "Allocated by the registry head"}))
        .await;
    ok(s, &b);

    // Olga cannot assign a judge (separate permission).
    let (s, b) = olga
        .post(&format!("/api/cases/{case_id}/assignments"), json!({"user_id": viktor_id, "role": "judge", "reason": "x"}))
        .await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");

    let (s, _) = sergei.get(&format!("/api/cases/{case_id}")).await;
    assert_eq!(s, StatusCode::OK);

    // Ending Sergei's assignment removes his access immediately.
    let (_, card) = elena.get(&format!("/api/cases/{case_id}")).await;
    let aid = card["assignments"].as_array().unwrap().iter().find(|a| a["user_id"] == sergei_id && a["end_at"].is_null()).unwrap()["id"].as_i64().unwrap();
    let (s, b) = elena.post(&format!("/api/cases/{case_id}/assignments/{aid}/end"), json!({"reason": "Moved to another island"})).await;
    ok(s, &b);
    let (s, _) = sergei.get(&format!("/api/cases/{case_id}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn restricted_case_hidden_from_view_all() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _) = register_case(&olga, "Sensitive family matter").await;
    let (_, card) = olga.get(&format!("/api/cases/{case_id}")).await;
    let version = card["case"]["version"].as_i64().unwrap();
    let (s, b) = olga.patch(&format!("/api/cases/{case_id}"), json!({"version": version, "restricted": true})).await;
    ok(s, &b);

    let elena = olga.switch("elena").await;
    let (s, _) = elena.get(&format!("/api/cases/{case_id}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, list) = elena.get("/api/cases?q=Sensitive").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn concurrent_edit_reports_version_conflict() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _) = register_case(&olga, "Original title").await;
    let (_, card) = olga.get(&format!("/api/cases/{case_id}")).await;
    let v = card["case"]["version"].as_i64().unwrap();
    let (s, b) = olga.patch(&format!("/api/cases/{case_id}"), json!({"version": v, "title": "First edit"})).await;
    ok(s, &b);
    let (s, b) = olga.patch(&format!("/api/cases/{case_id}"), json!({"version": v, "title": "Second edit"})).await;
    err(s, &b, StatusCode::CONFLICT, "version_conflict");
    assert_eq!(b["error"]["details"]["current"]["title"], "First edit");
}

#[tokio::test]
async fn closing_requires_basis_and_no_open_items_then_reopen() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _) = register_case(&olga, "Closable").await;

    let (s, b) = olga.post(&format!("/api/cases/{case_id}/close"), json!({"basis": "made_up"})).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = olga.post(&format!("/api/cases/{case_id}/close"), json!({"basis": "other"})).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");

    let vid = settlement_document(&olga, &app, case_id).await;
    let (s, b) = olga.post(&format!("/api/cases/{case_id}/close"), json!({"basis": "settled", "note": "Parties settled", "basis_document_version_id": vid})).await;
    ok(s, &b);
    let (_, card) = olga.get(&format!("/api/cases/{case_id}")).await;
    assert_eq!(card["case"]["status"], "closed");
    assert_eq!(card["next_actions"].as_array().unwrap().len(), 0);

    // Closed case cannot be edited; Olga cannot reopen; Elena can, with a reason.
    let v = card["case"]["version"].as_i64().unwrap();
    let (s, b) = olga.patch(&format!("/api/cases/{case_id}"), json!({"version": v, "title": "x"})).await;
    err(s, &b, StatusCode::CONFLICT, "invalid_transition");
    let (s, _) = olga.post(&format!("/api/cases/{case_id}/reopen"), json!({"reason": "x"})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let elena = olga.switch("elena").await;
    let (s, b) = elena.post(&format!("/api/cases/{case_id}/reopen"), json!({})).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = elena.post(&format!("/api/cases/{case_id}/reopen"), json!({"reason": "Settlement not honoured"})).await;
    ok(s, &b);
    assert_eq!(b["case"]["status"], "reopened");
    let hist = b["status_history"].as_array().unwrap();
    assert_eq!(hist.iter().map(|h| h["to_status"].as_str().unwrap()).collect::<Vec<_>>(), ["registered", "closed", "reopened"]);
}

#[tokio::test]
async fn same_name_parties_are_never_merged() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (s, a) = olga.post("/api/parties", json!({"kind": "person", "name": "John Smith"})).await;
    ok(s, &a);
    let (s, b) = olga.post("/api/parties", json!({"kind": "person", "name": "John Smith"})).await;
    ok(s, &b);
    assert_ne!(a["id"], b["id"]);
    assert_eq!(b["same_name_records"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn audit_chain_and_immutability() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    register_case(&olga, "Audited").await;
    let db = olga.db(&app);
    let conn = db.open().unwrap();
    let (n, broken) = tuvalu_court::audit::verify_chain(&conn).unwrap();
    assert!(n >= 4);
    assert_eq!(broken, None);
    assert!(conn.execute("DELETE FROM audit_events", []).is_err());
    assert!(conn.execute("UPDATE audit_events SET summary = 'x'", []).is_err());
    assert!(conn.execute("DELETE FROM cases", []).is_err());
}

#[tokio::test]
async fn sandboxes_are_isolated() {
    let app = TestApp::demo();
    let a = app.persona("olga").await;
    let b = app.persona("olga").await;
    let (case_id, _) = register_case(&a, "Only in A").await;
    let (s, _) = b.get(&format!("/api/cases/{case_id}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Resetting B does not touch A.
    let (s, _) = b.post("/api/demo/reset", json!({})).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = a.get(&format!("/api/cases/{case_id}")).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn csrf_header_required_for_mutations() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/intakes")
        .header("host", "localhost")
        .header("cookie", format!("tcr_sandbox={}; tcr_session={}", olga.sandbox.clone().unwrap(), olga.session.clone().unwrap()))
        .header("content-type", "application/json")
        .body(axum::body::Body::from("{}"))
        .unwrap();
    let (s, b, _, _) = olga.clone().send(req).await;
    err(s, &b, StatusCode::FORBIDDEN, "csrf");
}
