//! The suite, run against the drivers this workspace ships as references.
//!
//! Two drivers, for two different reasons.
//!
//! `InMemoryProvider` is the calibration subject: its behaviour is obvious by
//! inspection, so a failure here means the *assertion* is wrong, not the
//! driver. Without it, a suite that only ever ran against real engines could
//! not tell those two cases apart.
//!
//! `NullMemoryProvider` is the opposite end — it accepts writes, discards them,
//! and reads back empty. Running the same assertions against it pins down which
//! parts of the contract a discard-everything driver must still uphold
//! (namespace isolation, an honest `forget`, a terminating export cursor,
//! errors that stay inside `MemoryError`) and which are vacuous for it.
//! A suite that could not run against `null` would be asserting storage rather
//! than the contract.

// A panic in a test IS the failure report — the same allowance the sibling
// conformance target carries.
#![allow(clippy::expect_used)]

use std::sync::Arc;

use tinymemory_api::null::NullMemoryProvider;
use tinymemory_api::provider::MemoryProvider;
use tinymemory_conformance::{assert_provider, InMemoryProvider};

#[tokio::test]
async fn the_in_memory_reference_driver_conforms() {
    assert_provider(Arc::new(InMemoryProvider::new())).await;
}

#[tokio::test]
async fn the_null_driver_conforms() {
    assert_provider(Arc::new(NullMemoryProvider::new())).await;
}

#[tokio::test]
async fn the_reference_driver_advertises_exactly_the_mandatory_families() {
    let provider = InMemoryProvider::new();
    let caps = provider.capabilities();
    assert_eq!(
        caps.len(),
        3,
        "the reference driver must advertise only what it can serve, got {caps:?}"
    );
    // Every optional accessor stays `None`, which is what makes the audit pass.
    assert!(provider.as_tree().is_none());
    assert!(provider.as_graph().is_none());
    assert!(provider.as_ingest().is_none());
}

/// The full driver is the third subject, and it is the one a *host* binds.
///
/// `InMemoryProvider` proves the assertions are right; `NullMemoryProvider`
/// proves which of them survive a driver that retains nothing. Neither answers
/// the question this driver exists for: a host testing its own layer above the
/// contract needs every optional family reachable, because its handlers ask for
/// them by accessor and take the `None` arm as "unsupported" rather than as
/// "empty". Running the same suite here keeps that convenience honest — a
/// driver that serves 27 families still has to uphold the three mandatory ones.
#[tokio::test]
async fn the_full_driver_conforms() {
    assert_provider(Arc::new(tinymemory_conformance::RecordingProvider::new())).await;
}

/// It advertises everything, which is the opposite of the reference driver's
/// claim and has to stay that way for `audit_provider` to pass: a driver that
/// advertised less than it serves fails the audit just as surely as one that
/// advertises more.
#[tokio::test]
async fn the_full_driver_advertises_every_family() {
    let provider = tinymemory_conformance::RecordingProvider::new();
    assert!(provider.as_tree().is_some());
    assert!(provider.as_chunks().is_some());
    assert!(provider.as_documents().is_some());
    assert!(provider.as_retrieval().is_some());
}

/// The full driver must actually retain, and this has to be asserted directly.
///
/// `assert_provider` skips every storage assertion when `retains_writes` probes
/// false, because a driver that accepts writes and discards them is a
/// legitimate binding — `NullMemoryProvider` is exactly that. The consequence
/// is that a *double* which drops writes by accident passes the whole suite
/// vacuously, which is precisely what happened here: the driver landed with
/// `store` returning `Ok(())` and `get` returning `Ok(None)`, and
/// `the_full_driver_conforms` went green having asserted nothing about storage.
///
/// The crate exports `retains_writes` for callers to catch this in their own
/// harnesses. It is worth spending it on our own.
#[tokio::test]
async fn the_full_driver_retains_writes() {
    let provider = tinymemory_conformance::RecordingProvider::new();
    assert!(
        tinymemory_conformance::retains_writes(&provider).await,
        "the full driver dropped a write — assert_provider would then skip \
         every storage assertion and pass vacuously"
    );
}

/// Every optional family the full driver serves must read back what it wrote.
///
/// `the_full_driver_retains_writes` asserts this for the entry tier only, and
/// that turned out to be too narrow: `put_document` stored while
/// `list_documents` answered `[]`, `put_tool_rule` stored while `tool_rules`
/// answered `[]`, `set_goals` stored while `goals` answered the default, and
/// `put_relation` stored while `relations` answered `[]`. Four write-only
/// families, each of which a host discovers as a handler round-trip that
/// silently returns nothing.
///
/// The suite could not catch it. `assert_provider`'s optional-family
/// assertions are gated on `as_*()`, and a driver that advertises a family and
/// discards its writes still satisfies every shape check. So this is a probe,
/// in the shape of `retains_writes`, spent once per family that stores.
#[tokio::test]
async fn the_full_driver_retains_every_family_it_serves() {
    use tinymemory_api::goals::GoalsDoc;
    use tinymemory_api::tool_memory::{ToolMemoryPriority, ToolMemoryRule, ToolMemorySource};
    use tinymemory_api::types::{GraphRelationRecord, NamespaceDocumentInput};

    let p = tinymemory_conformance::RecordingProvider::new();

    // documents: put -> list
    let documents = p.as_documents().expect("documents");
    documents
        .put_document(NamespaceDocumentInput {
            namespace: "retention".into(),
            key: "k".into(),
            title: "t".into(),
            content: "c".into(),
            source_type: "conformance".into(),
            priority: "normal".into(),
            tags: vec![],
            metadata: serde_json::Value::Null,
            category: "core".into(),
            session_id: None,
            document_id: None,
            taint: tinymemory_api::types::MemoryTaint::Internal,
        })
        .await
        .expect("put_document");
    let listed = documents
        .list_documents(Some("retention"))
        .await
        .expect("list_documents");
    assert!(
        listed["documents"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|d| d["key"] == "k")),
        "put_document stored nothing list_documents can see: {listed}"
    );

    // graph relations: put -> read
    let graph = p.as_graph().expect("graph");
    graph
        .put_relation(GraphRelationRecord {
            namespace: Some("retention".into()),
            subject: "s".into(),
            predicate: "p".into(),
            object: "o".into(),
            attrs: serde_json::Value::Null,
            updated_at: 0.0,
            evidence_count: 1,
            order_index: None,
            document_ids: vec![],
            chunk_ids: vec![],
        })
        .await
        .expect("put_relation");
    assert!(
        !graph
            .relations(Some("retention"), Some("s"), None, 10)
            .await
            .expect("relations")
            .is_empty(),
        "put_relation stored nothing relations can see"
    );

    // tool memory: put -> list -> delete
    let tools = p.as_tool_memory().expect("tool_memory");
    tools
        .put_tool_rule(ToolMemoryRule {
            id: "r1".into(),
            tool_name: "shell".into(),
            rule: "be careful".into(),
            priority: ToolMemoryPriority::Normal,
            source: ToolMemorySource::UserExplicit,
            tags: vec![],
            created_at: String::new(),
            updated_at: String::new(),
        })
        .await
        .expect("put_tool_rule");
    assert_eq!(
        tools.tool_rules("shell").await.expect("tool_rules").len(),
        1,
        "put_tool_rule stored nothing tool_rules can see"
    );
    assert!(
        tools
            .delete_tool_rule("shell", "r1")
            .await
            .expect("delete_tool_rule"),
        "delete_tool_rule did not find the rule that was just written"
    );

    // goals: set -> read
    let goals = p.as_goals().expect("goals");
    let doc = GoalsDoc {
        items: vec![tinymemory_api::goals::GoalItem::new("g1", "ship it")],
    };
    goals.set_goals(doc).await.expect("set_goals");
    assert_eq!(
        goals.goals().await.expect("goals").items.len(),
        1,
        "set_goals stored nothing goals can see"
    );
}
