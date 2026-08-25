use coop_exec::{execute, ExecContext, SandboxMode, Sink, Stream};
use coop_types::Limits;
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
        cancel: None,
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
        cancel: None,
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
        cancel: None,
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
