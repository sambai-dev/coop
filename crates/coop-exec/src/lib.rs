pub mod naive;

#[cfg(target_os = "linux")]
pub mod linux_sandbox;

use coop_types::Limits;
use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};
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

#[derive(Debug, Clone)]
pub struct ExecContext {
    pub job_key: String,
    pub language: String,
    pub code: String,
    pub stdin: Option<String>,
    pub limits: Limits,
    pub workdir: PathBuf,
    pub interpreter_override: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExecOutcome {
    pub status: coop_types::OutcomeStatus,
    pub exit_code: Option<i32>,
    pub killed_by: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    Off,
    Namespaces,
}

impl SandboxMode {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "off" | "none" | "naive" => SandboxMode::Off,
            "ns" | "namespaces" | "sandbox" => SandboxMode::Namespaces,
            _ => SandboxMode::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SandboxMode::Off => "off",
            SandboxMode::Namespaces => "namespaces+cgroups-v2",
        }
    }
}

pub fn namespace_sandbox_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        nix::unistd::Uid::effective().is_root()
            && std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

pub async fn execute(
    ctx: ExecContext,
    sink: Arc<dyn Sink>,
    mode: SandboxMode,
) -> io::Result<ExecOutcome> {
    #[cfg(target_os = "linux")]
    if mode == SandboxMode::Namespaces {
        return linux_sandbox::run(ctx, sink).await;
    }
    let _ = &mode;
    naive::run(ctx, sink).await
}

pub fn resolve_interpreter(language: &str, override_bin: Option<&str>) -> String {
    if let Some(bin) = override_bin {
        return bin.to_string();
    }
    match language {
        "python" => {
            if cfg!(windows) {
                "python".to_string()
            } else {
                "python3".to_string()
            }
        }
        "node" => "node".to_string(),
        _ => "bash".to_string(),
    }
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
