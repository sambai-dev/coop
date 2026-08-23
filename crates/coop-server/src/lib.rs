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
) -> Result<(axum::Router, AppState, mpsc::Receiver<String>), String> {
    let rate_per_min = cfg.rate_per_min;
    let workers = cfg.workers;
    let (queue_tx, queue_rx) = mpsc::channel(1024);
    let sandbox_mode = resolve_sandbox(&cfg.sandbox, crate::config::is_production())?;

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
    Ok((app, state, queue_rx))
}

/// F8: sandbox selection never silently degrades. Explicit namespace requests
/// are validated against the host; auto/unknown configurations fail closed in
/// production instead of falling back to unprotected execution.
pub fn resolve_sandbox(setting: &str, production: bool) -> Result<coop_exec::SandboxMode, String> {
    resolve_sandbox_with(
        setting,
        production,
        coop_exec::namespace_sandbox_available(),
    )
}

fn resolve_sandbox_with(
    setting: &str,
    production: bool,
    available: bool,
) -> Result<coop_exec::SandboxMode, String> {
    let setting = setting.trim();
    match setting.to_ascii_lowercase().as_str() {
        // Explicit opt-out stays honored in any environment.
        "off" | "none" | "naive" => Ok(coop_exec::SandboxMode::Off),
        // An explicit namespace request must actually be satisfiable.
        "ns" | "namespaces" | "sandbox" if available => Ok(coop_exec::SandboxMode::Namespaces),
        "ns" | "namespaces" | "sandbox" => Err(
            "COOP_SANDBOX requests namespace isolation, but the namespace sandbox is \
             unavailable on this host (needs root + cgroup v2 unified hierarchy)"
                .to_string(),
        ),
        // auto / empty / unrecognized: prefer namespaces, but refuse to start
        // unprotected in production rather than degrading silently.
        _ if available => Ok(coop_exec::SandboxMode::Namespaces),
        _ if production => Err(format!(
            "COOP_SANDBOX={setting:?}: namespace sandbox unavailable on this host \
             (needs root + cgroup v2 unified hierarchy); refusing to serve production \
             traffic without kernel isolation"
        )),
        _ => {
            tracing::warn!(
                "namespace sandbox unavailable on this host (needs root + cgroup v2); \
                 running executors WITHOUT kernel isolation"
            );
            Ok(coop_exec::SandboxMode::Off)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_ns_unavailable_is_config_error_even_in_dev() {
        for s in ["ns", "namespaces", "sandbox", " NS "] {
            let err = resolve_sandbox_with(s, false, false).unwrap_err();
            assert!(err.contains("COOP_SANDBOX"), "{err}");
        }
    }

    #[test]
    fn explicit_off_is_honored_without_availability() {
        for s in ["off", "none", "naive", "OFF"] {
            assert_eq!(
                resolve_sandbox_with(s, true, false).unwrap(),
                coop_exec::SandboxMode::Off
            );
        }
    }

    #[test]
    fn auto_and_unknown_fail_closed_in_production_without_namespaces() {
        for s in ["auto", "", "bogus-value"] {
            assert!(
                resolve_sandbox_with(s, true, false).is_err(),
                "production must refuse to start unprotected: {s:?}"
            );
        }
    }

    #[test]
    fn auto_degrades_to_off_in_dev_only() {
        assert_eq!(
            resolve_sandbox_with("auto", false, false).unwrap(),
            coop_exec::SandboxMode::Off
        );
    }

    #[test]
    fn available_host_selects_namespaces() {
        assert_eq!(
            resolve_sandbox_with("auto", true, true).unwrap(),
            coop_exec::SandboxMode::Namespaces
        );
        assert_eq!(
            resolve_sandbox_with("ns", false, true).unwrap(),
            coop_exec::SandboxMode::Namespaces
        );
    }
}
