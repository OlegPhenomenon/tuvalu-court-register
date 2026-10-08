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
    assert!(tuvalu_court::backup::installation_id(&backup, &key, &app.dir.join("identity-stage")).unwrap().starts_with("legacy-"));
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

#[tokio::test]
async fn f19_reload_before_totp_can_restart_login() {
    let app = TestApp::production();
    let _enrolled = enrolled(&app).await;
    let db = app.state.main_db.as_ref().unwrap();
    let conn = db.open().unwrap();
    conn.execute("UPDATE users SET totp_last_step=NULL", [])
        .unwrap();
    let secret: String = conn
        .query_row(
            "SELECT totp_secret FROM users WHERE username='demostaff'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut c = app.anon();
    let login = json!({"username":"demostaff","password":"temporary-pass-99"});
    let (s, b) = c
        .raw(Method::POST, "/api/auth/login", Some(login.clone()))
        .await;
    ok(s, &b);
    err(
        c.get("/api/auth/me").await.0,
        &c.get("/api/auth/me").await.1,
        StatusCode::UNAUTHORIZED,
        "mfa_required",
    );
    let (s, b) = c.get("/api/auth/mode").await;
    ok(s, &b);
    let (s, b) = c.raw(Method::POST, "/api/auth/login", Some(login)).await;
    ok(s, &b);
    assert!(b["mfa_required"].as_bool().unwrap());
    let (s, b) = c
        .raw(
            Method::POST,
            "/api/auth/totp",
            Some(json!({"code":auth::current_totp(&secret).unwrap()})),
        )
        .await;
    ok(s, &b);
    let (s, b) = c.get("/api/cases").await;
    err(s, &b, StatusCode::FORBIDDEN, "password_change_required");
    ok(c.get("/api/auth/mode").await.0, &Value::Null);
}

#[test]
fn f10_pdf_dictionary_tokenizer_evasions() {
    let app = TestApp::production();
    let db = app.state.main_db.as_ref().unwrap();
    let active = miniz_oxide::deflate::compress_to_vec_zlib(
        &b"1 0 <</Type/Catalog/OpenAction<</S/JavaScript/J#53(evil)>> >>".repeat(20),
        6,
    );
    for extra in [
        "%endobj\n",
        "/Note(endobj)",
        "/Note(stream\\(nested\\))",
        "/Meta<</Note(endobj)>>",
        "/Note<656e646f626a>",
    ] {
        let mut bytes = format!("%PDF-1.5\n1 0 obj <</Type/ObjStm/N 1/First 4/Filter/FlateDecode{extra}/Length {}>>stream\n",active.len()).into_bytes();
        bytes.extend(&active);
        bytes.extend(b"\nendstream\nendobj\n%%EOF");
        let stored = tuvalu_court::storage::store(db, &bytes, "active.pdf", 1_000_000).unwrap();
        assert_eq!(
            stored.scan_status, "quarantined",
            "evasion: {extra}; {:?}",
            stored.scan_note
        );
    }
    for bytes in [
        &b"%PDF-1.5\n1 0 obj <</Note(unterminated>> endobj\n%%EOF"[..],
        &b"%PDF-1.5\n1 0 obj <</Length 4/Filter/Unknown>>stream\nabcd\nendstream\nendobj\n%%EOF"[..],
        &b"%PDF-1.5\n1 0 obj <</Type/ObjStm/Filter/Unknown/Length 4>>stream\nabcd\nendstream\nendobj\n%%EOF"[..],
    ] {
        assert_eq!(tuvalu_court::storage::store(db,bytes,"bad.pdf",1_000_000).unwrap().scan_status,"quarantined");
    }
}

#[test]
fn f16_kamal_commands_route_through_image_entrypoint() {
    let docs = include_str!("../docs/OPERATIONS.md");
    assert!(include_str!("../Dockerfile").contains("ENTRYPOINT [\"/usr/local/bin/tuvalu-court\"]"));
    for line in docs.lines().filter(|l| l.starts_with("kamal app exec")) {
        assert!(
            !line.contains("'/usr/local/bin/tuvalu-court"),
            "new-container exec repeats ENTRYPOINT: {line}"
        );
        let command = line
            .split('\'')
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        assert!(
            [
                "create-user",
                "grant",
                "revoke",
                "gen-key",
                "backup",
                "restore",
                "verify-audit"
            ]
            .contains(&command)
        );
    }
}

#[test]
fn f16_restore_confirmation_uses_destination_disk_and_cleans_up() {
    let app = TestApp::production();
    let db = app.state.main_db.as_ref().unwrap();
    let key = app.dir.join("restore.key");
    let archive = app.dir.join("restore.tcrb");
    let target = app.dir.join("restored");
    tuvalu_court::backup::gen_key(&key).unwrap();
    tuvalu_court::backup::backup(db, &archive, &key).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tuvalu-court"))
        .args([
            "restore",
            archive.to_str().unwrap(),
            key.to_str().unwrap(),
            target.to_str().unwrap(),
            "--yes",
        ])
        .env("TCR_MODE", "production")
        .env("TCR_AV", "off")
        .env_remove("TCR_ENV_FILE")
        .env("TMPDIR", app.dir.join("unavailable-system-temp"))
        .output()
        .unwrap();
    success(&output);
    assert!(target.join("court.sqlite").is_file());
    assert!(!target.join(".restore-tmp").exists());
    assert!(!app.dir.join("unavailable-system-temp").exists());
    let failed = app.dir.join("failed-restore");
    let mut damaged = std::fs::read(&archive).unwrap();
    *damaged.last_mut().unwrap() ^= 1;
    let bad = app.dir.join("damaged.tcrb");
    std::fs::write(&bad, damaged).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tuvalu-court"))
        .args([
            "restore",
            bad.to_str().unwrap(),
            key.to_str().unwrap(),
            failed.to_str().unwrap(),
            "--yes",
        ])
        .env("TCR_MODE", "production")
        .env("TCR_AV", "off")
        .env_remove("TCR_ENV_FILE")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!failed.join(".restore-tmp").exists());
    assert!(!failed.join("court.sqlite").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f09_stalled_smtp_does_not_block_http_writes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = tuvalu_court::config::Config::for_tests(
        Default::default(),
        tuvalu_court::config::Mode::Production,
    );
    cfg.smtp_url = Some(format!(
        "smtp://localhost:{}",
        listener.local_addr().unwrap().port()
    ));
    cfg.mail_from = Some("registry@example.invalid".into());
    cfg.smtp_timeout_secs = 3;
    let (app, c) = seeded_production(cfg).await;
    let (cid, _) = register_case(&c, "DEMO stalled mail").await;
    let id = queue_notice(&c, cid, vec![]).await;
    let db = app.state.main_db.clone().unwrap();
    let job = tokio::task::spawn_blocking(move || outbox::process(&db));
    let (socket, _) = listener.accept().await.unwrap();
    let conn = app.state.main_db.as_ref().unwrap().open().unwrap();
    let (status, inventory, message_id): (String, String, String) = conn
        .query_row(
            "SELECT status,attachments_json,message_id FROM delivery_attempts WHERE dispatch_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, "in_flight");
    assert_eq!(inventory, "[]");
    assert!(message_id.starts_with(&format!("<dispatch-{id}-")));
    assert_eq!(process(&app).await, 0, "another worker stole a live claim");
    let write = tokio::time::timeout(std::time::Duration::from_millis(800), c.post("/api/intakes",json!({"sender_name":"DEMO Concurrent","channel":"counter","received_date":today(),"description":"DEMO writer during SMTP"}))).await;
    let result = job.await.unwrap().unwrap();
    drop(socket);
    assert_eq!(result, 1);
    let (s, b) = write.expect("HTTP write blocked behind SMTP transaction");
    ok(s, &b);
    assert_eq!(
        c.get(&format!("/api/dispatches/{id}")).await.1["status"],
        "failed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f09_expired_claim_recovers_once_with_same_message_id() {
    let (port, messages, server) = fake_smtp(false, false).await;
    let mut cfg = tuvalu_court::config::Config::for_tests(
        Default::default(),
        tuvalu_court::config::Mode::Production,
    );
    cfg.smtp_url = Some(format!("smtps://demo:demo%40password@localhost:{port}"));
    cfg.mail_from = Some("registry@example.invalid".into());
    cfg.smtp_ca_pem = Some(include_bytes!("fixtures/smtp-cert.pem").to_vec());
    let (app, c) = seeded_production(cfg).await;
    let (cid, _) = register_case(&c, "DEMO recovered claim").await;
    let id = queue_notice(&c, cid, vec![]).await;
    let db = app.state.main_db.as_ref().unwrap();
    let conn = db.open().unwrap();
    let message_id = format!("<dispatch-{id}-DEMO-original-identity@tuvalu-court.invalid>");
    conn.execute("INSERT INTO delivery_attempts(dispatch_id,attempt_no,status,at,claim,message_id,attachments_json,dispatch_version)
        SELECT id,1,'in_flight','2000-01-01T00:00:00Z','DEMO crashed claim',?2,'[]',version FROM dispatches WHERE id=?1",
        rusqlite::params![id,message_id]).unwrap();
    assert_eq!(
        process(&app).await,
        0,
        "recovery must respect retry backoff"
    );
    let (_, b) = c.get(&format!("/api/dispatches/{id}")).await;
    assert_eq!(b["status"], "failed");
    assert_eq!(b["attempts"][0]["status"], "failed");
    assert!(
        b["attempts"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("ambiguous")
    );
    conn.execute(
        "UPDATE mail_retries SET retry_at='2000-01-01T00:00:00Z' WHERE dispatch_id=?1",
        [id],
    )
    .unwrap();
    let (first, second) = tokio::join!(process(&app), process(&app));
    assert_eq!(first + second, 1);
    assert_eq!(process(&app).await, 0);
    let ids: Vec<String> = conn
        .prepare(
            "SELECT message_id FROM delivery_attempts WHERE dispatch_id=?1 ORDER BY attempt_no",
        )
        .unwrap()
        .query_map([id], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(ids, vec![message_id.clone(), message_id.clone()]);
    for _ in 0..20 {
        if !messages.lock().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(messages.lock().len(), 1);
    assert!(String::from_utf8_lossy(&messages.lock()[0]).contains(&message_id));
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM mailbox WHERE dispatch_id=?1",
            [id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(conn.execute("UPDATE delivery_attempts SET status='sent' WHERE claim='DEMO crashed claim' AND status='in_flight'",[]).unwrap(),0);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f09_corrupt_attachment_fails_only_its_dispatch() {
    for corrupt in [false, true] {
        let (port, messages, server) = fake_smtp(false, false).await;
        let mut cfg = tuvalu_court::config::Config::for_tests(
            Default::default(),
            tuvalu_court::config::Mode::Production,
        );
        cfg.smtp_url = Some(format!("smtps://demo:demo%40password@localhost:{port}"));
        cfg.mail_from = Some("registry@example.invalid".into());
        cfg.smtp_ca_pem = Some(include_bytes!("fixtures/smtp-cert.pem").to_vec());
        let (app, c) = seeded_production(cfg).await;
        let (cid, _) = register_case(&c, "DEMO broken attachment").await;
        let db = app.state.main_db.as_ref().unwrap();
        let uid: i64 = db
            .open()
            .unwrap()
            .query_row("SELECT id FROM users WHERE persona='olga'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let (_, vid) = insert_document(
            db,
            cid,
            "DEMO missing bytes",
            "evidence",
            "administrative",
            uid,
        );
        let bad = queue_notice(&c, cid, vec![vid]).await;
        let good = queue_notice(&c, cid, vec![]).await;
        let conn = db.open().unwrap();
        let key: String = conn
            .query_row(
                "SELECT storage_key FROM document_versions WHERE id=?1",
                [vid],
                |r| r.get(0),
            )
            .unwrap();
        let path = db.files_dir().join(&key);
        if corrupt {
            std::fs::write(path, b"DEMO corrupt").unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
        let work = db.clone();
        let result = tokio::task::spawn_blocking(move || outbox::process(&work))
            .await
            .unwrap();
        assert!(result.is_ok(), "attachment aborted whole batch: {result:?}");
        assert_eq!(result.unwrap(), 2);
        let (_, b) = c.get(&format!("/api/dispatches/{bad}")).await;
        assert_eq!(b["status"], "failed");
        assert!(b["failure_reason"].as_str().unwrap().contains("Attachment"));
        assert!(b["reviewed_at"].is_null(), "attachment failure must require another preview");
        let (s,b)=c.post(&format!("/api/dispatches/{bad}/retry"),json!({})).await;
        err(s,&b,StatusCode::CONFLICT,"review_required");
        assert_eq!(
            c.get(&format!("/api/dispatches/{good}")).await.1["status"],
            "sent"
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM mail_retries WHERE dispatch_id=?1",
                [bad],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(process(&app).await, 0);
        for _ in 0..20 {
            if !messages.lock().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(messages.lock().len(), 1);
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn f10_import_scan_does_not_block_http_writes() {
    use tokio::io::AsyncWriteExt;
    let _lock = HEAVY.lock().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = tuvalu_court::config::Config::for_tests(
        Default::default(),
        tuvalu_court::config::Mode::Production,
    );
    cfg.clamd = Some(format!("tcp://{}", listener.local_addr().unwrap()));
    cfg.scan_timeout_ms = 3000;
    let (app, olga) = seeded_production(cfg).await;
    let (_, number) = register_case(&olga, "DEMO import contention").await;
    let db = app.state.main_db.as_ref().unwrap();
    let uid: i64 = db
        .open()
        .unwrap()
        .query_row("SELECT id FROM users WHERE persona='elena'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let mut c = olga.clone();
    c.session = Some(auth::create_session(&db.open().unwrap(), uid, true, 12).unwrap());
    let manifest = format!(
        "case_number,filename,title,doc_type,visibility,document_date\n{number},import.pdf,DEMO import,evidence,party_material,2026-10-08\n"
    );
    let archive = zip_files(&[
        ("manifest.csv", manifest.as_bytes()),
        ("import.pdf", &pdf("DEMO imported bytes")),
    ]);
    let (s, b) = c
        .upload("/api/import/files/preview", &[], "package.zip", &archive)
        .await;
    ok(s, &b);
    let batch = b["batch_id"].as_i64().unwrap();
    let importer = c.clone();
    let job = tokio::spawn(async move {
        importer
            .post_idem(
                &format!("/api/import/{batch}/commit"),
                "DEMO stalled import",
                json!({}),
            )
            .await
    });
    let (mut socket, _) = listener.accept().await.unwrap();
    let write=tokio::time::timeout(std::time::Duration::from_millis(800),olga.post("/api/intakes",json!({"sender_name":"DEMO Concurrent","channel":"counter","received_date":today(),"description":"DEMO writer during scan"}))).await;
    // Release clamd before asserting so the original request and blocking pool always finish.
    socket.write_all(b"stream: OK\0").await.unwrap();
    let (s, b) = job.await.unwrap();
    ok(s, &b);
    let (s, b) = write.expect("HTTP write blocked behind import scanner transaction");
    ok(s, &b);
}

fn r3_stream(dict: &str, payload: &[u8]) -> Vec<u8> {
    let mut bytes = format!("%PDF-1.5\n1 0 obj <</Type/Pages/Count 0/Kids[]>> endobj\n2 0 obj <<{dict}/Length {}>>stream\n", payload.len()).into_bytes();
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n%%EOF");
    bytes
}

#[test]
fn r3_pdf_realistic_corpus_is_clean() {
    let app = TestApp::production();
    let db = app.state.main_db.as_ref().unwrap();
    for name in [
        "cups-text.pdf",
        "cups-rtf-export.pdf",
        "quartz-truetype.pdf",
        "quartz-flate-image.pdf",
    ] {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/pdf")
                .join(name),
        )
        .unwrap();
        assert!(bytes.len() <= 60 * 1024);
        let stored = tuvalu_court::storage::store(db, &bytes, name, 1_000_000).unwrap();
        assert_eq!(
            stored.scan_status, "clean",
            "{name}: {:?}",
            stored.scan_note
        );
    }
}

#[test]
fn r3_pdf_non_object_payloads_are_clean() {
    let app = TestApp::production();
    let db = app.state.main_db.as_ref().unwrap();
    for dict in [
        "",
        "/Type/XRef",
        "/Subtype/Image/Width 1/Height 1",
        "/Length1 12",
        "/FunctionType 4",
    ] {
        for compressed in [false, true] {
            // Pixel/font bytes, inline image data and PostScript are not PDF objects.
            let payload = b"BI /W 1 /H 1 ID \x00\xff<({ DEMO } EI { dup mul }";
            let (dict, payload) = if compressed {
                (
                    format!("{dict}/Filter/FlateDecode"),
                    miniz_oxide::deflate::compress_to_vec_zlib(payload, 6),
                )
            } else {
                (dict.to_string(), payload.to_vec())
            };
            let stored = tuvalu_court::storage::store(
                db,
                &r3_stream(&dict, &payload),
                "demo.pdf",
                1_000_000,
            )
            .unwrap();
            assert_eq!(
                stored.scan_status, "clean",
                "{dict}: {:?}",
                stored.scan_note
            );
        }
    }
    for dict in ["", "/Type/ObjStm"] {
        let encoded = miniz_oxide::deflate::compress_to_vec_zlib(b"/J#53(DEMO active)", 6);
        assert_eq!(
            tuvalu_court::storage::store(
                db,
                &r3_stream(&format!("{dict}/Filter/FlateDecode"), &encoded),
                "active.pdf",
                1_000_000
            )
            .unwrap()
            .scan_status,
            "quarantined"
        );
    }
}

#[test]
fn r3_pdf_predictor_encoded_object_stream_is_quarantined() {
    let app = TestApp::production();
    let db = app.state.main_db.as_ref().unwrap();
    let active = b"1 0 <</Type/Catalog/OpenAction<</S/JavaScript/JS(DEMO)>> >>";
    // PNG Up: the first row is spaces; the second row reconstructs the active object.
    let mut predicted = vec![2];
    predicted.extend(std::iter::repeat_n(b' ', active.len()));
    predicted.push(2);
    predicted.extend(active.iter().map(|b| b.wrapping_sub(b' ')));
    let predicted = miniz_oxide::deflate::compress_to_vec_zlib(&predicted, 6);
    let dict = format!(
        "/Type/ObjStm/N 1/First 4/Filter/FlateDecode/DecodeParms<</Predictor 12/Columns {}>>",
        active.len()
    );
    let stored = tuvalu_court::storage::store(
        db,
        &r3_stream(&dict, &predicted),
        "predicted.pdf",
        1_000_000,
    )
    .unwrap();
    assert_eq!(stored.scan_status, "quarantined", "{:?}", stored.scan_note);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r3_import_scan_survives_dropped_request() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let _lock = HEAVY.lock().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = tuvalu_court::config::Config::for_tests(
        Default::default(),
        tuvalu_court::config::Mode::Production,
    );
    cfg.clamd = Some(format!("tcp://{}", listener.local_addr().unwrap()));
    cfg.scan_timeout_ms = 3000;
    let (app, olga) = seeded_production(cfg).await;
    let (_, number) = register_case(&olga, "DEMO cancelled import").await;
    let db = app.state.main_db.as_ref().unwrap();
    let conn = db.open().unwrap();
    let uid: i64 = conn
        .query_row("SELECT id FROM users WHERE persona='elena'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let mut c = olga.clone();
    c.session = Some(auth::create_session(&conn, uid, true, 12).unwrap());
    let manifest = format!(
        "case_number,filename,title,doc_type,visibility,document_date\n{number},one.pdf,DEMO one,evidence,party_material,{}\n{number},two.pdf,DEMO two,evidence,party_material,{}\n",
        today(),
        today()
    );
    let archive = zip_files(&[
        ("manifest.csv", manifest.as_bytes()),
        ("one.pdf", &pdf("DEMO one")),
        ("two.pdf", &pdf("DEMO two")),
    ]);
    let (s, b) = c
        .upload("/api/import/files/preview", &[], "package.zip", &archive)
        .await;
    ok(s, &b);
    let batch = b["batch_id"].as_i64().unwrap();
    let job = tokio::spawn(async move {
        c.post_idem(
            &format!("/api/import/{batch}/commit"),
            "DEMO cancelled import",
            json!({}),
        )
        .await
    });
    let (mut socket, _) =
        tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
    let mut command = [0; 10];
    socket.read_exact(&mut command).await.unwrap();
    assert_eq!(&command, b"zINSTREAM\0");
    loop {
        let n = socket.read_u32().await.unwrap();
        if n == 0 {
            break;
        }
        socket.read_exact(&mut vec![0; n as usize]).await.unwrap();
    }
    job.abort();
    assert!(job.await.unwrap_err().is_cancelled());
    // Both the live scan and the next version in the batch must survive cancellation.
    let scanner = tokio::spawn(async move {
        socket.write_all(b"stream: OK\0").await.unwrap();
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.read_exact(&mut command).await.unwrap();
        loop {
            let n = socket.read_u32().await.unwrap();
            if n == 0 {
                break;
            }
            socket.read_exact(&mut vec![0; n as usize]).await.unwrap();
        }
        socket.write_all(b"stream: OK\0").await.unwrap();
    });
    let verdict = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let clean: i64 = conn.query_row("SELECT COUNT(*) FROM document_versions v JOIN documents d ON d.id=v.document_id WHERE d.title IN ('DEMO one','DEMO two') AND v.scan_status='clean'", [], |r| r.get(0)).unwrap();
            if clean == 2 { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await;
    scanner.abort();
    assert!(
        verdict.is_ok(),
        "cancelled import left committed versions pending"
    );
    assert_eq!(tuvalu_court::audit::verify_chain(&conn).unwrap().1, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r3_restore_revokes_source_sessions_and_allows_password_totp_login() {
    let app = TestApp::production();
    let c = enrolled(&app).await;
    let db = app.state.main_db.as_ref().unwrap();
    let conn = db.open().unwrap();
    // The restored account must accept a fresh code, not replay a consumed one.
    let secret: String = conn
        .query_row(
            "SELECT totp_secret FROM users WHERE username='demostaff'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute(
        "UPDATE users SET must_change_password=0,totp_last_step=NULL",
        [],
    )
    .unwrap();
    let uid: i64 = conn
        .query_row("SELECT id FROM users WHERE username='demostaff'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let pending_cookie = auth::create_session(&conn, uid, false, 12).unwrap();
    let key = app.dir.join("sessions.key");
    let archive = app.dir.join("sessions.tcrb");
    let target = app.dir.join("restored");
    tuvalu_court::backup::gen_key(&key).unwrap();
    tuvalu_court::backup::backup(db, &archive, &key).unwrap();
    tuvalu_court::backup::restore(&archive, &key, &target).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let child = Command::new(env!("CARGO_BIN_EXE_tuvalu-court"))
        .arg("serve")
        .env("TCR_MODE", "production")
        .env("TCR_AV", "off")
        .env("TCR_DATA_DIR", &target)
        .env("TCR_BIND", format!("127.0.0.1:{port}"))
        .env_remove("TCR_ENV_FILE")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut server = RunningServer(child);
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            break;
        }
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "restored server exited"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    for cookie in [c.session.as_deref().unwrap(), &pending_cookie] {
        assert_eq!(
            network_http(port, "GET", "/api/auth/me", json!({}), Some(cookie))
                .await
                .0,
            401,
            "source cookie survived restore"
        );
    }
    let (status, b, cookie) = network_http(
        port,
        "POST",
        "/api/auth/login",
        json!({"username":"demostaff","password":"temporary-pass-99"}),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(b["mfa_required"], true);
    let (status, _, cookie) = network_http(
        port,
        "POST",
        "/api/auth/totp",
        json!({"code":auth::current_totp(&secret).unwrap()}),
        cookie.as_deref(),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        network_http(port, "GET", "/api/cases", json!({}), cookie.as_deref())
            .await
            .0,
        200
    );
    assert_eq!(
        c.get("/api/auth/me").await.0,
        StatusCode::OK,
        "restore mutated source sessions"
    );
}

#[test]
fn r3_pdf_untyped_asciihex_stream_is_quarantined() {
    let app = TestApp::production();
    let db = app.state.main_db.as_ref().unwrap();
    let active = b"1 0 <</Type/Catalog/OpenAction<</S/JavaScript/JS(DEMO)>> >>";
    let encoded = format!("{}>", hex::encode(active));
    let stored = tuvalu_court::storage::store(
        db,
        &r3_stream("/N 1/First 4/Filter/ASCIIHexDecode", encoded.as_bytes()),
        "hex.pdf",
        1_000_000,
    )
    .unwrap();
    assert_eq!(stored.scan_status, "quarantined", "{:?}", stored.scan_note);
}

#[test]
fn r3_pdf_large_token_count_is_clean() {
    let app = TestApp::production();
    let db = app.state.main_db.as_ref().unwrap();
    let mut bytes = b"%PDF-1.4\n1 0 obj [".to_vec();
    bytes.extend_from_slice(&b"0 ".repeat(300_000));
    bytes.extend_from_slice(b"] endobj\n%%EOF");
    let stored = tuvalu_court::storage::store(db, &bytes, "large.pdf", 1_000_000).unwrap();
    assert_eq!(stored.scan_status, "clean", "{:?}", stored.scan_note);
}

#[test]
fn r3_pdf_predictors_images_and_decode_parameters() {
    let app = TestApp::production();
    let db = app.state.main_db.as_ref().unwrap();
    let clean = b"1 0 << /Type /Pages /Count 0 /Kids [] >>";
    for (predictor, bytes) in [
        (
            2,
            clean
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    if i == 0 {
                        *b
                    } else {
                        b.wrapping_sub(clean[i - 1])
                    }
                })
                .collect::<Vec<_>>(),
        ),
        (10, [&[0][..], clean].concat()),
        (12, [&[2][..], clean].concat()),
        (
            15,
            [
                &[4][..],
                &clean
                    .iter()
                    .enumerate()
                    .map(|(i, b)| {
                        if i == 0 {
                            *b
                        } else {
                            b.wrapping_sub(clean[i - 1])
                        }
                    })
                    .collect::<Vec<_>>(),
            ]
            .concat(),
        ),
    ] {
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&bytes, 6);
        let dict = format!(
            "/Type/ObjStm/N 1/First 4/Filter[/FlateDecode]/DecodeParms[<</Predictor {predictor}/Columns {}>>]",
            clean.len()
        );
        let stored = tuvalu_court::storage::store(
            db,
            &r3_stream(&dict, &compressed),
            "predictor.pdf",
            1_000_000,
        )
        .unwrap();
        assert_eq!(
            stored.scan_status, "clean",
            "predictor {predictor}: {:?}",
            stored.scan_note
        );
    }
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(b"DEMO", 6);
    for params in [
        "<</Predictor 99>>",
        "<</Predictor 12/BitsPerComponent 16>>",
        "5 0 R",
        "[null null]",
        "<</Predictor 12/Columns 999999999>>",
    ] {
        let stored = tuvalu_court::storage::store(
            db,
            &r3_stream(
                &format!("/Filter/FlateDecode/DecodeParms {params}"),
                &compressed,
            ),
            "opaque.pdf",
            1_000_000,
        )
        .unwrap();
        assert_eq!(
            stored.scan_status, "quarantined",
            "{params}: {:?}",
            stored.scan_note
        );
    }
    for filter in [
        "ASCIIHexDecode",
        "DCTDecode",
        "JPXDecode",
        "CCITTFaxDecode",
        "JBIG2Decode",
    ] {
        let stored = tuvalu_court::storage::store(
            db,
            &r3_stream(
                &format!("/Subtype/Image/Filter/{filter}"),
                b"DEMO opaque image",
            ),
            "image.pdf",
            1_000_000,
        )
        .unwrap();
        assert_eq!(
            stored.scan_status, "clean",
            "image {filter}: {:?}",
            stored.scan_note
        );
        let stored = tuvalu_court::storage::store(
            db,
            &r3_stream(
                &format!("/Type/ObjStm/Subtype/Image/Filter/{filter}"),
                b"DEMO hidden object",
            ),
            "object.pdf",
            1_000_000,
        )
        .unwrap();
        assert_eq!(stored.scan_status, "quarantined");
    }
    let stored = tuvalu_court::storage::store(
        db,
        &r3_stream(
            "/Subtype/Image/Filter/FlateDecode/DecodeParms<</Predictor 12/BitsPerComponent 16>>",
            &compressed,
        ),
        "image.pdf",
        1_000_000,
    )
    .unwrap();
    assert_eq!(stored.scan_status, "clean", "{:?}", stored.scan_note);
    let stored = tuvalu_court::storage::store(
        db,
        &r3_stream("/Filter/FlateDecode/DecodeParms null", &compressed),
        "content.pdf",
        1_000_000,
    )
    .unwrap();
    assert_eq!(stored.scan_status, "clean", "{:?}", stored.scan_note);
}

#[test]
fn r3_pdf_indirect_length_preserves_compressed_line_endings() {
    let app = TestApp::production();
    let db = app.state.main_db.as_ref().unwrap();
    let compressed = (0..1000)
        .find_map(|n| {
            let payload = format!("DEMO indirect stream {n}");
            let compressed = miniz_oxide::deflate::compress_to_vec_zlib(payload.as_bytes(), 6);
            matches!(compressed.last(), Some(b'\n' | b'\r')).then_some(compressed)
        })
        .unwrap();
    let mut bytes = b"%PDF-1.5\n1 0 obj <</Filter/FlateDecode/Length 2 0 R>>stream\n".to_vec();
    bytes.extend_from_slice(&compressed);
    bytes.extend_from_slice(
        format!(
            "\nendstream\nendobj\n2 0 obj {} endobj\n%%EOF",
            compressed.len()
        )
        .as_bytes(),
    );
    let stored = tuvalu_court::storage::store(db, &bytes, "indirect.pdf", 1_000_000).unwrap();
    assert_eq!(stored.scan_status, "clean", "{:?}", stored.scan_note);
}

#[test]
fn r3_pdf_payload_name_boundaries_avoid_incidental_binary_matches() {
    let app = TestApp::production();
    let db = app.state.main_db.as_ref().unwrap();
    for payload in [
        &b"\xffDEMO-pixels/JS \x00\xff"[..],
        &b"DEMO-profile/AA "[..],
    ] {
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(payload, 6);
        let stored = tuvalu_court::storage::store(
            db,
            &r3_stream("/Filter/FlateDecode", &compressed),
            "demo.pdf",
            1_000_000,
        )
        .unwrap();
        assert_eq!(stored.scan_status, "clean", "{:?}", stored.scan_note);
    }
    for payload in [
        &b"<</Type/Catalog/OpenAction<</S/JavaScript/J#53(DEMO)>>>>"[..],
        &b"<</Pages 1 0 R/AA<</O 2 0 R>>>>"[..],
        &b"<</Count 1/AA<</O 2 0 R>>>>"[..],
        &b"<</Flag true/AA<</O 2 0 R>>>>"[..],
    ] {
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(payload, 6);
        let stored = tuvalu_court::storage::store(
            db,
            &r3_stream("/Filter/FlateDecode", &compressed),
            "active.pdf",
            1_000_000,
        )
        .unwrap();
        assert_eq!(stored.scan_status, "quarantined", "{:?}", stored.scan_note);
    }
}

#[test]
fn r3_pdf_xml_thumbnail_names_are_not_action_keys() {
    let app = TestApp::production();
    let db = app.state.main_db.as_ref().unwrap();
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(
        b"<DEMO-thumbnail>\n/AA/DEMO\n/JS/DEMO\n</DEMO-thumbnail>",
        6,
    );
    let stored = tuvalu_court::storage::store(
        db,
        &r3_stream("/Type/Metadata/Subtype/XML/Filter/FlateDecode", &compressed),
        "metadata.pdf",
        1_000_000,
    )
    .unwrap();
    assert_eq!(stored.scan_status, "clean", "{:?}", stored.scan_note);
}

// Follow-up PDF compatibility and fail-closed action/encoding regressions.
fn r3_followup_pdf_status(bytes: &[u8], expected: &str, limit: u64) {
    let app = TestApp::production();
    let db = app.state.main_db.as_ref().unwrap();
    let stored = tuvalu_court::storage::store(db, bytes, "demo.pdf", limit).unwrap();
    assert_eq!(stored.scan_status, expected, "{:?}", stored.scan_note);
}

#[test]
fn r3_followup_pdf_actions_and_duplicate_keys() {
    for body in [
        "1 0 obj <</OpenAction[3 0 R/Fit]>> endobj",
        "1 0 obj <</OpenAction 2 0 R>> endobj 2 0 obj [3 0 R/Fit] endobj",
        "1 0 obj <</AA<</O<</S/GoTo/D[3 0 R/Fit]>>>>>> endobj",
        "1 0 obj <</OpenAction 2 0 R>> endobj 2 0 obj <</S 4 0 R/D[3 0 R/Fit]>> endobj 4 0 obj /GoTo endobj",
        "1 0 obj <</Type/Annot/Subtype/Link/A<</S/URI/URI(https://example.test/)>>>> endobj",
        "1 0 obj <</OpenAction<</S/GoToR/F(other.pdf)/D[0/Fit]>>>> endobj",
        "1 0 obj <</OpenAction<</S/ResetForm>>>> endobj",
        "1 0 obj <</OpenAction<</S/Sound>>>> endobj",
        "1 0 obj <</OpenAction<</S/Movie>>>> endobj",
        "1 0 obj <</Producer(DEMO first)/Producer(DEMO second)>> endobj",
        "1 0 obj <</Note/Launch/Label/SubmitForm>> endobj",
        "1 0 obj <</Note/Encrypt/Encrypt null>> endobj",
        "1 0 obj <</Subtype/Link/A 2 0 R>> endobj 2 0 obj <</S 4 0 R/URI(https://example.test/)>> endobj 4 0 obj /URI endobj",
        "1 0 obj <</Type/Font/CharProcs<</A 9 0 R>>>> endobj",
    ] {
        r3_followup_pdf_status(
            format!("%PDF-1.5\n{body}\n%%EOF").as_bytes(),
            "clean",
            1_000_000,
        );
    }
    for body in [
        "1 0 obj <</OpenAction 2 0 R>> endobj 2 0 obj <</S 4 0 R>> endobj 4 0 obj /JavaScript endobj",
        "1 0 obj <</OpenAction 2 0 R>> endobj 2 0 obj <</S/Launch>> endobj",
        "1 0 obj <</OpenAction 2 0 R>> endobj",
        "1 0 obj <</OpenAction 2 0 R>> endobj 2 0 obj <</S 4 0 R>> endobj",
        "1 0 obj <</AA<</O 7 0 R>>>> endobj",
        "1 0 obj <</OpenAction<</S/URI/Next 2 0 R>>>> endobj 2 0 obj <</S/Launch>> endobj",
        "1 0 obj <</OpenAction<</S/GoToR/F 2 0 R>>>> endobj 2 0 obj <</F(DEMO.cmd)>> endobj",
        "1 0 obj <</OpenAction<</S/GoToR/F<FEFF00640065006D006F002E006500780065>>>>>> endobj",
        "1 0 obj <</S/URI/S/Launch>> endobj",
        "1 0 obj <</S/URI/#53/URI>> endobj",
        "1 0 obj <</Note/J#53>> endobj",
        "1 0 obj <</Encrypt 2 0 R>> endobj",
        "1 0 obj <</Subtype/Link/A 9 0 R>> endobj",
        "1 0 obj <</Title(DEMO outline)/A 9 0 R>> endobj",
        "1 0 obj <</OpenAction<</S/GoToR/F<</FS/URL/F(file:///DEMO%2Eexe)>>>>>> endobj",
        "1 0 obj <</OpenAction<</S/GoToR/F(DEMO.exe:Zone.Identifier)>>>> endobj",
        "1 0 obj <</OpenAction 2 0 R>> endobj 2 0 obj <</S/URI/Next 2 0 R>> endobj",
    ] {
        r3_followup_pdf_status(
            format!("%PDF-1.5\n{body}\n%%EOF").as_bytes(),
            "quarantined",
            1_000_000,
        );
    }
    for action in [
        "JavaScript",
        "Launch",
        "SubmitForm",
        "ImportData",
        "Rendition",
        "GoToE",
    ] {
        r3_followup_pdf_status(
            format!("%PDF-1.5\n1 0 obj <</OpenAction<</S/{action}>>>> endobj\n%%EOF").as_bytes(),
            "quarantined",
            1_000_000,
        );
    }
}

fn r3_followup_ascii85(bytes: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    for group in bytes.chunks(4) {
        let mut word = [0; 4];
        word[..group.len()].copy_from_slice(group);
        let mut value = u32::from_be_bytes(word);
        if value == 0 && group.len() == 4 {
            encoded.push(b'z');
            continue;
        }
        let mut digits = [0; 5];
        for digit in digits.iter_mut().rev() {
            *digit = (value % 85) as u8 + b'!';
            value /= 85;
        }
        encoded.extend_from_slice(&digits[..group.len() + 1]);
    }
    encoded.extend_from_slice(b"~>");
    encoded
}
fn r3_followup_runlength(bytes: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    for group in bytes.chunks(128) {
        encoded.push(group.len() as u8 - 1);
        encoded.extend_from_slice(group);
    }
    encoded.push(128);
    encoded
}
// Literal LZW codes exercise all width transitions and dictionary saturation,
// without coupling the fixture to a compression library or the production decoder.
fn r3_followup_lzw(bytes: &[u8], early: usize) -> Vec<u8> {
    let mut encoded = Vec::new();
    let mut bits = 0u32;
    let mut available = 0usize;
    let mut width = 9;
    let mut next = 258usize;
    for (index, code) in std::iter::once(256u16)
        .chain(bytes.iter().map(|b| *b as u16))
        .chain(std::iter::once(257))
        .enumerate()
    {
        bits = (bits << width) | code as u32;
        available += width;
        while available >= 8 {
            available -= 8;
            encoded.push((bits >> available) as u8);
            bits &= (1 << available) - 1;
        }
        if index > 1 && code != 257 && next < 4096 {
            next += 1;
            if width < 12 && next + early == 1 << width {
                width += 1;
            }
        }
    }
    if available > 0 {
        encoded.push((bits << (8 - available)) as u8);
    }
    encoded
}

#[test]
fn r3_followup_pdf_encoded_object_streams() {
    for active in [false, true] {
        let content = if active {
            b"8 0 <</JS(DEMO)>>".as_slice()
        } else {
            b"8 0 <</Producer(DEMO)>>"
        };
        let expected = if active { "quarantined" } else { "clean" };
        for (filter, params, encoded) in [
            (
                "ASCIIHexDecode",
                "",
                format!("{}>", hex::encode(content)).into_bytes(),
            ),
            ("ASCII85Decode", "", r3_followup_ascii85(content)),
            ("RunLengthDecode", "", r3_followup_runlength(content)),
            (
                "LZWDecode",
                "/DecodeParms<</EarlyChange 0>>",
                r3_followup_lzw(content, 0),
            ),
            (
                "LZWDecode",
                "/DecodeParms<</EarlyChange 1>>",
                r3_followup_lzw(content, 1),
            ),
        ] {
            let dict = format!("/Type/ObjStm/N 1/First 4/Filter/{filter}{params}");
            r3_followup_pdf_status(&r3_stream(&dict, &encoded), expected, 1_000_000);
        }
        let flate = miniz_oxide::deflate::compress_to_vec_zlib(content, 6);
        let encoded = r3_followup_ascii85(&flate);
        r3_followup_pdf_status(
            &r3_stream(
                "/Type/ObjStm/N 1/First 4/Filter[/ASCII85Decode/FlateDecode]/DecodeParms[null null]",
                &encoded,
            ),
            expected,
            1_000_000,
        );
    }
    // The action dictionary and /S target both live in compressed objects.
    for action in ["GoTo", "Launch", "JavaScript"] {
        let first_object = b"<</S 9 0 R/D[3 0 R/Fit]>>";
        let header = format!("8 0 9 {} ", first_object.len() + 1);
        let content = [
            header.as_bytes(),
            first_object,
            b" ",
            format!("/{action}").as_bytes(),
        ]
        .concat();
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&content, 6);
        let mut pdf = r3_stream(
            &format!("/Type/ObjStm/N 2/First {}/Filter/FlateDecode", header.len()),
            &compressed,
        );
        pdf.extend_from_slice(b"\n10 0 obj <</OpenAction 8 0 R>> endobj\n%%EOF");
        r3_followup_pdf_status(
            &pdf,
            if action == "GoTo" {
                "clean"
            } else {
                "quarantined"
            },
            1_000_000,
        );
    }
    for early in [0, 1] {
        let mut content = b"8 0 <</Note(".to_vec();
        content.extend_from_slice(&b"DEMO literal text ".repeat(500));
        content.extend_from_slice(b")>>");
        r3_followup_pdf_status(
            &r3_stream(
                &format!(
                    "/Type/ObjStm/N 1/First 4/Filter/LZWDecode/DecodeParms<</EarlyChange {early}>>"
                ),
                &r3_followup_lzw(&content, early),
            ),
            "clean",
            1_000_000,
        );
    }
    // LZW KwKwK: clear, A, new-code 258, EOD decodes to AAA.
    let codes = [256u16, 65, 258, 257];
    let bits = codes
        .iter()
        .fold(0u64, |bits, code| (bits << 9) | *code as u64)
        << 4;
    r3_followup_pdf_status(
        &r3_stream("/Filter/LZWDecode", &bits.to_be_bytes()[3..]),
        "clean",
        1_000_000,
    );
    r3_followup_pdf_status(
        &r3_stream("/Filter/RunLengthDecode", &[255, b'A', 128]),
        "clean",
        1_000_000,
    );
    r3_followup_pdf_status(
        &r3_stream("/Filter/ASCII85Decode", b"z~>"),
        "clean",
        1_000_000,
    );
    r3_followup_pdf_status(
        &r3_stream("/Filter/ASCIIHexDecode", b"4>"),
        "clean",
        1_000_000,
    );
}

#[test]
fn r3_followup_pdf_streaming_limits_and_boundaries() {
    // Drop the 30 MiB source before scanning: only a tiny encoded stream remains.
    let compressed = {
        let content = vec![b' '; 30 * 1024 * 1024];
        miniz_oxide::deflate::compress_to_vec_zlib(&content, 6)
    };
    let pdf = r3_stream("/Filter/FlateDecode", &compressed);
    r3_followup_pdf_status(&pdf, "clean", 1_000_000);
    r3_followup_pdf_status(&pdf, "quarantined", 100_000);
    for offset in 8180..8200 {
        let mut content = vec![b' '; offset];
        content.extend_from_slice(b"/J#53(DEMO)");
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&content, 6);
        r3_followup_pdf_status(
            &r3_stream("/Filter/FlateDecode", &compressed),
            "quarantined",
            1_000_000,
        );
        let mut content = vec![b' '; offset];
        content.extend_from_slice(b"/JavaScriptLongBenignName ");
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&content, 6);
        r3_followup_pdf_status(
            &r3_stream("/Filter/FlateDecode", &compressed),
            "clean",
            1_000_000,
        );
    }
    for (filter, data) in [
        ("ASCIIHexDecode", b"xyz>".as_slice()),
        ("ASCII85Decode", b"!~>"),
        ("ASCII85Decode", b"uuuuu~>"),
        ("RunLengthDecode", b"\x05abc"),
        ("LZWDecode", b"\xff\xff"),
        ("DCTDecode", b"DEMO uninspectable non-image"),
    ] {
        r3_followup_pdf_status(
            &r3_stream(&format!("/Filter/{filter}"), data),
            "quarantined",
            1_000_000,
        );
    }
}

#[test]
fn r3_followup_pdf_predictor_chains_and_type_ambiguity() {
    for active in [false, true] {
        let content = if active {
            b"8 0 <</JS(DEMO)>>".as_slice()
        } else {
            b"8 0 <</Producer(DEMO)>>"
        };
        let mut predicted = vec![0];
        predicted.extend_from_slice(content);
        for early in [0, 1] {
            let lzw = r3_followup_lzw(&predicted, early);
            let encoded = r3_followup_ascii85(&lzw);
            let dict = format!(
                "/Type/ObjStm/N 1/First 4/Filter[/ASCII85Decode/LZWDecode]/DecodeParms[null<</EarlyChange {early}/Predictor 15/Columns {}>>]",
                content.len()
            );
            r3_followup_pdf_status(
                &r3_stream(&dict, &encoded),
                if active { "quarantined" } else { "clean" },
                1_000_000,
            );
        }
    }
    r3_followup_pdf_status(
        &r3_stream(
            "/Type 9 0 R/Subtype/Image/Filter/ASCIIHexDecode",
            b"2F4A53>",
        ),
        "quarantined",
        1_000_000,
    );
    for dict in [
        "/Filter/FlateDecode/Filter/FlateDecode",
        "/Filter/FlateDecode/DecodeParms<</Predictor 1/Predictor 1>>",
        "/Type/ObjStm/N 1/First 4/First 4/Filter/FlateDecode",
    ] {
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(b"8 0 <</Producer(DEMO)>>", 6);
        r3_followup_pdf_status(&r3_stream(dict, &compressed), "quarantined", 1_000_000);
    }
    // A missing object-stream header remains malformed even when its body is benign.
    r3_followup_pdf_status(
        &r3_stream("/Type/ObjStm", b"<</Producer(DEMO)>>"),
        "quarantined",
        1_000_000,
    );
}

#[test]
fn r3_followup_pdf_stream_names_are_not_xml_paths() {
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(b"/J#53/DEMO", 6);
    r3_followup_pdf_status(
        &r3_stream("/Filter/FlateDecode", &compressed),
        "quarantined",
        1_000_000,
    );
}

#[test]
fn r3_followup_pdf_image_exception_requires_image_syntax() {
    for dict in [
        "/Type/Metadata/Subtype/Image/Filter/ASCIIHexDecode",
        "/Type/XObject/Subtype/Image/N 1/First 4/Filter/ASCIIHexDecode",
    ] {
        let encoded = format!("{}>", hex::encode(b"8 0 <</S/Launch>>"));
        r3_followup_pdf_status(
            &r3_stream(dict, encoded.as_bytes()),
            "quarantined",
            1_000_000,
        );
    }
}

// Robustness inputs are generated here so no large or hostile fixtures are checked in.
fn r5_action_fanout(count: usize, fanout: usize) -> Vec<u8> {
    let mut pdf = b"%PDF-1.5\n".to_vec();
    for id in 1..=count {
        pdf.extend_from_slice(format!("{id} 0 obj <</S/GoTo/D[0/Fit]").as_bytes());
        if id < count {
            pdf.extend_from_slice(b"/Next[");
            pdf.extend_from_slice(format!("{} 0 R ", id + 1).repeat(fanout).as_bytes());
            pdf.extend_from_slice(b"]");
        }
        pdf.extend_from_slice(b">> endobj\n");
    }
    pdf.extend_from_slice(b"%%EOF");
    pdf
}

#[test]
fn r5_pdf_action_fanout_is_bounded() {
    use std::time::{Duration, Instant};
    // Kill the pre-fix exponential traversal rather than hanging the test runner.
    const CHILD: &str = "TCR_R5_FANOUT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "r5_pdf_action_fanout_is_bounded", "--nocapture"])
            .env(CHILD, "1")
            .spawn()
            .unwrap();
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "fan-out child failed: {status}");
                return;
            }
            if start.elapsed() > Duration::from_secs(5) {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("PDF action fan-out exceeded watchdog");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let app = TestApp::production();
    let db = app.state.main_db.as_ref().unwrap();
    for (count, fanout) in [(40, 2), (5, 1000)] {
        let bytes = r5_action_fanout(count, fanout);
        let start = Instant::now();
        for _ in 0..2 {
            let stored =
                tuvalu_court::storage::store(db, &bytes, "fanout.pdf", 15 * 1024 * 1024).unwrap();
            assert_eq!(stored.scan_status, "clean", "{:?}", stored.scan_note);
            assert_eq!(
                stored.scan_note.as_deref(),
                Some("Format checks only, no antivirus (TCR_AV=off)")
            );
        }
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "{count} x {fanout}: {:?}",
            start.elapsed()
        );
    }
    // Distinct roots must share one budget, even with only shallow action chains.
    let mut bytes = b"%PDF-1.5\n".to_vec();
    for id in 1..=60 {
        bytes.extend_from_slice(format!("{id} 0 obj <</S/GoTo/Next[").as_bytes());
        bytes.extend_from_slice(&b"<</S/GoTo>> ".repeat(1000));
        bytes.extend_from_slice(b"]>> endobj\n");
    }
    bytes.extend_from_slice(b"%%EOF");
    let start = Instant::now();
    let stored = tuvalu_court::storage::store(db, &bytes, "budget.pdf", 15 * 1024 * 1024).unwrap();
    assert_eq!(stored.scan_status, "quarantined");
    assert_eq!(
        stored.scan_note.as_deref(),
        Some("PDF action/reference work budget exceeded")
    );
    assert!(start.elapsed() < Duration::from_secs(3));
}

fn r5_zero_payload(actions: usize, size: usize) -> Vec<u8> {
    let mut content = b"/S /x ".repeat(actions);
    content.resize(content.len() + size, 0);
    let encoded = miniz_oxide::deflate::compress_to_vec_zlib(&content, 6);
    r3_stream("/Filter/FlateDecode", &encoded)
}

#[test]
fn r5_pdf_name_scan_tails_and_fixed_verdict_are_bounded() {
    use std::time::{Duration, Instant};
    let app = TestApp::production();
    let db = app.state.main_db.as_ref().unwrap();
    let mut timings = Vec::new();
    // Proportional versions of the reviewer's 900 MiB zero stream, within the decode budget.
    for size in [8 * 1024 * 1024, 32 * 1024 * 1024] {
        let bytes = r5_zero_payload(64, size);
        let start = Instant::now();
        let stored =
            tuvalu_court::storage::store(db, &bytes, "tails.pdf", 15 * 1024 * 1024).unwrap();
        let elapsed = start.elapsed();
        assert_eq!(stored.scan_status, "clean", "{:?}", stored.scan_note);
        assert!(
            elapsed < Duration::from_secs(3),
            "{size} bytes: {elapsed:?}"
        );
        timings.push(elapsed);
    }
    assert!(
        timings[1] <= timings[0] * 6 + Duration::from_millis(100),
        "{timings:?}"
    );
    let bytes = r5_zero_payload(70, 64 * 1024 * 1024);
    let start = Instant::now();
    let stored =
        tuvalu_court::storage::store(db, &bytes, "many-actions.pdf", 15 * 1024 * 1024).unwrap();
    assert_eq!(stored.scan_status, "quarantined");
    assert_eq!(
        stored.scan_note.as_deref(),
        Some("PDF payload actions exceed inspection limit")
    );
    assert!(
        start.elapsed() < Duration::from_millis(250),
        "fixed verdict: {:?}",
        start.elapsed()
    );
}

#[test]
fn r5_pdf_attachments_without_embeddedfile_type_are_quarantined() {
    for body in [
        "1 0 obj <</Type/Filespec/F(demo.txt)/EF<</F 2 0 R>>>> endobj\n2 0 obj <</Length 4>>stream\nDEMO\nendstream endobj",
        "1 0 obj <</Type/Annot/Subtype/FileAttachment/FS 2 0 R>> endobj\n2 0 obj <</F(demo.txt)>> endobj",
        "1 0 obj <</EF null>> endobj",
        "1 0 obj <</Subtype 2 0 R>> endobj\n2 0 obj /FileAttachment endobj",
    ] {
        r3_followup_pdf_status(
            format!("%PDF-1.5\n{body}\n%%EOF").as_bytes(),
            "quarantined",
            1_000_000,
        );
    }
}

#[cfg(debug_assertions)]
static R5_INSPECTIONS: std::sync::LazyLock<parking_lot::Mutex<usize>> =
    std::sync::LazyLock::new(|| parking_lot::Mutex::new(0));

#[cfg(debug_assertions)]
fn r5_assert_no_writer_during_inspection(db: &tuvalu_court::db::Db) {
    let conn = db.open().unwrap();
    conn.busy_timeout(std::time::Duration::ZERO).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
        .expect("storage inspection held the SQLite writer lock");
    *R5_INSPECTIONS.lock() += 1;
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn r5_import_inspects_before_opening_write_transaction() {
    let _lock = HEAVY.lock().await;
    let cfg = tuvalu_court::config::Config::for_tests(
        Default::default(),
        tuvalu_court::config::Mode::Production,
    );
    let (app, olga) = seeded_production(cfg).await;
    let (_, number) = register_case(&olga, "DEMO format inspection lock").await;
    let db = app.state.main_db.as_ref().unwrap();
    let uid = db
        .open()
        .unwrap()
        .query_row("SELECT id FROM users WHERE persona='elena'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let mut c = olga.clone();
    c.session = Some(auth::create_session(&db.open().unwrap(), uid, true, 12).unwrap());
    let bytes = r5_zero_payload(64, 8 * 1024 * 1024);
    let manifest = format!(
        "case_number,filename,title,doc_type,visibility,document_date\n{number},import.pdf,DEMO format inspection,evidence,party_material,2026-10-08\n"
    );
    let archive = zip_files(&[
        ("manifest.csv", manifest.as_bytes()),
        ("import.pdf", &bytes),
    ]);
    let (s, b) = c
        .upload("/api/import/files/preview", &[], "package.zip", &archive)
        .await;
    ok(s, &b);
    let batch = b["batch_id"].as_i64().unwrap();
    *R5_INSPECTIONS.lock() = 0;
    tuvalu_court::storage::set_inspection_hook(db, Some(r5_assert_no_writer_during_inspection));
    let path = format!("/api/import/{batch}/commit");
    let (s, b) = c
        .post_idem(&path, "DEMO inspection boundary", json!({}))
        .await;
    tuvalu_court::storage::set_inspection_hook(db, None);
    ok(s, &b);
    assert_eq!(b["summary"]["created"], 1);
    assert_eq!(
        *R5_INSPECTIONS.lock(),
        1,
        "import must inspect exactly once"
    );
    let vid = b["created"][0]["version_id"].as_i64().unwrap();
    assert_eq!(
        db.open()
            .unwrap()
            .query_row(
                "SELECT scan_status FROM document_versions WHERE id=?1",
                [vid],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "clean"
    );
    let (s, replay) = c
        .post_idem(&path, "DEMO inspection boundary", json!({}))
        .await;
    ok(s, &replay);
    assert_eq!(b, replay);
}
