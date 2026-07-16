# Pub/sub contract (v1)

> **Status:** stable contract, v1. Tracking: elyra-6 (part of the Redis-free
> epic elyra-2). Substrate half of pub/sub: an append-only topic table tailed
> past a cursor, carried across nodes by the replication log. Overview:
> [STORAGE_PRIMITIVES.md](../STORAGE_PRIMITIVES.md).

A topic is **an append-only table**; publishing is an `INSERT`; subscribing is
tailing rows past a per-subscriber cursor. Because each subscriber keeps its own
cursor it is a **durable, replayable log** — not fire-and-forget — and the
cross-node transport is the replication log SQL Anywhere already ships.

This backs Askr's broadcasting bridge (`askr_broadcast()` / SSE / the
Pusher-compatible WebSocket) in elyra-13, so Laravel Echo works with no Redis.

Timestamps are **unix seconds**. Payloads are opaque bytes.

---

## Schema

```sql
CREATE TABLE IF NOT EXISTS askr_events (
  seq        INTEGER PRIMARY KEY AUTOINCREMENT,   -- monotonic cursor (never reused)
  channel    TEXT    NOT NULL,
  payload    BLOB    NOT NULL,
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

-- Tail a channel past a cursor cheaply.
CREATE INDEX IF NOT EXISTS askr_events_chan ON askr_events (channel, seq);

-- Optional: durable subscriber cursors (a subscriber may also keep its cursor
-- client-side). Enables resume-after-restart without re-reading the whole log.
CREATE TABLE IF NOT EXISTS askr_subscribers (
  name       TEXT PRIMARY KEY,
  cursor     INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
```

`seq` is monotonic and gap-free-enough to be a total order per node. Across nodes,
ordering is per-origin; combine with [CRDT](../CRDT.md) for multi-writer topics.

---

## Operations

### Publish

```sql
INSERT INTO askr_events (channel, payload) VALUES (:channel, :payload)
RETURNING seq;
```

### Subscribe (tail past a cursor)

One channel:

```sql
SELECT seq, payload, created_at
FROM askr_events
WHERE channel = :channel AND seq > :cursor
ORDER BY seq
LIMIT :batch;
```

Many channels (wildcard fan-in): replace the predicate with
`channel IN (:c1, :c2, …)` or `channel GLOB :pattern`. The subscriber advances
its cursor to the largest `seq` it has processed.

### Persist a subscriber cursor (optional)

```sql
INSERT INTO askr_subscribers (name, cursor, updated_at)
VALUES (:name, :cursor, unixepoch())
ON CONFLICT(name) DO UPDATE SET cursor = excluded.cursor, updated_at = excluded.updated_at;
```

### Retention (trim the log)

Keep the log bounded; a subscriber that is further behind than retention loses
messages (document the window). Trim by age or by size:

```sql
-- By age:
DELETE FROM askr_events WHERE created_at < unixepoch() - :retention_seconds;

-- Or by size (keep the newest N):
DELETE FROM askr_events
WHERE seq <= (SELECT max(seq) FROM askr_events) - :keep_last;
```

---

## Wakeups — do not tight-poll

A subscriber must **not** spin on the tail query. Two acceptable strategies:

1. **Local (embedded / same process as the writer):** hook SQLite's update
   notification (`sqlite3_update_hook` / the runtime's equivalent) on
   `askr_events` and only run the tail query when a row was inserted.
2. **Remote / cross-node:** long-poll — issue the tail query; if it returns rows,
   deliver and repeat immediately; if empty, block up to `:wait` seconds (server
   holds the request) before returning empty. The replication apply on a replica
   is the natural wakeup signal for tailing the local copy.

The reference poll/backoff for the fallback path: immediate re-query while
non-empty; otherwise long-poll with `:wait ≈ 20s` and jitter.

---

## Cross-node delivery

**The transport is already built.** Every `INSERT` into `askr_events` is shipped,
in order, to every replica by the same WAL/CDC replication stream that feeds
embedded replicas. A subscriber on another device tails its **local** copy of
`askr_events`, so *publish-on-the-primary, subscribe-on-the-edge* works with no
message broker. Askr fan-out (elyra-13) reads the local tail and pushes to
connected SSE/WebSocket clients on that node.

---

## Guarantees & edge cases

- **At-least-once, replayable.** A subscriber re-reads from its cursor after a
  crash; delivery beyond retention is lost (bounded log, by design).
- **Ordering** is total per node by `seq`; per-origin across nodes.
- **No exactly-once.** Consumers should be idempotent or dedupe on `seq`.
- **Backpressure** is the subscriber's cursor lag; monitor `max(seq) - cursor`.
- **Presence/auth** (private/presence channels) is a runtime concern (Askr's
  Pusher endpoint), not part of this substrate contract.

## Naming & configuration

Table names are fixed (`askr_events`, `askr_subscribers`). Retention window, batch
size and long-poll wait are **runtime** settings (elyra-13).

## Compatibility

Contract **v1**. Additive, default-valued columns are backward compatible; other
changes require **v2** + migration.
