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

/// `n` points in `dims` dimensions from a fixed linear congruential sequence,
/// so every run builds the same index.
async fn points(conn: &Connection, table: &str, index_params: &str, n: usize, dims: usize) {
    conn.execute(
        &format!("CREATE TABLE {table} (id INTEGER PRIMARY KEY, v FLOAT32({dims}))"),
        (),
    )
    .await
    .unwrap();
    conn.execute(
        &format!("CREATE INDEX {table}_idx ON {table}(sqlanywhere_vector_idx(v{index_params}))"),
        (),
    )
    .await
    .unwrap();
    let mut seed: u64 = 42;
    for id in 1..=n {
        let v: Vec<String> = (0..dims)
            .map(|_| {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                format!("{:.4}", (seed >> 33) as f64 / (1u64 << 31) as f64 - 0.5)
            })
            .collect();
        conn.execute(
            &format!(
                "INSERT INTO {table} VALUES ({id}, vector32('[{}]'))",
                v.join(",")
            ),
            (),
        )
        .await
        .unwrap();
    }
}

async fn stat(conn: &Connection, index: &str, field: &str) -> String {
    text(
        conn,
        &format!("SELECT json_extract(vector_index_stats('{index}'), '$.{field}') || ''"),
    )
    .await
}

#[tokio::test]
async fn vector_index_stats_describes_the_index() {
    let conn = conn().await;
    points(&conn, "pts", "", 120, 8).await;

    // The parameters, with the defaults filled in.
    assert_eq!(stat(&conn, "pts_idx", "type").await, "diskann");
    assert_eq!(stat(&conn, "pts_idx", "metric").await, "cosine");
    assert_eq!(stat(&conn, "pts_idx", "vector_type").await, "float32");
    assert_eq!(stat(&conn, "pts_idx", "dims").await, "8");
    assert_eq!(
        text(
            &conn,
            "SELECT json_type(vector_index_stats('pts_idx'), '$.compress_neighbors')"
        )
        .await,
        "null"
    );
    assert_eq!(stat(&conn, "pts_idx", "search_l").await, "200");
    assert_eq!(stat(&conn, "pts_idx", "insert_l").await, "70");
    assert_eq!(stat(&conn, "pts_idx", "alpha").await, "1.2");

    // The graph: one node per row, every one connected, none past its limit.
    let nodes: i64 = stat(&conn, "pts_idx", "nodes").await.parse().unwrap();
    let edges: i64 = stat(&conn, "pts_idx", "edges").await.parse().unwrap();
    let max: i64 = stat(&conn, "pts_idx", "max_neighbors")
        .await
        .parse()
        .unwrap();
    let avg: f64 = stat(&conn, "pts_idx", "avg_neighbors")
        .await
        .parse()
        .unwrap();
    assert_eq!(nodes, 120);
    assert!(edges > 0 && max > 0);
    assert!((avg - edges as f64 / nodes as f64).abs() < 1e-9);
    assert!(avg <= max as f64, "{avg} > {max}");
    assert_eq!(stat(&conn, "pts_idx", "nodes_without_neighbors").await, "0");
    // Counted again from the node blocks themselves: a node's neighbour count
    // is the little-endian u16 after its 8-byte rowid.
    let mut rows = conn
        .query("SELECT data FROM pts_idx_shadow", ())
        .await
        .unwrap();
    let mut counts = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        let block = row.get::<Vec<u8>>(0).unwrap();
        counts.push(u16::from_le_bytes([block[8], block[9]]) as i64);
    }
    assert_eq!(counts.iter().sum::<i64>(), edges);
    let full: i64 = stat(&conn, "pts_idx", "nodes_full").await.parse().unwrap();
    assert_eq!(full, counts.iter().filter(|&&c| c == max).count() as i64);
    let block: i64 = stat(&conn, "pts_idx", "block_size").await.parse().unwrap();
    assert_eq!(
        stat(&conn, "pts_idx", "bytes").await,
        (nodes * block).to_string(),
        "every node is one block"
    );

    // It follows the table.
    conn.execute("DELETE FROM pts WHERE id > 100", ())
        .await
        .unwrap();
    assert_eq!(stat(&conn, "pts_idx", "nodes").await, "100");
}

#[tokio::test]
async fn vector_index_stats_reports_the_options_an_index_was_built_with() {
    let conn = conn().await;
    points(&conn, "plain", "", 20, 64).await;
    points(
        &conn,
        "tuned",
        ", 'metric=l2', 'compress_neighbors=float8', 'search_l=50'",
        20,
        64,
    )
    .await;
    assert_eq!(stat(&conn, "tuned_idx", "metric").await, "l2");
    assert_eq!(
        stat(&conn, "tuned_idx", "compress_neighbors").await,
        "float8"
    );
    assert_eq!(stat(&conn, "tuned_idx", "search_l").await, "50");
    // Compressed neighbour vectors keep as many neighbours in a smaller block.
    for field in ["max_neighbors", "nodes"] {
        assert_eq!(
            stat(&conn, "tuned_idx", field).await,
            stat(&conn, "plain_idx", field).await
        );
    }
    let plain: i64 = stat(&conn, "plain_idx", "bytes").await.parse().unwrap();
    let tuned: i64 = stat(&conn, "tuned_idx", "bytes").await.parse().unwrap();
    assert!(tuned * 2 < plain, "{tuned} bytes compressed, {plain} plain");
    // An explicit schema works too.
    assert_eq!(stat(&conn, "main.tuned_idx", "metric").await, "l2");
}

#[tokio::test]
async fn vector_index_stats_refuses_what_is_not_a_vector_index() {
    let conn = conn().await;
    points(&conn, "pts", "", 3, 4).await;
    conn.execute("CREATE INDEX pts_id ON pts(id)", ())
        .await
        .unwrap();
    for arg in ["'missing'", "'pts_id'", "42", "NULL", "'nowhere.pts_idx'"] {
        let sql = format!("SELECT vector_index_stats({arg})");
        // The error surfaces when the row is stepped, not when it is prepared.
        let err = match conn.query(&sql, ()).await {
            Err(e) => e,
            Ok(mut rows) => rows.next().await.expect_err("an error"),
        };
        assert!(
            err.to_string().contains("vector_index_stats"),
            "{arg}: {err}"
        );
    }
}

/// The recall recipe in docs/VECTOR_SEARCH.md: how many of the exact nearest
/// neighbours the index finds, measured with the table's own rows as queries.
#[tokio::test]
async fn recall_can_be_measured_in_sql() {
    let conn = conn().await;
    points(&conn, "pts", "", 300, 16).await;
    let recall = real(
        &conn,
        "WITH q AS (SELECT id, v FROM pts WHERE id % 10 = 0)
         SELECT sum(a.id IN (SELECT e.id FROM (
                               SELECT p.id, vector_distance_cos(p.v, q.v) AS d
                               FROM pts AS p ORDER BY d LIMIT 10) AS e))
                / (10.0 * (SELECT count(*) FROM q))
         FROM q, vector_top_k('pts_idx', q.v, 10) AS a",
    )
    .await;
    assert!(recall > 0.9 && recall <= 1.0, "recall@10 = {recall}");
}

/// vector_top_k with its query vector taken from another table, once per row:
/// in a join and in a correlated subquery. Its xBestIndex used to accept a
/// plan without that argument, which SQLite refused as a malfunction.
#[tokio::test]
async fn vector_top_k_takes_its_query_from_another_table() {
    let conn = conn().await;
    points(&conn, "pts", "", 40, 8).await;
    let mut expected = Vec::new();
    for id in [5, 17, 33] {
        expected.push(
            ids(
                &conn,
                &format!(
                    "SELECT a.id FROM vector_top_k('pts_idx', \
                     (SELECT v FROM pts WHERE id = {id}), 3) AS a"
                ),
            )
            .await,
        );
    }
    let joined = names(
        &conn,
        "SELECT group_concat(a.id) FROM pts AS q, vector_top_k('pts_idx', q.v, 3) AS a \
         WHERE q.id IN (5, 17, 33) GROUP BY q.id ORDER BY q.id",
    )
    .await;
    let correlated = names(
        &conn,
        "SELECT (SELECT group_concat(id) FROM vector_top_k('pts_idx', q.v, 3)) \
         FROM pts AS q WHERE q.id IN (5, 17, 33) ORDER BY q.id",
    )
    .await;
    let expected: Vec<String> = expected
        .iter()
        .map(|ids| ids.iter().map(i64::to_string).collect::<Vec<_>>().join(","))
        .collect();
    assert_eq!(joined, expected);
    assert_eq!(correlated, expected);
}

/// A `ROLLBACK TO` that undoes a schema change resets the schema, and only a
/// statement that reads a table loads it again. `vector_index_stats` reads
/// none, so it used to find no index at all until something else did:
/// "not a vector index" for an index that was there all along.
#[tokio::test]
async fn vector_index_stats_after_a_schema_change_is_rolled_back() {
    let conn = conn().await;
    conn.execute(
        "CREATE TABLE pts (id INTEGER PRIMARY KEY, v FLOAT32(2))",
        (),
    )
    .await
    .unwrap();
    conn.execute("CREATE INDEX pts_idx ON pts(sqlanywhere_vector_idx(v))", ())
        .await
        .unwrap();
    conn.execute(
        "INSERT INTO pts VALUES (1, vector32('[1,0]')), (2, vector32('[0,1]'))",
        (),
    )
    .await
    .unwrap();
    let nodes = "SELECT json_extract(vector_index_stats('pts_idx'), '$.nodes')";
    assert_eq!(int(&conn, nodes).await, 2);

    for undo in ["ROLLBACK TO s", "ROLLBACK"] {
        let begin = if undo == "ROLLBACK" {
            "BEGIN"
        } else {
            "SAVEPOINT s"
        };
        conn.execute(begin, ()).await.unwrap();
        conn.execute(
            "CREATE INDEX other ON pts(sqlanywhere_vector_idx(v, 'compress_neighbors=float8'))",
            (),
        )
        .await
        .unwrap();
        conn.execute(undo, ()).await.unwrap();
        assert_eq!(int(&conn, nodes).await, 2, "after {undo}");
        if undo == "ROLLBACK TO s" {
            conn.execute("RELEASE s", ()).await.unwrap();
        }
    }
}
