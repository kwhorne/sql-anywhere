#![allow(deprecated)]

mod dumps;
mod meta;
mod shared_schema;

use std::path::PathBuf;
use std::time::Duration;

use crate::common::http::Client;
use crate::common::net::{init_tracing, SimServer, TestServer, TurmoilAcceptor, TurmoilConnector};
use serde_json::json;
use sqlanywhere::{Database, Value};
use sqlanywhere_server::config::{AdminApiConfig, RpcServerConfig, UserApiConfig};
use tempfile::tempdir;
use turmoil::{Builder, Sim};

fn make_primary(sim: &mut Sim, path: PathBuf) {
    init_tracing();
    sim.host("primary", move || {
        let path = path.clone();
        async move {
            let server = TestServer {
                path: path.into(),
                user_api_config: UserApiConfig {
                    ..Default::default()
                },
                admin_api_config: Some(AdminApiConfig {
                    acceptor: TurmoilAcceptor::bind(([0, 0, 0, 0], 9090)).await?,
                    connector: TurmoilConnector,
                    disable_metrics: true,
                    auth_key: None,
                }),
                rpc_server_config: Some(RpcServerConfig {
                    acceptor: TurmoilAcceptor::bind(([0, 0, 0, 0], 4567)).await?,
                    tls_config: None,
                }),
                disable_namespaces: false,
                disable_default_namespace: true,
                ..Default::default()
            };

            server.start_sim(8080).await?;

            Ok(())
        }
    });
}

#[test]
fn fork_namespace() {
    let mut sim = Builder::new()
        .simulation_duration(Duration::from_secs(1000))
        .build();
    let tmp = tempdir().unwrap();
    make_primary(&mut sim, tmp.path().to_path_buf());

    sim.client("client", async {
        let client = Client::new();
        client
            .post("http://primary:9090/v1/namespaces/foo/create", json!({}))
            .await?;

        let foo =
            Database::open_remote_with_connector("http://foo.primary:8080", "", TurmoilConnector)?;
        let foo_conn = foo.connect()?;

        foo_conn.execute("create table test (c)", ()).await?;
        foo_conn.execute("insert into test values (42)", ()).await?;

        client
            .post("http://primary:9090/v1/namespaces/foo/fork/bar", ())
            .await?;

        let bar =
            Database::open_remote_with_connector("http://bar.primary:8080", "", TurmoilConnector)?;
        let bar_conn = bar.connect()?;

        // what's in foo is in bar as well
        let mut rows = bar_conn.query("select count(*) from test", ()).await?;
        assert!(matches!(
            rows.next().await.unwrap().unwrap().get_value(0).unwrap(),
            Value::Integer(1)
        ));

        bar_conn.execute("insert into test values (42)", ()).await?;

        // add something to bar
        let mut rows = bar_conn.query("select count(*) from test", ()).await?;
        assert!(matches!(
            rows.next().await.unwrap().unwrap().get_value(0)?,
            Value::Integer(2)
        ));

        // ... and make sure it doesn't exist in foo
        let mut rows = foo_conn.query("select count(*) from test", ()).await?;
        assert!(matches!(
            rows.next().await.unwrap().unwrap().get_value(0)?,
            Value::Integer(1)
        ));

        Ok(())
    });

    sim.run().unwrap();
}

#[test]
fn delete_namespace() {
    let mut sim = Builder::new()
        .simulation_duration(Duration::from_secs(1000))
        .build();
    let tmp = tempdir().unwrap();
    make_primary(&mut sim, tmp.path().to_path_buf());

    sim.client("client", async {
        let client = Client::new();
        client
            .post("http://primary:9090/v1/namespaces/foo/create", json!({}))
            .await?;

        let foo =
            Database::open_remote_with_connector("http://foo.primary:8080", "", TurmoilConnector)?;
        let foo_conn = foo.connect()?;
        foo_conn.execute("create table test (c)", ()).await?;

        client
            .delete("http://primary:9090/v1/namespaces/foo", json!({}))
            .await
            .unwrap();
        // namespace doesn't exist anymore
        let res = foo_conn.execute("create table test (c)", ()).await;
        assert!(res.is_err());

        Ok(())
    });

    sim.run().unwrap();
}

/// `POST /v1/namespaces/:ns/branch/:name` makes a branch and its base, and
/// `merge3` merges the branch back into the live namespace.
#[test]
fn branch_a_namespace_and_merge_it_back() {
    use sqlanywhere::merge3::{Merge3, Merged};

    let mut sim = Builder::new()
        .simulation_duration(Duration::from_secs(1000))
        .build();
    let tmp = tempdir().unwrap();
    make_primary(&mut sim, tmp.path().to_path_buf());

    sim.client("client", async {
        let client = Client::new();
        client
            .post("http://primary:9090/v1/namespaces/docs/create", json!({}))
            .await?;
        let open = |ns: &str| {
            Database::open_remote_with_connector(
                format!("http://{ns}.primary:8080"),
                "",
                TurmoilConnector,
            )
            .unwrap()
            .connect()
            .unwrap()
        };
        let main = open("docs");
        main.execute_batch(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, body TEXT);
             INSERT INTO docs VALUES (1, 'one', 'a'), (2, 'two', 'b'), (3, 'three', 'c');",
        )
        .await?;

        let resp = client
            .post("http://primary:9090/v1/namespaces/docs/branch/agent", ())
            .await?
            .json_value()
            .await?;
        assert_eq!(resp, json!({ "branch": "agent", "base": "agent@base" }));

        // An agent works on the branch while main moves on.
        let branch = open("agent");
        branch
            .execute_batch(
                "INSERT INTO docs VALUES (10, 'ten', 'x');
                 UPDATE docs SET title = 'ONE' WHERE id = 1;
                 DELETE FROM docs WHERE id = 2;",
            )
            .await?;
        main.execute("UPDATE docs SET body = 'A' WHERE id = 1", ())
            .await?;
        main.execute("INSERT INTO docs VALUES (20, 'twenty', 'y')", ())
            .await?;

        let merge = Merge3::new("agent@base", "agent");
        let plan = merge.diff(&main).await?;
        assert_eq!((plan.changes(), plan.conflicts()), (3, 0), "{plan:?}");
        let Merged::Applied(_) = merge.merge(&main).await? else {
            panic!("refused");
        };

        let mut rows = main
            .query("SELECT id, title, body FROM docs ORDER BY id", ())
            .await?;
        let mut got = Vec::new();
        while let Some(row) = rows.next().await? {
            got.push((
                row.get::<i64>(0)?,
                row.get::<String>(1)?,
                row.get::<String>(2)?,
            ));
        }
        let want = [
            (1, "ONE", "A"),
            (3, "three", "c"),
            (10, "ten", "x"),
            (20, "twenty", "y"),
        ];
        assert_eq!(got, want.map(|(i, t, b)| (i, t.to_string(), b.to_string())));

        // A conflict refuses the merge and changes nothing.
        branch
            .execute("UPDATE docs SET body = 'branch' WHERE id = 3", ())
            .await?;
        main.execute("UPDATE docs SET body = 'main' WHERE id = 3", ())
            .await?;
        let Merged::Refused(plan) = merge.merge(&main).await? else {
            panic!("applied a conflict");
        };
        assert_eq!(plan.conflicts(), 1);
        let mut rows = main.query("SELECT body FROM docs WHERE id = 3", ()).await?;
        assert_eq!(rows.next().await?.unwrap().get::<String>(0)?, "main");

        // A name in use cannot be branched to again.
        let err = client
            .post("http://primary:9090/v1/namespaces/docs/branch/agent", ())
            .await;
        assert!(
            err.is_err() || !err.unwrap().status().is_success(),
            "branching twice"
        );

        Ok(())
    });

    sim.run().unwrap();
}
