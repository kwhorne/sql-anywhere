//! Database branches: fork, diff, merge and pull through `sqlanywhere::branch`.
//!
//! Gated on `SQLANYWHERE_CRSQLITE` (path to a built `crsqlite.{dylib,so}`, see
//! `scripts/build-crsqlite.sh`); the tests skip and pass when it is unset, so
//! the main workspace CI is unaffected.

use sqlanywhere::branch::{self, ChangeKind, Side};
use sqlanywhere::{embed, params, Builder, Connection, Value};

const DIMS: usize = 16;

fn extension() -> Option<String> {
    let ext = std::env::var("SQLANYWHERE_CRSQLITE").ok();
    if ext.is_none() {
        eprintln!(
            "skipping: set SQLANYWHERE_CRSQLITE to a built crsqlite.{{dylib,so}} \
             (see scripts/build-crsqlite.sh) to run this test"
        );
    }
    ext
}

async fn open(ext: &str) -> Connection {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.load_extension_enable().unwrap();
    conn.load_extension(ext, Some("sqlite3_crsqlite_init"))
        .unwrap_or_else(|e| panic!("failed to load cr-sqlite from '{ext}': {e}"));
    conn.load_extension_disable().unwrap();
    conn
}

async fn call(conn: &Connection, sql: &str) {
    let mut rows = conn.query(sql, ()).await.unwrap();
    while rows.next().await.unwrap().is_some() {}
}

async fn scalar(conn: &Connection, sql: &str) -> Value {
    let mut rows = conn.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get_value(0).unwrap()
}

/// A parent with a tracked, vector-indexed table, an untracked audit log fed
/// by a trigger, and a view.
async fn main_db(ext: &str) -> Connection {
    let conn = open(ext).await;
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
    conn.execute("CREATE TABLE audit (msg TEXT)", ())
        .await
        .unwrap();
    conn.execute(
        "CREATE TRIGGER docs_audit AFTER INSERT ON docs \
         BEGIN INSERT INTO audit VALUES ('added ' || new.id); END",
        (),
    )
    .await
    .unwrap();
    conn.execute("CREATE VIEW titles AS SELECT body FROM docs", ())
        .await
        .unwrap();
    for (id, body) in [(1, "alpha alpha"), (2, "beta beta"), (3, "gamma gamma")] {
        put(&conn, id, body).await;
    }
    conn
}

async fn put(conn: &Connection, id: i64, body: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO docs (id, body, emb) VALUES (?, ?, vector32(?))",
        params![id, body, embed(body, DIMS)],
    )
    .await
    .unwrap();
}

async fn body(conn: &Connection, id: i64) -> Option<String> {
    let mut rows = conn
        .query("SELECT body FROM docs WHERE id = ?", params![id])
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .map(|r| r.get::<String>(0).unwrap())
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
async fn fork_copies_schema_and_data() {
    let Some(ext) = extension() else { return };
    let main = main_db(&ext).await;
    let br = open(&ext).await;

    let info = branch::fork(&main, &br).await.unwrap();
    assert_eq!(info.tracked_tables, vec!["docs".to_string()]);
    assert_eq!(info.untracked_tables, vec!["audit".to_string()]);
    assert_eq!(branch::info(&br).await.unwrap(), info);

    assert_eq!(
        scalar(&br, "SELECT count(*) FROM docs").await,
        Value::Integer(3)
    );
    assert_eq!(
        scalar(&br, "SELECT count(*) FROM titles").await,
        Value::Integer(3)
    );
    // The trigger ran on main; creating it after the data means it did not run
    // again for the same rows on the branch.
    assert_eq!(
        scalar(&br, "SELECT count(*) FROM audit").await,
        Value::Integer(3)
    );
    // The vector index was rebuilt from the copied rows.
    assert_eq!(
        nearest(&br, "beta beta").await.as_deref(),
        Some("beta beta")
    );

    // Nothing written on the branch yet, so nothing to merge.
    assert!(branch::diff(&main, &br).await.unwrap().is_empty());
}

#[tokio::test]
async fn diff_then_merge() {
    let Some(ext) = extension() else { return };
    let main = main_db(&ext).await;
    let br = open(&ext).await;
    branch::fork(&main, &br).await.unwrap();

    put(&br, 2, "beta rewritten entirely").await;
    put(&br, 4, "delta delta").await;
    br.execute("DELETE FROM docs WHERE id = 3", ())
        .await
        .unwrap();

    let diff = branch::diff(&main, &br).await.unwrap();
    assert!(diff.conflicts.is_empty(), "{:?}", diff.conflicts);
    assert_eq!(diff.untracked_tables, vec!["audit".to_string()]);
    let kinds: Vec<_> = diff.rows.iter().map(|r| (r.pk.clone(), r.kind)).collect();
    assert_eq!(
        kinds,
        vec![
            (vec![Value::Integer(2)], ChangeKind::Update),
            (vec![Value::Integer(4)], ChangeKind::Insert),
            (vec![Value::Integer(3)], ChangeKind::Delete),
        ]
    );

    let update = &diff.rows[0];
    let body_change = update.columns.iter().find(|c| c.column == "body").unwrap();
    assert_eq!(body_change.on_main, Some(Value::Text("beta beta".into())));
    assert_eq!(
        body_change.on_branch,
        Value::Text("beta rewritten entirely".into())
    );
    assert_eq!(body_change.vector_distance, None, "body is not a vector");
    let emb_change = update.columns.iter().find(|c| c.column == "emb").unwrap();
    let moved = emb_change
        .vector_distance
        .expect("emb is a vector on both sides");
    assert!(moved > 0.0 && moved <= 2.0, "embedding moved by {moved}");

    let insert = &diff.rows[1];
    assert!(insert.columns.iter().all(|c| c.on_main.is_none()));
    assert!(diff.rows[2].columns.is_empty());

    // Reviewing is read-only: main is untouched until the merge.
    assert_eq!(body(&main, 2).await.as_deref(), Some("beta beta"));

    let report = branch::merge(&main, &br).await.unwrap();
    assert!(report.changes_applied > 0);
    assert!(report.conflicts.is_empty());
    assert_eq!(
        body(&main, 2).await.as_deref(),
        Some("beta rewritten entirely")
    );
    assert_eq!(body(&main, 4).await.as_deref(), Some("delta delta"));
    assert_eq!(body(&main, 3).await, None);
    assert_eq!(
        nearest(&main, "delta delta").await.as_deref(),
        Some("delta delta")
    );

    // Merged changes are not pending any more, and are not sent twice.
    assert!(branch::diff(&main, &br).await.unwrap().is_empty());
    assert_eq!(branch::merge(&main, &br).await.unwrap().changes_applied, 0);

    // A later write is the only thing the next merge carries.
    put(&br, 5, "epsilon").await;
    let diff = branch::diff(&main, &br).await.unwrap();
    assert_eq!(diff.rows.len(), 1);
    assert_eq!(diff.rows[0].pk, vec![Value::Integer(5)]);
}

#[tokio::test]
async fn concurrent_edits_are_reported_with_their_winner() {
    let Some(ext) = extension() else { return };
    let main = main_db(&ext).await;
    let br = open(&ext).await;
    branch::fork(&main, &br).await.unwrap();

    // Same cell on both sides, and a row one side deletes while the other edits.
    br.execute("UPDATE docs SET body = 'branch says' WHERE id = 1", ())
        .await
        .unwrap();
    main.execute("UPDATE docs SET body = 'main says' WHERE id = 1", ())
        .await
        .unwrap();
    br.execute("UPDATE docs SET body = 'branch keeps it' WHERE id = 2", ())
        .await
        .unwrap();
    main.execute("DELETE FROM docs WHERE id = 2", ())
        .await
        .unwrap();
    // Main and branch agreeing is not a conflict.
    br.execute("UPDATE docs SET body = 'same' WHERE id = 3", ())
        .await
        .unwrap();
    main.execute("UPDATE docs SET body = 'same' WHERE id = 3", ())
        .await
        .unwrap();

    let diff = branch::diff(&main, &br).await.unwrap();
    let predicted: Vec<_> = diff
        .conflicts
        .iter()
        .map(|c| (c.pk.clone(), c.column.clone(), c.winner))
        .collect();
    assert_eq!(
        predicted,
        vec![
            (vec![Value::Integer(1)], Some("body".to_string()), None),
            (vec![Value::Integer(2)], None, None),
        ]
    );

    let report = branch::merge(&main, &br).await.unwrap();
    assert_eq!(report.conflicts.len(), 2);
    for conflict in &report.conflicts {
        let winner = conflict.winner.expect("a merge observes the winner");
        match (conflict.pk[0].clone(), conflict.column.as_deref()) {
            (Value::Integer(1), Some("body")) => {
                let expect = if winner == Side::Branch {
                    "branch says"
                } else {
                    "main says"
                };
                assert_eq!(body(&main, 1).await.as_deref(), Some(expect));
            }
            (Value::Integer(2), None) => {
                let exists = body(&main, 2).await.is_some();
                assert_eq!(exists, winner == Side::Branch, "row 2 winner {winner:?}");
            }
            other => panic!("unexpected conflict {other:?}"),
        }
    }

    // Pulling brings the branch to the same state main converged to.
    branch::pull(&main, &br).await.unwrap();
    for id in 1..=3 {
        assert_eq!(body(&br, id).await, body(&main, id).await, "row {id}");
    }
}

#[tokio::test]
async fn pull_moves_the_base_forward() {
    let Some(ext) = extension() else { return };
    let main = main_db(&ext).await;
    let br = open(&ext).await;
    let forked = branch::fork(&main, &br).await.unwrap();

    main.execute("UPDATE docs SET body = 'newer on main' WHERE id = 1", ())
        .await
        .unwrap();
    put(&main, 9, "iota").await;

    let report = branch::pull(&main, &br).await.unwrap();
    assert!(report.changes_applied > 0);
    assert!(report.conflicts.is_empty());
    assert_eq!(body(&br, 1).await.as_deref(), Some("newer on main"));
    assert_eq!(body(&br, 9).await.as_deref(), Some("iota"));
    // A pulled row is an ordinary write on the branch, so its triggers fire.
    assert_eq!(
        scalar(&br, "SELECT count(*) FROM audit").await,
        Value::Integer(4)
    );
    assert!(branch::info(&br).await.unwrap().base_version > forked.base_version);

    // Pulled changes are main's, not the branch's: nothing to merge back, and
    // editing a cell main already changed is no longer a conflict.
    assert!(branch::diff(&main, &br).await.unwrap().is_empty());
    br.execute("UPDATE docs SET body = 'on top of main' WHERE id = 1", ())
        .await
        .unwrap();
    let diff = branch::diff(&main, &br).await.unwrap();
    assert_eq!(diff.rows.len(), 1);
    assert!(diff.conflicts.is_empty());
    assert_eq!(
        diff.rows[0].columns[0].on_main,
        Some(Value::Text("newer on main".into()))
    );
}

#[tokio::test]
async fn misuse_is_refused() {
    let Some(ext) = extension() else { return };
    let main = main_db(&ext).await;

    // Forking into a database that already has tables.
    let busy = main_db(&ext).await;
    assert!(branch::fork(&main, &busy).await.is_err());

    // Diffing something that is not a branch.
    let plain = open(&ext).await;
    assert!(branch::diff(&main, &plain).await.is_err());

    // Diffing or merging a branch against a database it was not forked from.
    let br = open(&ext).await;
    branch::fork(&main, &br).await.unwrap();
    let stranger = main_db(&ext).await;
    assert!(branch::diff(&stranger, &br).await.is_err());
    assert!(branch::merge(&stranger, &br).await.is_err());
}
