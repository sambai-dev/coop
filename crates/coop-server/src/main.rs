use coop_server::config::Config;
use coop_server::{build_app, scheduler, VERSION};
use coop_store::Store;
use std::path::Path;
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = Config::from_env();
    let store = Arc::new(
        Store::open(Path::new(&cfg.db_path))
            .await
            .expect("failed to open sqlite event store"),
    );

    let (app, state, queue_rx) = build_app(cfg, store);
    scheduler::spawn_workers(state.clone(), queue_rx);

    let addr = state.cfg.addr.clone();
    let listener = TcpListener::bind(&addr).await.expect("failed to bind");
    tracing::info!(
        version = VERSION,
        addr = %addr,
        sandbox = state.sandbox_mode.as_str(),
        workers = state.cfg.workers,
        dashboard = format!("http://{addr}/"),
        "coop is listening"
    );

    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!(error = %e, "server terminated with error");
        std::process::exit(1);
    }
}
