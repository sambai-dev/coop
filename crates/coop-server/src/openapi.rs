use crate::bus::WireEvent;
use crate::routes::{
    CancellationResponse, CapabilitiesResponse, ErrorBody, ErrorEnvelope, ExecutionCapabilities,
    ExecutionPolicy, FeatureCapabilities, JobDetail, JobView, LimitCapabilities, ListJobsResponse,
    ReplayResponse, ResultView, SchedulerStatus, StatusResponse, StreamTicketResponse,
    SubmitResponse, WhoAmIResponse,
};
use axum::Json;
use coop_types::{EffectiveJobSpec, EffectiveLimits, JobSpec, LimitEnforcement, Limits};
use utoipa::openapi::security::{Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Coop API",
        version = crate::VERSION,
        description = "Self-hosted, audit-first execution gateway for AI agents. Submit policy-bound code, stream output, and verify retained hash-chained execution evidence.",
    ),
    paths(
        crate::routes::submit,
        crate::routes::list_jobs,
        crate::routes::get_job,
        crate::routes::cancel_job,
        crate::routes::replay,
        crate::routes::job_result,
        crate::routes::stream,
        crate::routes::stream_ticket,
        crate::routes::metrics,
        crate::routes::status,
        crate::routes::capabilities,
        crate::routes::whoami,
        crate::routes::health,
        crate::routes::ready
    ),
    components(schemas(
        JobSpec,
        Limits,
        EffectiveJobSpec,
        EffectiveLimits,
        LimitEnforcement,
        JobView,
        CancellationResponse,
        JobDetail,
        ExecutionPolicy,
        ResultView,
        SubmitResponse,
        ListJobsResponse,
        ReplayResponse,
        StreamTicketResponse,
        WhoAmIResponse,
        CapabilitiesResponse,
        ExecutionCapabilities,
        LimitCapabilities,
        FeatureCapabilities,
        StatusResponse,
        SchedulerStatus,
        WireEvent,
        ErrorEnvelope,
        ErrorBody
    )),
    modifiers(&SecurityAddon),
    security(("bearer_auth" = []))
)]
pub struct ApiDoc;

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        openapi
            .components
            .get_or_insert_default()
            .add_security_scheme(
                "bearer_auth",
                SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
            );
    }
}

pub async fn serve() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_has_bearer_auth_streaming_readiness_and_error_schema() {
        let value = serde_json::to_value(ApiDoc::openapi()).expect("serialize openapi");
        assert_eq!(
            value["components"]["securitySchemes"]["bearer_auth"]["scheme"],
            "bearer"
        );
        for path in [
            "/healthz",
            "/readyz",
            "/v1/jobs/{id}/stream",
            "/v1/jobs/{id}/stream-ticket",
            "/v1/capabilities",
            "/v1/whoami",
        ] {
            assert!(value["paths"].get(path).is_some(), "missing {path}");
        }
        assert!(value["components"]["schemas"]["ErrorEnvelope"].is_object());
        assert_eq!(
            value["paths"]["/healthz"]["get"]["security"],
            serde_json::json!([])
        );
        assert_eq!(
            value["paths"]["/readyz"]["get"]["security"],
            serde_json::json!([])
        );
        let submit_responses = &value["paths"]["/v1/jobs"]["post"]["responses"];
        for status in ["408", "415", "422", "429", "503", "507"] {
            assert!(
                submit_responses.get(status).is_some(),
                "submit OpenAPI missing {status} response"
            );
        }
    }
}
