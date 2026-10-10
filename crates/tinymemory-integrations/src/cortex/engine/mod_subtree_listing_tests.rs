//! The scope listing a subtree reach sends names whole segments: Direct
//! terminates the node path with the separator, so a backend matching plain
//! string prefixes cannot list `user:anna` under `user:ann`. The hosted route
//! refuses a trailing separator, so there the bare node path is sent and the
//! reach narrows the answer.

use super::*;
use crate::cortex::testing::{Shared, both};
use std::sync::atomic::Ordering;
use tinymemory_api::{LearningKind, MemoryMeta};

const ANN: &str = "app:tinymemory/user:ann";

fn learning(text: &str, namespace: &str) -> StoreItem {
    let meta = MemoryMeta {
        namespace: namespace.parse().unwrap(),
        ..MemoryMeta::default()
    };
    StoreItem::learning(text, LearningKind::Fact, 0.9, meta)
}

/// `user:ann`, her sub-agent and `user:anna`, whose name starts with `ann`.
fn trees() -> Vec<StoreItem> {
    vec![
        learning("ann prefers tea", "user:ann"),
        learning("ann's scout found a cafe", "user:ann/agent:scout"),
        learning("anna prefers coffee", "user:anna"),
    ]
}

/// The decoded `prefix=` of every scope listing sent after `mark`.
fn listed_prefixes(state: &Shared, mark: usize) -> Vec<String> {
    state.requests()[mark..]
        .iter()
        .filter(|request| request.contains("scopes"))
        .filter_map(|request| request.split_once("prefix=").map(|(_, rest)| rest))
        .map(|rest| rest.split('&').next().unwrap_or_default())
        .map(|value| value.replace("%3A", ":").replace("%2F", "/"))
        .collect()
}

#[tokio::test]
async fn a_listing_sends_the_bare_node_path_on_both_wires() {
    for (engine, state) in both().await {
        engine.store_many(trees()).await.unwrap();
        let mark = state.requests().len();

        engine.log.scopes(ANN).await.unwrap();

        // The live server refuses a separator-terminated prefix.
        assert_eq!(listed_prefixes(&state, mark), vec![ANN.to_string()]);
    }
}

#[tokio::test]
async fn a_string_prefix_backend_lists_no_sibling_on_either_wire() {
    for (engine, state) in both().await {
        state.string_prefix_scopes.store(true, Ordering::SeqCst);
        engine.store_many(trees()).await.unwrap();

        let listed = engine.log.scopes(ANN).await.unwrap();

        assert!(
            listed
                .iter()
                .any(|path| path.starts_with("app:tinymemory/user:ann/")),
            "{listed:?}"
        );
        assert!(
            listed.iter().all(|path| !path.contains("user:anna")),
            "{listed:?}"
        );
    }
}

#[test]
fn whole_segment_matching_keeps_a_tenant_prefix_and_drops_a_sibling() {
    use crate::cortex::log::below_whole_segments as below;
    assert!(below("app:tinymemory/user:ann/app:learnings", ANN));
    assert!(below("org:1/app:tinymemory/user:ann/app:learnings", ANN));
    assert!(below(ANN, ANN));
    assert!(!below("app:tinymemory/user:anna/app:learnings", ANN));
}

#[tokio::test]
async fn a_subtree_reach_on_a_string_prefix_backend_reads_only_its_own_tree() {
    for (engine, state) in both().await {
        state.string_prefix_scopes.store(true, Ordering::SeqCst);
        let receipts = engine.store_many(trees()).await.unwrap();
        let ids: Vec<ItemId> = receipts.iter().map(|r| r.id.clone()).collect();

        let report = engine
            .forget_within(ids.clone(), Reach::subtree("user:ann".parse().unwrap()))
            .await
            .unwrap();

        assert_eq!(report.forgotten, 2, "ann's item and her sub-agent's");
        let anna = engine
            .get(GetRequest {
                ids: vec![ids[2].clone()],
                reach: None,
            })
            .await
            .unwrap();
        assert_eq!(anna.len(), 1, "anna's item survives");
    }
}
