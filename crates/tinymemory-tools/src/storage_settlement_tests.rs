use super::*;

#[tokio::test]
async fn concurrent_settlements_at_history_limit_both_ack_jobs() {
    let (clock, _) = clock_at(30_000);
    let backend = backend(clock.clone()).await.unwrap();
    let left = DriverQueueStore::new(
        Arc::clone(&backend),
        Scope::local(),
        clock.clone(),
        Duration::from_secs(30),
    )
    .unwrap();
    let right = DriverQueueStore::new(
        Arc::clone(&backend),
        Scope::local(),
        clock,
        Duration::from_secs(30),
    )
    .unwrap();
    let now = chrono::Utc::now();
    left.import_legacy(&QueueState {
        pending: Vec::new(),
        history: (0..crate::queue::MAX_HISTORY)
            .map(|n| JobRun {
                id: format!("old-{n}"),
                job: "build_beliefs".into(),
                root: Namespace::ROOT.to_string(),
                ran_at: now,
                outcome: "done".into(),
                reason: None,
                built: None,
                stored: 0,
            })
            .collect(),
    })
    .unwrap();
    let jobs: Vec<_> = ["history-left", "history-right"]
        .into_iter()
        .map(|id| QueuedJob {
            id: id.into(),
            root: Namespace::ROOT.to_string(),
            job: build(id),
            queued_at: now,
            attempts: 0,
            last_error: None,
        })
        .collect();
    for job in jobs.clone() {
        left.enqueue_item(job).unwrap();
    }
    let (left_job, left_claim) = left.claim_next(&Selection::All, now, &[]).unwrap().unwrap();
    let (right_job, right_claim) = right
        .claim_next(&Selection::All, now, &[])
        .unwrap()
        .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let left_barrier = Arc::clone(&barrier);
    let left_run = JobRun {
        id: left_job.id.clone(),
        job: "build_beliefs".into(),
        root: left_job.root.clone(),
        ran_at: now,
        outcome: "done".into(),
        reason: None,
        built: None,
        stored: 0,
    };
    let right_run = JobRun {
        id: right_job.id.clone(),
        job: "build_beliefs".into(),
        root: right_job.root.clone(),
        ran_at: now,
        outcome: "done".into(),
        reason: None,
        built: None,
        stored: 0,
    };
    let left_thread = std::thread::spawn(move || {
        let mut synchronized = false;
        left.settle_claim_after_history_snapshot(
            &left_claim,
            None,
            left_run,
            Vec::new(),
            Some(Box::new(move || {
                if !synchronized {
                    left_barrier.wait();
                    synchronized = true;
                }
            })),
        )
    });
    let right_thread = std::thread::spawn(move || {
        let mut synchronized = false;
        right.settle_claim_after_history_snapshot(
            &right_claim,
            None,
            right_run,
            Vec::new(),
            Some(Box::new(move || {
                if !synchronized {
                    barrier.wait();
                    synchronized = true;
                }
            })),
        )
    });
    assert!(left_thread.join().unwrap().unwrap());
    assert!(right_thread.join().unwrap().unwrap());

    let state = DriverQueueStore::new(
        backend,
        Scope::local(),
        Arc::new(|| 30_000),
        Duration::from_secs(30),
    )
    .unwrap()
    .load();
    assert!(state.pending.is_empty());
    assert_eq!(state.history.len(), crate::queue::MAX_HISTORY);
    assert!(state.history.iter().any(|run| run.id == "history-left"));
    assert!(state.history.iter().any(|run| run.id == "history-right"));
}

#[tokio::test]
async fn settlement_stops_after_bounded_history_conflicts() {
    const EXPECTED_ATTEMPTS: usize = 5;
    let (clock, _) = clock_at(35_000);
    let backend = backend(clock.clone()).await.unwrap();
    let store = DriverQueueStore::new(
        Arc::clone(&backend),
        Scope::local(),
        clock.clone(),
        Duration::from_secs(30),
    )
    .unwrap();
    let now = chrono::Utc::now();
    let source = QueuedJob {
        id: "bounded-history-source".into(),
        root: Namespace::ROOT.to_string(),
        job: build("bounded-history"),
        queued_at: now,
        attempts: 0,
        last_error: None,
    };
    let history = (0..crate::queue::MAX_HISTORY)
        .map(|n| JobRun {
            id: format!("bounded-old-{n}"),
            job: "build_beliefs".into(),
            root: Namespace::ROOT.to_string(),
            ran_at: now,
            outcome: "done".into(),
            reason: None,
            built: None,
            stored: 0,
        })
        .collect();
    store
        .import_legacy(&QueueState {
            pending: Vec::new(),
            history,
        })
        .unwrap();
    store.enqueue_item(source.clone()).unwrap();
    let (_, claim) = store
        .claim_next(&Selection::All, now, &[])
        .unwrap()
        .unwrap();
    let run = JobRun {
        id: source.id.clone(),
        job: "build_beliefs".into(),
        root: source.root.clone(),
        ran_at: now,
        outcome: "done".into(),
        reason: None,
        built: None,
        stored: 0,
    };
    enum SettlementEvent {
        HistorySnapshot,
        Finished(Result<bool, String>),
    }
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let hook_tx = event_tx.clone();
        let hook_release = release_rx;
        let result = store.settle_claim_after_history_snapshot(
            &claim,
            None,
            run,
            Vec::new(),
            Some(Box::new(move || {
                hook_tx.send(SettlementEvent::HistorySnapshot).unwrap();
                hook_release.recv().unwrap();
            })),
        );
        event_tx.send(SettlementEvent::Finished(result)).unwrap();
    });

    let documents = backend
        .for_scope(&Scope::local())
        .unwrap()
        .documents()
        .clone();
    let mut snapshots = 0;
    let result = loop {
        match event_rx.recv().unwrap() {
            SettlementEvent::HistorySnapshot => {
                snapshots += 1;
                if snapshots <= EXPECTED_ATTEMPTS {
                    let page = documents
                        .query_as::<serde_json::Value>(
                            JOB_RUNS,
                            &Query::all().sort(Sort::asc("sequence")).limit(1),
                        )
                        .await
                        .unwrap();
                    let oldest = page.items.first().expect("full history has an oldest row");
                    documents
                        .put_as(
                            JOB_RUNS,
                            &oldest.id,
                            &oldest.doc,
                            Precondition::Version(oldest.version),
                        )
                        .await
                        .unwrap();
                }
                release_tx.send(()).unwrap();
            }
            SettlementEvent::Finished(result) => break result,
        }
    };
    worker.join().unwrap();
    assert!(
        matches!(result, Err(ref error) if error.contains("remained conflicting after 5 attempts")),
        "settlement must stop with a bounded conflict error, got {result:?}"
    );
    assert_eq!(snapshots, EXPECTED_ATTEMPTS);
    let observer =
        DriverQueueStore::new(backend, Scope::local(), clock, Duration::from_secs(30)).unwrap();
    assert!(
        observer
            .load()
            .pending
            .iter()
            .any(|job| job.id == source.id)
    );
}

#[tokio::test]
async fn settlement_deduplicates_identical_follow_ups_and_rejects_divergent_ids() {
    let (clock, _) = clock_at(40_000);
    let backend = backend(clock.clone()).await.unwrap();
    let store = DriverQueueStore::new(
        Arc::clone(&backend),
        Scope::local(),
        clock.clone(),
        Duration::from_secs(30),
    )
    .unwrap();
    let now = chrono::Utc::now();
    let source = QueuedJob {
        id: "source-job".into(),
        root: Namespace::ROOT.to_string(),
        job: build("source"),
        queued_at: now,
        attempts: 0,
        last_error: None,
    };
    let follow_up = QueuedJob {
        id: "follow-up".into(),
        root: Namespace::ROOT.to_string(),
        job: build("follow-up"),
        queued_at: now,
        attempts: 0,
        last_error: None,
    };
    store.enqueue_item(source.clone()).unwrap();
    let (_, claim) = store
        .claim_next(&Selection::All, now, &[])
        .unwrap()
        .unwrap();
    let run = JobRun {
        id: source.id.clone(),
        job: "build_beliefs".into(),
        root: source.root.clone(),
        ran_at: now,
        outcome: "done".into(),
        reason: None,
        built: None,
        stored: 0,
    };
    assert!(
        store
            .settle(
                &claim,
                None,
                run,
                vec![follow_up.clone(), follow_up.clone()]
            )
            .unwrap()
    );
    let state = store.load();
    assert_eq!(state.pending, vec![follow_up.clone()]);

    let second_source = QueuedJob {
        id: "second-source-job".into(),
        ..source.clone()
    };
    store.enqueue_item(second_source.clone()).unwrap();
    let (_, claim) = store
        .claim_next(&Selection::One(second_source.id.clone()), now, &[])
        .unwrap()
        .unwrap();
    let mut divergent = follow_up.clone();
    divergent.job = build("different-payload");
    let run = JobRun {
        id: second_source.id.clone(),
        job: "build_beliefs".into(),
        root: second_source.root.clone(),
        ran_at: now,
        outcome: "done".into(),
        reason: None,
        built: None,
        stored: 0,
    };
    let error = store
        .settle(&claim, None, run, vec![follow_up, divergent])
        .unwrap_err();
    assert!(error.contains("conflicting follow-up jobs share id"));
    assert!(
        store
            .load()
            .pending
            .iter()
            .any(|job| job.id == second_source.id)
    );
}
