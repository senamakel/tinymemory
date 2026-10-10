//! A data-driven long thread that checks recall packs never become stored turns.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tinymemory_api::{
    ForgetTarget, LearningKind, ListRequest, MemoryEngine, MemoryMeta, StoreItem,
};
use tinymemory_tools::{AgentMemory, RecallPolicy};

use crate::agent::ScriptedAgent;
use crate::{Error, layout};

#[derive(Deserialize)]
struct Script {
    turns: usize,
    user_text: String,
    learning: String,
}

/// Metrics for the 500-turn feedback-loop probe.
#[derive(Serialize)]
pub(crate) struct LoopGuardReport {
    turns: usize,
    timeouts: usize,
    nonempty_packs: usize,
    first_50_mean_tokens: f64,
    last_50_mean_tokens: f64,
    first_50_duplicate_rate: f64,
    last_50_duplicate_rate: f64,
    pub(crate) echoed_items: usize,
}

fn mean(values: impl Iterator<Item = usize>) -> f64 {
    let values: Vec<usize> = values.collect();
    values.iter().sum::<usize>() as f64 / values.len().max(1) as f64
}

/// Replay the fixture and inspect every stored item for injected pack tags.
pub(crate) async fn run(
    engine: Arc<dyn MemoryEngine>,
    run: u64,
    policy: &RecallPolicy,
) -> Result<LoopGuardReport, Error> {
    let script: Script = serde_json::from_str(include_str!("data/loop_guard.json"))?;
    let layout = layout(run, "loop_guard", "main")?;
    let meta = MemoryMeta {
        namespace: layout.learnings().clone(),
        ..MemoryMeta::default()
    };
    engine
        .store(StoreItem::learning(
            script.learning.clone(),
            LearningKind::Fact,
            1.0,
            meta,
        ))
        .await?;
    let memory =
        AgentMemory::new(engine.clone(), layout.clone(), "loop-agent")?.with_policy(policy.clone());
    let runner = memory.background();
    let mut agent = ScriptedAgent::new(memory, "loop-thread", 8).openhuman();
    let mut tokens = Vec::with_capacity(script.turns);
    let mut duplicates = Vec::with_capacity(script.turns);
    let mut timeouts = 0;
    let mut nonempty_packs = 0;
    for _ in 0..script.turns {
        let record = agent.user(&script.user_text, &[]).await?;
        timeouts += usize::from(record.timed_out);
        nonempty_packs += usize::from(record.pack_tokens > 0);
        tokens.push(record.pack_tokens);
        duplicates.push((record.duplicate_lines, record.pack_lines));
        for job in record.jobs {
            runner.run(job).await?;
        }
    }
    let _ = agent.flush().await?;
    let mut echoed_items = 0;
    let mut cursor = None;
    loop {
        let mut req = ListRequest::new(layout.holistic_filter(), 100);
        req.cursor = cursor;
        let page = engine.export(req).await?;
        echoed_items += page
            .items
            .iter()
            .filter(|item| {
                serde_json::to_string(&item.item)
                    .is_ok_and(|body| body.contains("<memory-context>"))
            })
            .count();
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    if std::env::var("CORTEX_DB_KEEP").is_err() {
        engine
            .forget(ForgetTarget::Filter(layout.holistic_filter()))
            .await?;
    }
    let span = script.turns.min(50);
    let rate = |slice: &[(usize, usize)]| {
        let repeated = slice
            .iter()
            .map(|(duplicates, _)| duplicates)
            .sum::<usize>();
        let lines = slice.iter().map(|(_, lines)| lines).sum::<usize>();
        if lines == 0 {
            0.0
        } else {
            100.0 * repeated as f64 / lines as f64
        }
    };
    Ok(LoopGuardReport {
        turns: script.turns,
        timeouts,
        nonempty_packs,
        first_50_mean_tokens: mean(tokens.iter().take(span).copied()),
        last_50_mean_tokens: mean(tokens.iter().rev().take(span).copied()),
        first_50_duplicate_rate: rate(&duplicates[..span]),
        last_50_duplicate_rate: rate(&duplicates[script.turns - span..]),
        echoed_items,
    })
}
