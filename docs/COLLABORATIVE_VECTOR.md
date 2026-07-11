# Collaborative, syncable vector index

> **The flagship combination.** CRDT offline merge × DiskANN vector search ×
> inline `embed()` — a shared, semantic knowledge base that lives on the edge and
> syncs offline, with no central server.

Most vector databases assume a central server. SQL Anywhere ships the three
pieces needed to do it the other way around, all in one embedded engine:

- **`embed()`** — turn text into a vector inline (no external service)
- **DiskANN vector index** — approximate nearest-neighbour search over those vectors
- **cr-sqlite CRDT** — conflict-free, multi-writer *offline* merge

Put together, several devices can each build a semantic index **offline and
independently**, then merge conflict-free. Afterwards **every device can
vector-search over every device's documents** — the index is maintained as
cr-sqlite applies merged rows to the base table.

This is verified: [`sqlanywhere/tests/collab_vector.rs`](../sqlanywhere/tests/collab_vector.rs)
asserts that a document indexed only on node B becomes the nearest neighbour on
node A after a merge.

## Why this is unusual

| Approach | Offline writes | Multi-writer merge | Semantic search | Infra |
|----------|:---:|:---:|:---:|-------|
| Central vector DB | ✗ | ✗ | ✓ | server + network |
| Embedded replica (read) | reads only | ✗ | ✓ | primary |
| **SQL Anywhere (this)** | ✓ | ✓ (CRDT) | ✓ | none — one file per device |

## How

1. Build the cr-sqlite extension (`scripts/build-crsqlite.sh`) or download it
   from a [release](https://github.com/kwhorne/sql-anywhere/releases).

2. On each device: load the extension, create a vector-indexed table, and mark
   it as a conflict-free replicated relation.

   ```sql
   .load ./crsqlite
   CREATE TABLE docs (
     id   INTEGER PRIMARY KEY NOT NULL,
     body TEXT NOT NULL DEFAULT '',
     emb  FLOAT32(128)
   );
   CREATE INDEX docs_vec ON docs(sqlanywhere_vector_idx(emb, 'metric=cosine'));
   SELECT crsql_as_crr('docs');
   ```

3. Index documents offline, embedding text inline:

   ```sql
   INSERT INTO docs (id, body, emb)
   VALUES (1, 'the cat sat on the mat', vector32(/* embed('the cat sat on the mat') */));
   ```

   From Rust, `embed()` does this for you:

   ```rust
   conn.execute(
       "INSERT INTO docs (id, body, emb) VALUES (?, ?, vector32(?))",
       params![id, body, sqlanywhere::embed(body, 128)],
   ).await?;
   ```

4. Sync by exchanging `crsql_changes` rows (see [docs/CRDT.md](CRDT.md)). After
   the merge, search finds documents from **all** devices:

   ```sql
   SELECT d.body
   FROM   vector_top_k('docs_vec', vector32(/* embed(query) */), 5) k
   JOIN   docs d ON d.id = k.id;
   ```

## Runnable example

```sh
scripts/build-crsqlite.sh
cargo run -p sqlanywhere --example collab_vector
```

```text
Before sync — node A only knows its own docs:
  A: search 'vehicle on the road' -> ["the cat sat on the mat", ...]

After sync — each node can search over BOTH devices' documents:
  A: search 'vehicle on the road' -> ["the car drove down the highway",
                                       "a truck delivered the heavy cargo", ...]
```

Source: [`sqlanywhere/examples/collab_vector.rs`](../sqlanywhere/examples/collab_vector.rs).

## Notes & tips

- The bundled [`embed()`](VECTOR_SEARCH.md#2-embedding-text-inline-with-embed) is
  a lexical reference embedder — great for a zero-dependency demo. For real
  semantic quality, compute embeddings with a model and store the numbers the
  same way; the CRDT + index behaviour is identical.
- Use [`compress_neighbors`](VECTOR_SEARCH.md#3-quantization-for-the-edge) to keep
  the synced index small on constrained devices.
- Combine with **hybrid search** (FTS5 + vector) for production-grade retrieval
  over the merged corpus.
- Status: **experimental**, tracking the cr-sqlite extension (not yet bundled in
  `sqld`).
