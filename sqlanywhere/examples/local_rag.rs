//! Local RAG retrieval, end to end, in a single embedded SQL Anywhere database.
//!
//! This example ties together the "vector-native edge" building blocks:
//!
//!   1. `embed()`               — turn text into a vector inline, no external model
//!   2. quantized DiskANN index — `compress_neighbors=float8` for a compact index
//!   3. FTS5 full-text index    — keyword relevance
//!   4. hybrid RRF retrieval    — fuse semantic + keyword ranking in one query
//!
//! Run it with:
//!
//! ```sh
//! cargo run -p sqlanywhere --example local_rag
//! ```

use sqlanywhere::{embed, params, Builder, Connection};

const DIMS: usize = 256;

/// A tiny knowledge base. In a real app these would be your documents/chunks.
const CORPUS: &[(&str, &str)] = &[
    (
        "Ownership",
        "Rust enforces memory safety through ownership and borrowing.",
    ),
    (
        "Async",
        "Async Rust uses futures and an executor to run tasks concurrently.",
    ),
    (
        "Vectors",
        "A vector database indexes embeddings for similarity search.",
    ),
    (
        "Baking",
        "This sponge cake recipe needs flour, sugar, eggs and butter.",
    ),
    (
        "Gardening",
        "Water tomatoes regularly and give them plenty of sunlight.",
    ),
    (
        "Edge",
        "Embedded replicas keep a local copy of the database for fast reads.",
    ),
];

async fn setup() -> Connection {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();

    conn.execute(
        &format!("CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, body TEXT, emb FLOAT32({DIMS}))"),
        (),
    )
    .await
    .unwrap();

    // Quantized DiskANN index (float8) — compact enough for constrained devices.
    conn.execute(
        "CREATE INDEX docs_vec ON docs(sqlanywhere_vector_idx(emb, 'metric=cosine', 'compress_neighbors=float8'))",
        (),
    )
    .await
    .unwrap();

    // FTS5 keyword index over the body text.
    conn.execute(
        "CREATE VIRTUAL TABLE docs_fts USING fts5(body, content='docs', content_rowid='id')",
        (),
    )
    .await
    .unwrap();

    // Ingest: embed each document inline — no external embedding service.
    for (i, (title, body)) in CORPUS.iter().enumerate() {
        let id = i as i64 + 1;
        conn.execute(
            "INSERT INTO docs (id, title, body, emb) VALUES (?, ?, ?, vector32(?))",
            params![id, *title, *body, embed(body, DIMS)],
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

    conn
}

/// Hybrid retrieval: fuse vector similarity and keyword relevance with
/// Reciprocal Rank Fusion, returning the top `k` (title, body) rows.
async fn retrieve(conn: &Connection, question: &str, k: i64) -> Vec<(String, String)> {
    let query_vec = embed(question, DIMS);

    let sql = "
        WITH v AS (
            SELECT k.id, ROW_NUMBER() OVER () AS vrank
            FROM vector_top_k('docs_vec', vector32(?1), ?2) k
        ),
        f AS (
            SELECT docs_fts.rowid AS id, ROW_NUMBER() OVER (ORDER BY rank) AS frank
            FROM docs_fts WHERE docs_fts MATCH ?3
        )
        SELECT d.title, d.body
        FROM docs d
        LEFT JOIN v ON v.id = d.id
        LEFT JOIN f ON f.id = d.id
        WHERE v.id IS NOT NULL OR f.id IS NOT NULL
        ORDER BY COALESCE(1.0/(60+v.vrank),0) + COALESCE(1.0/(60+f.frank),0) DESC
        LIMIT ?4";

    // Turn the question into an FTS5 OR-query of its words so keyword matching
    // is forgiving (any shared word contributes).
    let fts_query = question
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .collect::<Vec<_>>()
        .join(" OR ");

    let mut rows = conn
        .query(sql, params![query_vec, k * 3, fts_query, k])
        .await
        .unwrap();

    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push((row.get::<String>(0).unwrap(), row.get::<String>(1).unwrap()));
    }
    out
}

#[tokio::main]
async fn main() {
    let conn = setup().await;

    for question in [
        "how does rust handle memory and ownership",
        "growing tomatoes in the garden",
        "similarity search over embeddings",
    ] {
        println!("\nQ: {question}");
        for (rank, (title, body)) in retrieve(&conn, question, 3).await.iter().enumerate() {
            println!("  {}. [{}] {}", rank + 1, title, body);
        }
    }
}
