//! Three-way merge of a database with a branch of it, row by row: no CRDT, no
//! history, ordinary tables.
//!
//! A branch here is a copy of the database taken together with a frozen copy
//! of the same moment, its base. On `sqld`,
//! `POST /v1/namespaces/:ns/branch/:name` makes both (`name@base` and
//! `name`); locally, two copies of the file do. Work happens on the branch;
//! [`Merge3::diff`] then says what the branch changed since the base, what
//! the live database changed meanwhile, and where the two collide, and
//! [`Merge3::merge`] applies the branch's changes to the live database in one
//! transaction.
//!
//! ```rust,no_run
//! # async fn run(conn: sqlanywhere::Connection) -> sqlanywhere::Result<()> {
//! use sqlanywhere::merge3::{Merge3, Merged};
//!
//! // `conn` is the live namespace; the branch and its base were made with
//! // POST /v1/namespaces/docs/branch/agent-7.
//! let merge = Merge3::new("agent-7@base", "agent-7");
//! let plan = merge.diff(&conn).await?;
//! println!("{} changes, {} conflicts", plan.changes(), plan.conflicts());
//! match merge.merge(&conn).await? {
//!     Merged::Applied(plan) => println!("merged {} rows", plan.changes()),
//!     Merged::Refused(plan) => println!("refused: {:?}", plan.schema_changes),
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Rows are matched by primary key, or rowid. A row the branch changed and the
//! live database did not is applied. A row both changed is a conflict, unless
//! both made it the same, or both updated it in different columns, in which
//! case the branch's columns are applied beside the live ones. The base and
//! the branch are attached for the duration (`ATTACH` by name: a namespace on
//! `sqld`, a file path locally), so the comparison and the copying run as SQL
//! where the data is.
//!
//! Merged tables are the ordinary tables of the base and the branch. Left out
//! are SQLite's own, this crate's (`sqlanywhere_*`), cr-sqlite's, virtual
//! tables and their shadow tables (an FTS5 index), and any named with
//! [`Merge3::skip`]: a table that triggers fill (an audit log, a search
//! index kept by triggers) should be skipped too, since applying the rows it
//! triggers from fills it again on the live side.

use std::collections::HashMap;

use crate::compare::{self, ColumnDiff, RowDiff, RowKind};
use crate::{Connection, Error, Result, TransactionBehavior, Value};

const BASE: &str = "sqlanywhere_merge_base";
const BRANCH: &str = "sqlanywhere_merge_branch";

/// What to do with a row both sides changed differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnConflict {
    /// Apply nothing, and return the conflicts.
    #[default]
    Abort,
    /// Leave the live row as it is.
    KeepMain,
    /// Make the live row what the branch has: replaced, or deleted.
    TakeBranch,
}

/// A three-way merge of `branch` into the connection's database, from `base`.
#[derive(Debug, Clone)]
pub struct Merge3 {
    base: String,
    branch: String,
    skip: Vec<String>,
    on_conflict: OnConflict,
}

/// What a branch changed, table by table, and what it collides with.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub tables: Vec<TablePlan>,
    /// Tables created, dropped or given different columns on either side
    /// since the base. A merge with any is refused: rows are copied column by
    /// column, which needs the same columns on all three.
    pub schema_changes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TablePlan {
    pub table: String,
    /// The columns rows are matched by: the primary key, or `["rowid"]`.
    pub key: Vec<String>,
    /// The branch's changes that apply cleanly, as differences from the base.
    /// An update lists only the columns to write.
    pub changes: Vec<RowDiff>,
    pub conflicts: Vec<Conflict>,
}

/// A row both sides changed differently since the base.
#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    /// How the branch changed it.
    pub branch: RowDiff,
    /// How the live database changed it.
    pub main: RowDiff,
}

/// What [`Merge3::merge`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum Merged {
    /// The changes were applied, and the conflicts resolved as asked.
    Applied(Plan),
    /// Nothing was applied: there are conflicts and [`OnConflict::Abort`], or
    /// schema changes.
    Refused(Plan),
}

impl Plan {
    /// Rows that apply cleanly.
    pub fn changes(&self) -> usize {
        self.tables.iter().map(|t| t.changes.len()).sum()
    }

    /// Rows both sides changed differently.
    pub fn conflicts(&self) -> usize {
        self.tables.iter().map(|t| t.conflicts.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.changes() == 0 && self.conflicts() == 0 && self.schema_changes.is_empty()
    }

    pub fn table(&self, table: &str) -> Option<&TablePlan> {
        self.tables.iter().find(|t| t.table == table)
    }
}

impl Merge3 {
    /// `base` and `branch` are what `ATTACH` takes: namespace names on
    /// `sqld`, file paths for a local database.
    pub fn new(base: impl Into<String>, branch: impl Into<String>) -> Self {
        Merge3 {
            base: base.into(),
            branch: branch.into(),
            skip: Vec::new(),
            on_conflict: OnConflict::Abort,
        }
    }

    /// Leave `table` out of the comparison and the merge.
    pub fn skip(mut self, table: impl Into<String>) -> Self {
        self.skip.push(table.into());
        self
    }

    pub fn on_conflict(mut self, on_conflict: OnConflict) -> Self {
        self.on_conflict = on_conflict;
        self
    }

    /// What a merge would do now. Changes nothing.
    pub async fn diff(&self, conn: &Connection) -> Result<Plan> {
        let tx = conn.transaction().await?;
        let plan = match self.attach(&tx).await {
            Ok(()) => self.plan(&tx).await,
            Err(e) => Err(e),
        };
        tx.rollback().await?;
        detach(conn).await;
        plan
    }

    /// Apply the branch's changes in one `IMMEDIATE` transaction, so the
    /// live database cannot move between the comparison and the writes.
    /// Foreign keys are checked at the commit, so the order rows are written
    /// in does not matter.
    pub async fn merge(&self, conn: &Connection) -> Result<Merged> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let outcome = match self.attach(&tx).await {
            Ok(()) => self.merge_in(&tx).await,
            Err(e) => Err(e),
        };
        match outcome {
            Ok(Merged::Applied(plan)) => {
                tx.commit().await?;
                detach(conn).await;
                Ok(Merged::Applied(plan))
            }
            other => {
                tx.rollback().await?;
                detach(conn).await;
                other
            }
        }
    }

    async fn merge_in(&self, conn: &Connection) -> Result<Merged> {
        let plan = self.plan(conn).await?;
        if !plan.schema_changes.is_empty()
            || (plan.conflicts() > 0 && self.on_conflict == OnConflict::Abort)
        {
            return Ok(Merged::Refused(plan));
        }
        conn.execute("PRAGMA defer_foreign_keys = ON", ()).await?;
        for table in &plan.tables {
            let columns = columns(conn, BRANCH, &table.table).await?;
            let apply = Apply {
                conn,
                table: &table.table,
                key: &table.key,
                columns: &columns,
            };
            for kind in [RowKind::Deleted, RowKind::Updated, RowKind::Inserted] {
                for row in table.changes.iter().filter(|r| r.kind == kind) {
                    apply.row(row).await?;
                }
            }
            if self.on_conflict == OnConflict::TakeBranch {
                for conflict in &table.conflicts {
                    apply.take(&conflict.branch).await?;
                }
            }
        }
        Ok(Merged::Applied(plan))
    }

    async fn attach(&self, conn: &Connection) -> Result<()> {
        // On sqld, ATTACH holds only inside a transaction, and that is where
        // this runs; a name is a namespace there and a path locally.
        conn.execute(&format!("ATTACH {} AS {BASE}", ident(&self.base)), ())
            .await?;
        conn.execute(&format!("ATTACH {} AS {BRANCH}", ident(&self.branch)), ())
            .await?;
        Ok(())
    }

    async fn plan(&self, conn: &Connection) -> Result<Plan> {
        let base = self.tables(conn, BASE).await?;
        let branch = self.tables(conn, BRANCH).await?;
        let main = self.tables(conn, "main").await?;
        let mut plan = Plan::default();
        for t in &branch {
            if !base.contains(t) {
                plan.schema_changes
                    .push(format!("{t}: created on the branch"));
            }
        }
        for t in &base {
            if !branch.contains(t) {
                plan.schema_changes
                    .push(format!("{t}: dropped on the branch"));
                continue;
            }
            if !main.contains(t) {
                plan.schema_changes.push(format!("{t}: dropped on main"));
                continue;
            }
            let ours = compare::table(conn, t, BASE, BRANCH).await?;
            let theirs = compare::table(conn, t, BASE, "main").await?;
            let mut changed = false;
            for (side, diff) in [("branch", &ours), ("main", &theirs)] {
                for c in &diff.added_columns {
                    plan.schema_changes
                        .push(format!("{t}: column {c} added on {side}"));
                    changed = true;
                }
                for c in &diff.dropped_columns {
                    plan.schema_changes
                        .push(format!("{t}: column {c} dropped on {side}"));
                    changed = true;
                }
            }
            if changed || ours.is_empty() {
                continue;
            }
            plan.tables.push(three_way(ours, &theirs));
        }
        Ok(plan)
    }

    /// The tables of `schema` that take part, by name.
    async fn tables(&self, conn: &Connection, schema: &str) -> Result<Vec<String>> {
        let mut rows = conn
            .query(
                "SELECT name FROM pragma_table_list WHERE schema = ? AND type = 'table' \
                 AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
                 AND name NOT LIKE 'sqlanywhere\\_%' ESCAPE '\\' \
                 AND name NOT LIKE '%\\_\\_crsql\\_%' ESCAPE '\\' \
                 ORDER BY name",
                [schema],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let name: String = row.get(0)?;
            if !self.skip.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
                out.push(name);
            }
        }
        Ok(out)
    }
}

/// Sort the branch's changes to one table into those that apply and those
/// that collide with what the live side did.
fn three_way(ours: compare::TableDiff, theirs: &compare::TableDiff) -> TablePlan {
    let by_key: HashMap<String, &RowDiff> = theirs
        .rows
        .iter()
        .map(|row| (key_of(&row.key), row))
        .collect();
    let mut changes = Vec::new();
    let mut conflicts = Vec::new();
    for row in ours.rows {
        let Some(main) = by_key.get(&key_of(&row.key)) else {
            changes.push(row);
            continue;
        };
        match (row.kind, main.kind) {
            (RowKind::Deleted, RowKind::Deleted) => {}
            (RowKind::Inserted, RowKind::Inserted) if afters(&row) == afters(main) => {}
            (RowKind::Updated, RowKind::Updated) => {
                // Columns both wrote must agree; the branch's other columns
                // are written beside the live side's.
                let mut clash = false;
                let mut columns = Vec::new();
                for column in &row.columns {
                    match main.column(&column.column) {
                        Some(theirs) if theirs.after == column.after => {}
                        Some(_) => clash = true,
                        None => columns.push(column.clone()),
                    }
                }
                if clash {
                    conflicts.push(Conflict {
                        branch: row,
                        main: (*main).clone(),
                    });
                } else if !columns.is_empty() {
                    changes.push(RowDiff { columns, ..row });
                }
            }
            _ => conflicts.push(Conflict {
                branch: row,
                main: (*main).clone(),
            }),
        }
    }
    TablePlan {
        table: ours.table,
        key: ours.key,
        changes,
        conflicts,
    }
}

fn afters(row: &RowDiff) -> Vec<(&str, &Option<Value>)> {
    row.columns
        .iter()
        .map(|c: &ColumnDiff| (c.column.as_str(), &c.after))
        .collect()
}

/// A key as a string that is equal exactly when the keys are.
fn key_of(key: &[Value]) -> String {
    format!("{key:?}")
}

/// Writes one table's rows from the attached branch into `main`.
struct Apply<'a> {
    conn: &'a Connection,
    table: &'a str,
    key: &'a [String],
    columns: &'a [String],
}

impl Apply<'_> {
    async fn row(&self, row: &RowDiff) -> Result<()> {
        let t = ident(self.table);
        let sql = match row.kind {
            RowKind::Deleted => format!("DELETE FROM main.{t} WHERE {}", self.matches(1)),
            RowKind::Inserted => {
                let cols = self.all_columns();
                format!(
                    "INSERT INTO main.{t} ({cols}) SELECT {cols} FROM {BRANCH}.{t} WHERE {}",
                    self.matches(1)
                )
            }
            RowKind::Updated => {
                let cols = row
                    .columns
                    .iter()
                    .map(|c| ident(&c.column))
                    .collect::<Vec<_>>()
                    .join(", ");
                let n = self.key.len();
                format!(
                    "UPDATE main.{t} SET ({cols}) = (SELECT {cols} FROM {BRANCH}.{t} WHERE {}) \
                     WHERE {}",
                    self.matches(1),
                    self.matches(n + 1)
                )
            }
        };
        self.run(&sql, &row.key, row.kind == RowKind::Updated).await
    }

    /// Make the live row what the branch has.
    async fn take(&self, branch: &RowDiff) -> Result<()> {
        if branch.kind == RowKind::Deleted {
            return self.row(branch).await;
        }
        let t = ident(self.table);
        let cols = self.all_columns();
        let sql = format!(
            "INSERT OR REPLACE INTO main.{t} ({cols}) SELECT {cols} FROM {BRANCH}.{t} WHERE {}",
            self.matches(1)
        );
        self.run(&sql, &branch.key, false).await
    }

    async fn run(&self, sql: &str, key: &[Value], twice: bool) -> Result<()> {
        let mut params = key.to_vec();
        if twice {
            params.extend(key.iter().cloned());
        }
        let changed = self.conn.execute(sql, params).await?;
        if changed != 1 {
            return Err(Error::Misuse(format!(
                "merge3: writing {:?} of {} changed {changed} rows, not 1",
                key, self.table
            )));
        }
        Ok(())
    }

    /// `k1 IS ?n AND k2 IS ?n+1 …`, unqualified: in a subquery it names the
    /// branch's row, outside it the live one.
    fn matches(&self, first: usize) -> String {
        self.key
            .iter()
            .enumerate()
            .map(|(i, k)| format!("{} IS ?{}", ident(k), first + i))
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    /// Every column, and the rowid when that is the key, so a row keeps it.
    fn all_columns(&self) -> String {
        let mut cols: Vec<String> = self.columns.iter().map(|c| ident(c)).collect();
        if self.key == ["rowid"] {
            cols.insert(0, "rowid".to_string());
        }
        cols.join(", ")
    }
}

async fn columns(conn: &Connection, schema: &str, table: &str) -> Result<Vec<String>> {
    let mut rows = conn
        .query("SELECT name FROM pragma_table_info(?, ?)", [table, schema])
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(row.get(0)?);
    }
    Ok(out)
}

async fn detach(conn: &Connection) {
    // A local transaction ran on `conn`, which keeps what it attached; a
    // remote one ran on a stream of its own, gone with it. Either way, SQLite
    // detaches only outside a transaction.
    for alias in [BASE, BRANCH] {
        let _ = conn.execute(&format!("DETACH {alias}"), ()).await;
    }
}

fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
