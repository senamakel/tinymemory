//! The episodic family: the turn-by-turn record of conversations.
//!
//! A driver advertising [`Capability::Episodic`](crate::capabilities::Capability::Episodic)
//! stores every chat turn in a full-text index and groups consecutive turns
//! into *conversation segments* — a segment being a stretch of turns about one
//! thing, closed when the subject changes and then summarised and embedded.
//!
//! # Why this is a family rather than a raw connection
//!
//! It is the last thing in the host that held a live `rusqlite::Connection`.
//! The archivist hook was handed one straight out of the session factory and
//! called free functions on it, which worked only because the engine was
//! compiled into this process. A connection cannot cross a bus, so either the
//! archivist's operations become a contract family or episodic capture stays
//! behind and the engine can never leave.
//!
//! What crosses is small and already typed: insert a turn, read a session's
//! turns back, and six segment-lifecycle operations. That was the whole surface
//! the raw connection was used for — no ad-hoc SQL, no schema knowledge.
//!
//! # The host keeps the policy, and it is not a small share
//!
//! Two of the archivist's eight engine calls took no connection at all —
//! deciding *whether* a new turn starts a new segment, and composing a summary
//! when no model is available. Neither touches storage, so both stay host-side
//! in `agent::harness::archivist`, next to the recap logic and the boundary
//! thresholds they read. This family persists what the host decided; it does
//! not decide.
//!
//! # `insert_turn` returns the id, and that is load-bearing
//!
//! The old code inserted a row and then issued `SELECT last_insert_rowid()` on
//! the same connection to learn its id. That is two operations relying on a
//! *connection-local* side effect, and it is wrong the moment anything else
//! shares the connection or the two hops cross a bus — `last_insert_rowid` is
//! per-connection state, so an interleaved insert from another task yields the
//! wrong id and the turn is filed under the wrong segment.
//!
//! Returning the id from the insert removes both problems at once: one round
//! trip instead of two, and no reliance on connection-local state. The engine
//! knows the id it just wrote; nothing else has to guess.

use serde::{Deserialize, Serialize};

/// One recorded turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EpisodicTurn {
    /// Row id, assigned by the driver on insert.
    ///
    /// `None` when the host is describing a turn to be written; always `Some`
    /// on a turn read back.
    #[serde(default)]
    pub id: Option<i64>,
    /// Session this turn belongs to.
    pub session_id: String,
    /// When it happened, epoch seconds with sub-second resolution.
    ///
    /// The archivist offsets an assistant turn by 1 ms from the user turn it
    /// answers so the pair sorts in order within one exchange; that convention
    /// is the host's and the driver must preserve the value it is given rather
    /// than re-stamping it.
    pub timestamp: f64,
    /// `"user"` or `"assistant"`. Open vocabulary — a driver must not reject an
    /// unfamiliar role.
    pub role: String,
    /// The turn's text.
    pub content: String,
    /// A short lesson extracted from tool failures, when there was one.
    #[serde(default)]
    pub lesson: Option<String>,
    /// Serialized tool-call summary, when the turn made any.
    #[serde(default)]
    pub tool_calls_json: Option<String>,
    /// Cost attributed to this turn, in microdollars.
    #[serde(default)]
    pub cost_microdollars: i64,
}

/// Where a segment sits in its lifecycle.
///
/// The wire carries the same lowercase identifiers the engine persists, so a
/// stored row and a contract payload spell each state the same way.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentStatus {
    /// Accepting turns.
    Open,
    /// No longer accepting turns, and not yet summarised.
    ///
    /// This is the state a segment is left in when its recap failed — the
    /// marker a re-summarisation pass selects on (oh#6186). It is reached on
    /// the ordinary path too, for the window between closing a segment and
    /// writing its summary.
    Closed,
    /// Closed, with a summary written.
    Summarised,
}

/// A stretch of consecutive turns about one subject.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConversationSegment {
    /// Stable id, chosen by the host.
    pub segment_id: String,
    /// Session the segment belongs to.
    pub session_id: String,
    /// Owning namespace.
    pub namespace: String,
    /// Row id of the first turn in the segment.
    pub start_episodic_id: i64,
    /// Row id of the last turn, once one has been appended.
    #[serde(default)]
    pub end_episodic_id: Option<i64>,
    /// Timestamp of the first turn.
    pub start_timestamp: f64,
    /// Timestamp of the last turn, once one has been appended.
    #[serde(default)]
    pub end_timestamp: Option<f64>,
    /// How many turns the segment holds.
    pub turn_count: i32,
    /// Summary, once the segment has been closed and summarised.
    #[serde(default)]
    pub summary: Option<String>,
    /// The segment's running embedding centroid, when it has one.
    ///
    /// Carried on the read so the host can run boundary detection against it
    /// without a second call: deciding whether the next turn still belongs to
    /// this segment is host policy, but it needs the centroid the driver
    /// holds.
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
    /// Whether the segment is still open.
    ///
    /// Retained as the original contract vocabulary. It cannot distinguish a
    /// segment that was closed and summarised from one that was closed and
    /// left unsummarised — both are `false` — which is what [`Self::status`]
    /// exists to answer.
    pub open: bool,
    /// Where the segment sits in the `open -> closed -> summarised` lifecycle,
    /// when the driver reports it.
    ///
    /// `None` means the driver predates this field, not that the status is
    /// unknown-but-knowable: a host must treat it as "cannot tell" and skip
    /// the segment rather than infer one from [`Self::open`] or a `NULL`
    /// summary (oh#6186). Both of those are ambiguous — a segment can be
    /// legitimately summarised with no summary text when it had nothing to
    /// fold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SegmentStatus>,
    /// Stable per-session sequence of the first user turn, when the backing
    /// store assigns one. The md-backed archivist store rounds timestamps to
    /// milliseconds, so a fast turn can sort before its segment's
    /// higher-precision start time — the sequence is the identity that
    /// survives that, and segment selection prefers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_seq: Option<u32>,
    /// Sequence of the last appended user turn, likewise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_seq: Option<u32>,
}

/// What kind of durable fact an extracted event records.
///
/// Mirrors the engine's `event_log.event_type` vocabulary; the wire carries
/// the same lowercase identifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    /// A stated fact about the world or the user.
    Fact,
    /// A decision that was made.
    Decision,
    /// A commitment somebody took on.
    Commitment,
    /// A preference the user expressed.
    Preference,
    /// A question left open.
    Question,
    /// Something anticipated to happen.
    Foresight,
}

/// One extracted event, ready to be recorded against its segment.
///
/// Events are segment-derived episodic artifacts — a summariser or heuristic
/// reads a closed segment and records the durable facts it found. The driver
/// owns the table; the caller owns the extraction policy, which is why the
/// record arrives fully formed rather than as text to extract from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EpisodicEvent {
    /// Caller-assigned id; an upsert key, so a re-run replaces its own rows.
    pub event_id: String,
    /// Segment the event was extracted from.
    pub segment_id: String,
    /// Session that segment belongs to.
    pub session_id: String,
    /// Namespace the event is scoped to.
    pub namespace: String,
    /// What kind of fact this is.
    pub kind: EventKind,
    /// The event, in prose.
    pub content: String,
    /// Who or what the event is about, when extraction identified one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// A time the prose refers to, verbatim, when extraction found one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_ref: Option<String>,
    /// Extraction confidence in `[0, 1]`.
    pub confidence: f64,
    /// Embedding for the content, when the caller computed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
    /// Turn ids the event was derived from, encoded by the caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_turn_ids: Option<String>,
    /// When the event was recorded, seconds since the epoch.
    pub created_at: f64,
}

#[cfg(test)]
#[path = "episodic_tests.rs"]
mod tests;
