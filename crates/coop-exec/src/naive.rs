use crate::bounded_output::BoundedOutput;
use crate::{
    ext_for, resolve_interpreter, ExecContext, ExecOutcome, ExecTelemetry, ExecutionObserver, Sink,
    Stream,
};
use coop_types::OutcomeStatus;
use std::io;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
#[cfg(windows)]
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    OpenThread, ResumeThread, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
};

const CONTROL_TICK: Duration = Duration::from_millis(20);
const DRAIN_GRACE: Duration = Duration::from_secs(2);
const INTERPRETER_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(20);
const INTERPRETER_PREFLIGHT_CONCURRENCY: usize = 4;
const INTERPRETER_PREFLIGHT_SENTINEL: &str = "COOP_NAIVE_PREFLIGHT_OK";
const INTERPRETER_PREFLIGHT_OUTPUT_LIMIT: usize = 64 * 1024;

// Embedded callers and the test harness can construct several app instances
// concurrently. Bound host-interpreter launches across the process so valid
// canaries are not falsely timed out by a startup fork storm. Each caller
// still executes a fresh, configuration-specific canary after acquiring its
// permit; no availability result is cached.
static INTERPRETER_PREFLIGHT_PERMITS: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(INTERPRETER_PREFLIGHT_CONCURRENCY);

/// Development-only subprocess executor.
///
/// This backend deliberately makes no hostile-code isolation promise. It does
/// still provide bounded I/O, deterministic deadlines, cancellation, and
/// best-effort process-group cleanup so local development cannot trivially
/// wedge the server.
pub async fn run(ctx: ExecContext, sink: Arc<dyn Sink>) -> io::Result<ExecOutcome> {
    run_observed(ctx, sink, ExecutionObserver::default()).await
}

pub(crate) async fn run_observed(
    ctx: ExecContext,
    sink: Arc<dyn Sink>,
    observer: ExecutionObserver,
) -> io::Result<ExecOutcome> {
    let src = ctx.workdir.join(format!("job.{}", ext_for(&ctx.language)));
    crate::write_private_file(&src, ctx.code.as_bytes())?;

    let interp = resolve_interpreter(&ctx.language, ctx.interpreter_override.as_deref())?;
    let mut cmd = Command::new(interp);
    cmd.current_dir(&ctx.workdir).arg(&src);
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_SUSPENDED);
    configure_child_environment(&mut cmd);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if ctx.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.kill_on_drop(true);

    let launch_permit = match ctx.begin_process_launch() {
        Ok(permit) => permit,
        Err(reason) => return Ok(ExecOutcome::cancelled_before_launch(reason)),
    };
    let mut child = cmd.spawn()?;
    let child_pid = child.id().expect("freshly spawned child has a pid");
    // Arm Unix cleanup before releasing the shared launch boundary. If
    // shutdown waits behind this spawn, it must never observe an unowned
    // process after the gate closes.
    #[cfg(unix)]
    let process_group = ProbeProcessGroup(child_pid);
    #[cfg(windows)]
    let windows_job = WindowsJob::attach_and_resume(&child);
    // Windows creation is suspended, so Job Object attachment and resume are
    // part of the effective launch boundary. No lock is held after this drop.
    drop(launch_permit);
    #[cfg(unix)]
    let _process_group = process_group;
    #[cfg(windows)]
    let mut windows_job = match windows_job {
        Ok(job) => job,
        Err(error) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(error);
        }
    };
    // On Windows this follows successful Job Object attachment and resume;
    // on Unix it follows successful process creation. Only now is the
    // development backend's wall/cancel supervisor an active workload fact.
    observer.mark_ready();
    let stdin_task = if let Some(mut stdin) = child.stdin.take() {
        let input = ctx.stdin.clone().unwrap_or_default();
        Some(tokio::spawn(async move {
            let _ = stdin.write_all(input.as_bytes()).await;
            let _ = stdin.shutdown().await;
        }))
    } else {
        None
    };

    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");
    let mut stdout_buf = [0_u8; 8192];
    let mut stderr_buf = [0_u8; 8192];
    let mut stdout_capture = BoundedOutput::new(Stream::Stdout);
    let mut stderr_capture = BoundedOutput::new(Stream::Stderr);

    let wall = Duration::from_secs(ctx.limits.wall_seconds.max(1) as u64);
    let started = Instant::now();
    let deadline = started + wall;
    let mut tick = tokio::time::interval(CONTROL_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut status: Option<std::process::ExitStatus> = None;
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut drain_deadline: Option<Instant> = None;
    let mut timed_out = false;
    let mut cancelled = false;

    while status.is_none() || !stdout_done || !stderr_done {
        let drain = async {
            match drain_deadline {
                Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
                None => std::future::pending::<()>().await,
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
                    if ctx.is_cancelled() && !cancelled && !timed_out {
                        cancelled = true;
                        sink.violation("job_cancelled", serde_json::json!({}));
                        #[cfg(windows)]
                        windows_job.terminate()?;
                        kill_process_group(child_pid);
                        let _ = child.start_kill();
                    } else if Instant::now() >= deadline && !cancelled && !timed_out {
                        timed_out = true;
                        sink.violation(
                            "wall_clock_exceeded",
                            serde_json::json!({"wall_seconds": ctx.limits.wall_seconds}),
                        );
                        #[cfg(windows)]
                        windows_job.terminate()?;
                        kill_process_group(child_pid);
                        let _ = child.start_kill();
                    }

                    if let Some(exit) = child.try_wait()? {
                        status = Some(exit);
                        #[cfg(windows)]
                        windows_job.terminate()?;
                        kill_process_group(child_pid);
                        drain_deadline = Some(Instant::now() + DRAIN_GRACE);
                    }
                }
            }

            _ = drain => {
                tracing::warn!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "development executor output drain reached its grace limit"
                );
                stdout_done = true;
                stderr_done = true;
            }

            read = read_stdout => match read {
                Ok(0) => {
                    stdout_capture.finish(sink.as_ref());
                    stdout_done = true;
                }
                Ok(n) => stdout_capture.push(&stdout_buf[..n], sink.as_ref()),
                Err(e) => {
                    tracing::debug!(error = %e, "stdout reader closed with an error");
                    stdout_capture.finish(sink.as_ref());
                    stdout_done = true;
                }
            },

            read = read_stderr => match read {
                Ok(0) => {
                    stderr_capture.finish(sink.as_ref());
                    stderr_done = true;
                }
                Ok(n) => stderr_capture.push(&stderr_buf[..n], sink.as_ref()),
                Err(e) => {
                    tracing::debug!(error = %e, "stderr reader closed with an error");
                    stderr_capture.finish(sink.as_ref());
                    stderr_done = true;
                }
            },
        }
    }

    if let Some(task) = stdin_task {
        task.abort();
    }
    stdout_capture.finish(sink.as_ref());
    stderr_capture.finish(sink.as_ref());

    let status = match status {
        Some(status) => status,
        None => child.wait().await?,
    };
    let telemetry = ExecTelemetry {
        wall_time_ms: started.elapsed().as_millis() as u64,
        cpu_time_usec: None,
        memory_peak_bytes: None,
        stdout: stdout_capture.telemetry(),
        stderr: stderr_capture.telemetry(),
    };

    if timed_out {
        return Ok(ExecOutcome {
            status: OutcomeStatus::TimedOut,
            exit_code: status.code(),
            killed_by: Some("wall-clock".into()),
            telemetry,
        });
    }
    if cancelled {
        return Ok(ExecOutcome {
            status: OutcomeStatus::Cancelled,
            exit_code: status.code(),
            killed_by: Some("cancelled".into()),
            telemetry,
        });
    }
    Ok(classify(status.code(), unix_signal(&status), telemetry))
}

pub(crate) async fn preflight_interpreter(
    language: &str,
    override_bin: Option<&str>,
) -> io::Result<String> {
    let _preflight_permit = INTERPRETER_PREFLIGHT_PERMITS
        .acquire()
        .await
        .map_err(|_| io::Error::other("interpreter preflight concurrency gate was closed"))?;
    let unresolved = resolve_interpreter(language, override_bin).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("{language} development interpreter resolution failed: {error}"),
        )
    })?;
    let executable = resolve_exact_executable(&unresolved).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("{language} development interpreter {unresolved:?} is unavailable: {error}"),
        )
    })?;
    let code = match language {
        "python" => "print('COOP_NAIVE_PREFLIGHT_OK')",
        "node" => "console.log('COOP_NAIVE_PREFLIGHT_OK')",
        // `cat` is intentionally external. A candidate is accepted only when
        // its Unix tool path works under Coop's sanitized environment; an
        // echo-only canary would miss a partially usable Bash installation.
        "bash" => concat!(
            "probe_value=\"$(printf '%s' COOP_NATIVE_BASH | cat)\" || exit 70\n",
            "test \"$probe_value\" = COOP_NATIVE_BASH || exit 71\n",
            "printf '%s\\n' COOP_NAIVE_PREFLIGHT_OK\n",
        ),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported interpreter language {language:?}"),
            ))
        }
    };

    static PREFLIGHT_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = PREFLIGHT_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "coop-interpreter-preflight-{}-{sequence}",
        std::process::id()
    ));
    std::fs::create_dir(&directory).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "create {language} interpreter preflight directory {}: {error}",
                directory.display()
            ),
        )
    })?;
    crate::owner_only_dir(&directory)?;
    let directory_guard = PreflightDirectory::new(directory.clone());
    let source = directory.join(format!("canary.{}", ext_for(language)));
    crate::write_private_file(&source, code.as_bytes())?;

    let mut command = Command::new(&executable);
    command
        .current_dir(&directory)
        .arg(&source)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(CREATE_SUSPENDED);
    configure_child_environment(&mut command);

    let mut child = command.spawn().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "start {language} development interpreter {}: {error}",
                executable.display()
            ),
        )
    })?;
    #[cfg(unix)]
    let child_pid = child
        .id()
        .ok_or_else(|| io::Error::other("preflight child has no process identifier"))?;
    // Arm process-group cleanup synchronously after spawn, before any
    // fallible pipe extraction or task scheduling. A fast candidate can fork
    // immediately, and Tokio may cancel this outer future before the spawned
    // waiter receives its first poll.
    #[cfg(unix)]
    let process_group = ProbeProcessGroup(child_pid);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("preflight child stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("preflight child stderr was not piped"))?;
    #[cfg(windows)]
    let windows_job = match WindowsJob::attach_and_resume(&child) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(io::Error::new(
                error.kind(),
                format!("supervise {language} interpreter preflight: {error}"),
            ));
        }
    };

    let mut waiter = spawn_preflight_waiter(
        child,
        stdout,
        stderr,
        #[cfg(unix)]
        process_group,
        #[cfg(windows)]
        windows_job,
    );
    let (status, stdout, stderr) =
        match tokio::time::timeout(INTERPRETER_PREFLIGHT_TIMEOUT, waiter.join()).await {
            Ok(joined) => joined.map_err(io::Error::other)??,
            Err(_) => {
                waiter.abort();
                let _ = waiter.join().await;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "{language} development interpreter preflight exceeded {} seconds",
                        INTERPRETER_PREFLIGHT_TIMEOUT.as_secs()
                    ),
                ));
            }
        };

    if stdout.exceeded || stderr.exceeded {
        return Err(io::Error::other(format!(
            "{language} development interpreter preflight output exceeded {INTERPRETER_PREFLIGHT_OUTPUT_LIMIT} bytes per stream"
        )));
    }
    let stdout = String::from_utf8_lossy(&stdout.bytes);
    let stderr = String::from_utf8_lossy(&stderr.bytes);
    if !status.success()
        || !stdout
            .lines()
            .any(|line| line == INTERPRETER_PREFLIGHT_SENTINEL)
    {
        return Err(io::Error::other(format!(
            "{language} development interpreter preflight failed with status {:?} or missing exact sentinel; stderr: {}",
            status.code(),
            stderr.trim()
        )));
    }
    directory_guard.cleanup().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "remove {language} interpreter preflight directory {}: {error}",
                directory.display()
            ),
        )
    })?;
    Ok(executable.to_string_lossy().into_owned())
}

fn spawn_preflight_waiter(
    mut child: Child,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    #[cfg(unix)] process_group: ProbeProcessGroup,
    #[cfg(windows)] mut windows_job: WindowsJob,
) -> AbortOnDropJoinHandle<io::Result<(std::process::ExitStatus, PreflightOutput, PreflightOutput)>>
{
    AbortOnDropJoinHandle::new(tokio::spawn(async move {
        // These guards are already armed before this task is created. Moving
        // them here transfers cleanup ownership without relying on this future
        // receiving a first poll.
        #[cfg(unix)]
        let child_pid = process_group.0;
        #[cfg(unix)]
        let _process_group = process_group;
        let wait_for_exit = async {
            let status = child.wait().await?;
            #[cfg(windows)]
            windows_job.terminate()?;
            #[cfg(unix)]
            kill_process_group(child_pid);
            Ok::<_, io::Error>(status)
        };
        let (status, stdout, stderr) = tokio::try_join!(
            wait_for_exit,
            drain_preflight_output(stdout),
            drain_preflight_output(stderr),
        )?;
        Ok((status, stdout, stderr))
    }))
}

fn resolve_exact_executable(program: &str) -> io::Result<std::path::PathBuf> {
    let requested = std::path::PathBuf::from(program);
    if requested.is_absolute() || requested.components().count() > 1 {
        return canonical_executable(&requested);
    }

    let names = executable_names(requested.as_os_str());
    for directory in std::env::split_paths(&sanitized_child_path()) {
        for name in &names {
            let candidate = directory.join(name);
            if let Ok(executable) = canonical_executable(&candidate) {
                return Ok(executable);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("{program:?} was not found on the sanitized child PATH"),
    ))
}

fn canonical_executable(path: &std::path::Path) -> io::Result<std::path::PathBuf> {
    let path = std::fs::canonicalize(path)?;
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not executable", path.display()),
            ));
        }
    }
    Ok(path)
}

fn executable_names(program: &std::ffi::OsStr) -> Vec<std::ffi::OsString> {
    let names = vec![program.to_os_string()];
    #[cfg(windows)]
    {
        if std::path::Path::new(program).extension().is_none() {
            let mut names = names;
            for extension in [".COM", ".EXE", ".BAT", ".CMD"] {
                let mut name = program.to_os_string();
                name.push(extension);
                names.push(name);
            }
            return names;
        }
    }
    names
}

fn sanitized_child_path() -> std::ffi::OsString {
    #[cfg(windows)]
    {
        windows_child_environment()
            .into_iter()
            .find_map(|(key, value)| key.eq_ignore_ascii_case("PATH").then_some(value))
            .expect("Windows child environment always contains PATH")
    }
    #[cfg(unix)]
    {
        let default_path = if cfg!(target_os = "macos") {
            "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin"
        } else {
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
        };
        platform_path(default_path)
    }
}

struct PreflightOutput {
    bytes: Vec<u8>,
    exceeded: bool,
}

async fn drain_preflight_output<R>(mut reader: R) -> io::Result<PreflightOutput>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(INTERPRETER_PREFLIGHT_OUTPUT_LIMIT);
    let mut exceeded = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = INTERPRETER_PREFLIGHT_OUTPUT_LIMIT.saturating_sub(bytes.len());
        let retain = remaining.min(read);
        bytes.extend_from_slice(&buffer[..retain]);
        exceeded |= retain < read;
    }
    Ok(PreflightOutput { bytes, exceeded })
}

struct PreflightDirectory(Option<std::path::PathBuf>);

/// Tokio detaches a spawned task when its `JoinHandle` is dropped. Interpreter
/// probing is part of startup and can itself be cancelled during shutdown, so
/// the supervising task must instead be aborted. Dropping that task releases
/// the child's kill-on-drop handle and the Unix process-group or Windows Job
/// Object guard it owns.
struct AbortOnDropJoinHandle<T> {
    handle: tokio::task::JoinHandle<T>,
}

impl<T> AbortOnDropJoinHandle<T> {
    fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self { handle }
    }

    fn abort(&self) {
        self.handle.abort();
    }

    async fn join(&mut self) -> Result<T, tokio::task::JoinError> {
        (&mut self.handle).await
    }
}

impl<T> Drop for AbortOnDropJoinHandle<T> {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl PreflightDirectory {
    fn new(path: std::path::PathBuf) -> Self {
        Self(Some(path))
    }

    fn cleanup(mut self) -> io::Result<()> {
        let path = self.0.take().expect("preflight directory is armed");
        std::fs::remove_dir_all(path)
    }
}

impl Drop for PreflightDirectory {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

#[cfg(unix)]
struct ProbeProcessGroup(u32);

#[cfg(unix)]
impl Drop for ProbeProcessGroup {
    fn drop(&mut self) {
        kill_process_group(self.0);
    }
}

#[cfg(all(test, unix))]
mod unix_preflight_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn process_alive(pid: i32) -> bool {
        (unsafe { libc::kill(pid, 0) }) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// Keep the current-thread runtime blocked until a candidate has forked,
    /// proving that dropping an entirely unpolled waiter still owns and kills
    /// both the interpreter and its fast descendant.
    #[tokio::test(flavor = "current_thread")]
    async fn preflight_guard_exists_before_waiter_first_poll() {
        let directory = std::env::temp_dir().join(format!(
            "coop-unpolled-preflight-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let marker = directory.join("pids");
        let script = directory.join("fast-fork-bash");
        std::fs::write(
            &script,
            "#!/bin/sh\nsleep 60 &\nchild=$!\nprintf '%s %s\\n' \"$$\" \"$child\" > pids\nwait\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut command = Command::new(&script);
        command
            .current_dir(&directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        let mut child = command.spawn().unwrap();
        let child_pid = child.id().unwrap();
        let process_group = ProbeProcessGroup(child_pid);
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let waiter = spawn_preflight_waiter(child, stdout, stderr, process_group);

        // Do not yield to Tokio: the nested waiter is guaranteed to remain
        // unpolled while the OS child independently reaches its fast fork.
        let marker_deadline = Instant::now() + Duration::from_secs(3);
        while !marker.is_file() && Instant::now() < marker_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let pids = std::fs::read_to_string(&marker).expect("fast-fork marker");
        let mut pids = pids.split_whitespace().map(|value| {
            value
                .parse::<i32>()
                .expect("fast-fork marker contained a pid")
        });
        let leader = pids.next().unwrap();
        let descendant = pids.next().unwrap();
        assert!(pids.next().is_none());

        drop(waiter);
        let reap_deadline = Instant::now() + Duration::from_secs(3);
        while (process_alive(leader) || process_alive(descendant)) && Instant::now() < reap_deadline
        {
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!process_alive(leader), "unpolled waiter retained leader");
        assert!(
            !process_alive(descendant),
            "unpolled waiter retained fast-fork descendant"
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }
}

fn configure_child_environment(cmd: &mut Command) {
    cmd.env_clear();

    #[cfg(windows)]
    {
        for (key, value) in windows_child_environment() {
            cmd.env(key, value);
        }
    }

    #[cfg(unix)]
    {
        let default_path = if cfg!(target_os = "macos") {
            "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin"
        } else {
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
        };
        cmd.env("PATH", platform_path(default_path));
        if !copy_parent_env(cmd, "HOME") {
            cmd.env("HOME", std::env::temp_dir());
        }
        if !copy_parent_env(cmd, "TMPDIR") {
            cmd.env("TMPDIR", std::env::temp_dir());
        }
        if !copy_parent_env(cmd, "LANG") {
            cmd.env("LANG", "C.UTF-8");
        }
        copy_parent_env(cmd, "LC_ALL");
        copy_parent_env(cmd, "LC_CTYPE");
    }
}

#[cfg(windows)]
fn parent_env_value(key: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(key).filter(|value| !value.is_empty())
}

#[cfg(windows)]
fn windows_child_environment() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    // These names are an allowlist, not a parent-environment copy. They are
    // required by Windows process lookup, CSPRNG/runtime loading,
    // home-directory discovery, and temporary-file APIs. The native-Bash
    // probe uses this same environment, so a candidate is accepted only if it
    // will work when the real job starts.
    let system_root =
        parent_env_value("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let mut values = vec![
        (std::ffi::OsString::from("SystemRoot"), system_root.clone()),
        (
            std::ffi::OsString::from("PATH"),
            platform_path(&system_root),
        ),
        (
            std::ffi::OsString::from("PATHEXT"),
            parent_env_value("PATHEXT")
                .unwrap_or_else(|| std::ffi::OsString::from(".COM;.EXE;.BAT;.CMD")),
        ),
    ];
    for key in ["HOMEDRIVE", "HOMEPATH", "HOME"] {
        if let Some(value) = parent_env_value(key) {
            values.push((std::ffi::OsString::from(key), value));
        }
    }
    let fallback = std::env::temp_dir().into_os_string();
    for key in ["USERPROFILE", "TEMP", "TMP"] {
        values.push((
            std::ffi::OsString::from(key),
            parent_env_value(key).unwrap_or_else(|| fallback.clone()),
        ));
    }
    values
}

#[cfg(unix)]
fn platform_path(default_path: &str) -> std::ffi::OsString {
    let mut entries = std::env::split_paths(std::ffi::OsStr::new(default_path)).collect::<Vec<_>>();
    if let Some(parent) = std::env::var_os("PATH") {
        for entry in std::env::split_paths(&parent) {
            if !entries.contains(&entry) {
                entries.push(entry);
            }
        }
    }
    std::env::join_paths(entries).unwrap_or_else(|_| default_path.into())
}

#[cfg(windows)]
fn platform_path(system_root: &std::ffi::OsStr) -> std::ffi::OsString {
    let root = std::path::PathBuf::from(system_root);
    let mut entries = vec![root.join("System32"), root];
    if let Some(parent) = std::env::var_os("PATH") {
        for entry in std::env::split_paths(&parent) {
            // Windows `join_paths` rejects quotes. Ignore only malformed
            // components rather than losing the fixed System32 prefix.
            if !entry.to_string_lossy().contains('"')
                && !entries.iter().any(|existing| {
                    existing
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&entry.to_string_lossy())
                })
            {
                entries.push(entry);
            }
        }
    }
    std::env::join_paths(entries).unwrap_or_else(|_| {
        std::env::join_paths([
            std::path::PathBuf::from(r"C:\Windows\System32"),
            std::path::PathBuf::from(r"C:\Windows"),
        ])
        .expect("static Windows PATH entries are valid")
    })
}

/// Resolve a native Windows Bash without launching it. The bounded startup
/// preflight is intentionally separate so job execution never performs a
/// synchronous or repeated capability probe.
#[cfg(windows)]
pub(crate) fn resolve_native_windows_bash(override_bin: Option<&str>) -> io::Result<String> {
    let mut candidates = Vec::new();
    if let Some(configured) = override_bin {
        let configured = std::path::PathBuf::from(configured);
        if configured.is_absolute() || configured.components().count() > 1 {
            push_unique_windows_path(&mut candidates, configured);
        } else {
            // A bare COOP_BASH still means "find native Bash", never "accept
            // whichever Windows application alias happens to win SearchPath".
            let configured_name = configured.to_string_lossy();
            if configured_name.eq_ignore_ascii_case("bash")
                || configured_name.eq_ignore_ascii_case("bash.exe")
            {
                push_known_git_bash_candidates(&mut candidates);
            }
            push_path_candidates(&mut candidates, &configured);
        }
    } else {
        // Prefer Git's bin/bash.exe because it normally establishes Git's
        // Unix tool mapping itself. Other native candidates remain eligible
        // when the bounded startup canary proves external tools work.
        push_known_git_bash_candidates(&mut candidates);
        push_path_candidates(&mut candidates, std::path::Path::new("bash.exe"));
    }

    let mut rejected = Vec::new();
    for candidate in candidates {
        if !candidate.is_file() {
            continue;
        }
        let resolved = match std::fs::canonicalize(&candidate) {
            Ok(path) => path,
            Err(error) => {
                rejected.push(format!("{} ({error})", candidate.display()));
                continue;
            }
        };
        if is_windows_bash_shim_location(&resolved) {
            rejected.push(format!(
                "{} (WSL/application-alias shim)",
                resolved.display()
            ));
            continue;
        }
        return Ok(resolved.to_string_lossy().into_owned());
    }

    let configured = override_bin
        .map(|value| format!(" configured by COOP_BASH={value:?}"))
        .unwrap_or_default();
    let detail = if rejected.is_empty() {
        String::new()
    } else {
        format!("; rejected: {}", rejected.join(", "))
    };
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "native Windows Bash is unavailable{configured}; install Git for Windows or set COOP_BASH to a native bash.exe{detail}"
        ),
    ))
}

#[cfg(windows)]
fn push_known_git_bash_candidates(candidates: &mut Vec<std::path::PathBuf>) {
    for variable in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = parent_env_value(variable) {
            push_unique_windows_path(
                candidates,
                std::path::PathBuf::from(root).join(r"Git\bin\bash.exe"),
            );
        }
    }
    if let Some(local) = parent_env_value("LOCALAPPDATA") {
        push_unique_windows_path(
            candidates,
            std::path::PathBuf::from(local).join(r"Programs\Git\bin\bash.exe"),
        );
    }
}

#[cfg(windows)]
fn push_path_candidates(candidates: &mut Vec<std::path::PathBuf>, executable: &std::path::Path) {
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            push_unique_windows_path(candidates, directory.join(executable));
        }
    }
}

#[cfg(windows)]
fn push_unique_windows_path(
    candidates: &mut Vec<std::path::PathBuf>,
    candidate: std::path::PathBuf,
) {
    if !candidates.iter().any(|existing| {
        existing
            .to_string_lossy()
            .eq_ignore_ascii_case(&candidate.to_string_lossy())
    }) {
        candidates.push(candidate);
    }
}

#[cfg(windows)]
fn is_windows_bash_shim_location(candidate: &std::path::Path) -> bool {
    let normalized = candidate
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    // canonicalize() commonly returns an extended-length `\\?\C:\...`
    // spelling. Compare the semantic path, not that Win32 API prefix.
    let normalized = normalized.strip_prefix(r"\\?\").unwrap_or(&normalized);
    let system_root =
        parent_env_value("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let system_root = std::path::PathBuf::from(system_root)
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase();
    normalized == system_root
        || normalized.starts_with(&format!("{system_root}\\"))
        || normalized.contains("\\microsoft\\windowsapps\\")
}

#[cfg(unix)]
fn copy_parent_env(cmd: &mut Command, key: &str) -> bool {
    std::env::var_os(key).is_some_and(|value| {
        if value.is_empty() {
            false
        } else {
            cmd.env(key, value);
            true
        }
    })
}

#[cfg(windows)]
struct WindowsJob {
    handle: OwnedHandle,
    terminated: bool,
}

#[cfg(windows)]
impl WindowsJob {
    fn attach_and_resume(child: &tokio::process::Child) -> io::Result<Self> {
        let raw_job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw_job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw_job) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                raw_job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            return Err(io::Error::last_os_error());
        }

        let process = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("spawned child has no Windows process handle"))?
            as HANDLE;
        if unsafe { AssignProcessToJobObject(raw_job, process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        resume_primary_thread(
            child.id().ok_or_else(|| {
                io::Error::other("spawned child has no Windows process identifier")
            })?,
        )?;

        Ok(Self {
            handle,
            terminated: false,
        })
    }

    fn terminate(&mut self) -> io::Result<()> {
        if self.terminated {
            return Ok(());
        }
        if unsafe { TerminateJobObject(self.raw(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        self.terminated = true;
        Ok(())
    }

    fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle() as HANDLE
    }
}

#[cfg(windows)]
fn resume_primary_thread(process_id: u32) -> io::Result<()> {
    let raw_snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw_snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw_snapshot) };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..THREADENTRY32::default()
    };
    let mut found = unsafe { Thread32First(snapshot.as_raw_handle() as HANDLE, &mut entry) } != 0;
    while found {
        if entry.th32OwnerProcessID == process_id {
            let raw_thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if raw_thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            let thread = unsafe { OwnedHandle::from_raw_handle(raw_thread) };
            if unsafe { ResumeThread(thread.as_raw_handle() as HANDLE) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        found = unsafe { Thread32Next(snapshot.as_raw_handle() as HANDLE, &mut entry) } != 0;
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "could not find the suspended child primary thread",
    ))
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // SAFETY: negative PIDs are the POSIX process-group form. This backend is
    // explicitly development-only; the production backend uses cgroup.kill.
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {}

fn classify(code: Option<i32>, signal: Option<i32>, telemetry: ExecTelemetry) -> ExecOutcome {
    match (code, signal) {
        (Some(0), _) => ExecOutcome {
            status: OutcomeStatus::Succeeded,
            exit_code: Some(0),
            killed_by: None,
            telemetry,
        },
        (Some(code), _) => ExecOutcome {
            status: OutcomeStatus::Failed,
            exit_code: Some(code),
            killed_by: None,
            telemetry,
        },
        (None, Some(signal)) => ExecOutcome {
            status: OutcomeStatus::Failed,
            exit_code: None,
            killed_by: Some(format!("signal-{signal}")),
            telemetry,
        },
        (None, None) => ExecOutcome {
            status: OutcomeStatus::Failed,
            exit_code: None,
            killed_by: None,
            telemetry,
        },
    }
}

#[cfg(unix)]
fn unix_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn unix_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}
