//! Compare a table with itself in another schema: what was inserted, updated
//! and deleted, and how far each embedding moved.
//!
//! The other schema is anything `ATTACH` can reach: the database as it was at
//! a moment (`POST /v1/namespaces/:ns/at`, see `docs/TIME_TRAVEL.md`), an older
//! copy of the file, a staging database. Nothing has to be prepared in
//! advance: the tables need not be CRRs and no history is kept; the comparison
//! is a few SQL queries over both versions of the table, so it runs wherever
//! the connection's SQL runs, a local file or a `sqld` namespace alike.
//!
//! ```rust,no_run
//! # async fn run(conn: sqlanywhere::Connection) -> sqlanywhere::Result<()> {
//! use sqlanywhere::compare;
//!
//! // On sqld, ATTACH holds only inside a transaction: compare on that.
//! let tx = conn.transaction().await?;
//! tx.execute(r#"ATTACH "docs@20261021T120000Z" AS past"#, ()).await?;
//! let diff = compare::table(&tx, "articles", "past", "main").await?;
//! tx.commit().await?;
//!
//! // Which articles changed meaning most since then?
//! for (row, distance) in diff.moved("embedding").iter().take(10) {
//!     println!("{:?} moved {distance:.3}", row.key);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Rows are matched by the primary key of the table in `after`, or by rowid
//! when it has none. Columns are compared by name; a column only one side has
//! is listed in [`TableDiff::added_columns`] or [`TableDiff::dropped_columns`]
//! and not compared.

use crate::{params, Connection, Error, Result, Value};

/// How a row differs between the two versions of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// Only in `after`.
    Inserted,
    /// In both, with at least one compared column different.
    Updated,
    /// Only in `before`.
    Deleted,
}

/// One column of a changed row.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDiff {
    pub column: String,
    /// `None` for an inserted row.
    pub before: Option<Value>,
    /// `None` for a deleted row.
    pub after: Option<Value>,
    /// Cosine distance between the two vectors, for an updated vector column
    /// (`FLOAT32(n)`, `F32_BLOB(n)` and the other vector types) that holds a
    /// vector on both sides.
    pub vector_distance: Option<f64>,
}

/// One row that differs.
#[derive(Debug, Clone, PartialEq)]
pub struct RowDiff {
    /// Key values, in key order (see [`TableDiff::key`]).
    pub key: Vec<Value>,
    pub kind: RowKind,
    /// Every compared column for an inserted or deleted row; only the columns
    /// that differ for an updated one.
    pub columns: Vec<ColumnDiff>,
}

impl RowDiff {
    /// The change to `column`, if this row has one.
    pub fn column(&self, column: &str) -> Option<&ColumnDiff> {
        self.columns.iter().find(|c| c.column == column)
    }
}

/// The difference between two versions of a table.
#[derive(Debug, Clone, PartialEq)]
pub struct TableDiff {
    pub table: String,
    /// The columns rows are matched by: the primary key, or `["rowid"]`.
    pub key: Vec<String>,
    /// Inserted, then updated, then deleted rows, each in key order.
    pub rows: Vec<RowDiff>,
    /// Columns only `after` has.
    pub added_columns: Vec<String>,
    /// Columns only `before` has.
    pub dropped_columns: Vec<String>,
}

impl TableDiff {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Rows of one kind.
    pub fn of_kind(&self, kind: RowKind) -> impl Iterator<Item = &RowDiff> {
        self.rows.iter().filter(move |r| r.kind == kind)
    }

    /// Updated rows whose vector in `column` moved, furthest first: the rows
    /// whose meaning changed most.
    pub fn moved(&self, column: &str) -> Vec<(&RowDiff, f64)> {
        let mut out: Vec<_> = self
            .rows
            .iter()
            .filter_map(|r| Some((r, r.column(column)?.vector_distance?)))
            .collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1));
        out
    }
}

/// Compare `table` in schema `before` with `table` in schema `after`.
///
/// Both are schema names as `ATTACH` gives them: `main`, `temp`, or an alias.
/// Reads both versions in full, so it suits the tables an application keeps,
/// not a billion-row archive.
pub async fn table(conn: &Connection, table: &str, before: &str, after: &str) -> Result<TableDiff> {
    let cols_after = columns(conn, after, table).await?;
    let cols_before = columns(conn, before, table).await?;
    let named = |cols: &[Column]| cols.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
    let (names_after, names_before) = (named(&cols_after), named(&cols_before));

    let mut key: Vec<_> = cols_after.iter().filter(|c| c.pk > 0).collect();
    key.sort_by_key(|c| c.pk);
    let key: Vec<String> = if key.is_empty() {
        vec!["rowid".to_string()]
    } else {
        key.into_iter().map(|c| c.name.clone()).collect()
    };
    if let Some(missing) = key
        .iter()
        .find(|k| *k != "rowid" && !names_before.contains(k))
    {
        return Err(Error::Misuse(format!(
            "compare::table: key column `{missing}` of {after}.{table} is missing in {before}"
        )));
    }

    let compared: Vec<&Column> = cols_after
        .iter()
        .filter(|c| names_before.contains(&c.name))
        .collect();
    let added_columns = names_after
        .iter()
        .filter(|n| !names_before.contains(n))
        .cloned()
        .collect();
    let dropped_columns = names_before
        .iter()
        .filter(|n| !names_after.contains(n))
        .cloned()
        .collect();

    let (b, a) = (
        format!("{}.{}", ident(before), ident(table)),
        format!("{}.{}", ident(after), ident(table)),
    );
    let on = key
        .iter()
        .map(|k| format!("o.{0} IS n.{0}", ident(k)))
        .collect::<Vec<_>>()
        .join(" AND ");
    let key_list = |alias: &str| {
        key.iter()
            .map(|k| format!("{alias}.{}", ident(k)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let col_list = |alias: &str| {
        compared
            .iter()
            .map(|c| format!("{alias}.{}", ident(&c.name)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let nk = key.len();
    let mut rows = Vec::new();

    // Only in after, only in before.
    for (kind, from, other, alias) in [
        (RowKind::Inserted, &a, &b, "n"),
        (RowKind::Deleted, &b, &a, "o"),
    ] {
        let missing = if alias == "n" { "o" } else { "n" };
        let sql = format!(
            "SELECT {}, {} FROM {from} AS {alias} \
             WHERE NOT EXISTS (SELECT 1 FROM {other} AS {missing} WHERE {on}) \
             ORDER BY {}",
            key_list(alias),
            col_list(alias),
            key_list(alias)
        );
        for row in all(conn, &sql).await? {
            let (key_vals, vals) = row.split_at(nk);
            rows.push((
                kind,
                key_vals.to_vec(),
                compared
                    .iter()
                    .zip(vals)
                    .map(|(c, v)| ColumnDiff {
                        column: c.name.clone(),
                        before: (kind == RowKind::Deleted).then(|| v.clone()),
                        after: (kind == RowKind::Inserted).then(|| v.clone()),
                        vector_distance: None,
                    })
                    .collect::<Vec<_>>(),
            ));
        }
    }

    // In both, with something different.
    let differs = compared
        .iter()
        .map(|c| format!("o.{0} IS NOT n.{0}", ident(&c.name)))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut updated = Vec::new();
    if !compared.is_empty() {
        let sql = format!(
            "SELECT {}, {}, {} FROM {a} AS n JOIN {b} AS o ON {on} WHERE {differs} ORDER BY {}",
            key_list("n"),
            col_list("o"),
            col_list("n"),
            key_list("n")
        );
        for row in all(conn, &sql).await? {
            let (key_vals, rest) = row.split_at(nk);
            let (olds, news) = rest.split_at(compared.len());
            let mut cols = Vec::new();
            for ((c, old), new) in compared.iter().zip(olds).zip(news) {
                if old == new {
                    continue;
                }
                let vector_distance = match (c.is_vector(), old, new) {
                    (true, Value::Blob(x), Value::Blob(y)) => distance(conn, x, y).await,
                    _ => None,
                };
                cols.push(ColumnDiff {
                    column: c.name.clone(),
                    before: Some(old.clone()),
                    after: Some(new.clone()),
                    vector_distance,
                });
            }
            updated.push((RowKind::Updated, key_vals.to_vec(), cols));
        }
    }
    // Inserted, updated, deleted.
    let deleted = rows.split_off(rows.iter().take_while(|r| r.0 == RowKind::Inserted).count());
    rows.extend(updated);
    rows.extend(deleted);

    Ok(TableDiff {
        table: table.to_string(),
        key,
        rows: rows
            .into_iter()
            .map(|(kind, key, columns)| RowDiff { key, kind, columns })
            .collect(),
        added_columns,
        dropped_columns,
    })
}

struct Column {
    name: String,
    decl: String,
    pk: i64,
}

impl Column {
    fn is_vector(&self) -> bool {
        let t = self.decl.to_ascii_uppercase();
        t.contains('(') && (t.starts_with("FLOAT") || t.ends_with("_BLOB") || t.contains("_BLOB("))
    }
}

async fn columns(conn: &Connection, schema: &str, table: &str) -> Result<Vec<Column>> {
    let mut rows = conn
        .query(
            "SELECT name, type, pk FROM pragma_table_info(?, ?)",
            params![table, schema],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(Column {
            name: row.get(0)?,
            decl: row.get(1)?,
            pk: row.get(2)?,
        });
    }
    if out.is_empty() {
        return Err(Error::Misuse(format!(
            "compare::table: no table `{table}` in schema `{schema}`"
        )));
    }
    Ok(out)
}

async fn distance(conn: &Connection, a: &[u8], b: &[u8]) -> Option<f64> {
    let mut rows = conn
        .query(
            "SELECT vector_distance_cos(?, ?)",
            params![a.to_vec(), b.to_vec()],
        )
        .await
        .ok()?;
    rows.next().await.ok()??.get::<f64>(0).ok()
}

async fn all(conn: &Connection, sql: &str) -> Result<Vec<Vec<Value>>> {
    let mut rows = conn.query(sql, ()).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(
            (0..row.column_count())
                .map(|i| row.get_value(i))
                .collect::<Result<_>>()?,
        );
    }
    Ok(out)
}

fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_columns_are_recognised_by_their_declared_type() {
        let col = |decl: &str| Column {
            name: "c".into(),
            decl: decl.into(),
            pk: 0,
        };
        for decl in ["FLOAT32(384)", "float8(16)", "F32_BLOB(3)", "FLOAT1BIT(64)"] {
            assert!(col(decl).is_vector(), "{decl}");
        }
        for decl in ["FLOAT", "BLOB", "TEXT", "INTEGER", ""] {
            assert!(!col(decl).is_vector(), "{decl}");
        }
    }
}
