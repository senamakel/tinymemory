# Namespaces

Memory is a **tree of nodes**. The root holds what every agent shares; below it
sit agents, teams, users, workspaces and projects, nested as deep as a host
needs. Every stored item lives at exactly one node. A reader names a `Reach`
saying which nodes it sees. All of it is in `tinymemory_api::namespace`.

Inside a node each item kind (learnings, documents, conversations) is kept
apart, so an engine can hold, recall and erase each on its own
([cortex.md](cortex.md) maps this onto scopes).

## Syntax

A `Namespace` is the path from the root, written as `/`-separated
`kind:id` segments:

```text
root                              the root (the empty path)
agent:researcher
team:acme/agent:writer
team:acme/agent:writer/agent:helper
```

- `""` (after trimming) and `root` both parse to the root; the root always
  prints as `root`.
- A segment is `kind:id`, split at the first `:`. A missing `:` or an unknown
  kind is `Error::InvalidRequest`.
- On the wire (serde) a namespace is that string. `MemoryMeta` omits it when
  it is the root, and old envelopes without it read as root.

### Segment kinds

| `SegmentKind` | Prefix in a path | Names |
| --- | --- | --- |
| `Agent` | `agent` | An agent, or a sub-agent nested under its parent |
| `Team` | `team` | A team of agents sharing memory |
| `User` | `user` | A human user |
| `Workspace` | `ws` | A shared workspace |
| `Project` | `project` | A project |
| `Source` | `source` | A knowledge source type (`source:pdf`): where the brain keeps each source's documents (see [lifecycle.md](lifecycle.md)) |
| `Service` | `service` | One automation, such as a workflow (`service:newsletter`), whose memory is its own |

`SegmentKind` is `#[non_exhaustive]`: a host matching on it needs a wildcard
arm.

`SegmentKind::as_str()` gives the path prefix (`ws` for `Workspace`). Note
that the enum's own serde form is `snake_case` of the variant, so
`Workspace` serialises as `"workspace"`; the `ws` prefix is only the
namespace path spelling.

### Limits

| Limit | Value | Error when exceeded |
| --- | --- | --- |
| Depth | at most 8 segments (`Namespace::new`, parsing) | `InvalidRequest("a namespace nests at most 8 deep")` |
| Segment id length | 1 to 128 characters | `InvalidRequest` |
| Segment id charset | `A-Z a-z 0-9 _ -` | `InvalidRequest` |

Because `:` and `/` are outside the charset, an id can never be confused
with the path syntax.

### Constructing

| Constructor | Behaviour |
| --- | --- |
| `Namespace::ROOT` / `Namespace::default()` | The root. |
| `Namespace::new(Vec<Segment>)` | Checks depth only. |
| `"team:acme/agent:writer".parse::<Namespace>()` | Parses and checks every segment. |
| `Namespace::agent("writer")` | One agent directly under the root, id [sanitised](#sanitising-host-ids). |
| `Segment::new(kind, id)` | Checks the id; rejects an invalid one. |
| `Segment::sanitized(kind, raw)` | Never fails; see below. |

Accessors: `is_root()`, `depth()` (root is 0), `segments()`; `Segment` has
`kind()` and `id()`.

### Sanitising host ids

Host identifiers (an email, a UUID with unusual characters, a display name)
rarely fit the charset. `Segment::sanitized(kind, raw)` maps any string onto a
valid id:

- a valid `raw` is kept unchanged;
- otherwise each illegal character becomes `-`, the result is cut to 119
  characters, and `-` plus the 8-hex-digit FNV-1a hash of the **original**
  is appended, so two different raw ids that clean to the same text stay
  distinct;
- an empty `raw` becomes `_`.

```text
"writer"            → agent:writer
"o'neil@acme.com"   → agent:o-neil-acme-com-<8 hex of the original>
""                  → agent:_
```

The hash is a stable, dependency-free disambiguator, not a security
boundary. (The empty-input result `_` is also a valid raw id, so `""` and
`"_"` produce the same segment.)

## Placement

An item's node is `MemoryMeta::namespace` (default: root). Placing an item is
just setting that field on the `StoreItem`'s metadata before `store`.

The namespace is part of the item's [fingerprint](operations.md#idempotency-and-fingerprints)
(a root namespace is not serialised, so root fingerprints are unchanged). So
the **same text learned by two agents is two items**, one per node, and each
can be forgotten on its own.

## Reach

```rust
pub struct Reach { pub at: Namespace, pub inherit: bool, pub descendants: bool }
```

| Field | Default | Meaning |
| --- | --- | --- |
| `at` | root | The node read from |
| `inherit` | `true` | Also read every ancestor of `at` (so an agent sees what its team and the root share) |
| `descendants` | `false` | Also read everything below `at` |

`Reach::admits(ns)` is true when `ns == at`, or `inherit` and `ns` is an
ancestor of `at`, or `descendants` and `ns` lies below `at` with no `service:`
segment between them. **A sibling is never admitted**: one agent's memory is
invisible to another unless written to a node both inherit.

**A service node is a sandbox.** A read of everything below a node never
enters a `service:` node below it, so a workflow's memory stays out of chat
packs, brain reads, holistic recall and "search everything". Only a reach at
the service node, or inside it, reads it. No reach at all (`MetaFilter::reach`
or `GetRequest::reach` left `None`) reads as `Reach::subtree(Namespace::ROOT)`
(`Reach::admitted_by`), so it skips sandboxes too. Ids are the exception:
`ForgetTarget::Ids` forgets an item wherever it lives (`forget_within` does
not: it looks only inside its reach).

| Constructor | `inherit` | `descendants` | Sees |
| --- | --- | --- | --- |
| `Reach::of(at)` | yes | no | `at` and its ancestors: an agent's ordinary reach |
| `Reach::exact(at)` | no | no | exactly one node |
| `Reach::subtree(at)` | no | yes | `at` and everything below it, no ancestors |
| `Reach::default()` | yes | no | `Reach::of(root)`: **only the root** |

`Reach::within(outer)` is the confinement test a host applies to a
caller-supplied reach: true when every namespace `self` admits, `outer`
admits too (so `self` may be used as given; any other would widen `outer`).
`at` must be in `outer`; with `inherit`, every ancestor of `at` must be in
`outer` too (inheriting above a subtree's top escapes it); with
`descendants`, `outer` must read descendants and `at` must lie at or below
`outer.at`, because the descendants of an ancestor include its siblings. A
`service:` sandbox below `outer.at` stays out of `outer`'s descendants, as in
`admits`. A reach at the deepest legal node (depth 8) has no descendants, so
`descendants` there counts as exact.

`Reach::nodes()` lists the nodes read exactly, root first (`at` and, when
`inherit`, its ancestors). Descendants cannot be enumerated from the reach;
an engine reads them as one subtree below `at`.

Two different notions of "everything":

- `MetaFilter::reach == None` reads like `Reach::subtree(Namespace::ROOT)`:
  **every namespace except a `service:` sandbox**.
- `Reach::default()` reads **only the root**. Reading a service sandbox takes
  a reach at (or inside) its node; only `ForgetTarget::Ids` reaches every
  namespace.

### Worked example

```text
root                                   R   shared by everyone
└── team:acme                          T   shared by the team
    ├── agent:writer                   W
    │   └── agent:helper               H   a sub-agent of the writer
    └── agent:editor                   E
```

Items exist at R, T, W, H and E. What each reach sees:

| Reach | Sees |
| --- | --- |
| `of(team:acme/agent:writer)` | R, T, W |
| `of(team:acme/agent:editor)` | R, T, E (never W or H) |
| `exact(team:acme/agent:writer)` | W |
| `of(team:acme/agent:writer/agent:helper)` | R, T, W, H |
| `subtree(team:acme/agent:writer)` | W, H |
| `subtree(team:acme)` | T, W, H, E (not R) |
| `Reach { at: team:acme, inherit: true, descendants: true }` | R, T, W, H, E |
| `of(root)` / `default()` | R |
| `subtree(root)` | R, T, W, H, E |
| no reach (`None`) | R, T, W, H, E |

With a service node `S` at `team:acme/agent:writer/service:digest`, every
row that reads descendants still leaves `S` out (`subtree(root)` and no reach
included); only `exact(S)`, `subtree(S)` or `of(S)` read it.

### A company above its tenants

A layout reads the subtree of its root, never what lies above it. To share
memory across tenants, a host nests every tenant below one company node and
names that node (and, optionally, the root) as a **core scope**
([specs/core-scopes.md](../specs/core-scopes.md)):

```text
root                                   core: "Core"     (optional)
└── ws:acme                            core: "Company"  company brain
    ├── team:hive                      layout root of the hive's agents
    └── team:other                     another tenant, never read by the hive
```

A core scope reads its node with `Reach::exact`, so the hive sees the
company's own items but nothing from `team:other`.

## What each operation does with reach

| Operation | Namespace handling |
| --- | --- |
| `store`, `store_many` | Write at `meta.namespace`. No reach involved. |
| `recall` | `filter.reach` confines the items the answer may draw on. |
| `fetch` | `filter.reach` confines the ranked items. |
| `list` | `filter.reach` confines the listing. |
| `explore` | `filter.reach` confines the items counted. `Facet::Namespace` groups by node (the path string, `root` for the root); narrowing a bucket sets `Reach::exact(node)`, overwriting any reach. |
| `get` | `GetRequest::reach` leaves out ids outside it, as if they named nothing. |
| `forget` by filter | `filter.reach` confines what is removed. |
| `forget` by ids | **Not scoped.** The ids are removed wherever they live. |
| `forget_within` | The reach confines both the ids removed and where the engine looks for them. |

Because forget by ids is unscoped, a caller confined to a reach forgets with
`forget_within(ids, reach)`: an id outside the reach is left alone, and the
engine reads no node outside it while looking (a `get` under the reach
followed by `ForgetTarget::Ids` removes the same items, but the engine may
then look for them in every node it holds). A
filter whose only field is a `reach` is non-empty, so it is a valid forget
target meaning "everything in reach".

Reach with `inherit` (the default) includes ancestors, so a forget by filter
under `Reach::of(agent)` can remove memory the agent shares with its team or
the root; use `Reach::exact` to confine removal to the agent's own node.

`MetaFilter::matches` applies the reach to the item's namespace like any
other field, and every engine must agree: the conformance `namespaces` check
covers reaches, `get`, `fetch`, the namespace facet, a forget scoped to one
node and a forget by id within a reach.

## How tools pin it

A model never chooses a namespace or a reach. `tinymemory-tools` takes them
from the host in a `ToolScope { place, reach, writes }`:

- every item a tool stores gets `meta.namespace = place`;
- every read filter's `reach` is **overwritten** with the scope's reach, and
  `memory_get` passes it as `GetRequest::reach`;
- `memory_forget` by ids reads them back under the reach first and forgets
  only those found;
- a `namespace` or `reach` key in a tool's arguments is refused with
  `InvalidRequest`, and the `namespace` facet is not offered to the model.

See [tools.md](tools.md) for the full contract. `context.md` takes the same
reach through `ContextSpec::reach`.
