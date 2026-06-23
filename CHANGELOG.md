# Changelog

All notable changes to SQL Anywhere are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.2.0]: https://github.com/kwhorne/sql-anywhere/releases/tag/v0.2.0
[0.1.0]: https://github.com/kwhorne/sql-anywhere/releases/tag/v0.1.0
