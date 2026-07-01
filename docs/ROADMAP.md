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
- [x] **`embed()` helper** — dependency-free reference embedder
      (`sqlanywhere::embed`) that turns text into a vector literal for
      `vector32(embed(text, dims))`, no external pre-compute. Uses the hashing
      trick (lexical, not semantic). Covered by `sqlanywhere/tests/embed.rs`.
      Next: a pluggable semantic backend (local ONNX / hosted API) and a
      SQL-level `embed()` UDF for use from `sqld`. *(reference embedder shipped)*
- [x] **Quantization** — opt-in via `compress_neighbors=` on the index (up to
      ~5.5× smaller). Next: auto-select a default based on index size and a
      recall/size knob. *(shipped opt-in; auto-default still open)*
- [ ] **`EXPLAIN` for the DiskANN index** — expose visited-node counts and a
      recall estimate for query tuning. *(medium effort)*

### Edge / local-first
- [ ] **CRDT offline merge** — activate the vendored cr-sqlite (`ext/crr`) to
      allow conflict-free multi-writer offline sync. Most differentiating,
      largest effort. *(high effort, high value)*
- [ ] **Selective / partial replication** — replicate only rows matching a
      predicate (per-user / per-tenant) instead of the whole database.
      *(high effort)*
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
- [ ] **OpenTelemetry / Prometheus dashboards** — ready-made metrics + tracing
      export for `sqld`. *(low–medium effort)*
- [ ] **Bottomless point-in-time recovery UX** — friendlier restore workflow on
      top of the existing S3 backup. *(medium effort)*
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
