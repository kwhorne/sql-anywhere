//! Live queries: a query result that keeps itself current and says what
//! changed.
//!
//! A [`LiveQuery`] runs a `SELECT` once and remembers the result. Each
//! [`refresh`](LiveQuery::refresh) first asks SQLite whether anything could have
//! changed since, which costs one pragma, and only then re-runs the query and
//! compares. What comes back is a [`Diff`] of rows inserted, updated and
//! deleted, or nothing at all when the result is the same, so a UI can render
//! exactly the rows that moved.
//!
//! Changes are noticed wherever they come from:
//!
//! - writes on the same connection, through `sqlite3_total_changes`;
//! - commits by any other connection to the same file, through
//!   `data_version`;
//! - frames an embedded replica pulls in with `Database::sync`, which reach the
//!   file the same way another connection's commit does.
//!
//! Both signals are needed. SQLite deliberately leaves `data_version` alone for
//! a connection's own commits, so a live query that watched only that would
//! miss every write made through the connection it runs on.
//!
//! ```rust
//! # async fn run() -> sqlanywhere::Result<()> {
//! use sqlanywhere::{live::LiveQuery, Builder};
//!
//! let db = Builder::new_local(":memory:").build().await?;
//! let conn = db.connect()?;
//! conn.execute("CREATE TABLE todo (id INTEGER PRIMARY KEY, title TEXT, done INTEGER)", ())
//!     .await?;
//!
//! let mut open = LiveQuery::new(&conn, "SELECT id, title FROM todo WHERE done = 0", ())
//!     .await?
//!     .keyed_by(&[0]);
//!
//! conn.execute("INSERT INTO todo VALUES (1, 'write docs', 0)", ()).await?;
//! if let Some(diff) = open.refresh().await? {
//!     assert_eq!(diff.inserted.len(), 1);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Live queries need the database file in this process: a local database or an
//! embedded replica. A purely remote connection has no file to watch.

use std::collections::HashMap;

use crate::params::{IntoParams, Params};
use crate::{Connection, Error, Result, Value};

/// A query whose result is kept current by [`refresh`](LiveQuery::refresh).
pub struct LiveQuery {
    conn: Connection,
    sql: String,
    params: Params,
    key: Option<Vec<usize>>,
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    seen: Seen,
}

/// How the result changed between two refreshes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Diff {
    /// Rows that are new to the result.
    pub inserted: Vec<Vec<Value>>,
    /// Rows whose key stayed but some other column changed, as (before,
    /// after). Only a keyed query reports updates; without a key an edited row
    /// is a deletion and an insertion.
    pub updated: Vec<(Vec<Value>, Vec<Value>)>,
    /// Rows that left the result.
    pub deleted: Vec<Vec<Value>>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.inserted.is_empty() && self.updated.is_empty() && self.deleted.is_empty()
    }
}

/// The change counters as of the last run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen {
    data_version: i64,
    total_changes: u64,
}

impl LiveQuery {
    /// Run `sql` and start watching it. The connection is cloned, so the live
    /// query sees the writes made through `conn` itself.
    pub async fn new(conn: &Connection, sql: &str, params: impl IntoParams) -> Result<Self> {
        let mut live = LiveQuery {
            conn: conn.clone(),
            sql: sql.to_string(),
            params: params.into_params()?,
            key: None,
            columns: Vec::new(),
            rows: Vec::new(),
            seen: Seen {
                data_version: 0,
                total_changes: 0,
            },
        };
        live.seen = live.counters().await?;
        let (columns, rows) = live.run().await?;
        live.columns = columns;
        live.rows = rows;
        Ok(live)
    }

    /// Identify rows by the columns at `columns` (zero-based), so a changed row
    /// is reported as an update rather than a deletion plus an insertion.
    ///
    /// The key must be unique within the result, as a primary key is. Rows
    /// that share a key are compared as a group, without updates.
    pub fn keyed_by(mut self, columns: &[usize]) -> Self {
        self.key = Some(columns.to_vec());
        self
    }

    /// Column names of the result.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// The result as of the last refresh, in the query's order.
    pub fn rows(&self) -> &[Vec<Value>] {
        &self.rows
    }

    /// Whether anything that could change the result has been written since
    /// the last refresh. One pragma; it does not run the query. A `true` can
    /// still lead to an empty refresh, when the write did not touch this
    /// result.
    pub async fn is_stale(&self) -> Result<bool> {
        Ok(self.counters().await? != self.seen)
    }

    /// Bring the result up to date. Returns `None` without running the query
    /// when nothing has been written since the last refresh, and `None` after
    /// running it when the result did not change.
    pub async fn refresh(&mut self) -> Result<Option<Diff>> {
        let now = self.counters().await?;
        if now == self.seen {
            return Ok(None);
        }
        // Read the counters before the query, so a write landing while it
        // runs is seen as a change by the next refresh rather than lost.
        self.seen = now;
        let (columns, rows) = self.run().await?;
        let diff = match &self.key {
            Some(key) => keyed_diff(&self.rows, &rows, key)?,
            None => bag_diff(&self.rows, &rows),
        };
        self.columns = columns;
        self.rows = rows;
        Ok((!diff.is_empty()).then_some(diff))
    }

    async fn counters(&self) -> Result<Seen> {
        // The table-valued form, not `PRAGMA data_version`: an embedded
        // replica classifies that pragma as a write and sends it to the
        // primary, which answers with the primary's counter for a fresh stream
        // and turns this one-pragma check into a network round trip. A SELECT
        // is served from the replica's own file.
        let mut rows = self
            .conn
            .query("SELECT data_version FROM pragma_data_version()", ())
            .await?;
        let data_version = match rows.next().await? {
            Some(row) => row.get::<i64>(0)?,
            None => {
                return Err(Error::Misuse(
                    "data_version returned nothing; live queries need a local database".into(),
                ))
            }
        };
        Ok(Seen {
            data_version,
            total_changes: self.conn.total_changes(),
        })
    }

    async fn run(&self) -> Result<(Vec<String>, Vec<Vec<Value>>)> {
        let mut rows = self.conn.query(&self.sql, self.params.clone()).await?;
        let columns = (0..rows.column_count())
            .map(|i| rows.column_name(i).unwrap_or_default().to_string())
            .collect();
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(
                (0..row.column_count())
                    .map(|i| row.get_value(i))
                    .collect::<Result<_>>()?,
            );
        }
        Ok((columns, out))
    }
}

/// A byte encoding of values that is equal exactly when the values are, so
/// rows can be hashed. `Value` holds an `f64`, so it is not `Hash` itself.
fn encode(values: impl IntoIterator<Item = impl std::borrow::Borrow<Value>>) -> Vec<u8> {
    let mut out = Vec::new();
    for v in values {
        match v.borrow() {
            Value::Null => out.push(0),
            Value::Integer(i) => {
                out.push(1);
                out.extend_from_slice(&i.to_le_bytes());
            }
            Value::Real(r) => {
                out.push(2);
                out.extend_from_slice(&r.to_bits().to_le_bytes());
            }
            Value::Text(t) => {
                out.push(3);
                out.extend_from_slice(&(t.len() as u64).to_le_bytes());
                out.extend_from_slice(t.as_bytes());
            }
            Value::Blob(b) => {
                out.push(4);
                out.extend_from_slice(&(b.len() as u64).to_le_bytes());
                out.extend_from_slice(b);
            }
        }
    }
    out
}

/// Compare two results as multisets: a row present more often now is
/// inserted, less often deleted.
fn bag_diff(old: &[Vec<Value>], new: &[Vec<Value>]) -> Diff {
    let mut count: HashMap<Vec<u8>, i64> = HashMap::new();
    for row in old {
        *count.entry(encode(row)).or_default() -= 1;
    }
    for row in new {
        *count.entry(encode(row)).or_default() += 1;
    }
    let mut diff = Diff::default();
    for row in new {
        let c = count.get_mut(&encode(row)).unwrap();
        if *c > 0 {
            *c -= 1;
            diff.inserted.push(row.clone());
        }
    }
    for row in old {
        let c = count.get_mut(&encode(row)).unwrap();
        if *c < 0 {
            *c += 1;
            diff.deleted.push(row.clone());
        }
    }
    diff
}

/// Compare two results by key: a key only in the new result is inserted, only
/// in the old deleted, and in both with other columns changed updated.
fn keyed_diff(old: &[Vec<Value>], new: &[Vec<Value>], key: &[usize]) -> Result<Diff> {
    let key_of = |row: &[Value]| -> Result<Vec<u8>> {
        let cells = key
            .iter()
            .map(|&i| {
                row.get(i).ok_or_else(|| {
                    Error::Misuse(format!(
                        "live query key column {i} is out of range for a {}-column result",
                        row.len()
                    ))
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(encode(cells))
    };
    let mut before: HashMap<Vec<u8>, Vec<&Vec<Value>>> = HashMap::new();
    for row in old {
        before.entry(key_of(row)?).or_default().push(row);
    }
    let mut after: HashMap<Vec<u8>, Vec<&Vec<Value>>> = HashMap::new();
    for row in new {
        after.entry(key_of(row)?).or_default().push(row);
    }

    let mut diff = Diff::default();
    for row in new {
        let k = key_of(row)?;
        let was = before.get(&k).map(Vec::as_slice).unwrap_or_default();
        let now = &after[&k];
        if was.is_empty() {
            diff.inserted.push(row.clone());
        } else if was.len() == 1 && now.len() == 1 {
            if was[0] != row {
                diff.updated.push((was[0].clone(), row.clone()));
            }
        } else if now.first() == Some(&row) {
            // A key shared by several rows: compare that group as a multiset,
            // once, at its first row.
            let was: Vec<_> = was.iter().map(|r| (*r).clone()).collect();
            let now: Vec<_> = now.iter().map(|r| (*r).clone()).collect();
            let part = bag_diff(&was, &now);
            diff.inserted.extend(part.inserted);
            diff.deleted.extend(part.deleted);
        }
    }
    for row in old {
        if !after.contains_key(&key_of(row)?) {
            diff.deleted.push(row.clone());
        }
    }
    Ok(diff)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, title: &str) -> Vec<Value> {
        vec![Value::Integer(id), Value::Text(title.into())]
    }

    #[test]
    fn bag_diff_counts_duplicates() {
        let old = vec![row(1, "a"), row(1, "a"), row(2, "b")];
        let new = vec![row(1, "a"), row(3, "c"), row(3, "c")];
        let diff = bag_diff(&old, &new);
        assert_eq!(diff.inserted, vec![row(3, "c"), row(3, "c")]);
        assert_eq!(diff.deleted, vec![row(1, "a"), row(2, "b")]);
        assert!(diff.updated.is_empty());
    }

    #[test]
    fn keyed_diff_reports_updates() {
        let old = vec![row(1, "a"), row(2, "b"), row(3, "c")];
        let new = vec![row(2, "B"), row(3, "c"), row(4, "d")];
        let diff = keyed_diff(&old, &new, &[0]).unwrap();
        assert_eq!(diff.inserted, vec![row(4, "d")]);
        assert_eq!(diff.updated, vec![(row(2, "b"), row(2, "B"))]);
        assert_eq!(diff.deleted, vec![row(1, "a")]);
    }

    #[test]
    fn a_shared_key_is_compared_as_a_group() {
        let old = vec![row(1, "a"), row(1, "b")];
        let new = vec![row(1, "b"), row(1, "c")];
        let diff = keyed_diff(&old, &new, &[0]).unwrap();
        assert_eq!(diff.inserted, vec![row(1, "c")]);
        assert_eq!(diff.deleted, vec![row(1, "a")]);
        assert!(diff.updated.is_empty());
    }

    #[test]
    fn values_of_different_types_never_collide() {
        assert_ne!(
            encode([Value::Integer(1)]),
            encode([Value::Real(f64::from_bits(1))])
        );
        assert_ne!(
            encode([Value::Text("ab".into()), Value::Text("c".into())]),
            encode([Value::Text("a".into()), Value::Text("bc".into())])
        );
    }

    #[test]
    fn a_key_out_of_range_is_an_error() {
        assert!(keyed_diff(&[row(1, "a")], &[row(1, "a")], &[5]).is_err());
    }
}
