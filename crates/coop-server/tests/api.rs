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
        metrics_token: Some("test-metrics-token".to_string()),
        workers: 2,
        tenant_concurrency: 4,
        tenant_queue_capacity: 64,
        rate_per_min: 10_000,
        max_job_mem_mb: 1024,
        memory_budget_mb: 4096,
        storage_global_mb: 16 * 1024,
        storage_tenant_mb: 4 * 1024,
        storage_free_reserve_mb: 0,
        sandbox: "off".to_string(),
        jobs_root: std::env::temp_dir()
            .join(format!("coop-jobs-test-{}", uuid::Uuid::now_v7()))
            .to_string_lossy()
            .into_owned(),
        rootfs: None,
        sandbox_helper: None,
        gvisor_runsc: None,
        gvisor_rootfs_sha256: None,
        gvisor_platform: "systrap".to_string(),
        gvisor_uid: 65_534,
        gvisor_gid: 65_534,
        production: false,
        unsafe_allow_naive: false,
        unsafe_allow_public_dev: false,
        python_bin: None,
        node_bin: None,
        bash_bin: None,
        retention_hours: 0,
        sweep_interval_secs: 3600,
        seccomp: false,
    }
}

async fn spawn_app() -> Router {
    spawn_app_with_limits(2, 4).await
}

async fn spawn_app_with_limits(workers: usize, tenant_concurrency: usize) -> Router {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let mut cfg = test_config(&db);
    cfg.workers = workers;
    cfg.tenant_concurrency = tenant_concurrency;
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (app, state, queue_rx) = coop_server::build_app(cfg, store).await.expect("build app");
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
    wait_terminal_with_key(app, job_id, "test-key").await
}

async fn wait_terminal_with_key(app: &Router, job_id: &str, key: &str) -> serde_json::Value {
    for _ in 0..150 {
        let (status, body) = send(
            app,
            request("GET", &format!("/v1/jobs/{job_id}"), Some(key), None),
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
    let (status, body) = send(
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
    assert_eq!(body["error"]["code"], "missing_api_key", "{body}");
    assert!(body["error"]["request_id"].is_string(), "{body}");
    assert_eq!(body["error"]["retryable"], false, "{body}");
}

#[tokio::test]
async fn malformed_json_returns_structured_error() {
    let app = spawn_app().await;
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/v1/jobs",
            Some("test-key"),
            Some("{not-json".to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_json", "{body}");
    assert!(body["error"]["request_id"].is_string(), "{body}");
}

#[tokio::test]
async fn full_decoded_code_and_stdin_fit_even_with_worst_case_json_escaping() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let cfg = test_config(&db);
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (app, _state, _queue_rx) = coop_server::build_app(cfg, store).await.expect("build app");
    let escaped = "\u{1}".repeat(1_048_576);
    let payload = serde_json::json!({
        "language": "python",
        "code": escaped,
        "stdin": escaped,
    })
    .to_string();
    assert!(
        payload.len() > 12_000_000,
        "test must exercise JSON expansion"
    );

    let (status, body) = send(
        &app,
        request("POST", "/v1/jobs", Some("test-key"), Some(payload)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn submit_preserves_unsupported_media_type_semantics() {
    let app = spawn_app().await;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/jobs")
        .header(header::AUTHORIZATION, "Bearer test-key")
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(r#"{"language":"python","code":"print(1)"}"#))
        .unwrap();
    let (status, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
    assert_eq!(body["error"]["code"], "unsupported_media_type", "{body}");
}

#[tokio::test]
async fn queued_detail_exposes_unknown_effective_policy_without_fabrication() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let cfg = test_config(&db);
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (app, _state, _queue_rx) = coop_server::build_app(cfg, store).await.expect("build app");
    let (status, accepted) = send(
        &app,
        request(
            "POST",
            "/v1/jobs",
            Some("test-key"),
            Some(r#"{"language":"python","code":"print(1)"}"#.to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{accepted}");
    let id = accepted["job_id"].as_str().unwrap();
    let (status, detail) = send(
        &app,
        request("GET", &format!("/v1/jobs/{id}"), Some("test-key"), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert!(detail["effective_spec"].is_null(), "{detail}");
    for field in [
        "sandbox",
        "seccomp",
        "network_allowed",
        "networking",
        "private_rootfs",
        "dedicated_bootstrap",
    ] {
        assert!(
            detail["execution_policy"][field].is_null(),
            "{field}: {detail}"
        );
    }
}

#[tokio::test]
async fn unavailable_development_runtime_is_not_advertised_or_admitted() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let mut cfg = test_config(&db);
    cfg.python_bin = Some(
        std::env::temp_dir()
            .join(format!("missing-coop-python-{}", uuid::Uuid::now_v7()))
            .to_string_lossy()
            .into_owned(),
    );
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (app, _state, _queue_rx) = coop_server::build_app(cfg, store).await.expect("build app");

    let (capabilities_status, capabilities) = send(
        &app,
        request("GET", "/v1/capabilities", Some("test-key"), None),
    )
    .await;
    assert_eq!(capabilities_status, StatusCode::OK, "{capabilities}");
    assert!(
        !capabilities["languages"]
            .as_array()
            .expect("languages")
            .iter()
            .any(|language| language == "python"),
        "{capabilities}"
    );
    assert_eq!(
        capabilities["execution"]["limit_enforcement"],
        serde_json::json!({
            "wall_seconds": true,
            "cpu_seconds": false,
            "mem_mb": false,
            "max_pids": false,
            "max_file_mb": false,
        }),
        "development capabilities must not advertise controls the subprocess backend does not enforce: {capabilities}"
    );
    let languages = capabilities["languages"]
        .as_array()
        .expect("capability languages");
    let language_rank = |language: &serde_json::Value| {
        coop_types::SUPPORTED_LANGUAGES
            .iter()
            .position(|candidate| Some(*candidate) == language.as_str())
            .expect("advertised language is supported")
    };
    assert!(
        languages
            .windows(2)
            .all(|pair| language_rank(&pair[0]) < language_rank(&pair[1])),
        "concurrent preflight completion must not reorder capabilities: {capabilities}"
    );

    let (submit_status, submit) = send(
        &app,
        request(
            "POST",
            "/v1/jobs",
            Some("test-key"),
            Some(serde_json::json!({"language":"python","code":"print(1)"}).to_string()),
        ),
    )
    .await;
    assert_eq!(submit_status, StatusCode::UNPROCESSABLE_ENTITY, "{submit}");
    assert_eq!(submit["error"]["code"], "runtime_unavailable", "{submit}");
}

#[tokio::test]
async fn saturated_admission_is_nonblocking_and_retryable() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let cfg = test_config(&db);
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (app, state, _queue_rx) = coop_server::build_app(cfg, store).await.expect("build app");
    for n in 0..coop_server::QUEUE_CAPACITY {
        state
            .admission
            .try_reserve(&format!("synthetic-tenant-{n}"), 256)
            .expect("reserve admission")
            .send(format!("synthetic-{n}"));
    }

    let started = std::time::Instant::now();
    let response = app
        .oneshot(request(
            "POST",
            "/v1/jobs",
            Some("test-key"),
            Some(r#"{"language":"python","code":"print(1)"}"#.to_string()),
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok()),
        Some("1")
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json error");
    assert_eq!(body["error"]["code"], "queue_saturated", "{body}");
    assert_eq!(body["error"]["retryable"], true, "{body}");
}

#[tokio::test]
async fn tenant_queue_capacity_returns_429_without_blocking_another_tenant() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let mut cfg = test_config(&db);
    cfg.tenant_queue_capacity = 2;
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (app, _state, _queue_rx) = coop_server::build_app(cfg, store).await.expect("build app");
    let body = r#"{"language":"python","code":"print(1)"}"#;
    for _ in 0..2 {
        let (status, value) = send(
            &app,
            request("POST", "/v1/jobs", Some("test-key"), Some(body.into())),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{value}");
    }
    let (status, value) = send(
        &app,
        request("POST", "/v1/jobs", Some("test-key"), Some(body.into())),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{value}");
    assert_eq!(value["error"]["code"], "tenant_queue_saturated");

    let (status, value) = send(
        &app,
        request("POST", "/v1/jobs", Some("other-key"), Some(body.into())),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
}

#[tokio::test]
async fn retained_storage_quota_maps_tenant_capacity_without_charging_other_tenants() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let mut cfg = test_config(&db);
    cfg.storage_tenant_mb = 64;
    cfg.storage_global_mb = 128;
    let store = Arc::new(
        Store::open_with_limits(&db, cfg.storage_limits())
            .await
            .expect("open limited store"),
    );
    let (app, _state, _queue_rx) = coop_server::build_app(cfg, store).await.expect("build app");
    let body = r#"{"language":"python","code":"print(1)"}"#;
    let (status, first) = send(
        &app,
        request("POST", "/v1/jobs", Some("test-key"), Some(body.into())),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, rejected) = send(
        &app,
        request("POST", "/v1/jobs", Some("test-key"), Some(body.into())),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{rejected}");
    assert_eq!(rejected["error"]["code"], "tenant_storage_quota");

    let (status, other) = send(
        &app,
        request("POST", "/v1/jobs", Some("other-key"), Some(body.into())),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{other}");
}

#[tokio::test]
async fn idempotency_replays_original_job_and_rejects_fingerprint_reuse() {
    let app = spawn_app().await;
    let make = |body: &str| {
        Request::builder()
            .method("POST")
            .uri("/v1/jobs")
            .header(header::AUTHORIZATION, "Bearer test-key")
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", "sdk-retry-1")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let first = app.clone().oneshot(make(
        r#"{"language":"python","code":"print(42)","limits":{"mem_mb":128}}"#,
    ));
    let first = first.await.unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    assert_eq!(first.headers()["idempotency-replayed"], "false");
    let first_location = first.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_string();
    let first_body = axum::body::to_bytes(first.into_body(), 1 << 20)
        .await
        .unwrap();
    let first_json: serde_json::Value = serde_json::from_slice(&first_body).unwrap();

    // Reordered object members canonicalize to the same parsed JobSpec.
    let second = app
        .clone()
        .oneshot(make(
            r#"{"limits":{"mem_mb":128},"code":"print(42)","language":"python"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::CREATED);
    assert_eq!(second.headers()["idempotency-replayed"], "true");
    assert_eq!(second.headers()[header::LOCATION], first_location);
    let second_body = axum::body::to_bytes(second.into_body(), 1 << 20)
        .await
        .unwrap();
    let second_json: serde_json::Value = serde_json::from_slice(&second_body).unwrap();
    assert_eq!(second_json["job_id"], first_json["job_id"]);

    let (status, conflict) = send(
        &app,
        make(r#"{"language":"python","code":"print(43)","limits":{"mem_mb":128}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{conflict}");
    assert_eq!(conflict["error"]["code"], "idempotency_key_reused");
}

#[tokio::test]
async fn shutdown_is_sticky_and_queued_work_does_not_start_after_it() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let cfg = test_config(&db);
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (_app, state, queue_rx) = coop_server::build_app(cfg, store).await.expect("build app");
    state
        .store
        .create_job_with_event(
            "shutdown-queued",
            "t1",
            "python",
            r#"{"language":"python","code":"print(1)"}"#,
        )
        .await
        .unwrap();
    state
        .admission
        .try_reserve("t1", 256)
        .unwrap()
        .send("shutdown-queued".to_string());

    // No watch receiver existed when this was published. A late subscriber
    // must still observe shutdown, and newly spawned workers must not start.
    state.begin_shutdown();
    assert!(*state.shutdown.subscribe().borrow());
    let workers = scheduler::spawn_workers(state.clone(), queue_rx);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let row = state
        .store
        .get_job("shutdown-queued")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "queued");
    let _ = workers.shutdown(&state, Duration::from_millis(250)).await;
}

#[tokio::test]
async fn readiness_monitor_exits_when_shutdown_was_already_sticky() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let cfg = test_config(&db);
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (_app, state, _queue_rx) = coop_server::build_app(cfg, store).await.expect("build app");
    state.begin_shutdown();

    let monitor = coop_server::readiness::spawn_monitor(state);
    tokio::time::timeout(Duration::from_millis(100), monitor)
        .await
        .expect("sticky shutdown stops monitor before its first tick")
        .expect("monitor joins cleanly");
}

#[tokio::test]
async fn long_result_wait_returns_promptly_when_shutdown_begins() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let cfg = test_config(&db);
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (app, state, _queue_rx) = coop_server::build_app(cfg, store).await.expect("build app");
    let (status, accepted) = send(
        &app,
        request(
            "POST",
            "/v1/jobs",
            Some("test-key"),
            Some(r#"{"language":"python","code":"print(1)"}"#.to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{accepted}");
    let id = accepted["job_id"].as_str().unwrap().to_string();
    let wait_app = app.clone();
    let waiter = tokio::spawn(async move {
        send(
            &wait_app,
            request(
                "GET",
                &format!("/v1/jobs/{id}/result?wait_seconds=300"),
                Some("test-key"),
                None,
            ),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(25)).await;
    state.begin_shutdown();
    let (status, body) = tokio::time::timeout(Duration::from_millis(500), waiter)
        .await
        .expect("result wait stopped on shutdown")
        .expect("wait task joined");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "shutting_down");
}

#[tokio::test]
async fn result_wait_honors_budget_before_recovery_registers_completion_watch() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let cfg = test_config(&db);
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (_unused_app, mut state, _queue_rx) = coop_server::build_app(cfg, Arc::clone(&store))
        .await
        .expect("build app");
    state.result_wait_admission = coop_server::LifetimeAdmission::new(1, 1);
    let app = coop_server::routes::router(state.clone());
    let job_id = "recovery-row-before-completion-watch";
    store
        .create_job_with_event(job_id, "t1", "bash", r#"{"language":"bash","code":":"}"#)
        .await
        .expect("create queued recovery fixture");
    assert!(
        state.bus.completion(job_id).is_none(),
        "fixture deliberately predates recovery watch registration"
    );

    let wait_app = app.clone();
    let waiter = tokio::spawn(async move {
        send(
            &wait_app,
            request(
                "GET",
                &format!("/v1/jobs/{job_id}/result?wait_seconds=2"),
                Some("test-key"),
                None,
            ),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Ok(probe) = state.result_wait_admission.try_acquire("t1") {
            drop(probe);
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("result handler entered its requested wait");
    store
        .finalize_with_event(job_id, "cancelled", None, 0, None)
        .await
        .expect("finalize recovery fixture");

    let (status, body) = tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("durable terminal polling woke the result request")
        .expect("result task joined");
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "cancelled", "{body}");
}

#[tokio::test]
async fn cancelled_envelope_reclaims_capacity_only_after_scheduler_dequeues_it() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let mut cfg = test_config(&db);
    cfg.workers = 1;
    cfg.tenant_concurrency = 1;
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (_app, state, queue_rx) = coop_server::build_app(cfg, store).await.expect("build app");
    state
        .store
        .create_job_with_event(
            "cancelled-envelope",
            "t1",
            "python",
            r#"{"language":"python","code":"print(1)"}"#,
        )
        .await
        .unwrap();
    state
        .store
        .cancel_queued_with_event("cancelled-envelope", "t1", None)
        .await
        .unwrap();
    state
        .admission
        .try_reserve("t1", 256)
        .unwrap()
        .send("cancelled-envelope".to_string());
    assert_eq!(state.admission.depth(), 1);
    let workers = scheduler::spawn_workers(state.clone(), queue_rx);
    for _ in 0..100 {
        if state.admission.depth() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        state.admission.depth(),
        0,
        "cancelled envelope leaked its slot"
    );
    let _ = workers.shutdown(&state, Duration::from_millis(250)).await;
}

#[tokio::test]
async fn rate_limit_errors_include_retry_after_and_request_id() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let mut cfg = test_config(&db);
    cfg.rate_per_min = 1;
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (app, _state, _queue_rx) = coop_server::build_app(cfg, store).await.expect("build app");

    let first = app
        .clone()
        .oneshot(request("GET", "/v1/whoami", Some("test-key"), None))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let second = app
        .oneshot(request("GET", "/v1/whoami", Some("test-key"), None))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(second.headers().contains_key("retry-after"));
    assert!(second.headers().contains_key("x-request-id"));
    let body = axum::body::to_bytes(second.into_body(), 1 << 20)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["code"], "rate_limit_exceeded");
    assert_eq!(body["error"]["retryable"], true);
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
async fn minimum_isolation_is_checked_atomically_before_persistence() {
    let app = spawn_app().await;
    let payload = serde_json::json!({
        "language": "python",
        "code": "print('must not queue')",
        "requirements": {"minimum_isolation": "gvisor-application-kernel"}
    })
    .to_string();
    let (status, body) = send(
        &app,
        request("POST", "/v1/jobs", Some("test-key"), Some(payload)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "minimum_isolation_unsatisfied");

    let (status, jobs) = send(
        &app,
        request("GET", "/v1/jobs?limit=100", Some("test-key"), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(jobs["items"].as_array().unwrap().is_empty(), "{jobs}");
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
    let leaked = other_jobs["items"]
        .as_array()
        .map(|a| a.iter().any(|j| j["job_id"] == *job_id))
        .unwrap_or(false);
    assert!(!leaked, "tenant t2 must not see tenant t1 jobs in listings");
}

#[tokio::test]
async fn list_jobs_supports_language_filters_and_stable_cursors() {
    let app = spawn_app().await;
    for (language, code) in [
        ("python", "print('one')"),
        ("python", "print('middle')"),
        ("python", "print('two')"),
    ] {
        let payload = serde_json::json!({ "language": language, "code": code }).to_string();
        let (status, body) = send(
            &app,
            request("POST", "/v1/jobs", Some("test-key"), Some(payload)),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    let (status, excluded) = send(
        &app,
        request(
            "GET",
            "/v1/jobs?language=node&limit=10",
            Some("test-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{excluded}");
    assert!(excluded["items"].as_array().is_some_and(Vec::is_empty));

    let (status, first) = send(
        &app,
        request(
            "GET",
            "/v1/jobs?language=python&limit=1",
            Some("test-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let first_items = first["items"].as_array().expect("items");
    assert_eq!(first_items.len(), 1, "{first}");
    assert_eq!(first_items[0]["language"], "python");
    for blob_field in ["requested_spec", "effective_spec", "receipt"] {
        assert!(
            first_items[0].get(blob_field).is_none(),
            "list projection must not load/expose {blob_field}: {first}"
        );
    }
    let first_id = first_items[0]["job_id"].as_str().unwrap().to_string();
    let cursor = first["next_cursor"].as_str().expect("next cursor");

    let (status, second) = send(
        &app,
        request(
            "GET",
            &format!("/v1/jobs?language=python&limit=1&cursor={cursor}"),
            Some("test-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let second_items = second["items"].as_array().expect("items");
    assert_eq!(second_items.len(), 1, "{second}");
    assert_eq!(second_items[0]["language"], "python");
    assert_ne!(second_items[0]["job_id"], first_id);
}

#[tokio::test]
async fn identity_capabilities_status_and_readiness_are_truthful() {
    let app = spawn_app().await;
    let (status, ready) = send(&app, request("GET", "/readyz", None, None)).await;
    assert_eq!(status, StatusCode::OK, "{ready}");
    assert_eq!(ready["ok"], true);

    let (status, who) = send(&app, request("GET", "/v1/whoami", Some("test-key"), None)).await;
    assert_eq!(status, StatusCode::OK, "{who}");
    assert_eq!(who["tenant"], "t1");

    let (status, capabilities) = send(
        &app,
        request("GET", "/v1/capabilities", Some("test-key"), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{capabilities}");
    assert_eq!(capabilities["execution"]["isolated"], false);
    assert_eq!(capabilities["execution"]["seccomp"], false);
    assert_eq!(capabilities["execution"]["networking"], "host");
    assert_eq!(capabilities["features"]["stream_tickets"], true);
    assert_eq!(capabilities["limits"]["mem_mb_max"], 1024);
    assert_eq!(capabilities["limits"]["concurrent_mem_mb_max"], 4096);

    let (status, service) = send(&app, request("GET", "/v1/status", Some("test-key"), None)).await;
    assert_eq!(status, StatusCode::OK, "{service}");
    assert_eq!(service["environment"], "development");
    assert_eq!(service["scheduler"]["queue_capacity"], 64);
    assert_eq!(service["scheduler"]["queue_depth"], 0);
}

#[tokio::test]
async fn status_queue_depth_is_tenant_scoped() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let cfg = test_config(&db);
    let store = Arc::new(Store::open(&db).await.unwrap());
    let (app, state, _queue_rx) = coop_server::build_app(cfg, store).await.unwrap();
    state
        .admission
        .try_reserve("t2", 256)
        .unwrap()
        .send("status-t2".to_string());
    let (_, t1) = send(&app, request("GET", "/v1/status", Some("test-key"), None)).await;
    let (_, t2) = send(&app, request("GET", "/v1/status", Some("other-key"), None)).await;
    assert_eq!(t1["scheduler"]["queue_depth"], 0);
    assert_eq!(t2["scheduler"]["queue_depth"], 1);
}

#[tokio::test]
async fn queued_job_keeps_acceptance_memory_ceiling_when_server_limit_increases() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let mut low = test_config(&db);
    low.max_job_mem_mb = 512;
    let store = Arc::new(Store::open(&db).await.unwrap());
    let (first_app, first_state, first_queue) = coop_server::build_app(low, Arc::clone(&store))
        .await
        .unwrap();
    let (status, submitted) = send(
        &first_app,
        request(
            "POST",
            "/v1/jobs",
            Some("test-key"),
            Some(
                r#"{"language":"python","code":"print('memory-policy')","limits":{"mem_mb":1024}}"#
                    .into(),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{submitted}");
    let job_id = submitted["job_id"].as_str().unwrap().to_string();
    drop((first_app, first_state, first_queue));

    let mut high = test_config(&db);
    high.max_job_mem_mb = 1024;
    let (app, state, queue_rx) = coop_server::build_app(high, Arc::clone(&store))
        .await
        .unwrap();
    let queued = store.queued_jobs_page(None, 10).await.unwrap();
    let row = queued.iter().find(|row| row.job_id == job_id).unwrap();
    assert_eq!(row.requested_mem_mb, 512);
    state.bus.register(&job_id);
    state
        .admission
        .reserve_recovery(&row.tenant, state.cfg.clamp_mem_mb(row.requested_mem_mb))
        .await
        .unwrap()
        .send(job_id.clone());
    scheduler::spawn_workers(state, queue_rx);
    let detail = wait_terminal(&app, &job_id).await;
    assert_eq!(detail["status"], "succeeded", "{detail}");
    assert_eq!(
        store.job_requested_mem_mb(&job_id).await.unwrap(),
        Some(512)
    );
}

#[tokio::test]
async fn stream_tickets_are_job_bound_and_one_use() {
    let app = spawn_app().await;
    let first = submit_python(&app, "print('first')").await;
    let second = submit_python(&app, "print('second')").await;
    let (status, ticket) = send(
        &app,
        request(
            "POST",
            &format!("/v1/jobs/{first}/stream-ticket"),
            Some("test-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ticket}");
    let stream_url = ticket["stream_url"].as_str().expect("stream_url");
    let query = stream_url.split_once('?').expect("ticket query").1;

    // A wrong job path cannot consume or use the grant.
    let (wrong_status, wrong) = send(
        &app,
        request(
            "GET",
            &format!("/v1/jobs/{second}/stream?{query}"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(wrong_status, StatusCode::UNAUTHORIZED, "{wrong}");
    assert_eq!(wrong["error"]["code"], "invalid_stream_ticket");

    // The correct path passes authentication (and then fails only because
    // this test request is not a WebSocket upgrade), consuming the ticket.
    let (first_use, _) = send(&app, request("GET", stream_url, None, None)).await;
    assert_ne!(first_use, StatusCode::UNAUTHORIZED);
    let (second_use, body) = send(&app, request("GET", stream_url, None, None)).await;
    assert_eq!(second_use, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["code"], "invalid_stream_ticket");
}

#[tokio::test]
async fn stream_ticket_is_rejected_after_shutdown_becomes_sticky() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let cfg = test_config(&db);
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (_unused_app, state, _queue_rx) = coop_server::build_app(cfg, Arc::clone(&store))
        .await
        .expect("build app");
    let job_id = "ticket-after-shutdown";
    store
        .create_job_with_event(job_id, "t1", "bash", r#"{"language":"bash","code":":"}"#)
        .await
        .expect("create ticket fixture");
    state.begin_shutdown();
    let app = coop_server::routes::router(state);

    let (status, body) = send(
        &app,
        request(
            "POST",
            &format!("/v1/jobs/{job_id}/stream-ticket"),
            Some("test-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "shutting_down", "{body}");
}

#[tokio::test]
async fn production_never_accepts_api_keys_in_stream_query_strings() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let cfg = test_config(&db);
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (_app, mut state, _queue_rx) = coop_server::build_app(cfg.clone(), store)
        .await
        .expect("build app");
    let mut production_cfg = cfg;
    production_cfg.production = true;
    production_cfg.unsafe_allow_naive = true;
    state.cfg = Arc::new(production_cfg);
    let app = coop_server::routes::router(state);
    let (status, body) = send(
        &app,
        request("GET", "/v1/jobs/unknown/stream?key=test-key", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["code"], "missing_api_key", "{body}");
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
    assert_eq!(final_view["requested_spec"]["language"], "python");
    assert_eq!(
        final_view["effective_spec"]["limits"]["allow_network"],
        true
    );
    assert_eq!(final_view["effective_spec"]["limits"]["wall_seconds"], 15);
    for unenforced in ["cpu_seconds", "mem_mb", "max_pids", "max_file_mb"] {
        assert!(
            final_view["effective_spec"]["limits"][unenforced].is_null(),
            "development backend must not claim {unenforced}: {final_view}"
        );
    }
    assert_eq!(final_view["execution_policy"]["bootstrap_ready"], true);
    assert_eq!(final_view["execution_policy"]["isolated"], false);
    assert_eq!(
        final_view["execution_policy"]["limit_enforcement"],
        serde_json::json!({
            "wall_seconds": true,
            "cpu_seconds": false,
            "mem_mb": false,
            "max_pids": false,
            "max_file_mb": false,
        })
    );
    assert_eq!(final_view["execution_policy"]["network_allowed"], true);
    assert_eq!(final_view["execution_policy"]["networking"], "host");
    assert_eq!(final_view["execution_policy"]["private_rootfs"], false);
    assert_eq!(final_view["execution_policy"]["dedicated_bootstrap"], false);
    assert_eq!(final_view["receipt"]["network_allowed"], true);
    assert_eq!(final_view["receipt"]["networking"], "host");
    assert_eq!(final_view["receipt"]["private_rootfs"], false);
    assert_eq!(final_view["receipt"]["dedicated_bootstrap"], false);
    assert_eq!(final_view["receipt"]["bootstrap_ready"], true);
    assert_eq!(final_view["receipt"]["isolated"], false);
    assert_eq!(
        final_view["receipt"]["effective_limits"]["wall_seconds"],
        15
    );
    assert!(final_view["receipt"]["effective_limits"]["cpu_seconds"].is_null());
    assert!(final_view["receipt"].is_object(), "{final_view}");
    assert!(final_view["receipt_sha256"].is_string(), "{final_view}");
    assert_eq!(
        final_view["receipt_sha256"], final_view["receipt"]["receipt_sha256"],
        "{final_view}"
    );
    assert_eq!(final_view["receipt"]["event_chain"]["version"], 1);

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
    let stdout: String = replay["events"]
        .as_array()
        .expect("replay array")
        .iter()
        .filter(|e| e["kind"] == "stdout")
        .filter_map(|e| e["data"]["line"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(stdout.contains("hello from coop"), "stdout was: {stdout}");

    let kinds: Vec<&str> = replay["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"started"));
    assert!(kinds.contains(&"finished"));
    assert!(replay["next_cursor"].is_i64(), "{replay}");
    assert!(replay["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["hash_version"] == 1 && event["event_hash"].is_string()));

    let first_seq = replay["events"][0]["seq"].as_i64().unwrap();
    let (_, after) = send(
        &app,
        request(
            "GET",
            &format!("/v1/jobs/{job_id}/replay?after={first_seq}"),
            Some("test-key"),
            None,
        ),
    )
    .await;
    assert!(after["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["seq"].as_i64().unwrap() > first_seq));
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
                    r#"{"language":"python","code":"import time; time.sleep(30)","limits":{"wall_seconds":30}}"#.into(),
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
            Some(r#"{"language":"python","code":"print('should-never-run')","limits":{"wall_seconds":30}}"#.into()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let job_id = body["job_id"].as_str().expect("job_id").to_string();

    let (status, cancellation) = send(
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
    assert_eq!(cancellation["cancellation_requested"], true);
    assert_eq!(cancellation["already_terminal"], false);
    assert_eq!(cancellation["job"]["job_id"], job_id);

    // The job must reach a terminal `cancelled` state without running.
    let view = wait_terminal(&app, &job_id).await;
    assert_eq!(view["status"], "cancelled", "{view}");

    // Cancelling again is a typed idempotent observation and emits no second
    // terminal transition.
    let (status, cancellation) = send(
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
    assert_eq!(cancellation["cancellation_requested"], false);
    assert_eq!(cancellation["already_terminal"], true);
    assert_eq!(cancellation["job"]["status"], "cancelled");

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
            Some(r#"{"language":"python","code":"import time; time.sleep(60)","limits":{"wall_seconds":60}}"#.into()),
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
            Some(
                r#"{"language":"python","code":"print('t1')","limits":{"wall_seconds":15}}"#.into(),
            ),
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
async fn tenant_waiters_do_not_occupy_workers_and_starve_other_tenants() {
    if !python_available() {
        eprintln!("skipping: no python interpreter on PATH");
        return;
    }
    let app = spawn_app_with_limits(2, 1).await;

    let first = submit_python(&app, "import time; time.sleep(5)").await;
    for _ in 0..100 {
        let (_, view) = send(
            &app,
            request("GET", &format!("/v1/jobs/{first}"), Some("test-key"), None),
        )
        .await;
        if view["status"] == "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // A second t1 job cannot acquire t1's permit. It must remain in the fair
    // dispatcher, not consume worker 2 while waiting.
    let second = submit_python(&app, "import time; time.sleep(5)").await;
    let (status, other) = send(
        &app,
        request(
            "POST",
            "/v1/jobs",
            Some("other-key"),
            Some(r#"{"language":"python","code":"print('other-tenant')"}"#.to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{other}");
    let other_id = other["job_id"].as_str().unwrap().to_string();

    let started = std::time::Instant::now();
    let other_view = wait_terminal_with_key(&app, &other_id, "other-key").await;
    assert_eq!(other_view["status"], "succeeded", "{other_view}");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "other tenant was starved behind a tenant semaphore waiter"
    );

    for id in [first, second] {
        let _ = send(
            &app,
            request("DELETE", &format!("/v1/jobs/{id}"), Some("test-key"), None),
        )
        .await;
    }
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
            Some(
                r#"{"language":"python","code":"print('hi')","limits":{"wall_seconds":15}}"#.into(),
            ),
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
    assert!(text.contains("coop_jobs_current"), "{text}");
    assert!(text.contains("coop_job_lifecycle_owners_current"), "{text}");
    assert!(!text.contains("coop_running_jobs"), "{text}");

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/metrics")
                .header(header::AUTHORIZATION, "Bearer test-metrics-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("global metrics");
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("global metrics body");
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains(
            "coop_http_server_requests_total{method=\"GET\",route=\"/v1/metrics\",status_class=\"2xx\"} 1"
        ),
        "{text}"
    );
    assert!(
        text.contains("coop_jobs_submitted_total{language=\"python\"} 1"),
        "{text}"
    );
}

#[tokio::test]
async fn global_metrics_are_separately_authorized_bounded_and_negotiated() {
    let app = spawn_app().await;

    for key in [None, Some("test-key")] {
        let response = app
            .clone()
            .oneshot(request("GET", "/metrics", key, None))
            .await
            .expect("metrics response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers().contains_key("x-request-id"));
        assert_eq!(
            response
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer realm=\"coop-metrics\"")
        );
    }

    let openmetrics = Request::builder()
        .method("GET")
        .uri("/metrics")
        .header(header::AUTHORIZATION, "Bearer test-metrics-token")
        .header(header::ACCEPT, "application/openmetrics-text;version=1.0.0")
        .body(Body::empty())
        .unwrap();
    let response = app
        .clone()
        .oneshot(openmetrics)
        .await
        .expect("OpenMetrics response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/openmetrics-text; version=1.0.0; charset=utf-8")
    );
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("metrics body");
    let text = String::from_utf8(bytes.to_vec()).expect("UTF-8 metrics");
    assert!(text.ends_with("# EOF\n"), "{text}");
    for family in [
        "coop_http_server_request_duration_seconds",
        "coop_admission_rejections_total",
        "coop_queue_depth",
        "coop_executions_active",
        "coop_storage_errors_total",
        "coop_output_truncations_total",
        "coop_recovered_jobs_total",
        "coop_retention_runs_total",
        "coop_capacity_used",
        "coop_build_info",
    ] {
        assert!(text.contains(family), "missing {family}");
    }
    for forbidden in ["tenant=", "job_id=", "request_id=", "trace_id="] {
        assert!(!text.contains(forbidden), "leaked {forbidden}");
    }

    let legacy = Request::builder()
        .method("GET")
        .uri("/metrics")
        .header(header::AUTHORIZATION, "Bearer test-metrics-token")
        .header(header::ACCEPT, "text/plain;version=0.0.4")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(legacy).await.expect("legacy metrics response");
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/plain; version=0.0.4; charset=utf-8")
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("legacy body");
    assert!(!bytes.ends_with(b"# EOF\n"));
}

#[tokio::test]
async fn ingress_request_id_covers_success_early_error_and_ignores_caller_value() {
    let app = spawn_app().await;
    let success = app
        .clone()
        .oneshot(request("GET", "/healthz", None, None))
        .await
        .expect("health response");
    let success_id = success
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .expect("success request ID");
    assert_eq!(
        uuid::Uuid::parse_str(success_id)
            .expect("UUID request ID")
            .get_version_num(),
        7
    );

    let unauthorized = Request::builder()
        .method("GET")
        .uri("/v1/whoami")
        .header("x-request-id", "caller-controlled")
        .header(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        )
        .header("baggage", "secret=must-not-propagate")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(unauthorized).await.expect("auth response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let header_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .expect("error request ID")
        .to_string();
    assert_ne!(header_id, "caller-controlled");
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("error body");
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["request_id"], header_id);
    assert!(!body.to_string().contains("must-not-propagate"));
    assert!(!body
        .to_string()
        .contains("4bf92f3577b34da6a3ce929d0e0e4736"));
}

// ---------------------------------------------------------------------------
// GET /v1/jobs/{id}/result
// ---------------------------------------------------------------------------

async fn submit_python(app: &Router, code: &str) -> String {
    let payload =
        serde_json::json!({ "language": "python", "code": code, "limits": { "wall_seconds": 15 } })
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
    let job_id = submit_python(&app, "print('scoped')").await;

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
    if !python_available() {
        eprintln!("skipping: no python interpreter on PATH");
        return;
    }
    let job_id = submit_python(&app, "import time; time.sleep(5)").await;

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
async fn result_wait_capacity_is_tenant_global_and_only_for_actual_waits() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let mut cfg = test_config(&db);
    cfg.api_keys
        .insert("third-key".to_string(), "t3".to_string());
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (_unused_app, mut state, _queue_rx) = coop_server::build_app(cfg, Arc::clone(&store))
        .await
        .expect("build app");
    state.result_wait_admission = coop_server::LifetimeAdmission::new(2, 1);
    let app = coop_server::routes::router(state.clone());

    for (id, tenant) in [
        ("result-cap-t1", "t1"),
        ("result-cap-t2", "t2"),
        ("result-cap-t3", "t3"),
        ("result-cap-terminal", "t1"),
    ] {
        store
            .create_job_with_event(id, tenant, "bash", r#"{"language":"bash","code":":"}"#)
            .await
            .expect("create queued job");
        state.bus.register(id);
    }
    store
        .finalize_with_event("result-cap-terminal", "cancelled", None, 0, None)
        .await
        .expect("finalize terminal fixture");

    let held_t1 = state
        .result_wait_admission
        .try_acquire("t1")
        .expect("hold t1 wait slot");
    let (status, body) = send(
        &app,
        request(
            "GET",
            "/v1/jobs/result-cap-t1/result?wait_seconds=300",
            Some("test-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"]["code"], "tenant_result_wait_capacity");

    let held_t2 = state
        .result_wait_admission
        .try_acquire("t2")
        .expect("hold t2 wait slot");
    let (status, body) = send(
        &app,
        request(
            "GET",
            "/v1/jobs/result-cap-t3/result?wait_seconds=300",
            Some("third-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "result_wait_capacity");

    let (status, body) = send(
        &app,
        request(
            "GET",
            "/v1/jobs/result-cap-t1/result?wait_seconds=0",
            Some("test-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let (status, body) = send(
        &app,
        request(
            "GET",
            "/v1/jobs/result-cap-terminal/result?wait_seconds=300",
            Some("test-key"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    drop((held_t1, held_t2));
    let _reclaimed = state
        .result_wait_admission
        .try_acquire("t1")
        .expect("dropping waits reclaims capacity");
}

#[tokio::test]
async fn large_response_capacity_guards_all_blob_endpoints_and_reclaims() {
    let db = std::env::temp_dir().join(format!("coop-test-{}.db", uuid::Uuid::now_v7()));
    let mut cfg = test_config(&db);
    cfg.api_keys
        .insert("third-key".to_string(), "t3".to_string());
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (_unused_app, mut state, _queue_rx) = coop_server::build_app(cfg, Arc::clone(&store))
        .await
        .expect("build app");
    state.large_response_admission = coop_server::LifetimeAdmission::new(2, 1);
    let app = coop_server::routes::router(state.clone());

    for (id, tenant) in [
        ("response-cap-t1", "t1"),
        ("response-cap-t2", "t2"),
        ("response-cap-t3", "t3"),
    ] {
        store
            .create_job_with_event(id, tenant, "bash", r#"{"language":"bash","code":":"}"#)
            .await
            .expect("create queued job");
    }

    let held_t1 = state
        .large_response_admission
        .try_acquire("t1")
        .expect("hold t1 response slot");
    for path in [
        "/v1/jobs/response-cap-t1",
        "/v1/jobs/response-cap-t1/replay",
        "/v1/jobs/response-cap-t1/result?wait_seconds=0",
    ] {
        let (status, body) = send(&app, request("GET", path, Some("test-key"), None)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{path}: {body}");
        assert_eq!(body["error"]["code"], "tenant_response_capacity");
    }

    let held_t2 = state
        .large_response_admission
        .try_acquire("t2")
        .expect("hold t2 response slot");
    let (status, body) = send(
        &app,
        request("GET", "/v1/jobs/response-cap-t3", Some("third-key"), None),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "response_capacity");

    drop((held_t1, held_t2));
    for (path, expected) in [
        ("/v1/jobs/response-cap-t1", StatusCode::OK),
        ("/v1/jobs/response-cap-t1/replay", StatusCode::OK),
        (
            "/v1/jobs/response-cap-t1/result?wait_seconds=0",
            StatusCode::ACCEPTED,
        ),
    ] {
        let (status, body) = send(&app, request("GET", path, Some("test-key"), None)).await;
        assert_eq!(status, expected, "{path}: {body}");
    }
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
