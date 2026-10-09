# Embedding columns

> **A vector column the database keeps in step with a text column.** Write the
> text; the embedding follows in the background, through the durable queue SQL
> Anywhere already ships.

Most applications that search by meaning keep two things in step by hand: the
text a user edits and the embedding computed from it. Embedding inline makes
every write wait on a model. Embedding later means building a job system, and
it opens three quiet failure modes: a vector computed from text that changed
while the model ran, an edit that never gets re-embedded, and a column that
mixes vectors from two models after an upgrade. An embedding column handles all
three.

Runnable from the docs of [`sqlanywhere::embedding_column`](../sqlanywhere/src/embedding_column.rs).
Verified: [`tests/embedding_column.rs`](../sqlanywhere/tests/embedding_column.rs).

## How

```rust
use sqlanywhere::{embedding_column::EmbeddingColumn, params};

// docs(id INTEGER PRIMARY KEY, body TEXT, emb FLOAT32(384)), indexed with
// sqlanywhere_vector_idx as usual.
let model = MyModel::load()?;               // any Embedder
let column = EmbeddingColumn::new("docs", "body", "emb");
column.install(&conn, &model).await?;       // enqueues the rows that exist

// Application code just writes text.
conn.execute("UPDATE docs SET body = ? WHERE id = ?", params![text, id]).await?;

// A worker, anywhere that can open the database, on a loop:
let report = column.process(&conn, &model, 100).await?;
```

| Method | Does |
| --- | --- |
| `install(conn, embedder)` | Create the queue tables if needed, record the column and its model, add the triggers, enqueue every existing row with text. |
| `process(conn, embedder, max_jobs)` | Claim and embed up to `max_jobs` rows. Safe to run from several workers at once. |
| `status(conn)` | The model, the backlog (`pending`, `ready`, `oldest_seconds`), and failed jobs. `is_current()` when nothing is pending. |
| `reembed(conn, embedder)` | Switch to another model and enqueue every row. |
| `uninstall(conn)` | Remove the triggers and pending jobs. Vectors already written stay. |

## What it guarantees

**Writes never wait for a model.** Triggers on the text column enqueue a job;
the write commits at the speed of an insert.

**The queue is the queue contract.** Jobs go into `askr_jobs` on the queue
`sqlanywhere.embed:<table>.<column>`, claimed, acked, released and
dead-lettered with the statements of the
[queue contract](contracts/QUEUE_CONTRACT.md), word for word. A unit test
compares them against the document, so the two cannot drift. That makes the
backlog durable, replicated, and visible to whatever already watches queue
depth, and a crashed worker's job comes back when its reservation lapses.

**A vector always matches the text it sits next to.** A worker embeds without
holding a transaction, so a slow model never blocks writers, and then writes
the vector only if the row still has the text it embedded. If the text changed
in the meantime, the stale vector is dropped; the edit queued its own job, which
embeds the new text.

**Edits collapse.** Ten edits before a worker gets to a row are one job, which
embeds the latest text. Writing the same text again, or changing another
column, queues nothing. Setting the text to `NULL` clears the vector.

**One model per column.** The column records `Embedder::model_id()`, and a
worker whose embedder reports another model, or another dimensionality, is
refused instead of writing. Implement `model_id` on your embedder with a name
that changes when the model does; the default only names the dimensionality.
`reembed` is the explicit way to change models: it records the new one and
enqueues every row, and `status().is_current()` says when the migration is
done. Until then the column holds vectors from both models.

**Failures back off, then stop.** An embedder that returns the wrong number of
dimensions, or text that is not text, puts the job back with the contract's
reference backoff (1 s doubling to an hour). On its last attempt it moves to
`askr_failed_jobs` with the reason, and `status().failed` counts it.

## Limits

- The table needs a rowid; `WITHOUT ROWID` tables are refused.
- `Embedder::embed` is synchronous, so a hosted model blocks the worker task
  while it runs. Run workers on their own task or thread.
- Embedding happens in the application process that calls `process`. A
  `sqld`-side worker, and an `embed()` SQL function, are not built yet.
