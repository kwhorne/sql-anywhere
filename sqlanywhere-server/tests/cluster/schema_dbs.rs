use std::time::Duration;

use serde_json::json;
use sqlanywhere::Database;
use turmoil::Builder;

use crate::common::http::Client;
use crate::common::net::TurmoilConnector;

use super::make_cluster;

/// Read a count from a connection that has just written to the same table.
///
/// `docs/CONSISTENCY_MODEL.md` states that "sqld guarantees that a process
/// (connection) will always see its write", so these reads are entitled to see
/// the row. When one comes up short there are two very different explanations
/// and the bare `assert_eq!` could not tell them apart, which is why this test
/// failing in CI was never actionable: the write may have been lost, or the
/// read may have been served by a replica that had not caught up.
///
/// Sampling again for a moment answers it. If the count reaches the expected
/// value shortly after, the write landed and the read was stale, which is the
/// guarantee being violated. If it stays put, the write never arrived.
async fn count(conn: &sqlanywhere::Connection, table: &str) -> u64 {
    conn.query(&format!("select count(*) from {table}"), ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<u64>(0)
        .unwrap()
}

async fn assert_count_after_write(conn: &sqlanywhere::Connection, table: &str, expected: u64) {
    let seen = count(conn, table).await;
    if seen == expected {
        return;
    }

    let mut samples = Vec::new();
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        samples.push(count(conn, table).await);
    }

    let verdict = if samples.iter().any(|c| *c == expected) {
        "the count reached the expected value on its own, so the write had landed \
         and this read was served by a replica that had not caught up. That is \
         read-your-writes being violated"
    } else {
        "the count never moved, so either the write did not arrive at all or this \
         expectation is wrong"
    };

    panic!(
        "count(*) from {table} was {seen}, expected {expected}, on the connection \
         that had just written to it.\n\
         Counts sampled over the next second: {samples:?}\n\
         Reading of that: {verdict}."
    );
}

#[test]
fn schema_migration_basics() {
    let mut sim = Builder::new()
        .simulation_duration(Duration::from_secs(1000))
        .build();
    make_cluster(&mut sim, 1, true);

    sim.client("client", async {
        let http = Client::new();

        assert!(http
            .post(
                "http://primary:9090/v1/namespaces/schema/create",
                json!({ "shared_schema": true })
            )
            .await
            .unwrap()
            .status()
            .is_success());
        assert!(http
            .post(
                "http://primary:9090/v1/namespaces/foo/create",
                json!({ "shared_schema_name": "schema" })
            )
            .await
            .unwrap()
            .status()
            .is_success());

        {
            let db = Database::open_remote_with_connector(
                "http://schema.primary:8080",
                "",
                TurmoilConnector,
            )
            .unwrap();
            let conn = db.connect().unwrap();
            conn.execute("create table test (x)", ()).await.unwrap();
        }

        {
            let db = Database::open_remote_with_connector(
                "http://foo.primary:8080",
                "",
                TurmoilConnector,
            )
            .unwrap();
            let conn = db.connect().unwrap();
            conn.execute("insert into test values (42)", ())
                .await
                .unwrap();

            assert_count_after_write(&conn, "test", 1).await;
        }

        {
            let db = Database::open_remote_with_connector(
                "http://schema.replica0:8080",
                "",
                TurmoilConnector,
            )
            .unwrap();
            let conn = db.connect().unwrap();
            conn.execute("create table test2 (x)", ()).await.unwrap();
        }

        {
            let db = Database::open_remote_with_connector(
                "http://foo.replica0:8080",
                "",
                TurmoilConnector,
            )
            .unwrap();
            let conn = db.connect().unwrap();
            conn.execute("insert into test values (42)", ())
                .await
                .unwrap();

            assert_count_after_write(&conn, "test", 2).await;
            // Not a read-your-writes case: test2 was created on the schema
            // namespace, and this checks it arrived empty here.
            assert_eq!(count(&conn, "test2").await, 0);
        }

        Ok(())
    });

    sim.run().unwrap();
}

#[test]
fn schema_migration_via_replica() {
    let mut sim = Builder::new()
        .simulation_duration(Duration::from_secs(1000))
        .build();
    make_cluster(&mut sim, 1, true);

    sim.client("client", async {
        let http = Client::new();

        assert!(http
            .post(
                "http://primary:9090/v1/namespaces/schema/create",
                json!({ "shared_schema": true })
            )
            .await
            .unwrap()
            .status()
            .is_success());
        assert!(http
            .post(
                "http://primary:9090/v1/namespaces/foo/create",
                json!({ "shared_schema_name": "schema" })
            )
            .await
            .unwrap()
            .status()
            .is_success());

        {
            let db = Database::open_remote_with_connector(
                "http://schema.primary:8080",
                "",
                TurmoilConnector,
            )
            .unwrap();
            let conn = db.connect().unwrap();
            conn.execute("create table test (x)", ()).await.unwrap();
        }

        {
            let db = Database::open_remote_with_connector(
                "http://schema.replica0:8080",
                "",
                TurmoilConnector,
            )
            .unwrap();
            let conn = db.connect().unwrap();
            conn.execute("select * from sqlite_master;", ())
                .await
                .unwrap();

            conn.execute("create table foo (x)", ()).await.unwrap();
        }

        Ok(())
    });

    sim.run().unwrap();
}
