//! CRDT offline multi-writer merge through the SQL Anywhere Rust API.
//!
//! Two independent local databases are edited **offline**, then synced by
//! exchanging `crsql_changes` rows — converging deterministically with no
//! central coordinator. This is the same conflict-free replication demonstrated
//! in `docs/CRDT.md`, driven entirely through `sqlanywhere`.
//!
//! Requires the cr-sqlite loadable extension. Build it with
//! `scripts/build-crsqlite.sh` (or download it from a release), then run:
//!
//! ```sh
//! # defaults to sqlanywhere-sqlite3/ext/crr/dist/crsqlite.{dylib,so}
//! cargo run -p sqlanywhere --example crdt_sync
//! # or point at a specific build:
//! SQLANYWHERE_CRSQLITE=/path/to/crsqlite.dylib cargo run -p sqlanywhere --example crdt_sync
//! ```

use sqlanywhere::{params, params::params_from_iter, Builder, Connection, Value};

/// Run a statement that returns rows (e.g. `SELECT crsql_as_crr(...)`) and
/// discard the result. `execute()` rejects row-returning statements.
async fn call(conn: &Connection, sql: &str) {
    let mut rows = conn.query(sql, ()).await.unwrap();
    while rows.next().await.unwrap().is_some() {}
}

/// Locate the cr-sqlite extension: env override, else the default build path
/// (platform-appropriate extension).
fn extension_path() -> String {
    if let Ok(p) = std::env::var("SQLANYWHERE_CRSQLITE") {
        return p;
    }
    let ext = if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    format!("sqlanywhere-sqlite3/ext/crr/dist/crsqlite.{ext}")
}

async fn open_node(path: &str, ext: &str) -> Connection {
    let db = Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.load_extension_enable().unwrap();
    conn.load_extension(ext, Some("sqlite3_crsqlite_init"))
        .unwrap_or_else(|e| panic!("failed to load cr-sqlite from '{ext}': {e}"));
    conn.load_extension_disable().unwrap();

    // A conflict-free replicated relation. NOT NULL columns need a DEFAULT.
    conn.execute(
        "CREATE TABLE todo (id INTEGER PRIMARY KEY NOT NULL, name TEXT NOT NULL DEFAULT '')",
        (),
    )
    .await
    .unwrap();
    call(&conn, "SELECT crsql_as_crr('todo')").await;
    conn
}

/// Copy every changeset row from `from` into `to` (a merge). Returns the number
/// of changes applied.
async fn merge(from: &Connection, to: &Connection) -> usize {
    // Read the whole changeset. (In a real app: filter by db_version per peer.)
    let mut rows = from.query("SELECT * FROM crsql_changes", ()).await.unwrap();

    // Collect rows first so we don't hold a read statement open across writes.
    let mut changes: Vec<Vec<Value>> = Vec::new();
    let mut ncol = 0;
    while let Some(row) = rows.next().await.unwrap() {
        ncol = row.column_count() as usize;
        changes.push(
            (0..ncol as i32)
                .map(|i| row.get_value(i).unwrap())
                .collect(),
        );
    }

    let placeholders = std::iter::repeat("?")
        .take(ncol)
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("INSERT INTO crsql_changes VALUES ({placeholders})");
    for change in &changes {
        to.execute(&sql, params_from_iter(change.iter().cloned()))
            .await
            .unwrap();
    }
    changes.len()
}

async fn dump(conn: &Connection, label: &str) {
    let mut rows = conn
        .query("SELECT id, name FROM todo ORDER BY id", ())
        .await
        .unwrap();
    print!("  {label}: ");
    while let Some(row) = rows.next().await.unwrap() {
        print!(
            "({}, {}) ",
            row.get::<i64>(0).unwrap(),
            row.get::<String>(1).unwrap()
        );
    }
    println!();
}

#[tokio::main]
async fn main() {
    let ext = extension_path();
    let tmp = std::env::temp_dir();
    let a_path = tmp.join("crdt_a.db");
    let b_path = tmp.join("crdt_b.db");
    let _ = std::fs::remove_file(&a_path);
    let _ = std::fs::remove_file(&b_path);

    println!("== CRDT offline multi-writer merge (via the sqlanywhere API) ==");
    let a = open_node(a_path.to_str().unwrap(), &ext).await;
    let b = open_node(b_path.to_str().unwrap(), &ext).await;

    // Each node edits while offline.
    for (id, name) in [(1, "buy milk"), (2, "walk dog")] {
        a.execute(
            "INSERT INTO todo(id, name) VALUES (?, ?)",
            params![id, name],
        )
        .await
        .unwrap();
    }
    b.execute("INSERT INTO todo(id, name) VALUES (3, 'write code')", ())
        .await
        .unwrap();

    println!("Before sync (offline edits):");
    dump(&a, "A").await;
    dump(&b, "B").await;

    // Bi-directional sync.
    let a2b = merge(&a, &b).await;
    let b2a = merge(&b, &a).await;
    println!("Synced: {a2b} changes A->B, {b2a} changes B->A");
    println!("After sync (converged):");
    dump(&a, "A").await;
    dump(&b, "B").await;

    // Concurrent conflicting edit to the SAME row while offline again.
    a.execute("UPDATE todo SET name='BUY MILK NOW' WHERE id=1", ())
        .await
        .unwrap();
    b.execute("UPDATE todo SET name='buy oat milk' WHERE id=1", ())
        .await
        .unwrap();
    merge(&a, &b).await;
    merge(&b, &a).await;
    println!("After concurrent conflict on id=1 (deterministic resolution):");
    dump(&a, "A").await;
    dump(&b, "B").await;

    call(&a, "SELECT crsql_finalize()").await;
    call(&b, "SELECT crsql_finalize()").await;
}
