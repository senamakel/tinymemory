//! CortexDB full-provider response validation tests.

use serde_json::json;
use tinymemory_api::error::MemoryError;

use super::{cortex_role, layer_limits, observed_at, receipt};

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

#[test]
fn answer_layer_limits_never_exceed_the_contract_total() {
    for limit in 1..12 {
        let limits = layer_limits(limit);
        let total: u64 = limits
            .as_object()
            .into_iter()
            .flat_map(|values| values.values())
            .filter_map(serde_json::Value::as_u64)
            .sum();
        assert_eq!(total, limit as u64);
        assert_eq!(limits.as_object().map(serde_json::Map::len), Some(5));
    }
}

#[test]
fn learning_time_converts_to_rfc3339_and_rejects_invalid_values() {
    assert_eq!(
        observed_at(1_700_000_000.5).ok().as_deref(),
        Some("2023-11-14T22:13:20.500+00:00")
    );
    assert!(observed_at(f64::NAN).is_err());
    assert!(observed_at(f64::INFINITY).is_err());
}

#[test]
fn named_human_speakers_keep_the_user_message_class() {
    assert_eq!(cortex_role("alice"), "user");
    assert_eq!(cortex_role("assistant"), "assistant");
    assert_eq!(cortex_role("tool"), "tool");
    assert_eq!(cortex_role("system"), "system");
}
