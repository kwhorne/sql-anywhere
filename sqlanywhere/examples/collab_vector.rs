//! Collaborative, syncable vector index — the flagship "vector-native edge"
//! combination: CRDT offline merge (cr-sqlite) × DiskANN vector search × inline
//! `embed()`.
//!
//! Two devices build a semantic index **offline and independently**, then merge
//! conflict-free. Afterwards each device can vector-search over *both* devices'
//! documents — a shared, semantic knowledge base that lives on the edge and
//! syncs offline, with no central server.
//!
//! Requires the cr-sqlite extension. Build it with `scripts/build-crsqlite.sh`
//! (or download it from a release), then run:
//!
//! ```sh
//! cargo run -p sqlanywhere --example collab_vector
//! # or: SQLANYWHERE_CRSQLITE=/path/to/crsqlite.dylib cargo run ...
//! ```

use sqlanywhere::{embed, params, params::params_from_iter, Builder, Connection, Value};

const DIMS: usize = 128;

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

async fn call(conn: &Connection, sql: &str) {
    let mut rows = conn.query(sql, ()).await.unwrap();
    while rows.next().await.unwrap().is_some() {}
}

/// Open a node: load cr-sqlite, create a vector-indexed table and mark it as a
/// conflict-free replicated relation.
async fn open_node(path: &str, ext: &str) -> Connection {
    let db = Builder::new_local(path).build().await.unwrap();
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

/// Insert a document, embedding its text inline.
async fn add(conn: &Connection, id: i64, body: &str) {
    conn.execute(
        "INSERT INTO docs (id, body, emb) VALUES (?, ?, vector32(?))",
        params![id, body, embed(body, DIMS)],
    )
    .await
    .unwrap();
}

/// Merge every changeset row from `from` into `to`.
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

/// Semantic search: embed the query text and return the nearest document bodies.
async fn search(conn: &Connection, query: &str) -> Vec<String> {
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

#[tokio::main]
async fn main() {
    let ext = extension_path();
    let tmp = std::env::temp_dir();
    let (pa, pb) = (tmp.join("collab_a.db"), tmp.join("collab_b.db"));
    let _ = std::fs::remove_file(&pa);
    let _ = std::fs::remove_file(&pb);

    println!("== Collaborative syncable vector index (CRDT x vector x embed) ==");
    let a = open_node(pa.to_str().unwrap(), &ext).await;
    let b = open_node(pb.to_str().unwrap(), &ext).await;

    // Two devices index different documents while OFFLINE.
    add(&a, 1, "the cat sat on the mat").await;
    add(&a, 2, "a dog chased the ball in the park").await;
    add(&b, 3, "the car drove down the highway").await;
    add(&b, 4, "a truck delivered the heavy cargo").await;

    println!("\nBefore sync — node A only knows its own docs:");
    println!(
        "  A: search 'vehicle on the road' -> {:?}",
        search(&a, "vehicle on the road").await
    );

    // Merge both ways: the semantic indexes converge, conflict-free.
    merge(&a, &b).await;
    merge(&b, &a).await;

    println!("\nAfter sync — each node can search over BOTH devices' documents:");
    println!(
        "  A: search 'vehicle on the road' -> {:?}",
        search(&a, "vehicle on the road").await
    );
    println!(
        "  A: search 'a pet animal'        -> {:?}",
        search(&a, "a pet animal").await
    );
    println!(
        "  B: search 'a pet animal'        -> {:?}",
        search(&b, "a pet animal").await
    );

    call(&a, "SELECT crsql_finalize()").await;
    call(&b, "SELECT crsql_finalize()").await;
}
