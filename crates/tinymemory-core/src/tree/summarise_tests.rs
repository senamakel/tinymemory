//! Behaviour of the deterministic fallback summariser.
//!
//! `summarise` itself needs a chat provider and is covered where the provider
//! seam is. [`fallback_summary`] needs nothing — it is the answer when no model
//! is reachable, which makes it the path a degraded install actually runs, and
//! it had no test of its own on either side of the host boundary.

use chrono::{TimeZone, Utc};

use super::summarise::{fallback_summary, SummaryInput};

fn input(id: &str, content: &str, entities: &[&str], topics: &[&str], score: f32) -> SummaryInput {
    let at = Utc
        .with_ymd_and_hms(2026, 5, 29, 9, 8, 7)
        .single()
        .expect("a real instant");
    SummaryInput {
        id: id.to_string(),
        content: content.to_string(),
        token_count: 0,
        entities: entities.iter().map(|e| (*e).to_string()).collect(),
        topics: topics.iter().map(|t| (*t).to_string()).collect(),
        time_range_start: at,
        time_range_end: at,
        score,
    }
}

/// Blank inputs are dropped rather than summarised into empty bullets, and the
/// budget is honoured.
///
/// The blank input carries an entity and the surviving one carries a topic, so
/// this also pins that the fallback propagates neither. That is easy to get
/// wrong in the direction that matters: carrying a dropped input's entities
/// forward would attribute them to a summary whose text never mentions them.
#[test]
fn a_blank_input_is_dropped_and_the_budget_is_honoured() {
    let inputs = vec![
        input("blank", "   ", &["ignored"], &[], 0.1),
        input(
            "long",
            &"alpha beta gamma delta epsilon zeta eta theta".repeat(20),
            &[],
            &["planning"],
            0.9,
        ),
    ];

    let out = fallback_summary(&inputs, 8);

    assert!(
        out.content.starts_with("— alpha"),
        "the blank input was not dropped: {:?}",
        out.content
    );
    assert!(
        out.token_count <= 9,
        "a budget of 8 produced {} tokens",
        out.token_count
    );
    assert!(
        out.entities.is_empty(),
        "a dropped input's entities were carried into the summary: {:?}",
        out.entities
    );
    assert!(
        out.topics.is_empty(),
        "topics were carried into a summary whose text does not mention them: {:?}",
        out.topics
    );
}

/// With nothing to summarise the fallback answers empty rather than a bullet
/// with no content behind it.
#[test]
fn no_inputs_produce_no_summary() {
    let out = fallback_summary(&[], 64);
    assert!(out.content.is_empty(), "got {:?}", out.content);
    assert_eq!(out.token_count, 0);
}
