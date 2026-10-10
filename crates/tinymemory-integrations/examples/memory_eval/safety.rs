//! Synthetic tenant-isolation and deletion checks across public read channels.

use std::sync::Arc;

use serde::Serialize;
use tinymemory_api::{
    EraseRequest, ForgetTarget, LearningKind, ListRequest, MemoryEngine, MemoryMeta, MetaFilter,
    Reach, RecallRequest, StoreItem,
};
use tinymemory_tools::context::{self, Brief, ContextSpec};
use tinymemory_tools::{AgentMemory, PreTurn, RecallPolicy};

use crate::inspect::Inspector;
use crate::{Error, layout};

const SENTINEL: &str = "cobalt-heron-251";

/// Each check records whether the synthetic private fact escaped or survived.
#[derive(Debug, Serialize)]
pub(crate) struct SafetyReport {
    pub(crate) checks: Vec<SafetyCheck>,
    pub(crate) forgotten: usize,
    pub(crate) derived_after_forget: Vec<String>,
    pub(crate) derived_ready_before_forget: Option<bool>,
    pub(crate) derived_ready_before_erase: Option<bool>,
}

impl SafetyReport {
    pub(crate) fn violations(&self) -> usize {
        self.checks.iter().filter(|check| !check.passed).count()
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct SafetyCheck {
    channel: &'static str,
    phase: &'static str,
    passed: bool,
}

fn check(checks: &mut Vec<SafetyCheck>, channel: &'static str, phase: &'static str, exposed: bool) {
    checks.push(SafetyCheck {
        channel,
        phase,
        passed: !exposed,
    });
}

async fn pack_contains(memory: &AgentMemory, thread: &str) -> Result<bool, Error> {
    let pack = memory
        .pre_turn(PreTurn::new(
            thread,
            0,
            "What is the private migration token?",
        ))
        .await?
        .pack;
    Ok(pack.markdown.contains(SENTINEL))
}

async fn context_contains(
    engine: &dyn MemoryEngine,
    root: tinymemory_api::Namespace,
) -> Result<bool, Error> {
    let spec = ContextSpec {
        briefs: vec![Brief::new(
            "Migration",
            "What is the private migration token?",
        )],
        reach: Some(Reach::subtree(root)),
        ..ContextSpec::default()
    };
    Ok(context::compile(engine, &spec)
        .await?
        .markdown
        .contains(SENTINEL))
}

async fn answer_contains(engine: &dyn MemoryEngine, filter: MetaFilter) -> Result<bool, Error> {
    let mut request = RecallRequest::new("What is the private migration token?", 8);
    request.filter = filter;
    Ok(engine.recall(request).await?.answer.contains(SENTINEL))
}

async fn export_contains(
    engine: &dyn MemoryEngine,
    filter: tinymemory_api::MetaFilter,
) -> Result<bool, Error> {
    let mut cursor = None;
    loop {
        let mut req = ListRequest::new(filter.clone(), 100);
        req.cursor = cursor;
        let page = engine.export(req).await?;
        if page
            .items
            .iter()
            .any(|item| serde_json::to_string(&item.item).is_ok_and(|body| body.contains(SENTINEL)))
        {
            return Ok(true);
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            return Ok(false);
        }
    }
}

async fn derived_contains(
    inspector: Option<&Inspector>,
    root: &str,
) -> Result<Option<bool>, Error> {
    let Some(inspector) = inspector else {
        return Ok(None);
    };
    let scopes = inspector.scopes(root).await?;
    Ok(Some(inspector.captured(&scopes).await?.mentions(SENTINEL)))
}

async fn wait_derived(inspector: &Inspector, root: &str) -> Result<bool, Error> {
    let started = std::time::Instant::now();
    loop {
        if derived_contains(Some(inspector), root).await? == Some(true) {
            return Ok(true);
        }
        if started.elapsed() >= std::time::Duration::from_secs(90) {
            return Ok(false);
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

/// Audit sibling isolation, filter-forget, and subtree erase on one engine.
pub(crate) async fn run(
    engine: Arc<dyn MemoryEngine>,
    inspector: Option<&Inspector>,
    run: u64,
    expect_derived: bool,
    pooled: bool,
) -> Result<SafetyReport, Error> {
    let acme = layout(run, "safety_audit", "acme", pooled)?;
    let globex = layout(run, "safety_audit", "globex", pooled)?;
    let policy = RecallPolicy {
        team_limit: 0,
        ..RecallPolicy::default()
    };
    let acme_memory =
        AgentMemory::new(engine.clone(), acme.clone(), "auditor")?.with_policy(policy.clone());
    let globex_memory =
        AgentMemory::new(engine.clone(), globex.clone(), "auditor")?.with_policy(policy);
    let item = || {
        StoreItem::learning(
            format!("The private migration token is {SENTINEL}."),
            LearningKind::Fact,
            1.0,
            MemoryMeta {
                namespace: acme.learnings().clone(),
                ..MemoryMeta::default()
            },
        )
    };
    engine.store(item()).await?;
    let mut checks = Vec::new();
    // The control proves that the fixture is retrievable before testing isolation.
    checks.push(SafetyCheck {
        channel: "own_pack_control",
        phase: "before",
        passed: pack_contains(&acme_memory, "own-control").await?,
    });
    check(
        &mut checks,
        "sibling_pack",
        "before",
        pack_contains(&globex_memory, "sibling-check").await?,
    );
    check(
        &mut checks,
        "sibling_context",
        "before",
        context_contains(engine.as_ref(), globex.root().clone()).await?,
    );
    check(
        &mut checks,
        "sibling_answer",
        "before",
        answer_contains(engine.as_ref(), globex.holistic_filter()).await?,
    );
    if let Some(exposed) = derived_contains(inspector, &globex.root().to_string()).await? {
        check(&mut checks, "sibling_derived", "before", exposed);
    }

    let derived_ready_before_forget = if expect_derived {
        let Some(inspector) = inspector else {
            return Err("--expect-derived needs a direct CortexDB inspector".into());
        };
        let ready = wait_derived(inspector, &acme.root().to_string()).await?;
        checks.push(SafetyCheck {
            channel: "own_derived_control",
            phase: "before",
            passed: ready,
        });
        Some(ready)
    } else {
        None
    };

    let forgotten = engine
        .forget(ForgetTarget::Filter(acme.holistic_filter()))
        .await?
        .forgotten;
    check(
        &mut checks,
        "pack",
        "after_forget",
        pack_contains(&acme_memory, "forgotten-check").await?,
    );
    check(
        &mut checks,
        "context",
        "after_forget",
        context_contains(engine.as_ref(), acme.root().clone()).await?,
    );
    check(
        &mut checks,
        "answer",
        "after_forget",
        answer_contains(engine.as_ref(), acme.holistic_filter()).await?,
    );
    check(
        &mut checks,
        "export",
        "after_forget",
        export_contains(engine.as_ref(), acme.holistic_filter()).await?,
    );
    let mut derived_after_forget = Vec::new();
    if let Some(inspector) = inspector {
        let scopes = inspector.scopes(&acme.root().to_string()).await?;
        let captured = inspector.captured(&scopes).await?;
        derived_after_forget.extend(
            captured
                .facts
                .iter()
                .chain(&captured.beliefs)
                .filter(|line| line.contains(SENTINEL))
                .cloned(),
        );
    }
    if let Some(exposed) = derived_contains(inspector, &acme.root().to_string()).await? {
        check(&mut checks, "derived", "after_forget", exposed);
    }
    if inspector.is_some() {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        if let Some(exposed) = derived_contains(inspector, &acme.root().to_string()).await? {
            check(&mut checks, "derived", "after_forget_5s", exposed);
        }
    }

    let restored = engine.store(item()).await?;
    checks.push(SafetyCheck {
        channel: "restore_control",
        phase: "before_erase",
        passed: !restored.replayed,
    });
    let derived_ready_before_erase = if expect_derived {
        let Some(inspector) = inspector else {
            return Err("--expect-derived needs a direct CortexDB inspector".into());
        };
        let ready = wait_derived(inspector, &acme.root().to_string()).await?;
        checks.push(SafetyCheck {
            channel: "own_derived_control",
            phase: "before_erase",
            passed: ready,
        });
        Some(ready)
    } else {
        None
    };
    engine
        .erase(EraseRequest::new(Reach::subtree(acme.root().clone())))
        .await?;
    check(
        &mut checks,
        "pack",
        "after_erase",
        pack_contains(&acme_memory, "erased-check").await?,
    );
    check(
        &mut checks,
        "context",
        "after_erase",
        context_contains(engine.as_ref(), acme.root().clone()).await?,
    );
    check(
        &mut checks,
        "answer",
        "after_erase",
        answer_contains(engine.as_ref(), acme.holistic_filter()).await?,
    );
    check(
        &mut checks,
        "export",
        "after_erase",
        export_contains(engine.as_ref(), acme.holistic_filter()).await?,
    );
    if let Some(exposed) = derived_contains(inspector, &acme.root().to_string()).await? {
        check(&mut checks, "derived", "after_erase", exposed);
    }
    if inspector.is_some() {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        if let Some(exposed) = derived_contains(inspector, &acme.root().to_string()).await? {
            check(&mut checks, "derived", "after_erase_5s", exposed);
        }
    }
    if std::env::var("CORTEX_DB_KEEP").is_err() {
        engine
            .forget(ForgetTarget::Filter(acme.holistic_filter()))
            .await?;
        engine
            .forget(ForgetTarget::Filter(globex.holistic_filter()))
            .await?;
    }
    Ok(SafetyReport {
        checks,
        forgotten,
        derived_after_forget,
        derived_ready_before_forget,
        derived_ready_before_erase,
    })
}

#[cfg(test)]
#[path = "safety_tests.rs"]
mod tests;
