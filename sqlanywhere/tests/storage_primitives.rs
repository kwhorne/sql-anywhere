//! Storage primitives are chapters of SQL Anywhere, not separate products:
//! a KV cache is a table with an expiry column, a durable queue is a table plus
//! an atomic `UPDATE … RETURNING` claim, and pub/sub is an append-only table
//! tailed by cursor (the replication log is the cross-node transport). These
//! tests assert each one composes in plain SQL.

use sqlanywhere::{Builder, Connection};

async fn conn() -> Connection {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    db.connect().unwrap()
}

async fn exec(conn: &Connection, sql: &str) {
    conn.execute(sql, ()).await.unwrap();
}

/// Run the atomic claim and return the claimed job's payload (column 1), if any.
async fn claim_payload(conn: &Connection) -> Option<String> {
    let mut rows = conn.query(CLAIM, ()).await.unwrap();
    rows.next()
        .await
        .unwrap()
        .map(|r| r.get::<String>(1).unwrap())
}

async fn strings(conn: &Connection, sql: &str) -> Vec<String> {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get::<String>(0).unwrap());
    }
    out
}

const CLAIM: &str = "UPDATE jobs SET locked_until = unixepoch() + 30, attempts = attempts + 1 \
     WHERE id = (SELECT id FROM jobs WHERE done = 0 \
                 AND (locked_until IS NULL OR locked_until <= unixepoch()) \
                 ORDER BY id LIMIT 1) \
     RETURNING id, payload, attempts";

#[tokio::test]
async fn kv_cache_with_ttl() {
    let conn = conn().await;
    exec(
        &conn,
        "CREATE TABLE kv(key TEXT PRIMARY KEY, value TEXT NOT NULL, expires_at INTEGER)",
    )
    .await;
    // A view filters expired entries lazily on read.
    exec(
        &conn,
        "CREATE VIEW kv_live AS SELECT key, value FROM kv \
         WHERE expires_at IS NULL OR expires_at > unixepoch()",
    )
    .await;

    exec(
        &conn,
        "INSERT OR REPLACE INTO kv VALUES ('live','v', unixepoch()+3600)",
    )
    .await;
    exec(
        &conn,
        "INSERT OR REPLACE INTO kv VALUES ('flash','v', unixepoch()-1)",
    )
    .await;
    exec(&conn, "INSERT OR REPLACE INTO kv VALUES ('perm','v', NULL)").await;

    // GET hides the expired key without deleting it yet.
    let live = strings(&conn, "SELECT key FROM kv_live ORDER BY key").await;
    assert_eq!(live, vec!["live", "perm"]);

    // Periodic sweep reclaims the space.
    exec(&conn, "DELETE FROM kv WHERE expires_at <= unixepoch()").await;
    let physical = strings(&conn, "SELECT key FROM kv ORDER BY key").await;
    assert_eq!(physical, vec!["live", "perm"]);
}

#[tokio::test]
async fn durable_queue_at_least_once() {
    let conn = conn().await;
    exec(
        &conn,
        "CREATE TABLE jobs(id INTEGER PRIMARY KEY, payload TEXT, done INTEGER DEFAULT 0, \
         locked_until INTEGER, attempts INTEGER DEFAULT 0)",
    )
    .await;
    exec(&conn, "INSERT INTO jobs(payload) VALUES ('A'),('B'),('C')").await;

    // Two workers claim different jobs — the atomic UPDATE prevents double
    // delivery. CLAIM returns (id, payload, attempts); read the payload (col 1).
    assert_eq!(claim_payload(&conn).await, Some("A".to_string()));
    assert_eq!(claim_payload(&conn).await, Some("B".to_string()));

    // Worker 1 acks its job.
    exec(&conn, "UPDATE jobs SET done = 1 WHERE payload = 'A'").await;

    // Worker 2 "crashes": its lock expires and the job becomes visible again.
    exec(
        &conn,
        "UPDATE jobs SET locked_until = unixepoch() - 1 WHERE payload = 'B'",
    )
    .await;

    // The next claim redelivers B (at-least-once), now on its 2nd attempt.
    let mut rows = conn.query(CLAIM, ()).await.unwrap();
    let row = rows.next().await.unwrap().expect("a job is available");
    assert_eq!(row.get::<String>(1).unwrap(), "B");
    assert_eq!(row.get::<i64>(2).unwrap(), 2, "redelivery bumps attempts");
}

#[tokio::test]
async fn pubsub_append_only_tail() {
    let conn = conn().await;
    exec(
        &conn,
        "CREATE TABLE events(seq INTEGER PRIMARY KEY, channel TEXT, payload TEXT)",
    )
    .await;
    // Publish = INSERT.
    exec(
        &conn,
        "INSERT INTO events(channel,payload) VALUES \
         ('orders','o1'),('orders','o2'),('audit','login')",
    )
    .await;

    // Subscriber tails its channel from a cursor.
    let first = strings(
        &conn,
        "SELECT payload FROM events WHERE channel='orders' AND seq > 0 ORDER BY seq",
    )
    .await;
    assert_eq!(first, vec!["o1", "o2"]);

    // More published; subscriber advances its cursor and sees only new messages.
    exec(
        &conn,
        "INSERT INTO events(channel,payload) VALUES ('orders','o3')",
    )
    .await;
    let tail = strings(
        &conn,
        "SELECT payload FROM events WHERE channel='orders' AND seq > 2 ORDER BY seq",
    )
    .await;
    assert_eq!(tail, vec!["o3"], "only messages after the cursor");

    // Channels are isolated.
    let audit = strings(
        &conn,
        "SELECT payload FROM events WHERE channel='audit' ORDER BY seq",
    )
    .await;
    assert_eq!(audit, vec!["login"]);
}
