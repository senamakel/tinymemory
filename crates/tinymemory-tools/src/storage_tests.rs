use super::*;

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tinymemory_api::conformance::ReferenceEngine;
use tinymemory_api::{LearningKind, MemoryEngine, MemoryMeta, Namespace, StoreItem};
use tinystoragedrivers_core::schema::Registry;
use tinystoragedrivers_core::{MemoryStorage, Scope, StorageBackend};

use crate::list_store::ListStore;
use crate::pending::{Binding, Entry, Limits, PendingWrites, WriteExecutor, WriteGate, WriteOperation};
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
    registry.prepare(backend.as_ref()).await.expect("schema prepared");
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
    assert!(second
        .claim_oldest(&[], i64::MAX, &|item| item.id.clone())
        .unwrap()
        .is_none());

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

#[allow(dead_code)]
fn _entry_shape_proof(entry: Entry) -> Entry {
    entry
}
