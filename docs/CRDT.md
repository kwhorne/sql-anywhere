# CRDT offline merge (cr-sqlite)

> **Status: experimental.** The extension builds and works (see the verified
> demo below), but it is not yet bundled into `sqld` or the prebuilt binaries.
> You build the loadable extension yourself and `load_extension` it.

SQL Anywhere can turn ordinary tables into **conflict-free replicated relations
(CRRs)** using the vendored [cr-sqlite](https://github.com/vlcn-io/cr-sqlite)
extension. This lets many databases be edited **offline** and later merged
**deterministically**, with no central coordinator — the missing half of a true
local-first stack alongside embedded replicas.

## Why

Embedded replicas give you a local read copy of a primary, but writes go through
the primary. Truly offline, multi-writer apps (field data collection, mobile
apps that work on a plane, collaborative local-first tools) need something
stronger: each device writes locally, and when connectivity returns the changes
merge without conflicts or a "last sync wins" data loss.

CRDTs provide exactly that. cr-sqlite implements them at the SQL layer: you keep
writing normal `INSERT`/`UPDATE`/`DELETE`, and the extension tracks per-column
versions so any two databases can exchange changesets and converge to the same
state — including deterministic resolution when two nodes edit the same row.

## How

1. **Get the extension.** Download a prebuilt archive from the
   [releases page](https://github.com/kwhorne/sql-anywhere/releases)
   (`crsqlite-<tag>-<target>.tar.gz`, for macOS Apple Silicon and Ubuntu
   Intel/ARM), or build it yourself (needs a pinned nightly + `build-std`,
   wrapped in a script):

   ```sh
   scripts/build-crsqlite.sh
   # -> sqlanywhere-sqlite3/ext/crr/dist/crsqlite.{dylib,so}
   ```

2. **Load it** into a connection and mark tables as CRRs. Every `NOT NULL`
   column must have a `DEFAULT` (a cr-sqlite requirement for schema
   compatibility):

   ```sql
   .load ./crsqlite            -- or sqlite3_load_extension(...)

   CREATE TABLE todo (
     id   INTEGER PRIMARY KEY NOT NULL,
     name TEXT    NOT NULL DEFAULT '',
     done INTEGER DEFAULT 0
   );
   SELECT crsql_as_crr('todo');   -- upgrade to a conflict-free replicated relation
   ```

3. **Edit offline** on each node as usual:

   ```sql
   INSERT INTO todo(id, name) VALUES (1, 'buy milk');
   ```

4. **Sync** by exchanging rows of the `crsql_changes` virtual table. Read changes
   from a source database and insert them into a target:

   ```sql
   -- on the receiving node, for each changeset row from the peer:
   INSERT INTO crsql_changes VALUES (/* table, pk, cid, val, col_version,
                                        db_version, site_id, cl, seq */);
   ```

   In practice you filter `SELECT * FROM crsql_changes WHERE db_version > ?` with
   the last version you received from that peer, ship those rows over your
   transport, and insert them on the other side.

5. **Finalize** when done with a connection:

   ```sql
   SELECT crsql_finalize();
   ```

## Verified example

The following two-node scenario is exercised end to end against the SQL Anywhere
amalgamation. Two databases edit **offline**, sync bi-directionally, then make a
**concurrent conflicting** edit to the same row:

```text
== cr-sqlite CRDT offline multi-writer merge ==
Before sync (offline edits):
  A: (1,buy milk) (2,walk dog)
  B: (3,write RUST code)
Synced: 4 changes A->B, 6 changes B->A
After sync (converged):
  A: (1,buy milk) (2,walk dog) (3,write RUST code)
  B: (1,buy milk) (2,walk dog) (3,write RUST code)
After concurrent conflict on id=1 (last-writer-wins, deterministic):
  A: (1,buy oat milk) (2,walk dog) (3,write RUST code)
  B: (1,buy oat milk) (2,walk dog) (3,write RUST code)
```

Both nodes converge to identical state, and the concurrent edit to `id=1`
resolves the same way on both sides — no coordinator, no lost writes.

## Loading from Rust

```rust
let db = sqlanywhere::Builder::new_local("app.db").build().await?;
let conn = db.connect()?;
conn.load_extension_enable()?;
conn.load_extension("path/to/crsqlite", Some("sqlite3_crsqlite_init"))?;
conn.load_extension_disable()?;
// ... crsql_as_crr('table'), edit, exchange crsql_changes, crsql_finalize()
```

## Key functions

| Function / table | Purpose |
|------------------|---------|
| `crsql_as_crr('table')` | Upgrade a table to a conflict-free replicated relation |
| `crsql_changes` (vtab) | Read the changeset (per-column versioned changes); insert rows to merge |
| `crsql_db_version()` | Current database version (high-water mark for sync) |
| `crsql_site_id()` | This database's unique site id |
| `crsql_finalize()` | Release cr-sqlite resources before closing the connection |

## Limitations & roadmap

- **Experimental.** Prebuilt extensions are attached to releases per platform,
  but the extension is not yet bundled *inside* `sqld`. Next step: optional
  static linking into `sqld` so no separate load is needed.
- Schema rules: `NOT NULL` columns need a `DEFAULT`; primary keys are required.
- See the upstream [cr-sqlite docs](https://vlcn.io) for the full data model,
  including causal-length deletes and fractional indexing.
