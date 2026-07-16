//! Storage primitives as chapters of SQL Anywhere, not separate products:
//! a KV cache with TTL, a durable work queue, and pub/sub — each just a table
//! (and, for pub/sub across nodes, the replication log you already have).
//!
//! ```sh
//! cargo run -p sqlanywhere --example storage_primitives
//! ```

use sqlanywhere::{Builder, Connection};

async fn exec(conn: &Connection, sql: &str) {
    conn.execute(sql, ()).await.unwrap();
}

async fn dump(conn: &Connection, label: &str, sql: &str) {
    let mut rows = conn.query(sql, ()).await.unwrap();
    print!("  {label}:");
    let mut any = false;
    while let Some(row) = rows.next().await.unwrap() {
        any = true;
        let n = row.column_count();
        let cols: Vec<String> = (0..n)
            .map(|i| format!("{:?}", row.get_value(i).unwrap()))
            .collect();
        print!(" [{}]", cols.join(", "));
    }
    if !any {
        print!(" (none)");
    }
    println!();
}

#[tokio::main]
async fn main() {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();

    // 1. KV cache with TTL — a table with an expiry column + a filtering view.
    println!("1. KV cache with TTL");
    exec(
        &conn,
        "CREATE TABLE kv(key TEXT PRIMARY KEY, value TEXT NOT NULL, expires_at INTEGER)",
    )
    .await;
    exec(
        &conn,
        "CREATE VIEW kv_live AS SELECT key, value FROM kv \
         WHERE expires_at IS NULL OR expires_at > unixepoch()",
    )
    .await;
    exec(
        &conn,
        "INSERT OR REPLACE INTO kv VALUES ('session','alice', unixepoch()+3600)",
    )
    .await;
    exec(
        &conn,
        "INSERT OR REPLACE INTO kv VALUES ('flash','boom', unixepoch()-1)",
    )
    .await;
    dump(
        &conn,
        "live keys (expired hidden lazily)",
        "SELECT key FROM kv_live ORDER BY key",
    )
    .await;
    exec(&conn, "DELETE FROM kv WHERE expires_at <= unixepoch()").await; // periodic sweep
    dump(
        &conn,
        "physical keys after sweep",
        "SELECT key FROM kv ORDER BY key",
    )
    .await;

    // 2. Durable queue — a table + an atomic UPDATE … RETURNING claim.
    println!("\n2. Durable work queue (visibility timeout, at-least-once)");
    exec(
        &conn,
        "CREATE TABLE jobs(id INTEGER PRIMARY KEY, payload TEXT, done INTEGER DEFAULT 0, \
         locked_until INTEGER, attempts INTEGER DEFAULT 0)",
    )
    .await;
    exec(
        &conn,
        "INSERT INTO jobs(payload) VALUES ('email A'),('email B')",
    )
    .await;
    let claim = "UPDATE jobs SET locked_until = unixepoch()+30, attempts = attempts+1 \
         WHERE id = (SELECT id FROM jobs WHERE done=0 \
                     AND (locked_until IS NULL OR locked_until <= unixepoch()) \
                     ORDER BY id LIMIT 1) \
         RETURNING id, payload, attempts";
    dump(&conn, "worker 1 claims", claim).await;
    dump(&conn, "worker 2 claims (different job)", claim).await;
    exec(
        &conn,
        "UPDATE jobs SET locked_until = unixepoch()-1 WHERE payload='email B'",
    )
    .await; // crash
    dump(&conn, "after B's lock expires, it is redelivered", claim).await;

    // 3. Pub/sub — an append-only topic tailed by cursor. Across nodes the
    //    replication log ships these rows to every embedded replica.
    println!("\n3. Pub/sub via an append-only topic");
    exec(
        &conn,
        "CREATE TABLE events(seq INTEGER PRIMARY KEY, channel TEXT, payload TEXT)",
    )
    .await;
    exec(
        &conn,
        "INSERT INTO events(channel,payload) VALUES ('orders','o1'),('orders','o2')",
    )
    .await;
    dump(
        &conn,
        "subscriber from cursor 0",
        "SELECT seq,payload FROM events WHERE channel='orders' AND seq>0 ORDER BY seq",
    )
    .await;
    exec(
        &conn,
        "INSERT INTO events(channel,payload) VALUES ('orders','o3')",
    )
    .await; // publish more
    dump(
        &conn,
        "subscriber tails from cursor 2",
        "SELECT seq,payload FROM events WHERE channel='orders' AND seq>2 ORDER BY seq",
    )
    .await;
}
