//! Database branches: fork, diff, merge and pull through `sqlanywhere::branch`.
//!
//! Gated on `SQLANYWHERE_CRSQLITE` (path to a built `crsqlite.{dylib,so}`, see
//! `scripts/build-crsqlite.sh`); the tests skip and pass when it is unset, so
//! the main workspace CI is unaffected.

use sqlanywhere::branch::{self, ChangeKind, Effect, Gate, Lens, MergePolicy, Side, Violation};
use sqlanywhere::contracts::{pubsub, queue};
use sqlanywhere::{embed, params, Builder, Connection, Value};

const DIMS: usize = 16;

fn extension() -> Option<String> {
    let ext = std::env::var("SQLANYWHERE_CRSQLITE").ok();
    if ext.is_none() {
        eprintln!(
            "skipping: set SQLANYWHERE_CRSQLITE to a built crsqlite.{{dylib,so}} \
             (see scripts/build-crsqlite.sh) to run this test"
        );
    }
    ext
}

async fn open(ext: &str) -> Connection {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.load_extension_enable().unwrap();
    conn.load_extension(ext, Some("sqlite3_crsqlite_init"))
        .unwrap_or_else(|e| panic!("failed to load cr-sqlite from '{ext}': {e}"));
    conn.load_extension_disable().unwrap();
    conn
}

async fn call(conn: &Connection, sql: &str) {
    let mut rows = conn.query(sql, ()).await.unwrap();
    while rows.next().await.unwrap().is_some() {}
}

async fn scalar(conn: &Connection, sql: &str) -> Value {
    let mut rows = conn.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get_value(0).unwrap()
}

/// A parent with a tracked, vector-indexed table, an untracked audit log fed
/// by a trigger, and a view.
async fn main_db(ext: &str) -> Connection {
    let conn = open(ext).await;
    conn.execute(
        &format!(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY NOT NULL, \
             body TEXT NOT NULL DEFAULT '', emb FLOAT32({DIMS}))"
        ),
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "CREATE INDEX docs_vec ON docs(sqlanywhere_vector_idx(emb, 'metric=cosine'))",
        (),
    )
    .await
    .unwrap();
    call(&conn, "SELECT crsql_as_crr('docs')").await;
    conn.execute("CREATE TABLE audit (msg TEXT)", ())
        .await
        .unwrap();
    conn.execute(
        "CREATE TRIGGER docs_audit AFTER INSERT ON docs \
         BEGIN INSERT INTO audit VALUES ('added ' || new.id); END",
        (),
    )
    .await
    .unwrap();
    conn.execute("CREATE VIEW titles AS SELECT body FROM docs", ())
        .await
        .unwrap();
    for (id, body) in [(1, "alpha alpha"), (2, "beta beta"), (3, "gamma gamma")] {
        put(&conn, id, body).await;
    }
    conn
}

async fn put(conn: &Connection, id: i64, body: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO docs (id, body, emb) VALUES (?, ?, vector32(?))",
        params![id, body, embed(body, DIMS)],
    )
    .await
    .unwrap();
}

async fn body(conn: &Connection, id: i64) -> Option<String> {
    let mut rows = conn
        .query("SELECT body FROM docs WHERE id = ?", params![id])
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .map(|r| r.get::<String>(0).unwrap())
}

async fn nearest(conn: &Connection, query: &str) -> Option<String> {
    let mut rows = conn
        .query(
            "SELECT d.body FROM vector_top_k('docs_vec', vector32(?), 1) k \
             JOIN docs d ON d.id = k.id",
            params![embed(query, DIMS)],
        )
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .map(|r| r.get::<String>(0).unwrap())
}

#[tokio::test]
async fn fork_copies_schema_and_data() {
    let Some(ext) = extension() else { return };
    let main = main_db(&ext).await;
    let br = open(&ext).await;

    let info = branch::fork(&main, &br).await.unwrap();
    assert_eq!(info.tracked_tables, vec!["docs".to_string()]);
    assert_eq!(info.untracked_tables, vec!["audit".to_string()]);
    assert_eq!(branch::info(&br).await.unwrap(), info);

    assert_eq!(
        scalar(&br, "SELECT count(*) FROM docs").await,
        Value::Integer(3)
    );
    assert_eq!(
        scalar(&br, "SELECT count(*) FROM titles").await,
        Value::Integer(3)
    );
    // The trigger ran on main; creating it after the data means it did not run
    // again for the same rows on the branch.
    assert_eq!(
        scalar(&br, "SELECT count(*) FROM audit").await,
        Value::Integer(3)
    );
    // The vector index was rebuilt from the copied rows.
    assert_eq!(
        nearest(&br, "beta beta").await.as_deref(),
        Some("beta beta")
    );

    // Nothing written on the branch yet, so nothing to merge.
    assert!(branch::diff(&main, &br).await.unwrap().is_empty());
}

#[tokio::test]
async fn diff_then_merge() {
    let Some(ext) = extension() else { return };
    let main = main_db(&ext).await;
    let br = open(&ext).await;
    branch::fork(&main, &br).await.unwrap();

    put(&br, 2, "beta rewritten entirely").await;
    put(&br, 4, "delta delta").await;
    br.execute("DELETE FROM docs WHERE id = 3", ())
        .await
        .unwrap();

    let diff = branch::diff(&main, &br).await.unwrap();
    assert!(diff.conflicts.is_empty(), "{:?}", diff.conflicts);
    assert_eq!(diff.untracked_tables, vec!["audit".to_string()]);
    let kinds: Vec<_> = diff.rows.iter().map(|r| (r.pk.clone(), r.kind)).collect();
    assert_eq!(
        kinds,
        vec![
            (vec![Value::Integer(2)], ChangeKind::Update),
            (vec![Value::Integer(4)], ChangeKind::Insert),
            (vec![Value::Integer(3)], ChangeKind::Delete),
        ]
    );

    let update = &diff.rows[0];
    let body_change = update.columns.iter().find(|c| c.column == "body").unwrap();
    assert_eq!(body_change.on_main, Some(Value::Text("beta beta".into())));
    assert_eq!(
        body_change.on_branch,
        Value::Text("beta rewritten entirely".into())
    );
    assert_eq!(body_change.vector_distance, None, "body is not a vector");
    let emb_change = update.columns.iter().find(|c| c.column == "emb").unwrap();
    let moved = emb_change
        .vector_distance
        .expect("emb is a vector on both sides");
    assert!(moved > 0.0 && moved <= 2.0, "embedding moved by {moved}");

    let insert = &diff.rows[1];
    assert!(insert.columns.iter().all(|c| c.on_main.is_none()));
    assert!(diff.rows[2].columns.is_empty());

    // Reviewing is read-only: main is untouched until the merge.
    assert_eq!(body(&main, 2).await.as_deref(), Some("beta beta"));

    let report = branch::merge(&main, &br).await.unwrap();
    assert!(report.changes_applied > 0);
    assert!(report.conflicts.is_empty());
    assert_eq!(
        body(&main, 2).await.as_deref(),
        Some("beta rewritten entirely")
    );
    assert_eq!(body(&main, 4).await.as_deref(), Some("delta delta"));
    assert_eq!(body(&main, 3).await, None);
    assert_eq!(
        nearest(&main, "delta delta").await.as_deref(),
        Some("delta delta")
    );

    // Merged changes are not pending any more, and are not sent twice.
    assert!(branch::diff(&main, &br).await.unwrap().is_empty());
    assert_eq!(branch::merge(&main, &br).await.unwrap().changes_applied, 0);

    // A later write is the only thing the next merge carries.
    put(&br, 5, "epsilon").await;
    let diff = branch::diff(&main, &br).await.unwrap();
    assert_eq!(diff.rows.len(), 1);
    assert_eq!(diff.rows[0].pk, vec![Value::Integer(5)]);
}

#[tokio::test]
async fn concurrent_edits_are_reported_with_their_winner() {
    let Some(ext) = extension() else { return };
    let main = main_db(&ext).await;
    let br = open(&ext).await;
    branch::fork(&main, &br).await.unwrap();

    // Same cell on both sides, and a row one side deletes while the other edits.
    br.execute("UPDATE docs SET body = 'branch says' WHERE id = 1", ())
        .await
        .unwrap();
    main.execute("UPDATE docs SET body = 'main says' WHERE id = 1", ())
        .await
        .unwrap();
    br.execute("UPDATE docs SET body = 'branch keeps it' WHERE id = 2", ())
        .await
        .unwrap();
    main.execute("DELETE FROM docs WHERE id = 2", ())
        .await
        .unwrap();
    // Main and branch agreeing is not a conflict.
    br.execute("UPDATE docs SET body = 'same' WHERE id = 3", ())
        .await
        .unwrap();
    main.execute("UPDATE docs SET body = 'same' WHERE id = 3", ())
        .await
        .unwrap();

    let diff = branch::diff(&main, &br).await.unwrap();
    let predicted: Vec<_> = diff
        .conflicts
        .iter()
        .map(|c| (c.pk.clone(), c.column.clone(), c.winner))
        .collect();
    assert_eq!(
        predicted,
        vec![
            (vec![Value::Integer(1)], Some("body".to_string()), None),
            (vec![Value::Integer(2)], None, None),
        ]
    );

    let report = branch::merge(&main, &br).await.unwrap();
    assert_eq!(report.conflicts.len(), 2);
    for conflict in &report.conflicts {
        let winner = conflict.winner.expect("a merge observes the winner");
        match (conflict.pk[0].clone(), conflict.column.as_deref()) {
            (Value::Integer(1), Some("body")) => {
                let expect = if winner == Side::Branch {
                    "branch says"
                } else {
                    "main says"
                };
                assert_eq!(body(&main, 1).await.as_deref(), Some(expect));
            }
            (Value::Integer(2), None) => {
                let exists = body(&main, 2).await.is_some();
                assert_eq!(exists, winner == Side::Branch, "row 2 winner {winner:?}");
            }
            other => panic!("unexpected conflict {other:?}"),
        }
    }

    // Pulling brings the branch to the same state main converged to.
    branch::pull(&main, &br).await.unwrap();
    for id in 1..=3 {
        assert_eq!(body(&br, id).await, body(&main, id).await, "row {id}");
    }
}

#[tokio::test]
async fn pull_moves_the_base_forward() {
    let Some(ext) = extension() else { return };
    let main = main_db(&ext).await;
    let br = open(&ext).await;
    let forked = branch::fork(&main, &br).await.unwrap();

    main.execute("UPDATE docs SET body = 'newer on main' WHERE id = 1", ())
        .await
        .unwrap();
    put(&main, 9, "iota").await;

    let report = branch::pull(&main, &br).await.unwrap();
    assert!(report.changes_applied > 0);
    assert!(report.conflicts.is_empty());
    assert_eq!(body(&br, 1).await.as_deref(), Some("newer on main"));
    assert_eq!(body(&br, 9).await.as_deref(), Some("iota"));
    // A pulled row is an ordinary write on the branch, so its triggers fire.
    assert_eq!(
        scalar(&br, "SELECT count(*) FROM audit").await,
        Value::Integer(4)
    );
    assert!(branch::info(&br).await.unwrap().base_version > forked.base_version);

    // Pulled changes are main's, not the branch's: nothing to merge back, and
    // editing a cell main already changed is no longer a conflict.
    assert!(branch::diff(&main, &br).await.unwrap().is_empty());
    br.execute("UPDATE docs SET body = 'on top of main' WHERE id = 1", ())
        .await
        .unwrap();
    let diff = branch::diff(&main, &br).await.unwrap();
    assert_eq!(diff.rows.len(), 1);
    assert!(diff.conflicts.is_empty());
    assert_eq!(
        diff.rows[0].columns[0].on_main,
        Some(Value::Text("newer on main".into()))
    );
}

#[tokio::test]
async fn misuse_is_refused() {
    let Some(ext) = extension() else { return };
    let main = main_db(&ext).await;

    // Forking into a database that already has tables.
    let busy = main_db(&ext).await;
    assert!(branch::fork(&main, &busy).await.is_err());

    // Diffing something that is not a branch.
    let plain = open(&ext).await;
    assert!(branch::diff(&main, &plain).await.is_err());

    // Diffing or merging a branch against a database it was not forked from.
    let br = open(&ext).await;
    branch::fork(&main, &br).await.unwrap();
    let stranger = main_db(&ext).await;
    assert!(branch::diff(&stranger, &br).await.is_err());
    assert!(branch::merge(&stranger, &br).await.is_err());
}

/// The jobs on `conn` as (queue, payload, available_at), oldest first.
async fn jobs(conn: &Connection) -> Vec<(String, Vec<u8>, i64)> {
    let mut rows = conn
        .query(
            "SELECT queue, payload, available_at FROM askr_jobs ORDER BY id",
            (),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push((r.get(0).unwrap(), r.get(1).unwrap(), r.get(2).unwrap()));
    }
    out
}

async fn events(conn: &Connection) -> Vec<String> {
    let mut rows = conn
        .query("SELECT channel FROM askr_events ORDER BY seq", ())
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get(0).unwrap());
    }
    out
}

#[tokio::test]
async fn jobs_and_events_queued_on_a_branch_are_carried_once() {
    let Some(ext) = extension() else { return };
    let main = main_db(&ext).await;
    queue::install(&main).await.unwrap();
    queue::enqueue(&main, "email", b"already on main", 0, 0, 25)
        .await
        .unwrap();
    let br = open(&ext).await;
    branch::fork(&main, &br).await.unwrap();

    // The agent queues work on its branch; pub/sub is installed only there.
    queue::enqueue(&br, "email", b"welcome", 0, 3600, 5)
        .await
        .unwrap();
    pubsub::install(&br).await.unwrap();
    pubsub::publish(&br, "agent", b"done").await.unwrap();
    let delayed_until = jobs(&br).await[1].2;

    let diff = branch::diff(&main, &br).await.unwrap();
    assert!(diff.rows.is_empty());
    assert!(!diff.is_empty(), "queued work alone is something to merge");
    assert_eq!(
        diff.effects,
        vec![
            Effect::Job {
                queue: "email".into(),
                payload: b"welcome".to_vec(),
                priority: 0,
                available_at: delayed_until,
                max_attempts: 5,
            },
            Effect::Event {
                channel: "agent".into(),
                payload: b"done".to_vec(),
            },
        ]
    );
    // Until the merge, main's workers cannot see it.
    assert_eq!(jobs(&main).await.len(), 1);

    let report = branch::merge(&main, &br).await.unwrap();
    assert_eq!(report.effects_applied, 2);
    assert_eq!(
        jobs(&main).await,
        vec![
            (
                "email".into(),
                b"already on main".to_vec(),
                jobs(&main).await[0].2
            ),
            ("email".into(), b"welcome".to_vec(), delayed_until),
        ]
    );
    assert_eq!(events(&main).await, vec!["agent".to_string()]);

    // Merged once: a second merge carries nothing, a new job only itself.
    assert!(branch::diff(&main, &br).await.unwrap().is_empty());
    assert_eq!(branch::merge(&main, &br).await.unwrap().effects_applied, 0);
    queue::enqueue(&br, "email", b"follow-up", 0, 0, 25)
        .await
        .unwrap();
    assert_eq!(branch::merge(&main, &br).await.unwrap().effects_applied, 1);
    assert_eq!(jobs(&main).await.len(), 3);

    // The parent's record is what counts, so a branch whose own bookkeeping
    // fell behind (a crash after main committed) does not enqueue twice.
    br.execute(
        "UPDATE sqlanywhere_branch SET value = 0 WHERE key IN ('jobs_mark', 'events_mark')",
        (),
    )
    .await
    .unwrap();
    assert!(branch::diff(&main, &br).await.unwrap().effects.is_empty());
    assert_eq!(branch::merge(&main, &br).await.unwrap().effects_applied, 0);
    assert_eq!(jobs(&main).await.len(), 3);
    assert_eq!(events(&main).await.len(), 1);
}

#[tokio::test]
async fn a_branch_from_before_effects_starts_carrying_them_after_its_next_merge() {
    let Some(ext) = extension() else { return };
    let main = main_db(&ext).await;
    queue::install(&main).await.unwrap();
    let br = open(&ext).await;
    branch::fork(&main, &br).await.unwrap();
    // As a 0.7.0 fork left it: no marks.
    br.execute(
        "DELETE FROM sqlanywhere_branch WHERE key IN ('jobs_mark', 'events_mark')",
        (),
    )
    .await
    .unwrap();
    queue::enqueue(&br, "email", b"unknowable", 0, 0, 25)
        .await
        .unwrap();
    // Which jobs are the branch's own cannot be told, so none are carried.
    assert!(branch::diff(&main, &br).await.unwrap().effects.is_empty());
    assert_eq!(branch::merge(&main, &br).await.unwrap().effects_applied, 0);
    assert!(jobs(&main).await.is_empty());

    queue::enqueue(&br, "email", b"after", 0, 0, 25)
        .await
        .unwrap();
    assert_eq!(branch::merge(&main, &br).await.unwrap().effects_applied, 1);
    assert_eq!(jobs(&main).await[0].1, b"after".to_vec());
}

#[tokio::test]
async fn merge_if_holds_a_branch_that_breaks_the_policy() {
    let Some(ext) = extension() else { return };
    let main = main_db(&ext).await;
    let br = open(&ext).await;
    branch::fork(&main, &br).await.unwrap();

    put(&br, 2, "beta rewritten entirely").await;
    br.execute("DELETE FROM docs WHERE id = 3", ())
        .await
        .unwrap();
    queue::install(&br).await.unwrap();
    queue::enqueue(&br, "payments", b"refund", 0, 0, 25)
        .await
        .unwrap();
    // main edits doc 2 too: a conflict.
    put(&main, 2, "beta edited on main").await;

    let held = |gate: Gate| match gate {
        Gate::Held { violations, diff } => {
            assert_eq!(diff.rows.len(), 2);
            violations
        }
        Gate::Merged(_) => panic!("merged a branch the policy should hold"),
    };
    let check = |policy: MergePolicy| {
        let (main, br) = (main.clone(), br.clone());
        async move { held(branch::merge_if(&main, &br, &policy).await.unwrap()) }
    };

    let v = check(MergePolicy::new().no_conflicts()).await;
    assert!(
        matches!(&v[..], [Violation::Conflict(c), ..] if c.table == "docs"),
        "{v:?}"
    );
    let v = check(MergePolicy::new().read_only("docs")).await;
    assert_eq!(v.len(), 2, "{v:?}");
    let v = check(MergePolicy::new().no_deletes_in("docs")).await;
    assert_eq!(
        v,
        vec![Violation::Delete {
            table: "docs".into(),
            pk: vec![Value::Integer(3)]
        }]
    );
    assert_eq!(v[0].to_string(), "docs[Integer(3)] deleted");
    let v = check(MergePolicy::new().no_deletes()).await;
    assert_eq!(v.len(), 1);
    let v = check(MergePolicy::new().max_rows(1)).await;
    assert_eq!(v, vec![Violation::TooManyRows { rows: 2, max: 1 }]);
    let v = check(MergePolicy::new().max_vector_distance(0.0)).await;
    assert!(
        matches!(&v[..], [Violation::VectorMoved { column, .. }] if column == "emb"),
        "{v:?}"
    );
    let v = check(MergePolicy::new().no_jobs_on("payments")).await;
    assert_eq!(
        v,
        vec![Violation::Job {
            queue: "payments".into()
        }]
    );

    // Held means untouched: main still has its own edit, doc 3, and no job.
    assert_eq!(body(&main, 2).await.as_deref(), Some("beta edited on main"));
    assert_eq!(body(&main, 3).await.as_deref(), Some("gamma gamma"));
    assert!(!has_jobs_table(&main).await);

    // Rules the branch passes merge it, and everything held is still pending.
    let gate = branch::merge_if(
        &main,
        &br,
        &MergePolicy::new()
            .max_rows(2)
            .max_vector_distance(2.0)
            .no_deletes_in("audit")
            .no_jobs_on("email")
            .no_events_on("agent"),
    )
    .await
    .unwrap();
    let Gate::Merged(report) = gate else {
        panic!("held a branch that passes")
    };
    assert_eq!(report.effects_applied, 1);
    // put writes body and emb, so both columns conflicted.
    let mut cols: Vec<_> = report.conflicts.iter().map(|c| c.column.clone()).collect();
    cols.sort();
    assert_eq!(cols, [Some("body".to_string()), Some("emb".to_string())]);
    assert_eq!(body(&main, 3).await, None);
    assert_eq!(jobs(&main).await[0].0, "payments");
}

async fn has_jobs_table(conn: &Connection) -> bool {
    scalar(
        conn,
        "SELECT count(*) FROM sqlite_master WHERE name = 'askr_jobs'",
    )
    .await
        == Value::Integer(1)
}

async fn ids(conn: &Connection, table: &str) -> Vec<i64> {
    let mut rows = conn
        .query(&format!("SELECT id FROM {table} ORDER BY id"), ())
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<i64>(0).unwrap());
    }
    out
}

#[tokio::test]
async fn a_partial_branch_holds_what_its_lens_selects() {
    let Some(ext) = extension() else { return };
    let main = main_db(&ext).await;
    for (id, text) in [
        (4, "delta"),
        (5, "epsilon"),
        (6, "zeta"),
        (7, "eta"),
        (8, "theta"),
    ] {
        put(&main, id, text).await;
    }
    main.execute_batch(
        "CREATE TABLE tags (id INTEGER PRIMARY KEY NOT NULL, name TEXT NOT NULL DEFAULT '');
         SELECT crsql_as_crr('tags');
         INSERT INTO tags VALUES (1, 'red'), (2, 'blue');",
    )
    .await
    .unwrap();
    let even = Lens::new().rows("docs", "id % 2 = ?", params![0]).unwrap();
    let br = open(&ext).await;
    let info = branch::fork_lens(&main, &br, &even).await.unwrap();
    assert_eq!(info.tracked_tables, ["docs", "tags"]);

    // Only the even documents; a table the lens does not name, in full.
    assert_eq!(ids(&br, "docs").await, [2, 4, 6, 8]);
    assert_eq!(ids(&br, "tags").await, [1, 2]);
    assert!(branch::diff(&main, &br).await.unwrap().is_empty());

    // Writing and merging: what the branch does not hold is not deleted.
    put(&br, 2, "beta edited on the branch").await;
    put(&br, 10, "kappa, new on the branch").await;
    let diff = branch::diff(&main, &br).await.unwrap();
    assert_eq!(diff.rows.len(), 2);
    branch::merge(&main, &br).await.unwrap();
    assert_eq!(ids(&main, "docs").await, [1, 2, 3, 4, 5, 6, 7, 8, 10]);
    assert_eq!(
        body(&main, 2).await.as_deref(),
        Some("beta edited on the branch")
    );

    // main moves on: a held row edited, another deleted, an odd row edited,
    // and a new even row.
    put(&main, 4, "delta edited on main").await;
    main.execute("DELETE FROM docs WHERE id = 6", ())
        .await
        .unwrap();
    put(&main, 3, "gamma edited on main").await;
    put(&main, 12, "mu, new on main").await;
    let report = branch::pull_lens(&main, &br, &even).await.unwrap();
    assert!(report.changes_applied > 0);
    // Held rows follow main, deletes too; the new even row arrives; the odd
    // one stays out.
    assert_eq!(ids(&br, "docs").await, [2, 4, 8, 10, 12]);
    assert_eq!(body(&br, 4).await.as_deref(), Some("delta edited on main"));
    assert_eq!(body(&br, 12).await.as_deref(), Some("mu, new on main"));
    assert!(branch::diff(&main, &br).await.unwrap().is_empty());

    // Another lens: rows entering it arrive whole, with columns written long
    // before the branch last looked; rows leaving it are kept.
    // An unmerged edit on a held row the new lens also selects, whose cells
    // main wrote last: pulling must not report main's old values of it as a
    // conflict.
    put(&br, 4, "delta edited again on the branch").await;
    let low = Lens::new().rows("docs", "id <= 4", ()).unwrap();
    let report = branch::pull_lens(&main, &br, &low).await.unwrap();
    assert!(report.conflicts.is_empty(), "{:?}", report.conflicts);
    assert_eq!(ids(&br, "docs").await, [1, 2, 3, 4, 8, 10, 12]);
    assert_eq!(body(&br, 3).await.as_deref(), Some("gamma edited on main"));
    assert_eq!(
        scalar(
            &br,
            "SELECT count(*) FROM docs WHERE id IN (1, 3) AND emb IS NOT NULL"
        )
        .await,
        Value::Integer(2)
    );
    assert_eq!(branch::diff(&main, &br).await.unwrap().rows.len(), 1);
    // The rows are whole on the branch: a merge back changes nothing on main
    // but the edit.
    let before = (body(&main, 1).await, body(&main, 3).await);
    branch::merge(&main, &br).await.unwrap();
    assert_eq!((body(&main, 1).await, body(&main, 3).await), before);
}

#[tokio::test]
async fn a_lens_can_select_by_meaning() {
    let Some(ext) = extension() else { return };
    let main = main_db(&ext).await;
    for (id, text) in [(4, "alpha alpha beta"), (5, "unrelated words here")] {
        put(&main, id, text).await;
    }
    // The two documents nearest to "alpha alpha".
    let near = Lens::new()
        .rows(
            "docs",
            "id IN (SELECT id FROM vector_top_k('docs_vec', vector32(?), 2))",
            params![embed("alpha alpha", DIMS)],
        )
        .unwrap();
    let br = open(&ext).await;
    branch::fork_lens(&main, &br, &near).await.unwrap();
    assert_eq!(ids(&br, "docs").await, [1, 4]);
    // Its vector index holds just those, and searches them.
    assert_eq!(
        nearest(&br, "alpha alpha beta").await.as_deref(),
        Some("alpha alpha beta")
    );
}

#[tokio::test]
async fn a_lens_is_checked() {
    let Some(ext) = extension() else { return };
    let main = main_db(&ext).await;
    let br = open(&ext).await;
    let untracked = Lens::new().rows("audit", "1", ()).unwrap();
    let err = branch::fork_lens(&main, &br, &untracked).await.unwrap_err();
    assert!(err.to_string().contains("not a tracked table"), "{err}");
    assert!(Lens::new()
        .rows("docs", "id = :id", sqlanywhere::named_params! {":id": 1})
        .is_err());
}

async fn open_path(ext: &str, path: &str) -> Connection {
    let db = Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.load_extension_enable().unwrap();
    conn.load_extension(ext, Some("sqlite3_crsqlite_init"))
        .unwrap();
    conn.load_extension_disable().unwrap();
    conn
}

/// The same writes on a branch made by `fork` and one made by `fork_file`
/// must look the same to diff, merge and pull.
#[tokio::test]
async fn fork_file_makes_the_same_branch_as_fork() {
    let Some(ext) = extension() else { return };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("branch.db");
    let path = path.to_str().unwrap();
    // Two identical parents, one per kind of fork.
    let (main_a, main_b) = (main_db(&ext).await, main_db(&ext).await);
    let by_rows = open(&ext).await;
    branch::fork(&main_a, &by_rows).await.unwrap();
    // main's record of what its branches carried is main's, not the new one's.
    main_b
        .execute(
            "CREATE TABLE sqlanywhere_branch_merges \
             (branch BLOB PRIMARY KEY, jobs_mark INTEGER NOT NULL, events_mark INTEGER NOT NULL)",
            (),
        )
        .await
        .unwrap();
    let info = branch::fork_file(&main_b, path).await.unwrap();
    let by_file = open_path(&ext, path).await;

    assert_eq!(info.tracked_tables, ["docs"]);
    assert_eq!(info.untracked_tables, ["audit"]);
    assert_eq!(info, branch::info(&by_file).await.unwrap());
    assert_eq!(
        scalar(
            &by_file,
            "SELECT count(*) FROM sqlite_master WHERE name = 'sqlanywhere_branch_merges'"
        )
        .await,
        Value::Integer(0)
    );
    assert_eq!(
        info.parent_site_id,
        match scalar(&main_b, "SELECT crsql_site_id()").await {
            Value::Blob(b) => b,
            other => panic!("{other:?}"),
        }
    );
    assert_eq!(
        Value::Integer(info.base_version),
        scalar(&main_b, "SELECT crsql_db_version()").await
    );
    assert_ne!(
        scalar(&by_file, "SELECT crsql_site_id()").await,
        scalar(&main_b, "SELECT crsql_site_id()").await,
        "the copy has a site id of its own"
    );
    // The point of the rewrite: main's history is main's, not the branch's.
    assert!(branch::diff(&main_b, &by_file).await.unwrap().is_empty());
    assert_eq!(body(&by_file, 2).await.as_deref(), Some("beta beta"));
    assert_eq!(
        nearest(&by_file, "gamma gamma").await.as_deref(),
        Some("gamma gamma")
    );

    // Same writes on both branches and both parents.
    for (main, br) in [(&main_a, &by_rows), (&main_b, &by_file)] {
        put(br, 2, "beta rewritten entirely").await;
        put(br, 4, "delta delta").await;
        br.execute("DELETE FROM docs WHERE id = 3", ())
            .await
            .unwrap();
        put(main, 1, "alpha edited on main").await;
        put(br, 1, "alpha edited on the branch").await;
    }
    let (diff_a, diff_b) = (
        branch::diff(&main_a, &by_rows).await.unwrap(),
        branch::diff(&main_b, &by_file).await.unwrap(),
    );
    assert_eq!(diff_a.rows, diff_b.rows);
    assert_eq!(diff_a.conflicts, diff_b.conflicts);
    assert!(!diff_b.conflicts.is_empty());

    let (merged_a, merged_b) = (
        branch::merge(&main_a, &by_rows).await.unwrap(),
        branch::merge(&main_b, &by_file).await.unwrap(),
    );
    assert_eq!(merged_a.changes_applied, merged_b.changes_applied);
    assert_eq!(merged_a.conflicts, merged_b.conflicts);
    for id in 1..=4 {
        assert_eq!(body(&main_a, id).await, body(&main_b, id).await, "doc {id}");
    }
    assert!(branch::diff(&main_b, &by_file).await.unwrap().is_empty());

    // And main's newer writes reach the branch by pull.
    put(&main_b, 5, "epsilon from main").await;
    branch::pull(&main_b, &by_file).await.unwrap();
    assert_eq!(
        body(&by_file, 5).await.as_deref(),
        Some("epsilon from main")
    );
    assert!(branch::diff(&main_b, &by_file).await.unwrap().is_empty());

    // A path that exists is refused rather than overwritten.
    let err = branch::fork_file(&main_b, path).await.unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");
}
