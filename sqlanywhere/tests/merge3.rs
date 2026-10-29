//! Three-way merges of a database with a branch of it, on local files: the
//! base and the branch are copies taken at one moment, as
//! `POST /v1/namespaces/:ns/branch/:name` makes them on sqld.

use sqlanywhere::compare::RowKind;
use sqlanywhere::merge3::{Merge3, Merged, OnConflict};
use sqlanywhere::{Builder, Connection, Value};

struct Branched {
    _dir: tempfile::TempDir,
    main: Connection,
    branch: Connection,
    merge: Merge3,
}

/// A database with `schema` and `rows`, and a base and a branch copied from
/// it at one moment.
async fn branched(schema: &str) -> Branched {
    let dir = tempfile::tempdir().unwrap();
    let path = |name: &str| dir.path().join(name).to_str().unwrap().to_string();
    let main = Builder::new_local(path("main.db"))
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    main.execute_batch(schema).await.unwrap();
    for copy in ["base.db", "branch.db"] {
        main.execute(&format!("VACUUM INTO '{}'", path(copy)), ())
            .await
            .unwrap();
    }
    let branch = Builder::new_local(path("branch.db"))
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap();
    let merge = Merge3::new(path("base.db"), path("branch.db"));
    Branched {
        _dir: dir,
        main,
        branch,
        merge,
    }
}

async fn rows(conn: &Connection, sql: &str) -> Vec<Vec<Value>> {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(
            (0..row.column_count())
                .map(|i| row.get_value(i).unwrap())
                .collect(),
        );
    }
    out
}

fn int(i: i64) -> Value {
    Value::Integer(i)
}

fn text(s: &str) -> Value {
    Value::Text(s.into())
}

const DOCS: &str = "CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, body TEXT);
    INSERT INTO docs VALUES (1, 'one', 'a'), (2, 'two', 'b'), (3, 'three', 'c'), (4, 'four', 'd');";

#[tokio::test]
async fn changes_on_both_sides_come_together() {
    let b = branched(DOCS).await;
    // The branch inserts, updates and deletes; main does the same elsewhere.
    b.branch
        .execute_batch(
            "INSERT INTO docs VALUES (10, 'ten', 'x');
             UPDATE docs SET title = 'ONE' WHERE id = 1;
             DELETE FROM docs WHERE id = 2;",
        )
        .await
        .unwrap();
    b.main
        .execute_batch(
            "INSERT INTO docs VALUES (20, 'twenty', 'y');
             UPDATE docs SET body = 'C' WHERE id = 3;",
        )
        .await
        .unwrap();

    let plan = b.merge.diff(&b.main).await.unwrap();
    assert_eq!(plan.changes(), 3);
    assert_eq!(plan.conflicts(), 0);
    let docs = plan.table("docs").unwrap();
    assert_eq!(docs.key, vec!["id"]);
    let kinds: Vec<_> = docs
        .changes
        .iter()
        .map(|r| (r.kind, r.key.clone()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (RowKind::Inserted, vec![int(10)]),
            (RowKind::Updated, vec![int(1)]),
            (RowKind::Deleted, vec![int(2)]),
        ]
    );
    // Diffing changes nothing, and can be repeated.
    assert_eq!(b.merge.diff(&b.main).await.unwrap(), plan);

    let Merged::Applied(applied) = b.merge.merge(&b.main).await.unwrap() else {
        panic!("refused");
    };
    assert_eq!(applied, plan);
    assert_eq!(
        rows(&b.main, "SELECT id, title, body FROM docs ORDER BY id").await,
        vec![
            vec![int(1), text("ONE"), text("a")],
            vec![int(3), text("three"), text("C")],
            vec![int(4), text("four"), text("d")],
            vec![int(10), text("ten"), text("x")],
            vec![int(20), text("twenty"), text("y")],
        ]
    );
    // Merged: nothing is left to apply.
    let again = b.merge.diff(&b.main).await.unwrap();
    assert_eq!((again.changes(), again.conflicts()), (0, 0), "{again:?}");
}

#[tokio::test]
async fn updates_to_different_columns_of_a_row_both_apply() {
    let b = branched(DOCS).await;
    b.branch
        .execute_batch(
            "UPDATE docs SET title = 'ONE' WHERE id = 1;
             UPDATE docs SET title = 'TWO', body = 'B' WHERE id = 2;",
        )
        .await
        .unwrap();
    // Main writes the other column of row 1, and makes row 2 what the branch
    // made it.
    b.main
        .execute_batch(
            "UPDATE docs SET body = 'A' WHERE id = 1;
             UPDATE docs SET title = 'TWO', body = 'B' WHERE id = 2;",
        )
        .await
        .unwrap();

    let plan = b.merge.diff(&b.main).await.unwrap();
    assert_eq!(plan.conflicts(), 0);
    let docs = plan.table("docs").unwrap();
    assert_eq!(docs.changes.len(), 1, "row 2 already agrees");
    assert_eq!(docs.changes[0].columns.len(), 1);
    assert_eq!(docs.changes[0].columns[0].column, "title");

    assert!(matches!(
        b.merge.merge(&b.main).await.unwrap(),
        Merged::Applied(_)
    ));
    assert_eq!(
        rows(
            &b.main,
            "SELECT title, body FROM docs WHERE id IN (1, 2) ORDER BY id"
        )
        .await,
        vec![vec![text("ONE"), text("A")], vec![text("TWO"), text("B")]]
    );
}

async fn conflicted() -> Branched {
    let b = branched(DOCS).await;
    b.branch
        .execute_batch(
            "UPDATE docs SET title = 'branch' WHERE id = 1;
             DELETE FROM docs WHERE id = 2;
             UPDATE docs SET title = 'branch' WHERE id = 3;
             INSERT INTO docs VALUES (5, 'five', 'e');",
        )
        .await
        .unwrap();
    b.main
        .execute_batch(
            "UPDATE docs SET title = 'main' WHERE id = 1;
             UPDATE docs SET body = 'main' WHERE id = 2;
             DELETE FROM docs WHERE id = 3;",
        )
        .await
        .unwrap();
    b
}

#[tokio::test]
async fn conflicts_refuse_the_whole_merge_by_default() {
    let b = conflicted().await;
    let before = rows(&b.main, "SELECT * FROM docs ORDER BY id").await;

    let Merged::Refused(plan) = b.merge.merge(&b.main).await.unwrap() else {
        panic!("applied despite conflicts");
    };
    let docs = plan.table("docs").unwrap();
    let conflicts: Vec<_> = docs
        .conflicts
        .iter()
        .map(|c| (c.branch.key.clone(), c.branch.kind, c.main.kind))
        .collect();
    assert_eq!(
        conflicts,
        vec![
            (vec![int(1)], RowKind::Updated, RowKind::Updated),
            (vec![int(3)], RowKind::Updated, RowKind::Deleted),
            (vec![int(2)], RowKind::Deleted, RowKind::Updated),
        ]
    );
    // Not even the clean insert went in.
    assert_eq!(docs.changes.len(), 1);
    assert_eq!(
        rows(&b.main, "SELECT * FROM docs ORDER BY id").await,
        before
    );
}

#[tokio::test]
async fn conflicts_can_keep_main_or_take_the_branch() {
    let b = conflicted().await;
    let keep = b.merge.clone().on_conflict(OnConflict::KeepMain);
    assert!(matches!(
        keep.merge(&b.main).await.unwrap(),
        Merged::Applied(_)
    ));
    assert_eq!(
        rows(&b.main, "SELECT id, title, body FROM docs ORDER BY id").await,
        vec![
            vec![int(1), text("main"), text("a")],
            vec![int(2), text("two"), text("main")],
            vec![int(4), text("four"), text("d")],
            vec![int(5), text("five"), text("e")],
        ]
    );

    let b = conflicted().await;
    let take = b.merge.clone().on_conflict(OnConflict::TakeBranch);
    assert!(matches!(
        take.merge(&b.main).await.unwrap(),
        Merged::Applied(_)
    ));
    // Row 3, deleted on main, is back as the branch has it; row 2, deleted
    // on the branch, is gone.
    assert_eq!(
        rows(&b.main, "SELECT id, title, body FROM docs ORDER BY id").await,
        vec![
            vec![int(1), text("branch"), text("a")],
            vec![int(3), text("branch"), text("c")],
            vec![int(4), text("four"), text("d")],
            vec![int(5), text("five"), text("e")],
        ]
    );
}

#[tokio::test]
async fn both_inserting_the_same_key_is_a_conflict_unless_the_rows_agree() {
    let b = branched(DOCS).await;
    b.branch
        .execute_batch(
            "INSERT INTO docs VALUES (5, 'five', 'e');
             INSERT INTO docs VALUES (6, 'six', 'branch');",
        )
        .await
        .unwrap();
    b.main
        .execute_batch(
            "INSERT INTO docs VALUES (5, 'five', 'e');
             INSERT INTO docs VALUES (6, 'six', 'main');",
        )
        .await
        .unwrap();
    let plan = b.merge.diff(&b.main).await.unwrap();
    let docs = plan.table("docs").unwrap();
    assert!(docs.changes.is_empty());
    assert_eq!(docs.conflicts.len(), 1);
    assert_eq!(docs.conflicts[0].branch.key, vec![int(6)]);
}

#[tokio::test]
async fn a_table_without_a_primary_key_keeps_its_rowids() {
    let b = branched(
        "CREATE TABLE notes (body TEXT);
         INSERT INTO notes VALUES ('a'), ('b');",
    )
    .await;
    b.branch
        .execute_batch(
            "INSERT INTO notes (rowid, body) VALUES (7, 'seven');
             UPDATE notes SET body = 'B' WHERE rowid = 2;",
        )
        .await
        .unwrap();
    assert!(matches!(
        b.merge.merge(&b.main).await.unwrap(),
        Merged::Applied(_)
    ));
    assert_eq!(
        rows(&b.main, "SELECT rowid, body FROM notes ORDER BY rowid").await,
        vec![
            vec![int(1), text("a")],
            vec![int(2), text("B")],
            vec![int(7), text("seven")],
        ]
    );
}

#[tokio::test]
async fn schema_changes_refuse_the_merge() {
    let b = branched(DOCS).await;
    b.branch
        .execute_batch(
            "ALTER TABLE docs ADD COLUMN tags TEXT;
             CREATE TABLE extra (x);",
        )
        .await
        .unwrap();
    let Merged::Refused(plan) = b
        .merge
        .clone()
        .on_conflict(OnConflict::TakeBranch)
        .merge(&b.main)
        .await
        .unwrap()
    else {
        panic!("applied a schema change");
    };
    assert_eq!(
        plan.schema_changes,
        vec![
            "extra: created on the branch".to_string(),
            "docs: column tags added on branch".to_string(),
        ]
    );
}

#[tokio::test]
async fn internal_virtual_and_skipped_tables_stay_out() {
    let b = branched(
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT);
         CREATE VIRTUAL TABLE docs_fts USING fts5(body, content='docs', content_rowid='id');
         CREATE TRIGGER docs_ai AFTER INSERT ON docs BEGIN
           INSERT INTO docs_fts(rowid, body) VALUES (new.id, new.body);
         END;
         CREATE TABLE audit (id INTEGER PRIMARY KEY, what TEXT);
         CREATE TRIGGER docs_audit AFTER INSERT ON docs BEGIN
           INSERT INTO audit (what) VALUES ('inserted ' || new.id);
         END;
         CREATE TABLE sqlanywhere_scratch (x);",
    )
    .await;
    b.branch
        .execute_batch(
            "INSERT INTO docs VALUES (1, 'ownership in rust');
             INSERT INTO sqlanywhere_scratch VALUES (1);",
        )
        .await
        .unwrap();

    let merge = b.merge.clone().skip("audit");
    let plan = merge.diff(&b.main).await.unwrap();
    let tables: Vec<_> = plan.tables.iter().map(|t| t.table.as_str()).collect();
    assert_eq!(tables, vec!["docs"], "{plan:?}");
    assert!(matches!(
        merge.merge(&b.main).await.unwrap(),
        Merged::Applied(_)
    ));

    // The triggers on main kept the search index and the audit log, once.
    assert_eq!(
        rows(
            &b.main,
            "SELECT rowid FROM docs_fts WHERE docs_fts MATCH 'rust'"
        )
        .await,
        vec![vec![int(1)]]
    );
    assert_eq!(
        rows(&b.main, "SELECT what FROM audit").await,
        vec![vec![text("inserted 1")]]
    );
    assert!(rows(&b.main, "SELECT * FROM sqlanywhere_scratch")
        .await
        .is_empty());
}

#[tokio::test]
async fn foreign_keys_are_checked_at_the_commit() {
    let b = branched(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE a_books (id INTEGER PRIMARY KEY, author INTEGER NOT NULL REFERENCES z_authors(id));
         CREATE TABLE z_authors (id INTEGER PRIMARY KEY, name TEXT);",
    )
    .await;
    b.main
        .execute("PRAGMA foreign_keys = ON", ())
        .await
        .unwrap();
    // The child table sorts first, so its row is written before its parent.
    b.branch
        .execute_batch(
            "INSERT INTO z_authors VALUES (1, 'ursula');
             INSERT INTO a_books VALUES (1, 1);",
        )
        .await
        .unwrap();
    assert!(matches!(
        b.merge.merge(&b.main).await.unwrap(),
        Merged::Applied(_)
    ));
    assert_eq!(
        rows(
            &b.main,
            "SELECT count(*) FROM a_books JOIN z_authors ON a_books.author = z_authors.id"
        )
        .await,
        vec![vec![int(1)]]
    );
}
