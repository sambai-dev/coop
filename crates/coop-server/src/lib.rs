pub mod auth;
pub mod bus;
pub mod config;
pub mod openapi;
pub mod ratelimit;
pub mod routes;
pub mod scheduler;

use crate::bus::Bus;
use crate::config::Config;
use coop_store::Store;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Semaphore};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub store: Arc<Store>,
    pub bus: Bus,
    pub queue_tx: mpsc::Sender<String>,
    pub tenant_sems: Arc<DashMap<String, Arc<Semaphore>>>,
    pub rate: Arc<ratelimit::RateLimiter>,
    pub sandbox_mode: coop_exec::SandboxMode,
}

pub fn build_app(
    cfg: Config,
    store: Arc<Store>,
) -> (axum::Router, AppState, mpsc::Receiver<String>) {
    let rate_per_min = cfg.rate_per_min;
    let workers = cfg.workers;
    let (queue_tx, queue_rx) = mpsc::channel(1024);
    let sandbox_mode = resolve_sandbox(&cfg.sandbox);

    let state = AppState {
        cfg: Arc::new(cfg),
        store,
        bus: Bus::default(),
        queue_tx,
        tenant_sems: Arc::new(DashMap::new()),
        rate: Arc::new(ratelimit::RateLimiter::new(rate_per_min)),
        sandbox_mode,
    };

    tracing::debug!(
        workers,
        sandbox = sandbox_mode.as_str(),
        "worker pool configured"
    );

    let app = routes::router(state.clone());
    (app, state, queue_rx)
}

pub fn resolve_sandbox(setting: &str) -> coop_exec::SandboxMode {
    let explicit = match setting.to_ascii_lowercase().as_str() {
        "off" | "none" | "naive" => Some(coop_exec::SandboxMode::Off),
        "ns" | "namespaces" | "sandbox" => Some(coop_exec::SandboxMode::Namespaces),
        _ => None,
    };
    explicit.unwrap_or_else(|| {
        if coop_exec::namespace_sandbox_available() {
            coop_exec::SandboxMode::Namespaces
        } else {
            tracing::warn!(
                "namespace sandbox unavailable on this host (needs root + cgroup v2); \
                 running executors WITHOUT kernel isolation"
            );
            coop_exec::SandboxMode::Off
        }
    })
}
