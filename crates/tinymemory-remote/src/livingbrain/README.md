# LivingBrain client

This module is a brain-scoped client for LivingBrain's hosted API. It is not a
`MemoryProvider`: captures are asynchronously compiled into native pages, so
the service cannot provide TinyMemory's exact namespace/key CRUD contract.

`LivingBrain` is constructed with the API endpoint, a bearer key, a subject id,
and one brain id. It sends the key only in `Authorization: Bearer` and attaches
the subject id as `x-subject-id`; neither credential is rendered through the
client's `Debug` output.

The public surface submits individual and bounded batch captures, conversation
turns, semantic searches, and source cleanup. It also reads native pages,
graphs, source status, and markdown exports. Native page and graph shapes are
returned as JSON because LivingBrain owns their schema.

Captures with a stable `origin_ref` are retry-safe and retry transient failures.
Captures without one make exactly one request. Batch captures require an
`origin_ref` per item and accept at most 100 items. Callers can preserve host
provenance with `Capture::source`.

`types.rs` contains the request and response contracts. `test.rs` uses a local
HTTP simulation to verify wire routes, headers, payloads, validation, and
credential redaction without contacting the hosted service.
