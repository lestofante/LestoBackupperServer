// Runs the server crate's disk housekeeping (server::housekeeping) in the
// background: once at startup, then daily. Configured by environment
// variables, logged at start.

use std::time::{Duration, Instant};

use crate::dispatch::SharedState;
use crate::human::Bytes;
use crate::config::env_f64;

const RUN_EVERY: Duration = Duration::from_secs(24 * 3600);
const DAY: f64 = 86_400.0;

/// `LESTO_UPLOAD_EXPIRY_DAYS` (default 7): how long an unfinished chunked
/// upload may sit untouched before it's deleted. `LESTO_RECLAIM_AFTER_DAYS`
/// (default 30; 0 keeps everything forever): how long a file no library or
/// album references keeps its bytes on disk.
pub fn settings_from_env() -> server::housekeeping::Settings {
    let expiry_days = env_f64("UPLOAD_EXPIRY_DAYS", 7.0).max(0.0);
    let reclaim_days = env_f64("RECLAIM_AFTER_DAYS", 30.0);
    tracing::info!(
        upload_expiry_days = expiry_days,
        reclaim_after_days = reclaim_days,
        "disk housekeeping: daily (LESTO_UPLOAD_EXPIRY_DAYS, LESTO_RECLAIM_AFTER_DAYS; 0 never reclaims)"
    );
    server::housekeeping::Settings {
        upload_expiry: Duration::from_secs_f64(expiry_days * DAY),
        reclaim_after: (reclaim_days > 0.0).then(|| Duration::from_secs_f64(reclaim_days * DAY)),
        ..Default::default()
    }
}

pub async fn run_forever(state: SharedState, settings: server::housekeeping::Settings) {
    loop {
        let state = state.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || run_once(&state, settings)).await {
            tracing::error!(error = %e, "disk housekeeping panicked");
        }
        tokio::time::sleep(RUN_EVERY).await;
    }
}

fn run_once(state: &SharedState, settings: server::housekeeping::Settings) {
    let started = Instant::now();
    let report = match server::housekeeping::run(&state.log, &state.objects_dir, settings) {
        Ok(report) => report,
        Err(e) => {
            tracing::warn!(error = %e, "disk housekeeping failed, will retry tomorrow");
            return;
        }
    };
    for hash in &report.lost {
        tracing::error!(%hash, "a reclaimed file is in a library again but its bytes are gone, until a device uploads it again");
    }
    tracing::info!(
        expired_uploads = report.expired_uploads,
        temp_files = report.temp_files,
        orphan_blobs = report.orphan_blobs,
        newly_unreferenced = report.newly_unreferenced,
        reclaimed = report.reclaimed_blobs,
        freed = %Bytes(report.freed_bytes),
        took = ?started.elapsed(),
        "disk housekeeping finished"
    );
}
