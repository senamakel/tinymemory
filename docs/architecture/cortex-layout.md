# CortexDB engine: scope layouts

How the CortexDB engine turns an item's namespace node and kind into a scope
path (`envelope::ScopeLayout`). There are two layouts. A host picks one per
engine, and switching moves nothing.

## Legacy (the default)

Every node keeps one `app:{kind}` leaf per kind, below the fixed TinyMemory
root:

```text
app:tinymemory/app:{documents,conversations,learnings}                   the root node
app:tinymemory/agent:researcher/app:{documents,conversations,learnings}  an agent
app:tinymemory/team:acme/agent:writer/app:learnings                      a team member
```

An engine with no scope root uses it, so existing memory stays where it is.

## V3: below a host's root

`EngineSettings::scope_root` (or `CortexEngine::with_scope_root`) names the
root, one person's: `org:<id>`. That person's memory is then one
subtree, which can be confined (a token scoped to `org:<id>/*`) and erased
as a whole. `user:` names the person's *actor* (the root's owner, a token's
`sub`), never a scope segment. Every kind keeps a leaf of its own, so no event sits on an inner
node, and erasing one node never takes a sibling kind with it:

| Item | Scope |
| --- | --- |
| learning at node N | `{root}/{N}/app:learnings` |
| conversation at N | `{root}/{N}/app:conversations` |
| document at N, N has a `source:` | `{root}/…/app:brain/source:…`, with `app:brain` before the first `source:` and no leaf |
| document at N, no `source:` | `{root}/{N}/app:documents` |
| any N with a `service:` | `app:flows` before the first `service:` |

For example, with root `org:42`:

```text
org:42/app:learnings                                     shared learnings
org:42/app:brain/source:gmail                            a connector's documents
org:42/app:brain/source:github/project:api               one repository
org:42/ws:main/app:conversations                         chats
org:42/ws:main/app:flows/service:newsletter/app:learnings  a workflow's memory
org:42/app:flows/service:digest/app:documents            a workflow with no workspace
```

The chat scope is shared by the person's agents. Each turn retains its
`agent_id` in TinyMemory's item metadata. History queries select one agent;
team queries read the same exact CortexDB scope and discard that agent's
turns before applying the section limit. Neither query widens to a sibling
person's `org:` root.

### The hosted wire: the tenant's root

The TinyHumans backend pins every scope below the caller's tenant root
(`org:<id>`), so a hosted engine names no root of its own
(`EngineSettings::tenant_root`, `CortexEngine::with_tenant_root`): it sends
`ws:main/app:conversations` and the stored path is
`org:<id>/ws:main/app:conversations`, the same tree as a direct engine rooted
at `org:<id>`. An unprefixed scope listing covers the whole tenant (the
backend bounds it), and an empty `prefix=` is never sent. A tenant root is
refused on the direct wire, where nothing would pin it.

### The retired `user:` root

Earlier v3 hosts rooted a person at `user:<id>`, which the hosted backend
stored as `org:<id>/user:<id>/…`. While that memory is being moved
(cortexdb-saas `reroot-user-segment`), a host names it as a retired root
(`EngineSettings::retired_scope_root`, `CortexEngine::with_retired_root`):

- every read covers each node below both roots, merged by item id, so an
  item held in both is one item;
- every forget (by id or by filter) and every direct erasure removes from
  both;
- writes go only below the root.

Once the move is verified, the host stops naming the retired root.

`app` never names a namespace node, so a path reads back to exactly one node
and kind (`ScopeLayout::parse`). Only the canonical spelling reads back: a
grouping node out of place, a missing leaf, or another root is somebody
else's scope and is skipped. On the direct wire a path must start at the
root (CortexDB's own API never prefixes one). On the hosted wire, whose
backend prefixes the tenant, the root is matched at its last occurrence. A
node that begins with the root's own segments, or a retired root's
(`user:42/…` while `user:42` is retired), is refused before it is written (`InvalidRequest`), so no written
path reads back two ways.

A root is `type:id` segments of the types CortexDB's hosted API admits
(`org dept team app user agent service ws project source`), with ids of
`[A-Za-z0-9_-]`. It may not hold the legacy root. Anything else is a
`Config` error when the engine is built.

### Root ownership

CortexDB makes the first writer of an unregistered scope its owner. With
`EngineSettings::scope_owner` (the actor `user:<id>`), a direct engine
registers the root (`org:<id>`) once, before its first write (`POST v1/scopes` with that owner), so the
person owns their root rather than whichever key writes first:

- `409 SCOPE_REGISTRATION_EXISTS` is not taken as ownership: the root's
  record is read (`GET v1/scopes?path=`) and, when the owner is not among its
  owners, the members are written back with it added
  (`PUT v1/scopes/members`);
- any other failure is logged and does not fail the write, and the next write
  tries again;
- the hosted backend keeps its own tenancy and is never asked.

No write can claim the root first: CortexDB auto-registers only the scope a
write lands in (measured on v0.10.4), and v3 never writes to the root itself,
only to the leaves below it. So a failed registration leaves the root
unregistered until a later write registers it with its owner.

### Reads

Discovery (a subtree reach, or none) lists from the reach's own node
(`ScopeLayout::node_prefixes`): the node's path with `app:flows` before a
`service:`, and, for a node holding a `source:`, the same with `app:brain`
before it too, where that node's sourced documents sit. A node without a
`source:` needs only the first, since a sourced document below it is grouped
after it. An unscoped read lists the whole root. Erasure finds its scopes
the same way. A v3 engine never reads the legacy tree, and a legacy engine
never reads a v3 root. Moving memory from one layout to the other is a
host's step, not a side effect of switching.
