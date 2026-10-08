use crate::config::{Config, Mode};
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::sandbox::SandboxManager;
use axum::http::HeaderMap;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    /// Production database (production mode only).
    pub main_db: Option<Db>,
    /// Demo sandboxes (demo mode only).
    pub sandboxes: Option<Arc<SandboxManager>>,
    /// Wake the outbox worker for a database after queuing dispatches.
    pub outbox: mpsc::UnboundedSender<Db>,
}

impl AppState {
    /// Initialise storage for the configured mode. Returns the state and the outbox receiver
    /// (to be handed to `worker::run`).
    pub fn init(cfg: Config) -> AppResult<(Self, mpsc::UnboundedReceiver<Db>)> {
        cfg.validate()?;
        std::fs::create_dir_all(&cfg.data_dir)?;
        let (tx, rx) = mpsc::unbounded_channel();
        let (main_db, sandboxes) = match cfg.mode {
            Mode::Production => {
                let db = Db::new(cfg.data_dir.join("court.sqlite"), cfg.data_dir.join("files"), None);
                let db = db.with_config(Arc::new(cfg.clone()));
                db.init()?;
                crate::seed::seed_reference(&db)?;
                crate::scan::recover_pending(&db)?;
                db.write_blocking(|tx| {
                    for (key,value) in [("mail_transport",crate::mail::status(&cfg)),("file_scanner",cfg.scanner_status())] {
                        tx.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",rusqlite::params![key,value])?;
                    }
                    tx.execute("UPDATE ref_items SET label='E-mail' WHERE kind='dispatch_method' AND code='email'", [])?;
                    Ok(())
                })?;
                (Some(db), None)
            }
            Mode::Demo => {
                let mgr = SandboxManager::init(
                    &cfg.data_dir,
                    cfg.sandbox_ttl_hours,
                    cfg.max_sandboxes,
                    cfg.sandbox_quota_bytes,
                )?;
                (None, Some(Arc::new(mgr)))
            }
        };
        Ok((Self { cfg: Arc::new(cfg), main_db, sandboxes, outbox: tx }, rx))
    }

    pub fn is_demo(&self) -> bool {
        self.cfg.mode == Mode::Demo
    }

    /// Database for this request: the production DB, or the caller's own sandbox.
    pub fn resolve_db(&self, headers: &HeaderMap) -> AppResult<(Db, Option<String>)> {
        if let Some(db) = &self.main_db {
            return Ok((db.clone(), None));
        }
        let mgr = self.sandboxes.as_ref().ok_or_else(|| AppError::internal("no storage configured"))?;
        let id = crate::auth::cookie_value(headers, crate::auth::SANDBOX_COOKIE)
            .ok_or_else(|| AppError::new(axum::http::StatusCode::UNAUTHORIZED, "no_sandbox", "Start a demo session first."))?;
        Ok((mgr.get(&id)?.with_config(self.cfg.clone()), Some(id)))
    }

    /// Ask the worker to process queued dispatches of `db`.
    pub fn kick_outbox(&self, db: &Db) {
        let _ = self.outbox.send(db.clone());
    }

    /// Every database the worker should scan on startup.
    pub fn all_dbs(&self) -> AppResult<Vec<Db>> {
        match (&self.main_db, &self.sandboxes) {
            (Some(db), _) => Ok(vec![db.clone()]),
            (None, Some(mgr)) => Ok(mgr.all()?.into_iter().map(|db| db.with_config(self.cfg.clone())).collect()),
            _ => Ok(vec![]),
        }
    }
}
