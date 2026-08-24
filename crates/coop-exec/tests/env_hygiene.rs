use coop_exec::{execute, ExecContext, SandboxMode, Sink, Stream};
use coop_types::{Limits, OutcomeStatus};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

struct Collect {
    stdout: Mutex<Vec<String>>,
    stderr: Mutex<Vec<String>>,
    violations: Mutex<Vec<String>>,
}

impl Sink for Collect {
    fn output(&self, stream: Stream, line: String) {
        match stream {
            Stream::Stdout => self.stdout.lock().unwrap().push(line),
            Stream::Stderr => self.stderr.lock().unwrap().push(line),
        }
    }

    fn violation(&self, rule: &'static str, _detail: Value) {
        self.violations.lock().unwrap().push(rule.to_string());
    }

    fn truncated(&self, _stream: Stream) {}
}

fn find_bash() -> PathBuf {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            #[cfg(windows)]
            let candidate = dir.join("bash.exe");
            #[cfg(not(windows))]
            let candidate = dir.join("bash");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    for fallback in ["/bin/bash", "/usr/bin/bash"] {
        let candidate = PathBuf::from(fallback);
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!("env-hygiene test requires bash on PATH or /bin/bash");
}

#[tokio::test]
async fn naive_mode_does_not_leak_host_env_to_jobs() {
    std::env::set_var("COOP_API_KEYS", "tenant:test-leak-probe");
    assert_eq!(
        std::env::var("COOP_API_KEYS").as_deref(),
        Ok("tenant:test-leak-probe"),
        "precondition: host secret present in the worker process"
    );

    let workdir = std::env::temp_dir().join(format!("coop-env-hygiene-{}", std::process::id()));
    fs::create_dir_all(&workdir).expect("create workdir");

    let sink = Arc::new(Collect {
        stdout: Mutex::new(Vec::new()),
        stderr: Mutex::new(Vec::new()),
        violations: Mutex::new(Vec::new()),
    });

    let ctx = ExecContext {
        job_key: "env-hygiene".to_string(),
        language: "bash".to_string(),
        code: "if [ -n \"$COOP_API_KEYS\" ]; then echo \"LEAKED=$COOP_API_KEYS\"; else echo CLEAN; fi\n"
            .to_string(),
        stdin: None,
        limits: Limits::default(),
        workdir: workdir.clone(),
        interpreter_override: Some(find_bash().to_string_lossy().into_owned()),
        cancel: None,
    };

    let outcome = execute(ctx, sink.clone(), SandboxMode::Off)
        .await
        .expect("naive execute");

    assert_eq!(
        outcome.status,
        OutcomeStatus::Succeeded,
        "stderr: {:?}, violations: {:?}",
        sink.stderr.lock().unwrap(),
        sink.violations.lock().unwrap()
    );

    let stdout = sink.stdout.lock().unwrap().join("\n");
    assert!(
        stdout.contains("CLEAN"),
        "job must observe a scrubbed environment; stdout was:\n{stdout}"
    );
    assert!(
        !stdout.contains("LEAKED"),
        "host secret COOP_API_KEYS leaked into sandbox=off job; stdout was:\n{stdout}"
    );

    let _ = fs::remove_dir_all(&workdir);
}
