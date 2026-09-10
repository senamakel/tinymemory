//! Behaviour of the deterministic fallback summariser.
//!
//! `summarise` itself needs a chat provider and is covered where the provider
//! seam is. [`fallback_summary`] needs nothing — it is the answer when no model
//! is reachable, which makes it the path a degraded install actually runs, and
//! it had no test of its own on either side of the host boundary.

use chrono::{TimeZone, Utc};

use super::summarise::{fallback_summary, SummaryInput};

fn input(id: &str, content: &str, entities: &[&str], topics: &[&str], score: f32) -> SummaryInput {
    let at = Utc
        .with_ymd_and_hms(2026, 5, 29, 9, 8, 7)
        .single()
        .expect("a real instant");
    SummaryInput {
        id: id.to_string(),
        content: content.to_string(),
        token_count: 0,
        entities: entities.iter().map(|e| (*e).to_string()).collect(),
        topics: topics.iter().map(|t| (*t).to_string()).collect(),
        time_range_start: at,
        time_range_end: at,
        score,
    }
}

/// Blank inputs are dropped rather than summarised into empty bullets, and the
/// budget is honoured.
///
/// The blank input carries an entity and the surviving one carries a topic, so
/// this also pins that the fallback propagates neither. That is easy to get
/// wrong in the direction that matters: carrying a dropped input's entities
/// forward would attribute them to a summary whose text never mentions them.
#[test]
fn a_blank_input_is_dropped_and_the_budget_is_honoured() {
    let inputs = vec![
        input("blank", "   ", &["ignored"], &[], 0.1),
        input(
            "long",
            &"alpha beta gamma delta epsilon zeta eta theta".repeat(20),
            &[],
            &["planning"],
            0.9,
        ),
    ];

    let out = fallback_summary(&inputs, 8);

    assert!(
        out.content.starts_with("— alpha"),
        "the blank input was not dropped: {:?}",
        out.content
    );
    assert!(
        out.token_count <= 9,
        "a budget of 8 produced {} tokens",
        out.token_count
    );
    assert!(
        out.entities.is_empty(),
        "a dropped input's entities were carried into the summary: {:?}",
        out.entities
    );
    assert!(
        out.topics.is_empty(),
        "topics were carried into a summary whose text does not mention them: {:?}",
        out.topics
    );
}

/// With nothing to summarise the fallback answers empty rather than a bullet
/// with no content behind it.
#[test]
fn no_inputs_produce_no_summary() {
    let out = fallback_summary(&[], 64);
    assert!(out.content.is_empty(), "got {:?}", out.content);
    assert_eq!(out.token_count, 0);
}

// ── Retry policy (oh#6187) ───────────────────────────────────────────────────

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use crate::chat::{test_override, ChatPrompt, ChatProvider};
use crate::engine::backend::tree::TreeKind;
use crate::tree::summarise::{retryable, summarise, SummaryContext};

/// A provider that fails its first `fail_times` calls with `error`, then
/// answers `response`. Counts every call so a test can assert how many
/// attempts the retry policy actually spent.
struct FlakyChatProvider {
    fail_times: usize,
    error: String,
    response: String,
    calls: AtomicUsize,
}

impl FlakyChatProvider {
    fn new(fail_times: usize, error: &str, response: &str) -> Arc<Self> {
        Arc::new(Self {
            fail_times,
            error: error.to_string(),
            response: response.to_string(),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ChatProvider for FlakyChatProvider {
    fn name(&self) -> &str {
        "test:flaky"
    }

    async fn chat_for_json(&self, _prompt: &ChatPrompt) -> anyhow::Result<String> {
        let seen = self.calls.fetch_add(1, Ordering::SeqCst);
        if seen < self.fail_times {
            anyhow::bail!("{}", self.error);
        }
        Ok(self.response.clone())
    }
}

fn context() -> SummaryContext<'static> {
    SummaryContext {
        tree_id: "seg-test",
        tree_kind: TreeKind::Source,
        target_level: 0,
        token_budget: 200,
        input_token_budget: 4_000,
        overhead_reserve_tokens: 400,
        ask: None,
    }
}

fn corpus() -> Vec<SummaryInput> {
    vec![
        input("a", "we agreed to ship the retry on friday", &[], &[], 0.9),
        input("b", "and to leave the timeout case alone", &[], &[], 0.9),
    ]
}

/// A dropped connection is retried, and a later attempt's answer is the one
/// that comes back.
///
/// This is the shape in the field report (oh#6156): the provider was briefly
/// unreachable, and one attempt was all the recap got. The assertion on
/// `calls` is the point — asserting only on the content would pass against
/// the old single-shot code the moment the first call happened to succeed.
#[tokio::test]
async fn a_transport_failure_is_retried_and_a_later_attempt_wins() {
    crate::test_seams::init();
    let provider = FlakyChatProvider::new(2, "error sending request", "the real recap");
    let config = tinymemory_api::host::test_support::TestHostConfig::default();

    let out = test_override::with_provider(provider.clone(), async {
        summarise(&config, &corpus(), &context()).await
    })
    .await
    .expect("the third attempt answers");

    assert_eq!(
        provider.calls(),
        3,
        "a transport failure must be retried, not surfaced on the first attempt"
    );
    assert!(
        out.content.contains("the real recap"),
        "the model's answer was not the one returned: {:?}",
        out.content
    );
}

/// A terminal failure consumes exactly one attempt.
///
/// Retrying an auth or quota failure cannot change the answer; it only spends
/// the budget before the attempt that could have helped, and delays the
/// caller's decision to leave the segment unsummarised.
#[tokio::test]
async fn a_terminal_failure_is_not_retried() {
    crate::test_seams::init();
    let provider = FlakyChatProvider::new(
        usize::MAX,
        "401 Unauthorized: invalid api key",
        "unreachable",
    );
    let config = tinymemory_api::host::test_support::TestHostConfig::default();

    let error = test_override::with_provider(provider.clone(), async {
        summarise(&config, &corpus(), &context()).await
    })
    .await
    .expect_err("an auth failure is still a failure");

    assert_eq!(
        provider.calls(),
        1,
        "a terminal failure was retried: {error:#}"
    );
    assert!(
        format!("{error:#}").contains("after 1 attempt(s)"),
        "the attempt count is missing from the error: {error:#}"
    );
}

/// The retry budget is bounded, and the error says how much of it was spent.
///
/// The count in the context is what lets the host's WARN distinguish "the
/// provider is configured wrong" from "the provider was down for longer than
/// we were willing to wait" — the two have different fixes and the log line is
/// the only place a reader sees either.
#[tokio::test]
async fn a_persistent_transport_failure_stops_at_the_attempt_budget() {
    crate::test_seams::init();
    let provider = FlakyChatProvider::new(usize::MAX, "error sending request", "unreachable");
    let config = tinymemory_api::host::test_support::TestHostConfig::default();

    let error = test_override::with_provider(provider.clone(), async {
        summarise(&config, &corpus(), &context()).await
    })
    .await
    .expect_err("a provider that never answers must still fail");

    assert_eq!(
        provider.calls(),
        3,
        "the attempt budget was not honoured: {error:#}"
    );
    assert!(
        format!("{error:#}").contains("after 3 attempt(s)"),
        "the attempt count is missing from the error: {error:#}"
    );
}

/// Nothing to fold short-circuits before the provider is built, so the retry
/// budget is never spent on an empty corpus.
#[tokio::test]
async fn an_empty_corpus_never_reaches_the_provider() {
    crate::test_seams::init();
    let provider = FlakyChatProvider::new(usize::MAX, "error sending request", "unreachable");
    let config = tinymemory_api::host::test_support::TestHostConfig::default();

    let out = test_override::with_provider(provider.clone(), async {
        summarise(&config, &[], &context()).await
    })
    .await
    .expect("nothing to fold is not an error");

    assert_eq!(provider.calls(), 0, "an empty fold called the provider");
    assert!(out.content.is_empty(), "content: {:?}", out.content);
}

/// A real `reqwest` connect failure classifies as retryable through the
/// downcast, not through the prose fallback.
///
/// Port 1 on loopback refuses immediately, so this needs no network and no
/// fixture server. It exists because the downcast is the arm that matters in
/// production and the prose list is only a backstop — a test that exercised
/// the needles alone would pass with the downcast arm deleted.
#[tokio::test]
async fn a_real_connect_error_is_classified_by_type() {
    let transport = reqwest::Client::new()
        .get("http://127.0.0.1:1/")
        .send()
        .await
        .expect_err("nothing listens on port 1");
    let error = anyhow::Error::new(transport).context("memory_tree::summarise: provider=test");

    assert!(
        retryable(&error),
        "a connect failure must be retryable: {error:#}"
    );
}

/// The classifier's truth table, on the prose fallback.
#[test]
fn retryable_only_accepts_transport_shaped_failures() {
    for text in [
        "error sending request",
        "tcp connect error: Connection refused",
        "DNS error: failed to lookup address",
        "connection reset by peer",
    ] {
        assert!(
            retryable(&anyhow::anyhow!("{text}")),
            "should retry: {text}"
        );
    }
    for text in [
        "401 Unauthorized",
        "429 quota exhausted for this month",
        "unknown model 'summarization-v1'",
        "operation timed out",
        "context window exceeded",
    ] {
        assert!(
            !retryable(&anyhow::anyhow!("{text}")),
            "should not retry: {text}"
        );
    }
}
