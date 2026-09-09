//! Document CRUD against the `memory_docs` table.
//!
//! Owns the upsert pipeline (with chunking + embedding, batched across
//! documents), metadata-only writes for high-frequency callers,
//! list/delete/clear-namespace operations, and the markdown sidecar files in
//! `memory/namespaces/<ns>/docs/`.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use uuid::Uuid;

use crate::store::safety;
use crate::store::types::{NamespaceDocumentInput, StoredMemoryDocument, GLOBAL_NAMESPACE};

use super::UnifiedMemory;

/// Token budget per vector chunk when a document is split for embedding.
const DOCUMENT_CHUNK_MAX_TOKENS: usize = 225;

/// Upper bound on chunk texts sent to the embedding provider in one request
/// when a batch of documents is embedded together
/// ([`UnifiedMemory::upsert_documents_presanitized`]).
///
/// Sized under every provider's per-request input cap (Cohere admits 96 texts,
/// the others more) and, at [`DOCUMENT_CHUNK_MAX_TOKENS`] per chunk, roughly
/// 14k tokens per request — under every provider's token budget — so a batch
/// of any length becomes a handful of bounded requests rather than one a
/// provider may refuse or time out. Still one to two orders of magnitude
/// fewer round-trips than the one-per-document write path it replaces.
pub(crate) const EMBED_REQUEST_MAX_TEXTS: usize = 64;

/// The row a document write addresses, resolved before anything is written.
///
/// Two identities can name a row: the `(namespace, key)` dedup key every
/// write carries, and the document id — the table's primary key — when the
/// caller supplies one. They can disagree, and `memory_docs` only upserts on
/// the first: a write whose key is new but whose requested id an existing row
/// holds used to fail on the primary key (openhuman#6147). Both write paths
/// resolve the row up front so the row they update, the chunks they replace
/// and the id they hand back all agree.
enum DocumentIdentity {
    /// A row with this `(namespace, key)` exists. Its id wins over any
    /// requested one: the upsert's `DO UPDATE` never rewrites `document_id`,
    /// so chunks written under a different id would be orphaned and the graph
    /// job queued under an id no row has.
    Existing {
        document_id: String,
        created_at: f64,
    },
    /// No row has this key, but the requested id names a row of the same
    /// namespace filed under another key: the same document, written when a
    /// different key rule applied (sync providers keyed by title before
    /// openhuman#4953 while already passing their stable id). The write
    /// re-keys that row and updates it.
    StaleKey {
        document_id: String,
        created_at: f64,
    },
    /// A new row. `document_id` is the requested id when it is free; `None`
    /// when the write must mint one — nothing usable was requested, or the
    /// requested id belongs to a row in another namespace, which is not this
    /// document and must not block it.
    New { document_id: Option<String> },
}

impl UnifiedMemory {
    /// Insert or update a document by `(namespace, key)`. Writes the markdown
    /// sidecar, replaces vector chunks, and embeds them with the configured
    /// provider.
    ///
    /// The one-document case of [`Self::upsert_documents_presanitized`]; see it
    /// for the write order and the failure contract.
    ///
    /// **Takes already-sanitized input.** The host secret/PII write gate runs
    /// in [`crate::store::write_gate`], which owns this
    /// method's only call site; use `UnifiedMemory::upsert_document` instead
    /// unless you are that gate. Calling this directly persists caller content
    /// verbatim, credentials and all.
    pub(crate) async fn upsert_document_presanitized(
        &self,
        input: NamespaceDocumentInput,
    ) -> Result<String, String> {
        self.upsert_documents_presanitized(vec![input])
            .await
            .pop()
            .unwrap_or_else(|| Err("document upsert produced no result".to_string()))
    }

    /// Insert or update many documents, embedding their chunks **together**:
    /// one provider request per [`EMBED_REQUEST_MAX_TEXTS`] chunk texts across
    /// the whole batch rather than one request per document (tinymemory#138).
    /// A connector pass of several hundred small items used to pay one
    /// embedding round-trip each; here it pays one per bounded group of texts.
    ///
    /// Order of operations: every document is chunked, every chunk text is
    /// embedded (see [`Self::embed_chunk_texts`]), then the documents are
    /// written one at a time in input order — each under its own per-key write
    /// lock, with the row, the chunk replacement and the new vectors in ONE
    /// transaction, so a reader never sees a row whose chunks are still being
    /// replaced.
    ///
    /// The first document whose write fails ends the batch: the result holds
    /// one entry per document attempted, in input order, with that failure as
    /// its last entry, and the documents after it are left untouched. An
    /// embedding failure is not a write failure — a request the provider
    /// refuses leaves the chunks it covered vector-less (keyword-searchable and
    /// re-embeddable), exactly as the single-document path always has, and the
    /// batch carries on.
    ///
    /// **Takes already-sanitized input** — same contract as
    /// [`Self::upsert_document_presanitized`]; go through
    /// `UnifiedMemory::upsert_documents` instead.
    pub(crate) async fn upsert_documents_presanitized(
        &self,
        inputs: Vec<NamespaceDocumentInput>,
    ) -> Vec<Result<String, String>> {
        let chunked: Vec<Vec<String>> = inputs
            .iter()
            .map(|input| Self::chunk_document_content(&input.content, DOCUMENT_CHUNK_MAX_TOKENS))
            .collect();
        let texts: Vec<&str> = chunked.iter().flatten().map(String::as_str).collect();
        let mut vectors = self.embed_chunk_texts(&texts).await.into_iter();

        let mut results = Vec::with_capacity(inputs.len());
        for (input, chunks) in inputs.into_iter().zip(chunked) {
            // `embed_chunk_texts` yields exactly one slot per text, in order,
            // so the next `chunks.len()` slots are this document's.
            let embeddings: Vec<Option<Vec<f32>>> = vectors.by_ref().take(chunks.len()).collect();
            let result = self
                .write_document_presanitized(input, chunks, embeddings)
                .await;
            let failed = result.is_err();
            results.push(result);
            if failed {
                break;
            }
        }
        results
    }

    /// Embed `texts` in requests of at most [`EMBED_REQUEST_MAX_TEXTS`],
    /// returning one slot per input position.
    ///
    /// Failure handling keeps the per-chunk resilience the single-document
    /// path always had:
    ///   * a request the provider refuses leaves every position it covered
    ///     `None` — logged, not propagated, because a vector-less chunk is
    ///     still keyword-searchable and re-embeddable while a failed write is
    ///     lost;
    ///   * a provider that returns fewer vectors than texts, or an empty vector
    ///     for a position (`NoopEmbedding`, NaN recovery), leaves those
    ///     positions `None` by position.
    async fn embed_chunk_texts(&self, texts: &[&str]) -> Vec<Option<Vec<f32>>> {
        let mut out: Vec<Option<Vec<f32>>> = Vec::with_capacity(texts.len());
        for request in texts.chunks(EMBED_REQUEST_MAX_TEXTS) {
            log::debug!(
                "[memory] batch-embedding {} chunk text(s) in one request",
                request.len()
            );
            match self.embedder.embed(request).await {
                Ok(vectors) => {
                    let mut vectors = vectors
                        .into_iter()
                        .map(|vector| (!vector.is_empty()).then_some(vector));
                    out.extend(request.iter().map(|_| vectors.next().flatten()));
                }
                Err(e) => {
                    log::warn!(
                        "[memory] batch embed failed for {} chunk text(s); storing them without vectors: {e}",
                        request.len()
                    );
                    out.resize(out.len() + request.len(), None);
                }
            }
        }
        out
    }

    /// Persist one document whose chunks and vectors were computed up front.
    ///
    /// Under the per-key write lock: resolve the document id and `created_at`,
    /// write the markdown sidecar, then commit the `memory_docs` row, the chunk
    /// replacement and the new `vector_chunks` rows in a single transaction.
    /// `embeddings` is aligned to `chunks` by position; a missing or `None`
    /// slot stores that chunk without a vector.
    async fn write_document_presanitized(
        &self,
        input: NamespaceDocumentInput,
        chunks: Vec<String>,
        mut embeddings: Vec<Option<Vec<f32>>>,
    ) -> Result<String, String> {
        let namespace = Self::sanitize_namespace(&input.namespace);
        // The logical (delimiter-preserving) namespace, PII-redacted the same
        // way `sanitize_namespace` redacts the storage address, so
        // `namespace_summaries` can report `conversation:thread-8f21` back
        // verbatim instead of the path-safe `conversation_thread-8f21`. Uses
        // the same blank-input fallback as `sanitize_namespace` and strips the
        // redaction placeholder's brackets so a PII-bearing sectioned
        // namespace stays `Namespace::parse`-able -- see
        // `canonical_logical_namespace`'s doc comment for both.
        let logical_namespace =
            safety::canonical_logical_namespace(&input.namespace, GLOBAL_NAMESPACE);
        let key = input.key.trim().to_string();
        if key.is_empty() {
            return Err("document key cannot be empty".to_string());
        }
        // Serialise writers of one key for the WHOLE write. A deterministic
        // document id stops two writers orphaning each other's chunks, but it
        // does not ORDER them: the id / `created_at` lookups, the sidecar write
        // (which awaits) and the transaction below must not interleave with
        // another writer of the same key, or writer B's row can land between
        // writer A's lookups and A's commit and be overwritten by content A
        // resolved against stale state. The metadata-only path below takes the
        // same lock: it writes the same row, so it must not interleave with a
        // full write either. Same guard shape as the sync path's per-connection
        // lock. Embedding happens before this lock is taken, so no provider
        // round-trip is ever awaited while holding it.
        let _write_guard = Self::document_write_lock(&self.db_path, &namespace, &key)
            .lock_owned()
            .await;
        let now = Self::now_ts();
        let identity = {
            let conn = self.conn.lock();
            Self::resolve_document_identity(&conn, &namespace, &key, input.document_id.as_deref())?
        };
        let (document_id, created_at, rekey) = match identity {
            DocumentIdentity::Existing {
                document_id,
                created_at,
            } => (document_id, created_at, false),
            DocumentIdentity::StaleKey {
                document_id,
                created_at,
            } => (document_id, created_at, true),
            // Derived from (namespace, key), NOT random. The lookup above and
            // the write below are separated by `.await`s, so two concurrent
            // stores of a not-yet-existing key both miss and both mint an id.
            // The ROW is safe -- `ON CONFLICT(namespace, key) DO UPDATE` keeps
            // exactly one -- but that clause does not update `document_id`,
            // and each writer has already written `vector_chunks` under ITS
            // OWN id. The loser's chunks are then unreachable from the row, so
            // `forget` (which deletes chunks by the row's document_id) leaves
            // them behind and recall keeps returning content the caller
            // deleted. A deterministic id makes both writers choose the same
            // one, so the second write updates the first's chunks instead of
            // orphaning them.
            DocumentIdentity::New { document_id } => (
                document_id.unwrap_or_else(|| Self::derive_document_id(&namespace, &key)),
                now,
                false,
            ),
        };
        let updated_at = now;
        let markdown_rel = self
            .write_markdown_doc(
                &namespace,
                &document_id,
                &input.title,
                &input.source_type,
                &input.priority,
                &input.tags,
                created_at,
                updated_at,
                &input.content,
            )
            .await
            .map_err(|e| e.to_string())?;

        let tags_json = serde_json::to_string(&input.tags).map_err(|e| e.to_string())?;
        let metadata_json = input.metadata.to_string();

        // Computed once; only attached to chunks that actually got a vector.
        let signature = self.embedder.signature();
        {
            let conn = self.conn.lock();
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| format!("begin tx: {e}"))?;
            if rekey {
                Self::rekey_document(&tx, &namespace, &document_id, &key)?;
            }
            tx.execute(
                "INSERT INTO memory_docs
                  (document_id, namespace, key, title, content, source_type, priority, tags_json, metadata_json, category, session_id, created_at, updated_at, markdown_rel_path, taint, logical_namespace)
                 VALUES
                  (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                 ON CONFLICT(namespace, key) DO UPDATE SET
                  title = excluded.title,
                  content = excluded.content,
                  source_type = excluded.source_type,
                  priority = excluded.priority,
                  tags_json = excluded.tags_json,
                  metadata_json = excluded.metadata_json,
                  category = excluded.category,
                  session_id = excluded.session_id,
                  updated_at = excluded.updated_at,
                  markdown_rel_path = excluded.markdown_rel_path,
                  taint = excluded.taint,
                  logical_namespace = excluded.logical_namespace",
                params![
                    document_id,
                    namespace,
                    key,
                    input.title,
                    input.content,
                    input.source_type,
                    input.priority,
                    tags_json,
                    metadata_json,
                    input.category,
                    input.session_id,
                    created_at,
                    updated_at,
                    markdown_rel,
                    input.taint.as_db_str(),
                    logical_namespace
                ],
            )
            .map_err(|e| format!("upsert memory_docs: {e}"))?;
            tx.execute(
                "DELETE FROM vector_chunks WHERE namespace = ?1 AND document_id = ?2",
                params![namespace, document_id],
            )
            .map_err(|e| format!("clear vector chunks: {e}"))?;
            for (idx, chunk) in chunks.iter().enumerate() {
                // Move the vector out by position so recall can exclude vectors
                // produced by a different embedding model (cross-model cosine is
                // meaningless) and guard against dimension mismatches. Missing
                // positions (short/empty provider result) stay vector-less.
                let embedded = embeddings.get_mut(idx).and_then(Option::take);
                let dim = embedded.as_ref().map(|v| v.len() as i64);
                let model_signature = embedded.as_ref().map(|_| signature.clone());
                let embedding = embedded.as_ref().map(|v| Self::vec_to_bytes(v));
                let chunk_id = format!("{document_id}:{idx}");
                tx.execute(
                    "INSERT OR REPLACE INTO vector_chunks
                      (namespace, document_id, chunk_id, text, embedding, metadata_json, created_at, updated_at, model_signature, dim)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        namespace,
                        document_id,
                        chunk_id,
                        chunk,
                        embedding,
                        json!({"lancedb_table": format!("ns_{namespace}"), "chunk_index": idx}).to_string(),
                        now,
                        now,
                        model_signature,
                        dim
                    ],
                )
                .map_err(|e| format!("insert vector chunk: {e}"))?;
            }
            tx.commit().map_err(|e| format!("commit tx: {e}"))?;
        }

        Ok(document_id)
    }

    /// Store a document (DB row + markdown file) without chunking, embedding,
    /// or graph extraction. Suitable for high-frequency, low-value writes
    /// (e.g. transient sync checkpoints) where the full ingestion pipeline
    /// would be too expensive.
    ///
    /// **Takes already-sanitized input** — same contract as
    /// [`Self::upsert_document_presanitized`]; go through
    /// `UnifiedMemory::upsert_document_metadata_only` instead.
    pub(crate) async fn upsert_document_metadata_only_presanitized(
        &self,
        input: NamespaceDocumentInput,
    ) -> Result<String, String> {
        let namespace = Self::sanitize_namespace(&input.namespace);
        // See `upsert_document_presanitized` — same delimiter-preserving,
        // PII-redacted logical namespace, same reason.
        let logical_namespace =
            safety::canonical_logical_namespace(&input.namespace, GLOBAL_NAMESPACE);
        let key = input.key.trim().to_string();
        if key.is_empty() {
            return Err("document key cannot be empty".to_string());
        }
        // Serialise writers of one key for the WHOLE operation. A deterministic
        // document id stops two writers orphaning each other's chunks, but it
        // does not ORDER them: the row write and the chunk replacement are
        // separated by embedding, which awaits. Without this, writer A can
        // update the row, await the embedder, and have B update the row and
        // replace the chunks in between -- leaving B's content beside A's
        // chunks, plus A's trailing chunks if A had more. The metadata-only
        // path below takes the same lock: it writes the same row, so it must
        // not interleave with a full write either. Same guard shape as the
        // sync path's per-connection lock.
        let _write_guard = Self::document_write_lock(&self.db_path, &namespace, &key)
            .lock_owned()
            .await;
        let now = Self::now_ts();
        let identity = {
            let conn = self.conn.lock();
            Self::resolve_document_identity(&conn, &namespace, &key, input.document_id.as_deref())?
        };
        let (document_id, created_at, rekey) = match identity {
            DocumentIdentity::Existing {
                document_id,
                created_at,
            } => (document_id, created_at, false),
            DocumentIdentity::StaleKey {
                document_id,
                created_at,
            } => (document_id, created_at, true),
            DocumentIdentity::New { document_id } => (
                document_id.unwrap_or_else(|| {
                    let ts = Self::now_ts() as u64;
                    let short = &Uuid::new_v4().to_string()[..8];
                    format!("{ts}_{short}")
                }),
                now,
                false,
            ),
        };
        let updated_at = now;
        let markdown_rel = self
            .write_markdown_doc(
                &namespace,
                &document_id,
                &input.title,
                &input.source_type,
                &input.priority,
                &input.tags,
                created_at,
                updated_at,
                &input.content,
            )
            .await
            .map_err(|e| e.to_string())?;

        let tags_json = serde_json::to_string(&input.tags).map_err(|e| e.to_string())?;
        let metadata_json = input.metadata.to_string();

        {
            let conn = self.conn.lock();
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| format!("begin tx: {e}"))?;
            if rekey {
                Self::rekey_document(&tx, &namespace, &document_id, &key)?;
            }
            tx.execute(
                "INSERT INTO memory_docs
                  (document_id, namespace, key, title, content, source_type, priority, tags_json, metadata_json, category, session_id, created_at, updated_at, markdown_rel_path, taint, logical_namespace)
                 VALUES
                  (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                 ON CONFLICT(namespace, key) DO UPDATE SET
                  title = excluded.title,
                  content = excluded.content,
                  source_type = excluded.source_type,
                  priority = excluded.priority,
                  tags_json = excluded.tags_json,
                  metadata_json = excluded.metadata_json,
                  category = excluded.category,
                  session_id = excluded.session_id,
                  updated_at = excluded.updated_at,
                  markdown_rel_path = excluded.markdown_rel_path,
                  taint = excluded.taint,
                  logical_namespace = excluded.logical_namespace",
                params![
                    document_id,
                    namespace,
                    key,
                    input.title,
                    input.content,
                    input.source_type,
                    input.priority,
                    tags_json,
                    metadata_json,
                    input.category,
                    input.session_id,
                    created_at,
                    updated_at,
                    markdown_rel,
                    input.taint.as_db_str(),
                    logical_namespace
                ],
            )
            .map_err(|e| format!("upsert memory_docs: {e}"))?;
            tx.commit().map_err(|e| format!("commit tx: {e}"))?;
        }

        Ok(document_id)
    }

    /// Fetch a single document by `(namespace, key)`.
    ///
    /// The same SELECT as [`Self::load_documents_for_scope`] with a `key`
    /// predicate bolted on — deliberately *not* implemented as
    /// `load_documents_for_scope(ns).find(…)`, which would load every document
    /// body in the namespace to return one.
    ///
    /// `key` goes through [`safety::canonical_document_key`], the exact
    /// transform `upsert_document` applies before writing the column. Reading
    /// the raw key here would reproduce #5164: the lookup misses, the caller
    /// treats the row as absent, and writes it again.
    pub(crate) async fn get_document_by_key(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<Option<StoredMemoryDocument>, String> {
        let conn = self.conn.lock();
        let ns = Self::sanitize_namespace(namespace);
        let key = safety::canonical_document_key(key);
        let mut stmt = conn
            .prepare(
                "SELECT
                    document_id,
                    namespace,
                    key,
                    title,
                    content,
                    source_type,
                    priority,
                    tags_json,
                    metadata_json,
                    category,
                    session_id,
                    created_at,
                    updated_at,
                    markdown_rel_path,
                    taint
                 FROM memory_docs
                 WHERE namespace = ?1 AND key = ?2
                 LIMIT 1",
            )
            .map_err(|e| format!("prepare get_document_by_key: {e}"))?;
        let mut rows = stmt
            .query(params![ns, key])
            .map_err(|e| format!("query get_document_by_key: {e}"))?;
        let Some(row) = rows
            .next()
            .map_err(|e| format!("row get_document_by_key: {e}"))?
        else {
            return Ok(None);
        };
        let tags_json: String = row.get(7).map_err(|e| e.to_string())?;
        let metadata_json: String = row.get(8).map_err(|e| e.to_string())?;
        let taint_str: String = row.get(14).map_err(|e| e.to_string())?;
        Ok(Some(StoredMemoryDocument {
            document_id: row.get(0).map_err(|e| e.to_string())?,
            namespace: row.get(1).map_err(|e| e.to_string())?,
            key: row.get(2).map_err(|e| e.to_string())?,
            title: row.get(3).map_err(|e| e.to_string())?,
            content: row.get(4).map_err(|e| e.to_string())?,
            source_type: row.get(5).map_err(|e| e.to_string())?,
            priority: row.get(6).map_err(|e| e.to_string())?,
            tags: serde_json::from_str(&tags_json).unwrap_or_default(),
            metadata: serde_json::from_str(&metadata_json).unwrap_or_else(|_| json!({})),
            category: row.get(9).map_err(|e| e.to_string())?,
            session_id: row.get(10).map_err(|e| e.to_string())?,
            created_at: row.get(11).map_err(|e| e.to_string())?,
            updated_at: row.get(12).map_err(|e| e.to_string())?,
            markdown_rel_path: row.get(13).map_err(|e| e.to_string())?,
            taint: crate::MemoryTaint::from_db_str(&taint_str),
        }))
    }

    pub(crate) async fn load_documents_for_scope(
        &self,
        namespace: &str,
    ) -> Result<Vec<StoredMemoryDocument>, String> {
        let conn = self.conn.lock();
        let ns = Self::sanitize_namespace(namespace);
        let mut stmt = conn
            .prepare(
                "SELECT
                    document_id,
                    namespace,
                    key,
                    title,
                    content,
                    source_type,
                    priority,
                    tags_json,
                    metadata_json,
                    category,
                    session_id,
                    created_at,
                    updated_at,
                    markdown_rel_path,
                    taint
                 FROM memory_docs
                 WHERE namespace = ?1
                 ORDER BY updated_at DESC",
            )
            .map_err(|e| format!("prepare load_documents_for_scope: {e}"))?;
        let mut rows = stmt
            .query(params![ns])
            .map_err(|e| format!("query load_documents_for_scope: {e}"))?;
        let mut docs = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| format!("row load_documents_for_scope: {e}"))?
        {
            docs.push(Self::row_to_stored_document(row)?);
        }
        Ok(docs)
    }

    /// Map one `memory_docs` row, in the column order
    /// [`Self::load_documents_for_scope`] selects it in, into a
    /// [`StoredMemoryDocument`].
    fn row_to_stored_document(row: &rusqlite::Row<'_>) -> Result<StoredMemoryDocument, String> {
        let tags_json: String = row.get(7).map_err(|e| e.to_string())?;
        let metadata_json: String = row.get(8).map_err(|e| e.to_string())?;
        // The `taint` column has a NOT NULL DEFAULT 'internal' clause
        // from the migration, so legacy rows that pre-date the column
        // surface as "internal" string and round-trip back to
        // `MemoryTaint::Internal`. Unknown / corrupted values fail
        // closed to `MemoryTaint::ExternalSync` inside `from_db_str`,
        // so a forward-rolled schema variant or a bad UPDATE can't
        // silently downgrade a row to user-authored content.
        let taint_str: String = row.get(14).map_err(|e| e.to_string())?;
        let taint = crate::MemoryTaint::from_db_str(&taint_str);
        Ok(StoredMemoryDocument {
            document_id: row.get(0).map_err(|e| e.to_string())?,
            namespace: row.get(1).map_err(|e| e.to_string())?,
            key: row.get(2).map_err(|e| e.to_string())?,
            title: row.get(3).map_err(|e| e.to_string())?,
            content: row.get(4).map_err(|e| e.to_string())?,
            source_type: row.get(5).map_err(|e| e.to_string())?,
            priority: row.get(6).map_err(|e| e.to_string())?,
            tags: serde_json::from_str(&tags_json).unwrap_or_default(),
            metadata: serde_json::from_str(&metadata_json).unwrap_or_else(|_| json!({})),
            category: row.get(9).map_err(|e| e.to_string())?,
            session_id: row.get(10).map_err(|e| e.to_string())?,
            created_at: row.get(11).map_err(|e| e.to_string())?,
            updated_at: row.get(12).map_err(|e| e.to_string())?,
            markdown_rel_path: row.get(13).map_err(|e| e.to_string())?,
            taint,
        })
    }

    /// List documents in a namespace, or across all namespaces when `None`.
    /// Returns `{ "documents": [...], "count": N }` JSON.
    pub async fn list_documents(&self, namespace: Option<&str>) -> Result<Value, String> {
        let conn = self.conn.lock();
        let mut docs = Vec::new();
        if let Some(ns) = namespace {
            let mut stmt = conn
                .prepare(
                    "SELECT document_id, namespace, key, title, source_type, priority, created_at, updated_at, taint
                     FROM memory_docs WHERE namespace = ?1 ORDER BY updated_at DESC",
                )
                .map_err(|e| format!("prepare list_documents: {e}"))?;
            let mut rows = stmt
                .query(params![Self::sanitize_namespace(ns)])
                .map_err(|e| format!("query list_documents: {e}"))?;
            while let Some(row) = rows
                .next()
                .map_err(|e| format!("row list_documents: {e}"))?
            {
                docs.push(json!({
                    "documentId": row.get::<_, String>(0).map_err(|e| e.to_string())?,
                    "namespace": row.get::<_, String>(1).map_err(|e| e.to_string())?,
                    "key": row.get::<_, String>(2).map_err(|e| e.to_string())?,
                    "title": row.get::<_, String>(3).map_err(|e| e.to_string())?,
                    "sourceType": row.get::<_, String>(4).map_err(|e| e.to_string())?,
                    "priority": row.get::<_, String>(5).map_err(|e| e.to_string())?,
                    "createdAt": row.get::<_, f64>(6).map_err(|e| e.to_string())?,
                    "updatedAt": row.get::<_, f64>(7).map_err(|e| e.to_string())?,
                    "taint": row.get::<_, String>(8).map_err(|e| e.to_string())?,
                }));
            }
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT document_id, namespace, key, title, source_type, priority, created_at, updated_at, taint
                     FROM memory_docs ORDER BY updated_at DESC",
                )
                .map_err(|e| format!("prepare list_documents: {e}"))?;
            let mut rows = stmt
                .query([])
                .map_err(|e| format!("query list_documents: {e}"))?;
            while let Some(row) = rows
                .next()
                .map_err(|e| format!("row list_documents: {e}"))?
            {
                docs.push(json!({
                    "documentId": row.get::<_, String>(0).map_err(|e| e.to_string())?,
                    "namespace": row.get::<_, String>(1).map_err(|e| e.to_string())?,
                    "key": row.get::<_, String>(2).map_err(|e| e.to_string())?,
                    "title": row.get::<_, String>(3).map_err(|e| e.to_string())?,
                    "sourceType": row.get::<_, String>(4).map_err(|e| e.to_string())?,
                    "priority": row.get::<_, String>(5).map_err(|e| e.to_string())?,
                    "createdAt": row.get::<_, f64>(6).map_err(|e| e.to_string())?,
                    "updatedAt": row.get::<_, f64>(7).map_err(|e| e.to_string())?,
                    "taint": row.get::<_, String>(8).map_err(|e| e.to_string())?,
                }));
            }
        }
        Ok(json!({ "documents": docs, "count": docs.len() }))
    }

    /// Return every distinct namespace that has at least one document.
    pub async fn list_namespaces(&self) -> Result<Vec<String>, String> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT DISTINCT namespace FROM memory_docs ORDER BY namespace")
            .map_err(|e| format!("prepare list_namespaces: {e}"))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("query list_namespaces: {e}"))?;
        let mut out = BTreeSet::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| format!("row list_namespaces: {e}"))?
        {
            let ns: String = row.get(0).map_err(|e| e.to_string())?;
            if !ns.trim().is_empty() {
                out.insert(ns);
            }
        }
        Ok(out.into_iter().collect())
    }

    /// Delete all documents, vector chunks, KV entries, and graph relations
    /// for the given namespace in a single transaction. Also removes the
    /// on-disk markdown directory (`namespaces/{ns}/docs/`).
    ///
    /// Scoped by the physical `namespace` column only, exactly as before
    /// `logical_namespace` existed: `sanitize_namespace` has always collapsed
    /// two differently-delimited names onto one physical address (`a:b_c` and
    /// `a_b:c` both sanitize to `a_b_c`), and every operation on this store —
    /// reads, writes, and this clear — has always treated that as one
    /// namespace. This call is no exception; isolating aliasing logical
    /// namespaces from each other is out of scope here (it would need
    /// `logical_namespace` columns, and matching write-path support, on
    /// `vector_chunks`, `kv_namespace`, and `graph_namespace` too, not just
    /// `memory_docs`).
    pub async fn clear_namespace(&self, namespace: &str) -> Result<(), String> {
        let ns = Self::sanitize_namespace(namespace);
        log::debug!("[memory] clear_namespace: starting for namespace={ns}");

        {
            let conn = self.conn.lock();
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| format!("clear_namespace begin tx: {e}"))?;

            let doc_count = tx
                .execute(
                    "DELETE FROM memory_docs WHERE namespace = ?1",
                    rusqlite::params![ns],
                )
                .map_err(|e| format!("clear_namespace delete memory_docs: {e}"))?;
            log::debug!("[memory] clear_namespace: deleted {doc_count} rows from memory_docs");

            let chunk_count = tx
                .execute(
                    "DELETE FROM vector_chunks WHERE namespace = ?1",
                    rusqlite::params![ns],
                )
                .map_err(|e| format!("clear_namespace delete vector_chunks: {e}"))?;
            log::debug!("[memory] clear_namespace: deleted {chunk_count} rows from vector_chunks");

            let kv_count = tx
                .execute(
                    "DELETE FROM kv_namespace WHERE namespace = ?1",
                    rusqlite::params![ns],
                )
                .map_err(|e| format!("clear_namespace delete kv_namespace: {e}"))?;
            log::debug!("[memory] clear_namespace: deleted {kv_count} rows from kv_namespace");

            let graph_count = tx
                .execute(
                    "DELETE FROM graph_namespace WHERE namespace = ?1",
                    rusqlite::params![ns],
                )
                .map_err(|e| format!("clear_namespace delete graph_namespace: {e}"))?;
            log::debug!(
                "[memory] clear_namespace: deleted {graph_count} rows from graph_namespace"
            );

            tx.commit()
                .map_err(|e| format!("clear_namespace commit tx: {e}"))?;
        }

        // Remove on-disk markdown files for this namespace.
        let docs_dir = self.namespace_dir(&ns).join("docs");
        if docs_dir.exists() {
            tokio::fs::remove_dir_all(&docs_dir).await.map_err(|e| {
                format!(
                    "clear_namespace remove docs dir {}: {e}",
                    docs_dir.display()
                )
            })?;
            log::debug!(
                "[memory] clear_namespace: removed docs directory {}",
                docs_dir.display()
            );
        }

        log::debug!("[memory] clear_namespace: completed for namespace={ns}");
        Ok(())
    }

    /// Delete a single document plus its vector chunks, graph relations, and
    /// markdown sidecar. Returns `{ "deleted": bool, "namespace", "documentId" }`.
    pub async fn delete_document(
        &self,
        namespace: &str,
        document_id: &str,
    ) -> Result<Value, String> {
        let ns = Self::sanitize_namespace(namespace);
        let rel_path: Option<String> = {
            let conn = self.conn.lock();
            conn.query_row(
                "SELECT markdown_rel_path FROM memory_docs WHERE namespace = ?1 AND document_id = ?2",
                params![ns, document_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("query delete_document path: {e}"))?
        };

        self.graph_remove_document_namespace(&ns, document_id)
            .await?;

        let deleted = {
            let conn = self.conn.lock();
            let deleted = conn
                .execute(
                    "DELETE FROM memory_docs WHERE namespace = ?1 AND document_id = ?2",
                    params![ns, document_id],
                )
                .map_err(|e| format!("delete memory_doc: {e}"))?
                > 0;
            conn.execute(
                "DELETE FROM vector_chunks WHERE namespace = ?1 AND document_id = ?2",
                params![ns, document_id],
            )
            .map_err(|e| format!("delete vector_chunks: {e}"))?;
            deleted
        };

        if let Some(rel) = rel_path {
            let abs = self.workspace_dir.join(rel);
            // Surface non-NotFound failures so storage drift between the DB
            // row and the markdown sidecar is diagnosable.
            if let Err(e) = tokio::fs::remove_file(&abs).await {
                if e.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("[memory] failed to remove sidecar {}: {e}", abs.display());
                }
            }
        }
        Ok(json!({"deleted": deleted, "namespace": ns, "documentId": document_id }))
    }

    /// The write lock for one `(database, namespace, key)`.
    ///
    /// Process-global rather than per-instance: the same store file can be
    /// opened by more than one `UnifiedMemory`, and a lock living on the
    /// instance would not serialise those. Keyed by db path so two workspaces
    /// never contend.
    ///
    /// The table only grows, bounded by the number of distinct keys this
    /// process has written -- one `Arc` and an unlocked mutex each.
    fn document_write_lock(
        db_path: &std::path::Path,
        namespace: &str,
        key: &str,
    ) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex, OnceLock};
        type Table = Mutex<HashMap<(String, String, String), Arc<tokio::sync::Mutex<()>>>>;
        static LOCKS: OnceLock<Table> = OnceLock::new();
        let table = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
        let id = (
            db_path.to_string_lossy().into_owned(),
            namespace.to_owned(),
            key.to_owned(),
        );
        // Recover from a poisoned table: it holds `Arc`s only, so a panicking
        // writer leaves nothing torn, and refusing every later write would be
        // a worse failure than continuing.
        let mut table = table
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(table.entry(id).or_default())
    }

    /// Resolve the row a write of `(namespace, key)` addresses; see
    /// [`DocumentIdentity`] for the cases. `requested_id` is the caller's
    /// `document_id`; it is trimmed, and a blank one is no request.
    fn resolve_document_identity(
        conn: &rusqlite::Connection,
        namespace: &str,
        key: &str,
        requested_id: Option<&str>,
    ) -> Result<DocumentIdentity, String> {
        let requested_id = requested_id.map(str::trim).filter(|id| !id.is_empty());
        let by_key = conn
            .query_row(
                "SELECT document_id, created_at FROM memory_docs WHERE namespace = ?1 AND key = ?2 LIMIT 1",
                params![namespace, key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
            )
            .optional()
            .map_err(|e| format!("lookup existing document: {e}"))?;
        if let Some((document_id, created_at)) = by_key {
            if requested_id.is_some_and(|requested| requested != document_id) {
                log::debug!(
                    "[memory] document write keeps the row's id over the requested one namespace={namespace}"
                );
            }
            return Ok(DocumentIdentity::Existing {
                document_id,
                created_at,
            });
        }
        let Some(requested_id) = requested_id else {
            return Ok(DocumentIdentity::New { document_id: None });
        };
        let owner = conn
            .query_row(
                "SELECT namespace, created_at FROM memory_docs WHERE document_id = ?1 LIMIT 1",
                params![requested_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
            )
            .optional()
            .map_err(|e| format!("lookup document id owner: {e}"))?;
        Ok(match owner {
            None => DocumentIdentity::New {
                document_id: Some(requested_id.to_owned()),
            },
            Some((owner, created_at)) if owner == namespace => DocumentIdentity::StaleKey {
                document_id: requested_id.to_owned(),
                created_at,
            },
            Some((owner, _)) => {
                log::warn!(
                    "[memory] requested document id belongs to another namespace; storing under a derived id namespace={namespace} owner_namespace={owner}"
                );
                DocumentIdentity::New { document_id: None }
            }
        })
    }

    /// Move the row `document_id` of `namespace` to `key`, so the upsert that
    /// follows updates it through `ON CONFLICT(namespace, key)` instead of
    /// inserting a second row the primary key then rejects. Runs inside the
    /// caller's transaction. Chunks, the markdown sidecar and graph relations
    /// are keyed by the id and need no change.
    fn rekey_document(
        conn: &rusqlite::Connection,
        namespace: &str,
        document_id: &str,
        key: &str,
    ) -> Result<(), String> {
        let rekeyed = conn
            .execute(
                "UPDATE memory_docs SET key = ?1 WHERE namespace = ?2 AND document_id = ?3",
                params![key, namespace, document_id],
            )
            .map_err(|e| format!("re-key memory_docs: {e}"))?;
        log::info!(
            "[memory] re-keyed a document to the key its write addressed it by namespace={namespace} rows={rekeyed}"
        );
        Ok(())
    }

    /// A document id derived from `(namespace, key)`.
    ///
    /// Deterministic so two concurrent first-writes of one key agree, which is
    /// what keeps `vector_chunks` addressable from the row. Hashed rather than
    /// concatenated so the id is a fixed-width opaque token whatever the
    /// namespace or key contains; the zero byte is a domain separator, so
    /// ("a","bc") and ("ab","c") cannot collide.
    pub(crate) fn derive_document_id(namespace: &str, key: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(namespace.as_bytes());
        hasher.update([0u8]);
        hasher.update(key.as_bytes());
        // sha2 0.11 returns a hybrid-array digest with no `LowerHex`, so hex is
        // folded byte-by-byte; the id keeps the first 32 hex chars (16 bytes).
        let hex = {
            use std::fmt::Write;
            hasher
                .finalize()
                .iter()
                .fold(String::with_capacity(64), |mut acc, b| {
                    let _ = write!(acc, "{b:02x}");
                    acc
                })
        };
        hex[..32].to_string()
    }
}

#[cfg(test)]
#[path = "documents_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "documents_document_id_tests.rs"]
mod document_id_tests;

#[cfg(test)]
#[path = "documents_identity_tests.rs"]
mod identity_tests;
