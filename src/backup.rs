//! Encrypted technical backup / verified restore (CLI only).
use crate::db::Db;
use crate::error::{AppError, AppResult};
use std::path::Path;

pub fn gen_key(_keyfile: &Path) -> AppResult<()> {
    Err(AppError::internal("not yet implemented"))
}
pub fn backup(_db: &Db, _out: &Path, _keyfile: &Path) -> AppResult<String> {
    Err(AppError::internal("not yet implemented"))
}
pub fn restore(_input: &Path, _keyfile: &Path, _target: &Path) -> AppResult<String> {
    Err(AppError::internal("not yet implemented"))
}
