use super::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};
use tinymemory_api::conformance::ReferenceEngine;
use tinymemory_api::{LearningKind, MemoryMeta, Namespace, StoreItem};
use tinystoragedrivers_core::schema::Registry;
use tinystoragedrivers_core::{
    Clock, DocumentStoreExt, Leases, MemoryStorage, Precondition, Query, Scope, Sort,
    StorageBackend, StorageError,
};

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

async fn backend(clock: Clock) -> Result<Arc<dyn StorageBackend>, StorageError> {
    let backend: Arc<dyn StorageBackend> = Arc::new(MemoryStorage::with_clock(clock));
    let registry = Registry::compose([storage_schema()?])?;
    registry.prepare(backend.as_ref()).await?;
    Ok(backend)
}

fn list_store(
    backend: Arc<dyn StorageBackend>,
    scope: Scope,
    clock: Clock,
) -> Result<DriverListStore<Item>, crate::list_store::StoreError> {
    DriverListStore::<Item>::new(
        backend,
        scope,
        TOOL_WRITES,
        clock,
        Duration::from_millis(10),
        |item| item.id.clone(),
    )
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ReadinessItem {
    id: String,
    ready: bool,
}

fn readiness_store(
    backend: Arc<dyn StorageBackend>,
    clock: Clock,
) -> Result<DriverListStore<ReadinessItem>, crate::list_store::StoreError> {
    DriverListStore::<ReadinessItem>::new(
        backend,
        Scope::local(),
        TOOL_WRITES,
        clock,
        Duration::from_millis(10),
        |item| item.id.clone(),
    )
}

fn deletion_store(
    backend: Arc<dyn StorageBackend>,
    clock: Clock,
) -> Result<DriverListStore<PendingDeletion>, crate::list_store::StoreError> {
    DriverListStore::<PendingDeletion>::new(
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
}

fn pending_store(
    backend: Arc<dyn StorageBackend>,
    clock: Clock,
) -> Result<DriverListStore<crate::pending::Entry>, crate::list_store::StoreError> {
    DriverListStore::<crate::pending::Entry>::new(
        backend,
        Scope::local(),
        TOOL_WRITES,
        clock,
        Duration::from_secs(30),
        |entry| entry.ticket.clone(),
    )
}

fn build(agent: &str) -> crate::background::BackgroundJob {
    use tinymemory_api::{ConsolidateRequest, ItemKind, Reach};
    crate::background::BackgroundJob::BuildBeliefs {
        request: ConsolidateRequest::new(Reach::exact(Namespace::agent(agent)))
            .kinds([ItemKind::Conversation]),
    }
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

#[path = "storage_claim_tests.rs"]
mod claim_tests;
#[path = "storage_consumer_tests.rs"]
mod consumer_tests;
#[path = "storage_import_tests.rs"]
mod import_tests;
#[path = "storage_queue_tests.rs"]
mod queue_tests;
#[path = "storage_settlement_tests.rs"]
mod settlement_tests;
