//! `search_l` per query (`vector_top_k`'s fourth argument) and the recall it
//! buys, measured and tuned by `sqlanywhere::recall`.

use sqlanywhere::{recall, Builder, Connection};

/// Deterministic pseudo-random numbers, so the corpus is the same every run.
fn lcg(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 33
}

/// `rows` random 16-dimensional vectors under a cosine DiskANN index with
/// one-bit neighbours, which keeps recall at a small `search_l` well short
/// of 1.
async fn corpus(rows: u32) -> Connection {
    let conn = Builder::new_local(":memory:")
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    conn.execute(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, emb FLOAT32(16))",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "CREATE INDEX docs_vec ON docs(sqlanywhere_vector_idx(emb, 'metric=cosine', \
         'compress_neighbors=float1bit', 'max_neighbors=8'))",
        (),
    )
    .await
    .unwrap();
    let mut state = 11u64;
    let tx = conn.transaction().await.unwrap();
    for id in 1..=rows {
        let v: Vec<String> = (0..16)
            .map(|_| format!("{:.3}", (lcg(&mut state) % 2000) as f64 / 1000.0 - 1.0))
            .collect();
        tx.execute(
            "INSERT INTO docs VALUES (?1, vector32(?2))",
            sqlanywhere::params![id, format!("[{}]", v.join(","))],
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    conn
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

async fn count(conn: &Connection, sql: &str) -> i64 {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let mut n = 0;
    while rows.next().await.unwrap().is_some() {
        n += 1;
    }
    n
}

#[tokio::test]
async fn search_l_is_a_fourth_argument() {
    let conn = corpus(200).await;
    let q = "(SELECT emb FROM docs WHERE id = 7)";
    for search_l in ["1", "10", "500", "NULL"] {
        assert_eq!(
            count(
                &conn,
                &format!("SELECT id FROM vector_top_k('docs_vec', {q}, 10, {search_l})")
            )
            .await,
            10,
            "search_l {search_l}"
        );
    }
    for bad in ["0", "-1", "'many'", "2.5", "1048577"] {
        let err = error(
            &conn,
            &format!("SELECT id FROM vector_top_k('docs_vec', {q}, 10, {bad})"),
        )
        .await;
        assert!(err.contains("search_l"), "{bad}: {err}");
    }
    // hybrid_top_k passes it on; NULL keeps a default.
    conn.execute("CREATE VIRTUAL TABLE docs_fts USING fts5(body)", ())
        .await
        .unwrap();
    assert_eq!(
        count(
            &conn,
            &format!("SELECT id FROM hybrid_top_k('docs_vec', 'docs_fts', {q}, NULL, 5, NULL, 64)")
        )
        .await,
        5
    );
    let err = error(
        &conn,
        &format!("SELECT id FROM hybrid_top_k('docs_vec', 'docs_fts', {q}, NULL, 5, 60, 0)"),
    )
    .await;
    assert!(err.contains("search_l"), "{err}");
}

#[tokio::test]
async fn a_larger_search_l_finds_more_of_the_true_neighbours() {
    let conn = corpus(2000).await;
    let narrow = recall::measure(&conn, "docs_vec", 10, Some(10), 100)
        .await
        .unwrap();
    let wide = recall::measure(&conn, "docs_vec", 10, Some(400), 100)
        .await
        .unwrap();
    let default = recall::measure(&conn, "docs_vec", 10, None, 100)
        .await
        .unwrap();
    assert!(narrow < 0.9, "recall@10 at search_l 10: {narrow}");
    assert!(wide > narrow + 0.05, "{narrow} -> {wide}");
    assert!(wide >= 0.9, "recall@10 at search_l 400: {wide}");
    assert!(default > narrow, "the index's own 200: {default}");
}

#[tokio::test]
async fn tune_finds_the_smallest_search_l_that_reaches_the_target() {
    let conn = corpus(2000).await;
    let tuning = recall::tune(&conn, "docs_vec", 10, 0.9, 100).await.unwrap();
    let chosen = tuning.search_l.expect("0.9 is reachable");
    let ladder: Vec<_> = tuning.steps.iter().map(|s| s.search_l).collect();
    assert_eq!(ladder.first(), Some(&10));
    assert_eq!(ladder.last(), Some(&chosen));
    assert!(ladder.windows(2).all(|w| w[1] == w[0] * 2), "{ladder:?}");
    assert!(tuning.steps.last().unwrap().recall >= 0.9);
    assert!(
        tuning.steps[..tuning.steps.len() - 1]
            .iter()
            .all(|s| s.recall < 0.9),
        "{tuning:?}"
    );
    assert!(
        ladder.len() > 1,
        "search_l 10 already reached 0.9: {tuning:?}"
    );

    // A target of 1 is met at the latest when search_l covers every row.
    let small = corpus(300).await;
    let all = recall::tune(&small, "docs_vec", 5, 1.0, 20).await.unwrap();
    assert!(all.search_l.is_some(), "{all:?}");
}

#[tokio::test]
async fn recall_names_what_it_cannot_measure() {
    let conn = corpus(50).await;
    conn.execute("CREATE INDEX docs_id ON docs(id)", ())
        .await
        .unwrap();
    for (result, expected) in [
        (
            recall::measure(&conn, "nope", 10, None, 10).await,
            "no index named nope",
        ),
        (
            recall::measure(&conn, "docs_id", 10, None, 10).await,
            "not a vector index",
        ),
        (
            recall::measure(&conn, "docs_vec", 0, None, 10).await,
            "at least 1",
        ),
        (
            recall::tune(&conn, "docs_vec", 10, 1.5, 10)
                .await
                .map(|_| 0.0),
            "not between 0 and 1",
        ),
    ] {
        let err = result.unwrap_err().to_string();
        assert!(err.contains(expected), "{err}");
    }
}

/// The k best are picked from the candidates: a `search_l` below k, the
/// index's own included, is raised to k rather than returning fewer rows.
#[tokio::test]
async fn k_above_search_l_still_returns_k_rows() {
    let conn = corpus(500).await;
    let q = "(SELECT emb FROM docs WHERE id = 7)";
    // The index's own search_l is 200.
    assert_eq!(
        count(
            &conn,
            &format!("SELECT id FROM vector_top_k('docs_vec', {q}, 300)")
        )
        .await,
        300
    );
    assert_eq!(
        count(
            &conn,
            &format!("SELECT id FROM vector_top_k('docs_vec', {q}, 50, 5)")
        )
        .await,
        50
    );
}
