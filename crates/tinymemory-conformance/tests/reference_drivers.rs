//! The suite, run against the drivers this workspace ships as references.
//!
//! Two drivers, for two different reasons.
//!
//! `InMemoryProvider` is the calibration subject: its behaviour is obvious by
//! inspection, so a failure here means the *assertion* is wrong, not the
//! driver. Without it, a suite that only ever ran against real engines could
//! not tell those two cases apart.
//!
//! `NullMemoryProvider` is the opposite end — it accepts writes, discards them,
//! and reads back empty. Running the same assertions against it pins down which
//! parts of the contract a discard-everything driver must still uphold
//! (namespace isolation, an honest `forget`, a terminating export cursor,
//! errors that stay inside `MemoryError`) and which are vacuous for it.
//! A suite that could not run against `null` would be asserting storage rather
//! than the contract.

use std::sync::Arc;

use tinymemory_api::null::NullMemoryProvider;
use tinymemory_api::provider::MemoryProvider;
use tinymemory_conformance::{assert_provider, InMemoryProvider};

#[tokio::test]
async fn the_in_memory_reference_driver_conforms() {
    assert_provider(Arc::new(InMemoryProvider::new())).await;
}

#[tokio::test]
async fn the_null_driver_conforms() {
    assert_provider(Arc::new(NullMemoryProvider::new())).await;
}

#[tokio::test]
async fn the_reference_driver_advertises_exactly_the_mandatory_families() {
    let provider = InMemoryProvider::new();
    let caps = provider.capabilities();
    assert_eq!(
        caps.len(),
        3,
        "the reference driver must advertise only what it can serve, got {caps:?}"
    );
    // Every optional accessor stays `None`, which is what makes the audit pass.
    assert!(provider.as_tree().is_none());
    assert!(provider.as_graph().is_none());
    assert!(provider.as_ingest().is_none());
}

/// The full driver is the third subject, and it is the one a *host* binds.
///
/// `InMemoryProvider` proves the assertions are right; `NullMemoryProvider`
/// proves which of them survive a driver that retains nothing. Neither answers
/// the question this driver exists for: a host testing its own layer above the
/// contract needs every optional family reachable, because its handlers ask for
/// them by accessor and take the `None` arm as "unsupported" rather than as
/// "empty". Running the same suite here keeps that convenience honest — a
/// driver that serves 27 families still has to uphold the three mandatory ones.
#[tokio::test]
async fn the_full_driver_conforms() {
    assert_provider(Arc::new(tinymemory_conformance::RecordingProvider::new())).await;
}

/// It advertises everything, which is the opposite of the reference driver's
/// claim and has to stay that way for `audit_provider` to pass: a driver that
/// advertised less than it serves fails the audit just as surely as one that
/// advertises more.
#[tokio::test]
async fn the_full_driver_advertises_every_family() {
    let provider = tinymemory_conformance::RecordingProvider::new();
    assert!(provider.as_tree().is_some());
    assert!(provider.as_chunks().is_some());
    assert!(provider.as_documents().is_some());
    assert!(provider.as_retrieval().is_some());
}

/// The full driver must actually retain, and this has to be asserted directly.
///
/// `assert_provider` skips every storage assertion when `retains_writes` probes
/// false, because a driver that accepts writes and discards them is a
/// legitimate binding — `NullMemoryProvider` is exactly that. The consequence
/// is that a *double* which drops writes by accident passes the whole suite
/// vacuously, which is precisely what happened here: the driver landed with
/// `store` returning `Ok(())` and `get` returning `Ok(None)`, and
/// `the_full_driver_conforms` went green having asserted nothing about storage.
///
/// The crate exports `retains_writes` for callers to catch this in their own
/// harnesses. It is worth spending it on our own.
#[tokio::test]
async fn the_full_driver_retains_writes() {
    let provider = tinymemory_conformance::RecordingProvider::new();
    assert!(
        tinymemory_conformance::retains_writes(&provider).await,
        "the full driver dropped a write — assert_provider would then skip \
         every storage assertion and pass vacuously"
    );
}
