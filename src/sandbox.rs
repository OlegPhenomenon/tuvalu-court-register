//! Demo mode: every visitor gets an isolated copy of the seeded template database + files.
//! A visitor can only address their own sandbox (random 128-bit id in an HttpOnly cookie, validated
//! as 32 lowercase hex chars before it ever touches a path); resetting replaces only that sandbox.
//! Idle sandboxes expire after the TTL. When the cap is reached, only sandboxes idle for more than
//! `EVICT_IDLE` are evicted; active visitors are never evicted — new visitors are refused instead.
//! This deletion applies to fictional demo datasets only; production mode never deletes cases.

use crate::db::Db;
use crate::error::{AppError, AppResult};
use axum::http::StatusCode;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const EVICT_IDLE: Duration = Duration::from_secs(2 * 3600);
const TOUCH_EVERY: Duration = Duration::from_secs(600);

pub struct SandboxManager {
    root: PathBuf,
    template: PathBuf,
    ttl: Duration,
    max: usize,
    quota_bytes: u64,
    /// id → last activity; presence also means "migrations already checked in this process".
    seen: Mutex<HashMap<String, SystemTime>>,
    /// Serialises create/reset/sweep so they never race on the same directory.
    admin: Mutex<()>,
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn no_sandbox() -> AppError {
    AppError::new(StatusCode::UNAUTHORIZED, "no_sandbox", "Start a demo session first.")
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if ft.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

impl SandboxManager {
    /// Build (or rebuild) the template from migrations + demo seed, then sweep expired sandboxes.
    pub fn init(data_dir: &Path, ttl_hours: i64, max: usize, quota_bytes: u64) -> AppResult<Self> {
        let root = data_dir.join("sandboxes");
        let template = data_dir.join("template");
        std::fs::create_dir_all(&root)?;
        if template.exists() {
            std::fs::remove_dir_all(&template)?;
        }
        let tdb = Db::new(template.join("db.sqlite"), template.join("files"), None);
        tdb.init()?;
        crate::seed::seed_reference(&tdb)?;
        crate::seed::seed_demo(&tdb)?;
        // Fold the WAL into the main file so a plain file copy is a consistent, closed snapshot.
        tdb.open()?.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
        let mgr = Self {
            root,
            template,
            ttl: Duration::from_secs(ttl_hours.max(1) as u64 * 3600),
            max: max.max(1),
            quota_bytes,
            seen: Mutex::new(HashMap::new()),
            admin: Mutex::new(()),
        };
        mgr.sweep()?;
        Ok(mgr)
    }

    fn dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    fn db_for(&self, id: &str) -> Db {
        let dir = self.dir(id);
        Db::new(dir.join("db.sqlite"), dir.join("files"), Some(self.quota_bytes))
    }

    fn last_activity(&self, id: &str) -> Option<SystemTime> {
        if let Some(t) = self.seen.lock().get(id) {
            return Some(*t);
        }
        std::fs::metadata(self.dir(id).join(".touch")).and_then(|m| m.modified()).ok()
    }

    fn idle_for(&self, id: &str) -> Option<Duration> {
        self.last_activity(id).map(|t| SystemTime::now().duration_since(t).unwrap_or_default())
    }

    /// Resolve an existing, non-expired sandbox.
    pub fn get(&self, id: &str) -> AppResult<Db> {
        if !valid_id(id) {
            return Err(no_sandbox());
        }
        match self.idle_for(id) {
            Some(idle) if idle <= self.ttl => {}
            _ => return Err(no_sandbox()),
        }
        let db = self.db_for(id);
        let first_in_process = !self.seen.lock().contains_key(id);
        if first_in_process {
            db.init()?; // apply migrations added since the sandbox was created
        }
        self.touch(id, first_in_process)?;
        Ok(db)
    }

    fn touch(&self, id: &str, force_file: bool) -> AppResult<()> {
        let now = SystemTime::now();
        let prev = self.seen.lock().insert(id.to_string(), now);
        let stale = prev.is_none_or(|p| now.duration_since(p).unwrap_or_default() > TOUCH_EVERY);
        if force_file || stale {
            std::fs::write(self.dir(id).join(".touch"), b"")?;
        }
        Ok(())
    }

    /// Create a fresh sandbox. At capacity, evicts the longest-idle sandbox only if it has been idle
    /// for more than two hours; otherwise refuses with 503.
    pub fn create(&self) -> AppResult<(String, Db)> {
        let _guard = self.admin.lock();
        self.sweep_locked()?;
        let mut ids = self.list()?;
        if ids.len() >= self.max {
            ids.sort_by_key(|(_, t)| *t);
            let (oldest, last) = &ids[0];
            if SystemTime::now().duration_since(*last).unwrap_or_default() < EVICT_IDLE {
                return Err(AppError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "demo_capacity",
                    "The demo is at capacity right now. Please try again later.",
                ));
            }
            self.remove(oldest)?;
        }
        let id = hex::encode(crate::auth::random_bytes::<16>());
        copy_dir(&self.template, &self.dir(&id))?;
        self.touch(&id, true)?;
        Ok((id.clone(), self.db_for(&id)))
    }

    /// Replace the caller's own sandbox with a fresh template copy (same id).
    pub fn reset(&self, id: &str) -> AppResult<Db> {
        let _guard = self.admin.lock();
        if !valid_id(id) || !self.dir(id).exists() {
            return Err(no_sandbox());
        }
        self.remove(id)?;
        copy_dir(&self.template, &self.dir(id))?;
        self.touch(id, true)?;
        Ok(self.db_for(id))
    }

    fn remove(&self, id: &str) -> AppResult<()> {
        debug_assert!(valid_id(id));
        self.seen.lock().remove(id);
        let dir = self.dir(id);
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        Ok(())
    }

    fn list(&self) -> AppResult<Vec<(String, SystemTime)>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let name = entry?.file_name().to_string_lossy().to_string();
            if valid_id(&name) {
                let t = self.last_activity(&name).unwrap_or(SystemTime::UNIX_EPOCH);
                out.push((name, t));
            }
        }
        Ok(out)
    }

    /// Delete expired sandboxes. Returns how many were removed.
    pub fn sweep(&self) -> AppResult<usize> {
        let _guard = self.admin.lock();
        self.sweep_locked()
    }

    fn sweep_locked(&self) -> AppResult<usize> {
        let now = SystemTime::now();
        let mut n = 0;
        for (id, t) in self.list()? {
            if now.duration_since(t).unwrap_or_default() > self.ttl {
                self.remove(&id)?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// All live sandbox databases (used by the outbox worker on startup).
    pub fn all(&self) -> AppResult<Vec<Db>> {
        Ok(self.list()?.into_iter().map(|(id, _)| self.db_for(&id)).collect())
    }
}
