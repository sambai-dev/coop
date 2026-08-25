use crate::bus::WireEvent;
use crate::AppState;
use coop_exec::{ExecContext, Sink, Stream};
use coop_types::{JobSpec, JobStatus};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Semaphore};

enum Op {
    Started,
    Output(Stream, String),
    Violation(&'static str, Value),
    Truncated(Stream),
    Finished {
        status: &'static str,
        exit_code: Option<i32>,
        duration_ms: i64,
    },
}

struct JobSink {
    tx: mpsc::UnboundedSender<Op>,
}

impl Sink for JobSink {
    fn output(&self, stream: Stream, line: String) {
        let _ = self.tx.send(Op::Output(stream, line));
    }

    fn violation(&self, rule: &'static str, detail: Value) {
        let _ = self.tx.send(Op::Violation(rule, detail));
    }

    fn truncated(&self, stream: Stream) {
        let _ = self.tx.send(Op::Truncated(stream));
    }
}

/// RAII guard for the per-job cancellation flag (deep-hunt #4). Every early
/// return after the flag is registered must remove it, otherwise the map and
/// the `coop_running_jobs` gauge grow forever. The guard is disarmed by
/// forgetting or dropping after an explicit remove in the normal path.
struct CancelGuard {
    map: Arc<dashmap::DashMap<String, Arc<std::sync::atomic::AtomicBool>>>,
    job_id: String,
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.map.remove(&self.job_id);
    }
}

pub fn spawn_workers(state: AppState, rx: mpsc::Receiver<String>) {
    let rx = Arc::new(tokio::sync::Mutex::new(rx));
    for worker_id in 0..state.cfg.workers {
        let state = state.clone();
        let rx = rx.clone();
        tokio::spawn(async move {
            loop {
                let job_id = {
                    let mut guard = rx.lock().await;
                    guard.recv().await
                };
                let Some(job_id) = job_id else { break };
                handle_job(state.clone(), job_id, worker_id).await;
            }
        });
    }
}

#[tracing::instrument(name = "job", skip_all, fields(job_id = %job_id))]
async fn handle_job(state: AppState, job_id: String, worker_id: usize) {
    let row = match state.store.get_job(&job_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            tracing::warn!("queued job vanished from store");
            return;
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to load queued job");
            return;
        }
    };

    // A cancelled-while-queued job arrives here after the DELETE endpoint
    // already finalized it. Skip silently; do not consume a worker slot.
    if let Some(status) = JobStatus::parse(&row.status) {
        if status.is_terminal() {
            return;
        }
    }

    let spec: JobSpec = match serde_json::from_str(&row.spec_json) {
        Ok(spec) => spec,
        Err(e) => {
            let _ = state.store.finish(&job_id, "error", None).await;
            tracing::error!(error = %e, "stored job spec is invalid");
            return;
        }
    };

    let semaphore = semaphore_for(&state, &row.tenant);
    let permit = match semaphore.acquire_owned().await {
        Ok(p) => p,
        Err(_) => return,
    };

    // Deep-hunt fix (#3): conditional transition so a concurrent cancel
    // cannot be overwritten. The old `set_started` was unconditional and
    // turned a just-cancelled row back to `running`.
    match state.store.set_started_if_queued(&job_id).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::info!(job_id = %job_id, "job was cancelled while queued — skipping execution");
            return;
        }
        Err(e) => {
            tracing::error!(error = %e, job_id = %job_id, "could not mark job started");
            return;
        }
    }

    let (op_tx, op_rx) = mpsc::unbounded_channel::<Op>();
    let pump = tokio::spawn(pump_events(state.clone(), job_id.clone(), op_rx));

    op_tx.send(Op::Started).ok();

    // Cancellation: register a flag for the running job; the DELETE endpoint
    // flips it and the executor's poll loop (20 ms tick) acts on it. The
    // entry API keeps a concurrently-installed cancellation (`true`) intact;
    // the `CancelGuard` guarantees removal on *every* exit path (including
    // workdir creation failure and panics in `execute`), fixing the leak
    // class F-003 called out in the audit.
    let cancel_flag = state
        .cancels
        .entry(job_id.clone())
        .or_insert_with(|| Arc::new(std::sync::atomic::AtomicBool::new(false)))
        .clone();
    let _cancel_guard = CancelGuard {
        map: Arc::clone(&state.cancels),
        job_id: job_id.clone(),
    };

    let workdir = std::path::PathBuf::from(&state.cfg.jobs_root).join(format!("job-{job_id}"));
    // N-1: workdirs hold tenant source and artifacts; mode 0700 (no-op off
    // unix) keeps sibling jobs from traversing each other's directories.
    // The jobs root itself is locked to 0700 on every job (not only at boot
    // from main) so the isolation invariant holds no matter which entry point
    // created it — a test harness, a CLI, or an operator's mkdir.
    if let Err(e) = tokio::fs::create_dir_all(&workdir)
        .await
        .and_then(|()| coop_exec::owner_only_dir(&workdir))
        .and_then(|()| coop_exec::owner_only_dir(std::path::Path::new(&state.cfg.jobs_root)))
    {
        tracing::error!(error = %e, "failed to create workdir");
        op_tx
            .send(Op::Violation("executor_error", executor_error_detail(&e)))
            .ok();
        finish_via(op_tx, "error", None, 0);
        pump.await.ok();
        return;
    }

    let ctx = ExecContext {
        job_key: short_key(&job_id),
        language: spec.language.clone(),
        code: spec.code,
        stdin: spec.stdin,
        limits: spec.limits.clamped(),
        workdir: workdir.clone(),
        interpreter_override: state.cfg.interpreter_override(&spec.language),
        cancel: Some(cancel_flag),
        seccomp: state.seccomp,
    };

    tracing::info!(
        worker = worker_id,
        tenant = %row.tenant,
        language = %spec.language,
        sandbox = coop_exec::SandboxMode::as_str(state.sandbox_mode),
        wall_seconds = ctx.limits.wall_seconds,
        mem_mb = ctx.limits.mem_mb,
        "executing job"
    );

    let started_at = std::time::Instant::now();
    let result = coop_exec::execute(
        ctx,
        Arc::new(JobSink { tx: op_tx.clone() }),
        state.sandbox_mode,
    )
    .await;

    match result {
        Ok(outcome) => {
            let status = coop_types::JobStatus::from(outcome.status);
            finish_via(
                op_tx,
                status.as_str(),
                outcome.exit_code,
                started_at.elapsed().as_millis() as i64,
            );
            tracing::info!(
                status = status.as_str(),
                exit_code = outcome.exit_code,
                killed_by = outcome.killed_by.as_deref().unwrap_or(""),
                duration_ms = started_at.elapsed().as_millis() as u64,
                "job finished"
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "executor failure");
            op_tx
                .send(Op::Violation("executor_error", executor_error_detail(&e)))
                .ok();
            finish_via(
                op_tx,
                "error",
                None,
                started_at.elapsed().as_millis() as i64,
            );
        }
    }

    drop(permit);
    let _ = tokio::fs::remove_dir_all(&workdir).await;
    state.cancels.remove(&job_id);
    pump.await.ok();
}

/// N6: tenant-visible executor errors are reduced to a coarse, generic code.
/// Raw io::Error text can name interpreter paths and cgroup/jobs_root
/// topology, so it stays in server-side tracing only (see the callers).
fn executor_error_detail(e: &std::io::Error) -> Value {
    let code = match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
            "executor_unavailable"
        }
        _ => "executor_failure",
    };
    json!({ "code": code })
}

fn finish_via(
    tx: mpsc::UnboundedSender<Op>,
    status: &'static str,
    exit_code: Option<i32>,
    duration_ms: i64,
) {
    tx.send(Op::Finished {
        status,
        exit_code,
        duration_ms,
    })
    .ok();
    drop(tx);
}

/// F-009 retention: periodically delete terminal jobs older than
/// `retention_hours` together with their events. A no-op when retention is
/// disabled (0). Errors are logged and swept again next interval — a failed
/// sweep must never take the server down.
pub fn spawn_retention_sweeper(state: AppState) {
    if state.cfg.retention_hours == 0 {
        tracing::info!("retention disabled (COOP_RETENTION_HOURS=0); event log grows unbounded");
        return;
    }
    let max_age_ms = (state.cfg.retention_hours * 3600 * 1000) as i64;
    let interval = Duration::from_secs(state.cfg.sweep_interval_secs);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // first tick fires immediately; skip it
        loop {
            ticker.tick().await;
            match state.store.prune_older_than(max_age_ms).await {
                Ok((jobs, events)) if jobs > 0 => {
                    tracing::info!(
                        jobs_deleted = jobs,
                        events_deleted = events,
                        "retention sweep pruned expired jobs"
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "retention sweep failed"),
            }
        }
    });
}

fn short_key(job_id: &str) -> String {
    job_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn semaphore_for(state: &AppState, tenant: &str) -> Arc<Semaphore> {
    state
        .tenant_sems
        .entry(tenant.to_string())
        .or_insert_with(|| Arc::new(Semaphore::new(state.cfg.tenant_concurrency)))
        .clone()
}

async fn pump_events(state: AppState, job_id: String, mut rx: mpsc::UnboundedReceiver<Op>) {
    let mut seq = state.store.last_seq(&job_id).await.unwrap_or(0);

    while let Some(op) = rx.recv().await {
        let (kind, data): (&str, Value) = match op {
            Op::Started => ("started", json!({})),
            Op::Output(stream, line) => (stream.as_str(), json!({ "line": line })),
            Op::Violation(rule, detail) => ("violation", json!({ "rule": rule, "detail": detail })),
            Op::Truncated(stream) => (
                "truncated",
                json!({ "stream": stream.as_str(), "max_lines": coop_types::MAX_OUTPUT_LINES }),
            ),
            Op::Finished {
                status,
                exit_code,
                duration_ms,
            } => (
                "finished",
                json!({ "status": status, "exit_code": exit_code, "duration_ms": duration_ms }),
            ),
        };

        seq += 1;
        match state.store.append_event(&job_id, kind, &data).await {
            Ok((event_seq, ts_ms)) => {
                seq = event_seq.max(seq);
                state.bus.send(
                    &job_id,
                    WireEvent {
                        seq,
                        ts_ms,
                        kind: kind.to_string(),
                        data: data.clone(),
                    },
                );
            }
            Err(e) => {
                tracing::error!(error = %e, kind, "failed to persist job event");
                continue;
            }
        }

        if kind == "finished" {
            let status = data["status"].as_str().unwrap_or("error").to_string();
            let exit_code = data["exit_code"].as_i64().map(|v| v as i32);
            if let Err(e) = state.store.finish(&job_id, &status, exit_code).await {
                tracing::error!(error = %e, "failed to finalize job row");
            }
            state.bus.remove(&job_id);
        }
    }
}
