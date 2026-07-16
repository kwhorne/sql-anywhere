# Durable queue contract (v1)

> **Status:** stable contract, v1. Tracking: elyra-5 (part of the Redis-free
> epic elyra-2). This is the substrate half of the durable queue: the tables,
> indexes and SQL the runtime (Askr) claims against. The conceptual overview is
> [STORAGE_PRIMITIVES.md](../STORAGE_PRIMITIVES.md); this document is the exact,
> versioned contract an implementer builds and tests to.

The queue is **a table plus an atomic claim**. It gives at-least-once delivery,
delayed jobs, priority, bounded retries with backoff, a visibility timeout, and
a dead-letter table — all in ordinary SQL, durable and replicated for free.

The semantics deliberately mirror Askr's in-process shared-memory queue
(`crates/askr/src/squeue.rs`: `push` / `pop` / `delete` / `release`) so the same
PHP-facing API (`askr_queue_*`) and Laravel queue driver work unchanged whether
the backend is L1 (shared memory, ephemeral) or L2 (this, durable/replicated).

All timestamps are **unix seconds** (`unixepoch()`). Payloads are opaque bytes
(the runtime stores a serialized Laravel job); the substrate never interprets them.

---

## Schema

```sql
CREATE TABLE IF NOT EXISTS askr_jobs (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  queue          TEXT    NOT NULL DEFAULT 'default',
  payload        BLOB    NOT NULL,
  priority       INTEGER NOT NULL DEFAULT 0,          -- higher is claimed first
  available_at   INTEGER NOT NULL,                    -- claimable when now >= this (delayed jobs)
  reserved_until INTEGER,                             -- NULL = not reserved; else claim/visibility expiry
  attempts       INTEGER NOT NULL DEFAULT 0,
  max_attempts   INTEGER NOT NULL DEFAULT 25,
  created_at     INTEGER NOT NULL DEFAULT (unixepoch())
);

-- Claim ordering: within a queue, pick the highest priority, then the oldest
-- ready job. reserved_until first keeps the range scan cheap for the common
-- "ready + unreserved" case.
CREATE INDEX IF NOT EXISTS askr_jobs_claim
  ON askr_jobs (queue, reserved_until, priority DESC, available_at, id);

CREATE TABLE IF NOT EXISTS askr_failed_jobs (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  uuid       TEXT,
  queue      TEXT    NOT NULL,
  payload    BLOB    NOT NULL,
  exception  TEXT,
  attempts   INTEGER NOT NULL,
  failed_at  INTEGER NOT NULL DEFAULT (unixepoch())
);
```

A job is **ready** when `available_at <= now` and it is **not reserved**
(`reserved_until IS NULL OR reserved_until <= now`). A reservation that lapses
(worker died before ack) makes the job ready again — that is the at-least-once
guarantee.

---

## Operations

Bind parameters are shown as `:name`. Every write is a single statement, so it is
atomic under concurrency without an explicit transaction.

### Enqueue

```sql
-- delay = 0 for "now"; > 0 for a delayed job.
INSERT INTO askr_jobs (queue, payload, priority, available_at, max_attempts)
VALUES (:queue, :payload, :priority, unixepoch() + :delay, :max_attempts)
RETURNING id;
```

Bulk enqueue is the same statement with multiple `VALUES` rows.

### Claim (pop)

Atomically reserve the next ready job for `:visibility` seconds and increment its
attempt counter. Returns nothing when the queue is empty.

```sql
UPDATE askr_jobs
SET reserved_until = unixepoch() + :visibility,
    attempts       = attempts + 1
WHERE id = (
  SELECT id FROM askr_jobs
  WHERE queue = :queue
    AND available_at <= unixepoch()
    AND (reserved_until IS NULL OR reserved_until <= unixepoch())
  ORDER BY priority DESC, available_at, id
  LIMIT 1
)
RETURNING id, payload, attempts, max_attempts;
```

The subselect + `UPDATE … RETURNING` is the atomic claim: two workers can never
receive the same row because the write serializes on it.

### Ack (delete on success)

```sql
DELETE FROM askr_jobs WHERE id = :id;
```

### Release (nack / retry with backoff)

The runtime computes `:backoff` (see below) and re-arms the job:

```sql
UPDATE askr_jobs
SET reserved_until = NULL,
    available_at   = unixepoch() + :backoff
WHERE id = :id;
```

`attempts` is **not** incremented here — it was incremented at claim time, so a
worker that dies mid-job still consumes an attempt (matching Laravel).

### Renew reservation (long-running jobs / heartbeat)

Extend the visibility window while still processing, so a slow job is not
redelivered:

```sql
UPDATE askr_jobs
SET reserved_until = unixepoch() + :visibility
WHERE id = :id AND reserved_until > unixepoch();
```

### Move to dead-letter (max attempts exceeded)

When a claim returns `attempts >= max_attempts`, the runtime records the failure
and deletes the job — ideally in one transaction:

```sql
BEGIN IMMEDIATE;
INSERT INTO askr_failed_jobs (uuid, queue, payload, exception, attempts)
SELECT :uuid, queue, payload, :exception, attempts FROM askr_jobs WHERE id = :id;
DELETE FROM askr_jobs WHERE id = :id;
COMMIT;
```

### Backlog (for autoscaling / metrics)

Feeds `askr_queue_ready`, `askr_queue_total`, `askr_queue_oldest_seconds`
(elyra-8):

```sql
SELECT
  count(*)                                                          AS total,
  count(*) FILTER (
    WHERE available_at <= unixepoch()
      AND (reserved_until IS NULL OR reserved_until <= unixepoch())
  )                                                                 AS ready,
  coalesce(unixepoch() - min(available_at) FILTER (
    WHERE available_at <= unixepoch()
      AND (reserved_until IS NULL OR reserved_until <= unixepoch())
  ), 0)                                                             AS oldest_seconds
FROM askr_jobs
WHERE queue = :queue;
```

---

## Retry / backoff

Backoff is a **runtime** policy, not stored in the substrate. The reference
policy (matching Laravel's default queue worker) is exponential with a cap:

```
backoff(attempts) = min(base * 2^(attempts - 1), max_backoff)   # seconds
# reference: base = 1, max_backoff = 3600
```

A per-job override may be carried inside `payload`; the substrate does not read it.

---

## Guarantees & edge cases

- **At-least-once.** A job is only removed on ack (or dead-letter). A crashed
  worker's reservation lapses and the job is redelivered — handlers must be
  idempotent.
- **No double delivery.** The atomic claim serializes; no two claimers get the
  same `id`.
- **Ordering** is best-effort (priority, then FIFO by `available_at`, `id`); it is
  not a strict total order across priorities or after retries.
- **Durability / replication.** Because it is a table, an enqueued job survives a
  process crash and is shipped to replicas by the replication log — a `sqld` node
  or embedded replica can drain the same queue.
- **Sweep of dead-letters** is an operator concern: `DELETE FROM askr_failed_jobs
  WHERE failed_at < unixepoch() - :retention;`

---

## Naming & configuration

- Table names are fixed (`askr_jobs`, `askr_failed_jobs`) so the runtime and any
  inspector agree without configuration.
- Visibility timeout, `max_attempts`, backoff base/cap and the poll interval are
  **runtime** settings (Askr `--queue` / `--queue-max` and the Laravel connector,
  elyra-12), not substrate settings.

## Compatibility

Contract **v1**. Additive columns (with defaults) are backward compatible.
Renaming/removing a column or changing claim semantics requires **v2** and a
migration. The runtime advertises the contract version it speaks.
