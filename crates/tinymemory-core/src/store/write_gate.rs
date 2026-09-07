//! Host write policy for memory documents — the secret/PII gate that runs
//! **before** the storage driver sees a document.
//!
//! # Why this module exists
//!
//! Redaction is host product policy, not persistence. Until this module
//! existed, `UnifiedMemory::upsert_document` ran the whole gate *inside* the
//! driver call: a caller handed it raw content and the SQL layer decided what
//! to scrub. `UnifiedMemory` is a candidate to move into the `tinycortex`
//! crate, and a persistence crate that owns "which substrings of a user's
//! document are secrets" is a policy decision shipped somewhere it cannot be
//! revisited per host.
//!
//! So the gate lives here and the driver methods
//! ([`UnifiedMemory::upsert_document_presanitized`] and
//! [`UnifiedMemory::upsert_document_metadata_only_presanitized`]) now take
//! already-sanitized input. This module re-declares `upsert_document` /
//! `upsert_document_metadata_only` as inherent methods on `UnifiedMemory` with
//! the **same names and signatures they always had**, so every existing caller
//! is routed through the gate without a single call-site edit — which is also
//! what makes the "no bypass" claim below checkable rather than hopeful. The
//! batch form, `upsert_documents`, is declared here for the same reason.
//!
//! # The gate, in order
//!
//! The three steps are one ordered policy unit and were hoisted together;
//! running the redactor over a key that has not been canonicalized first would
//! scrub a different string than the one the row is addressed by.
//!
//! 1. **Reject** a namespace or key that looks like a secret. The identifier is
//!    the row's address and is echoed in logs, so a credential there is not
//!    something to redact-and-continue.
//! 2. **Canonicalize** a PII-bearing key rather than rejecting the write
//!    (#5164): rejection returned `Err` on every attempt and callers retry, so
//!    one such key produced an unthrottled error loop (3,055 Sentry events from
//!    a single user). `safety::canonical_document_key` is strict-gated so
//!    scanner-built identifiers (WhatsApp JIDs, `+1…` chat ids, timestamps)
//!    keep their identity, and the by-key read paths (`Memory::get` /
//!    `Memory::forget`) canonicalize through the same helper, so a rewritten
//!    identifier stays addressable instead of reading back as a missing row.
//! 3. **Redact** secret/PII content out of every field via
//!    `safety::sanitize_document_input`.
//!
//! Provenance `taint` is deliberately untouched by all three — sanitization is
//! content cleaning, and the taint is the signal the subconscious gate reads.
//!
//! # No bypass
//!
//! `upsert_document_presanitized` / `upsert_documents_presanitized` /
//! `upsert_document_metadata_only_presanitized` are `pub(crate)` and newly
//! named, and this module holds their only call sites outside `documents.rs`
//! itself (where the one-document method is the one-element case of the batch
//! one) — verify with:
//!
//! ```text
//! rg 'upsert_documents?(_metadata_only)?_presanitized' src/
//! ```
//!
//! Every other writer in the tree (`Memory::store_with_taint`, `MemoryClient`,
//! the ingestion queue, the RPC handlers, tests) calls the unsuffixed names and
//! is therefore gated. `write_gate_tests.rs` pins both halves: the gate
//! redacts, and the raw driver method does not.

use crate::store::safety;
use crate::store::types::NamespaceDocumentInput;

use super::namespace_store::UnifiedMemory;

/// Outcome of running the host write gate over a caller-supplied document.
enum GateOutcome {
    /// The (possibly rewritten) input the driver may persist.
    Admit(Box<NamespaceDocumentInput>),
    /// The write is refused; the string is the caller-facing error.
    Reject(String),
}

/// Run the secret/PII gate over `input`.
///
/// `flow` is a short grep tag naming the write path (`"document"` /
/// `"metadata-only"`) so the two callers' log lines stay distinguishable.
fn gate(input: NamespaceDocumentInput, flow: &str) -> GateOutcome {
    // 1. Reject a secret-like address outright.
    if safety::has_likely_secret(&input.namespace) || safety::has_likely_secret(&input.key) {
        log::warn!(
            "[memory:write_gate] {flow} write rejected due to secret-like namespace/key \
             namespace_chars={} key_chars={}",
            input.namespace.chars().count(),
            input.key.chars().count()
        );
        return GateOutcome::Reject("document namespace/key cannot contain secrets".to_string());
    }

    // 2. Canonicalize a PII-bearing key rather than rejecting the write (#5164).
    let input = {
        let key = safety::canonical_document_key(&input.key);
        if key != input.key {
            log::info!(
                "[memory:write_gate] {flow} write canonicalized PII-like key key_chars={}",
                input.key.chars().count()
            );
        }
        NamespaceDocumentInput { key, ..input }
    };

    // 3. Redact secret/PII content out of every field.
    let sanitized = safety::sanitize_document_input(input);
    let input = sanitized.value;
    if sanitized.report.changed() {
        log::warn!(
            "[memory:write_gate] {flow} write sanitized namespace_chars={} key_chars={} \
             text_redactions={} key_redactions={} blocked_secret_hits={} depth_redactions={} \
             pii_redactions={}",
            input.namespace.chars().count(),
            input.key.chars().count(),
            sanitized.report.text_redactions,
            sanitized.report.key_redactions,
            sanitized.report.blocked_secret_hits,
            sanitized.report.depth_redactions,
            sanitized.report.pii_redactions
        );
    } else {
        log::trace!("[memory:write_gate] {flow} write passed the gate unchanged");
    }

    GateOutcome::Admit(Box::new(input))
}

impl UnifiedMemory {
    /// Insert or update a document by `(namespace, key)`, applying the host
    /// secret/PII write gate first.
    ///
    /// This is the entry point every writer should use. It runs the gate
    /// documented at the module level and then delegates to
    /// `Self::upsert_document_presanitized`, which does the persistence
    /// (markdown sidecar, `memory_docs` upsert, chunking, embedding).
    ///
    /// # Errors
    ///
    /// Returns `Err` when the namespace or key looks like a credential, when
    /// the key is empty, or on any storage/embedding failure.
    pub async fn upsert_document(&self, input: NamespaceDocumentInput) -> Result<String, String> {
        match gate(input, "document") {
            GateOutcome::Reject(err) => Err(err),
            GateOutcome::Admit(input) => self.upsert_document_presanitized(*input).await,
        }
    }

    /// Insert or update many documents, applying the host secret/PII write
    /// gate to each and embedding their chunks together — one provider request
    /// per bounded group of chunk texts across the batch rather than one per
    /// document (tinymemory#138).
    ///
    /// Inputs are gated in order and the admitted prefix is written by
    /// `Self::upsert_documents_presanitized`, which stops at the first write
    /// failure; the first gate rejection, if any, ends the batch the same way.
    /// The result holds one entry per document attempted, in input order, so a
    /// failure is always the last entry and every document before it was
    /// written.
    ///
    /// # Errors
    ///
    /// Per document, the same failure modes as [`Self::upsert_document`].
    pub async fn upsert_documents(
        &self,
        inputs: Vec<NamespaceDocumentInput>,
    ) -> Vec<Result<String, String>> {
        let mut admitted = Vec::with_capacity(inputs.len());
        let mut rejection = None;
        for input in inputs {
            match gate(input, "document") {
                GateOutcome::Admit(input) => admitted.push(*input),
                GateOutcome::Reject(err) => {
                    rejection = Some(err);
                    break;
                }
            }
        }
        let mut results = self.upsert_documents_presanitized(admitted).await;
        if let Some(err) = rejection {
            // Only when the admitted prefix was written in full: a write failure
            // in it is already the batch's last entry, and everything after a
            // failure — the rejected document included — stays unattempted.
            if results.iter().all(Result::is_ok) {
                results.push(Err(err));
            }
        }
        results
    }

    /// Store a document without chunking, embedding, or graph extraction,
    /// applying the host secret/PII write gate first.
    ///
    /// Same gate as [`Self::upsert_document`]; suitable for high-frequency,
    /// low-value writes (e.g. transient sync checkpoints) where the full
    /// ingestion pipeline would be too expensive.
    ///
    /// # Errors
    ///
    /// Same failure modes as [`Self::upsert_document`].
    pub async fn upsert_document_metadata_only(
        &self,
        input: NamespaceDocumentInput,
    ) -> Result<String, String> {
        match gate(input, "metadata-only") {
            GateOutcome::Reject(err) => Err(err),
            GateOutcome::Admit(input) => {
                self.upsert_document_metadata_only_presanitized(*input)
                    .await
            }
        }
    }
}

#[cfg(test)]
#[path = "write_gate_tests.rs"]
mod tests;
