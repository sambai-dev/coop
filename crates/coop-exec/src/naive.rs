use crate::{ext_for, resolve_interpreter, ExecContext, ExecOutcome, Sink, Stream};
use coop_types::{OutcomeStatus, MAX_OUTPUT_LINES};
use std::io;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::Duration;

pub async fn run(ctx: ExecContext, sink: Arc<dyn Sink>) -> io::Result<ExecOutcome> {
    let src = ctx.workdir.join(format!("job.{}", ext_for(&ctx.language)));
    crate::write_private_file(&src, ctx.code.as_bytes())?;

    let interp = resolve_interpreter(&ctx.language, ctx.interpreter_override.as_deref());
    let mut cmd = Command::new(interp);
    cmd.current_dir(&ctx.workdir).arg(&src);
    // N-2: make the child a process-group leader so a group-wide SIGKILL can
    // reap background descendants that would otherwise outlive the job while
    // holding our output pipes open.
    #[cfg(unix)]
    cmd.process_group(0);
    cmd.env_clear();
    cmd.env(
        "PATH",
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
    );
    cmd.env("HOME", "/tmp/home");
    cmd.env("TMPDIR", "/tmp");
    cmd.env("LANG", "C.UTF-8");
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if ctx.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn()?;
    // Freshly spawned, so the pid is always present; the child is its own
    // process-group leader on unix (pgid == pid).
    let child_pid = child.id().expect("freshly spawned child has a pid");

    // Deep-hunt fix (worker-wedge DoS): never await the stdin transfer on the
    // supervision path. A child that never drains its pipe (e.g. a busy loop
    // that ignores stdin) would previously park `write_all` *before* any
    // wall-clock/cancel logic existed, wedging the worker task permanently
    // while the job sat in `running` forever. The transfer now runs on its
    // own task; when supervision kills the process group the pipe closes and
    // the pending write errors out, so timeouts stay authoritative.
    if let Some(mut si) = child.stdin.take() {
        let input = ctx.stdin.clone().unwrap_or_default();
        tokio::spawn(async move {
            let _ = si.write_all(input.as_bytes()).await;
            let _ = si.shutdown().await;
        });
    }

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let (tx, mut rx) = mpsc::unbounded_channel::<(Stream, String)>();
    spawn_reader(stdout, Stream::Stdout, tx.clone());
    spawn_reader(stderr, Stream::Stderr, tx);

    // N-2: parity with the linux_sandbox supervise loop. A bare kill of the
    // direct child leaves background descendants alive; they inherit the
    // output pipes, so the drain below never sees EOF and the worker is
    // pinned forever. Reap the leader first, SIGKILL the whole group, then
    // bound the remaining drain by the wall budget with a small grace.
    const DRAIN_GRACE: Duration = Duration::from_secs(2);
    let wall = Duration::from_secs(ctx.limits.wall_seconds.max(1) as u64);
    let started = std::time::Instant::now();
    let wall_deadline = started + wall;

    let mut counts = (0usize, 0usize);
    // Per-stream truncation flags: each stream emits exactly one `truncated`
    // event when it crosses the line cap, independent of the other stream.
    let mut truncated = (false, false);
    let mut timed_out = false;
    let mut cancelled = false;
    let mut killed_on_timeout = false;
    let mut reaped: Option<std::process::ExitStatus> = None;
    let mut rx_closed = false;
    let mut drain_deadline: Option<tokio::time::Instant> = None;

    while reaped.is_none() || !rx_closed {
        let drain_guard = async {
            match drain_deadline {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            biased;

            _ = drain_guard => {
                tracing::warn!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "stream drain cut off: wall budget exhausted after reap"
                );
                rx_closed = true;
            }

            item = recv_output(&mut rx, rx_closed) => match item {
                Some((stream, line)) => {
                    let (count, truncated) = match stream {
                        Stream::Stdout => (&mut counts.0, &mut truncated.0),
                        Stream::Stderr => (&mut counts.1, &mut truncated.1),
                    };
                    if *count < MAX_OUTPUT_LINES {
                        *count += 1;
                        sink.output(stream, line);
                    } else if !*truncated {
                        *truncated = true;
                        sink.truncated(stream);
                    }
                }
                None => rx_closed = true,
            },

            polled = poll_status(&mut child, reaped.is_some()) => match polled.expect("not pending") {
                Ok(Some(status)) => {
                    reaped = Some(status);
                    kill_process_group(child_pid);
                    let _ = child.start_kill();
                    drain_deadline = Some(
                        tokio::time::Instant::from_std(wall_deadline)
                            .max(tokio::time::Instant::now() + DRAIN_GRACE),
                    );
                }
                Ok(None) => {
                    // Cancellation beats the wall clock: kill the whole group
                    // immediately and classify the run as cancelled.
                    if ctx.is_cancelled() && !killed_on_timeout {
                        killed_on_timeout = true;
                        cancelled = true;
                        sink.violation("job_cancelled", serde_json::json!({
                            "wall_seconds": ctx.limits.wall_seconds
                        }));
                        kill_process_group(child_pid);
                        let _ = child.start_kill();
                    } else if !killed_on_timeout && std::time::Instant::now() >= wall_deadline {
                        killed_on_timeout = true;
                        timed_out = true;
                        sink.violation("wall_clock_exceeded", serde_json::json!({
                            "wall_seconds": ctx.limits.wall_seconds
                        }));
                        kill_process_group(child_pid);
                        let _ = child.start_kill();
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => return Err(e),
            },
        }
    }

    let status = match reaped {
        Some(status) => status,
        None => child.wait().await?,
    };
    if timed_out {
        return Ok(ExecOutcome {
            status: OutcomeStatus::TimedOut,
            exit_code: status.code(),
            killed_by: Some("wall-clock".into()),
        });
    }
    if cancelled {
        return Ok(ExecOutcome {
            status: OutcomeStatus::Cancelled,
            exit_code: status.code().or(unix_signal(&status)),
            killed_by: Some("cancelled".into()),
        });
    }
    Ok(classify(status.code(), unix_signal(&status), false))
}

/// N-2: once the channel is closed (`done`), park forever instead of spinning
/// on the `None` that `recv` would otherwise return immediately.
async fn recv_output(
    rx: &mut mpsc::UnboundedReceiver<(Stream, String)>,
    done: bool,
) -> Option<(Stream, String)> {
    if done {
        std::future::pending::<()>().await;
        None
    } else {
        rx.recv().await
    }
}

async fn poll_status(
    child: &mut tokio::process::Child,
    reaped: bool,
) -> Option<io::Result<Option<std::process::ExitStatus>>> {
    if reaped {
        std::future::pending::<()>().await;
        None
    } else {
        Some(child.try_wait())
    }
}

/// N-2: negative pid targets the whole process group — the leader plus every
/// descendant that inherited it. Signal delivery cannot fail meaningfully
/// here (the group exists while the leader lives), so the result is ignored.
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {}

fn classify(code: Option<i32>, signal: Option<i32>, timed_out: bool) -> ExecOutcome {
    if timed_out {
        return ExecOutcome {
            status: OutcomeStatus::TimedOut,
            exit_code: code,
            killed_by: Some("wall-clock".into()),
        };
    }
    match (code, signal) {
        (Some(0), _) => ExecOutcome {
            status: OutcomeStatus::Succeeded,
            exit_code: Some(0),
            killed_by: None,
        },
        (Some(c), _) => ExecOutcome {
            status: OutcomeStatus::Failed,
            exit_code: Some(c),
            killed_by: None,
        },
        (None, Some(sig)) => ExecOutcome {
            status: OutcomeStatus::Failed,
            exit_code: None,
            killed_by: Some(format!("signal-{sig}")),
        },
        (None, None) => ExecOutcome {
            status: OutcomeStatus::Failed,
            exit_code: None,
            killed_by: None,
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

fn spawn_reader<S>(reader: S, stream: Stream, tx: mpsc::UnboundedSender<(Stream, String)>)
where
    S: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send((stream, line)).is_err() {
                break;
            }
        }
    });
}
