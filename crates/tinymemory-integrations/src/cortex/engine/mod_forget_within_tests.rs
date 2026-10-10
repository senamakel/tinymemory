//! Forget by id within a reach: the ids are looked up only in the scopes the
//! reach admits, so no scope of another tree is listed or read, on either
//! wire, while forget by id alone still finds an id wherever it lives.

use super::*;
use crate::cortex::testing::{Shared, both};
use tinymemory_api::{LearningKind, MemoryMeta, MetaFilter};

/// A learning at `namespace`.
fn learning(text: &str, namespace: &str) -> StoreItem {
    let meta = MemoryMeta {
        namespace: namespace.parse().unwrap(),
        ..MemoryMeta::default()
    };
    StoreItem::learning(text, LearningKind::Fact, 0.9, meta)
}

/// Three users' trees side by side: `user:ann`, its sub-agent, and
/// `user:anna`, whose name starts with `ann`.
fn trees() -> [StoreItem; 3] {
    [
        learning("ann prefers tea", "user:ann"),
        learning("ann's scout found a cafe", "user:ann/agent:scout"),
        learning("anna prefers coffee", "user:anna"),
    ]
}

/// Percent-decodes the few escapes a scope path carries.
fn decode(value: &str) -> String {
    value
        .replace("%3A", ":")
        .replace("%3a", ":")
        .replace("%2F", "/")
        .replace("%2f", "/")
}

/// Every scope (or scope listing prefix) the requests after `mark` name,
/// decoded: a `scope=` or `prefix=` query parameter, or a body's `scope`.
fn scopes_named(state: &Shared, mark: usize, forgets_mark: usize) -> Vec<String> {
    let seen = state.seen.lock().unwrap();
    let mut named: Vec<String> = seen.requests[mark..]
        .iter()
        .filter_map(|request| request.split_once('?').map(|(_, query)| query.to_string()))
        .flat_map(|query| {
            query
                .split('&')
                .filter_map(|pair| pair.split_once('='))
                .filter(|(key, _)| *key == "scope" || *key == "prefix")
                .map(|(_, value)| decode(value))
                .collect::<Vec<_>>()
        })
        .collect();
    named.extend(
        seen.forgets[forgets_mark..]
            .iter()
            .filter_map(|body| body.get("scope").and_then(|s| s.as_str()).map(String::from)),
    );
    named
}

/// Whether `scope` lies at or below the node whose path is `node`.
fn inside(scope: &str, node: &str) -> bool {
    scope == node || scope.starts_with(&format!("{node}/"))
}

fn marks(state: &Shared) -> (usize, usize) {
    let seen = state.seen.lock().unwrap();
    (seen.requests.len(), seen.forgets.len())
}

#[tokio::test]
async fn a_subtree_reach_forgets_its_own_ids_and_reads_no_other_tree() {
    for (engine, state) in both().await {
        let receipts = engine.store_many(trees().to_vec()).await.unwrap();
        let ids: Vec<ItemId> = receipts.iter().map(|r| r.id.clone()).collect();
        let (mark, forgets_mark) = marks(&state);

        let report = engine
            .forget_within(ids.clone(), Reach::subtree("user:ann".parse().unwrap()))
            .await
            .unwrap();
        assert_eq!(report.forgotten, 2, "ann's item and her sub-agent's");

        let named = scopes_named(&state, mark, forgets_mark);
        assert!(!named.is_empty(), "the forget named no scope at all");
        let ann = "app:tinymemory/user:ann";
        for scope in &named {
            assert!(inside(scope, ann), "named {scope:?} outside {ann:?}");
        }
        assert!(
            named.iter().all(|scope| !scope.contains("user:anna")),
            "{named:?}"
        );

        let left = engine
            .list(ListRequest::new(MetaFilter::default(), 10))
            .await
            .unwrap();
        let left: Vec<ItemId> = left.items.into_iter().map(|hit| hit.id).collect();
        assert_eq!(left, vec![ids[2].clone()], "anna's item survives");
    }
}

#[tokio::test]
async fn an_exact_reach_lists_nothing_and_reads_only_its_node() {
    for (engine, state) in both().await {
        let receipts = engine.store_many(trees().to_vec()).await.unwrap();
        let ids: Vec<ItemId> = receipts.iter().map(|r| r.id.clone()).collect();
        let (mark, forgets_mark) = marks(&state);

        let report = engine
            .forget_within(ids.clone(), Reach::exact("user:ann".parse().unwrap()))
            .await
            .unwrap();
        assert_eq!(report.forgotten, 1, "only ann's own node");

        let requests = state.requests()[mark..].to_vec();
        assert!(
            requests.iter().all(|r| !r.contains("scopes")),
            "an exact reach needs no scope listing: {requests:?}"
        );
        let node = "app:tinymemory/user:ann/";
        for scope in scopes_named(&state, mark, forgets_mark) {
            assert!(
                scope.starts_with(node) && !scope[node.len()..].contains('/'),
                "named {scope:?}, not one of ann's own kind scopes"
            );
        }
    }
}

#[tokio::test]
async fn an_id_outside_the_reach_is_left_alone_and_sends_no_forget() {
    for (engine, state) in both().await {
        let receipts = engine.store_many(trees().to_vec()).await.unwrap();
        let anna = receipts[2].id.clone();
        let (_, forgets_mark) = marks(&state);

        let report = engine
            .forget_within(
                vec![anna.clone()],
                Reach::subtree("user:ann".parse().unwrap()),
            )
            .await
            .unwrap();
        assert_eq!(report.forgotten, 0);
        assert_eq!(state.seen.lock().unwrap().forgets.len(), forgets_mark);

        let hits = engine
            .get(GetRequest {
                ids: vec![anna.clone()],
                reach: None,
            })
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "anna's item is still there");
    }
}

#[tokio::test]
async fn forget_within_refuses_no_ids_without_a_request() {
    for (engine, state) in both().await {
        let before = state.requests().len();
        let error = engine
            .forget_within(Vec::new(), Reach::default())
            .await
            .expect_err("no ids");
        assert!(
            matches!(error, crate::cortex::Error::InvalidRequest(_)),
            "{error:?}"
        );
        assert_eq!(state.requests().len(), before);
    }
}
