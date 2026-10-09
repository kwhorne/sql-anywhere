//! Embedding columns: a vector column kept in step with a text column through
//! the durable queue, via `sqlanywhere::embedding_column`.

use sqlanywhere::embedding_column::{EmbeddingColumn, ProcessReport};
use sqlanywhere::{embed, params, Builder, Connection, Embedder, LexicalEmbedder, Value};

const DIMS: usize = 16;

/// The lexical embedder under a different model name: same space, new id.
struct Renamed(&'static str);

impl Embedder for Renamed {
    fn dims(&self) -> usize {
        DIMS
    }
    fn embed(&self, text: &str) -> Vec<f32> {
        // Reversed words, so its vectors differ from the lexical model's.
        let flipped: String = text.split(' ').rev().collect::<Vec<_>>().join(" X ");
        LexicalEmbedder::new(DIMS).embed(&flipped)
    }
    fn model_id(&self) -> String {
        self.0.to_string()
    }
}

/// An embedder that is broken: it returns the wrong number of dimensions.
struct Broken;

impl Embedder for Broken {
    fn dims(&self) -> usize {
        DIMS
    }
    fn embed(&self, _: &str) -> Vec<f32> {
        vec![0.0; DIMS - 1]
    }
    fn model_id(&self) -> String {
        LexicalEmbedder::new(DIMS).model_id()
    }
}

async fn setup() -> (Connection, EmbeddingColumn, LexicalEmbedder) {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute(
        &format!(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, tag TEXT, emb FLOAT32({DIMS}))"
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
    (
        conn,
        EmbeddingColumn::new("docs", "body", "emb"),
        LexicalEmbedder::new(DIMS),
    )
}

async fn scalar(conn: &Connection, sql: &str) -> Value {
    let mut rows = conn.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get_value(0).unwrap()
}

/// The stored vector of row `id`, as the literal `vector_extract` prints.
async fn stored(conn: &Connection, id: i64) -> Option<String> {
    let mut rows = conn
        .query(
            "SELECT vector_extract(emb) FROM docs WHERE id = ? AND emb IS NOT NULL",
            params![id],
        )
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .map(|r| r.get::<String>(0).unwrap())
}

/// What `model` would store for `text`, read back through the same path.
async fn expected<E: Embedder>(conn: &Connection, model: &E, text: &str) -> String {
    let mut rows = conn
        .query(
            "SELECT vector_extract(vector32(?))",
            params![model.embed_literal(text)],
        )
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap()
}

async fn nearest(conn: &Connection, query: &str) -> Option<String> {
    let mut rows = conn
        .query(
            "SELECT d.body FROM vector_top_k('docs_vec', vector32(?), 1) k \
             JOIN docs d ON d.id = k.id",
            params![embed(query, DIMS)],
        )
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .map(|r| r.get::<String>(0).unwrap())
}

#[tokio::test]
async fn install_backfills_and_writes_enqueue() {
    let (conn, column, model) = setup().await;
    conn.execute(
        "INSERT INTO docs (id, body) VALUES (1, 'alpha alpha'), (2, 'beta beta'), (3, NULL)",
        (),
    )
    .await
    .unwrap();

    // Rows with text are backfilled; a NULL body has nothing to embed.
    assert_eq!(column.install(&conn, &model).await.unwrap(), 2);
    let status = column.status(&conn).await.unwrap();
    assert_eq!((status.pending, status.ready), (2, 2));
    assert_eq!(status.model, "lexical-fnv1a-16d");
    assert!(!status.is_current());

    // Writing never waits for the model: the vector is not there yet.
    conn.execute("INSERT INTO docs (id, body) VALUES (4, 'gamma gamma')", ())
        .await
        .unwrap();
    assert_eq!(stored(&conn, 4).await, None);
    assert_eq!(column.status(&conn).await.unwrap().pending, 3);
    // A new row without text is not work.
    conn.execute("INSERT INTO docs (id, body) VALUES (5, NULL)", ())
        .await
        .unwrap();
    assert_eq!(column.status(&conn).await.unwrap().pending, 3);

    let report = column.process(&conn, &model, 100).await.unwrap();
    assert_eq!(
        report,
        ProcessReport {
            embedded: 3,
            ..Default::default()
        }
    );
    assert!(column.status(&conn).await.unwrap().is_current());
    for (id, text) in [(1, "alpha alpha"), (2, "beta beta"), (4, "gamma gamma")] {
        assert_eq!(
            stored(&conn, id).await,
            Some(expected(&conn, &model, text).await)
        );
    }
    assert_eq!(stored(&conn, 3).await, None);
    assert_eq!(
        nearest(&conn, "gamma gamma").await.as_deref(),
        Some("gamma gamma")
    );
}

#[tokio::test]
async fn only_changed_text_is_reembedded() {
    let (conn, column, model) = setup().await;
    column.install(&conn, &model).await.unwrap();
    conn.execute("INSERT INTO docs (id, body) VALUES (1, 'first')", ())
        .await
        .unwrap();
    column.process(&conn, &model, 100).await.unwrap();

    // Another column, or the same text again, is not a change to embed.
    conn.execute("UPDATE docs SET tag = 'x' WHERE id = 1", ())
        .await
        .unwrap();
    conn.execute("UPDATE docs SET body = 'first' WHERE id = 1", ())
        .await
        .unwrap();
    assert!(column.status(&conn).await.unwrap().is_current());

    // Several edits before a worker gets there collapse into one job, which
    // embeds the latest text.
    for text in ["second", "third", "fourth"] {
        conn.execute("UPDATE docs SET body = ? WHERE id = 1", params![text])
            .await
            .unwrap();
    }
    assert_eq!(column.status(&conn).await.unwrap().pending, 1);
    assert_eq!(
        column.process(&conn, &model, 100).await.unwrap().embedded,
        1
    );
    assert_eq!(
        stored(&conn, 1).await,
        Some(expected(&conn, &model, "fourth").await)
    );

    // Clearing the text clears the vector.
    conn.execute("UPDATE docs SET body = NULL WHERE id = 1", ())
        .await
        .unwrap();
    column.process(&conn, &model, 100).await.unwrap();
    assert_eq!(stored(&conn, 1).await, None);
}

/// An embedder that, while it is embedding, has someone else rewrite the row
/// from a second connection: the race a slow model opens.
struct EditsMidway {
    path: String,
}

impl Embedder for EditsMidway {
    fn dims(&self) -> usize {
        DIMS
    }
    fn embed(&self, text: &str) -> Vec<f32> {
        if text == "before" {
            let path = self.path.clone();
            // The worker holds no transaction while embedding, so this write
            // goes through rather than waiting on a lock.
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(async {
                        let db = Builder::new_local(&path).build().await.unwrap();
                        let conn = db.connect().unwrap();
                        conn.execute("UPDATE docs SET body = 'after' WHERE id = 1", ())
                            .await
                            .unwrap();
                    })
            })
            .join()
            .unwrap();
        }
        LexicalEmbedder::new(DIMS).embed(text)
    }
    fn model_id(&self) -> String {
        LexicalEmbedder::new(DIMS).model_id()
    }
}

#[tokio::test]
async fn text_edited_mid_embedding_is_not_overwritten_with_a_stale_vector() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("race.db").to_str().unwrap().to_string();
    let db = Builder::new_local(&path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute(
        &format!("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, emb FLOAT32({DIMS}))"),
        (),
    )
    .await
    .unwrap();
    let column = EmbeddingColumn::new("docs", "body", "emb");
    let model = EditsMidway { path };
    column.install(&conn, &model).await.unwrap();
    conn.execute("INSERT INTO docs (id, body) VALUES (1, 'before')", ())
        .await
        .unwrap();

    // The first job embeds "before", but by the time it writes, the row says
    // "after": the stale vector is dropped and the edit's own job is queued.
    let first = column.process(&conn, &model, 1).await.unwrap();
    assert_eq!((first.embedded, first.skipped), (0, 1));
    assert_eq!(stored(&conn, 1).await, None);
    assert_eq!(column.status(&conn).await.unwrap().pending, 1);

    let second = column.process(&conn, &model, 100).await.unwrap();
    assert_eq!(second.embedded, 1);
    let lexical = LexicalEmbedder::new(DIMS);
    assert_eq!(
        stored(&conn, 1).await,
        Some(expected(&conn, &lexical, "after").await)
    );
}

#[tokio::test]
async fn a_worker_with_another_model_is_refused_until_reembed() {
    let (conn, column, model) = setup().await;
    column.install(&conn, &model).await.unwrap();
    conn.execute(
        "INSERT INTO docs (id, body) VALUES (1, 'one'), (2, 'two')",
        (),
    )
    .await
    .unwrap();
    column.process(&conn, &model, 100).await.unwrap();

    let next = Renamed("next-model-v2");
    let refused = column.process(&conn, &next, 100).await;
    assert!(refused.is_err(), "a different model must not write");

    // Switching models is explicit, and re-embeds every row.
    assert_eq!(column.reembed(&conn, &next).await.unwrap(), 2);
    assert_eq!(column.status(&conn).await.unwrap().model, "next-model-v2");
    assert!(
        column.process(&conn, &model, 100).await.is_err(),
        "old model now refused"
    );
    assert_eq!(column.process(&conn, &next, 100).await.unwrap().embedded, 2);
    for (id, text) in [(1, "one"), (2, "two")] {
        assert_eq!(
            stored(&conn, id).await,
            Some(expected(&conn, &next, text).await)
        );
    }
}

#[tokio::test]
async fn failures_back_off_then_dead_letter() {
    let (conn, column, model) = setup().await;
    column.install(&conn, &model).await.unwrap();
    conn.execute("INSERT INTO docs (id, body) VALUES (1, 'one')", ())
        .await
        .unwrap();

    // A failed attempt goes back on the queue, not ready until its backoff.
    let report = column.process(&conn, &Broken, 100).await.unwrap();
    assert_eq!(report.retried, 1);
    let status = column.status(&conn).await.unwrap();
    assert_eq!((status.pending, status.ready, status.failed), (1, 0, 0));

    // On its last attempt it moves to the dead-letter table with the reason.
    conn.execute(
        "UPDATE askr_jobs SET available_at = unixepoch(), max_attempts = 2",
        (),
    )
    .await
    .unwrap();
    let report = column.process(&conn, &Broken, 100).await.unwrap();
    assert_eq!(report.dead_lettered, 1);
    let status = column.status(&conn).await.unwrap();
    assert_eq!((status.pending, status.failed), (0, 1));
    let reason = scalar(&conn, "SELECT exception FROM askr_failed_jobs").await;
    assert!(
        matches!(&reason, Value::Text(t) if t.contains("15 dims, expected 16")),
        "{reason:?}"
    );
}

#[tokio::test]
async fn deleted_rows_are_skipped() {
    let (conn, column, model) = setup().await;
    column.install(&conn, &model).await.unwrap();
    conn.execute("INSERT INTO docs (id, body) VALUES (1, 'gone soon')", ())
        .await
        .unwrap();
    conn.execute("DELETE FROM docs WHERE id = 1", ())
        .await
        .unwrap();
    let report = column.process(&conn, &model, 100).await.unwrap();
    assert_eq!((report.embedded, report.skipped), (0, 1));
    assert!(column.status(&conn).await.unwrap().is_current());
}

#[tokio::test]
async fn install_checks_the_target() {
    let (conn, column, model) = setup().await;
    // Not a vector column.
    assert!(EmbeddingColumn::new("docs", "body", "tag")
        .install(&conn, &model)
        .await
        .is_err());
    // Wrong dimensions for the model.
    assert!(column
        .install(&conn, &LexicalEmbedder::new(DIMS * 2))
        .await
        .is_err());
    // Twice.
    column.install(&conn, &model).await.unwrap();
    assert!(column.install(&conn, &model).await.is_err());
    // Uninstall stops the triggers and keeps the vectors.
    conn.execute("INSERT INTO docs (id, body) VALUES (1, 'kept')", ())
        .await
        .unwrap();
    column.process(&conn, &model, 100).await.unwrap();
    column.uninstall(&conn).await.unwrap();
    conn.execute("UPDATE docs SET body = 'changed' WHERE id = 1", ())
        .await
        .unwrap();
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM askr_jobs").await,
        Value::Integer(0)
    );
    assert_eq!(
        stored(&conn, 1).await,
        Some(expected(&conn, &model, "kept").await)
    );
    assert!(column.status(&conn).await.is_err());
}
