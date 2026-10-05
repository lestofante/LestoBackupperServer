// Runs the server crate's blob integrity check (server::scrub) in the
// background: once at startup, then hourly; each run checks only the blobs
// whose last verification is older than the interval, so a restart doesn't
// re-read everything. Configured by environment variables, logged at start.

use std::time::{Duration, Instant};

use crate::config::env_f64;
use crate::dispatch::SharedState;
use crate::human::Bytes;

const RUN_EVERY: Duration = Duration::from_secs(3600);

pub struct ScrubConfig {
    pub interval: Duration,
    pub max_bytes_per_sec: u64,
}

impl ScrubConfig {
    /// `LESTO_SCRUB_INTERVAL_DAYS` (default 7; fractions allowed; 0 turns
    /// the check off) and `LESTO_SCRUB_MAX_MIBPS` (read cap, default 50).
    pub fn from_env() -> Option<ScrubConfig> {
        let days = env_f64("SCRUB_INTERVAL_DAYS", 7.0);
        let mibps = env_f64("SCRUB_MAX_MIBPS", 50.0);
        if days <= 0.0 {
            tracing::warn!("blob integrity check disabled (LESTO_SCRUB_INTERVAL_DAYS=0)");
            return None;
        }
        tracing::info!(
            interval_days = days,
            max_mibps = mibps,
            "blob integrity check: every blob re-verified after the interval (LESTO_SCRUB_INTERVAL_DAYS, LESTO_SCRUB_MAX_MIBPS)"
        );
        Some(ScrubConfig { interval: Duration::from_secs_f64(days * 86_400.0), max_bytes_per_sec: (mibps * 1024.0 * 1024.0) as u64 })
    }
}


pub async fn run_forever(state: SharedState, config: ScrubConfig) {
    loop {
        let state = state.clone();
        let (interval, max_bytes_per_sec) = (config.interval, config.max_bytes_per_sec);
        if let Err(e) = tokio::task::spawn_blocking(move || run_once(&state, interval, max_bytes_per_sec)).await {
            tracing::error!(error = %e, "blob integrity check panicked");
        }
        tokio::time::sleep(RUN_EVERY).await;
    }
}

fn run_once(state: &SharedState, interval: Duration, max_bytes_per_sec: u64) {
    let due = match server::scrub::due(&state.log, interval) {
        Ok(due) => due,
        Err(e) => {
            tracing::warn!(error = %e, "blob integrity check: cannot list due blobs");
            return;
        }
    };
    if due.is_empty() {
        return;
    }

    let total: u64 = due.iter().map(|b| b.size).sum();
    tracing::info!(blobs = due.len(), size = %Bytes(total), "blob integrity check started");
    let started = Instant::now();
    let mut throttle = server::scrub::Throttle::new(max_bytes_per_sec);
    let (mut problems, mut failed) = (0usize, 0usize);
    for blob in &due {
        match server::scrub::verify_and_record(&state.log, &state.objects_dir, blob, &mut throttle) {
            Ok(None) => {}
            Ok(Some(problem)) => {
                problems += 1;
                tracing::error!(hash = %problem.hash, status = ?problem.status, detail = %problem.detail, "blob failed its integrity check");
            }
            Err(e) => {
                failed += 1;
                tracing::warn!(hash = %blob.hash, error = %e, "could not check blob, will retry next run");
            }
        }
    }
    tracing::info!(blobs = due.len(), size = %Bytes(total), problems, could_not_check = failed, took = ?started.elapsed(), "blob integrity check finished");
}
