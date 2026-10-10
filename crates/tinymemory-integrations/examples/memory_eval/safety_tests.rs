//! The reference engine must pass the synthetic safety audit.

use super::*;
use tinymemory_api::conformance::ReferenceEngine;

#[tokio::test]
async fn sibling_memory_stays_isolated_and_deletions_leave_no_public_trace() {
    let engine = Arc::new(ReferenceEngine::new());
    let report = run(engine, None, 251, false, false).await.unwrap();
    assert_eq!(report.violations(), 0, "{report:?}");
    assert_eq!(report.checks.len(), 13);
}
