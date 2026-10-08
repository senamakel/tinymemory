//! Tests for the connector-sync predicates.

use super::*;

#[test]
fn namespaces() {
    assert!(is_connector_namespace("skill-gmail"));
    assert!(is_connector_namespace("source:gmail:conn1"));
    assert!(!is_connector_namespace("source_gmail"));
    assert!(!is_connector_namespace("global"));
    assert!(!is_connector_namespace("document:notes"));
    assert!(!is_connector_namespace("skills"));
}

#[test]
fn chunk_identity() {
    assert!(chunk_source_by_identity("email", "anything"));
    assert!(chunk_source_by_identity("chat", "slack:conn1"));
    assert!(chunk_source_by_identity("document", "notion:c:page"));
    for toolkit in CONNECTOR_TOOLKIT_PREFIXES {
        assert!(chunk_source_by_identity("document", &format!("{toolkit}x")));
    }
    assert!(!chunk_source_by_identity("document", "mem_src:folder"));
    assert!(!chunk_source_by_identity("chat", "conversations:agent"));
    assert!(!chunk_source_by_identity("document", "slackish:x"));
}

#[test]
fn profile_prefix_matches_constant() {
    assert!(PROFILE_KEPT.contains(PROFILE_SKILL_PREFIX));
}
