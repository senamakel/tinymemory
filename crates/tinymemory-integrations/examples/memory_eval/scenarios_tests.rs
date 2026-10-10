//! Determinism and placement checks for generated benchmark fixtures.

use super::*;

#[test]
fn scaled_needles_move_without_changing_the_decoys() {
    let early = scaled(100, "early", 17).unwrap();
    let late = scaled(100, "late", 17).unwrap();
    let [
        Step::BulkDocuments {
            needle_at: early_at,
            ..
        },
    ] = early.steps.as_slice()
    else {
        panic!("one bulk step")
    };
    let [
        Step::BulkDocuments {
            needle_at: late_at, ..
        },
    ] = late.steps.as_slice()
    else {
        panic!("one bulk step")
    };
    assert_eq!((*early_at, *late_at), (10, 89));
    assert_eq!(
        scale_document(20, *early_at, 17),
        scale_document(20, *late_at, 17)
    );
    assert!(scale_document(*early_at, *early_at, 17).contains("Mira Solis"));
    assert!(!scale_document(20, *early_at, 17).contains("Mira Solis"));
}

#[test]
fn scaled_fixture_rejects_unbounded_sizes_and_unknown_positions() {
    assert!(scaled(99, "early", 1).is_err());
    assert!(scaled(100, "unknown", 1).is_err());
}
