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

async fn rows(conn: &Connection, sql: &str) -> Vec<Vec<sqlanywhere::Value>> {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let n = rows.column_count();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push((0..n).map(|i| row.get_value(i).unwrap()).collect());
    }
    out
}

async fn error(conn: &Connection, sql: &str) -> String {
    match conn.query(sql, ()).await {
        Err(e) => e.to_string(),
        Ok(mut rows) => match rows.next().await {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected {sql} to fail"),
        },
    }
}

#[tokio::test]
async fn hybrid_top_k_fuses_both_rankings_in_one_call() {
    use sqlanywhere::Value::{Integer, Null};

    let conn = conn().await;
    seed(&conn).await;

    // The same question as the RRF recipe above, as a table-valued function.
    let ranked = rows(
        &conn,
        "SELECT h.id, d.title, h.vector_rank, h.fts_rank \
         FROM hybrid_top_k('docs_vec', 'docs_fts', vector32('[1,0,0,0]'), 'ownership', 4) h \
         JOIN docs d ON d.id = h.id",
    )
    .await;
    let order: Vec<_> = ranked.iter().map(|r| r[0].clone()).collect();
    assert_eq!(
        order.first(),
        Some(&Integer(1)),
        "doc 1 is first in both rankings"
    );
    assert_eq!(ranked[0][2], Integer(1));
    assert_eq!(ranked[0][3], Integer(1));
    assert_eq!(order.last(), Some(&Integer(2)), "doc 2 is near in neither");
    assert_eq!(ranked.len(), 4);
    // Doc 2 does not mention "ownership", so it has no keyword rank.
    assert_eq!(ranked[3][3], Null);

    // A NULL query leaves the keyword ranker out, a NULL vector the vector ranker.
    let vector_only = rows(
        &conn,
        "SELECT id, fts_rank FROM hybrid_top_k('docs_vec', 'docs_fts', vector32('[1,0,0,0]'), NULL, 2)",
    )
    .await;
    assert_eq!(vector_only.len(), 2);
    assert!(vector_only.iter().all(|r| r[1] == Null));
    let keyword_only = ids(
        &conn,
        "SELECT id FROM hybrid_top_k('docs_vec', 'docs_fts', NULL, 'ownership', 10)",
    )
    .await;
    assert_eq!(
        keyword_only,
        ids(
            &conn,
            "SELECT rowid FROM docs_fts WHERE docs_fts MATCH 'ownership' ORDER BY rank"
        )
        .await
    );

    // k bounds the result, and a schema-qualified FTS table works.
    assert_eq!(
        ids(
            &conn,
            "SELECT id FROM hybrid_top_k('docs_vec', 'main.docs_fts', vector32('[1,0,0,0]'), 'ownership', 1)"
        )
        .await,
        vec![1]
    );
    assert!(ids(
        &conn,
        "SELECT id FROM hybrid_top_k('docs_vec', 'docs_fts', vector32('[1,0,0,0]'), 'ownership', 0)"
    )
    .await
    .is_empty());
}

/// Deterministic pseudo-random numbers, so the corpus is the same every run.
fn lcg(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 33
}

#[tokio::test]
async fn hybrid_top_k_agrees_with_the_rrf_recipe() {
    let conn = conn().await;
    conn.execute(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, emb FLOAT32(8))",
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
    conn.execute("CREATE VIRTUAL TABLE docs_fts USING fts5(body)", ())
        .await
        .unwrap();

    const WORDS: [&str; 12] = [
        "rust",
        "ownership",
        "async",
        "garden",
        "kitchen",
        "safety",
        "vector",
        "index",
        "query",
        "merge",
        "branch",
        "replica",
    ];
    let mut state = 7u64;
    for id in 1..=300 {
        let body: Vec<&str> = (0..6)
            .map(|_| WORDS[(lcg(&mut state) % WORDS.len() as u64) as usize])
            .collect();
        let emb: Vec<String> = (0..8)
            .map(|_| format!("{:.3}", (lcg(&mut state) % 1000) as f64 / 1000.0 - 0.5))
            .collect();
        conn.execute(
            "INSERT INTO docs VALUES (?1, ?2, vector32(?3))",
            sqlanywhere::params![id, body.join(" "), format!("[{}]", emb.join(","))],
        )
        .await
        .unwrap();
    }
    conn.execute(
        "INSERT INTO docs_fts(rowid, body) SELECT id, body FROM docs",
        (),
    )
    .await
    .unwrap();

    let cases = [
        ("[0.4,-0.2,0.1,0,0.3,-0.4,0.2,0.1]", "ownership", 10, 60.0),
        (
            "[-0.1,0.3,0.2,-0.4,0,0.1,-0.2,0.4]",
            "vector OR index",
            25,
            60.0,
        ),
        (
            "[0.2,0.2,-0.3,0.1,-0.1,0.4,0,-0.2]",
            "rust AND safety",
            5,
            1.0,
        ),
        ("[0,0,0.5,0.5,-0.5,0,0.1,0]", "replica", 40, 0.0),
    ];
    for (vector, query, k, rrf_k) in cases {
        // Each ranker contributes its top k, as hybrid_top_k documents.
        let recipe = rows(
            &conn,
            &format!(
                "WITH v AS (
                     SELECT k.id, ROW_NUMBER() OVER () AS vrank
                     FROM vector_top_k('docs_vec', vector32('{vector}'), {k}) k
                 ),
                 f AS (
                     SELECT rowid AS id, ROW_NUMBER() OVER (ORDER BY rank) AS frank
                     FROM (SELECT rowid, rank FROM docs_fts WHERE docs_fts MATCH '{query}'
                           ORDER BY rank LIMIT {k})
                 ),
                 ids AS (SELECT id FROM v UNION SELECT id FROM f)
                 SELECT ids.id,
                        COALESCE(1.0/({rrf_k}+v.vrank),0) + COALESCE(1.0/({rrf_k}+f.frank),0) AS score,
                        v.vrank, f.frank
                 FROM ids LEFT JOIN v ON v.id = ids.id LEFT JOIN f ON f.id = ids.id
                 ORDER BY score DESC, ids.id
                 LIMIT {k}"
            ),
        )
        .await;
        let fused = rows(
            &conn,
            &format!(
                "SELECT id, score, vector_rank, fts_rank FROM hybrid_top_k(
                     'docs_vec', 'docs_fts', vector32('{vector}'), '{query}', {k}, {rrf_k})"
            ),
        )
        .await;
        assert!(!fused.is_empty(), "{query}: no results");
        assert_eq!(fused, recipe, "{query} k={k} rrf_k={rrf_k}");
    }

    // rrf_k defaults to 60.
    let explicit = rows(
        &conn,
        "SELECT id, score FROM hybrid_top_k('docs_vec', 'docs_fts', vector32('[0,0,0.5,0.5,-0.5,0,0.1,0]'), 'garden', 10, 60)",
    )
    .await;
    let default = rows(
        &conn,
        "SELECT id, score FROM hybrid_top_k('docs_vec', 'docs_fts', vector32('[0,0,0.5,0.5,-0.5,0,0.1,0]'), 'garden', 10)",
    )
    .await;
    assert_eq!(explicit, default);
}

#[tokio::test]
async fn hybrid_top_k_per_row_of_another_table() {
    let conn = conn().await;
    seed(&conn).await;
    conn.execute("CREATE TABLE questions (q TEXT, v BLOB, terms TEXT)", ())
        .await
        .unwrap();
    conn.execute(
        "INSERT INTO questions VALUES \
         ('rust', vector32('[1,0,0,0]'), 'ownership'), \
         ('food', vector32('[0,1,0,0]'), 'kitchen')",
        (),
    )
    .await
    .unwrap();

    // The vector and the query come from the outer row, as with vector_top_k.
    let best = rows(
        &conn,
        "SELECT q.q, (SELECT h.id FROM hybrid_top_k('docs_vec', 'docs_fts', q.v, q.terms, 3) h LIMIT 1) \
         FROM questions q ORDER BY q.q",
    )
    .await;
    use sqlanywhere::Value::{Integer, Text};
    assert_eq!(
        best,
        vec![
            vec![Text("food".into()), Integer(2)],
            vec![Text("rust".into()), Integer(1)],
        ]
    );
    let joined = rows(
        &conn,
        "SELECT q.q, h.id FROM questions q, hybrid_top_k('docs_vec', 'docs_fts', q.v, q.terms, 1) h \
         ORDER BY q.q",
    )
    .await;
    assert_eq!(joined, best);
}

#[tokio::test]
async fn hybrid_top_k_reports_bad_arguments() {
    let conn = conn().await;
    seed(&conn).await;
    conn.execute("CREATE TABLE plain (body TEXT)", ())
        .await
        .unwrap();

    for (sql, expected) in [
        (
            "SELECT * FROM hybrid_top_k('docs_vec', 'docs_fts', vector32('[1,0,0,0]'), 'x')",
            "expected arguments",
        ),
        (
            "SELECT * FROM hybrid_top_k('docs_vec', 'docs_fts', vector32('[1,0,0,0]'), 'x', -1)",
            "k must be a non-negative integer",
        ),
        (
            "SELECT * FROM hybrid_top_k('docs_vec', 'docs_fts', vector32('[1,0,0,0]'), 'x', 3, 'sixty')",
            "rrf_k must be a non-negative number",
        ),
        (
            "SELECT * FROM hybrid_top_k('docs_vec', 'nope', vector32('[1,0,0,0]'), 'x', 3)",
            "no such table: nope",
        ),
        (
            "SELECT * FROM hybrid_top_k('docs_vec', 'plain', NULL, 'x', 3)",
            "fts_table 'plain' must be an FTS5 table",
        ),
        (
            "SELECT * FROM hybrid_top_k('nope', 'docs_fts', vector32('[1,0,0,0]'), 'x', 3)",
            "vector index(search)",
        ),
        (
            "SELECT * FROM hybrid_top_k('docs_vec', 'docs_fts', NULL, 'ownership AND', 3)",
            "fts5: syntax error",
        ),
        (
            "SELECT * FROM hybrid_top_k(1, 'docs_fts', NULL, 'x', 3)",
            "vector_index must be the name of a vector index",
        ),
    ] {
        let err = error(&conn, sql).await;
        assert!(err.contains(expected), "{sql}: {err}");
    }
}
