mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};

// ------------------------------------------------------------------ helpers

/// Register a case and have Elena assign Viktor as judge. Returns (case_id, number, viktor_id, room_id).
async fn seeded_case(olga: &Client, title: &str) -> (i64, String, i64, i64) {
    let (case_id, number) = register_case(olga, title).await;
    let elena = olga.switch("elena").await;
    let viktor = user_id(&elena, "Viktor").await;
    let (s, b) = elena
        .post(
            &format!("/api/cases/{case_id}/assignments"),
            json!({"user_id": viktor, "role": "judge", "reason": "Allocated by the registry head"}),
        )
        .await;
    ok(s, &b);
    let (_, refs) = olga.get("/api/ref").await;
    let room = refs["rooms"].as_array().unwrap()[0]["id"].as_i64().unwrap();
    (case_id, number, viktor, room)
}

/// Second demo room (for room-vs-judge conflict isolation).
async fn second_room(c: &Client) -> i64 {
    let (_, refs) = c.get("/api/ref").await;
    refs["rooms"].as_array().unwrap()[1]["id"].as_i64().unwrap()
}

/// party_ids of the case's participants.
async fn party_ids(c: &Client, case_id: i64) -> Vec<i64> {
    let (_, card) = c.get(&format!("/api/cases/{case_id}")).await;
    card["participants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["party_id"].as_i64().unwrap())
        .collect()
}

fn hearing_body(start: &str, end: &str, room: i64, confirm: bool) -> Value {
    json!({ "hearing_type": "hearing", "starts_local": start, "ends_local": end, "room_id": room, "confirm": confirm })
}

/// Create a draft hearing and return its JSON.
async fn draft(c: &Client, case_id: i64, start: &str, end: &str, room: i64) -> Value {
    let (s, b) = c
        .post(&format!("/api/cases/{case_id}/hearings"), hearing_body(start, end, room, false))
        .await;
    ok(s, &b);
    assert_eq!(b["status"], "draft");
    b
}

/// Create a confirmed (scheduled) hearing and return its JSON.
async fn scheduled(c: &Client, case_id: i64, start: &str, end: &str, room: i64) -> Value {
    let (s, b) = c
        .post(&format!("/api/cases/{case_id}/hearings"), hearing_body(start, end, room, true))
        .await;
    ok(s, &b);
    assert_eq!(b["status"], "scheduled");
    b
}

/// Grant a permission directly in the sandbox DB (the admin API belongs to another slice).
async fn grant_perm(c: &Client, app: &TestApp, display_prefix: &str, permission: &str) {
    let uid = user_id(c, display_prefix).await;
    let conn = c.db(app).open().unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO user_permissions (user_id, permission, granted_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![uid, permission, tuvalu_court::time::now_utc()],
    )
    .unwrap();
}

fn audit_count(c: &Client, app: &TestApp, action: &str) -> i64 {
    let conn = c.db(app).open().unwrap();
    conn.query_row("SELECT COUNT(*) FROM audit_events WHERE action = ?1", [action], |r| r.get(0))
        .unwrap()
}

fn audit_count_for_case(c: &Client, app: &TestApp, action: &str, case_id: i64) -> i64 {
    let conn = c.db(app).open().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM audit_events WHERE action = ?1 AND case_id = ?2",
        rusqlite::params![action, case_id],
        |r| r.get(0),
    )
    .unwrap()
}

fn audit_summary(c: &Client, app: &TestApp, action: &str) -> String {
    let conn = c.db(app).open().unwrap();
    conn.query_row(
        "SELECT COALESCE(GROUP_CONCAT(summary, '\n'), '') FROM audit_events WHERE action = ?1",
        [action],
        |r| r.get(0),
    )
    .unwrap()
}

fn case_counts(c: &Client, app: &TestApp, case_id: i64) -> (i64, i64, i64) {
    c.db(app)
        .open()
        .unwrap()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM hearings WHERE case_id = ?1),
                (SELECT COUNT(*) FROM tasks WHERE case_id = ?1),
                (SELECT COUNT(*) FROM audit_events WHERE case_id = ?1)",
            [case_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

// ------------------------------------------------------------------ hearings

#[tokio::test]
async fn draft_patch_confirm_and_boundary() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, number, _viktor, room) = seeded_case(&olga, "Boundary check").await;

    // Draft creation (confirm=false) with an idempotency key; replay returns the same hearing.
    let body = hearing_body("2026-11-17T09:00", "2026-11-17T10:00", room, false);
    let (s, first) = olga
        .post_idem(&format!("/api/cases/{case_id}/hearings"), "h-create-1", body.clone())
        .await;
    ok(s, &first);
    assert_eq!(first["status"], "draft");
    assert_eq!(first["case_number"], number);
    assert_eq!(first["hearing_type_label"], "Hearing");
    assert_eq!(first["starts_local"], "2026-11-17T09:00");
    assert_eq!(first["judge_name"], "Viktor Hale"); // defaults to the assigned judge
    let (s, again) = olga.post_idem(&format!("/api/cases/{case_id}/hearings"), "h-create-1", body).await;
    ok(s, &again);
    assert_eq!(again["id"], first["id"]);
    let hid = first["id"].as_i64().unwrap();
    let v = first["version"].as_i64().unwrap();

    // A draft may be edited (versioned).
    let (s, b) = olga
        .patch(
            &format!("/api/hearings/{hid}"),
            json!({"version": v, "ends_local": "2026-11-17T09:30"}),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["ends_local"], "2026-11-17T09:30");
    let (s, b) = olga
        .patch(&format!("/api/hearings/{hid}"), json!({"version": v, "notes": "stale"}))
        .await;
    err(s, &b, StatusCode::CONFLICT, "version_conflict");

    // Confirm → scheduled.
    let (s, b) = olga.post(&format!("/api/hearings/{hid}/confirm"), json!({"version":v+1})).await;
    ok(s, &b);
    assert_eq!(b["status"], "scheduled");
    // A confirmed hearing can no longer be edited.
    let (s, b) = olga
        .patch(&format!("/api/hearings/{hid}"), json!({"version": b["version"], "notes": "x"}))
        .await;
    err(s, &b, StatusCode::CONFLICT, "invalid_transition");

    // [start, end) boundary: 09:30–10:30 does not clash with 09:00–09:30.
    let _h2 = scheduled(&olga, case_id, "2026-11-17T09:30", "2026-11-17T10:30", room).await;

    // Overlap in the same room → 409 hearing_conflict with details.
    let (s, b) = olga
        .post(
            &format!("/api/cases/{case_id}/hearings"),
            hearing_body("2026-11-17T10:00", "2026-11-17T11:00", room, true),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "hearing_conflict");
    let conflicts = b["error"]["details"]["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0]["case_number"], number);
    assert_eq!(conflicts[0]["starts_local"], "2026-11-17T09:30");
}

#[tokio::test]
async fn conflict_override_requires_permission() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case1, num1, _v, room) = seeded_case(&olga, "Restricted conflicting A").await;
    // Restrict case1: Elena (view_all) must not see it.
    let (_, card) = olga.get(&format!("/api/cases/{case1}")).await;
    let (s, b) = olga
        .patch(
            &format!("/api/cases/{case1}"),
            json!({"version": card["case"]["version"], "restricted": true}),
        )
        .await;
    ok(s, &b);
    let _h = scheduled(&olga, case1, "2026-11-20T09:00", "2026-11-20T10:00", room).await;

    let (case2, _n2, _v2, _r) = seeded_case(&olga, "Conflicting B").await;
    let room2 = second_room(&olga).await;

    // Olga sees the conflicting hearing (she is on case1) → conflicts listed.
    let (s, b) = olga
        .post(
            &format!("/api/cases/{case2}/hearings"),
            hearing_body("2026-11-20T09:30", "2026-11-20T10:30", room2, true),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "hearing_conflict");
    assert_eq!(b["error"]["details"]["conflicts"][0]["case_number"], num1);
    assert_eq!(b["error"]["details"]["hidden_conflicts"], 0);

    // Elena has hearing.schedule granted; she cannot see restricted case1 → hidden conflict.
    let elena = olga.switch("elena").await;
    grant_perm(&elena, &app, "Elena", "hearing.schedule").await;
    let (s, b) = elena
        .post(
            &format!("/api/cases/{case2}/hearings"),
            hearing_body("2026-11-20T09:30", "2026-11-20T10:30", room2, true),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "hearing_conflict");
    assert_eq!(b["error"]["details"]["conflicts"].as_array().unwrap().len(), 0);
    assert_eq!(b["error"]["details"]["hidden_conflicts"], 1);
    assert!(!b.to_string().contains(&num1));

    // Olga supplies an override reason but lacks hearing.override_conflict → 403.
    let mut over = hearing_body("2026-11-20T09:30", "2026-11-20T10:30", room2, true);
    over["override_reason"] = json!("Only courtroom available");
    let (s, b) = olga.post(&format!("/api/cases/{case2}/hearings"), over.clone()).await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");

    // Elena has both permissions → the override is booked and audited.
    let (s, b) = elena.post(&format!("/api/cases/{case2}/hearings"), over).await;
    ok(s, &b);
    assert_eq!(b["status"], "scheduled");
    assert_eq!(b["conflict_override"], 1);
    assert_eq!(b["override_reason"], "Only courtroom available");
    assert!(audit_count(&elena, &app, "hearing.conflict_override") >= 1);
    assert!(!b.to_string().contains(&num1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_confirmation_lets_one_win() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case1, _n1, _v, room) = seeded_case(&olga, "Race A").await;
    let (case2, _n2, _v2, _r2) = seeded_case(&olga, "Race B").await;
    let h1 = draft(&olga, case1, "2026-11-21T09:00", "2026-11-21T10:00", room).await;
    let h2 = draft(&olga, case2, "2026-11-21T09:15", "2026-11-21T10:15", room).await;
    let id1 = h1["id"].as_i64().unwrap();
    let id2 = h2["id"].as_i64().unwrap();

    let c1 = olga.clone();
    let c2 = olga.clone();
    let (r1, r2) = (
        tokio::spawn(async move { c1.post(&format!("/api/hearings/{id1}/confirm"), json!({"version":1})).await }),
        tokio::spawn(async move { c2.post(&format!("/api/hearings/{id2}/confirm"), json!({"version":1})).await }),
    );
    let (s1, b1) = r1.await.unwrap();
    let (s2, b2) = r2.await.unwrap();
    let mut statuses = [s1, s2];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CONFLICT], "exactly one confirmation may win");
    let failed = if s1 == StatusCode::CONFLICT { b1 } else { b2 };
    assert_eq!(failed["error"]["code"], "hearing_conflict");
    assert_eq!(failed["error"]["details"]["conflicts"].as_array().unwrap().len(), 1);
    assert_eq!(audit_count(&olga, &app, "hearing.confirmed"), 1);
}

#[tokio::test]
async fn adjourn_links_new_hearing_and_frees_the_slot() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _n, _v, room) = seeded_case(&olga, "Adjournment").await;
    let pids = party_ids(&olga, case_id).await;
    assert_eq!(pids.len(), 2);

    let mut body = hearing_body("2026-11-17T09:00", "2026-11-17T10:00", room, true);
    body["participants"] = json!([
        { "party_id": pids[0], "role": "claimant" },
        { "party_id": pids[1], "role": "respondent" },
    ]);
    let (s, h) = olga.post(&format!("/api/cases/{case_id}/hearings"), body).await;
    ok(s, &h);
    let hid = h["id"].as_i64().unwrap();

    // Reason and authoriser are mandatory.
    let (s, b) = olga
        .post(
            &format!("/api/hearings/{hid}/adjourn"),
            json!({"starts_local": "2026-11-19T09:00", "ends_local": "2026-11-19T10:00"}),
        )
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");

    // Adjourn 17 Nov 09:00 → 19 Nov 09:00 (idempotent).
    let adj = json!({
        "starts_local": "2026-11-19T09:00", "ends_local": "2026-11-19T10:00",
        "reason": "Judge unavailable on the 17th", "authorised_by": "Judge Viktor Hale",
    });
    let (s, b) = olga.post_idem(&format!("/api/hearings/{hid}/adjourn"), "adj-1", adj.clone()).await;
    ok(s, &b);
    let old = &b["old"];
    let new = &b["new"];
    assert_eq!(old["id"], hid);
    assert_eq!(old["status"], "adjourned");
    assert_eq!(old["status_reason"], "Judge unavailable on the 17th");
    assert_eq!(old["status_authorised_by"], "Judge Viktor Hale");
    assert_eq!(old["adjourned_to_id"], new["id"]);
    assert_eq!(new["status"], "scheduled");
    assert_eq!(new["previous_hearing_id"], hid);
    assert_eq!(new["starts_local"], "2026-11-19T09:00");
    assert_eq!(new["participants"].as_array().unwrap().len(), 2);

    // One renotify task per required participant, assigned to the actor (no service officer yet).
    let olga_id = user_id(&olga, "Olga").await;
    let tasks = b["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    assert!(
        tasks
            .iter()
            .all(|t| t["kind"] == "renotify" && t["assignee_user_id"] == olga_id && t["hearing_id"] == new["id"])
    );
    assert!(
        tasks[0]["title"]
            .as_str()
            .unwrap()
            .contains("of the new hearing date (19 Nov 2026 09:00)")
    );

    // Replay with the same key: identical result, no second adjournment.
    let (s, b2) = olga.post_idem(&format!("/api/hearings/{hid}/adjourn"), "adj-1", adj).await;
    ok(s, &b2);
    assert_eq!(b2["new"]["id"], new["id"]);

    // The old slot is free: another hearing can take 17 Nov 09:00 in the same room.
    let _other = scheduled(&olga, case_id, "2026-11-17T09:00", "2026-11-17T10:00", room).await;

    // The audit trail explains the move.
    assert!(audit_summary(&olga, &app, "hearing.adjourned").contains("17 Nov 2026 09:00 adjourned to 19 Nov 2026 09:00"));

    // The case list shows the whole chain, newest first.
    let (_, list) = olga.get(&format!("/api/cases/{case_id}/hearings")).await;
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["id"], new["id"]);
}

#[tokio::test]
async fn outcome_held_with_attendance_task_and_next_hearing() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _n, viktor, room) = seeded_case(&olga, "Outcome").await;
    let pids = party_ids(&olga, case_id).await;
    let mut body = hearing_body("2026-11-19T09:00", "2026-11-19T10:00", room, true);
    body["participants"] = json!([
        { "party_id": pids[0], "role": "claimant" },
        { "party_id": pids[1], "role": "respondent" },
    ]);
    let (s, h) = olga
        .post(&format!("/api/cases/{case_id}/hearings"), body)
        .await;
    ok(s, &h);
    let hid = h["id"].as_i64().unwrap();
    let p0 = h["participants"][0]["id"].as_i64().unwrap();
    let p1 = h["participants"][1]["id"].as_i64().unwrap();

    // Olga lacks hearing.record_outcome → 403.
    let (s, b) = olga
        .post(
            &format!("/api/hearings/{hid}/outcome"),
            json!({"held": true, "outcome_summary": "x"}),
        )
        .await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");

    let viktor_c = olga.switch("viktor").await;
    let (s, b) = viktor_c
        .post(
            &format!("/api/hearings/{hid}/outcome"),
            json!({
                "held": true,
                "attendance": [
                    {"participant_id": p0, "attended": true},
                    {"participant_id": p1, "attended": false},
                ],
                "outcome_summary": "Heard both sides; decision reserved.",
                "next_step": "Written decision to follow.",
                "next_task": {"title": "Draft the decision", "assignee_user_id": viktor, "due_date": "2026-11-30"},
                "next_hearing": {"starts_local": "2026-11-24T09:00", "ends_local": "2026-11-24T10:00"},
            }),
        )
        .await;
    ok(s, &b);
    // Demo mode permits recording ahead of the hearing time and says so.
    assert_eq!(
        b["demo_note"],
        "Recorded ahead of the hearing time (demo only)"
    );
    let h = &b["hearing"];
    assert_eq!(h["status"], "held");
    assert_eq!(h["outcome_summary"], "Heard both sides; decision reserved.");
    assert_eq!(h["outcome_recorded_by_name"], "Viktor Hale");
    assert!(h["outcome_recorded_at"].is_string());
    assert_eq!(h["participants"][0]["attended"], 1);
    assert_eq!(h["participants"][1]["attended"], 0);
    // Follow-up task and continuation hearing were created in the same transaction.
    assert_eq!(b["task"]["kind"], "follow_up");
    assert_eq!(b["task"]["hearing_id"], hid);
    assert_eq!(b["task"]["assignee_name"], "Viktor Hale");
    assert_eq!(b["next_hearing"]["status"], "draft");
    assert_eq!(
        b["next_hearing_note"],
        "Next hearing saved as a draft for a scheduler to confirm."
    );
    assert_eq!(b["next_hearing"]["previous_hearing_id"], hid);
    assert_eq!(
        b["next_hearing"]["participants"].as_array().unwrap().len(),
        2
    );

    // A held hearing NEVER changes the case status.
    let (_, card) = olga.get(&format!("/api/cases/{case_id}")).await;
    assert_eq!(card["case"]["status"], "registered");

    // A held hearing cannot be adjourned; only corrected with the separate permission.
    let (s, b) = olga
        .post(
            &format!("/api/hearings/{hid}/adjourn"),
            json!({"starts_local": "2026-11-26T09:00", "ends_local": "2026-11-26T10:00", "reason": "x", "authorised_by": "y"}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "invalid_transition");
    let (s, b) = olga
        .post(
            &format!("/api/hearings/{hid}/correct"),
            json!({"reason": "wrong button", "status": "cancelled"}),
        )
        .await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    let (s, b) = viktor_c
        .post(
            &format!("/api/hearings/{hid}/correct"),
            json!({"reason": "Recorded on the wrong hearing", "status": "cancelled"}),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["status"], "cancelled");
    assert_eq!(b["status_reason"], "Recorded on the wrong hearing");
    assert_eq!(b["outcome_summary"], "Heard both sides; decision reserved."); // nothing cleared
}

#[tokio::test]
async fn outcome_not_held_needs_reason_and_keeps_the_record() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _n, _v, room) = seeded_case(&olga, "Not held").await;
    let h = scheduled(&olga, case_id, "2026-11-25T09:00", "2026-11-25T10:00", room).await;
    let hid = h["id"].as_i64().unwrap();
    let viktor = olga.switch("viktor").await;

    let (s, b) = viktor.post(&format!("/api/hearings/{hid}/outcome"), json!({"held": false})).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");

    let (s, b) = viktor
        .post(
            &format!("/api/hearings/{hid}/outcome"),
            json!({"held": false, "reason": "The respondent did not appear"}),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["hearing"]["status"], "cancelled");
    assert_eq!(b["hearing"]["status_reason"], "The respondent did not appear");

    // The date, participants and preparation stay on record.
    let (_, list) = olga.get(&format!("/api/cases/{case_id}/hearings")).await;
    let h = list["items"].as_array().unwrap().iter().find(|x| x["id"] == hid).unwrap();
    assert_eq!(h["starts_local"], "2026-11-25T09:00");
}

#[tokio::test]
async fn hearings_follow_case_visibility() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _n, _v, room) = seeded_case(&olga, "Visibility").await;
    let h = scheduled(&olga, case_id, "2026-11-19T09:00", "2026-11-19T10:00", room).await;
    let hid = h["id"].as_i64().unwrap();

    // Sergei is not assigned to the case: 404 everywhere, excluded from the feed.
    let sergei = olga.switch("sergei").await;
    let (s, _) = sergei.get(&format!("/api/cases/{case_id}/hearings")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = sergei.get(&format!("/api/hearings/{hid}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, feed) = sergei.get("/api/hearings?from=2026-11-01&to=2026-12-31").await;
    assert!(feed["items"].as_array().unwrap().iter().all(|x| x["id"] != hid));

    // Elena (view_all) sees it in the case list and in the calendar feed.
    let elena = olga.switch("elena").await;
    let (s, list) = elena.get(&format!("/api/cases/{case_id}/hearings")).await;
    ok(s, &list);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let (_, feed) = elena.get("/api/hearings?from=2026-11-19&to=2026-11-19").await;
    assert!(feed["items"].as_array().unwrap().iter().any(|x| x["id"] == hid));
}

// ------------------------------------------------------------------ tasks

#[tokio::test]
async fn task_lifecycle_and_assignee_access() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _n, _v, _room) = seeded_case(&olga, "Tasks").await;
    let elena = olga.switch("elena").await;
    let sergei_id = user_id(&elena, "Sergei").await;

    // Sergei has no access to the case → cannot be assigned a task on it.
    let (s, b) = olga
        .post(
            &format!("/api/cases/{case_id}/tasks"),
            json!({"title": "Deliver the file", "assignee_user_id": sergei_id}),
        )
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    assert!(b["error"]["message"].as_str().unwrap().contains("has no access to this case"));

    // After assignment the same request works.
    let (s, b) = elena
        .post(
            &format!("/api/cases/{case_id}/assignments"),
            json!({"user_id": sergei_id, "role": "service_officer", "reason": "Will deliver notices"}),
        )
        .await;
    ok(s, &b);
    let (s, t) = olga
        .post(
            &format!("/api/cases/{case_id}/tasks"),
            json!({"title": "Deliver the file", "assignee_user_id": sergei_id, "due_date": "2026-11-20"}),
        )
        .await;
    ok(s, &t);
    assert_eq!(t["status"], "open");
    assert_eq!(t["assignee_name"], "Sergei Novak");
    let tid = t["id"].as_i64().unwrap();
    let tv = t["version"].as_i64().unwrap();

    // Edit with optimistic locking.
    let (s, b) = olga
        .patch(&format!("/api/tasks/{tid}"), json!({"version": tv, "due_date": "2026-11-21"}))
        .await;
    ok(s, &b);
    assert_eq!(b["due_date"], "2026-11-21");

    // Result and reason are mandatory for the respective transitions.
    let sergei = olga.switch("sergei").await;
    let (s, b) = sergei.post(&format!("/api/tasks/{tid}/complete"), json!({})).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = sergei
        .post(
            &format!("/api/tasks/{tid}/complete"),
            json!({"result": "Handed over at the counter"}),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["status"], "done");
    assert_eq!(b["result"], "Handed over at the counter");
    assert!(b["closed_by_name"].is_string());
    // Done is final.
    let (s, b) = sergei.post(&format!("/api/tasks/{tid}/cancel"), json!({"reason": "x"})).await;
    err(s, &b, StatusCode::CONFLICT, "invalid_transition");

    // cancel requires a reason.
    let (s, t2) = olga
        .post(&format!("/api/cases/{case_id}/tasks"), json!({"title": "Prepare room booking"}))
        .await;
    ok(s, &t2);
    let tid2 = t2["id"].as_i64().unwrap();
    let (s, b) = olga.post(&format!("/api/tasks/{tid2}/cancel"), json!({})).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = olga
        .post(&format!("/api/tasks/{tid2}/cancel"), json!({"reason": "Booking moved to dispatch"}))
        .await;
    ok(s, &b);
    assert_eq!(b["status"], "cancelled");
    assert_eq!(b["status_reason"], "Booking moved to dispatch");

    // carry-forward requires a reason and leaves an explained open item resolved.
    let (s, t3) = olga
        .post(&format!("/api/cases/{case_id}/tasks"), json!({"title": "Check service address"}))
        .await;
    ok(s, &t3);
    let tid3 = t3["id"].as_i64().unwrap();
    let (s, b) = olga.post(&format!("/api/tasks/{tid3}/carry-forward"), json!({})).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = olga
        .post(
            &format!("/api/tasks/{tid3}/carry-forward"),
            json!({"reason": "Continues after closure"}),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["status"], "carried_forward");

    // Sergei's personal list shows his remaining open tasks only (here: none).
    let (_, mine) = sergei.get("/api/tasks?mine=1&status=open").await;
    assert!(mine["items"].as_array().unwrap().iter().all(|t| t["assignee_user_id"] == sergei_id));

    // Pavel has no case access at all.
    let pavel = olga.switch("pavel").await;
    let (s, _) = pavel.get(&format!("/api/cases/{case_id}/tasks")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = pavel.get(&format!("/api/tasks/{tid}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn open_items_block_closing_until_resolved() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (case_id, _n, _v, room) = seeded_case(&olga, "Closing blocked").await;
    let h = scheduled(&olga, case_id, "2026-11-27T09:00", "2026-11-27T10:00", room).await;
    let hid = h["id"].as_i64().unwrap();
    let (s, t) = olga
        .post(
            &format!("/api/cases/{case_id}/tasks"),
            json!({"title": "File the attendance sheet"}),
        )
        .await;
    ok(s, &t);
    let tid = t["id"].as_i64().unwrap();

    // A scheduled hearing and an open task block closing.
    let (s, b) = olga.post(&format!("/api/cases/{case_id}/close"), json!({"basis": "decided"})).await;
    err(s, &b, StatusCode::CONFLICT, "open_items");
    let kinds: Vec<&str> = b["error"]["details"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"hearing") && kinds.contains(&"task"), "items: {kinds:?}");

    // Resolve each item explicitly, then closing succeeds.
    let (s, b) = olga
        .post(
            &format!("/api/hearings/{hid}/cancel"),
            json!({"reason": "Settled before the hearing"}),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["status"], "cancelled");
    let (s, _) = olga
        .post(
            &format!("/api/tasks/{tid}/carry-forward"),
            json!({"reason": "Sheet arrives with the decision copies"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let vid = settlement_document(&olga, &app, case_id).await;
    let (s, b) = olga.post(&format!("/api/cases/{case_id}/close"), json!({"basis": "settled", "basis_document_version_id": vid})).await;
    ok(s, &b);

    // No new hearings on a closed case.
    let (s, b) = olga
        .post(
            &format!("/api/cases/{case_id}/hearings"),
            hearing_body("2026-12-01T09:00", "2026-12-01T10:00", room, true),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "invalid_transition");
}

#[tokio::test]
async fn calendar_uses_court_dates_and_all_filters() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _, judge, room) = seeded_case(&olga, "Calendar dates").await;
    let before = draft(&olga, cid, "2026-11-16T23:00", "2026-11-16T23:30", room).await;
    let first = draft(&olga, cid, "2026-11-17T00:00", "2026-11-17T00:30", room).await;
    let last = draft(&olga, cid, "2026-11-17T23:30", "2026-11-18T00:00", room).await;
    let after = draft(&olga, cid, "2026-11-18T00:00", "2026-11-18T00:30", room).await;
    let (s, feed) = olga
        .get(&format!(
            "/api/hearings?from=2026-11-17&to=2026-11-17&case_id={cid}&judge={judge}&room={room}"
        ))
        .await;
    ok(s, &feed);
    let ids: Vec<_> = feed["items"].as_array().unwrap().iter().map(|h| h["id"].clone()).collect();
    assert_eq!(ids, [first["id"].clone(), last["id"].clone()]);
    assert!(!ids.contains(&before["id"]) && !ids.contains(&after["id"]));
    assert_eq!(first["starts_at"], "2026-11-16T12:00:00Z");
    let (s, feed) = olga
        .get(&format!("/api/hearings?from=2026-11-17&to=2026-11-17&case_id={cid}&judge=&room="))
        .await;
    ok(s, &feed);
    assert_eq!(feed["items"].as_array().unwrap().len(), 2);
    let room2 = second_room(&olga).await;
    let (s, feed) = olga.get(&format!("/api/hearings?case_id={cid}&room={room2}")).await;
    ok(s, &feed);
    assert_eq!(feed["items"], json!([]));
    let (s, b) = olga.get("/api/hearings?from=2026-02-30").await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    for key in [
        "id",
        "case_id",
        "case_number",
        "hearing_type",
        "hearing_type_label",
        "status",
        "starts_at",
        "ends_at",
        "starts_local",
        "ends_local",
        "room_id",
        "room_name",
        "judge_user_id",
        "judge_name",
        "notes",
        "previous_hearing_id",
        "adjourned_to_id",
        "status_reason",
        "status_authorised_by",
        "conflict_override",
        "override_reason",
        "outcome_summary",
        "next_step",
        "outcome_recorded_by_name",
        "outcome_recorded_at",
        "version",
        "participants",
    ] {
        assert!(first.get(key).is_some(), "hearing JSON missing {key}");
    }
}

#[tokio::test]
async fn room_only_conflicts_and_buffer_on_both_sides() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    // No judge on either case: this isolates room conflicts.
    let (cid, number) = register_case(&olga, "Room only").await;
    let (_, refs) = olga.get("/api/ref").await;
    let room = refs["rooms"][0]["id"].as_i64().unwrap();
    let room2 = second_room(&olga).await;
    let original = scheduled(&olga, cid, "2026-11-17T09:00", "2026-11-17T10:00", room).await;
    assert!(original["judge_user_id"].is_null());
    let (cid2, _) = register_case(&olga, "Other room booking").await;
    let (s, b) = olga
        .post(
            &format!("/api/cases/{cid2}/hearings"),
            hearing_body("2026-11-17T09:15", "2026-11-17T09:45", room, true),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "hearing_conflict");
    assert_eq!(b["error"]["details"]["conflicts"][0]["case_number"], number);
    scheduled(&olga, cid2, "2026-11-17T09:15", "2026-11-17T09:45", room2).await;
    olga.db(&app)
        .open()
        .unwrap()
        .execute("UPDATE settings SET value = '15' WHERE key = 'hearing_buffer_minutes'", [])
        .unwrap();
    for (start, end) in [("2026-11-17T08:00", "2026-11-17T08:46"), ("2026-11-17T10:14", "2026-11-17T11:00")] {
        let h = draft(&olga, cid2, start, end, room).await;
        let (s, b) = olga.post(&format!("/api/hearings/{}/confirm", h["id"]), json!({"version":1})).await;
        err(s, &b, StatusCode::CONFLICT, "hearing_conflict");
        assert_eq!(b["error"]["details"]["conflicts"][0]["hearing_id"], original["id"]);
    }
    scheduled(&olga, cid2, "2026-11-17T08:00", "2026-11-17T08:45", room).await;
    scheduled(&olga, cid2, "2026-11-17T10:15", "2026-11-17T11:00", room).await;
}

#[tokio::test]
async fn hearing_validation_and_draft_cancellation() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _, judge, room) = seeded_case(&olga, "Validate hearing").await;
    let (other, _) = register_case(&olga, "Foreign participants").await;
    let foreign = party_ids(&olga, other).await[0];
    let sergei = user_id(&olga, "Sergei").await;
    let url = format!("/api/cases/{cid}/hearings");
    let base = hearing_body("2026-11-17T09:00", "2026-11-17T10:00", room, false);
    let before = case_counts(&olga, &app, cid);
    for invalid in [
        json!({"hearing_type": "unknown"}),
        json!({"ends_local": "2026-11-17T09:00"}),
        json!({"starts_local": "bad"}),
        json!({"room_id": 999999}),
        json!({"judge_user_id": sergei}),
        json!({"participants": [{"party_id": foreign, "role": "claimant"}]}),
        json!({"participants": [{"party_id": foreign, "user_id": judge, "role": "claimant"}]}),
        json!({"participants": [{"user_id": 999999, "role": "clerk"}]}),
    ] {
        let mut body = base.clone();
        body.as_object_mut().unwrap().extend(invalid.as_object().unwrap().clone());
        let (s, b) = olga.post(&url, body).await;
        err(s, &b, StatusCode::BAD_REQUEST, "validation");
        assert_eq!(case_counts(&olga, &app, cid), before);
    }
    let p = party_ids(&olga, cid).await[0];
    let mut body = base;
    body["participants"] = json!([{"party_id": p, "role": "claimant", "required": false}]);
    let (s, h) = olga.post(&url, body).await;
    ok(s, &h);
    let id = h["id"].as_i64().unwrap();
    for suffix in ["outcome", "adjourn", "correct"] {
        let body = match suffix {
            "outcome" => json!({"held": true, "outcome_summary": "Draft never held"}),
            "adjourn" => json!({"starts_local": "2026-11-19T09:00", "ends_local": "2026-11-19T10:00", "reason": "x", "authorised_by": "y"}),
            _ => json!({"reason": "x", "status": "scheduled"}),
        };
        let actor = if suffix == "adjourn" {
            olga.clone()
        } else {
            olga.switch("viktor").await
        };
        let (s, b) = actor.post(&format!("/api/hearings/{id}/{suffix}"), body).await;
        err(s, &b, StatusCode::CONFLICT, "invalid_transition");
    }
    let (s, b) = olga.post(&format!("/api/hearings/{id}/cancel"), json!({"reason": "  "})).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, cancelled) = olga
        .post(&format!("/api/hearings/{id}/cancel"), json!({"reason": "Booked in error"}))
        .await;
    ok(s, &cancelled);
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["starts_at"], h["starts_at"]);
    assert_eq!(cancelled["participants"], h["participants"]);
    let (s, b) = olga.post(&format!("/api/hearings/{id}/confirm"), json!({"version":1})).await;
    err(s, &b, StatusCode::CONFLICT, "invalid_transition");
    let (s, b) = olga.post(&format!("/api/hearings/{id}/cancel"), json!({"reason": "Again"})).await;
    err(s, &b, StatusCode::CONFLICT, "invalid_transition");
}

#[tokio::test]
async fn adjourn_conflict_rolls_back_and_renotifies_only_required_participants() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _, judge, room) = seeded_case(&olga, "Adjourn rollback").await;
    let elena = olga.switch("elena").await;
    let service = user_id(&elena, "Sergei").await;
    let (s, b) = elena
        .post(
            &format!("/api/cases/{cid}/assignments"),
            json!({"user_id": service, "role": "service_officer", "reason": "Re-notify participants"}),
        )
        .await;
    ok(s, &b);
    let p = party_ids(&olga, cid).await;
    let mut body = hearing_body("2026-11-17T09:00", "2026-11-17T10:00", room, true);
    body["participants"] = json!([{"party_id": p[0], "role": "claimant"}, {"party_id": p[1], "role": "respondent", "required": false}, {"user_id": judge, "role": "judge"}]);
    let (s, h) = olga.post(&format!("/api/cases/{cid}/hearings"), body).await;
    ok(s, &h);
    let id = h["id"].as_i64().unwrap();
    scheduled(&olga, cid, "2026-11-19T09:00", "2026-11-19T10:00", room).await;
    let before = case_counts(&olga, &app, cid);
    let mut req = json!({"starts_local": "2026-11-19T09:00", "ends_local": "2026-11-19T10:00", "reason": "Judge unavailable", "authorised_by": "Judge Viktor Hale"});
    let (s, b) = olga
        .post_idem(&format!("/api/hearings/{id}/adjourn"), "retry-after-conflict", req.clone())
        .await;
    err(s, &b, StatusCode::CONFLICT, "hearing_conflict");
    assert_eq!(case_counts(&olga, &app, cid), before);
    let (_, unchanged) = olga.get(&format!("/api/hearings/{id}")).await;
    assert_eq!(unchanged, h);
    req["starts_local"] = json!("2026-11-19T11:00");
    req["ends_local"] = json!("2026-11-19T12:00");
    let (s, moved) = olga
        .post_idem(&format!("/api/hearings/{id}/adjourn"), "retry-after-conflict", req.clone())
        .await;
    ok(s, &moved);
    assert_eq!(moved["tasks"].as_array().unwrap().len(), 2);
    assert!(moved["tasks"].as_array().unwrap().iter().all(|t| t["assignee_user_id"] == service));
    assert_eq!(moved["new"]["participants"][1]["required"], 0);
    assert!(
        moved["new"]["participants"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["attended"].is_null())
    );
    assert_eq!(audit_count_for_case(&olga, &app, "task.created", cid), 2);
    let after = case_counts(&olga, &app, cid);
    let (s, replay) = olga
        .post_idem(&format!("/api/hearings/{id}/adjourn"), "retry-after-conflict", req.clone())
        .await;
    ok(s, &replay);
    assert_eq!(replay, moved);
    assert_eq!(case_counts(&olga, &app, cid), after);
    req["reason"] = json!("Different request");
    let (s, b) = olga
        .post_idem(&format!("/api/hearings/{id}/adjourn"), "retry-after-conflict", req)
        .await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    let conn = olga.db(&app).open().unwrap();
    assert_eq!(tuvalu_court::audit::verify_chain(&conn).unwrap().1, None);
    assert!(
        conn.execute("UPDATE hearings SET starts_at = '2026-11-16T20:00:00Z' WHERE id = ?1", [id])
            .is_err()
    );
}

#[tokio::test]
async fn outcome_followups_are_atomic_and_correction_checks_conflicts() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _, judge, room) = seeded_case(&olga, "Outcome rollback").await;
    grant_perm(&olga, &app, "Viktor", "hearing.schedule").await;
    let viktor = olga.switch("viktor").await;
    let party = party_ids(&olga, cid).await[0];
    let mut body = hearing_body("2026-11-17T09:00", "2026-11-17T10:00", room, true);
    body["participants"] = json!([{"party_id": party, "role": "claimant"}]);
    let (s, original) = olga.post(&format!("/api/cases/{cid}/hearings"), body).await;
    ok(s, &original);
    let id = original["id"].as_i64().unwrap();
    let pid = original["participants"][0]["id"].as_i64().unwrap();
    let blocked = scheduled(&olga, cid, "2026-11-19T09:00", "2026-11-19T10:00", room).await;
    let before = case_counts(&olga, &app, cid);
    let req = json!({"held": true, "outcome_summary": "Decision reserved", "attendance": [{"participant_id": pid, "attended": true}],
        "next_task": {"title": "Draft decision", "assignee_user_id": judge},
        "next_hearing": {"starts_local": "2026-11-19T09:00", "ends_local": "2026-11-19T10:00"}});
    let (s, b) = viktor
        .post(&format!("/api/hearings/{id}/outcome"), req)
        .await;
    err(s, &b, StatusCode::CONFLICT, "hearing_conflict");
    assert_eq!(
        b["error"]["details"]["conflicts"][0]["hearing_id"],
        blocked["id"]
    );
    assert_eq!(case_counts(&olga, &app, cid), before);
    let (_, unchanged) = olga.get(&format!("/api/hearings/{id}")).await;
    assert_eq!(unchanged, original);
    let sergei = user_id(&olga, "Sergei").await;
    for req in [
        json!({"held": true, "outcome_summary": " "}),
        json!({"held": true, "outcome_summary": "x", "attendance": [{"participant_id": 999999, "attended": true}]}),
        json!({"held": true, "outcome_summary": "x", "next_task": {"title": "No access", "assignee_user_id": sergei}}),
    ] {
        let (s, b) = viktor
            .post(&format!("/api/hearings/{id}/outcome"), req)
            .await;
        err(s, &b, StatusCode::BAD_REQUEST, "validation");
        assert_eq!(case_counts(&olga, &app, cid), before);
    }
    let (s, outcome) = viktor
        .post(
            &format!("/api/hearings/{id}/outcome"),
            json!({"held": true, "outcome_summary": "Decision reserved", "attendance": [{"participant_id": pid, "attended": true}]}),
        )
        .await;
    ok(s, &outcome);
    let held = outcome["hearing"].clone();
    for req in [
        json!({"status": "scheduled"}),
        json!({"status": "draft", "reason": "Wrong record"}),
    ] {
        let (s, b) = viktor
            .post(&format!("/api/hearings/{id}/correct"), req)
            .await;
        err(s, &b, StatusCode::BAD_REQUEST, "validation");
    }
    let other = scheduled(&olga, cid, "2026-11-17T10:00", "2026-11-17T11:00", room).await;
    olga.db(&app)
        .open()
        .unwrap()
        .execute(
            "UPDATE settings SET value = '15' WHERE key = 'hearing_buffer_minutes'",
            [],
        )
        .unwrap();
    let (s, b) = viktor
        .post(
            &format!("/api/hearings/{id}/correct"),
            json!({"status": "scheduled", "reason": "Recorded on the wrong date"}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "hearing_conflict");
    let (_, unchanged) = olga.get(&format!("/api/hearings/{id}")).await;
    assert_eq!(unchanged, held);
    let (s, b) = olga
        .post(
            &format!("/api/hearings/{}/cancel", other["id"]),
            json!({"reason": "Room no longer needed"}),
        )
        .await;
    ok(s, &b);
    let (s, corrected) = viktor
        .post(
            &format!("/api/hearings/{id}/correct"),
            json!({"status": "scheduled", "reason": "Recorded on the wrong date"}),
        )
        .await;
    ok(s, &corrected);
    assert_eq!(corrected["status"], "scheduled");
    for field in [
        "starts_at",
        "ends_at",
        "outcome_summary",
        "outcome_recorded_at",
        "participants",
    ] {
        assert_eq!(corrected[field], held[field], "correction changed {field}");
    }
    let conn = olga.db(&app).open().unwrap();
    let details: String = conn
        .query_row(
            "SELECT details FROM audit_events WHERE action = 'hearing.corrected' AND entity_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    let details: Value = serde_json::from_str(&details).unwrap();
    assert_eq!(details["before"], held);
    let (_, card) = olga.get(&format!("/api/cases/{cid}")).await;
    assert_eq!(card["case"]["status"], "registered");
}

#[tokio::test]
async fn hearing_permissions_unassigned_users_and_sandboxes() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _, _, room) = seeded_case(&olga, "Hearing denials").await;
    let h = draft(&olga, cid, "2026-11-17T09:00", "2026-11-17T10:00", room).await;
    let id = h["id"].as_i64().unwrap();
    let viktor = olga.switch("viktor").await;
    let sergei = olga.switch("sergei").await;
    let other_sandbox = app.persona("olga").await;
    let create_url = format!("/api/cases/{cid}/hearings");
    let req = hearing_body("2026-11-19T09:00", "2026-11-19T10:00", room, false);
    let (s, b) = viktor.post(&create_url, req.clone()).await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    for client in [&sergei, &other_sandbox] {
        let (s, b) = client.post(&create_url, req.clone()).await;
        err(s, &b, StatusCode::NOT_FOUND, "not_found");
    }
    let actions = [
        ("confirm", json!({"version":1})),
        ("cancel", json!({"reason": "x"})),
        (
            "adjourn",
            json!({"starts_local": "2026-11-19T09:00", "ends_local": "2026-11-19T10:00", "reason": "x", "authorised_by": "Judge"}),
        ),
        ("outcome", json!({"held": true, "outcome_summary": "x"})),
        ("correct", json!({"reason": "x", "status": "scheduled"})),
    ];
    for (action, body) in actions {
        let url = format!("/api/hearings/{id}/{action}");
        for client in [&sergei, &other_sandbox] {
            let (s, b) = client.post(&url, body.clone()).await;
            err(s, &b, StatusCode::NOT_FOUND, "not_found");
        }
        let denied = if matches!(action, "outcome" | "correct") { &olga } else { &viktor };
        let (s, b) = denied.post(&url, body).await;
        err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    }
    let patch = json!({"version": h["version"], "notes": "Changed"});
    let (s, b) = viktor.patch(&format!("/api/hearings/{id}"), patch.clone()).await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    for client in [&sergei, &other_sandbox] {
        let (s, b) = client.patch(&format!("/api/hearings/{id}"), patch.clone()).await;
        err(s, &b, StatusCode::NOT_FOUND, "not_found");
        let (s, b) = client.get(&format!("/api/cases/{cid}/hearings")).await;
        err(s, &b, StatusCode::NOT_FOUND, "not_found");
        let (s, feed) = client.get(&format!("/api/hearings?case_id={cid}")).await;
        ok(s, &feed);
        assert_eq!(feed["items"], json!([]));
    }
    // Replay is re-authorised after access is revoked.
    let (s, original) = olga.post_idem(&create_url, "revoke-create", req.clone()).await;
    ok(s, &original);
    let uid = user_id(&olga, "Olga").await;
    olga.db(&app)
        .open()
        .unwrap()
        .execute(
            "UPDATE case_assignments SET end_at = ?1 WHERE case_id = ?2 AND user_id = ?3",
            rusqlite::params![tuvalu_court::time::now_utc(), cid, uid],
        )
        .unwrap();
    let (s, b) = olga.post_idem(&create_url, "revoke-create", req).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn confirm_rechecks_judge_assignment_and_override_is_audited() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _, judge, room) = seeded_case(&olga, "Judge changed").await;
    let h = draft(&olga, cid, "2026-11-17T09:00", "2026-11-17T10:00", room).await;
    let id = h["id"].as_i64().unwrap();
    let conn = olga.db(&app).open().unwrap();
    conn.execute(
        "UPDATE case_assignments SET end_at = ?1 WHERE case_id = ?2 AND role = 'judge'",
        rusqlite::params![tuvalu_court::time::now_utc(), cid],
    )
    .unwrap();
    let (s, b) = olga.post(&format!("/api/hearings/{id}/confirm"), json!({"version":1})).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let elena = olga.switch("elena").await;
    let (s, b) = elena
        .post(
            &format!("/api/cases/{cid}/assignments"),
            json!({"user_id": judge, "role": "judge", "reason": "Back on this case"}),
        )
        .await;
    ok(s, &b);
    scheduled(&olga, cid, "2026-11-17T09:15", "2026-11-17T10:15", room).await;
    let (s, b) = olga
        .post(&format!("/api/hearings/{id}/confirm"), json!({"version":1,"override_reason": " "}))
        .await;
    err(s, &b, StatusCode::CONFLICT, "hearing_conflict");
    let (s, b) = olga
        .post(&format!("/api/hearings/{id}/confirm"), json!({"version":1,"override_reason": "Urgent hearing"}))
        .await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    grant_perm(&elena, &app, "Elena", "hearing.schedule").await;
    let (s, b) = elena
        .post(&format!("/api/hearings/{id}/confirm"), json!({"version":1,"override_reason": "Urgent hearing"}))
        .await;
    ok(s, &b);
    assert_eq!(b["conflict_override"], 1);
    let details: String = conn
        .query_row(
            "SELECT details FROM audit_events WHERE action = 'hearing.conflict_override' AND entity_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    let details: Value = serde_json::from_str(&details).unwrap();
    assert_eq!(details["reason"], "Urgent hearing");
    let (s, b) = elena.post(&format!("/api/hearings/{id}/confirm"), json!({"version":1})).await;
    err(s, &b, StatusCode::CONFLICT, "invalid_transition");

    // A judge assigned after draft creation is resolved and persisted on confirmation.
    let (new_case, _) = register_case(&olga, "Judge assigned after drafting").await;
    let mut req = hearing_body("2026-11-23T09:00", "2026-11-23T10:00", room, false);
    req["judge_user_id"] = json!(judge);
    let (s, b) = olga.post(&format!("/api/cases/{new_case}/hearings"), req).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let later = draft(&olga, new_case, "2026-11-23T09:00", "2026-11-23T10:00", room).await;
    assert!(later["judge_user_id"].is_null());
    let (s, b) = elena
        .post(
            &format!("/api/cases/{new_case}/assignments"),
            json!({"user_id": judge, "role": "judge", "reason": "Assigned after drafting"}),
        )
        .await;
    ok(s, &b);
    let (s, b) = olga.post(&format!("/api/hearings/{}/confirm", later["id"]), json!({"version":1})).await;
    ok(s, &b);
    assert_eq!(b["judge_user_id"], judge);
}

fn production_client(app: &TestApp, username: &str) -> Client {
    let conn = app.state.main_db.as_ref().unwrap().open().unwrap();
    let uid: i64 = conn
        .query_row("SELECT id FROM users WHERE username = ?1", [username], |r| r.get(0))
        .unwrap();
    let token = tuvalu_court::auth::create_session(&conn, uid, true, 1).unwrap();
    Client {
        router: app.router.clone(),
        sandbox: None,
        session: Some(token),
    }
}

#[tokio::test]
async fn production_rejects_future_outcomes_but_accepts_past_hearings() {
    let app = TestApp::production();
    // Fictional fixtures only; requests still run through the production router and authentication.
    tuvalu_court::seed::seed_demo(app.state.main_db.as_ref().unwrap()).unwrap();
    let olga = production_client(&app, "olga");
    let elena = production_client(&app, "elena");
    let viktor = production_client(&app, "viktor");
    let (cid, _) = register_case(&olga, "Production time rule").await;
    let judge = user_id(&olga, "Viktor").await;
    let (s, b) = elena
        .post(
            &format!("/api/cases/{cid}/assignments"),
            json!({"user_id": judge, "role": "judge", "reason": "Assigned"}),
        )
        .await;
    ok(s, &b);
    let (_, refs) = olga.get("/api/ref").await;
    let room = refs["rooms"][0]["id"].as_i64().unwrap();
    let future = scheduled(&olga, cid, "2099-11-17T09:00", "2099-11-17T10:00", room).await;
    let before = case_counts(&olga, &app, cid);
    for req in [
        json!({"held": true, "outcome_summary": "Too early"}),
        json!({"held": false, "reason": "Too early"}),
    ] {
        let (s, b) = viktor.post(&format!("/api/hearings/{}/outcome", future["id"]), req).await;
        err(s, &b, StatusCode::CONFLICT, "invalid_transition");
        assert_eq!(case_counts(&olga, &app, cid), before);
    }
    let past = scheduled(&olga, cid, "2001-11-17T09:00", "2001-11-17T10:00", room).await;
    let (s, b) = viktor
        .post(
            &format!("/api/hearings/{}/outcome", past["id"]),
            json!({"held": true, "outcome_summary": "Historical hearing recorded"}),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["hearing"]["status"], "held");
    assert!(b.get("demo_note").is_none());
}

#[tokio::test]
async fn task_edits_validate_links_assignees_dates_and_versions() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _, _, room) = seeded_case(&olga, "Task editing").await;
    let (other, _) = register_case(&olga, "Other task case").await;
    let foreign = draft(&olga, other, "2026-11-19T09:00", "2026-11-19T10:00", room).await;
    let before = case_counts(&olga, &app, cid);
    let sergei = user_id(&olga, "Sergei").await;
    let url = format!("/api/cases/{cid}/tasks");
    for req in [
        json!({"title": " "}),
        json!({"title": "Bad link", "hearing_id": foreign["id"]}),
        json!({"title": "Bad date", "due_date": "2026-02-30"}),
        json!({"title": "Missing user", "assignee_user_id": 999999}),
        json!({"title": "No access", "assignee_user_id": sergei}),
    ] {
        let (s, b) = olga.post(&url, req).await;
        err(s, &b, StatusCode::BAD_REQUEST, "validation");
        assert_eq!(case_counts(&olga, &app, cid), before);
    }
    let own = draft(&olga, cid, "2026-11-17T09:00", "2026-11-17T10:00", room).await;
    let uid = user_id(&olga, "Olga").await;
    let (s, t) = olga.post(&url, json!({"title": " Prepare record ", "description": "Initial", "hearing_id": own["id"], "assignee_user_id": uid, "due_date": "2026-11-30"})).await;
    ok(s, &t);
    assert_eq!(t["title"], "Prepare record");
    assert_eq!(t["kind"], "general");
    assert_eq!(t["hearing_id"], own["id"]);
    for key in [
        "id",
        "case_id",
        "case_number",
        "intake_id",
        "hearing_id",
        "kind",
        "title",
        "description",
        "assignee_user_id",
        "assignee_name",
        "due_date",
        "status",
        "result",
        "status_reason",
        "created_by_name",
        "created_at",
        "closed_by_name",
        "closed_at",
        "version",
    ] {
        assert!(t.get(key).is_some(), "task JSON missing {key}");
    }
    let id = t["id"].as_i64().unwrap();
    let version = t["version"].as_i64().unwrap();
    for change in [
        json!({"title": " "}),
        json!({"assignee_user_id": sergei}),
        json!({"due_date": "bad"}),
    ] {
        let mut req = change;
        req["version"] = json!(version);
        let (s, b) = olga.patch(&format!("/api/tasks/{id}"), req).await;
        err(s, &b, StatusCode::BAD_REQUEST, "validation");
        let (_, current) = olga.get(&format!("/api/tasks/{id}")).await;
        assert_eq!(current, t);
    }
    let (s, updated) = olga
        .patch(
            &format!("/api/tasks/{id}"),
            json!({"version": version, "title": "Updated title", "description": "", "due_date": ""}),
        )
        .await;
    ok(s, &updated);
    assert_eq!(updated["version"], version + 1);
    assert!(updated["description"].is_null() && updated["due_date"].is_null());
    let (s, b) = olga
        .patch(&format!("/api/tasks/{id}"), json!({"version": version, "title": "Stale"}))
        .await;
    err(s, &b, StatusCode::CONFLICT, "version_conflict");
    assert_eq!(b["error"]["details"]["current"], updated);
    let (s, b) = olga.post(&format!("/api/tasks/{id}/complete"), json!({"result": " "})).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, done) = olga.post(&format!("/api/tasks/{id}/complete"), json!({"result": "Filed"})).await;
    ok(s, &done);
    let (s, b) = olga
        .patch(
            &format!("/api/tasks/{id}"),
            json!({"version": done["version"], "title": "After done"}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "invalid_transition");
    let (_, card) = olga.get(&format!("/api/cases/{cid}")).await;
    assert_eq!(card["case"]["status"], "registered");
}

#[tokio::test]
async fn task_permissions_assignee_exception_and_ended_assignment() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _, judge, _) = seeded_case(&olga, "Task permissions").await;
    let viktor = olga.switch("viktor").await;
    let sergei = olga.switch("sergei").await;
    let other_sandbox = app.persona("olga").await;
    let mut tasks = vec![];
    for title in ["Complete", "Cancel", "Carry forward"] {
        let (s, t) = olga
            .post(
                &format!("/api/cases/{cid}/tasks"),
                json!({"title": title, "assignee_user_id": judge}),
            )
            .await;
        ok(s, &t);
        tasks.push(t);
    }
    let uid = user_id(&olga, "Olga").await;
    let conn = olga.db(&app).open().unwrap();
    conn.execute(
        "DELETE FROM user_permissions WHERE permission = 'task.manage' AND user_id IN (?1, ?2)",
        rusqlite::params![uid, judge],
    )
    .unwrap();
    let id = tasks[0]["id"].as_i64().unwrap();
    let (s, b) = olga.post(&format!("/api/cases/{cid}/tasks"), json!({"title": "New"})).await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    let (s, b) = viktor
        .patch(
            &format!("/api/tasks/{id}"),
            json!({"version": tasks[0]["version"], "title": "Edit"}),
        )
        .await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    for (i, (action, body, expected)) in [
        ("complete", json!({"result": "Filed"}), "done"),
        ("cancel", json!({"reason": "Not needed"}), "cancelled"),
        ("carry-forward", json!({"reason": "Continue later"}), "carried_forward"),
    ]
    .into_iter()
    .enumerate()
    {
        let tid = tasks[i]["id"].as_i64().unwrap();
        let url = format!("/api/tasks/{tid}/{action}");
        let (s, b) = olga.post(&url, body.clone()).await;
        err(s, &b, StatusCode::FORBIDDEN, "forbidden");
        for client in [&sergei, &other_sandbox] {
            let (s, b) = client.post(&url, body.clone()).await;
            err(s, &b, StatusCode::NOT_FOUND, "not_found");
        }
        let (s, changed) = viktor.post(&url, body).await;
        ok(s, &changed);
        assert_eq!(changed["status"], expected);
        assert_eq!(changed["closed_by_name"], "Viktor Hale");
        for (action, body) in [
            ("complete", json!({"result": "Again"})),
            ("cancel", json!({"reason": "Again"})),
            ("carry-forward", json!({"reason": "Again"})),
        ] {
            let (s, b) = viktor.post(&format!("/api/tasks/{tid}/{action}"), body).await;
            err(s, &b, StatusCode::CONFLICT, "invalid_transition");
        }
    }
    let (s, mine) = viktor.get("/api/tasks?mine=1&status=done").await;
    ok(s, &mine);
    assert_eq!(mine["items"].as_array().unwrap().len(), 1);
    assert_eq!(mine["items"][0]["id"], id);
    for client in [&sergei, &other_sandbox] {
        let (s, b) = client.get(&format!("/api/cases/{cid}/tasks")).await;
        err(s, &b, StatusCode::NOT_FOUND, "not_found");
        let (s, b) = client.post(&format!("/api/cases/{cid}/tasks"), json!({"title": "No access"})).await;
        err(s, &b, StatusCode::NOT_FOUND, "not_found");
        let (s, b) = client
            .patch(&format!("/api/tasks/{id}"), json!({"version": 1, "title": "No access"}))
            .await;
        err(s, &b, StatusCode::NOT_FOUND, "not_found");
        let (s, list) = client.get("/api/tasks").await;
        ok(s, &list);
        assert!(
            list["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["case_id"] != cid)
        );
    }
    conn.execute(
        "UPDATE case_assignments SET end_at = ?1 WHERE case_id = ?2 AND user_id = ?3",
        rusqlite::params![tuvalu_court::time::now_utc(), cid, judge],
    )
    .unwrap();
    let (s, b) = viktor.get(&format!("/api/tasks/{id}")).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    let (s, mine) = viktor.get("/api/tasks?mine=1").await;
    ok(s, &mine);
    assert_eq!(mine["items"], json!([]));
}

#[tokio::test]
async fn intake_tasks_require_intake_permission_and_orphan_tasks_are_hidden() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let sergei = olga.switch("sergei").await;
    let intake = new_intake(&olga, "Intake task").await;
    let uid = user_id(&olga, "Olga").await;
    let conn = olga.db(&app).open().unwrap();
    conn.execute(
        "INSERT INTO tasks (intake_id, kind, title, status, created_by, created_at) VALUES (?1, 'general', 'Check intake', 'open', ?2, ?3)",
        rusqlite::params![intake, uid, tuvalu_court::time::now_utc()],
    )
    .unwrap();
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO tasks (title, status, assignee_user_id, created_by, created_at) VALUES ('Unlinked', 'open', ?1, ?1, ?2)",
        rusqlite::params![uid, tuvalu_court::time::now_utc()],
    )
    .unwrap();
    let orphan = conn.last_insert_rowid();
    let (s, list) = olga.get("/api/tasks?status=open").await;
    ok(s, &list);
    let items = list["items"].as_array().unwrap();
    let for_intake: Vec<&Value> = items.iter().filter(|t| t["intake_id"] == intake).collect();
    assert_eq!(for_intake.len(), 1);
    assert_eq!(for_intake[0]["id"], id);
    assert!(items.iter().all(|t| t["id"] != orphan));
    let (s, b) = olga.get(&format!("/api/tasks/{orphan}")).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    let (s, b) = sergei.get(&format!("/api/tasks/{id}")).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    let (s, list) = sergei.get("/api/tasks").await;
    ok(s, &list);
    assert!(
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["id"] != id)
    );
    let (s, b) = olga.post(&format!("/api/tasks/{id}/complete"), json!({"result": "Checked"})).await;
    ok(s, &b);
    assert_eq!(b["status"], "done");
}

#[tokio::test]
async fn continuation_requires_scheduler_confirmation_and_scheduled_outcome_checks_conflicts() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _, _, room) = seeded_case(&olga, "Continuation permissions").await;
    let h = scheduled(&olga, cid, "2026-11-19T09:00", "2026-11-19T10:00", room).await;
    let blocked = scheduled(&olga, cid, "2026-11-20T09:00", "2026-11-20T10:00", room).await;
    let viktor = olga.switch("viktor").await;
    let body = json!({"held":true,"outcome_summary":"Continue later","next_hearing":{"starts_local":"2026-11-20T09:00","ends_local":"2026-11-20T10:00"}});
    let (s, b) = viktor
        .post(&format!("/api/hearings/{}/outcome", h["id"]), body.clone())
        .await;
    ok(s, &b);
    assert_eq!(b["next_hearing"]["status"], "draft");
    let nid = b["next_hearing"]["id"].as_i64().unwrap();
    let (s, b) = olga
        .post(&format!("/api/hearings/{nid}/confirm"), json!({"version":1}))
        .await;
    err(s, &b, StatusCode::CONFLICT, "hearing_conflict");
    let (s, b) = olga
        .post(
            &format!("/api/hearings/{}/cancel", blocked["id"]),
            json!({"reason":"Rescheduled"}),
        )
        .await;
    ok(s, &b);
    let (s, b) = olga
        .post(&format!("/api/hearings/{nid}/confirm"), json!({"version":1}))
        .await;
    ok(s, &b);
    assert_eq!(b["status"], "scheduled");
    grant_perm(&olga, &app, "Viktor", "hearing.schedule").await;
    let blocked = scheduled(&olga, cid, "2026-11-21T09:00", "2026-11-21T10:00", room).await;
    let request = json!({"held":true,"outcome_summary":"Continue again","next_hearing":{"starts_local":"2026-11-21T09:00","ends_local":"2026-11-21T10:00"}});
    let (s, b) = viktor
        .post(&format!("/api/hearings/{nid}/outcome"), request)
        .await;
    err(s, &b, StatusCode::CONFLICT, "hearing_conflict");
    let (_, h) = olga.get(&format!("/api/hearings/{nid}")).await;
    assert_eq!(h["status"], "scheduled");
    let (s,b)=viktor.post(&format!("/api/hearings/{nid}/outcome"),json!({"held":true,"outcome_summary":"Continue again","next_hearing":{"starts_local":"2026-11-22T09:00","ends_local":"2026-11-22T10:00"}})).await;
    ok(s, &b);
    assert_eq!(b["next_hearing"]["status"], "scheduled");
    assert!(b["next_hearing_note"].is_null());
    assert_eq!(blocked["status"], "scheduled");
}

/// A finalised decision dated after today on `cid`, as recorded ahead of time in the demo walkthrough.
fn future_decision(app_db: &tuvalu_court::db::Db, cid: i64, uid: i64, date: &str) -> i64 {
    let (_, vid) = insert_document(app_db, cid, "DEMO order recorded ahead", "decision", "party_material", uid);
    let conn = app_db.open().unwrap();
    conn.execute(
        "INSERT INTO decisions(case_id,title,decision_date,status,document_id,document_version_id,author_user_id,finalised_by,finalised_at,created_at)
         SELECT ?1,'DEMO order recorded ahead',?2,'finalised',document_id,id,?3,?3,uploaded_at,uploaded_at FROM document_versions WHERE id=?4",
        rusqlite::params![cid, date, uid, vid],
    )
    .unwrap();
    conn.last_insert_rowid()
}

#[tokio::test]
async fn demo_closes_on_evidence_recorded_ahead_but_production_never_closes_in_the_future() {
    let evidence_date = "2099-11-19";
    // Demo: the walkthrough records the 19 Nov outcome and decision ahead of time, so the case
    // closes on exactly that date; any other future date or today is refused.
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    let (cid, _) = register_case(&olga, "DEMO closing on early evidence").await;
    let uid = user_id(&olga, "Olga").await;
    let did = future_decision(&olga.db(&app), cid, uid, evidence_date);
    let path = format!("/api/cases/{cid}/close");
    for date in [today(), "2099-11-20".to_string()] {
        let (s, b) = olga.post(&path, json!({"basis":"decided","basis_decision_id":did,"closed_date":date})).await;
        err(s, &b, StatusCode::BAD_REQUEST, "validation");
    }
    let (s, b) = olga.post(&path, json!({"basis":"decided","basis_decision_id":did,"closed_date":evidence_date})).await;
    ok(s, &b);
    assert_eq!(b["closed_date"], evidence_date);

    // Production: no early evidence exception — a future closing date is always refused.
    let app = TestApp::production();
    tuvalu_court::seed::seed_demo(app.state.main_db.as_ref().unwrap()).unwrap();
    let olga = production_client(&app, "olga");
    let (cid, _) = register_case(&olga, "Production closing date").await;
    let uid = user_id(&olga, "Olga").await;
    let did = future_decision(app.state.main_db.as_ref().unwrap(), cid, uid, evidence_date);
    let (s, b) = olga
        .post(&format!("/api/cases/{cid}/close"), json!({"basis":"decided","basis_decision_id":did,"closed_date":evidence_date}))
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    assert_eq!(b["error"]["message"], "Closed date must be on or after registration and no later than today.");
}
