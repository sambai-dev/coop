use coop_store::{JobCursor, ListJobsQuery, Store};
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

fn test_db(label: &str) -> PathBuf {
    std::env::temp_dir()
        .join(format!("coop-store-{label}-{}", uuid::Uuid::now_v7()))
        .join("coop.db")
}

#[tokio::test]
async fn schema_lifecycle_recovery_and_receipt_are_consistent() {
    let db = test_db("lifecycle");
    let store = Store::open(&db).await.unwrap();
    assert_eq!(store.schema_version().await.unwrap(), 3);

    store
        .create_job("queued", "tenant-a", "python", r#"{"code":"1"}"#)
        .await
        .unwrap();
    store
        .create_job("running", "tenant-a", "python", r#"{"code":"2"}"#)
        .await
        .unwrap();
    store
        .create_job("done", "tenant-a", "python", r#"{"code":"3"}"#)
        .await
        .unwrap();

    let effective = json!({"language":"python","limits":{"wall_seconds":5}});
    let started = store
        .start_with_event_if_queued("running", &effective)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(started.kind, "started");
    assert_eq!(started.hash_version, 1);

    store
        .finalize_with_event(
            "done",
            "succeeded",
            Some(0),
            7,
            Some(&json!({"policy":"default"})),
        )
        .await
        .unwrap()
        .unwrap();

    let recovered = store.recover_stale_running().await.unwrap();
    assert_eq!(recovered, 1, "only work which actually started is failed");

    let queued = store.get_job("queued").await.unwrap().unwrap();
    assert_eq!(queued.status, "queued", "accepted work survives a restart");
    assert!(queued.finished_at_ms.is_none());
    assert_eq!(store.queued_job_ids(10).await.unwrap(), vec!["queued"]);

    let running = store.get_job("running").await.unwrap().unwrap();
    assert_eq!(running.status, "error");
    assert!(running.finished_at_ms.is_some());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(running.effective_spec_json.as_deref().unwrap())
            .unwrap(),
        effective
    );
    let receipt: serde_json::Value =
        serde_json::from_str(running.receipt_json.as_deref().unwrap()).unwrap();
    assert_eq!(receipt["terminal_reason"], "server_restarted");
    assert_eq!(receipt["event_chain"]["events"], 3);

    let events = store.events_for("running").await.unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
        ["accepted", "started", "finished"]
    );
    assert_eq!(events[1].prev_hash, events[0].event_hash);
    assert_eq!(events[2].prev_hash, events[1].event_hash);
    let verification = store.verify_event_chain("running").await.unwrap();
    assert!(verification.valid);
    assert_eq!(verification.head.verified_event_count, 3);
    assert_eq!(verification.head.legacy_event_count, 0);
}

#[tokio::test]
async fn queued_cancel_is_tenant_scoped_atomic_and_idempotent() {
    let db = test_db("cancel");
    let store = Store::open(&db).await.unwrap();
    store
        .create_job("q1", "tenant-a", "python", "{}")
        .await
        .unwrap();

    assert!(store
        .cancel_queued_with_event("q1", "tenant-b", None)
        .await
        .unwrap()
        .is_none());
    assert_eq!(store.get_job("q1").await.unwrap().unwrap().status, "queued");

    let finished = store
        .cancel_queued_with_event(
            "q1",
            "tenant-a",
            Some(&json!({"terminal_reason":"user_cancelled"})),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(finished.kind, "finished");
    assert_eq!(finished.data["status"], "cancelled");
    assert!(!finished.event_hash.is_empty());

    assert!(store
        .cancel_queued_with_event("q1", "tenant-a", None)
        .await
        .unwrap()
        .is_none());
    assert!(!store.set_started_if_queued("q1").await.unwrap());

    let row = store.get_job("q1").await.unwrap().unwrap();
    assert_eq!(row.status, "cancelled");
    let receipt: serde_json::Value =
        serde_json::from_str(row.receipt_json.as_deref().unwrap()).unwrap();
    assert_eq!(receipt["terminal_reason"], "user_cancelled");
    assert_eq!(receipt["finished_at_ms"], row.finished_at_ms.unwrap());
    assert_eq!(receipt["duration_ms"], 0);
    assert_eq!(receipt["event_chain"]["head"], finished.event_hash);
    assert_eq!(receipt["event_chain"]["events"], 2);
    assert_eq!(store.events_for("q1").await.unwrap().len(), 2);
}

#[tokio::test]
async fn concurrent_finalizers_have_exactly_one_winner_and_one_finished_event() {
    let db = test_db("finalize-race");
    let store = Store::open(&db).await.unwrap();
    store
        .create_job("race", "tenant-a", "python", "{}")
        .await
        .unwrap();
    store
        .start_with_event_if_queued("race", &json!({"effective":true}))
        .await
        .unwrap()
        .unwrap();

    let succeeded_receipt = json!({"worker":1});
    let failed_receipt = json!({"worker":2});
    let (succeeded, failed) = tokio::join!(
        store.finalize_with_event("race", "succeeded", Some(0), 10, Some(&succeeded_receipt)),
        store.finalize_with_event("race", "failed", Some(1), 11, Some(&failed_receipt)),
    );
    let wins = [succeeded.unwrap(), failed.unwrap()]
        .into_iter()
        .filter(Option::is_some)
        .count();
    assert_eq!(wins, 1);

    let events = store.events_for("race").await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "finished")
            .count(),
        1
    );
    assert_eq!(events.len(), 3);
    assert!(store.verify_event_chain("race").await.unwrap().valid);
}

#[tokio::test]
async fn job_and_event_keyset_pagination_are_stable_and_strictly_scoped() {
    let db = test_db("pagination");
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
    store
        .finalize_with_event("a2", "succeeded", Some(0), 1, None)
        .await
        .unwrap()
        .unwrap();

    let filtered = store
        .list_jobs_page(ListJobsQuery {
            tenant: Some("tenant-a".to_string()),
            status: Some("succeeded".to_string()),
            language: Some("node".to_string()),
            limit: 50,
            ..ListJobsQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].job_id, "a2");

    let first = store
        .list_jobs_page(ListJobsQuery {
            tenant: Some("tenant-a".to_string()),
            limit: 2,
            ..ListJobsQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(first.len(), 2);
    let last = first.last().unwrap();
    let second = store
        .list_jobs_page(ListJobsQuery {
            tenant: Some("tenant-a".to_string()),
            before: Some(JobCursor {
                created_at_ms: last.created_at_ms,
                job_id: last.job_id.clone(),
            }),
            limit: 2,
            ..ListJobsQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(second.len(), 1);
    assert!(!first
        .iter()
        .any(|left| second.iter().any(|right| left.job_id == right.job_id)));

    assert!(store.list_jobs(Some(""), 50).await.unwrap().is_empty());
    assert_eq!(store.list_jobs(None, 50).await.unwrap().len(), 4);
    assert_eq!(
        store.count_by_status_for_tenant("tenant-a").await.unwrap(),
        vec![("queued".to_string(), 2), ("succeeded".to_string(), 1)]
    );

    for index in 1..=3 {
        store
            .append_event_row("a1", "stdout", &json!({"line":index}))
            .await
            .unwrap();
    }
    let page_one = store.events_after("a1", 0, 2).await.unwrap();
    assert_eq!(page_one.len(), 2);
    let page_two = store
        .events_after("a1", page_one.last().unwrap().seq, 2)
        .await
        .unwrap();
    assert_eq!(page_two.len(), 2);
    assert!(page_two[0].seq > page_one[1].seq);
    assert!(store.verify_event_chain("a1").await.unwrap().valid);
}

#[tokio::test]
async fn retention_is_finished_time_based_bounded_and_cascades_events() {
    let db = test_db("retention");
    let store = Store::open(&db).await.unwrap();
    for job_id in ["old-1", "old-2", "keep-queued"] {
        store
            .create_job(job_id, "tenant-a", "python", "{}")
            .await
            .unwrap();
    }
    for job_id in ["old-1", "old-2"] {
        store
            .finalize_with_event(job_id, "succeeded", Some(0), 1, None)
            .await
            .unwrap()
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(5)).await;

    let first = store.prune_older_than_batch(0, 1).await.unwrap();
    assert_eq!(first.jobs_deleted, 1);
    assert_eq!(first.events_deleted, 2);
    assert!(first.more_remaining);

    let second = store.prune_older_than_batch(0, 1).await.unwrap();
    assert_eq!(second.jobs_deleted, 1);
    assert_eq!(second.events_deleted, 2);
    assert!(!second.more_remaining);
    assert!(store.get_job("keep-queued").await.unwrap().is_some());
    assert_eq!(
        store.list_jobs(Some("tenant-a"), 50).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn invalid_identity_and_json_are_rejected_before_insertion() {
    let db = test_db("validation");
    let store = Store::open(&db).await.unwrap();
    assert!(store
        .create_job("blank-tenant", "", "python", "{}")
        .await
        .is_err());
    assert!(store
        .create_job("bad-json", "tenant-a", "python", "not-json")
        .await
        .is_err());
    assert!(store.list_jobs(None, 50).await.unwrap().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn sqlite_directory_database_and_sidecars_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let db = test_db("permissions");
    let store = Store::open(&db).await.unwrap();
    store
        .create_job("permissions", "tenant-a", "python", "{}")
        .await
        .unwrap();

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
}
