//! Standing vector queries: semantic pub/sub.
//!
//! A vector search is asked when someone looks. An interest turns it around: it
//! is a vector and a distance registered once, and every time a row's vector
//! lands within that distance an event is published on the interest's channel,
//! through the [pub/sub contract](crate::contracts::pubsub). "Tell my agent
//! whenever a support ticket comes in that looks like this one."
//!
//! ```rust,no_run
//! # async fn run(conn: sqlanywhere::Connection) -> sqlanywhere::Result<()> {
//! use sqlanywhere::contracts::pubsub;
//! use sqlanywhere::interests::Interests;
//!
//! let tickets = Interests::on("tickets", "emb");
//! tickets.install(&conn).await?;
//! tickets
//!     .subscribe(&conn, "refunds", "agent.refunds", "[0.12, -0.4, ...]", 0.25)
//!     .await?;
//!
//! // Any write that gives a ticket a vector near enough publishes, in the same
//! // transaction, on "agent.refunds":
//! // {"table":"tickets","column":"emb","rowid":7,"interest":"refunds","distance":0.08}
//! let mut events = pubsub::subscribe(&conn, "agent.refunds", 0, 100).await?;
//! # Ok(())
//! # }
//! ```
//!
//! Matching is two triggers on the table, so it does not matter who writes the
//! vector: the application, an [embedding column](crate::embedding_column)
//! worker, a CRDT merge, or the primary behind an embedded replica. The event is
//! written in the same transaction as the vector, so a rolled-back write
//! publishes nothing. Distances are cosine distances (0 identical, 2 opposite),
//! and every interest is checked on every write of the column, which suits
//! hundreds or thousands of interests rather than millions.

use crate::contracts::pubsub;
use crate::{params, Connection, Embedder, Error, Result};

/// Every interest, for every watched column.
const REGISTRY: &str = "sqlanywhere_interests";

/// Interests in the vectors of one column.
#[derive(Debug, Clone)]
pub struct Interests {
    table: String,
    column: String,
}

/// An interest a row's vector is within reach of.
#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub interest: String,
    pub channel: String,
    pub distance: f64,
}

impl Interests {
    /// Interests in `table.column`, a vector column (`FLOAT32(n)` and the
    /// other vector types) of a table with a rowid.
    pub fn on(table: &str, column: &str) -> Self {
        Interests {
            table: table.to_string(),
            column: column.to_string(),
        }
    }

    /// Create the pub/sub tables, the registry and the column's triggers.
    /// Idempotent.
    pub async fn install(&self, conn: &Connection) -> Result<()> {
        self.vector_type(conn).await?;
        let mut rows = conn
            .query(
                "SELECT wr FROM pragma_table_list WHERE name = ? AND schema = 'main'",
                params![self.table.as_str()],
            )
            .await?;
        if let Some(row) = rows.next().await? {
            if row.get::<i64>(0)? != 0 {
                return Err(self.misuse("needs a table with a rowid, not WITHOUT ROWID"));
            }
        }
        drop(rows);

        let tx = conn.transaction().await?;
        pubsub::install(&tx).await?;
        tx.execute(
            &format!(
                "CREATE TABLE IF NOT EXISTS {REGISTRY} (\
                 tbl TEXT NOT NULL, col TEXT NOT NULL, name TEXT NOT NULL, \
                 channel TEXT NOT NULL, max_distance REAL NOT NULL, vec BLOB NOT NULL, \
                 PRIMARY KEY (tbl, col, name))"
            ),
            (),
        )
        .await?;
        let (table, column) = (ident(&self.table), ident(&self.column));
        let publish = self.publish_sql();
        tx.execute(
            &format!(
                "CREATE TRIGGER IF NOT EXISTS {} AFTER INSERT ON {table} \
                 WHEN new.{column} IS NOT NULL BEGIN {publish}; END",
                ident(&self.trigger("ins"))
            ),
            (),
        )
        .await?;
        tx.execute(
            &format!(
                "CREATE TRIGGER IF NOT EXISTS {} AFTER UPDATE OF {column} ON {table} \
                 WHEN new.{column} IS NOT NULL AND new.{column} IS NOT old.{column} \
                 BEGIN {publish}; END",
                ident(&self.trigger("upd"))
            ),
            (),
        )
        .await?;
        tx.commit().await
    }

    /// Drop the column's triggers and forget its interests. Events already
    /// published stay.
    pub async fn uninstall(&self, conn: &Connection) -> Result<()> {
        let tx = conn.transaction().await?;
        for kind in ["ins", "upd"] {
            tx.execute(
                &format!("DROP TRIGGER IF EXISTS {}", ident(&self.trigger(kind))),
                (),
            )
            .await?;
        }
        if has_registry(&tx).await? {
            tx.execute(
                &format!("DELETE FROM {REGISTRY} WHERE tbl = ? AND col = ?"),
                params![self.table.as_str(), self.column.as_str()],
            )
            .await?;
        }
        tx.commit().await
    }

    /// Publish on `channel` whenever a row's vector comes within
    /// `max_distance` of `vector`, a vector literal such as `'[0.1, 0.2]'`.
    /// Subscribing again under the same `name` replaces the interest.
    pub async fn subscribe(
        &self,
        conn: &Connection,
        name: &str,
        channel: &str,
        vector: &str,
        max_distance: f64,
    ) -> Result<()> {
        let (ctor, dims) = self.vector_type(conn).await?;
        let given = literal_dims(vector)
            .ok_or_else(|| self.misuse(&format!("`{vector}` is not a vector literal")))?;
        if given != dims {
            // A vector of the wrong length would never match; refuse it now
            // rather than leave an interest that silently never fires.
            return Err(self.misuse(&format!(
                "the interest has {given} dimensions, the column {dims}"
            )));
        }
        if !(0.0..=2.0).contains(&max_distance) {
            return Err(self.misuse("max_distance is a cosine distance, from 0 to 2"));
        }
        conn.execute(
            &format!(
                "INSERT OR REPLACE INTO {REGISTRY} (tbl, col, name, channel, max_distance, vec) \
                 VALUES (?, ?, ?, ?, ?, {ctor}(?))"
            ),
            params![
                self.table.as_str(),
                self.column.as_str(),
                name,
                channel,
                max_distance,
                vector
            ],
        )
        .await
        .map_err(|e| match e {
            Error::SqliteFailure(..) if e.to_string().contains("no such table") => {
                self.misuse("install it before subscribing")
            }
            e => e,
        })?;
        Ok(())
    }

    /// [`subscribe`](Self::subscribe) to what `text` means, as `embedder`
    /// sees it. Use the embedder that fills the column, or the distances
    /// compare vectors from different models.
    pub async fn subscribe_text<E: Embedder + ?Sized>(
        &self,
        conn: &Connection,
        name: &str,
        channel: &str,
        embedder: &E,
        text: &str,
        max_distance: f64,
    ) -> Result<()> {
        let vector = crate::to_vector_literal(&embedder.embed(text));
        self.subscribe(conn, name, channel, &vector, max_distance)
            .await
    }

    /// Stop publishing for `name`. Returns whether it existed.
    pub async fn unsubscribe(&self, conn: &Connection, name: &str) -> Result<bool> {
        if !has_registry(conn).await? {
            return Ok(false);
        }
        let n = conn
            .execute(
                &format!("DELETE FROM {REGISTRY} WHERE tbl = ? AND col = ? AND name = ?"),
                params![self.table.as_str(), self.column.as_str(), name],
            )
            .await?;
        Ok(n > 0)
    }

    /// The interests the row at `rowid` is within reach of now, nearest first:
    /// what a write of its vector would publish. For inspection, and for
    /// matching rows that were written before an interest existed.
    pub async fn matching(&self, conn: &Connection, rowid: i64) -> Result<Vec<Match>> {
        if !has_registry(conn).await? {
            return Ok(Vec::new());
        }
        let mut rows = conn
            .query(
                &format!(
                    "SELECT i.name, i.channel, i.d FROM ({}) AS i \
                     WHERE i.d <= i.max_distance ORDER BY i.d, i.name",
                    self.distances_sql(&format!(
                        "(SELECT {} FROM {} WHERE rowid = {rowid})",
                        ident(&self.column),
                        ident(&self.table)
                    ))
                ),
                (),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(Match {
                interest: row.get(0)?,
                channel: row.get(1)?,
                distance: row.get(2)?,
            });
        }
        Ok(out)
    }

    /// The trigger body: one event per interest within reach of `new`'s vector.
    fn publish_sql(&self) -> String {
        format!(
            "INSERT INTO askr_events (channel, payload) \
             SELECT i.channel, CAST(json_object('table', {}, 'column', {}, 'rowid', new.rowid, \
             'interest', i.name, 'distance', i.d) AS BLOB) \
             FROM ({}) AS i WHERE i.d <= i.max_distance ORDER BY i.d, i.name",
            literal(&self.table),
            literal(&self.column),
            self.distances_sql(&format!("new.{}", ident(&self.column)))
        )
    }

    /// Every interest in this column with its distance to `vector`, an SQL
    /// expression. A value that is not a vector of the interest's size gets no
    /// distance, and matches nothing, rather than failing the write.
    fn distances_sql(&self, vector: &str) -> String {
        format!(
            "SELECT name, channel, max_distance, \
             CASE WHEN typeof({vector}) = 'blob' AND length({vector}) = length(vec) \
             THEN vector_distance_cos(vec, {vector}) END AS d \
             FROM {REGISTRY} WHERE tbl = {} AND col = {}",
            literal(&self.table),
            literal(&self.column)
        )
    }

    fn trigger(&self, kind: &str) -> String {
        format!("sqlanywhere_interest_{}_{}_{kind}", self.table, self.column)
    }

    /// The SQL function that makes a vector of the column's type, and its
    /// dimensions.
    async fn vector_type(&self, conn: &Connection) -> Result<(&'static str, usize)> {
        let mut rows = conn
            .query(
                "SELECT type FROM pragma_table_info(?) WHERE name = ?",
                params![self.table.as_str(), self.column.as_str()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(self.misuse("no such table or column"));
        };
        let decl = row.get::<String>(0)?.to_ascii_uppercase();
        let (base, dims) = decl
            .split_once('(')
            .and_then(|(base, rest)| {
                Some((base.trim(), rest.strip_suffix(')')?.trim().parse().ok()?))
            })
            .ok_or_else(|| self.misuse(&format!("`{decl}` is not a vector column type")))?;
        let ctor = match base {
            "FLOAT32" | "F32_BLOB" => "vector32",
            "FLOAT64" | "F64_BLOB" => "vector64",
            "FLOAT16" | "F16_BLOB" => "vector16",
            "FLOATB16" | "FB16_BLOB" => "vectorb16",
            "FLOAT8" | "F8_BLOB" => "vector8",
            "FLOAT1BIT" | "F1BIT_BLOB" => "vector1bit",
            _ => return Err(self.misuse(&format!("`{decl}` is not a vector column type"))),
        };
        Ok((ctor, dims))
    }

    fn misuse(&self, what: &str) -> Error {
        Error::Misuse(format!(
            "interests on {}.{}: {what}",
            self.table, self.column
        ))
    }
}

/// The number of elements in a `[a, b, c]` literal.
fn literal_dims(vector: &str) -> Option<usize> {
    let inner = vector.trim().strip_prefix('[')?.strip_suffix(']')?.trim();
    if inner.is_empty() {
        return Some(0);
    }
    inner
        .split(',')
        .map(|x| x.trim().parse::<f64>().ok())
        .collect::<Option<Vec<_>>>()
        .map(|v| v.len())
}

async fn has_registry(conn: &Connection) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?",
            params![REGISTRY],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_dims_counts_elements() {
        assert_eq!(literal_dims("[1, 2.5, -3e-2]"), Some(3));
        assert_eq!(literal_dims(" [0] "), Some(1));
        assert_eq!(literal_dims("[]"), Some(0));
        assert_eq!(literal_dims("1, 2"), None);
        assert_eq!(literal_dims("[1, x]"), None);
    }
}
