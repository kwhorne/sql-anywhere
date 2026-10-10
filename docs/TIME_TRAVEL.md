# Querying the past

> **Your database as it was at any moment in the backup, queried in SQL beside
> the database as it is now.**

With [bottomless](../bottomless) replication on, every change a `sqld` primary
makes is already in object storage, batch by batch, with the time it was
written. One admin call turns any moment in that history into a read-only
namespace, and `ATTACH` puts it next to the live data:

```sh
curl -X POST http://localhost:9090/v1/namespaces/orders/at \
     -d '{"timestamp": "2026-10-01T12:00:00"}'
# {"namespace": "orders@20261001T120000Z"}
```

```sql
ATTACH "orders@20261001T120000Z" AS past;

-- What changed since then?
SELECT cur.id, old.status AS was, cur.status AS is
FROM main.orders AS cur
LEFT JOIN past.orders AS old ON old.id = cur.id
WHERE old.status IS NOT cur.status;

-- Restore one row that was deleted by mistake.
INSERT INTO main.orders SELECT * FROM past.orders WHERE id = 42;
```

Verified against a mock S3:
`sqlanywhere-server/src/test/bottomless.rs::namespace_at_a_moment_is_attachable`
writes, picks a moment, writes again, and checks that the namespace at that
moment has exactly the first write, refuses writes, and joins against the live
table after `ATTACH`. It is also the only end-to-end test of forking at a
timestamp, which this is built on.

## What you get

- **A namespace named after the moment**, `<namespace>@<YYYYMMDDTHHMMSS>Z`.
  The name is deterministic, so asking for the same moment twice returns the
  namespace the first request made, and two clients asking at once share one.
- **Read-only.** Writes are refused with the reason, so history cannot be
  edited by mistake.
- **Attachable.** `allow_attach` is on, so any namespace the caller may attach
  from can `ATTACH` it. The name has `@` in it, so it is written in double
  quotes, as above. It can also be queried on its own, like any namespace.
- **Its own namespace.** It is restored once, when created, and stays until it
  is deleted with `DELETE /v1/namespaces/<name>`.

## Precision

Bottomless writes the WAL to object storage in batches, every
`max_batch_interval`, and stamps each batch with the second it was written.
Restoring to a moment replays every batch stamped at or before it, and a
transaction split across batches is applied only when its commit frame is. So
the past you get is the last committed state that had reached the backup by
that second: exact for anything older than a batch interval, and possibly a
batch behind for writes made in the last instant before it.

## Limits

- Needs bottomless replication on the primary; without it the request fails
  with "backup service not configured".
- Each moment is a full restore into a new namespace, from the latest snapshot
  before it plus the WAL batches after that, so it costs what a restore costs.
  It suits looking back, auditing and recovering rows, not running many
  moments per second.
- The namespace is not deleted for you. An expiry is a natural next step.
