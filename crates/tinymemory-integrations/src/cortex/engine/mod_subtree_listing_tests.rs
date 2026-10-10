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
async fn a_direct_listing_ends_its_prefix_with_the_separator_and_a_hosted_one_does_not() {
    for (index, (engine, state)) in both().await.into_iter().enumerate() {
        let hosted = index == 1;
        engine.store_many(trees()).await.unwrap();
        let mark = state.requests().len();

        engine.log.scopes(ANN).await.unwrap();

        let expected = if hosted {
            ANN.to_string()
        } else {
            format!("{ANN}/")
        };
        assert_eq!(listed_prefixes(&state, mark), vec![expected]);
    }
}

#[tokio::test]
async fn a_string_prefix_backend_lists_no_sibling_on_direct() {
    for (index, (engine, state)) in both().await.into_iter().enumerate() {
        let hosted = index == 1;
        state.string_prefix_scopes.store(true, Ordering::SeqCst);
        engine.store_many(trees()).await.unwrap();

        let listed = engine.log.scopes(ANN).await.unwrap();

        assert!(
            listed
                .iter()
                .any(|path| path.starts_with("app:tinymemory/user:ann/"))
        );
        let sibling = listed.iter().any(|path| path.contains("user:anna"));
        // Direct never asks for the sibling. Hosted cannot terminate its
        // prefix, so the sibling is answered and dropped by the reach.
        assert_eq!(sibling, hosted, "{listed:?}");
    }
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
