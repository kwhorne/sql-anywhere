//! The generated contract API (`sqlanywhere::contracts`), called the way a
//! runtime calls it. The SQL behind every function is proved statement by
//! statement in `contract_conformance.rs`; this shows the generated layer binds
//! the right parameters, wraps the right statements in a transaction, and
//! returns what each annotation promises.

use sqlanywhere::contracts::{cache, pubsub, queue};
use sqlanywhere::{Builder, Connection};

async fn db() -> Connection {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    queue::install(&conn).await.unwrap();
    cache::install(&conn).await.unwrap();
    pubsub::install(&conn).await.unwrap();
    // Installing twice is harmless: every schema statement is IF NOT EXISTS.
    queue::install(&conn).await.unwrap();
    conn
}

async fn count(conn: &Connection, table: &str) -> i64 {
    let mut rows = conn
        .query(&format!("SELECT count(*) FROM {table}"), ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

#[tokio::test]
async fn queue_round_trip() {
    let conn = db().await;
    assert_eq!(queue::VERSION, 1);

    let id: i64 = queue::enqueue(&conn, "mail", b"job-1", 0, 0, 3)
        .await
        .unwrap()
        .get(0)
        .unwrap();
    let backlog = queue::backlog(&conn, "mail").await.unwrap();
    assert_eq!(backlog.get::<i64>(1).unwrap(), 1, "one ready job");

    let claimed = queue::claim(&conn, 30, "mail").await.unwrap().unwrap();
    assert_eq!(claimed.get::<i64>(0).unwrap(), id);
    assert_eq!(claimed.get::<Vec<u8>>(1).unwrap(), b"job-1");
    assert!(queue::claim(&conn, 30, "mail").await.unwrap().is_none());
    assert_eq!(queue::renew(&conn, 60, id).await.unwrap(), 1);

    assert_eq!(queue::release(&conn, 0, id).await.unwrap(), 1);
    assert!(queue::claim(&conn, 30, "mail").await.unwrap().is_some());
    assert_eq!(queue::ack(&conn, id).await.unwrap(), 1);
    assert_eq!(count(&conn, "askr_jobs").await, 0);

    // The dead-letter move is two statements in one IMMEDIATE transaction.
    let id: i64 = queue::enqueue(&conn, "mail", b"job-2", 0, 0, 1)
        .await
        .unwrap()
        .get(0)
        .unwrap();
    let changed = queue::dead_letter(&conn, Some("uuid-2"), Some("boom"), id)
        .await
        .unwrap();
    assert_eq!(changed, 2, "one row copied, one deleted");
    assert_eq!(count(&conn, "askr_jobs").await, 0);
    let mut rows = conn
        .query("SELECT uuid, exception FROM askr_failed_jobs", ())
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "uuid-2");
    assert_eq!(row.get::<String>(1).unwrap(), "boom");

    // Nullable parameters bind NULL.
    let id: i64 = queue::enqueue(&conn, "mail", b"job-3", 0, 0, 1)
        .await
        .unwrap()
        .get(0)
        .unwrap();
    queue::dead_letter(&conn, None, None, id).await.unwrap();
    assert_eq!(
        count(
            &conn,
            "askr_failed_jobs WHERE uuid IS NULL AND exception IS NULL"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn cache_round_trip() {
    let conn = db().await;

    assert_eq!(cache::set(&conn, "a", b"1", None).await.unwrap(), 1);
    let got = cache::get(&conn, "a").await.unwrap().unwrap();
    assert_eq!(got.get::<Vec<u8>>(0).unwrap(), b"1");
    assert!(cache::get(&conn, "missing").await.unwrap().is_none());

    // An entry that has expired is not served.
    cache::set(&conn, "old", b"x", Some(1)).await.unwrap();
    assert!(cache::get(&conn, "old").await.unwrap().is_none());
    assert_eq!(cache::sweep(&conn).await.unwrap(), 1);

    let n: i64 = cache::increment(&conn, "hits", 5, None)
        .await
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(n, 5);
    let n: i64 = cache::increment(&conn, "hits", -2, None)
        .await
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(n, 3);

    // A lock is held when a row comes back, and only its owner releases it.
    assert!(cache::lock(&conn, "lock:x", "me", 60)
        .await
        .unwrap()
        .is_some());
    assert!(cache::lock(&conn, "lock:x", "you", 60)
        .await
        .unwrap()
        .is_none());
    assert_eq!(cache::unlock(&conn, "lock:x", "you").await.unwrap(), 0);
    assert_eq!(cache::unlock(&conn, "lock:x", "me").await.unwrap(), 1);

    // Invalidating a tag is two deletes in one transaction.
    cache::set(&conn, "t1", b"v", None).await.unwrap();
    cache::set(&conn, "t2", b"v", None).await.unwrap();
    cache::tag(&conn, "users", "t1").await.unwrap();
    cache::tag(&conn, "users", "t2").await.unwrap();
    assert_eq!(cache::invalidate_tag(&conn, "users").await.unwrap(), 4);
    assert!(cache::get(&conn, "t1").await.unwrap().is_none());
    assert!(cache::get(&conn, "a").await.unwrap().is_some());

    assert_eq!(cache::forget(&conn, "a").await.unwrap(), 1);
    assert!(cache::flush(&conn).await.unwrap() >= 1);
    assert_eq!(count(&conn, "askr_cache").await, 0);
}

#[tokio::test]
async fn pubsub_round_trip() {
    let conn = db().await;
    let mut last = 0;
    for body in [b"e1", b"e2", b"e3"] {
        last = pubsub::publish(&conn, "news", body)
            .await
            .unwrap()
            .get(0)
            .unwrap();
    }
    pubsub::publish(&conn, "other", b"x").await.unwrap();

    let mut rows = pubsub::subscribe(&conn, "news", 0, 2).await.unwrap();
    let mut seen = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        seen.push(row.get::<Vec<u8>>(1).unwrap());
    }
    assert_eq!(seen, vec![b"e1".to_vec(), b"e2".to_vec()]);

    assert_eq!(pubsub::save_cursor(&conn, "worker", last).await.unwrap(), 1);
    assert_eq!(pubsub::trim_to_last(&conn, 1).await.unwrap(), 3);
    assert_eq!(pubsub::trim_by_age(&conn, 3600).await.unwrap(), 0);
    assert_eq!(count(&conn, "askr_events").await, 1);
}
