use super::*;

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tinymemory_api::conformance::ReferenceEngine;
use tinymemory_api::{LearningKind, MemoryMeta, Namespace, StoreItem};
use tinystoragedrivers_core::schema::Registry;
use tinystoragedrivers_core::{Blocking, MemoryStorage, Precondition, Scope, StorageBackend};

use crate::deletion::{Backoff, DeletionExecutor, DeletionQueue, Drain, PendingDeletion};
use crate::list_store::ListStore;
use crate::pending::{Binding, Limits, PendingWrites, WriteExecutor, WriteGate, WriteOperation};
use crate::queue::{JobQueue, JobRun, QueueState, QueueStore, QueuedJob, Selection};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Item {
    id: String,
    value: String,
}

fn clock_at(value: u64) -> (Clock, Arc<AtomicU64>) {
    let now = Arc::new(AtomicU64::new(value));
    let read = Arc::clone(&now);
    (Arc::new(move || read.load(Ordering::SeqCst)), now)
}

async fn backend(clock: Clock) -> Arc<dyn StorageBackend> {
    let backend: Arc<dyn StorageBackend> = Arc::new(MemoryStorage::with_clock(clock));
    let registry = Registry::compose([storage_schema().expect("valid TinyMemory schema")])
        .expect("schema composes");
    registry
        .prepare(backend.as_ref())
        .await
        .expect("schema prepared");
    backend
}

fn list_store(
    backend: Arc<dyn StorageBackend>,
    scope: Scope,
    clock: Clock,
) -> DriverListStore<Item> {
    DriverListStore::new(
        backend,
        scope,
        TOOL_WRITES,
        clock,
        Duration::from_millis(10),
        |item| item.id.clone(),
    )
    .expect("store opened")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ReadinessItem {
    id: String,
    ready: bool,
}

fn readiness_store(
    backend: Arc<dyn StorageBackend>,
    clock: Clock,
) -> DriverListStore<ReadinessItem> {
    DriverListStore::new(
        backend,
        Scope::local(),
        TOOL_WRITES,
        clock,
        Duration::from_millis(10),
        |item| item.id.clone(),
    )
    .expect("store opened")
}

fn deletion_store(
    backend: Arc<dyn StorageBackend>,
    clock: Clock,
) -> DriverListStore<PendingDeletion> {
    DriverListStore::new(
        backend,
        Scope::local(),
        DELETIONS,
        clock,
        Duration::from_secs(30),
        |item| match item {
            PendingDeletion::Thread { thread_id } => format!("thread:{thread_id}"),
            PendingDeletion::Source { source_id } => format!("source:{source_id}"),
        },
    )
    .expect("store opened")
}

fn pending_store(
    backend: Arc<dyn StorageBackend>,
    clock: Clock,
) -> DriverListStore<crate::pending::Entry> {
    DriverListStore::new(
        backend,
        Scope::local(),
        TOOL_WRITES,
        clock,
        Duration::from_secs(30),
        |entry| entry.ticket.clone(),
    )
    .expect("store opened")
}

#[tokio::test]
async fn claim_rereads_eligibility_after_candidate_scan() {
    let (clock, _) = clock_at(1_000);
    let backend = backend(clock.clone()).await;
    let first = readiness_store(Arc::clone(&backend), clock.clone());
    let second = readiness_store(Arc::clone(&backend), clock);
    first
        .append(
            "same-row",
            ReadinessItem {
                id: "same-row".into(),
                ready: true,
            },
        )
        .unwrap();

    let update_backend = Arc::clone(&backend);
    let update_blocking = Blocking::new().unwrap();
    let hook = Arc::new(move |id: &str, point: super::list::ClaimPoint| {
        if id != "same-row" || !matches!(point, super::list::ClaimPoint::CandidateRead) {
            return;
        }
        let backend = Arc::clone(&update_backend);
        update_blocking
            .run(async move {
                let docs = backend.for_scope(&Scope::local())?.documents().clone();
                let current = docs
                    .get_as::<super::Stored<ReadinessItem>>(TOOL_WRITES, "same-row")
                    .await?
                    .expect("candidate still exists");
                let version = current.version;
                let mut changed = current.doc;
                changed.item.ready = false;
                docs.put_as(
                    TOOL_WRITES,
                    "same-row",
                    &changed,
                    Precondition::Version(version),
                )
                .await?;
                Ok::<_, tinystoragedrivers_core::StorageError>(())
            })
            .unwrap()
            .unwrap();
    });

    let claimed = first
        .claim_item_with_hook(Vec::new(), i64::MAX, |item| item.ready, hook)
        .unwrap();
    assert!(
        claimed.is_none(),
        "settled row is not claimed from stale snapshot"
    );
    assert!(!second.load().unwrap()[0].ready);
}

#[tokio::test]
async fn claim_write_is_fenced_if_lease_epoch_changes_after_acquisition() {
    let (clock, now) = clock_at(2_000);
    let backend = backend(clock.clone()).await;
    let first = list_store(Arc::clone(&backend), Scope::local(), clock.clone());
    let second = list_store(Arc::clone(&backend), Scope::local(), clock);
    first
        .append(
            "one",
            Item {
                id: "one".into(),
                value: "pending".into(),
            },
        )
        .unwrap();

    let takeover = Arc::new(move |id: &str, point: super::list::ClaimPoint| {
        if id == "one" && matches!(point, super::list::ClaimPoint::LeaseAcquired) {
            now.store(2_011, Ordering::SeqCst);
            assert!(
                second
                    .claim_oldest(&[], i64::MAX, &|item| item.id.clone())
                    .unwrap()
                    .is_some()
            );
        }
    });
    let stale_claim = first
        .claim_item_with_hook(Vec::new(), i64::MAX, |_| true, takeover)
        .unwrap();
    assert!(
        stale_claim.is_none(),
        "expired lease owner cannot claim the row"
    );
}

#[tokio::test]
async fn driver_claims_are_exclusive_fenced_and_recover_after_expiry() {
    let (clock, now) = clock_at(1_000);
    let backend = backend(clock.clone()).await;
    let first = list_store(Arc::clone(&backend), Scope::local(), clock.clone());
    let second = list_store(Arc::clone(&backend), Scope::local(), clock);
    first
        .append(
            "one",
            Item {
                id: "one".into(),
                value: "first".into(),
            },
        )
        .unwrap();

    let old = first
        .claim_oldest(&[], i64::MAX, &|item| item.id.clone())
        .unwrap()
        .expect("first consumer claims item");
    assert!(
        second
            .claim_oldest(&[], i64::MAX, &|item| item.id.clone())
            .unwrap()
            .is_none()
    );

    now.store(1_011, Ordering::SeqCst);
    let reclaimed = second
        .claim_oldest(&[], i64::MAX, &|item| item.id.clone())
        .unwrap()
        .expect("expired lease is reclaimable");
    assert!(reclaimed.epoch > old.epoch);
    assert!(!first.acknowledge(&old).unwrap(), "stale owner is fenced");
    assert!(second.acknowledge(&reclaimed).unwrap());
    assert!(first.load().unwrap().is_empty());
}

#[tokio::test]
async fn driver_retry_nacks_until_its_visibility_deadline() {
    let (clock, now) = clock_at(5_000);
    let backend = backend(clock.clone()).await;
    let store = list_store(backend, Scope::local(), clock);
    store
        .append(
            "retry",
            Item {
                id: "retry".into(),
                value: "before retry".into(),
            },
        )
        .unwrap();
    let claim = store
        .claim_oldest(&[], 5_000, &|item| item.id.clone())
        .unwrap()
        .unwrap();
    assert!(
        store
            .retry(
                &claim,
                Item {
                    id: "retry".into(),
                    value: "after retry".into(),
                },
                Some(5_010),
            )
            .unwrap()
    );
    assert!(
        store
            .claim_oldest(&[], 5_009, &|item| item.id.clone())
            .unwrap()
            .is_none()
    );

    now.store(5_011, Ordering::SeqCst);
    let retried = store
        .claim_oldest(&[], 5_011, &|item| item.id.clone())
        .unwrap()
        .expect("nacked item returns when due");
    assert_eq!(retried.item.value, "after retry");
    assert!(store.acknowledge(&retried).unwrap());
    assert!(store.load().unwrap().is_empty());
}

#[tokio::test]
async fn driver_list_preserves_fifo_scope_and_idempotent_target_wins_import() {
    let (clock, _) = clock_at(10_000);
    let backend = backend(clock.clone()).await;
    let scope = Scope::local();
    let store = list_store(Arc::clone(&backend), scope.clone(), clock.clone());
    let other = list_store(
        Arc::clone(&backend),
        Scope::new("tenant:other").unwrap(),
        clock,
    );
    store
        .append(
            "target",
            Item {
                id: "target".into(),
                value: "new target value".into(),
            },
        )
        .unwrap();

    let first_import = store
        .import_items([
            (
                "target".into(),
                Item {
                    id: "target".into(),
                    value: "legacy value".into(),
                },
            ),
            (
                "occurrence-1".into(),
                Item {
                    id: "occurrence-1".into(),
                    value: "same".into(),
                },
            ),
            (
                "occurrence-2".into(),
                Item {
                    id: "occurrence-2".into(),
                    value: "same".into(),
                },
            ),
        ])
        .unwrap();
    assert_eq!(first_import.imported, 2);
    assert_eq!(first_import.conflicts, 1);
    let retry = store
        .import_items([
            (
                "target".into(),
                Item {
                    id: "target".into(),
                    value: "legacy value".into(),
                },
            ),
            (
                "occurrence-1".into(),
                Item {
                    id: "occurrence-1".into(),
                    value: "same".into(),
                },
            ),
            (
                "occurrence-2".into(),
                Item {
                    id: "occurrence-2".into(),
                    value: "same".into(),
                },
            ),
        ])
        .unwrap();
    assert_eq!(retry.identical, 2);
    assert_eq!(retry.conflicts, 1);
    assert_eq!(store.load().unwrap()[0].id, "occurrence-1");
    assert_eq!(store.load().unwrap()[1].id, "occurrence-2");
    assert_eq!(store.load().unwrap()[2].value, "new target value");
    assert!(other.load().unwrap().is_empty());

    let before = store.load().unwrap();
    assert!(
        store
            .save(&[
                Item {
                    id: "occurrence-2".into(),
                    value: "updated second".into(),
                },
                Item {
                    id: "occurrence-1".into(),
                    value: "first".into(),
                },
            ])
            .is_err()
    );
    assert_eq!(store.load().unwrap(), before);
}

#[tokio::test]
async fn concurrent_import_handles_reread_absent_insert_conflicts() {
    let (clock, _) = clock_at(15_000);
    let backend = backend(clock.clone()).await;
    let first = list_store(Arc::clone(&backend), Scope::local(), clock.clone());
    let second = list_store(backend, Scope::local(), clock);
    let entries = || {
        vec![(
            "stable-legacy-occurrence".to_owned(),
            Item {
                id: "stable-legacy-occurrence".into(),
                value: "legacy".into(),
            },
        )]
    };
    let after_absent = Arc::new(std::sync::Barrier::new(2));
    let left_barrier = Arc::clone(&after_absent);
    let left = std::thread::spawn(move || {
        first
            .import_items_with_hook(entries(), Arc::new(move |_| left_barrier.wait()))
            .unwrap()
    });
    let right_barrier = Arc::clone(&after_absent);
    let right = std::thread::spawn(move || {
        second
            .import_items_with_hook(entries(), Arc::new(move |_| right_barrier.wait()))
            .unwrap()
    });
    let left = left.join().unwrap();
    let right = right.join().unwrap();
    assert_eq!(left.imported + right.imported, 1);
    assert_eq!(left.identical + right.identical, 1);
    assert_eq!(left.conflicts + right.conflicts, 0);
}

fn build(agent: &str) -> crate::background::BackgroundJob {
    use tinymemory_api::{ConsolidateRequest, ItemKind, Reach};
    crate::background::BackgroundJob::BuildBeliefs {
        request: ConsolidateRequest::new(Reach::exact(Namespace::agent(agent)))
            .kinds([ItemKind::Conversation]),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_driver_job_queue_consumers_run_a_healthy_job_once() {
    let (clock, _) = clock_at(20_000);
    let backend = backend(clock.clone()).await;
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
    let backend = backend(clock.clone()).await;
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

fn learn(text: &str) -> WriteOperation {
    WriteOperation::Learn {
        item: StoreItem::learning(text, LearningKind::Fact, 0.5, MemoryMeta::default()),
    }
}

struct PausingWriteCounter {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl WriteExecutor for PausingWriteCounter {
    async fn run(&self, _: &WriteOperation) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_driver_pending_write_consumers_execute_a_healthy_item_once() {
    let (clock, _) = clock_at(30_000);
    let backend = backend(clock.clone()).await;
    let left = Arc::new(PendingWrites::new(
        pending_store(Arc::clone(&backend), clock.clone()),
        Limits::default(),
    ));
    let right = PendingWrites::new(pending_store(backend, clock), Limits::default());
    let gate = WriteGate::new("shared");
    left.enqueue(
        learn("once"),
        Namespace::ROOT,
        Binding("owner".into()),
        &gate,
    )
    .unwrap();
    let executor = Arc::new(PausingWriteCounter {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        calls: AtomicUsize::new(0),
    });
    let generation = gate.generation();
    let left_queue = Arc::clone(&left);
    let left_gate = gate.clone();
    let left_executor = Arc::clone(&executor);
    let left_drain = tokio::spawn(async move {
        left_queue
            .drain(&*left_executor, &left_gate, generation, |_| true, |_| {})
            .await
    });
    executor.started.notified().await;
    let right_count = right
        .drain(&*executor, &gate, generation, |_| true, |_| {})
        .await;
    executor.release.notify_one();
    let left_count = left_drain.await.unwrap();
    assert_eq!(left_count + right_count, 1);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    assert!(left.is_empty());
}

struct PausingDeletionCounter {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl DeletionExecutor for PausingDeletionCounter {
    fn is_reachable(&self) -> bool {
        true
    }

    async fn run(&self, _: &PendingDeletion) -> Result<usize, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        self.release.notified().await;
        Ok(1)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_driver_deletion_consumers_execute_a_healthy_item_once() {
    let (clock, _) = clock_at(40_000);
    let backend = backend(clock.clone()).await;
    let left = Arc::new(DeletionQueue::new(
        deletion_store(Arc::clone(&backend), clock.clone()),
        Backoff::default(),
    ));
    let right = DeletionQueue::new(deletion_store(backend, clock), Backoff::default());
    left.enqueue(PendingDeletion::Thread {
        thread_id: "thread-one".into(),
    });
    let executor = Arc::new(PausingDeletionCounter {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        calls: AtomicUsize::new(0),
    });
    let left_queue = Arc::clone(&left);
    let left_executor = Arc::clone(&executor);
    let left_drain = tokio::spawn(async move {
        left_queue
            .drain(&*left_executor, Drain::Force, chrono::Utc::now())
            .await
    });
    executor.started.notified().await;
    let right_count = right
        .drain(&*executor, Drain::Force, chrono::Utc::now())
        .await;
    executor.release.notify_one();
    let left_count = left_drain.await.unwrap();
    assert_eq!(left_count + right_count, 1);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    assert!(left.pending().is_empty());
}
