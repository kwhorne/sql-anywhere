//! Collaborative syncable vector index: CRDT offline merge × vector search.
//!
//! Proves that a document indexed on one node becomes vector-searchable on
//! another node after a conflict-free CRDT merge — i.e. the DiskANN index is
//! maintained as cr-sqlite applies merged rows to the base table.
//!
//! Gated on `SQLANYWHERE_CRSQLITE` (path to a built `crsqlite.{dylib,so}`); the
//! test skips + passes when it is unset, so the main workspace CI is unaffected.

use sqlanywhere::{embed, params, params::params_from_iter, Builder, Connection, Value};

const DIMS: usize = 64;

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
        &format!(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY NOT NULL, \
             body TEXT NOT NULL DEFAULT '', emb FLOAT32({DIMS}))"
        ),
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "CREATE INDEX docs_vec ON docs(sqlanywhere_vector_idx(emb, 'metric=cosine'))",
        (),
    )
    .await
    .unwrap();
    call(&conn, "SELECT crsql_as_crr('docs')").await;
    conn
}

async fn add(conn: &Connection, id: i64, body: &str) {
    conn.execute(
        "INSERT INTO docs (id, body, emb) VALUES (?, ?, vector32(?))",
        params![id, body, embed(body, DIMS)],
    )
    .await
    .unwrap();
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

async fn nearest(conn: &Connection, query: &str) -> Vec<String> {
    let mut rows = conn
        .query(
            "SELECT d.body FROM vector_top_k('docs_vec', vector32(?), 3) k \
             JOIN docs d ON d.id = k.id",
            params![embed(query, DIMS)],
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get::<String>(0).unwrap());
    }
    out
}

async fn count(conn: &Connection) -> i64 {
    let mut rows = conn.query("SELECT count(*) FROM docs", ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

#[tokio::test]
async fn merged_document_is_vector_searchable() {
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

    // Well-separated vocabularies so exact-text queries have an exact nearest.
    const A_DOC1: &str = "alpha alpha alpha alpha";
    const A_DOC2: &str = "beta beta beta beta";
    const B_DOC: &str = "gamma gamma gamma gamma";

    let a = open_node(&ext).await;
    let b = open_node(&ext).await;

    add(&a, 1, A_DOC1).await;
    add(&a, 2, A_DOC2).await;
    add(&b, 3, B_DOC).await;

    // Before sync, node A has never seen node B's document.
    assert_eq!(count(&a).await, 2);
    assert!(
        !nearest(&a, B_DOC).await.iter().any(|d| d == B_DOC),
        "node A must not have B's doc before sync"
    );

    // Conflict-free bi-directional merge.
    merge(&a, &b).await;
    merge(&b, &a).await;

    // After sync, B's document is present AND vector-searchable on node A.
    assert_eq!(count(&a).await, 3, "node A should have all three docs");
    let hits = nearest(&a, B_DOC).await;
    assert_eq!(
        hits.first().map(String::as_str),
        Some(B_DOC),
        "B's doc should be the nearest neighbour on A after merge; got {hits:?}"
    );

    // And symmetrically, A's docs are searchable on node B.
    assert_eq!(count(&b).await, 3);
    let hits_b = nearest(&b, A_DOC1).await;
    assert_eq!(hits_b.first().map(String::as_str), Some(A_DOC1));

    call(&a, "SELECT crsql_finalize()").await;
    call(&b, "SELECT crsql_finalize()").await;
}
