//! Vector data tests for the SQL Anywhere embedded database.
//!
//! Exercises vector storage and round-trip, distance functions, the DiskANN
//! vector index with approximate-nearest-neighbour search, and index
//! consistency across updates and deletes.

use sqlanywhere::{Builder, Connection};

async fn conn() -> Connection {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    db.connect().unwrap()
}

/// First column of the first row as text.
async fn text(conn: &Connection, sql: &str) -> String {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let row = rows
        .next()
        .await
        .unwrap()
        .expect("expected at least one row");
    row.get::<String>(0).unwrap()
}

/// First column of the first row as a float.
async fn real(conn: &Connection, sql: &str) -> f64 {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let row = rows
        .next()
        .await
        .unwrap()
        .expect("expected at least one row");
    row.get::<f64>(0).unwrap()
}

/// First column of the first row as an integer.
async fn int(conn: &Connection, sql: &str) -> i64 {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let row = rows
        .next()
        .await
        .unwrap()
        .expect("expected at least one row");
    row.get::<i64>(0).unwrap()
}

/// First (integer) column of every row.
async fn ids(conn: &Connection, sql: &str) -> Vec<i64> {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get::<i64>(0).unwrap());
    }
    out
}

/// First (text) column of every row.
async fn names(conn: &Connection, sql: &str) -> Vec<String> {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get::<String>(0).unwrap());
    }
    out
}

#[tokio::test]
async fn vector_storage_round_trip() {
    let conn = conn().await;

    assert_eq!(
        text(&conn, "SELECT vector_extract(vector32('[1,2,3,4]'))").await,
        "[1,2,3,4]"
    );
    assert_eq!(
        text(&conn, "SELECT vector_extract(vector64('[1.5,2.5]'))").await,
        "[1.5,2.5]"
    );
    assert_eq!(
        text(&conn, "SELECT vector_extract(vector('[7,8,9]'))").await,
        "[7,8,9]"
    );

    // Stored in a typed column and read back.
    conn.execute("CREATE TABLE t (v FLOAT32(3))", ())
        .await
        .unwrap();
    conn.execute("INSERT INTO t VALUES (vector32('[10,20,30]'))", ())
        .await
        .unwrap();
    assert_eq!(
        text(&conn, "SELECT vector_extract(v) FROM t").await,
        "[10,20,30]"
    );
}

#[tokio::test]
async fn vector_distance_functions() {
    let conn = conn().await;

    // L2 distance between [0,0] and [3,4] is 5.
    assert!(
        (real(
            &conn,
            "SELECT vector_distance_l2(vector32('[0,0]'), vector32('[3,4]'))"
        )
        .await
            - 5.0)
            .abs()
            < 1e-6
    );
    // Cosine distance: identical vectors -> 0, opposite -> 2.
    assert!(
        real(
            &conn,
            "SELECT vector_distance_cos(vector32('[1,0]'), vector32('[1,0]'))"
        )
        .await
        .abs()
            < 1e-6
    );
    assert!(
        (real(
            &conn,
            "SELECT vector_distance_cos(vector32('[1,0]'), vector32('[-1,0]'))"
        )
        .await
            - 2.0)
            .abs()
            < 1e-6
    );
}

#[tokio::test]
async fn diskann_index_nearest_neighbour_search() {
    let conn = conn().await;

    conn.execute(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, emb FLOAT32(4))",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "CREATE INDEX items_idx ON items(sqlanywhere_vector_idx(emb))",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO items VALUES \
         (1,'a',vector32('[1,2,3,4]')), \
         (2,'b',vector32('[-100,-100,-100,-100]')), \
         (3,'c',vector32('[10,10,-10,-10]')), \
         (4,'d',vector32('[-1,2,3,4]'))",
        (),
    )
    .await
    .unwrap();

    // The DiskANN shadow table backing the index should exist.
    assert_eq!(
        int(
            &conn,
            "SELECT count(*) FROM sqlite_master WHERE name LIKE '%vector_meta%'"
        )
        .await,
        1
    );

    // Approximate nearest neighbours of [1,1,1,1].
    let near = names(
        &conn,
        "SELECT items.name FROM vector_top_k('items_idx', vector32('[1,1,1,1]'), 3) k \
         JOIN items ON items.id = k.id",
    )
    .await;
    assert_eq!(near, vec!["a", "d", "c"]);
}

#[tokio::test]
async fn vector_index_consistent_after_update_and_delete() {
    let conn = conn().await;

    conn.execute(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, emb FLOAT32(4))",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "CREATE INDEX items_idx ON items(sqlanywhere_vector_idx(emb))",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO items VALUES \
         (1,vector32('[1,2,3,4]')), \
         (2,vector32('[-100,-100,-100,-100]')), \
         (3,vector32('[10,10,-10,-10]')), \
         (4,vector32('[-1,2,3,4]'))",
        (),
    )
    .await
    .unwrap();

    // Delete a row and confirm it is gone.
    conn.execute("DELETE FROM items WHERE id = 2", ())
        .await
        .unwrap();
    assert_eq!(int(&conn, "SELECT count(*) FROM items").await, 3);

    // Move row 4 onto the query point and confirm it becomes the nearest.
    conn.execute(
        "UPDATE items SET emb = vector32('[1,1,1,1]') WHERE id = 4",
        (),
    )
    .await
    .unwrap();
    let nearest = ids(
        &conn,
        "SELECT items.id FROM vector_top_k('items_idx', vector32('[1,1,1,1]'), 1) k \
         JOIN items ON items.id = k.id",
    )
    .await;
    assert_eq!(nearest, vec![4]);
}

#[tokio::test]
async fn quantized_index_variants_build_and_search() {
    // `compress_neighbors` quantizes the neighbour vectors stored in the DiskANN
    // graph, shrinking the index (float1bit is ~5x smaller than float32) while
    // keeping search working. Verify every variant builds and returns results.
    for compress in ["float32", "float16", "float8", "float1bit"] {
        let conn = conn().await;
        conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v FLOAT32(4))", ())
            .await
            .unwrap();
        conn.execute(
            &format!(
                "CREATE INDEX t_idx ON t(sqlanywhere_vector_idx(v, 'metric=cosine', 'compress_neighbors={compress}'))"
            ),
            (),
        )
        .await
        .unwrap_or_else(|e| panic!("index build failed for {compress}: {e}"));
        conn.execute(
            "INSERT INTO t VALUES \
             (1,vector32('[1,0,0,0]')), \
             (2,vector32('[0,1,0,0]')), \
             (3,vector32('[0,0,1,0]')), \
             (4,vector32('[0,0,0,1]'))",
            (),
        )
        .await
        .unwrap();

        let hits = ids(
            &conn,
            "SELECT k.id FROM vector_top_k('t_idx', vector32('[1,0,0,0]'), 3) k",
        )
        .await;
        assert_eq!(hits.len(), 3, "{compress}: expected 3 hits");
    }
}

#[tokio::test]
async fn quantized_float8_index_preserves_nearest_neighbour() {
    // float8 keeps enough precision to preserve the nearest-neighbour ranking
    // for well-separated vectors.
    let conn = conn().await;
    conn.execute(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, emb FLOAT32(4))",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "CREATE INDEX items_idx ON items(sqlanywhere_vector_idx(emb, 'metric=cosine', 'compress_neighbors=float8'))",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO items VALUES \
         (1,vector32('[1,2,3,4]')), \
         (2,vector32('[-100,-100,-100,-100]')), \
         (3,vector32('[10,10,-10,-10]')), \
         (4,vector32('[1,2,3,5]'))",
        (),
    )
    .await
    .unwrap();

    // Query [1,2,3,4.1] sits between id 1 ([1,2,3,4]) and id 4 ([1,2,3,5]) but
    // closest to id 1; float8 compression must preserve that ranking.
    let nearest = ids(
        &conn,
        "SELECT k.id FROM vector_top_k('items_idx', vector32('[1,2,3,4.1]'), 2) k",
    )
    .await;
    assert_eq!(
        nearest.first(),
        Some(&1),
        "nearest to [1,2,3,4.1] should be id 1"
    );
    assert_eq!(nearest.len(), 2);
}

#[tokio::test]
async fn vector_index_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vectors.db");
    let path = path.to_str().unwrap();

    // Phase 1: create, index and populate on disk.
    {
        let db = Builder::new_local(path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "CREATE TABLE emb (id INTEGER PRIMARY KEY, v FLOAT32(4))",
            (),
        )
        .await
        .unwrap();
        conn.execute("CREATE INDEX emb_idx ON emb(sqlanywhere_vector_idx(v))", ())
            .await
            .unwrap();
        conn.execute(
            "INSERT INTO emb VALUES \
             (1,vector32('[1,0,0,0]')), \
             (2,vector32('[0,1,0,0]')), \
             (3,vector32('[0.9,0.1,0,0]'))",
            (),
        )
        .await
        .unwrap();
    }

    // Phase 2: reopen from disk and query the persisted vector index.
    {
        let db = Builder::new_local(path).build().await.unwrap();
        let conn = db.connect().unwrap();
        let near = ids(
            &conn,
            "SELECT emb.id FROM vector_top_k('emb_idx', vector32('[1,0,0,0]'), 2) k \
             JOIN emb ON emb.id = k.id",
        )
        .await;
        assert_eq!(near, vec![1, 3]);
    }
}
