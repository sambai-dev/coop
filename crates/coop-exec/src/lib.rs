mod bounded_output;
pub mod naive;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod gvisor;

#[cfg(target_os = "linux")]
pub mod linux_sandbox;

#[cfg(target_os = "linux")]
pub mod oci_init;

#[cfg(target_os = "linux")]
pub mod seccomp;

use coop_types::{EffectiveLimits, IsolationClass, LimitEnforcement, Limits};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    pub fn as_str(self) -> &'static str {
        match self {
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
        }
    }
}

pub trait Sink: Send + Sync {
    fn output(&self, stream: Stream, line: String);
    fn violation(&self, rule: &'static str, detail: Value);
    fn truncated(&self, stream: Stream);
}

/// Sticky, process-wide launch fence shared by the server and executors.
///
/// Closing the gate linearizes with process creation: `close` waits only for
/// launch handshakes already in progress, then permanently rejects later
/// launches. A permit is released as soon as the OS (or namespace helper)
/// acknowledges process creation; it is never retained while a job runs.
#[derive(Debug)]
pub struct ExecutionStartGate {
    state: std::sync::Mutex<ExecutionStartState>,
    drained: std::sync::Condvar,
}

#[derive(Debug, Default)]
struct ExecutionStartState {
    closed: bool,
    active: usize,
}

impl Default for ExecutionStartGate {
    fn default() -> Self {
        Self {
            state: std::sync::Mutex::new(ExecutionStartState::default()),
            drained: std::sync::Condvar::new(),
        }
    }
}

impl ExecutionStartGate {
    pub fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        while state.active != 0 {
            state = self
                .drained
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    pub fn is_closed(&self) -> bool {
        self.state.lock().map(|state| state.closed).unwrap_or(true)
    }

    fn enter(self: &Arc<Self>) -> Result<ExecutionStartPermit, ()> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                poisoned.into_inner().closed = true;
                return Err(());
            }
        };
        if state.closed {
            return Err(());
        }
        state.active = state.active.checked_add(1).ok_or(())?;
        Ok(ExecutionStartPermit {
            gate: Arc::clone(self),
        })
    }
}

struct ExecutionStartPermit {
    gate: Arc<ExecutionStartGate>,
}

impl Drop for ExecutionStartPermit {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active = state
            .active
            .checked_sub(1)
            .expect("execution start permit count underflow");
        if state.active == 0 {
            self.gate.drained.notify_all();
        }
    }
}

/// Per-job cancellation token whose cancellation operation also fences the
/// process-start boundary. This makes a cancellation that wins the gate
/// prevent launch, while an already-running job keeps the cheap atomic poll.
#[derive(Debug, Default)]
pub struct ExecutionCancellation {
    cancelled: std::sync::atomic::AtomicBool,
    start_gate: Arc<ExecutionStartGate>,
}

impl ExecutionCancellation {
    pub fn cancel(&self) {
        self.start_gate.close();
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
    }

    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[derive(Debug, Clone)]
pub struct ExecContext {
    pub job_key: String,
    pub language: String,
    pub code: String,
    pub stdin: Option<String>,
    pub limits: Limits,
    pub workdir: PathBuf,
    pub interpreter_override: Option<String>,
    /// Canonical private root filesystem for the Linux namespace backend.
    /// Host `/` is never used as an implicit sandbox root.
    pub rootfs: Option<PathBuf>,
    /// Absolute path to the dedicated single-threaded sandbox bootstrap
    /// executable. When absent, a sibling `coop-sandbox-init` is resolved.
    pub helper_path: Option<PathBuf>,
    /// Cancellation signal. When the referenced flag flips to true, the
    /// executor kills the job (whole process group) at its next poll tick
    /// and reports `OutcomeStatus::Cancelled`. Cloned freely; a missing
    /// token simply means "never cancelled".
    pub cancel: Option<Arc<ExecutionCancellation>>,
    /// Optional process-wide launch gate. Servers close this before
    /// publishing sticky shutdown, so a shutdown that wins the gate prevents
    /// any later user/helper process creation. Direct crate callers may omit
    /// it to retain an always-open launch boundary.
    pub start_gate: Option<Arc<ExecutionStartGate>>,
    /// Install a seccomp-BPF allowlist before exec (Linux namespace backend
    /// only). Default on: F-005 mitigation. `COOP_SECCOMP=off` disables it.
    pub seccomp: bool,
}

impl ExecContext {
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
    }

    pub(crate) fn begin_process_launch(&self) -> Result<ProcessLaunchPermit, &'static str> {
        let process = self
            .start_gate
            .as_ref()
            .map(ExecutionStartGate::enter)
            .transpose()
            .map_err(|()| "server_shutdown_before_launch")?;
        let job = self
            .cancel
            .as_ref()
            .map(|token| token.start_gate.enter())
            .transpose()
            .map_err(|()| "cancelled_before_launch")?;
        Ok(ProcessLaunchPermit {
            _process: process,
            _job: job,
        })
    }
}

pub(crate) struct ProcessLaunchPermit {
    _process: Option<ExecutionStartPermit>,
    _job: Option<ExecutionStartPermit>,
}

#[derive(Debug, Clone)]
pub struct ExecOutcome {
    pub status: coop_types::OutcomeStatus,
    pub exit_code: Option<i32>,
    pub killed_by: Option<String>,
    pub telemetry: ExecTelemetry,
}

impl ExecOutcome {
    pub(crate) fn cancelled_before_launch(reason: &'static str) -> Self {
        Self {
            status: coop_types::OutcomeStatus::Cancelled,
            exit_code: None,
            killed_by: Some(reason.to_string()),
            telemetry: ExecTelemetry::default(),
        }
    }
}

/// Executor-observed posture for one attempt. `bootstrap_ready` is set only
/// at the backend's actual workload-ready boundary, never from configuration
/// or from the presence of telemetry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionProvenance {
    pub backend: String,
    #[serde(default)]
    pub isolation_class: IsolationClass,
    pub bootstrap_ready: bool,
    pub isolated: bool,
    pub private_rootfs: bool,
    pub dedicated_bootstrap: bool,
    pub seccomp: bool,
    pub network_allowed: Option<bool>,
    pub networking: Option<String>,
    pub limit_enforcement: LimitEnforcement,
    #[serde(default)]
    pub runtime_version: Option<String>,
    #[serde(default)]
    pub runtime_sha256: Option<String>,
    #[serde(default)]
    pub rootfs_sha256: Option<String>,
    #[serde(default)]
    pub config_sha256: Option<String>,
}

impl ExecutionProvenance {
    pub fn not_ready(mode: SandboxMode) -> Self {
        Self::observed(mode, false, false)
    }

    fn observed(mode: SandboxMode, seccomp_requested: bool, ready: bool) -> Self {
        let isolated = ready && mode == SandboxMode::Namespaces;
        let development = ready && mode == SandboxMode::Off;
        Self {
            backend: mode.as_str().to_string(),
            isolation_class: if ready {
                mode.isolation_class()
            } else {
                IsolationClass::None
            },
            bootstrap_ready: ready,
            isolated,
            private_rootfs: isolated,
            dedicated_bootstrap: isolated,
            seccomp: isolated && seccomp_requested,
            network_allowed: ready.then_some(development),
            networking: ready.then(|| {
                if isolated {
                    "disabled".to_string()
                } else {
                    "host".to_string()
                }
            }),
            limit_enforcement: if isolated {
                LimitEnforcement::NAMESPACE_SANDBOX
            } else if development {
                LimitEnforcement::DEVELOPMENT_SUBPROCESS
            } else {
                LimitEnforcement::NONE
            },
            runtime_version: None,
            runtime_sha256: None,
            rootfs_sha256: None,
            config_sha256: None,
        }
    }

    pub fn effective_limits(&self, limits: &Limits) -> EffectiveLimits {
        EffectiveLimits::from_enforcement(limits, &self.limit_enforcement, self.network_allowed)
    }
}

#[derive(Debug)]
pub struct ExecutionReport {
    pub outcome: io::Result<ExecOutcome>,
    pub provenance: ExecutionProvenance,
}

#[derive(Clone, Default)]
pub(crate) struct ExecutionObserver {
    ready: Arc<std::sync::atomic::AtomicBool>,
}

impl ExecutionObserver {
    pub(crate) fn mark_ready(&self) {
        self.ready.store(true, std::sync::atomic::Ordering::Release);
    }

    fn is_ready(&self) -> bool {
        self.ready.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[derive(Debug, Clone, Default)]
pub struct StreamTelemetry {
    pub bytes_seen: u64,
    pub bytes_emitted: u64,
    pub records_emitted: u64,
    pub sha256: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ExecTelemetry {
    pub wall_time_ms: u64,
    pub cpu_time_usec: Option<u64>,
    pub memory_peak_bytes: Option<u64>,
    pub stdout: StreamTelemetry,
    pub stderr: StreamTelemetry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    Off,
    Namespaces,
    Gvisor,
}

impl SandboxMode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "off" | "none" | "naive" => Ok(SandboxMode::Off),
            "ns" | "namespaces" | "sandbox" => Ok(SandboxMode::Namespaces),
            "gvisor" | "runsc" => Ok(SandboxMode::Gvisor),
            other => Err(format!("unknown sandbox mode {other:?}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SandboxMode::Off => "off",
            SandboxMode::Namespaces => "namespaces+cgroups-v2+private-rootfs",
            SandboxMode::Gvisor => "gvisor-oci",
        }
    }

    pub const fn isolation_class(self) -> IsolationClass {
        match self {
            SandboxMode::Off => IsolationClass::None,
            SandboxMode::Namespaces => IsolationClass::LinuxSharedKernel,
            SandboxMode::Gvisor => IsolationClass::GvisorApplicationKernel,
        }
    }
}

pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub async fn quiesce_stale_gvisor_workloads(jobs_root: &Path) -> io::Result<usize> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        gvisor::quiesce_stale_workloads(jobs_root).await
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        let _ = jobs_root;
        Ok(0)
    }
}

/// Detect provider-owned gVisor recovery state before provider selection.
///
/// Callers use this to force strict jobs-root validation and cgroup
/// quiescence even when the newly requested provider is Off. Otherwise an
/// operator switching providers after a launcher crash could leave the old
/// tenant workload running while startup merely reports the foreign state.
pub fn stale_gvisor_state_present(jobs_root: &Path) -> io::Result<bool> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        gvisor::stale_state_present(jobs_root)
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        let _ = jobs_root;
        Ok(false)
    }
}

async fn reject_foreign_gvisor_state(jobs_root: &Path) -> io::Result<()> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        if gvisor::stale_state_present(jobs_root)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "stale gVisor state was quiesced; restart once with the matching reviewed gVisor provider to delete its runtime state before selecting another provider",
            ));
        }
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    let _ = jobs_root;
    Ok(())
}

/// Inputs used by a provider's fail-closed startup execution gate.
#[derive(Debug, Clone)]
pub struct ProviderPreflight {
    pub jobs_root: PathBuf,
    pub rootfs: Option<PathBuf>,
    pub helper: Option<PathBuf>,
    pub seccomp: bool,
    pub interpreter_overrides: Vec<(String, Option<String>)>,
}

/// Runtime-provider boundary shared by built-in and external executors.
///
/// Implementations own execution, cleanup, crash reconciliation, and the
/// provenance that crosses their actual workload-ready boundary. Selection is
/// explicit at process startup; providers must never fall back to another
/// implementation after a launch failure.
pub trait ExecutionProvider: Send + Sync {
    fn mode(&self) -> SandboxMode;

    fn isolation_class(&self) -> IsolationClass {
        self.mode().isolation_class()
    }

    fn not_ready_provenance(&self) -> ExecutionProvenance {
        ExecutionProvenance::not_ready(self.mode())
    }

    fn execute<'a>(
        &'a self,
        ctx: ExecContext,
        sink: Arc<dyn Sink>,
    ) -> ProviderFuture<'a, ExecutionReport>;

    fn preflight<'a>(&'a self, input: ProviderPreflight) -> ProviderFuture<'a, io::Result<()>>;

    fn reconcile<'a>(&'a self, jobs_root: &'a Path) -> ProviderFuture<'a, io::Result<()>>;

    /// True when a failed execution left provider-owned recovery metadata in
    /// the job directory. The scheduler must preserve that directory for the
    /// next startup reconciliation instead of erasing the only cleanup handle.
    fn has_recovery_state(&self, _workdir: &Path) -> bool {
        false
    }
}

#[derive(Debug, Default)]
pub struct OffProvider;

impl ExecutionProvider for OffProvider {
    fn mode(&self) -> SandboxMode {
        SandboxMode::Off
    }

    fn execute<'a>(
        &'a self,
        ctx: ExecContext,
        sink: Arc<dyn Sink>,
    ) -> ProviderFuture<'a, ExecutionReport> {
        Box::pin(execute_reported(ctx, sink, SandboxMode::Off))
    }

    fn preflight<'a>(&'a self, _input: ProviderPreflight) -> ProviderFuture<'a, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn reconcile<'a>(&'a self, jobs_root: &'a Path) -> ProviderFuture<'a, io::Result<()>> {
        Box::pin(reject_foreign_gvisor_state(jobs_root))
    }
}

#[derive(Debug, Default)]
pub struct NamespaceProvider;

impl ExecutionProvider for NamespaceProvider {
    fn mode(&self) -> SandboxMode {
        SandboxMode::Namespaces
    }

    fn execute<'a>(
        &'a self,
        ctx: ExecContext,
        sink: Arc<dyn Sink>,
    ) -> ProviderFuture<'a, ExecutionReport> {
        Box::pin(execute_reported(ctx, sink, SandboxMode::Namespaces))
    }

    fn preflight<'a>(&'a self, input: ProviderPreflight) -> ProviderFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let rootfs = input.rootfs.as_deref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "namespace preflight requires rootfs",
                )
            })?;
            let helper = input.helper.as_deref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "namespace preflight requires helper",
                )
            })?;
            let overrides = input
                .interpreter_overrides
                .iter()
                .map(|(language, executable)| (language.as_str(), executable.as_deref()))
                .collect::<Vec<_>>();
            namespace_sandbox_execution_preflight(
                rootfs,
                helper,
                &input.jobs_root,
                input.seccomp,
                &overrides,
            )
            .await
        })
    }

    fn reconcile<'a>(&'a self, jobs_root: &'a Path) -> ProviderFuture<'a, io::Result<()>> {
        Box::pin(reject_foreign_gvisor_state(jobs_root))
    }
}

pub fn namespace_sandbox_available() -> bool {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        match namespace_sandbox_host_preflight() {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(error = %error, "namespace sandbox host preflight failed");
                false
            }
        }
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        false
    }
}

/// Validate the writable delegated cgroup-v2 controls used by every namespace
/// job. Rootfs/helper namespace bootstrap is validated by the server's
/// disposable execution preflight before it opens admission.
pub fn namespace_sandbox_host_preflight() -> io::Result<()> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        linux_sandbox::preflight_cgroup_runtime()
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the namespace runtime is supported only on Linux x86_64",
        ))
    }
}

/// Run one static, disposable job for every supplied interpreter through the
/// complete helper, namespace, private-root, privilege-drop, cgroup, and
/// optional seccomp path. Servers should complete this before opening
/// admission or reporting readiness.
pub async fn namespace_sandbox_execution_preflight(
    rootfs: &Path,
    helper: &Path,
    jobs_root: &Path,
    seccomp: bool,
    interpreter_overrides: &[(&str, Option<&str>)],
) -> io::Result<()> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        #[derive(Default)]
        struct PreflightSink {
            saw_sentinel: std::sync::atomic::AtomicBool,
        }
        impl Sink for PreflightSink {
            fn output(&self, stream: Stream, line: String) {
                if stream == Stream::Stdout && line == "COOP_PREFLIGHT_OK" {
                    self.saw_sentinel
                        .store(true, std::sync::atomic::Ordering::Release);
                }
            }
            fn violation(&self, _rule: &'static str, _detail: Value) {}
            fn truncated(&self, _stream: Stream) {}
        }

        if interpreter_overrides.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "namespace execution preflight requires at least one interpreter",
            ));
        }
        for (language, _) in interpreter_overrides {
            if !matches!(*language, "python" | "node" | "bash") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unsupported preflight language {language:?}"),
                ));
            }
        }
        namespace_sandbox_host_preflight()?;
        for (index, (language, override_bin)) in interpreter_overrides.iter().enumerate() {
            let code = match *language {
                // Exercise the private device nodes after the workload has
                // dropped to nobody. This also makes startup readiness catch
                // inherited-umask regressions that mask mknod permissions.
                "python" => {
                    r#"import os, stat
for path, expected_mode in (('/dev/null', 0o666), ('/dev/urandom', 0o444)):
    metadata = os.stat(path, follow_symlinks=False)
    assert stat.S_ISCHR(metadata.st_mode), (path, metadata.st_mode)
    assert (metadata.st_uid, metadata.st_gid) == (0, 0), (path, metadata.st_uid, metadata.st_gid)
    assert stat.S_IMODE(metadata.st_mode) == expected_mode, (path, stat.S_IMODE(metadata.st_mode))
with open('/dev/urandom', 'rb', buffering=0) as source:
    assert len(source.read(32)) == 32
with open('/dev/null', 'wb', buffering=0) as sink:
    assert sink.write(b'COOP_DEVICE_CANARY') == 18
print('COOP_PREFLIGHT_OK')"#
                }
                // Keep stdout in the Node canary. Node probes io_uring while
                // bringing up its async output path, so a no-op script would
                // not protect against overly fatal seccomp policy regressions.
                "node" => "console.log('COOP_PREFLIGHT_OK')",
                "bash" => "printf '%s\\n' COOP_PREFLIGHT_OK",
                _ => unreachable!("preflight languages were validated above"),
            };
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| {
                    io::Error::other(format!(
                        "{language} namespace execution preflight could not create an identifier: {error}"
                    ))
                })?
                .as_nanos();
            let suffix = format!("{}-{index}-{unique:x}", std::process::id());
            let job_key = format!("startup-{}", &suffix[..suffix.len().min(48)]);
            let workdir = jobs_root.join(format!(".coop-preflight-{suffix}"));
            std::fs::create_dir(&workdir).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "{language} namespace execution preflight could not create workdir {}: {error}",
                        workdir.display()
                    ),
                )
            })?;
            if let Err(error) = owner_only_dir(&workdir) {
                let _ = std::fs::remove_dir(&workdir);
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "{language} namespace execution preflight could not secure workdir {}: {error}",
                        workdir.display()
                    ),
                ));
            }
            let ctx = ExecContext {
                job_key,
                language: (*language).to_string(),
                code: code.to_string(),
                stdin: None,
                limits: Limits::default(),
                workdir: workdir.clone(),
                interpreter_override: override_bin.map(ToString::to_string),
                rootfs: Some(rootfs.to_path_buf()),
                helper_path: Some(helper.to_path_buf()),
                cancel: None,
                start_gate: None,
                seccomp,
            };
            let sink = Arc::new(PreflightSink::default());
            let result = linux_sandbox::run(ctx, sink.clone()).await;
            let cleanup = std::fs::remove_dir_all(&workdir);
            let outcome = match (result, cleanup) {
                (Ok(outcome), Ok(())) => outcome,
                (Ok(_), Err(error)) => {
                    return Err(io::Error::other(format!(
                        "remove {language} namespace preflight workdir {}: {error}",
                        workdir.display()
                    )))
                }
                (Err(error), Ok(())) => {
                    return Err(io::Error::new(
                        error.kind(),
                        format!("{language} namespace execution preflight failed: {error}"),
                    ))
                }
                (Err(error), Err(cleanup_error)) => {
                    return Err(io::Error::new(
                        error.kind(),
                        format!(
                            "{language} namespace execution preflight failed: {error}; cleanup of {} also failed: {cleanup_error}",
                            workdir.display()
                        ),
                    ))
                }
            };
            if outcome.status != coop_types::OutcomeStatus::Succeeded {
                return Err(io::Error::other(format!(
                    "{language} namespace execution preflight ended with {:?}",
                    outcome.status
                )));
            }
            if !sink.saw_sentinel.load(std::sync::atomic::Ordering::Acquire) {
                return Err(io::Error::other(format!(
                    "{language} namespace execution preflight did not emit its exact success sentinel"
                )));
            }
        }
        Ok(())
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        let _ = (rootfs, helper, jobs_root, seccomp, interpreter_overrides);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the namespace runtime is supported only on Linux x86_64",
        ))
    }
}

pub async fn execute(
    ctx: ExecContext,
    sink: Arc<dyn Sink>,
    mode: SandboxMode,
) -> io::Result<ExecOutcome> {
    execute_reported(ctx, sink, mode).await.outcome
}

/// Execute while retaining backend readiness even when the attempt returns an
/// I/O error. This is the evidence-bearing entry point for the server.
pub async fn execute_reported(
    ctx: ExecContext,
    sink: Arc<dyn Sink>,
    mode: SandboxMode,
) -> ExecutionReport {
    if mode == SandboxMode::Gvisor {
        return ExecutionReport {
            outcome: Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "gVisor execution requires an explicitly configured GvisorProvider; refusing subprocess fallback",
            )),
            provenance: ExecutionProvenance::not_ready(mode),
        };
    }
    let seccomp_requested = ctx.seccomp;
    let observer = ExecutionObserver::default();
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    if mode == SandboxMode::Namespaces {
        let outcome = linux_sandbox::run_observed(ctx, sink, observer.clone()).await;
        return ExecutionReport {
            outcome,
            provenance: ExecutionProvenance::observed(mode, seccomp_requested, observer.is_ready()),
        };
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    if mode == SandboxMode::Namespaces {
        return ExecutionReport {
            outcome: Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the namespace/seccomp backend supports Linux x86_64 only; refusing unisolated fallback",
            )),
            provenance: ExecutionProvenance::not_ready(mode),
        };
    }
    let outcome = naive::run_observed(ctx, sink, observer.clone()).await;
    ExecutionReport {
        outcome,
        provenance: ExecutionProvenance::observed(mode, seccomp_requested, observer.is_ready()),
    }
}

/// Resolve and startup-probe one host interpreter for the development
/// subprocess backend. The canary runs under the same sanitized environment
/// and platform process-tree supervision as a real job, with a fixed timeout.
///
/// The returned value is an exact executable path. Servers should cache that
/// path during startup/capability discovery and pass it as `interpreter_override`
/// for admitted jobs, avoiding both PATH drift and per-job probing.
pub async fn preflight_naive_interpreter(
    language: &str,
    override_bin: Option<&str>,
) -> io::Result<String> {
    naive::preflight_interpreter(language, override_bin).await
}

/// Resolve the executable used for a language.
///
/// Windows Bash is deliberately special: the operating system commonly
/// exposes a `System32\\bash.exe` WSL launcher that cannot consume the native
/// `C:\\...` source paths used by the development executor. Resolution there
/// rejects known WSL/application-alias locations and prefers Git for Windows,
/// but does not launch the candidate. Capability/admission callers must use
/// `preflight_naive_interpreter` to execute the bounded startup canary and
/// cache its returned exact path.
pub fn resolve_interpreter(language: &str, override_bin: Option<&str>) -> io::Result<String> {
    if override_bin.is_some_and(str::is_empty) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{language} interpreter override must not be empty"),
        ));
    }

    #[cfg(windows)]
    if language == "bash" {
        return naive::resolve_native_windows_bash(override_bin);
    }

    if let Some(bin) = override_bin {
        return Ok(bin.to_string());
    }
    let interpreter = match language {
        "python" => {
            if cfg!(windows) {
                "python".to_string()
            } else {
                "python3".to_string()
            }
        }
        "node" => "node".to_string(),
        "bash" => "bash".to_string(),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported interpreter language {language:?}"),
            ))
        }
    };
    Ok(interpreter)
}

pub fn ext_for(language: &str) -> &'static str {
    match language {
        "python" => "py",
        "node" => "js",
        _ => "sh",
    }
}

/// N-1: the jobs root and per-job workdirs hold tenant source and artifacts.
/// Mode 0700 keeps them traversable by the server account only, so a job
/// running under another local uid (e.g. `nobody` in the sandbox) cannot
/// enumerate or enter sibling workdirs. No-op where POSIX modes do not exist,
/// so host builds for other platforms stay green.
pub fn owner_only_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// N-1: job source files contain another tenant's code. Mode 0600 restricts
/// them to the server account; sandboxed jobs get a sanitized staging copy
/// instead of host-path access (see `linux_sandbox`). No-op off unix.
pub fn owner_only_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Create-or-truncate `path` and write `bytes`, applying mode 0600 *at
/// creation time* on unix. Writing first and chmodding after leaves a brief
/// umask-dependent window in which tenant source is world-readable on the
/// host; opening with the mode up front closes it.
pub fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn closing_start_gate_waits_only_for_active_launch_permits() {
        let gate = Arc::new(ExecutionStartGate::default());
        let permit = gate.enter().expect("open gate");
        let (closed_tx, closed_rx) = mpsc::channel();
        let closer_gate = Arc::clone(&gate);
        let closer = std::thread::spawn(move || {
            closer_gate.close();
            closed_tx.send(()).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while !gate.is_closed() {
            assert!(
                Instant::now() < deadline,
                "close did not publish its sticky state"
            );
            std::thread::yield_now();
        }
        assert!(
            closed_rx.try_recv().is_err(),
            "close returned before the active launch was acknowledged"
        );

        drop(permit);
        closed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("close must drain after the launch permit is released");
        closer.join().unwrap();
        assert!(gate.enter().is_err(), "a closed gate must stay closed");
    }
}
