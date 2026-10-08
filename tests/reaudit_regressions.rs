//! Regressions for the re-audit of 6ace6b8 (R01 replay re-renders under current access,
//! R02 `case.view_all` is read-only for party records, R03 decision changes need access to the
//! bound material). The first five tests are the auditor's proposals, unchanged; each failed on
//! 6ace6b8. All fixtures fictional, local router only.
mod common;
use common::*;
use axum::http::StatusCode;
use rusqlite::params;
use serde_json::{json, Value};

const SECRET: &str = "DEMO PRIVATE REAUDIT TITLE";
fn allow(c: &Client, app: &TestApp, uid: i64, permission: &str) {
    c.db(app).open().unwrap().execute(
        "INSERT OR IGNORE INTO user_permissions(user_id,permission,granted_at) VALUES(?1,?2,?3)",
        params![uid,permission,tuvalu_court::time::now_utc()],
    ).unwrap();
}
fn assert_no_secret(body: &Value, sha: &str) {
    let text=body.to_string();
    assert!(!text.contains(SECRET), "Revoked/restricted title returned: {text}");
    assert!(!text.contains(sha), "Revoked/restricted checksum returned: {text}");
}
async fn fixture() -> (TestApp, Client, i64, i64, i64, i64, String) {
    let app=TestApp::demo();
    let clerk=app.persona("olga").await;
    let (cid,_)=register_case(&clerk,"DEMO repeat audit case").await;
    let clerk_id=user_id(&clerk,"Olga").await;
    let judge_id=user_id(&clerk,"Viktor").await;
    allow(&clerk,&app,clerk_id,"decision.draft");
    allow(&clerk,&app,clerk_id,"decision.finalise");
    let (doc,vid)=insert_document(&clerk.db(&app),cid,SECRET,"decision","restricted",judge_id);
    let sha:String=clerk.db(&app).open().unwrap().query_row(
        "SELECT sha256 FROM document_versions WHERE id=?1",[vid],|r|r.get(0)).unwrap();
    (app,clerk,cid,clerk_id,doc,vid,sha)
}
fn grant(c:&Client,app:&TestApp,uid:i64,doc:i64){
    c.db(app).open().unwrap().execute(
        "INSERT INTO document_grants(document_id,user_id,reason,granted_by,granted_at) VALUES(?1,?2,'DEMO temporary',?2,?3)",
        params![doc,uid,tuvalu_court::time::now_utc()]).unwrap();
}
fn revoke(c:&Client,app:&TestApp,uid:i64,doc:i64){
    c.db(app).open().unwrap().execute(
        "UPDATE document_grants SET revoked_at=?3 WHERE document_id=?1 AND user_id=?2 AND revoked_at IS NULL",
        params![doc,uid,tuvalu_court::time::now_utc()]).unwrap();
}

async fn judge_draft(app:&TestApp,c:&Client,cid:i64,vid:i64) -> Value {
    let head=c.switch("elena").await;
    let judge_id=user_id(c,"Viktor").await;
    let(s,b)=head.post(&format!("/api/cases/{cid}/assignments"),json!({"user_id":judge_id,"role":"judge","reason":"DEMO independent author"})).await;
    ok(s,&b);
    let judge=c.switch("viktor").await;
    let(s,d)=judge.post(&format!("/api/cases/{cid}/decisions"),json!({"title":SECRET,"document_version_id":vid})).await;
    ok(s,&d);
    assert!(app.dir.exists());
    d
}

#[tokio::test]
async fn r01_replay_create_after_document_grant_revoked_must_not_return_cached_secrets() {
    let (app,c,cid,uid,doc,vid,sha)=fixture().await;
    grant(&c,&app,uid,doc);
    let path=format!("/api/cases/{cid}/decisions");
    let req=json!({"title":SECRET,"document_version_id":vid});
    let (s,created)=c.post_idem(&path,"reaudit-r01-create",req.clone()).await;
    ok(s,&created);
    assert_eq!(created["sha256"],sha);
    revoke(&c,&app,uid,doc);
    let (s,ordinary)=c.get(&format!("/api/decisions/{}",created["id"])).await;
    ok(s,&ordinary); assert_eq!(ordinary["restricted"],true); assert_no_secret(&ordinary,&sha);
    let (s,replayed)=c.post_idem(&path,"reaudit-r01-create",req).await;
    assert!(s.is_success() || s==StatusCode::FORBIDDEN || s==StatusCode::NOT_FOUND,"{s}: {replayed}");
    assert_no_secret(&replayed,&sha); // Fails on 6ace6b8 according to the reviewed code path.
    let count:i64=c.db(&app).open().unwrap().query_row("SELECT COUNT(*) FROM decisions WHERE case_id=?1",[cid],|r|r.get(0)).unwrap();
    assert_eq!(count,1,"Redaction must not re-execute creation");
}

#[tokio::test]
async fn r01_replay_finalise_after_document_grant_revoked_must_be_redacted_or_denied() {
    let (app,c,cid,uid,doc,vid,sha)=fixture().await;
    grant(&c,&app,uid,doc);
    let(s,d)=c.post(&format!("/api/cases/{cid}/decisions"),json!({"title":SECRET,"document_version_id":vid})).await;
    ok(s,&d);
    let path=format!("/api/decisions/{}/finalise",d["id"]);
    let req=json!({"version":d["version"],"document_version_id":vid,"decision_date":today()});
    let(s,done)=c.post_idem(&path,"reaudit-r01-finalise",req.clone()).await; ok(s,&done);
    revoke(&c,&app,uid,doc);
    let(s,b)=c.post_idem(&path,"reaudit-r01-finalise",req).await;
    assert!(s.is_success() || s==StatusCode::FORBIDDEN || s==StatusCode::NOT_FOUND,"{s}: {b}");
    assert_no_secret(&b,&sha);
}

#[tokio::test]
async fn r02_case_view_all_alone_must_not_edit_party_contact() {
    let app=TestApp::demo(); let c=app.persona("olga").await;
    let(cid,_)=register_case(&c,"DEMO read-only case").await;
    let observer=c.switch("elena").await;
    let uid=user_id(&c,"Elena").await;
    let db=c.db(&app); let conn=db.open().unwrap();
    conn.execute("DELETE FROM user_permissions WHERE user_id=?1",[uid]).unwrap();
    allow(&c,&app,uid,"case.view_all");
    let pid:i64=conn.query_row("SELECT party_id FROM case_participations WHERE case_id=?1 LIMIT 1",[cid],|r|r.get(0)).unwrap();
    let path=format!("/api/parties/{pid}");
    let(s,shown)=observer.get(&path).await; ok(s,&shown);
    let before=shown["party"].clone();
    let(s,changed)=observer.patch(&path,json!({"version":before["version"],"name":before["name"],"contact_email":"changed@example.invalid"})).await;
    assert!(s==StatusCode::FORBIDDEN || s==StatusCode::NOT_FOUND,"Read permission allowed a write: {s} {changed}");
    let email:Option<String>=conn.query_row("SELECT contact_email FROM parties WHERE id=?1",[pid],|r|r.get(0)).unwrap();
    assert_eq!(json!(email),before["contact_email"]);
    assert_eq!(shown["editable"],false);
}

#[tokio::test]
async fn r03_hidden_draft_cannot_be_declassified_by_replacing_its_attachment() {
    let(app,c,cid,uid,_doc,vid,sha)=fixture().await;
    let d=judge_draft(&app,&c,cid,vid).await;
    // The clerk has NEVER had a grant to the judge's restricted source material.
    let(_,replacement)=insert_document(&c.db(&app),cid,"DEMO accessible replacement","decision","party_material",uid);
    let path=format!("/api/decisions/{}",d["id"]);
    let(s,hidden)=c.get(&path).await; ok(s,&hidden); assert_eq!(hidden["restricted"],true);
    let(s,b)=c.patch(&path,json!({"version":hidden["version"],"document_version_id":replacement})).await;
    assert_no_secret(&b,&sha); // Current code preserves the hidden title and now returns it openly.
    assert!(s==StatusCode::FORBIDDEN || s==StatusCode::NOT_FOUND,"Replacement of inaccessible current material allowed: {s} {b}");
    let actual:i64=c.db(&app).open().unwrap().query_row("SELECT document_version_id FROM decisions WHERE id=?1",[d["id"].as_i64().unwrap()],|r|r.get(0)).unwrap();
    assert_eq!(actual,vid);
}

#[tokio::test]
async fn r03_hidden_draft_withdraw_requires_access_to_current_material_or_explicit_separate_authority() {
    let(app,c,cid,_uid,_doc,vid,_)=fixture().await;
    let d=judge_draft(&app,&c,cid,vid).await;
    let(s,b)=c.post(&format!("/api/decisions/{}/withdraw",d["id"]),json!({"reason":"DEMO no current document access"})).await;
    assert!(s==StatusCode::FORBIDDEN || s==StatusCode::NOT_FOUND,"Hidden draft withdrawn with only generic draft permission: {s} {b}");
    let status:String=c.db(&app).open().unwrap().query_row("SELECT status FROM decisions WHERE id=?1",[d["id"].as_i64().unwrap()],|r|r.get(0)).unwrap();
    assert_eq!(status,"draft");
}

// ---------------------------------------------------------------- additional R01–R03 regressions

#[tokio::test]
async fn r01_replay_without_access_change_returns_the_same_record_and_runs_nothing() {
    let (app,c,cid,uid,doc,vid,sha)=fixture().await;
    grant(&c,&app,uid,doc);
    let path=format!("/api/cases/{cid}/decisions");
    let req=json!({"title":SECRET,"document_version_id":vid});
    let (s,first)=c.post_idem(&path,"reaudit-positive",req.clone()).await; ok(s,&first);
    let (s,again)=c.post_idem(&path,"reaudit-positive",req).await; ok(s,&again);
    assert_eq!(first,again);
    assert_eq!(again["sha256"],sha);
    let count:i64=c.db(&app).open().unwrap().query_row("SELECT COUNT(*) FROM decisions WHERE case_id=?1",[cid],|r|r.get(0)).unwrap();
    assert_eq!(count,1);
}

#[tokio::test]
async fn r01_dispatch_replay_after_document_grant_revoked_does_not_return_cached_metadata() {
    let (app,c,cid,uid,doc,vid,sha)=fixture().await;
    grant(&c,&app,uid,doc);
    let path=format!("/api/cases/{cid}/dispatches");
    let req=json!({"kind":"working_document","method":"post","recipient_name":"DEMO recipient","address":"DEMO address","purpose":"DEMO share","version_ids":[vid],"include_restricted":true});
    let (s,first)=c.post_idem(&path,"reaudit-dispatch",req.clone()).await; ok(s,&first);
    assert!(first.to_string().contains(&sha));
    revoke(&c,&app,uid,doc);
    let (s,ordinary)=c.get(&format!("/api/dispatches/{}",first["id"])).await;
    ok(s,&ordinary); assert_no_secret(&ordinary,&sha);
    let (s,replayed)=c.post_idem(&path,"reaudit-dispatch",req).await;
    assert!(s.is_success() || s==StatusCode::FORBIDDEN || s==StatusCode::NOT_FOUND,"{s}: {replayed}");
    assert_no_secret(&replayed,&sha);
    let count:i64=c.db(&app).open().unwrap().query_row("SELECT COUNT(*) FROM dispatches WHERE case_id=?1",[cid],|r|r.get(0)).unwrap();
    assert_eq!(count,1,"A replay must not create a second dispatch");
}

#[tokio::test]
async fn r01_party_create_replay_shows_current_contacts_not_the_stored_copy() {
    let app=TestApp::demo(); let c=app.persona("olga").await;
    let (s,twin)=c.post("/api/parties",json!({"kind":"person","name":"DEMO Twin","contact_email":"old@example.invalid"})).await; ok(s,&twin);
    let body=json!({"kind":"person","name":"DEMO Twin","contact_email":"second@example.invalid"});
    let (s,first)=c.post_idem("/api/parties","reaudit-party",body.clone()).await; ok(s,&first);
    assert_eq!(first["same_name_records"][0]["contact_email"],"old@example.invalid");
    let path=format!("/api/parties/{}",twin["id"]);
    let (_,shown)=c.get(&path).await;
    let (s,b)=c.patch(&path,json!({"version":shown["party"]["version"],"name":"DEMO Twin","contact_email":"new@example.invalid"})).await; ok(s,&b);
    let (s,replayed)=c.post_idem("/api/parties","reaudit-party",body).await; ok(s,&replayed);
    assert_eq!(replayed["id"],first["id"]);
    assert_eq!(replayed["same_name_records"][0]["contact_email"],"new@example.invalid");
    assert!(!replayed.to_string().contains("old@example.invalid"));
}

#[tokio::test]
async fn r02_view_all_observer_reads_but_cannot_create_and_party_edit_grants_correction() {
    let app=TestApp::demo(); let c=app.persona("olga").await;
    let (cid,_)=register_case(&c,"DEMO observer case").await;
    let uid=user_id(&c,"Elena").await;
    let observer=c.switch("elena").await;
    let db=c.db(&app); let conn=db.open().unwrap();
    conn.execute("DELETE FROM user_permissions WHERE user_id=?1",[uid]).unwrap();
    allow(&c,&app,uid,"case.view_all");
    let (s,list)=observer.get("/api/parties?q=Fenwick").await; ok(s,&list);
    assert!(!list["items"].as_array().unwrap().is_empty());
    let (s,b)=observer.post("/api/parties",json!({"kind":"person","name":"DEMO observer write"})).await;
    err(s,&b,StatusCode::FORBIDDEN,"forbidden");
    let pid:i64=conn.query_row("SELECT party_id FROM case_participations WHERE case_id=?1 LIMIT 1",[cid],|r|r.get(0)).unwrap();
    let path=format!("/api/parties/{pid}");
    allow(&c,&app,uid,"party.edit");
    let (s,shown)=observer.get(&path).await; ok(s,&shown);
    assert_eq!(shown["editable"],true);
    let (s,b)=observer.patch(&path,json!({"version":shown["party"]["version"],"name":shown["party"]["name"],"contact_email":"corrected@example.invalid"})).await;
    ok(s,&b);
    assert_eq!(b["contact_email"],"corrected@example.invalid");
}

#[tokio::test]
async fn r03_hidden_draft_cannot_be_finalised_and_hidden_finalised_decision_cannot_be_amended() {
    let(app,c,cid,uid,_doc,vid,_sha)=fixture().await;
    let d=judge_draft(&app,&c,cid,vid).await;
    let(s,b)=c.post(&format!("/api/decisions/{}/finalise",d["id"]),json!({"version":d["version"],"document_version_id":vid,"decision_date":today()})).await;
    assert!(s==StatusCode::FORBIDDEN || s==StatusCode::NOT_FOUND,"Hidden draft finalised: {s} {b}");
    let judge=c.switch("viktor").await;
    let(s,done)=judge.post(&format!("/api/decisions/{}/finalise",d["id"]),json!({"version":d["version"],"document_version_id":vid,"decision_date":today()})).await;
    ok(s,&done);
    let(_,replacement)=insert_document(&c.db(&app),cid,"DEMO accessible amendment","decision","party_material",uid);
    let(s,b)=c.post(&format!("/api/decisions/{}/amend",d["id"]),json!({"amendment_basis":"DEMO hidden correction","document_version_id":replacement})).await;
    assert!(s==StatusCode::FORBIDDEN || s==StatusCode::NOT_FOUND,"Hidden finalised decision amended: {s} {b}");
    assert!(!b.to_string().contains(SECRET));
    let count:i64=c.db(&app).open().unwrap().query_row("SELECT COUNT(*) FROM decisions WHERE case_id=?1",[cid],|r|r.get(0)).unwrap();
    assert_eq!(count,1);
}

#[tokio::test]
async fn r03_author_with_access_still_replaces_the_attachment() {
    let(app,c,cid,_uid,_doc,vid,_sha)=fixture().await;
    let d=judge_draft(&app,&c,cid,vid).await;
    let judge=c.switch("viktor").await;
    let judge_id=user_id(&c,"Viktor").await;
    let(_,replacement)=insert_document(&c.db(&app),cid,"DEMO judge replacement","decision","party_material",judge_id);
    let(s,b)=judge.patch(&format!("/api/decisions/{}",d["id"]),json!({"version":d["version"],"document_version_id":replacement})).await;
    ok(s,&b);
    assert_eq!(b["document_version_id"],replacement);
    assert_eq!(b["title"],SECRET);
}
