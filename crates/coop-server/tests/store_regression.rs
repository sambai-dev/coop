use coop_store::Store;

#[tokio::test]
async fn recover_stale_running_sets_finished_at() {
    let db = std::env::temp_dir().join(format!("coop-test-recover-{}.db", uuid::Uuid::now_v7()));
    let _ = std::fs::remove_file(&db);
    let store = Store::open(&db).await.unwrap();
    store.create_job("j1", "t1", "python", "{}").await.unwrap();
    store.create_job("j2", "t1", "python", "{}").await.unwrap();
    // j1 stays queued, j2 moves to running
    store.create_job("j3", "t1", "python", "{}").await.unwrap();
    let _ = store.set_started_if_queued("j2").await.unwrap();
    store.finish("j3", "succeeded", Some(0)).await.unwrap();

    let n = store.recover_stale_running().await.unwrap();
    assert_eq!(n, 2, "j1 queued + j2 running should be recovered");

    for id in ["j1", "j2"] {
        let row = store.get_job(id).await.unwrap().unwrap();
        assert_eq!(row.status, "error", "{id} must be error");
        assert!(
            row.finished_at_ms.is_some(),
            "{id} finished_at_ms must be set (deep-hunt regression: was NULL via ?2 mismatch)"
        );
    }
    let row = store.get_job("j3").await.unwrap().unwrap();
    assert_eq!(row.status, "succeeded", "terminal job untouched");
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn conditional_start_and_cancel_are_atomic() {
    let db = std::env::temp_dir().join(format!("coop-test-cond-{}.db", uuid::Uuid::now_v7()));
    let _ = std::fs::remove_file(&db);
    let store = Store::open(&db).await.unwrap();
    store.create_job("q1", "t1", "python", "{}").await.unwrap();

    // cancel while queued → succeeds, subsequent start must fail
    assert!(store.cancel_if_queued("q1").await.unwrap());
    assert!(
        !store.set_started_if_queued("q1").await.unwrap(),
        "cancelled-while-queued job must not transition to running"
    );

    // fresh job: start succeeds, subsequent cancel must fail (now running)
    store.create_job("q2", "t1", "python", "{}").await.unwrap();
    assert!(store.set_started_if_queued("q2").await.unwrap());
    assert!(
        !store.cancel_if_queued("q2").await.unwrap(),
        "running job must not be cancelled via queued path"
    );

    // double-cancel / double-start are idempotent false
    assert!(!store.cancel_if_queued("q2").await.unwrap());
    assert!(!store.set_started_if_queued("q2").await.unwrap());

    let _ = std::fs::remove_file(&db);
}
