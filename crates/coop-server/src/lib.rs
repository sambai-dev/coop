pub mod auth;
pub mod bus;
pub mod config;
pub mod metrics;
pub mod openapi;
pub mod ratelimit;
pub mod readiness;
pub(crate) mod request_context;
pub mod routes;
pub mod scheduler;
pub mod transport;

use crate::bus::Bus;
use crate::config::Config;
use coop_store::Store;
use dashmap::DashMap;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const QUEUE_CAPACITY: usize = 1024;
pub const ACTIVE_SUBMIT_BODIES_GLOBAL: usize = 4;
pub const ACTIVE_SUBMIT_BODIES_PER_TENANT: usize = 2;
pub const ACTIVE_STREAMS_GLOBAL: usize = 128;
pub const ACTIVE_STREAMS_PER_TENANT: usize = 16;
pub const ACTIVE_RESULT_WAITS_GLOBAL: usize = 64;
pub const ACTIVE_RESULT_WAITS_PER_TENANT: usize = 8;
pub const ACTIVE_LARGE_RESPONSES_GLOBAL: usize = 4;
pub const ACTIVE_LARGE_RESPONSES_PER_TENANT: usize = 1;
const INTERPRETER_PREFLIGHT_CONCURRENCY: usize = 3;
static INTERPRETER_PREFLIGHT_SLOTS: Semaphore =
    Semaphore::const_new(INTERPRETER_PREFLIGHT_CONCURRENCY);

/// Nonblocking global + per-tenant lifetime bound for HTTP work that remains
/// resident after the request rate-limit decision (body buffering, streams,
/// and long result waits). The permit is RAII-owned for the whole bounded
/// lifetime, including unwind/cancellation.
#[derive(Clone)]
pub struct LifetimeAdmission {
    global: Arc<Semaphore>,
    tenants: Arc<DashMap<String, Arc<Semaphore>>>,
    per_tenant: usize,
    global_capacity: usize,
    accepting: Arc<std::sync::atomic::AtomicBool>,
}

pub struct LifetimePermit {
    _global: OwnedSemaphorePermit,
    _tenant: OwnedSemaphorePermit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryLifetimeError {
    GlobalFull,
    TenantFull,
    Closed,
}

impl LifetimeAdmission {
    pub fn new(global: usize, per_tenant: usize) -> Self {
        assert!(global > 0, "global lifetime capacity must be positive");
        assert!(per_tenant > 0, "tenant lifetime capacity must be positive");
        Self {
            global: Arc::new(Semaphore::new(global)),
            tenants: Arc::new(DashMap::new()),
            per_tenant,
            global_capacity: global,
            accepting: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    pub fn try_acquire(&self, tenant: &str) -> Result<LifetimePermit, TryLifetimeError> {
        if !self.accepting.load(std::sync::atomic::Ordering::Acquire) {
            return Err(TryLifetimeError::Closed);
        }
        let global = Arc::clone(&self.global)
            .try_acquire_owned()
            .map_err(|error| match error {
                tokio::sync::TryAcquireError::Closed => TryLifetimeError::Closed,
                tokio::sync::TryAcquireError::NoPermits => TryLifetimeError::GlobalFull,
            })?;
        let tenant_slots = self
            .tenants
            .entry(tenant.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(self.per_tenant)))
            .clone();
        let tenant = tenant_slots
            .try_acquire_owned()
            .map_err(|error| match error {
                tokio::sync::TryAcquireError::Closed => TryLifetimeError::Closed,
                tokio::sync::TryAcquireError::NoPermits => TryLifetimeError::TenantFull,
            })?;
        if !self.accepting.load(std::sync::atomic::Ordering::Acquire) {
            return Err(TryLifetimeError::Closed);
        }
        Ok(LifetimePermit {
            _global: global,
            _tenant: tenant,
        })
    }

    pub fn close(&self) {
        self.accepting
            .store(false, std::sync::atomic::Ordering::Release);
        self.global.close();
        for slots in self.tenants.iter() {
            slots.close();
        }
    }

    pub fn capacity(&self) -> usize {
        self.global_capacity
    }

    pub fn depth(&self) -> usize {
        self.global_capacity
            .saturating_sub(self.global.available_permits())
    }
}

#[derive(Clone)]
pub struct RunningJob {
    pub tenant: String,
    pub cancel: Arc<coop_exec::ExecutionCancellation>,
}

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub store: Arc<Store>,
    pub bus: Bus,
    pub admission: scheduler::Admission,
    pub tenant_sems: Arc<DashMap<String, Arc<Semaphore>>>,
    /// Weighted aggregate memory budget. Each dispatched job owns exactly its
    /// server-clamped MiB request until execution/finalization returns.
    pub memory_slots: Arc<Semaphore>,
    pub rate: Arc<ratelimit::RateLimiter>,
    /// Fixed-cardinality, process-local operational telemetry. This registry
    /// intentionally contains no tenant, job, request, trace, or raw path
    /// dimensions.
    pub metrics: Arc<metrics::Metrics>,
    /// Digest of the separately scoped global scrape credential. `None`
    /// disables `/metrics` without affecting tenant API availability.
    pub metrics_token_digest: Option<[u8; 32]>,
    /// O(1) readiness snapshot fed by one bounded background store probe.
    pub readiness: Arc<readiness::ReadinessCache>,
    pub sandbox_mode: coop_exec::SandboxMode,
    /// F-005: install a seccomp-BPF allowlist in sandboxed jobs (see Config).
    pub seccomp: bool,
    /// Exact host executables that passed the development executor's bounded
    /// startup canary. Empty for the namespace backend, whose interpreters are
    /// verified inside the private rootfs by the full startup preflight.
    pub resolved_naive_interpreters: Arc<HashMap<String, String>>,
    /// Languages this process can truthfully admit and advertise.
    pub available_languages: Arc<Vec<String>>,
    /// Sticky process-launch fence. Shutdown closes this before publishing
    /// its sticky signal, linearizing against the short spawn critical
    /// sections in both executor backends.
    pub execution_start_gate: Arc<coop_exec::ExecutionStartGate>,
    /// Cancellation flags for RUNNING jobs, keyed by job id. The scheduler
    /// inserts a flag when a job starts executing; the cancel endpoint flips
    /// it and the executor's poll loop acts on it within one tick (~20 ms).
    /// Entries are removed when the job finishes.
    pub cancels: Arc<DashMap<String, RunningJob>>,
    pub stream_tickets: Arc<DashMap<String, auth::StreamTicket>>,
    /// Request correlation retained only while this process owns a job. The
    /// durable integration boundary is documented separately; no source,
    /// output, tenant secret, baggage, or raw trace state is retained here.
    pub(crate) job_traces: Arc<DashMap<String, request_context::JobTraceContext>>,
    pub started_at: std::time::Instant,
    pub shutdown: watch::Sender<bool>,
    /// False only while the binary is reconciling durable startup state.
    /// Embedded callers start ready and may opt into the same gate.
    pub startup_ready: Arc<std::sync::atomic::AtomicBool>,
    pub submit_body_admission: LifetimeAdmission,
    pub stream_admission: LifetimeAdmission,
    pub result_wait_admission: LifetimeAdmission,
    pub large_response_admission: LifetimeAdmission,
}

impl AppState {
    /// Close admission before publishing the sticky shutdown signal. Existing
    /// reservations may finish their durable handoff, but new and waiting
    /// reservations fail immediately.
    pub fn begin_shutdown(&self) {
        self.admission.close();
        self.submit_body_admission.close();
        self.stream_admission.close();
        self.result_wait_admission.close();
        self.large_response_admission.close();
        // A spawn already inside its short critical section completes first;
        // after close returns, no executor can create a helper/user process.
        // The gate is never held for the job's execution lifetime.
        self.execution_start_gate.close();
        self.shutdown.send_replace(true);
        for running in self.cancels.iter() {
            running.cancel.cancel();
        }
    }
}

pub async fn build_app(
    cfg: Config,
    store: Arc<Store>,
) -> Result<(axum::Router, AppState, mpsc::Receiver<scheduler::QueuedJob>), String> {
    let expected_storage_limits = cfg.storage_limits();
    if store.storage_limits() != expected_storage_limits {
        return Err(
            "Store policy does not match Config storage quotas; open it with Store::open_with_limits using the configured global, tenant, and free-space limits"
                .to_string(),
        );
    }
    let rate_per_min = cfg.rate_per_min;
    let metrics_token_digest = cfg.metrics_token.as_deref().map(metrics::token_digest);
    let workers = cfg.workers;
    let memory_budget_mb = cfg.memory_budget_mb;
    let (admission, queue_rx) =
        scheduler::Admission::channel(QUEUE_CAPACITY, cfg.tenant_queue_capacity);
    let sandbox_mode = resolve_sandbox(&cfg)?;
    cfg.validate_resolved_listener_security(sandbox_mode)?;
    // F-005: only meaningful when kernel isolation is actually in play; the
    // naive backend has no exec boundary to put a filter in front of.
    let seccomp_enabled = cfg.seccomp && matches!(sandbox_mode, coop_exec::SandboxMode::Namespaces);

    // N-1: tenant isolation requires the jobs root to be server-private
    // (0700). The binary path enforces this in main(), but any embedder that
    // calls build_app directly must get the same guarantee, or the default-
    // mode parent lets sandboxed jobs enumerate sibling workdir names.
    crate::config::prepare_jobs_root(
        Path::new(&cfg.jobs_root),
        cfg.production || matches!(sandbox_mode, coop_exec::SandboxMode::Namespaces),
    )?;

    let mut resolved_naive_interpreters = HashMap::new();
    let available_languages = if matches!(sandbox_mode, coop_exec::SandboxMode::Off) {
        // Each executor probe owns its own bounded timeout/tree cleanup. Poll
        // all configured runtimes together so a missing or wedged binary adds
        // at most one probe window to startup rather than one per language.
        // `join_all` preserves this input order; capabilities below retain the
        // stable python/node/bash ordering regardless of completion order.
        let preflights = coop_types::SUPPORTED_LANGUAGES.iter().map(|&language| {
            let override_bin = cfg.interpreter_override(language);
            async move {
                // Embedded callers and parallel test/app construction must
                // not turn capability discovery into an unbounded process
                // burst. One ordinary server still probes all three runtimes
                // concurrently; additional builders wait without spawning.
                let _slot = INTERPRETER_PREFLIGHT_SLOTS
                    .acquire()
                    .await
                    .expect("static interpreter preflight semaphore stays open");
                (
                    language,
                    coop_exec::preflight_naive_interpreter(language, override_bin.as_deref()).await,
                )
            }
        });
        for (language, result) in futures_util::future::join_all(preflights).await {
            match result {
                Ok(executable) => {
                    resolved_naive_interpreters.insert(language.to_string(), executable);
                }
                Err(error) => tracing::warn!(
                    language,
                    error = %error,
                    "development runtime unavailable; omitting it from capabilities"
                ),
            }
        }
        coop_types::SUPPORTED_LANGUAGES
            .iter()
            .filter(|language| resolved_naive_interpreters.contains_key(**language))
            .map(|language| (*language).to_string())
            .collect()
    } else {
        coop_types::SUPPORTED_LANGUAGES
            .iter()
            .map(|language| (*language).to_string())
            .collect()
    };

    let (shutdown, _shutdown_rx) = watch::channel(false);
    let state = AppState {
        cfg: Arc::new(cfg),
        store,
        bus: Bus::default(),
        admission,
        tenant_sems: Arc::new(DashMap::new()),
        memory_slots: Arc::new(Semaphore::new(memory_budget_mb as usize)),
        rate: Arc::new(ratelimit::RateLimiter::new(rate_per_min)),
        metrics: Arc::new(metrics::Metrics::new()),
        metrics_token_digest,
        readiness: Arc::new(readiness::ReadinessCache::new()),
        sandbox_mode,
        seccomp: seccomp_enabled,
        resolved_naive_interpreters: Arc::new(resolved_naive_interpreters),
        available_languages: Arc::new(available_languages),
        execution_start_gate: Arc::new(coop_exec::ExecutionStartGate::default()),
        cancels: Arc::new(DashMap::new()),
        stream_tickets: Arc::new(DashMap::new()),
        job_traces: Arc::new(DashMap::new()),
        started_at: std::time::Instant::now(),
        shutdown,
        startup_ready: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        submit_body_admission: LifetimeAdmission::new(
            ACTIVE_SUBMIT_BODIES_GLOBAL,
            ACTIVE_SUBMIT_BODIES_PER_TENANT,
        ),
        stream_admission: LifetimeAdmission::new(ACTIVE_STREAMS_GLOBAL, ACTIVE_STREAMS_PER_TENANT),
        result_wait_admission: LifetimeAdmission::new(
            ACTIVE_RESULT_WAITS_GLOBAL,
            ACTIVE_RESULT_WAITS_PER_TENANT,
        ),
        large_response_admission: LifetimeAdmission::new(
            ACTIVE_LARGE_RESPONSES_GLOBAL,
            ACTIVE_LARGE_RESPONSES_PER_TENANT,
        ),
    };

    tracing::debug!(
        workers,
        sandbox = sandbox_mode.as_str(),
        "worker pool configured"
    );

    readiness::prime(&state).await;
    let app = routes::router(state.clone());
    std::mem::drop(readiness::spawn_monitor(state.clone()));
    Ok((app, state, queue_rx))
}

/// F8: sandbox selection never silently degrades. Explicit namespace requests
/// are validated against the host; auto/unknown configurations fail closed in
/// production instead of falling back to unprotected execution.
pub fn resolve_sandbox(cfg: &Config) -> Result<coop_exec::SandboxMode, String> {
    if matches!(
        cfg.sandbox.trim().to_ascii_lowercase().as_str(),
        "off" | "none" | "naive"
    ) {
        return resolve_sandbox_with(
            &cfg.sandbox,
            cfg.production,
            false,
            false,
            false,
            cfg.unsafe_allow_naive,
        );
    }
    let rootfs_ready = match cfg.rootfs.as_deref() {
        Some(rootfs) => validate_rootfs(Path::new(rootfs))?,
        None => false,
    };
    let helper_ready = match cfg.sandbox_helper.as_deref() {
        Some(helper) => validate_sandbox_helper(Path::new(helper))?,
        None => false,
    };
    resolve_sandbox_with(
        &cfg.sandbox,
        cfg.production,
        coop_exec::namespace_sandbox_available(),
        rootfs_ready,
        helper_ready,
        cfg.unsafe_allow_naive,
    )
}

fn validate_sandbox_helper(path: &Path) -> Result<bool, String> {
    if !path.is_absolute() {
        return Err("COOP_SANDBOX_HELPER must be an absolute path".to_string());
    }
    ensure_no_redirected_ancestors(path, "COOP_SANDBOX_HELPER")?;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|e| format!("cannot inspect COOP_SANDBOX_HELPER {}: {e}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "COOP_SANDBOX_HELPER {} must be a regular non-symlink file",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != 0 || metadata.permissions().mode() & 0o022 != 0 {
            return Err(format!(
                "COOP_SANDBOX_HELPER {} must be root-owned and not group/world writable",
                path.display()
            ));
        }
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!(
                "COOP_SANDBOX_HELPER {} is not executable",
                path.display()
            ));
        }
    }
    Ok(true)
}

fn validate_rootfs(path: &Path) -> Result<bool, String> {
    if !path.is_absolute() {
        return Err("COOP_ROOTFS must be an absolute path".to_string());
    }
    crate::config::validate_jobs_root(path)
        .map_err(|error| error.replace("COOP_JOBS_ROOT", "COOP_ROOTFS"))?;
    ensure_no_redirected_ancestors(path, "COOP_ROOTFS")?;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|e| format!("cannot inspect COOP_ROOTFS {}: {e}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "COOP_ROOTFS {} must be a real directory, not a symlink",
            path.display()
        ));
    }
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("cannot resolve COOP_ROOTFS {}: {e}", path.display()))?;
    if canonical.parent().is_none() {
        return Err(
            "COOP_ROOTFS must be a private root filesystem; host / is forbidden".to_string(),
        );
    }
    for required in [".pivot_old", "tmp", "proc", "dev", "work"] {
        let required_path = canonical.join(required);
        let required_metadata = std::fs::symlink_metadata(&required_path)
            .map_err(|e| format!("COOP_ROOTFS is missing required directory /{required}: {e}"))?;
        if required_metadata.file_type().is_symlink() || !required_metadata.is_dir() {
            return Err(format!("COOP_ROOTFS /{required} must be a real directory"));
        }
    }
    if std::fs::read_dir(canonical.join(".pivot_old"))
        .map_err(|e| format!("cannot inspect COOP_ROOTFS /.pivot_old: {e}"))?
        .next()
        .is_some()
    {
        return Err("COOP_ROOTFS /.pivot_old must be empty".to_string());
    }
    Ok(true)
}

fn ensure_no_redirected_ancestors(path: &Path, setting: &str) -> Result<(), String> {
    for ancestor in path.ancestors().filter(|ancestor| ancestor.exists()) {
        let metadata = std::fs::symlink_metadata(ancestor).map_err(|e| {
            format!(
                "cannot inspect {setting} ancestor {}: {e}",
                ancestor.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "{setting} must not traverse a symlink: {}",
                ancestor.display()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if metadata.uid() != 0 || metadata.permissions().mode() & 0o022 != 0 {
                return Err(format!(
                    "{setting} must traverse only root-owned, non-writable components; {} is insecure",
                    ancestor.display()
                ));
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(format!(
                    "{setting} must not traverse a junction or reparse point: {}",
                    ancestor.display()
                ));
            }
        }
    }
    Ok(())
}

fn resolve_sandbox_with(
    setting: &str,
    production: bool,
    available: bool,
    rootfs_ready: bool,
    helper_ready: bool,
    unsafe_allow_naive: bool,
) -> Result<coop_exec::SandboxMode, String> {
    let setting = setting.trim();
    match setting.to_ascii_lowercase().as_str() {
        "off" | "none" | "naive" if production && !unsafe_allow_naive => Err(
            "COOP_SANDBOX=off in production requires the conspicuous acknowledgement COOP_UNSAFE_ALLOW_NAIVE=true"
                .to_string(),
        ),
        "off" | "none" | "naive" => Ok(coop_exec::SandboxMode::Off),
        // An explicit namespace request must actually be satisfiable.
        "ns" | "namespaces" | "sandbox" if available && rootfs_ready && helper_ready => {
            Ok(coop_exec::SandboxMode::Namespaces)
        }
        "ns" | "namespaces" | "sandbox" if !rootfs_ready => Err(
            "COOP_SANDBOX requests namespace isolation, but COOP_ROOTFS is missing or invalid; host / is never used as a sandbox root"
                .to_string(),
        ),
        "ns" | "namespaces" | "sandbox" if !helper_ready => Err(
            "COOP_SANDBOX requests namespace isolation, but COOP_SANDBOX_HELPER is missing or invalid"
                .to_string(),
        ),
        "ns" | "namespaces" | "sandbox" => Err(
            "COOP_SANDBOX requests namespace isolation, but the namespace sandbox is \
             unavailable on this host (needs root + cgroup v2 unified hierarchy)"
                .to_string(),
        ),
        // auto / empty / unrecognized: prefer namespaces, but refuse to start
        // unprotected in production rather than degrading silently.
        "auto" | "" if available && rootfs_ready && helper_ready => {
            Ok(coop_exec::SandboxMode::Namespaces)
        }
        "auto" | "" if production && !rootfs_ready => Err(
            "COOP_ROOTFS is required for namespace isolation in production".to_string(),
        ),
        "auto" | "" if production && !helper_ready => Err(
            "COOP_SANDBOX_HELPER is required for namespace isolation in production".to_string(),
        ),
        "auto" | "" if production => Err(format!(
            "COOP_SANDBOX={setting:?}: namespace sandbox unavailable on this host \
             (needs root + cgroup v2 unified hierarchy); refusing to serve production \
             traffic without kernel isolation"
        )),
        "auto" | "" => {
            tracing::warn!(
                "namespace sandbox unavailable or COOP_ROOTFS missing; \
                 running executors WITHOUT kernel isolation"
            );
            Ok(coop_exec::SandboxMode::Off)
        }
        _ => Err(format!(
            "invalid COOP_SANDBOX value {setting:?}; expected auto, namespaces, or off"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifetime_admission_enforces_global_tenant_reclaim_and_close() {
        let admission = LifetimeAdmission::new(3, 2);
        let first = admission.try_acquire("tenant-a").expect("tenant a slot 1");
        let second = admission.try_acquire("tenant-a").expect("tenant a slot 2");
        assert_eq!(
            admission.try_acquire("tenant-a").err(),
            Some(TryLifetimeError::TenantFull)
        );
        let other = admission
            .try_acquire("tenant-b")
            .expect("isolated tenant slot");
        assert_eq!(
            admission.try_acquire("tenant-c").err(),
            Some(TryLifetimeError::GlobalFull)
        );

        drop(first);
        let reclaimed = admission
            .try_acquire("tenant-a")
            .expect("drop reclaims global and tenant slots");
        drop((second, other, reclaimed));

        admission.close();
        assert_eq!(
            admission.try_acquire("tenant-a").err(),
            Some(TryLifetimeError::Closed)
        );
    }

    #[tokio::test]
    async fn lifetime_admission_reclaims_capacity_when_owner_panics() {
        let admission = LifetimeAdmission::new(1, 1);
        let permit = admission.try_acquire("tenant-a").expect("sole permit");
        let task = tokio::spawn(async move {
            let _permit = permit;
            panic!("simulated upgraded socket task panic");
        });
        let error = task.await.expect_err("task must panic");
        assert!(error.is_panic());
        let _reclaimed = admission
            .try_acquire("tenant-a")
            .expect("unwind reclaims both admission permits");
    }

    #[test]
    fn explicit_ns_unavailable_is_config_error_even_in_dev() {
        for s in ["ns", "namespaces", "sandbox", " NS "] {
            let err = resolve_sandbox_with(s, false, false, true, true, false).unwrap_err();
            assert!(err.contains("COOP_SANDBOX"), "{err}");
        }
    }

    #[test]
    fn explicit_off_is_honored_without_availability() {
        for s in ["off", "none", "naive", "OFF"] {
            assert_eq!(
                resolve_sandbox_with(s, true, false, false, false, true).unwrap(),
                coop_exec::SandboxMode::Off
            );
        }
    }

    #[test]
    fn auto_and_unknown_fail_closed_in_production_without_namespaces() {
        for s in ["auto", "", "bogus-value"] {
            assert!(
                resolve_sandbox_with(s, true, false, true, true, false).is_err(),
                "production must refuse to start unprotected: {s:?}"
            );
        }
    }

    #[test]
    fn auto_degrades_to_off_in_dev_only() {
        assert_eq!(
            resolve_sandbox_with("auto", false, false, false, false, false).unwrap(),
            coop_exec::SandboxMode::Off
        );
    }

    #[test]
    fn available_host_selects_namespaces() {
        assert_eq!(
            resolve_sandbox_with("auto", true, true, true, true, false).unwrap(),
            coop_exec::SandboxMode::Namespaces
        );
        assert_eq!(
            resolve_sandbox_with("ns", false, true, true, true, false).unwrap(),
            coop_exec::SandboxMode::Namespaces
        );
    }

    #[test]
    fn production_off_requires_explicit_unsafe_acknowledgement() {
        assert!(resolve_sandbox_with("off", true, false, false, false, false).is_err());
        assert_eq!(
            resolve_sandbox_with("off", true, false, false, false, true).unwrap(),
            coop_exec::SandboxMode::Off
        );
    }

    #[tokio::test]
    async fn build_app_rejects_store_policy_mismatch_even_in_development() {
        let base = std::env::temp_dir().join(format!(
            "coop-store-policy-mismatch-{}",
            uuid::Uuid::now_v7()
        ));
        let db = base.join("coop.db");
        let jobs = base.join("jobs");
        let source = |key: &str| match key {
            "COOP_API_KEYS" => Some("tenant:a-long-development-key".to_string()),
            "COOP_SANDBOX" => Some("off".to_string()),
            "COOP_STORAGE_TENANT_MB" => Some("128".to_string()),
            "COOP_STORAGE_GLOBAL_MB" => Some("256".to_string()),
            "COOP_STORAGE_FREE_RESERVE_MB" => Some("0".to_string()),
            "COOP_JOBS_ROOT" => Some(jobs.to_string_lossy().into_owned()),
            _ => None,
        };
        let cfg = Config::from_sources(&source, false).unwrap();
        let store = Arc::new(Store::open(&db).await.unwrap());
        match build_app(cfg, store).await {
            Err(error) => assert!(error.contains("Store policy does not match"), "{error}"),
            Ok(_) => panic!("development embedder bypassed configured storage quotas"),
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn explicit_off_does_not_validate_unused_namespace_artifacts() {
        let mut cfg = Config::from_sources(&|_| None, false).expect("development config");
        cfg.sandbox = "off".to_string();
        cfg.rootfs = Some("relative-unused-rootfs".to_string());
        cfg.sandbox_helper = Some("relative-unused-helper".to_string());
        assert_eq!(resolve_sandbox(&cfg).unwrap(), coop_exec::SandboxMode::Off);
    }

    #[test]
    fn namespaces_require_private_rootfs() {
        assert!(resolve_sandbox_with("namespaces", false, true, false, true, false).is_err());
    }

    #[test]
    fn namespaces_require_bootstrap_helper() {
        assert!(resolve_sandbox_with("namespaces", false, true, true, false, false).is_err());
    }
}
