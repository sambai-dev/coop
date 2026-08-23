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
    tokio::fs::write(&src, &ctx.code).await?;

    let interp = resolve_interpreter(&ctx.language, ctx.interpreter_override.as_deref());
    let mut cmd = Command::new(interp);
    cmd.current_dir(&ctx.workdir).arg(&src);
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

    if let Some(input) = ctx.stdin.clone() {
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(input.as_bytes()).await;
            let _ = si.shutdown().await;
        }
    }

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let (tx, mut rx) = mpsc::unbounded_channel::<(Stream, String)>();
    spawn_reader(stdout, Stream::Stdout, tx.clone());
    spawn_reader(stderr, Stream::Stderr, tx);

    let wall = Duration::from_secs(ctx.limits.wall_seconds.max(1) as u64);
    let deadline = tokio::time::sleep(wall);
    tokio::pin!(deadline);

    let mut counts = (0usize, 0usize);
    let mut truncated = false;
    let mut timed_out = false;

    loop {
        tokio::select! {
            item = rx.recv() => match item {
                Some((stream, line)) => {
                    let count = match stream {
                        Stream::Stdout => &mut counts.0,
                        Stream::Stderr => &mut counts.1,
                    };
                    if *count < MAX_OUTPUT_LINES {
                        *count += 1;
                        sink.output(stream, line);
                    } else if !truncated {
                        truncated = true;
                        sink.truncated(stream);
                    }
                }
                None => break,
            },
            _ = &mut deadline, if !timed_out => {
                timed_out = true;
                sink.violation("wall_clock_exceeded", serde_json::json!({
                    "wall_seconds": ctx.limits.wall_seconds
                }));
                let _ = child.start_kill();
            }
        }
    }

    let status = child.wait().await?;
    Ok(classify(status.code(), unix_signal(&status), timed_out))
}

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
