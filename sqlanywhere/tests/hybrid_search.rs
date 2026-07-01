//! Hybrid search: combine DiskANN vector similarity with FTS5 full-text
//! keyword relevance in a single SQL query, fused with Reciprocal Rank Fusion
//! (RRF). This is the recommended pattern for local-first / edge RAG: neither
//! pure vector nor pure keyword search alone, but a fused ranking that rewards
//! documents strong in both signals.

use sqlanywhere::{Builder, Connection};

async fn conn() -> Connection {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    db.connect().unwrap()
}

async fn ids(conn: &Connection, sql: &str) -> Vec<i64> {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get::<i64>(0).unwrap());
    }
    out
}

/// Seed a small corpus with both an embedding column (DiskANN indexed) and an
/// FTS5 full-text index over the body text.
async fn seed(conn: &Connection) {
    conn.execute(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, body TEXT, emb FLOAT32(4))",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "CREATE INDEX docs_vec ON docs(sqlanywhere_vector_idx(emb))",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "CREATE VIRTUAL TABLE docs_fts USING fts5(body, content='docs', content_rowid='id')",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO docs VALUES \
         (1,'Rust guide','memory safety and ownership in systems programming',vector32('[1,0,0,0]')), \
         (2,'Cooking','a recipe for safety in the kitchen with sharp knives',vector32('[0,1,0,0]')), \
         (3,'Rust async','async runtime and ownership across await points',vector32('[0.9,0.1,0,0]')), \
         (4,'Gardening','ownership of a garden and safety gloves',vector32('[0,0,1,0]'))",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO docs_fts(rowid, body) SELECT id, body FROM docs",
        (),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn fts5_is_available() {
    let conn = conn().await;
    // Creating an FTS5 virtual table proves the module is compiled in.
    conn.execute("CREATE VIRTUAL TABLE t USING fts5(body)", ())
        .await
        .unwrap();
    conn.execute("INSERT INTO t(body) VALUES ('the quick brown fox')", ())
        .await
        .unwrap();
    let hits = ids(
        &conn,
        "SELECT rowid FROM t WHERE t MATCH 'quick' ORDER BY rank",
    )
    .await;
    assert_eq!(hits, vec![1]);
}

#[tokio::test]
async fn keyword_and_vector_search_independently() {
    let conn = conn().await;
    seed(&conn).await;

    // Pure keyword: every doc mentioning "ownership".
    let mut kw = ids(
        &conn,
        "SELECT rowid FROM docs_fts WHERE docs_fts MATCH 'ownership'",
    )
    .await;
    kw.sort();
    assert_eq!(kw, vec![1, 3, 4]);

    // Pure vector: nearest neighbours of [1,0,0,0].
    let vec_hits = ids(
        &conn,
        "SELECT k.id FROM vector_top_k('docs_vec', vector32('[1,0,0,0]'), 2) k",
    )
    .await;
    // docs 1 and 3 have embeddings closest to [1,0,0,0].
    let mut sorted = vec_hits.clone();
    sorted.sort();
    assert_eq!(sorted, vec![1, 3]);
}

#[tokio::test]
async fn hybrid_rrf_ranks_documents_strong_in_both_signals_first() {
    let conn = conn().await;
    seed(&conn).await;

    // Reciprocal Rank Fusion: score = sum over each ranker of 1/(k + rank).
    // Doc 1 ("Rust guide") is top in BOTH the vector and keyword rankers, so it
    // must come first. Doc 2 ("Cooking") matches neither signal well, so last.
    let ranked = ids(
        &conn,
        "WITH v AS (
             SELECT k.id, ROW_NUMBER() OVER () AS vrank
             FROM vector_top_k('docs_vec', vector32('[1,0,0,0]'), 4) k
         ),
         f AS (
             SELECT docs_fts.rowid AS id, ROW_NUMBER() OVER (ORDER BY rank) AS frank
             FROM docs_fts WHERE docs_fts MATCH 'ownership'
         )
         SELECT d.id
         FROM docs d
         LEFT JOIN v ON v.id = d.id
         LEFT JOIN f ON f.id = d.id
         WHERE v.id IS NOT NULL OR f.id IS NOT NULL
         ORDER BY (COALESCE(1.0/(60+v.vrank),0) + COALESCE(1.0/(60+f.frank),0)) DESC",
    )
    .await;

    // Doc 1 first (strong in both), doc 2 last (weak in both).
    assert_eq!(ranked.first(), Some(&1), "expected doc 1 ranked first");
    assert_eq!(ranked.last(), Some(&2), "expected doc 2 ranked last");
    // All four documents participate in the fused ranking.
    assert_eq!(ranked.len(), 4);
}
