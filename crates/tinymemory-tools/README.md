# tinymemory-tools

The agent-facing side of TinyMemory, over any `tinymemory_api::MemoryEngine`:

- **`tools`**: seven memory tools a host offers its model, as
  runtime-neutral specs (name, description, JSON Schema) plus one `call`
  entry point that runs them and returns compact JSON.
- **`context`**: the `context.md` compiler, a token-budgeted brief a host
  injects at the start of a session.
- **`recall`**: holistic recall, the one read across several scopes,
  rendered as one budgeted `ContextPack`. `context.md` is one preset of it.
- **`layout`**, **`brain`**, **`lifecycle`**, **`background`**: the agent
  memory lifecycle. `MemoryLayout` is the standard tree: brain documents by
  source, per-agent conversations, and learnings. `Brain` ingests documents.
  `AgentMemory` serves session start, pre-turn context, post-turn logging and
  compaction. `BackgroundJob` carries the belief builds those calls hand back.
  See [`docs/architecture/lifecycle.md`](../../docs/architecture/lifecycle.md)
  and the runnable `examples/agent_loop.rs` and `examples/brain.rs`.

The crate has no tool-runtime dependency (no MCP, no `tinytools`). A host
adapts `ToolSpec` to whatever runtime it uses; see
[Adapting to a tool runtime](#adapting-to-a-tool-runtime). The design is
written up in [`docs/architecture/tools.md`](../../docs/architecture/tools.md).

## Durable queues

Enable the `storage` feature to use `storage::DriverQueueStore` for background
jobs and `storage::DriverListStore<T>` for pending writes and deletions. The
host supplies its already-open TinyStorageDrivers backend and scope, and
composes `storage::storage_schema()` with the other module schemas before
serving requests. These adapters store one document per queue item and use
TinyStorageDrivers' atomic claim, visibility lease, and fenced settlement
ports; storage mode does not replace a whole queue snapshot during processing.

For legacy desktop files, the host reads and decodes the source without
modifying it, then passes the records and stable IDs to `import_items` or
`DriverQueueStore::import_legacy`. Repeating an import is safe: existing target
rows win, and the report distinguishes inserted, identical, and conflicting
records. FIFO imports should be passed as one ordered batch with stable
occurrence IDs, including for identical values. Keep the source file until the
host has verified the imported target.

The default lease clock is the system clock; hosts can inject TinyStorageDrivers'
clock for deterministic tests. Distributed hosts should keep clocks reasonably
synchronized at lease-expiry boundaries. A crashed worker's item becomes
available after its lease expires, so queue execution is at-least-once across
crashes. A long-running operation can overlap a reclaimed attempt; a lease
cannot provide exactly-once external side effects.

## The tools

| Tool | Kind | Arguments | Result |
| --- | --- | --- | --- |
| `memory_recall` | read | `question`, `filter?`, `limit?`, `instructions?`, `refers_to?` | `{answer, citations: [...]}` |
| `memory_fetch` | read | `query`, `mode?`, `filter?`, `limit?`, `cursor?`, `refers_to?` | `{hits: [...], next_cursor?}` |
| `memory_list` | read | `filter?`, `limit?`, `cursor?` | `{items: [...], next_cursor?}` |
| `memory_get` | read | `ids` | `{items: [...], missing: [ids]}` |
| `memory_explore` | read | `facet`, `filter?`, `limit?` | `{facet, buckets: [{value, count}], total, missing, more_buckets, truncated}` |
| `memory_store` | write | one of `learning` / `document` / `conversation`, `tags?` | `{id, replayed}` |
| `memory_forget` | write | `ids` **or** a non-empty `filter` | `{forgotten, skipped: [ids]}` |

Details:

- `limit` is `1..=50`, default `10`. `ids` lists `1..=200` ids.
- `memory_fetch`'s `mode` enum lists exactly the engine's
  `EngineDescriptor::fetch_modes`; with no mode named, `hybrid` is used when
  served, otherwise the engine's first mode. An engine serving no mode gets no
  `memory_fetch` tool at all.
- `memory_store` takes `learning: {text, learning_kind?, confidence?,
  evidence?}` (kind defaults to `fact`, confidence to `0.8`),
  `document: {title?, text}` or `conversation: {turns: [{role, text}]}`.
  Storing the same memory twice is a replay, not a duplicate.
- `memory_explore`'s facets are `kind`, `source`, `source_id`, `workspace`,
  `folder`, `file_path`, `language`, `repo`, `url`, `thread`, `agent`,
  `tool_call` and `tag`. The `namespace` facet is deliberately absent.
- The model-facing `filter` is a subset of `MetaFilter`: `kinds`, `sources`,
  `tags_any`, `workspace`, `folder`, `file_path`, `repo`, `url`, `thread_id`,
  `agent_id`, `observed_after` and `observed_before` (RFC 3339). It never has
  a namespace or a reach.
- `refers_to` (`{from, to}`, local `YYYY-MM-DD` dates) is the days the
  question is about: hits from them rank first and nothing is dropped
  (`tinymemory_api::TimeHint`), read in the host's zone
  (`MemoryTools::in_zone`). Unlike `observed_after`/`observed_before`, which
  filter by when a memory was recorded, it keeps a memory recorded on Monday
  about Tuesday.
- A hit renders as `{id, kind, text, score, confidence?, meta}` where `meta`
  is a subset: `source {kind, id?}`, `file_path`, `url`, `thread_id`, `tags`,
  `observed_at`. Scores are rounded to four places. The namespace is never
  rendered.

The exact names and schemas are frozen in
[`tests/fixtures/tool_contracts.json`](tests/fixtures/tool_contracts.json).
A change to them changes what every host's model is told, so the
`tool_contracts` test fails until the fixture is regenerated on purpose:

```sh
BLESS_TOOL_CONTRACTS=1 cargo test -p tinymemory-tools --test tool_contracts
```

## Scoping invariants

This crate exists so that a model can never choose whose memory it touches.
An earlier tool layer let the model pass a namespace, which let one tenant's
agent read another's memory. Here, the namespace and the reach are fixed by
the host in a `ToolScope` and are never tool arguments:

| Field | Meaning |
| --- | --- |
| `place: Namespace` | the node every stored item is written to |
| `reach: Option<Reach>` | the nodes every read (and forget) is confined to; `None` reads every namespace |
| `writes: bool` | whether `memory_store` and `memory_forget` are offered |

What the tools enforce:

1. **Writes land at `place`.** `memory_store` builds the item's metadata
   itself: namespace `place`, source `agent`, the model's tags, and
   `observed_at` set to the time of the call.
2. **Reads use the scope's reach.** Every recall, fetch, list and explore
   filter has its `reach` overwritten with the scope's; `memory_get` passes it
   as `GetRequest::reach`, and ids outside it come back as `missing`.
3. **Forget cannot reach out.** By ids, the ids are first read back with
   `get` under the reach and only those found are forgotten; the rest are
   returned as `skipped` and survive. By filter, the model's filter must set
   at least one field (a reach alone would mean "everything in reach") and is
   then confined to the reach.
4. **Attempts are refused, not ignored.** A `namespace` or `reach` key
   anywhere in the arguments, at any depth, is an `Error::InvalidRequest`
   saying the host fixes it. Every schema object sets
   `additionalProperties: false`, and any other unknown key is refused too.

Note that a reach with `inherit` (the default from `Reach::of`) includes the
node's ancestors, so a forget may remove memory the agent shares with its
team or the root. A host that wants forgets confined to the agent's own node
sets `Reach::exact(place)`, or offers read-only tools.

### Building a scope

```rust,ignore
use std::sync::Arc;
use tinymemory_api::{Namespace, Reach};
use tinymemory_tools::MemoryTools;

// Single tenant: root, reads everything, writes enabled.
let tools = MemoryTools::new(engine.clone());

// One agent: writes to its node, reads it and its ancestors, never a sibling.
let agent = MemoryTools::new(engine.clone()).placed_at("team:acme/agent:writer".parse()?);

// Read-only, team-wide.
let auditor = MemoryTools::new(engine)
    .placed_at("team:acme".parse()?)
    .reach(Reach::subtree("team:acme".parse()?))
    .read_only();
```

`placed_at` resets the reach to `Reach::of(place)`, so a placed scope never
reads more than its own branch by accident; call `reach` after it to change
that. `MemoryTools::with_scope` takes a `ToolScope` directly.

## Errors

`MemoryTools::call` returns `tinymemory_api::Result<serde_json::Value>`:

- `Error::InvalidRequest` for an unknown tool name, arguments that are not an
  object, a host-fixed key, an unknown key, or a missing, mistyped or
  out-of-range value. Messages are lowercase and name the tool and the field
  (`memory_list: \`filter.kinds\` \`memo\` is not an item kind`), so they can
  be shown to the model as the tool's error output.
- `Error::Unsupported` for a write tool on read-only tools, and for
  `memory_fetch` on an engine serving no fetch mode. The call is well formed;
  the tools simply do not offer the operation, which is what this variant
  means across the contract.
- Anything the engine returns.

## Adapting to a tool runtime

`ToolSpec` is three fields: `name`, `description` and `parameters` (a JSON
Schema object). Most runtimes want exactly that:

- **MCP**: `name` → `name`, `description` → `description`,
  `parameters` → `inputSchema`. On `tools/call`, pass `arguments` to
  `MemoryTools::call` and return the result serialised as text content;
  return an error result with the error's message on `Err`.
- **OpenAI-style function calling**: `{"type": "function", "function":
  {"name", "description", "parameters"}}`. The schemas are closed objects, so
  they also suit strict mode where the runtime accepts optional properties.
- **Anthropic tool use**: `name`, `description`, `input_schema`.

A typical adapter:

```rust,ignore
for spec in tools.specs() {
    runtime.register(spec.name, spec.description, spec.parameters);
}
// When the model calls a tool:
let output = match tools.call(&call.name, call.arguments).await {
    Ok(value) => value.to_string(),
    Err(error) => format!("error: {error}"),
};
```

Build one `MemoryTools` per agent session from the session's identity, never
from anything the model said. `specs()` is cheap; call it per session so a
read-only or differently scoped agent sees only its own tools.

## `context.md`

`tinymemory_tools::context::compile(&engine, &ContextSpec::default())` recalls
one question per `Brief` (four defaults: about the user, active work,
preferences and standing instructions, recent important events), lists the
stored learnings newest and most confident first, and renders markdown with
`generated_at`, `engine`, `tokens` and `refs` frontmatter. The document fits
`budget_tokens` (default 1,500, four characters per token): learnings are
trimmed first, then the last brief. A failing brief is skipped and logged, and
an empty engine yields an empty document. `ContextSpec::reach` confines the
brief to one agent's part of the tree. Details:
[`docs/architecture/tools.md`](../../docs/architecture/tools.md#contextmd).

## Layout

```text
src/
├── lib.rs              # crate docs and re-exports
├── context/            # the context.md compiler
└── tools/
    ├── mod.rs          # MemoryTools, ToolScope, dispatch
    ├── spec/           # ToolSpec, tool names, JSON Schemas, limits
    ├── args/           # strict argument reading and the model filter
    ├── read/           # recall, fetch, list, get, explore
    ├── write/          # store, forget
    └── render/         # compact result JSON
tests/
├── tools_roundtrip.rs  # every tool against the reference engine
├── tools_scoping.rs    # the scoping invariants
├── tool_contracts.rs   # frozen names and schemas
└── fixtures/tool_contracts.json
```
