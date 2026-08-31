//! Per-job gVisor OCI execution provider.
//!
//! Every attempt receives a unique OCI bundle, runsc state root, cgroup, and
//! container ID. A one-way inherited pipe carries the readiness nonce from the
//! trusted in-sandbox PID1; no host filesystem socket or FIFO is mounted into
//! the sandbox. The provider never invokes `runsc do` and never falls back to
//! another executor.

use crate::bounded_output::BoundedOutput;
use crate::linux_sandbox::{
    cgroup_populated_checked, cleanup_job_cgroup, cleanup_job_cgroup_by_key,
    create_job_cgroup_with_pids_overhead, read_named_counter_checked, read_scalar,
    resolve_rootfs_interpreter, validate_rootfs, CgroupLease,
};
use crate::{
    ext_for, ExecContext, ExecOutcome, ExecTelemetry, ExecutionProvenance, ExecutionProvider,
    ExecutionReport, ProviderFuture, ProviderPreflight, SandboxMode, Sink, Stream,
};
use coop_types::{
    IsolationClass, LimitEnforcement, Limits, OutcomeStatus, MAX_CODE_BYTES, MAX_STDIN_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

pub const REVIEWED_RUNSC_VERSION: &str = "runsc version release-20260817.0";
pub const REVIEWED_RUNSC_SHA256_X86_64: &str =
    "048b89aada69dc3333422e139d6e9d02f8ab06bda52398060e0fbdacca00074c";
pub const OCI_INIT_PATH: &str = "/usr/local/bin/rookhold-oci-init";
pub const ROOTFS_MANIFEST_PATH: &str = "/.coop-rootfs.manifest";

const CONTROL_TICK: Duration = Duration::from_millis(20);
const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(15);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
const CONTROL_EXECUTABLE_BUSY_RETRIES: usize = 4;
const CONTROL_EXECUTABLE_BUSY_BACKOFF: Duration = Duration::from_millis(10);
const DRAIN_GRACE: Duration = Duration::from_secs(2);
const MAX_READY_FRAME: usize = 128;
const PRE_READY_OUTPUT_LIMIT: usize = 64 * 1024;
const READY_PREFIX: &str = "COOP_GVISOR_READY_V1\t";
const RUNTIME_DIR: &str = ".coop-gvisor";
const LEASE_FILE: &str = "lease.json";
const LEASE_VERSION: u32 = 1;
const RUNTIME_PID_OVERHEAD: u32 = 128;
const MIN_HOST_MAX_MAP_COUNT: u64 = 4_194_304;
const WORKLOAD_UID_DEFAULT: u32 = 65_534;
const WORKLOAD_GID_DEFAULT: u32 = 65_534;

/// Kill every cgroup associated with discoverable gVisor recovery state
/// without requiring runsc or the rootfs to be present. This must run before
/// validating the newly selected provider so a broken upgrade cannot leave
/// old tenant code executing indefinitely.
pub async fn quiesce_stale_workloads(jobs_root: &Path) -> io::Result<usize> {
    let mut found = 0;
    for entry in provider_job_directories(jobs_root)? {
        if !entry.path().join(RUNTIME_DIR).exists() {
            continue;
        }
        let name = entry.file_name();
        let name = name
            .to_str()
            .and_then(|name| name.strip_prefix("job-"))
            .filter(|key| valid_identifier(key, 64))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid gVisor recovery directory",
                )
            })?;
        cleanup_job_cgroup_by_key(name).await?;
        found += 1;
    }
    Ok(found)
}

pub fn stale_state_present(jobs_root: &Path) -> io::Result<bool> {
    Ok(provider_job_directories(jobs_root)?
        .into_iter()
        .any(|entry| entry.path().join(RUNTIME_DIR).exists()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GvisorPlatform {
    Systrap,
    Kvm,
}

impl GvisorPlatform {
    pub fn parse(value: &str) -> io::Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "systrap" => Ok(Self::Systrap),
            "kvm" => Ok(Self::Kvm),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ROOKHOLD_GVISOR_PLATFORM must be systrap or kvm",
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Systrap => "systrap",
            Self::Kvm => "kvm",
        }
    }
}

#[derive(Debug, Clone)]
pub struct GvisorProvider {
    runsc: PathBuf,
    rootfs: PathBuf,
    platform: GvisorPlatform,
    uid: u32,
    gid: u32,
    runtime_version: String,
    runtime_sha256: String,
    rootfs_sha256: String,
    init_sha256: String,
}

impl GvisorProvider {
    pub async fn new(
        runsc: PathBuf,
        rootfs: PathBuf,
        platform: GvisorPlatform,
        uid: Option<u32>,
        gid: Option<u32>,
        rootfs_sha256: String,
    ) -> io::Result<Self> {
        let uid = uid.unwrap_or(WORKLOAD_UID_DEFAULT);
        let gid = gid.unwrap_or(WORKLOAD_GID_DEFAULT);
        if uid == 0 || gid == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "gVisor workload UID and GID must be nonzero",
            ));
        }
        validate_host_settings()?;
        let runsc = validate_trusted_executable(&runsc, "ROOKHOLD_GVISOR_RUNSC")?;
        let runtime_sha256 = hash_file(&runsc)?;
        if runtime_sha256 != REVIEWED_RUNSC_SHA256_X86_64 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "ROOKHOLD_GVISOR_RUNSC digest {runtime_sha256} is not the reviewed {REVIEWED_RUNSC_SHA256_X86_64}"
                ),
            ));
        }
        let runtime_version = read_runsc_version(&runsc).await?;
        if !runtime_version.starts_with(REVIEWED_RUNSC_VERSION) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("unexpected reviewed runsc version: {runtime_version:?}"),
            ));
        }

        let rootfs = validate_rootfs_root(&rootfs)?;
        let init = rootfs.join(OCI_INIT_PATH.trim_start_matches('/'));
        let _ = validate_trusted_executable(&init, "gVisor OCI init")?;
        let init_sha256 = hash_file(&init)?;
        if !valid_sha256(&rootfs_sha256) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "configured gVisor rootfs digest must be 64 hexadecimal characters",
            ));
        }
        let manifest = rootfs.join(ROOTFS_MANIFEST_PATH.trim_start_matches('/'));
        let manifest_digest = rootfs_sha256.clone();
        let manifest_init = init_sha256.clone();
        let manifest_root = rootfs.clone();
        tokio::task::spawn_blocking(move || {
            validate_rootfs_manifest(&manifest_root, &manifest, &manifest_digest, &manifest_init)
        })
        .await
        .map_err(|error| io::Error::other(format!("rootfs manifest task failed: {error}")))??;

        Ok(Self {
            runsc,
            rootfs,
            platform,
            uid,
            gid,
            runtime_version,
            runtime_sha256,
            rootfs_sha256,
            init_sha256,
        })
    }

    pub fn configured_provenance(
        &self,
        ready: bool,
        config_sha256: Option<String>,
    ) -> ExecutionProvenance {
        let observed = ready.then_some(());
        ExecutionProvenance {
            backend: SandboxMode::Gvisor.as_str().to_string(),
            isolation_class: if ready {
                IsolationClass::GvisorApplicationKernel
            } else {
                IsolationClass::None
            },
            bootstrap_ready: ready,
            isolated: ready,
            private_rootfs: ready,
            dedicated_bootstrap: ready,
            // This is distinct from runsc's own host seccomp sandbox. Rookhold
            // does not claim that its namespace guest filter was installed.
            seccomp: false,
            network_allowed: observed.map(|()| false),
            networking: observed.map(|()| "disabled".to_string()),
            limit_enforcement: if ready {
                LimitEnforcement::NAMESPACE_SANDBOX
            } else {
                LimitEnforcement::NONE
            },
            runtime_version: observed.map(|()| self.runtime_version.clone()),
            runtime_sha256: observed.map(|()| self.runtime_sha256.clone()),
            rootfs_sha256: observed.map(|()| self.rootfs_sha256.clone()),
            config_sha256: observed.and(config_sha256),
        }
    }

    async fn run_observed(&self, ctx: ExecContext, sink: Arc<dyn Sink>) -> ExecutionReport {
        match self.run_inner(ctx, sink).await {
            Ok((outcome, config_sha256, ready)) => ExecutionReport {
                outcome: Ok(outcome),
                provenance: self.configured_provenance(ready, Some(config_sha256)),
            },
            Err(failure) => ExecutionReport {
                outcome: Err(failure.error),
                provenance: self.configured_provenance(failure.ready, failure.config_sha256),
            },
        }
    }

    async fn run_inner(
        &self,
        ctx: ExecContext,
        sink: Arc<dyn Sink>,
    ) -> Result<(ExecOutcome, String, bool), RunFailure> {
        // Keep the provider boundary safe for direct callers as well as the
        // HTTP server. Payload staging and the stdin writer live outside the
        // guest cgroup, so reject oversized input before allocating runtime
        // state, writing the jobs filesystem, or cloning stdin.
        if ctx.code.len() > MAX_CODE_BYTES {
            return Err(RunFailure::before_ready(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("code exceeds {MAX_CODE_BYTES} bytes"),
            )));
        }
        if ctx
            .stdin
            .as_ref()
            .is_some_and(|value| value.len() > MAX_STDIN_BYTES)
        {
            return Err(RunFailure::before_ready(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("stdin exceeds {MAX_STDIN_BYTES} bytes"),
            )));
        }
        let rootfs =
            validate_rootfs(&self.rootfs, &ctx.workdir).map_err(RunFailure::before_ready)?;
        let interpreter =
            crate::resolve_interpreter(&ctx.language, ctx.interpreter_override.as_deref())
                .and_then(|configured| resolve_rootfs_interpreter(&rootfs, &configured))
                .map_err(RunFailure::before_ready)?;
        let nonce = random_nonce().map_err(RunFailure::before_ready)?;
        let container_id = container_id(&ctx.job_key, &nonce).map_err(RunFailure::before_ready)?;

        let runtime_dir = ctx.workdir.join(RUNTIME_DIR);
        let bundle = runtime_dir.join("bundle");
        let state_root = runtime_dir.join("state");
        let payload = runtime_dir.join("payload");
        let input = ctx.workdir.join("input");
        let output = ctx.workdir.join("output");
        create_private_dir(&runtime_dir).map_err(RunFailure::before_ready)?;
        create_private_dir(&bundle).map_err(RunFailure::before_ready)?;
        create_private_dir(&state_root).map_err(RunFailure::before_ready)?;
        prepare_payload(&payload, &ctx).map_err(RunFailure::before_ready)?;

        let cgroup =
            create_job_cgroup_with_pids_overhead(&ctx.job_key, &ctx.limits, RUNTIME_PID_OVERHEAD)
                .map_err(RunFailure::before_ready)?;
        let cgroup_path = cgroup.path().to_path_buf();
        let cgroup_oci_path = cgroup_oci_path(&cgroup_path).map_err(RunFailure::before_ready)?;
        let cpu_before = read_named_counter_checked(cgroup_path.join("cpu.stat"), "usage_usec")
            .map_err(RunFailure::before_ready)?;
        let oom_before = read_named_counter_checked(cgroup_path.join("memory.events"), "oom_kill")
            .map_err(RunFailure::before_ready)?;

        let source = format!("/work/job.{}", ext_for(&ctx.language));
        let spec = build_spec(
            &rootfs,
            &payload,
            &input,
            &output,
            &cgroup_oci_path,
            &ctx.limits,
            self.uid,
            self.gid,
            &nonce,
            &interpreter,
            &source,
            &self.init_sha256,
            &self.rootfs_sha256,
            self.platform.as_str(),
            &self.runtime_sha256,
        );
        let spec_bytes = serde_json::to_vec(&spec)
            .map_err(|error| RunFailure::before_ready(io::Error::other(error)))?;
        let config_sha256 = hex(Sha256::digest(&spec_bytes).as_slice());
        write_fixed_file(&bundle.join("config.json"), &spec_bytes, 0o400)
            .map_err(RunFailure::before_ready)?;
        let lease = LeaseMetadata {
            version: LEASE_VERSION,
            container_id: container_id.clone(),
            job_key: ctx.job_key.clone(),
            runtime_sha256: self.runtime_sha256.clone(),
            config_sha256: config_sha256.clone(),
        };
        let lease_bytes = serde_json::to_vec(&lease)
            .map_err(|error| RunFailure::before_ready(io::Error::other(error)))?;
        write_lease_atomic(&runtime_dir, &lease_bytes).map_err(RunFailure::before_ready)?;

        let (ready_read, ready_write) = pipe_cloexec().map_err(RunFailure::before_ready)?;
        let mut command = self.run_command(&state_root, &runtime_dir);
        command
            .arg("run")
            .arg(format!("--bundle={}", bundle.display()))
            .arg("--pass-fd=3:3")
            .arg(format!(
                "--user-log={}",
                runtime_dir.join("user.log").display()
            ))
            .arg(&container_id)
            .current_dir(&runtime_dir)
            .stdin(if ctx.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let launch = match ctx.begin_process_launch() {
            Ok(permit) => permit,
            Err(reason) => {
                cleanup_unstarted(&runtime_dir, cgroup)
                    .await
                    .map_err(RunFailure::before_ready)?;
                return Ok((
                    ExecOutcome::cancelled_before_launch(reason),
                    config_sha256,
                    false,
                ));
            }
        };
        // Move the runsc launcher into the already-bounded job cgroup in the
        // post-fork/pre-exec child. This closes the crash window in which the
        // server could die after authorizing spawn but before runsc created
        // queryable OCI state: startup reconciliation's cgroup.kill then
        // covers the launcher itself as well as every eventual descendant.
        let cgroup_procs = match fs::OpenOptions::new()
            .write(true)
            .open(cgroup_path.join("cgroup.procs"))
        {
            Ok(file) => file,
            Err(error) => {
                drop(launch);
                cleanup_unstarted(&runtime_dir, cgroup)
                    .await
                    .map_err(RunFailure::before_ready)?;
                return Err(RunFailure::before_ready(io::Error::new(
                    error.kind(),
                    format!("open job cgroup.procs before runsc spawn: {error}"),
                )));
            }
        };
        prepare_runsc_child(
            &mut command,
            ready_write.as_raw_fd(),
            cgroup_procs.as_raw_fd(),
        );
        let spawn = command.spawn();
        drop(cgroup_procs);
        let mut child = match spawn {
            Ok(child) => child,
            Err(error) => {
                let cleanup = cleanup_unstarted(&runtime_dir, cgroup).await;
                let error = match cleanup {
                    Ok(()) => error,
                    Err(cleanup) => io::Error::new(
                        cleanup.kind(),
                        format!("runsc spawn failed: {error}; cleanup also failed: {cleanup}"),
                    ),
                };
                return Err(RunFailure {
                    error,
                    ready: false,
                    config_sha256: Some(config_sha256),
                });
            }
        };
        drop(ready_write);
        drop(launch);

        let stdin_task = if let Some(mut stdin) = child.stdin.take() {
            let input = ctx.stdin.clone().unwrap_or_default();
            Some(tokio::spawn(async move {
                let _ = stdin.write_all(input.as_bytes()).await;
                let _ = stdin.shutdown().await;
            }))
        } else {
            None
        };
        let mut stdout = child.stdout.take().ok_or_else(|| RunFailure {
            error: io::Error::other("runsc stdout was not piped"),
            ready: false,
            config_sha256: Some(config_sha256.clone()),
        })?;
        let mut stderr = child.stderr.take().ok_or_else(|| RunFailure {
            error: io::Error::other("runsc stderr was not piped"),
            ready: false,
            config_sha256: Some(config_sha256.clone()),
        })?;
        let ready_file = fs::File::from(ready_read);
        let mut ready_reader = tokio::fs::File::from_std(ready_file);

        let result = self
            .supervise(
                &ctx,
                sink,
                &container_id,
                &state_root,
                &runtime_dir,
                &cgroup_path,
                cpu_before,
                oom_before,
                nonce,
                &mut child,
                &mut ready_reader,
                &mut stdout,
                &mut stderr,
            )
            .await;
        if let Some(task) = stdin_task {
            task.abort();
        }

        let cleanup = self
            .cleanup_runtime(&state_root, &runtime_dir, &container_id, cgroup)
            .await;
        match (result, cleanup) {
            (Ok((outcome, ready)), Ok(())) => Ok((outcome, config_sha256, ready)),
            (Ok((_, ready)), Err(error)) => Err(RunFailure {
                error,
                ready,
                config_sha256: Some(config_sha256),
            }),
            (Err(mut failure), Ok(())) => {
                failure.config_sha256 = Some(config_sha256);
                Err(failure)
            }
            (Err(failure), Err(cleanup_error)) => Err(RunFailure {
                error: io::Error::new(
                    cleanup_error.kind(),
                    format!(
                        "{}; gVisor cleanup also failed: {cleanup_error}",
                        failure.error
                    ),
                ),
                ready: failure.ready,
                config_sha256: Some(config_sha256),
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn supervise(
        &self,
        ctx: &ExecContext,
        sink: Arc<dyn Sink>,
        container_id: &str,
        state_root: &Path,
        runtime_dir: &Path,
        cgroup: &Path,
        cpu_before: u64,
        oom_before: u64,
        nonce: String,
        child: &mut Child,
        ready_reader: &mut tokio::fs::File,
        stdout: &mut tokio::process::ChildStdout,
        stderr: &mut tokio::process::ChildStderr,
    ) -> Result<(ExecOutcome, bool), RunFailure> {
        let started = Instant::now();
        let wall_deadline = started + Duration::from_secs(ctx.limits.wall_seconds.max(1) as u64);
        let bootstrap_deadline = started + BOOTSTRAP_TIMEOUT;
        let cpu_budget = u64::from(ctx.limits.cpu_seconds.max(1)) * 1_000_000;
        let mut tick = tokio::time::interval(CONTROL_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut stdout_buf = [0_u8; 8192];
        let mut stderr_buf = [0_u8; 8192];
        let mut stdout_capture = BoundedOutput::new(Stream::Stdout);
        let mut stderr_capture = BoundedOutput::new(Stream::Stderr);
        let mut pre_stdout = Vec::new();
        let mut pre_stderr = Vec::new();
        let mut ready_buf = Vec::new();
        let mut ready_done = false;
        let mut ready = false;
        let mut status = None;
        let mut stdout_done = false;
        let mut stderr_done = false;
        let mut timed_out = false;
        let mut cpu_exceeded = false;
        let mut cancelled = false;
        let mut bootstrap_failed = false;
        let mut kill_started = None;
        let mut drain_deadline = None;

        while status.is_none() || !stdout_done || !stderr_done {
            let drain = async {
                match drain_deadline {
                    Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
                    None => std::future::pending::<()>().await,
                }
            };
            let read_ready = async {
                if ready_done {
                    std::future::pending::<io::Result<(usize, [u8; 64])>>().await
                } else {
                    let mut byte = [0_u8; 64];
                    let count = ready_reader.read(&mut byte).await?;
                    Ok((count, byte))
                }
            };
            let read_stdout = async {
                if stdout_done {
                    std::future::pending::<io::Result<usize>>().await
                } else {
                    stdout.read(&mut stdout_buf).await
                }
            };
            let read_stderr = async {
                if stderr_done {
                    std::future::pending::<io::Result<usize>>().await
                } else {
                    stderr.read(&mut stderr_buf).await
                }
            };

            tokio::select! {
                biased;

                _ = tick.tick() => {
                    if status.is_none() {
                        let now = Instant::now();
                        let cause = if ctx.is_cancelled() && !cancelled && !timed_out && !cpu_exceeded {
                            cancelled = true;
                            sink.violation("job_cancelled", json!({}));
                            Some("cancelled")
                        } else if now >= wall_deadline && !cancelled && !timed_out && !cpu_exceeded {
                            timed_out = true;
                            sink.violation("wall_clock_exceeded", json!({"wall_seconds": ctx.limits.wall_seconds}));
                            Some("wall")
                        } else if read_named_counter_checked(cgroup.join("cpu.stat"), "usage_usec")
                            .map_err(|error| RunFailure::new(error, ready))?
                            .saturating_sub(cpu_before) >= cpu_budget
                            && !cancelled && !timed_out && !cpu_exceeded
                        {
                            cpu_exceeded = true;
                            sink.violation("cpu_time_exceeded", json!({"cpu_seconds": ctx.limits.cpu_seconds}));
                            Some("cpu")
                        } else if !ready && now >= bootstrap_deadline && !bootstrap_failed {
                            bootstrap_failed = true;
                            Some("bootstrap")
                        } else {
                            None
                        };
                        if cause.is_some() && kill_started.is_none() {
                            let _ = self.kill_runtime(state_root, runtime_dir, container_id).await;
                            kill_started = Some(Instant::now());
                        }
                        if let Some(killed_at) = kill_started {
                            if now.duration_since(killed_at) >= DRAIN_GRACE {
                                let _ = child.start_kill();
                            }
                        }
                        if let Some(exit) = child.try_wait().map_err(|error| RunFailure::new(error, ready))? {
                            status = Some(exit);
                            drain_deadline = Some(Instant::now() + DRAIN_GRACE);
                        }
                    }
                }

                _ = drain => {
                    stdout_done = true;
                    stderr_done = true;
                }

                result = read_ready => match result {
                    Ok((0, _)) => {
                        ready_done = true;
                        if !ready {
                            bootstrap_failed = true;
                            if kill_started.is_none() {
                                let _ = self.kill_runtime(state_root, runtime_dir, container_id).await;
                                kill_started = Some(Instant::now());
                            }
                        }
                    }
                    Ok((count, bytes)) => {
                        if ready_buf.len().saturating_add(count) > MAX_READY_FRAME {
                            bootstrap_failed = true;
                            ready_done = true;
                            if kill_started.is_none() {
                                let _ = self.kill_runtime(state_root, runtime_dir, container_id).await;
                                kill_started = Some(Instant::now());
                            }
                        } else {
                            ready_buf.extend_from_slice(&bytes[..count]);
                            let expected = format!("{READY_PREFIX}{nonce}\n");
                            if ready_buf.len() >= expected.len() {
                                ready_done = true;
                                let populated = cgroup_populated_checked(cgroup)
                                    .map_err(|error| RunFailure::new(error, false))?;
                                if ready_buf != expected.as_bytes() || !populated {
                                    bootstrap_failed = true;
                                    if kill_started.is_none() {
                                        let _ = self.kill_runtime(state_root, runtime_dir, container_id).await;
                                        kill_started = Some(Instant::now());
                                    }
                                } else {
                                    ready = true;
                                    stdout_capture.push(&pre_stdout, sink.as_ref());
                                    stderr_capture.push(&pre_stderr, sink.as_ref());
                                    pre_stdout.clear();
                                    pre_stderr.clear();
                                }
                            }
                        }
                    }
                    Err(error) => return Err(RunFailure::new(error, ready)),
                },

                read = read_stdout => match read {
                    Ok(0) => {
                        if ready { stdout_capture.finish(sink.as_ref()); }
                        stdout_done = true;
                    }
                    Ok(count) if ready => stdout_capture.push(&stdout_buf[..count], sink.as_ref()),
                    Ok(count) => push_pre_ready(&mut pre_stdout, &stdout_buf[..count])
                        .map_err(|error| RunFailure::new(error, false))?,
                    Err(error) => return Err(RunFailure::new(error, ready)),
                },

                read = read_stderr => match read {
                    Ok(0) => {
                        if ready { stderr_capture.finish(sink.as_ref()); }
                        stderr_done = true;
                    }
                    Ok(count) if ready => stderr_capture.push(&stderr_buf[..count], sink.as_ref()),
                    Ok(count) => push_pre_ready(&mut pre_stderr, &stderr_buf[..count])
                        .map_err(|error| RunFailure::new(error, false))?,
                    Err(error) => return Err(RunFailure::new(error, ready)),
                },
            }
        }

        stdout_capture.finish(sink.as_ref());
        stderr_capture.finish(sink.as_ref());
        let status = match status {
            Some(status) => status,
            None => child
                .wait()
                .await
                .map_err(|error| RunFailure::new(error, ready))?,
        };
        let cpu_after = read_named_counter_checked(cgroup.join("cpu.stat"), "usage_usec")
            .map_err(|error| RunFailure::new(error, ready))?;
        let oom_after = read_named_counter_checked(cgroup.join("memory.events"), "oom_kill")
            .map_err(|error| RunFailure::new(error, ready))?;
        let telemetry = ExecTelemetry {
            wall_time_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            cpu_time_usec: Some(cpu_after.saturating_sub(cpu_before)),
            memory_peak_bytes: read_scalar(cgroup.join("memory.peak")),
            stdout: stdout_capture.telemetry(),
            stderr: stderr_capture.telemetry(),
        };

        if !ready {
            let outcome = if cancelled {
                Some(ExecOutcome {
                    status: OutcomeStatus::Cancelled,
                    exit_code: status.code(),
                    killed_by: Some("cancelled".to_string()),
                    telemetry: telemetry.clone(),
                })
            } else if timed_out {
                Some(ExecOutcome {
                    status: OutcomeStatus::TimedOut,
                    exit_code: status.code(),
                    killed_by: Some("wall-clock".to_string()),
                    telemetry: telemetry.clone(),
                })
            } else if cpu_exceeded {
                Some(ExecOutcome {
                    status: OutcomeStatus::Failed,
                    exit_code: status.code(),
                    killed_by: Some("cgroup-cpu".to_string()),
                    telemetry: telemetry.clone(),
                })
            } else if oom_after > oom_before {
                Some(ExecOutcome {
                    status: OutcomeStatus::OomKilled,
                    exit_code: status.code(),
                    killed_by: Some("cgroup-oom".to_string()),
                    telemetry: telemetry.clone(),
                })
            } else {
                None
            };
            if let Some(outcome) = outcome {
                return Ok((outcome, false));
            }
            let detail = first_runtime_line(&pre_stderr)
                .or_else(|| first_runtime_line(&pre_stdout))
                .unwrap_or("runsc exited without a user-visible diagnostic");
            return Err(RunFailure::new(
                io::Error::other(format!(
                    "gVisor workload never crossed its authenticated ready boundary: {detail}"
                )),
                false,
            ));
        }
        if bootstrap_failed {
            return Err(RunFailure::new(
                io::Error::other("gVisor readiness became inconsistent after authentication"),
                true,
            ));
        }

        let outcome = if cancelled {
            ExecOutcome {
                status: OutcomeStatus::Cancelled,
                exit_code: status.code(),
                killed_by: Some("cancelled".to_string()),
                telemetry,
            }
        } else if timed_out {
            ExecOutcome {
                status: OutcomeStatus::TimedOut,
                exit_code: status.code(),
                killed_by: Some("wall-clock".to_string()),
                telemetry,
            }
        } else if cpu_exceeded {
            ExecOutcome {
                status: OutcomeStatus::Failed,
                exit_code: status.code(),
                killed_by: Some("cgroup-cpu".to_string()),
                telemetry,
            }
        } else if oom_after > oom_before {
            ExecOutcome {
                status: OutcomeStatus::OomKilled,
                exit_code: status.code(),
                killed_by: Some("cgroup-oom".to_string()),
                telemetry,
            }
        } else if status.success() {
            ExecOutcome {
                status: OutcomeStatus::Succeeded,
                exit_code: status.code(),
                killed_by: None,
                telemetry,
            }
        } else {
            ExecOutcome {
                status: OutcomeStatus::Failed,
                exit_code: status.code(),
                killed_by: status.signal().map(|signal| format!("signal-{signal}")),
                telemetry,
            }
        };
        Ok((outcome, true))
    }

    async fn runtime_container_present(
        &self,
        state_root: &Path,
        runtime_dir: &Path,
        container_id: &str,
    ) -> io::Result<bool> {
        let output = self
            .control_output(state_root, runtime_dir, &["list", "--format=json"])
            .await?;
        if !output.status.success() {
            return Err(control_error("list", &output));
        }
        if output.stdout.len() > 64 * 1024 || output.stderr.len() > 64 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "runsc list produced an excessive control response",
            ));
        }
        // runsc serializes its nil empty slice as JSON null and a populated
        // list as an array. Both are machine-readable stable states.
        let containers: Vec<ListedContainer> =
            serde_json::from_slice::<Option<Vec<ListedContainer>>>(&output.stdout)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
                .unwrap_or_default();
        if containers.len() > 1
            || containers
                .iter()
                .any(|container| container.id != container_id)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "per-job runsc state root contains an unexpected container",
            ));
        }
        Ok(containers
            .iter()
            .any(|container| container.id == container_id))
    }

    async fn ensure_runtime_deleted(
        &self,
        state_root: &Path,
        runtime_dir: &Path,
        container_id: &str,
        kill_first: bool,
    ) -> io::Result<()> {
        if !self
            .runtime_container_present(state_root, runtime_dir, container_id)
            .await?
        {
            return Ok(());
        }
        if kill_first {
            // A stopped container can reject kill. Deletion below is forced
            // and its authoritative list verification remains mandatory.
            let _ = self
                .kill_runtime(state_root, runtime_dir, container_id)
                .await;
        }
        let delete = self
            .control_output(
                state_root,
                runtime_dir,
                &["delete", "--force", container_id],
            )
            .await;
        let present = self
            .runtime_container_present(state_root, runtime_dir, container_id)
            .await?;
        if !present {
            return Ok(());
        }
        match delete {
            Ok(output) if output.status.success() => Err(io::Error::other(
                "runsc delete returned success but the container remained listed",
            )),
            Ok(output) => Err(control_error("delete", &output)),
            Err(error) => Err(error),
        }
    }

    async fn cleanup_runtime(
        &self,
        state_root: &Path,
        runtime_dir: &Path,
        container_id: &str,
        cgroup: CgroupLease,
    ) -> io::Result<()> {
        let runtime_cleanup = self
            .ensure_runtime_deleted(state_root, runtime_dir, container_id, false)
            .await;
        let cgroup_cleanup = cleanup_job_cgroup(cgroup).await;
        match (runtime_cleanup, cgroup_cleanup) {
            (Err(runtime), Err(cgroup)) => {
                return Err(io::Error::new(
                    runtime.kind(),
                    format!("{runtime}; cgroup cleanup also failed: {cgroup}"),
                ));
            }
            (Err(error), Ok(())) | (Ok(()), Err(error)) => return Err(error),
            (Ok(()), Ok(())) => {}
        }
        match fs::remove_dir_all(runtime_dir) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io::Error::new(
                error.kind(),
                format!("remove gVisor runtime directory: {error}"),
            )),
        }
    }

    async fn kill_runtime(
        &self,
        state_root: &Path,
        runtime_dir: &Path,
        container_id: &str,
    ) -> io::Result<()> {
        let output = self
            .control_output(
                state_root,
                runtime_dir,
                &["kill", "--all", container_id, "SIGKILL"],
            )
            .await?;
        if output.status.success() {
            Ok(())
        } else {
            Err(control_error("kill", &output))
        }
    }

    async fn control_output(
        &self,
        state_root: &Path,
        runtime_dir: &Path,
        arguments: &[&str],
    ) -> io::Result<std::process::Output> {
        let deadline = tokio::time::Instant::now() + CONTROL_TIMEOUT;
        for attempt in 0..=CONTROL_EXECUTABLE_BUSY_RETRIES {
            let mut command = self.run_command(state_root, runtime_dir);
            command
                .args(arguments)
                .stdin(Stdio::null())
                .kill_on_drop(true);
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "runsc control command timed out",
                ));
            }
            match tokio::time::timeout(remaining, command.output()).await {
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "runsc control command timed out",
                    ));
                }
                Ok(Err(error))
                    if error.kind() == io::ErrorKind::ExecutableFileBusy
                        && attempt < CONTROL_EXECUTABLE_BUSY_RETRIES =>
                {
                    tokio::time::sleep(CONTROL_EXECUTABLE_BUSY_BACKOFF).await;
                }
                Ok(result) => return result,
            }
        }
        unreachable!("bounded runsc control retries always return")
    }

    fn run_command(&self, state_root: &Path, runtime_dir: &Path) -> Command {
        let mut command = Command::new(&self.runsc);
        command
            .env_clear()
            .arg(format!("--root={}", state_root.display()))
            .arg(format!("--shared-root={}", state_root.display()))
            .arg(format!("--platform={}", self.platform.as_str()))
            .arg("--network=none")
            .arg("--host-uds=none")
            .arg("--host-fifo=none")
            .arg("--directfs=false")
            .arg("--overlay2=none")
            .arg("--allow-suid=false")
            .arg("--net-raw=false")
            .arg("--allow-flag-override=false")
            .arg("--gofer-network-namespace=new")
            .arg("--gvisor-marker-file=true")
            .arg("--sidecar-release-enforcement-policy=ALWAYS")
            // Host settings are validated by Rookhold before the protected
            // service starts. runsc must not need write access to kernel
            // tunables from inside the systemd sandbox.
            .arg("--host-settings=check")
            .arg("--watchdog-action=panic")
            .arg(format!("--log={}", runtime_dir.join("runsc.log").display()))
            .arg(format!(
                "--panic-log={}",
                runtime_dir.join("panic.log").display()
            ));
        command
    }

    async fn reconcile_inner(&self, jobs_root: &Path) -> io::Result<()> {
        for entry in provider_job_directories(jobs_root)? {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let directory_key = name.strip_prefix("job-").unwrap_or("");
            if !valid_identifier(directory_key, 64) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "gVisor reconciliation encountered an invalid job directory",
                ));
            }
            let runtime_dir = entry.path().join(RUNTIME_DIR);
            if !runtime_dir.exists() {
                continue;
            }
            let lease_path = runtime_dir.join(LEASE_FILE);
            let lease_bytes = match fs::read(&lease_path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    // The lease directory and temporary file are created
                    // before spawn. Absence of the atomically-renamed lease
                    // therefore proves no runtime launch was authorized.
                    cleanup_job_cgroup_by_key(directory_key).await?;
                    fs::remove_dir_all(entry.path())?;
                    continue;
                }
                Err(error) => {
                    cleanup_job_cgroup_by_key(directory_key).await?;
                    return Err(error);
                }
            };
            let lease: LeaseMetadata = match serde_json::from_slice(&lease_bytes) {
                Ok(lease) => lease,
                Err(error) => {
                    cleanup_job_cgroup_by_key(directory_key).await?;
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "invalid gVisor recovery lease; workload was killed and state was preserved: {error}"
                        ),
                    ));
                }
            };
            if let Err(error) = validate_lease(&lease, name) {
                cleanup_job_cgroup_by_key(directory_key).await?;
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "invalid gVisor recovery lease; workload was killed and state was preserved: {error}"
                    ),
                ));
            }
            if lease.runtime_sha256 != self.runtime_sha256 {
                // Runtime state may not be forwards compatible, but the host
                // cgroup kill is version-independent. Stop tenant code before
                // refusing startup and asking the operator for the matching
                // reviewed runsc to remove its state.
                cleanup_job_cgroup_by_key(&lease.job_key).await?;
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "stale gVisor state belongs to a different reviewed runsc; workload was killed but the matching runtime is required to delete its state",
                ));
            }
            let state_root = runtime_dir.join("state");
            // Kill the host cgroup before consulting runtime metadata. The
            // runsc launcher joins this cgroup in pre_exec, so this covers a
            // server crash immediately after fork as well as a fully-created
            // sandbox. Persistent OCI state is then deleted only after the
            // reviewed runtime's machine-readable list confirms its identity.
            cleanup_job_cgroup_by_key(&lease.job_key).await?;
            self.ensure_runtime_deleted(&state_root, &runtime_dir, &lease.container_id, true)
                .await?;
            fs::remove_dir_all(entry.path())?;
        }
        Ok(())
    }
}

impl ExecutionProvider for GvisorProvider {
    fn mode(&self) -> SandboxMode {
        SandboxMode::Gvisor
    }

    fn not_ready_provenance(&self) -> ExecutionProvenance {
        self.configured_provenance(false, None)
    }

    fn execute<'a>(
        &'a self,
        ctx: ExecContext,
        sink: Arc<dyn Sink>,
    ) -> ProviderFuture<'a, ExecutionReport> {
        Box::pin(self.run_observed(ctx, sink))
    }

    fn preflight<'a>(&'a self, input: ProviderPreflight) -> ProviderFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let input_rootfs = input
                .rootfs
                .as_deref()
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "gVisor preflight requires rootfs",
                    )
                })?
                .canonicalize()?;
            if input_rootfs != self.rootfs {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "gVisor preflight rootfs differs from provider configuration",
                ));
            }
            if input.interpreter_overrides.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "gVisor preflight requires at least one interpreter",
                ));
            }
            for (index, (language, executable)) in input.interpreter_overrides.iter().enumerate() {
                let code = match language.as_str() {
                    "python" => "print('COOP_PREFLIGHT_OK')",
                    "node" => "console.log('COOP_PREFLIGHT_OK')",
                    "bash" => "printf '%s\\n' COOP_PREFLIGHT_OK",
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "unsupported gVisor preflight language",
                        ))
                    }
                };
                let nonce = random_nonce()?;
                let job_key = format!("preflight-{}-{index}-{}", std::process::id(), &nonce[..16]);
                let workdir = input.jobs_root.join(format!("job-{job_key}"));
                create_private_dir(&workdir)?;
                let sink = Arc::new(PreflightSink::default());
                let context = ExecContext {
                    job_key,
                    language: language.clone(),
                    code: code.to_string(),
                    stdin: None,
                    limits: Limits::default(),
                    workdir: workdir.clone(),
                    interpreter_override: executable.clone(),
                    rootfs: Some(self.rootfs.clone()),
                    helper_path: None,
                    cancel: None,
                    start_gate: None,
                    seccomp: false,
                };
                let report = self.run_observed(context, sink.clone()).await;
                if self.has_recovery_state(&workdir) {
                    return Err(report.outcome.err().unwrap_or_else(|| {
                        io::Error::other(
                            "gVisor preflight retained unexpected provider recovery state",
                        )
                    }));
                }
                fs::remove_dir_all(&workdir).map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!("remove gVisor preflight workdir: {error}"),
                    )
                })?;
                let outcome = report.outcome?;
                if outcome.status != OutcomeStatus::Succeeded
                    || !report.provenance.bootstrap_ready
                    || !sink.saw_sentinel.load(std::sync::atomic::Ordering::Acquire)
                {
                    return Err(io::Error::other(format!(
                        "{language} gVisor execution preflight did not prove the complete runtime boundary"
                    )));
                }
            }
            Ok(())
        })
    }

    fn reconcile<'a>(&'a self, jobs_root: &'a Path) -> ProviderFuture<'a, io::Result<()>> {
        Box::pin(self.reconcile_inner(jobs_root))
    }

    fn has_recovery_state(&self, workdir: &Path) -> bool {
        workdir.join(RUNTIME_DIR).exists()
    }
}

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

#[derive(Debug)]
struct RunFailure {
    error: io::Error,
    ready: bool,
    config_sha256: Option<String>,
}

impl RunFailure {
    fn before_ready(error: io::Error) -> Self {
        Self::new(error, false)
    }

    fn new(error: io::Error, ready: bool) -> Self {
        Self {
            error,
            ready,
            config_sha256: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseMetadata {
    version: u32,
    container_id: String,
    job_key: String,
    runtime_sha256: String,
    config_sha256: String,
}

#[derive(Debug, Deserialize)]
struct ListedContainer {
    id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootfsManifestEntry {
    kind: String,
    gid: u32,
    mode: u32,
    path: String,
    uid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rdev: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<String>,
}

fn validate_lease(lease: &LeaseMetadata, directory: &str) -> io::Result<()> {
    let expected_key = directory.strip_prefix("job-").unwrap_or("");
    if lease.version != LEASE_VERSION
        || lease.job_key != expected_key
        || !valid_identifier(&lease.container_id, 120)
        || !valid_identifier(&lease.job_key, 64)
        || !valid_sha256(&lease.runtime_sha256)
        || !valid_sha256(&lease.config_sha256)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "gVisor crash-reconciliation lease is invalid or belongs to a different runtime",
        ));
    }
    Ok(())
}

fn provider_job_directories(jobs_root: &Path) -> io::Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(jobs_root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut jobs = Vec::new();
    for entry in entries {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !name.starts_with("job-") {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "gVisor recovery encountered a redirected job path",
            ));
        }
        jobs.push(entry);
    }
    Ok(jobs)
}

#[allow(clippy::too_many_arguments)]
fn build_spec(
    rootfs: &Path,
    payload: &Path,
    input: &Path,
    output: &Path,
    cgroup_path: &str,
    limits: &Limits,
    uid: u32,
    gid: u32,
    nonce: &str,
    interpreter: &str,
    source: &str,
    init_sha256: &str,
    rootfs_sha256: &str,
    platform: &str,
    runtime_sha256: &str,
) -> Value {
    let memory = u64::from(limits.mem_mb) * 1024 * 1024;
    let file = u64::from(limits.max_file_mb) * 1024 * 1024;
    let tmp_size = file.min(memory);
    let host_pids = u64::from(limits.max_pids) + u64::from(RUNTIME_PID_OVERHEAD);
    json!({
        "ociVersion": "1.2.1",
        "process": {
            "terminal": false,
            "user": { "uid": uid, "gid": gid, "additionalGids": [] },
            "args": [
                OCI_INIT_PATH,
                "--internal-v1",
                nonce,
                uid.to_string(),
                gid.to_string(),
                interpreter,
                source,
            ],
            "env": [
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "HOME=/tmp/home",
                "TMPDIR=/tmp",
                "LANG=C.UTF-8",
            ],
            "cwd": "/",
            "capabilities": {
                "bounding": [], "effective": [], "inheritable": [], "permitted": [], "ambient": []
            },
            "rlimits": [
                { "type": "RLIMIT_CPU", "soft": u64::from(limits.cpu_seconds) + 1, "hard": u64::from(limits.cpu_seconds) + 2 },
                { "type": "RLIMIT_NOFILE", "soft": 256, "hard": 256 },
                { "type": "RLIMIT_FSIZE", "soft": file, "hard": file },
                { "type": "RLIMIT_CORE", "soft": 0, "hard": 0 },
                { "type": "RLIMIT_NPROC", "soft": u64::from(limits.max_pids) + 1, "hard": u64::from(limits.max_pids) + 1 }
            ],
            "noNewPrivileges": true,
        },
        "root": { "path": rootfs, "readonly": true },
        "hostname": "coop-job",
        "mounts": [
            { "destination": "/proc", "type": "proc", "source": "proc", "options": ["nosuid", "noexec", "nodev"] },
            { "destination": "/dev", "type": "tmpfs", "source": "tmpfs", "options": ["nosuid", "noexec", "strictatime", "mode=755", "size=65536"] },
            { "destination": "/sys", "type": "sysfs", "source": "sysfs", "options": ["nosuid", "noexec", "nodev", "ro"] },
            { "destination": "/tmp", "type": "tmpfs", "source": "tmpfs", "options": ["nosuid", "noexec", "nodev", "mode=1777", format!("size={tmp_size}")] },
            { "destination": "/var/tmp", "type": "tmpfs", "source": "tmpfs", "options": ["nosuid", "noexec", "nodev", "mode=1777", format!("size={tmp_size}")] },
            { "destination": "/work", "type": "bind", "source": payload, "options": ["rbind", "ro", "nosuid", "nodev", "noexec"] },
            { "destination": "/input", "type": "bind", "source": input, "options": ["rbind", "ro", "nosuid", "nodev", "noexec"] },
            { "destination": "/output", "type": "bind", "source": output, "options": ["rbind", "rw", "nosuid", "nodev", "noexec"] }
        ],
        "linux": {
            "namespaces": [
                { "type": "pid" }, { "type": "network" }, { "type": "ipc" },
                { "type": "uts" }, { "type": "mount" }
            ],
            "cgroupsPath": cgroup_path,
            "resources": {
                "memory": { "limit": memory, "swap": 0, "disableOOMKiller": false },
                "cpu": { "quota": 100000, "period": 100000 },
                "pids": { "limit": host_pids },
                "unified": {
                    "memory.max": memory.to_string(),
                    "memory.swap.max": "0",
                    "memory.oom.group": "1",
                    "cpu.max": "100000 100000",
                    "pids.max": host_pids.to_string()
                }
            },
            "maskedPaths": [
                "/proc/acpi", "/proc/asound", "/proc/kcore", "/proc/keys",
                "/proc/latency_stats", "/proc/timer_list", "/proc/timer_stats",
                "/proc/sched_debug", "/proc/scsi", "/sys/firmware"
            ],
            "readonlyPaths": [
                "/proc/bus", "/proc/fs", "/proc/irq", "/proc/sys", "/proc/sysrq-trigger"
            ]
        },
        "annotations": {
            "dev.coop.isolation-class": "gvisor-application-kernel",
            "dev.coop.network": "none",
            "dev.coop.oci-init-sha256": init_sha256,
            "dev.coop.rootfs-sha256": rootfs_sha256,
            "dev.coop.gvisor-platform": platform,
            "dev.coop.runsc-sha256": runtime_sha256
        }
    })
}

fn prepare_runsc_child(command: &mut Command, inherited_fd: i32, cgroup_procs_fd: i32) {
    // SAFETY: the closure uses only async-signal-safe descriptor syscalls and
    // captures plain integers. Writing "0" to cgroup.procs attaches the
    // calling child. All other inherited descriptors are CLOEXEC.
    unsafe {
        command.pre_exec(move || {
            let current = b"0";
            if libc::write(cgroup_procs_fd, current.as_ptr().cast(), current.len())
                != current.len() as isize
            {
                return Err(io::Error::last_os_error());
            }
            if libc::dup2(inherited_fd, 3) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

fn pipe_cloexec() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0_i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe2 returned two fresh owned descriptors.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn prepare_payload(path: &Path, ctx: &ExecContext) -> io::Result<()> {
    create_private_dir(path)?;
    write_fixed_file(
        &path.join(format!("job.{}", ext_for(&ctx.language))),
        ctx.code.as_bytes(),
        0o444,
    )?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o555))
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn write_fixed_file(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode & !0o111)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    // The next caller may execute the file immediately. Close the writable
    // descriptor before publishing the final mode so Linux never observes an
    // executable that is still open for writing (ETXTBSY on overlay filesystems).
    drop(file);
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

fn write_lease_atomic(runtime_dir: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = runtime_dir.join("lease.json.tmp");
    let destination = runtime_dir.join(LEASE_FILE);
    write_fixed_file(&temporary, bytes, 0o600)?;
    fs::rename(&temporary, &destination)?;
    fs::File::open(runtime_dir)?.sync_all()
}

fn push_pre_ready(buffer: &mut Vec<u8>, bytes: &[u8]) -> io::Result<()> {
    if buffer.len().saturating_add(bytes.len()) > PRE_READY_OUTPUT_LIMIT {
        return Err(io::Error::other(
            "gVisor emitted excessive output before authenticated readiness",
        ));
    }
    buffer.extend_from_slice(bytes);
    Ok(())
}

async fn cleanup_unstarted(runtime_dir: &Path, cgroup: CgroupLease) -> io::Result<()> {
    cleanup_job_cgroup(cgroup).await?;
    match fs::remove_dir_all(runtime_dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn first_runtime_line(bytes: &[u8]) -> Option<&str> {
    std::str::from_utf8(bytes)
        .ok()?
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
}

fn cgroup_oci_path(path: &Path) -> io::Result<String> {
    let relative = path.strip_prefix("/sys/fs/cgroup").map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "job cgroup escaped the unified cgroup mount",
        )
    })?;
    Ok(format!(
        "/{}",
        relative.to_string_lossy().replace('\\', "/")
    ))
}

fn container_id(job_key: &str, nonce: &str) -> io::Result<String> {
    if !valid_identifier(job_key, 64) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid gVisor job key",
        ));
    }
    Ok(format!("coop-{job_key}-{}", &nonce[..16]))
}

fn valid_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn random_nonce() -> io::Result<String> {
    let mut bytes = [0_u8; 24];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(hex(&bytes))
}

fn validate_rootfs_root(path: &Path) -> io::Result<PathBuf> {
    ensure_absolute_trusted_path(path, "ROOKHOLD_ROOTFS")?;
    let canonical = fs::canonicalize(path)?;
    if canonical == Path::new("/") || !canonical.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "gVisor requires a dedicated private rootfs",
        ));
    }
    for required in [
        "tmp", "var/tmp", "proc", "dev", "sys", "work", "input", "output",
    ] {
        let target = canonical.join(required);
        let metadata = fs::symlink_metadata(&target)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("gVisor rootfs /{required} must be a real directory"),
            ));
        }
    }
    Ok(canonical)
}

fn validate_host_settings() -> io::Result<()> {
    let value = fs::read_to_string("/proc/sys/vm/max_map_count")?;
    let value = value.trim().parse::<u64>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid vm.max_map_count: {error}"),
        )
    })?;
    if value < MIN_HOST_MAX_MAP_COUNT {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "vm.max_map_count is {value}; reviewed gVisor production requires at least {MIN_HOST_MAX_MAP_COUNT} before the protected Rookhold service starts"
            ),
        ));
    }
    Ok(())
}

fn validate_trusted_executable(path: &Path, label: &str) -> io::Result<PathBuf> {
    ensure_absolute_trusted_path(path, label)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{label} must be a root-owned, non-writable executable"),
        ));
    }
    fs::canonicalize(path)
}

fn validate_rootfs_manifest(
    rootfs: &Path,
    path: &Path,
    expected_digest: &str,
    init_sha256: &str,
) -> io::Result<()> {
    ensure_absolute_trusted_path(path, "gVisor rootfs manifest")?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.len() > 64 * 1024 * 1024
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "gVisor rootfs manifest must be a bounded root-owned, non-writable regular file",
        ));
    }
    let observed = hash_file(path)?;
    if observed != expected_digest {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "gVisor rootfs manifest digest mismatch: configured {expected_digest}, observed {observed}"
            ),
        ));
    }
    let manifest = fs::read_to_string(path)?;
    let expected = manifest
        .lines()
        .map(|line| {
            serde_json::from_str::<RootfsManifestEntry>(line)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        })
        .collect::<io::Result<Vec<_>>>()?;
    let mut actual = Vec::with_capacity(expected.len());
    let root_device = fs::symlink_metadata(rootfs)?.dev();
    observe_rootfs(rootfs, rootfs, root_device, &mut actual)?;
    if expected != actual {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "gVisor rootfs content, metadata, symlinks, or manifest coverage changed after the trusted manifest was built",
        ));
    }
    let mut init_entries = actual
        .iter()
        .filter(|entry| entry.path == "usr/local/bin/rookhold-oci-init");
    if init_entries
        .next()
        .and_then(|entry| entry.sha256.as_deref())
        != Some(init_sha256)
        || init_entries.next().is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "gVisor rootfs manifest does not uniquely bind the trusted OCI init",
        ));
    }
    Ok(())
}

fn observe_rootfs(
    root: &Path,
    path: &Path,
    root_device: u64,
    output: &mut Vec<RootfsManifestEntry>,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if path != root && metadata.dev() != root_device {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "trusted rootfs crossed a mounted filesystem",
        ));
    }
    let relative = if path == root {
        ".".to_string()
    } else {
        path.strip_prefix(root)
            .map_err(io::Error::other)?
            .to_str()
            .map(str::to_string)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 rootfs path"))?
    };
    if relative.contains(['\0', '\n', '\r']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "rootfs path contains a forbidden control character",
        ));
    }
    let mut entry = RootfsManifestEntry {
        kind: String::new(),
        gid: metadata.gid(),
        mode: metadata.mode() & 0o7777,
        path: relative,
        uid: metadata.uid(),
        rdev: None,
        sha256: None,
        size: None,
        target: None,
    };
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        entry.kind = "directory".to_string();
    } else if file_type.is_file() {
        entry.kind = "file".to_string();
        entry.sha256 = Some(hash_file(path)?);
        entry.size = Some(metadata.len());
    } else if file_type.is_symlink() {
        entry.kind = "symlink".to_string();
        let target = fs::read_link(path)?;
        let target = target
            .to_str()
            .map(str::to_string)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 symlink"))?;
        if target.contains(['\0', '\n', '\r']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "rootfs symlink contains a forbidden control character",
            ));
        }
        entry.target = Some(target);
    } else if file_type.is_char_device() {
        entry.kind = "character".to_string();
        entry.rdev = Some(metadata.rdev());
    } else if file_type.is_block_device() {
        entry.kind = "block".to_string();
        entry.rdev = Some(metadata.rdev());
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trusted rootfs contains a host socket or FIFO",
        ));
    }
    output.push(entry);

    if file_type.is_dir() {
        let mut children = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by(|left, right| {
            left.file_name()
                .as_bytes()
                .cmp(right.file_name().as_bytes())
        });
        for child in children {
            if path == root
                && matches!(
                    child.file_name().to_str(),
                    Some(".coop-rootfs.manifest" | ".coop-rootfs.manifest.tmp")
                )
            {
                continue;
            }
            observe_rootfs(root, &child.path(), root_device, output)?;
        }
    }
    Ok(())
}

fn ensure_absolute_trusted_path(path: &Path, label: &str) -> io::Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} must be an absolute normalized path"),
        ));
    }
    let mut current = PathBuf::from("/");
    for component in path.components() {
        if let Component::Normal(part) = component {
            current.push(part);
            if current.exists() {
                let metadata = fs::symlink_metadata(&current)?;
                if metadata.file_type().is_symlink()
                    || metadata.uid() != 0
                    || metadata.mode() & 0o022 != 0
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!("{label} traverses an untrusted component"),
                    ));
                }
            }
        }
    }
    Ok(())
}

async fn read_runsc_version(runsc: &Path) -> io::Result<String> {
    let mut command = Command::new(runsc);
    command
        .env_clear()
        .arg("--version")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(CONTROL_TIMEOUT, command.output())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "runsc --version timed out"))??;
    if !output.status.success() || output.stdout.len() > 4096 || output.stderr.len() > 4096 {
        return Err(io::Error::other("reviewed runsc version probe failed"));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_string())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn hash_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex(hasher.finalize().as_slice()))
}

fn control_error(operation: &str, output: &std::process::Output) -> io::Error {
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.lines().next().unwrap_or("runsc command failed");
    io::Error::other(format!("runsc {operation} failed: {detail}"))
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(TABLE[(byte >> 4) as usize] as char);
        output.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fake_provider() -> (GvisorProvider, PathBuf, PathBuf) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "coop-fake-runsc-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        let runtime_dir = base.join("runtime");
        let state_root = runtime_dir.join("state");
        fs::create_dir(&runtime_dir).unwrap();
        fs::create_dir(&state_root).unwrap();
        let log = base.join("calls.log");
        let present = base.join("present");
        let script = base.join("runsc");
        let source = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  *'list --format=json'*)\n    if test -f '{}'; then printf '%s\\n' '[{{\"id\":\"coop-test-0123456789abcdef\"}}]'; else printf '%s\\n' 'null'; fi\n    ;;\n  *'delete --force coop-test-0123456789abcdef'*) rm -f '{}' ;;\nesac\nexit 0\n",
            log.display(),
            present.display(),
            present.display(),
        );
        write_fixed_file(&script, source.as_bytes(), 0o700).unwrap();
        (
            GvisorProvider {
                runsc: script,
                rootfs: base.join("rootfs"),
                platform: GvisorPlatform::Systrap,
                uid: 65_534,
                gid: 65_534,
                runtime_version: REVIEWED_RUNSC_VERSION.to_string(),
                runtime_sha256: REVIEWED_RUNSC_SHA256_X86_64.to_string(),
                rootfs_sha256: "a".repeat(64),
                init_sha256: "b".repeat(64),
            },
            runtime_dir,
            log,
        )
    }

    #[test]
    fn reviewed_runtime_is_immutably_pinned() {
        assert_eq!(REVIEWED_RUNSC_VERSION, "runsc version release-20260817.0");
        assert_eq!(REVIEWED_RUNSC_SHA256_X86_64.len(), 64);
        assert!(valid_sha256(REVIEWED_RUNSC_SHA256_X86_64));
    }

    #[test]
    fn oci_spec_is_fixed_deny_by_default_and_resource_bound() {
        let root = Path::new("/opt/coop/rootfs");
        let payload = Path::new("/var/lib/coop/jobs/job-a/.coop-gvisor/payload");
        let input = Path::new("/var/lib/coop/jobs/job-a/input");
        let output = Path::new("/var/lib/coop/jobs/job-a/output");
        let spec = build_spec(
            root,
            payload,
            input,
            output,
            "/system.slice/coop.service/coop-jobs/job-a",
            &Limits::default(),
            65_534,
            65_534,
            "0123456789abcdef0123456789abcdef0123456789abcdef",
            "/usr/bin/python3",
            "/work/job.py",
            &"b".repeat(64),
            &"c".repeat(64),
            "systrap",
            REVIEWED_RUNSC_SHA256_X86_64,
        );
        assert_eq!(spec["root"]["readonly"], true);
        assert_eq!(spec["process"]["noNewPrivileges"], true);
        assert_eq!(spec["process"]["capabilities"]["effective"], json!([]));
        assert_eq!(spec["linux"]["namespaces"][1]["type"], "network");
        assert_eq!(spec["linux"]["resources"]["memory"]["swap"], 0);
        let work = spec["mounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|mount| mount["destination"] == "/work")
            .unwrap();
        assert!(work["options"].as_array().unwrap().contains(&json!("ro")));
    }

    #[test]
    fn lease_validation_binds_job_runtime_and_config() {
        let lease = LeaseMetadata {
            version: LEASE_VERSION,
            container_id: "coop-abc-0123456789abcdef".to_string(),
            job_key: "abc".to_string(),
            runtime_sha256: REVIEWED_RUNSC_SHA256_X86_64.to_string(),
            config_sha256: "a".repeat(64),
        };
        validate_lease(&lease, "job-abc").unwrap();
        assert!(validate_lease(&lease, "job-other").is_err());
    }

    fn direct_context(workdir: PathBuf) -> ExecContext {
        ExecContext {
            job_key: "direct-input-bound".to_string(),
            language: "python".to_string(),
            code: "print('ok')".to_string(),
            stdin: None,
            limits: Limits::default(),
            workdir,
            rootfs: None,
            helper_path: None,
            interpreter_override: None,
            cancel: None,
            start_gate: None,
            seccomp: false,
        }
    }

    #[tokio::test]
    async fn direct_provider_rejects_oversized_code_before_staging() {
        let (provider, runtime_dir, _) = fake_provider();
        let base = runtime_dir.parent().unwrap();
        let workdir = base.join("oversized-code-workdir");
        let mut ctx = direct_context(workdir.clone());
        ctx.code = "x".repeat(MAX_CODE_BYTES + 1);

        let report = provider
            .execute(ctx, Arc::new(PreflightSink::default()))
            .await;
        let error = report.outcome.expect_err("oversized code must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("code exceeds"));
        assert!(!workdir.exists(), "provider staged oversized code");
        let _ = fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn direct_provider_rejects_oversized_stdin_before_cloning() {
        let (provider, runtime_dir, _) = fake_provider();
        let base = runtime_dir.parent().unwrap();
        let workdir = base.join("oversized-stdin-workdir");
        let mut ctx = direct_context(workdir.clone());
        ctx.stdin = Some("x".repeat(MAX_STDIN_BYTES + 1));

        let report = provider
            .execute(ctx, Arc::new(PreflightSink::default()))
            .await;
        let error = report
            .outcome
            .expect_err("oversized stdin must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("stdin exceeds"));
        assert!(!workdir.exists(), "provider staged oversized stdin");
        let _ = fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn fake_runtime_observes_fixed_deny_flags_and_control_lifecycle() {
        let (provider, runtime_dir, log) = fake_provider();
        let state_root = runtime_dir.join("state");
        provider
            .kill_runtime(&state_root, &runtime_dir, "coop-test-0123456789abcdef")
            .await
            .unwrap();
        let deleted = provider
            .control_output(
                &state_root,
                &runtime_dir,
                &["delete", "--force", "coop-test-0123456789abcdef"],
            )
            .await
            .unwrap();
        assert!(deleted.status.success());
        let calls = fs::read_to_string(&log).unwrap();
        for required in [
            "--network=none",
            "--host-uds=none",
            "--host-fifo=none",
            "--directfs=false",
            "--allow-flag-override=false",
            "kill --all coop-test-0123456789abcdef SIGKILL",
            "delete --force coop-test-0123456789abcdef",
        ] {
            assert!(
                calls.contains(required),
                "missing {required:?} in {calls:?}"
            );
        }
        let _ = fs::remove_dir_all(runtime_dir.parent().unwrap());
    }

    #[tokio::test]
    async fn pre_spawn_crash_uses_machine_readable_empty_runtime_list() {
        let (provider, runtime_dir, log) = fake_provider();
        let state_root = runtime_dir.join("state");
        provider
            .ensure_runtime_deleted(
                &state_root,
                &runtime_dir,
                "coop-test-0123456789abcdef",
                true,
            )
            .await
            .unwrap();
        let calls = fs::read_to_string(&log).unwrap();
        assert!(calls.contains("list --format=json"));
        assert!(!calls.contains("delete --force"));
        let _ = fs::remove_dir_all(runtime_dir.parent().unwrap());
    }

    #[tokio::test]
    async fn fake_runtime_delete_is_verified_by_a_second_list() {
        let (provider, runtime_dir, log) = fake_provider();
        let base = runtime_dir.parent().unwrap();
        write_fixed_file(&base.join("present"), b"present", 0o600).unwrap();
        let state_root = runtime_dir.join("state");
        provider
            .ensure_runtime_deleted(
                &state_root,
                &runtime_dir,
                "coop-test-0123456789abcdef",
                true,
            )
            .await
            .unwrap();
        let calls = fs::read_to_string(&log).unwrap();
        assert_eq!(calls.matches("list --format=json").count(), 2);
        assert!(calls.contains("kill --all coop-test-0123456789abcdef SIGKILL"));
        assert!(calls.contains("delete --force coop-test-0123456789abcdef"));
        let _ = fs::remove_dir_all(base);
    }
}
