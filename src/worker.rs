//! Background worker: delivers queued dispatches (to the local mailbox) and sweeps demo sandboxes.
//! Delivery itself lives in `outbox::process`, which re-checks permissions before sending.

use crate::db::Db;
use crate::state::AppState;
use std::time::Duration;
use tokio::sync::mpsc;

async fn process(db: Db) {
    let path = db.path().display().to_string();
    match tokio::task::spawn_blocking(move || crate::outbox::process(&db)).await {
        Ok(Ok(0)) => {}
        Ok(Ok(n)) => tracing::info!("outbox: processed {n} dispatch(es) in {path}"),
        Ok(Err(e)) => tracing::warn!("outbox: {path}: {e}"),
        Err(e) => tracing::error!("outbox task panicked: {e}"),
    }
}

pub async fn run(state: AppState, mut rx: mpsc::UnboundedReceiver<Db>) {
    // Crash recovery: anything left queued from a previous run.
    if let Ok(dbs) = state.all_dbs() {
        for db in dbs {
            process(db).await;
        }
    }
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    let mut sweeps = 0u32;
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Some(db) => process(db).await,
                None => break,
            },
            _ = tick.tick() => {
                if let Some(db) = &state.main_db {
                    process(db.clone()).await;
                }
                sweeps += 1;
                if sweeps.is_multiple_of(60)
                    && let Some(mgr) = state.sandboxes.clone()
                {
                    match tokio::task::spawn_blocking(move || mgr.sweep()).await {
                        Ok(Ok(n)) if n > 0 => tracing::info!("removed {n} expired demo sandbox(es)"),
                        Ok(Err(e)) => tracing::warn!("sandbox sweep failed: {e}"),
                        _ => {}
                    }
                }
            }
        }
    }
}
