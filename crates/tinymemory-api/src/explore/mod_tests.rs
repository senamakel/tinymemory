//! Facet values and narrowing, request validation, and the listing-based
//! defaults over a paging test engine.

use super::*;

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use crate::consolidate::Consolidation;
use crate::engine::{EngineDescriptor, EngineHealth};
use crate::item::{StoreItem, StoreReceipt};
use crate::meta::{SourceRef, ToolCallRef};
use crate::query::{
    FetchPage, FetchRequest, ForgetReport, ForgetTarget, ListPage, RecallAnswer, RecallRequest,
};

/// Lists fixed hits two per page, counting the pages read.
struct Paging {
    descriptor: EngineDescriptor,
    hits: Vec<Hit>,
    pages: AtomicUsize,
    /// Every forget target, in order.
    forgets: Mutex<Vec<ForgetTarget>>,
}

impl Paging {
    fn new(hits: Vec<Hit>) -> Self {
        Self {
            descriptor: EngineDescriptor {
                id: "paging",
                label: "Paging",
                description: "A test engine.",
                hosted: false,
                needs_endpoint: false,
                needs_key: false,
                default_endpoint: None,
                fetch_modes: Vec::new(),
                consolidation: Consolidation::None,
            },
            hits,
            pages: AtomicUsize::new(0),
            forgets: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl MemoryEngine for Paging {
    fn descriptor(&self) -> &EngineDescriptor {
        &self.descriptor
    }
    async fn health(&self) -> EngineHealth {
        EngineHealth::Ok
    }
    async fn recall(&self, _: RecallRequest) -> Result<RecallAnswer> {
        Err(Error::Unsupported("recall".into()))
    }
    async fn fetch(&self, _: FetchRequest) -> Result<FetchPage> {
        Err(Error::Unsupported("fetch".into()))
    }
    async fn store(&self, _: StoreItem) -> Result<StoreReceipt> {
        Err(Error::Unsupported("store".into()))
    }
    async fn forget(&self, target: ForgetTarget) -> Result<ForgetReport> {
        let forgotten = match &target {
            ForgetTarget::Ids(ids) => ids.len(),
            ForgetTarget::Filter(_) => 0,
        };
        self.forgets.lock().unwrap().push(target);
        Ok(ForgetReport { forgotten })
    }
    async fn list(&self, req: ListRequest) -> Result<ListPage> {
        self.pages.fetch_add(1, Ordering::SeqCst);
        let start: usize = req.cursor.as_deref().map_or(0, |c| c.parse().unwrap());
        let matching: Vec<&Hit> = self
            .hits
            .iter()
            .filter(|hit| req.filter.matches(hit.kind, &hit.meta))
            .collect();
        let end = (start + req.limit.min(2)).min(matching.len());
        Ok(ListPage {
            items: matching[start..end].iter().map(|h| (*h).clone()).collect(),
            next_cursor: (end < matching.len()).then(|| end.to_string()),
        })
    }
}

fn hit(id: &str, kind: ItemKind, meta: MemoryMeta) -> Hit {
    Hit {
        id: ItemId::new(id),
        kind,
        text: id.to_string(),
        meta,
        score: 0.0,
        confidence: None,
    }
}

fn folder_doc(id: &str, folder: &str, tags: &[&str]) -> Hit {
    hit(
        id,
        ItemKind::Document,
        MemoryMeta {
            folder: Some(folder.into()),
            file_path: Some(format!("{folder}/{id}.md")),
            source: SourceRef {
                kind: SourceKind::Folder,
                id: Some("src-1".into()),
            },
            tags: tags.iter().map(|t| t.to_string()).collect(),
            ..MemoryMeta::default()
        },
    )
}

fn fixture() -> Vec<Hit> {
    vec![
        folder_doc("a", "/notes", &["rust"]),
        folder_doc("b", "/notes", &["rust", "async"]),
        folder_doc("c", "/notes/deep", &[]),
        hit(
            "d",
            ItemKind::Learning,
            MemoryMeta {
                tool_call: Some(ToolCallRef {
                    name: "memory".into(),
                    id: None,
                }),
                ..MemoryMeta::default()
            },
        ),
        hit(
            "e",
            ItemKind::Conversation,
            MemoryMeta {
                thread_id: Some("t-1".into()),
                source: SourceRef {
                    kind: SourceKind::Conversation,
                    id: None,
                },
                ..MemoryMeta::default()
            },
        ),
    ]
}

/// Every facet. [`exhaustive`] fails to compile when a facet is added and
/// not listed here.
const FACETS: [Facet; 14] = [
    Facet::Kind,
    Facet::Source,
    Facet::SourceId,
    Facet::Workspace,
    Facet::Folder,
    Facet::FilePath,
    Facet::Language,
    Facet::Repo,
    Facet::Url,
    Facet::Thread,
    Facet::Agent,
    Facet::ToolCall,
    Facet::Tag,
    Facet::Namespace,
];

fn exhaustive(facet: Facet) {
    match facet {
        Facet::Kind
        | Facet::Source
        | Facet::SourceId
        | Facet::Workspace
        | Facet::Folder
        | Facet::FilePath
        | Facet::Language
        | Facet::Repo
        | Facet::Url
        | Facet::Thread
        | Facet::Agent
        | Facet::ToolCall
        | Facet::Tag
        | Facet::Namespace => {}
    }
}

#[test]
fn every_facet_round_trips_its_wire_name() {
    for facet in FACETS {
        exhaustive(facet);
        let json = serde_json::to_value(facet).unwrap();
        assert_eq!(json, serde_json::json!(facet.as_str()));
        assert_eq!(serde_json::from_value::<Facet>(json).unwrap(), facet);
    }
}

#[test]
fn a_narrowed_filter_admits_exactly_the_items_with_that_value() {
    let items = fixture();
    for facet in FACETS {
        for item in &items {
            for value in facet.values(item.kind, &item.meta) {
                let mut filter = MetaFilter::default();
                facet.narrow(&mut filter, &value).unwrap();
                for other in &items {
                    let carries = facet.values(other.kind, &other.meta).contains(&value)
                        || (matches!(facet, Facet::Folder | Facet::FilePath)
                            && filter.matches(other.kind, &other.meta));
                    assert_eq!(
                        filter.matches(other.kind, &other.meta),
                        carries,
                        "{} = {value} on {}",
                        facet.as_str(),
                        other.id.as_str()
                    );
                }
            }
        }
    }
}

#[test]
fn folders_narrow_by_prefix() {
    let mut filter = MetaFilter::default();
    Facet::Folder.narrow(&mut filter, "/notes").unwrap();
    let deep = folder_doc("c", "/notes/deep", &[]);
    assert!(filter.matches(deep.kind, &deep.meta));
}

#[test]
fn narrowing_refuses_blank_and_unknown_values() {
    let mut filter = MetaFilter::default();
    for (facet, value) in [
        (Facet::Workspace, "  "),
        (Facet::Kind, "memo"),
        (Facet::Source, "carrier-pigeon"),
    ] {
        assert!(
            matches!(
                facet.narrow(&mut filter, value),
                Err(Error::InvalidRequest(_))
            ),
            "{} {value:?}",
            facet.as_str()
        );
    }
    assert!(filter.is_empty(), "a refused value narrows nothing");
}

#[test]
fn explore_limits_are_checked() {
    let mut req = ExploreRequest::new(Facet::Kind, 0);
    assert!(req.validate().is_err());
    req.limit = MAX_BUCKETS + 1;
    assert!(req.validate().is_err());
    req.limit = 10;
    req.scan_limit = 0;
    assert!(req.validate().is_err());
    req.scan_limit = MAX_SCAN_LIMIT + 1;
    assert!(req.validate().is_err());
    req.scan_limit = 1;
    assert!(req.validate().is_ok());
    let parsed: ExploreRequest =
        serde_json::from_value(serde_json::json!({ "facet": "folder", "limit": 5 })).unwrap();
    assert_eq!(parsed.scan_limit, DEFAULT_SCAN_LIMIT);
}

#[test]
fn get_ids_are_checked() {
    let blank = GetRequest {
        ids: vec![ItemId::new(" ")],
        reach: None,
    };
    let none = GetRequest {
        ids: Vec::new(),
        reach: None,
    };
    let many = GetRequest {
        ids: (0..=MAX_GET_IDS)
            .map(|i| ItemId::new(i.to_string()))
            .collect(),
        reach: None,
    };
    for req in [blank, none, many] {
        assert!(matches!(req.validate(), Err(Error::InvalidRequest(_))));
    }
}

#[tokio::test]
async fn explore_counts_every_page_largest_first() {
    let engine = Paging::new(fixture());
    let page = engine
        .explore(ExploreRequest::new(Facet::Kind, 10))
        .await
        .unwrap();
    assert_eq!(page.facet, Facet::Kind);
    assert_eq!(
        page.buckets,
        vec![
            FacetBucket {
                value: "document".into(),
                count: 3
            },
            FacetBucket {
                value: "conversation".into(),
                count: 1
            },
            FacetBucket {
                value: "learning".into(),
                count: 1
            },
        ]
    );
    assert_eq!((page.total, page.missing, page.more_buckets), (5, 0, 0));
    assert!(!page.truncated);
    assert_eq!(
        engine.pages.load(Ordering::SeqCst),
        3,
        "five items, two a page"
    );
}

#[tokio::test]
async fn explore_counts_missing_values_and_multi_valued_tags() {
    let engine = Paging::new(fixture());
    let page = engine
        .explore(ExploreRequest::new(Facet::Tag, 10))
        .await
        .unwrap();
    let counts: Vec<(&str, u64)> = page
        .buckets
        .iter()
        .map(|b| (b.value.as_str(), b.count))
        .collect();
    assert_eq!(counts, [("rust", 2), ("async", 1)]);
    assert_eq!(page.total, 5);
    assert_eq!(page.missing, 3, "c, d and e carry no tag");
}

#[tokio::test]
async fn explore_respects_the_filter_and_the_bucket_limit() {
    let engine = Paging::new(fixture());
    let mut req = ExploreRequest::new(Facet::Folder, 1);
    req.filter = MetaFilter::kinds([ItemKind::Document]);
    let page = engine.explore(req).await.unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(
        page.buckets,
        vec![FacetBucket {
            value: "/notes".into(),
            count: 2
        }]
    );
    assert_eq!(page.more_buckets, 1, "/notes/deep was left out");
}

#[tokio::test]
async fn explore_stops_at_the_scan_limit_and_says_so() {
    let engine = Paging::new(fixture());
    let mut req = ExploreRequest::new(Facet::Kind, 10);
    req.scan_limit = 3;
    let page = engine.explore(req).await.unwrap();
    assert!(page.truncated);
    assert_eq!(page.total, 3);
}

#[tokio::test]
async fn explore_refuses_an_invalid_request_before_listing() {
    let engine = Paging::new(fixture());
    let error = engine
        .explore(ExploreRequest::new(Facet::Kind, 0))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::InvalidRequest(_)));
    assert_eq!(engine.pages.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn get_returns_named_items_in_request_order_and_stops_early() {
    let engine = Paging::new(fixture());
    let hits = engine
        .get(GetRequest {
            ids: vec![ItemId::new("b"), ItemId::new("missing"), ItemId::new("a")],
            reach: None,
        })
        .await
        .unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, ["b", "a"]);

    let engine = Paging::new(fixture());
    engine
        .get(GetRequest {
            ids: vec![ItemId::new("a")],
            reach: None,
        })
        .await
        .unwrap();
    assert_eq!(
        engine.pages.load(Ordering::SeqCst),
        1,
        "found on the first page, so no more are read"
    );
}

#[test]
fn namespace_facet_groups_by_node_and_narrows_to_exactly_it() {
    let mut meta = MemoryMeta::default();
    assert_eq!(Facet::Namespace.values(ItemKind::Learning, &meta), ["root"]);
    meta.namespace = "team:acme/agent:writer".parse().unwrap();
    assert_eq!(
        Facet::Namespace.values(ItemKind::Learning, &meta),
        ["team:acme/agent:writer"]
    );
    let mut filter = MetaFilter::default();
    Facet::Namespace
        .narrow(&mut filter, "team:acme/agent:writer")
        .unwrap();
    assert!(filter.matches(ItemKind::Learning, &meta));
    meta.namespace = "team:acme".parse().unwrap();
    assert!(
        !filter.matches(ItemKind::Learning, &meta),
        "exactly that node"
    );
    assert!(Facet::Namespace.narrow(&mut filter, "nope").is_err());
}

#[tokio::test]
async fn get_leaves_out_ids_beyond_the_reach() {
    let engine = Paging::new(fixture());
    let hits = engine
        .get(GetRequest {
            ids: vec![ItemId::new("a")],
            reach: Some(Reach::exact("agent:other".parse().unwrap())),
        })
        .await
        .unwrap();
    assert!(hits.is_empty());
}

/// Two items at sibling agents, and one at the root.
fn agents() -> Vec<Hit> {
    let at = |id: &str, namespace: &str| {
        hit(
            id,
            ItemKind::Learning,
            MemoryMeta {
                namespace: namespace.parse().unwrap(),
                ..MemoryMeta::default()
            },
        )
    };
    vec![
        at("mine", "agent:ann"),
        at("theirs", "agent:anna"),
        at("shared", ""),
    ]
}

#[test]
fn forget_within_ids_are_checked_and_deduplicated() {
    assert!(matches!(
        forget_within_ids(Vec::new()),
        Err(Error::InvalidRequest(_))
    ));
    assert_eq!(
        forget_within_ids(vec![ItemId::new("a"), ItemId::new(" ")]),
        Err(Error::InvalidRequest("an id must not be blank".to_string()))
    );
    assert_eq!(
        forget_within_ids(vec![ItemId::new("b"), ItemId::new("a"), ItemId::new("b")]).unwrap(),
        vec![ItemId::new("b"), ItemId::new("a")]
    );
}

#[tokio::test]
async fn forget_within_forgets_only_the_ids_inside_the_reach() {
    let engine = Paging::new(agents());
    let report = engine
        .forget_within(
            vec![
                ItemId::new("mine"),
                ItemId::new("theirs"),
                ItemId::new("gone"),
            ],
            Reach::exact("agent:ann".parse().unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(report.forgotten, 1);
    assert_eq!(
        *engine.forgets.lock().unwrap(),
        vec![ForgetTarget::Ids(vec![ItemId::new("mine")])],
        "only the id found in reach is forgotten"
    );
}

#[tokio::test]
async fn forget_within_sends_no_forget_when_nothing_is_in_reach() {
    let engine = Paging::new(agents());
    let report = engine
        .forget_within(
            vec![ItemId::new("theirs")],
            Reach::subtree("agent:ann".parse().unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(report, ForgetReport::default());
    assert!(engine.forgets.lock().unwrap().is_empty());
}

#[tokio::test]
async fn forget_within_reads_ids_back_in_get_sized_batches() {
    let mut hits = agents();
    hits.extend((0..MAX_GET_IDS).map(|i| {
        hit(
            &format!("n{i}"),
            ItemKind::Learning,
            MemoryMeta {
                namespace: "agent:ann".parse().unwrap(),
                ..MemoryMeta::default()
            },
        )
    }));
    let engine = Paging::new(hits);
    let mut ids: Vec<ItemId> = (0..MAX_GET_IDS)
        .map(|i| ItemId::new(format!("n{i}")))
        .collect();
    ids.push(ItemId::new("theirs"));
    let report = engine
        .forget_within(ids, Reach::of("agent:ann".parse().unwrap()))
        .await
        .unwrap();
    assert_eq!(report.forgotten, MAX_GET_IDS);
}

#[tokio::test]
async fn forget_within_refuses_no_ids_before_reading() {
    let engine = Paging::new(agents());
    let error = engine
        .forget_within(Vec::new(), Reach::default())
        .await
        .expect_err("no ids");
    assert!(matches!(error, Error::InvalidRequest(_)));
    assert_eq!(engine.pages.load(Ordering::SeqCst), 0);
}
