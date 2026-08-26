use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct Tenant(pub String);

#[derive(Debug, Clone)]
pub struct StreamTicket {
    pub job_id: String,
    pub tenant: String,
    pub expires_at_ms: i64,
}

pub const STREAM_TICKET_TTL_MS: i64 = 30_000;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Mint a high-entropy, one-use WebSocket credential. Two independently
/// randomized UUIDv7 values provide substantially more entropy than a job id
/// while keeping the implementation dependency-free.
pub fn issue_stream_ticket(state: &crate::AppState, job_id: &str, tenant: &str) -> (String, i64) {
    let expires_at_ms = now_ms() + STREAM_TICKET_TTL_MS;
    let token = format!(
        "{}{}",
        uuid::Uuid::now_v7().simple(),
        uuid::Uuid::now_v7().simple()
    );
    state
        .stream_tickets
        .retain(|_, ticket| ticket.expires_at_ms > now_ms());
    state.stream_tickets.insert(
        token.clone(),
        StreamTicket {
            job_id: job_id.to_string(),
            tenant: tenant.to_string(),
            expires_at_ms,
        },
    );
    (token, expires_at_ms)
}

fn key_digest(key: &str) -> [u8; 32] {
    Sha256::digest(key.as_bytes()).into()
}

/// Constant-time equality over equal-length digests: XOR-fold, no early returns,
/// so timing cannot reveal which candidate key matched.
fn ct_digest_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Look up the presented key among candidates by comparing SHA-256 digests
/// in constant time. Always scans every candidate so total work does not
/// depend on which entry matches.
fn authenticate(api_keys: &HashMap<String, String>, presented: &str) -> Option<String> {
    let presented = key_digest(presented);
    let mut tenant = None;
    for (candidate, t) in api_keys.iter() {
        if ct_digest_eq(&presented, &key_digest(candidate)) {
            tenant = Some(t.clone());
        }
    }
    tenant
}

fn extract_bearer(req: &Request) -> Option<String> {
    let value = req.headers().get(header::AUTHORIZATION)?;
    let value = value.to_str().ok()?;
    let (scheme, credential) = value.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !credential.is_empty())
        .then(|| credential.to_string())
}

fn extract_query_param(req: &Request, name: &str) -> Option<String> {
    let query = req.uri().query()?;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == name {
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
    if let Some(key) = extract_bearer(&req) {
        return match authenticate(&state.cfg.api_keys, &key) {
            Some(tenant) => {
                req.extensions_mut().insert(Tenant(tenant));
                next.run(req).await
            }
            None => crate::routes::api_error(
                StatusCode::UNAUTHORIZED,
                "invalid_api_key",
                "invalid API key",
                false,
            ),
        };
    }

    let path = req.uri().path().to_string();
    if path.ends_with("/stream") {
        if let Some(token) = extract_query_param(&req, "ticket") {
            if let Some(grant) = state.stream_tickets.get(&token).map(|entry| entry.clone()) {
                let expected_path = format!("/v1/jobs/{}/stream", grant.job_id);
                if grant.expires_at_ms > now_ms() && path == expected_path {
                    // Only one racing upgrade can consume the credential.
                    if state.stream_tickets.remove(&token).is_some() {
                        req.extensions_mut().insert(Tenant(grant.tenant));
                        return next.run(req).await;
                    }
                }
                if grant.expires_at_ms <= now_ms() {
                    state.stream_tickets.remove(&token);
                }
            }
            return crate::routes::api_error(
                StatusCode::UNAUTHORIZED,
                "invalid_stream_ticket",
                "stream ticket is invalid, expired, or already used",
                false,
            );
        }

        // Compatibility for local development only. Production never accepts
        // long-lived API keys in URLs, where proxies and access logs expose
        // them. Clients should migrate to POST .../stream-ticket.
        if !state.cfg.production {
            if let Some(key) = extract_query_param(&req, "key") {
                return match authenticate(&state.cfg.api_keys, &key) {
                    Some(tenant) => {
                        tracing::warn!("deprecated WebSocket ?key= authentication used");
                        req.extensions_mut().insert(Tenant(tenant));
                        next.run(req).await
                    }
                    None => crate::routes::api_error(
                        StatusCode::UNAUTHORIZED,
                        "invalid_api_key",
                        "invalid API key",
                        false,
                    ),
                };
            }
        }
    }

    crate::routes::api_error(
        StatusCode::UNAUTHORIZED,
        "missing_api_key",
        "missing bearer API key",
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn ct_digest_eq_matches_only_identical_digests() {
        let a = key_digest("alpha");
        assert!(ct_digest_eq(&a, &key_digest("alpha")));
        assert!(!ct_digest_eq(&a, &key_digest("alphb")));
        assert!(!ct_digest_eq(&a, &key_digest("beta")));
    }

    #[test]
    fn digest_is_deterministic_and_distinct_per_key() {
        assert_eq!(key_digest("k1"), key_digest("k1"));
        assert_ne!(key_digest("k1"), key_digest("k2"));
    }

    #[test]
    fn authenticate_accepts_exact_key_and_returns_tenant() {
        let cfg = keys(&[("s3cr3t", "acme")]);
        assert_eq!(authenticate(&cfg, "s3cr3t"), Some("acme".to_string()));
    }

    #[test]
    fn authenticate_rejects_wrong_prefixes_supersets_and_unknown() {
        let cfg = keys(&[("s3cr3t", "acme")]);
        for bad in ["", "s", "s3cr3", "s3cr3tx", "x s3cr3t", "S3CR3T"] {
            assert_eq!(authenticate(&cfg, bad), None, "{bad}");
        }
    }

    #[test]
    fn authenticate_picks_right_tenant_among_many() {
        let cfg = keys(&[
            ("aaa", "tenant-a"),
            ("bbb", "tenant-b"),
            ("ccc", "tenant-c"),
        ]);
        assert_eq!(authenticate(&cfg, "bbb"), Some("tenant-b".to_string()));
        assert_eq!(authenticate(&cfg, "zzz"), None);
    }

    #[test]
    fn authenticate_on_empty_keyset_is_none() {
        assert_eq!(authenticate(&keys(&[]), "coop-dev-key"), None);
    }
}
