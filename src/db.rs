use crate::error::{AppError, AppResult};
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Ordered schema migrations; `PRAGMA user_version` stores how many were applied.
const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001_init.sql"),
    include_str!("migrations/0002_hearings.sql"),
    include_str!("migrations/0003_documents.sql"),
    include_str!("migrations/0004_dispatch.sql"),
    include_str!("migrations/0005_reports.sql"),
    include_str!("migrations/0006_admin.sql"),
    include_str!("migrations/0007_access.sql"),
    include_str!("migrations/0008_audit_judicial.sql"),
    include_str!("migrations/0009_audit_workflow.sql"),
    include_str!("migrations/0010_audit_install.sql"),
    include_str!("migrations/0012_outbox_claim.sql"),
];

/// Handle to one court database (production DB or one demo sandbox) and its private file store.
#[derive(Clone, Debug)]
pub struct Db {
    inner: Arc<DbInner>,
    runtime: Option<Arc<crate::config::Config>>,
}

#[derive(Debug)]
struct DbInner {
    db_path: PathBuf,
    files_dir: PathBuf,
    /// Total stored-bytes quota for this database's files (demo sandboxes only).
    quota_bytes: Option<u64>,
}

impl Db {
    pub fn new(db_path: PathBuf, files_dir: PathBuf, quota_bytes: Option<u64>) -> Self {
        Self { inner: Arc::new(DbInner { db_path, files_dir, quota_bytes }), runtime: None }
    }

    pub fn with_config(mut self, cfg: Arc<crate::config::Config>) -> Self {
        self.runtime = Some(cfg);
        self
    }
    pub fn config(&self) -> Option<&crate::config::Config> { self.runtime.as_deref() }

    pub fn path(&self) -> &Path {
        &self.inner.db_path
    }
    pub fn files_dir(&self) -> &Path {
        &self.inner.files_dir
    }
    pub fn quota_bytes(&self) -> Option<u64> {
        self.inner.quota_bytes
    }

    /// Open a connection with the project-wide pragmas.
    pub fn open(&self) -> AppResult<Connection> {
        open_conn(&self.inner.db_path)
    }

    /// Create the file layout and apply pending migrations.
    pub fn init(&self) -> AppResult<()> {
        if let Some(parent) = self.inner.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(&self.inner.files_dir)?;
        let mut conn = self.open()?;
        migrate(&mut conn)
    }

    /// Run a read-only unit of work on the blocking pool.
    pub async fn read<T, F>(&self, f: F) -> AppResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> AppResult<T> + Send + 'static,
    {
        let db = self.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.open()?;
            f(&conn)
        })
        .await
        .map_err(|e| AppError::internal(format!("worker join error: {e}")))?
    }

    /// Run a write unit of work inside `BEGIN IMMEDIATE` (single writer) on the blocking pool.
    /// Commit on `Ok`, roll back on `Err`. State change + audit + outbox rows MUST share one call.
    pub async fn write<T, F>(&self, f: F) -> AppResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction) -> AppResult<T> + Send + 'static,
    {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.write_blocking(f))
            .await
            .map_err(|e| AppError::internal(format!("worker join error: {e}")))?
    }

    /// Synchronous variant of [`Db::write`] for CLI/seed code.
    pub fn write_blocking<T, F>(&self, f: F) -> AppResult<T>
    where
        F: FnOnce(&Transaction) -> AppResult<T>,
    {
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }
}

pub fn open_conn(path: &Path) -> AppResult<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL; PRAGMA temp_store=MEMORY;",
    )?;
    Ok(conn)
}

pub fn migrate(conn: &mut Connection) -> AppResult<()> {
    let applied: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(applied as usize) {
        // Installation migration rebuilds a CHECK constraint without renaming FK targets.
        let rebuild = sql.contains("-- rebuild document_versions");
        if rebuild { conn.execute_batch("PRAGMA foreign_keys=OFF;")?; }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
        if rebuild {
            conn.execute_batch("PRAGMA foreign_keys=ON;")?;
            let broken = conn.prepare("PRAGMA foreign_key_check")?.exists([])?;
            if broken { return Err(AppError::internal("Migration failed foreign-key verification")); }
        }
    }
    Ok(())
}

/// Settings helper with default.
pub fn setting(conn: &Connection, key: &str, default: &str) -> AppResult<String> {
    let v: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
        .map(Some)
        .or_else(|e| if matches!(e, rusqlite::Error::QueryReturnedNoRows) { Ok(None) } else { Err(e) })?;
    Ok(v.unwrap_or_else(|| default.to_string()))
}

/// Optimistic-locking helper: bumps `version` when it matches, else returns `Ok(false)`.
pub fn bump_version(tx: &Transaction, table: &str, id: i64, expected: i64) -> AppResult<bool> {
    let n = tx.execute(
        &format!("UPDATE {table} SET version = version + 1 WHERE id = ?1 AND version = ?2"),
        rusqlite::params![id, expected],
    )?;
    Ok(n == 1)
}
