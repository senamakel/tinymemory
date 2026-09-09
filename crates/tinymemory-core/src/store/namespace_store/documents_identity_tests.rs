//! Tests for how a document write resolves the row it addresses when the
//! caller supplies a `document_id` (the table's primary key) alongside the
//! `(namespace, key)` dedup key. The two can name different rows, and the
//! write must land on one row instead of failing on the primary key
//! (openhuman#6147).

use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;

use crate::store::{NamespaceDocumentInput, UnifiedMemory};
use tinymemory_api::host::NoopEmbedding;

fn open() -> (TempDir, UnifiedMemory) {
    let tmp = TempDir::new().unwrap();
    let memory = UnifiedMemory::new(tmp.path(), Arc::new(NoopEmbedding), None).unwrap();
    (tmp, memory)
}

fn input(
    namespace: &str,
    key: &str,
    document_id: Option<&str>,
    content: &str,
) -> NamespaceDocumentInput {
    NamespaceDocumentInput {
        namespace: namespace.to_string(),
        key: key.to_string(),
        title: key.to_string(),
        content: content.to_string(),
        source_type: "doc".to_string(),
        priority: "medium".to_string(),
        tags: vec![],
        metadata: json!({}),
        category: "core".to_string(),
        session_id: None,
        document_id: document_id.map(str::to_owned),
        taint: crate::MemoryTaint::ExternalSync,
    }
}

/// `(document_id, key, content, created_at)` of every row in `namespace`,
/// ordered by key.
fn stored_rows(memory: &UnifiedMemory, namespace: &str) -> Vec<(String, String, String, f64)> {
    let conn = memory.conn.lock();
    let mut statement = conn
        .prepare(
            "SELECT document_id, key, content, created_at FROM memory_docs
              WHERE namespace = ?1 ORDER BY key",
        )
        .unwrap();
    let rows = statement
        .query_map(
            rusqlite::params![UnifiedMemory::sanitize_namespace(namespace)],
            |row| {
                Ok::<_, rusqlite::Error>((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, f64>(3)?,
                ))
            },
        )
        .unwrap();
    rows.map(Result::unwrap).collect()
}

/// The distinct document ids `vector_chunks` holds for `namespace`.
fn chunk_document_ids(memory: &UnifiedMemory, namespace: &str) -> Vec<String> {
    let conn = memory.conn.lock();
    let mut statement = conn
        .prepare(
            "SELECT DISTINCT document_id FROM vector_chunks
              WHERE namespace = ?1 ORDER BY document_id",
        )
        .unwrap();
    let ids = statement
        .query_map(
            rusqlite::params![UnifiedMemory::sanitize_namespace(namespace)],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    ids.map(Result::unwrap).collect()
}

/// The reporter's shape (openhuman#6147). Sync providers used to key their
/// documents by TITLE while already passing the stable `{toolkit}:{id}` as
/// the document id; since openhuman#4953 the key is that id. Re-syncing such
/// a document writes a NEW `(namespace, key)` whose requested id the old row
/// still holds. Before this fix the insert failed with
/// `UNIQUE constraint failed: memory_docs.document_id`, and a provider that
/// does not tolerate scope errors aborted its whole run on every tick that
/// reached the item.
#[tokio::test]
async fn a_requested_id_that_names_a_row_under_a_stale_key_updates_that_row() {
    let (_tmp, memory) = open();
    let namespace = "skill-github";
    let stable_id = "github:4892120323";

    let legacy_id = memory
        .upsert_document(input(
            namespace,
            "Fix the login page",
            Some(stable_id),
            "issue body v1",
        ))
        .await
        .unwrap();
    assert_eq!(legacy_id, stable_id);
    let legacy_created_at = stored_rows(&memory, namespace)[0].3;

    let resynced_id = memory
        .upsert_document(input(
            namespace,
            stable_id,
            Some(stable_id),
            "issue body v2",
        ))
        .await
        .expect("a re-sync keyed by the stable id must update the title-keyed row");
    assert_eq!(resynced_id, legacy_id);

    let rows = stored_rows(&memory, namespace);
    assert_eq!(
        rows.len(),
        1,
        "the re-sync must update the row in place, not add a second one"
    );
    let (document_id, key, content, created_at) = &rows[0];
    assert_eq!(document_id, stable_id);
    assert_eq!(
        key, stable_id,
        "the row now carries the key the write addressed it by"
    );
    assert_eq!(content, "issue body v2");
    assert_eq!(
        *created_at, legacy_created_at,
        "re-keying is an update: created_at survives"
    );
    assert_eq!(
        chunk_document_ids(&memory, namespace),
        vec![stable_id.to_string()],
        "the chunks stay addressable from the row's id"
    );
    assert!(
        memory
            .get_document_by_key(namespace, stable_id)
            .await
            .unwrap()
            .is_some(),
        "the row resolves by its new key"
    );
    assert!(
        memory
            .get_document_by_key(namespace, "Fix the login page")
            .await
            .unwrap()
            .is_none(),
        "the stale key no longer resolves"
    );
}

/// A requested id that another namespace's row already holds must not block
/// this namespace's write: the store mints its usual derived id instead and
/// leaves the other row alone. Document ids are addressed per namespace
/// everywhere else (`delete_document`, chunk and graph lookups), so a foreign
/// row is not "the same document".
#[tokio::test]
async fn a_requested_id_owned_by_another_namespace_stores_under_a_derived_id() {
    let (_tmp, memory) = open();
    memory
        .upsert_document(input(
            "skill-github",
            "github:1",
            Some("github:1"),
            "issue in github",
        ))
        .await
        .unwrap();

    let stored = memory
        .upsert_document(input(
            "notes",
            "github:1",
            Some("github:1"),
            "a note about it",
        ))
        .await
        .expect("a foreign row must not block the write");

    assert_eq!(
        stored,
        UnifiedMemory::derive_document_id("notes", "github:1")
    );
    let notes = stored_rows(&memory, "notes");
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].0, stored);
    assert_eq!(notes[0].2, "a note about it");
    assert_eq!(chunk_document_ids(&memory, "notes"), vec![stored]);
    let github = stored_rows(&memory, "skill-github");
    assert_eq!(github.len(), 1);
    assert_eq!(
        (
            github[0].0.as_str(),
            github[0].1.as_str(),
            github[0].2.as_str()
        ),
        ("github:1", "github:1", "issue in github"),
        "the other namespace's row is untouched"
    );
}

/// When a row already exists for `(namespace, key)`, its id wins over a
/// different requested one. The upsert's `DO UPDATE` never rewrites
/// `document_id`, so honouring the request would hand back — and write the
/// chunks and queue the graph job under — an id no row has.
#[tokio::test]
async fn the_row_id_wins_over_a_conflicting_requested_id() {
    let (_tmp, memory) = open();
    let derived = memory
        .upsert_document(input("notes", "plan", None, "draft one"))
        .await
        .unwrap();
    assert_eq!(derived, UnifiedMemory::derive_document_id("notes", "plan"));

    let stored = memory
        .upsert_document(input("notes", "plan", Some("plan-v2"), "draft two"))
        .await
        .unwrap();

    assert_eq!(stored, derived, "the existing row's id is the write's id");
    let notes = stored_rows(&memory, "notes");
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].0, derived);
    assert_eq!(notes[0].2, "draft two");
    assert_eq!(
        chunk_document_ids(&memory, "notes"),
        vec![derived],
        "no chunks are written under an id the row does not have"
    );
}

/// The metadata-only path writes the same row, so it resolves the row the
/// same way: through a stale key when the requested id names one.
#[tokio::test]
async fn a_metadata_only_write_reaches_a_row_under_a_stale_key_too() {
    let (_tmp, memory) = open();
    let namespace = "skill-github";
    memory
        .upsert_document(input(
            namespace,
            "Fix the login page",
            Some("github:7"),
            "issue body v1",
        ))
        .await
        .unwrap();
    let legacy_created_at = stored_rows(&memory, namespace)[0].3;

    let stored = memory
        .upsert_document_metadata_only(input(
            namespace,
            "github:7",
            Some("github:7"),
            "issue body v2",
        ))
        .await
        .expect("the light write must update the title-keyed row");

    assert_eq!(stored, "github:7");
    let rows = stored_rows(&memory, namespace);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].1.as_str(), rows[0].2.as_str()),
        ("github:7", "issue body v2")
    );
    assert_eq!(rows[0].3, legacy_created_at);
    assert_eq!(
        chunk_document_ids(&memory, namespace),
        vec!["github:7".to_string()],
        "a metadata-only write leaves the row's chunks in place"
    );
}

/// The per-key write lock serialises writers of one key only. A writer that
/// resolved its row before the transaction can find, inside it, that another
/// writer reached the same document through a different key and moved it
/// meanwhile; the transaction re-checks the key itself so the upsert lands on
/// the row instead of tripping the primary key.
#[tokio::test]
async fn the_write_transaction_rekeys_a_row_another_writer_moved_meanwhile() {
    let (_tmp, memory) = open();
    let namespace = "skill-github";
    let stored_namespace = UnifiedMemory::sanitize_namespace(namespace);
    memory
        .upsert_document(input(
            namespace,
            "moved-key",
            Some("github:9"),
            "issue body v1",
        ))
        .await
        .unwrap();

    {
        let conn = memory.conn.lock();
        assert!(
            UnifiedMemory::rekey_document_in_namespace(
                &conn,
                &stored_namespace,
                "github:9",
                "github:9"
            )
            .unwrap(),
            "a row held under another key is moved under the key being written"
        );
        assert!(
            !UnifiedMemory::rekey_document_in_namespace(
                &conn,
                &stored_namespace,
                "github:9",
                "github:9"
            )
            .unwrap(),
            "a row already under the key is left alone"
        );
        assert!(
            !UnifiedMemory::rekey_document_in_namespace(
                &conn,
                &stored_namespace,
                "github:404",
                "x"
            )
            .unwrap(),
            "a document the namespace does not hold is nothing to re-key"
        );
    }

    let stored = memory
        .upsert_document(input(
            namespace,
            "github:9",
            Some("github:9"),
            "issue body v2",
        ))
        .await
        .expect("the upsert lands on the moved row");
    assert_eq!(stored, "github:9");
    let rows = stored_rows(&memory, namespace);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].1.as_str(), rows[0].2.as_str()),
        ("github:9", "issue body v2")
    );
}

/// A blank requested id is no id: the write mints its own rather than making
/// the empty string a primary key.
#[tokio::test]
async fn a_blank_requested_id_is_ignored() {
    let (_tmp, memory) = open();
    let stored = memory
        .upsert_document(input("notes", "plan", Some("   "), "draft"))
        .await
        .unwrap();
    assert_eq!(stored, UnifiedMemory::derive_document_id("notes", "plan"));
}
