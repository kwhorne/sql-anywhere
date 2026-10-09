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

The functions take connections, not paths, so the branch lives wherever you
open it: a file beside the parent, a temp file, or `:memory:`.

| Function | Does |
| --- | --- |
| `fork(main, branch)` | Copy main's schema and data into an empty database and mark it as main's branch. |
| `diff(main, branch)` | List the rows the branch changed since the fork or the last merge, beside main's current values, and the conflicts a merge would hit. Read-only. |
| `merge(main, branch)` | Apply those changes to main. Report every conflict and which side won. |
| `pull(main, branch)` | Apply main's newer changes to the branch, so later diffs are measured against them. |
| `info(branch)` | The parent, the main version the branch has seen, and which tables are tracked. |

## A diff that understands vectors

A changed embedding is two different blobs, and a byte diff cannot say whether
the meaning moved a little or a lot. For a vector column (`FLOAT32(n)` and the
other vector types) holding a vector on both sides, each `ColumnChange` carries
`vector_distance`: the cosine distance between main's vector and the branch's.
Reviewing an agent that re-embedded documents, you can sort by how far each
embedding moved and look at the outliers first.

## Semantics

**Tracked and untracked tables.** Only tables that are CRRs on main are tracked.
Every other table is copied once, at fork time, and listed in `untracked_tables`
on `BranchInfo` and on every `BranchDiff`, because its later changes are
neither diffed nor merged. A diff that looks empty while the branch wrote to an
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
conflict, check `diff(...).conflicts` first. `pull` reports conflicts the same
way, from the branch's side.

**Views and triggers.** The fork creates them after the data is in, so a
trigger does not run a second time for rows it already handled on main. Rows
that arrive later through `pull` or `merge` are applied by cr-sqlite as ordinary
writes, and triggers on the receiving side fire for them.

## Limits

- The fork copies through `crsql_changes`, one statement per change. It is
  meant for databases an application or an agent works on, not for cloning a
  large production database; a file-level copy with a fresh site id is the
  faster path, and is not built yet.
- `merge` and `pull` run against two local connections. Branching a database
  served by `sqld`, and a `sqld` endpoint for it, are not built yet.
- Schema changes on either side after the fork are not carried. Fork again after
  a migration.
