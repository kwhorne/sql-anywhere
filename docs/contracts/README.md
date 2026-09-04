# Storage-primitive contracts

Stable, versioned contracts for the storage primitives described in
[STORAGE_PRIMITIVES.md](../STORAGE_PRIMITIVES.md). Each defines the exact tables,
indexes, SQL and semantics an implementer builds and tests to — the **substrate**
half of the Redis-free stack (epic elyra-2). The **runtime** half (Askr) is
documented in `askr/docs/STORAGE_BACKEND.md`.

| Contract | Backs | Tracking |
| --- | --- | --- |
| [Durable queue](./QUEUE_CONTRACT.md) | Laravel queue driver, `askr_queue_*`, worker autoscaling | elyra-5 |
| [KV cache](./CACHE_CONTRACT.md) | Laravel cache store, `Cache::lock()`, counters, sessions | elyra-7 |
| [Pub/sub](./PUBSUB_CONTRACT.md) | Broadcasting / SSE / Pusher-compatible WebSocket | elyra-6 |

## Conformance

These contracts are **executable, CI-verified specs**, not just prose:
[`sqlanywhere/tests/contract_conformance.rs`](../../sqlanywhere/tests/contract_conformance.rs)
(`queue_contract_v1`, `cache_contract_v1`, `pubsub_contract_v1`) reads the SQL
**out of these documents** and runs it against the engine. Not a copy of it: the
statements below are the ones executed, so a document and its proof cannot drift
apart. Edit a statement here and the test either proves the new semantics or
goes red.

That matters most for the queue's claim, which is a subtle atomic `UPDATE` that
every consumer has to reproduce exactly. Mutating it here is how the tests get
checked for teeth: dropping the `reserved_until` guard, the `available_at`
guard, the priority ordering, or the attempt counter each turns
`queue_contract_v1` red.

The Askr runtime therefore builds against a proven contract that cannot silently
drift from what the substrate actually does.

## Principles

- **Mirror the L1 semantics.** Askr already ships in-process shared-memory
  versions (`squeue.rs`, `cache.rs`). These contracts match their semantics so
  the same PHP-facing API works whether the backend is L1 (ephemeral) or L2
  (durable/replicated, here).
- **Ordinary SQL.** No stored procedures or extensions — every operation is a
  single, atomic SQL statement in the SQLite dialect (`unixepoch()`,
  `ON CONFLICT`, `UPDATE … RETURNING`).
- **Durable & replicated for free.** Because they are tables, they survive
  crashes and ride the existing replication log to replicas and the edge.
- **Versioned.** Each contract is v1; additive, default-valued columns are
  backward compatible, anything else is a new major version + migration.
