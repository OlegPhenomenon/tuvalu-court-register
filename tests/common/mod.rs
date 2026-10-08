//! Integration-test harness: a real router over a temp data dir, driven through HTTP requests.
//! Usage:
//!   let app = TestApp::demo();                 // fresh demo installation (template seeded)
//!   let olga = app.persona("olga").await;      // new sandbox + persona session
//!   let elena = olga.switch("elena").await;    // same sandbox, other person
//!   let (status, body) = olga.post("/api/intakes", json!({...})).await;
#![allow(dead_code)]

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::path::PathBuf;
use tower::ServiceExt;
use tuvalu_court::config::{Config, Mode};
use tuvalu_court::state::AppState;

pub struct TestApp {
    pub router: Router,
    pub state: AppState,
    pub dir: PathBuf,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tcr-test-{}", hex::encode(tuvalu_court::auth::random_bytes::<8>())));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

impl TestApp {
    pub fn demo() -> Self {
        Self::with_mode(Mode::Demo)
    }

    pub fn production() -> Self {
        Self::with_mode(Mode::Production)
    }

    fn with_mode(mode: Mode) -> Self {
        let dir = temp_dir();
        let (state, _rx) = AppState::init(Config::for_tests(dir.clone(), mode)).expect("init");
        // The outbox receiver is dropped: tests call `tuvalu_court::outbox::process` explicitly.
        let router = tuvalu_court::api::router(state.clone());
        Self { router, state, dir }
    }

    /// A client with no cookies.
    pub fn anon(&self) -> Client {
        Client { router: self.router.clone(), sandbox: None, session: None }
    }

    /// New sandbox + signed in as `persona` (demo mode).
    pub async fn persona(&self, persona: &str) -> Client {
        let mut c = self.anon();
        let res = c.raw(Method::POST, "/api/demo/start", Some(json!({}))).await;
        assert_eq!(res.0, StatusCode::OK, "demo/start: {}", res.1);
        c.switch_in_place(persona).await;
        c
    }
}

#[derive(Clone)]
pub struct Client {
    pub router: Router,
    pub sandbox: Option<String>,
    pub session: Option<String>,
}

impl Client {
    /// Same sandbox, different person (new session).
    pub async fn switch(&self, persona: &str) -> Client {
        let mut c = Client { router: self.router.clone(), sandbox: self.sandbox.clone(), session: None };
        c.switch_in_place(persona).await;
        c
    }

    async fn switch_in_place(&mut self, persona: &str) {
        let (s, b) = self.raw(Method::POST, "/api/demo/login", Some(json!({ "persona": persona }))).await;
        assert_eq!(s, StatusCode::OK, "demo/login {persona}: {b}");
    }

    fn cookie_header(&self) -> String {
        let mut parts = vec![];
        if let Some(s) = &self.sandbox {
            parts.push(format!("tcr_sandbox={s}"));
        }
        if let Some(s) = &self.session {
            parts.push(format!("tcr_session={s}"));
        }
        parts.join("; ")
    }

    fn absorb_cookies(&mut self, res: &axum::response::Response) {
        for v in res.headers().get_all(header::SET_COOKIE) {
            let v = v.to_str().unwrap();
            let (kv, _) = v.split_once(';').unwrap_or((v, ""));
            let (k, val) = kv.split_once('=').unwrap();
            let val = if val.is_empty() { None } else { Some(val.to_string()) };
            match k {
                "tcr_sandbox" => self.sandbox = val,
                "tcr_session" => self.session = val,
                _ => {}
            }
        }
    }

    pub async fn send(&mut self, req: Request<Body>) -> (StatusCode, Value, axum::http::HeaderMap, Vec<u8>) {
        let res = self.router.clone().oneshot(req).await.unwrap();
        self.absorb_cookies(&res);
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json, headers, bytes)
    }

    fn builder(&self, method: Method, path: &str) -> axum::http::request::Builder {
        let mut b = Request::builder().method(method.clone()).uri(path).header(header::HOST, "localhost");
        let cookies = self.cookie_header();
        if !cookies.is_empty() {
            b = b.header(header::COOKIE, cookies);
        }
        if method != Method::GET {
            b = b.header("x-tcr", "1");
        }
        b
    }

    /// Request that also updates cookies on this client (login/start/switch).
    pub async fn raw(&mut self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let req = self
            .builder(method, path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty))
            .unwrap();
        let (s, v, _, _) = self.send(req).await;
        (s, v)
    }

    pub async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.clone().raw(Method::GET, path, None).await
    }
    pub async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.clone().raw(Method::POST, path, Some(body)).await
    }
    pub async fn patch(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.clone().raw(Method::PATCH, path, Some(body)).await
    }
    pub async fn delete(&self, path: &str) -> (StatusCode, Value) {
        self.clone().raw(Method::DELETE, path, None).await
    }

    /// POST with an Idempotency-Key header.
    pub async fn post_idem(&self, path: &str, key: &str, body: Value) -> (StatusCode, Value) {
        let req = self
            .builder(Method::POST, path)
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", key)
            .body(Body::from(body.to_string()))
            .unwrap();
        let (s, v, _, _) = self.clone().send(req).await;
        (s, v)
    }

    /// GET returning raw bytes + headers (downloads).
    pub async fn get_bytes(&self, path: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let req = self.builder(Method::GET, path).body(Body::empty()).unwrap();
        let (s, _, h, b) = self.clone().send(req).await;
        (s, h, b)
    }

    /// Multipart upload: text `fields` + one file part named `file`.
    pub async fn upload(&self, path: &str, fields: &[(&str, &str)], filename: &str, bytes: &[u8]) -> (StatusCode, Value) {
        let boundary = "----tcrtestboundary";
        let mut body = Vec::new();
        for (k, v) in fields {
            body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes());
        }
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let req = self
            .builder(Method::POST, path)
            .header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
            .body(Body::from(body))
            .unwrap();
        let (s, v, _, _) = self.clone().send(req).await;
        (s, v)
    }

    /// Multipart upload with a stable command key, preserving the ordinary upload helper.
    pub async fn upload_idem(&self, path: &str, key: &str, fields: &[(&str, &str)], filename: &str, bytes: &[u8]) -> (StatusCode, Value) {
        let boundary = "----tcrtestidem";
        let mut body = Vec::new();
        for (k, v) in fields {
            body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes());
        }
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let req = self.builder(Method::POST, path)
            .header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
            .header("idempotency-key", key).body(Body::from(body)).unwrap();
        let (s, v, _, _) = self.clone().send(req).await;
        (s, v)
    }

    /// PATCH with an Idempotency-Key, for task reassignment retries.
    pub async fn patch_idem(&self, path: &str, key: &str, body: Value) -> (StatusCode, Value) {
        let req = self.builder(Method::PATCH, path).header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", key).body(Body::from(body.to_string())).unwrap();
        let (s, v, _, _) = self.clone().send(req).await;
        (s, v)
    }

    /// The sandbox database behind this client (to run the outbox or inspect rows in tests).
    pub fn db(&self, app: &TestApp) -> tuvalu_court::db::Db {
        let mut h = axum::http::HeaderMap::new();
        h.insert(header::COOKIE, self.cookie_header().parse().unwrap());
        app.state.resolve_db(&h).unwrap().0
    }
}

/// A tiny valid PDF with the given text (for upload tests).
pub fn pdf(text: &str) -> Vec<u8> {
    tuvalu_court::seed_demo::demo_pdf("TEST", &[text])
}

/// Court-local date `days` from today, YYYY-MM-DD.
pub fn today() -> String {
    tuvalu_court::time::today_local()
}

pub fn ok(s: StatusCode, body: &Value) {
    assert!(s.is_success(), "expected success, got {s}: {body}");
}

pub fn err(s: StatusCode, body: &Value, status: StatusCode, code: &str) {
    assert_eq!(s, status, "body: {body}");
    assert_eq!(body["error"]["code"], code, "body: {body}");
}

/// Create an intake as Olga and return its id.
pub async fn new_intake(c: &Client, sender: &str) -> i64 {
    let (s, b) = c
        .post(
            "/api/intakes",
            json!({
                "sender_name": sender, "channel": "counter", "origin_island": "funafuti",
                "received_date": today(), "description": "Claim about an unpaid boat repair (DEMO)",
                "is_paper_original": true, "paper_location": "Registry cabinet A, folder 12"
            }),
        )
        .await;
    ok(s, &b);
    b["id"].as_i64().unwrap()
}

/// Intake → ready → registered case in DEMO-CIV with two participants. Returns (case_id, number).
pub async fn register_case(c: &Client, title: &str) -> (i64, String) {
    let intake = new_intake(c, "Alexei Fenwick").await;
    let (s, b) = c.post(&format!("/api/intakes/{intake}/mark-ready"), json!({})).await;
    ok(s, &b);
    let (s, refs) = c.get("/api/ref").await;
    ok(s, &refs);
    let registry_id = refs["registries"].as_array().unwrap().iter().find(|r| r["series"] == "DEMO-CIV").unwrap()["id"].as_i64().unwrap();
    let (s, b) = c
        .post(
            &format!("/api/intakes/{intake}/register"),
            json!({
                "registry_id": registry_id, "category": "civil_contract", "title": title,
                "participants": [
                    { "new_party": { "kind": "person", "name": "Alexei Fenwick", "contact_email": "alexei@example.invalid" }, "role": "claimant", "service_contact": "alexei@example.invalid" },
                    { "new_party": { "kind": "person", "name": "Maria Calder", "contact_email": "maria@example.invalid" }, "role": "respondent", "service_contact": "maria@example.invalid" }
                ]
            }),
        )
        .await;
    ok(s, &b);
    (b["case_id"].as_i64().unwrap(), b["number"].as_str().unwrap().to_string())
}

/// User id of a demo persona.
pub async fn user_id(c: &Client, persona_display_prefix: &str) -> i64 {
    let (_, refs) = c.get("/api/ref").await;
    refs["staff"].as_array().unwrap().iter().find(|u| u["display_name"].as_str().unwrap().starts_with(persona_display_prefix)).unwrap()["id"]
        .as_i64()
        .unwrap()
}

/// Insert a document + version 1 directly (bypasses the upload endpoint) so modules can be tested
/// independently. `visibility`: administrative | party_material | restricted | judicial_note.
/// Returns (document_id, version_id).
pub fn insert_document(db: &tuvalu_court::db::Db, case_id: i64, title: &str, doc_type: &str, visibility: &str, created_by: i64) -> (i64, i64) {
    let bytes = pdf(title);
    let (key, sha) = tuvalu_court::storage::write_blob(db, &bytes).unwrap();
    let now = tuvalu_court::time::now_utc();
    let conn = db.open().unwrap();
    conn.execute(
        "INSERT INTO documents (case_id, title, doc_type, source, visibility, created_by, created_at)
         VALUES (?1, ?2, ?3, 'court', ?4, ?5, ?6)",
        rusqlite::params![case_id, title, doc_type, visibility, created_by, now],
    )
    .unwrap();
    let doc = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO document_versions (document_id, version_no, filename, content_type, size_bytes, sha256, storage_key, scan_status, uploaded_by, uploaded_at)
         VALUES (?1, 1, ?2, 'application/pdf', ?3, ?4, ?5, 'clean', ?6, ?7)",
        rusqlite::params![doc, format!("{}.pdf", title.replace(' ', "_")), bytes.len() as i64, sha, key, created_by, now],
    )
    .unwrap();
    (doc, conn.last_insert_rowid())
}

/// A fictional dated settlement document for closure tests that do not hold a hearing.
pub async fn settlement_document(c: &Client, app: &TestApp, case_id: i64) -> i64 {
    let uid = user_id(c, "Olga").await;
    let db = c.db(app);
    let (doc, vid) = insert_document(&db, case_id, "DEMO settlement agreement", "correspondence", "party_material", uid);
    db.open().unwrap().execute("UPDATE documents SET document_date=?2 WHERE id=?1", rusqlite::params![doc, today()]).unwrap();
    vid
}

/// A database at schema version 6 (before the audit migrations) holding legacy rows written with
/// that schema only: a case, a document with two versions, a finalised decision, a sent dispatch
/// with its mailbox copy and a queued notice. Today's DEMO seed targets the current schema.
pub fn legacy_v6_db(dir: &std::path::Path) -> tuvalu_court::db::Db {
    let db = tuvalu_court::db::Db::new(dir.join("legacy.sqlite"), dir.join("legacy-files"), None);
    std::fs::create_dir_all(db.files_dir()).unwrap();
    let conn = db.open().unwrap();
    for sql in [
        include_str!("../../src/migrations/0001_init.sql"),
        include_str!("../../src/migrations/0002_hearings.sql"),
        include_str!("../../src/migrations/0003_documents.sql"),
        include_str!("../../src/migrations/0004_dispatch.sql"),
        include_str!("../../src/migrations/0005_reports.sql"),
        include_str!("../../src/migrations/0006_admin.sql"),
    ] {
        conn.execute_batch(sql).unwrap();
    }
    conn.execute_batch("PRAGMA user_version=6").unwrap();
    tuvalu_court::seed::seed_reference(&db).unwrap();
    let now = tuvalu_court::time::now_utc();
    conn.execute(
        "INSERT INTO users (username, display_name, password_hash, is_judge, created_at) VALUES ('legacyjudge', 'DEMO Legacy Judge', 'x', 1, ?1)",
        [&now],
    )
    .unwrap();
    let uid = conn.last_insert_rowid();
    conn.execute_batch("INSERT INTO court_units (code, name) VALUES ('DEMO-LEG', 'DEMO legacy court unit (fictional)')").unwrap();
    conn.execute(
        "INSERT INTO registries (court_unit_id, series, name) VALUES (?1, 'DEMO-CIV', 'DEMO civil register')",
        [conn.last_insert_rowid()],
    )
    .unwrap();
    let registry = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO cases (registry_id, year, seq, number, title, category, status, registered_date, registered_at, registered_by, updated_at)
         VALUES (?1, 2025, 1, 'DEMO-CIV-2025-0001', 'DEMO legacy case', 'civil_contract', 'active', '2025-03-01', ?2, ?3, ?2)",
        rusqlite::params![registry, now, uid],
    )
    .unwrap();
    let case_id = conn.last_insert_rowid();
    let (doc, v1) = insert_document(&db, case_id, "DEMO legacy order", "order", "party_material", uid);
    let v2_bytes = pdf("DEMO legacy order v2");
    let (blob, sha) = tuvalu_court::storage::write_blob(&db, &v2_bytes).unwrap();
    conn.execute(
        "INSERT INTO document_versions (document_id, version_no, filename, content_type, size_bytes, sha256, storage_key, scan_status, uploaded_by, uploaded_at)
         VALUES (?1, 2, 'DEMO_legacy_order_v2.pdf', 'application/pdf', ?6, ?2, ?3, 'clean', ?4, ?5)",
        rusqlite::params![doc, sha, blob, uid, now, v2_bytes.len() as i64],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO decisions (case_id, title, decision_date, status, document_id, document_version_id, author_user_id, finalised_by, finalised_at, created_at)
         VALUES (?1, 'DEMO legacy order', '2025-04-01', 'finalised', ?2, ?3, ?4, ?4, ?5, ?5)",
        rusqlite::params![case_id, doc, v1, uid, now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO parties (kind, name, contact_email, created_by, created_at) VALUES ('person', 'DEMO Legacy Party', 'legacy.party@example.invalid', ?1, ?2)",
        rusqlite::params![uid, now],
    )
    .unwrap();
    let party = conn.last_insert_rowid();
    for (kind, status) in [("copies", "sent"), ("notice", "queued")] {
        conn.execute(
            "INSERT INTO dispatches (case_id, kind, recipient_party_id, recipient_name, method, address, subject, body, status, prepared_by, prepared_at, queued_by, queued_at, sent_at)
             VALUES (?1, ?2, ?3, 'DEMO Legacy Party', 'email', 'legacy.party@example.invalid', 'DEMO legacy message', 'DEMO body', ?4, ?5, ?6, ?5, ?6,
                     CASE WHEN ?4 = 'sent' THEN ?6 END)",
            rusqlite::params![case_id, kind, party, status, uid, now],
        )
        .unwrap();
        let dispatch = conn.last_insert_rowid();
        conn.execute("INSERT INTO dispatch_items (dispatch_id, document_version_id) VALUES (?1, ?2)", rusqlite::params![dispatch, v1]).unwrap();
        if status == "sent" {
            conn.execute(
                "INSERT INTO mailbox (dispatch_id, attempt_no, to_address, subject, body, attachments, delivered_at)
                 VALUES (?1, 1, 'legacy.party@example.invalid', 'DEMO legacy message', 'DEMO body', '[]', ?2)",
                rusqlite::params![dispatch, now],
            )
            .unwrap();
        }
    }
    db
}
