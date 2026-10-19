use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_driver_job_queue_consumers_run_a_healthy_job_once() {
    let (clock, _) = clock_at(20_000);
    let backend = backend(clock.clone()).await.unwrap();
    let left = JobQueue::new(
        DriverQueueStore::new(
            Arc::clone(&backend),
            Scope::local(),
            clock.clone(),
            Duration::from_secs(30),
        )
        .unwrap(),
    );
    let right = JobQueue::new(
        DriverQueueStore::new(
            Arc::clone(&backend),
            Scope::local(),
            clock,
            Duration::from_secs(30),
        )
        .unwrap(),
    );
    left.enqueue(&Namespace::ROOT, vec![build("once")]).await;

    let (left_runs, right_runs) = tokio::join!(
        left.run(Arc::new(ReferenceEngine::new()), Selection::All, 0),
        right.run(Arc::new(ReferenceEngine::new()), Selection::All, 0),
    );
    assert_eq!(left_runs.unwrap().len() + right_runs.unwrap().len(), 1);
    let state = left.snapshot().await;
    assert!(state.pending.is_empty());
    assert_eq!(state.history.len(), 1);
}

#[tokio::test]
async fn driver_queue_import_is_idempotent_and_preserves_newer_target_jobs() {
    let (clock, _) = clock_at(25_000);
    let backend = backend(clock.clone()).await.unwrap();
    let store =
        DriverQueueStore::new(backend, Scope::local(), clock, Duration::from_secs(30)).unwrap();
    let now = chrono::Utc::now();
    let newer = QueuedJob {
        id: "newer-native-job".into(),
        root: Namespace::ROOT.to_string(),
        job: build("native"),
        queued_at: now,
        attempts: 0,
        last_error: None,
    };
    let target_wins = QueuedJob {
        id: "target-wins".into(),
        root: Namespace::ROOT.to_string(),
        job: build("new-target-value"),
        queued_at: now,
        attempts: 0,
        last_error: None,
    };
    store.enqueue_item(newer.clone()).unwrap();
    store.enqueue_item(target_wins.clone()).unwrap();
    let legacy_job = QueuedJob {
        id: "legacy-job".into(),
        root: Namespace::ROOT.to_string(),
        job: build("legacy"),
        queued_at: now - chrono::Duration::hours(1),
        attempts: 0,
        last_error: None,
    };
    let legacy_state = QueueState {
        pending: vec![
            legacy_job.clone(),
            QueuedJob {
                job: build("old-target-value"),
                ..target_wins.clone()
            },
        ],
        history: vec![JobRun {
            id: "completed-legacy".into(),
            job: "build_beliefs".into(),
            root: Namespace::ROOT.to_string(),
            ran_at: now - chrono::Duration::days(1),
            outcome: "done".into(),
            reason: None,
            built: Some(3),
            stored: 0,
        }],
    };

    let imported = store.import_legacy(&legacy_state).unwrap();
    assert_eq!(imported.jobs.imported, 1);
    assert_eq!(imported.jobs.conflicts, 1);
    assert_eq!(imported.runs.imported, 1);
    let repeated = store.import_legacy(&legacy_state).unwrap();
    assert_eq!(repeated.jobs.identical, 1);
    assert_eq!(repeated.jobs.conflicts, 1);
    assert_eq!(repeated.runs.identical, 1);
    let loaded = store.load();
    assert_eq!(loaded.pending[0], legacy_job);
    assert_eq!(loaded.pending[1], newer);
    assert_eq!(loaded.pending[2], target_wins);
    assert_eq!(loaded.history, legacy_state.history);

    assert!(
        store
            .save(&QueueState {
                pending: vec![newer.clone()],
                history: Vec::new(),
            })
            .is_err()
    );
    let replaced = store.load();
    assert_eq!(replaced.pending, loaded.pending);
    assert_eq!(replaced.history, loaded.history);
}
