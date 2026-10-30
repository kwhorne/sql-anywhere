# SQL Anywhere Roadmap

This is a living document of where SQL Anywhere is headed. It is intentionally
opinionated: the differentiating bet is that SQL Anywhere is one of the few
engines to ship **native vector search *and* bi-directional edge replication**
in the same file-compatible SQLite fork. The roadmap leans into that
combination — a **vector-native edge database** for local-first and RAG.

## Shipped

- **0.2.0** — stabilised fork: full rebrand, green CI (Linux + Windows),
  prebuilt `sqld` binaries (macOS Apple Silicon, Ubuntu Intel/ARM), independence
  from upstream crates, verified vector search and replication.
- **Hybrid search (vector + FTS5)** — RRF fusion of DiskANN and full-text
  ranking in a single query. Documented in the README and covered by
  `sqlanywhere/tests/hybrid_search.rs`. *(landed post-0.2.0)*
- **Vector quantization** — `compress_neighbors=float16|float8|float1bit` on the
  DiskANN index shrinks the on-disk index up to ~5.5× for edge devices.
  Documented in the README and covered by `sqlanywhere/tests/vector.rs`.
  *(landed post-0.2.0)*

## 0.3.0 — "vector-native edge" (proposed)

Ordered by value / risk. Items build on capabilities that already exist in the
engine wherever possible.

### AI-native

- [x] **Conformance-tested storage contracts** — the queue/cache/pub-sub
      contracts in [docs/contracts/](contracts/README.md) are executable specs
      verified by `sqlanywhere/tests/contract_conformance.rs`, so the Askr
      runtime (substrate × runtime, epic elyra-2) builds against a proven seam.
- [x] **A typed API compiled from the contracts** — `cargo xtask gen-contracts`
      turns the annotated contract documents into `sqlanywhere::contracts`
      (`queue::claim(&conn, visibility, queue)` and 21 more) and a
      language-neutral `contracts.json`, and CI fails when a document changes
      without them. Askr can call the contracts instead of transcribing them.
      See [docs/contracts/README.md](contracts/README.md#generated-api).
      The same generator writes a PHP client on PDO (`clients/php`), tested
      in CI on PHP 8.1 and 8.4, and a TypeScript client
      (`clients/typescript`) with adapters for `@libsql/client` and
      `node:sqlite`, tested in CI on Node 22 and 24 and locally against
      `sqld` over HTTP. Next: publishing both packages.
- [x] **Storage primitives as chapters, not products** — a KV cache with TTL, a
      durable work queue (atomic `UPDATE … RETURNING` claim, at-least-once), and
      pub/sub over the replication log, all composed in plain SQL. See
      [docs/STORAGE_PRIMITIVES.md](STORAGE_PRIMITIVES.md); verified
      (`sqlanywhere/tests/storage_primitives.rs`).
- [x] **Search as a chapter, not a product** — full-text (FTS5), faceted
      (`GROUP BY`), vector (DiskANN) and hybrid (RRF) are one engine, composed in
      plain SQL. See [docs/SEARCH.md](SEARCH.md); faceted search verified
      (`sqlanywhere/tests/faceted_search.rs`).
- [x] **Collaborative, syncable vector index** — CRDT × vector search × `embed()`:
      a shared semantic index that several devices build offline and merge
      conflict-free, then search over all documents. Verified
      (`sqlanywhere/tests/collab_vector.rs`) and documented
      ([docs/COLLABORATIVE_VECTOR.md](COLLABORATIVE_VECTOR.md)).
      *(experimental; landed post-0.3.1)*
- [x] **`embed()` helper** — dependency-free reference embedder
      (`sqlanywhere::embed`) that turns text into a vector literal for
      `vector32(embed(text, dims))`, no external pre-compute. Uses the hashing
      trick (lexical, not semantic). Covered by `sqlanywhere/tests/embed.rs`.
      Next: a pluggable semantic backend (local ONNX / hosted API) and a
      SQL-level `embed()` UDF for use from `sqld`. *(reference embedder shipped)*
- [x] **Embedding columns** — a vector column kept in step with a text column:
      triggers enqueue jobs on the queue contract's own tables, workers embed
      in the background and never write a vector over text that changed while
      the model ran, and each column records its model so a worker with another
      one is refused until an explicit `reembed`. See
      [docs/EMBEDDING_COLUMNS.md](EMBEDDING_COLUMNS.md); verified
      (`sqlanywhere/tests/embedding_column.rs`). Next: a `sqld`-side worker.
- [x] **Local RAG capstone example** — `sqlanywhere/examples/local_rag.rs` ties
      embed + quantized DiskANN + FTS5 + hybrid RRF into one runnable retrieval
      pipeline. *(landed post-0.2.0)*
- [x] **Quantization** — opt-in via `compress_neighbors=` on the index (up to
      ~5.5× smaller). Next: auto-select a default based on index size and a
      recall/size knob. *(shipped opt-in; auto-default still open)*
- [x] **Interests: standing vector queries** — register a vector and a
      distance, and every write that puts a row's vector within reach
      publishes on the pub/sub contract, in the same transaction, whoever
      wrote it (app, embedding worker, merge, replica's primary). See
      [docs/INTERESTS.md](INTERESTS.md); verified
      (`sqlanywhere/tests/interests.rs`). Next: a vector index over the
      interests themselves for very many of them.
- [x] **Looking inside a DiskANN index** — `vector_index_stats('idx')` returns
      the index's parameters, node and neighbour counts and size as JSON;
      every node a search visits counts as a row read (`rows_read`); and
      recall@k is measured in plain SQL, now that `vector_top_k` takes its
      query from another table's column. See
      [docs/VECTOR_SEARCH.md](VECTOR_SEARCH.md#6-looking-inside-an-index);
      verified (`sqlanywhere/tests/vector.rs`). `search_l` is set per query,
      as `vector_top_k`'s fourth argument, and `sqlanywhere::recall::tune`
      finds the smallest that reaches a recall target
      (`sqlanywhere/tests/recall.rs`). Next: the same for
      `compress_neighbors`, which needs an index rebuilt per candidate.
- [x] **Hybrid search in one call** — `hybrid_top_k(vector_index, fts_table,
      vector, query, k [, rrf_k [, search_l [, vector_weight [, fts_weight]]]])`
      fuses the DiskANN and FTS5 rankings with Reciprocal Rank Fusion inside
      the engine, returning each row's score and its rank in both; the same
      answer as the 15-line RRF query, shown equal on a 300-row corpus, with
      and without weights. Either ranking can be weighted over the other. See
      [docs/VECTOR_SEARCH.md](VECTOR_SEARCH.md#4-hybrid-search-vector--keyword);
      verified (`sqlanywhere/tests/hybrid_search.rs`). Next: learning the
      weights from relevance judgements.

### Edge / local-first
- [x] **CRDT offline merge (experimental)** — the vendored cr-sqlite (`ext/crr`)
      builds into a loadable extension via `scripts/build-crsqlite.sh` and turns
      tables into conflict-free replicated relations for offline multi-writer
      sync. Verified end to end (two nodes converge, concurrent conflicts
      resolve deterministically). See [docs/CRDT.md](CRDT.md). Prebuilt
      extensions are attached to releases for macOS Apple Silicon and Ubuntu
      Intel/ARM. Next: optional static linking into `sqld`.
      *(experimental; per-platform artifacts shipped)*
- [x] **Database branches (experimental)** — fork a database, write to the
      fork freely, review a diff (with how far each embedding moved), and merge
      it back conflict-free while the parent keeps taking writes; shaped for AI
      agents that work on their own branch and are merged after review. Jobs
      and events the branch queues wait for the merge and are carried exactly
      once, and `merge_if` merges on a policy (no conflicts, read-only tables,
      no deletes, how far embeddings may move, …) so harmless changes merge
      on their own. See [docs/BRANCHES.md](BRANCHES.md); verified
      (`sqlanywhere/tests/branch.rs`).
      `fork_file` forks a large database by copying the file (~19× faster at
      20k rows). A `sqld` namespace is branched with
      `POST /v1/namespaces/:ns/branch/:name` (the branch and a frozen base)
      and merged back three ways by `sqlanywhere::merge3`, row by row, no
      CRDT (`namespaces::branch_a_namespace_and_merge_it_back`). Next: a lens
      for namespace branches.
- [x] **Live queries** — a query result that keeps itself current and returns a
      row-level diff (inserted / updated / deleted) instead of a fresh result,
      re-running only when something was written. Follows the connection's own
      writes, other connections, and embedded replica sync. See
      [docs/LIVE_QUERIES.md](LIVE_QUERIES.md); verified
      (`sqlanywhere/tests/live.rs`, and against a real primary in
      `embedded_replica::live_query_follows_replica_sync`). With `live::track`
      a live query re-runs only for writes to the tables it reads, found from
      its own `EXPLAIN`, on replicas too. Over a connection to `sqld`,
      `LiveQuery::wait` waits for the server to announce a commit to a tracked
      table (`/beta/listen`, server-sent events) and returns the diff it
      made: no polling (`sqlanywhere-server/tests/standalone/live.rs`). Next:
      the same over a Hrana WebSocket, and the server computing one diff for
      many clients.
- [ ] **Selective / partial replication** — replicate only rows matching a
      predicate (per-user / per-tenant) instead of the whole database. First
      step shipped: partial branches, where a `Lens` (a relational or
      `vector_top_k` predicate per table) chooses the rows a branch holds and
      `pull_lens` keeps them current (see
      [docs/BRANCHES.md](BRANCHES.md#partial-branches-a-lens)). Next: the same
      between a device and `sqld`, over Hrana. *(high effort)*
- [ ] **Browser / WASM build** — a working `sqlanywhere` in the browser with
      OPFS persistence + sync, from the existing `bindings/wasm`. *(medium–high
      effort)*

### Developer experience
- [ ] **Official Elyra TypeScript & Python clients** — replace the last external
      `libsql-client` alias with first-party `@elyra/sql-anywhere` clients.
      *(medium effort)*
- [ ] **`sqld` web dashboard** — inspect databases, run queries, view
      replication status. *(medium effort)*

### Operations
- [x] **Signed extension repository** — releases carry `SHA256SUMS`, a
      `MANIFEST.json` (per-artifact digest, size and extension interface
      version) and a detached Ed25519 signature over it, driven by
      `cargo xtask {extension-keygen,sign-extensions,verify-extensions}`.
      Replaces download-a-`.so`-and-hope for the prebuilt cr-sqlite extension.
      See [docs/EXTENSION_REPOSITORY.md](EXTENSION_REPOSITORY.md). Next: decide
      whether the loader itself should enforce signatures (needs a key
      distribution and local-development story; the open questions are written
      down in that doc).
- [x] **Versioned extension ABI** — `sqlanywhere_api_routines` carries an
      `iVersion` first member with `SQLANYWHERE_API_VERSION` and a
      `SQLANYWHERE_API_ATLEAST(V)` guard, so an extension built against a newer
      header cannot read past an older host's thunk. Verified
      (`sqlanywhere-sqlite3/test/rust_suite/src/extension_abi.rs`).
- [ ] **OpenTelemetry / Prometheus dashboards** — ready-made metrics + tracing
      export for `sqld`. *(low–medium effort)*
- [x] **Querying the past** — `POST /v1/namespaces/:ns/at` turns any moment in
      the bottomless backup into a read-only namespace that the live one can
      `ATTACH`, so history and the present are compared, and deleted rows
      recovered, in one SQL query. See [docs/TIME_TRAVEL.md](TIME_TRAVEL.md);
      verified against a mock S3, which is also the first end-to-end test of
      forking at a timestamp. An optional `ttl_seconds` deletes them, backups
      included, and survives restarts. `compare::table` turns the two versions
      of a table into a typed diff with how far each embedding moved, so
      "which documents changed meaning since Tuesday" is one call.
- [ ] **Windows `sqld` port** — port the replication layer's positional file
      I/O (`pread`/`pwrite`) and atomic rename to Windows. *(medium effort,
      deferred from 0.2.0)*

## Guiding principles

1. **Stay a drop-in SQLite** — never break the file format for databases that
   don't opt into extra features.
2. **Edge-first** — every feature should make sense running inside an
   application process, offline, close to the user.
3. **Batteries included, but composable** — prefer SQL-level primitives
   (functions, indexes) that users can combine, over rigid APIs.

Have an idea or want to pick something up? Open an issue or a PR.
