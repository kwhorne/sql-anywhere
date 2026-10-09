//! Live queries through `sqlanywhere::live`: a result that keeps itself
//! current, from writes on its own connection and from other connections.

use sqlanywhere::live::{Diff, LiveQuery};
use sqlanywhere::{params, Builder, Connection, Value};

async fn todo_db(path: &str) -> Connection {
    let db = Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS todo (id INTEGER PRIMARY KEY, title TEXT, done INTEGER)",
        (),
    )
    .await
    .unwrap();
    conn.execute("CREATE TABLE IF NOT EXISTS other (x)", ())
        .await
        .unwrap();
    conn
}

fn row(id: i64, title: &str) -> Vec<Value> {
    vec![Value::Integer(id), Value::Text(title.into())]
}

const OPEN: &str = "SELECT id, title FROM todo WHERE done = 0 ORDER BY id";

#[tokio::test]
async fn writes_on_the_same_connection_are_seen() {
    // SQLite does not bump data_version for a connection's own commits, so
    // this is the case a live query watching only that pragma would miss.
    let conn = todo_db(":memory:").await;
    conn.execute("INSERT INTO todo VALUES (1, 'one', 0), (2, 'two', 0)", ())
        .await
        .unwrap();
    let mut live = LiveQuery::new(&conn, OPEN, ())
        .await
        .unwrap()
        .keyed_by(&[0]);
    assert_eq!(live.columns(), ["id", "title"]);
    assert_eq!(live.rows(), [row(1, "one"), row(2, "two")]);
    assert_eq!(live.refresh().await.unwrap(), None);

    conn.execute("INSERT INTO todo VALUES (3, 'three', 0)", ())
        .await
        .unwrap();
    conn.execute("UPDATE todo SET title = 'TWO' WHERE id = 2", ())
        .await
        .unwrap();
    conn.execute("UPDATE todo SET done = 1 WHERE id = 1", ())
        .await
        .unwrap();
    assert!(live.is_stale().await.unwrap());
    assert_eq!(
        live.refresh().await.unwrap(),
        Some(Diff {
            inserted: vec![row(3, "three")],
            updated: vec![(row(2, "two"), row(2, "TWO"))],
            deleted: vec![row(1, "one")],
        })
    );
    assert_eq!(live.rows(), [row(2, "TWO"), row(3, "three")]);
    assert!(!live.is_stale().await.unwrap());
}

#[tokio::test]
async fn commits_from_another_connection_are_seen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("live.db");
    let path = path.to_str().unwrap();
    let reader = todo_db(path).await;
    let writer = todo_db(path).await;

    let mut live = LiveQuery::new(&reader, OPEN, ()).await.unwrap();
    assert!(live.rows().is_empty());
    assert!(!live.is_stale().await.unwrap());

    writer
        .execute("INSERT INTO todo VALUES (1, 'from elsewhere', 0)", ())
        .await
        .unwrap();
    assert!(live.is_stale().await.unwrap());
    let diff = live.refresh().await.unwrap().expect("a change");
    // Unkeyed: rows come and go as whole rows.
    assert_eq!(diff.inserted, vec![row(1, "from elsewhere")]);

    writer
        .execute("UPDATE todo SET title = 'renamed' WHERE id = 1", ())
        .await
        .unwrap();
    let diff = live.refresh().await.unwrap().expect("a change");
    assert_eq!(diff.inserted, vec![row(1, "renamed")]);
    assert_eq!(diff.deleted, vec![row(1, "from elsewhere")]);
    assert!(diff.updated.is_empty(), "updates need a key");
}

#[tokio::test]
async fn writes_that_do_not_touch_the_result_report_nothing() {
    let conn = todo_db(":memory:").await;
    conn.execute("INSERT INTO todo VALUES (1, 'one', 0)", ())
        .await
        .unwrap();
    let mut live = LiveQuery::new(&conn, OPEN, ()).await.unwrap();

    // Another table, a row the filter excludes, and a write that is undone.
    conn.execute("INSERT INTO other VALUES (1)", ())
        .await
        .unwrap();
    conn.execute("INSERT INTO todo VALUES (2, 'finished', 1)", ())
        .await
        .unwrap();
    let tx = conn.transaction().await.unwrap();
    tx.execute("DELETE FROM todo", ()).await.unwrap();
    tx.rollback().await.unwrap();

    // Something was written, so the query is re-run, but its result is the same.
    assert!(live.is_stale().await.unwrap());
    assert_eq!(live.refresh().await.unwrap(), None);
    assert!(!live.is_stale().await.unwrap());
}

#[tokio::test]
async fn parameters_are_kept_across_refreshes() {
    let conn = todo_db(":memory:").await;
    let mut live = LiveQuery::new(
        &conn,
        "SELECT id, title FROM todo WHERE done = ? ORDER BY id",
        params![1],
    )
    .await
    .unwrap()
    .keyed_by(&[0]);
    conn.execute("INSERT INTO todo VALUES (1, 'open', 0), (2, 'done', 1)", ())
        .await
        .unwrap();
    let diff = live.refresh().await.unwrap().expect("a change");
    assert_eq!(diff.inserted, vec![row(2, "done")]);
}

#[tokio::test]
async fn a_bad_key_or_a_dropped_table_is_an_error() {
    let conn = todo_db(":memory:").await;
    conn.execute("INSERT INTO todo VALUES (1, 'one', 0)", ())
        .await
        .unwrap();
    let mut live = LiveQuery::new(&conn, OPEN, ())
        .await
        .unwrap()
        .keyed_by(&[7]);
    conn.execute("INSERT INTO todo VALUES (2, 'two', 0)", ())
        .await
        .unwrap();
    assert!(live.refresh().await.is_err());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drop.db");
    let path = path.to_str().unwrap();
    let reader = todo_db(path).await;
    let writer = todo_db(path).await;
    let mut live = LiveQuery::new(&reader, OPEN, ()).await.unwrap();
    writer.execute("DROP TABLE todo", ()).await.unwrap();
    assert!(live.refresh().await.is_err());
}
