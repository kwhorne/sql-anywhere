# Search

> **Search is a feature of SQL Anywhere, not a separate product — a chapter, not
> a service.** Full-text, faceted, vector, and hybrid search are all the *same
> engine*: one database file, ordinary SQL, no dedicated search server to
> deploy, sync, or keep consistent.

Most stacks bolt a search product (Elasticsearch, Meilisearch, Typesense, a
managed vector DB) *next to* the primary database, then spend real effort
shipping data between them and reconciling when they drift. SQL Anywhere folds
all of it into the database you already have. The vector engine and the
full-text engine share the same rows; a facet is a `GROUP BY`; a hybrid ranking
is a `JOIN`.

## The four capabilities, one engine

| Capability | Mechanism | Deep dive |
|-----------|-----------|-----------|
| **Full-text** | FTS5 inverted index (`MATCH`, `rank`) | this page |
| **Faceted** | `GROUP BY` / `WHERE` over the matched set | this page |
| **Vector (semantic)** | DiskANN index (`vector_top_k`) + `embed()` / pluggable [`Embedder`] | [VECTOR_SEARCH.md](VECTOR_SEARCH.md) |
| **Hybrid** | Vector + FTS5 fused with Reciprocal Rank Fusion | [`tests/hybrid_search.rs`](../sqlanywhere/tests/hybrid_search.rs) |

And because it is all just tables, search **syncs** with everything else:
embedded replicas for read scale-out, CRDT for offline multi-writer merge, even a
[collaborative vector index](COLLABORATIVE_VECTOR.md).

## Full-text search

Add an FTS5 inverted index over the text column(s), kept in sync with the base
table:

```sql
CREATE TABLE products (id INTEGER PRIMARY KEY, title TEXT, brand TEXT,
                       category TEXT, price REAL);

-- Inverted index over `title`, backed by the products table.
CREATE VIRTUAL TABLE products_fts USING fts5(title, content='products', content_rowid='id');
INSERT INTO products_fts(rowid, title) SELECT id, title FROM products;

-- Query it. `rank` is FTS5's relevance score.
SELECT p.title, p.category
FROM   products_fts f JOIN products p ON p.id = f.rowid
WHERE  products_fts MATCH 'wireless'
ORDER  BY rank;
```

(In production, keep `products_fts` current with triggers on
`INSERT`/`UPDATE`/`DELETE`, per the FTS5 external-content pattern.)

## Faceted search

A facet is just an aggregate over the *same* matched result set — count how many
matches fall into each category, brand, price bucket, etc.:

```sql
-- Counts per category for the current query.
SELECT p.category, count(*)
FROM   products_fts f JOIN products p ON p.id = f.rowid
WHERE  products_fts MATCH 'wireless'
GROUP  BY p.category
ORDER  BY 2 DESC;
--   peripherals | 2
--   audio       | 1
```

**Drill-down** is the same query with more `WHERE` constraints — combine
full-text, a facet selection, and a numeric range in one statement:

```sql
SELECT p.title, p.price
FROM   products_fts f JOIN products p ON p.id = f.rowid
WHERE  products_fts MATCH 'wireless'
  AND  p.category = 'peripherals'
  AND  p.price < 55;
--   wireless keyboard | 49.0
```

Runnable: [`examples/faceted_search.rs`](../sqlanywhere/examples/faceted_search.rs).
Verified: [`tests/faceted_search.rs`](../sqlanywhere/tests/faceted_search.rs).

## Vector and hybrid

Semantic search and the recommended hybrid (vector + keyword) ranking are
covered in [VECTOR_SEARCH.md](VECTOR_SEARCH.md). The point of *this* chapter is
that they are not a different system — the `products` table above can grow a
`FLOAT32(n)` column and a DiskANN index and participate in the same queries.

## Why this is a chapter, not a product

- **No second datastore.** Your rows and your index are the same database file;
  there is nothing to ETL, no eventual-consistency window between "the record"
  and "the search document."
- **Transactions.** A write and its index update commit together.
- **It runs at the edge.** Full-text, facets, and vectors work in-process on a
  phone, a laptop, or a `sqld` node — offline, and they sync.
- **It's just SQL.** Facets, filters, ranking, and joins compose with the rest
  of your schema instead of a bespoke query DSL.

If you genuinely need a horizontally-sharded, billion-document search cluster,
use a dedicated engine. For the search that lives *inside* an application —
catalog search, docs search, RAG retrieval, in-app filtering — it belongs here.

[`Embedder`]: ../sqlanywhere/src/embed.rs
