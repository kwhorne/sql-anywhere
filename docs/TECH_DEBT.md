# Technical Debt Inventory

> Auto-generated from source markers (TODO, FIXME, HACK, XXX).
> Last updated: 2026-06-23 · **97 markers** (35 FIXME, 59 TODO, 1 HACK, 2 XXX)

## Summary by area

| Area | FIXME | TODO | HACK | XXX | Total |
|------|-------|------|------|-----|-------|
| **sqlanywhere** (client library) | 8 | 18 | 0 | 0 | 26 |
| **sqlanywhere-server** | 17 | 34 | 1 | 1 | 53 |
| **sqlanywhere-hrana** | 0 | 1 | 0 | 0 | 1 |
| **sqlanywhere-replication** | 2 | 1 | 0 | 0 | 3 |
| **sqlanywhere-sys** | 2 | 2 | 0 | 0 | 4 |
| **bottomless** | 4 | 3 | 0 | 0 | 7 |

---

## 🔴 FIXME — Known bugs or broken behavior (35)

### sqlanywhere (client library)

| File | Line | Description |
|------|------|-------------|
| `hrana/hyper.rs` | 266 | Named arg parsing rules are too simplistic; may require full AST parsing |
| `hrana/hyper.rs` | 280 | Several blockers for correct named-arg handling |
| `hrana/cursor.rs` | 100 | Compatibility hack for current client API; needs cleanup |
| `replication/connection.rs` | 648 | Unclear if multi-statement metadata fetch is ever wanted |
| `replication/connection.rs` | 772 | Need to decide if RemoteStatement is single or multi-statement |
| `replication/connection.rs` | 783 | Same as above (duplicated concern) |
| `replication/connection.rs` | 804 | Same as above (duplicated concern) |
| `parser.rs` | 212 | Temporary workaround for Atlas integration (XXX) |

### sqlanywhere-server

| File | Line | Description |
|------|------|-------------|
| `namespace/store.rs` | 238 | Could delete namespace while trying to fork it |
| `namespace/store.rs` | 435 | Default namespace check is in the wrong place |
| `namespace/configurator/helpers.rs` | 60 | Per-db configuration not figured out |
| `namespace/configurator/helpers.rs` | 76 | Bug in `logger::checkpoint_db` — using regular checkpointing as workaround |
| `namespace/configurator/helpers.rs` | 228 | Bottomless requires proper config; not enforced |
| `namespace/configurator/primary.rs` | 133 | Namespace creation is not truly atomic; consider temp dirs |
| `namespace/meta_store.rs` | 558 | Metastore restore correctness not guaranteed |
| `schema/db.rs` | 577 | Constraint error reported instead of proper error message |
| `schema/scheduler.rs` | 838 | Too much coupling in test setup |
| `http/user/types.rs` | 97 | Large blob payload could block the main thread |
| `replication/snapshot.rs` | 190 | Snapshot code not robust enough; unclear error handling |
| `replication/snapshot_store.rs` | 155 | Blocking I/O in async context (should be async) |
| `replication/primary/logger.rs` | 109 | Should take a file lock to prevent concurrent writes |
| `replication/primary/logger.rs` | 397 | Dest path never changes; should be stored/cached |
| `replication/primary/logger.rs` | 824 | Calling rusqlite checkpoint is a bug; needs custom impl |
| `rpc/proxy.rs` | 558 | Missing tracking of recently disconnected clients |
| `rpc/proxy.rs` | 688 | Copy-pasted code from `execute()` — extract to helper |
| `rpc/proxy.rs` | 731 | Double `map_err` looks incorrect |
| `query_result_builder.rs` | 91 | Code in wrong module |

### sqlanywhere-replication

| File | Line | Description |
|------|------|-------------|
| `meta.rs` | 145 | Extra syscall — could use `read_exact_at` instead of tokio API |
| `injector/sqlanywhere_injector.rs` | 28 | Injector needs optimization |

### sqlanywhere-sys

| File | Line | Description |
|------|------|-------------|
| `wal/wrapper.rs` | 762 | Bypassing WAL wrappers directly |
| `wal/wrapper.rs` | 774 | Bypassing WAL wrappers directly (same pattern) |

### bottomless

| File | Line | Description |
|------|------|-------------|
| `wal.rs` | 248 | Computations use host endianness — broken on big-endian |
| `replicator.rs` | 617 | Function is likely buggy; output not checked |
| `replicator.rs` | 1100 | Can't rely on change counter in WAL mode |
| `replicator.rs` | 1171 | Assumes bucket stores only generation data |

---

## 🟡 TODO — Missing features or improvements (59)

### sqlanywhere (client library)

| File | Line | Description |
|------|------|-------------|
| `database.rs` | 42, 54, 62 | Remove unused fields once sync code is updated (3×) |
| `sync.rs` | 866 | Upcasting should only happen at API boundary |
| `sync.rs` | 941 | Underflow risk if server returns lower `max_frame_no` |
| `sync.rs` | 1036 | Make crash-proof |
| `params.rs` | 186 | Unnecessary allocation in param conversion |
| `params.rs` | 259 | Consider renaming trait to `ToSql` |
| `local/mod.rs` | 1 | Decide what to keep from local module |
| `local/impls.rs` | 47 | Can we reuse the conn passed to the transaction? |
| `local/rows.rs` | 20–21 | `unsafe impl Send/Sync for Rows` — safety not verified (2×) |
| `replication/client.rs` | 110 | Map errors correctly |
| `replication/mod.rs` | 121 | Pass params to replication |
| `replication/remote_client.rs` | 178 | Check if `4096 * frames.len()` is correct |
| `replication/connection.rs` | 1 | Move to `remote/mod.rs` |
| `replication/connection.rs` | 316 | Arc the params to cheaply clone |
| `replication/connection.rs` | 827 | Switch to VecDeque to reduce allocations |
| `parser.rs` | 180 | Check if optimize can be safely performed |

### sqlanywhere-server

| File | Line | Description |
|------|------|-------------|
| `database/schema.rs` | 70 | Pass proper config |
| `hrana/ws/session.rs` | 132 | Function too long with duplicated code; needs refactor |
| `namespace/store.rs` | 68 | Not clear if snapshot-on-evict is correct |
| `namespace/configurator/replica.rs` | 127 | Bug where primary reports valid frame incorrectly |
| `namespace/configurator/helpers.rs` | 85 | Figure out why the checkpoint workaround is needed |
| `namespace/configurator/helpers.rs` | 424 | Once separate WAL checkpoint fiber exists, revisit this |
| `namespace/meta_store.rs` | 71 | Use a concurrent hashmap to avoid blocking connection creation |
| `namespace/meta_store.rs` | 141 | Logic should probably move to bottomless |
| `namespace/meta_store.rs` | 495 | If no entry exists, ensure update is sent |
| `schema/db.rs` | 129 | Handle corrupted meta |
| `schema/scheduler.rs` | 107 | Optimization: try enqueue more work at once |
| `schema/scheduler.rs` | 389 | Refactor — function is a mess (borrow checker makes it hard) |
| `schema/scheduler.rs` | 594 | Add backoff |
| `schema/scheduler.rs` | 595 | Fine if backup fails here, but should be explicit |
| `schema/scheduler.rs` | 667, 688 | Ensure state transition is valid (2×) |
| `schema/scheduler.rs` | 755 | Check that all tasks reported success before migration |
| `http/admin/mod.rs` | 413 | Move check into meta store |
| `http/user/result_builder.rs` | 296 | How to return `last_frame_no`? |
| `http/user/trace.rs` | 68 | Tracing is not correct; needs fix |
| `http/user/mod.rs` | 435 | Remove workaround when tower-http upgraded to 0.5.3 |
| `replication/snapshot.rs` | 324 | Handle compacted snapshot recovery after kill |
| `replication/snapshot_store.rs` | 45 | Use a pool for concurrent reads and writes |
| `replication/snapshot_store.rs` | 242 | Handle error properly |
| `pager.rs` | 19 | Multiple free lists if contention is measured |
| `pager.rs` | 65 | Key > 0 check may be unnecessary |
| `rpc/proxy.rs` | 661, 730 | Not necessarily a permission denied error (2×) |
| `rpc/proxy.rs` | 667 | Handle cleanup on peer disconnect |
| `rpc/replica_proxy.rs` | 109 | Handle cleanup on peer disconnect |
| `query_result_builder.rs` | 123 | Un-default this so it must be explicitly implemented |

### sqlanywhere-hrana

| File | Line | Description |
|------|------|-------------|
| `protobuf.rs` | 383 | Unnecessary copy in protobuf conversion |

### sqlanywhere-replication

| File | Line | Description |
|------|------|-------------|
| `lib.rs` | 29 | Make cipher configurable |

### sqlanywhere-sys

| File | Line | Description |
|------|------|-------------|
| `wal/ffi.rs` | 45 | `xFile: None` — not all WALs are single-file based |
| `wal/mod.rs` | 135 | Move `SQLANYWHERE_PAGE_SIZE` to a better location |

### bottomless

| File | Line | Description |
|------|------|-------------|
| `wal.rs` | 197 | Specialize non-compressed file cloning |
| `uuid_utils.rs` | 64 | Possible information loss on encoding |
| `replicator.rs` | 938 | Connection leak — dropping it hangs for some reason |

---

## 🟠 HACK / XXX — Workarounds (3)

| File | Line | Type | Description |
|------|------|------|-------------|
| `sqlanywhere-server/src/replication/replicator_client.rs` | 174 | HACK | Load shared schema DB before main schema is replicated |
| `sqlanywhere-server/src/query_analysis.rs` | 298 | XXX | Temporary workaround for Atlas integration |
| `sqlanywhere/src/parser.rs` | 212 | XXX | Temporary workaround for Atlas integration |

---

## Recommended priorities

### P0 — Safety / correctness
1. `local/rows.rs:20-21` — `unsafe impl Send/Sync` without safety proof
2. `bottomless/replicator.rs:617` — Buggy function (output not checked)
3. `bottomless/wal.rs:248` — Host-endianness assumption breaks big-endian
4. `namespace/store.rs:238` — Race: delete namespace during fork
5. `replication/primary/logger.rs:824` — Wrong checkpoint implementation

### P1 — Data integrity
6. `replication/primary/logger.rs:109` — Missing file lock
7. `namespace/configurator/primary.rs:133` — Non-atomic namespace creation
8. `namespace/meta_store.rs:558` — Metastore restore correctness
9. `replication/snapshot.rs:190` — Snapshot robustness

### P2 — Performance
10. `namespace/meta_store.rs:71` — Blocking hashmap in connection creation
11. `http/user/types.rs:97` — Large blob blocking main thread
12. `replication/snapshot_store.rs:155` — Sync I/O in async context

### P3 — Code quality / refactoring
13. Everything else (duplicated code, naming, module structure)
