//! End-to-end test for the reference `embed()` helper: embed text inline,
//! store it in a DiskANN-indexed vector column, and run similarity search.

use sqlanywhere::{embed, params, Builder, Connection};

const DIMS: usize = 128;

async fn conn() -> Connection {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    db.connect().unwrap()
}

async fn ids(
    conn: &Connection,
    sql: &str,
    params: impl sqlanywhere::params::IntoParams,
) -> Vec<i64> {
    let mut rows = conn.query(sql, params).await.unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get::<i64>(0).unwrap());
    }
    out
}

#[tokio::test]
async fn embed_then_index_then_search() {
    let conn = conn().await;
    conn.execute(
        &format!("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, emb FLOAT32({DIMS}))"),
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "CREATE INDEX docs_idx ON docs(sqlanywhere_vector_idx(emb, 'metric=cosine'))",
        (),
    )
    .await
    .unwrap();

    let corpus = [
        (1, "rust memory safety and ownership"),
        (2, "a recipe for chocolate cake with sugar"),
        (3, "async runtime and ownership in rust"),
        (4, "gardening tips for growing tomatoes"),
    ];

    // Embed each document inline via vector32(embed(...)) — no external model.
    for (id, body) in corpus {
        conn.execute(
            "INSERT INTO docs (id, body, emb) VALUES (?, ?, vector32(?))",
            params![id, body, embed(body, DIMS)],
        )
        .await
        .unwrap();
    }

    // Query with a text embedding that shares vocabulary with docs 1 and 3.
    let query = embed("ownership in rust", DIMS);
    let hits = ids(
        &conn,
        "SELECT k.id FROM vector_top_k('docs_idx', vector32(?), 2) k",
        params![query],
    )
    .await;

    assert_eq!(hits.len(), 2);
    // The two rust/ownership docs (1 and 3) should be the nearest neighbours.
    let mut sorted = hits.clone();
    sorted.sort();
    assert_eq!(sorted, vec![1, 3], "expected the rust docs, got {hits:?}");
}
