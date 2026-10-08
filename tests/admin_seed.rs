mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{Value, json};

async fn put(c: &Client, path: &str, body: Value) -> (StatusCode, Value) {
    c.clone().raw(Method::PUT, path, Some(body)).await
}

fn find<'a>(items: &'a [Value], key: &str, value: &str) -> &'a Value {
    items
        .iter()
        .find(|i| i[key].as_str() == Some(value))
        .unwrap_or_else(|| panic!("no {key}={value} in {items:?}"))
}

fn uid_by_persona(c: &rusqlite::Connection, persona: &str) -> i64 {
    c.query_row("SELECT id FROM users WHERE persona = ?1", [persona], |r| {
        r.get(0)
    })
    .unwrap()
}

// ------------------------------------------------------------------ users (C01)

#[tokio::test]
async fn admin_user_management_in_demo() {
    let app = TestApp::demo();
    let pavel = app.persona("pavel").await;

    // Permission catalogue marks which powers a technical admin may grant.
    let (s, perms) = pavel.get("/api/admin/permissions").await;
    ok(s, &perms);
    let grantable = |k: &str| {
        find(perms.as_array().unwrap(), "key", k)["admin_grantable"]
            .as_bool()
            .unwrap()
    };
    assert!(grantable("task.manage") && grantable("admin.settings"));
    for p in [
        "case.view_all",
        "case.view_restricted",
        "decision.finalise",
        "case.assign_judge",
        "admin.users",
    ] {
        assert!(!grantable(p), "{p} must not be admin-grantable");
    }

    // Demo mode: new people cannot sign in — a note explains the persona switcher.
    let (s, b) = pavel
        .post(
            "/api/admin/users",
            json!({ "username": "kioa.t", "display_name": "Kioa Temoa", "title": "Clerk",
                    "permissions": ["intake.manage", "task.manage"] }),
        )
        .await;
    ok(s, &b);
    let kioa = b["id"].as_i64().unwrap();
    assert!(b["temporary_password"].is_null());
    assert!(b["note"].as_str().unwrap().contains("persona switcher"));

    let long = "o".repeat(40);
    for u in ["x", "ab", "has space", "UPPER", "nöname", long.as_str()] {
        let (s, b) = pavel
            .post(
                "/api/admin/users",
                json!({ "username": u, "display_name": "X" }),
            )
            .await;
        err(s, &b, StatusCode::BAD_REQUEST, "validation");
    }

    // A technical admin cannot grant case-visibility or judicial powers — not on create…
    for p in [
        "case.view_all",
        "decision.finalise",
        "case.assign_judge",
        "admin.users",
    ] {
        let (s, b) = pavel
            .post(
                "/api/admin/users",
                json!({ "username": "tupou", "display_name": "Tupou", "permissions": [p] }),
            )
            .await;
        err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    }
    // …and not via the permissions endpoint either.
    let (s, b) = put(
        &pavel,
        &format!("/api/admin/users/{kioa}/permissions"),
        json!({ "permissions": ["task.manage", "decision.finalise"] }),
    )
    .await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");

    // A technical admin cannot change their OWN permissions.
    let (_, me) = pavel.get("/api/auth/me").await;
    let pavel_id = me["user"]["id"].as_i64().unwrap();
    let (s, b) = put(
        &pavel,
        &format!("/api/admin/users/{pavel_id}/permissions"),
        json!({ "permissions": [] }),
    )
    .await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");

    // Grantable permissions are replaced; unchanged rows keep their provenance.
    let (s, b) = put(
        &pavel,
        &format!("/api/admin/users/{kioa}/permissions"),
        json!({ "permissions": ["report.view"] }),
    )
    .await;
    ok(s, &b);
    let perms: Vec<&str> = b["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(perms, ["report.view"]);
    let (_, users) = pavel.get("/api/admin/users").await;
    let elena = find(users.as_array().unwrap(), "username", "elena")["id"]
        .as_i64()
        .unwrap();
    let (s, b) = put(
        &pavel,
        &format!("/api/admin/users/{elena}/permissions"),
        json!({ "permissions": ["report.view"] }),
    )
    .await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    assert!(
        b["error"]["message"]
            .as_str()
            .unwrap()
            .contains("tuvalu-court grant")
    );

    let db = pavel.db(&app);
    let conn = db.open().unwrap();
    let provenance = |id| {
        let mut st = conn.prepare("SELECT permission, granted_by, granted_at FROM user_permissions WHERE user_id = ?1 ORDER BY permission").unwrap();
        st.query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    };
    let elena_before = provenance(elena);
    let kioa_before = provenance(kioa);
    let (s, b) = put(
        &pavel,
        &format!("/api/admin/users/{kioa}/permissions"),
        json!({ "permissions": ["report.view", "task.manage"] }),
    )
    .await;
    ok(s, &b);
    assert_eq!(
        provenance(kioa).iter().find(|p| p.0 == "report.view"),
        kioa_before.first()
    );
    assert_eq!(provenance(elena), elena_before);
    let (s, b) = put(
        &pavel,
        &format!("/api/admin/users/{kioa}/permissions"),
        json!({ "permissions": ["task.manage"] }),
    )
    .await;
    ok(s, &b);
    assert_eq!(
        provenance(kioa)
            .iter()
            .map(|p| p.0.as_str())
            .collect::<Vec<_>>(),
        ["task.manage"]
    );

    // Profile update with audit.
    let (s, b) = pavel
        .patch(
            &format!("/api/admin/users/{kioa}"),
            json!({ "display_name": "Kioa Temoa-Bell", "title": "Senior clerk" }),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["display_name"], "Kioa Temoa-Bell");
}

#[tokio::test]
async fn deactivate_ends_sessions_and_assignments() {
    let app = TestApp::demo();
    let pavel = app.persona("pavel").await;
    let sergei = pavel.switch("sergei").await;
    let sergei_id = user_id(&pavel, "Sergei").await;
    let pavel_id = user_id(&pavel, "Pavel").await;

    let (s, _) = sergei.get("/api/queue").await;
    assert_eq!(s, StatusCode::OK);

    // Reason is mandatory; you cannot deactivate yourself.
    let (s, b) = pavel
        .post(
            &format!("/api/admin/users/{sergei_id}/deactivate"),
            json!({}),
        )
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = pavel
        .post(
            &format!("/api/admin/users/{pavel_id}/deactivate"),
            json!({ "reason": "x" }),
        )
        .await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");

    let (s, b) = pavel
        .post(
            &format!("/api/admin/users/{sergei_id}/deactivate"),
            json!({ "reason": "Left the court service" }),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["active"], 0);
    assert!(b["deactivated_at"].is_string());

    // His next request is refused — all sessions were revoked.
    let (s, _) = sergei.get("/api/queue").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // All active case assignments ended with the recorded reason.
    let db = pavel.db(&app);
    let conn = db.open().unwrap();
    let open: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM case_assignments WHERE user_id = ?1 AND end_at IS NULL",
            [sergei_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(open, 0);
    let ended: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM case_assignments WHERE user_id = ?1 AND end_reason LIKE 'Account deactivated%'",
            [sergei_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(ended >= 2, "seeded assignments should have ended: {ended}");

    let unaudited: i64 = conn.query_row(
        "SELECT COUNT(*) FROM case_assignments a WHERE a.user_id = ?1 AND a.end_reason LIKE 'Account deactivated%'
         AND NOT EXISTS (SELECT 1 FROM audit_events e WHERE e.action = 'case.unassigned' AND e.case_id = a.case_id
             AND json_extract(e.details, '$.assignment_id') = a.id
             AND json_extract(e.details, '$.user_id') = a.user_id
             AND json_extract(e.details, '$.role') = a.role
             AND json_extract(e.details, '$.reason') = a.end_reason)", [sergei_id], |r| r.get(0)).unwrap();
    assert_eq!(unaudited, 0);
    let events: i64 = conn.query_row("SELECT COUNT(*) FROM audit_events WHERE action = 'case.unassigned' AND json_extract(details, '$.user_id') = ?1",
        [sergei_id], |r| r.get(0)).unwrap();
    assert_eq!(events, ended);

    // Reactivation restores the account but not the assignments.
    let (s, b) = pavel
        .post(
            &format!("/api/admin/users/{sergei_id}/reactivate"),
            json!({ "reason": "Returned to duty" }),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["active"], 1);
    let open: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM case_assignments WHERE user_id = ?1 AND end_at IS NULL",
            [sergei_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(open, 0);
}

#[tokio::test]
async fn admin_denied_without_permission() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;
    for path in [
        "/api/admin/users",
        "/api/admin/permissions",
        "/api/admin/court-units",
        "/api/admin/registries",
        "/api/admin/rooms",
        "/api/admin/ref-items",
        "/api/admin/templates",
        "/api/admin/settings",
    ] {
        let (s, b) = olga.get(path).await;
        err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    }
    let (s, b) = olga.post("/api/admin/rooms", json!({ "name": "X" })).await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    let (s, b) = olga
        .post(
            "/api/admin/users/1/revoke-sessions",
            json!({ "reason": "x" }),
        )
        .await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    let (s, b) = put(&olga, "/api/admin/settings", json!({ "court_name": "X" })).await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    // A visitor without a demo session cannot call admin endpoints either.
    let (s, b) = app.anon().get("/api/admin/users").await;
    err(s, &b, StatusCode::UNAUTHORIZED, "no_sandbox");
}

// ------------------------------------------------------------------ settings entities (C18)

#[tokio::test]
async fn admin_reference_data() {
    let app = TestApp::demo();
    let pavel = app.persona("pavel").await;

    // Registries: a series cannot change once a case number was issued from it.
    let (s, regs) = pavel.get("/api/admin/registries").await;
    ok(s, &regs);
    let civ = find(regs.as_array().unwrap(), "series", "DEMO-CIV");
    assert!(civ["cases"].as_i64().unwrap() >= 3);
    let civ_id = civ["id"].as_i64().unwrap();
    let (s, b) = pavel
        .patch(
            &format!("/api/admin/registries/{civ_id}"),
            json!({ "series": "DEMO-NEW" }),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "in_use");
    let (s, b) = pavel
        .patch(
            &format!("/api/admin/registries/{civ_id}"),
            json!({ "name": "DEMO civil register (main)" }),
        )
        .await;
    ok(s, &b);

    let unit_id = pavel.get("/api/admin/court-units").await.1[0]["id"]
        .as_i64()
        .unwrap();
    let (s, b) = pavel
        .post(
            "/api/admin/registries",
            json!({ "court_unit_id": unit_id, "series": "bad series", "name": "X" }),
        )
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = pavel
        .post(
            "/api/admin/registries",
            json!({ "court_unit_id": unit_id, "series": "DEMO-SML", "name": "DEMO small claims" }),
        )
        .await;
    ok(s, &b);
    let new_reg = b["id"].as_i64().unwrap();
    let (s, b) = pavel
        .patch(
            &format!("/api/admin/registries/{new_reg}"),
            json!({ "series": "DEMO-SMC" }),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["series"], "DEMO-SMC");

    // Reference items: codes are immutable once issued.
    let (s, items) = pavel.get("/api/admin/ref-items?kind=case_category").await;
    ok(s, &items);
    let items = items.as_array().unwrap();
    assert!(!items.is_empty() && items.iter().all(|i| i["kind"] == "case_category"));
    let item = items[0]["id"].as_i64().unwrap();
    let (s, b) = pavel
        .post(
            "/api/admin/ref-items",
            json!({ "kind": "bogus_kind", "code": "xx", "label": "X" }),
        )
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = pavel.post("/api/admin/ref-items", json!({ "kind": "hearing_type", "code": "status_hearing", "label": "Status hearing", "sort": 90 })).await;
    ok(s, &b);
    let rid = b["id"].as_i64().unwrap();
    let (s, b) = pavel
        .patch(
            &format!("/api/admin/ref-items/{rid}"),
            json!({ "code": "other_code" }),
        )
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = pavel
        .patch(
            &format!("/api/admin/ref-items/{item}"),
            json!({ "code": "ZZZ" }),
        )
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = pavel
        .patch(
            &format!("/api/admin/ref-items/{rid}"),
            json!({ "label": "Status hearing (listing)", "active": false }),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["active"], 0);

    // Templates: placeholders must come from the allowed set.
    let (s, b) = pavel
        .post(
            "/api/admin/templates",
            json!({ "code": "bad_t", "name": "X", "subject": "Hi {bogus}", "body": "x" }),
        )
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = pavel
        .post(
            "/api/admin/templates",
            json!({ "code": "reminder_t", "name": "Hearing reminder",
                    "subject": "{court}: reminder about {case_number}",
                    "body": "Dear {recipient},\n\nThe hearing in {case_number} ({case_title}) is on {hearing_local} in {room}." }),
        )
        .await;
    ok(s, &b);
    let tid = b["id"].as_i64().unwrap();
    let (s, b) = pavel
        .patch(
            &format!("/api/admin/templates/{tid}"),
            json!({ "body": "See {nonsense}." }),
        )
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = pavel
        .patch(
            &format!("/api/admin/templates/{tid}"),
            json!({ "active": false }),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["active"], 0);

    // Rooms are deactivated, never deleted.
    let (s, b) = pavel
        .post(
            "/api/admin/rooms",
            json!({ "name": "DEMO Side room", "location": "Annex", "court_unit_id": unit_id }),
        )
        .await;
    ok(s, &b);
    let room = b["id"].as_i64().unwrap();
    let (s, b) = pavel
        .patch(
            &format!("/api/admin/rooms/{room}"),
            json!({ "active": false }),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["active"], 0);
    let (s, b) = app.anon().get("/api/admin/rooms").await;
    err(s, &b, StatusCode::UNAUTHORIZED, "no_sandbox");

    // Court units.
    let (s, b) = pavel
        .post(
            "/api/admin/court-units",
            json!({ "code": "DEMO-OF", "name": "DEMO outer-island unit" }),
        )
        .await;
    ok(s, &b);
    let (s, b) = pavel
        .patch(
            &format!("/api/admin/court-units/{}", b["id"].as_i64().unwrap()),
            json!({ "active": false }),
        )
        .await;
    ok(s, &b);
}

#[tokio::test]
async fn admin_settings_validation() {
    let app = TestApp::demo();
    let pavel = app.persona("pavel").await;

    let (s, b) = pavel.get("/api/admin/settings").await;
    ok(s, &b);
    assert_eq!(b["mode"], "demo");
    assert_eq!(b["timezone"], "Pacific/Funafuti (UTC+12, fixed)");

    let (s, b) = put(
        &pavel,
        "/api/admin/settings",
        json!({ "hearing_buffer_minutes": 300 }),
    )
    .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = put(
        &pavel,
        "/api/admin/settings",
        json!({ "hearing_buffer_minutes": -5 }),
    )
    .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = put(
        &pavel,
        "/api/admin/settings",
        json!({ "intake_reference_prefix": "abc" }),
    )
    .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = put(
        &pavel,
        "/api/admin/settings",
        json!({ "intake_reference_prefix": "TOOLONGX" }),
    )
    .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");

    let (s, b) = put(
        &pavel,
        "/api/admin/settings",
        json!({ "court_name": "DEMO Magistrates Court Registry (test)", "hearing_buffer_minutes": 15, "intake_reference_prefix": "IN" }),
    )
    .await;
    ok(s, &b);
    assert_eq!(b["hearing_buffer_minutes"], 15);

    // The change is audited.
    let db = pavel.db(&app);
    let conn = db.open().unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_events WHERE action = 'settings.updated'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(n >= 1);
}

// ------------------------------------------------------------------ production mode

async fn login_and_enrol(c: &mut Client, username: &str, password: &str) -> Value {
    let (s, b) = c
        .raw(
            Method::POST,
            "/api/auth/login",
            Some(json!({ "username": username, "password": password })),
        )
        .await;
    ok(s, &b);
    let (s, setup) = c.post("/api/auth/totp/setup", json!({})).await;
    ok(s, &setup);
    let secret = setup["secret"].as_str().unwrap();
    let code = tuvalu_court::auth::current_totp(secret).unwrap();
    let (s, _) = c
        .raw(
            Method::POST,
            "/api/auth/totp/enable",
            Some(json!({ "code": code })),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    b
}

#[tokio::test]
async fn production_admin_account_lifecycle() {
    let app = TestApp::production();
    let db = app.state.main_db.clone().unwrap();
    tuvalu_court::seed::create_user(
        &db,
        "root",
        "Root Admin",
        "root-password-99",
        false,
        &["admin.users".to_string(), "admin.settings".to_string()],
    )
    .unwrap();

    let mut admin = app.anon();
    let (s, b) = admin
        .raw(
            Method::POST,
            "/api/auth/login",
            Some(json!({ "username": "root", "password": "root-password-99" })),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["enroll_required"], true); // no second factor yet
    login_and_enrol(&mut admin, "root", "root-password-99").await;

    // Judges, administrators and holders of court-authority powers require CLI management.
    for (name, judge, perms) in [
        ("judge", true, vec!["document.manage"]),
        ("authority", false, vec!["case.view_all"]),
        ("administrator", false, vec!["admin.users"]),
    ] {
        let id = tuvalu_court::seed::create_user(
            &db,
            name,
            name,
            "protected-password-99",
            judge,
            &perms.into_iter().map(str::to_string).collect::<Vec<_>>(),
        )
        .unwrap();
        let conn = db.open().unwrap();
        conn.execute(
            "UPDATE users SET totp_secret = 'UNCHANGED' WHERE id = ?1",
            [id],
        )
        .unwrap();
        let before: (String, String, i64) = conn
            .query_row(
                "SELECT password_hash, totp_secret, active FROM users WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        for action in ["reset-password", "reset-mfa", "deactivate"] {
            let (s, b) = admin
                .post(
                    &format!("/api/admin/users/{id}/{action}"),
                    json!({ "reason": "Recovery" }),
                )
                .await;
            err(s, &b, StatusCode::FORBIDDEN, "forbidden");
            assert!(
                b["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("tuvalu-court grant")
            );
        }
        let (s, b) = put(
            &admin,
            &format!("/api/admin/users/{id}/permissions"),
            json!({ "permissions": [] }),
        )
        .await;
        err(s, &b, StatusCode::FORBIDDEN, "forbidden");
        let after: (String, String, i64) = conn
            .query_row(
                "SELECT password_hash, totp_secret, active FROM users WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(before, after);
    }

    // Creating a user returns a one-time temporary password.
    let (s, b) = admin
        .post("/api/admin/users", json!({ "username": "kioa", "display_name": "Kioa Temoa", "permissions": ["task.manage"] }))
        .await;
    ok(s, &b);
    let kioa_id = b["id"].as_i64().unwrap();
    let temp1 = b["temporary_password"].as_str().unwrap().to_string();
    assert_eq!(temp1.len(), 16);

    // The temporary password signs in and asks for enrolment.
    let mut kioa = app.anon();
    let (s, b) = kioa
        .raw(
            Method::POST,
            "/api/auth/login",
            Some(json!({ "username": "kioa", "password": temp1 })),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["enroll_required"], true);
    login_and_enrol(&mut kioa, "kioa", &temp1).await;
    let (s, _) = kioa.get("/api/auth/me").await;
    assert_eq!(s, StatusCode::OK);

    // reset-password: new temporary password once, sessions revoked, TOTP kept.
    let (s, b) = admin
        .post(
            &format!("/api/admin/users/{kioa_id}/reset-password"),
            json!({ "reason": "Password forgotten" }),
        )
        .await;
    ok(s, &b);
    let temp2 = b["temporary_password"].as_str().unwrap().to_string();
    assert_ne!(temp1, temp2);
    let (s, _) = kioa.get("/api/auth/me").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, b) = kioa
        .raw(
            Method::POST,
            "/api/auth/login",
            Some(json!({ "username": "kioa", "password": temp2 })),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["mfa_required"], true); // second factor survives a password reset

    // reset-mfa: clears enrolment and ends the pending session.
    let (s, b) = admin
        .post(
            &format!("/api/admin/users/{kioa_id}/reset-mfa"),
            json!({ "reason": "Phone replaced" }),
        )
        .await;
    ok(s, &b);
    let (s, b) = kioa
        .raw(
            Method::POST,
            "/api/auth/login",
            Some(json!({ "username": "kioa", "password": temp2 })),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["enroll_required"], true);

    // revoke-sessions ends everything immediately.
    login_and_enrol(&mut kioa, "kioa", &temp2).await;
    let (s, _) = admin
        .post(
            &format!("/api/admin/users/{kioa_id}/revoke-sessions"),
            json!({ "reason": "Device lost" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = kioa.get("/api/auth/me").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // In demo mode, password reset is refused — personas have no passwords.
    let demo = TestApp::demo();
    let pavel = demo.persona("pavel").await;
    let (_, users) = pavel.get("/api/admin/users").await;
    let anyone = users.as_array().unwrap()[0]["id"].as_i64().unwrap();
    let (s, b) = pavel
        .post(
            &format!("/api/admin/users/{anyone}/reset-password"),
            json!({ "reason": "x" }),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "not_available_in_demo");
}

// ------------------------------------------------------------------ the seeded demo dataset (spec §12)

#[tokio::test]
async fn demo_seed_dataset_is_complete_and_consistent() {
    let app = TestApp::demo();
    let olga = app.persona("olga").await;

    let (s, list) = olga.get("/api/cases").await;
    ok(s, &list);
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 6, "seeded cases: {items:?}");
    assert!(
        items
            .iter()
            .all(|c| c["number"].as_str().unwrap().starts_with("DEMO-"))
    );
    let by_title = |t: &str| find(items, "title", t);
    for (title, status) in [
        ("DEMO — Unpaid fishing boat repair", "active"),
        ("DEMO — Boundary fence contribution", "active"),
        ("DEMO — Care arrangements for two children", "closed"),
        ("DEMO — Refund for faulty solar panels", "reopened"),
        ("DEMO — Guardianship assessment", "active"),
        ("DEMO — Theft of fishing gear", "registered"),
    ] {
        assert_eq!(by_title(title)["status"], status, "{title}");
    }

    // The CRM case has no judge → "Assign a judge" is its next step.
    let crm_id = by_title("DEMO — Theft of fishing gear")["id"]
        .as_i64()
        .unwrap();
    let (_, card) = olga.get(&format!("/api/cases/{crm_id}")).await;
    assert!(
        card["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["code"] == "assign_judge")
    );

    // The incomplete intake is waiting with its missing-items text.
    let (_, intakes) = olga.get("/api/intakes?status=needs_information").await;
    let tavita = find(
        intakes["items"].as_array().unwrap(),
        "sender_name",
        "Tavita Lomasi (DEMO)",
    );
    assert_eq!(tavita["channel"], "post");
    assert_eq!(tavita["origin_island"], "nukufetau");

    let db = olga.db(&app);
    let conn = db.open().unwrap();

    // Adjourned pair on the boundary case: linked rows, reason and authoriser kept.
    let case3 = by_title("DEMO — Boundary fence contribution")["id"]
        .as_i64()
        .unwrap();
    let mut st = conn
        .prepare("SELECT status, previous_hearing_id, adjourned_to_id, status_reason, status_authorised_by FROM hearings WHERE case_id = ?1 ORDER BY id")
        .unwrap();
    type HearingRow = (
        String,
        Option<i64>,
        Option<i64>,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<HearingRow> = st
        .query_map([case3], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "adjourned");
    assert_eq!(
        rows[0].3.as_deref(),
        Some("Respondent requested time to obtain counsel")
    );
    assert_eq!(rows[0].4.as_deref(), Some("Judge Viktor Hale"));
    assert_eq!(rows[1].0, "scheduled");
    assert_eq!(rows[0].2, Some(hearing_id(&conn, case3, "scheduled"))); // adjourned_to → new
    assert_eq!(rows[1].1, Some(hearing_id(&conn, case3, "adjourned"))); // previous → old
    let local_starts: Vec<String> = conn
        .prepare("SELECT starts_at FROM hearings WHERE case_id = ?1 ORDER BY id")
        .unwrap()
        .query_map([case3], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| tuvalu_court::time::utc_to_local(&r.unwrap()))
        .collect();
    assert_eq!(local_starts, ["2026-11-10T14:00", "2026-11-12T14:00"]);
    let renotify: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks WHERE case_id = ?1 AND kind = 'renotify' AND status = 'open'", [case3], |r| r.get(0))
        .unwrap();
    assert_eq!(renotify, 2);

    // Closed family case: held hearing, finalised decision, confirmed copy deliveries.
    let case4 = by_title("DEMO — Care arrangements for two children")["id"]
        .as_i64()
        .unwrap();
    let held: i64 = conn
        .query_row("SELECT COUNT(*) FROM hearings WHERE case_id = ?1 AND status = 'held' AND outcome_recorded_by IS NOT NULL", [case4], |r| r.get(0))
        .unwrap();
    assert_eq!(held, 1);
    let finalised: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM decisions WHERE case_id = ?1 AND status = 'finalised'",
            [case4],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(finalised, 1);
    let copies: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM delivery_confirmations dc JOIN dispatches d ON d.id = dc.dispatch_id
             WHERE d.case_id = ?1 AND d.kind = 'copies' AND dc.kind = 'human_handover'",
            [case4],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(copies, 2);

    // Every sent dispatch has a local mailbox copy; nothing leaves the server.
    let mail: i64 = conn
        .query_row("SELECT COUNT(*) FROM mailbox", [], |r| r.get(0))
        .unwrap();
    assert!(
        mail >= 5,
        "expected notices, an info request and copy packages in the mailbox"
    );

    // Restricted case: Sergei is initially unassigned; Elena is assigned to manage grants.
    let case6 = by_title("DEMO — Guardianship assessment")["id"]
        .as_i64()
        .unwrap();
    let sergei = olga.switch("sergei").await;
    let (s, _) = sergei.get(&format!("/api/cases/{case6}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, slist) = sergei.get("/api/cases").await;
    assert!(
        slist["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["id"] != case6)
    );
    let elena = olga.switch("elena").await;
    let (s, _) = elena.get(&format!("/api/cases/{case6}")).await;
    assert_eq!(s, StatusCode::OK);
    let elena_assignment: String = conn.query_row(
        "SELECT reason FROM case_assignments WHERE case_id = ?1 AND user_id = ?2 AND role = 'registry_head' AND end_at IS NULL",
        rusqlite::params![case6, uid_by_persona(&conn, "elena")], |r| r.get(0)).unwrap();
    assert!(!elena_assignment.is_empty());
    let (s, b) = elena.post(&format!("/api/cases/{case6}/assignments"),
        json!({ "user_id": uid_by_persona(&conn, "sergei"), "role": "service_officer", "reason": "Visibility check" })).await;
    ok(s, &b);
    for (persona, expected) in [
        ("olga", vec!["restricted", "party_material"]),
        (
            "viktor",
            vec!["restricted", "party_material", "judicial_note"],
        ),
        ("sergei", vec!["party_material"]),
        ("elena", vec!["party_material"]),
    ] {
        let actor = tuvalu_court::auth::load_actor(&conn, uid_by_persona(&conn, persona), None)
            .unwrap()
            .unwrap();
        let docs: Vec<(i64, String)> = conn
            .prepare("SELECT id, visibility FROM documents WHERE case_id = ?1")
            .unwrap()
            .query_map([case6], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(docs.len(), 3);
        let sql = tuvalu_court::policy::document_visible_sql(&actor, "d");
        for (id, visibility) in docs {
            let visible: bool = conn
                .query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM documents d WHERE d.id = ?1 AND {sql})"),
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                visible,
                expected.contains(&visibility.as_str()),
                "{persona}: {visibility}"
            );
            assert_eq!(
                tuvalu_court::policy::require_document(&conn, &actor, id).is_ok(),
                visible
            );
        }
    }
    let missing_status_audits: i64 = conn.query_row(
        "SELECT COUNT(*) FROM case_status_history h WHERE h.to_status NOT IN ('closed', 'reopened') AND h.from_status IS NOT NULL
         AND NOT EXISTS (SELECT 1 FROM audit_events e WHERE e.case_id = h.case_id AND e.action = 'case.status_changed'
             AND json_extract(e.details, '$.from') = h.from_status AND json_extract(e.details, '$.to') = h.to_status)", [], |r| r.get(0)).unwrap();
    assert_eq!(missing_status_audits, 0);

    // The audit chain is intact and every stored file's checksum matches its bytes.
    let (n_events, broken) = tuvalu_court::audit::verify_chain(&conn).unwrap();
    assert_eq!(broken, None);
    assert!(
        n_events > 50,
        "the seeded history should be rich: {n_events}"
    );
    let mut st = conn
        .prepare("SELECT storage_key, sha256 FROM document_versions")
        .unwrap();
    let rows: Vec<(String, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(rows.len() >= 4);
    for (key, sha) in rows {
        tuvalu_court::storage::read(&db, &key, &sha).expect("stored file matches its checksum");
    }

    // Olga's work queue is not empty (the waiting intake at minimum).
    let (s, queue) = olga.get("/api/queue").await;
    ok(s, &queue);
    assert!(!queue["items"].as_array().unwrap().is_empty());
}

fn hearing_id(c: &rusqlite::Connection, case_id: i64, status: &str) -> i64 {
    c.query_row(
        "SELECT id FROM hearings WHERE case_id = ?1 AND status = ?2",
        rusqlite::params![case_id, status],
        |r| r.get(0),
    )
    .unwrap()
}
