use coop_exec::{
    execute, execute_reported, ExecContext, ExecutionCancellation, ExecutionStartGate, SandboxMode,
    Sink, Stream,
};
use coop_types::{LimitEnforcement, Limits, OutcomeStatus};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Collect {
    stdout: Mutex<Vec<String>>,
    stderr: Mutex<Vec<String>>,
    violations: Mutex<Vec<String>>,
    truncated: Mutex<Vec<Stream>>,
}

impl Sink for Collect {
    fn output(&self, stream: Stream, line: String) {
        match stream {
            Stream::Stdout => self.stdout.lock().unwrap().push(line),
            Stream::Stderr => self.stderr.lock().unwrap().push(line),
        }
    }
    fn violation(&self, rule: &'static str, _d: Value) {
        self.violations.lock().unwrap().push(rule.to_string());
    }
    fn truncated(&self, stream: Stream) {
        self.truncated.lock().unwrap().push(stream);
    }
}

fn limits_wall(wall: u32) -> Limits {
    Limits {
        wall_seconds: wall,
        ..Limits::default()
    }
}

fn workdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("coop-reg-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn collector() -> Arc<Collect> {
    Arc::new(Collect {
        stdout: Default::default(),
        stderr: Default::default(),
        violations: Default::default(),
        truncated: Default::default(),
    })
}

#[cfg(unix)]
fn unix_process_alive(pid: i32) -> bool {
    (unsafe { libc::kill(pid, 0) }) == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
async fn assert_unix_process_reaped(pid: i32, context: &str) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while unix_process_alive(pid) {
        assert!(
            Instant::now() < deadline,
            "{context} left process {pid} alive"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reported_development_posture_exposes_only_enforced_limits() {
    let resolved = match coop_exec::preflight_naive_interpreter("python", None).await {
        Ok(resolved) => resolved,
        Err(error) => {
            eprintln!("skipping: python preflight unavailable: {error}");
            return;
        }
    };
    let dir = workdir("reported-development-posture");
    let limits = Limits::default();
    let report = execute_reported(
        ExecContext {
            job_key: "reported-development-posture".into(),
            language: "python".into(),
            code: "print('ready')".into(),
            stdin: None,
            limits: limits.clone(),
            workdir: dir.clone(),
            interpreter_override: Some(resolved),
            rootfs: None,
            helper_path: None,
            cancel: None,
            start_gate: None,
            seccomp: false,
        },
        collector(),
        SandboxMode::Off,
    )
    .await;
    report.outcome.expect("development execution");
    assert!(report.provenance.bootstrap_ready);
    assert!(!report.provenance.isolated);
    assert_eq!(
        report.provenance.limit_enforcement,
        LimitEnforcement::DEVELOPMENT_SUBPROCESS
    );
    let effective = report.provenance.effective_limits(&limits);
    assert_eq!(effective.wall_seconds, Some(limits.wall_seconds));
    assert_eq!(effective.cpu_seconds, None);
    assert_eq!(effective.mem_mb, None);
    assert_eq!(effective.max_pids, None);
    assert_eq!(effective.max_file_mb, None);
    assert_eq!(effective.allow_network, Some(true));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_development_spawn_never_claims_ready_posture() {
    let dir = workdir("failed-development-posture");
    let limits = Limits::default();
    let report = execute_reported(
        ExecContext {
            job_key: "failed-development-posture".into(),
            language: "python".into(),
            code: "print('never')".into(),
            stdin: None,
            limits: limits.clone(),
            workdir: dir.clone(),
            interpreter_override: Some(
                dir.join("definitely-missing-interpreter")
                    .to_string_lossy()
                    .into_owned(),
            ),
            rootfs: None,
            helper_path: None,
            cancel: None,
            start_gate: None,
            seccomp: false,
        },
        collector(),
        SandboxMode::Off,
    )
    .await;
    assert!(report.outcome.is_err());
    assert!(!report.provenance.bootstrap_ready);
    assert_eq!(report.provenance.limit_enforcement, LimitEnforcement::NONE);
    let effective = report.provenance.effective_limits(&limits);
    assert_eq!(effective.wall_seconds, None);
    assert_eq!(effective.allow_network, None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closed_start_boundaries_prevent_naive_process_creation() {
    let resolved = match coop_exec::preflight_naive_interpreter("python", None).await {
        Ok(resolved) => resolved,
        Err(error) => {
            eprintln!("skipping: python preflight unavailable: {error}");
            return;
        }
    };

    for (tag, expected_reason, cancel, start_gate) in [
        {
            let gate = Arc::new(ExecutionStartGate::default());
            gate.close();
            (
                "closed-global-start-gate",
                "server_shutdown_before_launch",
                None,
                Some(gate),
            )
        },
        {
            let cancel = Arc::new(ExecutionCancellation::default());
            cancel.cancel();
            (
                "closed-job-start-gate",
                "cancelled_before_launch",
                Some(cancel),
                None,
            )
        },
    ] {
        let dir = workdir(tag);
        let marker = dir.join("process-was-launched");
        let marker_literal = serde_json::to_string(marker.to_string_lossy().as_ref()).unwrap();
        let report = execute_reported(
            ExecContext {
                job_key: tag.into(),
                language: "python".into(),
                code: format!("open({marker_literal}, 'w').write('launched')"),
                stdin: None,
                limits: limits_wall(10),
                workdir: dir.clone(),
                interpreter_override: Some(resolved.clone()),
                rootfs: None,
                helper_path: None,
                cancel,
                start_gate,
                seccomp: false,
            },
            collector(),
            SandboxMode::Off,
        )
        .await;
        let outcome = report.outcome.expect("closed launch is a cancellation");
        assert_eq!(outcome.status, OutcomeStatus::Cancelled);
        assert_eq!(outcome.killed_by.as_deref(), Some(expected_reason));
        assert!(!report.provenance.bootstrap_ready);
        assert!(
            !marker.exists(),
            "a process crossed the closed {tag} boundary"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The development subprocess backend scrubs secrets while preserving the
/// small platform environment needed for home/temp discovery and child lookup.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn naive_environment_keeps_platform_runtime_prerequisites() {
    let dir = workdir("platform-env");
    let sink = collector();
    let code = r#"
import os, pathlib, shutil, subprocess, sys, tempfile
home = pathlib.Path.home()
assert home.is_absolute(), home
assert pathlib.Path(tempfile.gettempdir()).is_absolute(), tempfile.gettempdir()
python_name = 'python.exe' if os.name == 'nt' else 'python3'
assert shutil.which(python_name), (python_name, os.environ.get('PATH'))
probe = ['where.exe', python_name] if os.name == 'nt' else ['sh', '-c', 'command -v python3']
child = subprocess.run(probe, check=True, capture_output=True, text=True)
assert child.stdout.strip(), child
print('PLATFORM-ENV-OK')
"#;
    let ctx = ExecContext {
        job_key: "platform-env".into(),
        language: "python".into(),
        code: code.into(),
        stdin: None,
        limits: limits_wall(10),
        workdir: dir.clone(),
        interpreter_override: None,
        rootfs: None,
        helper_path: None,
        cancel: None,
        start_gate: None,
        seccomp: false,
    };
    let outcome = execute(ctx, sink.clone(), SandboxMode::Off).await.unwrap();
    assert_eq!(format!("{:?}", outcome.status), "Succeeded");
    assert!(sink
        .stdout
        .lock()
        .unwrap()
        .iter()
        .any(|line| line == "PLATFORM-ENV-OK"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Development-mode capability discovery must validate every advertised
/// runtime under the exact sanitized child environment and return an exact
/// executable path that admission can cache.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn naive_preflight_validates_all_supported_interpreters() {
    for language in ["python", "node", "bash"] {
        let executable = coop_exec::preflight_naive_interpreter(language, None)
            .await
            .unwrap_or_else(|error| panic!("{language} startup preflight failed: {error}"));
        let executable = std::path::Path::new(&executable);
        assert!(
            executable.is_absolute() && executable.is_file(),
            "{language} preflight did not return an exact executable: {}",
            executable.display()
        );
    }
}

/// Cancelling server startup must cancel the interpreter supervisor rather
/// than detaching it and leaving the candidate's process group alive.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_naive_preflight_reaps_its_process_group() {
    use std::os::unix::fs::PermissionsExt;

    let dir = workdir("cancelled-naive-preflight");
    let pid_file = dir.join("probe.pid");
    let interpreter = dir.join("hanging-bash");
    let quoted_pid_file = pid_file.to_string_lossy().replace('\'', "'\\''");
    std::fs::write(
        &interpreter,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > '{quoted_pid_file}'\nwhile :; do sleep 60; done\n"
        ),
    )
    .expect("write hanging interpreter");
    std::fs::set_permissions(&interpreter, std::fs::Permissions::from_mode(0o700))
        .expect("make hanging interpreter executable");

    let configured = interpreter.to_string_lossy().into_owned();
    let task = tokio::spawn(async move {
        coop_exec::preflight_naive_interpreter("bash", Some(&configured)).await
    });
    let startup_deadline = Instant::now() + Duration::from_secs(3);
    while !pid_file.is_file() && Instant::now() < startup_deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("hanging preflight started")
        .trim()
        .parse()
        .expect("probe pid");

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_unix_process_reaped(pid, "cancelled interpreter preflight").await;
    std::fs::remove_dir_all(&dir).expect("cancelled preflight released its files");
}

/// Force-aborting an Off-mode execution future must still release its Unix
/// process group, including descendants that outlive the interpreter leader.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn aborted_naive_execution_reaps_its_process_group() {
    let dir = workdir("aborted-naive-execution");
    let parent_pid = dir.join("parent.pid");
    let child_pid = dir.join("child.pid");
    let ctx = ExecContext {
        job_key: "aborted-naive-execution".into(),
        language: "bash".into(),
        code: "printf '%s\\n' \"$$\" > parent.pid\nsleep 60 &\nprintf '%s\\n' \"$!\" > child.pid\nwait\n".into(),
        stdin: None,
        limits: limits_wall(120),
        workdir: dir.clone(),
        interpreter_override: None,
        rootfs: None,
        helper_path: None,
        cancel: None,
        start_gate: None,
        seccomp: false,
    };
    let task = tokio::spawn(async move { execute(ctx, collector(), SandboxMode::Off).await });
    let startup_deadline = Instant::now() + Duration::from_secs(3);
    while (!parent_pid.is_file() || !child_pid.is_file()) && Instant::now() < startup_deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let parent: i32 = std::fs::read_to_string(&parent_pid)
        .expect("naive interpreter started")
        .trim()
        .parse()
        .expect("parent pid");
    let child: i32 = std::fs::read_to_string(&child_pid)
        .expect("naive descendant started")
        .trim()
        .parse()
        .expect("child pid");

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_unix_process_reaped(parent, "aborted naive execution").await;
    assert_unix_process_reaped(child, "aborted naive execution descendant").await;
    std::fs::remove_dir_all(&dir).expect("aborted naive execution released its workdir");
}

/// stdin is delivered to the job (deep-hunt: naive off-path write blocked
/// before supervision, and ns backend silently used /dev/null).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stdin_is_delivered_to_job() {
    let dir = workdir("stdin-deliver");
    let sink = Arc::new(Collect {
        stdout: Default::default(),
        stderr: Default::default(),
        violations: Default::default(),
        truncated: Default::default(),
    });
    let ctx = ExecContext {
        job_key: "stdin-deliver".into(),
        language: "python".into(),
        code: "import sys; print('ECHO:'+sys.stdin.read().strip())".into(),
        stdin: Some("hello-from-test\n".into()),
        limits: limits_wall(10),
        workdir: dir.clone(),
        interpreter_override: None,
        rootfs: None,
        helper_path: None,
        cancel: None,
        start_gate: None,
        seccomp: false,
    };
    let out = execute(ctx, sink.clone(), SandboxMode::Off).await.unwrap();
    assert_eq!(format!("{:?}", out.status), "Succeeded");
    let stdout = sink.stdout.lock().unwrap().join("\n");
    assert!(
        stdout.contains("ECHO:hello-from-test"),
        "stdin must reach the child; stdout was: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A child that ignores stdin must still be killed at wall_clock even when
/// stdin exceeds pipe capacity (deep-hunt: off-path `write_all` wedged the
/// worker forever).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_stdin_does_not_wedge_worker() {
    let dir = workdir("stdin-wedge");
    let big = "A".repeat(200_000);
    let sink = Arc::new(Collect {
        stdout: Default::default(),
        stderr: Default::default(),
        violations: Default::default(),
        truncated: Default::default(),
    });
    let ctx = ExecContext {
        job_key: "stdin-wedge".into(),
        language: "python".into(),
        code: "while True: pass".into(),
        stdin: Some(big),
        limits: limits_wall(3),
        workdir: dir.clone(),
        interpreter_override: None,
        rootfs: None,
        helper_path: None,
        cancel: None,
        start_gate: None,
        seccomp: false,
    };
    let started = Instant::now();
    let res = tokio::time::timeout(
        Duration::from_secs(20),
        execute(ctx, sink, SandboxMode::Off),
    )
    .await
    .expect("execute must return (wall clock or wedge)");
    let elapsed = started.elapsed();
    let out = res.unwrap();
    assert_eq!(
        format!("{:?}", out.status),
        "TimedOut",
        "busy loop with large stdin must hit wall clock"
    );
    assert!(
        elapsed < Duration::from_secs(12),
        "must be killed near wall=3s, took {elapsed:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Per-stream truncation: each stream emits exactly one `truncated` event
/// when it crosses MAX_OUTPUT_LINES, independent of the other stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn truncation_is_per_stream() {
    let dir = workdir("trunc");
    let sink = Arc::new(Collect {
        stdout: Default::default(),
        stderr: Default::default(),
        violations: Default::default(),
        truncated: Default::default(),
    });
    // Emit 10_010 lines to each stream; cap is 10_000, so each must truncate once.
    let code = r#"
import sys
for i in range(10010):
    print(f"o{i}")
    print(f"e{i}", file=sys.stderr)
"#;
    let ctx = ExecContext {
        job_key: "trunc".into(),
        language: "python".into(),
        code: code.into(),
        stdin: None,
        limits: limits_wall(15),
        workdir: dir.clone(),
        interpreter_override: None,
        rootfs: None,
        helper_path: None,
        cancel: None,
        start_gate: None,
        seccomp: false,
    };
    let out = execute(ctx, sink.clone(), SandboxMode::Off).await.unwrap();
    assert_eq!(format!("{:?}", out.status), "Succeeded");
    assert_eq!(
        sink.stdout.lock().unwrap().len(),
        coop_types::MAX_OUTPUT_LINES,
        "stdout must be capped"
    );
    assert_eq!(
        sink.stderr.lock().unwrap().len(),
        coop_types::MAX_OUTPUT_LINES,
        "stderr must be capped"
    );
    let truncated = sink.truncated.lock().unwrap();
    let stdout_trunc = truncated.iter().filter(|s| **s == Stream::Stdout).count();
    let stderr_trunc = truncated.iter().filter(|s| **s == Stream::Stderr).count();
    assert_eq!(
        stdout_trunc, 1,
        "exactly one stdout truncation, got {truncated:?}"
    );
    assert_eq!(
        stderr_trunc, 1,
        "exactly one stderr truncation, got {truncated:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stream without newlines used to grow one unbounded String in the server
/// heap. Fixed-buffer decoding must cap retained bytes while continuing to
/// drain the child to completion.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unterminated_output_is_byte_bounded() {
    let dir = workdir("unterminated");
    let sink = Arc::new(Collect {
        stdout: Default::default(),
        stderr: Default::default(),
        violations: Default::default(),
        truncated: Default::default(),
    });
    let ctx = ExecContext {
        job_key: "unterminated".into(),
        language: "python".into(),
        code: "import os; os.write(1, b'A' * (8 * 1024 * 1024))".into(),
        stdin: None,
        limits: limits_wall(15),
        workdir: dir.clone(),
        interpreter_override: None,
        rootfs: None,
        helper_path: None,
        cancel: None,
        start_gate: None,
        seccomp: false,
    };
    let outcome = execute(ctx, sink.clone(), SandboxMode::Off).await.unwrap();
    assert_eq!(format!("{:?}", outcome.status), "Succeeded");
    assert!(outcome.telemetry.stdout.truncated);
    assert_eq!(outcome.telemetry.stdout.bytes_seen, 8 * 1024 * 1024);
    assert!(
        outcome.telemetry.stdout.bytes_emitted <= coop_types::MAX_OUTPUT_BYTES_PER_STREAM as u64
    );
    assert_eq!(sink.truncated.lock().unwrap().len(), 1);
    assert!(sink
        .stdout
        .lock()
        .unwrap()
        .iter()
        .all(|record| record.len() <= coop_types::MAX_OUTPUT_RECORD_BYTES));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Output is intentionally always ready. The control tick is the first
/// biased branch, so cancellation remains authoritative under a flood.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn output_flood_cannot_starve_cancellation() {
    let dir = workdir("cancel-flood");
    let cancelled = Arc::new(ExecutionCancellation::default());
    let sink = Arc::new(Collect {
        stdout: Default::default(),
        stderr: Default::default(),
        violations: Default::default(),
        truncated: Default::default(),
    });
    let ctx = ExecContext {
        job_key: "cancel-flood".into(),
        language: "python".into(),
        code: "import os\nwhile True:\n os.write(1,b'x'*8192)\n os.write(2,b'y'*8192)".into(),
        stdin: None,
        limits: limits_wall(30),
        workdir: dir.clone(),
        interpreter_override: None,
        rootfs: None,
        helper_path: None,
        cancel: Some(cancelled.clone()),
        start_gate: None,
        seccomp: false,
    };
    let task = tokio::spawn(execute(ctx, sink, SandboxMode::Off));
    tokio::time::sleep(Duration::from_millis(250)).await;
    let started = Instant::now();
    cancelled.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("cancellation must not be starved")
        .expect("executor task")
        .expect("execution result");
    assert_eq!(format!("{:?}", outcome.status), "Cancelled");
    assert!(started.elapsed() < Duration::from_secs(2));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Node on Windows aborts during CSPRNG initialization when SystemRoot is
/// absent. Environment sanitization must preserve that platform prerequisite
/// without inheriting the rest of the parent environment.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn node_starts_with_sanitized_windows_environment() {
    if !std::process::Command::new("node")
        .arg("--version")
        .status()
        .is_ok_and(|status| status.success())
    {
        return;
    }
    let dir = workdir("node-system-root");
    let sink = collector();
    let ctx = ExecContext {
        job_key: "node-system-root".into(),
        language: "node".into(),
        code: r#"
const os = require('node:os');
const { spawnSync } = require('node:child_process');
if (!os.homedir() || !os.tmpdir()) throw new Error('missing home/temp environment');
const where = spawnSync('where.exe', ['node'], { encoding: 'utf8' });
if (where.status !== 0 || !where.stdout.trim()) throw new Error(`where.exe failed: ${where.stderr}`);
console.log('NODE-CSPRNG-OK');
"#
        .into(),
        stdin: None,
        limits: limits_wall(10),
        workdir: dir.clone(),
        interpreter_override: None,
        rootfs: None,
        helper_path: None,
        cancel: None,
        start_gate: None,
        seccomp: false,
    };
    let outcome = execute(ctx, sink.clone(), SandboxMode::Off).await.unwrap();
    assert_eq!(format!("{:?}", outcome.status), "Succeeded");
    assert!(sink
        .stdout
        .lock()
        .unwrap()
        .iter()
        .any(|line| line == "NODE-CSPRNG-OK"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The Windows resolver must select a native Bash that accepts `C:\\...`
/// script paths and retains Git's external Unix tools under Coop's sanitized
/// environment. A bare `bash` used to select System32's WSL launcher and fail
/// every default Bash job with exit 127.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn windows_default_bash_uses_a_probed_native_runtime() {
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let wsl_shim = std::path::PathBuf::from(system_root).join(r"System32\bash.exe");
    if wsl_shim.is_file() {
        let error = coop_exec::resolve_interpreter("bash", Some(&wsl_shim.to_string_lossy()))
            .expect_err("the System32 WSL launcher must never satisfy native Bash");
        assert!(
            error.to_string().contains("WSL/application-alias shim"),
            "WSL rejection must be explicit: {error}"
        );
    }

    let resolved = match coop_exec::preflight_naive_interpreter("bash", None).await {
        Ok(path) => path,
        Err(error) => {
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
            assert!(
                error
                    .to_string()
                    .contains("native Windows Bash is unavailable"),
                "missing native Bash must be reported truthfully: {error}"
            );
            return;
        }
    };
    let normalized = resolved.replace('/', "\\").to_ascii_lowercase();
    assert!(
        !normalized.contains("\\windows\\system32\\")
            && !normalized.contains("\\microsoft\\windowsapps\\"),
        "WSL/application-alias shim was accepted: {resolved}"
    );

    let dir = workdir("windows-default-native-bash");
    let sink = collector();
    let ctx = ExecContext {
        job_key: "windows-default-native-bash".into(),
        language: "bash".into(),
        code: concat!(
            "value=\"$(printf '%s' NATIVE_BASH | cat)\"\n",
            "test \"$value\" = NATIVE_BASH\n",
            "printf '%s\\n' WINDOWS-NATIVE-BASH-OK\n",
        )
        .into(),
        stdin: None,
        limits: limits_wall(10),
        workdir: dir.clone(),
        interpreter_override: None,
        rootfs: None,
        helper_path: None,
        cancel: None,
        start_gate: None,
        seccomp: false,
    };
    let outcome = execute(ctx, sink.clone(), SandboxMode::Off)
        .await
        .expect("default Windows Bash execution");
    assert_eq!(format!("{:?}", outcome.status), "Succeeded");
    assert!(sink
        .stdout
        .lock()
        .unwrap()
        .iter()
        .any(|line| line == "WINDOWS-NATIVE-BASH-OK"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(windows)]
fn build_windows_interpreter_fixture(
    directory: &std::path::Path,
    probe_count: &std::path::Path,
    job_count: &std::path::Path,
) -> std::path::PathBuf {
    let source = directory.join("interpreter_fixture.rs");
    let executable = directory.join("fixture-bash.exe");
    let code = format!(
        r#"
use std::io::Write;

const PROBE_COUNT: &str = {probe_count:?};
const JOB_COUNT: &str = {job_count:?};

fn append(path: &str, value: &str) {{
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(file, "{{value}}").unwrap();
}}

fn main() {{
    let executable = std::env::current_exe().unwrap();
    if executable.file_name().unwrap().to_string_lossy().starts_with("hanging-") {{
        append(PROBE_COUNT, &format!("hanging:{{}}", std::process::id()));
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    }}
    if executable.file_name().unwrap().to_string_lossy().starts_with("flooding-") {{
        let line = "X".repeat(8192);
        for _ in 0..32 {{
            println!("{{line}}");
        }}
        return;
    }}
    let source = std::env::args().nth(1).expect("source argument");
    let script = std::fs::read_to_string(source).unwrap();
    if script.contains("COOP_NAIVE_PREFLIGHT_OK") {{
        append(PROBE_COUNT, "probe");
        println!("COOP_NAIVE_PREFLIGHT_OK");
    }} else {{
        append(JOB_COUNT, "job");
        println!("FAKE-JOB-OK");
    }}
}}
"#,
        probe_count = probe_count.to_string_lossy(),
        job_count = job_count.to_string_lossy(),
    );
    std::fs::write(&source, code).expect("write interpreter fixture source");
    let output = std::process::Command::new("rustc")
        .arg("--edition=2021")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("run rustc for interpreter fixture");
    assert!(
        output.status.success(),
        "compile interpreter fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    executable
}

/// Startup probing is independently bounded and returns the exact executable
/// that the server can cache. Passing that returned path to jobs must launch
/// only the jobs themselves; it must not repeat the capability probe.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn windows_interpreter_preflight_is_bounded_cancel_safe_and_not_repeated_per_job() {
    let dir = workdir("windows-bounded-interpreter-preflight");
    let probe_count = dir.join("probe-count.txt");
    let job_count = dir.join("job-count.txt");
    let fixture = build_windows_interpreter_fixture(&dir, &probe_count, &job_count);
    let hanging = dir.join("hanging-bash.exe");
    std::fs::copy(&fixture, &hanging).expect("copy hanging interpreter fixture");
    let cancelled = dir.join("hanging-cancelled-bash.exe");
    std::fs::copy(&fixture, &cancelled).expect("copy cancelled interpreter fixture");
    let flooding = dir.join("flooding-bash.exe");
    std::fs::copy(&fixture, &flooding).expect("copy flooding interpreter fixture");

    let flood = coop_exec::preflight_naive_interpreter("bash", Some(&flooding.to_string_lossy()))
        .await
        .expect_err("preflight output must be retained within a fixed bound");
    assert!(
        flood.to_string().contains("output exceeded 65536 bytes"),
        "output-bound failure was not explicit: {flood}"
    );

    let cancelled_config = cancelled.to_string_lossy().into_owned();
    let cancelled_task = tokio::spawn(async move {
        coop_exec::preflight_naive_interpreter("bash", Some(&cancelled_config)).await
    });
    let startup_deadline = Instant::now() + Duration::from_secs(3);
    while std::fs::read_to_string(&probe_count)
        .map(|content| !content.lines().any(|line| line.starts_with("hanging:")))
        .unwrap_or(true)
        && Instant::now() < startup_deadline
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        std::fs::read_to_string(&probe_count)
            .expect("cancelled preflight started")
            .lines()
            .any(|line| line.starts_with("hanging:")),
        "cancelled preflight never launched its configured interpreter"
    );
    cancelled_task.abort();
    assert!(cancelled_task.await.unwrap_err().is_cancelled());
    let release_deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match std::fs::remove_file(&cancelled) {
            Ok(()) => break,
            Err(error) if Instant::now() < release_deadline => {
                let _ = error;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => panic!(
                "cancelled preflight retained its Windows process/Job Object and executable: {error}"
            ),
        }
    }

    let started = Instant::now();
    let timeout = coop_exec::preflight_naive_interpreter("bash", Some(&hanging.to_string_lossy()))
        .await
        .expect_err("a hanging configured interpreter must fail boundedly");
    assert_eq!(timeout.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "ten-second preflight timeout was not enforced: {:?}",
        started.elapsed()
    );

    let resolved = coop_exec::preflight_naive_interpreter("bash", Some(&fixture.to_string_lossy()))
        .await
        .expect("working interpreter fixture preflight");
    assert_eq!(
        std::fs::read_to_string(&probe_count)
            .unwrap()
            .lines()
            .filter(|line| *line == "probe")
            .count(),
        1,
        "startup should perform one probe"
    );

    for index in 0..2 {
        let job_dir = dir.join(format!("job-{index}"));
        std::fs::create_dir(&job_dir).unwrap();
        let ctx = ExecContext {
            job_key: format!("cached-interpreter-{index}"),
            language: "bash".into(),
            code: "printf '%s\\n' ignored".into(),
            stdin: None,
            limits: limits_wall(10),
            workdir: job_dir,
            interpreter_override: Some(resolved.clone()),
            rootfs: None,
            helper_path: None,
            cancel: None,
            start_gate: None,
            seccomp: false,
        };
        let outcome = execute(ctx, collector(), SandboxMode::Off)
            .await
            .expect("execute through cached interpreter path");
        assert_eq!(format!("{:?}", outcome.status), "Succeeded");
    }
    assert_eq!(
        std::fs::read_to_string(&probe_count)
            .unwrap()
            .lines()
            .filter(|line| *line == "probe")
            .count(),
        1,
        "job execution must not repeat startup probing"
    );
    assert_eq!(
        std::fs::read_to_string(&job_count).unwrap().lines().count(),
        2,
        "the cached executable should run exactly once per job"
    );
    std::fs::remove_dir_all(&dir)
        .expect("bounded preflight must release the hanging executable and workdir");
}

/// CREATE_SUSPENDED closes the assign-to-job race: the descendant can only
/// start after its parent belongs to the kill-on-close Job Object.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn windows_timeout_kills_the_entire_descendant_tree() {
    let dir = workdir("windows-job-tree");
    let marker = dir.join("descendant-survived.txt");
    let sink = collector();
    let descendant = format!(
        "import pathlib,time; time.sleep(3); pathlib.Path({marker:?}).write_text('survived'); time.sleep(30)"
    );
    let code = format!(
        "import subprocess,sys,time\nsubprocess.Popen([sys.executable,'-c',{descendant:?}])\nprint('DESCENDANT-SPAWNED', flush=True)\ntime.sleep(30)"
    );
    let ctx = ExecContext {
        job_key: "windows-job-tree".into(),
        language: "python".into(),
        code,
        stdin: None,
        limits: limits_wall(1),
        workdir: dir.clone(),
        interpreter_override: None,
        rootfs: None,
        helper_path: None,
        cancel: None,
        start_gate: None,
        seccomp: false,
    };
    let started = Instant::now();
    let outcome = execute(ctx, sink, SandboxMode::Off).await.unwrap();
    assert_eq!(format!("{:?}", outcome.status), "TimedOut");
    assert!(started.elapsed() < Duration::from_secs(8));
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        !marker.exists(),
        "descendant survived the parent timeout and escaped its Windows Job Object"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A successful leader must not let a background descendant outlive the
/// execution. Closing or terminating the Job Object must reap the whole tree.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn windows_normal_exit_kills_the_entire_descendant_tree() {
    let dir = workdir("windows-job-normal-exit-tree");
    let marker = dir.join("descendant-survived.txt");
    let sink = collector();
    let descendant = format!(
        "import pathlib,time; time.sleep(1); pathlib.Path({marker:?}).write_text('survived'); time.sleep(30)"
    );
    let code = format!(
        "import subprocess,sys\nsubprocess.Popen([sys.executable,'-c',{descendant:?}])\nprint('DESCENDANT-SPAWNED', flush=True)"
    );
    let ctx = ExecContext {
        job_key: "windows-job-normal-exit-tree".into(),
        language: "python".into(),
        code,
        stdin: None,
        limits: limits_wall(10),
        workdir: dir.clone(),
        interpreter_override: None,
        rootfs: None,
        helper_path: None,
        cancel: None,
        start_gate: None,
        seccomp: false,
    };
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        execute(ctx, sink.clone(), SandboxMode::Off),
    )
    .await
    .expect("normal exit must not wait for the background descendant")
    .unwrap();
    assert_eq!(format!("{:?}", outcome.status), "Succeeded");
    assert!(sink
        .stdout
        .lock()
        .unwrap()
        .iter()
        .any(|line| line == "DESCENDANT-SPAWNED"));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !marker.exists(),
        "descendant survived its parent's normal exit and escaped its Windows Job Object"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Explicit cancellation must terminate descendants through the same Job
/// Object path as a wall-clock timeout.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn windows_cancellation_kills_the_entire_descendant_tree() {
    let dir = workdir("windows-job-cancel-tree");
    let ready = dir.join("descendant-ready.txt");
    let marker = dir.join("descendant-survived.txt");
    let cancelled = Arc::new(ExecutionCancellation::default());
    let descendant = format!(
        "import pathlib,time; pathlib.Path({ready:?}).write_text('ready'); time.sleep(1); pathlib.Path({marker:?}).write_text('survived'); time.sleep(30)"
    );
    let code = format!(
        "import subprocess,sys,time\nsubprocess.Popen([sys.executable,'-c',{descendant:?}])\ntime.sleep(30)"
    );
    let ctx = ExecContext {
        job_key: "windows-job-cancel-tree".into(),
        language: "python".into(),
        code,
        stdin: None,
        limits: limits_wall(30),
        workdir: dir.clone(),
        interpreter_override: None,
        rootfs: None,
        helper_path: None,
        cancel: Some(cancelled.clone()),
        start_gate: None,
        seccomp: false,
    };
    let task = tokio::spawn(execute(ctx, collector(), SandboxMode::Off));
    let ready_deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() && Instant::now() < ready_deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        ready.exists(),
        "descendant did not start before cancellation"
    );

    let started = Instant::now();
    cancelled.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancellation must not wait for the background descendant")
        .expect("executor task")
        .expect("execution result");
    assert_eq!(format!("{:?}", outcome.status), "Cancelled");
    assert!(started.elapsed() < Duration::from_secs(3));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !marker.exists(),
        "descendant survived explicit cancellation and escaped its Windows Job Object"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
#[tokio::test]
async fn namespaces_mode_never_falls_back_to_naive_execution() {
    let dir = workdir("namespaces-fail-closed");
    let ctx = ExecContext {
        job_key: "namespaces-fail-closed".into(),
        language: "python".into(),
        code: "print('MUST-NOT-RUN')".into(),
        stdin: None,
        limits: limits_wall(10),
        workdir: dir.clone(),
        interpreter_override: None,
        rootfs: None,
        helper_path: None,
        cancel: None,
        start_gate: None,
        seccomp: false,
    };
    let error = execute(ctx, collector(), SandboxMode::Namespaces)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    assert!(!dir.join("job.py").exists(), "naive source staging ran");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires root, delegated cgroup v2, COOP_ROOTFS, and COOP_SANDBOX_HELPER"]
async fn aborting_namespace_execution_reaps_its_cgroup() {
    let rootfs =
        std::path::PathBuf::from(std::env::var("COOP_ROOTFS").expect("COOP_ROOTFS is required"));
    let helper = std::path::PathBuf::from(
        std::env::var("COOP_SANDBOX_HELPER").expect("COOP_SANDBOX_HELPER is required"),
    );
    let key = format!("abort-reap-{}", std::process::id());
    let dir = workdir("namespace-abort-reap");
    let membership = std::fs::read_to_string("/proc/self/cgroup").unwrap();
    let relative = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .expect("unified cgroup membership");
    let group = std::path::Path::new("/sys/fs/cgroup")
        .join(relative.trim_start_matches('/'))
        .join("coop-jobs")
        .join(format!("job-{key}"));
    let ctx = ExecContext {
        job_key: key,
        language: "python".into(),
        code: "while True: pass".into(),
        stdin: None,
        limits: limits_wall(30),
        workdir: dir.clone(),
        interpreter_override: None,
        rootfs: Some(rootfs),
        helper_path: Some(helper),
        cancel: None,
        start_gate: None,
        seccomp: true,
    };
    let task = tokio::spawn(execute(ctx, collector(), SandboxMode::Namespaces));
    let populated_deadline = Instant::now() + Duration::from_secs(10);
    while std::fs::read_to_string(group.join("cgroup.procs"))
        .map(|procs| procs.trim().is_empty())
        .unwrap_or(true)
        && Instant::now() < populated_deadline
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        group.join("cgroup.procs").is_file(),
        "namespace execution never created its cgroup at {}",
        group.display()
    );

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let cleanup_deadline = Instant::now() + Duration::from_secs(6);
    while group.exists() && Instant::now() < cleanup_deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !group.exists(),
        "aborted execution leaked cgroup {}",
        group.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Startup readiness must exercise every advertised interpreter, including
/// its configured rootfs-internal executable override, through the complete
/// namespace and seccomp path.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires root, delegated cgroup v2, COOP_ROOTFS, and COOP_SANDBOX_HELPER"]
async fn namespace_preflight_runs_all_configured_interpreters() {
    let rootfs =
        std::path::PathBuf::from(std::env::var("COOP_ROOTFS").expect("COOP_ROOTFS is required"));
    let helper = std::path::PathBuf::from(
        std::env::var("COOP_SANDBOX_HELPER").expect("COOP_SANDBOX_HELPER is required"),
    );
    let jobs_root = workdir("namespace-preflight");
    coop_exec::namespace_sandbox_execution_preflight(
        &rootfs,
        &helper,
        &jobs_root,
        true,
        &[
            ("python", Some("/usr/bin/python3")),
            ("node", Some("/usr/bin/node")),
            ("bash", Some("/usr/bin/bash")),
        ],
    )
    .await
    .expect("all configured interpreters must pass the full execution preflight");
    let false_node_success = coop_exec::namespace_sandbox_execution_preflight(
        &rootfs,
        &helper,
        &jobs_root,
        true,
        &[("node", Some("/bin/true"))],
    )
    .await
    .expect_err("a successful executable that ignores Node code must fail readiness");
    assert!(
        false_node_success.to_string().contains("node")
            && false_node_success.to_string().contains("sentinel"),
        "false-success failure must identify Node and its missing sentinel: {false_node_success}"
    );
    let node_error = coop_exec::namespace_sandbox_execution_preflight(
        &rootfs,
        &helper,
        &jobs_root,
        true,
        &[("node", Some("/usr/bin/coop-node-does-not-exist"))],
    )
    .await
    .expect_err("a missing configured Node interpreter must fail readiness");
    assert!(
        node_error.to_string().contains("node"),
        "configured-interpreter failure must identify its language: {node_error}"
    );
    assert_eq!(
        std::fs::read_dir(&jobs_root).unwrap().count(),
        0,
        "execution preflight must remove every disposable workdir"
    );
    let _ = std::fs::remove_dir(&jobs_root);
}
