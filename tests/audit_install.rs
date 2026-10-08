mod common;
use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{Value, json};
use std::process::{Command, Stdio};
use tuvalu_court::{auth, outbox, seed};
/// Import/export share one process-wide heavy-operation permit; serialise the tests that use it.
static HEAVY: std::sync::LazyLock<tokio::sync::Mutex<()>> = std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

async fn enrolled(app: &TestApp) -> Client {
    let db = app.state.main_db.as_ref().unwrap();
    let uid = seed::create_user(
        db,
        "demostaff",
        "DEMO Staff",
        "temporary-pass-99",
        false,
        &[
            "case.view_all".into(),
            "intake.manage".into(),
            "dispatch.manage".into(),
            "admin.settings".into(),
        ],
    )
    .unwrap();
    db.open()
        .unwrap()
        .execute("UPDATE users SET must_change_password=1 WHERE id=?1", [uid])
        .unwrap();
    let mut c = app.anon();
    let (s, b) = c
        .raw(
            Method::POST,
            "/api/auth/login",
            Some(json!({"username":"demostaff","password":"temporary-pass-99"})),
        )
        .await;
    ok(s, &b);
    let (s, b) = c.post("/api/auth/totp/setup", json!({})).await;
    ok(s, &b);
    let code = auth::current_totp(b["secret"].as_str().unwrap()).unwrap();
    let (s, b) = c.raw(Method::POST, "/api/auth/totp/enable", Some(json!({"code":code}))).await;
    ok(s, &b);
    c
}

#[tokio::test]
async fn f19_t34_t36_temporary_password_blocks_direct_http() {
    let app = TestApp::production();
    let mut c = enrolled(&app).await;
    let (s, b) = c.get("/api/cases").await;
    err(s, &b, StatusCode::FORBIDDEN, "password_change_required");
    let (_, me) = c.get("/api/auth/me").await;
    assert_eq!(me["must_change_password"], true);
    let db = app.state.main_db.as_ref().unwrap();
    let uid = db
        .open()
        .unwrap()
        .query_row("SELECT id FROM users WHERE username='demostaff'", [], |r| r.get(0))
        .unwrap();
    let mut other = c.clone();
    other.session = Some(auth::create_session(&db.open().unwrap(), uid, true, 12).unwrap());
    // The next TOTP step is accepted; use it without sleeping across a wall-clock boundary.
    let conn = db.open().unwrap();
    conn.execute("UPDATE users SET totp_last_step=NULL WHERE id=?1", [uid]).unwrap();
    let secret: String = conn
        .query_row("SELECT totp_secret FROM users WHERE id=?1", [uid], |r| r.get(0))
        .unwrap();
    let (s, b) = c
        .raw(
            Method::POST,
            "/api/auth/password",
            Some(json!({"current":"temporary-pass-99","new":"replacement-pass-99","code":auth::current_totp(&secret).unwrap()})),
        )
        .await;
    ok(s, &b);
    ok(c.get("/api/cases").await.0, &Value::Null);
    assert_eq!(other.get("/api/auth/me").await.0, StatusCode::UNAUTHORIZED);
    let (s, b) = app
        .anon()
        .post("/api/auth/login", json!({"username":"demostaff","password":"temporary-pass-99"}))
        .await;
    err(s, &b, StatusCode::UNAUTHORIZED, "bad_credentials");
    assert_eq!(
        db.open()
            .unwrap()
            .query_row("SELECT totp_secret FROM users WHERE id=?1", [uid], |r| r.get::<_, String>(0))
            .unwrap(),
        secret
    );
}

#[tokio::test]
async fn f10_t34_encoded_pdf_action_is_quarantined() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO file checks").await;
    let (s, b) = c
        .upload(
            &format!("/api/cases/{cid}/documents"),
            &[
                ("title", "DEMO active PDF"),
                ("doc_type", "other"),
                ("visibility", "administrative"),
                ("source", "court"),
            ],
            "active.pdf",
            b"%PDF-1.4\n1 0 obj << /J#61vaScript (active) >> endobj\n%%EOF",
        )
        .await;
    ok(s, &b);
    assert_eq!(b["versions"][0]["scan_status"], "quarantined", "{b}");
}

#[test]
fn f16_t33_t36_cli_refuses_uninitialised_directory() {
    let dir = std::env::temp_dir().join(format!("tcr-cli-{}", auth::random_token()));
    let output = Command::new(env!("CARGO_BIN_EXE_tuvalu-court"))
        .args(["create-user", "demo", "DEMO Operator"])
        .env("TCR_DATA_DIR", &dir)
        .env("TCR_MODE", "production")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!dir.exists(), "maintenance command created {}", dir.display());
}

#[tokio::test]
async fn f09_t13_t35_production_without_transport_stays_queued() {
    let app = TestApp::production();
    let c = enrolled(&app).await;
    // This test is independent of the password gate, which is exercised above.
    let db = app.state.main_db.as_ref().unwrap();
    db.open().unwrap().execute("UPDATE users SET must_change_password=0", []).unwrap();
    let iid = new_intake(&c, "DEMO Recipient").await;
    let (s, b) = c
        .post(
            &format!("/api/intakes/{iid}/request-info"),
            json!({"missing_items":"DEMO attachment","recipient_name":"DEMO Recipient","method":"email","address":"demo@example.invalid"}),
        )
        .await;
    ok(s, &b);
    let id = b["dispatch_id"].as_i64().unwrap();
    let (s, b) = c.post(&format!("/api/dispatches/{id}/preview"), json!({})).await;
    ok(s, &b);
    let (s, b) = c
        .post_idem(&format!("/api/dispatches/{id}/queue"), "demo-mail-queue", json!({}))
        .await;
    ok(s, &b);
    outbox::process(db).unwrap();
    let conn = db.open().unwrap();
    assert_eq!(
        conn.query_row("SELECT status FROM dispatches WHERE id=?1", [id], |r| r.get::<_, String>(0))
            .unwrap(),
        "queued"
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM mailbox", [], |r| r.get::<_, i64>(0)).unwrap(),
        0
    );
}

fn configured(mut cfg: tuvalu_court::config::Config) -> TestApp {
    cfg.data_dir = std::env::temp_dir().join(format!("tcr-install-{}", auth::random_token()));
    let (state, _) = tuvalu_court::state::AppState::init(cfg.clone()).unwrap();
    TestApp {
        router: tuvalu_court::api::router(state.clone()),
        state,
        dir: cfg.data_dir,
    }
}

async fn seeded_production(cfg: tuvalu_court::config::Config) -> (TestApp, Client) {
    let app = configured(cfg);
    let db = app.state.main_db.as_ref().unwrap();
    seed::seed_demo(db).unwrap();
    let uid = db
        .open()
        .unwrap()
        .query_row("SELECT id FROM users WHERE persona='olga'", [], |r| r.get(0))
        .unwrap();
    let mut c = app.anon();
    c.session = Some(auth::create_session(&db.open().unwrap(), uid, true, 12).unwrap());
    (app, c)
}

async fn queue_notice(c: &Client, cid: i64, versions: Vec<i64>) -> i64 {
    let (s,b)=c.post(&format!("/api/cases/{cid}/dispatches"),json!({"kind":"notice","method":"email","recipient_name":"DEMO Recipient","address":"demo@example.invalid","subject":"DEMO SMTP test","body":"DEMO transport test","version_ids":versions})).await;
    ok(s, &b);
    let id = b["id"].as_i64().unwrap();
    let (s, b) = c.post(&format!("/api/dispatches/{id}/preview"), json!({})).await;
    ok(s, &b);
    let (s, b) = c
        .post_idem(&format!("/api/dispatches/{id}/queue"), &format!("mail-{id}"), json!({}))
        .await;
    ok(s, &b);
    id
}

async fn process(app: &TestApp) -> usize {
    let db = app.state.main_db.clone().unwrap();
    tokio::task::spawn_blocking(move || outbox::process(&db)).await.unwrap().unwrap()
}

async fn smtp_dialog<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(socket: S, banner: bool) -> Vec<u8> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut io = BufReader::new(socket);
    if banner {
        io.get_mut().write_all(b"220 localhost DEMO ESMTP\r\n").await.unwrap();
    }
    let mut message = Vec::new();
    loop {
        let mut line = String::new();
        if io.read_line(&mut line).await.unwrap_or(0) == 0 {
            break;
        }
        let response = if line.starts_with("EHLO") {
            "250-localhost\r\n250-AUTH PLAIN\r\n250 SIZE 20000000\r\n"
        } else if line.starts_with("AUTH PLAIN") {
            assert_eq!(line.trim_end(), format!("AUTH PLAIN {}", base64_test(b"\0demo\0demo@password")));
            "235 Authenticated DEMO\r\n"
        } else if line.starts_with("MAIL FROM") || line.starts_with("RCPT TO") {
            "250 OK\r\n"
        } else if line == "DATA\r\n" {
            io.get_mut().write_all(b"354 End with dot\r\n").await.unwrap();
            loop {
                let mut row = Vec::new();
                io.read_until(b'\n', &mut row).await.unwrap();
                if row == b".\r\n" {
                    break;
                }
                assert!(!row.is_empty());
                message.extend(row);
            }
            "250 queued DEMO\r\n"
        } else if line == "QUIT\r\n" {
            "221 Bye\r\n"
        } else {
            "250 OK\r\n"
        };
        if io.get_mut().write_all(response.as_bytes()).await.is_err() {
            break;
        }
    }
    message
}

async fn fake_smtp(
    starttls: bool,
    refuse_first: bool,
) -> (u16, std::sync::Arc<parking_lot::Mutex<Vec<Vec<u8>>>>, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio_rustls::{
        TlsAcceptor,
        rustls::{
            ServerConfig,
            pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
        },
    };
    let tls = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(include_bytes!("fixtures/smtp-cert.der").to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(include_bytes!("fixtures/smtp-key.der").to_vec())),
        )
        .unwrap();
    let acceptor = TlsAcceptor::from(std::sync::Arc::new(tls));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let messages = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let out = messages.clone();
    let handle = tokio::spawn(async move {
        let mut first = refuse_first;
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            if first {
                first = false;
                if starttls {
                    let _ = socket.write_all(b"421 DEMO temporarily unavailable\r\n").await;
                }
                drop(socket);
                continue;
            }
            if starttls {
                let mut io = BufReader::new(socket);
                io.get_mut().write_all(b"220 localhost DEMO ESMTP\r\n").await.unwrap();
                let mut line = String::new();
                io.read_line(&mut line).await.unwrap();
                assert!(line.starts_with("EHLO"));
                io.get_mut().write_all(b"250-localhost\r\n250 STARTTLS\r\n").await.unwrap();
                line.clear();
                io.read_line(&mut line).await.unwrap();
                assert_eq!(line, "STARTTLS\r\n");
                io.get_mut().write_all(b"220 Start TLS\r\n").await.unwrap();
                socket = io.into_inner();
            }
            let Ok(socket) = acceptor.accept(socket).await else {
                continue;
            };
            let message = smtp_dialog(socket, !starttls).await;
            if !message.is_empty() {
                out.lock().push(message);
            }
        }
    });
    (port, messages, handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f09_t13_t35_smtp_failure_retries_exact_versions_once() {
    for starttls in [true, false] {
        let (port, messages, server) = fake_smtp(starttls, true).await;
        let mut cfg = tuvalu_court::config::Config::for_tests(Default::default(), tuvalu_court::config::Mode::Production);
        cfg.smtp_url = Some(format!(
            "{}://demo:demo%40password@localhost:{port}",
            if starttls { "smtp" } else { "smtps" }
        ));
        cfg.mail_from = Some("DEMO Registry <registry@example.invalid>".into());
        cfg.smtp_ca_pem = Some(include_bytes!("fixtures/smtp-cert.pem").to_vec());
        let (app, c) = seeded_production(cfg).await;
        let (cid, _) = register_case(&c, "DEMO transport recovery").await;
        let (s, b) = c
            .upload(
                &format!("/api/cases/{cid}/documents"),
                &[
                    ("title", "DEMO queued version"),
                    ("doc_type", "other"),
                    ("visibility", "administrative"),
                    ("source", "court"),
                ],
                "queued.pdf",
                &pdf("DEMO original queued bytes"),
            )
            .await;
        ok(s, &b);
        let did = b["id"].as_i64().unwrap();
        let vid = b["versions"][0]["id"].as_i64().unwrap();
        let id = queue_notice(&c, cid, vec![vid]).await;
        // Later upload must never replace the queued attachment.
        let (s, b) = c
            .upload(
                &format!("/api/documents/{did}/versions"),
                &[("note", "DEMO replacement")],
                "new.pdf",
                &pdf("DEMO replacement bytes"),
            )
            .await;
        ok(s, &b);
        let db = app.state.main_db.as_ref().unwrap();
        let conn = db.open().unwrap();
        let counts: (i64, i64) = conn
            .query_row("SELECT (SELECT COUNT(*) FROM cases),(SELECT COUNT(*) FROM decisions)", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(process(&app).await, 1);
        let (s, b) = c.get(&format!("/api/dispatches/{id}")).await;
        ok(s, &b);
        assert_eq!(b["status"], "failed");
        assert!(b["failure_reason"].as_str().unwrap().contains("SMTP"));
        assert_eq!(process(&app).await, 0, "retry must respect backoff");
        conn.execute("UPDATE mail_retries SET retry_at='2000-01-01T00:00:00Z' WHERE dispatch_id=?1", [id])
            .unwrap();
        let (first, second) = tokio::join!(process(&app), process(&app));
        assert_eq!(first + second, 1);
        assert_eq!(process(&app).await, 0);
        for _ in 0..20 {
            if !messages.lock().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(messages.lock().len(), 1);
        let text = String::from_utf8(messages.lock()[0].clone()).unwrap();
        assert!(text.contains("queued.pdf"));
        assert!(!text.contains("new.pdf"));
        // The MIME payload contains the exact checksum-bound queued bytes (base64, wrapped at 76 columns).
        let expected = base64_test(&pdf("DEMO original queued bytes"));
        assert!(text.replace("\r\n", "").contains(&expected));
        let (_, b) = c.get(&format!("/api/dispatches/{id}")).await;
        assert_eq!(b["status"], "sent");
        assert_eq!(b["attempts"].as_array().unwrap().len(), 2);
        assert_eq!(b["confirmations"], json!([]));
        assert_eq!(b["assessments"], json!([]));
        assert_eq!(
            conn.query_row("SELECT (SELECT COUNT(*) FROM cases),(SELECT COUNT(*) FROM decisions)", [], |r| Ok(
                (r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)
            ))
            .unwrap(),
            counts
        );
        let (_, mail) = c.get(&format!("/api/mailbox?dispatch_id={id}")).await;
        assert_eq!(mail["items"][0]["transport"], "sent via SMTP");
        assert_eq!(mail["items"][0]["attachments"][0]["document_version_id"], vid);
        server.abort();
    }
}

fn base64_test(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let a = c[0] as u32;
        let b = *c.get(1).unwrap_or(&0) as u32;
        let d = *c.get(2).unwrap_or(&0) as u32;
        let n = (a << 16) | (b << 8) | d;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if c.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 { ALPHABET[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[tokio::test]
async fn f09_t35_demo_ignores_smtp_even_when_configured() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let env_app = TestApp::demo();
    let env_file = env_app.dir.join("demo.env");
    std::fs::write(
        &env_file,
        format!(
            "TCR_MODE=demo\nTCR_SMTP_URL=smtp://localhost:{}\nTCR_MAIL_FROM=demo@example.invalid\n",
            listener.local_addr().unwrap().port()
        ),
    )
    .unwrap();
    let cfg = tuvalu_court::config::Config::load(Some(&env_file)).unwrap();
    let app = configured(cfg);
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO never contacts SMTP").await;
    let id = queue_notice(&c, cid, vec![]).await;
    let db = c.db(&app);
    outbox::process(&db).unwrap();
    assert_eq!(c.get(&format!("/api/dispatches/{id}")).await.1["status"], "sent");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

fn png() -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let at = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc = !0u32;
        for &b in &out[at..] {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        out.extend_from_slice(&(!crc).to_be_bytes());
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
    chunk(&mut out, b"IDAT", &miniz_oxide::deflate::compress_to_vec_zlib(&[0, 255, 0, 0], 6));
    chunk(&mut out, b"IEND", &[]);
    out
}
fn jpeg() -> Vec<u8> {
    // A structural JPEG witness: SOI, baseline SOF, SOS, escaped entropy and EOI.
    vec![
        0xff, 0xd8, 0xff, 0xc0, 0, 11, 8, 0, 1, 0, 1, 1, 1, 0x11, 0, 0xff, 0xda, 0, 8, 1, 1, 0, 0, 63, 0, 42, 0xff, 0, 43, 0xff, 0xd9,
    ]
}
fn zip_files(entries: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(bytes).unwrap();
    }
    z.finish().unwrap().into_inner()
}

#[tokio::test]
async fn f10_t34_format_checks_validate_images_and_active_documents() {
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO formats").await;
    let ct = b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>";
    let doc = b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"/>";
    let docx = zip_files(&[("[Content_Types].xml", ct), ("word/document.xml", doc)]);
    for (name, data) in [
        ("valid.pdf", pdf("DEMO ordinary PDF")),
        ("valid.png", png()),
        ("valid.jpg", jpeg()),
        ("valid.docx", docx),
    ] {
        let (s, b) = c
            .upload(
                &format!("/api/cases/{cid}/documents"),
                &[
                    ("title", "DEMO valid format"),
                    ("doc_type", "other"),
                    ("visibility", "administrative"),
                    ("source", "court"),
                ],
                name,
                &data,
            )
            .await;
        ok(s, &b);
        assert_eq!(b["versions"][0]["scan_status"], "clean", "{name}: {b}");
        assert!(b["versions"][0]["scan_note"].as_str().unwrap().contains("no antivirus"));
    }
    let mut bad_crc = png();
    bad_crc[29] ^= 1;
    let mut object_stream = b"%PDF-1.5\n1 0 obj << /Type /ObjStm /Filter /FlateDecode >>\nstream\n".to_vec();
    object_stream.extend(miniz_oxide::deflate::compress_to_vec_zlib(b"<< /Open#41ction /SubmitForm >>", 6));
    object_stream.extend_from_slice(b"\nendstream\nendobj\n%%EOF");
    let ole = zip_files(&[
        ("[Content_Types].xml", ct),
        ("word/document.xml", doc),
        ("word/embeddings/oleObject1.bin", b"DEMO OLE"),
    ]);
    let macro_xml = zip_files(&[
        (
            "[Content_Types].xml",
            b"<Types><Override ContentType='application/vnd.ms-word.document.macroEnabled.main+xml'/></Types>",
        ),
        ("word/document.xml", doc),
    ]);
    let mut damaged_jpeg = jpeg();
    damaged_jpeg.truncate(damaged_jpeg.len() - 2);
    let mut dangerous = vec![
        ("bad-crc.png", bad_crc),
        ("truncated.png", b"\x89PNG\r\n\x1a\n".to_vec()),
        ("missing-end.jpg", damaged_jpeg),
        ("stream.pdf", object_stream),
        ("ole.docx", ole),
        ("macro.docx", macro_xml),
    ];
    for key in [
        "/JavaScript",
        "/JS",
        "/OpenAction",
        "/AA",
        "/Launch",
        "/EmbeddedFile",
        "/EmbeddedFiles",
        "/RichMedia",
        "/XFA",
        "/SubmitForm",
        "/ImportData",
    ] {
        dangerous.push(("active.pdf", format!("%PDF-1.4\n<< {key} >>\n%%EOF").into_bytes()));
    }
    for (name, data) in dangerous {
        let (s, b) = c
            .upload(
                &format!("/api/cases/{cid}/documents"),
                &[
                    ("title", "DEMO dangerous format"),
                    ("doc_type", "other"),
                    ("visibility", "administrative"),
                    ("source", "court"),
                ],
                name,
                &data,
            )
            .await;
        ok(s, &b);
        assert_eq!(b["versions"][0]["scan_status"], "quarantined", "{name}: {b}");
    }
}

async fn fake_clamd(verdict: &'static str) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("tcp://{}", listener.local_addr().unwrap());
    let job = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut command = [0; 10];
        socket.read_exact(&mut command).await.unwrap();
        assert_eq!(&command, b"zINSTREAM\0");
        let mut data = Vec::new();
        loop {
            let length = socket.read_u32().await.unwrap() as usize;
            if length == 0 {
                break;
            }
            let mut chunk = vec![0; length];
            socket.read_exact(&mut chunk).await.unwrap();
            data.extend(chunk);
        }
        match verdict {
            "drop" => {}
            "stall" => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
            _ => {
                socket.write_all(verdict.as_bytes()).await.unwrap();
            }
        }
        data
    });
    (endpoint, job)
}

#[tokio::test]
async fn f10_t34_clamd_clean_infected_error_timeout() {
    for (reply, expected) in [
        ("stream: OK\0", "clean"),
        ("stream: Eicar FOUND\0", "quarantined"),
        ("drop", "quarantined"),
        ("stall", "quarantined"),
    ] {
        let (endpoint, server) = fake_clamd(reply).await;
        let mut cfg = tuvalu_court::config::Config::for_tests(Default::default(), tuvalu_court::config::Mode::Production);
        cfg.clamd = Some(endpoint);
        cfg.scan_timeout_ms = 100;
        let (app, c) = seeded_production(cfg).await;
        let (cid, _) = register_case(&c, "DEMO scanner verdict").await;
        let bytes = pdf("DEMO scan input");
        let (s, b) = c
            .upload(
                &format!("/api/cases/{cid}/documents"),
                &[
                    ("title", "DEMO scanner input"),
                    ("doc_type", "other"),
                    ("visibility", "administrative"),
                    ("source", "court"),
                ],
                "scan.pdf",
                &bytes,
            )
            .await;
        ok(s, &b);
        assert_eq!(b["versions"][0]["scan_status"], expected, "{reply}: {b}");
        assert_eq!(server.await.unwrap(), bytes);
        let vid = b["versions"][0]["id"].as_i64().unwrap();
        assert_eq!(
            c.get_bytes(&format!("/api/document-versions/{vid}/download")).await.0,
            if expected == "clean" {
                StatusCode::OK
            } else {
                StatusCode::CONFLICT
            }
        );
        let conn = app.state.main_db.as_ref().unwrap().open().unwrap();
        let admin = conn
            .query_row("SELECT id FROM users WHERE persona='pavel'", [], |r| r.get(0))
            .unwrap();
        let mut a = c.clone();
        a.session = Some(auth::create_session(&conn, admin, true, 12).unwrap());
        let (_, b) = a.get("/api/admin/settings").await;
        assert!(b["file_scanner"].as_str().unwrap().contains("ClamAV"));
    }
}

#[tokio::test]
async fn f10_t34_pending_and_quarantine_block_all_file_channels() {
    let _lock = HEAVY.lock().await;
    let app = TestApp::demo();
    let c = app.persona("olga").await;
    let (cid, _) = register_case(&c, "DEMO blocked channels").await;
    let db = c.db(&app);
    let uid = user_id(&c, "Olga").await;
    for state in ["quarantined", "pending_scan"] {
        let (_, vid) = insert_document(&db, cid, "DEMO unavailable attachment", "evidence", "administrative", uid);
        let id = queue_notice(&c, cid, vec![vid]).await;
        outbox::process(&db).unwrap();
        let conn = db.open().unwrap();
        conn.execute(
            "UPDATE document_versions SET scan_status=?2,scan_note='DEMO unavailable' WHERE id=?1",
            rusqlite::params![vid, state],
        )
        .unwrap();
        for suffix in ["", "?inline=1"] {
            let (s, _, bytes) = c.get_bytes(&format!("/api/document-versions/{vid}/download{suffix}")).await;
            assert_eq!(s, StatusCode::CONFLICT);
            assert!(!bytes.starts_with(b"%PDF"));
        }
        let (s, _, bytes) = c.get_bytes(&format!("/api/document-versions/{vid}/download?inline=true")).await;
        assert!(!s.is_success());
        assert!(!bytes.starts_with(b"%PDF"));
        let (s, b) = c
            .post(
                &format!("/api/cases/{cid}/export"),
                json!({"purpose":"DEMO participant copy","version_ids":[vid]}),
            )
            .await;
        assert!(!s.is_success(), "{b}");
        let (_, mail) = c.get(&format!("/api/mailbox?dispatch_id={id}")).await;
        ok(c.get(&format!("/api/mailbox?dispatch_id={id}")).await.0, &mail);
        let link_vid = mail["items"][0]["attachments"][0]["document_version_id"].as_i64().unwrap();
        assert_eq!(link_vid, vid);
        assert_eq!(
            c.get_bytes(&format!("/api/document-versions/{link_vid}/download")).await.0,
            StatusCode::CONFLICT
        );
        let (s,b)=c.post(&format!("/api/cases/{cid}/dispatches"),json!({"kind":"notice","method":"email","recipient_name":"DEMO Recipient","address":"demo@example.invalid","subject":"DEMO blocked","body":"DEMO blocked","version_ids":[vid]})).await;
        assert!(!s.is_success(), "{b}");
        let (key, sha): (String, String) = conn
            .query_row("SELECT storage_key,sha256 FROM document_versions WHERE id=?1", [vid], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert!(tuvalu_court::storage::read(&db, &key, &sha).is_err());
    }
}

#[tokio::test]
async fn f10_t34_file_import_is_not_a_quarantine_bypass() {
    let _lock = HEAVY.lock().await;
    let (endpoint, server) = fake_clamd("stream: Eicar FOUND\0").await;
    let mut cfg = tuvalu_court::config::Config::for_tests(Default::default(), tuvalu_court::config::Mode::Production);
    cfg.clamd = Some(endpoint);
    let (app, olga) = seeded_production(cfg).await;
    let (cid, number) = register_case(&olga, "DEMO package import").await;
    let db = app.state.main_db.as_ref().unwrap();
    let conn = db.open().unwrap();
    let uid = conn
        .query_row("SELECT id FROM users WHERE persona='elena'", [], |r| r.get(0))
        .unwrap();
    let mut c = olga.clone();
    c.session = Some(auth::create_session(&conn, uid, true, 12).unwrap());
    let bytes = pdf("DEMO imported scanner witness");
    let manifest = format!(
        "case_number,filename,title,doc_type,visibility,document_date\n{number},import.pdf,DEMO imported file,evidence,party_material,2026-10-08\n"
    );
    let archive = zip_files(&[("manifest.csv", manifest.as_bytes()), ("import.pdf", &bytes)]);
    let (s, b) = c.upload("/api/import/files/preview", &[], "package.zip", &archive).await;
    ok(s, &b);
    let batch = b["batch_id"].as_i64().unwrap();
    let (s, b) = c
        .post_idem(&format!("/api/import/{batch}/commit"), "DEMO infected import", json!({}))
        .await;
    ok(s, &b);
    let vid = b["created"][0]["version_id"].as_i64().unwrap();
    assert_eq!(server.await.unwrap(), bytes);
    assert_eq!(
        c.get_bytes(&format!("/api/document-versions/{vid}/download")).await.0,
        StatusCode::CONFLICT
    );
    let (s, _) = c
        .post(
            &format!("/api/cases/{cid}/export"),
            json!({"purpose":"DEMO imported material","version_ids":[vid]}),
        )
        .await;
    assert!(!s.is_success());
}

#[test]
fn f10_t36_production_requires_scanner_or_explicit_av_off() {
    let dir = std::env::temp_dir().join(format!("tcr-av-{}", auth::random_token()));
    let out = Command::new(env!("CARGO_BIN_EXE_tuvalu-court"))
        .arg("serve")
        .env("TCR_MODE", "production")
        .env("TCR_DATA_DIR", &dir)
        .env_remove("TCR_AV")
        .env_remove("TCR_CLAMD")
        .env_remove("TCR_ENV_FILE")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("TCR_AV=off"));
    assert!(!dir.exists());
}

struct RunningServer(std::process::Child);
impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn cli(app: &TestApp, args: &[&str], password: Option<&str>) -> std::process::Output {
    use std::io::Write;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tuvalu-court"));
    cmd.current_dir(&app.dir)
        .env_remove("TCR_ENV_FILE")
        .env_remove("TCR_DATA_DIR")
        .env_remove("TCR_MODE");
    cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    if let Some(password) = password {
        child.stdin.take().unwrap().write_all(format!("{password}\n").as_bytes()).unwrap();
    } else {
        drop(child.stdin.take());
    }
    child.wait_with_output().unwrap()
}
fn success(out: &std::process::Output) {
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

async fn network_http(port: u16, method: &str, path: &str, body: Value, cookie: Option<&str>) -> (u16, Value, Option<String>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let data = body.to_string();
    let cookies = cookie.map(|c| format!("Cookie: tcr_session={c}\r\n")).unwrap_or_default();
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nX-TCR: 1\r\n{cookies}Content-Length: {}\r\n\r\n{data}",data.len()).as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.unwrap();
    let response = String::from_utf8(bytes).unwrap();
    let (headers, body) = response.split_once("\r\n\r\n").unwrap();
    let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
    let cookie = headers.lines().find_map(|line| {
        line.strip_prefix("set-cookie: tcr_session=")
            .or_else(|| line.strip_prefix("Set-Cookie: tcr_session="))
            .map(|v| v.split(';').next().unwrap().to_string())
    });
    (status, serde_json::from_str(body).unwrap(), cookie)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f16_t33_t36_env_file_cli_backup_restore_and_real_server_login() {
    let cfg = tuvalu_court::config::Config::for_tests(Default::default(), tuvalu_court::config::Mode::Production);
    let (app, c) = seeded_production(cfg).await;
    let (cid, _) = register_case(&c, "DEMO installation roundtrip").await;
    let db = app.state.main_db.as_ref().unwrap();
    let conn = db.open().unwrap();
    let uid = conn
        .query_row("SELECT id FROM users WHERE persona='olga'", [], |r| r.get(0))
        .unwrap();
    let (did, vid) = insert_document(db, cid, "DEMO backup original", "evidence", "restricted", uid);
    conn.execute(
        "INSERT INTO document_grants(document_id,user_id,granted_by,reason,granted_at) VALUES(?1,?2,?2,'DEMO explicit grant',?3)",
        rusqlite::params![did, uid, tuvalu_court::time::now_utc()],
    )
    .unwrap();
    let (s, b) = c
        .upload(
            &format!("/api/documents/{did}/versions"),
            &[("note", "DEMO corrected backup material")],
            "second.pdf",
            &pdf("DEMO second version"),
        )
        .await;
    ok(s, &b);
    let config = app.dir.join("server.env");
    std::fs::write(
        &config,
        format!(
            "TCR_MODE=production\nTCR_DATA_DIR='{}'\nTCR_AV=off\nTCR_BIND=127.0.0.1:0\n",
            app.dir.display()
        ),
    )
    .unwrap();
    let env = config.to_str().unwrap();
    let out = cli(
        &app,
        &[
            "--env-file",
            env,
            "create-user",
            "demooperator",
            "DEMO Operator",
            "--perm",
            "case.view_all",
        ],
        Some("DEMO operator password"),
    );
    success(&out);
    let id = tuvalu_court::db::setting(&conn, "installation_id", "").unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(&id));
    assert!(text.contains(app.dir.to_str().unwrap()));
    assert!(!app.dir.join("data").exists());
    assert_eq!(
        conn.query_row("SELECT must_change_password FROM users WHERE username='demooperator'", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    for (cmd, perm) in [("grant", "report.view"), ("revoke", "report.view")] {
        success(&cli(&app, &[cmd, "demooperator", perm, "--env-file", env], None));
    }
    let key = app.dir.join("backup.key");
    let packed = app.dir.join("roundtrip.tcrb");
    success(&cli(&app, &["gen-key", key.to_str().unwrap()], None));
    success(&cli(
        &app,
        &["--env-file", env, "backup", packed.to_str().unwrap(), key.to_str().unwrap()],
        None,
    ));
    let restored = app.dir.join("restored");
    let target = restored.to_str().unwrap();
    let refusal = cli(
        &app,
        &[
            "--env-file",
            env,
            "restore",
            packed.to_str().unwrap(),
            key.to_str().unwrap(),
            target,
        ],
        None,
    );
    assert!(!refusal.status.success());
    assert!(!restored.exists());
    success(&cli(
        &app,
        &[
            "--env-file",
            env,
            "restore",
            packed.to_str().unwrap(),
            key.to_str().unwrap(),
            target,
            "--yes",
        ],
        None,
    ));
    let restored_db = tuvalu_court::db::Db::new(restored.join("court.sqlite"), restored.join("files"), None);
    let restored_conn = restored_db.open().unwrap();
    assert_eq!(tuvalu_court::db::setting(&restored_conn, "installation_id", "").unwrap(), id);
    for table in [
        "cases",
        "documents",
        "document_versions",
        "document_grants",
        "audit_events",
        "user_permissions",
        "users",
    ] {
        let a: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).unwrap();
        let b: i64 = restored_conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(a, b, "{table}");
    }
    let mut stmt = conn
        .prepare("SELECT storage_key,sha256 FROM document_versions ORDER BY id")
        .unwrap();
    for row in stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
    {
        let (key, sha) = row.unwrap();
        assert_eq!(
            tuvalu_court::storage::read(db, &key, &sha).unwrap(),
            tuvalu_court::storage::read(&restored_db, &key, &sha).unwrap()
        );
    }
    assert_eq!(
        tuvalu_court::audit::verify_chain(&restored_conn).unwrap(),
        tuvalu_court::audit::verify_chain(&conn).unwrap()
    );
    assert_eq!(tuvalu_court::audit::verify_chain(&restored_conn).unwrap().1, None);
    assert_eq!(
        restored_conn
            .query_row("SELECT version_no FROM document_versions WHERE id=?1", [vid], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let restore_env = app.dir.join("restored.env");
    std::fs::write(
        &restore_env,
        format!(
            "TCR_MODE=production\nTCR_DATA_DIR={}\nTCR_AV=off\nTCR_BIND=127.0.0.1:{port}\n",
            restored.display()
        ),
    )
    .unwrap();
    let mut audit_cmd = Command::new(env!("CARGO_BIN_EXE_tuvalu-court"));
    let verify = audit_cmd
        .arg("verify-audit")
        .env("TCR_ENV_FILE", &restore_env)
        .env("TCR_DATA_DIR", app.dir.join("WRONG"))
        .output()
        .unwrap();
    success(&verify);
    assert!(String::from_utf8_lossy(&verify.stdout).contains("Audit chain intact"));
    assert!(!app.dir.join("WRONG").exists());
    let child = Command::new(env!("CARGO_BIN_EXE_tuvalu-court"))
        .args(["--env-file", restore_env.to_str().unwrap(), "serve"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut server = RunningServer(child);
    let mut ready = false;
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            ready = true;
            break;
        }
        assert!(server.0.try_wait().unwrap().is_none(), "server exited");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(ready);
    let (status, b, cookie) = network_http(
        port,
        "POST",
        "/api/auth/login",
        json!({"username":"demooperator","password":"DEMO operator password"}),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(b["enroll_required"], true);
    let cookie = cookie.unwrap();
    let (s, b, _) = network_http(port, "POST", "/api/auth/totp/setup", json!({}), Some(&cookie)).await;
    assert_eq!(s, 200);
    let secret = b["secret"].as_str().unwrap();
    let (s, b, new_cookie) = network_http(
        port,
        "POST",
        "/api/auth/totp/enable",
        json!({"code":auth::current_totp(secret).unwrap()}),
        Some(&cookie),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(b["must_change_password"], true);
    let cookie = new_cookie.unwrap();
    assert_eq!(network_http(port, "GET", "/api/cases", json!({}), Some(&cookie)).await.0, 403);
    let step = tuvalu_court::time::now().unix_timestamp() as u64 / 30 + 1;
    let code = format!("{:06}", auth::totp_at(&auth::base32_decode(secret).unwrap(), step));
    let (s, _, new_cookie) = network_http(
        port,
        "POST",
        "/api/auth/password",
        json!({"current":"DEMO operator password","new":"DEMO restored password","code":code}),
        Some(&cookie),
    )
    .await;
    assert_eq!(s, 200);
    let (s, b, _) = network_http(port, "GET", &format!("/api/cases/{cid}"), json!({}), new_cookie.as_deref()).await;
    assert_eq!(s, 200);
    assert_eq!(b["case"]["title"], "DEMO installation roundtrip");
}

#[tokio::test]
async fn f10_t34_upload_is_pending_until_clamd_verdict() {
    let _lock = HEAVY.lock().await;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = tuvalu_court::config::Config::for_tests(Default::default(), tuvalu_court::config::Mode::Production);
    cfg.clamd = Some(format!("tcp://{}", listener.local_addr().unwrap()));
    cfg.scan_timeout_ms = 2000;
    let (app, c) = seeded_production(cfg).await;
    let (cid, _) = register_case(&c, "DEMO pending upload").await;
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let scanner = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut command = [0; 10];
        socket.read_exact(&mut command).await.unwrap();
        loop {
            let n = socket.read_u32().await.unwrap();
            if n == 0 {
                break;
            }
            let mut data = vec![0; n as usize];
            socket.read_exact(&mut data).await.unwrap();
        }
        seen_tx.send(()).unwrap();
        release_rx.await.unwrap();
        socket.write_all(b"stream: OK\0").await.unwrap();
    });
    let upload_client = c.clone();
    let upload = tokio::spawn(async move {
        upload_client
            .upload(
                &format!("/api/cases/{cid}/documents"),
                &[
                    ("title", "DEMO awaiting scan"),
                    ("doc_type", "other"),
                    ("source", "court"),
                    ("visibility", "administrative"),
                ],
                "pending.pdf",
                &pdf("DEMO pending input"),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), seen_rx)
        .await
        .unwrap()
        .unwrap();
    let conn = app.state.main_db.as_ref().unwrap().open().unwrap();
    let vid: i64 = conn
        .query_row("SELECT id FROM document_versions WHERE scan_status='pending_scan'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        c.get_bytes(&format!("/api/document-versions/{vid}/download?inline=1")).await.0,
        StatusCode::CONFLICT
    );
    assert!(
        !c.post(
            &format!("/api/cases/{cid}/export"),
            json!({"purpose":"DEMO pending","version_ids":[vid]})
        )
        .await
        .0
        .is_success()
    );
    release_tx.send(()).unwrap();
    scanner.await.unwrap();
    let (s, b) = upload.await.unwrap();
    ok(s, &b);
    assert_eq!(b["versions"][0]["scan_status"], "clean");
    assert_eq!(
        c.get_bytes(&format!("/api/document-versions/{vid}/download")).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn f19_t34_t36_own_password_requires_totp_and_gate_covers_api_routes() {
    let app = TestApp::production();
    let mut c = enrolled(&app).await;
    for path in [
        "/api/cases",
        "/api/queue",
        "/api/ref",
        "/api/admin/settings",
        "/api/auth/mode",
        "/api/health",
        "/api/demo/personas",
    ] {
        let (s, b) = c.get(path).await;
        err(s, &b, StatusCode::FORBIDDEN, "password_change_required");
    }
    let (s, b) = c.post("/api/intakes", json!({})).await;
    err(s, &b, StatusCode::FORBIDDEN, "password_change_required");
    let (s, b) = c.post("/api/auth/totp/setup", json!({})).await;
    err(s, &b, StatusCode::CONFLICT, "already_enrolled");
    let (s, b) = c
        .post("/api/auth/password", json!({"current":"temporary-pass-99","new":"new-password-99"}))
        .await;
    err(s, &b, StatusCode::UNAUTHORIZED, "bad_code");
    let db = app.state.main_db.as_ref().unwrap();
    let conn = db.open().unwrap();
    let (uid, secret): (i64, String) = conn
        .query_row("SELECT id,totp_secret FROM users WHERE username='demostaff'", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(
        conn.query_row("SELECT must_change_password FROM users WHERE id=?1", [uid], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    let step = tuvalu_court::time::now().unix_timestamp() as u64 / 30 + 1;
    let code = format!("{:06}", auth::totp_at(&auth::base32_decode(&secret).unwrap(), step));
    let (s, b) = c
        .raw(
            Method::POST,
            "/api/auth/password",
            Some(json!({"current":"temporary-pass-99","new":"new-password-99","code":code})),
        )
        .await;
    ok(s, &b);
    assert_eq!(c.get("/api/auth/me").await.1["must_change_password"], false);
    // A normal Settings change is also protected by a fresh code, even after the gate clears.
    let (s, b) = c
        .post(
            "/api/auth/password",
            json!({"current":"new-password-99","new":"second-password-99"}),
        )
        .await;
    err(s, &b, StatusCode::UNAUTHORIZED, "bad_code");
    let (s, b) = c
        .post("/api/auth/password", json!({"current":"new-password-99","new":"short","code":code}))
        .await;
    err(s, &b, StatusCode::BAD_REQUEST, "validation");
    let (s, b) = c
        .post(
            "/api/auth/password",
            json!({"current":"wrong-password-99","new":"second-password-99","code":code}),
        )
        .await;
    err(s, &b, StatusCode::UNAUTHORIZED, "bad_credentials");
    let (s, b) = c
        .post(
            "/api/auth/password",
            json!({"current":"new-password-99","new":"second-password-99","code":code}),
        )
        .await;
    err(s, &b, StatusCode::UNAUTHORIZED, "bad_code");
    // Establish a distinct accepted step without waiting on the wall clock.
    conn.execute("UPDATE users SET totp_last_step=NULL WHERE id=?1", [uid]).unwrap();
    let (s, b) = c
        .raw(
            Method::POST,
            "/api/auth/password",
            Some(json!({"current":"new-password-99","new":"second-password-99","code":auth::current_totp(&secret).unwrap()})),
        )
        .await;
    ok(s, &b);
    assert_eq!(c.get("/api/cases").await.0, StatusCode::OK);
    let (s, b) = app
        .anon()
        .post("/api/auth/login", json!({"username":"demostaff","password":"new-password-99"}))
        .await;
    err(s, &b, StatusCode::UNAUTHORIZED, "bad_credentials");
}

#[tokio::test]
async fn f10_t34_unix_clamd_and_interrupted_scan_fail_closed() {
    #[cfg(unix)]
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let app = TestApp::production();
        let socket_path = app.dir.join("clamd.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let endpoint = format!("unix:{}", socket_path.display());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut cmd = [0; 10];
            socket.read_exact(&mut cmd).await.unwrap();
            loop {
                let n = socket.read_u32().await.unwrap();
                if n == 0 {
                    break;
                }
                let mut b = vec![0; n as usize];
                socket.read_exact(&mut b).await.unwrap();
            }
            socket.write_all(b"stream: OK\0").await.unwrap();
        });
        assert!(tuvalu_court::scan::check(&endpoint, b"DEMO stream", 1000).await.is_ok());
        server.await.unwrap();
        let db = app.state.main_db.as_ref().unwrap();
        seed::seed_demo(db).unwrap();
        let conn = db.open().unwrap();
        let (cid, uid): (i64, i64) = conn
            .query_row(
                "SELECT id,(SELECT id FROM users WHERE persona='olga') FROM cases ORDER BY id LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let (_, vid) = insert_document(db, cid, "DEMO interrupted scan", "evidence", "administrative", uid);
        conn.execute("UPDATE document_versions SET scan_status='pending_scan' WHERE id=?1", [vid])
            .unwrap();
        tuvalu_court::scan::recover_pending(db).unwrap();
        assert_eq!(
            conn.query_row("SELECT scan_status FROM document_versions WHERE id=?1", [vid], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "quarantined"
        );
        assert_eq!(tuvalu_court::audit::verify_chain(&conn).unwrap().1, None);
    }
}

#[test]
fn f16_t36_all_maintenance_commands_refuse_missing_source() {
    let app = TestApp::demo();
    let missing = app.dir.join("absent");
    let env = app.dir.join("missing.env");
    std::fs::write(
        &env,
        format!("TCR_MODE=production\nTCR_DATA_DIR={}\nTCR_AV=off\n", missing.display()),
    )
    .unwrap();
    for args in [
        vec!["grant", "demostaff", "case.view_all"],
        vec!["revoke", "demostaff", "case.view_all"],
        vec!["backup", "out.tcrb", "backup.key"],
        vec!["verify-audit"],
    ] {
        let mut args = args;
        args.extend(["--env-file", env.to_str().unwrap()]);
        let out = cli(&app, &args, None);
        assert!(!out.status.success());
        assert!(!missing.exists());
        assert!(!app.dir.join("out.tcrb").exists());
    }
    let out = cli(
        &app,
        &[
            "--env-file",
            env.to_str().unwrap(),
            "restore",
            "missing.tcrb",
            "missing.key",
            "restore",
            "--yes",
        ],
        None,
    );
    assert!(!out.status.success());
    assert!(!app.dir.join("restore").exists());
    assert!(!missing.exists());
}

#[test]
fn f10_f16_t33_t34_upgrade_preserves_old_versions_and_legacy_backups() {
    let app = TestApp::demo();
    let db = legacy_v6_db(&app.dir);
    let conn = db.open().unwrap();
    let versions: i64 = conn.query_row("SELECT COUNT(*) FROM document_versions", [], |r| r.get(0)).unwrap();
    let decisions: i64 = conn.query_row("SELECT COUNT(*) FROM decisions", [], |r| r.get(0)).unwrap();
    let key = app.dir.join("old.key");
    let backup = app.dir.join("old.tcrb");
    tuvalu_court::backup::gen_key(&key).unwrap();
    tuvalu_court::backup::backup(&db, &backup, &key).unwrap();
    assert!(tuvalu_court::backup::installation_id(&backup, &key).unwrap().starts_with("legacy-"));
    db.init().unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM document_versions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        versions
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM decisions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        decisions
    );
    assert!(!conn.prepare("PRAGMA foreign_key_check").unwrap().exists([]).unwrap());
    assert_eq!(tuvalu_court::db::setting(&conn, "installation_id", "").unwrap().len(), 32);
    assert_eq!(tuvalu_court::audit::verify_chain(&conn).unwrap().1, None);
}
