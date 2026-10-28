//! Partial branches: a branch that holds only the rows a lens selects.
//!
//! A [`Lens`] names, per tracked table, a SQL predicate over that table's
//! rows: relational (`project_id = ?`) or semantic
//! (`id IN (SELECT id FROM vector_top_k('docs_vec', vector32(?), 500))`).
//! [`fork_lens`] makes a branch with only those rows, each with its whole
//! history, so a laptop or an agent works on the part of a large database it
//! needs. The branch is otherwise an ordinary branch: write to it freely,
//! [`diff`](super::diff) and [`merge`](super::merge) it back, and
//! [`pull_lens`] keeps it current:
//!
//! - rows the branch holds receive every change `main` made since the branch
//!   last looked, whether or not they still match the lens, so a row
//!   deleted on `main` is deleted on the branch too;
//! - rows that match the lens now and the branch does not hold yet arrive with
//!   their whole history.
//!
//! A row that stops matching is kept, not evicted: removing it on the branch
//! would be a delete, and a merge would carry that delete back to `main`.
//! Tables a lens does not name are held in full, as by [`fork`](super::fork).

use std::collections::HashSet;

use super::*;
use crate::params::{IntoParams, Params};

/// Which rows of which tables a partial branch holds.
#[derive(Debug, Clone, Default)]
pub struct Lens {
    tables: Vec<(String, String, Vec<Value>)>,
}

impl Lens {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hold only the rows of `table` for which `predicate`, a SQL expression
    /// over its columns, is true. `params` bind the predicate's `?`s, in
    /// order; named parameters are not supported.
    pub fn rows(mut self, table: &str, predicate: &str, params: impl IntoParams) -> Result<Self> {
        let params = match params.into_params()? {
            Params::None => Vec::new(),
            Params::Positional(values) => values,
            Params::Named(_) => {
                return Err(Error::Misuse(
                    "branch::Lens: use positional parameters (?) in a predicate".into(),
                ))
            }
        };
        self.tables
            .push((table.to_string(), predicate.to_string(), params));
        Ok(self)
    }

    fn predicate(&self, table: &str) -> Option<(&str, &[Value])> {
        self.tables
            .iter()
            .find(|(t, _, _)| t == table)
            .map(|(_, p, v)| (p.as_str(), v.as_slice()))
    }

    /// Every table the lens names must be tracked on `main`: a lens on any
    /// other table could not be honoured.
    fn check(&self, tracked: &[String]) -> Result<()> {
        match self.tables.iter().find(|(t, _, _)| !tracked.contains(t)) {
            Some((t, _, _)) => Err(Error::Misuse(format!(
                "branch::Lens: `{t}` is not a tracked table (a CRR) on main"
            ))),
            None => Ok(()),
        }
    }
}

/// [`fork`](super::fork) into the empty database behind `branch`, holding
/// only the rows `lens` selects in the tables it names.
pub async fn fork_lens(main: &Connection, branch: &Connection, lens: &Lens) -> Result<BranchInfo> {
    lens.check(&tracked_tables(main).await?)?;
    fork_with(main, branch, Some(lens)).await
}

/// [`pull`](super::pull) for a partial branch: changes to the rows it holds,
/// and the rows that match `lens` now, which may be a different lens from the
/// one it was forked with.
pub async fn pull_lens(main: &Connection, branch: &Connection, lens: &Lens) -> Result<MergeReport> {
    let meta = Meta::read(branch).await?;
    meta.check_parent(main).await?;
    let ours = local_changes(branch, meta.merged_version).await?;

    let mut held = Vec::new();
    for (table, _, _) in &lens.tables {
        let pk = pack(branch, table).await?;
        let keys = all(
            branch,
            &format!("SELECT {pk} FROM {}", ident(&format!("{table}__crsql_pks"))),
            (),
        )
        .await?;
        held.push((
            table.clone(),
            keys.into_iter()
                .map(|r| blob(&r[0]))
                .collect::<Result<HashSet<_>>>()?,
        ));
    }

    let tx = main.transaction().await?;
    let tracked = tracked_tables(&tx).await?;
    lens.check(&tracked)?;
    let mut theirs = Vec::new();
    for table in &tracked {
        let since = table_changes(&tx, table, meta.base_version, &ours.site_id, None).await?;
        match (lens.predicate(table), held.iter().find(|(t, _)| t == table)) {
            (Some(predicate), Some((_, held))) => {
                // What the branch holds, since it last looked...
                theirs.extend(since.into_iter().filter(|c| held.contains(&c.pk)));
                // ...and what it does not hold yet, from the beginning. Not
                // the held rows' early history again: it would be reported
                // as conflicting with the branch's unmerged edits.
                let new = table_changes(&tx, table, 0, &ours.site_id, Some(predicate)).await?;
                theirs.extend(new.into_iter().filter(|c| !held.contains(&c.pk)));
            }
            _ => theirs.extend(since),
        }
    }
    let version = int(&one(&tx, "SELECT crsql_db_version()", ()).await?)?;
    tx.commit().await?;

    let tx = branch.transaction().await?;
    apply(&tx, &theirs).await?;
    set_meta(&tx, "base_version", Value::Integer(version)).await?;
    tx.commit().await?;

    let conflicts = resolve(branch, branch, conflicting(&ours.changes, &theirs)).await?;
    Ok(MergeReport {
        changes_applied: theirs.len(),
        effects_applied: 0,
        conflicts,
    })
}

/// The whole history of the rows `lens` selects, and of every row of the
/// tracked tables it does not name, table by table in the order written.
pub(super) async fn changes_in(
    conn: &Connection,
    tracked: &[String],
    lens: &Lens,
) -> Result<Vec<Change>> {
    lens.check(tracked)?;
    let mut out = Vec::new();
    for table in tracked {
        out.extend(table_changes(conn, table, 0, &[], lens.predicate(table)).await?);
    }
    Ok(out)
}

/// The changes to `table` after `version` from any site but `exclude`,
/// limited to the rows `predicate` selects now.
async fn table_changes(
    conn: &Connection,
    table: &str,
    version: i64,
    exclude: &[u8],
    predicate: Option<(&str, &[Value])>,
) -> Result<Vec<Change>> {
    let mut params = vec![
        Value::Text(table.to_string()),
        Value::Integer(version),
        Value::Blob(exclude.to_vec()),
    ];
    let mut sql = format!(
        "SELECT {CHANGE_COLUMNS} FROM crsql_changes \
         WHERE [table] = ? AND db_version > ? AND site_id IS NOT ?"
    );
    if let Some((predicate, values)) = predicate {
        sql.push_str(&format!(
            " AND pk IN (SELECT {} FROM {} WHERE {predicate})",
            pack(conn, table).await?,
            ident(table)
        ));
        params.extend(values.iter().cloned());
    }
    sql.push_str(" ORDER BY db_version, seq");
    all(conn, &sql, Params::Positional(params))
        .await?
        .into_iter()
        .map(Change::from_row)
        .collect()
}

/// `crsql_pack_columns` over `table`'s primary key, in key order: the `pk` of
/// its rows in `crsql_changes`.
async fn pack(conn: &Connection, table: &str) -> Result<String> {
    let cols = all(
        conn,
        "SELECT name FROM pragma_table_info(?) WHERE pk > 0 ORDER BY pk",
        params![table],
    )
    .await?
    .iter()
    .map(|r| text(&r[0]).map(|c| ident(&c)))
    .collect::<Result<Vec<_>>>()?;
    Ok(format!("crsql_pack_columns({})", cols.join(", ")))
}
