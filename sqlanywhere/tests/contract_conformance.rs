//! Conformance tests for the storage-primitive **contracts** in
//! `docs/contracts/` — the substrate half of the Redis-free stack (epic
//! elyra-2) that the Askr runtime builds against.
//!
//! Each test runs the *exact* SQL from a contract and asserts the documented
//! semantics, so the contracts are an executable, CI-verified spec that cannot
//! silently drift from what Askr's L2 drivers implement. Time-dependent cases
//! (delay, visibility timeout, expiry) are made deterministic by writing the
//! relevant timestamp columns directly instead of sleeping.
//!
//! Contracts: `docs/contracts/QUEUE_CONTRACT.md`, `CACHE_CONTRACT.md`,
//! `PUBSUB_CONTRACT.md`.

use std::sync::LazyLock;

use sqlanywhere::{params, params::IntoParams, Builder, Connection};

async fn conn() -> Connection {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    db.connect().unwrap()
}

async fn exec(conn: &Connection, sql: &str, p: impl IntoParams) {
    conn.execute(sql, p).await.unwrap();
}

async fn one_i64(conn: &Connection, sql: &str, p: impl IntoParams) -> Option<i64> {
    let mut rows = conn.query(sql, p).await.unwrap();
    match rows.next().await.unwrap() {
        Some(r) => Some(r.get::<i64>(0).unwrap()),
        None => None,
    }
}

async fn one_str(conn: &Connection, sql: &str, p: impl IntoParams) -> Option<String> {
    let mut rows = conn.query(sql, p).await.unwrap();
    match rows.next().await.unwrap() {
        Some(r) => Some(r.get::<String>(0).unwrap()),
        None => None,
    }
}

// ---------------------------------------------------------------------------
// Queue contract (v1)
// ---------------------------------------------------------------------------

const QUEUE_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS askr_jobs (
       id INTEGER PRIMARY KEY AUTOINCREMENT,
       queue TEXT NOT NULL DEFAULT 'default',
       payload BLOB NOT NULL,
       priority INTEGER NOT NULL DEFAULT 0,
       available_at INTEGER NOT NULL,
       reserved_until INTEGER,
       attempts INTEGER NOT NULL DEFAULT 0,
       max_attempts INTEGER NOT NULL DEFAULT 25,
       created_at INTEGER NOT NULL DEFAULT (unixepoch()))",
    "CREATE INDEX IF NOT EXISTS askr_jobs_claim
       ON askr_jobs (queue, reserved_until, priority DESC, available_at, id)",
    "CREATE TABLE IF NOT EXISTS askr_failed_jobs (
       id INTEGER PRIMARY KEY AUTOINCREMENT,
       uuid TEXT, queue TEXT NOT NULL, payload BLOB NOT NULL,
       exception TEXT, attempts INTEGER NOT NULL,
       failed_at INTEGER NOT NULL DEFAULT (unixepoch()))",
];

const QUEUE_CONTRACT: &str = include_str!("../../docs/contracts/QUEUE_CONTRACT.md");
const CACHE_CONTRACT: &str = include_str!("../../docs/contracts/CACHE_CONTRACT.md");
const PUBSUB_CONTRACT: &str = include_str!("../../docs/contracts/PUBSUB_CONTRACT.md");

/// Take the SQL a contract publishes under `heading`, and bind its named
/// parameters positionally in `order`.
///
/// This test is the thing that lets the contracts be called executable specs, so
/// it should execute what the documents say rather than a copy of it. The claim
/// in particular is a subtle atomic `UPDATE` that every consumer has to
/// reproduce exactly; keeping a second copy here meant the contract and its
/// proof could drift apart in silence, and then a green test would say nothing
/// about what the document promises.
///
/// It fails loudly rather than quietly running the wrong thing: a missing
/// heading, a missing SQL block, a parameter the caller did not list, or a
/// listed parameter the SQL does not use.
fn contract_sql(contract: &str, heading: &str, order: &[&str]) -> String {
    let start = contract
        .find(&format!("### {heading}"))
        .unwrap_or_else(|| panic!("no heading {heading:?} in the contract"));
    let body = &contract[start..];
    let open = body
        .find("```sql")
        .unwrap_or_else(|| panic!("no sql block under {heading:?}"));
    let rest = &body[open + "```sql".len()..];
    let close = rest
        .find("```")
        .unwrap_or_else(|| panic!("unterminated sql block under {heading:?}"));
    let mut sql = rest[..close].trim().trim_end_matches(';').to_string();

    // Longest first, so one parameter name cannot clobber another it prefixes.
    let mut binds: Vec<(String, String)> = order
        .iter()
        .enumerate()
        .map(|(i, name)| (format!(":{name}"), format!("?{}", i + 1)))
        .collect();
    binds.sort_by_key(|(placeholder, _)| std::cmp::Reverse(placeholder.len()));

    for (placeholder, positional) in &binds {
        assert!(
            sql.contains(placeholder.as_str()),
            "{heading:?} does not use {placeholder}, but the test binds it"
        );
        sql = sql.replace(placeholder.as_str(), positional);
    }

    let unbound: Vec<String> = sql
        .split(':')
        .skip(1)
        .map(|tail| {
            tail.chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>()
        })
        .filter(|name| !name.is_empty())
        .collect();
    assert!(
        unbound.is_empty(),
        "{heading:?} uses named parameters the test does not bind: {unbound:?}"
    );
    sql
}

static ENQUEUE: LazyLock<String> = LazyLock::new(|| {
    contract_sql(
        QUEUE_CONTRACT,
        "Enqueue",
        &["queue", "payload", "priority", "delay", "max_attempts"],
    )
});

static CLAIM: LazyLock<String> =
    LazyLock::new(|| contract_sql(QUEUE_CONTRACT, "Claim (pop)", &["queue", "visibility"]));

/// Claim and return (id, payload, attempts, max_attempts).
async fn claim(conn: &Connection, queue: &str, visibility: i64) -> Option<(i64, String, i64, i64)> {
    let mut rows = conn
        .query(&CLAIM, params![queue, visibility])
        .await
        .unwrap();
    rows.next().await.unwrap().map(|r| {
        (
            r.get::<i64>(0).unwrap(),
            r.get::<String>(1).unwrap(),
            r.get::<i64>(2).unwrap(),
            r.get::<i64>(3).unwrap(),
        )
    })
}

#[tokio::test]
async fn queue_contract_v1() {
    let conn = conn().await;
    for ddl in QUEUE_SCHEMA {
        exec(&conn, ddl, ()).await;
    }

    // Enqueue returns the new id.
    let id_a = one_i64(&conn, &ENQUEUE, params!["default", "A", 0, 0, 25])
        .await
        .unwrap();
    let id_b = one_i64(&conn, &ENQUEUE, params!["default", "B", 0, 0, 25])
        .await
        .unwrap();
    assert!(id_b > id_a);

    // Priority beats FIFO: a higher-priority job jumps ahead.
    let id_hi = one_i64(&conn, &ENQUEUE, params!["default", "HI", 10, 0, 25])
        .await
        .unwrap();
    let first = claim(&conn, "default", 30).await.unwrap();
    assert_eq!(first.0, id_hi, "highest priority claimed first");
    assert_eq!(first.1, "HI");
    assert_eq!(first.2, 1, "attempts incremented at claim");

    // No double delivery: the next two claims are the two remaining distinct jobs.
    let c1 = claim(&conn, "default", 30).await.unwrap();
    let c2 = claim(&conn, "default", 30).await.unwrap();
    assert_ne!(c1.0, c2.0);
    assert_eq!(
        [c1.0, c2.0]
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        [id_a, id_b].iter().copied().collect()
    );

    // All three are now reserved -> queue looks empty to a new claimer.
    assert!(
        claim(&conn, "default", 30).await.is_none(),
        "reserved jobs are not re-claimed"
    );

    // Ack removes a job permanently.
    exec(
        &conn,
        "DELETE FROM askr_jobs WHERE id = ?1",
        params![first.0],
    )
    .await;

    // A lapsed reservation (worker died) makes the job ready again -> at-least-once.
    exec(
        &conn,
        "UPDATE askr_jobs SET reserved_until = unixepoch() - 1 WHERE id = ?1",
        params![c1.0],
    )
    .await;
    let redelivered = claim(&conn, "default", 30).await.unwrap();
    assert_eq!(redelivered.0, c1.0);
    assert_eq!(redelivered.2, 2, "redelivery consumes another attempt");

    // Release (nack) re-arms immediately with backoff = 0; attempts unchanged.
    exec(
        &conn,
        "UPDATE askr_jobs SET reserved_until = NULL, available_at = unixepoch() + 0 WHERE id = ?1",
        params![c2.0],
    )
    .await;
    let after_release = claim(&conn, "default", 30).await.unwrap();
    assert_eq!(after_release.0, c2.0);

    // Delayed job is not claimable until its time comes.
    let delayed = one_i64(&conn, &ENQUEUE, params!["default", "LATER", 0, 3600, 25])
        .await
        .unwrap();
    // Drain what is ready and check the delayed job is never among it. Asserting
    // only that nothing is claimable at the end cannot tell the two cases apart:
    // a claim that ignored available_at would simply take the delayed job during
    // the drain, leaving the queue empty either way. Mutating the contract to
    // drop the `available_at <= unixepoch()` guard used to leave this test green.
    while let Some(claimed) = claim(&conn, "default", 30).await {
        assert_ne!(
            claimed.0, delayed,
            "claimed a job whose available_at is still in the future"
        );
    }

    // Dead-letter: move an exhausted job into askr_failed_jobs and delete it.
    exec(
        &conn,
        "INSERT INTO askr_jobs (queue, payload, available_at, attempts, max_attempts)
                 VALUES ('default', 'DEAD', unixepoch(), 25, 25)",
        (),
    )
    .await;
    let dead = claim(&conn, "default", 30).await.unwrap();
    assert!(dead.2 >= dead.3, "attempts reached max_attempts");
    exec(
        &conn,
        "INSERT INTO askr_failed_jobs (uuid, queue, payload, exception, attempts)
          SELECT ?2, queue, payload, ?3, attempts FROM askr_jobs WHERE id = ?1",
        params![dead.0, "uuid-1", "boom"],
    )
    .await;
    exec(
        &conn,
        "DELETE FROM askr_jobs WHERE id = ?1",
        params![dead.0],
    )
    .await;
    assert_eq!(
        one_i64(&conn, "SELECT count(*) FROM askr_failed_jobs", ()).await,
        Some(1)
    );
    assert_eq!(
        one_str(&conn, "SELECT payload FROM askr_failed_jobs", ())
            .await
            .as_deref(),
        Some("DEAD")
    );

    // Backlog query (FILTER) returns total / ready / oldest_seconds.
    let mut rows = conn
        .query(
            "SELECT count(*) AS total,
                count(*) FILTER (WHERE available_at <= unixepoch()
                     AND (reserved_until IS NULL OR reserved_until <= unixepoch())) AS ready,
                coalesce(unixepoch() - min(available_at) FILTER (WHERE available_at <= unixepoch()
                     AND (reserved_until IS NULL OR reserved_until <= unixepoch())), 0) AS oldest
         FROM askr_jobs WHERE queue = ?1",
            params!["default"],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let (total, ready) = (row.get::<i64>(0).unwrap(), row.get::<i64>(1).unwrap());
    assert!(total >= 1, "delayed job still counts toward total");
    assert_eq!(
        ready, 0,
        "only a delayed + reserved jobs remain, none ready"
    );
}

// ---------------------------------------------------------------------------
// Cache contract (v1)
// ---------------------------------------------------------------------------

const CACHE_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS askr_cache (
       key TEXT PRIMARY KEY, value BLOB NOT NULL, expires_at INTEGER)",
    "CREATE INDEX IF NOT EXISTS askr_cache_expiry ON askr_cache (expires_at)",
    "CREATE VIEW IF NOT EXISTS askr_cache_live AS
       SELECT key, value FROM askr_cache
       WHERE expires_at IS NULL OR expires_at > unixepoch()",
    "CREATE TABLE IF NOT EXISTS askr_cache_tags (
       tag TEXT NOT NULL, key TEXT NOT NULL, PRIMARY KEY (tag, key))",
];

static SET: LazyLock<String> = LazyLock::new(|| {
    contract_sql(
        CACHE_CONTRACT,
        "Set (put, with optional TTL)",
        &["key", "value", "expires_at"],
    )
});
static GET: LazyLock<String> = LazyLock::new(|| contract_sql(CACHE_CONTRACT, "Get", &["key"]));
static ADD: LazyLock<String> = LazyLock::new(|| {
    contract_sql(
        CACHE_CONTRACT,
        "Atomic add (SETNX — the basis for `Cache::lock()`)",
        &["key", "owner", "ttl"],
    )
});
static INCR: LazyLock<String> = LazyLock::new(|| {
    contract_sql(
        CACHE_CONTRACT,
        "Atomic increment / decrement (counters, rate limiting)",
        &["key", "delta", "expires_at"],
    )
});

#[tokio::test]
async fn cache_contract_v1() {
    let conn = conn().await;
    for ddl in CACHE_SCHEMA {
        exec(&conn, ddl, ()).await;
    }

    // Set + get with TTL; forever = NULL expiry.
    exec(&conn, &SET, params!["k1", "v1", 9_999_999_999i64]).await;
    exec(&conn, &SET, params!["perm", "v2", None::<i64>]).await;
    assert_eq!(
        one_str(&conn, &GET, params!["k1"]).await.as_deref(),
        Some("v1")
    );
    assert_eq!(
        one_str(&conn, &GET, params!["perm"]).await.as_deref(),
        Some("v2")
    );

    // Expired entry is a miss via the live view (before any sweep).
    exec(&conn, &SET, params!["gone", "x", 1i64]).await; // expires_at in the distant past
    assert_eq!(
        one_str(&conn, &GET, params!["gone"]).await,
        None,
        "expired -> miss"
    );

    // Atomic increment: missing -> delta, then accumulates.
    assert_eq!(one_i64(&conn, &INCR, params!["ctr", 5]).await, Some(5));
    assert_eq!(one_i64(&conn, &INCR, params!["ctr", 3]).await, Some(8));

    // Atomic add (SETNX): fresh acquires, held does not, expired can be stolen.
    assert_eq!(
        one_i64(&conn, &ADD, params!["lock", "owA", 30]).await,
        Some(1),
        "fresh lock acquired"
    );
    assert_eq!(
        one_i64(&conn, &ADD, params!["lock", "owB", 30]).await,
        None,
        "held lock not acquired"
    );
    exec(
        &conn,
        "UPDATE askr_cache SET expires_at = unixepoch() - 1 WHERE key = 'lock'",
        (),
    )
    .await;
    assert_eq!(
        one_i64(&conn, &ADD, params!["lock", "owB", 30]).await,
        Some(1),
        "expired lock stolen"
    );

    // Release only if still the owner.
    exec(
        &conn,
        "DELETE FROM askr_cache WHERE key = ?1 AND value = ?2",
        params!["lock", "owA"],
    )
    .await;
    assert_eq!(
        one_str(&conn, "SELECT value FROM askr_cache WHERE key='lock'", ())
            .await
            .as_deref(),
        Some("owB"),
        "wrong owner cannot release"
    );
    exec(
        &conn,
        "DELETE FROM askr_cache WHERE key = ?1 AND value = ?2",
        params!["lock", "owB"],
    )
    .await;
    assert_eq!(
        one_str(&conn, "SELECT value FROM askr_cache WHERE key='lock'", ()).await,
        None
    );

    // Tags: invalidate a whole tag.
    exec(&conn, &SET, params!["p:1", "a", None::<i64>]).await;
    exec(&conn, &SET, params!["p:2", "b", None::<i64>]).await;
    exec(
        &conn,
        "INSERT OR IGNORE INTO askr_cache_tags (tag, key) VALUES ('posts','p:1'),('posts','p:2')",
        (),
    )
    .await;
    exec(
        &conn,
        "DELETE FROM askr_cache WHERE key IN (SELECT key FROM askr_cache_tags WHERE tag = ?1)",
        params!["posts"],
    )
    .await;
    exec(
        &conn,
        "DELETE FROM askr_cache_tags WHERE tag = ?1",
        params!["posts"],
    )
    .await;
    assert_eq!(
        one_str(&conn, &GET, params!["p:1"]).await,
        None,
        "tag invalidation removed the key"
    );

    // Sweep + flush.
    exec(
        &conn,
        "DELETE FROM askr_cache WHERE expires_at IS NOT NULL AND expires_at <= unixepoch()",
        (),
    )
    .await;
    exec(&conn, "DELETE FROM askr_cache", ()).await;
    assert_eq!(
        one_i64(&conn, "SELECT count(*) FROM askr_cache", ()).await,
        Some(0)
    );
}

// ---------------------------------------------------------------------------
// Pub/sub contract (v1)
// ---------------------------------------------------------------------------

const PUBSUB_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS askr_events (
       seq INTEGER PRIMARY KEY AUTOINCREMENT, channel TEXT NOT NULL,
       payload BLOB NOT NULL, created_at INTEGER NOT NULL DEFAULT (unixepoch()))",
    "CREATE INDEX IF NOT EXISTS askr_events_chan ON askr_events (channel, seq)",
    "CREATE TABLE IF NOT EXISTS askr_subscribers (
       name TEXT PRIMARY KEY, cursor INTEGER NOT NULL DEFAULT 0,
       updated_at INTEGER NOT NULL DEFAULT (unixepoch()))",
];

static PUBLISH: LazyLock<String> =
    LazyLock::new(|| contract_sql(PUBSUB_CONTRACT, "Publish", &["channel", "payload"]));

#[tokio::test]
async fn pubsub_contract_v1() {
    let conn = conn().await;
    for ddl in PUBSUB_SCHEMA {
        exec(&conn, ddl, ()).await;
    }

    // Publish returns a monotonic seq.
    let s1 = one_i64(&conn, &PUBLISH, params!["orders", "o1"])
        .await
        .unwrap();
    let s2 = one_i64(&conn, &PUBLISH, params!["orders", "o2"])
        .await
        .unwrap();
    let _s3 = one_i64(&conn, &PUBLISH, params!["audit", "login"])
        .await
        .unwrap();
    assert!(s2 > s1, "seq is monotonic");

    // Tail one channel past a cursor with a batch limit.
    let tail = |cursor: i64| {
        conn.query(
            "SELECT seq, payload, created_at FROM askr_events
             WHERE channel = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
            params!["orders", cursor, 10],
        )
    };
    let mut rows = tail(0).await.unwrap();
    let mut got = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        got.push(r.get::<String>(1).unwrap());
    }
    assert_eq!(got, vec!["o1", "o2"], "only this channel, in order");

    // Publish more; tailing past the last seen seq yields only the new message.
    let s4 = one_i64(&conn, &PUBLISH, params!["orders", "o3"])
        .await
        .unwrap();
    let mut rows = tail(s2).await.unwrap();
    let mut newer = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        newer.push((r.get::<i64>(0).unwrap(), r.get::<String>(1).unwrap()));
    }
    assert_eq!(newer, vec![(s4, "o3".to_string())]);

    // Multi-channel fan-in.
    let mut rows = conn
        .query(
            "SELECT payload FROM askr_events WHERE channel IN (?1, ?2) ORDER BY seq",
            params!["orders", "audit"],
        )
        .await
        .unwrap();
    let mut all = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        all.push(r.get::<String>(0).unwrap());
    }
    assert_eq!(all, vec!["o1", "o2", "login", "o3"]);

    // Persist a subscriber cursor (upsert).
    exec(&conn,
         "INSERT INTO askr_subscribers (name, cursor, updated_at) VALUES (?1, ?2, unixepoch())
          ON CONFLICT(name) DO UPDATE SET cursor = excluded.cursor, updated_at = excluded.updated_at",
         params!["sse-1", s4]).await;
    assert_eq!(
        one_i64(
            &conn,
            "SELECT cursor FROM askr_subscribers WHERE name = ?1",
            params!["sse-1"]
        )
        .await,
        Some(s4)
    );

    // Retention: keep only the newest N by seq.
    exec(
        &conn,
        "DELETE FROM askr_events WHERE seq <= (SELECT max(seq) FROM askr_events) - ?1",
        params![2],
    )
    .await;
    assert_eq!(
        one_i64(&conn, "SELECT count(*) FROM askr_events", ()).await,
        Some(2),
        "trimmed to newest 2"
    );
}
