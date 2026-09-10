//! OpenHuman chat-provider adapter for tinycortex summary preparation.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::chat::{build_chat_provider, ChatPrompt};
use crate::Config;

/// Provider calls one `summarise` will make before giving up (oh#6187).
///
/// A recap that gives up on the first transient costs the segment its summary
/// permanently: the caller writes nothing on failure, and nothing re-runs the
/// recap on its own.
const MAX_ATTEMPTS: u32 = 3;

/// Backoff before retry `n` is `RETRY_BASE_BACKOFF * 2^(n-1)`.
///
/// Same shape as `tinymemory-remote`'s read retry, for the same reason: two
/// summarisers that fail against the same unreachable provider should not come
/// back in lockstep.
const RETRY_BASE_BACKOFF: Duration = Duration::from_millis(250);

/// Ceiling on the wall time one `summarise` may spend in total, provider calls
/// and backoff together.
///
/// An attempt count alone does not bound this: the caller
/// (`flush_open_segment` in the host archivist) is awaited **unbounded** at
/// session wind-down, so three slow attempts would land directly on the time
/// it takes the app to close. The deadline is checked before committing to a
/// retry, never mid-call — it shortens the retry chain, it does not cancel an
/// attempt already in flight.
const MAX_TOTAL_ELAPSED: Duration = Duration::from_secs(20);

/// Whether a failed provider call is worth another attempt.
///
/// Deliberately narrow. Two classes qualify:
///
/// - the request never reached the model — a connect or request-phase failure,
///   which is exactly the `error sending request` in oh#6156;
/// - the model answered that it cannot serve right now — `429`, and the
///   gateway trio `502`/`503`/`504`.
///
/// Everything else returns `false`, including plain **timeouts**. A timeout is
/// ambiguous about whether the model ran: `reqwest` cannot separate a connect
/// timeout from a read timeout on a response that was generated and billed, so
/// retrying one risks paying twice for work the caller never sees. Auth
/// failures, unknown models and exhausted quota are terminal by nature and
/// retrying them only burns the budget before the attempt that could have
/// helped.
pub(super) fn retryable(error: &anyhow::Error) -> bool {
    // The provider call is in-process — the host's chat seam is a direct call,
    // not a bus hop — so the typed `reqwest::Error` is still in the chain here.
    // This is the only layer where that is true: `MemoryTree::summarise` is
    // reached through the module bus, which flattens the error to a string.
    for cause in error.chain() {
        if let Some(err) = cause.downcast_ref::<reqwest::Error>() {
            if err.is_connect() || err.is_request() {
                return true;
            }
            return matches!(
                err.status().map(|status| status.as_u16()),
                Some(429 | 502 | 503 | 504)
            );
        }
    }
    // Fallback for a host whose provider stack wrapped the transport failure
    // in its own error type, or built against a different `reqwest` (a
    // downcast across two semver-compatible copies still fails). Kept to
    // phrases that cannot describe anything but a connection that was never
    // established — the general fragility of matching on prose is why this is
    // the fallback and not the rule.
    const TRANSPORT_NEEDLES: &[&str] = &[
        "error sending request",
        "connection refused",
        "connection reset",
        "connection closed before message completed",
        "tcp connect error",
        "dns error",
    ];
    let message = format!("{error:#}").to_ascii_lowercase();
    TRANSPORT_NEEDLES
        .iter()
        .any(|needle| message.contains(needle))
}

pub use crate::engine::backend::tree::{SummaryContext, SummaryInput};

/// Compatibility result carrying provider usage alongside the crate-owned
/// summary output fields.
#[derive(Clone, Debug, Default)]
pub struct SummaryOutput {
    pub content: String,
    pub token_count: u32,
    pub entities: Vec<String>,
    pub topics: Vec<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub charged_amount_usd: Option<f64>,
}

pub async fn summarise(
    config: &Config,
    inputs: &[SummaryInput],
    context: &SummaryContext<'_>,
) -> Result<SummaryOutput> {
    let Some(prepared) = crate::engine::backend::tree::prepare_summary_prompt(
        inputs,
        context,
        config.output_language(),
    ) else {
        return Ok(SummaryOutput::default());
    };
    let provider =
        build_chat_provider(config).context("memory_tree::summarise: build chat provider")?;
    log::debug!(
        "[memory_tree::summarise] provider={} level={} inputs={} budget={}",
        provider.name(),
        context.target_level,
        inputs.len(),
        prepared.effective_budget
    );
    // Retried, because a caller cannot recover from a transient here: this
    // function never substitutes a fallback (that is the caller's, by
    // contract), and the host archivist writes nothing at all when it fails
    // (oh#6156) with nothing scheduled to come back for the segment. One
    // dropped connection would otherwise cost that stretch of history its
    // summary for good. See `retryable` for what does and does not qualify.
    let started = Instant::now();
    let mut attempt = 0_u32;
    let (text, usage) = loop {
        attempt += 1;
        let outcome = provider
            .chat_for_text_with_usage(&ChatPrompt {
                system: prepared.system.clone(),
                user: prepared.user.clone(),
                temperature: 0.0,
                kind: "memory_tree::summarise",
                max_tokens: None,
            })
            .await;
        match outcome {
            Ok(value) => break value,
            Err(error) => {
                let backoff = RETRY_BASE_BACKOFF * 2_u32.pow(attempt - 1);
                let exhausted = attempt >= MAX_ATTEMPTS;
                let out_of_time = started.elapsed() + backoff >= MAX_TOTAL_ELAPSED;
                if exhausted || out_of_time || !retryable(&error) {
                    // The attempt count rides the context so the host's WARN
                    // says whether this was a single terminal failure or a
                    // transient that outlasted the whole budget.
                    return Err(error).with_context(|| {
                        format!(
                            "memory_tree::summarise: provider={} after {attempt} attempt(s)",
                            provider.name()
                        )
                    });
                }
                log::debug!(
                    "[memory_tree::summarise] provider={} attempt {attempt}/{MAX_ATTEMPTS} \
                     failed, retrying in {backoff:?}: {error:#}",
                    provider.name()
                );
                tokio::time::sleep(backoff).await;
            }
        }
    };
    let output =
        crate::engine::backend::tree::finish_provider_summary(&text, prepared.effective_budget);
    let input_tokens = usage.as_ref().map_or(0, |usage| usage.input_tokens);
    let output_tokens = usage.as_ref().map_or(0, |usage| usage.output_tokens);
    let charged_amount_usd = usage
        .as_ref()
        .map(|usage| usage.charged_amount_usd)
        .filter(|amount| *amount > 0.0);
    log::debug!(
        "[memory_tree::summarise] complete tokens={} usage_input={} usage_output={}",
        output.token_count,
        input_tokens,
        output_tokens
    );
    Ok(SummaryOutput {
        content: output.content,
        token_count: output.token_count,
        entities: output.entities,
        topics: output.topics,
        input_tokens,
        output_tokens,
        charged_amount_usd,
    })
}

pub fn fallback_summary(inputs: &[SummaryInput], budget: u32) -> SummaryOutput {
    let output = crate::engine::backend::tree::fallback_summary(inputs, budget);
    SummaryOutput {
        content: output.content,
        token_count: output.token_count,
        entities: output.entities,
        topics: output.topics,
        ..SummaryOutput::default()
    }
}
