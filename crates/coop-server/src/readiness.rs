//! Cached readiness state.
//!
//! `/readyz` must remain O(1) and must not turn an unauthenticated probe storm
//! into a SQLite connection storm. One background task performs the existing
//! read-path check. If that task stops, blocks, or panics, freshness expires and
//! readiness fails closed. This deliberately does not claim write-path health:
//! a future store-owned probe can feed the same cache without changing the HTTP
//! contract or adding per-request I/O.

use crate::metrics::StorageOperation;
use crate::AppState;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const PROBE_INTERVAL: Duration = Duration::from_secs(2);
const PROBE_MAX_AGE: Duration = Duration::from_secs(10);

pub struct ReadinessCache {
    created_at: Instant,
    storage_ok: AtomicBool,
    last_probe_elapsed_ms: AtomicU64,
}

impl Default for ReadinessCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadinessCache {
    pub fn new() -> Self {
        Self {
            created_at: Instant::now(),
            // Readiness is earned by an explicit probe in build_app; never let
            // task scheduling create an optimistic readiness window.
            storage_ok: AtomicBool::new(false),
            last_probe_elapsed_ms: AtomicU64::new(0),
        }
    }

    pub fn storage_ready(&self) -> bool {
        self.storage_ok.load(Ordering::Acquire) && self.probe_age() <= PROBE_MAX_AGE
    }

    pub fn probe_age(&self) -> Duration {
        let now = elapsed_millis(self.created_at);
        let observed = self.last_probe_elapsed_ms.load(Ordering::Acquire);
        Duration::from_millis(now.saturating_sub(observed))
    }

    fn observe_storage(&self, ok: bool) -> bool {
        self.last_probe_elapsed_ms
            .store(elapsed_millis(self.created_at), Ordering::Release);
        self.storage_ok.swap(ok, Ordering::AcqRel)
    }

    #[cfg(test)]
    pub(crate) fn force_unhealthy(&self) {
        self.last_probe_elapsed_ms.store(0, Ordering::Release);
        self.storage_ok.store(false, Ordering::Release);
    }
}

pub fn spawn_monitor(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut shutdown = state.shutdown.subscribe();
        if *shutdown.borrow() {
            return;
        }
        let mut ticker = tokio::time::interval(PROBE_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            if *shutdown.borrow() {
                return;
            }
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                    continue;
                }
                _ = ticker.tick() => {}
            }

            probe_once(&state).await;
        }
    })
}

pub async fn prime(state: &AppState) {
    probe_once(state).await;
}

async fn probe_once(state: &AppState) {
    let started_at = Instant::now();
    let result = state.store.readiness_probe().await;
    state.metrics.observe_storage(
        StorageOperation::Readiness,
        started_at.elapsed(),
        result.is_ok(),
    );
    let was_ok = state.readiness.observe_storage(result.is_ok());
    match result {
        Ok(_) if !was_ok => tracing::info!("readiness storage probe recovered"),
        Ok(_) => {}
        Err(error) if was_ok => tracing::warn!(%error, "readiness storage probe failed"),
        Err(_) => {}
    }
}

fn elapsed_millis(started_at: Instant) -> u64 {
    started_at.elapsed().as_millis().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_starts_unready_then_tracks_probe_transitions() {
        let cache = ReadinessCache::new();
        assert!(!cache.storage_ready());
        assert!(!cache.observe_storage(true));
        assert!(cache.storage_ready());
        assert!(cache.observe_storage(false));
        assert!(!cache.storage_ready());
        assert!(!cache.observe_storage(true));
        assert!(cache.storage_ready());
    }

    #[test]
    fn explicit_unhealthy_state_is_not_ready() {
        let cache = ReadinessCache::new();
        cache.force_unhealthy();
        assert!(!cache.storage_ready());
    }

    #[test]
    fn monitor_freshness_expires_without_an_update() {
        let cache = ReadinessCache {
            created_at: Instant::now() - PROBE_MAX_AGE - Duration::from_millis(1),
            storage_ok: AtomicBool::new(true),
            last_probe_elapsed_ms: AtomicU64::new(0),
        };
        assert!(!cache.storage_ready());
    }
}
