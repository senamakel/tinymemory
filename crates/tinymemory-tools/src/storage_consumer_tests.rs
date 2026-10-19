use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_driver_pending_write_consumers_execute_a_healthy_item_once() {
    let (clock, _) = clock_at(30_000);
    let backend = backend(clock.clone()).await.unwrap();
    let left = Arc::new(PendingWrites::new(
        pending_store(Arc::clone(&backend), clock.clone()).unwrap(),
        Limits::default(),
    ));
    let right = PendingWrites::new(pending_store(backend, clock).unwrap(), Limits::default());
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
    let left_gate = gate;
    let left_executor = Arc::clone(&executor);
    let left_drain = tokio::spawn(async move {
        left_queue
            .drain(&*left_executor, &left_gate, generation, |_| true, |_| {})
            .await
    });
    executor.started.notified().await;
    let right_count = right
        .drain(
            &*executor,
            &WriteGate::new("shared"),
            generation,
            |_| true,
            |_| {},
        )
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
    let backend = backend(clock.clone()).await.unwrap();
    let left = Arc::new(DeletionQueue::new(
        deletion_store(Arc::clone(&backend), clock.clone()).unwrap(),
        Backoff::default(),
    ));
    let right = DeletionQueue::new(deletion_store(backend, clock).unwrap(), Backoff::default());
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
