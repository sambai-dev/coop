use coop_server::config::Config;
use coop_server::transport::{
    self, WriteTimeoutListener, HTTP_CONNECTION_MAX_LIFETIME, HTTP_HEADER_READ_TIMEOUT,
    HTTP_MAX_ACCEPTED_CONNECTIONS, HTTP_WRITE_PROGRESS_TIMEOUT,
};
use coop_server::{build_app, scheduler, VERSION};
use coop_store::{JobCursor, Store};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

const HTTP_DRAIN_GRACE: Duration = Duration::from_secs(10);
const WORKER_DRAIN_GRACE: Duration = Duration::from_secs(30);
const RECOVERY_PAGE_SIZE: i64 = 256;
const STORAGE_RETRY_ATTEMPTS: usize = 8;

enum RuntimeStop {
    Operator,
    Http(String),
    RecoveryFatal(String),
    WorkerFatal(String),
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    if let Err(error) = run().await {
        tracing::error!(%error, "coop terminated");
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let cfg = Config::from_env()?;
    let addr = cfg.addr.clone();

    // Bind before opening or recovering the event store. A duplicate process
    // using the configured address cannot mutate durable job state.
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|error| format!("failed to bind {addr}: {error}"))?;
    let bound_addr = listener
        .local_addr()
        .map_err(|error| format!("failed to inspect bound listener {addr}: {error}"))?;
    let listener = WriteTimeoutListener::new(listener, HTTP_WRITE_PROGRESS_TIMEOUT);

    // The adjacent OS lock covers the stronger case: another process using a
    // different port but the same canonical SQLite state cannot run recovery
    // or workers concurrently. Keep the file handle alive for this run.
    let _instance_lock = acquire_instance_lock(Path::new(&cfg.db_path))?;
    let store_limits = cfg.storage_limits();
    let store = Arc::new(
        Store::open_with_limits(Path::new(&cfg.db_path), store_limits)
            .await
            .map_err(|error| format!("failed to open sqlite event store: {error}"))?,
    );

    // F8: an unsatisfiable sandbox configuration is a startup error, not a
    // silent downgrade to unprotected execution.
    let (app, state, queue_rx) = build_app(cfg, store).await?;
    state
        .cfg
        .validate_bound_listener_security(bound_addr, state.sandbox_mode)?;
    state
        .startup_ready
        .store(false, std::sync::atomic::Ordering::Release);

    if matches!(state.sandbox_mode, coop_exec::SandboxMode::Namespaces) {
        let rootfs = state
            .cfg
            .rootfs
            .as_deref()
            .ok_or_else(|| "namespace preflight requires COOP_ROOTFS".to_string())?;
        let helper = state
            .cfg
            .sandbox_helper
            .as_deref()
            .ok_or_else(|| "namespace preflight requires COOP_SANDBOX_HELPER".to_string())?;
        coop_exec::namespace_sandbox_execution_preflight(
            Path::new(rootfs),
            Path::new(helper),
            Path::new(&state.cfg.jobs_root),
            state.seccomp,
            &[
                ("python", state.cfg.python_bin.as_deref()),
                ("node", state.cfg.node_bin.as_deref()),
                ("bash", state.cfg.bash_bin.as_deref()),
            ],
        )
        .await
        .map_err(|error| format!("namespace execution preflight failed: {error}"))?;
    }

    // Boot recovery: a process that was running when the previous server
    // stopped cannot be resumed, so finalize it with restart evidence. Queued
    // jobs remain accepted and are re-enqueued below.
    let recovered = recover_stale_running_retrying(&state).await?;
    if recovered > 0 {
        tracing::warn!(
            recovered,
            "marked interrupted running jobs from previous process as error"
        );
    }

    let mut workers = scheduler::spawn_workers(state.clone(), queue_rx);
    scheduler::spawn_retention_sweeper(state.clone());

    // Queued work is fed through the same global admission leases in stable
    // keyset pages. Read APIs and health are available during a large replay,
    // while readiness and POST admission remain gated until the backlog has
    // been fully claimed.
    let recovery_state = state.clone();
    let mut recovery = tokio::spawn(async move {
        let restored = recover_queued_jobs(recovery_state.clone()).await?;
        recovery_state
            .startup_ready
            .store(true, std::sync::atomic::Ordering::Release);
        Ok::<usize, String>(restored)
    });

    tracing::info!(
        version = VERSION,
        addr = %addr,
        sandbox = state.sandbox_mode.as_str(),
        workers = state.cfg.workers,
        http_conn_capacity = HTTP_MAX_ACCEPTED_CONNECTIONS,
        http_conn_max_lifetime_s = HTTP_CONNECTION_MAX_LIFETIME.as_secs(),
        http_header_timeout_s = HTTP_HEADER_READ_TIMEOUT.as_secs(),
        http_write_progress_timeout_s = HTTP_WRITE_PROGRESS_TIMEOUT.as_secs(),
        dashboard = format!("http://{addr}/"),
        "coop is listening"
    );

    let (http_stop_tx, http_stop_rx) = tokio::sync::oneshot::channel::<()>();
    let mut server = Box::pin(transport::serve(listener, app, async move {
        let _ = http_stop_rx.await;
    }));
    let mut http_stop_tx = Some(http_stop_tx);

    let mut recovery_finished = false;
    let stop = tokio::select! {
        // A supervised task publishes its retained diagnosis before calling
        // begin_shutdown. Poll this branch first so that sticky internal
        // shutdown cannot be misclassified as a clean operator request.
        biased;
        error = workers.failure() => RuntimeStop::WorkerFatal(error),
        result = server.as_mut() => {
            RuntimeStop::Http(match result {
                Ok(()) => "HTTP server stopped unexpectedly".to_string(),
                Err(error) => format!("HTTP server terminated: {error}"),
            })
        }
        () = shutdown_requested(&state) => {
            RuntimeStop::Operator
        }
        completion = &mut recovery => {
            recovery_finished = true;
            match classify_recovery_completion(completion) {
                Ok(restored) => {
                    tracing::info!(restored, "durable queued-job recovery complete");
                    tokio::select! {
                        biased;
                        error = workers.failure() => RuntimeStop::WorkerFatal(error),
                        result = server.as_mut() => RuntimeStop::Http(match result {
                            Ok(()) => "HTTP server stopped unexpectedly".to_string(),
                            Err(error) => format!("HTTP server terminated: {error}"),
                        }),
                        () = shutdown_requested(&state) => RuntimeStop::Operator,
                    }
                }
                Err(error) => RuntimeStop::RecoveryFatal(error),
            }
        }
    };
    // `biased` controls polling order, but it does not repoll an earlier
    // pending branch when another thread wakes both failure and shutdown in
    // the middle of one select poll. The supervisor sends the retained error
    // before publishing shutdown, so this post-select check deterministically
    // upgrades scheduler-caused shutdown instead of returning exit status 0.
    let stop = match stop {
        RuntimeStop::Operator => reconcile_operator_stop(workers.try_failure()),
        other => other,
    };

    state.begin_shutdown();
    if let Some(sender) = http_stop_tx.take() {
        let _ = sender.send(());
    }

    let server_already_stopped = matches!(&stop, RuntimeStop::Http(_));
    let mut fatal_error = match stop {
        RuntimeStop::Operator => {
            tracing::info!("shutdown requested; draining HTTP and cancelling active jobs");
            None
        }
        RuntimeStop::Http(error)
        | RuntimeStop::RecoveryFatal(error)
        | RuntimeStop::WorkerFatal(error) => {
            tracing::error!(%error, "fatal server lifecycle failure; shutting down");
            Some(error)
        }
    };

    // The server future may already have completed in the HTTP stop branch.
    // Polling a completed future again is invalid, so only drain it for the
    // operator and recovery-failure paths.
    if !server_already_stopped {
        match tokio::time::timeout(HTTP_DRAIN_GRACE, server.as_mut()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) if fatal_error.is_none() => {
                fatal_error = Some(format!("HTTP server terminated during drain: {error}"));
            }
            Ok(Err(error)) => {
                tracing::error!(%error, "HTTP server failed while draining after fatal error");
            }
            Err(_) => {
                tracing::warn!(
                    grace_seconds = HTTP_DRAIN_GRACE.as_secs(),
                    "HTTP drain deadline elapsed; aborting remaining connection tasks"
                );
            }
        }
    }
    // Dropping `transport::serve` force-closes guarded IO (including sockets
    // already transferred into a WebSocket upgrade) and drops its JoinSet,
    // aborting HTTP tasks still present after the bounded graceful drain. Keep
    // this explicit and before the worker drain so the log above describes an
    // action that has already happened, not one deferred until `run` returns.
    drop(server);

    if !recovery_finished {
        match tokio::time::timeout(Duration::from_secs(1), &mut recovery).await {
            Ok(completion) => {
                if let Err(error) = classify_recovery_shutdown_completion(completion) {
                    if fatal_error.is_none() {
                        tracing::error!(
                            %error,
                            "recovery failed while shutdown was being selected; upgrading process exit to fatal"
                        );
                        fatal_error = Some(error);
                    }
                }
            }
            Err(_) => {
                recovery.abort();
                let _ = recovery.await;
            }
        }
    }
    if let Some(worker_error) = workers.shutdown(&state, WORKER_DRAIN_GRACE).await {
        if fatal_error.is_none() {
            tracing::error!(
                error = %worker_error,
                "scheduler failure completed during shutdown; upgrading process exit to fatal"
            );
            fatal_error = Some(worker_error);
        }
    }
    if let Some(error) = fatal_error {
        return Err(error);
    }
    Ok(())
}

fn reconcile_operator_stop(retained_worker_failure: Option<String>) -> RuntimeStop {
    retained_worker_failure.map_or(RuntimeStop::Operator, RuntimeStop::WorkerFatal)
}

fn classify_recovery_completion(
    completion: Result<Result<usize, String>, tokio::task::JoinError>,
) -> Result<usize, String> {
    match completion {
        Ok(Ok(restored)) => Ok(restored),
        Ok(Err(error)) => Err(format!("queued-job recovery failed: {error}")),
        Err(error) if error.is_panic() => {
            Err(format!("queued-job recovery task panicked: {error}"))
        }
        Err(error) => Err(format!("queued-job recovery task failed: {error}")),
    }
}

fn classify_recovery_shutdown_completion(
    completion: Result<Result<usize, String>, tokio::task::JoinError>,
) -> Result<(), String> {
    match completion {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) if error == "shutdown requested" => Ok(()),
        other => classify_recovery_completion(other).map(|_| ()),
    }
}

async fn recover_stale_running_retrying(state: &coop_server::AppState) -> Result<u64, String> {
    let mut delay = Duration::from_millis(20);
    for attempt in 1..=STORAGE_RETRY_ATTEMPTS {
        match state.store.recover_stale_running().await {
            Ok(recovered) => return Ok(recovered),
            Err(error) if attempt == STORAGE_RETRY_ATTEMPTS => {
                return Err(format!(
                    "boot recovery failed after {STORAGE_RETRY_ATTEMPTS} attempts: {error}"
                ));
            }
            Err(error) => {
                tracing::warn!(%error, attempt, "boot recovery storage failure; retrying");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(1));
            }
        }
    }
    unreachable!("retry loop returns on its final attempt")
}

async fn recover_queued_jobs(state: coop_server::AppState) -> Result<usize, String> {
    let mut cursor: Option<JobCursor> = None;
    let mut restored = 0_usize;
    loop {
        if *state.shutdown.borrow() {
            return Err("shutdown requested".to_string());
        }
        let page = queued_page_retrying(&state, cursor.as_ref()).await?;
        if page.is_empty() {
            return Ok(restored);
        }
        let page_len = page.len();
        for row in &page {
            if *state.shutdown.borrow() {
                return Err("shutdown requested".to_string());
            }
            let reservation = state
                .admission
                .reserve_recovery(&row.tenant, state.cfg.clamp_mem_mb(row.requested_mem_mb))
                .await
                // Admission is closed only by AppState::begin_shutdown. The
                // sticky watch publication follows immediately, but reserve
                // can observe the close first; classify both orderings as the
                // same expected recovery stop rather than a fatal boot error.
                .map_err(|_| "shutdown requested".to_string())?;
            state.bus.register(&row.job_id);
            reservation.send(row.job_id.clone());
            restored += 1;
        }
        cursor = page.last().map(JobCursor::from);
        if page_len < RECOVERY_PAGE_SIZE as usize {
            return Ok(restored);
        }
    }
}

async fn queued_page_retrying(
    state: &coop_server::AppState,
    cursor: Option<&JobCursor>,
) -> Result<Vec<coop_store::QueuedJobRow>, String> {
    let mut delay = Duration::from_millis(20);
    for attempt in 1..=STORAGE_RETRY_ATTEMPTS {
        if *state.shutdown.borrow() {
            return Err("shutdown requested".to_string());
        }
        match state
            .store
            .queued_jobs_page(cursor, RECOVERY_PAGE_SIZE)
            .await
        {
            Ok(page) => return Ok(page),
            Err(error) if attempt == STORAGE_RETRY_ATTEMPTS => {
                return Err(format!(
                    "queued recovery page failed after {STORAGE_RETRY_ATTEMPTS} attempts: {error}"
                ));
            }
            Err(error) => {
                tracing::warn!(%error, attempt, "queued recovery page failed; retrying");
                let mut shutdown = state.shutdown.subscribe();
                if *shutdown.borrow() {
                    return Err("shutdown requested".to_string());
                }
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = shutdown.wait_for(|value| *value) => return Err("shutdown requested".to_string()),
                }
                delay = (delay * 2).min(Duration::from_secs(1));
            }
        }
    }
    unreachable!("retry loop returns on its final attempt")
}

async fn shutdown_requested(state: &coop_server::AppState) {
    let mut internal = state.shutdown.subscribe();
    if *internal.borrow() {
        return;
    }
    tokio::select! {
        () = shutdown_signal() => {}
        _ = internal.wait_for(|value| *value) => {}
    }
}

struct InstanceLock {
    _adjacent: File,
    // Windows has no systemd-style private temporary namespace, so a stable
    // file-identity lock still protects hard-link aliases there. Unix rejects
    // multiply linked database files instead (see `acquire_instance_lock`).
    _identity: Option<File>,
}

fn acquire_instance_lock(db_path: &Path) -> Result<InstanceLock, String> {
    let absolute = absolute_path(db_path)?;
    let parent = absolute
        .parent()
        .ok_or_else(|| format!("COOP_DB {} has no parent directory", db_path.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "failed to create COOP_DB parent {} before locking: {error}",
            parent.display()
        )
    })?;
    let canonical_parent = parent.canonicalize().map_err(|error| {
        format!(
            "failed to canonicalize COOP_DB parent {}: {error}",
            parent.display()
        )
    })?;
    let filename = absolute
        .file_name()
        .ok_or_else(|| format!("COOP_DB {} has no file name", db_path.display()))?
        .to_string_lossy();
    let lock_path = canonical_parent.join(format!(".{filename}.coop.lock"));
    let adjacent = open_and_lock(&lock_path)?;

    if std::fs::symlink_metadata(&absolute).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(format!(
            "COOP_DB {} must not be a symlink",
            absolute.display()
        ));
    }
    let mut db_options = OpenOptions::new();
    db_options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        db_options.mode(0o600);
    }
    let db_file = db_options.open(&absolute).map_err(|error| {
        format!(
            "failed to bootstrap COOP_DB {}: {error}",
            absolute.display()
        )
    })?;
    let db_metadata = db_file
        .metadata()
        .map_err(|error| format!("failed to identify COOP_DB {}: {error}", absolute.display()))?;
    if !db_metadata.is_file() {
        return Err(format!(
            "COOP_DB {} must be a regular file",
            absolute.display()
        ));
    }

    // An inode-derived lock below `temp_dir()` is not process-global on a
    // systemd service with PrivateTmp=yes. Reject Unix hard links entirely so
    // every supported database path is protected by the canonical adjacent
    // lock shared by service and manual processes alike. A new alias created
    // after this check raises nlink and is rejected by the second process.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if db_metadata.nlink() != 1 {
            return Err(format!(
                "COOP_DB {} has {} hard links; hard-linked SQLite files are unsupported",
                absolute.display(),
                db_metadata.nlink()
            ));
        }
    }

    #[cfg(not(unix))]
    let identity = {
        let identity = storage_file_identity(&db_file, &db_metadata, &absolute)?;
        let identity_root = identity_lock_root()?;
        let identity_path = identity_root.join(format!("{identity}.lock"));
        Some(open_and_lock(&identity_path).map_err(|error| {
            format!(
                "another coop process already owns the same SQLite file identity as {}: {error}",
                absolute.display()
            )
        })?)
    };
    #[cfg(unix)]
    let identity = None;

    Ok(InstanceLock {
        _adjacent: adjacent,
        _identity: identity,
    })
}

fn open_and_lock(lock_path: &Path) -> Result<File, String> {
    if std::fs::symlink_metadata(lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(format!(
            "instance lock {} must not be a symlink",
            lock_path.display()
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(lock_path).map_err(|error| {
        format!(
            "failed to open instance lock {}: {error}",
            lock_path.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                format!(
                    "failed to secure instance lock {}: {error}",
                    lock_path.display()
                )
            })?;
    }
    file.try_lock().map_err(|error| {
        format!(
            "instance lock {} is already held: {error}",
            lock_path.display()
        )
    })?;
    Ok(file)
}

#[cfg(windows)]
fn storage_file_identity(
    file: &File,
    _metadata: &std::fs::Metadata,
    path: &Path,
) -> Result<String, String> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ByHandleFileInformation {
        attributes: u32,
        creation_time: FileTime,
        last_access_time: FileTime,
        last_write_time: FileTime,
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(
            file: *mut c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    let mut information = ByHandleFileInformation::default();
    // SAFETY: `file` owns a live Windows handle, and `information` points to a
    // correctly laid-out writable BY_HANDLE_FILE_INFORMATION buffer.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) };
    if ok == 0 {
        return Err(format!(
            "failed to identify COOP_DB {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let index =
        (u64::from(information.file_index_high) << 32) | u64::from(information.file_index_low);
    Ok(format!(
        "windows-{:x}-{index:x}",
        information.volume_serial_number
    ))
}

#[cfg(not(any(unix, windows)))]
fn storage_file_identity(
    _file: &File,
    _metadata: &std::fs::Metadata,
    path: &Path,
) -> Result<String, String> {
    path.canonicalize()
        .map(|path| format!("path-{}", path.to_string_lossy()))
        .map_err(|error| format!("failed to identify COOP_DB {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn identity_lock_root() -> Result<PathBuf, String> {
    let root = std::env::temp_dir().join("coop-instance-locks");

    if std::fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(format!(
            "instance lock directory {} must not be a symlink",
            root.display()
        ));
    }
    std::fs::create_dir_all(&root)
        .map_err(|error| format!("failed to create instance lock directory: {error}"))?;
    Ok(root)
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|error| format!("failed to resolve current directory: {error}"))
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(e) = result { tracing::error!(error = %e, "Ctrl-C handler failed"); }
            }
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    if let Err(e) = tokio::signal::ctrl_c().await {
        tracing::error!(error = %e, "Ctrl-C handler failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_worker_failure_upgrades_operator_stop_to_fatal() {
        match reconcile_operator_stop(Some("scheduler worker 0 panicked".to_string())) {
            RuntimeStop::WorkerFatal(error) => {
                assert!(error.contains("worker 0 panicked"), "{error}");
            }
            _ => panic!("retained worker failure must be fatal"),
        }
        assert!(matches!(
            reconcile_operator_stop(None),
            RuntimeStop::Operator
        ));
    }

    #[tokio::test]
    async fn queued_recovery_error_and_panic_are_fatal() {
        let storage_failure =
            tokio::spawn(async { Err::<usize, String>("persistent storage failure".to_string()) })
                .await;
        let error = classify_recovery_completion(storage_failure).expect_err("must be fatal");
        assert!(error.contains("persistent storage failure"), "{error}");

        let panic = tokio::spawn(async { panic!("recovery invariant") }).await;
        let error = classify_recovery_completion(panic).expect_err("panic must be fatal");
        assert!(error.contains("panicked"), "{error}");

        let expected_stop =
            tokio::spawn(async { Err::<usize, String>("shutdown requested".to_string()) }).await;
        classify_recovery_shutdown_completion(expected_stop)
            .expect("operator shutdown is an expected recovery stop");

        let late_failure =
            tokio::spawn(async { Err::<usize, String>("late storage failure".to_string()) }).await;
        let error = classify_recovery_shutdown_completion(late_failure)
            .expect_err("a recovery error racing shutdown remains fatal");
        assert!(error.contains("late storage failure"), "{error}");

        let late_panic = tokio::spawn(async { panic!("late recovery invariant") }).await;
        let error = classify_recovery_shutdown_completion(late_panic)
            .expect_err("a recovery panic racing shutdown remains fatal");
        assert!(error.contains("panicked"), "{error}");
    }

    #[test]
    fn instance_lock_rejects_hardlink_aliases_and_reclaims_supported_path() {
        let base = std::env::temp_dir().join(format!("coop-lock-test-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&base).unwrap();
        let original = base.join("primary.db");
        let alias = base.join("alias.db");
        File::create(&original).unwrap();

        let first = acquire_instance_lock(&original).expect("first owner");
        std::fs::hard_link(&original, &alias).unwrap();
        let error = acquire_instance_lock(&alias)
            .err()
            .expect("hardlink alias must be rejected");
        #[cfg(unix)]
        assert!(error.contains("hard-linked SQLite files"), "{error}");
        #[cfg(not(unix))]
        assert!(error.contains("same SQLite file identity"), "{error}");
        drop(first);

        // Restore the supported single-link shape; the adjacent lock is
        // reclaimable once the prior owner exits.
        std::fs::remove_file(&original).unwrap();
        let second = acquire_instance_lock(&alias).expect("lock is reclaimable after owner exits");
        drop(second);
        let _ = std::fs::remove_dir_all(base);
    }
}
