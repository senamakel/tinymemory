//! The reference engine's own behaviour, independent of the suite.

use crate::{ItemKind, LearningKind, MemoryMeta};

use super::*;

#[tokio::test]
async fn bulk_store_preserves_order_and_marks_existing_and_in_batch_replays() {
    let engine = ReferenceEngine::new();
    let a = StoreItem::document("alpha", MemoryMeta::default());
    let b = StoreItem::document("beta", MemoryMeta::default());
    engine.store(a.clone()).await.unwrap();
    let receipts = engine
        .store_many(vec![a.clone(), b.clone(), b.clone()])
        .await
        .unwrap();
    assert_eq!(
        receipts
            .iter()
            .map(|receipt| receipt.replayed)
            .collect::<Vec<_>>(),
        vec![true, false, true]
    );
    assert_eq!(engine.len(), 2);
    let page = engine
        .export(ListRequest::new(MetaFilter::default(), 10))
        .await
        .unwrap();
    assert_eq!(
        page.items
            .into_iter()
            .map(|entry| entry.item)
            .collect::<Vec<_>>(),
        vec![a, b]
    );
}

#[tokio::test]
async fn a_bad_cursor_is_an_invalid_request() {
    let engine = ReferenceEngine::new();
    let mut request = ListRequest::new(MetaFilter::default(), 1);
    request.cursor = Some("not-a-number".into());
    assert!(matches!(
        engine.list(request).await,
        Err(Error::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn pages_follow_the_cursor_to_the_end() {
    let engine = ReferenceEngine::new();
    for text in ["one", "two", "three"] {
        engine
            .store(StoreItem::document(text, MemoryMeta::default()))
            .await
            .unwrap();
    }
    assert_eq!(engine.len(), 3);
    let first = engine
        .list(ListRequest::new(MetaFilter::default(), 2))
        .await
        .unwrap();
    assert_eq!(first.items.len(), 2);
    let mut next = ListRequest::new(MetaFilter::default(), 2);
    next.cursor = first.next_cursor;
    let second = engine.list(next).await.unwrap();
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.next_cursor, None);
}

#[tokio::test]
async fn export_hands_each_item_back_whole_once_by_cursor() {
    let engine = ReferenceEngine::new();
    let mut stored = Vec::new();
    for i in 0..5 {
        let item = StoreItem::learning(
            format!("fact {i}"),
            LearningKind::Preference,
            0.5,
            MemoryMeta::default(),
        );
        engine.store(item.clone()).await.unwrap();
        stored.push(item);
    }
    let (mut exported, mut cursor) = (Vec::new(), None);
    for _ in 0..10 {
        let mut request = ListRequest::new(MetaFilter::default(), 2);
        request.cursor = cursor;
        let page = engine.export(request).await.unwrap();
        assert!(page.incomplete.is_empty());
        exported.extend(page.items);
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert!(cursor.is_none(), "the cursor ends");
    assert_eq!(exported.len(), stored.len(), "each item once");
    for (exported, stored) in exported.iter().zip(&stored) {
        assert_eq!(&exported.item, stored, "whole, in storage order");
        assert_eq!(exported.id.as_str(), stored.fingerprint());
    }
}

#[tokio::test]
async fn recall_on_an_empty_engine_has_no_citations() {
    let engine = ReferenceEngine::default();
    assert!(engine.is_empty());
    let answer = engine
        .recall(RecallRequest::new("anything", 3))
        .await
        .unwrap();
    assert!(answer.citations.is_empty());
    assert_eq!(answer.model.as_deref(), Some(REFERENCE_ENGINE_ID));
    assert_eq!(engine.health().await, EngineHealth::Ok);
}

#[tokio::test]
async fn fetch_ranks_better_keyword_matches_first() {
    let engine = ReferenceEngine::new();
    engine
        .store(StoreItem::learning(
            "tea preference",
            LearningKind::Preference,
            0.9,
            MemoryMeta::default(),
        ))
        .await
        .unwrap();
    engine
        .store(StoreItem::document("tea", MemoryMeta::default()))
        .await
        .unwrap();
    let page = engine
        .fetch(FetchRequest::new("tea preference", FetchMode::Keyword, 5))
        .await
        .unwrap();
    assert_eq!(page.hits.len(), 2);
    assert_eq!(page.hits[0].kind, ItemKind::Learning);
    assert!(page.hits[0].score > page.hits[1].score);
}
