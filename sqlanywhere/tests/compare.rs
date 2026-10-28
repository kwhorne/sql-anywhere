//! `sqlanywhere::compare`: a table against itself in an attached schema, here
//! a snapshot of the file taken with `VACUUM INTO`, which is what a namespace
//! at a moment (`/v1/namespaces/:ns/at`) looks like to SQL.

use sqlanywhere::compare::{self, RowKind};
use sqlanywhere::{embed, params, Builder, Connection, Value};

const DIMS: usize = 16;

/// A database, and a way to snapshot it as `past`.
struct Fixture {
    _dir: tempfile::TempDir,
    conn: Connection,
    snapshot: String,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("now.db");
        let db = Builder::new_local(path.to_str().unwrap())
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        let snapshot = dir.path().join("then.db").to_str().unwrap().to_string();
        Fixture {
            _dir: dir,
            conn,
            snapshot,
        }
    }

    async fn snapshot(&self) {
        self.conn
            .execute("VACUUM INTO ?", params![self.snapshot.as_str()])
            .await
            .unwrap();
        self.conn
            .execute("ATTACH ? AS past", params![self.snapshot.as_str()])
            .await
            .unwrap();
    }
}

async fn put(conn: &Connection, id: i64, title: &str, body: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO docs (id, title, emb) VALUES (?, ?, vector32(?))",
        params![id, title, embed(body, DIMS)],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn inserted_updated_and_deleted_rows_with_how_far_vectors_moved() {
    let f = Fixture::new().await;
    let conn = &f.conn;
    conn.execute(
        &format!("CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, emb FLOAT32({DIMS}))"),
        (),
    )
    .await
    .unwrap();
    put(conn, 1, "one", "the cat sat on the mat").await;
    put(conn, 2, "two", "rust borrow checker lifetimes").await;
    put(conn, 3, "three", "sourdough bread starter recipe").await;
    put(conn, 4, "four", "sqlite write ahead log checkpoint").await;
    f.snapshot().await;

    conn.execute("DELETE FROM docs WHERE id = 1", ())
        .await
        .unwrap();
    conn.execute("UPDATE docs SET title = 'TWO' WHERE id = 2", ())
        .await
        .unwrap();
    // A rewrite with another meaning, and a small edit.
    put(conn, 3, "three", "quarterly tax filing deadlines").await;
    put(conn, 4, "four", "sqlite write ahead log checkpoint tuning").await;
    put(conn, 5, "five", "new document").await;

    let diff = compare::table(conn, "docs", "past", "main").await.unwrap();
    assert_eq!(diff.key, ["id"]);
    let kinds: Vec<_> = diff
        .rows
        .iter()
        .map(|r| (r.key[0].clone(), r.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            (Value::Integer(5), RowKind::Inserted),
            (Value::Integer(2), RowKind::Updated),
            (Value::Integer(3), RowKind::Updated),
            (Value::Integer(4), RowKind::Updated),
            (Value::Integer(1), RowKind::Deleted),
        ]
    );

    // An update lists only what changed, before and after.
    let two = &diff.rows[1];
    assert_eq!(two.columns.len(), 1);
    let title = two.column("title").unwrap();
    assert_eq!(title.before, Some(Value::Text("two".into())));
    assert_eq!(title.after, Some(Value::Text("TWO".into())));
    assert_eq!(title.vector_distance, None);

    // Inserted and deleted rows carry every column, on their side only.
    let five = &diff.rows[0];
    assert!(five
        .columns
        .iter()
        .all(|c| c.before.is_none() && c.after.is_some()));
    let one = &diff.rows[4];
    assert_eq!(one.columns.len(), 3);
    assert!(one
        .columns
        .iter()
        .all(|c| c.after.is_none() && c.before.is_some()));

    // The rewrite moved further than the edit, and moved() puts it first.
    let moved = diff.moved("emb");
    assert_eq!(moved.len(), 2);
    assert_eq!(moved[0].0.key, [Value::Integer(3)]);
    assert_eq!(moved[1].0.key, [Value::Integer(4)]);
    assert!(moved[0].1 > moved[1].1 && moved[1].1 > 0.0, "{moved:?}");

    // Compared the other way round, inserts and deletes swap.
    let back = compare::table(conn, "docs", "main", "past").await.unwrap();
    assert_eq!(back.of_kind(RowKind::Inserted).count(), 1);
    assert_eq!(
        back.of_kind(RowKind::Inserted).next().unwrap().key,
        [Value::Integer(1)]
    );
    assert_eq!(
        back.of_kind(RowKind::Deleted).next().unwrap().key,
        [Value::Integer(5)]
    );

    // A table compared with an identical copy has no differences.
    let same = compare::table(conn, "docs", "past", "past").await.unwrap();
    assert!(same.is_empty());
}

#[tokio::test]
async fn keys_nulls_and_columns_that_came_or_went() {
    let f = Fixture::new().await;
    let conn = &f.conn;
    conn.execute_batch(
        "CREATE TABLE stock (shop TEXT, sku TEXT, qty INTEGER, note TEXT, legacy TEXT,
                             PRIMARY KEY (sku, shop)) WITHOUT ROWID;
         INSERT INTO stock VALUES ('oslo', 'a', 1, NULL, 'x'), ('oslo', 'b', 2, NULL, 'y'),
                                  ('bergen', 'a', 3, 'n', 'z');
         CREATE TABLE log (msg TEXT);
         INSERT INTO log VALUES ('first'), ('second');",
    )
    .await
    .unwrap();
    f.snapshot().await;
    conn.execute_batch(
        "UPDATE stock SET note = 'restocked' WHERE shop = 'oslo' AND sku = 'a';
         UPDATE stock SET note = NULL WHERE shop = 'bergen';
         UPDATE stock SET qty = qty WHERE sku = 'b';
         ALTER TABLE stock DROP COLUMN legacy;
         ALTER TABLE stock ADD COLUMN price REAL;
         UPDATE log SET msg = 'SECOND' WHERE rowid = 2;",
    )
    .await
    .unwrap();

    let diff = compare::table(conn, "stock", "past", "main").await.unwrap();
    // The composite key, in primary-key order rather than column order.
    assert_eq!(diff.key, ["sku", "shop"]);
    assert_eq!(diff.added_columns, ["price"]);
    assert_eq!(diff.dropped_columns, ["legacy"]);
    // NULL to a value and a value to NULL are changes; writing the same value
    // and a column only one side has are not.
    let changed: Vec<_> = diff
        .rows
        .iter()
        .map(|r| (r.key.clone(), r.kind, r.columns[0].column.clone()))
        .collect();
    assert_eq!(
        changed,
        [
            (
                vec![Value::Text("a".into()), Value::Text("bergen".into())],
                RowKind::Updated,
                "note".to_string()
            ),
            (
                vec![Value::Text("a".into()), Value::Text("oslo".into())],
                RowKind::Updated,
                "note".to_string()
            ),
        ]
    );

    // No primary key: rows are matched by rowid.
    let log = compare::table(conn, "log", "past", "main").await.unwrap();
    assert_eq!(log.key, ["rowid"]);
    assert_eq!(log.rows.len(), 1);
    assert_eq!(log.rows[0].key, [Value::Integer(2)]);
}

#[tokio::test]
async fn a_missing_table_or_key_is_an_error() {
    let f = Fixture::new().await;
    let conn = &f.conn;
    conn.execute("CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT)", ())
        .await
        .unwrap();
    f.snapshot().await;
    conn.execute("CREATE TABLE only_now (x)", ()).await.unwrap();
    let err = compare::table(conn, "only_now", "past", "main")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("schema `past`"), "{err}");
    assert!(compare::table(conn, "t", "nowhere", "main").await.is_err());

    // A key the other side does not have cannot match rows.
    conn.execute_batch(
        "CREATE TABLE t2 (uuid TEXT PRIMARY KEY, a INTEGER, b TEXT);
         DROP TABLE t; ALTER TABLE t2 RENAME TO t;",
    )
    .await
    .unwrap();
    let err = compare::table(conn, "t", "past", "main").await.unwrap_err();
    assert!(err.to_string().contains("key"), "{err}");
}
