use super::*;

#[tokio::test]
async fn driver_list_preserves_fifo_scope_and_idempotent_target_wins_import() {
    let (clock, _) = clock_at(10_000);
    let backend = backend(clock.clone()).await.unwrap();
    let scope = Scope::local();
    let store = list_store(Arc::clone(&backend), scope.clone(), clock.clone()).unwrap();
    let other = list_store(
        Arc::clone(&backend),
        Scope::new("tenant:other").unwrap(),
        clock,
    )
    .unwrap();
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
    let backend = backend(clock.clone()).await.unwrap();
    let first = list_store(Arc::clone(&backend), Scope::local(), clock.clone()).unwrap();
    let second = list_store(backend, Scope::local(), clock).unwrap();
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
            .import_items_with_hook(
                entries(),
                Arc::new(move |_| {
                    left_barrier.wait();
                }),
            )
            .unwrap()
    });
    let right_barrier = Arc::clone(&after_absent);
    let right = std::thread::spawn(move || {
        second
            .import_items_with_hook(
                entries(),
                Arc::new(move |_| {
                    right_barrier.wait();
                }),
            )
            .unwrap()
    });
    let left = left.join().unwrap();
    let right = right.join().unwrap();
    assert_eq!(left.imported + right.imported, 1);
    assert_eq!(left.identical + right.identical, 1);
    assert_eq!(left.conflicts + right.conflicts, 0);
}
