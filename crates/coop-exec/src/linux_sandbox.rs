use crate::{ext_for, resolve_interpreter, ExecContext, ExecOutcome, Sink, Stream};
use coop_types::{Limits, OutcomeStatus, MAX_OUTPUT_LINES};
use nix::mount::{mount, MsFlags};
use nix::sched::{unshare, CloneFlags};
use nix::sys::resource::{setrlimit, Resource};
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{chdir, close, dup2, execve, fork, setgid, setsid, setuid, ForkResult, Gid, Uid};
use serde_json::json;
use std::ffi::CString;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader, Lines};

const NOBODY_GID: u32 = 65534;
const NOBODY_UID: u32 = 65534;

const DRAIN_GRACE: Duration = Duration::from_secs(2);

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
    let mem_bytes = ctx.limits.mem_mb as u64 * 1024 * 1024;
    let oom_before = read_oom_kills(&cg_dir);

    let (out_parent, out_child) = UnixStream::pair().map_err(io::Error::other)?;
    let (err_parent, err_child) = UnixStream::pair().map_err(io::Error::other)?;

    let out_r = out_parent.into_raw_fd();
    let err_r = err_parent.into_raw_fd();
    let out_w = out_child.into_raw_fd();
    let err_w = err_child.into_raw_fd();

    let plan = ChildPlan {
        cg_dir: cg_dir_c,
        prog,
        argv,
        envp,
        cpu_seconds: u64::from(ctx.limits.cpu_seconds.clamp(1, 240)),
        mem_bytes,
        max_pids: u64::from(ctx.limits.max_pids),
        fsize_bytes: u64::from(ctx.limits.max_file_mb) * 1024 * 1024,
        out_r,
        out_w,
        err_r,
        err_w,
    };

    let fork_result = unsafe { fork() }.map_err(io::Error::other)?;

    match fork_result {
        ForkResult::Child => child_setup(&plan),
        ForkResult::Parent { child } => {
            let _ = close(plan.out_w);
            let _ = close(plan.err_w);

            let out_std = unsafe { UnixStream::from_raw_fd(plan.out_r) };
            let err_std = unsafe { UnixStream::from_raw_fd(plan.err_r) };
            out_std.set_nonblocking(true)?;
            err_std.set_nonblocking(true)?;
            let out_tokio = tokio::net::UnixStream::from_std(out_std)?;
            let err_tokio = tokio::net::UnixStream::from_std(err_std)?;

            let sctx = SuperviseCtx {
                child,
                sink,
                cg_dir,
                limits: ctx.limits,
                oom_before,
                started,
            };

            supervise(
                sctx,
                BufReader::new(out_tokio).lines(),
                BufReader::new(err_tokio).lines(),
            )
            .await
        }
    }
}

struct ChildPlan {
    cg_dir: CString,
    prog: CString,
    argv: Vec<CString>,
    envp: Vec<CString>,
    cpu_seconds: u64,
    mem_bytes: u64,
    max_pids: u64,
    fsize_bytes: u64,
    out_r: i32,
    out_w: i32,
    err_r: i32,
    err_w: i32,
}

fn child_setup(plan: &ChildPlan) -> ! {
    let _ = close(plan.out_r);
    let _ = close(plan.err_r);

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
    let tmp_opts = format!("size={},mode=1777", plan.mem_bytes);
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

    let cpu = plan.cpu_seconds;
    let _ = setrlimit(Resource::RLIMIT_CPU, cpu + 1, cpu + 2);
    let _ = setrlimit(Resource::RLIMIT_AS, plan.mem_bytes, plan.mem_bytes);
    let _ = setrlimit(Resource::RLIMIT_NPROC, plan.max_pids, plan.max_pids);
    let _ = setrlimit(Resource::RLIMIT_NOFILE, 256, 512);
    let _ = setrlimit(Resource::RLIMIT_FSIZE, plan.fsize_bytes, plan.fsize_bytes);

    if fs::write(
        format!("{}/cgroup.procs", plan.cg_dir.to_string_lossy()),
        std::process::id().to_string(),
    )
    .is_err()
    {
        std::process::exit(126);
    }

    if Uid::effective().is_root() {
        let _ = setgid(Gid::from_raw(NOBODY_GID));
        let _ = setuid(Uid::from_raw(NOBODY_UID));
    }

    let _ = chdir("/tmp");

    let _ = dup2(plan.out_w, 1);
    let _ = dup2(plan.err_w, 2);
    let _ = close(plan.out_w);
    let _ = close(plan.err_w);
    if let Ok(null) = fs::File::open("/dev/null") {
        let _ = dup2(null.as_raw_fd(), 0);
    }

    let argv_refs: Vec<&CString> = plan.argv.iter().collect();
    let envp_refs: Vec<&CString> = plan.envp.iter().collect();
    let _ = execve(&plan.prog, &argv_refs, &envp_refs);
    std::process::exit(127);
}

async fn next_line(
    reader: &mut Lines<BufReader<tokio::net::UnixStream>>,
    done: bool,
) -> Option<io::Result<Option<String>>> {
    if done {
        std::future::pending::<()>().await;
        None
    } else {
        Some(reader.next_line().await)
    }
}

async fn poll_status(child: nix::unistd::Pid, reaped: bool) -> Option<nix::Result<WaitStatus>> {
    if reaped {
        std::future::pending::<()>().await;
        None
    } else {
        Some(waitpid(child, Some(WaitPidFlag::WNOHANG)))
    }
}

struct LineRouter<'a> {
    sink: &'a Arc<dyn Sink>,
    counts: (usize, usize),
    truncated: bool,
}

impl LineRouter<'_> {
    fn route(&mut self, stream: Stream, line: String) {
        let count = match stream {
            Stream::Stdout => &mut self.counts.0,
            Stream::Stderr => &mut self.counts.1,
        };
        if *count < MAX_OUTPUT_LINES {
            *count += 1;
            self.sink.output(stream, line);
        } else if !self.truncated {
            self.truncated = true;
            self.sink.truncated(stream);
        }
    }
}

struct SuperviseCtx {
    child: nix::unistd::Pid,
    sink: Arc<dyn Sink>,
    cg_dir: PathBuf,
    limits: Limits,
    oom_before: u64,
    started: Instant,
}

async fn supervise(
    ctx: SuperviseCtx,
    mut out_lines: Lines<BufReader<tokio::net::UnixStream>>,
    mut err_lines: Lines<BufReader<tokio::net::UnixStream>>,
) -> io::Result<ExecOutcome> {
    let wall = Duration::from_secs(ctx.limits.wall_seconds.max(1) as u64);
    let deadline = ctx.started + wall;
    let mut killed_on_timeout = false;
    let mut router = LineRouter {
        sink: &ctx.sink,
        counts: (0, 0),
        truncated: false,
    };

    let mut reaped: Option<WaitStatus> = None;
    let mut out_done = false;
    let mut err_done = false;
    let mut drain_deadline: Option<Instant> = None;

    while reaped.is_none() || !out_done || !err_done {
        let drain_guard = async {
            match drain_deadline {
                Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            biased;

            _ = drain_guard => {
                tracing::warn!(
                    elapsed_ms = ctx.started.elapsed().as_millis() as u64,
                    "stream drain cut off: wall budget exhausted after reap"
                );
                out_done = true;
                err_done = true;
            }

            line = next_line(&mut out_lines, out_done) => match line {
                Some(Ok(Some(text))) => router.route(Stream::Stdout, text),
                Some(Ok(None)) | Some(Err(_)) | None => out_done = true,
            },

            line = next_line(&mut err_lines, err_done) => match line {
                Some(Ok(Some(text))) => router.route(Stream::Stderr, text),
                Some(Ok(None)) | Some(Err(_)) | None => err_done = true,
            },

            polled = poll_status(ctx.child, reaped.is_some()) => match polled.expect("not pending") {
                Ok(WaitStatus::StillAlive) => {
                    if !killed_on_timeout && Instant::now() >= deadline {
                        killed_on_timeout = true;
                        ctx.sink.violation(
                            "wall_clock_exceeded",
                            json!({"wall_seconds": ctx.limits.wall_seconds}),
                        );
                        let _ = kill(neg_pid(ctx.child), Signal::SIGKILL);
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Ok(status) => {
                    reaped = Some(status);
                    let _ = kill(neg_pid(ctx.child), Signal::SIGKILL);
                    drain_deadline = Some(deadline.max(Instant::now() + DRAIN_GRACE));
                }
                Err(nix::errno::Errno::EINTR) => {}
                Err(e) => {
                    cleanup_cgroup(&ctx.cg_dir);
                    return Err(io::Error::other(e));
                }
            },
        }
    }

    let status = reaped.expect("loop exits only after reap");
    let oom_after = read_oom_kills(&ctx.cg_dir);
    cleanup_cgroup(&ctx.cg_dir);

    tracing::debug!(
        elapsed_ms = ctx.started.elapsed().as_millis() as u64,
        stdout_lines = router.counts.0,
        stderr_lines = router.counts.1,
        "sandboxed job finished"
    );

    Ok(classify(
        status,
        ctx.limits,
        oom_after > ctx.oom_before,
        killed_on_timeout,
        ctx.sink.as_ref(),
    ))
}

fn classify(
    status: WaitStatus,
    limits: Limits,
    oom: bool,
    killed_on_timeout: bool,
    sink: &dyn Sink,
) -> ExecOutcome {
    match status {
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
            if sig == Signal::SIGKILL && oom {
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
    }
}

fn neg_pid(pid: nix::unistd::Pid) -> nix::unistd::Pid {
    nix::unistd::Pid::from_raw(-pid.as_raw())
}

fn prepare_cgroup(limits: &Limits, job_key: &str) -> io::Result<PathBuf> {
    let cgroup_root = Path::new("/sys/fs/cgroup");
    let base = cgroup_root.join("coop-jobs");

    fs::create_dir_all(&base)
        .map_err(|e| io::Error::other(format!("cgroup: create {}: {e}", base.display())))?;

    enable_controllers_for(cgroup_root);
    enable_controllers_for(&base);

    let dir = base.join(format!("job-{job_key}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)
        .map_err(|e| io::Error::other(format!("cgroup: create {}: {e}", dir.display())))?;

    let mem_bytes = limits.mem_mb as u64 * 1024 * 1024;
    fs::write(dir.join("memory.max"), mem_bytes.to_string())
        .map_err(|e| io::Error::other(format!("cgroup: write memory.max: {e}")))?;
    if let Err(e) = fs::write(dir.join("memory.swap.max"), "0") {
        tracing::debug!(error = %e, "memory.swap.max not available; continuing");
    }
    fs::write(
        dir.join("cpu.max"),
        format!("{} 1000000", limits.cpu_seconds as u64 * 1_000_000),
    )
    .map_err(|e| io::Error::other(format!("cgroup: write cpu.max: {e}")))?;
    fs::write(dir.join("pids.max"), limits.max_pids.to_string())
        .map_err(|e| io::Error::other(format!("cgroup: write pids.max: {e}")))?;

    Ok(dir)
}

const NEEDED_CONTROLLERS: [&str; 3] = ["memory", "pids", "cpu"];

fn enable_controllers_for(root: &Path) {
    let controllers_path = root.join("cgroup.controllers");

    let Ok(available) = fs::read_to_string(&controllers_path) else {
        tracing::debug!(
            path = %controllers_path.display(),
            "cannot read available controllers"
        );
        return;
    };

    let subtree = root.join("cgroup.subtree_control");
    let enabled = fs::read_to_string(&subtree).unwrap_or_default();

    let mut tokens = String::new();
    for controller in NEEDED_CONTROLLERS {
        if available.split_whitespace().any(|c| c == controller)
            && !enabled.split_whitespace().any(|c| c == controller)
        {
            tokens.push_str(&format!("+{controller} "));
        }
    }

    if tokens.is_empty() {
        return;
    }

    match fs::write(&subtree, tokens.trim()) {
        Ok(_) => tracing::debug!(
            path = %subtree.display(),
            tokens = tokens.trim(),
            "enabled controllers in subtree_control"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            path = %subtree.display(),
            "could not enable controllers (EBUSY means processes sit directly \
             in this group — move them into a child scope or use systemd delegation)"
        ),
    }
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
