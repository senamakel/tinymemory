use super::*;

#[tokio::test]
async fn claim_rereads_eligibility_after_candidate_scan() {
    let (clock, _) = clock_at(1_000);
    let backend = backend(clock.clone()).await.unwrap();
    let first = readiness_store(Arc::clone(&backend), clock.clone()).unwrap();
    let second = readiness_store(Arc::clone(&backend), clock).unwrap();
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
    let hook = Arc::new(
        move |id: &str,
              point: super::list::ClaimPoint|
              -> Pin<Box<dyn Future<Output = Result<(), StorageError>> + Send>> {
            if id != "same-row" || !matches!(point, super::list::ClaimPoint::CandidateRead) {
                return Box::pin(async { Ok(()) });
            }
            let backend = Arc::clone(&update_backend);
            Box::pin(async move {
                let docs = backend.for_scope(&Scope::local())?.documents().clone();
                let current = docs
                    .get_as::<super::list::Stored<ReadinessItem>>(TOOL_WRITES, "same-row")
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
                Ok(())
            })
        },
    );

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
    let backend = backend(clock.clone()).await.unwrap();
    let first = list_store(Arc::clone(&backend), Scope::local(), clock.clone()).unwrap();
    first
        .append(
            "one",
            Item {
                id: "one".into(),
                value: "pending".into(),
            },
        )
        .unwrap();

    let takeover_backend = Arc::clone(&backend);
    let takeover_clock = clock.clone();
    let takeover = Arc::new(
        move |id: &str,
              point: super::list::ClaimPoint|
              -> Pin<Box<dyn Future<Output = Result<(), StorageError>> + Send>> {
            if id == "one" && matches!(point, super::list::ClaimPoint::LeaseAcquired) {
                now.store(2_011, Ordering::SeqCst);
                let backend = Arc::clone(&takeover_backend);
                let clock = takeover_clock.clone();
                return Box::pin(async move {
                    let documents = backend.for_scope(&Scope::local())?.documents().clone();
                    let acquired = Leases::new(&documents)
                        .with_clock(clock)
                        .acquire(
                            "tinymemory.queue",
                            "tinymemory.tool_writes:one",
                            "takeover",
                            10,
                        )
                        .await?;
                    assert!(acquired.is_some(), "expired claim lease can be taken over");
                    Ok(())
                });
            }
            Box::pin(async { Ok(()) })
        },
    );
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
    let backend = backend(clock.clone()).await.unwrap();
    let first = list_store(Arc::clone(&backend), Scope::local(), clock.clone()).unwrap();
    let second = list_store(Arc::clone(&backend), Scope::local(), clock).unwrap();
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
async fn one_store_handle_cannot_claim_a_live_item_twice() {
    let (clock, _) = clock_at(4_000);
    let backend = backend(clock.clone()).await.unwrap();
    let store = list_store(backend, Scope::local(), clock).unwrap();
    store
        .append(
            "single-owner",
            Item {
                id: "single-owner".into(),
                value: "pending".into(),
            },
        )
        .unwrap();
    let claim = store
        .claim_oldest(&[], i64::MAX, &|item| item.id.clone())
        .unwrap()
        .expect("first claim owns the item");

    assert!(
        store
            .claim_oldest(&[], i64::MAX, &|item| item.id.clone())
            .unwrap()
            .is_none()
    );
    assert!(store.acknowledge(&claim).unwrap());
}

#[tokio::test]
async fn driver_retry_nacks_until_its_visibility_deadline() {
    let (clock, now) = clock_at(5_000);
    let backend = backend(clock.clone()).await.unwrap();
    let store = list_store(backend, Scope::local(), clock).unwrap();
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
