use std::path::PathBuf;

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
}

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Config {
    pub fn from_env() -> Self {
        let mode = match std::env::var("TCR_MODE").as_deref() {
            Ok("production") => Mode::Production,
            _ => Mode::Demo,
        };
        Self {
            mode,
            data_dir: PathBuf::from(std::env::var("TCR_DATA_DIR").unwrap_or_else(|_| "./data".into())),
            bind: std::env::var("TCR_BIND").unwrap_or_else(|_| "127.0.0.1:8088".into()),
            cookie_secure: env_or("TCR_COOKIE_SECURE", false),
            sandbox_ttl_hours: env_or("TCR_SANDBOX_TTL_HOURS", 72),
            max_sandboxes: env_or("TCR_MAX_SANDBOXES", 200),
            sandbox_quota_bytes: env_or::<u64>("TCR_SANDBOX_QUOTA_MB", 25) * 1024 * 1024,
            upload_max_bytes: env_or::<u64>("TCR_UPLOAD_MAX_MB", 15) * 1024 * 1024,
            session_hours: env_or("TCR_SESSION_HOURS", 12),
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
        }
    }
}
