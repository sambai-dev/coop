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

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/v1/jobs", post(submit).get(list_jobs))
        .route("/v1/jobs/{id}", get(get_job))
        .route("/v1/jobs/{id}/replay", get(replay))
        .route("/v1/jobs/{id}/stream", get(stream))
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
pub async fn owns_job(state: &AppState, id: &str, tenant: &str) -> bool {
    matches!(
        state.store.get_job(id).await,
        Ok(Some(row)) if row.tenant == tenant
    )
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
async fn health(State(state): State<AppState>) -> Response {
    Json(serde_json::json!({
        "ok": true,
        "version": crate::VERSION,
        "sandbox": state.sandbox_mode.as_str(),
    }))
    .into_response()
}

async fn dashboard() -> Html<&'static str> {
    Html(include_str!("dashboard.html"))
}
