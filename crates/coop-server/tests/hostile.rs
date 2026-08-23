#![cfg(target_os = "linux")]

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

fn init_tracing() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new("info,coop_server=debug")
                }),
            )
            .with_test_writer()
            .try_init();
    });
}

fn is_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

fn preflight() -> bool {
    init_tracing();

    if !is_root() {
        eprintln!("SKIP: hostile suite needs root (namespaces + cgroup delegation)");
        return false;
    }

    let controllers = std::path::Path::new("/sys/fs/cgroup/cgroup.controllers");
    if !controllers.exists() {
        eprintln!(
            "SKIP: cgroup v2 unified hierarchy not found ({controllers:?} missing); \
             host may be running cgroup v1"
        );
        return false;
    }

    eprintln!("preflight: root=yes, cgroup-v2=yes — running containment suite");
    true
}

async fn spawn_app_with_root() -> (Router, String) {
    let db = std::env::temp_dir().join(format!("coop-hostile-{}.db", uuid::Uuid::now_v7()));
    let mut api_keys = HashMap::new();
    api_keys.insert("test-key".to_string(), "t1".to_string());
    let jobs_root = format!("/var/lib/coop/jobs-test-{}", uuid::Uuid::now_v7());
    let cfg = Config {
        addr: "127.0.0.1:0".to_string(),
        db_path: db.to_string_lossy().into_owned(),
        api_keys,
        workers: 2,
        tenant_concurrency: 4,
        rate_per_min: 10_000,
        sandbox: "ns".to_string(),
        jobs_root,
        python_bin: None,
        node_bin: None,
        bash_bin: None,
    };
    let store = Arc::new(Store::open(&db).await.expect("open store"));
    let (app, state, queue_rx) = coop_server::build_app(cfg, store).expect("build app");
    eprintln!(
        "sandbox resolved by server: {}",
        state.sandbox_mode.as_str()
    );
    scheduler::spawn_workers(state, queue_rx);
    let cfg_jobs_root = state.cfg.jobs_root.clone();
    (app, cfg_jobs_root)
}

async fn spawn_app() -> Router {
    spawn_app_with_root().await.0
}

async fn send_raw(app: &Router, req: Request<Body>) -> serde_json::Value {
    let res = app.clone().oneshot(req).await.expect("oneshot");
    assert_eq!(res.status(), StatusCode::OK, "unexpected http status");
    let bytes = axum::body::to_bytes(res.into_body(), 4 << 20)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json body")
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
    assert_eq!(res.status(), StatusCode::CREATED, "submit rejected");
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
        let v = send_raw(app, req).await;
        let status = v["status"].as_str().unwrap_or("").to_string();
        if TERMINAL.contains(&status.as_str()) {
            return (status, started.elapsed().as_secs_f64());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("job {job_id} never reached a terminal state within 30s");
}

async fn replay_events(app: &Router, job_id: &str) -> serde_json::Value {
    let req = Request::builder()
        .method("GET")
        .uri(format!("/v1/jobs/{job_id}/replay"))
        .header(header::AUTHORIZATION, "Bearer test-key")
        .body(Body::empty())
        .unwrap();
    send_raw(app, req).await
}

async fn replay_stdout(app: &Router, job_id: &str) -> String {
    replay_events(app, job_id)
        .await
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|e| e["kind"] == "stdout")
        .filter_map(|e| e["data"]["line"].as_str().map(str::to_string))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_events(events: &serde_json::Value) -> String {
    events
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|e| {
                    format!(
                        "  {:>3} [{:<9}] {}",
                        e["seq"],
                        e["kind"].as_str().unwrap_or("?"),
                        e["data"]
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| "<no events>".to_string())
}

async fn expect_status(app: &Router, job_id: &str, acceptable: &[&str]) -> (String, f64) {
    let (status, elapsed) = wait_terminal(app, job_id).await;
    if !acceptable.contains(&status.as_str()) {
        let events = replay_events(app, job_id).await;
        panic!(
            "job {job_id}: expected one of {acceptable:?}, got '{status}' after {elapsed:.2}s\n\
             --- event log ---\n{}\n-----------------",
            render_events(&events)
        );
    }
    (status, elapsed)
}

async fn assert_host_still_serves(app: &Router) {
    let id = submit(
        app,
        "python",
        "print('host-alive')",
        serde_json::json!({ "wall_seconds": 10 }),
    )
    .await;
    let (status, _) = expect_status(app, &id, &["succeeded"]).await;
    assert_eq!(
        status, "succeeded",
        "host must stay healthy after hostile job"
    );
}

const FORK_BOMB: &str = include_str!("../../../hostile-jobs/fork_bomb.sh");
const BACKGROUND_HOLDER: &str = include_str!("../../../hostile-jobs/background_holder.sh");
const MEMORY_BOMB: &str = include_str!("../../../hostile-jobs/memory_bomb.py");
const INFINITE_LOOP: &str = include_str!("../../../hostile-jobs/infinite_loop.py");
const NETWORK_PROBE: &str = include_str!("../../../hostile-jobs/network_probe.py");
const DISK_FILLER: &str = include_str!("../../../hostile-jobs/disk_filler.py");
const ESCAPE_PROBE: &str = include_str!("../../../hostile-jobs/escape_probe.py");
const PID_BOMB: &str = include_str!("../../../hostile-jobs/pid_bomb.py");

#[tokio::test]
#[ignore]
async fn contains_fork_bomb() {
    if !preflight() {
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
    let (_, elapsed) = expect_status(&app, &id, &["succeeded", "failed", "timed_out"]).await;
    assert!(
        elapsed < 25.0,
        "fork bomb must be contained quickly, took {elapsed}s"
    );
    let stdout = replay_stdout(&app, &id).await;
    let spawned: u32 = stdout
        .split("spawned=")
        .nth(1)
        .map(|rest| {
            rest.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(u32::MAX)
        })
        .unwrap_or(0);
    assert!(
        spawned <= 100,
        "pids.max must cap spawning; job reported spawned={spawned}\nstdout:\n{stdout}"
    );
    assert_host_still_serves(&app).await;
}

#[tokio::test]
#[ignore]
async fn background_holder_does_not_hang_worker() {
    if !preflight() {
        return;
    }
    let app = spawn_app().await;
    let id = submit(
        &app,
        "bash",
        BACKGROUND_HOLDER,
        serde_json::json!({ "wall_seconds": 5 }),
    )
    .await;
    let (status, elapsed) = expect_status(&app, &id, &["succeeded"]).await;
    assert_eq!(status, "succeeded");
    let stdout = replay_stdout(&app, &id).await;
    assert!(
        stdout.contains("done"),
        "'echo done' output must survive the post-reap group kill; stdout was:\n{stdout}"
    );
    assert!(
        elapsed < 15.0,
        "background `sleep infinity` holding the pipes must not pin the worker \
         past wall(5s)+grace; job took {elapsed:.2}s"
    );
    assert_host_still_serves(&app).await;
}

#[tokio::test]
#[ignore]
async fn contains_memory_bomb() {
    if !preflight() {
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
    expect_status(&app, &id, &["oom_killed", "failed"]).await;
    assert_host_still_serves(&app).await;
}

#[tokio::test]
#[ignore]
async fn kills_infinite_loop_on_wall_clock() {
    if !preflight() {
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
    let (_, elapsed) = expect_status(&app, &id, &["timed_out"]).await;
    assert!(
        elapsed < 15.0,
        "wall-clock kill must land near t=3s, took {elapsed}s"
    );
    assert_host_still_serves(&app).await;
}

#[tokio::test]
#[ignore]
async fn network_is_disabled_by_default() {
    if !preflight() {
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
    expect_status(&app, &id, &["succeeded"]).await;
    let stdout = replay_stdout(&app, &id).await;
    assert!(
        stdout.contains("network blocked"),
        "probe must confirm the network is unreachable; stdout was:\n{stdout}"
    );
    assert_host_still_serves(&app).await;
}

#[tokio::test]
#[ignore]
async fn disk_filler_hits_filesystem_cap() {
    if !preflight() {
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
    let (_, elapsed) = expect_status(&app, &id, &["failed", "timed_out"]).await;
    assert!(
        elapsed < 40.0,
        "disk filler must die fast against capped tmpfs, took {elapsed}s"
    );
    assert_host_still_serves(&app).await;
}

#[tokio::test]
#[ignore]
async fn escape_probes_fail_without_leaking() {
    if !preflight() {
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
    expect_status(&app, &id, &["failed"]).await;
    let stdout = replay_stdout(&app, &id).await;
    assert!(
        !stdout.contains("LEAK"),
        "sandbox must not allow host reads/writes; probe reported:\n{stdout}"
    );
    assert_host_still_serves(&app).await;
}

#[tokio::test]
#[ignore]
async fn pid_bomb_is_capped() {
    if !preflight() {
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
    expect_status(&app, &id, &["failed", "timed_out"]).await;
    let stdout = replay_stdout(&app, &id).await;
    assert!(
        stdout.contains("spawn refused"),
        "process-spawn storm must hit pids.max; stdout was:\n{stdout}"
    );
    assert_host_still_serves(&app).await;
}

/// N-1: two jobs run concurrently on the shared jobs root; job B knows job
/// A's workdir path exactly (the test composes it) and must get nothing —
/// no directory listing, no file contents. The jobs root and each workdir
/// are 0700 for the server account while sandboxed jobs run unprivileged,
/// so both the relative sweep and the absolute read come up empty.
#[tokio::test]
#[ignore]
async fn sibling_workdir_not_readable() {
    if !preflight() {
        return;
    }
    let (app, jobs_root) = spawn_app_with_root().await;

    let marker = format!("coop-src-marker-{}", uuid::Uuid::now_v7());
    let victim_code =
        format!("MARKER = {marker:?}\nprint('alive', flush=True)\nimport time\ntime.sleep(8)\n");
    let victim = submit(
        &app,
        "python",
        &victim_code,
        serde_json::json!({ "wall_seconds": 15 }),
    )
    .await;

    // Job B starts while A is still running (workers=2), so A's workdir and
    // its 0600 source file exist for the whole probe window.
    let target = format!("{jobs_root}/job-{victim}/job.py");
    let probe_code = format!(
        r#"import glob
hits = sorted(glob.glob('../job-*'))
hits += sorted(glob.glob({jobs_root:?} + '/job-*'))
try:
    with open({target:?}) as fh:
        hits.append(fh.read())
except OSError:
    pass
print('PROBE-HITS', len(hits))
"#,
    );
    let prober = submit(
        &app,
        "python",
        &probe_code,
        serde_json::json!({ "wall_seconds": 10 }),
    )
    .await;

    let (victim_status, _) = expect_status(&app, &victim, &["succeeded"]).await;
    assert_eq!(victim_status, "succeeded", "victim must run normally");
    let (prober_status, _) = expect_status(&app, &prober, &["succeeded"]).await;
    assert_eq!(prober_status, "succeeded");
    let stdout = replay_stdout(&app, &prober).await;
    assert!(
        stdout.contains("PROBE-HITS 0"),
        "sibling workdir enumeration/read must return nothing; stdout was:\n{stdout}"
    );
    assert!(
        !stdout.contains(&marker),
        "victim source must never reach another job; stdout was:\n{stdout}"
    );
    assert_host_still_serves(&app).await;
}
