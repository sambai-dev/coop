use crate::bus::WireEvent;
use crate::AppState;
use coop_exec::{ExecContext, Sink, Stream};
use coop_types::JobSpec;
use serde_json::{json, Value};
use std::sync::Arc;
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

    if let Err(e) = state.store.set_started(&job_id).await {
        tracing::error!(error = %e, "could not mark job started");
    }

    let (op_tx, op_rx) = mpsc::unbounded_channel::<Op>();
    let pump = tokio::spawn(pump_events(state.clone(), job_id.clone(), op_rx));

    op_tx.send(Op::Started).ok();

    let workdir = std::path::PathBuf::from(&state.cfg.jobs_root).join(format!("job-{job_id}"));
    if tokio::fs::create_dir_all(&workdir).await.is_err() {
        op_tx
            .send(Op::Violation(
                "executor_error",
                json!({ "message": "failed to create workdir" }),
            ))
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
                .send(Op::Violation(
                    "executor_error",
                    json!({ "message": e.to_string() }),
                ))
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
    pump.await.ok();
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
