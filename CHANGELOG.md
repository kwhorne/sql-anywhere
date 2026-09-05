# Changelog

All notable changes to SQL Anywhere are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.2] - 2026-09-05

### Changed

- **The shutdown signal is now a level-triggered `CancellationToken`, and the
  `Notify` on `Server.shutdown` is deprecated.** `Notify::notify_waiters` is
  edge-triggered: it reaches only the tasks already parked on `notified()` at
  that instant, and a task not yet polled misses it for good and runs on
  forever. That is the shutdown hang. #36 and #40 narrowed the window by polling
  early; this closes it, because a cancelled token stays cancelled and a
  listener that only starts afterwards still sees it. `Server::shutdown_token()`
  is the new initiator. The old field still works: `start` bridges it into the
  token, so existing callers keep the narrow window they have today rather than
  breaking. `Server` gains a public `shutdown_token` field. That is a compile-time
  break for anyone constructing it by struct literal without
  `..Default::default()`; such callers add the field or the spread.

- **The storage contracts are now the source of the SQL their tests run.**
  `contract_conformance.rs` held its own copy of every contract statement, so
  "executable spec" meant a test that contained a transcription of the document
  rather than the document itself. Nothing stopped the two from drifting, and
  the queue's claim is a subtle atomic `UPDATE` that a consumer has to reproduce
  exactly. The tests now read the SQL out of `docs/contracts/*.md` and execute
  it verbatim, so editing a contract either proves the new semantics or turns
  the test red. All 18 operations the contracts publish with SQL are now executed
  from the document, so every one can be mutation-checked, and
  `every_contract_operation_is_covered_or_listed` fails if that slips.

### Fixed

- **The lock's release statement was transcribed into the test rather than read
  from the contract, and the coverage guard could not see it.** The lock
  operation publishes two SQL blocks under one heading, acquire and release. The
  extractor reached only the first, so release was a hand copy in the test, and
  the guard counted headings rather than blocks, so a heading with an uncovered
  second block looked fully covered. Both are fixed: the extractor takes a block
  index, bounded to the heading's own section so an index past the end fails
  loudly instead of reading the next operation, and the guard now enumerates
  every block. Release is executed from the document, and mutating it either way,
  ignoring the owner or deleting nothing, turns `cache_contract_v1` red. Every
  block the three contracts publish is now covered.

- **The cache lock contract read as though `acquired` could be `0`.** The
  SETNX statement ends in `RETURNING (value = :owner) AS acquired`, which
  invites a consumer to branch on that value. It never is `0`: `RETURNING` only
  fires for a row that was written, and a written row always carries `:owner`,
  so the comparison is redundant with the `WHERE` guard. The real signal is
  whether a row came back at all, and a consumer that treated an empty result as
  an error, or waited for a `0` that cannot arrive, would have been misled. The
  prose now says so plainly, and the SQL carries a comment; the statement itself
  is unchanged, so nothing built against it moves. Found by mutation testing:
  replacing the expression with a constant `1` changed nothing.

- **Three further contract guarantees were documented but unverified**, found
  the same way once the remaining operations were executed from the documents.
  Tag invalidation was never shown to be scoped to its tag, because the test had
  only one tag, so deleting every tagged key passed. The sweep was never shown
  to spare live rows, because the test flushed the table immediately after and
  asserted it was empty, which a sweep of `DELETE FROM askr_cache` also passes.
  The subscriber cursor was never shown to advance, because it was only ever
  saved once, which cannot tell an upsert from an insert that ignores the
  conflict. All three are asserted now.

- **Two more contract guarantees were documented but unverified.** Mutating the
  cache contract found them, the same way mutating the queue contract found the
  delay gap. The increment says it "treats a missing/expired entry as `0`", but
  removing that reset left `cache_contract_v1` green, because nothing ever
  incremented an expired counter. The upsert carries
  `expires_at = excluded.expires_at` so an overwrite moves the expiry, but
  removing that left the test green too, because nothing ever overwrote a key
  with a different TTL. Both are asserted now.

- **The queue contract's delay semantics were not actually verified.** Making
  the document the source made it possible to check the tests for teeth by
  mutating the contract, and one mutation slipped through: removing the
  `available_at <= unixepoch()` guard from the claim, which lets a delayed job
  be taken before its time, left `queue_contract_v1` green. The test drained the
  queue and then asserted it was empty, which cannot distinguish "the delayed
  job was correctly skipped" from "the delayed job was claimed during the
  drain". It now asserts the delayed job is never among what is claimed.

## [0.6.1] - 2026-09-04

Three real bugs found by chasing tests that looked like noise. Every
embedded-replica connection was being closed twice, the server library killed
the host process on a failed shutdown, and shutdown itself could hang
indefinitely. None of them were visible as failures; all three were sitting in
test output nobody read because the tests around them were usually green.

No API or file-format changes.

### Changed

- **Shutdown now completes instead of hanging, at the cost of dropping
  connections that will not drain.** `sqld` stopped accepting new connections
  when signalled and then waited for the in-flight ones with no deadline, so a
  client holding a connection open blocked shutdown for as long as it liked.
  In practice that meant shutdown hung until `shutdown_timeout` and then failed,
  which is a slow deploy, a container that will not die, and eventually a
  SIGKILL from whatever is supervising it. Each HTTP service now gets a bounded
  drain window after the signal, and anything still in flight when it closes is
  dropped. The window is derived from `shutdown_timeout` rather than configured
  separately, so there is still one knob and the outer timeout goes back to
  being a backstop rather than the mechanism. Reproduced at roughly one run in
  four on slow storage before, 40 out of 40 clean after, and the full server
  suite is green across repeated runs.


- **The server library no longer calls `std::process::exit` on a failed
  shutdown.** `Server::start` killed the host process when graceful shutdown
  failed or timed out. That is wrong for a library: the integration tests run
  the server in-process, and so does anything embedding sqld, so a shutdown
  problem took the whole host down with it and left nothing to diagnose. It now
  returns an error. The `sqld` binary is unaffected, since `main` propagates it
  and still exits non-zero with the same message.

- **`extension-keygen` no longer writes the private key into the repository.**
  It defaulted the whole key pair to `sqlanywhere-sqlite3/ext/`, so the trust
  root's private half landed in a working tree, where `.gitignore` is the only
  thing standing between it and a `git add -f`, a stray archive, or an `rsync`.
  The public half still goes to the repo, since CI reads it from a fresh
  checkout; the private half now goes to `$XDG_CONFIG_HOME/sqlanywhere/`
  (falling back to `~/.config/sqlanywhere/`), in a directory created mode 700
  with the key mode 600. Writing it inside a working tree is refused outright,
  including via a relative path that climbs back in, rather than merely warned
  about. Both paths are overridable with `--pubkey` and `--secret`, and the
  command now prints the exact `gh secret set` line to run next.

### Fixed

- **Every embedded-replica connection was closed twice.**
  `SqlanywhereConnection`'s `Drop` called `disconnect()`, and then the
  `Connection` field it had just called it on was itself dropped, whose `Drop`
  called `disconnect()` again on the same value. The `drop_ref` count is
  identical at both calls, so both concluded they were the last owner and both
  called `sqlite3_close_v2` on the same handle. The refcount guard answers "is
  anyone else still using this connection", not "have I already closed it".
  Closing an already-freed handle is undefined behaviour, survivable here only
  by luck. SQLite reported the second close as `SQLITE_MISUSE`, which had been
  sitting unread in the embedded-replica test output as `sqlite error 21: API
  call with invalid database connection pointer`, next to `misuse at line
  185183`, the `SQLITE_MISUSE_BKPT` in `sqlite3Close`. `disconnect()` now clears
  the handle it closed, which makes it idempotent, and is pinned by
  `disconnect_closes_once`, a test that fails without the fix. Present in 0.6.0
  and every earlier release; no database or file format is affected.

- **Two flaky tests in `sqlanywhere-server`, with unrelated causes.**
  `test_many_concurrent` asserted that opening a write transaction always
  succeeds. It does not: `ConnectionManager::acquire` returns `SQLITE_BUSY` on
  purpose while a checkpoint holds the slot, so that writers cannot starve the
  checkpointer, and the test crossed the auto-checkpoint threshold often enough
  to meet one on a loaded runner. It now retries on a busy error, bounded, with
  the error classification pinned by its own test.
  `local_sync_with_writes` exhausted a 120s simulated budget that in practice
  measured disk speed rather than protocol steps, because the test performs real
  file I/O inside a simulation whose clock advances independently of it:
  reproduced at 0/12 on overlayfs against 12/12 on tmpfs with the same binary,
  and raised to the 1000s the rest of these simulations use. The full server
  suite now runs 200/200 with no retries.

## [0.6.0] - 2026-09-03

Extensions you can trust. The loadable-extension ABI can now describe itself, so
a version mismatch is something an extension detects rather than crashes on, and
every extension this project publishes ships with a signed manifest saying
exactly what it is.

**This release changes the extension ABI.** Prebuilt extensions from 0.5.2 and
earlier must be re-downloaded; see below.

### Breaking

- **The extension thunk's layout changed: extensions built against 0.5.2 or
  earlier must be rebuilt or re-downloaded.** `iVersion` is now the first member
  of `sqlanywhere_api_routines`, which moves `close_hook`. An extension compiled
  against an older `sqlite3ext.h` reads offset 0 expecting a function pointer,
  finds the interface-version integer instead, and calls it; loading one into
  0.6.0 kills the process (verified: SIGBUS). This affects the prebuilt
  `crsqlite-*` archives attached to every release from `v0.3.1` through
  `v0.5.2`. Use the `v0.6.0` builds instead.

  Nothing else moves: the SQLite C API is unchanged and database files stay
  byte-compatible with stock SQLite, so only loadable extensions are affected.
  This is also the only time the layout can move. From 0.6.0 on, `iVersion` lets
  an extension detect a mismatch instead of crashing on it, and the release
  manifest records the interface version each artifact was built against, so a
  bad pairing is caught before the file is ever loaded.

### Added

- **Versioned loadable-extension ABI.** `sqlanywhere_api_routines` — the SQL
  Anywhere thunk handed to extension entry points alongside the stock
  `sqlite3_api_routines` — now carries an `iVersion` as its first member, and
  `sqlite3ext.h` gained `SQLANYWHERE_API_VERSION` plus a
  `SQLANYWHERE_API_ATLEAST(V)` guard. An extension is compiled against one copy
  of the header and then loaded by whatever host library the user happens to
  have, so without a version field an extension built against a newer header
  would read past the end of an older host's structure, with no way to detect
  it. The structure holds one member today (`close_hook`), so this is the last
  moment the field can be added at all; the cost of adding it now is the
  one-time layout change described under **Breaking** below. Documented in
  [`sqlanywhere_extensions.md`](sqlanywhere-sqlite3/doc/sqlanywhere_extensions.md)
  and verified by
  `sqlanywhere-sqlite3/test/rust_suite/src/extension_abi.rs`, which compiles a
  real out-of-tree loadable extension and checks that the host's advertised
  version agrees with its own header and that a probe for a not-yet-implemented
  version is declined rather than followed.

- **Signed extension repository.** A prebuilt extension is code you download
  and then run with the full privileges of your database process, but installing
  one meant fetching a shared object from a release page and hoping. Releases
  now carry `SHA256SUMS` for integrity, a `MANIFEST.json` describing every
  artifact (digest, size, and the extension interface version it was compiled
  against), and a detached Ed25519 signature over that manifest's exact bytes.
  Three new tasks drive it: `cargo xtask extension-keygen` (a one-time,
  deliberately manual step, since CI must not mint its own trust root),
  `sign-extensions`, and `verify-extensions`, which checks the signature, then
  each digest, then that the artifact's interface version is one the host
  implements. Keys carry short ids so rotation does not need a flag day, and an
  unsigned release cannot pass as a signed one: `verify-extensions` fails unless
  told `--allow-unsigned`. Wired into
  [`crsqlite.yml`](.github/workflows/crsqlite.yml) on every build, not just
  tags, so the tooling stays covered. New
  [`docs/EXTENSION_REPOSITORY.md`](docs/EXTENSION_REPOSITORY.md), which also
  records what is deliberately *not* done: the loader still opens any file you
  point it at, and making it enforce signatures is a policy decision with open
  questions listed there.

### Removed

- **`publish-crsqlite.yml`.** It attached its own unsigned
  `crsqlite-linux-x86_64.zip` on `v*` tags, built independently of the
  per-target workflow, so that archive sat outside `MANIFEST.json` and nobody
  could verify it. `crsqlite.yml` already builds the same Linux x86-64 target
  (plus Apple Silicon and Linux ARM), `docs/CRDT.md` already documents the
  `crsqlite-<tag>-<target>.tar.gz` naming rather than the zip, and every tag
  ever pushed matches the `v*.*.*` pattern the remaining workflow triggers on,
  so nothing is lost. The dead `prebuild-test.*` trigger goes with it.

### Fixed

- **Stale SQLite version in the README.** The sample shell transcript claimed
  the fork is based on SQLite 3.43.0; the bundled amalgamation has been 3.47.0
  for some time. Corrected while bumping the version marker on the same line.

- **Null-thunk dereference in the vendored cr-sqlite extension.** `crsqlite.c`
  called `sqlanywhere_close_hook` unconditionally, but a host built with
  `SQLITE_OMIT_LOAD_EXTENSION` passes no SQL Anywhere thunk at all. The call is
  now guarded by `SQLANYWHERE_API_ATLEAST(1)`.

## [0.5.2] - 2026-07-16

The substrate half of the Redis-free stack, made provable: the queue, cache and
pub/sub contracts are now executable, CI-verified specs the Askr runtime builds
its L2 drivers against.

### Added

- **Conformance-tested storage contracts (substrate × Askr runtime).** The
  queue, cache and pub/sub contracts in `docs/contracts/` are now executable,
  CI-verified specs: `sqlanywhere/tests/contract_conformance.rs` runs the exact
  contract SQL and asserts the documented semantics (queue at-least-once claim /
  priority / delay / dead-letter / backlog; cache TTL / atomic increment / SETNX
  locks / tag invalidation; pub/sub monotonic tail / cursor / retention). This
  is the substrate half of the Redis-free stack (epic elyra-2); the Askr runtime
  (`askr/docs/STORAGE_BACKEND.md`) now builds its L2 drivers against a proven
  contract that cannot silently drift.

## [0.5.1] - 2026-07-16

Another chapter, not another product: the everyday storage primitives — a KV
cache with TTL, a durable work queue, and pub/sub — shown to compose out of plain
SQL over the replicated SQLite engine, with no Redis/SQS/Kafka alongside.

### Added

- **Storage primitives, as chapters not products.** A KV cache with TTL, a
  durable work queue, and pub/sub all compose out of plain SQL over the
  replicated SQLite engine — no Redis/SQS/Kafka alongside. The cache is a table
  with an expiry column (lazy-filtering view + periodic sweep); the queue is a
  table with an atomic `UPDATE … RETURNING` claim and a visibility timeout
  (at-least-once); pub/sub is an append-only topic tailed by cursor, carried
  across nodes by the replication log. New `docs/STORAGE_PRIMITIVES.md`;
  demonstrated (`sqlanywhere/examples/storage_primitives.rs`) and verified
  (`sqlanywhere/tests/storage_primitives.rs`, 3 tests).

## [0.5.0] - 2026-07-16

Search, made whole and made honest. Full-text, faceted, vector and hybrid search
are unified as **one engine, one chapter — not a separate product**; real
semantic embeddings become first-class via a pluggable `Embedder` trait (with a
worked neural example); and every published Docker image is now smoke-tested on
both architectures.

### Added

- **Search, unified as one chapter (not a product).** Full-text (FTS5 inverted
  index), faceted (`GROUP BY` over the matched set), vector (DiskANN) and hybrid
  (RRF) search are the same engine composed in plain SQL — no separate search
  service. New `docs/SEARCH.md` ties them together; faceted search is
  demonstrated (`sqlanywhere/examples/faceted_search.rs`) and verified
  (`sqlanywhere/tests/faceted_search.rs`, 3 tests: full-text match, facet counts,
  drill-down).
- **Pluggable embeddings (`Embedder` trait).** Bring your own semantic model
  (local ONNX/candle, or a hosted API) and feed it into the same
  `vector32(...)` storage and `vector_top_k` search path as the built-in
  embedder. The dependency-free `embed()` is now the `LexicalEmbedder`
  implementation of this trait; a `to_vector_literal()` helper formats any raw
  vector for `vector32`. Output of `embed()` is unchanged (back-compatible).
- **Worked semantic-search example** (`examples/semantic-search`) plugging a real
  local sentence-transformer (all-MiniLM-L6-v2 via candle) into the `Embedder`
  trait — kept out of the main workspace so its ML dependencies never touch the
  core build. Demonstrates true semantic matching (finds "the cat sat on the
  mat" for "a small feline rested on a rug").
- **Docker release smoke test.** `scripts/smoke-test-docker.sh` boots a published
  image and asserts the HTTP API serves a real vector search (`vector_top_k` over
  a DiskANN index). Wired into `docker.yml` to run on both `amd64` and `arm64`
  after the manifest is published, so a broken release image can never pass
  silently.

## [0.4.0] - 2026-07-08

The flagship **collaborative, syncable vector index** — CRDT offline merge ×
DiskANN vector search × inline `embed()` — plus multi-arch **Docker images** for
Ubuntu Intel and ARM alongside the prebuilt binaries.

### Added

- **Collaborative, syncable vector index (experimental).** The flagship
  combination of CRDT offline merge × DiskANN vector search × inline `embed()`:
  several devices build a semantic index offline and independently, then merge
  conflict-free — afterwards every device can vector-search over every device's
  documents (the index is maintained as cr-sqlite applies merged rows). Verified
  by `sqlanywhere/tests/collab_vector.rs` (a doc indexed only on node B becomes
  the nearest neighbour on node A after merge) and demonstrated by
  `sqlanywhere/examples/collab_vector.rs`. Guide: `docs/COLLABORATIVE_VECTOR.md`.
- **Docker images.** Multi-arch `sqld` container images (`linux/amd64` and
  `linux/arm64`) are built and published to `ghcr.io/kwhorne/sqlanywhere-server`
  on each release, alongside the prebuilt binaries.

## [0.3.1] - 2026-07-02

Experimental **CRDT offline merge** via the vendored cr-sqlite extension —
conflict-free multi-writer offline sync, the other half of a local-first stack
alongside embedded replicas. Additive and opt-in.

### Added

- **CRDT offline merge (experimental).** The vendored cr-sqlite extension
  (`sqlanywhere-sqlite3/ext/crr`) now builds into a loadable extension via
  `scripts/build-crsqlite.sh`, turning tables into conflict-free replicated
  relations with `crsql_as_crr(...)`. Multiple databases can be edited offline
  and merged deterministically by exchanging `crsql_changes` rows. Verified end
  to end (two nodes converge; concurrent same-row edits resolve the same way on
  both sides). Guide: `docs/CRDT.md`; continuously built by the `crsqlite.yml`
  CI workflow. Not yet bundled into `sqld` or the prebuilt binaries.
- **CRDT via the Rust API.** `sqlanywhere/examples/crdt_sync.rs` demonstrates
  offline multi-writer merge driven entirely through the `sqlanywhere` client
  (`load_extension` + `crsql_as_crr` + `crsql_changes`), and
  `sqlanywhere/tests/crdt.rs` asserts it (gated on `SQLANYWHERE_CRSQLITE`, run in
  CI against a freshly built extension). Prebuilt extensions are attached to
  releases for macOS Apple Silicon and Ubuntu Intel/ARM.

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

[0.6.2]: https://github.com/kwhorne/sql-anywhere/releases/tag/v0.6.2
[0.6.1]: https://github.com/kwhorne/sql-anywhere/releases/tag/v0.6.1
[0.6.0]: https://github.com/kwhorne/sql-anywhere/releases/tag/v0.6.0
[0.5.2]: https://github.com/kwhorne/sql-anywhere/releases/tag/v0.5.2
[0.5.1]: https://github.com/kwhorne/sql-anywhere/releases/tag/v0.5.1
[0.5.0]: https://github.com/kwhorne/sql-anywhere/releases/tag/v0.5.0
[0.4.0]: https://github.com/kwhorne/sql-anywhere/releases/tag/v0.4.0
[0.3.1]: https://github.com/kwhorne/sql-anywhere/releases/tag/v0.3.1
[0.3.0]: https://github.com/kwhorne/sql-anywhere/releases/tag/v0.3.0
[0.2.0]: https://github.com/kwhorne/sql-anywhere/releases/tag/v0.2.0
[0.1.0]: https://github.com/kwhorne/sql-anywhere/releases/tag/v0.1.0
