//! CortexDB full-provider response validation tests.

use serde_json::json;
use tinymemory_api::error::MemoryError;

use super::receipt;

#[test]
fn receipt_requires_an_event_id_and_boolean_replay_flag() {
    assert!(matches!(
        receipt(&json!({"replayed_from_idempotency": false})),
        Err(MemoryError::Backend(_))
    ));
    assert!(matches!(
        receipt(&json!({"event_id": "evt-1"})),
        Err(MemoryError::Backend(_))
    ));
    assert!(matches!(
        receipt(&json!({"event_id": "evt-1", "replayed_from_idempotency": "false"})),
        Err(MemoryError::Backend(_))
    ));
    assert_eq!(
        receipt(&json!({"event_id": "evt-1", "replayed_from_idempotency": true})).ok(),
        Some(("evt-1".to_string(), true))
    );
}
