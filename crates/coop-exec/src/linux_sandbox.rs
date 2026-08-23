use crate::{ext_for, resolve_interpreter, ExecContext, ExecOutcome, Sink, Stream};
use coop_types::{Limits, OutcomeStatus, MAX_OUTPUT_LINES};
use nix::fcntl::{fcntl, FcntlArg, FdFlag};
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

// F5: bootstrap stages reported to the parent over the setup socket when the
// child cannot finish preparing its sandbox. These strings are safe for
// tenant-visible violation events; errno detail stays in tracing.
const STAGE_NAMESPACES: &str = "entering kernel namespaces";
const STAGE_ID_MAP: &str = "mapping sandbox user ids";
const STAGE_ROOT_MOUNTS: &str = "preparing read-only root mounts";
const STAGE_TMP_MOUNT: &str = "mounting private tmp";
const STAGE_CGROUP_ATTACH: &str = "attaching job cgroup";
const STAGE_EXEC: &str = "starting interpreter";

const DRAIN_GRACE: Duration = Duration::from_secs(2);

pub async fn run(ctx: ExecContext, sink: Arc<dyn Sink>) -> io::Result<ExecOutcome> {
    // N7: arm a guard so every error path before supervision starts (source
    // write, interpreter resolution, CString/socket setup, fork) removes the
    // fresh cgroup instead of orphaning a directory with live knobs on every
    // failed submit. `disarm` hands ownership to `supervise`, which cleans up
    // from then on.
    let cg_guard = CgroupGuard::new(prepare_cgroup(&ctx.limits, &ctx.job_key)?);
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
    let cg_dir_c = CString::new(cg_guard.path().as_os_str().as_bytes())?;
    let mem_bytes = ctx.limits.mem_mb as u64 * 1024 * 1024;
    let oom_before = read_oom_kills(cg_guard.path());

    let (out_parent, out_child) = UnixStream::pair().map_err(io::Error::other)?;
    let (err_parent, err_child) = UnixStream::pair().map_err(io::Error::other)?;

    let out_r = out_parent.into_raw_fd();
    let err_r = err_parent.into_raw_fd();
    let out_w = out_child.into_raw_fd();
    let err_w = err_child.into_raw_fd();

    // F5: a dedicated channel so a child that cannot finish bootstrapping its
    // sandbox reports *why* instead of collapsing into a bare exit(126). The
    // write end is close-on-exec, so the parent reads EOF exactly when execve
    // succeeds; any line that arrives means setup failed before exec.
    let (setup_parent, setup_child) = UnixStream::pair().map_err(io::Error::other)?;
    fcntl(
        setup_child.as_raw_fd(),
        FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC),
    )
    .map_err(io::Error::other)?;
    let setup_r = setup_parent.into_raw_fd();
    let setup_w = setup_child.into_raw_fd();

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
        setup_r,
        setup_w,
        // F5: without an owning user namespace, unshare(CLONE_NEWNS|…) is
        // EPERM for unprivileged users and every job silently died at 126.
        unpriv_userns: !Uid::effective().is_root(),
        euid: Uid::effective().as_raw(),
        egid: Gid::effective().as_raw(),
    };

    let fork_result = unsafe { fork() }.map_err(io::Error::other)?;

    match fork_result {
        ForkResult::Child => child_setup(&plan),
        ForkResult::Parent { child } => {
            let _ = close(plan.out_w);
            let _ = close(plan.err_w);
            let _ = close(plan.setup_w);

            let out_std = unsafe { UnixStream::from_raw_fd(plan.out_r) };
            let err_std = unsafe { UnixStream::from_raw_fd(plan.err_r) };
            out_std.set_nonblocking(true)?;
            err_std.set_nonblocking(true)?;
            let setup_std = unsafe { UnixStream::from_raw_fd(plan.setup_r) };
            setup_std.set_nonblocking(true)?;
            let out_tokio = tokio::net::UnixStream::from_std(out_std)?;
            let err_tokio = tokio::net::UnixStream::from_std(err_std)?;
            let setup_tokio = tokio::net::UnixStream::from_std(setup_std)?;

            let sctx = SuperviseCtx {
                child,
                sink,
                cg_dir: cg_guard.disarm(),
                limits: ctx.limits,
                oom_before,
                started,
                setup_lines: BufReader::new(setup_tokio).lines(),
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
    setup_r: i32,
    setup_w: i32,
    unpriv_userns: bool,
    euid: u32,
    egid: u32,
}

/// Report a sandbox-bootstrap failure to the parent (stage + errno detail)
/// and exit 126. The parent turns this into a violation event naming the
/// failing stage; the raw reason goes to server logs only.
fn report_setup_failure(setup_w: i32, stage: &str, reason: &str) -> ! {
    use std::io::Write;
    let mut f = unsafe { fs::File::from_raw_fd(setup_w) };
    let _ = writeln!(f, "{stage}\t{reason}");
    let _ = f.flush();
    std::process::exit(126);
}

/// F5: a fresh user namespace grants its creator full capabilities *inside
/// it*, which is what makes the mount/pid/net/ipc/uts namespaces reachable
/// without root. Only the invoking user's own effective id can be mapped
/// without CAP_SETUID in the parent namespace, and setgroups must be denied
/// before gid_map (user_namespaces(7)).
fn write_own_id_maps(uid: u32, gid: u32) -> io::Result<()> {
    fs::write("/proc/self/setgroups", b"deny\n")?;
    fs::write("/proc/self/uid_map", format!("0 {uid} 1\n"))?;
    fs::write("/proc/self/gid_map", format!("0 {gid} 1\n"))?;
    Ok(())
}

fn child_setup(plan: &ChildPlan) -> ! {
    let _ = close(plan.out_r);
    let _ = close(plan.err_r);
    let _ = close(plan.setup_r);

    let _ = setsid();

    // F5: join CLONE_NEWUSER for unprivileged hosts; the kernel creates the
    // user namespace first, so the capability checks for the rest of the set
    // pass against it.
    let mut ns_flags = CloneFlags::CLONE_NEWNS
        | CloneFlags::CLONE_NEWPID
        | CloneFlags::CLONE_NEWNET
        | CloneFlags::CLONE_NEWIPC
        | CloneFlags::CLONE_NEWUTS;
    if plan.unpriv_userns {
        ns_flags |= CloneFlags::CLONE_NEWUSER;
    }
    if let Err(e) = unshare(ns_flags) {
        report_setup_failure(plan.setup_w, STAGE_NAMESPACES, &e.to_string());
    }

    if plan.unpriv_userns {
        if let Err(e) = write_own_id_maps(plan.euid, plan.egid) {
            report_setup_failure(plan.setup_w, STAGE_ID_MAP, &e.to_string());
        }
    }

    let _ =
        mount::<str, str, str, str>(None, "/", None, MsFlags::MS_REC | MsFlags::MS_PRIVATE, None);
    if let Err(e) = mount::<str, str, str, str>(
        Some("/"),
        "/",
        None,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None,
    ) {
        report_setup_failure(plan.setup_w, STAGE_ROOT_MOUNTS, &e.to_string());
    }
    if let Err(e) = mount::<str, str, str, str>(
        Some("/"),
        "/",
        None,
        MsFlags::MS_BIND
            | MsFlags::MS_REMOUNT
            | MsFlags::MS_RDONLY
            | MsFlags::MS_NOSUID
            | MsFlags::MS_NODEV,
        None,
    ) {
        report_setup_failure(plan.setup_w, STAGE_ROOT_MOUNTS, &e.to_string());
    }
    let tmp_opts = format!("size={},mode=1777", plan.mem_bytes);
    if let Err(e) = mount::<str, str, str, str>(
        None,
        "/tmp",
        Some("tmpfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
        Some(tmp_opts.as_str()),
    ) {
        report_setup_failure(plan.setup_w, STAGE_TMP_MOUNT, &e.to_string());
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

    if let Err(e) = fs::write(
        format!("{}/cgroup.procs", plan.cg_dir.to_string_lossy()),
        std::process::id().to_string(),
    ) {
        report_setup_failure(plan.setup_w, STAGE_CGROUP_ATTACH, &e.to_string());
    }

    // Inside a user namespace we are uid 0 mapped back to the invoking host
    // user; nobody (65534) cannot be mapped without CAP_SETUID in the parent
    // namespace, so those credentials are kept instead.
    if !plan.unpriv_userns && Uid::effective().is_root() {
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
    // nix 0.29 returns Result<Infallible, Errno>: execve only ever comes back
    // on failure, so bind the errno and report it (this diverges).
    let Err(e) = execve(&plan.prog, &argv_refs, &envp_refs);
    report_setup_failure(plan.setup_w, STAGE_EXEC, &e.to_string());
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
    setup_lines: Lines<BufReader<tokio::net::UnixStream>>,
}

/// F5: what the child reported over the setup socket. `stage` is a canonical,
/// tenant-safe description of the failing bootstrap step; `reason` is the raw
/// errno text and stays in server logs.
#[derive(Debug)]
struct BootstrapFailure {
    stage: &'static str,
    reason: String,
}

impl BootstrapFailure {
    /// Wire format: "<stage>\t<errno text>". Stages are canonicalized so only
    /// known, sanitized descriptions can reach tenant events.
    fn parse(line: &str) -> Option<Self> {
        let (stage, reason) = line.split_once('\t')?;
        let stage = match stage {
            STAGE_NAMESPACES => STAGE_NAMESPACES,
            STAGE_ID_MAP => STAGE_ID_MAP,
            STAGE_ROOT_MOUNTS => STAGE_ROOT_MOUNTS,
            STAGE_TMP_MOUNT => STAGE_TMP_MOUNT,
            STAGE_CGROUP_ATTACH => STAGE_CGROUP_ATTACH,
            STAGE_EXEC => STAGE_EXEC,
            _ => "initializing sandbox",
        };
        Some(Self {
            stage,
            reason: reason.trim_end().to_string(),
        })
    }
}

async fn supervise(
    mut ctx: SuperviseCtx,
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
    let mut setup_done = false;
    let mut bootstrap_failure: Option<BootstrapFailure> = None;
    let mut drain_deadline: Option<Instant> = None;

    while reaped.is_none() || !out_done || !err_done || !setup_done {
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

            note = next_line(&mut ctx.setup_lines, setup_done) => match note {
                Some(Ok(Some(text))) => {
                    // The child holds this socket open only until it execs or
                    // dies; a line here always means bootstrap failed.
                    bootstrap_failure = BootstrapFailure::parse(&text).or(bootstrap_failure);
                }
                Some(Ok(None)) | Some(Err(_)) | None => setup_done = true,
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

    // F5: a 126 used to be a silent collapse. If the child reported a stage,
    // emit a violation naming it and fail the job with an error instead.
    if let (WaitStatus::Exited(_, 126), Some(failure)) = (status, bootstrap_failure) {
        tracing::error!(
            stage = failure.stage,
            reason = %failure.reason,
            "sandbox bootstrap failed"
        );
        ctx.sink.violation(
            "sandbox_bootstrap_failed",
            json!({ "stage": failure.stage }),
        );
        return Err(io::Error::other(format!(
            "sandbox bootstrap failed during '{}' ({})",
            failure.stage, failure.reason
        )));
    }

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

    // F4: the old code ran `remove_dir_all` followed by `create_dir_all` here.
    // That pair is racy and unsafe: two same-key jobs could interleave
    // remove/create, and the remove could delete the cgroup of a *live* job
    // that had just been handed this path. `create_dir` is a single atomic
    // mkdir(2) that fails with `AlreadyExists` instead, so an existing
    // directory is never deleted blindly — recovery lives in
    // `create_cgroup_dir`.
    let dir = create_cgroup_dir(&base, job_key)?;

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

/// Create the per-job cgroup directory atomically (F4). `fs::create_dir` is
/// one exclusive mkdir(2): if `job-{job_key}` already exists we must decide
/// whether it is a stale leftover from a crashed job or a *live* job's cgroup,
/// which must never be deleted.
fn create_cgroup_dir(base: &Path, job_key: &str) -> io::Result<PathBuf> {
    let dir = base.join(format!("job-{job_key}"));
    match fs::create_dir(&dir) {
        Ok(()) => Ok(dir),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if cgroup_has_processes(&dir) {
                // A live job owns this cgroup — never delete it; take a fresh
                // unique name instead.
                let fresh = base.join(format!(
                    "job-{job_key}-{}-{:x}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or_default()
                ));
                fs::create_dir(&fresh).map_err(|e| {
                    io::Error::other(format!("cgroup: create {}: {e}", fresh.display()))
                })?;
                Ok(fresh)
            } else {
                // No member processes: stale cgroup left behind by a crashed
                // job, safe to reclaim. The kernel backstops this — rmdir(2)
                // on a populated cgroup fails with EBUSY, so a live group
                // cannot be removed even if one races in after the check.
                let _ = fs::remove_dir_all(&dir);
                fs::create_dir(&dir).map_err(|e| {
                    io::Error::other(format!("cgroup: create {}: {e}", dir.display()))
                })?;
                Ok(dir)
            }
        }
        Err(e) => Err(io::Error::other(format!(
            "cgroup: create {}: {e}",
            dir.display()
        ))),
    }
}

/// Conservative liveness probe: an unreadable `cgroup.procs` counts as live.
fn cgroup_has_processes(dir: &Path) -> bool {
    match fs::read_to_string(dir.join("cgroup.procs")) {
        Ok(procs) => procs.split_whitespace().next().is_some(),
        Err(_) => true,
    }
}

/// RAII guard for the per-job cgroup directory (N7): every `?` between
/// `prepare_cgroup` and `supervise` (source write, interpreter resolution,
/// CString/socket setup, fork) returns through this guard, so failed submits
/// no longer orphan cgroup directories with live knobs. `disarm` hands
/// ownership to the supervision path, which cleans up from then on.
struct CgroupGuard {
    dir: Option<PathBuf>,
}

impl CgroupGuard {
    fn new(dir: PathBuf) -> Self {
        Self { dir: Some(dir) }
    }

    fn path(&self) -> &Path {
        self.dir.as_deref().expect("cgroup guard is armed")
    }

    /// Consume the guard without cleaning up; the caller takes over cleanup.
    fn disarm(mut self) -> PathBuf {
        self.dir.take().expect("cgroup guard is armed")
    }
}

impl Drop for CgroupGuard {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            cleanup_cgroup(&dir);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand-in for `/sys/fs/cgroup/coop-jobs`: a plain temp dir, with a fake
    /// `cgroup.procs` file controlling the liveness probe.
    fn temp_base(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("coop-cg-{tag}-{}", std::process::id()));
        fs::create_dir_all(&base).expect("create temp cgroup base");
        base
    }

    #[test]
    fn fresh_job_key_uses_deterministic_dir() {
        let base = temp_base("fresh");
        let dir = create_cgroup_dir(&base, "abc").expect("create fresh cgroup dir");
        assert_eq!(dir, base.join("job-abc"));
        assert!(dir.is_dir());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn stale_dir_from_crashed_job_is_reclaimed() {
        let base = temp_base("stale");
        let stale = base.join("job-abc");
        fs::create_dir(&stale).expect("seed stale dir");
        fs::write(stale.join("cgroup.procs"), "").expect("empty procs = crashed job");

        let dir = create_cgroup_dir(&base, "abc").expect("reclaim stale cgroup dir");
        assert_eq!(dir, stale, "stale cgroup is removed and recreated in place");
        assert!(dir.is_dir());
        assert!(
            !dir.join("cgroup.procs").exists(),
            "reclaimed dir is fresh, not the crashed job's leftover"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn live_job_dir_is_never_deleted() {
        let base = temp_base("live");
        let live = base.join("job-abc");
        fs::create_dir(&live).expect("seed live dir");
        fs::write(live.join("cgroup.procs"), "1234\n").expect("populated procs = live job");

        let dir = create_cgroup_dir(&base, "abc").expect("fall back to a fresh dir");
        assert_ne!(
            dir, live,
            "a populated cgroup must not be deleted; a unique name is used instead"
        );
        assert!(dir.is_dir());
        assert_eq!(
            fs::read_to_string(live.join("cgroup.procs")).expect("live cgroup untouched"),
            "1234\n"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn guard_cleans_up_unless_disarmed() {
        let base = temp_base("guard");

        let doomed = create_cgroup_dir(&base, "doomed").expect("create dir");
        drop(CgroupGuard::new(doomed.clone()));
        assert!(!doomed.exists(), "armed guard removes the cgroup on drop");

        let kept = create_cgroup_dir(&base, "kept").expect("create dir");
        let owned = CgroupGuard::new(kept.clone()).disarm();
        assert_eq!(owned, kept);
        assert!(kept.is_dir(), "disarmed guard leaves the cgroup in place");

        let _ = fs::remove_dir_all(&base);
    }
}
