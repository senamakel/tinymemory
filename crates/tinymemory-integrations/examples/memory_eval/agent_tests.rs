//! Regression coverage for the OpenHuman turn mirror.

use super::*;
use std::sync::Arc;
use tinymemory_api::conformance::ReferenceEngine;
use tinymemory_api::{LearningKind, ListRequest, MemoryEngine, MemoryMeta, StoreItem};
use tinymemory_tools::MemoryLayout;

#[test]
fn openhuman_defaults_to_plain_recall_and_resumes_after_compaction() {
    assert_eq!(HostHook::for_turn(false, false), HostHook::Plain);
    assert_eq!(HostHook::for_turn(true, false), HostHook::Resumed);
    assert_eq!(HostHook::for_turn(false, true), HostHook::Dated);
    assert_eq!(HostHook::for_turn(true, true), HostHook::DatedResumed);
}

#[test]
fn logged_reply_keeps_bounded_tool_results() {
    let long = "a".repeat(MAX_TOOL_LINE_CHARS + 20);
    let result = logged_reply(
        " Done. ",
        &[
            ToolStep {
                name: "read",
                result: "file  contents\n  changed",
            },
            ToolStep {
                name: "empty",
                result: "  ",
            },
        ],
    );
    assert_eq!(result, "Done.\n\nTools:\n- read → file contents changed");
    assert_eq!(
        logged_reply(
            "Done.",
            &[ToolStep {
                name: "read",
                result: Box::leak(long.into_boxed_str())
            }]
        )
        .lines()
        .last()
        .unwrap()
        .chars()
        .count(),
        "- read → ".chars().count() + MAX_TOOL_LINE_CHARS
    );
}

#[tokio::test]
async fn openhuman_turn_logs_tool_result_without_echoing_the_pack() {
    let engine = Arc::new(ReferenceEngine::new());
    let layout = MemoryLayout::default();
    engine
        .store(StoreItem::learning(
            "The release word is copper-lantern.",
            LearningKind::Fact,
            1.0,
            MemoryMeta {
                namespace: layout.learnings().clone(),
                ..MemoryMeta::default()
            },
        ))
        .await
        .unwrap();
    let memory = AgentMemory::new(engine.clone(), layout.clone(), "agent")
        .unwrap()
        .with_policy(crate::RecallPolicy {
            team_limit: 0,
            ..crate::RecallPolicy::default()
        });
    let mut agent = ScriptedAgent::new(memory, "thread", 8).openhuman();
    let record = agent
        .user(
            "What is the release word?",
            &[ToolStep {
                name: "read_file",
                result: "retry logic is in src/retry.rs",
            }],
        )
        .await
        .unwrap();
    assert!(!record.timed_out);
    assert!(record.logged);
    assert!(record.pack_tokens > 0);

    let exported = engine
        .export(ListRequest::new(layout.holistic_filter(), 100))
        .await
        .unwrap();
    let stored = serde_json::to_string(&exported.items).unwrap();
    assert!(stored.contains("read_file → retry logic is in src/retry.rs"));
    assert!(!stored.contains("<memory-context>"));
}
