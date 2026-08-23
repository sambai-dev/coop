use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Tenant(pub String);

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
    match authenticate(&state.cfg.api_keys, &key) {
        Some(tenant) => {
            req.extensions_mut().insert(Tenant(tenant));
            next.run(req).await
        }
        None => (StatusCode::UNAUTHORIZED, "invalid API key").into_response(),
    }
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
