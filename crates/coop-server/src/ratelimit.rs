use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;

pub struct RateLimiter {
    counts: DashMap<(String, u64), u32>,
    limit: u32,
}

impl RateLimiter {
    pub fn new(limit: u32) -> Self {
        Self {
            counts: DashMap::new(),
            limit: limit.max(1),
        }
    }

    pub fn allow(&self, tenant: &str) -> bool {
        let minute = now_minute();
        if self.counts.len() > 100_000 {
            self.counts.retain(|(_, m), _| *m >= minute);
        }
        let mut entry = self.counts.entry((tenant.to_string(), minute)).or_insert(0);
        *entry += 1;
        *entry <= self.limit
    }
}

fn now_minute() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 60
}

pub async fn middleware(
    State(state): State<crate::AppState>,
    req: Request,
    next: Next,
) -> Response {
    let Some(tenant) = req.extensions().get::<crate::auth::Tenant>() else {
        return (StatusCode::UNAUTHORIZED, "missing tenant").into_response();
    };
    if state.rate.allow(&tenant.0) {
        next.run(req).await
    } else {
        (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response()
    }
}
