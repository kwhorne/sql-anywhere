//! Live queries over a connection to `sqld`: the server announces commits and
//! `LiveQuery::wait` returns the diffs they make.

use std::sync::Arc;
use std::time::Duration;

use sqlanywhere::live::{self, LiveQuery};
use sqlanywhere::{Database, Value};
use tokio::sync::Notify;
use tokio::time::{sleep, timeout};

use super::make_standalone_server;
use crate::common::net::TurmoilConnector;

/// Long enough for any announcement in the simulation; a wait that takes this
/// long has missed one.
const PATIENCE: Duration = Duration::from_secs(30);

fn remote() -> Database {
    Database::open_remote_with_connector("http://primary:8080", "", TurmoilConnector).unwrap()
}

#[test]
fn wait_returns_the_diff_of_another_clients_commit() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(1000))
        .build();
    sim.host("primary", make_standalone_server);
    let ready = Arc::new(Notify::new());

    sim.client("watcher", {
        let ready = ready.clone();
        async move {
            let conn = remote().connect()?;
            conn.execute(
                "CREATE TABLE todo (id INTEGER PRIMARY KEY, title TEXT, done INTEGER)",
                (),
            )
            .await?;
            conn.execute("CREATE TABLE other (x)", ()).await?;
            live::track(&conn, "todo").await?;
            live::track(&conn, "other").await?;

            let mut open = LiveQuery::new(&conn, "SELECT id, title FROM todo WHERE done = 0", ())
                .await?
                .keyed_by(&[0]);
            assert_eq!(open.watched_tables(), Some(&["todo".to_string()][..]));
            ready.notify_one();

            // The writer first commits to `other` and rolls a transaction on
            // `todo` back; neither changes the result, so this is the insert.
            let diff = timeout(PATIENCE, open.wait()).await??;
            assert_eq!(
                diff.inserted,
                vec![vec![Value::Integer(1), Value::Text("write docs".into())]]
            );
            assert!(diff.updated.is_empty() && diff.deleted.is_empty());

            // Two rows in one transaction arrive together.
            let diff = timeout(PATIENCE, open.wait()).await??;
            assert_eq!(diff.inserted.len(), 2, "{diff:?}");

            let diff = timeout(PATIENCE, open.wait()).await??;
            assert_eq!(
                diff.updated,
                vec![(
                    vec![Value::Integer(1), Value::Text("write docs".into())],
                    vec![Value::Integer(1), Value::Text("write the docs".into())]
                )]
            );

            // Marking a row done takes it out of the result.
            let diff = timeout(PATIENCE, open.wait()).await??;
            assert_eq!(diff.deleted.len(), 1, "{diff:?}");

            // A DELETE without a WHERE.
            let diff = timeout(PATIENCE, open.wait()).await??;
            assert_eq!(diff.deleted.len(), 2, "{diff:?}");
            assert!(open.rows().is_empty());

            Ok(())
        }
    });

    sim.client("writer", async move {
        ready.notified().await;
        let conn = remote().connect()?;
        // Let the watcher be waiting, so what follows is announced rather
        // than found by its first look.
        sleep(Duration::from_secs(2)).await;
        conn.execute("INSERT INTO other VALUES (1)", ()).await?;
        let tx = conn.transaction().await?;
        tx.execute("INSERT INTO todo VALUES (9, 'never', 0)", ())
            .await?;
        tx.rollback().await?;
        sleep(Duration::from_secs(1)).await;
        conn.execute("INSERT INTO todo VALUES (1, 'write docs', 0)", ())
            .await?;
        sleep(Duration::from_secs(1)).await;
        let tx = conn.transaction().await?;
        tx.execute("INSERT INTO todo VALUES (2, 'test', 0)", ())
            .await?;
        tx.execute("INSERT INTO todo VALUES (3, 'ship', 0)", ())
            .await?;
        tx.commit().await?;
        sleep(Duration::from_secs(1)).await;
        conn.execute("UPDATE todo SET title = 'write the docs' WHERE id = 1", ())
            .await?;
        sleep(Duration::from_secs(1)).await;
        conn.execute("UPDATE todo SET done = 1 WHERE id = 3", ())
            .await?;
        sleep(Duration::from_secs(1)).await;
        conn.execute("DELETE FROM todo", ()).await?;
        Ok(())
    });

    sim.run().unwrap();
}

#[test]
fn wait_catches_a_commit_made_before_it() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(1000))
        .build();
    sim.host("primary", make_standalone_server);
    sim.client("client", async {
        let db = remote();
        let conn = db.connect()?;
        let other = db.connect()?;
        conn.execute("CREATE TABLE t (x INTEGER PRIMARY KEY)", ())
            .await?;
        live::track(&conn, "t").await?;
        let mut all = LiveQuery::new(&conn, "SELECT x FROM t", ()).await?;

        // Committed before anyone listens: the first wait finds it.
        other.execute("INSERT INTO t VALUES (1)", ()).await?;
        let diff = timeout(PATIENCE, all.wait()).await??;
        assert_eq!(diff.inserted, vec![vec![Value::Integer(1)]]);

        // A wait given up on leaves the listener; the commit made meanwhile
        // is announced to the next one.
        assert!(timeout(Duration::from_secs(1), all.wait()).await.is_err());
        other.execute("INSERT INTO t VALUES (2)", ()).await?;
        let diff = timeout(PATIENCE, all.wait()).await??;
        assert_eq!(diff.inserted, vec![vec![Value::Integer(2)]]);
        Ok(())
    });
    sim.run().unwrap();
}

#[test]
fn wait_needs_tracked_tables_and_sqld() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(1000))
        .build();
    sim.host("primary", make_standalone_server);
    sim.client("client", async {
        let conn = remote().connect()?;
        conn.execute("CREATE TABLE t (x)", ()).await?;
        let mut untracked = LiveQuery::new(&conn, "SELECT x FROM t", ()).await?;
        let err = untracked.wait().await.unwrap_err().to_string();
        assert!(err.contains("live::track"), "{err}");

        let local = sqlanywhere::Builder::new_local(":memory:")
            .build()
            .await?
            .connect()?;
        local.execute("CREATE TABLE t (x)", ()).await?;
        live::track(&local, "t").await?;
        let mut tracked = LiveQuery::new(&local, "SELECT x FROM t", ()).await?;
        let err = tracked.wait().await.unwrap_err().to_string();
        assert!(err.contains("only a connection to sqld"), "{err}");
        Ok(())
    });
    sim.run().unwrap();
}

#[test]
fn wait_sees_a_without_rowid_table() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(1000))
        .build();
    sim.host("primary", make_standalone_server);
    sim.client("client", async {
        let db = remote();
        let conn = db.connect()?;
        let other = db.connect()?;
        // SQLite's update hook says nothing about WITHOUT ROWID tables.
        conn.execute(
            "CREATE TABLE tags (name TEXT PRIMARY KEY, n INTEGER) WITHOUT ROWID",
            (),
        )
        .await?;
        live::track(&conn, "tags").await?;
        let mut tags = LiveQuery::new(&conn, "SELECT name, n FROM tags", ())
            .await?
            .keyed_by(&[0]);
        // Listening, and nothing to catch up on.
        assert!(timeout(Duration::from_secs(1), tags.wait()).await.is_err());

        other
            .execute("INSERT INTO tags VALUES ('rust', 1)", ())
            .await?;
        let diff = timeout(PATIENCE, tags.wait()).await??;
        assert_eq!(
            diff.inserted,
            vec![vec![Value::Text("rust".into()), Value::Integer(1)]]
        );
        other.execute("UPDATE tags SET n = 2", ()).await?;
        let diff = timeout(PATIENCE, tags.wait()).await??;
        assert_eq!(diff.updated.len(), 1, "{diff:?}");
        Ok(())
    });
    sim.run().unwrap();
}
