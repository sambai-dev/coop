use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use dashmap::DashMap;

pub struct RateLimiter {
    counts: DashMap<(String, u64), u32>,
    limit: u32,
}

pub struct RateDecision {
    pub allowed: bool,
    pub remaining: u32,
    pub retry_after_secs: u64,
}

impl RateLimiter {
    pub fn new(limit: u32) -> Self {
        Self {
            counts: DashMap::new(),
            limit: limit.max(1),
        }
    }

    pub fn check(&self, tenant: &str) -> RateDecision {
        let minute = now_minute();
        if self.counts.len() > 100_000 {
            self.counts.retain(|(_, m), _| *m >= minute);
        }
        let mut entry = self.counts.entry((tenant.to_string(), minute)).or_insert(0);
        *entry += 1;
        RateDecision {
            allowed: *entry <= self.limit,
            remaining: self.limit.saturating_sub(*entry),
            retry_after_secs: 60 - (now_secs() % 60),
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn now_minute() -> u64 {
    now_secs() / 60
}

pub async fn middleware(
    State(state): State<crate::AppState>,
    req: Request,
    next: Next,
) -> Response {
    let Some(tenant) = req.extensions().get::<crate::auth::Tenant>() else {
        return crate::routes::api_error(
            StatusCode::UNAUTHORIZED,
            "missing_tenant",
            "authenticated tenant context is missing",
            false,
        );
    };
    let decision = state.rate.check(&tenant.0);
    if decision.allowed {
        let mut response = next.run(req).await;
        if let Ok(value) = HeaderValue::from_str(&state.cfg.rate_per_min.to_string()) {
            response.headers_mut().insert("x-ratelimit-limit", value);
        }
        if let Ok(value) = HeaderValue::from_str(&decision.remaining.to_string()) {
            response
                .headers_mut()
                .insert("x-ratelimit-remaining", value);
        }
        response
    } else {
        crate::routes::api_error_with_retry(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_exceeded",
            "rate limit exceeded",
            true,
            Some(decision.retry_after_secs),
        )
    }
}
