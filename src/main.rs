use std::path::PathBuf;
use tuvalu_court::config::{Config, Mode};
use tuvalu_court::db::Db;
use tuvalu_court::error::{AppError, AppResult};

const USAGE: &str = "\
tuvalu-court — Tuvalu Court Register (independent prototype)

USAGE:
  tuvalu-court [--env-file <path>] [serve]                         run the web server (env: TCR_MODE, TCR_DATA_DIR, TCR_BIND, ...)
  tuvalu-court create-user <username> <display name> [--judge] [--perm P]...   (production DB; prompts for password on stdin)
  tuvalu-court grant <username> <permission>   grant any permission (court authority; not available in the admin UI)
  tuvalu-court revoke <username> <permission>
  tuvalu-court gen-key <keyfile>               create a backup encryption key (keep it OFF the server/repo)
  tuvalu-court backup <out.tcrb> <keyfile>     encrypted full backup of the production DB + files
  tuvalu-court restore <in.tcrb> <keyfile> <empty-data-dir>   restore and verify into an empty directory (requires --yes or installation-id confirmation)
  tuvalu-court verify-audit                    recompute the audit hash chain
";

fn production_db(cfg: &Config) -> AppResult<Db> {
    if cfg.mode != Mode::Production { return Err(AppError::validation("Maintenance commands require TCR_MODE=production and the server configuration.")); }
    let path = cfg.data_dir.join("court.sqlite");
    if !path.is_file() {
        return Err(AppError::validation("No initialised database in the configured data directory. Start the server with the same --env-file first."));
    }
    let conn = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == 0 { return Err(AppError::validation("Database is not initialised.")); }
    let installation = tuvalu_court::db::setting(&conn, "installation_id", "")?;
    if installation.is_empty() { return Err(AppError::validation("Start the upgraded server first to establish its installation id.")); }
    println!("Data directory: {}\nInstallation: {installation}", std::fs::canonicalize(&cfg.data_dir)?.display());
    Ok(Db::new(path, cfg.data_dir.join("files"), None))
}

fn user_id(db: &Db, username: &str) -> AppResult<i64> {
    db.open()?
        .query_row("SELECT id FROM users WHERE username = ?1", [username], |r| r.get(0))
        .map_err(|_| AppError::validation(format!("No user '{username}'")))
}

fn read_password() -> AppResult<String> {
    eprint!("Password (min 12 chars): ");
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    let s = s.trim_end_matches(['\n', '\r']).to_string();
    if s.chars().count() < 12 {
        return Err(AppError::validation("Password must be at least 12 characters."));
    }
    Ok(s)
}

fn run(mut args: Vec<String>) -> AppResult<()> {
    let mut env_file = None;
    while let Some(i) = args.iter().position(|a| a == "--env-file") {
        if env_file.is_some() || i + 1 >= args.len() { return Err(AppError::validation("Use --env-file <path> once.")); }
        env_file = Some(PathBuf::from(args.remove(i + 1)));
        args.remove(i);
    }
    let cfg = Config::load(env_file.as_deref())?;
    let cmd = args.first().map(String::as_str).unwrap_or("serve");
    match (cmd, &args[1.min(args.len())..]) {
        ("serve", _) => serve(cfg),
        ("create-user", [username, display, rest @ ..]) => {
            let db = production_db(&cfg)?;
            let password = read_password()?;
            let judge = rest.iter().any(|a| a == "--judge");
            let perms: Vec<String> =
                rest.windows(2).filter(|w| w[0] == "--perm").map(|w| w[1].clone()).collect();
            let id = tuvalu_court::seed::create_user(&db, username, display, &password, judge, &perms)?;
            println!("Created user #{id} '{username}'. They must enrol a sign-in code and change their temporary password at first login.");
            Ok(())
        }
        ("grant" | "revoke", [username, permission]) => {
            if !tuvalu_court::policy::perm::ALL.iter().any(|(p, _)| p == permission) {
                return Err(AppError::validation(format!("Unknown permission '{permission}'")));
            }
            let db = production_db(&cfg)?;
            let uid = user_id(&db, username)?;
            let (grant, perm) = (cmd == "grant", permission.clone());
            db.write_blocking(move |tx| {
                if grant {
                    tx.execute(
                        "INSERT OR IGNORE INTO user_permissions (user_id, permission, granted_at) VALUES (?1, ?2, ?3)",
                        rusqlite::params![uid, perm, tuvalu_court::time::now_utc()],
                    )?;
                } else {
                    tx.execute("DELETE FROM user_permissions WHERE user_id = ?1 AND permission = ?2", rusqlite::params![uid, perm])?;
                }
                tuvalu_court::audit::record(
                    tx,
                    None,
                    tuvalu_court::audit::Event::new(
                        if grant { "user.permission_granted" } else { "user.permission_revoked" },
                        "user",
                        uid,
                        format!("{} '{perm}' via command line", if grant { "Granted" } else { "Revoked" }),
                    ),
                )?;
                Ok(())
            })?;
            println!("{cmd} ok");
            Ok(())
        }
        ("gen-key", [keyfile]) => tuvalu_court::backup::gen_key(&PathBuf::from(keyfile)),
        ("backup", [out, keyfile]) => {
            let db = production_db(&cfg)?;
            let summary = tuvalu_court::backup::backup(&db, &PathBuf::from(out), &PathBuf::from(keyfile))?;
            println!("{summary}");
            Ok(())
        }
        ("restore", [input, keyfile, target, rest @ ..]) => {
            use std::io::{IsTerminal, Write};
            if rest.iter().any(|a| a != "--yes") { return Err(AppError::validation("Restore accepts only --yes after its paths.")); }
            // Restore reads an existing, initialised DB from the authenticated backup, never creates a source DB.
            let installation = tuvalu_court::backup::installation_id(&PathBuf::from(input), &PathBuf::from(keyfile), &PathBuf::from(target))?;
            let target_path = PathBuf::from(target);
            let resolved = if target_path.is_absolute() { target_path } else { std::env::current_dir()?.join(target_path) };
            println!("Restore data directory: {}\nInstallation from backup: {installation}", resolved.display());
            if !rest.iter().any(|a| a == "--yes") {
                if !std::io::stdin().is_terminal() { return Err(AppError::validation("Restore requires --yes or interactive confirmation of the installation id.")); }
                eprint!("Type installation id to restore: "); std::io::stderr().flush()?;
                let mut confirmation = String::new(); std::io::stdin().read_line(&mut confirmation)?;
                if confirmation.trim() != installation { return Err(AppError::validation("Installation id did not match.")); }
            }
            let summary =
                tuvalu_court::backup::restore(&PathBuf::from(input), &PathBuf::from(keyfile), &PathBuf::from(target))?;
            println!("{summary}");
            Ok(())
        }
        ("verify-audit", _) => {
            let db = production_db(&cfg)?;
            let (n, broken) = tuvalu_court::audit::verify_chain(&db.open()?)?;
            match broken {
                None => println!("Audit chain intact: {n} events."),
                Some(id) => {
                    println!("Audit chain BROKEN at event #{id} (checked {n}).");
                    std::process::exit(2);
                }
            }
            Ok(())
        }
        _ => {
            eprint!("{USAGE}");
            std::process::exit(64);
        }
    }
}

fn serve(cfg: Config) -> AppResult<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(16)
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let bind = cfg.bind.clone();
        let mode = cfg.mode;
        let (state, rx) = tuvalu_court::state::AppState::init(cfg)?;
        tokio::spawn(tuvalu_court::worker::run(state.clone(), rx));
        let app = tuvalu_court::api::router(state);
        let listener = tokio::net::TcpListener::bind(&bind).await?;
        tracing::info!("Tuvalu Court Register listening on http://{bind} ({})", if mode == Mode::Demo { "demo" } else { "production" });
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await?;
        Ok(())
    })
}

/// Ctrl-C or SIGTERM (systemd / docker stop).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    if let Err(e) = run(std::env::args().skip(1).collect()) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
