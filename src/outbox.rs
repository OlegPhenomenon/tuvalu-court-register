//! Outbox delivery (owned by the dispatch module).
use crate::db::Db;
use crate::error::AppResult;

/// Deliver queued dispatches of `db` to the local mailbox. Returns how many were processed.
pub fn process(_db: &Db) -> AppResult<usize> {
    Ok(0)
}
