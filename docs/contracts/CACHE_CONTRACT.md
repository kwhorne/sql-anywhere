# KV cache contract (v1)

> **Status:** stable contract, v1. Conformance-tested by `sqlanywhere/tests/contract_conformance.rs::cache_contract_v1`. Tracking: elyra-7 (part of the Redis-free
> epic elyra-2). Substrate half of the durable cache: the tables and SQL the
> runtime (Askr) reads/writes. Overview: [STORAGE_PRIMITIVES.md](../STORAGE_PRIMITIVES.md).

A cache is **a table with an expiry column**. It gives get/set with TTL, tag
invalidation, atomic counters (rate limiting) and atomic `add` (locks) — durable,
transactional and replicated, unlike an in-memory cache.

The semantics mirror Askr's shared-memory cache (`crates/askr/src/cache.rs`:
`set` / `get` / `delete` / atomic `increment` / atomic `add`) so the PHP-facing
`askr_cache_*` API and the Laravel cache store + `Cache::lock()` (elyra-11) work
unchanged whether the backend is L1 (shared memory) or L2 (this, durable).

Timestamps are **unix seconds**. Values are opaque bytes; the substrate does not
interpret them except for the integer counter operations, which require a value
that parses as an integer.

---

## Schema

```sql
CREATE TABLE IF NOT EXISTS askr_cache (
  key        TEXT PRIMARY KEY,
  value      BLOB NOT NULL,
  expires_at INTEGER            -- unix secs; NULL = never expires
);

-- Lets the sweep find expired rows without a full scan.
CREATE INDEX IF NOT EXISTS askr_cache_expiry ON askr_cache (expires_at);

-- Reads never see expired entries.
CREATE VIEW IF NOT EXISTS askr_cache_live AS
  SELECT key, value FROM askr_cache
  WHERE expires_at IS NULL OR expires_at > unixepoch();

-- Tag → key mapping for tagged invalidation (optional; only if tags are used).
CREATE TABLE IF NOT EXISTS askr_cache_tags (
  tag TEXT NOT NULL,
  key TEXT NOT NULL,
  PRIMARY KEY (tag, key)
);
CREATE INDEX IF NOT EXISTS askr_cache_tags_key ON askr_cache_tags (key);
```

---

## Operations

### Set (put, with optional TTL)

```sql
-- ttl > 0: expires_at = now + ttl. ttl = 0 / forever: expires_at = NULL.
INSERT INTO askr_cache (key, value, expires_at)
VALUES (:key, :value, :expires_at)
ON CONFLICT(key) DO UPDATE SET value = excluded.value, expires_at = excluded.expires_at;
```

### Get

```sql
SELECT value FROM askr_cache_live WHERE key = :key;
```

An expired row returning no result is a miss; the sweep reclaims it later.

### Forget / flush

```sql
DELETE FROM askr_cache WHERE key = :key;               -- forget
DELETE FROM askr_cache;                                 -- flush all
```

### Atomic increment / decrement (counters, rate limiting)

Single-statement, so it is atomic under concurrency. Treats a missing/expired
entry as `0`:

```sql
INSERT INTO askr_cache (key, value, expires_at)
VALUES (:key, :delta, :expires_at)
ON CONFLICT(key) DO UPDATE SET
  value = CAST(
    CASE WHEN expires_at IS NOT NULL AND expires_at <= unixepoch()
         THEN 0 ELSE value END AS INTEGER) + :delta
RETURNING CAST(value AS INTEGER);
```

Decrement is the same with a negative `:delta`.

### Atomic add (SETNX — the basis for `Cache::lock()`)

Acquire only if absent or expired. The lock is held iff a row was written:

```sql
INSERT INTO askr_cache (key, value, expires_at)
VALUES (:key, :owner, unixepoch() + :ttl)
ON CONFLICT(key) DO UPDATE SET
  value = excluded.value, expires_at = excluded.expires_at
  WHERE askr_cache.expires_at IS NOT NULL
    AND askr_cache.expires_at <= unixepoch()          -- only steal an expired lock
RETURNING (value = :owner) AS acquired;
```

Release only if still the owner (prevents releasing someone else's lock):

```sql
DELETE FROM askr_cache WHERE key = :key AND value = :owner;
```

### Tags

```sql
-- On a tagged set, also record the mappings.
INSERT OR IGNORE INTO askr_cache_tags (tag, key) VALUES (:tag, :key);

-- Invalidate a whole tag.
DELETE FROM askr_cache
WHERE key IN (SELECT key FROM askr_cache_tags WHERE tag = :tag);
DELETE FROM askr_cache_tags WHERE tag = :tag;
```

### Sweep (reclaim expired rows)

A cron, background task, or trigger:

```sql
DELETE FROM askr_cache WHERE expires_at IS NOT NULL AND expires_at <= unixepoch();
```

---

## Guarantees & edge cases

- **Lazy expiry + sweep.** Reads use `askr_cache_live` so an expired value is
  never returned even before the sweep runs.
- **Atomicity.** Counters and `add`/lock are single statements — safe for many
  concurrent workers and boxes.
- **Durability / replication.** Writes are transactional and shipped to replicas;
  a cache entry (or a lock) survives a crash. For a purely local, ephemeral fast
  path use L1 (Askr shared memory) with this as the durable, shared L2.
- **Consistency of tag invalidation across nodes** is carried by replication; for
  instant cross-worker invalidation the runtime may also signal via the pub/sub
  contract ([PUBSUB_CONTRACT.md](./PUBSUB_CONTRACT.md)).
- **Large values.** Askr's L1 splits size classes (4 KB / 64 KB); the substrate
  has no fixed cap — a `BLOB` holds sessions, serialized collections and rendered
  fragments.

## Naming & configuration

Table/view names are fixed (`askr_cache`, `askr_cache_live`, `askr_cache_tags`).
TTL defaults, the sweep interval and the L1/L2 layering policy are **runtime**
settings (elyra-10 / elyra-11), not substrate settings.

## Compatibility

Contract **v1**. Additive, default-valued columns are backward compatible; other
changes require **v2** + migration.
