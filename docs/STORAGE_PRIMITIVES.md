# Storage primitives

> **A cache, a queue, and a pub/sub bus are features of SQL Anywhere, not
> separate products.** SQL Anywhere is already embeddable, replication-ready, and
> built on SQLite — so a cache is just a table with an expiry column, a queue is
> a table plus an atomic claim, and pub/sub is an append-only table carried by
> the replication log you already have.

The usual architecture bolts Redis, SQS, and Kafka *next to* the database, then
pays to keep three more systems running, consistent, and reachable. For the
storage primitives an application actually reaches for, you don't need any of
them: they compose out of ordinary SQL and the machinery SQL Anywhere already
ships.

Runnable: [`examples/storage_primitives.rs`](../sqlanywhere/examples/storage_primitives.rs).
Verified: [`tests/storage_primitives.rs`](../sqlanywhere/tests/storage_primitives.rs).

## KV cache with TTL

A cache is a table with an expiry column. Filter expired entries lazily on read
with a view, and reclaim space with a periodic sweep:

```sql
CREATE TABLE kv(key TEXT PRIMARY KEY, value TEXT NOT NULL, expires_at INTEGER);

-- Reads never see expired entries.
CREATE VIEW kv_live AS
  SELECT key, value FROM kv
  WHERE expires_at IS NULL OR expires_at > unixepoch();

-- SET key = value with a 1-hour TTL (NULL = no expiry).
INSERT OR REPLACE INTO kv VALUES ('session:1', 'alice', unixepoch() + 3600);

-- GET
SELECT value FROM kv_live WHERE key = 'session:1';

-- Periodic sweep (a cron, a background task, or a trigger).
DELETE FROM kv WHERE expires_at <= unixepoch();
```

Because it's a table it is transactional, queryable (`SELECT … WHERE value LIKE …`),
and — unlike an in-memory cache — **durable and replicated** for free.

## Durable work queue

A queue is a table plus an atomic *claim*. `UPDATE … RETURNING` with a subselect
hands each worker exactly one job, and a visibility-timeout column gives
at-least-once delivery: if a worker dies, its lock expires and the job is
redelivered.

```sql
CREATE TABLE jobs(
  id           INTEGER PRIMARY KEY,
  payload      TEXT,
  done         INTEGER DEFAULT 0,
  locked_until INTEGER,           -- claim expiry (visibility timeout)
  attempts     INTEGER DEFAULT 0
);

-- Enqueue
INSERT INTO jobs(payload) VALUES ('send email');

-- Dequeue: atomically claim the next visible job (safe for many workers).
UPDATE jobs SET locked_until = unixepoch() + 30, attempts = attempts + 1
WHERE id = (
  SELECT id FROM jobs
  WHERE done = 0 AND (locked_until IS NULL OR locked_until <= unixepoch())
  ORDER BY id LIMIT 1
)
RETURNING id, payload, attempts;

-- Ack on success
UPDATE jobs SET done = 1 WHERE id = :id;
```

The claim is a single write statement, so concurrent workers never receive the
same job. Persistence and replication mean an enqueued job survives a crash and
can be drained by a `sqld` node or an embedded replica.

## Pub/sub via the replication log

A topic is an append-only table; publishing is an `INSERT`; subscribing is
tailing rows past a cursor. Each subscriber keeps its own cursor, so it is a
durable, replayable log — not fire-and-forget.

```sql
CREATE TABLE events(
  seq     INTEGER PRIMARY KEY,     -- monotonic cursor
  channel TEXT,
  payload TEXT,
  ts      INTEGER DEFAULT (unixepoch())
);

-- Publish
INSERT INTO events(channel, payload) VALUES ('orders', 'order#42');

-- Subscribe: tail new messages on a channel since your cursor.
SELECT seq, payload FROM events
WHERE channel = 'orders' AND seq > :cursor
ORDER BY seq;
```

**Across nodes, the transport is already built.** SQL Anywhere's replication log
(the same WAL/CDC stream that feeds embedded replicas) ships every `INSERT` to
every replica in order. A subscriber on another device tails its local copy of
`events` — so publish-on-the-primary, subscribe-on-the-edge works with no
message broker. Combine with [CRDT](CRDT.md) for multi-writer topics that merge
offline.

## Why these are chapters, not products

- **One system to run.** No Redis/SQS/Kafka to deploy, secure, scale, or page on.
- **One consistency model.** A job enqueue, a cache write, and a domain change
  commit in the *same transaction* — no dual-write races.
- **It works at the edge and offline**, and it syncs, because it's the same
  replicated SQLite file as everything else.
- **It's inspectable.** Your queue and cache are just tables you can `SELECT`,
  index, and join.

For internet-scale, multi-tenant infrastructure (millions of messages/sec across
a fleet) the dedicated systems still earn their keep. For the cache, queue, and
event stream that live *inside* an application, they belong here.
