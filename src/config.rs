use crate::error::{AppError, AppResult};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Public showcase: one isolated sandbox per visitor, persona switching.
    Demo,
    /// Single court installation: password + TOTP sign-in.
    Production,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub mode: Mode,
    pub data_dir: PathBuf,
    pub bind: String,
    pub cookie_secure: bool,
    pub sandbox_ttl_hours: i64,
    pub max_sandboxes: usize,
    pub sandbox_quota_bytes: u64,
    pub upload_max_bytes: u64,
    pub session_hours: i64,
    pub smtp_url: Option<String>,
    pub mail_from: Option<String>,
    pub smtp_timeout_secs: u64,
    /// Additional CA for a private SMTP installation; webpki roots remain enabled.
    pub smtp_ca_pem: Option<Vec<u8>>,
    pub clamd: Option<String>,
    pub scan_timeout_ms: u64,
    pub av_off: bool,
}

impl Config {
    pub fn from_env() -> Self {
        Self::from_values(&std::env::vars().collect())
    }

    /// Same literal KEY=value file used by systemd. Explicit file values take precedence.
    /// Shell commands/interpolation are never executed.
    pub fn load(env_file: Option<&Path>) -> AppResult<Self> {
        let mut values: BTreeMap<String, String> = std::env::vars().collect();
        let fallback = values.get("TCR_ENV_FILE").map(PathBuf::from);
        if let Some(path) = env_file.or(fallback.as_deref()) {
            for (line_no, line) in std::fs::read_to_string(path)?.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let line = line.strip_prefix("export ").unwrap_or(line);
                let (key, value) = line
                    .split_once('=')
                    .ok_or_else(|| AppError::validation(format!("Invalid env file line {}", line_no + 1)))?;
                let key = key.trim();
                if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    return Err(AppError::validation(format!("Invalid env file key on line {}", line_no + 1)));
                }
                let value = value.trim();
                let value = if value.starts_with(['\"', '\'']) {
                    let quote = value.as_bytes()[0] as char;
                    value
                        .strip_prefix(quote)
                        .and_then(|v| v.strip_suffix(quote))
                        .ok_or_else(|| AppError::validation(format!("Unclosed quote on env file line {}", line_no + 1)))?
                } else {
                    value
                };
                values.insert(key.to_string(), value.to_string());
            }
        }
        Ok(Self::from_values(&values))
    }

    fn from_values(v: &BTreeMap<String, String>) -> Self {
        fn number<T: std::str::FromStr>(v: &BTreeMap<String, String>, key: &str, default: T) -> T {
            v.get(key).and_then(|v| v.parse().ok()).unwrap_or(default)
        }
        let mode = if v.get("TCR_MODE").is_some_and(|v| v == "production") {
            Mode::Production
        } else {
            Mode::Demo
        };
        let optional = |k: &str| v.get(k).filter(|v| !v.trim().is_empty()).cloned();
        if mode == Mode::Demo && (optional("TCR_SMTP_URL").is_some() || optional("TCR_MAIL_FROM").is_some()) {
            tracing::warn!("SMTP configuration ignored: demo always uses the local mailbox");
        }
        Self {
            mode,
            data_dir: PathBuf::from(v.get("TCR_DATA_DIR").map(String::as_str).unwrap_or("./data")),
            bind: v.get("TCR_BIND").cloned().unwrap_or_else(|| "127.0.0.1:8088".into()),
            cookie_secure: number(v, "TCR_COOKIE_SECURE", false),
            sandbox_ttl_hours: number(v, "TCR_SANDBOX_TTL_HOURS", 72),
            max_sandboxes: number(v, "TCR_MAX_SANDBOXES", 200),
            sandbox_quota_bytes: number::<u64>(v, "TCR_SANDBOX_QUOTA_MB", 25) * 1024 * 1024,
            upload_max_bytes: number::<u64>(v, "TCR_UPLOAD_MAX_MB", 15) * 1024 * 1024,
            session_hours: number(v, "TCR_SESSION_HOURS", 12),
            smtp_url: optional("TCR_SMTP_URL"),
            mail_from: optional("TCR_MAIL_FROM"),
            smtp_timeout_secs: number::<u64>(v, "TCR_SMTP_TIMEOUT_SECS", 20).clamp(1, 120),
            smtp_ca_pem: None,
            clamd: optional("TCR_CLAMD"),
            scan_timeout_ms: number::<u64>(v, "TCR_SCAN_TIMEOUT_MS", 10000).clamp(10, 120000),
            av_off: v.get("TCR_AV").is_some_and(|v| v == "off"),
        }
    }

    pub fn validate(&self) -> AppResult<()> {
        if self.mode == Mode::Production && self.clamd.is_none() && !self.av_off {
            return Err(AppError::validation(
                "Production requires TCR_CLAMD or explicit TCR_AV=off (format checks only, no antivirus).",
            ));
        }
        if self.mode == Mode::Production
            && let Some(endpoint) = &self.clamd
        {
            crate::scan::validate_endpoint(endpoint)?;
        }
        crate::mail::validate(self)
    }

    pub fn scanner_status(&self) -> &'static str {
        if self.mode == Mode::Demo {
            "DEMO: format checks only, no antivirus"
        } else if self.clamd.is_some() {
            "ClamAV configured; files unavailable until a clean verdict"
        } else {
            "Format checks only, no antivirus (TCR_AV=off)"
        }
    }

    /// Config for tests: given data dir and mode, everything else default.
    pub fn for_tests(data_dir: PathBuf, mode: Mode) -> Self {
        Self {
            mode,
            data_dir,
            bind: "127.0.0.1:0".into(),
            cookie_secure: false,
            sandbox_ttl_hours: 72,
            max_sandboxes: 50,
            sandbox_quota_bytes: 25 * 1024 * 1024,
            upload_max_bytes: 15 * 1024 * 1024,
            session_hours: 12,
            smtp_url: None,
            mail_from: None,
            smtp_timeout_secs: 2,
            smtp_ca_pem: None,
            clamd: None,
            scan_timeout_ms: 100,
            av_off: true,
        }
    }
}
