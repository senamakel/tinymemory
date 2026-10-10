//! Scoring a pack against a probe, and summing the scores up.
//!
//! A pack is cut into **units**: each bullet, and each prose paragraph (an
//! answered section), in the order the model reads them. Every check is a
//! case-insensitive substring match:
//!
//! - **hit**: every `expect` string is somewhere in the pack.
//! - **rank**: the 1-based unit holding the first `expect` string. Its
//!   reciprocal averages to the MRR.
//! - **stale first**: a superseded value comes in an earlier unit than the
//!   current one, or the current one is missing. That is the error a reader
//!   is most likely to repeat. **Fresh first** is its complement, counted
//!   over the probes that name superseded values.
//! - **leak**: a `forbidden` string is in the pack.
//! - **answer**: the scripted agent's extractive answer (see `agent`) holds
//!   every `expect` string and no stale one. With `--llm`, a model's answer
//!   from the same pack is graded the same way.

use serde::Serialize;

use crate::agent::answer;
use crate::scenarios::{Probe, Style, Via};

/// One probe's outcome in one phase.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProbeResult {
    pub(crate) scenario: &'static str,
    pub(crate) phase: &'static str,
    pub(crate) id: &'static str,
    pub(crate) via: &'static str,
    pub(crate) style: &'static str,
    /// Whether the host deadline expired before this probe received a pack.
    pub(crate) timed_out: bool,
    /// `None` when the probe expects nothing (a pure leak check).
    pub(crate) hit: Option<bool>,
    pub(crate) rank: Option<usize>,
    /// The heading of the section holding the first expected string.
    pub(crate) section: Option<String>,
    /// Whether the probe names superseded values.
    pub(crate) contradiction: bool,
    pub(crate) stale_present: bool,
    pub(crate) stale_first: bool,
    /// Whether the probe names forbidden strings.
    pub(crate) leak_checked: bool,
    pub(crate) leak: bool,
    pub(crate) answer: Option<String>,
    pub(crate) answer_ok: Option<bool>,
    /// The `--llm` model's answer from the same pack.
    pub(crate) llm_answer: Option<String>,
    pub(crate) llm_ok: Option<bool>,
    /// What the `--llm` answer took: tokens, and dollars where priced.
    pub(crate) llm_tokens: u64,
    pub(crate) llm_cost_usd: Option<f64>,
    /// Synthesis phase on CortexDB: whether a fact or belief CortexDB
    /// derived holds the expected answer, whether or not the pack shows it.
    pub(crate) captured: Option<bool>,
    pub(crate) ms: f64,
    pub(crate) tokens: usize,
    pub(crate) units: usize,
    pub(crate) markdown: String,
}

/// Which lifecycle call a probe used.
pub(crate) fn via_name(via: &Via) -> &'static str {
    match via {
        Via::Ask => "pre_turn",
        Via::Resume { .. } => "start_session",
        Via::Compact { .. } => "recall_for_compaction",
        Via::Continue { .. } => "pre_turn (in thread)",
        Via::ContextDoc { .. } => "context.md",
    }
}

/// The units of `markdown`, each with its section heading.
fn units(markdown: &str) -> Vec<(String, String)> {
    let mut heading = String::new();
    let mut out = Vec::new();
    let mut in_frontmatter = false;
    for line in markdown.lines() {
        let line = line.trim();
        if line == "---" {
            in_frontmatter = !in_frontmatter;
            continue;
        }
        if in_frontmatter || line.is_empty() || line.starts_with("# ") {
            continue;
        }
        if let Some(title) = line.strip_prefix("## ") {
            heading = title.to_string();
            continue;
        }
        let body = line.strip_prefix("- ").unwrap_or(line);
        out.push((heading.clone(), body.to_lowercase()));
    }
    out
}

/// Scores one pack.
pub(crate) fn score(
    scenario: &'static str,
    phase: &'static str,
    probe: &Probe,
    markdown: &str,
    tokens: usize,
    ms: f64,
) -> ProbeResult {
    let lower = markdown.to_lowercase();
    let units = units(markdown);
    let first = |needle: &str| {
        let needle = needle.to_lowercase();
        units.iter().position(|(_, unit)| unit.contains(&needle))
    };
    let expected = probe.expect.first().and_then(|needle| first(needle));
    let stale_at = probe.stale.iter().filter_map(|needle| first(needle)).min();
    let has = |needle: &&str| lower.contains(&needle.to_lowercase());
    let answered = answer(markdown, probe.question);
    let answer_ok = grade(probe, answered.as_deref());
    ProbeResult {
        scenario,
        phase,
        id: probe.id,
        via: via_name(&probe.via),
        style: match probe.style {
            Style::Lexical => "lexical",
            Style::Paraphrase => "paraphrase",
        },
        timed_out: false,
        hit: (!probe.expect.is_empty()).then(|| probe.expect.iter().all(has)),
        rank: expected.map(|at| at + 1),
        section: expected.map(|at| units[at].0.clone()),
        contradiction: !probe.stale.is_empty(),
        stale_present: probe.stale.iter().any(has),
        stale_first: match (stale_at, expected) {
            (Some(stale), Some(fresh)) => stale < fresh,
            (Some(_), None) => true,
            _ => false,
        },
        leak_checked: !probe.forbidden.is_empty(),
        leak: probe.forbidden.iter().any(has),
        answer: answered,
        answer_ok,
        llm_answer: None,
        llm_ok: None,
        llm_tokens: 0,
        llm_cost_usd: None,
        captured: None,
        ms,
        tokens,
        units: units.len(),
        markdown: markdown.to_string(),
    }
}

/// Whether `answer` is right for `probe`: every expected string (or one of
/// its accepted wordings) and no stale one. `None` for a probe that expects nothing.
pub(crate) fn grade(probe: &Probe, answer: Option<&str>) -> Option<bool> {
    (!probe.expect.is_empty()).then(|| {
        answer.is_some_and(|text| {
            let text = text.to_lowercase();
            let has = |needle: &&str| text.contains(&needle.to_lowercase());
            (probe.expect.iter().all(has) || probe.accept.iter().any(has))
                && !probe.stale.iter().any(has)
        })
    })
}

/// Totals over a set of results.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct Totals {
    pub(crate) probes: usize,
    pub(crate) scored: usize,
    pub(crate) hits: usize,
    pub(crate) mrr: f64,
    pub(crate) answers_ok: usize,
    pub(crate) llm_scored: usize,
    pub(crate) llm_ok: usize,
    pub(crate) captured_checked: usize,
    pub(crate) captured: usize,
    pub(crate) contradictions: usize,
    pub(crate) fresh_first: usize,
    pub(crate) leak_checks: usize,
    pub(crate) leaks: usize,
}

impl Totals {
    /// Sums `results`.
    pub(crate) fn of<'a>(results: impl IntoIterator<Item = &'a ProbeResult>) -> Self {
        let mut totals = Self::default();
        let mut reciprocal = 0.0;
        for result in results {
            totals.probes += 1;
            if let Some(hit) = result.hit {
                totals.scored += 1;
                totals.hits += usize::from(hit);
                reciprocal += result.rank.map_or(0.0, |rank| 1.0 / rank as f64);
                totals.answers_ok += usize::from(result.answer_ok == Some(true));
                if let Some(held) = result.captured {
                    totals.captured_checked += 1;
                    totals.captured += usize::from(held);
                }
                if let Some(ok) = result.llm_ok {
                    totals.llm_scored += 1;
                    totals.llm_ok += usize::from(ok);
                }
            }
            if result.contradiction {
                totals.contradictions += 1;
                totals.fresh_first += usize::from(result.hit == Some(true) && !result.stale_first);
            }
            totals.leak_checks += usize::from(result.leak_checked);
            totals.leaks += usize::from(result.leak);
        }
        if totals.scored > 0 {
            totals.mrr = reciprocal / totals.scored as f64;
        }
        totals
    }

    /// `part` of `whole` as a percentage cell.
    pub(crate) fn pct(part: usize, whole: usize) -> String {
        if whole == 0 {
            "–".to_string()
        } else {
            format!(
                "{:.0}% ({part}/{whole})",
                100.0 * part as f64 / whole as f64
            )
        }
    }
}

/// Latency percentiles of a set of samples, in milliseconds.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct Latency {
    pub(crate) n: usize,
    pub(crate) p50: f64,
    pub(crate) p95: f64,
    pub(crate) p99: f64,
    pub(crate) max: f64,
}

impl Latency {
    /// Percentiles of `samples`.
    pub(crate) fn of(samples: &[f64]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
        Self {
            n: sorted.len(),
            p50: at(0.5),
            p95: at(0.95),
            p99: at(0.99),
            max: sorted[sorted.len() - 1],
        }
    }
}
