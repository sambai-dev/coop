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

const TERMINAL: [&str; 6] = [
    "succeeded",
    "failed",
    "timed_out",
    "oom_killed",
    "cancelled",
    "error",
];

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
        retention_hours: 0,
        sweep_interval_secs: 3600,
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

fn bash_sleep_available() -> bool {
    std::process::Command::new("bash")
        .arg("-c")
        .arg("command -v sleep")
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

// ---------------------------------------------------------------------------
// Cancellation (DELETE /v1/jobs/{id})
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_queued_job_finalizes_without_execution() {
    let app = spawn_app().await;

    // Deterministic queuing: fill the 2-worker pool with long-running
    // blockers so the victim below cannot start before its DELETE lands.
    // (A bare `echo` victim completes in milliseconds and raced this test
    // on CI: sometimes already terminal, making the expected 200 a 409.)
    let mut blockers = Vec::new();
    for _ in 0..2 {
        let (status, body) = send(
            &app,
            request(
                "POST",
                "/v1/jobs",
                Some("test-key"),
                Some(
                    r#"{"language":"bash","code":"sleep 30","limits":{"wall_seconds":30}}"#.into(),
                ),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        blockers.push(body["job_id"].as_str().expect("job_id").to_string());
    }

    let (status, body) = send(
        &app,
        request(
            "POST",
            "/v1/jobs",
            Some("test-key"),
            Some(r#"{"language":"bash","code":"echo should-never-run","limits":{"wall_seconds":30}}"#.into()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let job_id = body["job_id"].as_str().expect("job_id").to_string();

    let (status, _) = send(
        &app,
        request(
            "DELETE",
            &format!("/v1/jobs/{job_id}"),
            Some("test-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The job must reach a terminal `cancelled` state without running.
    let view = wait_terminal(&app, &job_id).await;
    assert_eq!(view["status"], "cancelled", "{view}");

    // Cancelling again is a 409 (idempotency guard), not a silent success.
    let (status, _) = send(
        &app,
        request(
            "DELETE",
            &format!("/v1/jobs/{job_id}"),
            Some("test-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Cleanup: cancel the blockers so the pool drains before the app drops.
    for id in &blockers {
        let _ = send(
            &app,
            request("DELETE", &format!("/v1/jobs/{id}"), Some("test-key"), None),
        )
        .await;
    }
}

#[tokio::test]
async fn cancel_running_job_kills_it_before_wall_clock() {
    let app = spawn_app().await;
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/v1/jobs",
            Some("test-key"),
            Some(r#"{"language":"bash","code":"sleep 60","limits":{"wall_seconds":60}}"#.into()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let job_id = body["job_id"].as_str().expect("job_id").to_string();

    // Wait until it is actually running so we exercise the kill path, not
    // the queued-skip path.
    for _ in 0..100 {
        let (_, v) = send(
            &app,
            request("GET", &format!("/v1/jobs/{job_id}"), Some("test-key"), None),
        )
        .await;
        if v["status"] == "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let started = std::time::Instant::now();
    let (status, _) = send(
        &app,
        request(
            "DELETE",
            &format!("/v1/jobs/{job_id}"),
            Some("test-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let view = wait_terminal(&app, &job_id).await;
    assert_eq!(view["status"], "cancelled", "{view}");
    // Must be cancelled far before the 60s wall clock.
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "cancel took too long: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn cancel_is_tenant_scoped() {
    let app = spawn_app().await;
    let (_, body) = send(
        &app,
        request(
            "POST",
            "/v1/jobs",
            Some("test-key"),
            Some(r#"{"language":"bash","code":"echo t1","limits":{"wall_seconds":15}}"#.into()),
        ),
    )
    .await;
    let job_id = body["job_id"].as_str().expect("job_id").to_string();

    // Tenant t2 cannot see or cancel tenant t1's job.
    let (status, _) = send(
        &app,
        request(
            "DELETE",
            &format!("/v1/jobs/{job_id}"),
            Some("other-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn metrics_endpoint_reports_job_counts() {
    let app = spawn_app().await;
    send(
        &app,
        request(
            "POST",
            "/v1/jobs",
            Some("test-key"),
            Some(r#"{"language":"bash","code":"echo hi","limits":{"wall_seconds":15}}"#.into()),
        ),
    )
    .await;

    let res = app
        .clone()
        .oneshot(request("GET", "/v1/metrics", Some("test-key"), None))
        .await
        .expect("oneshot");
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/plain"), "content-type was {ct}");
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .expect("body");
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("coop_jobs_total"), "{text}");
    assert!(text.contains("coop_running_jobs"), "{text}");
}

// ---------------------------------------------------------------------------
// GET /v1/jobs/{id}/result
// ---------------------------------------------------------------------------

async fn submit_bash(app: &Router, code: &str) -> String {
    let payload =
        serde_json::json!({ "language": "bash", "code": code, "limits": { "wall_seconds": 15 } })
            .to_string();
    let (status, body) = send(
        app,
        request("POST", "/v1/jobs", Some("test-key"), Some(payload)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["job_id"].as_str().expect("job_id").to_string()
}

#[tokio::test]
async fn result_endpoint_folds_output_into_one_response() {
    if !python_available() {
        eprintln!("skipping: no python interpreter on PATH");
        return;
    }
    let app = spawn_app().await;
    let payload = serde_json::json!({
        "language": "python",
        "code": "print('out-line'); import sys; print('err-line', file=sys.stderr)",
    })
    .to_string();
    let (status, body) = send(
        &app,
        request("POST", "/v1/jobs", Some("test-key"), Some(payload)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let job_id = body["job_id"].as_str().expect("job_id").to_string();

    // Server-side wait: a single call must block until terminal and return
    // the folded result.
    let (status, body) = send(
        &app,
        request(
            "GET",
            &format!("/v1/jobs/{job_id}/result?wait_seconds=30"),
            Some("test-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["job_id"], job_id);
    assert_eq!(body["status"], "succeeded", "{body}");
    assert_eq!(body["exit_code"], 0);
    assert_eq!(body["stdout"], "out-line");
    assert_eq!(body["stderr"], "err-line");
    assert_eq!(body["truncated"], false);
    assert!(body["violations"]
        .as_array()
        .expect("violations")
        .is_empty());
    assert!(body["duration_ms"].is_i64(), "{body}");
}

#[tokio::test]
async fn result_is_tenant_scoped_and_404_for_unknown_jobs() {
    let app = spawn_app().await;
    let job_id = submit_bash(&app, "echo scoped").await;

    // Another tenant must see a missing job, not someone else's result.
    let (status, _) = send(
        &app,
        request(
            "GET",
            &format!("/v1/jobs/{job_id}/result"),
            Some("other-key"),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "cross-tenant result read must look like a missing job"
    );

    // Unknown ids look the same way.
    let (status, _) = send(
        &app,
        request(
            "GET",
            "/v1/jobs/01a00000-0000-7000-8000-000000000000/result",
            Some("test-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn result_with_zero_wait_returns_202_while_running() {
    let app = spawn_app().await;
    if !bash_sleep_available() {
        eprintln!("skipping: no bash sleep on PATH");
        return;
    }
    let job_id = submit_bash(&app, "sleep 5").await;

    // A zero wait budget must return immediately with a partial view while
    // the job is still running.
    let started = std::time::Instant::now();
    let (status, body) = send(
        &app,
        request(
            "GET",
            &format!("/v1/jobs/{job_id}/result?wait_seconds=0"),
            Some("test-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    // Not terminal yet; whether the worker has already picked it up
    // (`queued` vs `running`) is a scheduling race the test must not
    // depend on.
    assert!(
        matches!(body["status"].as_str(), Some("queued" | "running")),
        "{body}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "wait_seconds=0 must not block"
    );
}

#[tokio::test]
async fn result_waits_for_terminal_within_budget() {
    if !python_available() {
        eprintln!("skipping: no python interpreter on PATH");
        return;
    }
    let app = spawn_app().await;
    let payload = serde_json::json!({
        "language": "python",
        "code": "import time; time.sleep(1.5); print('late but done')",
    })
    .to_string();
    let (status, body) = send(
        &app,
        request("POST", "/v1/jobs", Some("test-key"), Some(payload)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let job_id = body["job_id"].as_str().expect("job_id").to_string();

    let (status, body) = send(
        &app,
        request(
            "GET",
            &format!("/v1/jobs/{job_id}/result?wait_seconds=30"),
            Some("test-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "succeeded", "{body}");
    assert_eq!(body["stdout"], "late but done");
}
