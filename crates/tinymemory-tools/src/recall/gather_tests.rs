//! Same-thread chronology must preserve retrieval's cross-thread order.

use super::*;
use tinymemory_api::{ItemId, MemoryMeta, TurnRange};

fn turn(thread: &str, index: u32) -> Hit {
    Hit {
        id: ItemId::new(format!("{thread}-{index}")),
        kind: ItemKind::Conversation,
        text: format!("turn {index}"),
        meta: MemoryMeta {
            thread_id: Some(thread.to_string()),
            turns: Some(TurnRange {
                first: index,
                last: index,
            }),
            ..MemoryMeta::default()
        },
        score: 1.0,
        confidence: None,
    }
}

#[test]
fn newer_turns_lead_within_each_thread_without_moving_other_threads() {
    let mut hits = vec![turn("a", 1), turn("b", 2), turn("a", 3), turn("b", 0)];
    newest_turns_first_within_thread(&mut hits);
    let ids: Vec<_> = hits.iter().map(|hit| hit.id.as_str()).collect();
    assert_eq!(ids, ["a-3", "b-2", "a-1", "b-0"]);
}
