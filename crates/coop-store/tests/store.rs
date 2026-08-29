use coop_store::{
    canonical_json, capacity_error_kind, compute_receipt_sha256, is_idempotency_conflict,
    CapacityErrorKind, CreateJobOutcome, IdempotencyLookup, IdempotencyRequest, JobCursor,
    ListJobsQuery, StorageLimits, Store, JOB_COMPLETION_RESERVE_BYTES, MAX_EVENT_BATCH_SIZE,
    MAX_RETENTION_EVENTS_PER_BATCH,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, Row, SqliteConnection};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

fn test_db(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir()
        .join(format!(
            "coop-store-{label}-{}-{nanos}-{}",
            std::process::id(),
            NEXT_PATH.fetch_add(1, Ordering::Relaxed)
        ))
        .join("coop.db")
}

async fn raw_connection(db: &Path) -> SqliteConnection {
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let options = SqliteConnectOptions::new()
        .filename(db)
        .create_if_missing(true);
    SqliteConnection::connect_with(&options).await.unwrap()
}

#[cfg(target_os = "macos")]
#[test]
fn macos_store_opens_beneath_the_trusted_system_temp_alias() {
    sqlx::test_block_on(async {
        let db = std::env::temp_dir().join(format!(
            "coop-store-macos-temp-alias-{}-{}-{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_PATH.fetch_add(1, Ordering::Relaxed)
        ));
        let store = Store::open(&db)
            .await
            .expect("macOS /var or /tmp system alias and exact temp root must be trusted");
        store
            .create_job("job", "tenant-a", "python", "{}")
            .await
            .unwrap();
        assert!(store.get_job_summary("job").await.unwrap().is_some());
    });
}

async fn create_v1_schema(connection: &mut SqliteConnection) {
    sqlx::query(
        "CREATE TABLE jobs (
            job_id TEXT PRIMARY KEY,
            tenant TEXT NOT NULL,
            language TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'queued',
            spec_json TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            started_at_ms INTEGER,
            finished_at_ms INTEGER,
            exit_code INTEGER
        )",
    )
    .execute(&mut *connection)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE events (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            job_id TEXT NOT NULL,
            ts_ms INTEGER NOT NULL,
            kind TEXT NOT NULL,
            data_json TEXT NOT NULL
        )",
    )
    .execute(&mut *connection)
    .await
    .unwrap();
}

#[test]
fn migrates_v01_database_without_fabricating_event_hashes() {
    sqlx::test_block_on(async {
        let db = test_db("migration");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let options = SqliteConnectOptions::new()
            .filename(&db)
            .create_if_missing(true);
        let mut connection = sqlx::SqliteConnection::connect_with(&options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE jobs (
                job_id TEXT PRIMARY KEY,
                tenant TEXT NOT NULL,
                language TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'queued',
                spec_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                started_at_ms INTEGER,
                finished_at_ms INTEGER,
                exit_code INTEGER
            )",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE events (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                job_id TEXT NOT NULL,
                ts_ms INTEGER NOT NULL,
                kind TEXT NOT NULL,
                data_json TEXT NOT NULL
            )",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO jobs VALUES
                ('legacy', 'tenant-a', 'python', 'succeeded', '{}', 10, 11, 12, 0),
                ('quarantined', '', 'python', 'queued', '{}', 20, NULL, NULL, NULL),
                ('invalid-json', 'tenant-a', 'python', 'queued', 'not-json', 30, NULL, NULL, NULL)",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO events(job_id, ts_ms, kind, data_json)
             VALUES
                ('legacy', 11, 'stdout', '{\"line\":\"kept\"}'),
                ('invalid-json', 31, 'stdout', 'not-json')",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query("PRAGMA user_version = 1")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();

        let store = Store::open(&db).await.unwrap();
        assert_eq!(store.schema_version().await.unwrap(), 3);
        let legacy = store.get_job("legacy").await.unwrap().unwrap();
        assert_eq!(legacy.status, "succeeded");
        assert!(legacy.effective_spec_json.is_none());
        assert!(legacy.receipt_json.is_none());
        let quarantined = store.get_job("quarantined").await.unwrap().unwrap();
        assert_eq!(quarantined.tenant, "__legacy_invalid__:quarantined");
        let invalid = store.get_job("invalid-json").await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&invalid.spec_json).unwrap(),
            json!({"legacy_invalid_spec_json":"not-json"})
        );
        assert_eq!(
            store.events_for("invalid-json").await.unwrap()[0].data,
            json!({"legacy_invalid_data_json":"not-json"})
        );
        store
            .finalize_with_event("invalid-json", "error", None, 0, None)
            .await
            .unwrap()
            .unwrap();
        let invalid = store.get_job("invalid-json").await.unwrap().unwrap();
        let receipt: serde_json::Value =
            serde_json::from_str(invalid.receipt_json.as_deref().unwrap()).unwrap();
        assert_eq!(receipt["event_chain"]["complete"], false);
        assert_eq!(receipt["event_chain"]["legacy_events"], 1);
        assert_eq!(receipt["event_chain"]["verified_events"], 1);

        let events = store.events_for("legacy").await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data["line"], "kept");
        assert_eq!(events[0].hash_version, 0);
        assert!(events[0].event_hash.is_empty());
        let verification = store.verify_event_chain("legacy").await.unwrap();
        assert!(verification.valid);
        assert_eq!(verification.head.legacy_event_count, 1);
        assert_eq!(verification.head.verified_event_count, 0);
        assert!(verification.head.head_hash.is_none());
    });
}

#[test]
fn lifecycle_events_receipt_and_hash_chain_commit_together() {
    sqlx::test_block_on(async {
        let db = test_db("lifecycle");
        let store = Store::open(&db).await.unwrap();
        store
            .create_job("job", "tenant-a", "python", r#"{"code":"print(1)"}"#)
            .await
            .unwrap();
        let effective = json!({"language":"python","limits":{"wall_seconds":5}});
        let started = store
            .start_with_event_if_queued("job", &effective)
            .await
            .unwrap()
            .unwrap();
        let finished = store
            .finalize_with_event(
                "job",
                "succeeded",
                Some(0),
                8,
                Some(&json!({
                    "policy":"default",
                    "job_id":"caller-value",
                    "outcome":"failed",
                    "exit_code":99,
                    "created_at_ms":-1,
                    "started_at_ms":-1,
                    "finished_at_ms":0,
                    "duration_ms":0,
                    "receipt_sha256":"caller-value",
                })),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(store
            .finalize_with_event("job", "failed", Some(1), 9, None)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .append_event_row("job", "stdout", &json!({"line":"too late"}))
            .await
            .is_err());

        let row = store.get_job("job").await.unwrap().unwrap();
        assert_eq!(row.status, "succeeded");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(row.effective_spec_json.as_deref().unwrap())
                .unwrap(),
            effective
        );
        let receipt: serde_json::Value =
            serde_json::from_str(row.receipt_json.as_deref().unwrap()).unwrap();
        assert_eq!(receipt["policy"], "default");
        assert_eq!(receipt["version"], 1);
        assert_eq!(receipt["job_id"], "job");
        assert_eq!(receipt["outcome"], "succeeded");
        assert_eq!(receipt["exit_code"], 0);
        assert_eq!(receipt["created_at_ms"], row.created_at_ms);
        assert_eq!(receipt["started_at_ms"], row.started_at_ms.unwrap());
        assert_eq!(receipt["finished_at_ms"], row.finished_at_ms.unwrap());
        assert_eq!(receipt["duration_ms"], 8);
        assert_eq!(receipt["event_chain"]["head"], finished.event_hash);
        assert_eq!(receipt["event_chain"]["events"], 3);
        assert_eq!(receipt["event_chain"]["event_count"], 3);
        assert_eq!(receipt["event_chain"]["complete"], true);
        assert_eq!(
            receipt["receipt_sha256"].as_str().unwrap(),
            compute_receipt_sha256(&receipt)
        );

        let events = store.events_for("job").await.unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, "accepted");
        assert_eq!(events[1].kind, "started");
        assert_eq!(events[2].kind, "finished");
        assert_eq!(started.prev_hash, events[0].event_hash);
        assert_eq!(finished.prev_hash, started.event_hash);
        let verification = store.verify_event_chain("job").await.unwrap();
        assert!(verification.valid);
        assert_eq!(verification.head.head_hash, Some(finished.event_hash));
    });
}

#[test]
fn terminal_finalize_atomically_replaces_initial_effective_spec_and_binds_its_digest() {
    sqlx::test_block_on(async {
        let db = test_db("observed-effective-spec");
        let store = Store::open(&db).await.unwrap();
        store
            .create_job("job", "tenant-a", "python", r#"{"code":"print(1)"}"#)
            .await
            .unwrap();
        let initial = json!({
            "backend": null,
            "network_allowed": null,
            "limit_enforcement": null,
        });
        let started = store
            .start_with_event_if_queued("job", &initial)
            .await
            .unwrap()
            .unwrap();
        assert!(started.data.get("effective_spec_sha256").is_none());
        assert_eq!(
            started.data["initial_effective_spec_sha256"]
                .as_str()
                .unwrap()
                .len(),
            64
        );

        let observed = json!({
            "backend": "linux_namespaces",
            "network_allowed": false,
            "limit_enforcement": {
                "memory": "cgroup_v2",
                "cpu": "rlimit",
            },
        });
        let finished = store
            .finalize_with_event_and_effective_spec(
                "job",
                "succeeded",
                Some(0),
                7,
                Some(&observed),
                None,
            )
            .await
            .unwrap()
            .unwrap();
        let row = store.get_job("job").await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(row.effective_spec_json.as_deref().unwrap())
                .unwrap(),
            observed
        );
        let expected_digest = format!("{:x}", Sha256::digest(canonical_json(&observed).as_bytes()));
        assert_eq!(finished.data["effective_spec_sha256"], expected_digest);
        assert!(store.verify_event_chain("job").await.unwrap().valid);
    });
}

#[test]
fn raw_event_deletion_dirties_validation_and_cannot_be_laundered_by_store_writes() {
    sqlx::test_block_on(async {
        let db = test_db("raw-event-delete");
        let store = Store::open(&db).await.unwrap();
        store
            .create_job("job", "tenant-a", "python", "{}")
            .await
            .unwrap();

        let mut connection = raw_connection(&db).await;
        sqlx::query("DELETE FROM events WHERE job_id = 'job'")
            .execute(&mut connection)
            .await
            .unwrap();
        let revision: i64 = sqlx::query_scalar(
            "SELECT row_validation_revision FROM store_integrity WHERE singleton = 1",
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(revision, 0);
        connection.close().await.unwrap();

        let error = store
            .append_event_row("job", "stdout", &json!({"line":"must fail"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("row validation is stale"));
    });
}

#[test]
fn batch_append_is_ordered_chained_bounded_and_atomic() {
    sqlx::test_block_on(async {
        let db = test_db("event-batch");
        let store = Store::open(&db).await.unwrap();
        store
            .create_job("job", "tenant-a", "python", r#"{"code":"print(1)"}"#)
            .await
            .unwrap();
        let started = store
            .start_with_event_if_queued("job", &json!({"language":"python"}))
            .await
            .unwrap()
            .unwrap();

        assert!(store
            .append_events_batch("missing", &[])
            .await
            .unwrap()
            .is_empty());
        let pending = vec![
            ("stdout".to_string(), json!({"line":"one"})),
            ("stderr".to_string(), json!({"line":"two"})),
            ("stdout".to_string(), json!({"line":"three"})),
        ];
        let appended = store.append_events_batch("job", &pending).await.unwrap();
        assert_eq!(appended.len(), pending.len());
        assert_eq!(
            appended
                .iter()
                .map(|event| event.kind.as_str())
                .collect::<Vec<_>>(),
            ["stdout", "stderr", "stdout"]
        );
        assert_eq!(appended[0].prev_hash, started.event_hash);
        for pair in appended.windows(2) {
            assert_eq!(pair[1].seq, pair[0].seq + 1);
            assert_eq!(pair[1].prev_hash, pair[0].event_hash);
        }
        assert_eq!(appended[0].data["line"], "one");
        assert_eq!(appended[2].data["line"], "three");
        assert!(store.verify_event_chain("job").await.unwrap().valid);

        let oversized = (0..=MAX_EVENT_BATCH_SIZE)
            .map(|index| ("stdout".to_string(), json!({"index":index})))
            .collect::<Vec<_>>();
        assert!(store.append_events_batch("job", &oversized).await.is_err());

        // Force the second insert to fail inside SQLite, proving that the
        // first insert from the same batch cannot escape the transaction.
        let options = SqliteConnectOptions::new().filename(&db);
        let mut connection = sqlx::SqliteConnection::connect_with(&options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TRIGGER reject_test_event
             BEFORE INSERT ON events
             WHEN NEW.kind = 'force_batch_failure'
             BEGIN
                 SELECT RAISE(ABORT, 'forced batch failure');
             END",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();

        let before = store.events_for("job").await.unwrap();
        let before_head = before.last().unwrap().event_hash.clone();
        assert!(store
            .append_events_batch(
                "job",
                &[
                    ("stdout".to_string(), json!({"line":"must roll back"})),
                    ("force_batch_failure".to_string(), json!({})),
                ],
            )
            .await
            .is_err());
        let after = store.events_for("job").await.unwrap();
        assert_eq!(after.len(), before.len());
        assert_eq!(after.last().unwrap().event_hash, before_head);
        assert!(store.verify_event_chain("job").await.unwrap().valid);
    });
}

#[test]
fn cancellation_pagination_and_retention_are_strict_and_bounded() {
    sqlx::test_block_on(async {
        let db = test_db("queries");
        let store = Store::open(&db).await.unwrap();
        for (job_id, tenant, language) in [
            ("a1", "tenant-a", "python"),
            ("a2", "tenant-a", "node"),
            ("a3", "tenant-a", "python"),
            ("b1", "tenant-b", "python"),
        ] {
            store
                .create_job(job_id, tenant, language, "{}")
                .await
                .unwrap();
        }
        assert!(store
            .cancel_queued_with_event("a2", "tenant-b", None)
            .await
            .unwrap()
            .is_none());
        store
            .cancel_queued_with_event("a2", "tenant-a", None)
            .await
            .unwrap()
            .unwrap();
        store
            .finalize_with_event("a1", "succeeded", Some(0), 1, None)
            .await
            .unwrap()
            .unwrap();

        let filtered = store
            .list_jobs_page(ListJobsQuery {
                tenant: Some("tenant-a".to_string()),
                status: Some("succeeded".to_string()),
                language: Some("python".to_string()),
                limit: 50,
                ..ListJobsQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].job_id, "a1");
        assert!(store.list_jobs(Some(""), 50).await.unwrap().is_empty());

        let filtered_summaries = store
            .list_job_summaries_page(ListJobsQuery {
                tenant: Some("tenant-a".to_string()),
                status: Some("succeeded".to_string()),
                language: Some("python".to_string()),
                limit: 50,
                ..ListJobsQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(filtered_summaries.len(), 1);
        assert_eq!(filtered_summaries[0].job_id, "a1");
        assert!(store
            .list_job_summaries_page(ListJobsQuery {
                tenant: Some(String::new()),
                ..ListJobsQuery::default()
            })
            .await
            .unwrap()
            .is_empty());

        let first = store
            .list_jobs_page(ListJobsQuery {
                tenant: Some("tenant-a".to_string()),
                limit: 2,
                ..ListJobsQuery::default()
            })
            .await
            .unwrap();
        let cursor_row = first.last().unwrap();
        let second = store
            .list_jobs_page(ListJobsQuery {
                tenant: Some("tenant-a".to_string()),
                before: Some(JobCursor {
                    created_at_ms: cursor_row.created_at_ms,
                    job_id: cursor_row.job_id.clone(),
                }),
                limit: 2,
                ..ListJobsQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(second.len(), 1);
        assert!(!first
            .iter()
            .any(|left| second.iter().any(|right| left.job_id == right.job_id)));

        let summary_first = store
            .list_job_summaries_page(ListJobsQuery {
                tenant: Some("tenant-a".to_string()),
                limit: 2,
                ..ListJobsQuery::default()
            })
            .await
            .unwrap();
        let summary_second = store
            .list_job_summaries_page(ListJobsQuery {
                tenant: Some("tenant-a".to_string()),
                before: Some(JobCursor::from(summary_first.last().unwrap())),
                limit: 2,
                ..ListJobsQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(summary_first.len(), 2);
        assert_eq!(summary_second.len(), 1);
        assert!(!summary_first.iter().any(|left| summary_second
            .iter()
            .any(|right| left.job_id == right.job_id)));

        for line in 1..=3 {
            store
                .append_event_row("a3", "stdout", &json!({"line":line}))
                .await
                .unwrap();
        }
        let event_page = store.events_after("a3", 0, 2).await.unwrap();
        assert_eq!(event_page.len(), 2);
        assert_eq!(
            store
                .events_after("a3", event_page[1].seq, 2)
                .await
                .unwrap()
                .len(),
            2
        );

        std::thread::sleep(std::time::Duration::from_millis(5));
        let first_prune = store.prune_older_than_batch(0, 1).await.unwrap();
        assert_eq!(first_prune.jobs_deleted, 1);
        assert_eq!(first_prune.events_deleted, 2);
        assert!(first_prune.more_remaining);
        let second_prune = store.prune_older_than_batch(0, 1).await.unwrap();
        assert_eq!(second_prune.jobs_deleted, 1);
        assert_eq!(second_prune.events_deleted, 2);
        assert!(!second_prune.more_remaining);
        assert!(store.get_job("a3").await.unwrap().is_some());
        assert!(store.get_job("b1").await.unwrap().is_some());
        store.compact().await.unwrap();
    });
}

#[test]
fn running_recovery_preserves_queued_work_and_emits_terminal_evidence() {
    sqlx::test_block_on(async {
        let db = test_db("recovery");
        let store = Store::open(&db).await.unwrap();
        store
            .create_job("queued", "tenant-a", "python", "{}")
            .await
            .unwrap();
        store
            .create_job(
                "running",
                "tenant-a",
                "python",
                &json!({
                    "code":"print(1)",
                    "stdin":"input",
                    "limits":{"wall_seconds":10}
                })
                .to_string(),
            )
            .await
            .unwrap();
        store
            .start_with_event_if_queued(
                "running",
                &json!({"limits":{"wall_seconds":5},"effective":true}),
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(store.recover_stale_running().await.unwrap(), 1);
        assert_eq!(
            store.get_job("queued").await.unwrap().unwrap().status,
            "queued"
        );
        let recovered = store.get_job("running").await.unwrap().unwrap();
        assert_eq!(recovered.status, "error");
        assert_eq!(store.queued_job_ids(10).await.unwrap(), ["queued"]);
        let receipt: serde_json::Value =
            serde_json::from_str(recovered.receipt_json.as_deref().unwrap()).unwrap();
        assert_eq!(receipt["terminal_reason"], "server_restarted");
        assert_eq!(receipt["killed_by"], "server_restarted");
        assert_eq!(receipt["evidence_complete"], false);
        assert_eq!(receipt["created_at_ms"], recovered.created_at_ms);
        assert_eq!(receipt["started_at_ms"], recovered.started_at_ms.unwrap());
        assert_eq!(receipt["requested_limits"]["wall_seconds"], 10);
        assert_eq!(receipt["code_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(receipt["stdin_sha256"].as_str().unwrap().len(), 64);
        for unavailable in [
            "backend",
            "seccomp",
            "network_allowed",
            "bootstrap_ready",
            "limit_enforcement",
            "effective_limits",
            "policy_sha256",
            "resource_usage",
            "output",
        ] {
            assert!(
                receipt.get(unavailable).is_none(),
                "{unavailable} was fabricated"
            );
        }
        assert_eq!(
            receipt["receipt_sha256"].as_str().unwrap(),
            compute_receipt_sha256(&receipt)
        );
        let events = store.events_for("running").await.unwrap();
        assert_eq!(events.last().unwrap().data["reason"], "server_restarted");
        assert!(store.verify_event_chain("running").await.unwrap().valid);
    });
}

#[test]
fn running_recovery_is_bounded_idempotent_and_atomic_between_rows() {
    sqlx::test_block_on(async {
        let db = test_db("bounded-running-recovery");
        let store = Store::open(&db).await.unwrap();
        let mut connection = raw_connection(&db).await;
        sqlx::query(
            "WITH RECURSIVE sequence(n) AS (
                 VALUES(1) UNION ALL SELECT n + 1 FROM sequence WHERE n < 256
             )
             INSERT INTO jobs(
                 job_id, tenant, language, status, spec_json,
                 effective_spec_json, created_at_ms, started_at_ms
             )
             SELECT printf('running-%03d', n), 'tenant-a', 'python', 'running',
                    '{}', '{}', n, n
             FROM sequence",
        )
        .execute(&mut connection)
        .await
        .unwrap();

        // Exercise the supported decoded maximum of 1 MiB each for code and
        // stdin without retaining all 256 requested/effective specs in memory.
        let maximal_spec = format!(
            r#"{{"code":"{}","stdin":"{}"}}"#,
            "x".repeat(1024 * 1024),
            "y".repeat(1024 * 1024)
        );
        sqlx::query(
            "UPDATE jobs SET spec_json = ?2, effective_spec_json = ?2
             WHERE job_id = ?1",
        )
        .bind("running-256")
        .bind(&maximal_spec)
        .execute(&mut connection)
        .await
        .unwrap();
        drop(maximal_spec);

        // Raw fixture writes deliberately dirty the durable validation marker.
        // Revalidate before exercising the normal Store write path; the
        // failure trigger is installed afterward so this maintenance pass
        // does not reconcile it away.
        store.validate_integrity().await.unwrap();

        // Fail after 128 independently committed jobs. The failing job has
        // already been updated when its event insert runs, so rollback must
        // restore the complete running state and leave no partial evidence.
        sqlx::query(
            "CREATE TRIGGER fail_mid_recovery
             BEFORE INSERT ON events
             WHEN NEW.job_id = 'running-129' AND NEW.kind = 'finished'
             BEGIN
                 SELECT RAISE(ABORT, 'forced recovery event failure');
             END",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();

        let error = store.recover_stale_running().await.unwrap_err();
        assert!(error.to_string().contains("forced recovery event failure"));
        let mut connection = raw_connection(&db).await;
        let recovered: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE status = 'error'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        let still_running: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE status = 'running'")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!((recovered, still_running), (128, 128));
        let partial_evidence: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE job_id = 'running-129'")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(partial_evidence, 0);
        let receipt: Option<String> =
            sqlx::query_scalar("SELECT receipt_json FROM jobs WHERE job_id = 'running-129'")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert!(receipt.is_none());

        sqlx::query("DROP TRIGGER fail_mid_recovery")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();

        assert_eq!(store.recover_stale_running().await.unwrap(), 128);
        assert_eq!(store.recover_stale_running().await.unwrap(), 0);
        let maximal = store.get_job("running-256").await.unwrap().unwrap();
        assert_eq!(maximal.status, "error");
        let receipt: serde_json::Value =
            serde_json::from_str(maximal.receipt_json.as_deref().unwrap()).unwrap();
        assert_eq!(receipt["code_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(receipt["stdin_sha256"].as_str().unwrap().len(), 64);
        assert!(store.verify_event_chain("running-129").await.unwrap().valid);
        assert!(store.verify_event_chain("running-256").await.unwrap().valid);
    });
}

#[test]
fn current_markers_require_current_tables_and_missing_history_is_repaired() {
    sqlx::test_block_on(async {
        let false_current = test_db("false-current-marker");
        let mut connection = raw_connection(&false_current).await;
        create_v1_schema(&mut connection).await;
        sqlx::query("PRAGMA user_version = 2")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();

        let error = Store::open(&false_current).await.unwrap_err();
        assert!(
            error.to_string().contains("missing required column"),
            "unexpected schema validation error: {error}"
        );

        let missing_history = test_db("missing-history");
        let store = Store::open(&missing_history).await.unwrap();
        let mut connection = raw_connection(&missing_history).await;
        sqlx::query("DELETE FROM schema_migrations")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();
        drop(store);

        let repaired = Store::open(&missing_history).await.unwrap();
        assert_eq!(repaired.schema_version().await.unwrap(), 3);
        let mut connection = raw_connection(&missing_history).await;
        let migration_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(migration_count, 3);
        connection.close().await.unwrap();

        let lost_markers = test_db("lost-current-markers");
        let store = Store::open(&lost_markers).await.unwrap();
        store
            .create_job("preserved", "tenant-a", "python", "{}")
            .await
            .unwrap();
        store
            .finalize_with_event("preserved", "succeeded", Some(0), 0, None)
            .await
            .unwrap()
            .unwrap();
        let before = store.get_job("preserved").await.unwrap().unwrap();
        let before_events = store.events_for("preserved").await.unwrap();
        let mut connection = raw_connection(&lost_markers).await;
        sqlx::query("DELETE FROM schema_migrations")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("PRAGMA user_version = 0")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();
        drop(store);

        let reopened = Store::open(&lost_markers).await.unwrap();
        let after = reopened.get_job("preserved").await.unwrap().unwrap();
        assert_eq!(after.receipt_json, before.receipt_json);
        assert_eq!(
            reopened.events_for("preserved").await.unwrap(),
            before_events
        );
        assert_eq!(reopened.schema_version().await.unwrap(), 3);
    });
}

#[test]
fn existing_v2_reconciles_covering_summary_and_recovery_indexes() {
    sqlx::test_block_on(async {
        let db = test_db("covering-index-reconciliation");
        let store = Store::open(&db).await.unwrap();
        drop(store);

        let mut connection = raw_connection(&db).await;
        for name in [
            "idx_jobs_tenant_created_summary",
            "idx_jobs_tenant_status_created_summary",
            "idx_jobs_tenant_language_created_summary",
            "idx_jobs_tenant_status_language_created_summary",
            "idx_jobs_id_summary",
            "idx_jobs_status_created_recovery",
        ] {
            sqlx::query(&format!("DROP INDEX {name}"))
                .execute(&mut connection)
                .await
                .unwrap();
        }
        // Simulate an existing v2 database from before the covering-index
        // optimization. Reopen must remove these owned, non-covering forms.
        sqlx::query(
            "CREATE INDEX idx_jobs_tenant_created
             ON jobs(tenant, created_at_ms DESC, job_id DESC)",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query("CREATE INDEX idx_jobs_status ON jobs(status)")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();

        let store = Store::open(&db).await.unwrap();
        let mut connection = raw_connection(&db).await;
        let rows = sqlx::query(
            "SELECT name, sql FROM sqlite_schema
             WHERE type = 'index' AND name IN (
                 'idx_jobs_tenant_created_summary',
                 'idx_jobs_tenant_status_created_summary',
                 'idx_jobs_tenant_language_created_summary',
                 'idx_jobs_tenant_status_language_created_summary',
                 'idx_jobs_id_summary',
                 'idx_jobs_status_created_recovery'
             ) ORDER BY name",
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        assert_eq!(rows.len(), 6);
        for row in rows {
            let sql = row.get::<String, _>("sql").to_ascii_lowercase();
            for blob in ["spec_json", "effective_spec_json", "receipt_json"] {
                assert!(
                    !sql.contains(blob),
                    "{} unexpectedly indexes {blob}: {sql}",
                    row.get::<String, _>("name")
                );
            }
        }
        let stale: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type = 'index'
               AND name IN ('idx_jobs_tenant_created', 'idx_jobs_status')",
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(stale, 0);
        connection.close().await.unwrap();
        drop(store);
    });
}

#[test]
fn legacy_non_integer_storage_is_normalized_and_new_text_timestamps_are_rejected() {
    sqlx::test_block_on(async {
        let db = test_db("storage-classes");
        let mut connection = raw_connection(&db).await;
        create_v1_schema(&mut connection).await;
        sqlx::query(
            "INSERT INTO jobs VALUES
             ('weird', 'tenant-a', 'python', 'succeeded', '{}',
              'not-an-integer', 3.5, 'not-an-integer', 'not-an-integer'),
             ('wide-exit', 'tenant-a', 'python', 'succeeded', '{}',
              1, NULL, 2, 2147483648)",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO events(job_id, ts_ms, kind, data_json)
             VALUES ('weird', 'not-an-integer', 'stdout', '{}')",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query("PRAGMA user_version = 1")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();

        let store = Store::open(&db).await.unwrap();
        let row = store.get_job("weird").await.unwrap().unwrap();
        assert_eq!(row.created_at_ms, 0);
        assert!(row.started_at_ms.is_none());
        assert!(row.finished_at_ms.is_some());
        assert!(row.exit_code.is_none());
        assert!(store
            .get_job("wide-exit")
            .await
            .unwrap()
            .unwrap()
            .exit_code
            .is_none());
        assert_eq!(store.events_for("weird").await.unwrap()[0].ts_ms, 0);

        let mut connection = raw_connection(&db).await;
        let invalid_insert = sqlx::query(
            "INSERT INTO jobs(job_id, tenant, language, status, spec_json, created_at_ms)
             VALUES ('bad-time', 'tenant-a', 'python', 'queued', '{}', 'junk')",
        )
        .execute(&mut connection)
        .await;
        assert!(invalid_insert.is_err());
        let wide_exit = sqlx::query(
            "INSERT INTO jobs(
                 job_id, tenant, language, status, spec_json,
                 created_at_ms, finished_at_ms, exit_code
             ) VALUES (
                 'bad-exit', 'tenant-a', 'python', 'succeeded', '{}',
                 0, 1, 2147483648
             )",
        )
        .execute(&mut connection)
        .await;
        assert!(wide_exit.is_err());
        connection.close().await.unwrap();
    });
}

#[test]
fn shape_compatible_current_database_with_poisoned_types_fails_closed() {
    sqlx::test_block_on(async {
        let db = test_db("current-storage-poison");
        let store = Store::open(&db).await.unwrap();
        store
            .create_job("poisoned", "tenant-a", "python", "{}")
            .await
            .unwrap();
        drop(store);

        let mut connection = raw_connection(&db).await;
        sqlx::query("DROP TRIGGER coop_jobs_storage_guard_update")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("UPDATE jobs SET created_at_ms = 'not-an-integer' WHERE job_id = 'poisoned'")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();

        let error = Store::open(&db).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("jobs contains values incompatible"),
            "unexpected validation error: {error}"
        );
    });
}

#[test]
fn current_database_with_invalid_utf8_fails_closed() {
    sqlx::test_block_on(async {
        let db = test_db("current-invalid-utf8");
        let store = Store::open(&db).await.unwrap();
        store
            .create_job("invalid-utf8", "tenant-a", "python", "{}")
            .await
            .unwrap();
        drop(store);

        let mut connection = raw_connection(&db).await;
        sqlx::query("DROP TRIGGER coop_jobs_storage_guard_update")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE jobs SET tenant = CAST(X'80' AS TEXT)
             WHERE job_id = 'invalid-utf8'",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();

        let error = Store::open(&db).await.unwrap_err();
        assert!(
            error.to_string().contains("invalid UTF-8"),
            "unexpected validation error: {error}"
        );
    });
}

#[test]
fn json_valid_payload_with_invalid_utf8_still_fails_closed() {
    sqlx::test_block_on(async {
        let db = test_db("json-valid-invalid-utf8");
        let store = Store::open(&db).await.unwrap();
        store
            .create_job("invalid-json-utf8", "tenant-a", "python", "{}")
            .await
            .unwrap();

        let mut connection = raw_connection(&db).await;
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&mut connection)
            .await
            .unwrap();
        // SQLite reports this as syntactically valid JSON even though the
        // string value contains the lone invalid UTF-8 byte 0x80.
        sqlx::query(
            "UPDATE jobs
             SET spec_json = CAST(X'7B2278223A2280227D' AS TEXT)
             WHERE job_id = 'invalid-json-utf8'",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        let json_valid: i64 = sqlx::query_scalar(
            "SELECT json_valid(spec_json) FROM jobs WHERE job_id = 'invalid-json-utf8'",
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(json_valid, 1);
        let validation_revision: i64 = sqlx::query_scalar(
            "SELECT row_validation_revision FROM store_integrity WHERE singleton = 1",
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(validation_revision, 0);
        connection.close().await.unwrap();

        let write_error = store
            .create_job("must-not-launder", "tenant-a", "python", "{}")
            .await
            .unwrap_err();
        assert!(
            write_error.to_string().contains("row validation is stale"),
            "unexpected dirty-marker write error: {write_error}"
        );
        drop(store);
        let error = Store::open(&db).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("jobs.spec_json contains invalid UTF-8"),
            "unexpected validation error: {error}"
        );
    });
}

#[test]
fn validated_max_payload_writes_keep_healthy_reopen_on_the_bounded_fast_path() {
    sqlx::test_block_on(async {
        let db = test_db("validated-fast-reopen");
        let store = Store::open(&db).await.unwrap();
        let requested = json!({
            "code": "x".repeat(1024 * 1024),
            "stdin": "y".repeat(1024 * 1024),
        })
        .to_string();
        store
            .create_job("maximal", "tenant-a", "python", &requested)
            .await
            .unwrap();
        let effective = json!({
            "code": "a".repeat(1024 * 1024),
            "stdin": "b".repeat(1024 * 1024),
        });
        store
            .start_with_event_if_queued("maximal", &effective)
            .await
            .unwrap()
            .unwrap();
        let receipt = json!({
            "output": {
                "stdout": "s".repeat(1024 * 1024),
                "stderr": "e".repeat(1024 * 1024),
            }
        });
        store
            .finalize_with_event("maximal", "succeeded", Some(0), 1, Some(&receipt))
            .await
            .unwrap()
            .unwrap();

        let mut connection = raw_connection(&db).await;
        let before: i64 =
            sqlx::query_scalar("SELECT full_scan_count FROM store_integrity WHERE singleton = 1")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        let revision: i64 = sqlx::query_scalar(
            "SELECT row_validation_revision FROM store_integrity WHERE singleton = 1",
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(revision, 2);
        connection.close().await.unwrap();
        drop(store);

        // A healthy same-revision reopen must not re-read the multi-megabyte
        // JSON columns. The durable counter changes only when the O(bytes)
        // validator actually runs.
        let reopened = Store::open(&db).await.unwrap();
        let mut connection = raw_connection(&db).await;
        let after_reopen: i64 =
            sqlx::query_scalar("SELECT full_scan_count FROM store_integrity WHERE singleton = 1")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(after_reopen, before);
        connection.close().await.unwrap();

        reopened.validate_integrity().await.unwrap();
        let mut connection = raw_connection(&db).await;
        let after_explicit_check: i64 =
            sqlx::query_scalar("SELECT full_scan_count FROM store_integrity WHERE singleton = 1")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(after_explicit_check, before + 1);
    });
}

#[test]
fn current_database_with_malformed_json_fails_closed() {
    sqlx::test_block_on(async {
        let db = test_db("current-malformed-json");
        let store = Store::open(&db).await.unwrap();
        store
            .create_job("malformed-json", "tenant-a", "python", "{}")
            .await
            .unwrap();
        drop(store);

        let mut connection = raw_connection(&db).await;
        sqlx::query("DROP TRIGGER coop_jobs_storage_guard_update")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE jobs SET spec_json = 'not-json'
             WHERE job_id = 'malformed-json'",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();

        let error = Store::open(&db).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("jobs contains values incompatible"),
            "unexpected validation error: {error}"
        );
    });
}

#[test]
fn owned_storage_guards_are_recreated_on_open() {
    sqlx::test_block_on(async {
        let db = test_db("stale-storage-guard");
        let store = Store::open(&db).await.unwrap();
        store
            .create_job("guarded", "tenant-a", "python", "{}")
            .await
            .unwrap();
        drop(store);

        let mut connection = raw_connection(&db).await;
        sqlx::query("DROP TRIGGER coop_jobs_storage_guard_update")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TRIGGER coop_jobs_storage_guard_update
             BEFORE UPDATE ON jobs BEGIN SELECT 1; END",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();

        let store = Store::open(&db).await.unwrap();
        let mut connection = raw_connection(&db).await;
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&mut connection)
            .await
            .unwrap();
        let poisoned = sqlx::query(
            "UPDATE jobs SET created_at_ms = 'not-an-integer'
             WHERE job_id = 'guarded'",
        )
        .execute(&mut connection)
        .await;
        assert!(poisoned.is_err());
        connection.close().await.unwrap();
        assert!(store.get_job("guarded").await.unwrap().is_some());
    });
}

#[test]
fn invalid_event_sequences_and_exhausted_autoincrement_fail_closed() {
    sqlx::test_block_on(async {
        let db = test_db("event-sequence-domain");
        let store = Store::open(&db).await.unwrap();
        store
            .create_job("job", "tenant-a", "python", "{}")
            .await
            .unwrap();
        let mut connection = raw_connection(&db).await;
        let non_positive = sqlx::query(
            "INSERT INTO events(
                 seq, job_id, ts_ms, kind, data_json,
                 prev_hash, event_hash, hash_version
             ) VALUES (0, 'job', 0, 'legacy', '{}', '', '', 0)",
        )
        .execute(&mut connection)
        .await;
        assert!(non_positive.is_err());
        sqlx::query(
            "UPDATE sqlite_sequence SET seq = 9223372036854775807
             WHERE name = 'events'",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();
        drop(store);

        let error = Store::open(&db).await.unwrap_err();
        assert!(
            error.to_string().contains("AUTOINCREMENT counter"),
            "unexpected sequence validation error: {error}"
        );

        for (label, corrupt) in [
            (
                "lowered-event-counter",
                "UPDATE sqlite_sequence SET seq = 0 WHERE name = 'events'",
            ),
            (
                "missing-event-counter",
                "DELETE FROM sqlite_sequence WHERE name = 'events'",
            ),
        ] {
            let db = test_db(label);
            let store = Store::open(&db).await.unwrap();
            store
                .create_job("job", "tenant-a", "python", "{}")
                .await
                .unwrap();
            drop(store);
            let mut connection = raw_connection(&db).await;
            sqlx::query(corrupt).execute(&mut connection).await.unwrap();
            connection.close().await.unwrap();
            let error = Store::open(&db).await.unwrap_err();
            assert!(
                error.to_string().contains("AUTOINCREMENT counter"),
                "unexpected sequence validation error: {error}"
            );
        }
    });
}

#[test]
fn legacy_migration_preserves_the_deleted_sequence_high_watermark() {
    sqlx::test_block_on(async {
        let db = test_db("legacy-sequence-high-watermark");
        let mut connection = raw_connection(&db).await;
        create_v1_schema(&mut connection).await;
        sqlx::query(
            "INSERT INTO jobs VALUES
             ('legacy', 'tenant-a', 'python', 'queued', '{}', 1, NULL, NULL, NULL)",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO events(job_id, ts_ms, kind, data_json) VALUES
             ('legacy', 1, 'legacy', '{}'),
             ('legacy', 2, 'legacy', '{}'),
             ('legacy', 3, 'legacy', '{}')",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query("DELETE FROM events WHERE seq = 3")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("PRAGMA user_version = 1")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();

        let store = Store::open(&db).await.unwrap();
        let appended = store
            .append_event_row("legacy", "stdout", &json!({"line":"new"}))
            .await
            .unwrap();
        assert_eq!(appended.seq, 4);
        assert_eq!(
            store
                .events_for("legacy")
                .await
                .unwrap()
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            [1, 2, 4]
        );
    });
}

#[test]
fn queued_recovery_cursor_is_stable_when_earlier_rows_disappear() {
    sqlx::test_block_on(async {
        let db = test_db("queued-pages");
        let store = Store::open(&db).await.unwrap();
        for (job_id, tenant) in [("q3", "tenant-c"), ("q1", "tenant-a"), ("q2", "tenant-b")] {
            store
                .create_job(job_id, tenant, "python", "{}")
                .await
                .unwrap();
        }
        let mut connection = raw_connection(&db).await;
        sqlx::query("UPDATE jobs SET created_at_ms = 42")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();
        store.validate_integrity().await.unwrap();

        let first = store.queued_jobs_page(None, 2).await.unwrap();
        assert_eq!(
            first
                .iter()
                .map(|row| row.job_id.as_str())
                .collect::<Vec<_>>(),
            ["q1", "q2"]
        );
        assert_eq!(first[0].tenant, "tenant-a");
        let cursor = JobCursor::from(first.last().unwrap());
        store
            .cancel_queued_with_event("q1", "tenant-a", None)
            .await
            .unwrap()
            .unwrap();
        let second = store.queued_jobs_page(Some(&cursor), 2).await.unwrap();
        assert_eq!(
            second
                .iter()
                .map(|row| row.job_id.as_str())
                .collect::<Vec<_>>(),
            ["q3"]
        );
    });
}

#[test]
fn job_page_supports_one_internal_lookahead_row() {
    sqlx::test_block_on(async {
        let db = test_db("job-lookahead");
        let store = Store::open(&db).await.unwrap();
        let mut connection = raw_connection(&db).await;
        sqlx::query(
            "WITH RECURSIVE sequence(n) AS (
                 VALUES(1) UNION ALL SELECT n + 1 FROM sequence WHERE n < 501
             )
             INSERT INTO jobs(job_id, tenant, language, status, spec_json, created_at_ms)
             SELECT printf('job-%03d', n), 'tenant-a', 'python', 'queued', '{}', 42
             FROM sequence",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();

        assert_eq!(store.list_jobs(None, 501).await.unwrap().len(), 500);
        assert_eq!(
            store
                .list_jobs_page(ListJobsQuery {
                    limit: 501,
                    ..ListJobsQuery::default()
                })
                .await
                .unwrap()
                .len(),
            501
        );
        assert_eq!(
            store
                .list_job_summaries_page(ListJobsQuery {
                    limit: 501,
                    ..ListJobsQuery::default()
                })
                .await
                .unwrap()
                .len(),
            501
        );
    });
}

#[test]
fn job_summary_page_does_not_load_maximal_payload_columns() {
    sqlx::test_block_on(async {
        let db = test_db("job-summary-projection");
        let store = Store::open(&db).await.unwrap();
        let mut connection = raw_connection(&db).await;
        sqlx::query(
            "WITH RECURSIVE sequence(n) AS (
                 VALUES(1) UNION ALL SELECT n + 1 FROM sequence WHERE n < 501
             )
             INSERT INTO jobs(job_id, tenant, language, status, spec_json, created_at_ms)
             SELECT printf('job-%03d', n), 'tenant-a', 'python', 'queued', '{}', 42
             FROM sequence",
        )
        .execute(&mut connection)
        .await
        .unwrap();

        // A requested spec can contain 1 MiB each of code and stdin. Exercise
        // that decoded-size boundary in every omitted JSON column.
        let maximal_payload = format!(r#"{{"payload":"{}"}}"#, "x".repeat(2 * 1024 * 1024));
        sqlx::query(
            "UPDATE jobs
             SET spec_json = ?2, effective_spec_json = ?2, receipt_json = ?2
             WHERE job_id = ?1",
        )
        .bind("job-501")
        .bind(&maximal_payload)
        .execute(&mut connection)
        .await
        .unwrap();
        drop(maximal_payload);

        connection.close().await.unwrap();
        drop(store);
        // Opening performs full physical/value validation. It must validate
        // maximal JSON in SQLite without returning those blobs in a 256-row
        // UTF-8 validation batch.
        let store = Store::open(&db).await.unwrap();

        let summaries = store
            .list_job_summaries_page(ListJobsQuery {
                limit: 501,
                ..ListJobsQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(summaries.len(), 501);
        assert_eq!(summaries[0].job_id, "job-501");
        assert_eq!(summaries[0].tenant, "tenant-a");
        let point = store.get_job_summary("job-501").await.unwrap().unwrap();
        assert_eq!(point.job_id, "job-501");
        assert_eq!(point.status, "queued");

        // Poison an omitted column after startup. If the summary query ever
        // regresses to SELECT * (or decodes the full JobRow), SQLx will reject
        // this invalid UTF-8 value just as the compatibility API does below.
        let mut connection = raw_connection(&db).await;
        sqlx::query("DROP TRIGGER coop_jobs_storage_guard_update")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE jobs SET spec_json = CAST(X'80' AS TEXT)
             WHERE job_id = 'job-501'",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();

        assert_eq!(
            store
                .list_job_summaries_page(ListJobsQuery {
                    limit: 501,
                    ..ListJobsQuery::default()
                })
                .await
                .unwrap()
                .len(),
            501
        );
        assert_eq!(
            store
                .get_job_summary("job-501")
                .await
                .unwrap()
                .unwrap()
                .status,
            "queued"
        );
        assert!(store.get_job("job-501").await.is_err());
        assert!(store
            .list_jobs_page(ListJobsQuery {
                limit: 501,
                ..ListJobsQuery::default()
            })
            .await
            .is_err());
    });
}

#[test]
fn oversized_retention_history_is_drained_under_a_hard_event_budget() {
    sqlx::test_block_on(async {
        let db = test_db("retention-event-budget");
        let store = Store::open(&db).await.unwrap();
        store
            .create_job("heavy", "tenant-a", "python", "{}")
            .await
            .unwrap();
        store
            .finalize_with_event("heavy", "succeeded", Some(0), 0, None)
            .await
            .unwrap()
            .unwrap();

        let legacy_rows = (MAX_RETENTION_EVENTS_PER_BATCH + 8) as i64;
        let mut connection = raw_connection(&db).await;
        sqlx::query(
            "WITH RECURSIVE sequence(n) AS (
                 VALUES(1) UNION ALL SELECT n + 1 FROM sequence WHERE n < ?2
             )
             INSERT INTO events(job_id, ts_ms, kind, data_json)
             SELECT ?1, n, 'legacy', '{}' FROM sequence",
        )
        .bind("heavy")
        .bind(legacy_rows)
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();
        store.validate_integrity().await.unwrap();

        std::thread::sleep(std::time::Duration::from_millis(5));
        let expected_events = legacy_rows as u64 + 2;
        let first = store.prune_older_than_batch(0, 1).await.unwrap();
        assert_eq!(first.jobs_deleted, 0);
        assert_eq!(first.events_deleted, MAX_RETENTION_EVENTS_PER_BATCH);
        assert!(first.more_remaining);
        assert!(store.get_job("heavy").await.unwrap().is_none());
        assert!(store.get_job_summary("heavy").await.unwrap().is_none());
        assert!(store.events_for("heavy").await.unwrap().is_empty());
        assert!(store.list_jobs(None, 10).await.unwrap().is_empty());
        assert_eq!(store.last_seq("heavy").await.unwrap(), 0);
        assert_eq!(
            store.event_chain_head("heavy").await.unwrap().event_count,
            0
        );
        assert!(store.count_by_status().await.unwrap().is_empty());
        let mut connection = raw_connection(&db).await;
        let physical_job: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE job_id = 'heavy'")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        let tombstone: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM retention_tombstones WHERE job_id = 'heavy'")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        let remaining_events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE job_id = 'heavy'")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(physical_job, 1);
        assert_eq!(tombstone, 1);
        assert_eq!(
            remaining_events as u64,
            expected_events - MAX_RETENTION_EVENTS_PER_BATCH
        );
        connection.close().await.unwrap();

        // A restart between chunks preserves both the hidden-state invariant
        // and the ability for the next sweep to resume draining.
        drop(store);
        let store = Store::open(&db).await.unwrap();
        assert!(store.get_job("heavy").await.unwrap().is_none());
        assert!(store.events_for("heavy").await.unwrap().is_empty());

        let second = store.prune_older_than_batch(0, 1).await.unwrap();
        assert_eq!(second.jobs_deleted, 1);
        assert_eq!(
            second.events_deleted,
            expected_events - MAX_RETENTION_EVENTS_PER_BATCH
        );
        assert!(!second.more_remaining);
        assert!(store.get_job("heavy").await.unwrap().is_none());
    });
}

#[test]
fn logical_storage_quota_is_atomic_tenant_scoped_and_reopen_safe() {
    sqlx::test_block_on(async {
        let db = test_db("logical-quota");
        let per_job = JOB_COMPLETION_RESERVE_BYTES + 16 * 1024;
        let limits = StorageLimits::new(per_job * 3, per_job + 1024, 0);
        let store = Store::open_with_limits(&db, limits).await.unwrap();
        let spec = r#"{"language":"python","code":"print('é')","limits":{"mem_mb":64}}"#;
        store
            .create_job_with_event_idempotent("quota-one", "tenant-a", "python", spec, 64, None)
            .await
            .unwrap();
        let error = store
            .create_job_with_event_idempotent("quota-two", "tenant-a", "python", spec, 64, None)
            .await
            .unwrap_err();
        assert_eq!(capacity_error_kind(&error), Some(CapacityErrorKind::Tenant));
        assert!(store.get_job("quota-two").await.unwrap().is_none());

        store
            .create_job_with_event_idempotent("quota-other", "tenant-b", "python", spec, 64, None)
            .await
            .unwrap();
        store
            .create_job_with_event_idempotent("quota-third", "tenant-c", "python", spec, 64, None)
            .await
            .unwrap();
        let error = store
            .create_job_with_event_idempotent("quota-global", "tenant-d", "python", spec, 64, None)
            .await
            .unwrap_err();
        assert_eq!(capacity_error_kind(&error), Some(CapacityErrorKind::Global));
        drop(store);
        let reopened = Store::open_with_limits(&db, limits).await.unwrap();
        reopened.validate_integrity().await.unwrap();
    });
}

#[test]
fn concurrent_near_quota_creates_have_exactly_one_winner() {
    sqlx::test_block_on(async {
        let db = test_db("logical-quota-race");
        let per_job = JOB_COMPLETION_RESERVE_BYTES + 16 * 1024;
        let limits = StorageLimits::new(per_job * 2, per_job + 1024, 0);
        let store = std::sync::Arc::new(Store::open_with_limits(&db, limits).await.unwrap());
        let spec = r#"{"language":"python","code":"print(1)"}"#;
        let left_store = std::sync::Arc::clone(&store);
        let right_store = std::sync::Arc::clone(&store);
        let (left, right) = futures_util::future::join(
            async move {
                left_store
                    .create_job_with_event_idempotent(
                        "race-left",
                        "tenant-a",
                        "python",
                        spec,
                        256,
                        None,
                    )
                    .await
            },
            async move {
                right_store
                    .create_job_with_event_idempotent(
                        "race-right",
                        "tenant-a",
                        "python",
                        spec,
                        256,
                        None,
                    )
                    .await
            },
        )
        .await;
        assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
        let failure = left.err().or_else(|| right.err()).unwrap();
        assert_eq!(
            capacity_error_kind(&failure),
            Some(CapacityErrorKind::Tenant)
        );
    });
}

#[test]
fn idempotency_is_tenant_scoped_fingerprint_bound_and_retention_coupled() {
    sqlx::test_block_on(async {
        let db = test_db("idempotency");
        let store = Store::open(&db).await.unwrap();
        let request = IdempotencyRequest {
            key: "opaque-key-1".to_string(),
            request_sha256: "a".repeat(64),
        };
        let first = store
            .create_job_with_event_idempotent(
                "idem-job",
                "tenant-a",
                "python",
                r#"{"language":"python","code":"print(1)"}"#,
                256,
                Some(&request),
            )
            .await
            .unwrap();
        assert!(matches!(first, CreateJobOutcome::Created(_)));
        drop(store);
        let store = Store::open(&db).await.unwrap();
        let replay = store
            .create_job_with_event_idempotent(
                "unused-generated-id",
                "tenant-a",
                "python",
                r#"{"language":"python","code":"print(1)"}"#,
                256,
                Some(&request),
            )
            .await
            .unwrap();
        assert_eq!(
            replay,
            CreateJobOutcome::Replayed {
                job_id: "idem-job".to_string()
            }
        );
        assert_eq!(store.events_for("idem-job").await.unwrap().len(), 1);
        assert!(store
            .get_job("unused-generated-id")
            .await
            .unwrap()
            .is_none());

        let conflict = IdempotencyRequest {
            key: request.key.clone(),
            request_sha256: "b".repeat(64),
        };
        let error = store
            .create_job_with_event_idempotent(
                "conflict-job",
                "tenant-a",
                "python",
                r#"{"language":"python","code":"print(2)"}"#,
                256,
                Some(&conflict),
            )
            .await
            .unwrap_err();
        assert!(is_idempotency_conflict(&error));
        assert_eq!(
            store
                .lookup_idempotency("tenant-b", &request)
                .await
                .unwrap(),
            IdempotencyLookup::Miss
        );

        store
            .finalize_with_event("idem-job", "succeeded", Some(0), 1, None)
            .await
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        store.prune_older_than_batch(0, 1).await.unwrap();
        assert_eq!(
            store
                .lookup_idempotency("tenant-a", &request)
                .await
                .unwrap(),
            IdempotencyLookup::Miss
        );
    });
}

#[test]
fn current_schema_missing_idempotency_table_fails_closed() {
    sqlx::test_block_on(async {
        let db = test_db("missing-idempotency-table");
        let store = Store::open(&db).await.unwrap();
        let request = IdempotencyRequest {
            key: "durable-key".to_string(),
            request_sha256: "c".repeat(64),
        };
        store
            .create_job_with_event_idempotent(
                "durable-idempotent-job",
                "tenant-a",
                "python",
                r#"{"language":"python","code":"print(1)"}"#,
                256,
                Some(&request),
            )
            .await
            .unwrap();
        drop(store);
        let mut connection = raw_connection(&db).await;
        sqlx::query("DROP TABLE idempotency_keys")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();
        let error = Store::open(&db)
            .await
            .expect_err("v3 mapping loss must not become an idempotency miss");
        assert!(
            error.to_string().contains("idempotency mappings"),
            "{error}"
        );
    });
}

#[test]
fn filesystem_reserve_rejects_growth_but_not_open_or_recovery() {
    sqlx::test_block_on(async {
        let db = test_db("filesystem-reserve");
        let limits = StorageLimits::new(i64::MAX as u64, i64::MAX as u64, u64::MAX);
        let store = Store::open_with_limits(&db, limits).await.unwrap();
        let error = store
            .create_job("blocked", "tenant-a", "python", r#"{"code":"print(1)"}"#)
            .await
            .unwrap_err();
        assert_eq!(
            capacity_error_kind(&error),
            Some(CapacityErrorKind::Filesystem)
        );
        assert!(store.get_job("blocked").await.unwrap().is_none());
        drop(store);
        Store::open_with_limits(&db, limits).await.unwrap();
    });
}

#[test]
fn retention_commit_failure_rolls_back_and_releases_the_writer() {
    sqlx::test_block_on(async {
        let db = test_db("retention-commit-rollback");
        let store = Store::open(&db).await.unwrap();
        store
            .create_job("expired", "tenant-a", "python", "{}")
            .await
            .unwrap();
        store
            .finalize_with_event("expired", "succeeded", Some(0), 0, None)
            .await
            .unwrap()
            .unwrap();

        let mut connection = raw_connection(&db).await;
        sqlx::query("CREATE TABLE retention_parent(id INTEGER PRIMARY KEY)")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE retention_child(
                 parent_id INTEGER REFERENCES retention_parent(id)
                     DEFERRABLE INITIALLY DEFERRED
             )",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TRIGGER force_retention_commit_failure
             AFTER DELETE ON jobs WHEN OLD.job_id = 'expired'
             BEGIN
                 INSERT INTO retention_child(parent_id) VALUES (999);
             END",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();

        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(store.prune_older_than_batch(0, 1).await.is_err());
        assert!(store.get_job("expired").await.unwrap().is_some());
        store
            .create_job("after-failure", "tenant-a", "python", "{}")
            .await
            .unwrap();
        store.compact().await.unwrap();
    });
}

#[test]
fn migration_commit_failure_does_not_leave_partial_history() {
    sqlx::test_block_on(async {
        let db = test_db("migration-commit-rollback");
        let store = Store::open(&db).await.unwrap();
        drop(store);

        let mut connection = raw_connection(&db).await;
        sqlx::query("DELETE FROM schema_migrations")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE migration_parent(id INTEGER PRIMARY KEY)")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE migration_child(
                 parent_id INTEGER REFERENCES migration_parent(id)
                     DEFERRABLE INITIALLY DEFERRED
             )",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TRIGGER force_migration_commit_failure
             AFTER INSERT ON schema_migrations
             BEGIN
                 INSERT INTO migration_child(parent_id) VALUES (999);
             END",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();

        assert!(Store::open(&db).await.is_err());
        let mut connection = raw_connection(&db).await;
        let history_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(history_count, 0);
        connection.close().await.unwrap();
    });
}

#[test]
fn rejects_blank_identity_and_invalid_json() {
    sqlx::test_block_on(async {
        let db = test_db("validation");
        let store = Store::open(&db).await.unwrap();
        assert!(store.create_job("blank", "", "python", "{}").await.is_err());
        assert!(store
            .create_job("invalid", "tenant-a", "python", "not-json")
            .await
            .is_err());
        assert!(store.list_jobs(None, 50).await.unwrap().is_empty());
    });
}

#[cfg(unix)]
#[test]
fn database_parent_file_and_sidecars_are_owner_only() {
    use std::os::unix::fs::symlink;
    use std::os::unix::fs::PermissionsExt;

    sqlx::test_block_on(async {
        let db = test_db("permissions");
        let store = Store::open(&db).await.unwrap();
        store
            .create_job("job", "tenant-a", "python", "{}")
            .await
            .unwrap();
        store.compact().await.unwrap();
        assert_eq!(
            std::fs::metadata(db.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&db).unwrap().permissions().mode() & 0o777,
            0o600
        );
        for suffix in ["-wal", "-shm"] {
            let candidate = PathBuf::from(format!("{}{suffix}", db.display()));
            if candidate.exists() {
                assert_eq!(
                    std::fs::metadata(candidate).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }

        let symlink_db = test_db("symlink");
        std::fs::create_dir_all(symlink_db.parent().unwrap()).unwrap();
        let target = symlink_db.with_extension("target");
        std::fs::write(&target, []).unwrap();
        symlink(&target, &symlink_db).unwrap();
        assert!(Store::open(&symlink_db).await.is_err());
    });
}
