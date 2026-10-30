//! One sqld per data directory: a second server on the same path is refused
//! while the first serves, and the path is free again once it stops.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use sqlanywhere::Database;
use tempfile::tempdir;
use tokio::sync::Notify;

use crate::common::net::{SimServer, TestServer, TurmoilConnector};

fn server(path: &Path) -> TestServer {
    TestServer {
        path: path.into(),
        ..Default::default()
    }
}

#[test]
fn a_second_server_on_the_same_data_directory_is_refused() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(1000))
        .build();
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("data.sqld");
    let stop = Arc::new(Notify::new());

    {
        let (path, stop) = (path.clone(), stop.clone());
        sim.host("primary", move || {
            let (path, stop) = (path.clone(), stop.clone());
            async move {
                // Serve, stop when told (dropping the server, as a crash
                // would), then serve the same directory again.
                tokio::select! {
                    res = server(&path).start_sim(8080) => res?,
                    _ = stop.notified() => (),
                }
                server(&path).start_sim(8080).await?;
                Ok(())
            }
        });
    }

    sim.client("test", async move {
        let db = Database::open_remote_with_connector("http://primary:8080", "", TurmoilConnector)?;
        let conn = db.connect()?;
        conn.execute("create table t (x)", ()).await?;

        let err = server(&path).start_sim(8081).await.unwrap_err();
        let err = format!("{err:#}");
        assert!(
            err.contains(&format!("in use by process {}", std::process::id())),
            "{err}"
        );

        // The first server never noticed.
        conn.execute("insert into t values (1)", ()).await?;

        // Stopped, it leaves the directory to whoever comes next.
        stop.notify_one();
        let mut tries = 0;
        let count = loop {
            let db =
                Database::open_remote_with_connector("http://primary:8080", "", TurmoilConnector)?;
            match db.connect()?.query("select count(*) from t", ()).await {
                Ok(mut rows) => break rows.next().await?.unwrap().get::<i64>(0)?,
                Err(_) if tries < 50 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => return Err(e.into()),
            }
        };
        assert_eq!(count, 1);

        Ok(())
    });

    sim.run().unwrap();
}
