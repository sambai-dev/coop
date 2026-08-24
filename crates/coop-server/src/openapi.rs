use crate::bus::WireEvent;
use crate::routes::{JobView, SubmitResponse};
use axum::Json;
use coop_types::{JobSpec, Limits};
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Coop API",
        version = crate::VERSION,
        description = "Self-hostable sandbox for AI agents. Submit code, stream output over WebSocket, replay any execution from the append-only event log.",
    ),
    paths(crate::routes::submit, crate::routes::list_jobs, crate::routes::get_job, crate::routes::cancel_job, crate::routes::replay, crate::routes::metrics, crate::routes::status),
    components(schemas(JobSpec, Limits, JobView, SubmitResponse, WireEvent))
)]
pub struct ApiDoc;

pub async fn serve() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}
