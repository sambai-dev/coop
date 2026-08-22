use crate::{ext_for, resolve_interpreter, ExecContext, ExecOutcome, Sink};
use coop_types::{Limits, OutcomeStatus};
use nix::mount::{mount, MsFlags};
use nix::sched::{unshare, CloneFlags};
use nix::sys::resource::{setrlimit, Resource};
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{chdir, execve, fork, setgid, setsid, setuid, ForkResult, Gid, Uid};
use serde_json::json;
use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const NOBODY_GID: u32 = 65534;
const NOBODY_UID: u32 = 65534;

pub async fn run(ctx: ExecContext, sink: Arc<dyn Sink>) -> io::Result<ExecOutcome> {
    let cg_dir = prepare_cgroup(&ctx.limits, &ctx.job_key)?;
    let started = Instant::now();

    let src_path = ctx.workdir.join(format!("job.{}", ext_for(&ctx.language)));
    tokio::fs::write(&src_path, &ctx.code).await?;

    let interp = resolve_interpreter(&ctx.language, ctx.interpreter_override.as_deref());
    let interp_abs = find_in_path(Path::new(&interp))
        .ok_or_else(|| io::Error::other(format!("interpreter not found: {interp}")))?;

    let prog = CString::new(interp_abs.as_os_str().as_bytes())?;
    let src_c = CString::new(src_path.as_os_str().as_bytes())?;
    let argv = vec![prog.clone(), src_c];
    let envp = vec![
        CString::new("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")?,
        CString::new("HOME=/tmp/home")?,
        CString::new("TMPDIR=/tmp")?,
        CString::new("LANG=C.UTF-8")?,
    ];
    let cg_dir_c = CString::new(cg_dir.as_os_str().as_bytes())?;
    let mem_bytes = ctx.limits.mem_mb.min(2048) as u64 * 1024 * 1024;
    let oom_before = read_oom_kills(&cg_dir);

    let fork_result = unsafe { fork() }.map_err(io::Error::other)?;

    match fork_result {
        ForkResult::Child => child_setup(&cg_dir_c, &prog, &argv, &envp, &ctx.limits, mem_bytes),
        ForkResult::Parent { child } => {
            supervise(child, sink, cg_dir, ctx.limits, oom_before, started).await
        }
    }
}

fn child_setup(
    cg_dir: &CString,
    prog: &CString,
    argv: &[CString],
    envp: &[CString],
    limits: &Limits,
    mem_bytes: u64,
) -> ! {
    let bail = |_: nix::errno::Errno| std::process::exit(126);
    let _ = setsid();

    let ns_flags = CloneFlags::CLONE_NEWNS
        | CloneFlags::CLONE_NEWPID
        | CloneFlags::CLONE_NEWNET
        | CloneFlags::CLONE_NEWIPC
        | CloneFlags::CLONE_NEWUTS;
    if unshare(ns_flags).is_err() {
        std::process::exit(126);
    }

    let _ =
        mount::<str, str, str, str>(None, "/", None, MsFlags::MS_REC | MsFlags::MS_PRIVATE, None);
    if mount::<str, str, str, str>(
        Some("/"),
        "/",
        None,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None,
    )
    .is_err()
    {
        std::process::exit(126);
    }
    if mount::<str, str, str, str>(
        Some("/"),
        "/",
        None,
        MsFlags::MS_BIND
            | MsFlags::MS_REMOUNT
            | MsFlags::MS_RDONLY
            | MsFlags::MS_NOSUID
            | MsFlags::MS_NODEV,
        None,
    )
    .is_err()
    {
        std::process::exit(126);
    }
    let tmp_opts = format!("size={},mode=1777", mem_bytes);
    if mount::<str, str, str, str>(
        None,
        "/tmp",
        Some("tmpfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
        Some(tmp_opts.as_str()),
    )
    .is_err()
    {
        std::process::exit(126);
    }
    let _ = fs::create_dir_all("/tmp/home");
    let _ = mount::<str, str, str, str>(
        None,
        "/proc",
        Some("proc"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
        None,
    );

    let cpu: u64 = u64::from(limits.cpu_seconds.clamp(1, 240));
    let _ = setrlimit(Resource::RLIMIT_CPU, cpu + 1, cpu + 2);
    let _ = setrlimit(Resource::RLIMIT_AS, mem_bytes, mem_bytes);
    let pids = u64::from(limits.max_pids);
    let _ = setrlimit(Resource::RLIMIT_NPROC, pids, pids);
    let _ = setrlimit(Resource::RLIMIT_NOFILE, 256, 512);
    let fsize = u64::from(limits.max_file_mb) * 1024 * 1024;
    let _ = setrlimit(Resource::RLIMIT_FSIZE, fsize, fsize);

    let _ = fs::write(
        format!("{}/cgroup.procs", cg_dir.to_string_lossy()),
        std::process::id().to_string(),
    );

    if Uid::effective().is_root() {
        let _ = setgid(Gid::from_raw(NOBODY_GID));
        let _ = setuid(Uid::from_raw(NOBODY_UID));
    }

    let _ = chdir("/tmp");

    let argv_refs: Vec<&CString> = argv.iter().collect();
    let envp_refs: Vec<&CString> = envp.iter().collect();
    let _ = execve(prog, &argv_refs, &envp_refs);
    std::process::exit(127);
}

async fn supervise(
    child: nix::unistd::Pid,
    sink: Arc<dyn Sink>,
    cg_dir: PathBuf,
    limits: Limits,
    oom_before: u64,
    started: Instant,
) -> io::Result<ExecOutcome> {
    let wall = Duration::from_secs(limits.wall_seconds.max(1) as u64);
    let deadline = started + wall;
    let mut killed_on_timeout = false;

    let status = loop {
        match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {
                if !killed_on_timeout && Instant::now() >= deadline {
                    killed_on_timeout = true;
                    sink.violation(
                        "wall_clock_exceeded",
                        json!({"wall_seconds": limits.wall_seconds}),
                    );
                    let _ = kill(neg_pid(child), Signal::SIGKILL);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok(st) => break st,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => {
                cleanup_cgroup(&cg_dir);
                return Err(io::Error::other(e));
            }
        }
    };

    let oom_after = read_oom_kills(&cg_dir);
    cleanup_cgroup(&cg_dir);

    let outcome = match status {
        WaitStatus::Exited(_, code) => ExecOutcome {
            status: if code == 0 {
                OutcomeStatus::Succeeded
            } else {
                OutcomeStatus::Failed
            },
            exit_code: Some(code),
            killed_by: None,
        },
        WaitStatus::Signaled(_, sig, _) => {
            let oom = sig == Signal::SIGKILL && oom_after > oom_before;
            if oom {
                sink.violation("memory_cap_exceeded", json!({"mem_mb": limits.mem_mb}));
                ExecOutcome {
                    status: OutcomeStatus::OomKilled,
                    exit_code: None,
                    killed_by: Some("cgroup-oom".into()),
                }
            } else if killed_on_timeout && sig == Signal::SIGKILL {
                ExecOutcome {
                    status: OutcomeStatus::TimedOut,
                    exit_code: None,
                    killed_by: Some("wall-clock".into()),
                }
            } else if sig == Signal::SIGXCPU {
                sink.violation(
                    "cpu_limit_exceeded",
                    json!({"cpu_seconds": limits.cpu_seconds}),
                );
                ExecOutcome {
                    status: OutcomeStatus::Failed,
                    exit_code: None,
                    killed_by: Some("rlimit-cpu".into()),
                }
            } else {
                ExecOutcome {
                    status: OutcomeStatus::Failed,
                    exit_code: None,
                    killed_by: Some(format!("{sig:?}")),
                }
            }
        }
        other => ExecOutcome {
            status: OutcomeStatus::Failed,
            exit_code: None,
            killed_by: Some(format!("{other:?}")),
        },
    };

    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        status = ?outcome.status,
        "sandboxed job finished"
    );
    Ok(outcome)
}

fn neg_pid(pid: nix::unistd::Pid) -> nix::unistd::Pid {
    nix::unistd::Pid::from_raw(-pid.as_raw())
}

fn prepare_cgroup(limits: &Limits, job_key: &str) -> io::Result<PathBuf> {
    let base = PathBuf::from("/sys/fs/cgroup/coop-jobs");
    fs::create_dir_all(&base)?;
    let dir = base.join(format!("job-{job_key}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("memory.max"), (limits.mem_mb as u64 * 1024 * 1024).to_string())?;
    fs::write(dir.join("memory.swap.max"), "0")?;
    fs::write(
        dir.join("cpu.max"),
        format!("{} 1000000", limits.cpu_seconds as u64 * 1_000_000),
    )?;
    fs::write(dir.join("pids.max"), limits.max_pids.to_string())?;
    Ok(dir)
}

fn read_oom_kills(dir: &Path) -> u64 {
    let Ok(text) = fs::read_to_string(dir.join("memory.events")) else {
        return 0;
    };
    text.lines()
        .find_map(|l| {
            l.strip_prefix("oom_kill ")
                .and_then(|v| v.trim().parse().ok())
        })
        .unwrap_or(0)
}

fn cleanup_cgroup(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

fn find_in_path(bin: &Path) -> Option<PathBuf> {
    if bin.components().count() > 1 {
        return fs::canonicalize(bin)
            .ok()
            .or_else(|| bin.is_file().then(|| bin.to_path_buf()));
    }
    let name = bin.to_string_lossy();
    let paths = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into());
    for dir in paths.split(':') {
        let candidate = Path::new(dir).join(&*name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
