//! CRDT offline-merge integration test through the SQL Anywhere Rust API.
//!
//! Requires the cr-sqlite loadable extension, so it is **gated**: it only runs
//! when `SQLANYWHERE_CRSQLITE` points at a built `crsqlite.{dylib,so}` (built
//! with `scripts/build-crsqlite.sh`). Without it the test prints a note and
//! passes, so the main workspace CI (which has no cr-sqlite build) is unaffected.

use sqlanywhere::{params, params::params_from_iter, Builder, Connection, Value};

async fn call(conn: &Connection, sql: &str) {
    let mut rows = conn.query(sql, ()).await.unwrap();
    while rows.next().await.unwrap().is_some() {}
}

async fn open_node(ext: &str) -> Connection {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.load_extension_enable().unwrap();
    conn.load_extension(ext, Some("sqlite3_crsqlite_init"))
        .unwrap_or_else(|e| panic!("failed to load cr-sqlite from '{ext}': {e}"));
    conn.load_extension_disable().unwrap();
    conn.execute(
        "CREATE TABLE todo (id INTEGER PRIMARY KEY NOT NULL, name TEXT NOT NULL DEFAULT '')",
        (),
    )
    .await
    .unwrap();
    call(&conn, "SELECT crsql_as_crr('todo')").await;
    conn
}

async fn merge(from: &Connection, to: &Connection) {
    let mut rows = from.query("SELECT * FROM crsql_changes", ()).await.unwrap();
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
}

async fn name_of(conn: &Connection, id: i64) -> Option<String> {
    let mut rows = conn
        .query("SELECT name FROM todo WHERE id = ?", params![id])
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .map(|r| r.get::<String>(0).unwrap())
}

async fn count(conn: &Connection) -> i64 {
    let mut rows = conn.query("SELECT count(*) FROM todo", ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

#[tokio::test]
async fn offline_multi_writer_merge_converges() {
    let ext = match std::env::var("SQLANYWHERE_CRSQLITE") {
        Ok(p) => p,
        Err(_) => {
            eprintln!(
                "skipping: set SQLANYWHERE_CRSQLITE to a built crsqlite.{{dylib,so}} \
                 (see scripts/build-crsqlite.sh) to run this test"
            );
            return;
        }
    };

    let a = open_node(&ext).await;
    let b = open_node(&ext).await;

    // Offline edits on each node.
    a.execute("INSERT INTO todo(id, name) VALUES (1, 'buy milk')", ())
        .await
        .unwrap();
    a.execute("INSERT INTO todo(id, name) VALUES (2, 'walk dog')", ())
        .await
        .unwrap();
    b.execute("INSERT INTO todo(id, name) VALUES (3, 'write code')", ())
        .await
        .unwrap();

    // Bi-directional sync -> both converge to all three rows.
    merge(&a, &b).await;
    merge(&b, &a).await;
    assert_eq!(count(&a).await, 3);
    assert_eq!(count(&b).await, 3);
    assert_eq!(name_of(&a, 3).await.as_deref(), Some("write code"));
    assert_eq!(name_of(&b, 1).await.as_deref(), Some("buy milk"));

    // Concurrent conflicting edit to the same row -> deterministic convergence.
    a.execute("UPDATE todo SET name='BUY MILK NOW' WHERE id=1", ())
        .await
        .unwrap();
    b.execute("UPDATE todo SET name='buy oat milk' WHERE id=1", ())
        .await
        .unwrap();
    merge(&a, &b).await;
    merge(&b, &a).await;

    let resolved_a = name_of(&a, 1).await;
    let resolved_b = name_of(&b, 1).await;
    assert_eq!(
        resolved_a, resolved_b,
        "both nodes must resolve the conflict to the same value"
    );
    assert!(resolved_a.is_some());

    call(&a, "SELECT crsql_finalize()").await;
    call(&b, "SELECT crsql_finalize()").await;
}
