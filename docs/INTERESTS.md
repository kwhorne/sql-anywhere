# Interests: standing vector queries

> **A vector search that runs when the data arrives, not when someone asks.**
> Register a vector and a distance once; every row whose vector lands within
> reach publishes an event.

A vector search answers the question asked now. An agent watching for support
tickets about refunds, or a feed of articles like the ones a reader saved,
needs the other direction: be told when something near *this* shows up. An
interest is that, built from pieces SQL Anywhere already has: a vector column,
`vector_distance_cos`, and the [pub/sub contract](contracts/PUBSUB_CONTRACT.md).

Verified: [`sqlanywhere/tests/interests.rs`](../sqlanywhere/tests/interests.rs).

## How

```rust
use sqlanywhere::contracts::pubsub;
use sqlanywhere::interests::Interests;

let tickets = Interests::on("tickets", "emb");      // a FLOAT32(n) column
tickets.install(&conn).await?;                       // once, like a migration

tickets.subscribe(&conn, "refunds", "agent.refunds", "[0.12, -0.4, …]", 0.25).await?;
// or from text, with the embedder that fills the column:
tickets.subscribe_text(&conn, "outages", "oncall", &embedder, "the site is down", 0.3).await?;

// Any write that gives a ticket a vector near enough publishes on the channel:
let mut events = pubsub::subscribe(&conn, "agent.refunds", cursor, 100).await?;
// payload: {"table":"tickets","column":"emb","rowid":7,"interest":"refunds","distance":0.08}
```

| Method | Does |
| --- | --- |
| `Interests::on(table, column)` | Interests in one vector column of a table with a rowid. |
| `install(conn)` / `uninstall(conn)` | Create the pub/sub tables, the registry `sqlanywhere_interests` and two triggers; drop the triggers and the column's interests. |
| `subscribe(conn, name, channel, vector, max_distance)` | Publish on `channel` whenever a row's vector comes within cosine distance `max_distance` of `vector`. The same `name` again replaces it. |
| `subscribe_text(conn, name, channel, embedder, text, max_distance)` | The same, for what `text` means to `embedder`. |
| `unsubscribe(conn, name)` | Stop it. |
| `matching(conn, rowid)` | The interests a row is within reach of now, nearest first: for inspection, and for rows written before an interest existed. |

## Semantics

- **Who writes the vector does not matter.** Matching is an `AFTER INSERT` and
  an `AFTER UPDATE OF column` trigger, so the application, an
  [embedding column](EMBEDDING_COLUMNS.md) worker filling the vector later, a
  [branch](BRANCHES.md) merge and a CRDT sync all publish. On an embedded
  replica the triggers run on the primary, and the events replicate like any
  other rows.
- **In the same transaction.** The event is written with the vector, so a
  rolled-back write publishes nothing, and a committed one cannot lose its
  event.
- **One event per interest within reach, nearest first.** Writing the same
  vector again, or changing another column, publishes nothing.
- **Writes never fail because of an interest.** A value that is not a vector
  of the interest's size (text, a vector of another length) gets no distance
  and matches nothing. `subscribe` refuses a vector of the wrong size, so an
  interest that could never fire is not left behind.
- **Cosine distance,** 0 for the same direction, 2 for the opposite.

## Cost

Every interest on the column is compared with every vector written to it: one
`vector_distance_cos` per interest per write. That suits hundreds or thousands
of interests; for millions, the interests themselves would want a vector
index, which is not built.
