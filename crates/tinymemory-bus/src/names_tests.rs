//! Tests for the member-name table.
//!
//! These are pinning tests, not behavioural ones. A member name is a string
//! that only fails at runtime, in a host, as an `UnknownMethod` — so the value
//! here is in catching a typo or a duplicate at `cargo test` time in this
//! crate, before the module or a host ever sees it.
// A failed assertion in a test is a panic either way; `expect` here says what
// the invariant was. Same allowance the crate's other test modules take.
#![allow(clippy::expect_used, clippy::panic)]

use super::{methods, BUS_NAME, METHODS, OBJECT_PATH};

#[test]
fn the_object_identity_is_pinned() {
    // Changing either of these breaks every deployed host at once, so they are
    // spelled out here rather than derived from anything.
    assert_eq!(BUS_NAME, "ai.tinyhumans.tinymemory.Memory");
    assert_eq!(OBJECT_PATH, "/ai/tinyhumans/tinymemory/Memory");
}

#[test]
fn no_member_name_appears_twice() {
    let mut sorted = METHODS;
    sorted.sort_unstable();
    let mut unique = sorted.to_vec();
    unique.dedup();
    assert_eq!(
        unique.len(),
        METHODS.len(),
        "a member name is listed more than once"
    );
}

#[test]
fn every_member_name_is_pascal_case() {
    // `#[tinybus::interface]` derives a member from its method identifier with
    // `pascal_case`, so anything else in this table is a hand-written name that
    // will not match what the module actually serves.
    for member in METHODS {
        let mut chars = member.chars();
        let first = chars.next().unwrap_or('_');
        assert!(
            first.is_ascii_uppercase(),
            "{member} does not start with an uppercase letter"
        );
        assert!(
            member.chars().all(|c| c.is_ascii_alphanumeric()),
            "{member} is not alphanumeric"
        );
    }
}

#[test]
fn the_constants_and_the_table_are_the_same_set() {
    // A spot check in both directions: a constant that is not in the table
    // would be invisible to the module's drift assertion, and a table entry
    // with no constant is a name a caller has to spell by hand.
    assert!(METHODS.contains(&methods::STORE));
    assert!(METHODS.contains(&methods::OPEN_STORE));
    assert!(METHODS.contains(&methods::WORKFLOW_IDENTITY_MATCHES));
    assert_eq!(methods::STORE, "Store");
    assert_eq!(methods::OPEN_STORE, "OpenStore");
    assert_eq!(
        methods::WORKFLOW_IDENTITY_MATCHES,
        "WorkflowIdentityMatches"
    );
}

#[test]
fn the_host_shed_members_are_spelled_as_the_module_derives_them() {
    // These three exist because a host is removing its direct engine link and
    // has nowhere else to ask. A typo in one of them is not a compile error on
    // either side — it is an `UnknownMethod` the first time a status panel or a
    // chunk inspector is opened against a released module — so the spellings
    // are pinned here rather than only read off the table.
    assert_eq!(methods::DEGRADED_STATE, "DegradedState");
    assert_eq!(methods::CHUNK_SCORE, "ChunkScore");
    assert_eq!(methods::SOURCE_INGEST_STATUS, "SourceIngestStatus");
    assert!(METHODS.contains(&methods::DEGRADED_STATE));
    assert!(METHODS.contains(&methods::CHUNK_SCORE));
    assert!(METHODS.contains(&methods::SOURCE_INGEST_STATUS));
}

#[test]
fn the_newest_members_are_appended_rather_than_filed_with_their_family() {
    // Member order is wire order: the module compares its served members
    // against this table as a *sequence*, so a new member filed beside its
    // family renumbers every member after it. That is invisible here and shows
    // up as the wrong method being invoked on a host built against a different
    // release, which is why the positions are asserted and not just
    // membership.
    //
    // By absolute index since the runtime-tree round: this began as a
    // tail-of-table assertion, which fires on every append and is then
    // re-pointed at the new tail — doing the drift-witness job once, at the
    // cost of re-stating it each round. The absolute slots are the released
    // wire positions themselves, which is the stronger pin and the one the
    // summariser-door test below already argues for.
    assert_eq!(METHODS[128], methods::DEGRADED_STATE);
    assert_eq!(METHODS[129], methods::CHUNK_SCORE);
    assert_eq!(METHODS[130], methods::SOURCE_INGEST_STATUS);
    // Scheduler-gate round (openhuman#5935 / tinymemory#126): appended at the
    // tail — slot 141, after the ingestion round's 138-140 — per this table's
    // append-only rule.
    assert_eq!(METHODS[141], methods::OVERRIDE_SCHEDULER_GATE);
    assert_eq!(methods::OVERRIDE_SCHEDULER_GATE, "OverrideSchedulerGate");
    // Connector-backfill round (openhuman#6012): slot 142. Its family sits at
    // 137 (`FlushPending`), and filing it there is exactly what this test
    // exists to catch — it renumbers 137 onward and a host built against
    // v1.13.8 then invokes the wrong member.
    assert_eq!(METHODS[142], methods::BACKFILL_CONNECTOR_TREES);
    assert_eq!(methods::BACKFILL_CONNECTOR_TREES, "BackfillConnectorTrees");
    // Re-summarisation round (openhuman#6186): slot 143. Its family — the
    // episodic segment members — sits at 55-63, and filing it there would
    // renumber every member from 56 on.
    assert_eq!(METHODS[143], methods::SEGMENTS_PENDING_SUMMARY);
    assert_eq!(methods::SEGMENTS_PENDING_SUMMARY, "SegmentsPendingSummary");
}

#[test]
fn the_summariser_door_holds_the_wire_slots_it_was_released_in() {
    // `Summarise` and `RootSummaries` are the two members a host reaches for
    // once it stops linking the engine, so their spellings are pinned here as
    // well as read off the table — a typo in either is an `UnknownMethod` the
    // first time a seal runs against a released module, not a compile error.
    assert_eq!(methods::SUMMARISE, "Summarise");
    assert_eq!(methods::ROOT_SUMMARIES, "RootSummaries");

    // Their *positions* are pinned too, and by absolute index rather than from
    // the end. Member order is wire order, so a member inserted ahead of these
    // renumbers both and every member after them; asserting from the tail would
    // move silently under the next append, which is exactly the edit this is
    // here to catch.
    assert_eq!(METHODS[126], methods::SUMMARISE);
    assert_eq!(METHODS[127], methods::ROOT_SUMMARIES);
}

#[test]
fn the_runtime_tree_doors_hold_the_wire_slots_they_were_released_in() {
    // The final seven doors of the engine shed: the members the host's
    // tree-summarizer RPC surface and the flavour tool stand on once nothing
    // in the host links the engine. Their spellings are pinned here as well as
    // read off the table for the reason every earlier round gives — a typo is
    // an `UnknownMethod` the first time a released module is asked, never a
    // compile error on either side.
    assert_eq!(methods::RUNTIME_BUFFER_WRITE, "RuntimeBufferWrite");
    assert_eq!(methods::RUNTIME_READ_NODE, "RuntimeReadNode");
    assert_eq!(methods::RUNTIME_READ_CHILDREN, "RuntimeReadChildren");
    assert_eq!(methods::RUNTIME_TREE_STATUS, "RuntimeTreeStatus");
    assert_eq!(methods::RUNTIME_SUMMARIZE, "RuntimeSummarize");
    assert_eq!(methods::RUNTIME_REBUILD, "RuntimeRebuild");
    assert_eq!(methods::FLAVOUR_PROFILE, "FlavourProfile");

    // Their positions are pinned by absolute index, not from the tail, for the
    // reason the summariser-door test above gives: member order is wire order,
    // and an assertion measured from the end moves silently under the next
    // append — which is exactly the edit this exists to catch.
    assert_eq!(METHODS.len(), 144);
    assert_eq!(METHODS[131], methods::RUNTIME_BUFFER_WRITE);
    assert_eq!(METHODS[132], methods::RUNTIME_READ_NODE);
    assert_eq!(METHODS[133], methods::RUNTIME_READ_CHILDREN);
    assert_eq!(METHODS[134], methods::RUNTIME_TREE_STATUS);
    assert_eq!(METHODS[135], methods::RUNTIME_SUMMARIZE);
    assert_eq!(METHODS[136], methods::RUNTIME_REBUILD);
    assert_eq!(METHODS[137], methods::FLAVOUR_PROFILE);

    // The granular ingestion and answer doors landed after the runtime-tree
    // round and must not renumber any of its released slots.
    assert_eq!(METHODS[138], methods::INGEST_LEARNING);
    assert_eq!(METHODS[139], methods::INGEST_EVENT);
    assert_eq!(METHODS[140], methods::ANSWER);
}
