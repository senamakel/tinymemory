//! Behavioural conformance for `MemoryProvider` drivers.
//!
//! TinyMemory's premise is that an engine can be swapped without the host
//! learning anything new. [`audit_provider`](tinymemory_api::provider::audit_provider)
//! checks that a driver's advertised capabilities match its reachable
//! accessors, which proves the *shape* is honest. Nothing checked that two
//! drivers answer the same question the same way — and that is the claim the
//! premise actually rests on.
//!
//! This crate is that check. Hand [`assert_provider`] any bound driver and it
//! drives the contract: the mandatory three families, upsert semantics on
//! `(namespace, key)`, namespace isolation, provenance preservation, recall
//! limits, export pagination, and import round-tripping.
//!
//! ```no_run
//! use std::sync::Arc;
//! use tinymemory_conformance::{assert_provider, InMemoryProvider};
//!
//! # async fn run() {
//! assert_provider(Arc::new(InMemoryProvider::new())).await;
//! # }
//! ```
//!
//! # What it deliberately does not depend on
//!
//! Only `tinymemory-api`. A conformance suite that pulled in an engine could
//! not prove interchangeability, because it would already have chosen one — and
//! reaching `tinymemory-core` would drag in a bundled SQLite and the embedded
//! engine besides (issue #18 §D).
//!
//! # Provenance is the sharp one
//!
//! [`assert_taint_is_preserved`] is not a formality. A driver that reads back
//! `Internal` for content stored as `ExternalSync` has laundered external
//! content into internal-trust content, and every policy gate keyed on taint is
//! then silently wrong. That failure is invisible until something acts on it.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod reference;
pub mod suite;

pub use reference::full::{Call, RecordingProvider, FULL_DRIVER_ID};
pub use reference::{InMemoryProvider, REFERENCE_DRIVER_ID};
pub use suite::{
    assert_awkward_content_round_trips, assert_capability_audit, assert_export_cursor_terminates,
    assert_export_import_round_trip, assert_forget_is_idempotent, assert_kv_round_trip,
    assert_list_filters_narrow, assert_namespaces_are_isolated,
    assert_namespaces_preserve_their_section, assert_provider,
    assert_recall_respects_limit_and_namespace, assert_store_get_round_trip,
    assert_taint_is_preserved, assert_upsert_replaces_rather_than_duplicates,
};
pub use suite::{
    // Exported alongside the assertions because a caller standing up its own
    // backend double needs it: `assert_provider` skips every write-path
    // assertion when the driver does not retain, so a double that silently
    // dropped writes would let a whole run pass vacuously. Probing for that
    // directly is how a caller proves its harness is real.
    retains_writes,
};
