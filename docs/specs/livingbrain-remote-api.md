# LivingBrain remote Brain API

**Status:** Accepted

**Owner:** TinyMemory maintainers

## Problem

Some hosts need a managed, shared knowledge brain instead of a local memory
engine. LivingBrain provides a hosted Brain API with capture, semantic search,
pages, graph, profile, change-feed, and markdown-export operations. It is not
a record store: captures are compiled into pages asynchronously and the public
API does not expose TinyMemory's exact `(namespace, key)` CRUD operations.

TinyMemory needs a defined integration boundary so a host can use this service
without treating it as a drop-in `Memory` implementation and silently breaking
the mandatory Core or Portability promises.

## Goals

- Add an optional `livingbrain` remote-engine feature exposed as
  `tinymemory::remote`.
- Provide a typed LivingBrain client for the supported Brain operations.
- Keep the client explicitly brain-scoped: every operation uses one configured
  `brain_id` and one `subject_id`.
- Send `Authorization: Bearer <api-key>` and `x-subject-id: <subject-id>` on
  every request; credentials must never appear in `Debug`, errors, examples,
  fixtures, or version-controlled configuration.
- Preserve LivingBrain's native concepts rather than flattening pages, graph
  edges, or asynchronous ingestion into fake TinyMemory records.
- Document the provider contract and a deterministic test double before an
  adapter is enabled in the facade.

## Non-goals

- Claiming that LivingBrain implements `tinymemory_api::traits::Memory` or
  advertising Core, Recall, or Portability through `MemoryTraitProvider`.
- Creating or deleting a customer's brain implicitly during client
  construction.
- Storing a user-supplied API key or subject id in repository files.
- Registering webhooks, connecting Telegram, or changing profile/brief
  settings in the first integration.

## Remote contract

The documented OpenAPI contract is version `1.0`. The public API gateway is
`https://api.livingbrain.com` and serves the OpenAPI document. The document's
alternate `api.lbs.chatchat.com` server value is not used as the default: it
was not DNS-resolvable from the supported build environment. The adapter
defaults to the public gateway and permits an HTTP(S) override only for tests
or a future documented deployment mode.

The initial public constructor is conceptually:

```rust
let brain = LivingBrain::new(
    "https://api.livingbrain.com",
    "lbk_...",
    "host-subject-id",
    "brain-id",
)?;
```

The eventual concrete name may follow the remote crate's conventions, but its
arguments and credential ownership are fixed by this specification. Empty
credentials, subject ids, and brain ids are rejected locally. The client owns
the API key; it accepts neither a prebuilt request client with headers nor a
global environment lookup. Hosts load secrets from their own secret store, for
example `LIVINGBRAIN_API_KEY`, and pass the value at construction.

### Supported operations

| TinyMemory-facing operation | LivingBrain endpoint | Required behavior |
| --- | --- | --- |
| `capture` | `POST /v1/brains/{brainId}/captures` | Submit note, text, URL, file, transcript, or integration input. `Capture::source` carries host provenance and `origin_ref` carries stable external identity. |
| `capture_batch` | `POST /v1/brains/{brainId}/captures/batch` | Submit a bounded batch and return the service's per-source outcome. |
| `capture_chat_turn` | `POST /v1/brains/{brainId}/captures/chat-turn` | Return LivingBrain's `worthy` decision; a not-worthy turn is a successful result, not an error. |
| `search` | `POST /v1/brains/{brainId}/search` | Return native page-search results, including `similarity`, page state, summary, and slug. |
| `page` / `pages` | `GET /v1/brains/{brainId}/pages/{slug}` / `GET /v1/brains/{brainId}/pages` | Read the native page model; do not invent a namespace/key translation. |
| `graph` | `GET /v1/brains/{brainId}/graph` | Return the service's graph payload intact enough to render or inspect connections. |
| `sources` | `GET /v1/brains/{brainId}/sources` | Expose ingest status so callers can observe asynchronous capture completion. |
| `remove_source` | `DELETE /v1/brains/{brainId}/sources/{sourceId}` | Remove a caller-created temporary ingest source, including after a live integration test. |
| `export_markdown` | `GET /v1/brains/{brainId}/export/markdown` | Return the native markdown bundle for user-directed export only. |

`capture` requires exactly one of `content` and `fetchUrl` when the selected
capture kind needs input. `originRef` is the service's deduplication key and
must be stable for retries of the same host event. The adapter must not retry a
capture with a newly generated `originRef`, because that converts a retry into
a duplicate ingestion. A batch is limited to 100 captures, each with a stable
`originRef`, and returns its per-source outcomes.

### Capability boundary

LivingBrain is a *brain API client*, not a TinyMemory mandatory-family driver.
It therefore has no `livingbrain_provider` function in the first release and
does not appear in `DriverRegistry::builtin()` as an `Embedded` or `External`
driver. A host that wants both systems may use LivingBrain for durable,
semantically compiled knowledge and continue binding a normal TinyMemory
`MemoryProvider` for exact record storage.

If a future version needs an engine adapter, it must first define a separate
durable envelope and prove exact get/list/forget/export behavior. A semantic
search result or a markdown export is not evidence of exact-key portability.

## Errors, retries, and limits

- Invalid endpoint or blank connection fields fail during construction.
- `401` and `403` are terminal credential/authorization failures and are never
  retried.
- `400` and `404` are terminal request or resource failures and are never
  retried.
- `429`, `502`, `503`, `504`, and transport timeouts use the existing bounded
  remote-read retry policy only when the request is idempotent. Capture retries
  require a caller-supplied stable `originRef`.
- Responses are subject to the remote crate's existing byte cap. An oversized
  response fails rather than being partially decoded.
- Search validates `top_k` and similarity bounds before issuing a request.

## Security and operational constraints

- The API key is a tenant credential; `x-subject-id` is the host's end-user
  identity. The host must not substitute a shared tenant id for the subject id.
- Capture content, search queries, page contents, and provenance leave the
  host. The host's egress, redaction, consent, taint, and audit policy must run
  before this client is called.
- The adapter must redact bearer values from all diagnostics and must not place
  headers in errors.
- Live tests are opt-in and read credentials only from the process environment;
  ordinary unit and conformance tests use a local HTTP double.

## Acceptance criteria

1. `tinymemory-remote` exposes an optional, documented `livingbrain` client
   without adding a mandatory-family provider or registry driver.
2. Each request carries both required headers, and tests prove neither header
   value is exposed by `Debug` or an error message.
3. A capture with a stable `originRef` can be retried safely; the adapter never
   synthesizes a different idempotency key for a retry.
4. Search, source-status inspection, page reads, graph retrieval, and markdown
   export decode documented responses with a local HTTP double.
5. Validation, authentication, rate-limit, transient, and oversized-response
   failures have deterministic behavior covered by tests.
6. The facade feature list and README describe LivingBrain as a Brain API
   client, not as a `MemoryProvider` engine.

## Resolved decisions

- The host configures an existing `brain_id`; creation remains an explicit
  product-level workflow rather than a side effect of connecting a client.
- The client exposes source status but does not poll. A host choosing to wait
  owns cadence, timeout, and user-visible progress policy.
- Search results and exports have stable Rust types. Pages and graphs remain
  JSON payloads initially, preserving the provider's evolving native model.
- The facade feature is named `livingbrain`, alongside the remote engines, and
  its documentation makes the client/provider distinction explicit.
