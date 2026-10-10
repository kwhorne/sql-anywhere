# Live queries

> **A query result that keeps itself current and says what changed.** Run a
> `SELECT` once; afterwards, ask for the difference.

A local-first UI shows query results and has to redraw them when the data
underneath changes: after the user's own edit, after another part of the app
writes, and after an embedded replica pulls the primary's changes. The usual
answer is to re-run every query on a timer and redraw everything. A
[`LiveQuery`](../sqlanywhere/src/live.rs) does less work and says more:

- it re-runs only when something was written since it last looked, which it
  learns from one local pragma;
- it returns a `Diff` of the rows inserted, updated and deleted, or `None`
  when the result is the same, so the UI touches only the rows that moved.

Verified: [`sqlanywhere/tests/live.rs`](../sqlanywhere/tests/live.rs), and
against a real primary and embedded replica in
`sqlanywhere-server/tests/embedded_replica` (`live_query_follows_replica_sync`).

## How

```rust
use sqlanywhere::live::LiveQuery;

let mut open = LiveQuery::new(&conn, "SELECT id, title FROM todo WHERE done = 0 ORDER BY id", ())
    .await?
    .keyed_by(&[0]);            // column 0 identifies a row
render(open.rows());

// After a write, after db.sync(), or on a short timer:
if let Some(diff) = open.refresh().await? {
    for row in &diff.inserted { /* add */ }
    for (before, after) in &diff.updated { /* patch */ }
    for row in &diff.deleted { /* remove */ }
}
```

| Method | Does |
| --- | --- |
| `LiveQuery::new(conn, sql, params)` | Run the query and start watching. |
| `.keyed_by(&[cols])` | Identify rows by these columns, so an edited row is an update instead of a deletion plus an insertion. |
| `rows()`, `columns()` | The result as of the last refresh, in the query's order. |
| `is_stale()` | Whether anything was written since the last refresh. One pragma, no query. |
| `refresh()` | Re-run if stale, and return the `Diff`, or `None` if nothing changed. |

Calling `refresh` on a timer is cheap: when nothing was written it is one
local pragma and no query, so a UI can poll many live queries every few dozen
milliseconds.

## Where changes come from

| Change | How it is noticed |
| --- | --- |
| A write on the live query's own connection | `sqlite3_total_changes` |
| A commit by another connection to the same file | `data_version` |
| Frames an embedded replica pulls in with `db.sync()` | `data_version`: the frames are written by the replicator's own connection |

Both signals are needed, because SQLite deliberately leaves `data_version`
alone for a connection's own commits. A live query that watched only that
would miss every write made through the connection it runs on.

On an embedded replica, `PRAGMA data_version` is not what it seems: the
replica classifies that pragma as a write and sends it to the primary, which
answers with its own counter for a fresh stream. A live query reads
`SELECT data_version FROM pragma_data_version()` instead, which the replica
serves from its own file. The replica test fails if that is switched back.

## Diffs

With a key, a row whose key is new is **inserted**, a row whose key is gone is
**deleted**, and a row whose key stayed while another column changed is
**updated**, with its before and after. Rows that share a key are compared as
a group, without updates.

Without a key, the result is compared as a multiset: an edited row is a
deletion of the old row and an insertion of the new one. Duplicate rows are
counted, so a result that goes from two identical rows to one reports one
deletion.

## Limits

- Any write makes every live query on that file stale, and it re-runs. The
  diff is still exact, and an unchanged result returns `None`, but the cost of
  a write is one re-run per live query. Narrowing this to the tables a query
  reads is possible for writes on the same connection; it is not built.
- Live queries need the database file in this process: a local database or an
  embedded replica. A purely remote connection has nothing to watch. Pushing
  diffs from `sqld` to remote clients over Hrana is not built.
- A diff is computed from two full results, so it suits the result sizes a UI
  shows, not a million-row export.
