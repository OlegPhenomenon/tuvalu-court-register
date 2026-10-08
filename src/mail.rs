//! Production SMTP delivery and retry scheduling. Demo always uses its local mailbox.
use crate::{
    config::{Config, Mode},
    db::Db,
    error::{AppError, AppResult},
    storage,
};
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Attachment, Body, MultiPart, SinglePart, header::ContentTransferEncoding},
    transport::smtp::client::{Certificate, Tls, TlsParameters},
};
use rusqlite::{Connection, params};
use serde_json::Value;
use std::{sync::LazyLock, time::Duration};

static NETWORK: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("mail runtime")
});

fn smtp(cfg: &Config) -> AppResult<AsyncSmtpTransport<Tokio1Executor>> {
    let raw = cfg
        .smtp_url
        .as_deref()
        .ok_or_else(|| AppError::validation("Mail transport not configured"))?;
    let mut url = url::Url::parse(raw).map_err(|_| AppError::validation("Invalid TCR_SMTP_URL"))?;
    if !matches!(url.scheme(), "smtp" | "smtps") || url.host_str().is_none() || url.query().is_some() || url.fragment().is_some() {
        return Err(AppError::validation(
            "TCR_SMTP_URL requires smtp:// (STARTTLS required) or smtps://; no query or fragment",
        ));
    }
    let implicit = url.scheme() == "smtps";
    let host = url.host_str().unwrap().to_string();
    if !implicit {
        url.set_query(Some("tls=required"));
    }
    let mut builder =
        AsyncSmtpTransport::<Tokio1Executor>::from_url(url.as_str()).map_err(|_| AppError::validation("Invalid SMTP configuration"))?;
    if let Some(pem) = &cfg.smtp_ca_pem {
        let ca = Certificate::from_pem(pem).map_err(|_| AppError::validation("Invalid SMTP CA"))?;
        let tls = TlsParameters::builder(host)
            .add_root_certificate(ca)
            .build_rustls()
            .map_err(|_| AppError::validation("Invalid SMTP TLS configuration"))?;
        builder = builder.tls(if implicit { Tls::Wrapper(tls) } else { Tls::Required(tls) });
    }
    Ok(builder.timeout(Some(Duration::from_secs(cfg.smtp_timeout_secs))).build())
}

pub fn validate(cfg: &Config) -> AppResult<()> {
    if cfg.mode == Mode::Demo {
        return Ok(());
    }
    if cfg.smtp_url.is_some() {
        smtp(cfg)?;
    }
    if let Some(from) = &cfg.mail_from {
        from.parse::<lettre::message::Mailbox>()
            .map_err(|_| AppError::validation("Invalid TCR_MAIL_FROM"))?;
    }
    Ok(())
}

pub fn status(cfg: &Config) -> &'static str {
    if cfg.mode == Mode::Demo {
        "DEMO: local mailbox only"
    } else if cfg.smtp_url.is_none() || cfg.mail_from.is_none() {
        "Mail transport not configured"
    } else {
        "SMTP configured (TLS required)"
    }
}

/// Compose outside a write transaction. Sending is a separate network-only step.
pub fn prepare(
    db: &Db,
    conn: &Connection,
    message_id: &str,
    address: &str,
    subject: &str,
    body: &str,
    items: &[Value],
) -> AppResult<Prepared> {
    let Some(cfg) = db.config().filter(|c| c.mode == Mode::Production) else {
        return Ok(Prepared::Local);
    };
    if !configured(db) {
        return Err(AppError::validation("Mail transport not configured"));
    }
    let transport = smtp(cfg)?;
    let mut parts = MultiPart::mixed().singlepart(SinglePart::plain(body.to_string()));
    for item in items {
        let vid = item["document_version_id"]
            .as_i64()
            .ok_or_else(|| AppError::validation("Missing attachment version"))?;
        let (key, sha, name, mime, scan): (String, String, String, String, String) = conn.query_row(
            "SELECT storage_key,sha256,filename,content_type,scan_status FROM document_versions WHERE id=?1",
            [vid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        if scan != "clean" {
            return Err(AppError::validation("Attachment is not clean"));
        }
        let bytes = storage::read(db, &key, &sha)
            .map_err(|_| AppError::validation(format!("Attachment version {vid} is unreadable or failed its integrity check; review this dispatch before retrying")))?;
        parts = parts.singlepart(
            Attachment::new(name).body(
                Body::new_with_encoding(bytes, ContentTransferEncoding::Base64)
                    .map_err(|_| AppError::internal("Attachment encoding failed"))?,
                mime.parse()
                    .map_err(|_| AppError::validation("Invalid attachment type"))?,
            ),
        );
    }
    let message = Message::builder()
        .from(
            cfg.mail_from
                .as_deref()
                .unwrap()
                .parse()
                .map_err(|_| AppError::validation("Invalid sender"))?,
        )
        .to(address
            .parse()
            .map_err(|_| AppError::validation("Invalid recipient"))?)
        // Stable across attempts, helping receiving systems recognise an ambiguous retry.
        .message_id(Some(message_id.to_string()))
        .subject(subject)
        .multipart(parts)
        .map_err(|_| AppError::validation("Could not compose email"))?;
    Ok(Prepared::Smtp {
        transport: Box::new(transport),
        message: Box::new(message),
        timeout: cfg.smtp_timeout_secs,
    })
}

pub enum Prepared {
    Local,
    Smtp {
        transport: Box<AsyncSmtpTransport<Tokio1Executor>>,
        message: Box<Message>,
        timeout: u64,
    },
}
impl Prepared {
    /// No connection or transaction is held during this operation.
    pub fn send(self) -> AppResult<&'static str> {
        let Self::Smtp {
            transport,
            message,
            timeout,
        } = self
        else {
            return Ok("local mailbox (DEMO)");
        };
        NETWORK.block_on(async {
            tokio::time::timeout(Duration::from_secs(timeout), transport.send(*message))
                .await
                .map_err(|_| AppError::validation("SMTP delivery timed out"))?
                .map_err(|e| AppError::validation(format!("SMTP delivery failed: {e}")))?;
            Ok("sent via SMTP")
        })
    }
}

pub fn configured(db: &Db) -> bool {
    db.config().is_none_or(|cfg| {
        cfg.mode != Mode::Production || (cfg.smtp_url.is_some() && cfg.mail_from.is_some())
    })
}

pub fn failed(conn: &Connection, id: i64, attempt: i64) -> AppResult<()> {
    let seconds = 60 * (1_i64 << (attempt - 1).clamp(0, 6));
    let at = crate::time::fmt_utc(crate::time::now() + time::Duration::seconds(seconds));
    conn.execute(
        "INSERT INTO mail_retries(dispatch_id,retry_at) VALUES(?1,?2) ON CONFLICT(dispatch_id) DO UPDATE SET retry_at=excluded.retry_at",
        params![id, at],
    )?;
    Ok(())
}

pub fn sent(conn: &Connection, id: i64, mailbox: i64, transport: &str) -> AppResult<()> {
    conn.execute("DELETE FROM mail_retries WHERE dispatch_id=?1", [id])?;
    conn.execute(
        "INSERT INTO mail_delivery_log(mailbox_id,transport) VALUES(?1,?2)",
        params![mailbox, transport],
    )?;
    Ok(())
}

/// Requeue only transport failures. Permission/document failures still require human review.
pub fn retry_due(db: &Db) -> AppResult<()> {
    if !db.config().is_some_and(|c| c.mode == Mode::Production) {
        return Ok(());
    }
    db.write_blocking(|tx| {
        let now = crate::time::now_utc();
        let mut stmt = tx.prepare("SELECT id,queued_by,case_id FROM dispatches WHERE status='failed' AND reviewed_by IS NOT NULL AND id IN (SELECT dispatch_id FROM mail_retries WHERE retry_at<=?1)")?;
        let rows = stmt.query_map([&now], |r|Ok((r.get::<_,i64>(0)?,r.get::<_,Option<i64>>(1)?,r.get::<_,Option<i64>>(2)?)))?.collect::<Result<Vec<_>,_>>()?;
        drop(stmt);
        for (id,uid,case) in rows {
            tx.execute("UPDATE dispatches SET status='queued',version=version+1 WHERE id=?1",[id])?;
            let actor = uid.map(|uid|crate::auth::load_actor(tx,uid,None)).transpose()?.flatten();
            crate::audit::record(tx,actor.as_ref(),crate::audit::Event::new("dispatch.retry_queued","dispatch",id,"SMTP retry queued after backoff").case(case))?;
        }
        Ok(())
    })
}
