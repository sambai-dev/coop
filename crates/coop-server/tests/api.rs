use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use coop_server::config::Config;
use coop_server::scheduler;
use coop_store::Store;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const TERMINAL: [&str; 5] = ["succeeded", "failed", "timed_out", "oom_killed", "error"];

fn test_config(db: &std::path::Path) -> Config {
    let mut api_keys = HashMap::new();
    api_keys.insert("test-key".to_string(), "t1".to_string());
    api_keys.insert("other-key".to_string(), "t2".to_string());
    Config {
        addr: "127.0.0.1:0".to_string(),
        db_path: db.to_string_lossy().into_owned(),
        api_keys,
        workers: 2,
        tenant_concurrency: 4,
        rate_per_min: 10_000,
        sandbox: "off".to_string(),
        jobs_root: std::env::temp_dir()
            .join(format!("coop-jobs-test-{}", uuid::Uuid::now_v7()))
            .to_string_lossy()
            .into_owned(),
        python_bin: None,
        node_bin: None,
        bash_bin: None,
    }
}

async fn spawn_app() -> Router {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let cfg = test_config(&db);
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (app, state, queue_rx) = coop_server::build_app(cfg, store).expect("build app");
    scheduler::spawn_workers(state, queue_rx);
    app
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = app.clone().oneshot(req).await.expect("oneshot");
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .expect("body");
    let value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::String(
            String::from_utf8_lossy(&bytes).into_owned(),
        ))
    };
    (status, value)
}

fn request(method: &str, uri: &str, key: Option<&str>, body: Option<String>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(k) = key {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {k}"));
    }
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(Body::from(body.unwrap_or_default()))
        .expect("request")
}

async fn wait_terminal(app: &Router, job_id: &str) -> serde_json::Value {
    for _ in 0..150 {
        let (status, body) = send(
            app,
            request("GET", &format!("/v1/jobs/{job_id}"), Some("test-key"), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        if body["status"]
            .as_str()
            .map(|s| TERMINAL.contains(&s))
            .unwrap_or(false)
        {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("job {job_id} did not reach a terminal state in time");
}

fn python_name() -> &'static str {
    if cfg!(windows) {
        "python"
    } else {
        "python3"
    }
}

fn python_available() -> bool {
    std::process::Command::new(python_name())
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn rejects_missing_api_key() {
    let app = spawn_app().await;
    let (status, _) = send(
        &app,
        request(
            "POST",
            "/v1/jobs",
            None,
            Some(r#"{"language":"python","code":"print(1)"}"#.into()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rejects_unknown_language() {
    let app = spawn_app().await;
    let (status, _) = send(
        &app,
        request(
            "POST",
            "/v1/jobs",
            Some("test-key"),
            Some(r#"{"language":"fortran","code":"PRINT *, 1"}"#.into()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn cross_tenant_reads_are_rejected() {
    let app = spawn_app().await;
    let payload = r#"{"language":"python","code":"print(1)"}"#.to_string();
    let (status, body) = send(
        &app,
        request("POST", "/v1/jobs", Some("test-key"), Some(payload)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let job_id = body["job_id"].as_str().expect("job_id");

    let (owner_status, _) = send(
        &app,
        request("GET", &format!("/v1/jobs/{job_id}"), Some("test-key"), None),
    )
    .await;
    assert_eq!(owner_status, StatusCode::OK);

    for path in [
        format!("/v1/jobs/{job_id}"),
        format!("/v1/jobs/{job_id}/replay"),
    ] {
        let (status, _) = send(&app, request("GET", &path, Some("other-key"), None)).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "cross-tenant read of {path} must look like a missing job"
        );
    }

    let (_, other_jobs) = send(
        &app,
        request("GET", "/v1/jobs?limit=100", Some("other-key"), None),
    )
    .await;
    let leaked = other_jobs
        .as_array()
        .map(|a| a.iter().any(|j| j["job_id"] == *job_id))
        .unwrap_or(false);
    assert!(!leaked, "tenant t2 must not see tenant t1 jobs in listings");
}

#[tokio::test]
async fn runs_python_hello_world_end_to_end() {
    if !python_available() {
        eprintln!("skipping: no python interpreter on PATH");
        return;
    }
    let app = spawn_app().await;
    let code = "print('hello from coop')".to_string();
    let payload = serde_json::json!({ "language": "python", "code": code }).to_string();

    let (status, body) = send(
        &app,
        request("POST", "/v1/jobs", Some("test-key"), Some(payload)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "submit failed: {body}");
    let job_id = body["job_id"].as_str().expect("job_id").to_string();

    let final_view = wait_terminal(&app, &job_id).await;
    assert_eq!(
        final_view["status"], "succeeded",
        "final view: {final_view}"
    );
    assert_eq!(final_view["exit_code"], 0);

    let (_, replay) = send(
        &app,
        request(
            "GET",
            &format!("/v1/jobs/{job_id}/replay"),
            Some("test-key"),
            None,
        ),
    )
    .await;
    let stdout: String = replay
        .as_array()
        .expect("replay array")
        .iter()
        .filter(|e| e["kind"] == "stdout")
        .filter_map(|e| e["data"]["line"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(stdout.contains("hello from coop"), "stdout was: {stdout}");

    let kinds: Vec<&str> = replay
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"started"));
    assert!(kinds.contains(&"finished"));
}
