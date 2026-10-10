//! Tests of the KPI arithmetic over hand-built scenario reports.

use super::*;
use crate::inspect::{Captured, Spend};
use crate::score::score;
use crate::{ScenarioReport, Synthesis};

/// The first probe of `scenario`, scored as a hit or a miss in `phase`.
fn probed(scenario: &'static str, phase: &'static str, hit: bool) -> ProbeResult {
    let probe = crate::scenarios::all()
        .into_iter()
        .find(|s| s.name == scenario)
        .expect("a scenario of the eval")
        .probes
        .remove(0);
    let markdown = if hit {
        format!("- {}", probe.expect.join(" "))
    } else {
        "- nothing relevant".to_string()
    };
    score(scenario, phase, &probe, &markdown, 10, 1.0)
}

fn report(name: &'static str, probes: Vec<ProbeResult>, conflicts: &[&str]) -> ScenarioReport {
    ScenarioReport {
        name,
        about: "",
        writes: 0,
        tool_calls: 0,
        pre_turn_timeouts: 0,
        ranked_ready: None,
        ranked_wait_ms: None,
        settle_ms: 0.0,
        synthesis: Synthesis {
            captured: Captured {
                conflicts: conflicts.iter().map(|c| (*c).to_string()).collect(),
                ..Captured::default()
            },
            ..Synthesis::default()
        },
        probes,
        usage: None,
    }
}

fn value(kpis: &[Kpi], name: &str) -> Option<f64> {
    kpis.iter()
        .find(|kpi| kpi.name == name)
        .unwrap_or_else(|| panic!("no KPI named {name}"))
        .value
}

fn usage(cost_usd: f64) -> Usage {
    Usage {
        calls: 4,
        tokens: 1000,
        cost_usd,
        by_role: [(
            "extraction".to_string(),
            Spend {
                calls: 4,
                tokens: 1000,
                cost_usd,
            },
        )]
        .into(),
    }
}

#[test]
fn synthesis_gain_is_the_hit_rate_the_belief_build_adds() {
    let reports = [report(
        "learnings",
        vec![
            probed("learnings", "recall", false),
            probed("learnings", "synthesis", true),
        ],
        &[],
    )];
    let kpis = compute(&reports, None, &Timings::default());
    assert_eq!(value(&kpis, "pack hit (recall)"), Some(0.0));
    assert_eq!(value(&kpis, "pack hit"), Some(100.0));
    assert_eq!(value(&kpis, "synthesis gain"), Some(100.0));
    assert_eq!(value(&kpis, "lesson in pack"), Some(100.0));
}

#[test]
fn planted_conflicts_count_as_flagged_and_others_as_spurious() {
    let reports = [
        report(
            "conflicts",
            vec![probed("conflicts", "synthesis", true)],
            &["value_conflict open: refund settles_within [five | ten]"],
        ),
        report(
            "brain_lookup",
            vec![probed("brain_lookup", "synthesis", true)],
            &["value_conflict open: office floor [2 | 3]"],
        ),
        // A superseded value may be flagged without counting against it.
        report(
            "contradictions",
            vec![probed("contradictions", "synthesis", true)],
            &["value_conflict open: region is [us-east-1 | eu-west-2]"],
        ),
    ];
    let kpis = compute(&reports, Some(&usage(0.0)), &Timings::default());
    assert_eq!(value(&kpis, "planted conflicts flagged"), Some(100.0));
    assert_eq!(value(&kpis, "spurious conflicts"), Some(1.0));
    assert_eq!(value(&kpis, "conflicts raised"), Some(3.0));
}

#[test]
fn a_missed_planted_conflict_lowers_the_flagged_rate() {
    let reports = [report(
        "conflicts",
        vec![probed("conflicts", "synthesis", false)],
        &[],
    )];
    let kpis = compute(&reports, Some(&usage(0.0)), &Timings::default());
    assert_eq!(value(&kpis, "planted conflicts flagged"), Some(0.0));
    assert_eq!(value(&kpis, "disagreement in pack"), Some(0.0));
}

#[test]
fn cost_per_correct_answer_prefers_the_model_answers_and_adds_their_cost() {
    let mut right = probed("surprise", "synthesis", true);
    right.llm_ok = Some(true);
    right.llm_tokens = 300;
    right.llm_cost_usd = Some(0.5);
    let mut wrong = probed("surprise", "synthesis", true);
    wrong.llm_ok = Some(false);
    wrong.llm_tokens = 200;
    wrong.llm_cost_usd = Some(0.5);
    let reports = [report("surprise", vec![right, wrong], &[])];
    let kpis = compute(&reports, Some(&usage(2.0)), &Timings::default());
    assert_eq!(value(&kpis, "answerer"), Some(1.0));
    assert_eq!(value(&kpis, "answerer tokens"), Some(500.0));
    // $2 of CortexDB plus $1 of answers over one correct model answer.
    assert_eq!(value(&kpis, "per correct answer"), Some(3.0));
    assert_eq!(value(&kpis, "extraction"), Some(2.0));
    assert_eq!(value(&kpis, "surprise answered"), Some(50.0));
}

#[test]
fn engine_only_kpis_are_unmeasured_on_the_reference_engine() {
    let reports = [report(
        "conflicts",
        vec![probed("conflicts", "synthesis", true)],
        &[],
    )];
    let kpis = compute(&reports, None, &Timings::default());
    for name in [
        "planted conflicts flagged",
        "spurious conflicts",
        "beliefs held",
        "CortexDB models",
        "per correct answer",
    ] {
        assert_eq!(value(&kpis, name), None, "{name}");
    }
    assert_eq!(value(&kpis, "model answer"), None);
}

#[test]
fn latency_reads_the_pre_turn_and_probe_samples() {
    let mut timings = Timings::default();
    for ms in [10.0, 20.0, 30.0] {
        timings.add("pre_turn (log + recall)", ms);
    }
    timings.add("probe pre_turn", 5.0);
    timings.add("probe context.md", 7.0);
    let kpis = compute(&[], None, &timings);
    assert_eq!(value(&kpis, "pre_turn p50"), Some(20.0));
    assert_eq!(value(&kpis, "probe p95"), Some(7.0));
    assert_eq!(value(&kpis, "pack hit"), None);
}

#[test]
fn timeout_rate_counts_scripted_turns_and_probe_turns() {
    let mut timed_out = probed("brain_lookup", "recall", false);
    timed_out.timed_out = true;
    let on_time = probed("brain_lookup", "synthesis", true);
    let mut report = report("brain_lookup", vec![timed_out, on_time], &[]);
    report.pre_turn_timeouts = 1;
    let mut timings = Timings::default();
    timings.add("pre_turn (log + recall)", 1_500.0);
    timings.add("pre_turn (log + recall)", 200.0);
    let kpis = compute(&[report], None, &timings);
    assert_eq!(value(&kpis, "pre_turn timeout rate"), Some(50.0));
}

#[test]
fn values_format_by_unit() {
    assert_eq!(format(62.4, Unit::Pct), "62%");
    assert_eq!(format(-4.0, Unit::Points), "-4 pp");
    assert_eq!(format(0.0123, Unit::Usd), "$0.012");
    assert_eq!(format(0.456, Unit::Score), "0.46");
}
