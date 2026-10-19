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
use crate::queue::{JobQueue, Selection};

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

#[tokio::test]
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

fn learn(text: &str) -> WriteOperation {
    WriteOperation::Learn {
        item: StoreItem::learning(text, LearningKind::Fact, 0.5, MemoryMeta::default()),
    }
}

struct WriteCounter(AtomicUsize);

#[async_trait::async_trait]
impl WriteExecutor for WriteCounter {
    async fn run(&self, _: &WriteOperation) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn two_driver_pending_write_consumers_execute_a_healthy_item_once() {
    let (clock, _) = clock_at(30_000);
    let backend = backend(clock.clone()).await;
    let left = PendingWrites::new(
        list_store(Arc::clone(&backend), Scope::local(), clock.clone()),
        Limits::default(),
    );
    let right = PendingWrites::new(
        list_store(Arc::clone(&backend), Scope::local(), clock),
        Limits::default(),
    );
    let gate = WriteGate::new("shared");
    left.enqueue(
        learn("once"),
        Namespace::ROOT,
        Binding("owner".into()),
        &gate,
    )
    .unwrap();
    let executor = WriteCounter(AtomicUsize::new(0));

    let (left_count, right_count) = tokio::join!(
        left.drain(&executor, &gate, gate.generation(), |_| true, |_| {}),
        right.drain(&executor, &gate, gate.generation(), |_| true, |_| {}),
    );
    assert_eq!(left_count + right_count, 1);
    assert_eq!(executor.0.load(Ordering::SeqCst), 1);
    assert!(left.is_empty());
}

struct DeletionCounter(AtomicUsize);

#[async_trait::async_trait]
impl DeletionExecutor for DeletionCounter {
    fn is_reachable(&self) -> bool {
        true
    }

    async fn run(&self, _: &PendingDeletion) -> Result<usize, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(1)
    }
}

#[tokio::test]
async fn two_driver_deletion_consumers_execute_a_healthy_item_once() {
    let (clock, _) = clock_at(40_000);
    let backend = backend(clock.clone()).await;
    let left = DeletionQueue::new(
        deletion_store(Arc::clone(&backend), clock.clone()),
        Backoff::default(),
    );
    let right = DeletionQueue::new(deletion_store(backend, clock), Backoff::default());
    left.enqueue(PendingDeletion::Thread {
        thread_id: "thread-one".into(),
    });
    let executor = DeletionCounter(AtomicUsize::new(0));

    let (left_count, right_count) = tokio::join!(
        left.drain(&executor, Drain::Force, chrono::Utc::now()),
        right.drain(&executor, Drain::Force, chrono::Utc::now()),
    );
    assert_eq!(left_count + right_count, 1);
    assert_eq!(executor.0.load(Ordering::SeqCst), 1);
    assert!(left.pending().is_empty());
}
