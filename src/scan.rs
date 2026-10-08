//! ClamAV INSTREAM hook. A configured scanner must explicitly return OK; every other outcome fails closed.
use crate::{
    config::{Config, Mode},
    error::{AppError, AppResult},
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub fn validate_endpoint(endpoint: &str) -> AppResult<()> {
    if let Some(address) = endpoint.strip_prefix("tcp://") {
        if address
            .rsplit_once(':')
            .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok_and(|p| p > 0))
        {
            return Ok(());
        }
    }
    #[cfg(unix)]
    if endpoint.strip_prefix("unix:").is_some_and(|p| p.starts_with('/') && p.len() > 1) {
        return Ok(());
    }
    Err(AppError::validation("TCR_CLAMD requires tcp://host:port or unix:/absolute/path"))
}

async fn stream<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(mut socket: S, bytes: &[u8]) -> Result<(), String> {
    socket.write_all(b"zINSTREAM\0").await.map_err(|_| "Scanner write failed")?;
    for chunk in bytes.chunks(64 * 1024) {
        socket
            .write_all(&(chunk.len() as u32).to_be_bytes())
            .await
            .map_err(|_| "Scanner write failed")?;
        socket.write_all(chunk).await.map_err(|_| "Scanner write failed")?;
    }
    socket.write_all(&0u32.to_be_bytes()).await.map_err(|_| "Scanner write failed")?;
    let mut reply = Vec::new();
    loop {
        let byte = socket.read_u8().await.map_err(|_| "Scanner closed without a verdict")?;
        if byte == 0 || byte == b'\n' {
            break;
        }
        if reply.len() >= 4096 {
            return Err("Scanner reply too large".into());
        }
        reply.push(byte);
    }
    let reply = std::str::from_utf8(&reply).map_err(|_| "Invalid scanner reply")?;
    if reply.trim() == "stream: OK" {
        Ok(())
    } else if reply.ends_with(" FOUND") {
        Err("ClamAV detected infected content".into())
    } else {
        Err("Scanner did not return a clean verdict".into())
    }
}

pub async fn check(endpoint: &str, bytes: &[u8], timeout_ms: u64) -> Result<(), String> {
    tokio::time::timeout(Duration::from_millis(timeout_ms), async {
        if let Some(address) = endpoint.strip_prefix("tcp://") {
            let socket = tokio::net::TcpStream::connect(address)
                .await
                .map_err(|_| "Scanner connection failed")?;
            return stream(socket, bytes).await;
        }
        #[cfg(unix)]
        if let Some(path) = endpoint.strip_prefix("unix:") {
            let socket = tokio::net::UnixStream::connect(path)
                .await
                .map_err(|_| "Scanner connection failed")?;
            return stream(socket, bytes).await;
        }
        Err("Invalid scanner endpoint".into())
    })
    .await
    .map_err(|_| "Scanner timed out".to_string())?
}

/// Used from the blocking storage/legacy-import unit of work. No clean result is published before this returns.
pub fn verdict(cfg: Option<&Config>, bytes: &[u8]) -> Result<Option<String>, String> {
    let Some(cfg) = cfg.filter(|c| c.mode == Mode::Production) else {
        return Ok(Some("DEMO: format checks only, no antivirus".into()));
    };
    let Some(endpoint) = &cfg.clamd else {
        return Ok(Some("Format checks only, no antivirus (TCR_AV=off)".into()));
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Scanner runtime failed".to_string())?;
    rt.block_on(check(endpoint, bytes, cfg.scan_timeout_ms))?;
    Ok(Some("Format checks and ClamAV: clean verdict".into()))
}

/// Complete one committed HTTP upload. Pending versions after a process crash remain unavailable;
/// they can be submitted as a new version after the scanner recovers.
pub async fn finish(db: crate::db::Db, key: String) -> AppResult<Option<(i64, String, Option<String>)>> {
    use rusqlite::{OptionalExtension, params};
    let read_db = db.clone();
    let pending = tokio::task::spawn_blocking(move || -> AppResult<_> {
        let conn = read_db.open()?;
        let row = conn.query_row("SELECT v.id,v.sha256,d.id,d.case_id,v.uploaded_by FROM document_versions v JOIN documents d ON d.id=v.document_id WHERE v.storage_key=?1 AND v.scan_status='pending_scan'", [&key],
            |r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,Option<i64>>(3)?,r.get::<_,i64>(4)?))).optional()?;
        Ok(row.map(|(id,sha,doc,case,uid)| (id,crate::storage::read_for_scan(&read_db,&key,&sha),doc,case,uid)))
    }).await.map_err(|e|AppError::internal(e.to_string()))??;
    let Some((id, bytes, doc, case, uid)) = pending else {
        return Ok(None);
    };
    let result = match (bytes, db.config().and_then(|c| c.clamd.as_deref())) {
        (Ok(bytes), Some(endpoint)) => check(endpoint, &bytes, db.config().unwrap().scan_timeout_ms).await,
        (Err(_), _) => Err("File integrity check failed before scanning".into()),
        _ => Err("Scanner no longer configured".into()),
    };
    let (status, note) = match result {
        Ok(()) => ("clean".to_string(), Some("Format checks and ClamAV: clean verdict".to_string())),
        Err(note) => ("quarantined".to_string(), Some(note)),
    };
    let (save_status, save_note) = (status.clone(), note.clone());
    db.write(move |tx| {
        let changed = tx.execute(
            "UPDATE document_versions SET scan_status=?2,scan_note=?3 WHERE id=?1 AND scan_status='pending_scan'",
            params![id, save_status, save_note],
        )?;
        if changed > 0 {
            let actor = crate::auth::load_actor(tx, uid, None)?;
            crate::audit::record(
                tx,
                actor.as_ref(),
                crate::audit::Event::new("document.scanned", "document", doc, "File safety check completed")
                    .case(case)
                    .details(serde_json::json!({"document_version_id":id,"scan_status":save_status,"scan_note":save_note})),
            )?;
        }
        Ok(())
    })
    .await?;
    Ok(Some((id, status, note)))
}

/// A process interruption never upgrades a pending file to clean.
pub fn recover_pending(db: &crate::db::Db) -> AppResult<()> {
    db.write_blocking(|tx| {
        let mut stmt=tx.prepare("SELECT v.id,d.id,d.case_id FROM document_versions v JOIN documents d ON d.id=v.document_id WHERE v.scan_status='pending_scan'")?;
        let rows=stmt.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<i64>>(2)?)))?.collect::<Result<Vec<_>,_>>()?;
        drop(stmt);
        for (version,doc,case) in rows {
            tx.execute("UPDATE document_versions SET scan_status='quarantined',scan_note='Scanner interrupted by server restart; submit a new version for checking' WHERE id=?1",[version])?;
            crate::audit::record(tx,None,crate::audit::Event::new("document.scanned","document",doc,"File quarantined after interrupted safety check").case(case)
                .details(serde_json::json!({"document_version_id":version,"scan_status":"quarantined"})))?;
        }
        Ok(())
    })
}
