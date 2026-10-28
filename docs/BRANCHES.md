# Database branches

> **Status: experimental**, like the [CRDT support](CRDT.md) it is built on.

A branch is a second database forked from a first one. You can write to it
freely, review exactly what it would change, and merge it back. Because it
rests on cr-sqlite's conflict-free merge, a branch never holds a lock on its
parent. The parent keeps taking writes while the branch exists, and a merge
converges the two deterministically instead of failing on a stale base.

That makes a branch the natural sandbox for anything that should not write to
the real data unreviewed:

- **AI agents.** Give each agent its own branch, let it act, show a person the
  diff, and merge only what is approved. A rejected branch is just a file to
  delete.
- **Rehearsals.** Run a data migration or a bulk fix on a branch, inspect the
  result, then merge or discard it.
- **Previews.** One branch per pull request or per user session, merged back
  when it is accepted.

Verified: [`sqlanywhere/tests/branch.rs`](../sqlanywhere/tests/branch.rs).

## The loop

```rust
use sqlanywhere::branch;

// Both connections have the cr-sqlite extension loaded (see docs/CRDT.md),
// and the tables to track are CRRs on main: SELECT crsql_as_crr('docs').
branch::fork(&main, &agent).await?;            // agent gets main's schema + data

agent.execute("UPDATE docs SET body = ? WHERE id = 2", params![draft]).await?;

let diff = branch::diff(&main, &agent).await?; // what a merge would do
for row in &diff.rows {
    println!("{} {:?} {:?}", row.table, row.pk, row.kind);
    for col in &row.columns {
        // on_main is the value main holds now; on_branch is the proposal.
        println!("  {}: {:?} -> {:?}", col.column, col.on_main, col.on_branch);
    }
}

if diff.conflicts.is_empty() && approved {
    let report = branch::merge(&main, &agent).await?;
}
```

Or let a policy decide, and send only what it holds to a person:

```rust
use sqlanywhere::branch::{Gate, MergePolicy};

let policy = MergePolicy::new()
    .no_conflicts()
    .read_only("billing")
    .no_deletes_in("customers")
    .max_vector_distance(0.2)
    .no_jobs_on("payments");

match branch::merge_if(&main, &agent, &policy).await? {
    Gate::Merged(report) => { /* done, nobody had to look */ }
    Gate::Held { diff, violations } => {
        for v in &violations { println!("held: {v}"); }
        ask_a_person(diff).await;   // then branch::merge, or drop the branch
    }
}
```

The functions take connections, not paths, so the branch lives wherever you
open it: a file beside the parent, a temp file, or `:memory:`.

| Function | Does |
| --- | --- |
| `fork(main, branch)` | Copy main's schema and data into an empty database and mark it as main's branch. |
| `fork_file(main, path)` | The same, by copying the file: for large databases (below). |
| `diff(main, branch)` | List the rows the branch changed since the fork or the last merge, beside main's current values, and the conflicts a merge would hit. Read-only. |
| `merge(main, branch)` | Apply those changes to main, and enqueue and publish the branch's jobs and events there. Report every conflict and which side won. |
| `merge_if(main, branch, policy)` | The same, only if the diff passes a `MergePolicy`; otherwise write nothing and return the diff with every violation. |
| `pull(main, branch)` | Apply main's newer changes to the branch, so later diffs are measured against them. |
| `info(branch)` | The parent, the main version the branch has seen, and which tables are tracked. |

## A diff that understands vectors

A changed embedding is two different blobs, and a byte diff cannot say whether
the meaning moved a little or a lot. For a vector column (`FLOAT32(n)` and the
other vector types) holding a vector on both sides, each `ColumnChange` carries
`vector_distance`: the cosine distance between main's vector and the branch's.
Reviewing an agent that re-embedded documents, you can sort by how far each
embedding moved and look at the outliers first.

## Jobs and events: side effects that wait for the merge

An agent rarely only edits rows. It also enqueues work ("send the welcome
mail") on the [queue contract](contracts/QUEUE_CONTRACT.md) and publishes events
on [pub/sub](contracts/PUBSUB_CONTRACT.md). On a branch, those are exactly what
must *not* happen yet, and must happen once the branch is accepted.

`askr_jobs` and `askr_events` cannot be CRRs: their keys are `AUTOINCREMENT`,
so the branch and main would both hand out job 5 and a CRDT merge would fold
two different jobs into one. A branch treats them as an outbox instead:

- `diff` lists every job the branch enqueued and every event it published since
  the fork or the last merge as `BranchDiff.effects`, in order. Rows copied
  from main at fork time are main's and are not listed.
- `merge` enqueues and publishes them on main through the contracts, in the
  same transaction as the branch's data, with main's own ids. A job keeps the
  branch's `available_at`, so a delayed job stays delayed and an overdue one is
  ready at once. Until then no worker on main can see them.
- They are carried **exactly once**. How far a branch's jobs and events have
  been carried is recorded on main, in `sqlanywhere_branch_merges`, in the
  same transaction as the jobs and events themselves; a crash between main's
  commit and the branch's own bookkeeping cannot enqueue them twice.

Only new jobs and events travel. Claiming, acking or deleting a job on a branch
changes the branch's copy and nothing else, so do not point workers at a
branch. A branch forked by 0.7.0 cannot tell its own jobs from main's copies;
it carries none until its first merge and every one after.

## Merging on a policy

`merge_if` takes a `MergePolicy` and merges only when the diff breaks none of
its rules:

| Rule | Holds a branch that |
| --- | --- |
| `no_conflicts()` | changed anything main also changed since the branch last saw it |
| `read_only(table)` | changed any row of `table` |
| `no_deletes()`, `no_deletes_in(table)` | deleted a row, anywhere or in `table` |
| `max_rows(n)` | changed more than `n` rows |
| `max_vector_distance(d)` | moved an embedding further than cosine distance `d` |
| `no_jobs_on(queue)`, `no_events_on(channel)` | enqueued on `queue`, published on `channel` |

What is merged is what was checked. `merge_if` takes main's write lock before
it computes the diff and holds it to the commit, and reads the branch's
changes once, so neither a write on main nor a later write on the branch can
slip in between. A held branch writes nothing to main and keeps everything
pending; `Gate::Held` returns the diff that was checked, and each `Violation`
prints as a sentence for a review queue. `MergePolicy::check(&diff)` is public,
for a caller that wants to add its own rules before deciding.

## Forking a large database

`fork` replays main's history into the branch one `crsql_changes` row at a
time, which is fine for the databases an agent works on and slow for a large
one. `fork_file(&main, "branch.db")` copies the file instead, with `VACUUM
INTO` (one consistent snapshot, page by page), and then gives the copy an
identity of its own. Open the file with the extension loaded and use it like
any branch:

```rust
let info = branch::fork_file(&main, "agent.db").await?;
let agent = open_with_crsqlite("agent.db").await?;   // your own helper
branch::diff(&main, &agent).await?;                   // empty: nothing written yet
```

Copying is not enough on its own. cr-sqlite marks a database's own writes
with site ordinal 0, so a copied file still believes every row main ever wrote
is its own, and the first `diff` would offer the whole database back. Before
anything opens the copy with cr-sqlite, `fork_file` moves those clock rows to
an ordinal for main's site id, gives ordinal 0 a new site id, records the
version main was at in the snapshot (computed from the copy's clock tables, as
cr-sqlite computes it), and writes the same bookkeeping `fork` does. `diff`,
`merge` and `pull` cannot tell the two kinds of fork apart, and the test runs
the same writes through both and compares.

With 20,000 rows, `fork` took 309 ms and `fork_file` 16 ms (release build,
Apple Silicon). The rewrite is one `UPDATE` per tracked table, so its cost
grows with the clock rows, without copying any data a second time.

## Semantics

**Tracked and untracked tables.** Only tables that are CRRs on main are tracked.
Every other table is copied once, at fork time, and listed in `untracked_tables`
on `BranchInfo` and on every `BranchDiff`, because its later changes are
neither diffed nor merged; new jobs and events are the exception above. A diff that looks empty while the branch wrote to an
untracked table says so through that list.

**What counts as the branch's own.** The fork fills tracked tables through
`crsql_changes`, so the copied history keeps main's site id. Only what is
written on the branch afterwards carries the branch's site id, and that is what
`diff` and `merge` see. Changes brought in by `pull` are main's, so they are
never offered back.

**Merging twice.** A merge records how far it got. The next merge sends only
what the branch wrote since, and merging an unchanged branch applies nothing.

**Conflicts.** A conflict is a cell both sides wrote with different values, or a
row one side deleted while the other changed it, since the branch last saw main
(the fork, or the last `pull`). Both sides writing the same value is agreement,
not a conflict. A merge never fails on a conflict: cr-sqlite resolves it the
same way on every node, and `MergeReport.conflicts` records which side won,
observed after the merge rather than predicted. To refuse a merge that would
conflict, use `merge_if` with `no_conflicts()`. `pull` reports conflicts the same
way, from the branch's side.

**Views and triggers.** The fork creates them after the data is in, so a
trigger does not run a second time for rows it already handled on main. Rows
that arrive later through `pull` or `merge` are applied by cr-sqlite as ordinary
writes, and triggers on the receiving side fire for them.

## Limits

- `fork_file` needs `main` to be a local database, and the copy is a full
  copy: it costs the file's size on disk, not a copy-on-write clone.
- `merge` and `pull` run against two local connections. Branching a database
  served by `sqld`, and a `sqld` endpoint for it, are not built yet.
- Schema changes on either side after the fork are not carried. Fork again after
  a migration.
