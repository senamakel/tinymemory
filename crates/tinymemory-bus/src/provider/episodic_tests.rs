//! Wire compatibility for the segment lifecycle marker (oh#6186).

// A failed assertion in a test is a panic either way; `unwrap`/`expect` here say
// which step failed, which a bare `?` in a `-> Result` test would not.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use super::{ConversationSegment, SegmentStatus};

fn segment(status: Option<SegmentStatus>) -> ConversationSegment {
    ConversationSegment {
        segment_id: "seg-1".to_string(),
        session_id: "sess-1".to_string(),
        namespace: "ns".to_string(),
        start_episodic_id: 1,
        end_episodic_id: Some(9),
        start_timestamp: 0.0,
        end_timestamp: Some(1.0),
        turn_count: 4,
        summary: None,
        embedding: None,
        open: false,
        status,
        start_seq: None,
        end_seq: None,
    }
}

/// The three lifecycle states survive a round trip under the identifiers the
/// engine already persists.
///
/// The spelling is the contract, not an implementation detail: a driver
/// writing `summarised` to SQLite and a payload saying `summarized` would make
/// the marker unreadable in exactly the case it exists for.
#[test]
fn every_status_round_trips_under_its_persisted_spelling() {
    for (status, wire) in [
        (SegmentStatus::Open, "open"),
        (SegmentStatus::Closed, "closed"),
        (SegmentStatus::Summarised, "summarised"),
    ] {
        let json = serde_json::to_string(&segment(Some(status))).expect("serialises");
        assert!(
            json.contains(&format!("\"status\":\"{wire}\"")),
            "{status:?} did not serialise as {wire}: {json}"
        );
        let back: ConversationSegment = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back.status, Some(status));
    }
}

/// A payload from a driver that predates the field decodes, and says so.
///
/// This is the compatibility that lets the field ship without a lockstep
/// upgrade: a host built against this contract must keep working against an
/// older released module. `None` has to stay distinguishable from a real
/// state — a host that read it as `Open`, or inferred `Closed` from
/// `open: false`, would re-summarise segments that were already summarised.
#[test]
fn a_payload_without_the_field_decodes_as_unknown() {
    let json = r#"{
        "segment_id": "seg-1",
        "session_id": "sess-1",
        "namespace": "ns",
        "start_episodic_id": 1,
        "start_timestamp": 0.0,
        "turn_count": 4,
        "open": false
    }"#;

    let decoded: ConversationSegment = serde_json::from_str(json).expect("deserialises");

    assert_eq!(decoded.status, None);
    assert!(!decoded.open);
}

/// The field is omitted when absent rather than emitted as `null`.
///
/// Keeps the payload a byte-for-byte match for what an older driver sends, so
/// a digest or golden fixture over the wire form does not move for a segment
/// whose status is unknown.
#[test]
fn an_unknown_status_is_omitted_from_the_wire() {
    let json = serde_json::to_string(&segment(None)).expect("serialises");
    assert!(!json.contains("status"), "status was emitted: {json}");
}

/// `open` cannot answer what `status` answers.
///
/// The regression this pins is the one in oh#6186: `open: false` was the only
/// signal a host had, and it is identical for a segment that was summarised
/// and one whose recap failed.
#[test]
fn open_alone_cannot_separate_closed_from_summarised() {
    let closed = segment(Some(SegmentStatus::Closed));
    let summarised = segment(Some(SegmentStatus::Summarised));

    assert_eq!(closed.open, summarised.open);
    assert_ne!(closed.status, summarised.status);
}
