//! Tests for the host secret/PII write gate.
//!
//! The gate was hoisted out of `namespace_store::documents` (H0, piece 2) so
//! the storage driver never decides what counts as a secret. These tests pin
//! **both** halves of that split, which is what makes the hoist provable rather
//! than cosmetic:
//!
//! * the gated entry points (`upsert_document`,
//!   `upsert_document_metadata_only`) still redact — the pre-existing
//!   behaviour, unchanged;
//! * the raw driver methods (`*_presanitized`) do **not** — they persist what
//!   they are handed, which is only safe because the gate is the sole caller.
//!
//! If someone folds redaction back into the driver, the second half fails.

use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;

use crate::store::{NamespaceDocumentInput, UnifiedMemory};
use tinymemory_api::host::NoopEmbedding;

/// A private key body, split so this source file does not itself contain a
/// scanner-tripping literal in one piece.
const PRIVATE_KEY_BODY: &str =
    "-----BEGIN PRIVATE KEY-----\nMIIBVgIBADANBgkq\n-----END PRIVATE KEY-----";

fn secret_doc(key: &str) -> NamespaceDocumentInput {
    NamespaceDocumentInput {
        namespace: "safe".to_string(),
        key: key.to_string(),
        title: "Bearer abcdefghijklmnop".to_string(),
        content: PRIVATE_KEY_BODY.to_string(),
        source_type: "doc".to_string(),
        priority: "medium".to_string(),
        tags: vec![],
        metadata: json!({}),
        category: "core".to_string(),
        session_id: None,
        document_id: None,
        taint: crate::MemoryTaint::Internal,
    }
}

fn fresh() -> (TempDir, UnifiedMemory) {
    let tmp = TempDir::new().unwrap();
    let memory = UnifiedMemory::new(tmp.path(), Arc::new(NoopEmbedding), None).unwrap();
    (tmp, memory)
}

#[tokio::test]
async fn gated_upsert_document_redacts_before_the_driver_persists() {
    let (_tmp, memory) = fresh();

    memory.upsert_document(secret_doc("note")).await.unwrap();

    let docs = memory.load_documents_for_scope("safe").await.unwrap();
    assert_eq!(docs.len(), 1);
    assert!(
        !docs[0].content.contains("BEGIN PRIVATE KEY"),
        "the gated entry point must redact private-key material, got {:?}",
        docs[0].content
    );
    assert!(
        !docs[0].title.contains("abcdefghijklmnop"),
        "the gated entry point must redact a bearer token in the title, got {:?}",
        docs[0].title
    );
}

#[tokio::test]
async fn gated_metadata_only_upsert_redacts_before_the_driver_persists() {
    let (_tmp, memory) = fresh();

    memory
        .upsert_document_metadata_only(secret_doc("note"))
        .await
        .unwrap();

    let docs = memory.load_documents_for_scope("safe").await.unwrap();
    assert_eq!(docs.len(), 1);
    assert!(
        !docs[0].content.contains("BEGIN PRIVATE KEY"),
        "the gated metadata-only entry point must redact, got {:?}",
        docs[0].content
    );
}

#[tokio::test]
async fn raw_driver_upsert_does_not_redact_so_the_gate_is_the_only_thing_doing_it() {
    let (_tmp, memory) = fresh();

    // Bypassing the gate deliberately: this is what proves redaction now lives
    // in `write_gate` and not inside the persistence call.
    memory
        .upsert_document_presanitized(secret_doc("note"))
        .await
        .unwrap();

    let docs = memory.load_documents_for_scope("safe").await.unwrap();
    assert_eq!(docs.len(), 1);
    assert!(
        docs[0].content.contains("BEGIN PRIVATE KEY"),
        "the raw driver method must persist its input verbatim — if this fails, redaction has \
         been folded back into the storage layer, got {:?}",
        docs[0].content
    );
}

#[tokio::test]
async fn raw_metadata_only_driver_upsert_does_not_redact() {
    let (_tmp, memory) = fresh();

    memory
        .upsert_document_metadata_only_presanitized(secret_doc("note"))
        .await
        .unwrap();

    let docs = memory.load_documents_for_scope("safe").await.unwrap();
    assert_eq!(docs.len(), 1);
    assert!(
        docs[0].content.contains("BEGIN PRIVATE KEY"),
        "the raw metadata-only driver method must persist its input verbatim, got {:?}",
        docs[0].content
    );
}

#[tokio::test]
async fn gate_rejects_a_secret_like_namespace_or_key_before_touching_the_driver() {
    let (_tmp, memory) = fresh();

    let mut secret_key = secret_doc("sk-1234567890123456789012345");
    secret_key.namespace = "safe".to_string();
    let err = memory.upsert_document(secret_key).await.unwrap_err();
    assert!(
        err.contains("cannot contain secrets"),
        "secret-like key must be refused, got {err:?}"
    );

    let mut secret_ns = secret_doc("note");
    secret_ns.namespace = "sk-1234567890123456789012345".to_string();
    let err = memory
        .upsert_document_metadata_only(secret_ns)
        .await
        .unwrap_err();
    assert!(
        err.contains("cannot contain secrets"),
        "secret-like namespace must be refused, got {err:?}"
    );

    // Nothing reached storage.
    assert!(memory
        .load_documents_for_scope("safe")
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn gate_canonicalizes_a_pii_like_key_and_keeps_the_row_addressable() {
    let (_tmp, memory) = fresh();

    memory
        .upsert_document(secret_doc("ssn-123-45-6789"))
        .await
        .unwrap();

    let docs = memory.load_documents_for_scope("safe").await.unwrap();
    assert_eq!(docs.len(), 1);
    assert!(
        !docs[0].key.contains("123-45-6789"),
        "a PII-like key must be canonicalized rather than stored raw, got {:?}",
        docs[0].key
    );
}

#[tokio::test]
async fn gated_batch_upsert_redacts_each_document_and_stops_at_the_first_rejection() {
    let (_tmp, memory) = fresh();

    let mut secret_key = secret_doc("sk-1234567890123456789012345");
    secret_key.namespace = "safe".to_string();
    let results = memory
        .upsert_documents(vec![
            secret_doc("first"),
            secret_key,
            secret_doc("never-attempted"),
        ])
        .await;

    assert_eq!(
        results.len(),
        2,
        "the rejected document ends the batch as its last entry, got {results:?}"
    );
    assert!(results[0].is_ok(), "{results:?}");
    let err = results[1].as_ref().unwrap_err();
    assert!(
        err.contains("cannot contain secrets"),
        "secret-like key must be refused, got {err:?}"
    );

    let docs = memory.load_documents_for_scope("safe").await.unwrap();
    assert_eq!(docs.len(), 1, "only the admitted prefix reaches storage");
    assert_eq!(docs[0].key, "first");
    assert!(
        !docs[0].content.contains("BEGIN PRIVATE KEY"),
        "a batched write must be redacted exactly like a single one, got {:?}",
        docs[0].content
    );
}
