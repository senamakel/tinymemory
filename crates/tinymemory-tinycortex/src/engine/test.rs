//! Capability honesty for the full engine provider.
//!
//! The point of lifting the optional families here (issue #18 §C3) is that a
//! host filtering its surface from a negotiated capability set gets the whole
//! engine rather than the mandatory third of it. That is only safe if the set
//! is true.
//!
//! These assert the rule directly rather than through a constructed provider.
//! Construction needs a `MemoryClient`, which needs the host's process-global
//! seams (`set_embedding_host` and friends) installed — and a test that installs
//! a process global is order-dependent, which `AGENTS.md` rules out. The
//! provider-level check that `capabilities()` equals the reachable accessors is
//! `audit_provider`, and it runs against a real engine in the conformance suite
//! once a host has wired those seams.

#![allow(clippy::expect_used, clippy::panic)]

use tinymemory_api::capabilities::{Capabilities, Capability};
use tinymemory_api::chunks::DataSource;
use tinymemory_api::error::MemoryError;
use tinymemory_api::host::MemoryHostConfig;
use tinymemory_api::provider::types::IngestItem;
use tinymemory_api::types::MemoryTaint;

use super::{
    advertised_capabilities, audit_entry, body_after_front_matter, degraded_capabilities,
    diagnosis_failure, facet_type_to_engine, handle_to_contract, handle_to_engine,
    like_prefix_pattern, parse_person_id, refuse_composio_dispatch, scope_to_engine,
    validate_ingest_item, EngineRuntimeConfig,
};

fn ingest_item(content: &str, mime: Option<&str>, taint: MemoryTaint) -> IngestItem {
    IngestItem {
        namespace: None,
        source: DataSource::Upload,
        source_id: "doc-1".to_string(),
        owner: "owner".to_string(),
        source_ref: None,
        content: content.to_string(),
        mime: mime.map(str::to_string),
        timestamp: None,
        tags: Vec::new(),
        taint,
        path_scope: None,
        author: None,
        channel_label: None,
        platform: None,
        to: Vec::new(),
        cc: Vec::new(),
        subject: None,
        list_unsubscribe: None,
    }
}

fn runtime_config() -> EngineRuntimeConfig {
    EngineRuntimeConfig {
        workspace_dir: "/workspace".into(),
        config_path: "/workspace/config.toml".into(),
        backend_api_url: String::new(),
        memory: Default::default(),
        memory_tree: Default::default(),
        scheduler_gate: Default::default(),
        local_ai: Default::default(),
        embeddings_provider: Some("ollama: nomic-embed-text".to_string()),
        memory_provider: Some("ollama: qwen3".to_string()),
        default_model: Some("host/model".to_string()),
        default_temperature: 0.3,
        output_language: Some("en".to_string()),
        memory_sources: serde_json::json!([{"id": "source-1"}]),
        memory_sync_interval_secs: Some(14_400),
        composio_mode: tinymemory_api::host::COMPOSIO_MODE_DIRECT.to_string(),
        composio_entity_id: "entity-1".to_string(),
    }
}

#[test]
fn the_mandatory_families_are_always_advertised() {
    let caps = advertised_capabilities();
    for mandatory in Capability::MANDATORY {
        assert!(
            caps.contains(mandatory),
            "`{}` must be advertised in every build",
            mandatory.as_str()
        );
    }
}

#[cfg(feature = "memory-git")]
#[test]
fn the_full_engine_advertises_every_family_with_memory_git() {
    // The lift's headline: this adapter used to advertise three families.
    assert_eq!(advertised_capabilities(), Capabilities::all());
    assert!(advertised_capabilities().contains(Capability::Diff));
}

#[cfg(not(feature = "memory-git"))]
#[test]
fn diff_is_withheld_when_the_snapshot_store_is_compiled_out() {
    // The gate has to reach the advertisement, not just the accessor. A build
    // that advertised `Diff` here would fail `audit_provider` — which is how
    // that audit earns its place.
    let caps = advertised_capabilities();
    assert!(!caps.contains(Capability::Diff));
    // Everything else the engine serves is still advertised: withholding one
    // family must not quietly withhold the rest.
    assert_eq!(caps, Capabilities::all().without(Capability::Diff));
    assert_eq!(caps.len(), Capabilities::all().len() - 1);
}

#[test]
fn ingest_accepts_decoded_text_mime_families() {
    for mime in [
        None,
        Some("text/plain"),
        Some("text/markdown; charset=utf-8"),
        Some("application/json"),
        Some("application/activity+json"),
        Some("application/xml"),
        Some("application/atom+xml"),
        Some("application/x-ndjson"),
    ] {
        validate_ingest_item(&ingest_item("decoded text", mime, MemoryTaint::Internal))
            .unwrap_or_else(|error| panic!("{mime:?} should be accepted: {error}"));
    }
}

#[test]
fn ingest_rejects_empty_binary_and_non_default_taint() {
    let cases = [
        ingest_item("   ", Some("text/plain"), MemoryTaint::Internal),
        ingest_item(
            "binary already decoded badly",
            Some("application/pdf"),
            MemoryTaint::Internal,
        ),
        ingest_item(
            "external content",
            Some("text/plain"),
            MemoryTaint::ExternalSync,
        ),
    ];
    for item in cases {
        assert!(
            matches!(validate_ingest_item(&item), Err(MemoryError::Invalid(_))),
            "invalid ingest item was accepted: {item:?}"
        );
    }
}

#[tokio::test]
async fn runtime_config_routes_models_and_round_trips_source_configuration() {
    let mut config = runtime_config();
    assert_eq!(
        config.workspace_dir(),
        &std::path::PathBuf::from("/workspace")
    );
    assert_eq!(
        config.config_path(),
        &std::path::PathBuf::from("/workspace/config.toml")
    );
    assert_eq!(
        config.memory_tree_content_root(),
        std::path::PathBuf::from("/workspace/memory_tree/content")
    );
    let _ = config.memory();
    let _ = config.memory_tree();
    let _ = config.scheduler_gate();
    let _ = config.local_ai();
    assert!(config.cloud_providers().is_empty());
    assert_eq!(
        config.embeddings_provider(),
        Some("ollama: nomic-embed-text")
    );
    assert_eq!(config.memory_provider(), Some("ollama: qwen3"));
    assert_eq!(
        config.workload_local_model("embeddings").as_deref(),
        Some("nomic-embed-text")
    );
    assert_eq!(
        config.workload_local_model("memory").as_deref(),
        Some("qwen3")
    );
    assert_eq!(config.workload_local_model("chat"), None);
    assert_eq!(config.default_model(), Some("host/model"));
    assert_eq!(config.default_temperature(), 0.3);
    assert_eq!(config.output_language(), Some("en"));
    assert!(config.as_any().is::<EngineRuntimeConfig>());
    assert!(config.to_arc().as_any().is::<EngineRuntimeConfig>());
    assert_eq!(config.api_url(), None);
    assert!(config.effective_backend_api_url().is_empty());
    // Not `Ok(None)`: see the accessor's own doc. `Ok(None)` reads as "signed
    // out" and sends a reader after a sign-in that cannot help.
    let session = config
        .session_token()
        .expect_err("a module-side config must refuse rather than report signed-out");
    assert!(session.contains("no backend session token"), "{session}");
    assert_eq!(config.memory_sync_interval_secs(), Some(14_400));
    assert!(config.onboarding_completed());
    assert!(!config.secrets_encrypt());
    assert!(config.composio().is_direct());
    assert_eq!(config.composio().entity_id, "entity-1");
    // The key never rides in the config — `composio_config` resolves it through
    // the `ComposioHost` seam, per call.
    assert!(config.composio().api_key.is_none());
    assert_eq!(config.composio_source_caps_migration_version(), 0);
    config.set_composio_source_caps_migration_version(2);
    config.apply_env_overrides();
    assert_eq!(
        config.memory_sources_json().expect("source JSON"),
        serde_json::json!([{"id": "source-1"}])
    );

    config
        .set_memory_sources_json(serde_json::json!([{"id": "source-2"}]))
        .expect("replace source JSON");
    assert_eq!(
        config.memory_sources_json().expect("source JSON"),
        serde_json::json!([{"id": "source-2"}])
    );
    config
        .save()
        .await
        .expect("the in-memory adapter save is a no-op");
}

/// The cadence is answered from the field, including the two values that mean
/// something other than a number of seconds.
///
/// This is the blocker the periodic loops could not see past: the accessor used
/// to answer the constant `Some(0)`, which
/// `sync::composio::periodic::effective_interval_secs` maps to `None` — the
/// contract's manual-only — so every source was skipped on every tick with
/// nothing logged. A cadence that reads as a *setting* has to come from the
/// host, and the only wrong answer that is silent is this one.
#[test]
fn the_sync_cadence_is_answered_from_the_host_and_not_from_a_constant() {
    let mut config = runtime_config();

    // No explicit user choice: callers fall back to the 24h default.
    config.memory_sync_interval_secs = None;
    assert_eq!(config.memory_sync_interval_secs(), None);

    // "Manual only", which the host can now actually express.
    config.memory_sync_interval_secs = Some(0);
    assert_eq!(config.memory_sync_interval_secs(), Some(0));

    config.memory_sync_interval_secs = Some(3_600);
    assert_eq!(config.memory_sync_interval_secs(), Some(3_600));
}

/// A host that states no Composio mode reads as "not direct", which is what an
/// unconfigured integration should look like — and is exactly what the accessor
/// answered before the field existed, so an older host's behaviour is unchanged.
#[test]
fn an_unstated_composio_mode_is_not_direct() {
    let config = EngineRuntimeConfig {
        composio_mode: String::new(),
        backend_api_url: String::new(),
        composio_entity_id: String::new(),
        ..runtime_config()
    };

    assert!(!config.composio().is_direct());
    assert!(config.composio().entity_id.is_empty());
}

/// Backend mode fails with the structural cause, not with a sign-in prompt.
///
/// `composio_config` reaches `session_token` only on its backend branch, so this
/// is the message a backend-mode Composio sync inside a module actually
/// produces. It has to say *why* — no sign-in fixes a config that has no field
/// for a bearer.
#[test]
fn the_backend_branch_names_why_a_module_cannot_serve_it() {
    let config = EngineRuntimeConfig {
        composio_mode: tinymemory_api::host::COMPOSIO_MODE_BACKEND.to_string(),
        ..runtime_config()
    };

    let error = config
        .session_token()
        .expect_err("backend mode must fail, and say why");

    assert!(error.contains("no backend session token"), "{error}");
    assert!(
        error.contains("ComposioHost::execute"),
        "the message must name what would close the gap: {error}"
    );
    // The old wording sent readers to a sign-in that cannot help.
    assert!(!error.contains("not configured"), "{error}");
}

#[test]
fn people_profile_and_scope_boundary_conversions_are_total_and_fail_closed() {
    use tinymemory_api::provider::types::SourceScope;
    use tinymemory_api::provider::{FacetType, PersonHandle};
    use tinymemory_core::store::namespace_store::profile::FacetType as EngineFacet;

    for handle in [
        PersonHandle::IMessage("+15551234567".into()),
        PersonHandle::Email("person@example.com".into()),
        PersonHandle::DisplayName("Ada Lovelace".into()),
    ] {
        assert_eq!(handle_to_contract(handle_to_engine(&handle)), handle);
    }

    let id = uuid::Uuid::nil().to_string();
    assert_eq!(parse_person_id(&id).expect("valid id").0.to_string(), id);
    assert!(matches!(
        parse_person_id("not-a-person-id"),
        Err(MemoryError::Invalid(_))
    ));

    assert_eq!(
        [
            FacetType::Preference,
            FacetType::Workflow,
            FacetType::Role,
            FacetType::Personality,
            FacetType::Context,
        ]
        .map(facet_type_to_engine),
        [
            EngineFacet::Preference,
            EngineFacet::Workflow,
            EngineFacet::Role,
            EngineFacet::Personality,
            EngineFacet::Context,
        ]
    );

    assert!(scope_to_engine(None).is_none());
    assert_eq!(
        scope_to_engine(Some(&SourceScope::new(["source-a", "source-b"])))
            .expect("scoped set")
            .len(),
        2
    );
}

#[test]
fn composio_dispatch_is_refused_regardless_of_toolkit() {
    // Composio connections are read by the connector module, not this
    // engine: reaching a connected account needs a credential this crate
    // does not hold and must not. Every toolkit-keyed dispatch entry point
    // refuses unconditionally now, rather than gating on which toolkit used
    // to have a native pipeline.
    let error = refuse_composio_dispatch("a composio connection");
    assert!(
        matches!(error, MemoryError::Invalid(_)),
        "expected Invalid, got {error:?}"
    );
    assert!(error.to_string().contains("connector module"));
}

#[test]
fn a_pipeline_failure_crosses_with_the_engines_own_wire_strings() {
    // The frontend resolves `remediation_key` to localised text and compares
    // `code` for equality, so a re-spelling on this side stops matching keys
    // that already exist. Pinned against the engine's own `as_str`.
    use tinymemory_core::tree::health::{FailureCode, PipelineFailure};

    let failure = PipelineFailure::new(FailureCode::EmbeddingsUnconfigured);
    let crossed = diagnosis_failure(&failure);
    assert_eq!(crossed.code, FailureCode::EmbeddingsUnconfigured.as_str());
    assert_eq!(
        crossed.class.as_deref(),
        Some(FailureCode::EmbeddingsUnconfigured.class().as_str())
    );
    assert_eq!(
        crossed.remediation_key,
        FailureCode::EmbeddingsUnconfigured.remediation_key()
    );
    assert_eq!(crossed.detail, None);
}

#[test]
fn an_audit_row_crosses_field_for_field_and_keeps_its_price() {
    // The row was priced when it was written. Carrying `estimated_cost_usd`
    // verbatim — rather than re-deriving it from the token counts on this side
    // — is what keeps a historical total summed at the rate it was recorded at.
    let entry = tinymemory_core::sync::audit::SyncAuditEntry {
        timestamp: chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_000, 0)
            .expect("valid timestamp"),
        source_id: "composio:gmail:conn-1".to_string(),
        source_kind: "composio".to_string(),
        scope: "gmail:conn-1".to_string(),
        items_fetched: 12,
        batches: 2,
        input_tokens: 1_000,
        output_tokens: 100,
        estimated_cost_usd: 0.42,
        composio_actions_called: 4,
        composio_cost_usd: 0.02,
        actual_charged_usd: None,
        duration_ms: 4_200,
        success: true,
        error: None,
        tree_ingest_failures: 0,
        tree_error: None,
    };
    let crossed = audit_entry(entry);
    assert_eq!(crossed.source_id, "composio:gmail:conn-1");
    assert_eq!(crossed.items_fetched, 12);
    assert!((crossed.estimated_cost_usd - 0.42).abs() < 1e-9);
    // The contract's own arithmetic, over the same fields the engine's copy
    // uses: estimate when nothing was charged, plus Composio's action cost.
    assert!((crossed.effective_cost_usd() - 0.44).abs() < 1e-9);
    assert!(crossed.success);
    assert_eq!(crossed.error, None);
}

#[test]
fn the_two_new_families_are_advertised_by_the_full_engine() {
    // The families exist because the driver serves them; advertising is what
    // makes a host register their RPC surface, and `audit_provider` fails the
    // bind if the accessor and the advertisement disagree.
    let caps = advertised_capabilities();
    assert!(caps.contains(Capability::SourceSync));
    assert!(caps.contains(Capability::CodingSessions));
}

/// `memory_sources_json` answers from the host's registry file when it
/// exists — a source added after load is visible — and from the load-time
/// snapshot only when there is no file (openhuman#5820).
#[test]
fn memory_sources_json_reads_the_live_registry_file_when_present() {
    use tinymemory_api::host::MemoryHostConfig;

    let root = tempfile::tempdir().expect("tempdir");
    let config_path = root.path().join("config.toml");
    let snapshot = serde_json::json!([
        { "id": "src_snapshot", "kind": "folder", "label": "old", "path": "." }
    ]);

    let mut config = runtime_config();
    config.workspace_dir = root.path().join("workspace");
    config.config_path = config_path.clone();
    config.memory_sources = snapshot.clone();

    // No file yet: the snapshot is the only answer.
    assert_eq!(
        config.memory_sources_json().expect("snapshot answer"),
        snapshot
    );

    // The host writes a registry entry after load; the live read sees it.
    std::fs::write(
        &config_path,
        "[[memory_sources]]\nid = \"src_live\"\nkind = \"folder\"\nlabel = \"new\"\npath = \".\"\n",
    )
    .expect("write the registry file");
    let live = config.memory_sources_json().expect("live answer");
    let ids: Vec<&str> = live
        .as_array()
        .expect("a JSON array of sources")
        .iter()
        .filter_map(|entry| entry.get("id").and_then(|id| id.as_str()))
        .collect();
    assert_eq!(
        ids,
        vec!["src_live"],
        "the file, not the snapshot, is the registry"
    );
}

/// With a registry file present, `set_memory_sources_json` writes through:
/// the next (live) getter returns the new entries and the file holds them
/// (openhuman#5820, review follow-up). Without a file, the snapshot is
/// updated and read back as before.
#[test]
fn set_memory_sources_json_writes_through_to_the_registry_file() {
    use tinymemory_api::host::MemoryHostConfig;

    let root = tempfile::tempdir().expect("tempdir");
    let config_path = root.path().join("config.toml");
    std::fs::write(&config_path, "other_key = 1\n").expect("seed the config file");

    let mut config = runtime_config();
    config.workspace_dir = root.path().join("workspace");
    config.config_path = config_path.clone();

    let entries = serde_json::json!([
        { "id": "src_written", "kind": "folder", "label": "written", "path": "." }
    ]);
    config
        .set_memory_sources_json(entries.clone())
        .expect("the setter writes through");

    // The live getter sees the update...
    let live = config.memory_sources_json().expect("live answer");
    let ids: Vec<&str> = live
        .as_array()
        .expect("a JSON array of sources")
        .iter()
        .filter_map(|entry| entry.get("id").and_then(|id| id.as_str()))
        .collect();
    assert_eq!(ids, vec!["src_written"]);

    // ...it is on disk, and the file's other keys survived the write.
    let on_disk = std::fs::read_to_string(&config_path).expect("read the config file");
    assert!(on_disk.contains("src_written"), "{on_disk}");
    assert!(on_disk.contains("other_key = 1"), "{on_disk}");

    // No file: the snapshot is the store.
    let mut snapshot_only = runtime_config();
    snapshot_only.config_path = root.path().join("missing").join("config.toml");
    snapshot_only
        .set_memory_sources_json(entries.clone())
        .expect("snapshot-only setter");
    assert_eq!(
        snapshot_only.memory_sources_json().expect("snapshot"),
        entries
    );
}

#[test]
fn a_degradation_snapshot_crosses_every_flag_and_its_cause() {
    // `Diagnose` and `DegradedState` answer the same question at different
    // prices, so a caller can reasonably compare them. One mapping, asserted
    // field by field, is what makes that comparison safe — a transposed pair
    // here would have the two members disagree about which capability is
    // reduced, and both would still look like plausible answers.
    use tinymemory_core::tree::health::{DegradedState, FailureCode, PipelineFailure};

    let degraded = DegradedState {
        semantic_recall: true,
        structure: false,
        storage: true,
        cause: Some(PipelineFailure::new(FailureCode::StorageUnavailable)),
    };
    let crossed = degraded_capabilities(&degraded);
    assert!(crossed.semantic_recall);
    assert!(!crossed.structure);
    assert!(crossed.storage);
    assert_eq!(
        crossed.cause.as_ref().map(|failure| failure.code.as_str()),
        Some(FailureCode::StorageUnavailable.as_str())
    );

    // Nothing degraded is nothing to explain: a cause carried over a cleared
    // set of flags would put a remediation on a panel with no fault on it.
    let clear = degraded_capabilities(&DegradedState::default());
    assert_eq!(
        clear,
        tinymemory_api::provider::DegradedCapabilities::default()
    );
    assert_eq!(clear.cause, None);
}

#[test]
fn a_chunk_id_prefix_is_matched_literally() {
    // The contract calls the prefix literal, so the driver has to make `LIKE`
    // agree. Every generated source id contains an underscore, which is `LIKE`'s
    // single-character wildcard — left unescaped, `mem_src:src_a:` would also
    // count another source's chunks, and the count would be wrong in the
    // direction that looks healthy.
    assert_eq!(
        like_prefix_pattern("mem_src:src_a:"),
        r"mem\_src:src\_a:%",
        "every underscore is escaped, not left as a wildcard"
    );
    assert_eq!(
        like_prefix_pattern("gmail:conn-1:"),
        "gmail:conn-1:%",
        "a prefix with no metacharacter gains only the trailing wildcard"
    );
    assert_eq!(
        like_prefix_pattern("100%_of\\it"),
        r"100\%\_of\\it%",
        "the escape character itself is escaped, as the ESCAPE clause requires"
    );
    assert_eq!(
        like_prefix_pattern(""),
        "%",
        "an empty prefix matches everything, which is what an empty prefix means"
    );
}

#[test]
fn the_front_matter_strip_decides_built_versus_not_the_way_the_host_did() {
    // The strip exists for one verdict — is there prose under the compiled
    // artifact's front-matter — and these are the host's own decision points,
    // reproduced: a well-formed artifact yields its body, a body of pure
    // whitespace reads as unbuilt, an opener with no closer never leaks the
    // delimiter as prose, and content with no front-matter at all is already
    // the body.
    assert_eq!(
        body_after_front_matter("---\nscope: persona/communication\n---\nShort sentences.\n"),
        "Short sentences.\n"
    );
    assert!(
        body_after_front_matter("---\nscope: x\n---\n \n\t")
            .trim()
            .is_empty(),
        "front-matter over whitespace is not a profile"
    );
    assert_eq!(
        body_after_front_matter("---\nscope: x\nno closer follows"),
        "scope: x\nno closer follows",
        "a malformed opener falls back to everything after it, not to the raw artifact"
    );
    assert_eq!(
        body_after_front_matter("plain body, no front matter"),
        "plain body, no front matter"
    );
}

// ── Error provenance (oh#6179) and the segment lifecycle marker (oh#6186) ────

/// `other` renders the whole `anyhow` chain, not just its outermost context.
///
/// This is the regression from oh#6179. `{error}` on an `anyhow::Error` prints
/// only the last context attached to it, so a summariser failure reached the
/// host as `memory_tree::summarise: provider=…` with the transport error
/// underneath it discarded. That string is then all a host has — the bus
/// carries a code and a message, not a chain — so the report was
/// unroot-causable and there was no typed error left for a caller to classify.
#[test]
fn other_preserves_the_whole_cause_chain() {
    let cause = anyhow::anyhow!("error sending request")
        .context("memory_tree::summarise: provider=inference:summarization-v1");

    let wrapped = super::TinycortexProvider::other("summarise tree inputs", cause);

    let rendered = wrapped.to_string();
    assert!(
        rendered.contains("summarise tree inputs"),
        "the call site is missing: {rendered}"
    );
    assert!(
        rendered.contains("provider=inference:summarization-v1"),
        "the provider context is missing: {rendered}"
    );
    assert!(
        rendered.contains("error sending request"),
        "the root cause was dropped — this is oh#6179: {rendered}"
    );
}

/// A closed-but-unsummarised segment reaches the contract as `Closed`, not
/// merely as `open: false`.
///
/// `open` is retained and still false for both closed states, so this pins the
/// pair rather than the new field alone: the bug oh#6186 describes is not that
/// `open` was wrong, it is that it was the only thing a host could see.
#[test]
fn a_closed_segment_carries_its_status_to_the_contract() {
    use tinymemory_api::provider::episodic::SegmentStatus as ContractStatus;
    use tinymemory_core::store::segments::{ConversationSegment as Row, SegmentStatus};

    let row = |status: SegmentStatus| Row {
        segment_id: "seg-1".to_string(),
        session_id: "sess-1".to_string(),
        namespace: "ns".to_string(),
        start_episodic_id: 1,
        end_episodic_id: Some(9),
        start_timestamp: 0.0,
        end_timestamp: Some(1.0),
        turn_count: 4,
        summary: None,
        embedding: None,
        topic_keywords: None,
        status,
        created_at: 0.0,
        updated_at: 1.0,
        start_seq: None,
        end_seq: None,
    };

    let closed = super::segment_to_contract(row(SegmentStatus::Closed));
    let summarised = super::segment_to_contract(row(SegmentStatus::Summarised));
    let open = super::segment_to_contract(row(SegmentStatus::Open));

    assert_eq!(closed.status, Some(ContractStatus::Closed));
    assert_eq!(summarised.status, Some(ContractStatus::Summarised));
    assert_eq!(open.status, Some(ContractStatus::Open));

    assert!(!closed.open, "a closed segment must not report as open");
    assert_eq!(
        closed.open, summarised.open,
        "`open` is deliberately unchanged — `status` is what separates these"
    );
    assert!(open.open);
}
