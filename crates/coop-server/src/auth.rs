use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

#[derive(Debug, Clone)]
pub struct Tenant(pub String);

fn extract_bearer(req: &Request) -> Option<String> {
    let value = req.headers().get(header::AUTHORIZATION)?;
    let value = value.to_str().ok()?;
    value.strip_prefix("Bearer ").map(str::to_string)
}

fn extract_query_key(req: &Request) -> Option<String> {
    let query = req.uri().query()?;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == "key" {
                return urldecode(v);
            }
        }
    }
    None
}

fn urldecode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                let byte = u8::from_str_radix(hex, 16).ok()?;
                out.push(byte);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

pub async fn middleware(
    State(state): State<crate::AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let key = extract_bearer(&req).or_else(|| extract_query_key(&req));
    let Some(key) = key else {
        return (StatusCode::UNAUTHORIZED, "missing API key").into_response();
    };
    match state.cfg.api_keys.get(&key) {
        Some(tenant) => {
            req.extensions_mut().insert(Tenant(tenant.clone()));
            next.run(req).await
        }
        None => (StatusCode::UNAUTHORIZED, "invalid API key").into_response(),
    }
}
