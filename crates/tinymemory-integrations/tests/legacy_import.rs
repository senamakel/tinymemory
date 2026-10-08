//! Imports v1 workspaces built from the verbatim v1 DDL and checks every
//! mapping, the order, and resumption.

// Fixture helpers in `support` build databases and fail loudly on setup errors.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::Path;

use support::{OLD_MEMORY_DDL, chunk, chunk_store, doc, facet, turn, workspace};
use tinymemory_api::{
    DocumentBody, LearningKind, Role, SourceKind, StoreItem, ToolCallRef, TurnRange,
};
use tinymemory_integrations::import::{
    Checkpoint, ChunkCursor, EXTERNAL_SYNC_TAG, Error, ImportedItem, LegacyCounts, LegacyWorkspace,
};

const T0: f64 = 1_700_000_000.0;

/// A workspace exercising every section and edge case.
fn rich() -> tempfile::TempDir {
    let (dir, conn) = workspace(support::MEMORY_DDL);
    doc(
        &conn,
        "d01",
        "document_notes",
        Some("document:notes"),
        "Plan",
        "Ship v2.",
        r#"["work"]"#,
        r#"{"url": "https://x.test/plan", "mime": "text/markdown"}"#,
        T0 + 0.5,
    );
    doc(
        &conn,
        "d02",
        "source_gh",
        Some("source:gh"),
        "README",
        "readme",
        "[]",
        "{}",
        T0,
    );
    doc(
        &conn,
        "d03",
        "event_chat",
        Some("event:chat"),
        "evt",
        r#"{"kind": "message"}"#,
        "[]",
        "{}",
        T0,
    );
    doc(
        &conn,
        "d04",
        "learning_style",
        None,
        "verbosity",
        r#"{"class":"style","key":"verbosity","value":"terse","cue_family":"explicit",
            "evidence":{"type":"episodic","episodic_id":42},"initial_confidence":0.8,
            "observed_at":1600000000.0}"#,
        "[]",
        "{}",
        T0,
    );
    doc(
        &conn,
        "d05",
        "global",
        Some("global"),
        "home",
        "User lives in Lisbon.",
        "[]",
        "{}",
        T0,
    );
    doc(
        &conn,
        "d06",
        "learning_identity",
        Some("learning:identity"),
        "x",
        "not json at all",
        "[]",
        "{}",
        T0 + 6.0,
    );
    doc(
        &conn,
        "d07",
        "user_notes",
        None,
        "user_notes",
        "remember milk",
        "not json",
        "",
        T0,
    );
    doc(
        &conn,
        "d08",
        "learning_tooling",
        Some("learning:tooling"),
        "shell",
        r#"{"class":"tooling","key":"shell","value":{"prefers":"zsh"},"initial_confidence":1.5}"#,
        "[]",
        "{}",
        T0 + 8.0,
    );
    doc(
        &conn,
        "d09",
        "document_blank",
        None,
        "blank",
        "   ",
        "[]",
        "{}",
        T0,
    );
    doc(
        &conn,
        "d10",
        "learning_veto",
        Some("learning:veto"),
        "emoji",
        r#"{"class":"veto","key":"emoji","value":"never","initial_confidence":0.6}"#,
        "[]",
        "{}",
        T0,
    );

    // Two threads interleaved in time.
    turn(&conn, "t-b", 100.0, "user", "b: hello", None);
    turn(&conn, "t-a", 101.0, "user", "a: hi", None);
    turn(
        &conn,
        "t-b",
        102.0,
        "assistant",
        "b: searching",
        Some(r#"[{"name":"search","id":"c1"}]"#),
    );
    turn(
        &conn,
        "t-a",
        103.0,
        "assistant",
        "a: hello back",
        Some("{oops"),
    );
    turn(&conn, "t-a", 103.0, "Tool", "a: tool output", None);
    turn(&conn, "t-a", 104.0, "narrator", "a: aside", None);
    turn(&conn, "t-a", 105.0, "user", "  ", None);

    facet(
        &conn,
        "f1",
        "preference",
        "tone",
        "terse",
        0.9,
        T0,
        "active",
        "pinned",
        Some("style"),
    );
    facet(
        &conn,
        "f2",
        "skill",
        "rust",
        "expert",
        1.2,
        T0,
        "provisional",
        "auto",
        None,
    );
    facet(
        &conn,
        "f3",
        "preference",
        "font",
        "serif",
        0.4,
        T0,
        "dropped",
        "auto",
        None,
    );
    facet(
        &conn,
        "f4",
        "role",
        "job",
        "pilot",
        0.7,
        T0,
        "active",
        "forgotten",
        None,
    );

    let chunks = chunk_store(dir.path());
    std::fs::write(
        dir.path().join("memory_tree/content/e1-1.md"),
        "full second part",
    )
    .unwrap();
    chunk(
        &chunks,
        "k3",
        "email",
        "e1",
        1,
        2_000,
        "second…",
        "[\"inbox\"]",
        Some("e1-1.md"),
    );
    chunk(
        &chunks,
        "k2",
        "email",
        "e1",
        0,
        1_000,
        "first part",
        "[\"inbox\"]",
        Some("gone.md"),
    );
    chunk(&chunks, "k1", "chat", "c1", 0, 3_000, "c: one", "[]", None);
    chunk(
        &chunks, "k4", "chat", "c1", 1, 4_000, "c: two", "[\"dm\"]", None,
    );
    dir
}

/// The checkpoint after document `id` (`Checkpoint` is non-exhaustive, so a
/// caller assigns its fields).
fn after_document(id: &str) -> Checkpoint {
    let mut checkpoint = Checkpoint::default();
    checkpoint.documents = Some(id.to_string());
    checkpoint
}

fn all(ws: &LegacyWorkspace) -> Vec<ImportedItem> {
    ws.items().collect::<Result<_, _>>().expect("import")
}

fn source_id(item: &StoreItem) -> String {
    item.meta().source.id.clone().expect("legacy id")
}

fn find<'a>(items: &'a [ImportedItem], id: &str) -> &'a StoreItem {
    &items
        .iter()
        .find(|imported| source_id(&imported.item) == id)
        .expect("legacy id was imported")
        .item
}

#[test]
fn refuses_a_missing_path() {
    let dir = tempfile::tempdir().unwrap();
    let err = LegacyWorkspace::open(dir.path().join("nope")).unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "{err:?}");
}

#[test]
fn refuses_a_directory_without_either_store() {
    let dir = tempfile::tempdir().unwrap();
    let err = LegacyWorkspace::open(dir.path()).unwrap_err();
    assert!(matches!(err, Error::NotLegacy { .. }), "{err:?}");
    assert!(
        err.to_string()
            .contains("neither memory/memory.db nor a usable memory_tree/chunks.db"),
        "{err}"
    );
}

#[test]
fn refuses_an_unusable_chunk_store_without_a_memory_db() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("memory_tree")).unwrap();
    rusqlite::Connection::open(dir.path().join("memory_tree/chunks.db"))
        .unwrap()
        .execute_batch("CREATE TABLE other (x TEXT);")
        .unwrap();
    let err = LegacyWorkspace::open(dir.path()).unwrap_err();
    assert!(matches!(err, Error::NotLegacy { .. }), "{err:?}");
}

#[test]
fn opens_a_store_with_only_a_chunk_store() {
    // The later v1 engine wrote memory_tree/chunks.db and no memory.db.
    let dir = tempfile::tempdir().unwrap();
    let chunks = chunk_store(dir.path());
    chunk(&chunks, "k1", "chat", "c1", 0, 3_000, "c: one", "[]", None);
    chunk(&chunks, "k2", "email", "e1", 0, 1_000, "hello", "[]", None);
    drop(chunks);

    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    assert!(!ws.has_memory_db());
    assert!(ws.has_chunks());
    let ids: Vec<String> = all(&ws).iter().map(|i| source_id(&i.item)).collect();
    assert_eq!(ids, ["mem_tree_chunks:chat:c1", "mem_tree_chunks:email:e1"]);
    let counts = ws.counts().unwrap();
    assert_eq!(counts.chunks, 2);
    assert_eq!(counts.total(), 2);
}

/// What `items()` yields, counted per section by legacy id and kind.
fn counted(items: &[ImportedItem]) -> LegacyCounts {
    let mut counts = LegacyCounts::default();
    for imported in items {
        let id = source_id(&imported.item);
        let slot = if id.starts_with("graph_") {
            &mut counts.relations
        } else if id.starts_with("file:") {
            &mut counts.files
        } else if id.starts_with("event_log:") {
            &mut counts.events
        } else if id.starts_with("episodic_log:lesson:") {
            &mut counts.lessons
        } else if id.starts_with("mem_tree_chunks:") {
            &mut counts.chunks
        } else if id.starts_with("episodic_log:") {
            &mut counts.conversations
        } else if id.starts_with("user_profile:") {
            &mut counts.profile
        } else if matches!(imported.item, StoreItem::Learning { .. }) {
            &mut counts.learnings
        } else {
            &mut counts.documents
        };
        *slot += 1;
    }
    counts
}

#[test]
fn counts_match_what_the_import_yields() {
    let dir = rich();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let counts = ws.counts().unwrap();
    assert_eq!(counts, counted(&all(&ws)));
    assert!(counts.documents > 0 && counts.learnings > 0 && counts.profile > 0);
    assert!(!counts.is_empty());
}

#[test]
fn counts_match_an_early_store_without_optional_columns() {
    let (dir, conn) = workspace(OLD_MEMORY_DDL);
    conn.execute_batch(
        "INSERT INTO memory_docs VALUES ('d1','document_notes','d1','t','body','chat','normal',
           '[]','{}','core',NULL,1.0,1.0,'');
         INSERT INTO memory_docs VALUES ('d2','learning_style','d2','t','x','chat','normal',
           '[]','{}','core',NULL,1.0,1.0,'');
         INSERT INTO memory_docs VALUES ('d3','event_chat','d3','t','e','chat','normal',
           '[]','{}','core',NULL,1.0,1.0,'');
         INSERT INTO user_profile (facet_id, facet_type, key, value, confidence, first_seen_at,
           last_seen_at) VALUES ('f1','preference','k','v',0.5,1.0,1.0), ('f2','preference','k2',' ',0.5,1.0,1.0);",
    )
    .unwrap();
    drop(conn);
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    assert_eq!(ws.counts().unwrap(), counted(&all(&ws)));
    assert_eq!(ws.counts().unwrap().total(), 3);
}

#[test]
fn an_empty_store_counts_nothing() {
    let (dir, conn) = workspace(support::MEMORY_DDL);
    drop(conn);
    let counts = LegacyWorkspace::open(dir.path()).unwrap().counts().unwrap();
    assert!(counts.is_empty());
}

#[test]
fn refuses_a_file_path() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    std::fs::write(&file, "x").unwrap();
    let err = LegacyWorkspace::open(&file).unwrap_err();
    assert!(matches!(err, Error::NotLegacy { .. }), "{err:?}");
}

#[test]
fn refuses_a_memory_db_that_is_not_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("memory")).unwrap();
    std::fs::write(
        dir.path().join("memory/memory.db"),
        "this is definitely not a sqlite database file, just some text",
    )
    .unwrap();
    let err = LegacyWorkspace::open(dir.path()).unwrap_err();
    assert!(matches!(err, Error::NotLegacy { .. }), "{err:?}");
}

#[test]
fn refuses_a_sqlite_store_of_another_shape() {
    let (dir, _conn) = workspace("CREATE TABLE notes (id TEXT);");
    let err = LegacyWorkspace::open(dir.path()).unwrap_err();
    match err {
        Error::NotLegacy { reason, .. } => assert!(reason.contains("memory_docs"), "{reason}"),
        other => panic!("expected NotLegacy, got {other:?}"),
    }
}

#[test]
fn an_empty_legacy_store_yields_nothing() {
    let (dir, _conn) = workspace(support::MEMORY_DDL);
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    assert!(!ws.has_chunks());
    assert_eq!(ws.items().count(), 0);
}

#[test]
fn yields_sections_and_rows_in_a_fixed_order() {
    let dir = rich();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    assert!(ws.has_chunks());
    let ids: Vec<String> = all(&ws).iter().map(|i| source_id(&i.item)).collect();
    assert_eq!(
        ids,
        [
            "memory_docs:d01",
            "memory_docs:d02",
            "memory_docs:d07",
            "mem_tree_chunks:chat:c1",
            "mem_tree_chunks:email:e1",
            "episodic_log:t-a",
            "episodic_log:t-b",
            "memory_docs:d04",
            "memory_docs:d05",
            "memory_docs:d06",
            "memory_docs:d08",
            "memory_docs:d10",
            "user_profile:f1",
            "user_profile:f2",
        ]
    );
}

#[test]
fn the_page_size_does_not_change_what_is_yielded() {
    let dir = rich();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let paged: Vec<ImportedItem> = ws
        .items()
        .with_page_size(1)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(paged, all(&ws));
    assert_eq!(all(&ws), all(&ws));
}

#[test]
fn every_item_is_a_valid_import_from_this_workspace() {
    let dir = rich();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let workspace_path = ws.path().display().to_string();
    assert!(Path::new(&workspace_path).is_absolute());
    for imported in all(&ws) {
        let meta = imported.item.meta();
        assert_eq!(meta.source.kind, SourceKind::Import);
        assert_eq!(meta.workspace.as_deref(), Some(workspace_path.as_str()));
        imported.item.validate().expect("storable");
    }
}

#[test]
fn maps_document_rows() {
    let dir = rich();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    let StoreItem::Document {
        title,
        body,
        mime,
        meta,
    } = find(&items, "memory_docs:d01")
    else {
        panic!("d01 is a document");
    };
    assert_eq!(title.as_deref(), Some("Plan"));
    assert_eq!(body, &DocumentBody::Text("Ship v2.".into()));
    assert_eq!(mime.as_deref(), Some("text/markdown"));
    assert_eq!(meta.url.as_deref(), Some("https://x.test/plan"));
    assert_eq!(meta.tags, ["work", "ns:document:notes"]);
    let observed = meta.observed_at.unwrap();
    assert_eq!(observed.timestamp(), 1_700_000_000);
    assert_eq!(observed.timestamp_subsec_millis(), 500);

    // A plain Memory::store row with no logical namespace and junk JSON.
    let StoreItem::Document {
        mime, meta, body, ..
    } = find(&items, "memory_docs:d07")
    else {
        panic!("d07 is a document");
    };
    assert_eq!(body, &DocumentBody::Text("remember milk".into()));
    assert_eq!(mime, &None);
    assert_eq!(meta.url, None);
    assert_eq!(meta.tags, ["ns:user_notes"]);
    assert_eq!(
        find(&items, "memory_docs:d02").meta().tags,
        ["ns:source:gh"]
    );
}

#[test]
fn skips_raw_events_and_blank_documents() {
    let dir = rich();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let ids: Vec<String> = all(&ws).iter().map(|i| source_id(&i.item)).collect();
    assert!(!ids.contains(&"memory_docs:d03".to_string()));
    assert!(!ids.contains(&"memory_docs:d09".to_string()));
}

#[test]
fn maps_learning_candidates() {
    let dir = rich();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    // Sanitised `learning_style` with a NULL logical namespace.
    let StoreItem::Learning {
        text,
        kind,
        confidence,
        evidence,
        meta,
    } = find(&items, "memory_docs:d04")
    else {
        panic!("d04 is a learning");
    };
    assert_eq!(text, "verbosity: terse");
    assert_eq!(*kind, LearningKind::Preference);
    assert_eq!(*confidence, 0.8);
    let evidence: serde_json::Value = serde_json::from_str(evidence.as_deref().unwrap()).unwrap();
    assert_eq!(
        evidence,
        serde_json::json!({"type": "episodic", "episodic_id": 42})
    );
    assert_eq!(meta.tags, ["style"]);
    assert_eq!(meta.observed_at.unwrap().timestamp(), 1_600_000_000);

    let StoreItem::Learning {
        text,
        kind,
        confidence,
        evidence,
        meta,
    } = find(&items, "memory_docs:d08")
    else {
        panic!("d08 is a learning");
    };
    assert_eq!(text, r#"shell: {"prefers":"zsh"}"#);
    assert_eq!(*kind, LearningKind::Procedure);
    assert_eq!(*confidence, 1.0, "clamped");
    assert_eq!(evidence, &None);
    assert_eq!(
        meta.observed_at.unwrap().timestamp(),
        1_700_000_008,
        "falls back to updated_at"
    );

    let StoreItem::Learning { kind, .. } = find(&items, "memory_docs:d10") else {
        panic!("d10 is a learning");
    };
    assert_eq!(*kind, LearningKind::Correction);
}

#[test]
fn keeps_unparseable_learnings_and_global_rows_as_text() {
    let dir = rich();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    let StoreItem::Learning {
        text,
        kind,
        confidence,
        meta,
        ..
    } = find(&items, "memory_docs:d06")
    else {
        panic!("d06 is a learning");
    };
    assert_eq!(text, "not json at all");
    assert_eq!(*kind, LearningKind::Other);
    assert_eq!(*confidence, 0.5);
    assert_eq!(meta.tags, ["identity"]);

    let StoreItem::Learning {
        text,
        kind,
        confidence,
        meta,
        ..
    } = find(&items, "memory_docs:d05")
    else {
        panic!("d05 is a learning");
    };
    assert_eq!(text, "User lives in Lisbon.");
    assert_eq!(*kind, LearningKind::Fact);
    assert_eq!(*confidence, 0.5);
    assert_eq!(meta.tags, ["global"]);
}

#[test]
fn maps_episodic_threads_to_conversations() {
    let dir = rich();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    let StoreItem::Conversation { turns, meta } = find(&items, "episodic_log:t-a") else {
        panic!("t-a is a conversation");
    };
    let rendered: Vec<(Role, &str)> = turns.iter().map(|t| (t.role, t.text.as_str())).collect();
    assert_eq!(
        rendered,
        [
            (Role::User, "a: hi"),
            (Role::Assistant, "a: hello back"),
            (Role::Tool, "a: tool output"),
            (Role::User, "a: aside"),
        ]
    );
    assert!(
        turns[1].tool_calls.is_empty(),
        "unparseable tool calls are dropped"
    );
    assert_eq!(turns[0].at.unwrap().timestamp(), 101);
    assert_eq!(meta.thread_id.as_deref(), Some("t-a"));
    assert_eq!(meta.turns, Some(TurnRange { first: 0, last: 3 }));
    assert_eq!(meta.observed_at.unwrap().timestamp(), 104);

    let StoreItem::Conversation { turns, meta } = find(&items, "episodic_log:t-b") else {
        panic!("t-b is a conversation");
    };
    assert_eq!(turns.len(), 2);
    assert_eq!(
        turns[1].tool_calls,
        [ToolCallRef {
            name: "search".into(),
            id: Some("c1".into()),
        }]
    );
    assert_eq!(meta.turns, Some(TurnRange { first: 0, last: 1 }));
}

#[test]
fn maps_live_profile_facets_to_preferences() {
    let dir = rich();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    let StoreItem::Learning {
        text,
        kind,
        confidence,
        evidence,
        meta,
    } = find(&items, "user_profile:f1")
    else {
        panic!("f1 is a learning");
    };
    assert_eq!(text, "tone: terse");
    assert_eq!(*kind, LearningKind::Preference);
    assert_eq!(*confidence, 0.9);
    assert_eq!(evidence.as_deref(), Some(r#"["seg-1"]"#));
    assert_eq!(meta.tags, ["preference", "style"]);
    assert_eq!(meta.observed_at.unwrap().timestamp(), 1_700_000_000);

    let StoreItem::Learning {
        confidence, meta, ..
    } = find(&items, "user_profile:f2")
    else {
        panic!("f2 is a learning");
    };
    assert_eq!(*confidence, 1.0);
    assert_eq!(meta.tags, ["skill"]);

    let ids: Vec<String> = items.iter().map(|i| source_id(&i.item)).collect();
    assert!(!ids.contains(&"user_profile:f3".to_string()), "dropped");
    assert!(!ids.contains(&"user_profile:f4".to_string()), "forgotten");
}

#[test]
fn maps_chunk_sources() {
    let dir = rich();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    let StoreItem::Document { body, meta, .. } = find(&items, "mem_tree_chunks:email:e1") else {
        panic!("email is a document");
    };
    // seq 0 has a missing content file (preview kept); seq 1 reads its file.
    assert_eq!(
        body,
        &DocumentBody::Text("first part\n\nfull second part".into())
    );
    assert_eq!(meta.tags, ["inbox", "source_kind:email"]);
    assert_eq!(meta.observed_at.unwrap().timestamp_millis(), 2_000);
    assert_eq!(meta.thread_id, None);

    let StoreItem::Conversation { turns, meta } = find(&items, "mem_tree_chunks:chat:c1") else {
        panic!("chat is a conversation");
    };
    let texts: Vec<&str> = turns.iter().map(|t| t.text.as_str()).collect();
    assert_eq!(texts, ["c: one", "c: two"]);
    assert!(turns.iter().all(|t| t.role == Role::User));
    assert_eq!(meta.thread_id.as_deref(), Some("c1"));
    assert_eq!(meta.turns, Some(TurnRange { first: 0, last: 1 }));
    assert_eq!(meta.tags, ["dm", "source_kind:chat"]);
}

#[test]
fn resuming_from_any_checkpoint_yields_exactly_the_remainder() {
    let dir = rich();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let everything = all(&ws);
    let from_start: Vec<ImportedItem> = ws
        .items_from(&Checkpoint::default())
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(from_start, everything);
    for (index, imported) in everything.iter().enumerate() {
        let persisted = imported.checkpoint.to_json().unwrap();
        let restored = Checkpoint::from_json(&persisted).unwrap();
        for page_size in [1, 3, 256] {
            let rest: Vec<ImportedItem> = ws
                .items_from(&restored)
                .with_page_size(page_size)
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(rest, everything[index + 1..], "resume after item {index}");
        }
    }
    let last = &everything.last().unwrap().checkpoint;
    assert_eq!(last.documents.as_deref(), Some("d07"));
    assert_eq!(
        last.chunks,
        Some(ChunkCursor {
            source_kind: "email".into(),
            source_id: "e1".into(),
        })
    );
    assert_eq!(last.conversations.as_deref(), Some("t-b"));
    assert_eq!(last.learnings.as_deref(), Some("d10"));
    assert_eq!(last.profile.as_deref(), Some("f2"));
}

#[test]
fn tags_externally_synced_rows_in_every_memory_docs_section() {
    let (dir, conn) = workspace(support::MEMORY_DDL);
    let rows = [
        // (id, namespace, logical, content, taint)
        (
            "e1",
            "document_gmail",
            "document:gmail",
            "Invoice due Friday",
            "external_sync",
        ),
        (
            "e2",
            "learning_style",
            "learning:style",
            r#"{"class":"style","key":"tone","value":"terse"}"#,
            "external_sync",
        ),
        (
            "e3",
            "global",
            "global",
            "Always cc finance",
            "external_sync",
        ),
        (
            "e4",
            "document_web",
            "document:web",
            "Unknown taint",
            "sideloaded",
        ),
        ("e5", "document_web", "document:web", "Blank taint", ""),
        (
            "i1",
            "document_notes",
            "document:notes",
            "My own note",
            "internal",
        ),
        (
            "i2",
            "document_notes",
            "document:notes",
            "Spelled loosely",
            " Internal ",
        ),
    ];
    for (id, namespace, logical, content, taint) in rows {
        doc(
            &conn,
            id,
            namespace,
            Some(logical),
            "",
            content,
            "[]",
            "{}",
            T0,
        );
        conn.execute(
            "UPDATE memory_docs SET taint = ?2 WHERE document_id = ?1",
            rusqlite::params![id, taint],
        )
        .unwrap();
    }
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    let tags = |id: &str| {
        find(&items, &format!("memory_docs:{id}"))
            .meta()
            .tags
            .clone()
    };

    assert_eq!(tags("e1"), ["ns:document:gmail", EXTERNAL_SYNC_TAG]);
    assert!(matches!(
        find(&items, "memory_docs:e2"),
        StoreItem::Learning { .. }
    ));
    assert_eq!(tags("e2"), ["style", EXTERNAL_SYNC_TAG]);
    assert_eq!(tags("e3"), ["global", EXTERNAL_SYNC_TAG]);
    // v1 decodes anything but `internal` as external; so does the importer.
    assert!(tags("e4").contains(&EXTERNAL_SYNC_TAG.to_string()));
    assert!(tags("e5").contains(&EXTERNAL_SYNC_TAG.to_string()));
    assert_eq!(tags("i1"), ["ns:document:notes"]);
    assert_eq!(tags("i2"), ["ns:document:notes"]);
}

#[test]
fn a_store_without_the_taint_column_reads_as_internal() {
    let (dir, conn) = workspace(OLD_MEMORY_DDL);
    conn.execute_batch(
        "INSERT INTO memory_docs (document_id, namespace, key, title, content, source_type,
           priority, tags_json, metadata_json, category, created_at, updated_at, markdown_rel_path)
         VALUES ('a', 'document_old', 'k', 'Old', 'old body', 'doc', 'n', '[]', '{}', 'core', 1, 1, '');",
    )
    .unwrap();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    assert!(items.iter().all(|imported| {
        !imported
            .item
            .meta()
            .tags
            .iter()
            .any(|t| t == EXTERNAL_SYNC_TAG)
    }));
}

#[test]
fn imports_an_early_v1_store_without_optional_columns() {
    let (dir, conn) = workspace(OLD_MEMORY_DDL);
    conn.execute_batch(
        "INSERT INTO memory_docs (document_id, namespace, key, title, content, source_type,
           priority, tags_json, metadata_json, category, created_at, updated_at, markdown_rel_path)
         VALUES
           ('a', 'document_old', 'k', 'Old', 'old body', 'doc', 'n', '[]', '{}', 'core', 1, 1, ''),
           ('b', 'learning_goal', 'g', 'g',
            '{\"class\":\"goal\",\"key\":\"ship\",\"value\":\"v2\"}', 'chat', 'n', '[]', '{}',
            'core', 1, 1, '');
         INSERT INTO episodic_log (session_id, timestamp, role, content)
           VALUES ('s', 1.0, 'user', 'hi');
         INSERT INTO user_profile (facet_id, facet_type, key, value, first_seen_at, last_seen_at)
           VALUES ('p', 'context', 'city', 'Lisbon', 1, 2);",
    )
    .unwrap();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    assert_eq!(items.len(), 4);
    assert_eq!(
        find(&items, "memory_docs:a").meta().tags,
        ["ns:document:old"]
    );
    let StoreItem::Learning {
        text,
        kind,
        confidence,
        ..
    } = find(&items, "memory_docs:b")
    else {
        panic!("b is a learning");
    };
    assert_eq!(text, "ship: v2");
    assert_eq!(*kind, LearningKind::Other);
    assert_eq!(*confidence, 0.5, "no initial_confidence");
    assert!(matches!(
        find(&items, "episodic_log:s"),
        StoreItem::Conversation { .. }
    ));
    let StoreItem::Learning {
        text, confidence, ..
    } = find(&items, "user_profile:p")
    else {
        panic!("p is a learning");
    };
    assert_eq!(text, "city: Lisbon");
    assert_eq!(*confidence, 0.5, "column default");
}

#[test]
fn a_chunk_store_without_its_table_is_skipped() {
    let (dir, _conn) = workspace(support::MEMORY_DDL);
    std::fs::create_dir_all(dir.path().join("memory_tree")).unwrap();
    rusqlite::Connection::open(dir.path().join("memory_tree/chunks.db"))
        .unwrap()
        .execute_batch("CREATE TABLE other (x TEXT);")
        .unwrap();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    assert!(!ws.has_chunks());
    assert_eq!(ws.items().count(), 0);
}

#[test]
fn an_unreadable_chunk_body_fails_once_then_stops() {
    let (dir, _conn) = workspace(support::MEMORY_DDL);
    let chunks = chunk_store(dir.path());
    std::fs::create_dir_all(dir.path().join("memory_tree/content/dir.md")).unwrap();
    chunk(
        &chunks,
        "k",
        "document",
        "x",
        0,
        1,
        "preview",
        "[]",
        Some("dir.md"),
    );
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let mut items = ws.items();
    assert!(matches!(items.next(), Some(Err(Error::Io { .. }))));
    assert!(items.next().is_none());
}

#[test]
fn a_row_sqlite_cannot_decode_is_a_sqlite_error() {
    let (dir, conn) = workspace(support::MEMORY_DDL);
    conn.execute_batch(
        "INSERT INTO user_profile (facet_id, facet_type, key, value, first_seen_at, last_seen_at)
           VALUES (X'00FF', 'context', 'k', 'v', 1, 1);",
    )
    .unwrap();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let mut items = ws.items();
    assert!(matches!(items.next(), Some(Err(Error::Sqlite(_)))));
    assert!(items.next().is_none());
}

// --- migrate: a v1 workspace into an engine, in resumable batches ---

mod migration {
    use super::after_document;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tinymemory_api::conformance::ReferenceEngine;
    use tinymemory_api::{
        EngineDescriptor, EngineHealth, FetchPage, FetchRequest, ForgetReport, ForgetTarget,
        ListPage, ListRequest, MAX_STORE_MANY, MemoryEngine, RecallAnswer, RecallRequest,
        StoreItem, StoreReceipt, async_trait,
    };
    use tinymemory_integrations::import::{
        Checkpoint, Error, LegacyWorkspace, MigrationReport, migrate, migrate_with,
    };

    use super::T0;
    use super::support::{doc, workspace};

    /// More documents than two full `store_many` batches hold.
    const COUNT: usize = 2 * MAX_STORE_MANY + 50;

    /// A workspace of `COUNT` documents, `d000` to `d249` in key order.
    fn documents() -> (tempfile::TempDir, LegacyWorkspace) {
        let (dir, conn) = workspace(super::support::MEMORY_DDL);
        for index in 0..COUNT {
            doc(
                &conn,
                &format!("d{index:03}"),
                "document_notes",
                None,
                &format!("Note {index}"),
                &format!("Body of note {index}."),
                "[]",
                "{}",
                T0 + index as f64,
            );
        }
        drop(conn);
        let legacy = LegacyWorkspace::open(dir.path()).unwrap();
        (dir, legacy)
    }

    fn key(checkpoint: &Checkpoint) -> Option<&str> {
        checkpoint.documents.as_deref()
    }

    /// Delegates to a [`ReferenceEngine`], failing the `fail_on`th
    /// `store_many` call (1-based) without storing anything.
    struct FailingOn {
        inner: ReferenceEngine,
        fail_on: usize,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl MemoryEngine for FailingOn {
        fn descriptor(&self) -> &EngineDescriptor {
            self.inner.descriptor()
        }
        async fn health(&self) -> EngineHealth {
            self.inner.health().await
        }
        async fn recall(&self, req: RecallRequest) -> tinymemory_api::Result<RecallAnswer> {
            self.inner.recall(req).await
        }
        async fn fetch(&self, req: FetchRequest) -> tinymemory_api::Result<FetchPage> {
            self.inner.fetch(req).await
        }
        async fn store(&self, item: StoreItem) -> tinymemory_api::Result<StoreReceipt> {
            self.inner.store(item).await
        }
        async fn store_many(
            &self,
            items: Vec<StoreItem>,
        ) -> tinymemory_api::Result<Vec<StoreReceipt>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.fail_on {
                return Err(tinymemory_api::Error::Unavailable("engine down".into()));
            }
            self.inner.store_many(items).await
        }
        async fn forget(&self, target: ForgetTarget) -> tinymemory_api::Result<ForgetReport> {
            self.inner.forget(target).await
        }
        async fn list(&self, req: ListRequest) -> tinymemory_api::Result<ListPage> {
            self.inner.list(req).await
        }
    }

    #[tokio::test]
    async fn a_full_migration_stores_every_item_in_bounded_batches() {
        let (_dir, legacy) = documents();
        let engine = ReferenceEngine::new();
        let report = migrate(&engine, legacy, None).await.unwrap();
        assert_eq!(
            report,
            MigrationReport {
                stored: COUNT,
                replayed: 0,
                batches: 3,
                checkpoint: after_document("d249"),
            }
        );
        assert_eq!(engine.len(), COUNT);
    }

    #[tokio::test]
    async fn a_second_run_is_all_replays() {
        let (dir, legacy) = documents();
        let engine = ReferenceEngine::new();
        migrate(&engine, legacy, None).await.unwrap();
        let again = migrate(&engine, LegacyWorkspace::open(dir.path()).unwrap(), None)
            .await
            .unwrap();
        assert_eq!((again.stored, again.replayed), (0, COUNT));
        assert_eq!(engine.len(), COUNT, "a re-run replays, it never duplicates");
    }

    #[tokio::test]
    async fn resuming_from_a_checkpoint_stores_only_the_rest() {
        let (_dir, legacy) = documents();
        let mid = legacy.items().nth(149).unwrap().unwrap().checkpoint;
        assert_eq!(key(&mid), Some("d149"));
        let engine = ReferenceEngine::new();
        let report = migrate(&engine, legacy, Some(mid)).await.unwrap();
        assert_eq!((report.stored, report.batches), (COUNT - 150, 1));
        assert_eq!(engine.len(), COUNT - 150);
        assert_eq!(key(&report.checkpoint), Some("d249"));
    }

    #[tokio::test]
    async fn the_callback_sees_each_committed_checkpoint_in_order() {
        let (_dir, legacy) = documents();
        let engine = ReferenceEngine::new();
        let mut seen = Vec::new();
        let report = migrate_with(&engine, legacy, None, |checkpoint: &Checkpoint| {
            seen.push(checkpoint.clone());
        })
        .await
        .unwrap();
        let keys: Vec<Option<&str>> = seen.iter().map(key).collect();
        assert_eq!(keys, [Some("d099"), Some("d199"), Some("d249")]);
        assert_eq!(seen.last(), Some(&report.checkpoint));
    }

    #[tokio::test]
    async fn an_engine_failure_carries_the_last_committed_checkpoint() {
        let (dir, legacy) = documents();
        let engine = FailingOn {
            inner: ReferenceEngine::new(),
            fail_on: 2,
            calls: AtomicUsize::new(0),
        };
        let error = migrate(&engine, legacy, None).await.unwrap_err();
        let checkpoint = error.checkpoint().cloned().unwrap();
        assert_eq!(key(&checkpoint), Some("d099"));
        match &error {
            Error::Engine { source, .. } => {
                assert!(matches!(source, tinymemory_api::Error::Unavailable(_)));
            }
            other => panic!("expected an engine error, got {other}"),
        }
        assert!(error.to_string().contains("engine down"), "{error}");
        assert_eq!(engine.inner.len(), MAX_STORE_MANY);

        let resumed = migrate(
            &engine,
            LegacyWorkspace::open(dir.path()).unwrap(),
            Some(checkpoint),
        )
        .await
        .unwrap();
        assert_eq!(
            (resumed.stored, resumed.replayed),
            (COUNT - MAX_STORE_MANY, 0)
        );
        assert_eq!(engine.inner.len(), COUNT);
    }

    #[tokio::test]
    async fn an_engine_failure_on_the_first_batch_carries_the_starting_checkpoint() {
        let (_dir, legacy) = documents();
        let engine = FailingOn {
            inner: ReferenceEngine::new(),
            fail_on: 1,
            calls: AtomicUsize::new(0),
        };
        let start = after_document("d009");
        let error = migrate(&engine, legacy, Some(start.clone()))
            .await
            .unwrap_err();
        assert_eq!(error.checkpoint(), Some(&start));
    }

    #[tokio::test]
    async fn an_empty_workspace_migrates_nothing() {
        let (dir, conn) = workspace(super::support::MEMORY_DDL);
        drop(conn);
        let engine = ReferenceEngine::new();
        let report = migrate(&engine, LegacyWorkspace::open(dir.path()).unwrap(), None)
            .await
            .unwrap();
        assert_eq!(report, MigrationReport::default());
        assert_eq!(engine.len(), 0);
    }

    #[test]
    fn a_migration_can_run_on_a_spawned_task() {
        fn assert_send<T: Send>(_: &T) {}
        let (_dir, legacy) = documents();
        let engine = ReferenceEngine::new();
        assert_send(&migrate(&engine, legacy, None));
    }
}

#[test]
fn counts_skip_rows_blank_by_unicode_whitespace_as_the_import_does() {
    let (dir, conn) = workspace(support::MEMORY_DDL);
    // A no-break space and an ideographic space: blank to `str::trim`, not
    // to SQLite's own `trim`.
    let blank = "\u{00a0}\u{3000}";
    doc(
        &conn,
        "d1",
        "document_notes",
        None,
        "t",
        blank,
        "[]",
        "{}",
        T0,
    );
    doc(&conn, "d2", "global", None, "t", blank, "[]", "{}", T0);
    turn(&conn, "t-1", 1.0, "user", blank, None);
    facet(
        &conn,
        "f1",
        "preference",
        "k",
        blank,
        0.5,
        T0,
        "active",
        "auto",
        None,
    );
    doc(
        &conn,
        "d3",
        "document_notes",
        None,
        "t",
        "kept",
        "[]",
        "{}",
        T0,
    );
    drop(conn);
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let counts = ws.counts().unwrap();
    assert_eq!(counts, counted(&all(&ws)));
    assert_eq!(counts.total(), 1);
}

#[test]
fn refuses_a_memory_db_that_is_not_a_file_even_beside_a_chunk_store() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("memory/memory.db")).unwrap();
    let chunks = chunk_store(dir.path());
    chunk(&chunks, "k1", "chat", "c1", 0, 3_000, "c: one", "[]", None);
    drop(chunks);
    let err = LegacyWorkspace::open(dir.path()).unwrap_err();
    assert!(matches!(err, Error::NotLegacy { .. }), "{err:?}");
    assert!(
        err.to_string().contains("memory/memory.db is not a file"),
        "{err}"
    );
}

#[test]
fn counts_from_an_older_release_decode_with_missing_sections_zero() {
    let counts: LegacyCounts = serde_json::from_str(r#"{"documents":2}"#).unwrap();
    assert_eq!(counts.documents, 2);
    assert_eq!(counts.total(), 2);
}

#[test]
fn a_counts_total_saturates() {
    let mut counts = LegacyCounts::default();
    counts.documents = u64::MAX;
    counts.chunks = 1;
    assert_eq!(counts.total(), u64::MAX);
}

#[test]
fn chunk_counts_resolve_bodies_as_the_import_does() {
    let dir = tempfile::tempdir().unwrap();
    let chunks = chunk_store(dir.path());
    std::fs::write(
        dir.path().join("memory_tree/content/blank.md"),
        " \u{00a0}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("memory_tree/content/full.md"),
        "the real body",
    )
    .unwrap();
    // A preview with text whose file body is blank: the file wins, skipped.
    chunk(
        &chunks,
        "k1",
        "email",
        "e1",
        0,
        1_000,
        "preview",
        "[]",
        Some("blank.md"),
    );
    // A blank preview whose file body has text: imported.
    chunk(
        &chunks,
        "k2",
        "email",
        "e2",
        0,
        1_000,
        "  ",
        "[]",
        Some("full.md"),
    );
    drop(chunks);
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let ids: Vec<String> = all(&ws).iter().map(|i| source_id(&i.item)).collect();
    assert_eq!(ids, ["mem_tree_chunks:email:e2"]);
    assert_eq!(ws.counts().unwrap().chunks, 1);
}

#[test]
fn a_chunk_store_that_is_not_sqlite_alone_is_not_legacy() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("memory_tree")).unwrap();
    std::fs::write(dir.path().join("memory_tree/chunks.db"), "not a database").unwrap();
    let err = LegacyWorkspace::open(dir.path()).unwrap_err();
    assert!(matches!(err, Error::NotLegacy { .. }), "{err:?}");
}

#[test]
fn blank_rows_never_fill_a_page() {
    let (dir, conn) = workspace(support::MEMORY_DDL);
    for i in 0..3 {
        doc(
            &conn,
            &format!("a{i}"),
            "document_notes",
            None,
            "t",
            " ",
            "[]",
            "{}",
            T0,
        );
        turn(&conn, &format!("s{i}"), 1.0, "user", "\u{00a0}", None);
        facet(
            &conn,
            &format!("b{i}"),
            "preference",
            &format!("k{i}"),
            " ",
            0.5,
            T0,
            "active",
            "auto",
            None,
        );
    }
    doc(
        &conn,
        "z",
        "document_notes",
        None,
        "t",
        "kept",
        "[]",
        "{}",
        T0,
    );
    turn(&conn, "z", 1.0, "user", "kept", None);
    facet(
        &conn,
        "z",
        "preference",
        "kz",
        "kept",
        0.5,
        T0,
        "active",
        "auto",
        None,
    );
    drop(conn);
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let paged: Vec<String> = ws
        .items()
        .with_page_size(1)
        .map(|i| source_id(&i.unwrap().item))
        .collect();
    assert_eq!(paged, ["memory_docs:z", "episodic_log:z", "user_profile:z"]);
    assert_eq!(ws.counts().unwrap().total(), 3);
}

/// A store with extracted events and turn lessons, beside a profile facet.
fn with_events() -> tempfile::TempDir {
    let (dir, conn) = workspace(support::MEMORY_DDL);
    conn.execute_batch(support::EVENTS_DDL).unwrap();
    conn.execute_batch(
        "INSERT INTO event_log (event_id, segment_id, session_id, event_type, content, subject,
           confidence, created_at) VALUES
           ('e2', 'seg', 't-1', 'decision', 'Ship v2 on Friday.', 'release', 0.9, 1700000000.0),
           ('e1', 'seg', 't-1', 'preference', 'Prefers short answers.', NULL, 1.4, 1700000001.0),
           ('e3', 'seg', 't-2', 'commitment', '   ', NULL, 0.5, 1700000002.0),
           ('e4', 'seg', '', 'foresight', 'Will travel in May.', '  ', 0.3, 1700000003.0);",
    )
    .unwrap();
    turn(&conn, "t-1", 100.0, "user", "hello", None);
    turn(&conn, "t-1", 101.0, "assistant", "hi", None);
    conn.execute_batch(
        "UPDATE episodic_log SET lesson = 'Greet back briefly.' WHERE content = 'hi';
         INSERT INTO episodic_log (session_id, timestamp, role, content, lesson)
           VALUES ('t-2', 102.0, 'assistant', 'ok', '  ');",
    )
    .unwrap();
    facet(
        &conn,
        "f1",
        "preference",
        "tone",
        "terse",
        0.9,
        T0,
        "active",
        "auto",
        None,
    );
    dir
}

#[test]
fn maps_extracted_events_to_learnings() {
    let dir = with_events();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    let StoreItem::Learning {
        text,
        kind,
        confidence,
        meta,
        ..
    } = find(&items, "event_log:e2")
    else {
        panic!("an event is a learning");
    };
    assert_eq!(text, "Ship v2 on Friday.");
    assert_eq!(*kind, LearningKind::Fact);
    assert!((confidence - 0.9).abs() < 1e-6);
    assert_eq!(meta.tags, ["event:decision", "subject:release"]);
    assert_eq!(meta.thread_id.as_deref(), Some("t-1"));
    assert_eq!(meta.observed_at.unwrap().timestamp(), 1_700_000_000);

    let StoreItem::Learning {
        kind, confidence, ..
    } = find(&items, "event_log:e1")
    else {
        panic!("an event is a learning");
    };
    assert_eq!(*kind, LearningKind::Preference);
    assert!((confidence - 1.0).abs() < 1e-6, "clamped");

    let StoreItem::Learning { kind, meta, .. } = find(&items, "event_log:e4") else {
        panic!("an event is a learning");
    };
    assert_eq!(*kind, LearningKind::Other);
    assert_eq!(meta.tags, ["event:foresight"]);
    assert_eq!(meta.thread_id, None);

    assert!(
        items.iter().all(|i| source_id(&i.item) != "event_log:e3"),
        "blank skipped"
    );
}

#[test]
fn maps_turn_lessons_to_learnings_in_their_thread() {
    let dir = with_events();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    let lessons: Vec<&ImportedItem> = items
        .iter()
        .filter(|i| source_id(&i.item).starts_with("episodic_log:lesson:"))
        .collect();
    assert_eq!(lessons.len(), 1, "the blank lesson is skipped");
    let StoreItem::Learning {
        text, kind, meta, ..
    } = &lessons[0].item
    else {
        panic!("a lesson is a learning");
    };
    assert_eq!(text, "Greet back briefly.");
    assert_eq!(*kind, LearningKind::Other);
    assert_eq!(meta.tags, ["lesson"]);
    assert_eq!(meta.thread_id.as_deref(), Some("t-1"));
    assert_eq!(meta.observed_at.unwrap().timestamp(), 101);
}

#[test]
fn events_and_lessons_come_last_and_are_counted() {
    let dir = with_events();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    let ids: Vec<String> = items.iter().map(|i| source_id(&i.item)).collect();
    let tail: Vec<&str> = ids[ids.len() - 4..].iter().map(String::as_str).collect();
    assert_eq!(
        tail,
        [
            "event_log:e1",
            "event_log:e2",
            "event_log:e4",
            "episodic_log:lesson:2"
        ]
    );
    let counts = ws.counts().unwrap();
    assert_eq!(counts, counted(&items));
    assert_eq!((counts.events, counts.lessons), (3, 1));
}

#[test]
fn a_checkpoint_from_before_events_resumes_into_them() {
    let dir = with_events();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    // What a host persisted after a complete import by a release without
    // the events and lessons sections.
    let old = Checkpoint::from_json(r#"{"conversations":"t-2","profile":"f1"}"#).unwrap();
    let rest: Vec<String> = ws
        .items_from(&old)
        .map(|i| source_id(&i.unwrap().item))
        .collect();
    assert_eq!(
        rest,
        [
            "event_log:e1",
            "event_log:e2",
            "event_log:e4",
            "episodic_log:lesson:2"
        ]
    );
}

#[test]
fn resuming_mid_lessons_yields_exactly_the_rest() {
    let dir = with_events();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    for (index, imported) in items.iter().enumerate() {
        let rest: Vec<String> = ws
            .items_from(&imported.checkpoint)
            .map(|i| source_id(&i.unwrap().item))
            .collect();
        let expected: Vec<String> = items[index + 1..]
            .iter()
            .map(|i| source_id(&i.item))
            .collect();
        assert_eq!(rest, expected, "after {}", source_id(&imported.item));
    }
}

#[test]
fn a_partial_event_table_is_skipped() {
    let (dir, conn) = workspace(support::MEMORY_DDL);
    conn.execute_batch(
        "CREATE TABLE event_log (event_id TEXT PRIMARY KEY, content TEXT NOT NULL);
         INSERT INTO event_log VALUES ('e1', 'something');",
    )
    .unwrap();
    drop(conn);
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    assert_eq!(ws.items().count(), 0);
    assert_eq!(ws.counts().unwrap().events, 0);
}

#[test]
fn blank_events_and_lessons_never_fill_a_page() {
    let (dir, conn) = workspace(support::MEMORY_DDL);
    conn.execute_batch(support::EVENTS_DDL).unwrap();
    conn.execute_batch(
        "INSERT INTO event_log (event_id, segment_id, session_id, event_type, content,
           confidence, created_at) VALUES
           ('e1', 's', 't', 'fact', ' ', 0.5, 1.0), ('e2', 's', 't', 'fact', 'kept', 0.5, 1.0);",
    )
    .unwrap();
    turn(&conn, "t", 1.0, "assistant", "a", None);
    turn(&conn, "t", 2.0, "assistant", "b", None);
    conn.execute_batch(
        "UPDATE episodic_log SET lesson = char(160) WHERE content = 'a';
         UPDATE episodic_log SET lesson = 'kept lesson' WHERE content = 'b';",
    )
    .unwrap();
    drop(conn);
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let ids: Vec<String> = ws
        .items()
        .with_page_size(1)
        .map(|i| source_id(&i.unwrap().item))
        .filter(|id| !id.starts_with("episodic_log:t"))
        .collect();
    assert_eq!(ids, ["event_log:e2", "episodic_log:lesson:2"]);
    let counts = ws.counts().unwrap();
    assert_eq!((counts.events, counts.lessons), (1, 1));
}

/// A store with graph relations and the two workspace files.
fn with_graph_and_files() -> tempfile::TempDir {
    let (dir, conn) = workspace(support::MEMORY_DDL);
    conn.execute_batch(support::GRAPH_DDL).unwrap();
    conn.execute_batch(
        "INSERT INTO graph_global VALUES ('Priya', 'works_at', 'Acme', '{}', 1700000000.0);
         INSERT INTO graph_global VALUES ('  ', 'knows', 'Arjun', '{}', 1700000000.0);
         INSERT INTO graph_namespace VALUES ('source_gmail', 'Arjun', 'owns', 'launch', '{}', 1700000001.0);
         INSERT INTO graph_namespace VALUES ('source_gmail', 'Launch', '', 'Nov 14', '{}', 1700000002.0);",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("MEMORY_GOALS.md"),
        "# Goals\n\n- [g1] Run a marathon\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("persona")).unwrap();
    std::fs::write(dir.path().join("persona/directives.md"), "  Be brief.  \n").unwrap();
    dir
}

#[test]
fn maps_graph_relations_to_fact_learnings() {
    let dir = with_graph_and_files();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    let StoreItem::Learning {
        text, kind, meta, ..
    } = find(&items, "graph_global:1")
    else {
        panic!("a relation is a learning");
    };
    assert_eq!(text, "Priya works_at Acme");
    assert_eq!(*kind, LearningKind::Fact);
    assert_eq!(meta.tags, ["graph"]);
    assert_eq!(meta.observed_at.unwrap().timestamp(), 1_700_000_000);

    let StoreItem::Learning { text, meta, .. } = find(&items, "graph_namespace:1") else {
        panic!("a relation is a learning");
    };
    assert_eq!(text, "Arjun owns launch");
    assert_eq!(meta.tags, ["graph", "ns:source_gmail"]);
    let StoreItem::Learning { text, .. } = find(&items, "graph_namespace:2") else {
        panic!("a relation is a learning");
    };
    assert_eq!(text, "Launch Nov 14", "a blank predicate is left out");
    assert!(
        items.iter().all(|i| source_id(&i.item) != "graph_global:2"),
        "a relation without a subject is skipped"
    );
}

#[test]
fn maps_the_goals_and_persona_files_to_learnings() {
    let dir = with_graph_and_files();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    let StoreItem::Learning {
        text, kind, meta, ..
    } = find(&items, "file:MEMORY_GOALS.md")
    else {
        panic!("the goals file is a learning");
    };
    assert_eq!(text, "# Goals\n\n- [g1] Run a marathon");
    assert_eq!(*kind, LearningKind::Other);
    assert_eq!(meta.tags, ["goals"]);
    assert!(meta.observed_at.is_some());
    let StoreItem::Learning { text, meta, .. } = find(&items, "file:persona/directives.md") else {
        panic!("the directives are a learning");
    };
    assert_eq!(text, "Be brief.");
    assert_eq!(meta.tags, ["persona"]);
    let ids: Vec<String> = items.iter().map(|i| source_id(&i.item)).collect();
    assert_eq!(
        &ids[ids.len() - 2..],
        ["file:MEMORY_GOALS.md", "file:persona/directives.md"]
    );
}

#[test]
fn graph_and_files_are_counted_and_resume_exactly() {
    let dir = with_graph_and_files();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    let counts = ws.counts().unwrap();
    assert_eq!(counts, counted(&items));
    assert_eq!((counts.relations, counts.files), (3, 2));
    for (index, imported) in items.iter().enumerate() {
        let rest: Vec<String> = ws
            .items_from(&imported.checkpoint)
            .map(|i| source_id(&i.unwrap().item))
            .collect();
        let expected: Vec<String> = items[index + 1..]
            .iter()
            .map(|i| source_id(&i.item))
            .collect();
        assert_eq!(rest, expected, "after {}", source_id(&imported.item));
    }
}

#[test]
fn missing_or_blank_workspace_files_are_skipped() {
    let (dir, conn) = workspace(support::MEMORY_DDL);
    drop(conn);
    std::fs::write(dir.path().join("MEMORY_GOALS.md"), " \n ").unwrap();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    assert_eq!(ws.items().count(), 0);
    assert_eq!(ws.counts().unwrap().files, 0);
}

#[test]
fn an_unreadable_workspace_file_is_an_io_error() {
    let (dir, conn) = workspace(support::MEMORY_DDL);
    drop(conn);
    std::fs::create_dir_all(dir.path().join("MEMORY_GOALS.md")).unwrap();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let err = ws.items().find_map(Result::err).expect("an error");
    assert!(matches!(err, Error::Io { .. }), "{err:?}");
    assert!(ws.counts().is_err());
}

#[test]
fn an_oversized_workspace_file_is_cut_and_tagged() {
    let (dir, conn) = workspace(support::MEMORY_DDL);
    drop(conn);
    // 300 KiB of a two-byte character, so the cut lands mid-character.
    let body = format!("x{}", "é".repeat(150 * 1024));
    std::fs::write(dir.path().join("MEMORY_GOALS.md"), body).unwrap();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let items = all(&ws);
    let StoreItem::Learning { text, meta, .. } = find(&items, "file:MEMORY_GOALS.md") else {
        panic!("the goals file is a learning");
    };
    // "x" then two-byte characters: the cap splits one, which is dropped.
    assert_eq!(text.len(), 256 * 1024 - 1);
    assert!(text.ends_with('é'));
    assert_eq!(meta.tags, ["goals", "truncated"]);
}

#[test]
fn an_oversized_file_with_an_invalid_byte_before_the_cut_is_an_io_error() {
    let (dir, conn) = workspace(support::MEMORY_DDL);
    drop(conn);
    let mut body = b"valid text\xffmore".to_vec();
    body.extend(std::iter::repeat_n(b'a', 300 * 1024));
    std::fs::write(dir.path().join("MEMORY_GOALS.md"), body).unwrap();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let err = ws.items().find_map(Result::err).expect("an error");
    assert!(matches!(err, Error::Io { .. }), "{err:?}");
}

#[test]
fn a_workspace_file_that_is_not_text_is_an_io_error() {
    let (dir, conn) = workspace(support::MEMORY_DDL);
    drop(conn);
    std::fs::write(dir.path().join("MEMORY_GOALS.md"), [0x66, 0xff, 0x66]).unwrap();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let err = ws.items().find_map(Result::err).expect("an error");
    assert!(matches!(err, Error::Io { .. }), "{err:?}");
}

#[test]
fn files_and_graph_pages_honour_the_page_size() {
    let dir = with_graph_and_files();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let paged: Vec<String> = ws
        .items()
        .with_page_size(1)
        .map(|i| source_id(&i.unwrap().item))
        .collect();
    let whole: Vec<String> = all(&ws).iter().map(|i| source_id(&i.item)).collect();
    assert_eq!(paged, whole);
}

/// A workspace with a main store and a profile's suffixed store.
fn with_profile_store() -> tempfile::TempDir {
    let (dir, conn) = workspace(support::MEMORY_DDL);
    doc(
        &conn,
        "d1",
        "document_notes",
        None,
        "Main",
        "main body",
        "[]",
        "{}",
        T0,
    );
    drop(conn);
    std::fs::create_dir_all(dir.path().join("memory-1")).unwrap();
    let profile = rusqlite::Connection::open(dir.path().join("memory-1/memory.db")).unwrap();
    profile.execute_batch(support::MEMORY_DDL).unwrap();
    doc(
        &profile,
        "d1",
        "document_notes",
        None,
        "Coder",
        "coder body",
        "[\"x\"]",
        "{}",
        T0,
    );
    facet(
        &profile,
        "f1",
        "preference",
        "lang",
        "rust",
        0.8,
        T0,
        "active",
        "auto",
        None,
    );
    drop(profile);
    std::fs::create_dir_all(dir.path().join("memory_tree-1/content")).unwrap();
    std::fs::write(dir.path().join("memory_tree-1/content/c.md"), "full chat").unwrap();
    let chunks = rusqlite::Connection::open(dir.path().join("memory_tree-1/chunks.db")).unwrap();
    chunks.execute_batch(support::CHUNKS_DDL).unwrap();
    chunk(
        &chunks,
        "k1",
        "chat",
        "c1",
        0,
        1_000,
        "preview",
        "[]",
        Some("c.md"),
    );
    drop(chunks);
    // Profile-only stores, and look-alikes that are not stores.
    std::fs::create_dir_all(dir.path().join("memory_tree-2")).unwrap();
    std::fs::create_dir_all(dir.path().join("memory_old")).unwrap();
    std::fs::write(dir.path().join("memory-3"), "a file").unwrap();
    std::fs::write(dir.path().join("MEMORY_GOALS.md"), "Run a marathon").unwrap();
    dir
}

#[test]
fn lists_the_profile_store_suffixes() {
    let dir = with_profile_store();
    assert_eq!(
        LegacyWorkspace::store_suffixes(dir.path()).unwrap(),
        ["-1", "-2"]
    );
}

#[test]
fn a_profile_store_yields_its_items_under_prefixed_ids_and_a_store_tag() {
    let dir = with_profile_store();
    let ws = LegacyWorkspace::open_store(dir.path(), "-1").unwrap();
    assert_eq!(ws.store_suffix(), "-1");
    let items = all(&ws);
    let ids: Vec<String> = items.iter().map(|i| source_id(&i.item)).collect();
    assert_eq!(
        ids,
        [
            "memory-1/memory_docs:d1",
            "memory-1/mem_tree_chunks:chat:c1",
            "memory-1/user_profile:f1"
        ],
        "no workspace files from a profile store"
    );
    let StoreItem::Document { meta, .. } = find(&items, "memory-1/memory_docs:d1") else {
        panic!("a document");
    };
    assert_eq!(meta.tags, ["x", "ns:document:notes", "store:memory-1"]);
    let StoreItem::Conversation { turns, meta } = find(&items, "memory-1/mem_tree_chunks:chat:c1")
    else {
        panic!("a conversation");
    };
    assert_eq!(
        turns[0].text, "full chat",
        "bodies resolve in the profile's tree"
    );
    assert!(meta.tags.contains(&"store:memory-1".to_string()));
    let counts = ws.counts().unwrap();
    assert_eq!(counts.total(), 3);
    assert_eq!(counts.files, 0);
}

#[test]
fn the_main_store_is_unchanged_by_profile_stores() {
    let dir = with_profile_store();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    assert_eq!(ws.store_suffix(), "");
    let ids: Vec<String> = all(&ws).iter().map(|i| source_id(&i.item)).collect();
    assert_eq!(ids, ["memory_docs:d1", "file:MEMORY_GOALS.md"]);
    let main = find(&all(&ws), "memory_docs:d1").meta().tags.clone();
    assert!(main.iter().all(|t| !t.starts_with("store:")), "{main:?}");
}

#[test]
fn an_empty_profile_tree_or_a_bad_suffix_is_not_a_store() {
    let dir = with_profile_store();
    let err = LegacyWorkspace::open_store(dir.path(), "-2").unwrap_err();
    assert!(matches!(err, Error::NotLegacy { .. }), "{err:?}");
    for bad in ["../x", "-", "1", "-a/b"] {
        let err = LegacyWorkspace::open_store(dir.path(), bad).unwrap_err();
        assert!(matches!(err, Error::NotLegacy { .. }), "{bad}: {err:?}");
    }
}

/// A workspace holding both connector syncs and the data that must stay.
fn with_connector_syncs() -> tempfile::TempDir {
    let (dir, conn) = workspace(&format!("{}{}", support::MEMORY_DDL, support::GRAPH_DDL));
    let add = |id: &str, ns: &str, logical: &str, content: &str| {
        doc(&conn, id, ns, Some(logical), "t", content, "[]", "{}", T0);
    };
    add("c01", "skill-gmail", "skill-gmail", "composio mail");
    add("c02", "source_gmail_c1", "source:gmail:c1", "connector doc");
    add("c03", "document_notes", "document:notes", "my notes");
    add("c04", "global", "global", "agent flow note");
    conn.execute(
        "UPDATE memory_docs SET taint = 'external_sync' WHERE document_id IN ('c01', 'c04')",
        [],
    )
    .unwrap();
    facet(
        &conn,
        "skill-gmail-c1-name",
        "identity",
        "skill:gmail:name",
        "Ann",
        0.9,
        T0,
        "active",
        "auto",
        None,
    );
    facet(
        &conn,
        "f-normal",
        "preference",
        "tone",
        "terse",
        0.9,
        T0,
        "active",
        "auto",
        None,
    );
    for (ns, object) in [
        ("skill-slack", "a"),
        ("source:notion:c", "b"),
        ("source_notion_c", "c"),
        ("document:notes", "d"),
    ] {
        conn.execute(
            "INSERT INTO graph_namespace (namespace, subject, predicate, object, attrs_json, updated_at) \
             VALUES (?1, 's', 'p', ?2, '{}', 1.0)",
            rusqlite::params![ns, object],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO graph_global (subject, predicate, object, attrs_json, updated_at) \
         VALUES ('s', 'p', 'g', '{}', 1.0)",
        [],
    )
    .unwrap();
    let chunks = chunk_store(dir.path());
    for (id, kind, source) in [
        ("k01", "email", "thread-1"),
        ("k02", "chat", "slack:conn1"),
        ("k03", "document", "notion:conn1:page"),
        ("k04", "document", "github:c:issue"),
        ("k05", "document", "linear:c:i"),
        ("k06", "document", "clickup:c:t"),
        ("k07", "document", "gmail:c:m"),
        ("k08", "document", "other-owner-sync"),
        ("k09", "document", "mem_src:folder"),
        ("k10", "chat", "conversations:agent"),
    ] {
        chunk(&chunks, id, kind, source, 0, 1_000, "text", "[]", None);
    }
    chunks
        .execute(
            "UPDATE mem_tree_chunks SET owner = 'jira-sync:conn1' WHERE id = 'k08'",
            [],
        )
        .unwrap();
    dir
}

fn legacy_ids(ws: &LegacyWorkspace) -> Vec<String> {
    all(ws).iter().map(|i| source_id(&i.item)).collect()
}

#[test]
fn connector_syncs_are_imported_unless_skipped() {
    let dir = with_connector_syncs();
    let ws = LegacyWorkspace::open(dir.path()).unwrap();
    let ids = legacy_ids(&ws);
    for id in [
        "memory_docs:c01",
        "memory_docs:c02",
        "user_profile:skill-gmail-c1-name",
        "mem_tree_chunks:email:thread-1",
        "mem_tree_chunks:document:other-owner-sync",
    ] {
        assert!(
            ids.iter().any(|i| i == id),
            "{id} missing by default: {ids:?}"
        );
    }
    assert_eq!(ws.counts().unwrap(), counted(&all(&ws)));
}

#[test]
fn skip_connector_syncs_drops_exactly_the_connector_rows() {
    let dir = with_connector_syncs();
    let ws = LegacyWorkspace::open(dir.path())
        .unwrap()
        .skip_connector_syncs(true);
    let ids = legacy_ids(&ws);
    let mut expected = vec![
        "memory_docs:c03",
        "memory_docs:c04", // global with external_sync taint stays
        "user_profile:f-normal",
        "graph_namespace:4",
        "graph_global:1",
        "mem_tree_chunks:chat:conversations:agent",
        "mem_tree_chunks:document:mem_src:folder",
    ];
    let mut got: Vec<&str> = ids.iter().map(String::as_str).collect();
    expected.sort_unstable();
    got.sort_unstable();
    assert_eq!(got, expected);
    let counts = ws.counts().unwrap();
    assert_eq!(counts, counted(&all(&ws)));
    assert_eq!(counts.total(), expected.len() as u64);
}

#[test]
fn skipping_connector_syncs_keeps_resumption_exact() {
    let dir = with_connector_syncs();
    let ws = LegacyWorkspace::open(dir.path())
        .unwrap()
        .skip_connector_syncs(true);
    let all_items = all(&ws);
    for (n, imported) in all_items.iter().enumerate() {
        let rest: Vec<String> = ws
            .items_from(&imported.checkpoint)
            .map(|i| source_id(&i.unwrap().item))
            .collect();
        let want: Vec<String> = all_items[n + 1..]
            .iter()
            .map(|i| source_id(&i.item))
            .collect();
        assert_eq!(rest, want);
    }
    let small: Vec<String> = ws
        .items()
        .with_page_size(1)
        .map(|i| source_id(&i.unwrap().item))
        .collect();
    assert_eq!(small, legacy_ids(&ws));
}

#[test]
fn the_owner_rule_needs_the_owner_column() {
    // A chunk store without `owner` cannot match on it; the other rules hold.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("memory_tree/content")).unwrap();
    let chunks = rusqlite::Connection::open(dir.path().join("memory_tree/chunks.db")).unwrap();
    chunks
        .execute_batch(
            "CREATE TABLE mem_tree_chunks (id TEXT PRIMARY KEY, source_kind TEXT NOT NULL,
               source_id TEXT NOT NULL, timestamp_ms INTEGER NOT NULL, tags_json TEXT NOT NULL,
               content TEXT NOT NULL, seq_in_source INTEGER NOT NULL);
             INSERT INTO mem_tree_chunks VALUES ('a', 'document', 'x-sync', 1, '[]', 'one', 0);
             INSERT INTO mem_tree_chunks VALUES ('b', 'email', 'y', 1, '[]', 'two', 0);",
        )
        .unwrap();
    drop(chunks);
    let ws = LegacyWorkspace::open(dir.path())
        .unwrap()
        .skip_connector_syncs(true);
    assert_eq!(legacy_ids(&ws), ["mem_tree_chunks:document:x-sync"]);
    assert_eq!(ws.counts().unwrap().chunks, 1);
}
