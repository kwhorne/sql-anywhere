//! Live queries through `sqlanywhere::live`: a result that keeps itself
//! current, from writes on its own connection and from other connections.

use sqlanywhere::live::{self, Diff, LiveQuery};
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

#[tokio::test]
async fn a_tracked_table_ignores_writes_elsewhere() {
    let conn = todo_db(":memory:").await;
    live::track(&conn, "todo").await.unwrap();
    let mut q = LiveQuery::new(&conn, OPEN, ())
        .await
        .unwrap()
        .keyed_by(&[0]);
    assert_eq!(q.watched_tables(), Some(&["todo".to_string()][..]));

    conn.execute("INSERT INTO other VALUES (1)", ())
        .await
        .unwrap();
    assert!(!q.is_stale().await.unwrap());

    conn.execute("INSERT INTO todo VALUES (1, 'one', 0)", ())
        .await
        .unwrap();
    assert!(q.is_stale().await.unwrap());
    assert_eq!(
        q.refresh().await.unwrap().unwrap().inserted,
        vec![row(1, "one")]
    );

    // A write inside an open transaction counts, and so does one that is
    // rolled back: the result is re-run and found unchanged.
    let tx = conn.transaction().await.unwrap();
    tx.execute("UPDATE todo SET title = 'ONE'", ())
        .await
        .unwrap();
    assert!(q.is_stale().await.unwrap());
    tx.rollback().await.unwrap();
    assert_eq!(q.refresh().await.unwrap(), None);

    // DELETE without WHERE is the truncate optimization, which skips the
    // update hook; triggers turn it off, so the delete is still counted.
    conn.execute("DELETE FROM todo", ()).await.unwrap();
    assert_eq!(
        q.refresh().await.unwrap().unwrap().deleted,
        vec![row(1, "one")]
    );
}

#[tokio::test]
async fn the_tables_are_found_through_views_joins_and_indexes() {
    let conn = todo_db(":memory:").await;
    conn.execute_batch(
        "CREATE TABLE tag (todo INTEGER, name TEXT);
         CREATE INDEX tag_name ON tag (name, todo);
         CREATE VIEW open_todo AS SELECT id, title FROM todo WHERE done = 0;",
    )
    .await
    .unwrap();
    for t in ["todo", "tag"] {
        live::track(&conn, t).await.unwrap();
    }
    let watched = |sql: &'static str| {
        let conn = conn.clone();
        async move {
            LiveQuery::new(&conn, sql, ())
                .await
                .unwrap()
                .watched_tables()
                .map(<[String]>::to_vec)
        }
    };
    let todo = Some(vec!["todo".to_string()]);
    let both = Some(vec!["tag".to_string(), "todo".to_string()]);
    assert_eq!(watched("SELECT * FROM open_todo").await, todo);
    assert_eq!(
        watched("SELECT t.title FROM open_todo t JOIN tag ON tag.todo = t.id").await,
        both
    );
    assert_eq!(
        watched("SELECT id FROM todo WHERE id IN (SELECT todo FROM tag WHERE name = 'x')").await,
        both
    );
    // Answered from the index alone: the plan opens tag_name, not tag.
    assert_eq!(
        watched("SELECT todo FROM tag WHERE name = 'x'").await,
        Some(vec!["tag".to_string()])
    );
}

#[tokio::test]
async fn anything_that_cannot_be_traced_watches_the_whole_file() {
    let conn = todo_db(":memory:").await;
    conn.execute_batch(
        "CREATE VIRTUAL TABLE note USING fts5(body);
         CREATE TEMP TABLE scratch (x);",
    )
    .await
    .unwrap();
    live::track(&conn, "todo").await.unwrap();
    for sql in [
        // An untracked table next to a tracked one.
        "SELECT id FROM todo, other",
        // A virtual table next to a tracked one.
        "SELECT todo.id FROM todo JOIN note ON note.rowid = todo.id WHERE note MATCH 'x'",
        // The schema table, which has no entry of its own.
        "SELECT id FROM todo WHERE id > (SELECT count(*) FROM sqlite_schema)",
        // A temporary table: its root page numbers belong to another file and
        // can coincide with a tracked table's.
        "SELECT x FROM scratch",
    ] {
        let q = LiveQuery::new(&conn, sql, ()).await.unwrap();
        assert_eq!(q.watched_tables(), None, "{sql}");
    }
    assert!(live::track(&conn, "open_todo").await.is_err());
    assert!(live::track(&conn, "note").await.is_err());
}

#[tokio::test]
async fn tracking_is_picked_up_and_dropped_by_a_running_query() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("track.db");
    let path = path.to_str().unwrap();
    let reader = todo_db(path).await;
    let writer = todo_db(path).await;
    let mut q = LiveQuery::new(&reader, OPEN, ()).await.unwrap();
    assert_eq!(q.watched_tables(), None);

    // Tracked from another connection: the schema moved, so the next refresh
    // works out the tables again.
    live::track(&writer, "TODO").await.unwrap();
    assert!(q.is_stale().await.unwrap());
    assert_eq!(q.refresh().await.unwrap(), None);
    assert_eq!(q.watched_tables(), Some(&["todo".to_string()][..]));

    writer
        .execute("INSERT INTO other VALUES (1)", ())
        .await
        .unwrap();
    assert!(!q.is_stale().await.unwrap());
    writer
        .execute("INSERT INTO todo VALUES (1, 'one', 0)", ())
        .await
        .unwrap();
    assert_eq!(
        q.refresh().await.unwrap().unwrap().inserted,
        vec![row(1, "one")]
    );

    live::untrack(&writer, "todo").await.unwrap();
    assert_eq!(q.refresh().await.unwrap(), None);
    assert_eq!(q.watched_tables(), None);
    writer
        .execute("INSERT INTO other VALUES (2)", ())
        .await
        .unwrap();
    assert!(q.is_stale().await.unwrap());
}

#[tokio::test]
async fn a_schema_change_on_the_same_connection_is_seen() {
    // Neither data_version nor total_changes moves for this connection's own
    // DDL; schema_version does.
    let conn = todo_db(":memory:").await;
    let mut q = LiveQuery::new(&conn, OPEN, ()).await.unwrap();
    conn.execute("DROP TABLE todo", ()).await.unwrap();
    assert!(q.is_stale().await.unwrap());
    assert!(q.refresh().await.is_err());
}

#[tokio::test]
async fn a_table_dropped_and_created_again_is_no_longer_tracked() {
    // DROP TABLE takes the triggers with it; the version row stays. Trusting
    // the row alone would watch versions nothing bumps any more.
    let conn = todo_db(":memory:").await;
    live::track(&conn, "todo").await.unwrap();
    conn.execute_batch(
        "DROP TABLE todo;
         CREATE TABLE todo (id INTEGER PRIMARY KEY, title TEXT, done INTEGER);",
    )
    .await
    .unwrap();
    let mut q = LiveQuery::new(&conn, OPEN, ()).await.unwrap();
    assert_eq!(q.watched_tables(), None);
    conn.execute("INSERT INTO todo VALUES (1, 'one', 0)", ())
        .await
        .unwrap();
    assert_eq!(
        q.refresh().await.unwrap().unwrap().inserted,
        vec![row(1, "one")]
    );

    assert!(live::track(&conn, "missing").await.is_err());
}
