//! Sessions, passwords, TOTP and request extractors.

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const SESSION_COOKIE: &str = "tcr_session";
pub const SANDBOX_COOKIE: &str = "tcr_sandbox";
pub const CSRF_HEADER: &str = "x-tcr";
const MAX_FAILED_LOGINS: i64 = 5;
const LOCK_MINUTES: i64 = 15;

/// The authenticated user performing a request.
#[derive(Debug, Clone, Serialize)]
pub struct Actor {
    pub user_id: i64,
    pub username: String,
    pub display_name: String,
    pub is_judge: bool,
    pub perms: BTreeSet<String>,
    #[serde(skip)]
    pub ip: Option<String>,
}

impl Actor {
    pub fn has(&self, perm: &str) -> bool {
        self.perms.contains(perm)
    }
    pub fn require(&self, perm: &str) -> AppResult<()> {
        if self.has(perm) {
            Ok(())
        } else {
            Err(AppError::forbidden(format!("You need the '{perm}' permission for this action.")))
        }
    }
}

// ------------------------------------------------------------------ randomness & hashing

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("OS randomness");
    b
}

pub fn random_token() -> String {
    hex::encode(random_bytes::<32>())
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub fn hash_password(password: &str) -> AppResult<String> {
    use argon2::password_hash::PasswordHasher;
    argon2::Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| AppError::internal(format!("password hashing failed: {e}")))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    use argon2::password_hash::{PasswordVerifier, phc::PasswordHash};
    match PasswordHash::new(hash) {
        Ok(parsed) => argon2::Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok(),
        Err(_) => false,
    }
}

// ------------------------------------------------------------------ TOTP (RFC 6238, SHA-1, 30 s, 6 digits)

const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in data {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            out.push(B32[((buf >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(B32[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.chars().filter(|c| !c.is_whitespace() && *c != '=') {
        let v = B32.iter().position(|&x| x as char == c.to_ascii_uppercase())? as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            out.push((buf >> (bits - 8)) as u8);
            bits -= 8;
        }
    }
    Some(out)
}

pub fn totp_at(secret: &[u8], counter: u64) -> u32 {
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac = <Hmac<sha1::Sha1> as KeyInit>::new_from_slice(secret).expect("hmac key");
    mac.update(&counter.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let off = (h[h.len() - 1] & 0x0f) as usize;
    let bin = ((h[off] as u32 & 0x7f) << 24) | ((h[off + 1] as u32) << 16) | ((h[off + 2] as u32) << 8) | h[off + 3] as u32;
    bin % 1_000_000
}

/// Accepts the current 30-second step and one step either side. Returns the matched step, which
/// must be greater than the user's `totp_last_step` (a code can be used only once).
pub fn verify_totp(secret_b32: &str, code: &str, last_step: Option<i64>) -> Option<i64> {
    let secret = base32_decode(secret_b32)?;
    let code = code.trim().parse::<u32>().ok()?;
    let step = crate::time::now().unix_timestamp() as u64 / 30;
    [step.saturating_sub(1), step, step + 1]
        .into_iter()
        .find(|&c| totp_at(&secret, c) == code)
        .map(|c| c as i64)
        .filter(|&c| last_step.is_none_or(|l| c > l))
}

/// Current TOTP code for a secret (tests and the demo enrolment helper).
pub fn current_totp(secret_b32: &str) -> Option<String> {
    let secret = base32_decode(secret_b32)?;
    Some(format!("{:06}", totp_at(&secret, crate::time::now().unix_timestamp() as u64 / 30)))
}

// ------------------------------------------------------------------ sessions

pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

pub fn set_cookie(name: &str, value: &str, max_age_secs: i64, secure: bool) -> String {
    format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_secs}{}",
        if secure { "; Secure" } else { "" }
    )
}

/// Create a session row; returns the raw token for the cookie.
pub fn create_session(conn: &Connection, user_id: i64, mfa_ok: bool, hours: i64) -> AppResult<String> {
    let token = random_token();
    let now = crate::time::now_utc();
    conn.execute(
        "INSERT INTO sessions (token_hash, user_id, created_at, last_seen_at, expires_at, mfa_ok) VALUES (?1, ?2, ?3, ?3, ?4, ?5)",
        params![sha256_hex(token.as_bytes()), user_id, now, crate::time::utc_in_hours(hours), mfa_ok as i64],
    )?;
    Ok(token)
}

pub fn revoke_user_sessions(conn: &Connection, user_id: i64) -> AppResult<usize> {
    Ok(conn.execute(
        "UPDATE sessions SET revoked_at = ?2 WHERE user_id = ?1 AND revoked_at IS NULL",
        params![user_id, crate::time::now_utc()],
    )?)
}

pub fn load_actor(conn: &Connection, user_id: i64, ip: Option<String>) -> AppResult<Option<Actor>> {
    let row = conn
        .query_row(
            "SELECT id, username, display_name, is_judge FROM users WHERE id = ?1 AND active = 1",
            [user_id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, bool>(3)?)),
        )
        .optional()?;
    let Some((id, username, display_name, is_judge)) = row else { return Ok(None) };
    let mut stmt = conn.prepare_cached("SELECT permission FROM user_permissions WHERE user_id = ?1")?;
    let perms = stmt.query_map([id], |r| r.get::<_, String>(0))?.collect::<Result<BTreeSet<_>, _>>()?;
    Ok(Some(Actor { user_id: id, username, display_name, is_judge, perms, ip }))
}

pub enum SessionLookup {
    None,
    NeedsMfa(i64),
    Ok(Actor),
}

pub fn lookup_session(conn: &Connection, token: &str, ip: Option<String>) -> AppResult<SessionLookup> {
    let now = crate::time::now_utc();
    let row = conn
        .query_row(
            "SELECT user_id, mfa_ok, last_seen_at FROM sessions
             WHERE token_hash = ?1 AND revoked_at IS NULL AND expires_at > ?2",
            params![sha256_hex(token.as_bytes()), now],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?, r.get::<_, String>(2)?)),
        )
        .optional()?;
    let Some((user_id, mfa_ok, last_seen)) = row else { return Ok(SessionLookup::None) };
    let Some(actor) = load_actor(conn, user_id, ip)? else { return Ok(SessionLookup::None) };
    if !mfa_ok {
        return Ok(SessionLookup::NeedsMfa(user_id));
    }
    // Throttled last-seen update (best effort; failures under write contention are ignored).
    if last_seen.as_str() < crate::time::fmt_utc(crate::time::now() - time::Duration::minutes(5)).as_str() {
        let _ = conn.execute(
            "UPDATE sessions SET last_seen_at = ?2 WHERE token_hash = ?1",
            params![sha256_hex(token.as_bytes()), now],
        );
    }
    Ok(SessionLookup::Ok(actor))
}

/// Brute-force protection. Returns Err when the account or IP is temporarily locked.
pub fn check_login_allowed(conn: &Connection, username: &str, ip: Option<&str>) -> AppResult<()> {
    let since = crate::time::fmt_utc(crate::time::now() - time::Duration::minutes(LOCK_MINUTES));
    let user_fails: i64 = conn.query_row(
        "SELECT COUNT(*) FROM login_attempts WHERE username = ?1 AND success = 0 AND at > ?2",
        params![username, since],
        |r| r.get(0),
    )?;
    let ip_fails: i64 = match ip {
        Some(ip) => conn.query_row(
            "SELECT COUNT(*) FROM login_attempts WHERE ip = ?1 AND success = 0 AND at > ?2",
            params![ip, since],
            |r| r.get(0),
        )?,
        None => 0,
    };
    if user_fails >= MAX_FAILED_LOGINS || ip_fails >= MAX_FAILED_LOGINS * 4 {
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "locked",
            format!("Too many failed attempts. Try again in {LOCK_MINUTES} minutes."),
        ));
    }
    Ok(())
}

pub fn record_login_attempt(conn: &Connection, username: &str, ip: Option<&str>, success: bool) -> AppResult<()> {
    conn.execute(
        "INSERT INTO login_attempts (username, ip, at, success) VALUES (?1, ?2, ?3, ?4)",
        params![username, ip, crate::time::now_utc(), success as i64],
    )?;
    Ok(())
}

// ------------------------------------------------------------------ extractors

fn client_ip(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_string())
        .or_else(|| parts.headers.get("x-real-ip").and_then(|v| v.to_str().ok()).map(str::to_string))
}

fn check_csrf(parts: &Parts) -> AppResult<()> {
    if matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return Ok(());
    }
    // Same-origin check: when the browser sends Origin, its host must equal the requested Host.
    if let Some(origin) = parts.headers.get(axum::http::header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let origin_host = origin.split_once("://").map(|(_, h)| h).unwrap_or(origin);
        let host = parts.headers.get(axum::http::header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
        if origin_host != host {
            return Err(AppError::new(StatusCode::FORBIDDEN, "csrf", "Cross-origin request rejected."));
        }
    }
    match parts.headers.get(CSRF_HEADER).and_then(|v| v.to_str().ok()) {
        Some("1") => Ok(()),
        _ => Err(AppError::new(StatusCode::FORBIDDEN, "csrf", "Missing request header X-TCR.")),
    }
}

/// Database of the caller (production DB or the visitor's own sandbox), no authentication.
pub struct DbCtx {
    pub db: Db,
    pub ip: Option<String>,
    pub sandbox_id: Option<String>,
}

impl FromRequestParts<AppState> for DbCtx {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        check_csrf(parts)?;
        let (db, sandbox_id) = state.resolve_db(&parts.headers)?;
        Ok(DbCtx { db, ip: client_ip(parts), sandbox_id })
    }
}

/// Authenticated request context: database + actor.
pub struct Ctx {
    pub db: Db,
    pub actor: Actor,
}

impl FromRequestParts<AppState> for Ctx {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let DbCtx { db, ip, .. } = DbCtx::from_request_parts(parts, state).await?;
        let Some(token) = cookie_value(&parts.headers, SESSION_COOKIE) else {
            return Err(AppError::unauthenticated());
        };
        let lookup = db.read(move |c| lookup_session(c, &token, ip)).await?;
        match lookup {
            SessionLookup::Ok(actor) => Ok(Ctx { db, actor }),
            SessionLookup::NeedsMfa(_) => {
                Err(AppError::new(StatusCode::UNAUTHORIZED, "mfa_required", "Enter your sign-in code."))
            }
            SessionLookup::None => Err(AppError::unauthenticated()),
        }
    }
}

/// `Idempotency-Key` request header (optional).
pub struct IdemKey(pub Option<String>);

impl<S: Send + Sync> FromRequestParts<S> for IdemKey {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let key = parts.headers.get("idempotency-key").and_then(|v| v.to_str().ok()).map(str::to_string);
        if let Some(k) = &key
            && (k.is_empty() || k.len() > 100)
        {
            return Err(AppError::validation("Invalid Idempotency-Key."));
        }
        Ok(IdemKey(key))
    }
}

/// Idempotent operation helper (call inside a write transaction, after authorization checks so a
/// replay is re-authorised). If `key` was already used by this user for the same operation and the
/// same request body, returns the stored result instead of running `op` again; a different body → 409.
pub fn idempotent<T, F>(
    tx: &Connection,
    actor: &Actor,
    key: &Option<String>,
    operation: &str,
    request: &impl Serialize,
    op: F,
) -> AppResult<serde_json::Value>
where
    T: Serialize,
    F: FnOnce() -> AppResult<T>,
{
    let request_hash = sha256_hex(serde_json::to_string(request)?.as_bytes());
    if let Some(k) = key {
        let stored: Option<(String, String, String)> = tx
            .query_row(
                "SELECT operation, request_hash, result_json FROM operation_keys WHERE user_id = ?1 AND key = ?2",
                params![actor.user_id, k],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((op_name, hash, json)) = stored {
            if op_name != operation || hash != request_hash {
                return Err(AppError::conflict(
                    "idempotency_mismatch",
                    "This request key was already used for a different request.",
                ));
            }
            return Ok(serde_json::from_str(&json)?);
        }
    }
    let value = serde_json::to_value(op()?)?;
    if let Some(k) = key {
        tx.execute(
            "INSERT INTO operation_keys (key, user_id, operation, request_hash, result_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![k, actor.user_id, operation, request_hash, value.to_string(), crate::time::now_utc()],
        )?;
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totp_rfc6238_vector() {
        // RFC 6238 SHA-1 secret "12345678901234567890", T=59s → 94287082 (8 digits) → 287082 (6 digits)
        assert_eq!(totp_at(b"12345678901234567890", 59 / 30), 287082);
    }

    #[test]
    fn base32_roundtrip() {
        let data = random_bytes::<20>();
        assert_eq!(base32_decode(&base32_encode(&data)).unwrap(), data);
    }

    #[test]
    fn password_hash_verifies() {
        let h = hash_password("correct horse").unwrap();
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("wrong", &h));
    }
}
