//! Standing vector queries through `sqlanywhere::interests`: a write that puts
//! a row's vector near an interest publishes on the interest's channel.

use sqlanywhere::contracts::pubsub;
use sqlanywhere::embedding_column::EmbeddingColumn;
use sqlanywhere::interests::{Interests, Match};
use sqlanywhere::{params, Builder, Connection, LexicalEmbedder};

async fn db() -> Connection {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute(
        "CREATE TABLE tickets (id INTEGER PRIMARY KEY, subject TEXT, emb FLOAT32(4))",
        (),
    )
    .await
    .unwrap();
    conn
}

async fn put(conn: &Connection, id: i64, emb: &str) {
    conn.execute(
        "INSERT INTO tickets (id, subject, emb) VALUES (?, 'x', vector32(?))",
        params![id, emb],
    )
    .await
    .unwrap();
}

/// (interest, rowid, distance) of every event on `channel`, in order.
async fn events(conn: &Connection, channel: &str) -> Vec<(String, i64, f64)> {
    events_on(conn, channel, "tickets").await
}

/// The same, checking every event is about `table.emb`.
async fn events_on(conn: &Connection, channel: &str, table: &str) -> Vec<(String, i64, f64)> {
    let mut rows = pubsub::subscribe(conn, channel, 0, 1000).await.unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        let payload = String::from_utf8(row.get::<Vec<u8>>(1).unwrap()).unwrap();
        let json = |key: &str| {
            let at = payload.find(&format!("\"{key}\":")).unwrap() + key.len() + 3;
            let rest = &payload[at..];
            rest[..rest.find([',', '}']).unwrap()]
                .trim_matches('"')
                .to_string()
        };
        assert!(
            payload.contains(&format!(r#""table":"{table}","column":"emb""#)),
            "{payload}"
        );
        out.push((
            json("interest"),
            json("rowid").parse().unwrap(),
            json("distance").parse().unwrap(),
        ));
    }
    out
}

#[tokio::test]
async fn a_vector_near_an_interest_publishes_on_its_channel() {
    let conn = db().await;
    let tickets = Interests::on("tickets", "emb");
    tickets.install(&conn).await.unwrap();
    tickets
        .subscribe(&conn, "refunds", "agent", "[1, 0, 0, 0]", 0.1)
        .await
        .unwrap();
    tickets
        .subscribe(&conn, "outages", "oncall", "[0, 1, 0, 0]", 0.1)
        .await
        .unwrap();
    // A wide interest on the same channel as refunds.
    tickets
        .subscribe(&conn, "anything", "agent", "[1, 1, 0, 0]", 0.5)
        .await
        .unwrap();

    put(&conn, 1, "[1, 0.05, 0, 0]").await; // refunds, and anything
    put(&conn, 2, "[0, 0, 1, 0]").await; // nothing
    put(&conn, 3, "[0.02, 1, 0, 0]").await; // outages, and anything
    conn.execute(
        "INSERT INTO tickets (id, subject) VALUES (4, 'no vector yet')",
        (),
    )
    .await
    .unwrap();

    let agent = events(&conn, "agent").await;
    let names: Vec<_> = agent.iter().map(|e| (e.0.as_str(), e.1)).collect();
    // Nearest first within one write.
    assert_eq!(names, [("refunds", 1), ("anything", 1), ("anything", 3)]);
    assert!(agent[0].2 < 0.01 && agent[1].2 > agent[0].2);
    let oncall: Vec<_> = events(&conn, "oncall")
        .await
        .into_iter()
        .map(|e| e.1)
        .collect();
    assert_eq!(oncall, [3]);

    // Changing another column, or writing the same vector again, publishes
    // nothing; changing the vector does.
    conn.execute("UPDATE tickets SET subject = 'y' WHERE id = 1", ())
        .await
        .unwrap();
    conn.execute(
        "UPDATE tickets SET emb = emb, subject = 'z' WHERE id = 1",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE tickets SET emb = vector32('[0, 1, 0, 0]') WHERE id = 4",
        (),
    )
    .await
    .unwrap();
    let oncall: Vec<_> = events(&conn, "oncall")
        .await
        .into_iter()
        .map(|e| e.1)
        .collect();
    assert_eq!(oncall, [3, 4]);
    // [0, 1, 0, 0] is 1 - 1/√2 ≈ 0.29 from the wide interest, inside its 0.5.
    let last = events(&conn, "agent").await.pop().unwrap();
    assert_eq!((last.0.as_str(), last.1), ("anything", 4));
    assert!(
        (last.2 - (1.0 - 1.0 / 2f64.sqrt())).abs() < 1e-4,
        "{}",
        last.2
    );

    // A rolled-back write publishes nothing.
    let tx = conn.transaction().await.unwrap();
    put(&tx, 5, "[1, 0, 0, 0]").await;
    tx.rollback().await.unwrap();
    assert_eq!(events(&conn, "agent").await.len(), 4);

    // What a row matches now, nearest first.
    assert_eq!(
        tickets
            .matching(&conn, 3)
            .await
            .unwrap()
            .iter()
            .map(|m| m.interest.as_str())
            .collect::<Vec<_>>(),
        ["outages", "anything"]
    );
    assert!(tickets.matching(&conn, 2).await.unwrap().is_empty());

    // Unsubscribed interests stop; uninstalling stops them all.
    assert!(tickets.unsubscribe(&conn, "refunds").await.unwrap());
    assert!(!tickets.unsubscribe(&conn, "refunds").await.unwrap());
    put(&conn, 6, "[1, 0, 0, 0]").await;
    assert_eq!(
        events(&conn, "agent")
            .await
            .last()
            .map(|e| (e.0.clone(), e.1)),
        Some(("anything".to_string(), 6))
    );
    tickets.uninstall(&conn).await.unwrap();
    put(&conn, 7, "[0, 1, 0, 0]").await;
    assert_eq!(events(&conn, "oncall").await.len(), 2);
    assert!(tickets.matching(&conn, 7).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_embedding_column_worker_triggers_it_too() {
    // The worker fills the vector later, with an UPDATE; that is a write like
    // any other.
    let conn = db().await;
    let embedder = LexicalEmbedder::new(4);
    let column = EmbeddingColumn::new("tickets", "subject", "emb");
    column.install(&conn, &embedder).await.unwrap();
    let tickets = Interests::on("tickets", "emb");
    tickets.install(&conn).await.unwrap();
    tickets
        .subscribe_text(
            &conn,
            "billing",
            "agent",
            &embedder,
            "refund my invoice",
            0.01,
        )
        .await
        .unwrap();

    conn.execute(
        "INSERT INTO tickets (id, subject) VALUES (1, 'refund my invoice'), (2, 'printer on fire')",
        (),
    )
    .await
    .unwrap();
    assert!(events(&conn, "agent").await.is_empty(), "no vector yet");
    column.process(&conn, &embedder, 10).await.unwrap();
    let agent = events(&conn, "agent").await;
    assert_eq!(agent.len(), 1, "{agent:?}");
    assert_eq!((agent[0].0.as_str(), agent[0].1), ("billing", 1));
    assert_eq!(
        tickets.matching(&conn, 1).await.unwrap(),
        [Match {
            interest: "billing".into(),
            channel: "agent".into(),
            distance: agent[0].2,
        }]
    );
}

#[tokio::test]
async fn misuse_is_refused_and_odd_values_never_fail_a_write() {
    let conn = db().await;
    conn.execute_batch(
        "CREATE TABLE plain (id INTEGER PRIMARY KEY, body TEXT);
         CREATE TABLE keyed (k TEXT PRIMARY KEY, emb FLOAT32(4)) WITHOUT ROWID;",
    )
    .await
    .unwrap();
    let tickets = Interests::on("tickets", "emb");
    for (interests, why) in [
        (Interests::on("plain", "body"), "vector column"),
        (Interests::on("missing", "emb"), "no such table"),
        (Interests::on("tickets", "nope"), "no such table or column"),
        (Interests::on("keyed", "emb"), "rowid"),
    ] {
        let err = interests.install(&conn).await.unwrap_err().to_string();
        assert!(err.contains(why), "{err}");
    }
    let err = tickets
        .subscribe(&conn, "early", "c", "[1, 0, 0, 0]", 0.1)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("install"), "{err}");

    tickets.install(&conn).await.unwrap();
    tickets.install(&conn).await.unwrap(); // idempotent
    for (vector, distance, why) in [
        ("[1, 0, 0]", 0.1, "dimensions"),
        ("1, 0, 0, 0", 0.1, "vector literal"),
        ("[1, 0, 0, 0]", 3.0, "from 0 to 2"),
    ] {
        let err = tickets
            .subscribe(&conn, "bad", "c", vector, distance)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains(why), "{err}");
    }

    tickets
        .subscribe(&conn, "any", "c", "[1, 0, 0, 0]", 2.0)
        .await
        .unwrap();
    // Values that are not a vector of the column's size are stored as before
    // and match nothing; the interest must not turn them into failed writes.
    conn.execute(
        "INSERT INTO tickets (id, emb) VALUES (1, 'not a vector')",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO tickets (id, emb) VALUES (2, vector32('[1, 0, 0, 0, 0, 0]'))",
        (),
    )
    .await
    .unwrap();
    put(&conn, 3, "[1, 0, 0, 0]").await;
    assert_eq!(
        events(&conn, "c")
            .await
            .iter()
            .map(|e| e.1)
            .collect::<Vec<_>>(),
        [3]
    );
}

#[tokio::test]
async fn interests_belong_to_their_own_table_and_column() {
    let conn = db().await;
    conn.execute(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, emb FLOAT32(4), alt FLOAT32(4))",
        (),
    )
    .await
    .unwrap();
    let (tickets, notes, alt) = (
        Interests::on("tickets", "emb"),
        Interests::on("notes", "emb"),
        Interests::on("notes", "alt"),
    );
    for (interests, channel) in [(&tickets, "t"), (&notes, "n"), (&alt, "a")] {
        interests.install(&conn).await.unwrap();
        interests
            .subscribe(&conn, "same name", channel, "[1, 0, 0, 0]", 0.1)
            .await
            .unwrap();
    }
    put(&conn, 1, "[1, 0, 0, 0]").await;
    conn.execute(
        "INSERT INTO notes VALUES (1, vector32('[1, 0, 0, 0]'), NULL)",
        (),
    )
    .await
    .unwrap();
    assert_eq!(events(&conn, "t").await.len(), 1);
    assert_eq!(events_on(&conn, "n", "notes").await.len(), 1);
    assert!(events(&conn, "a").await.is_empty());
    // Unsubscribing one leaves the others with the same name alone.
    notes.unsubscribe(&conn, "same name").await.unwrap();
    put(&conn, 2, "[1, 0, 0, 0]").await;
    assert_eq!(events(&conn, "t").await.len(), 2);
}
