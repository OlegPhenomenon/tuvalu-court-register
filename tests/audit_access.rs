mod common;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use common::*;
use rusqlite::params;
use serde_json::{Value, json};
use std::io::{Cursor, Read};
static HEAVY: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

async fn scopes(app: &TestApp) -> (Client, i64, i64, i64) {
    let c = app.persona("olga").await;
    let (visible, _) = register_case(&c, "DEMO visible scope").await;
    let (hidden, _) = register_case(&c, "DEMO hidden scope").await;
    let db = c.db(app);
    let conn = db.open().unwrap();
    conn.execute("UPDATE cases SET restricted=1 WHERE id=?1", [hidden])
        .unwrap();
    conn.execute(
        "UPDATE case_assignments SET end_at=?1 WHERE case_id=?2",
        params![tuvalu_court::time::now_utc(), hidden],
    )
    .unwrap();
    let pid = conn
        .query_row(
            "SELECT party_id FROM case_participations WHERE case_id=?1 LIMIT 1",
            [hidden],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute(
        "UPDATE parties SET name='DEMO Hidden Person' WHERE id=?1",
        [pid],
    )
    .unwrap();
    (c, visible, hidden, pid)
}

#[tokio::test]
async fn f01_t27_t05_party_directory_and_links_follow_record_scope() {
    let app = TestApp::demo();
    let (c, visible, hidden, pid) = scopes(&app).await;
    assert_eq!(
        c.get(&format!("/api/cases/{hidden}")).await.0,
        StatusCode::NOT_FOUND
    );
    let (_, list) = c.get("/api/parties?q=Hidden").await;
    assert!(!list.to_string().contains("DEMO Hidden Person"), "{list}");
    err(
        c.get(&format!("/api/parties/{pid}")).await.0,
        &c.get(&format!("/api/parties/{pid}")).await.1,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    let (s, b) = c
        .patch(
            &format!("/api/parties/{pid}"),
            json!({"version":1,"name":"DEMO Changed"}),
        )
        .await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    let (s, b) = c
        .post(
            "/api/parties",
            json!({"kind":"person","name":"DEMO Hidden Person"}),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["same_name_records"], json!([]));
    assert_ne!(b["id"], pid);
    for body in [
        json!({"party_id":pid,"role":"claimant"}),
        json!({"party_id":b["id"],"role":"claimant","representative_party_id":pid,"representation_basis":"DEMO mandate"}),
    ] {
        let (s, b) = c
            .post(&format!("/api/cases/{visible}/participants"), body)
            .await;
        err(s, &b, StatusCode::NOT_FOUND, "not_found");
    }
    let (_, search) = c.get("/api/search?q=Hidden").await;
    assert!(!search.to_string().contains("DEMO Hidden Person"));
    let pavel = c.switch("pavel").await;
    assert_eq!(
        pavel.get(&format!("/api/parties/{pid}")).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn f01_t27_shared_party_edit_is_neutral_conflict() {
    let app = TestApp::demo();
    let (c, visible, hidden, _) = scopes(&app).await;
    let conn = c.db(&app).open().unwrap();
    let pid: i64 = conn
        .query_row(
            "SELECT party_id FROM case_participations WHERE case_id=?1 LIMIT 1",
            [visible],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute("INSERT INTO case_participations(case_id,party_id,role,added_at) VALUES(?1,?2,'claimant',?3)",params![hidden,pid,tuvalu_court::time::now_utc()]).unwrap();
    let (s, b) = c
        .patch(
            &format!("/api/parties/{pid}"),
            json!({"version":1,"name":"DEMO correction"}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "party_shared");
    assert!(!b.to_string().contains("hidden scope"));
}

async fn restricted_decision(app: &TestApp, c: &Client, cid: i64) -> (i64, i64, String) {
    let elena = c.switch("elena").await;
    let uid = user_id(&elena, "Viktor").await;
    let (s, b) = elena
        .post(
            &format!("/api/cases/{cid}/assignments"),
            json!({"user_id":uid,"role":"judge","reason":"DEMO review"}),
        )
        .await;
    ok(s, &b);
    let db = c.db(app);
    let (_, vid) = insert_document(
        &db,
        cid,
        "DEMO Unselected Secret",
        "decision",
        "restricted",
        uid,
    );
    let judge = c.switch("viktor").await;
    let (s, d) = judge
        .post(
            &format!("/api/cases/{cid}/decisions"),
            json!({"title":"DEMO Unselected Secret","document_version_id":vid}),
        )
        .await;
    ok(s, &d);
    let sha = db
        .open()
        .unwrap()
        .query_row(
            "SELECT sha256 FROM document_versions WHERE id=?1",
            [vid],
            |r| r.get(0),
        )
        .unwrap();
    (d["id"].as_i64().unwrap(), vid, sha)
}

#[tokio::test]
async fn f02_t16_t27_restricted_decision_metadata_and_audit_are_redacted() {
    let app = TestApp::demo();
    let (c, cid, _, _) = scopes(&app).await;
    let (did, vid, sha) = restricted_decision(&app, &c, cid).await;
    let elena = c.switch("elena").await;
    for path in [
        format!("/api/decisions/{did}"),
        format!("/api/cases/{cid}/decisions"),
        "/api/decisions".into(),
        format!("/api/cases/{cid}/history"),
        format!("/api/cases/{cid}"),
        "/api/queue".into(),
        "/api/search?q=Secret".into(),
    ] {
        let (s, b) = c.get(&path).await;
        ok(s, &b);
        let text = b.to_string();
        assert!(!text.contains("DEMO Unselected Secret"), "{path}: {text}");
        assert!(!text.contains(&sha));
        assert!(!text.contains("DEMO_Unselected_Secret.pdf"));
    }
    let (s, b) = elena.get("/api/audit").await;
    ok(s, &b);
    assert!(!b.to_string().contains("DEMO Unselected Secret"));
    let (_, d) = c.get(&format!("/api/decisions/{did}")).await;
    assert_eq!(d["restricted"], true);
    assert_eq!(d["document_title"], "Restricted document");
    assert_eq!(d["document_version_id"], vid);
    assert!(d.get("filename").is_none());
    let (s, b) = c
        .post(
            &format!("/api/cases/{cid}/close"),
            json!({"basis":"settled","note":"DEMO agreement","closed_date":today()}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "open_items");
    assert!(!b.to_string().contains("DEMO Unselected Secret"));
    let judge = c.switch("viktor").await;
    let (_, full) = judge.get(&format!("/api/decisions/{did}")).await;
    assert_eq!(full["document_title"], "DEMO Unselected Secret");
    assert_eq!(full["sha256"], sha);
}

async fn package(c: &Client, cid: i64, ids: Value) -> Vec<u8> {
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/cases/{cid}/export"))
        .header("host", "localhost")
        .header("x-tcr", "1")
        .header(
            "cookie",
            format!(
                "tcr_sandbox={}; tcr_session={}",
                c.sandbox.as_ref().unwrap(),
                c.session.as_ref().unwrap()
            ),
        )
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"purpose":"DEMO selected package","version_ids":ids}).to_string(),
        ))
        .unwrap();
    let (s, b, _, bytes) = c.clone().send(req).await;
    ok(s, &b);
    bytes
}

#[tokio::test]
async fn f03_t21_t32_every_zip_entry_excludes_unselected_material() {
    let _lock = HEAVY.lock().await;
    let app = TestApp::demo();
    let (c, cid, _, _) = scopes(&app).await;
    let (did, vid, sha) = restricted_decision(&app, &c, cid).await;
    let uid = user_id(&c, "Olga").await;
    let (_, selected) = insert_document(
        &c.db(&app),
        cid,
        "DEMO Selected",
        "submission",
        "administrative",
        uid,
    );
    let elena = c.switch("elena").await;
    let db = c.db(&app);
    let conn = db.open().unwrap();
    conn.execute(
        "UPDATE decisions SET status='finalised',finalised_at=?1,finalised_by=?2 WHERE id=?3",
        params![tuvalu_court::time::now_utc(), uid, did],
    )
    .unwrap();
    // Even an exporter who may open the excluded material must not transmit it.
    conn.execute("INSERT INTO document_grants(document_id,user_id,granted_by,granted_at,reason) SELECT document_id,?1,?1,?2,'DEMO' FROM document_versions WHERE id=?3",params![user_id(&elena,"Elena").await,tuvalu_court::time::now_utc(),vid]).unwrap();
    for ids in [json!([selected]), json!([])] {
        let mut zip = zip::ZipArchive::new(Cursor::new(package(&elena, cid, ids).await)).unwrap();
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).unwrap();
            let mut bytes = vec![];
            entry.read_to_end(&mut bytes).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            for secret in [
                "DEMO Unselected Secret",
                "DEMO_Unselected_Secret.pdf",
                sha.as_str(),
            ] {
                assert!(
                    !text.contains(secret),
                    "{} leaked {secret}: {text}",
                    entry.name()
                );
            }
        }
        let mut m = String::new();
        zip.by_name("manifest.json")
            .unwrap()
            .read_to_string(&mut m)
            .unwrap();
        let m: Value = serde_json::from_str(&m).unwrap();
        assert_eq!(m["decisions"], json!([]));
    }
}

#[tokio::test]
async fn f05_t06_t27_case_patch_cannot_bypass_assignment_permission() {
    let app = TestApp::demo();
    let (c, cid, hidden, _) = scopes(&app).await;
    let uid = user_id(&c, "Sergei").await;
    let(s,b)=c.patch(&format!("/api/cases/{cid}"),json!({"version":1,"responsible_user_id":uid,"assignment_reason":"DEMO manual assignment"})).await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    let(s,b)=c.patch(&format!("/api/cases/{hidden}"),json!({"version":1,"responsible_user_id":uid,"assignment_reason":"DEMO manual assignment"})).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    let elena = c.switch("elena").await;
    let(s,b)=elena.patch(&format!("/api/cases/{cid}"),json!({"version":1,"responsible_user_id":uid,"assignment_reason":"DEMO manual assignment"})).await;
    ok(s, &b);
    let pavel = user_id(&elena, "Pavel").await;
    let(s,b)=elena.patch(&format!("/api/cases/{cid}"),json!({"version":b["case"]["version"],"responsible_user_id":pavel,"assignment_reason":"DEMO invalid assignment"})).await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
}

#[tokio::test]
async fn f06_t07_t30_import_rejects_ineligible_assignees_per_row() {
    let _lock = HEAVY.lock().await;
    let app = TestApp::demo();
    let (c, _, _, _) = scopes(&app).await;
    let elena = c.switch("elena").await;
    let conn = c.db(&app).open().unwrap();
    let name: String = conn
        .query_row(
            "SELECT username FROM users WHERE display_name LIKE 'Pavel%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let csv = format!(
        "number,category,title,registered_date,status,responsible_username,closed_date,closure_basis,parties\nDEMO-CIV-2001-0901,civil_contract,DEMO bad admin,2001-01-01,registered,{name},,,\n"
    );
    let (s, b) = elena
        .upload("/api/import/cases/preview", &[], "demo.csv", csv.as_bytes())
        .await;
    ok(s, &b);
    assert_eq!(b["rows"][0]["action"], "error", "{b}");
    let (s, result) = elena
        .post(&format!("/api/import/{}/commit", b["batch_id"]), json!({}))
        .await;
    ok(s, &result);
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM cases WHERE number='DEMO-CIV-2001-0901'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn f12_t02_t32_history_contains_original_intake_events_once() {
    let _lock = HEAVY.lock().await;
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let iid = new_intake(&c, "DEMO intake sender").await;
    let (s, b) = c
        .upload(
            &format!("/api/intakes/{iid}/documents"),
            &[
                ("title", "DEMO Intake attachment"),
                ("doc_type", "claim"),
                ("visibility", "administrative"),
                ("source", "party"),
            ],
            "demo.pdf",
            &pdf("DEMO intake"),
        )
        .await;
    ok(s, &b);
    let (s,b)=c.post(&format!("/api/intakes/{iid}/request-info"),json!({"missing_items":"DEMO missing attachment","method":"post","address":"DEMO registry address"})).await;
    ok(s, &b);
    let (s,b)=c.post(&format!("/api/intakes/{iid}/supplement"),json!({"sender_name":"DEMO supplement sender","channel":"counter","received_date":today(),"description":"DEMO missing attachment supplied","is_paper_original":false})).await;
    ok(s, &b);
    let supplement = b["id"].as_i64().unwrap();
    let (s, b) = c
        .upload(
            &format!("/api/intakes/{supplement}/documents"),
            &[
                ("title", "DEMO Supplement attachment"),
                ("doc_type", "supplement"),
                ("source", "party"),
                ("visibility", "party_material"),
            ],
            "supplement.pdf",
            &pdf("DEMO supplement"),
        )
        .await;
    ok(s, &b);
    let (s, b) = c
        .post(&format!("/api/intakes/{iid}/mark-ready"), json!({}))
        .await;
    ok(s, &b);
    let (_, refs) = c.get("/api/ref").await;
    let registry = refs["registries"][0]["id"].clone();
    let(s,b)=c.post(&format!("/api/intakes/{iid}/register"),json!({"registry_id":registry,"category":"civil_contract","title":"DEMO Origin history"})).await;
    ok(s, &b);
    let cid = b["case_id"].as_i64().unwrap();
    let (_, h) = c.get(&format!("/api/cases/{cid}/history")).await;
    for action in [
        "intake.received",
        "intake.information_requested",
        "intake.supplemented",
        "intake.ready",
        "document.uploaded",
        "case.registered",
    ] {
        assert!(
            h["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["action"] == action),
            "{h}"
        );
    }
    let ids: Vec<_> = h["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_i64().unwrap())
        .collect();
    assert_eq!(
        ids.len(),
        ids.iter().collect::<std::collections::BTreeSet<_>>().len()
    );
    let mut zip = zip::ZipArchive::new(Cursor::new(package(&c, cid, json!([])).await)).unwrap();
    let manifest: Value = serde_json::from_reader(zip.by_name("manifest.json").unwrap()).unwrap();
    assert!(
        manifest["chronology"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["action"] == "intake.received")
    );
    assert!(
        !manifest["chronology"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["action"] == "document.uploaded")
    );
    let (s, b) = c.get(&format!("/api/intakes/{supplement}")).await;
    ok(s, &b);
    assert_eq!(b["intake"]["case_id"], cid);
}

#[tokio::test]
async fn f13_t04_t05_party_and_participation_commands_replay_once() {
    let app = TestApp::demo();
    let (c, cid, _, _) = scopes(&app).await;
    let req = json!({"kind":"person","name":"DEMO retry person"});
    let (s, p) = c.post_idem("/api/parties", "party-key", req.clone()).await;
    ok(s, &p);
    let (s, replay) = c.post_idem("/api/parties", "party-key", req).await;
    ok(s, &replay);
    assert_eq!(p, replay);
    let (s, b) = c
        .post_idem(
            "/api/parties",
            "party-key",
            json!({"kind":"person","name":"DEMO different"}),
        )
        .await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    let path = format!("/api/cases/{cid}/participants");
    let req = json!({"party_id":p["id"],"role":"claimant"});
    let (s, b) = c.post_idem(&path, "add-key", req.clone()).await;
    ok(s, &b);
    assert_eq!(c.post_idem(&path, "add-key", req).await.1, b);
    let pid = b["participants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["party_id"] == p["id"])
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let path = format!("/api/cases/{cid}/participants/{pid}/end");
    let req = json!({"reason":"DEMO ended"});
    let (s, b) = c.post_idem(&path, "end-key", req.clone()).await;
    ok(s, &b);
    let (s, replay) = c.post_idem(&path, "end-key", req).await;
    ok(s, &replay);
    assert_eq!(b, replay);
}

#[tokio::test]
async fn f14_t05_t29_contact_and_representation_edits_use_versions() {
    let app = TestApp::demo();
    let (c, cid, _, _) = scopes(&app).await;
    let (_, data) = c.get(&format!("/api/cases/{cid}")).await;
    let p = &data["participants"][0];
    let pid = p["id"].as_i64().unwrap();
    let dispatch_req = json!({"kind":"notice","method":"post","recipient_party_id":p["party_id"],"subject":"DEMO contact test","body":"DEMO notice","purpose":"DEMO contact test","version_ids":[]});
    let (s, old_dispatch) = c
        .post(
            &format!("/api/cases/{cid}/dispatches"),
            dispatch_req.clone(),
        )
        .await;
    ok(s, &old_dispatch);
    let path = format!("/api/cases/{cid}/participants/{pid}");
    let req = json!({"version":1,"role":"respondent","service_contact":"DEMO new contact","representative_party_id":null,"representation_basis":null});
    let (s, b) = c.patch(&path, req.clone()).await;
    ok(s, &b);
    let (s, b) = c.patch(&path, req).await;
    err(s, &b, StatusCode::CONFLICT, "version_conflict");
    let path = format!("/api/parties/{}", p["party_id"]);
    let(s,b)=c.patch(&path,json!({"version":1,"name":"DEMO corrected spelling","contact_email":"demo@example.invalid","contact_phone":"DEMO phone","address":"DEMO postal address"})).await;
    ok(s, &b);
    let (s, new_dispatch) = c
        .post(&format!("/api/cases/{cid}/dispatches"), dispatch_req)
        .await;
    ok(s, &new_dispatch);
    assert_eq!(new_dispatch["address"], "DEMO new contact");
    let (s, snapshot) = c
        .get(&format!("/api/dispatches/{}", old_dispatch["id"]))
        .await;
    ok(s, &snapshot);
    assert_eq!(snapshot["address"], old_dispatch["address"]);
    assert_ne!(snapshot["address"], new_dispatch["address"]);
    let elena = c.switch("elena").await;
    let (_, audit) = elena.get("/api/audit?action=party.updated").await;
    assert!(audit.to_string().contains("contact_email"));
    assert!(!audit.to_string().contains("demo@example.invalid"));
}

#[tokio::test]
async fn f02_t16_t27_audit_does_not_reveal_replaced_restricted_versions() {
    let app = TestApp::demo();
    let (c, cid, _, _) = scopes(&app).await;
    let (did, _, sha) = restricted_decision(&app, &c, cid).await;
    let uid = user_id(&c, "Viktor").await;
    let (_, public) = insert_document(
        &c.db(&app),
        cid,
        "DEMO Public replacement",
        "decision",
        "administrative",
        uid,
    );
    let judge = c.switch("viktor").await;
    let (s, b) = judge
        .patch(
            &format!("/api/decisions/{did}"),
            json!({"version":1,"title":"DEMO Public replacement","document_version_id":public}),
        )
        .await;
    ok(s, &b);
    let elena = c.switch("elena").await;
    for path in ["/api/audit".into(), format!("/api/cases/{cid}/history")] {
        let (_, b) = elena.get(&path).await;
        assert!(!b.to_string().contains("DEMO Unselected Secret"), "{b}");
        assert!(!b.to_string().contains(&sha));
    }
}

#[tokio::test]
async fn f01_t27_intake_links_cannot_reveal_hidden_contacts() {
    let app = TestApp::demo();
    let (c, _, _, pid) = scopes(&app).await;
    let req = json!({"sender_name":"DEMO intake sender","sender_party_id":pid,"channel":"counter","received_date":today(),"description":"DEMO request","is_paper_original":false});
    let (s, b) = c.post("/api/intakes", req.clone()).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    let iid = new_intake(&c, "DEMO permitted sender").await;
    let mut req = req;
    req["version"] = json!(1);
    let (s, b) = c.patch(&format!("/api/intakes/{iid}"), req).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    let (s, p) = c
        .post(
            "/api/parties",
            json!({"kind":"person","name":"DEMO intake contact"}),
        )
        .await;
    ok(s, &p);
    let(s,b)=c.post("/api/intakes",json!({"sender_name":"DEMO intake contact","sender_party_id":p["id"],"channel":"counter","received_date":today(),"description":"DEMO request","is_paper_original":false})).await;
    ok(s, &b);
    let (s, b) = c
        .patch(
            &format!("/api/parties/{}", p["id"]),
            json!({"version":1,"name":"DEMO corrected intake contact"}),
        )
        .await;
    ok(s, &b);
}

#[tokio::test]
async fn f13_t04_t27_replays_reauthorize_and_different_participant_bodies_conflict() {
    let app = TestApp::demo();
    let (c, cid, _, _) = scopes(&app).await;
    let req = json!({"new_party":{"kind":"person","name":"DEMO retry linked"},"role":"claimant"});
    let path = format!("/api/cases/{cid}/participants");
    let (s, b) = c.post_idem(&path, "linked-key", req.clone()).await;
    ok(s, &b);
    let mut changed = req.clone();
    changed["role"] = json!("respondent");
    let (s, b) = c.post_idem(&path, "linked-key", changed).await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    let conn = c.db(&app).open().unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM parties WHERE name='DEMO retry linked'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    conn.execute(
        "UPDATE case_assignments SET end_at=?1 WHERE case_id=?2",
        params![tuvalu_court::time::now_utc(), cid],
    )
    .unwrap();
    let (s, b) = c.post_idem(&path, "linked-key", req).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn f06_t06_t07_t30_import_checks_active_permissions_and_rechecks_on_commit() {
    let _lock = HEAVY.lock().await;
    let app = TestApp::demo();
    let (c, _, _, _) = scopes(&app).await;
    let elena = c.switch("elena").await;
    let conn = c.db(&app).open().unwrap();
    let sergei = user_id(&c, "Sergei").await;
    let olga = user_id(&c, "Olga").await;
    conn.execute("DELETE FROM user_permissions WHERE user_id=?1", [sergei])
        .unwrap();
    let username = |uid| {
        conn.query_row("SELECT username FROM users WHERE id=?1", [uid], |r| {
            r.get::<_, String>(0)
        })
        .unwrap()
    };
    let csv = format!(
        "number,category,title,registered_date,status,responsible_username,closed_date,closure_basis,parties\nDEMO-CIV-2001-0902,civil_contract,DEMO no powers,2001-01-01,registered,{},,,\nDEMO-CIV-2001-0903,civil_contract,DEMO active staff,2001-01-01,registered,{},,,\n",
        username(sergei),
        username(olga)
    );
    let (s, b) = elena
        .upload("/api/import/cases/preview", &[], "demo.csv", csv.as_bytes())
        .await;
    ok(s, &b);
    assert_eq!(b["rows"][0]["action"], "error");
    assert_eq!(b["rows"][1]["action"], "create");
    conn.execute("UPDATE users SET active=0 WHERE id=?1", [olga])
        .unwrap();
    let (s, r) = elena
        .post(&format!("/api/import/{}/commit", b["batch_id"]), json!({}))
        .await;
    err(s, &r, StatusCode::CONFLICT, "import_changed");
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM cases WHERE number IN ('DEMO-CIV-2001-0902','DEMO-CIV-2001-0903')",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}

#[tokio::test]
async fn f14_t05_t27_t29_participant_edits_check_scope_permissions_and_preserve_contacts() {
    let app = TestApp::demo();
    let (c, cid, hidden, hid) = scopes(&app).await;
    let (_, d) = c.get(&format!("/api/cases/{cid}")).await;
    let p = &d["participants"][0];
    let pid = p["id"].as_i64().unwrap();
    let req = json!({"version":1,"role":"claimant","representative_party_id":hid,"representation_basis":"DEMO hidden lawyer","service_contact":"DEMO service"});
    let path = format!("/api/cases/{cid}/participants/{pid}");
    let (s, b) = c.patch(&path, req.clone()).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    let sergei = c.switch("sergei").await;
    let elena = c.switch("elena").await;
    let(s,b)=elena.post(&format!("/api/cases/{cid}/assignments"),json!({"user_id":user_id(&c,"Sergei").await,"role":"service_officer","reason":"DEMO service"})).await;
    ok(s, &b);
    let (s, b) = sergei.patch(&path, req.clone()).await;
    err(s, &b, StatusCode::FORBIDDEN, "forbidden");
    let (s, b) = c
        .patch(&format!("/api/cases/{hidden}/participants/{pid}"), req)
        .await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    let conn = c.db(&app).open().unwrap();
    conn.execute(
        "UPDATE parties SET notes='DEMO retain ordinary notes' WHERE id=?1",
        [p["party_id"].as_i64()],
    )
    .unwrap();
    let (s, b) = c
        .patch(
            &format!("/api/parties/{}", p["party_id"]),
            json!({"version":1,"name":"DEMO spelling"}),
        )
        .await;
    ok(s, &b);
    assert_eq!(b["notes"], "DEMO retain ordinary notes");
}

#[tokio::test]
async fn f13_t04_t27_party_creation_replay_cannot_reveal_a_now_hidden_party() {
    let app = TestApp::demo();
    let (c, cid, _, _) = scopes(&app).await;
    let req = json!({"kind":"person","name":"DEMO later hidden"});
    let (s, p) = c
        .post_idem("/api/parties", "new-hidden-party", req.clone())
        .await;
    ok(s, &p);
    let (s, b) = c
        .post(
            &format!("/api/cases/{cid}/participants"),
            json!({"party_id":p["id"],"role":"claimant"}),
        )
        .await;
    ok(s, &b);
    c.db(&app)
        .open()
        .unwrap()
        .execute(
            "UPDATE case_assignments SET end_at=?1 WHERE case_id=?2",
            params![tuvalu_court::time::now_utc(), cid],
        )
        .unwrap();
    let (s, b) = c.post_idem("/api/parties", "new-hidden-party", req).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
}

async fn patch_idem(c: &Client, path: &str, key: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("PATCH")
        .uri(path)
        .header("host", "localhost")
        .header("x-tcr", "1")
        .header("idempotency-key", key)
        .header(
            "cookie",
            format!(
                "tcr_sandbox={}; tcr_session={}",
                c.sandbox.as_ref().unwrap(),
                c.session.as_ref().unwrap()
            ),
        )
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (s, b, _, _) = c.clone().send(req).await;
    (s, b)
}

#[tokio::test]
async fn f13_t04_t05_representative_changes_replay_once() {
    let app = TestApp::demo();
    let (c, cid, _, _) = scopes(&app).await;
    let (_, d) = c.get(&format!("/api/cases/{cid}")).await;
    let pid = d["participants"][0]["id"].as_i64().unwrap();
    let (s, p) = c
        .post(
            "/api/parties",
            json!({"kind":"person","name":"DEMO lawyer"}),
        )
        .await;
    ok(s, &p);
    let path = format!("/api/cases/{cid}/participants/{pid}");
    let req = json!({"version":1,"role":"claimant","representative_party_id":p["id"],"representation_basis":"DEMO mandate","service_contact":"DEMO address"});
    let (s, b) = patch_idem(&c, &path, "lawyer-add", req.clone()).await;
    ok(s, &b);
    let (s, replay) = patch_idem(&c, &path, "lawyer-add", req.clone()).await;
    ok(s, &replay);
    assert_eq!(b, replay);
    let mut changed = req;
    changed["role"] = json!("respondent");
    let (s, b) = patch_idem(&c, &path, "lawyer-add", changed).await;
    err(s, &b, StatusCode::CONFLICT, "idempotency_mismatch");
    let req = json!({"version":2,"role":"claimant","representative_party_id":null,"representation_basis":null,"service_contact":"DEMO address"});
    let (s, b) = patch_idem(&c, &path, "lawyer-remove", req.clone()).await;
    ok(s, &b);
    let (s, replay) = patch_idem(&c, &path, "lawyer-remove", req).await;
    ok(s, &replay);
    assert_eq!(b, replay);
}

#[tokio::test]
async fn f01_t27_document_source_cannot_make_a_hidden_contact_visible() {
    let app = TestApp::demo();
    let (c, cid, _, pid) = scopes(&app).await;
    let source = pid.to_string();
    let (s, b) = c
        .upload(
            &format!("/api/cases/{cid}/documents"),
            &[
                ("title", "DEMO attempted source link"),
                ("doc_type", "evidence"),
                ("source", "party"),
                ("source_party_id", &source),
                ("visibility", "party_material"),
            ],
            "demo.pdf",
            &pdf("DEMO public material"),
        )
        .await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
    assert_eq!(
        c.get(&format!("/api/parties/{pid}")).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn f01_t27_linked_intake_dispatch_follows_hidden_case_scope() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    register_case(&c, "DEMO visible control").await;
    let (s, p) = c
        .post(
            "/api/parties",
            json!({"kind":"person","name":"DEMO hidden intake sender"}),
        )
        .await;
    ok(s, &p);
    let(s,i)=c.post("/api/intakes",json!({"sender_name":"DEMO hidden intake sender","sender_party_id":p["id"],"channel":"counter","received_date":today(),"description":"DEMO linked contact","is_paper_original":false})).await;
    ok(s, &i);
    let iid = i["id"].as_i64().unwrap();
    let (s, b) = c
        .post(
            &format!("/api/intakes/{iid}/request-info"),
            json!({"missing_items":"DEMO signature","method":"post","address":"DEMO address"}),
        )
        .await;
    ok(s, &b);
    let (s, b) = c
        .post(&format!("/api/intakes/{iid}/mark-ready"), json!({}))
        .await;
    ok(s, &b);
    let (_, refs) = c.get("/api/ref").await;
    let(s,b)=c.post(&format!("/api/intakes/{iid}/register"),json!({"registry_id":refs["registries"][0]["id"],"category":"civil_contract","title":"DEMO hidden linked intake","participants":[{"party_id":p["id"],"role":"claimant"}]})).await;
    ok(s, &b);
    let cid = b["case_id"].as_i64().unwrap();
    let conn = c.db(&app).open().unwrap();
    conn.execute("UPDATE cases SET restricted=1 WHERE id=?1", [cid])
        .unwrap();
    conn.execute(
        "UPDATE case_assignments SET end_at=?1 WHERE case_id=?2",
        params![tuvalu_court::time::now_utc(), cid],
    )
    .unwrap();
    let (s, b) = c.get(&format!("/api/parties/{}", p["id"])).await;
    err(s, &b, StatusCode::NOT_FOUND, "not_found");
}
