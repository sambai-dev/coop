#![cfg(target_os = "linux")]

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use coop_server::config::Config;
use coop_server::{routes, scheduler};
use coop_store::Store;
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Semaphore};
use tower::ServiceExt;

const TERMINAL: [&str; 5] = ["succeeded", "failed", "timed_out", "oom_killed", "error"];

fn is_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

async fn spawn_app() -> Router {
    let db = std::env::temp_dir().join(format!("coop-hostile-{}.db", uuid::Uuid::now_v7()));
    let mut api_keys = HashMap::new();
    api_keys.insert("test-key".to_string(), "t1".to_string());
    let cfg = Config {
        addr: "127.0.0.1:0".to_string(),
        db_path: db.to_string_lossy().into_owned(),
        api_keys,
        workers: 2,
        tenant_concurrency: 4,
        rate_per_min: 10_000,
        sandbox: "ns".to_string(),
        python_bin: None,
        node_bin: None,
        bash_bin: None,
    };
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (app, state, queue_rx) = coop_server::build_app(cfg, store);
    scheduler::spawn_workers(state, queue_rx);
    app
}

async fn submit(app: &Router, language: &str, code: &str, limits: serde_json::Value) -> String {
    let payload = serde_json::json!({ "language": language, "code": code, "limits": limits });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/jobs")
        .header(header::AUTHORIZATION, "Bearer test-key")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    v["job_id"].as_str().unwrap().to_string()
}

async fn wait_terminal(app: &Router, job_id: &str) -> (String, f64) {
    let started = std::time::Instant::now();
    for _ in 0..300 {
        let req = Request::builder()
            .method("GET")
            .uri(format!("/v1/jobs/{job_id}"))
            .header(header::AUTHORIZATION, "Bearer test-key")
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let status = v["status"].as_str().unwrap_or("").to_string();
        if TERMINAL.contains(&status.as_str()) {
            return (status, started.elapsed().as_secs_f64());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("job {job_id} never finished");
}

async fn assert_host_still_serves(app: &Router) {
    let id = submit(
        app,
        "python",
        "print('host-alive')",
        serde_json::json!({ "wall_seconds": 10 }),
    )
    .await;
    let (status, _) = wait_terminal(app, &id).await;
    assert_eq!(
        status, "succeeded",
        "host must stay healthy after hostile job"
    );
}

const FORK_BOMB: &str = include_str!("../../../hostile-jobs/fork_bomb.sh");
const MEMORY_BOMB: &str = include_str!("../../../hostile-jobs/memory_bomb.py");
const INFINITE_LOOP: &str = include_str!("../../../hostile-jobs/infinite_loop.py");
const NETWORK_PROBE: &str = include_str!("../../../hostile-jobs/network_probe.py");
const DISK_FILLER: &str = include_str!("../../../hostile-jobs/disk_filler.py");
const ESCAPE_PROBE: &str = include_str!("../../../hostile-jobs/escape_probe.py");
const PID_BOMB: &str = include_str!("../../../hostile-jobs/pid_bomb.py");

#[tokio::test]
#[ignore]
async fn contains_fork_bomb() {
    if !is_root() {
        eprintln!("skipping: needs root");
        return;
    }
    let app = spawn_app().await;
    let id = submit(
        &app,
        "bash",
        FORK_BOMB,
        serde_json::json!({ "wall_seconds": 8, "max_pids": 32 }),
    )
    .await;
    let (status, elapsed) = wait_terminal(&app, &id).await;
    assert_ne!(status, "running");
    assert!(
        elapsed < 25.0,
        "fork bomb must be contained quickly, took {elapsed}s"
    );
    assert_host_still_serves(&app).await;
}

#[tokio::test]
#[ignore]
async fn contains_memory_bomb() {
    if !is_root() {
        eprintln!("skipping: needs root");
        return;
    }
    let app = spawn_app().await;
    let id = submit(
        &app,
        "python",
        MEMORY_BOMB,
        serde_json::json!({ "wall_seconds": 15, "mem_mb": 128 }),
    )
    .await;
    let (status, elapsed) = wait_terminal(&app, &id).await;
    assert!(
        matches!(status.as_str(), "oom_killed" | "failed"),
        "memory bomb must die by OOM or allocation failure, got {status}"
    );
    assert!(elapsed < 30.0);
    assert_host_still_serves(&app).await;
}

#[tokio::test]
#[ignore]
async fn kills_infinite_loop_on_wall_clock() {
    if !is_root() {
        eprintln!("skipping: needs root");
        return;
    }
    let app = spawn_app().await;
    let id = submit(
        &app,
        "python",
        INFINITE_LOOP,
        serde_json::json!({ "wall_seconds": 3 }),
    )
    .await;
    let (status, elapsed) = wait_terminal(&app, &id).await;
    assert_eq!(
        status, "timed_out",
        "infinite loop should hit wall clock, got {status}"
    );
    assert!(elapsed < 15.0);
    assert_host_still_serves(&app).await;
}

#[tokio::test]
#[ignore]
async fn network_is_disabled_by_default() {
    if !is_root() {
        eprintln!("skipping: needs root");
        return;
    }
    let app = spawn_app().await;
    let id = submit(
        &app,
        "python",
        NETWORK_PROBE,
        serde_json::json!({ "wall_seconds": 10 }),
    )
    .await;
    let (status, _) = wait_terminal(&app, &id).await;
    assert_eq!(
        status, "succeeded",
        "probe exits 0 only when network is blocked"
    );
    assert_host_still_serves(&app).await;
}

#[tokio::test]
#[ignore]
async fn disk_filler_hits_filesystem_cap() {
    if !is_root() {
        eprintln!("skipping: needs root");
        return;
    }
    let app = spawn_app().await;
    let id = submit(
        &app,
        "python",
        DISK_FILLER,
        serde_json::json!({ "wall_seconds": 20, "mem_mb": 128 }),
    )
    .await;
    let (status, elapsed) = wait_terminal(&app, &id).await;
    assert_eq!(
        status, "failed",
        "disk filler must fail against capped tmpfs"
    );
    assert!(elapsed < 40.0);
    assert_host_still_serves(&app).await;
}

#[tokio::test]
#[ignore]
async fn escape_probes_fail() {
    if !is_root() {
        eprintln!("skipping: needs root");
        return;
    }
    let app = spawn_app().await;
    let id = submit(
        &app,
        "python",
        ESCAPE_PROBE,
        serde_json::json!({ "wall_seconds": 10 }),
    )
    .await;
    let (status, _) = wait_terminal(&app, &id).await;
    assert_eq!(
        status, "failed",
        "escape probe must not succeed inside sandbox"
    );
    assert_host_still_serves(&app).await;
}

#[tokio::test]
#[ignore]
async fn pid_bomb_is_capped() {
    if !is_root() {
        eprintln!("skipping: needs root");
        return;
    }
    let app = spawn_app().await;
    let id = submit(
        &app,
        "python",
        PID_BOMB,
        serde_json::json!({ "wall_seconds": 8, "max_pids": 32 }),
    )
    .await;
    let (status, elapsed) = wait_terminal(&app, &id).await;
    assert_ne!(status, "running");
    assert!(elapsed < 25.0);
    assert_host_still_serves(&app).await;
}
