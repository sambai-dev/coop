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

fn is_usable_bash(p: &PathBuf) -> bool {
    // WSL's C:\Windows\System32\bash.exe answers --version but fails on
    // Windows paths (the job script is a Windows path). Probe with a real
    // script file containing a Windows path so the WSL shim is rejected.
    let probe_dir = std::env::temp_dir().join(format!("coop-bash-probe-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&probe_dir);
    let probe_file = probe_dir.join("probe.sh");
    if std::fs::write(&probe_file, "echo probe-ok\n").is_err() {
        return false;
    }
    let ok = std::process::Command::new(p)
        .arg(&probe_file)
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("probe-ok"))
        .unwrap_or(false);
    let _ = std::fs::remove_file(&probe_file);
    let _ = std::fs::remove_dir(&probe_dir);
    ok
}

fn find_bash() -> PathBuf {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            #[cfg(windows)]
            let candidate = dir.join("bash.exe");
            #[cfg(not(windows))]
            let candidate = dir.join("bash");
            if candidate.is_file() && is_usable_bash(&candidate) {
                return candidate;
            }
        }
    }
    // Explicit Git-for-Windows location that is often not on PATH ordering
    // but is the only usable bash on many Windows dev machines.
    for fallback in [
        "C:\\Program Files\\Git\\usr\\bin\\bash.exe",
        "C:\\Program Files\\Git\\bin\\bash.exe",
        "/bin/bash",
        "/usr/bin/bash",
    ] {
        let candidate = PathBuf::from(fallback);
        if candidate.is_file() && is_usable_bash(&candidate) {
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
        seccomp: false,
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
