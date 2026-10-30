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
every consumer has to reproduce exactly. Mutating a statement here is also how
the tests get checked for teeth, and it has found real gaps twice: dropping the
`available_at` guard from the claim, the expired-counter reset from the cache
increment, or `expires_at = excluded.expires_at` from the cache upsert each used
to leave the suite green. All three are caught now, along with the
`reserved_until` guard, priority ordering, the attempt counter, the live-view
read, the lock's steal guard, the lock owner, the increment's delta, and the
published channel and sequence.

Coverage is complete: all 18 operations these contracts publish with SQL are
executed from the document, so every one of them can be mutation-checked.
`every_contract_operation_is_covered_or_listed` keeps it that way, failing if an
operation is added to a contract without being classified, or listed here after
being removed from one.

The Askr runtime therefore builds against a proven contract that cannot silently
drift from what the substrate actually does.

## Generated API

Every statement in an Operations section carries an annotation in the style of
[sqlc](https://sqlc.dev): `-- name: Claim :opt` names the operation and says
what it returns (`:exec` rows changed, `:one` exactly one row, `:opt` zero or
one, `:many` any number). The annotation is what tells alternatives apart from
sequences: forget and flush are two operations under one heading, while the
dead-letter move is one operation of several statements. An operation with
several statements runs them in one transaction, `IMMEDIATE` when the contract
spells `BEGIN IMMEDIATE`.

`cargo xtask gen-contracts` compiles the annotated documents and the parameter
table below into:

- [`sqlanywhere/src/contracts.rs`](../../sqlanywhere/src/contracts.rs), the
  typed Rust API: `sqlanywhere::contracts::queue::claim(&conn, visibility, queue)`
  and so on, plus each contract's `install` and the statements as constants.
- [`clients/php/src/`](../../clients/php), the same API for PHP on PDO:
  `Queue::claim($db, $visibility, $queue)` and so on, rows as arrays by column
  name. See [its README](../../clients/php/README.md).
- [`clients/typescript/src/`](../../clients/typescript), the same API for
  TypeScript: `queue.claim(db, { visibility, queue })` and so on, on
  `@libsql/client` (a `sqld` server over HTTP, or a file) or `node:sqlite`. See
  [its README](../../clients/typescript/README.md).
- [`clients/python/sqlanywhere_contracts/`](../../clients/python), the same
  API for Python: `queue.claim(db, visibility=30, queue="mail")` and so on,
  on the standard library's `sqlite3` or on `sqld` over HTTP, with no
  dependencies. See [its README](../../clients/python/README.md).
- [`contracts.json`](./contracts.json), the same operations as data, for
  generating a client in any other language.

None of these files is edited by hand. `generated_contracts_are_fresh` in `xtask`
fails CI when a document changes without them being regenerated, so the API is
always the SQL this page publishes and the conformance tests prove.

## Parameters

One name means one thing in every contract. The generator types each
parameter from this table, and refuses an annotated statement that uses a name
missing from it. `NULL` marks a parameter that may be null.

| Parameter | Type | Meaning |
| --- | --- | --- |
| `queue` | TEXT | Queue name. |
| `payload` | BLOB | Opaque job or event body; the substrate never reads it. |
| `priority` | INTEGER | Higher is claimed first. |
| `delay` | INTEGER | Seconds before a new job becomes claimable. |
| `max_attempts` | INTEGER | Claims allowed before the job is dead-lettered. |
| `visibility` | INTEGER | Seconds a claim reserves a job. |
| `id` | INTEGER | Job id, as `Enqueue` and `Claim` return it. |
| `backoff` | INTEGER | Seconds before a released job is claimable again. |
| `uuid` | TEXT NULL | Caller's id for a failed job. |
| `exception` | TEXT NULL | Why a job failed. |
| `key` | TEXT | Cache key. |
| `value` | BLOB | Cached value. |
| `expires_at` | INTEGER NULL | Unix seconds the entry expires; null never expires. |
| `delta` | INTEGER | Amount to add to a counter; negative decrements. |
| `owner` | TEXT | Lock holder token. |
| `ttl` | INTEGER | Seconds a lock is held before it may be stolen. |
| `tag` | TEXT | Cache tag. |
| `channel` | TEXT | Pub/sub channel. |
| `cursor` | INTEGER | Last `seq` a subscriber has seen. |
| `batch` | INTEGER | Most events to return at once. |
| `name` | TEXT | Durable subscriber name. |
| `retention_seconds` | INTEGER | Age past which events are trimmed. |
| `keep_last` | INTEGER | Newest events to keep when trimming by size. |

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
