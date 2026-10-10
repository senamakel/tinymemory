//! The write tools against the reference engine: shapes, metadata, and
//! forget's id resolution and filter rules.

use super::*;
use serde_json::json;
use tinymemory_api::conformance::ReferenceEngine;
use tinymemory_api::{Error, ListRequest, MetaFilter, Namespace};

fn scope() -> ToolScope {
    ToolScope::at(Namespace::agent("writer"))
}

fn invalid(result: Result<Value>) -> String {
    match result {
        Err(Error::InvalidRequest(message)) => message,
        other => panic!("expected an invalid request, got {other:?}"),
    }
}

async fn everything(engine: &ReferenceEngine) -> Vec<tinymemory_api::Hit> {
    engine
        .list(ListRequest::new(MetaFilter::default(), 100))
        .await
        .unwrap()
        .items
}

#[tokio::test]
async fn store_needs_exactly_one_shape() {
    let engine = ReferenceEngine::new();
    for value in [
        json!({}),
        json!({ "tags": ["x"] }),
        json!({ "learning": { "text": "a" }, "document": { "text": "b" } }),
    ] {
        let text = invalid(store(&engine, &scope(), &value).await);
        assert!(text.contains("exactly one of"), "{text}");
    }
    assert!(engine.is_empty());
}

#[tokio::test]
async fn store_builds_the_metadata_itself() {
    let engine = ReferenceEngine::new();
    let result = store(
        &engine,
        &scope(),
        &json!({ "learning": { "text": "likes tea", "learning_kind": "preference",
                               "confidence": 0.5, "evidence": "said so" },
                 "tags": ["drink"] }),
    )
    .await
    .unwrap();
    assert_eq!(result["replayed"], json!(false));
    let stored = everything(&engine).await;
    let meta = &stored[0].meta;
    assert_eq!(meta.namespace, Namespace::agent("writer"));
    assert_eq!(meta.source.kind, SourceKind::Agent);
    assert_eq!(meta.tags, ["drink"]);
    assert!(meta.observed_at.is_some());
    assert_eq!(stored[0].confidence, Some(0.5));
}

#[tokio::test]
async fn store_takes_documents_and_conversations() {
    let engine = ReferenceEngine::new();
    store(
        &engine,
        &scope(),
        &json!({ "document": { "title": "T", "text": "body" } }),
    )
    .await
    .unwrap();
    store(
        &engine,
        &scope(),
        &json!({ "conversation": { "turns": [
            { "role": "user", "text": "hi" }, { "role": "assistant", "text": "hello" }
        ] } }),
    )
    .await
    .unwrap();
    let texts: Vec<String> = everything(&engine)
        .await
        .into_iter()
        .map(|h| h.text)
        .collect();
    assert_eq!(texts, ["# T\n\nbody", "user: hi\nassistant: hello"]);
}

#[tokio::test]
async fn store_refuses_bad_shapes_by_field() {
    let engine = ReferenceEngine::new();
    let cases = [
        (
            json!({ "learning": { "text": "x", "learning_kind": "rumour" } }),
            "`learning.learning_kind`",
        ),
        (
            json!({ "learning": { "text": "x", "confidence": 2 } }),
            "`learning.confidence`",
        ),
        (json!({ "learning": { "text": " " } }), "`learning.text`"),
        (json!({ "learning": "x" }), "`learning` must be an object"),
        (
            json!({ "document": { "title": "no body" } }),
            "`document.text`",
        ),
        (
            json!({ "conversation": { "turns": [] } }),
            "`conversation.turns`",
        ),
        (
            json!({ "conversation": { "turns": [{ "role": "robot", "text": "x" }] } }),
            "`conversation.turns[].role`",
        ),
        (
            json!({ "conversation": { "turns": [{ "role": "user" }] } }),
            "`conversation.turns[].text`",
        ),
        (
            json!({ "learning": { "text": "x", "namespace": "root" } }),
            "`learning.namespace` is fixed by the host",
        ),
    ];
    for (value, expected) in cases {
        let text = invalid(store(&engine, &scope(), &value).await);
        assert!(text.contains(expected), "{value}: {text}");
    }
}

#[tokio::test]
async fn forget_needs_exactly_one_target_and_a_non_empty_filter() {
    let engine = ReferenceEngine::new();
    for value in [
        json!({}),
        json!({ "ids": ["a"], "filter": { "tags_any": ["x"] } }),
    ] {
        assert!(invalid(forget(&engine, &scope(), &value).await).contains("exactly one of"));
    }
    let text = invalid(forget(&engine, &scope(), &json!({ "filter": {} })).await);
    assert!(
        text.contains("`filter` must set at least one field"),
        "{text}"
    );
}

#[tokio::test]
async fn forget_by_ids_skips_what_is_out_of_reach() {
    let engine = ReferenceEngine::new();
    let mine = store(
        &engine,
        &scope(),
        &json!({ "learning": { "text": "mine" } }),
    )
    .await
    .unwrap();
    let other = ToolScope::at(Namespace::agent("other"));
    let theirs = store(
        &engine,
        &other,
        &json!({ "learning": { "text": "theirs" } }),
    )
    .await
    .unwrap();
    let result = forget(
        &engine,
        &scope(),
        &json!({ "ids": [mine["id"], theirs["id"], "nothing"] }),
    )
    .await
    .unwrap();
    assert_eq!(
        result,
        json!({ "forgotten": 1, "skipped": [theirs["id"], "nothing"] })
    );
    let left: Vec<String> = everything(&engine)
        .await
        .into_iter()
        .map(|h| h.text)
        .collect();
    assert_eq!(left, ["theirs"]);
}

#[tokio::test]
async fn forget_by_ids_with_nothing_in_reach_forgets_nothing() {
    let engine = ReferenceEngine::new();
    let result = forget(&engine, &scope(), &json!({ "ids": ["nothing"] }))
        .await
        .unwrap();
    assert_eq!(result, json!({ "forgotten": 0, "skipped": ["nothing"] }));
}

/// The reference engine, recording each forget and the reach of each
/// reach-confined forget.
struct Recording {
    inner: ReferenceEngine,
    forgets: std::sync::Mutex<Vec<ForgetTarget>>,
    within: std::sync::Mutex<Vec<tinymemory_api::Reach>>,
}

#[async_trait::async_trait]
impl MemoryEngine for Recording {
    fn descriptor(&self) -> &tinymemory_api::EngineDescriptor {
        self.inner.descriptor()
    }
    async fn health(&self) -> tinymemory_api::EngineHealth {
        self.inner.health().await
    }
    async fn recall(
        &self,
        req: tinymemory_api::RecallRequest,
    ) -> tinymemory_api::Result<tinymemory_api::RecallAnswer> {
        self.inner.recall(req).await
    }
    async fn fetch(
        &self,
        req: tinymemory_api::FetchRequest,
    ) -> tinymemory_api::Result<tinymemory_api::FetchPage> {
        self.inner.fetch(req).await
    }
    async fn store(&self, item: StoreItem) -> tinymemory_api::Result<tinymemory_api::StoreReceipt> {
        self.inner.store(item).await
    }
    async fn forget(&self, target: ForgetTarget) -> tinymemory_api::Result<ForgetReport> {
        self.forgets.lock().unwrap().push(target.clone());
        self.inner.forget(target).await
    }
    async fn forget_within(
        &self,
        ids: Vec<ItemId>,
        reach: tinymemory_api::Reach,
    ) -> tinymemory_api::Result<ForgetReport> {
        self.within.lock().unwrap().push(reach.clone());
        tinymemory_api::forget_within_by_get(self, ids, reach).await
    }
    async fn list(&self, req: ListRequest) -> tinymemory_api::Result<tinymemory_api::ListPage> {
        self.inner.list(req).await
    }
}

#[tokio::test]
async fn a_scoped_forget_by_ids_is_confined_to_the_scopes_reach() {
    let engine = Recording {
        inner: ReferenceEngine::new(),
        forgets: std::sync::Mutex::default(),
        within: std::sync::Mutex::default(),
    };
    let mine = store(&engine, &scope(), &json!({ "learning": { "text": "mine" } }))
        .await
        .unwrap();
    let reach = tinymemory_api::Reach::exact(Namespace::agent("writer"));
    let scoped = scope().with_reach(reach.clone());
    let result = forget(&engine, &scoped, &json!({ "ids": [mine["id"]] }))
        .await
        .unwrap();
    assert_eq!(result, json!({ "forgotten": 1, "skipped": [] }));
    assert_eq!(*engine.within.lock().unwrap(), vec![reach]);

    // Unscoped, the ids go to the plain forget by id.
    let again = store(&engine, &scope(), &json!({ "learning": { "text": "again" } }))
        .await
        .unwrap();
    let unscoped = ToolScope::default();
    forget(&engine, &unscoped, &json!({ "ids": [again["id"]] }))
        .await
        .unwrap();
    assert_eq!(engine.within.lock().unwrap().len(), 1);
    assert!(
        engine
            .forgets
            .lock()
            .unwrap()
            .contains(&ForgetTarget::Ids(vec![ItemId::new(
                again["id"].as_str().unwrap()
            )]))
    );
}
