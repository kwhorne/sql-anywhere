# Changelog

All notable changes to SQL Anywhere are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-06-24

**Theme: vector-native edge.** SQL Anywhere is one of the few engines to ship
native vector search *and* bi-directional edge replication in the same
file-compatible SQLite fork. 0.3.0 leans into that: it turns the vector engine
into a batteries-included toolkit for local-first / edge RAG — embed text
inline, index it compactly, and retrieve with fused semantic + keyword ranking,
all inside one embedded database.

Everything below is additive and opt-in; databases that don't use the new
features remain byte-compatible with stock SQLite.

### Added

- **Hybrid search (vector + FTS5).** Fuse DiskANN vector similarity with SQLite
  FTS5 full-text relevance in a single query using Reciprocal Rank Fusion (RRF),
  so documents strong in *both* signals rank highest — the state-of-the-art
  retrieval pattern for RAG. No new engine code: it composes primitives the
  engine already ships. Documented in the README; covered by
  `sqlanywhere/tests/hybrid_search.rs` (3 tests).

- **Vector quantization for the edge.** The DiskANN index now accepts
  `compress_neighbors=float16|float8|float1bit` to quantize the neighbour
  vectors stored in the graph. Measured on 800×32-dim cosine vectors the on-disk
  index shrinks **1.9× / 2.8× / 5.5×** respectively while search keeps working —
  a large win on memory-constrained devices. Covered by
  `sqlanywhere/tests/vector.rs` (2 tests).

- **`embed()` reference text embedder.** `sqlanywhere::embed(text, dims)` turns
  text into an L2-normalized vector literal for inline
  `vector32(embed(text, dims))`, so you can build a vector column without a
  separate pre-compute step. Uses the hashing trick (FNV-1a bag-of-words), so
  it is deterministic and dependency-free. It is *lexical*, not semantic — for
  production semantic search, compute embeddings with a real model and store
  them the same way. Covered by `src/embed.rs` (5 unit tests + doctest) and
  `sqlanywhere/tests/embed.rs` (E2E).

- **Local RAG capstone example.** `sqlanywhere/examples/local_rag.rs` is a
  runnable, end-to-end retrieval pipeline in a single embedded database:
  `embed()` → quantized (float8) DiskANN index → FTS5 keyword index → hybrid RRF
  retrieval. Run it with `cargo run -p sqlanywhere --example local_rag`.

- **`docs/VECTOR_SEARCH.md`** — a why / how / examples guide for all of the
  above, and **`docs/ROADMAP.md`** — direction for the release.

### Verification

- New automated tests for hybrid search, quantization and `embed()` all run in
  CI. Vector search and replication were re-verified end to end (see
  `sqlanywhere/tests/vector.rs`, `sqlanywhere/tests/replication.rs`, and the
  server `embedded_replica` suite).

### Notes

- CRDT offline-merge (cr-sqlite) remains on the roadmap; it requires a pinned
  nightly toolchain plus `build-std` and C linking and is tracked as a separate
  effort.

## [0.2.0] - 2026-06-23

The first stabilization release after the initial fork. Focuses on build
reproducibility, full independence from upstream packages, fixing rebrand-era
bugs, and getting CI green across Linux and Windows.

### Fixed

- **WASM user-defined functions**: corrected a truncated internal table name
  (`sqlanywhere_wasm_func_table`) caused by a hard-coded string length left over
  from the rename. WASM UDFs failed with `no such table: sqlanywhere_wasm_func_`
  before this fix. Patched in both `src/` and the bundled amalgamations.
- **Native library name**: the SQLite-compatible C library now builds
  consistently as `libsqlanywhere.{a,la,dylib}` / `sqlanywhere.lib`. The rename
  had accidentally produced an invalid `sqlite3.la` (no `lib` prefix, rejected by
  libtool) and a mangled `sqlanywhereite3` target.
- **Autotools/MSVC build**: regenerated `autoconf/Makefile.msc` from
  `Makefile.msc` so `srctree-check` passes again.
- **P0 safety issues**:
  - Documented the `Send`/`Sync` soundness of `local::Rows` (SQLITE_THREADSAFE,
    `Arc` ownership, single-task `RefCell` access).
  - Hardened `bottomless::Replicator::wait_until_snapshotted` with explicit
    control flow and clear error semantics.
  - Removed a TOCTOU race in namespace fork (redundant existence check before the
    lock-guarded check).
- Applied `cargo fmt` across the workspace.

### Changed

- **Full independence from upstream crates.io packages**: replaced the external
  `libsql-client` (dev) and `libsql-wasmtime-bindings` dependencies. The
  bottomless integration test now dogfoods the in-tree `sqlanywhere` client, and
  the WASM runtime uses the in-tree `wasmtime-bindings` crate.
- Renamed the C-binding crate `sql-experimental` → `sqlanywhere-experimental`
  (output `libsqlanywhere_experimental.a`), removing the last `libsql`-looking
  artifact.
- Modernized CI: `actions/checkout` v2/v3 → v4, `actions/cache` v3 → v4, replaced
  deprecated `actions-rs/cargo@v1` with direct `cargo` commands; Windows builds
  skip the `encryption` feature (no Visual Studio CMake generator on runners).
- Added/normalized `Cargo.toml` descriptions and keywords for all crates.
- Documentation links now point to <https://elyracode.com/docs/sqlanywhere>;
  source links point to `github.com/kwhorne/sql-anywhere`.

### Added

- `CHANGELOG.md` (this file).
- `docs/TECH_DEBT.md` — a categorized inventory of the inherited code markers
  (35 FIXME, 59 TODO, 1 HACK, 2 XXX) with recommended priorities.
- Build prerequisites table in the README (Rust, C compiler, libclang, protoc,
  cmake) with per-OS install commands; CI installs cmake where the `encryption`
  feature is built.
- `workflow_dispatch` triggers on the core CI workflows for manual runs.

### CI status

Green on Linux and Windows for: Rust (fmt/check/test/encryption), C bindings,
Extensions (vector, UDF, cr-sqlite), and the Makefile/WASM SQLite test suite.

## [0.1.0] - 2026-06-21

Initial release of **SQL Anywhere** — an embeddable, replication-ready SQL engine
built on SQLite, maintained by [Elyra](https://elyracode.com/sqlanywhere).

### Added

- Complete fork and rebrand to SQL Anywhere across the entire codebase: Rust
  crates, the SQLite C fork, FFI bindings, bundled amalgamations, and binary test
  fixtures (WASM modules and the DiskANN vector-index database).
- Embedded Rust API (`sqlanywhere`), server (`sqld` / `sqlanywhere-server`),
  Hrana remote protocol, replication primitives, and bottomless S3-backed WAL
  replication.
- Original project README, set the workspace and C-library version to `0.1.0`,
  and published the `v0.1.0` tag and GitHub release.

[0.3.0]: https://github.com/kwhorne/sql-anywhere/releases/tag/v0.3.0
[0.2.0]: https://github.com/kwhorne/sql-anywhere/releases/tag/v0.2.0
[0.1.0]: https://github.com/kwhorne/sql-anywhere/releases/tag/v0.1.0
