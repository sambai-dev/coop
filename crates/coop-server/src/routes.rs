use crate::auth::Tenant;
use crate::bus::WireEvent;
use crate::AppState;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coop_store::JobRow;
use coop_types::{JobSpec, JobStatus, SUPPORTED_LANGUAGES};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Serialize, ToSchema)]
pub struct SubmitResponse {
    pub job_id: String,
    pub status: String,
    pub stream_url: String,
    pub replay_url: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct JobView {
    pub job_id: String,
    pub tenant: String,
    pub language: String,
    pub status: String,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    pub exit_code: Option<i32>,
}

impl JobView {
    fn from_row(row: &JobRow) -> Self {
        Self {
            job_id: row.job_id.clone(),
            tenant: row.tenant.clone(),
            language: row.language.clone(),
            status: row.status.clone(),
            created_at_ms: row.created_at_ms,
            started_at_ms: row.started_at_ms,
            finished_at_ms: row.finished_at_ms,
            exit_code: row.exit_code,
        }
    }
}

/// One-call job outcome for agent tool loops: the terminal view plus stdout /
/// stderr folded out of the append-only event log server-side, so clients get
/// a ready-to-use string instead of replaying raw events themselves.
#[derive(Debug, Serialize, ToSchema)]
pub struct ResultView {
    pub job_id: String,
    pub status: String,
    pub exit_code: Option<i32>,
    #[schema(value_type = Option<i64>)]
    pub duration_ms: Option<i64>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    #[schema(value_type = Vec<Object>)]
    pub violations: Vec<serde_json::Value>,
}

/// True when the job exists AND belongs to `tenant`. Foreign jobs are
/// indistinguishable from missing ones (same IDOR posture as F-001).
async fn owns_job(state: &AppState, id: &str, tenant: &str) -> bool {
    matches!(
        state.store.get_job(id).await,
        Ok(Some(row)) if row.tenant == tenant
    )
}

/// Server-side wait policy for GET /v1/jobs/{id}/result.
const RESULT_DEFAULT_WAIT_SECONDS: u64 = 60;
const RESULT_MAX_WAIT_SECONDS: u64 = 300;
const RESULT_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/v1/jobs", post(submit).get(list_jobs))
        .route("/v1/jobs/{id}", get(get_job).delete(cancel_job))
        .route("/v1/jobs/{id}/replay", get(replay))
        .route("/v1/jobs/{id}/result", get(job_result))
        .route("/v1/jobs/{id}/stream", get(stream))
        .route("/v1/metrics", get(metrics))
        .route("/v1/status", get(status))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::ratelimit::middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware,
        ));

    Router::new()
        .route("/", get(dashboard))
        .route("/healthz", get(health))
        .route("/openapi.json", get(crate::openapi::serve))
        .merge(api)
        .with_state(state)
}

fn internal_error(context: &str, e: impl std::fmt::Display) -> Response {
    tracing::error!(context, error = %e, "internal error");
    (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
}

#[utoipa::path(
    post,
    path = "/v1/jobs",
    request_body = JobSpec,
    responses(
        (status = 201, description = "Job accepted", body = SubmitResponse),
        (status = 400, description = "Invalid job spec"),
        (status = 401, description = "Missing or invalid API key"),
        (status = 429, description = "Rate limit exceeded"),
        (status = 503, description = "Queue saturated")
    )
)]
pub async fn submit(
    State(state): State<AppState>,
    Extension(tenant): Extension<Tenant>,
    Json(spec): Json<JobSpec>,
) -> Response {
    if !coop_types::is_supported_language(&spec.language) {
        return (
            StatusCode::BAD_REQUEST,
            format!("unsupported language; expected one of {SUPPORTED_LANGUAGES:?}"),
        )
            .into_response();
    }
    if spec.code.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "code must not be empty").into_response();
    }

    let job_id = Uuid::now_v7().to_string();
    let spec_json = match serde_json::to_string(&spec) {
        Ok(s) => s,
        Err(e) => return internal_error("serialize job spec", e),
    };

    if let Err(e) = state
        .store
        .create_job(&job_id, &tenant.0, &spec.language, &spec_json)
        .await
    {
        return internal_error("persist job", e);
    }
    state.bus.register(&job_id);

    if state.queue_tx.send(job_id.clone()).await.is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "job queue saturated").into_response();
    }

    tracing::info!(job_id = %job_id, tenant = %tenant.0, language = %spec.language, "job submitted");

    (
        StatusCode::CREATED,
        Json(SubmitResponse {
            stream_url: format!("/v1/jobs/{job_id}/stream"),
            replay_url: format!("/v1/jobs/{job_id}/replay"),
            job_id,
            status: "queued".to_string(),
        }),
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/v1/jobs",
    params(("limit" = Option<i64>, Query, description = "Max rows (1-500), default 50")),
    responses((status = 200, body = [JobView]), (status = 401))
)]
pub async fn list_jobs(
    State(state): State<AppState>,
    Extension(tenant): Extension<Tenant>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let limit = params.get("limit").and_then(|l| l.parse::<i64>().ok());
    match state
        .store
        .list_jobs(Some(&tenant.0), limit.unwrap_or(50))
        .await
    {
        Ok(rows) => Json(rows.iter().map(JobView::from_row).collect::<Vec<_>>()).into_response(),
        Err(e) => internal_error("list jobs", e),
    }
}

#[utoipa::path(
    get,
    path = "/v1/jobs/{id}",
    params(("id" = String, Path, description = "Job id")),
    responses((status = 200, body = JobView), (status = 404), (status = 401))
)]
pub async fn get_job(
    State(state): State<AppState>,
    Extension(tenant): Extension<Tenant>,
    Path(id): Path<String>,
) -> Response {
    match state.store.get_job(&id).await {
        Ok(Some(row)) if row.tenant == tenant.0 => Json(JobView::from_row(&row)).into_response(),
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => internal_error("get job", e),
    }
}

/// Cancel a job. Running jobs are killed (whole process group) within one
/// executor poll tick and finish as `cancelled`; queued jobs are marked
/// `cancelled` immediately so the scheduler skips them. Idempotent: an
/// already-terminal job returns 409 with its current status.
#[utoipa::path(
    delete,
    path = "/v1/jobs/{id}",
    params(("id" = String, Path, description = "Job id")),
    responses(
        (status = 200, description = "Cancellation accepted"),
        (status = 404, description = "Unknown or foreign job"),
        (status = 409, description = "Job already in a terminal state"),
        (status = 401, description = "Missing or invalid API key")
    )
)]
pub async fn cancel_job(
    State(state): State<AppState>,
    Extension(tenant): Extension<Tenant>,
    Path(id): Path<String>,
) -> Response {
    let row = match state.store.get_job(&id).await {
        Ok(Some(row)) if row.tenant == tenant.0 => row,
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return internal_error("load job for cancel", e),
    };

    match JobStatus::parse(&row.status) {
        Some(status) if status.is_terminal() => {
            // Idempotency guard: cancelling a finished job is a caller error,
            // not a silent success.
            return (StatusCode::CONFLICT, format!("job already {}", row.status)).into_response();
        }
        _ => {}
    }

    if let Some(flag) = state.cancels.get(&id) {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
        tracing::info!(job_id = %id, tenant = %tenant.0, "job cancellation requested");
        return StatusCode::OK.into_response();
    }

    // No flag means the job is queued but not yet picked up by a worker.
    // Finalize it directly; the worker will see a terminal status when it
    // loads the row and bail out before executing anything.
    if let Err(e) = state.store.finish(&id, "cancelled", None).await {
        return internal_error("cancel queued job", e);
    }
    tracing::info!(job_id = %id, tenant = %tenant.0, "queued job cancelled");
    StatusCode::OK.into_response()
}

/// Prometheus text-format metrics derived from the job store plus live
/// executor state. No per-tenant labels here: this endpoint is for ops
/// dashboards, not per-customer billing.
#[utoipa::path(
    get,
    path = "/v1/metrics",
    responses((status = 200, description = "Prometheus text format"), (status = 401))
)]
pub async fn metrics(State(state): State<AppState>) -> Response {
    let mut body = String::with_capacity(512);
    body.push_str("# HELP coop_jobs_total Total jobs by status.\n");
    body.push_str("# TYPE coop_jobs_total counter\n");
    match state.store.count_by_status().await {
        Ok(rows) => {
            for (status, n) in rows {
                body.push_str(&format!("coop_jobs_total{{status=\"{status}\"}} {n}\n"));
            }
        }
        Err(e) => return internal_error("count jobs for metrics", e),
    }
    body.push_str(&format!(
        "# HELP coop_running_jobs Jobs currently executing.\n# TYPE coop_running_jobs gauge\ncoop_running_jobs {}\n",
        state.cancels.len()
    ));
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        body,
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/v1/jobs/{id}/replay",
    params(("id" = String, Path, description = "Job id")),
    responses((status = 200, body = [WireEvent]), (status = 404), (status = 401))
)]
pub async fn replay(
    State(state): State<AppState>,
    Extension(tenant): Extension<Tenant>,
    Path(id): Path<String>,
) -> Response {
    if !owns_job(&state, &id, &tenant.0).await {
        return StatusCode::NOT_FOUND.into_response();
    }
    match state.store.events_for(&id).await {
        Ok(events) => Json(
            events
                .into_iter()
                .map(|e| WireEvent {
                    seq: e.seq,
                    ts_ms: e.ts_ms,
                    kind: e.kind,
                    data: e.data,
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => internal_error("load events", e),
    }
}

/// One-call outcome for agent tool loops: waits (server-side) for a terminal
/// state and returns the view plus stdout/stderr folded out of the event log.
/// `?wait_seconds=` caps the in-request wait (0 = return current state
/// immediately); when the wait budget expires with the job still running the
/// response is 202 with whatever has been produced so far.
#[utoipa::path(
    get,
    path = "/v1/jobs/{id}/result",
    params(
        ("id" = String, Path, description = "Job id"),
        ("wait_seconds" = Option<u64>, Query, description = "Max seconds to wait for a terminal state (0-300, default 60)")
    ),
    responses(
        (status = 200, description = "Job reached a terminal state", body = ResultView),
        (status = 202, description = "Still running when the wait budget expired; partial output included", body = ResultView),
        (status = 404), (status = 401)
    )
)]
pub async fn job_result(
    State(state): State<AppState>,
    Extension(tenant): Extension<Tenant>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if !owns_job(&state, &id, &tenant.0).await {
        return StatusCode::NOT_FOUND.into_response();
    }

    let wait_seconds = params
        .get("wait_seconds")
        .and_then(|w| w.parse::<u64>().ok())
        .unwrap_or(RESULT_DEFAULT_WAIT_SECONDS)
        .min(RESULT_MAX_WAIT_SECONDS);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait_seconds);
    let row = loop {
        match state.store.get_job(&id).await {
            Ok(Some(row)) => {
                let terminal = JobStatus::parse(&row.status).is_some_and(|s| s.is_terminal());
                if terminal || tokio::time::Instant::now() >= deadline {
                    break row;
                }
            }
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(e) => return internal_error("load job for result", e),
        }
        tokio::time::sleep(RESULT_POLL_INTERVAL).await;
    };

    match state.store.events_for(&id).await {
        Ok(events) => {
            let view = fold_result(&row, &events);
            let code = if JobStatus::parse(&row.status).is_some_and(|s| s.is_terminal()) {
                StatusCode::OK
            } else {
                StatusCode::ACCEPTED
            };
            (code, Json(view)).into_response()
        }
        Err(e) => internal_error("load events for result", e),
    }
}

/// Fold an ordered event list into a flat result: stdout/stderr joined
/// line-wise, truncation flag, and every sandbox violation raised during the
/// run. Deterministic over the replayable log — no live state involved.
fn fold_result(row: &JobRow, events: &[coop_store::EventRow]) -> ResultView {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut truncated = false;
    let mut violations = Vec::new();
    for event in events {
        match event.kind.as_str() {
            "stdout" | "stderr" => {
                if let Some(line) = event.data.get("line").and_then(serde_json::Value::as_str) {
                    if event.kind == "stdout" {
                        stdout.push(line);
                    } else {
                        stderr.push(line);
                    }
                }
            }
            "truncated" => truncated = true,
            "violation" => violations.push(event.data.clone()),
            _ => {}
        }
    }
    ResultView {
        job_id: row.job_id.clone(),
        status: row.status.clone(),
        exit_code: row.exit_code,
        duration_ms: match (row.started_at_ms, row.finished_at_ms) {
            (Some(started), Some(finished)) => Some(finished - started),
            _ => None,
        },
        stdout: stdout.join("\n"),
        stderr: stderr.join("\n"),
        truncated,
        violations,
    }
}

async fn stream(
    State(state): State<AppState>,
    Extension(tenant): Extension<Tenant>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    if !owns_job(&state, &id, &tenant.0).await {
        return StatusCode::NOT_FOUND.into_response();
    }
    ws.on_upgrade(move |socket| stream_socket(state, id, socket))
}

async fn stream_socket(state: AppState, job_id: String, socket: WebSocket) {
    let (mut tx, mut rx) = socket.split();
    let mut live = state.bus.subscribe(&job_id);
    let mut sent_max: i64 = 0;

    if !send_history(&state, &job_id, &mut tx, &mut sent_max).await {
        return;
    }

    loop {
        tokio::select! {
            biased;

            incoming = rx.next() => match incoming {
                Some(Ok(Message::Text(t))) => {
                    if t.as_str() == "ping" && tx.send(Message::text("pong")).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(_)) => break,
            },

            event = next_live(&mut live) => match event {
                None => {
                    if job_terminal(&state, &job_id).await {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Some(Err(RecvError::Lagged(_))) => {
                    if !send_history(&state, &job_id, &mut tx, &mut sent_max).await {
                        break;
                    }
                }
                Some(Err(RecvError::Closed)) => {
                    live = None;
                }
                Some(Ok(ev)) => {
                    if ev.seq > sent_max {
                        sent_max = ev.seq;
                        let payload = serde_json::to_string(ev.as_ref()).unwrap_or_default();
                        if tx.send(Message::text(payload)).await.is_err() {
                            break;
                        }
                        if ev.kind == "finished" {
                            break;
                        }
                    }
                }
            },
        }
    }

    let _ = tx.send(Message::Close(None)).await;
}

async fn next_live(
    live: &mut Option<tokio::sync::broadcast::Receiver<Arc<WireEvent>>>,
) -> Option<Result<Arc<WireEvent>, RecvError>> {
    match live.as_mut() {
        Some(receiver) => Some(receiver.recv().await),
        None => {
            std::future::pending::<()>().await;
            None
        }
    }
}

async fn send_history(
    state: &AppState,
    job_id: &str,
    tx: &mut SplitSink<WebSocket, Message>,
    sent_max: &mut i64,
) -> bool {
    match state.store.events_for(job_id).await {
        Ok(events) => {
            for e in events.into_iter() {
                if e.seq <= *sent_max {
                    continue;
                }
                *sent_max = e.seq;
                let payload = serde_json::to_string(&WireEvent {
                    seq: e.seq,
                    ts_ms: e.ts_ms,
                    kind: e.kind,
                    data: e.data,
                })
                .unwrap_or_default();
                if tx.send(Message::text(payload)).await.is_err() {
                    return false;
                }
            }
            true
        }
        Err(_) => false,
    }
}

async fn job_terminal(state: &AppState, job_id: &str) -> bool {
    matches!(
        state.store.get_job(job_id).await.ok().flatten().map(|r| JobStatus::parse(&r.status)),
        Some(Some(status)) if status.is_terminal()
    )
}

#[utoipa::path(get, path = "/healthz", responses((status = 200)))]
async fn health() -> Response {
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Version + sandbox mode detail, behind the authenticated API surface
/// (the unauthenticated /healthz stays status-only so it leaks nothing).
#[utoipa::path(
    get,
    path = "/v1/status",
    responses((status = 200), (status = 401, description = "Missing or invalid API key"))
)]
pub async fn status(State(state): State<AppState>) -> Response {
    Json(serde_json::json!({
        "version": crate::VERSION,
        "sandbox": state.sandbox_mode.as_str(),
    }))
    .into_response()
}

async fn dashboard() -> Html<&'static str> {
    Html(include_str!("dashboard.html"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use coop_store::EventRow;

    fn event(seq: i64, kind: &str, data: serde_json::Value) -> EventRow {
        EventRow {
            seq,
            ts_ms: seq * 10,
            kind: kind.to_string(),
            data,
        }
    }

    fn line(n: i64, stream: &str, text: &str) -> EventRow {
        event(n, stream, serde_json::json!({ "line": text }))
    }

    fn row(
        status: &str,
        started: Option<i64>,
        finished: Option<i64>,
        exit_code: Option<i32>,
    ) -> JobRow {
        JobRow {
            job_id: "job-1".into(),
            tenant: "t1".into(),
            language: "python".into(),
            status: status.into(),
            created_at_ms: 1,
            started_at_ms: started,
            finished_at_ms: finished,
            exit_code,
            spec_json: "{}".into(),
        }
    }

    #[test]
    fn fold_result_joins_streams_flags_and_violations() {
        let r = row("succeeded", Some(10), Some(45), Some(0));
        let events = vec![
            event(1, "started", serde_json::json!({})),
            line(2, "stdout", "one"),
            line(3, "stderr", "boom"),
            line(4, "stdout", "two"),
            event(5, "truncated", serde_json::json!({"stream": "stdout"})),
            event(
                6,
                "violation",
                serde_json::json!({"rule": "wall_clock_exceeded"}),
            ),
            event(7, "finished", serde_json::json!({"status": "succeeded"})),
        ];
        let v = fold_result(&r, &events);
        assert_eq!(v.status, "succeeded");
        assert_eq!(v.exit_code, Some(0));
        assert_eq!(v.duration_ms, Some(35));
        assert_eq!(v.stdout, "one\ntwo");
        assert_eq!(v.stderr, "boom");
        assert!(v.truncated);
        assert_eq!(v.violations.len(), 1);
        assert_eq!(v.violations[0]["rule"], "wall_clock_exceeded");
    }

    #[test]
    fn fold_result_empty_log_yields_empty_fields_and_no_duration() {
        let v = fold_result(&row("running", None, None, None), &[]);
        assert_eq!(v.stdout, "");
        assert_eq!(v.stderr, "");
        assert!(!v.truncated);
        assert!(v.violations.is_empty());
        assert_eq!(v.duration_ms, None);
    }

    #[tokio::test]
    async fn healthz_payload_is_status_only() {
        let resp = health().await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(bytes.as_ref(), br#"{"ok":true}"#);
    }
}
