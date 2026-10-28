# Vector search & local RAG guide

This guide covers the "vector-native edge" features introduced in **0.3.0**:
inline embeddings, quantized DiskANN indexes, and hybrid (vector + keyword)
retrieval. It explains *why* each exists, *how* to use it, and gives runnable
*examples*.

> All of these are ordinary SQL over a normal SQLite-compatible database. There
> is no separate vector service to run and no special file format — a database
> that doesn't use these features stays byte-compatible with stock SQLite.

## Contents

- [Why](#why)
- [1. Storing and searching vectors](#1-storing-and-searching-vectors)
- [2. Embedding text inline with `embed()`](#2-embedding-text-inline-with-embed)
- [3. Quantization for the edge](#3-quantization-for-the-edge)
- [4. Hybrid search (vector + keyword)](#4-hybrid-search-vector--keyword)
- [5. Putting it together: local RAG](#5-putting-it-together-local-rag)
- [6. Looking inside an index](#6-looking-inside-an-index)
- [Reference](#reference)

## Why

Retrieval-augmented generation (RAG) and semantic features usually mean running
a **separate** vector database next to your primary store, keeping the two in
sync, and paying a network hop on every query. On the edge — mobile apps, CLIs,
desktop apps, embedded devices — that is often impractical.

SQL Anywhere already ships a DiskANN vector index *and* edge replication inside a
single embedded engine. 0.3.0 makes that combination practical for real
workloads:

- **`embed()`** removes the "compute embeddings elsewhere first" step for
  prototyping and lexical use cases.
- **Quantization** makes the index small enough to ship on constrained devices.
- **Hybrid search** gives production-grade retrieval quality by fusing semantic
  and keyword signals — in one SQL query, against local data.

The result: retrieval with **no extra infrastructure and no network round-trip**.

## 1. Storing and searching vectors

**Why.** Similarity search ("find the rows most like this one") powers
recommendations, semantic search and RAG. SQL Anywhere stores embeddings as a
native column type and indexes them with DiskANN for approximate
nearest-neighbour (ANN) search.

**How.** Declare a `FLOATn(dim)` column, build an index with
`sqlanywhere_vector_idx(...)`, and query with `vector_top_k(...)`.

```sql
CREATE TABLE movies (
  id    INTEGER PRIMARY KEY,
  title TEXT,
  emb   FLOAT32(4)          -- 4-dimensional f32 embedding
);

CREATE INDEX movies_idx ON movies(sqlanywhere_vector_idx(emb));

INSERT INTO movies VALUES
  (1, 'Action',   vector32('[1, 0, 0, 0]')),
  (2, 'Romance',  vector32('[0, 1, 0, 0]')),
  (3, 'Thriller', vector32('[0.8, 0.2, 0, 0]'));

-- 2 nearest neighbours of a query vector
SELECT movies.title
FROM   vector_top_k('movies_idx', vector32('[1, 0, 0, 0]'), 2) AS k
JOIN   movies ON movies.id = k.id;
-- => 'Action', 'Thriller'
```

Distances are available directly too: `vector_distance_l2(a, b)` and
`vector_distance_cos(a, b)`. Vectors come in several precisions — `vector32`,
`vector64`, `vector16`, `vector8`, `vector1bit`, `vectorb16` — and
`vector_extract(v)` reads a vector back as text.

Runnable, tested examples: [`sqlanywhere/tests/vector.rs`](../sqlanywhere/tests/vector.rs).

## 2. Embedding text inline with `embed()`

**Why.** Building a vector column normally means running an embedding model
*before* you touch the database. For prototyping, lexical search, and
zero-dependency setups that friction is unnecessary.

**How.** `sqlanywhere::embed(text, dims)` returns a normalized vector literal you
pass straight to `vector32`:

```rust
use sqlanywhere::{embed, params};

conn.execute(
    "INSERT INTO docs (body, emb) VALUES (?, vector32(?))",
    params![text, embed(text, 128)],
).await?;
```

**What it is (and isn't).** `embed()` uses the *hashing trick*: text is
tokenized into words, each word is hashed (stable FNV-1a) into one of `dims`
buckets with a signed contribution, and the result is L2-normalized. Documents
that share vocabulary get similar vectors, so cosine similarity works as a
**lexical** signal. It is deterministic and dependency-free — perfect as a
default and for tests.

It is **not** a neural/semantic embedding: no synonyms, no context. For
production semantic search, plug in a real model — see below.

**Bring your own (semantic) embedder.** `embed()` is just the built-in
[`LexicalEmbedder`]. The [`Embedder`] trait lets you feed a real model (local
ONNX/candle, or a hosted API) into the *exact same* `vector32(...)` path — the
index and `vector_top_k` behave identically no matter how the vectors were
produced:

```rust
use sqlanywhere::Embedder;

struct MyModel;
impl Embedder for MyModel {
    fn dims(&self) -> usize { 384 }
    fn embed(&self, text: &str) -> Vec<f32> {
        // call your local model or hosted API here…
        my_model::encode(text)
    }
}

// embed_literal() formats the vector for vector32(?)
conn.execute(
    "INSERT INTO docs (body, emb) VALUES (?, vector32(?))",
    params![text, MyModel.embed_literal(text)],
).await?;
```

Declare the `FLOAT32(n)` column to match `MyModel::dims()`.

**Worked example.** [`examples/semantic-search`](../examples/semantic-search)
runs a real local sentence-transformer (all-MiniLM-L6-v2 via candle) end to end:
it correctly ranks *"the cat sat on the mat"* first for the query *"a small
feline rested on a rug"* — which shares no words with it — something the lexical
embedder cannot do. It also shows how to swap in ONNX or a hosted API.

[`Embedder`]: https://docs.rs/sqlanywhere/latest/sqlanywhere/trait.Embedder.html
[`LexicalEmbedder`]: https://docs.rs/sqlanywhere/latest/sqlanywhere/struct.LexicalEmbedder.html

Tests: [`sqlanywhere/tests/embed.rs`](../sqlanywhere/tests/embed.rs).

## 3. Quantization for the edge

**Why.** A DiskANN graph stores neighbour vectors at every node. At full `f32`
precision that dominates index size — a problem when shipping to phones or
embedded devices.

**How.** Pass `compress_neighbors=` when creating the index:

```sql
CREATE INDEX movies_idx ON movies(
  sqlanywhere_vector_idx(emb, 'metric=cosine', 'compress_neighbors=float1bit')
);
```

**Measured impact** (800 × 32-dim vectors, cosine; search still returns
results):

| `compress_neighbors` | Index size | vs `float32` |
|----------------------|-----------|--------------|
| `float32` (default)  | 3352 KB   | 1× |
| `float16`            | 1744 KB   | 1.9× smaller |
| `float8`             | 1212 KB   | 2.8× smaller |
| `float1bit`          | 604 KB    | 5.5× smaller |

Lower precision trades a little recall for a lot of space. `float8` is a good
default for edge; `float1bit` is extreme compression for very large corpora.
Other index knobs: `metric=cosine|l2`, `max_neighbors`, `alpha`, `search_l`,
`insert_l`.

## 4. Hybrid search (vector + keyword)

**Why.** Pure vector search misses exact terms (names, codes, rare words); pure
keyword search misses paraphrases. Production retrieval systems combine both.

**How.** SQLite's FTS5 full-text index and the vector index live in the same
database, so you can fuse their rankings with **Reciprocal Rank Fusion (RRF)** —
`score = Σ 1/(k + rank)` across rankers — in one query:

```sql
WITH v AS (                              -- semantic (vector) ranking
  SELECT k.id, ROW_NUMBER() OVER () AS vrank
  FROM vector_top_k('docs_vec', vector32('[1,0,0,0]'), 20) k
),
f AS (                                   -- keyword (FTS5) ranking
  SELECT docs_fts.rowid AS id, ROW_NUMBER() OVER (ORDER BY rank) AS frank
  FROM docs_fts WHERE docs_fts MATCH 'ownership'
)
SELECT d.id, d.title
FROM docs d
LEFT JOIN v ON v.id = d.id
LEFT JOIN f ON f.id = d.id
WHERE v.id IS NOT NULL OR f.id IS NOT NULL
ORDER BY COALESCE(1.0/(60+v.vrank),0) + COALESCE(1.0/(60+f.frank),0) DESC;
```

Documents strong in **both** rankers surface first. `k = 60` is the usual RRF
constant; raise it to flatten the contribution of top ranks.

Tests: [`sqlanywhere/tests/hybrid_search.rs`](../sqlanywhere/tests/hybrid_search.rs).

## 5. Putting it together: local RAG

The capstone example wires all of the above into one retrieval pipeline —
inline embedding, a quantized index, an FTS5 index, and hybrid RRF — in a single
embedded database:

```sh
cargo run -p sqlanywhere --example local_rag
```

```text
Q: how does rust handle memory and ownership
  1. [Ownership] Rust enforces memory safety through ownership and borrowing.
Q: similarity search over embeddings
  1. [Vectors] A vector database indexes embeddings for similarity search.
```

Source: [`sqlanywhere/examples/local_rag.rs`](../sqlanywhere/examples/local_rag.rs).

Because it is just an embedded database, the same pipeline runs inside an
**embedded replica** (see the [Replication section of the
README](../README.md#replication--embedded-replicas)): sync documents from a
primary, then embed, index and retrieve entirely on-device.

## 6. Looking inside an index

**Why.** `search_l`, `max_neighbors` and `compress_neighbors` trade recall for
speed and size, and a search that returns rows says nothing about how good
they are or how much work they took. Tuning needs three numbers: what the
index is, how much a search reads, and how often it finds the true nearest
neighbours.

**What the index is.** `vector_index_stats('index')` returns a JSON object:

```sql
SELECT vector_index_stats('movies_idx');
-- {"type":"diskann","metric":"cosine","vector_type":"float32","dims":384,
--  "compress_neighbors":"float8","format":3,"block_size":...,"max_neighbors":...,
--  "alpha":1.2,"search_l":200,"insert_l":70,
--  "nodes":12000,"edges":...,"avg_neighbors":...,
--  "nodes_without_neighbors":0,"nodes_full":...,"bytes":...}

SELECT json_extract(vector_index_stats('movies_idx'), '$.bytes') / 1024 AS kb;
```

The parameters come with their defaults filled in. `nodes` is one per indexed
row; `avg_neighbors` against `max_neighbors` shows how connected the graph is,
and `nodes_without_neighbors` above 0 in a larger index means some rows can
only be found as a starting point. `bytes` is the size of the node blocks,
the number `compress_neighbors` shrinks. It reads every node block once, so it
is a tuning tool, not something to call per query. It works from any client,
`sqld` included, and accepts `schema.index`.

**How much a search reads.** Each node block `vector_top_k` visits counts as a
row read by the statement: `SQLANYWHERE_STMTSTATUS_ROWS_READ` from C,
`rows_read` in every Hrana result from `sqld`, and `Rows read` under `.stats on`
in the shell. With 500 points, a top-5 search read 24 rows at `search_l=8` and
206 at the default `search_l=200`.

**How often it is right.** Recall@k is the share of the exact k nearest
neighbours the index returns. Measure it in plain SQL, with the table's own
rows as queries (here every tenth):

```sql
WITH q AS (SELECT id, emb FROM movies WHERE id % 10 = 0)
SELECT sum(a.id IN (SELECT e.id FROM (
                      SELECT m.id, vector_distance_cos(m.emb, q.emb) AS d
                      FROM movies AS m ORDER BY d LIMIT 10) AS e))
       / (10.0 * (SELECT count(*) FROM q)) AS recall_at_10
FROM q, vector_top_k('movies_idx', q.emb, 10) AS a;
```

`vector_top_k` takes its query vector from a column of another table here,
once per row: a top-k per row, in a join or a correlated subquery. The exact
neighbours come from a full scan, so measure on a sample. The distance is a
selected column rather than an `ORDER BY` expression because SQLite does not
let an `ORDER BY` inside an `IN (...)` subquery refer to the outer query.

## Reference

| Function | Purpose |
|----------|---------|
| `vector32(t)` / `vector64(t)` / `vector16(t)` / `vector8(t)` / `vector1bit(t)` / `vectorb16(t)` | Build a typed vector from a `'[...]'` literal |
| `vector_extract(v)` | Read a vector back as text |
| `vector_distance_l2(a, b)` | Euclidean (L2) distance |
| `vector_distance_cos(a, b)` | Cosine distance (0 = identical, 2 = opposite) |
| `sqlanywhere_vector_idx(col, 'metric=...', 'compress_neighbors=...', ...)` | Declare a DiskANN index in `CREATE INDEX` |
| `vector_top_k('index_name', query_vec, k)` | ANN search returning the `k` nearest row ids; `query_vec` may be a column of an earlier table in the join |
| `vector_index_stats('index_name')` | The index's parameters, node and neighbour counts and size, as JSON |
| `sqlanywhere::embed(text, dims)` (Rust) | Reference lexical embedder → vector literal |

Index parameters: `type=diskann`, `metric=cosine|l2`,
`compress_neighbors=float32|float16|float8|float1bit|floatb16`, `max_neighbors`,
`alpha`, `search_l`, `insert_l`.
