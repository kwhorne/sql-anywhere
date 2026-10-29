# Live queries

> **A query result that keeps itself current and says what changed.** Run a
> `SELECT` once; afterwards, ask for the difference.

A local-first UI shows query results and has to redraw them when the data
underneath changes: after the user's own edit, after another part of the app
writes, and after an embedded replica pulls the primary's changes. The usual
answer is to re-run every query on a timer and redraw everything. A
[`LiveQuery`](../sqlanywhere/src/live.rs) does less work and says more:

- it re-runs only when something was written since it last looked, which it
  learns from one small local query, and, once its tables are tracked, only
  when one of *its* tables was written;
- it returns a `Diff` of the rows inserted, updated and deleted, or `None`
  when the result is the same, so the UI touches only the rows that moved;
- over a connection to `sqld`, it can [wait](#live-queries-over-sqld) for the
  server to announce a commit instead of asking.

Verified: [`sqlanywhere/tests/live.rs`](../sqlanywhere/tests/live.rs), and
against a real primary and embedded replica in
`sqlanywhere-server/tests/embedded_replica` (`live_query_follows_replica_sync`,
`a_tracked_live_query_on_a_replica`); waiting on `sqld` in
`sqlanywhere-server/tests/standalone/live.rs`.

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
| `is_stale()` | Whether anything was written since the last refresh. One small query; the live one is not run. |
| `refresh()` | Re-run if stale, and return the `Diff`, or `None` if nothing changed. |
| `watched_tables()` | The tables whose writes make it stale, or `None` when any write to the file does. |
| `wait()` | On a connection to `sqld`: wait until a commit changes the result, and return the `Diff` ([below](#live-queries-over-sqld)). |
| `live::track(conn, table)`, `live::untrack(conn, table)` | Make a table's writes distinguishable from other tables' (below). |

Calling `refresh` on a timer is cheap: when nothing was written it is one
small local query and no re-run, so a UI can poll many live queries every few
dozen milliseconds.

## Watching only the tables a query reads

By default a write to *any* table makes every live query on the file stale:
the counters SQLite offers are per file. In an app that also keeps a queue, a
cache or embedding jobs in the same file, that is a re-run per live query for
every job claimed. Tracking narrows it:

```rust
use sqlanywhere::live;

live::track(&conn, "todo").await?;   // once, like a migration
let open = LiveQuery::new(&conn, "SELECT id, title FROM todo WHERE done = 0", ()).await?;
assert_eq!(open.watched_tables(), Some(&["todo".to_string()][..]));
// Writes to askr_jobs, other tables, …: open.is_stale() stays false.
```

`track` gives the table an `AFTER INSERT`, `AFTER UPDATE` and `AFTER DELETE`
trigger that bump its row in `sqlanywhere_live_tables`. A live query works out
the tables it reads from its own `EXPLAIN`: the b-trees the plan opens, mapped
to their tables through `sqlite_schema`, so views, joins, subqueries and
covering indexes all lead to the tables underneath. When every one of them is
tracked it compares their versions instead of the file's counters.

Why triggers rather than a hook in the connection:

- **They see every writer.** The versions are rows written in the same
  transaction as the change, so another connection's commit, and frames an
  embedded replica pulls in with `db.sync()`, carry them along with the data.
  An update hook sees only its own connection, and nothing at all on a replica,
  where writes are made on the primary.
- **They do not miss deletes.** `DELETE FROM t` without a `WHERE` takes
  SQLite's truncate shortcut, which skips the update hook. A table with
  triggers does not take the shortcut.

A live query keeps watching the whole file when it reads a table that is not
tracked, a virtual table (FTS5, `vector_top_k`, a table-valued pragma), or a
table in another schema (`temp`, an attached database). A table counts as
tracked only while it has both its version row and its three triggers: `DROP
TABLE` takes the triggers with it, so a table created again under the same name
is not tracked until `track` runs again. Any schema change, including one made
on the live query's own connection, makes it stale and has it work out its
tables again; tracking a table therefore takes effect on running queries at
their next refresh.

The cost is one extra row update per row written to a tracked table. On an
embedded replica, `track` is DDL and goes to the primary like any write; the
tables a live query reads are worked out from the replica's own file, so
creating one does not need the primary.

## Live queries over `sqld`

A client with no database file of its own, connected to `sqld` over HTTP, can
keep a result current without polling. `wait` holds until the server announces
a commit that changes the result, and returns the diff:

```rust
let db = Database::open_remote("https://db.example.com", token)?;
let conn = db.connect()?;
live::track(&conn, "todo").await?;   // once, like a migration

let mut open = LiveQuery::new(&conn, "SELECT id, title FROM todo WHERE done = 0", ())
    .await?
    .keyed_by(&[0]);
loop {
    let diff = open.wait().await?;   // another client's commit, pushed
    apply(&diff);
}
```

`sqld` announces commits as server-sent events on `GET /beta/listen?table=…`,
one event per commit to the table, with counts of rows inserted, updated and
deleted. A live query listens to `sqlanywhere_live_tables`, where the version
triggers of its tracked tables write, so one stream covers however many
tables it reads. Each announcement is followed by a `refresh`: the versions
say whether one of *its* tables moved, and only then does the query re-run.
Commits that do not change the result are waited through.

- **Every table the query reads must be tracked.** Announcements come from
  SQLite's update hook, which says nothing about `WITHOUT ROWID` tables; the
  version triggers fire for those too, on a table it does report. `wait` on a
  query that reads an untracked table fails at once and says so.
- **Nothing is lost between results.** The server subscribes before it answers
  the request, and the first `wait` refreshes once it is listening, so a
  commit made after the last result and before listening is returned at once.
  Dropping a `wait` (a timeout) keeps the subscription; the next one goes on.
- **An announcement follows its commit.** The server sends it once the
  statement that committed has finished, not from the commit hook, which runs
  before the commit is done: a client that reads straight away sees the
  change.
- **Connect to the primary.** Commits reach a replica `sqld` as frames, past
  the hooks that announce them, so a replica turns listeners away: it redirects
  them to its primary when it knows the primary's URL, and refuses them (`database is
  not a primary`) when it does not. `wait` reports either at once rather than
  waiting for announcements that would never come.
- **A dropped stream is an error.** If the server goes away, `wait` returns
  the error; the next `wait` listens again and catches up.

On a local database or an embedded replica `wait` fails without waiting: there
is no server to announce commits, and `refresh` after writes or a sync is the
way to go.

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

- Without tracking, any write makes every live query on that file stale, and
  it re-runs. The diff is still exact, and an unchanged result returns `None`,
  but the cost of a write is one re-run per live query. Tracking narrows this
  to tables, not rows: a write to a tracked table re-runs every live query that
  reads it, whether or not the row is in its result.
- On a remote connection, a live query needs its tables tracked: the file's
  own counters belong to the server's connection, not the client's. The
  server does not compute diffs: each client re-runs its query after an
  announcement that concerns it, so many clients on one busy table mean many
  re-runs.
- A diff is computed from two full results, so it suits the result sizes a UI
  shows, not a million-row export.
