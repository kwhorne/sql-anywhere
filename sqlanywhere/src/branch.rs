//! Database branches: fork a database, change the fork freely, review a diff,
//! and merge it back conflict-free.
//!
//! A branch is an ordinary second database that knows which database it was
//! forked from. It is built on the cr-sqlite CRDT extension (see
//! `docs/CRDT.md`), so a branch is not a lock on its parent: the parent keeps
//! taking writes while the branch exists, and [`merge`] converges the two
//! deterministically instead of failing on a stale base. That makes a branch a
//! cheap sandbox for anything that should not write to the real data
//! unreviewed, such as an AI agent, a migration rehearsal, or a preview
//! environment:
//!
//! ```text
//! fork(main, branch)        branch gets main's schema and data
//!   … writes on branch …    main may keep changing too
//! diff(main, branch)        what the branch would change, and where main moved
//! merge(main, branch)       apply it; every conflict reports who won
//! ```
//!
//! Both connections must have the cr-sqlite extension loaded, and only tables
//! that are conflict-free replicated relations on `main`
//! (`SELECT crsql_as_crr('t')`) are tracked. Other tables are copied into the
//! branch once, at fork time, and are listed as `untracked_tables`: changes to
//! them are never diffed or merged.
//!
//! The functions take connections rather than paths, so the branch can live
//! wherever the caller opens it: a file next to the parent, a temp file, or
//! `:memory:`.

use std::collections::HashMap;

use crate::params::params_from_iter;
use crate::{params, Connection, Error, Result, Value};

/// Name of the table a branch keeps its bookkeeping in.
const META_TABLE: &str = "sqlanywhere_branch";

/// cr-sqlite's column id for "the row itself" (creation or deletion).
const ROW_SENTINEL: &str = "-1";

/// The columns of `crsql_changes`, in the order they are read and re-inserted.
const CHANGE_COLUMNS: &str = "[table], pk, cid, val, col_version, db_version, site_id, cl, seq";

/// Where a branch stands relative to its parent.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchInfo {
    /// cr-sqlite site id of the parent database.
    pub parent_site_id: Vec<u8>,
    /// The parent's `crsql_db_version()` the branch has seen, as of the fork
    /// or the last [`pull`].
    pub base_version: i64,
    /// Tables that are diffed and merged.
    pub tracked_tables: Vec<String>,
    /// Tables copied at fork time whose later changes are never merged.
    pub untracked_tables: Vec<String>,
}

/// What happened to a row on the branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// The row does not exist on the parent.
    Insert,
    /// The row exists on the parent and some of its columns differ.
    Update,
    /// The branch deleted the row.
    Delete,
}

/// One column the branch wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnChange {
    pub column: String,
    /// The value on the parent right now, `None` when the row is absent there.
    pub on_main: Option<Value>,
    /// The value the branch wrote.
    pub on_branch: Value,
    /// Cosine distance between the parent's and the branch's vector, for a
    /// vector column (`FLOAT32(n)` and friends) holding a vector on both sides.
    /// This is how far an embedding moved, which a byte diff of two blobs
    /// cannot say.
    pub vector_distance: Option<f64>,
}

/// One row the branch changed.
#[derive(Debug, Clone, PartialEq)]
pub struct RowChange {
    pub table: String,
    /// Primary-key values, in primary-key order.
    pub pk: Vec<Value>,
    pub kind: ChangeKind,
    /// Columns written, empty for a [`ChangeKind::Delete`].
    pub columns: Vec<ColumnChange>,
}

/// Which side a conflict was resolved in favour of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Main,
    Branch,
}

/// A place where the branch and its parent both changed the same thing since
/// the branch last saw the parent.
#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    pub table: String,
    pub pk: Vec<Value>,
    /// The column both sides wrote, or `None` when one side deleted the row
    /// the other changed.
    pub column: Option<String>,
    /// Whose change survived. `None` in a [`diff`], which only predicts that
    /// there will be a conflict; [`merge`] and [`pull`] observe the outcome.
    pub winner: Option<Side>,
}

/// The pending changes of a branch.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchDiff {
    pub rows: Vec<RowChange>,
    pub conflicts: Vec<Conflict>,
    /// Tables whose changes this diff cannot see and a merge will not carry.
    pub untracked_tables: Vec<String>,
}

impl BranchDiff {
    /// True when merging would change nothing on the parent.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// The outcome of a [`merge`] or [`pull`].
#[derive(Debug, Clone, PartialEq)]
pub struct MergeReport {
    /// Number of `crsql_changes` rows applied to the target.
    pub changes_applied: usize,
    /// Every conflict, with the side that won.
    pub conflicts: Vec<Conflict>,
}

/// Turn the empty database behind `branch` into a branch of `main`.
///
/// Copies `main`'s schema and data in one consistent snapshot. Tracked tables
/// are filled through `crsql_changes`, so their history keeps `main`'s site id
/// and only what is written on the branch afterwards counts as the branch's
/// own. Views and triggers are created after the data is in, so a trigger does
/// not fire a second time for rows that it already handled on `main`.
pub async fn fork(main: &Connection, branch: &Connection) -> Result<BranchInfo> {
    if !schema_objects(branch).await?.is_empty() {
        return Err(Error::Misuse(
            "branch::fork needs an empty database to fork into".into(),
        ));
    }

    let tx = main.transaction().await?;
    let objects = schema_objects(&tx).await?;
    let tracked = tracked_tables(&tx).await?;
    let site_id = blob(&one(&tx, "SELECT crsql_site_id()", ()).await?)?;
    let version = int(&one(&tx, "SELECT crsql_db_version()", ()).await?)?;
    let changes = changes_since(&tx, 0, None).await?;
    let mut untracked = Vec::new();
    let mut plain_rows = Vec::new();
    for obj in objects.iter().filter(|o| o.kind == "table") {
        if !tracked.contains(&obj.name) {
            untracked.push(obj.name.clone());
            let rows = all(&tx, &format!("SELECT * FROM {}", ident(&obj.name)), ()).await?;
            plain_rows.push((obj.name.clone(), rows));
        }
    }
    tx.commit().await?;

    let tx = branch.transaction().await?;
    let (structure, behaviour): (Vec<_>, Vec<_>) = objects
        .iter()
        .partition(|o| o.kind == "table" || o.kind == "index");
    for obj in &structure {
        tx.execute(&obj.sql, ()).await?;
    }
    for table in &tracked {
        all(&tx, "SELECT crsql_as_crr(?)", params![table.as_str()]).await?;
    }
    for (table, rows) in plain_rows {
        for row in rows {
            let sql = format!(
                "INSERT INTO {} VALUES ({})",
                ident(&table),
                placeholders(row.len())
            );
            tx.execute(&sql, params_from_iter(row)).await?;
        }
    }
    apply(&tx, &changes).await?;
    for obj in &behaviour {
        tx.execute(&obj.sql, ()).await?;
    }
    tx.execute(
        &format!("CREATE TABLE {META_TABLE} (key TEXT PRIMARY KEY, value)"),
        (),
    )
    .await?;
    for (key, value) in [
        ("parent_site_id", Value::Blob(site_id.clone())),
        ("base_version", Value::Integer(version)),
        ("merged_version", Value::Integer(0)),
    ] {
        set_meta(&tx, key, value).await?;
    }
    tx.commit().await?;

    Ok(BranchInfo {
        parent_site_id: site_id,
        base_version: version,
        tracked_tables: tracked,
        untracked_tables: untracked,
    })
}

/// Describe a branch: its parent, how far it has seen the parent, and which
/// tables it tracks.
pub async fn info(branch: &Connection) -> Result<BranchInfo> {
    let meta = Meta::read(branch).await?;
    let tracked = tracked_tables(branch).await?;
    let untracked = schema_objects(branch)
        .await?
        .into_iter()
        .filter(|o| o.kind == "table" && !tracked.contains(&o.name))
        .map(|o| o.name)
        .collect();
    Ok(BranchInfo {
        parent_site_id: meta.parent_site_id,
        base_version: meta.base_version,
        tracked_tables: tracked,
        untracked_tables: untracked,
    })
}

/// What merging `branch` into `main` would do, without doing it.
///
/// Lists every row the branch changed since it was forked or last merged, next
/// to the value `main` holds now, and every place `main` changed the same thing
/// since the branch last saw it.
pub async fn diff(main: &Connection, branch: &Connection) -> Result<BranchDiff> {
    let meta = Meta::read(branch).await?;
    meta.check_parent(main).await?;
    let ours = local_changes(branch, meta.merged_version).await?;
    let theirs = changes_since(main, meta.base_version, Some(&ours.site_id)).await?;

    let mut schemas = Schemas::default();
    let mut rows: Vec<RowChange> = Vec::new();
    let mut index: HashMap<(String, Vec<u8>), usize> = HashMap::new();
    for change in &ours.changes {
        let key = (change.table.clone(), change.pk.clone());
        let at = match index.get(&key) {
            Some(&at) => at,
            None => {
                let pk = unpack(branch, &change.pk).await?;
                let on_main = schemas.lookup(main, &change.table, &pk).await?;
                rows.push(RowChange {
                    table: change.table.clone(),
                    pk,
                    kind: if on_main.is_some() {
                        ChangeKind::Update
                    } else {
                        ChangeKind::Insert
                    },
                    columns: Vec::new(),
                });
                index.insert(key, rows.len() - 1);
                rows.len() - 1
            }
        };
        let row = &mut rows[at];
        if change.is_delete() {
            row.kind = ChangeKind::Delete;
            row.columns.clear();
            continue;
        }
        if change.cid == ROW_SENTINEL || row.kind == ChangeKind::Delete {
            continue;
        }
        let on_main = schemas
            .lookup(main, &change.table, &row.pk)
            .await?
            .and_then(|mut cols| cols.remove(&change.cid));
        let vector_distance = match (
            &on_main,
            schemas.is_vector(main, &change.table, &change.cid).await?,
        ) {
            (Some(Value::Blob(a)), true) => match &change.val {
                Value::Blob(b) => distance(main, a, b).await,
                _ => None,
            },
            _ => None,
        };
        row.columns.push(ColumnChange {
            column: change.cid.clone(),
            on_main,
            on_branch: change.val.clone(),
            vector_distance,
        });
    }
    // A row the branch only touched with a no-op sentinel has nothing to show.
    rows.retain(|r| r.kind == ChangeKind::Delete || !r.columns.is_empty());

    let mut conflicts = Vec::new();
    for (ours, theirs) in conflicting(&ours.changes, &theirs) {
        let row_level = ours.is_delete() || theirs.is_delete();
        conflicts.push(Conflict {
            table: ours.table.clone(),
            pk: unpack(branch, &ours.pk).await?,
            column: (!row_level).then(|| ours.cid.clone()),
            winner: None,
        });
    }
    dedup(&mut conflicts);

    Ok(BranchDiff {
        rows,
        conflicts,
        untracked_tables: info(branch).await?.untracked_tables,
    })
}

/// Apply the branch's changes to `main`.
///
/// Changes already merged are not sent again, so merging the same branch
/// repeatedly carries only what was written since. Conflicts never fail a
/// merge: cr-sqlite resolves them the same way on every node, and the report
/// says which side won each one. Check [`diff`] first to refuse a merge that
/// would conflict.
pub async fn merge(main: &Connection, branch: &Connection) -> Result<MergeReport> {
    let meta = Meta::read(branch).await?;
    meta.check_parent(main).await?;
    let tx = branch.transaction().await?;
    let ours = local_changes(&tx, meta.merged_version).await?;
    let version = int(&one(&tx, "SELECT crsql_db_version()", ()).await?)?;
    tx.commit().await?;
    let theirs = changes_since(main, meta.base_version, Some(&ours.site_id)).await?;

    let tx = main.transaction().await?;
    apply(&tx, &ours.changes).await?;
    tx.commit().await?;
    set_meta(branch, "merged_version", Value::Integer(version)).await?;

    let conflicts = resolve(main, branch, conflicting(&ours.changes, &theirs)).await?;
    Ok(MergeReport {
        changes_applied: ours.changes.len(),
        conflicts,
    })
}

/// Bring the branch up to date with what `main` has written since the branch
/// last saw it, so later diffs only report conflicts with newer changes.
pub async fn pull(main: &Connection, branch: &Connection) -> Result<MergeReport> {
    let meta = Meta::read(branch).await?;
    meta.check_parent(main).await?;
    let ours = local_changes(branch, meta.merged_version).await?;

    let tx = main.transaction().await?;
    let theirs = changes_since(&tx, meta.base_version, Some(&ours.site_id)).await?;
    let version = int(&one(&tx, "SELECT crsql_db_version()", ()).await?)?;
    tx.commit().await?;

    let tx = branch.transaction().await?;
    apply(&tx, &theirs).await?;
    set_meta(&tx, "base_version", Value::Integer(version)).await?;
    tx.commit().await?;

    let conflicts = resolve(branch, branch, conflicting(&ours.changes, &theirs)).await?;
    Ok(MergeReport {
        changes_applied: theirs.len(),
        conflicts,
    })
}

/// One row of `crsql_changes`.
#[derive(Debug, Clone)]
struct Change {
    table: String,
    pk: Vec<u8>,
    cid: String,
    val: Value,
    cl: i64,
    raw: Vec<Value>,
}

impl Change {
    fn from_row(raw: Vec<Value>) -> Result<Self> {
        Ok(Change {
            table: text(&raw[0])?,
            pk: blob(&raw[1])?,
            cid: text(&raw[2])?,
            val: raw[3].clone(),
            cl: int(&raw[7])?,
            raw,
        })
    }

    /// cr-sqlite marks a deleted row with the sentinel column at an even
    /// causal length.
    fn is_delete(&self) -> bool {
        self.cid == ROW_SENTINEL && self.cl % 2 == 0
    }
}

struct LocalChanges {
    site_id: Vec<u8>,
    changes: Vec<Change>,
}

/// The changes written on `conn` itself after `version`.
async fn local_changes(conn: &Connection, version: i64) -> Result<LocalChanges> {
    let site_id = blob(&one(conn, "SELECT crsql_site_id()", ()).await?)?;
    let rows = all(
        conn,
        &format!(
            "SELECT {CHANGE_COLUMNS} FROM crsql_changes \
             WHERE db_version > ? AND site_id = ? ORDER BY db_version, seq"
        ),
        params![version, site_id.clone()],
    )
    .await?;
    let changes = rows
        .into_iter()
        .map(Change::from_row)
        .collect::<Result<_>>()?;
    Ok(LocalChanges { site_id, changes })
}

/// Every change `conn` holds after `version`, from any site but `exclude`.
async fn changes_since(
    conn: &Connection,
    version: i64,
    exclude: Option<&[u8]>,
) -> Result<Vec<Change>> {
    let rows = all(
        conn,
        &format!(
            "SELECT {CHANGE_COLUMNS} FROM crsql_changes \
             WHERE db_version > ? AND site_id IS NOT ? ORDER BY db_version, seq"
        ),
        params![
            version,
            exclude.map_or(Value::Null, |s| Value::Blob(s.to_vec()))
        ],
    )
    .await?;
    rows.into_iter().map(Change::from_row).collect()
}

async fn apply(conn: &Connection, changes: &[Change]) -> Result<()> {
    let sql = format!(
        "INSERT INTO crsql_changes ({CHANGE_COLUMNS}) VALUES ({})",
        placeholders(9)
    );
    for change in changes {
        conn.execute(&sql, params_from_iter(change.raw.clone()))
            .await?;
    }
    Ok(())
}

/// Pairs of (ours, theirs) that touch the same cell, or the same row when
/// either side deleted it. Writing the same value on both sides is agreement,
/// not a conflict.
fn conflicting<'a>(ours: &'a [Change], theirs: &'a [Change]) -> Vec<(&'a Change, &'a Change)> {
    let mut out = Vec::new();
    for a in ours {
        for b in theirs {
            if a.table != b.table || a.pk != b.pk {
                continue;
            }
            let row_level = a.is_delete() || b.is_delete();
            let same_cell = a.cid == b.cid && a.cid != ROW_SENTINEL;
            if (row_level && a.is_delete() != b.is_delete()) || (same_cell && a.val != b.val) {
                out.push((a, b));
            }
        }
    }
    out
}

/// Look at `target` after a merge or pull and record which side each conflict
/// went to.
async fn resolve(
    target: &Connection,
    branch: &Connection,
    pairs: Vec<(&Change, &Change)>,
) -> Result<Vec<Conflict>> {
    let mut schemas = Schemas::default();
    let mut out = Vec::new();
    for (ours, theirs) in pairs {
        let pk = unpack(branch, &ours.pk).await?;
        let row = schemas.lookup(target, &ours.table, &pk).await?;
        let (column, winner) = if ours.is_delete() || theirs.is_delete() {
            // Whoever deleted it won exactly when the row is gone.
            let gone = row.is_none();
            let branch_deleted = ours.is_delete();
            let winner = if gone == branch_deleted {
                Side::Branch
            } else {
                Side::Main
            };
            (None, winner)
        } else {
            let now = row.and_then(|mut r| r.remove(&ours.cid));
            let winner = if now.as_ref() == Some(&ours.val) {
                Side::Branch
            } else {
                Side::Main
            };
            (Some(ours.cid.clone()), winner)
        };
        out.push(Conflict {
            table: ours.table.clone(),
            pk,
            column,
            winner: Some(winner),
        });
    }
    dedup(&mut out);
    Ok(out)
}

fn dedup(conflicts: &mut Vec<Conflict>) {
    let mut seen = Vec::new();
    conflicts.retain(|c| {
        let key = (c.table.clone(), c.pk.clone(), c.column.clone());
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
}

/// The branch's bookkeeping row set.
struct Meta {
    parent_site_id: Vec<u8>,
    base_version: i64,
    merged_version: i64,
}

impl Meta {
    async fn read(branch: &Connection) -> Result<Self> {
        let exists = one(
            branch,
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
            params![META_TABLE],
        )
        .await?;
        if int(&exists)? == 0 {
            return Err(Error::Misuse(
                "not a branch: fork it with branch::fork first".into(),
            ));
        }
        let get = |key: &'static str| async move {
            one(
                branch,
                &format!("SELECT value FROM {META_TABLE} WHERE key = ?"),
                params![key],
            )
            .await
        };
        Ok(Meta {
            parent_site_id: blob(&get("parent_site_id").await?)?,
            base_version: int(&get("base_version").await?)?,
            merged_version: int(&get("merged_version").await?)?,
        })
    }

    async fn check_parent(&self, main: &Connection) -> Result<()> {
        let site = blob(&one(main, "SELECT crsql_site_id()", ()).await?)?;
        if site != self.parent_site_id {
            return Err(Error::Misuse(
                "this branch was forked from a different database".into(),
            ));
        }
        Ok(())
    }
}

async fn set_meta(conn: &Connection, key: &str, value: Value) -> Result<()> {
    conn.execute(
        &format!("INSERT OR REPLACE INTO {META_TABLE} (key, value) VALUES (?, ?)"),
        params![key, value],
    )
    .await?;
    Ok(())
}

/// Column layout of the tables a diff looks rows up in.
#[derive(Default)]
struct Schemas {
    tables: HashMap<String, Vec<(String, String, i64)>>,
}

impl Schemas {
    async fn columns(
        &mut self,
        conn: &Connection,
        table: &str,
    ) -> Result<&[(String, String, i64)]> {
        if !self.tables.contains_key(table) {
            let rows = all(
                conn,
                "SELECT name, type, pk FROM pragma_table_info(?)",
                params![table],
            )
            .await?;
            let cols = rows
                .iter()
                .map(|r| Ok((text(&r[0])?, text(&r[1])?, int(&r[2])?)))
                .collect::<Result<Vec<_>>>()?;
            self.tables.insert(table.to_string(), cols);
        }
        Ok(&self.tables[table])
    }

    async fn is_vector(&mut self, conn: &Connection, table: &str, column: &str) -> Result<bool> {
        let cols = self.columns(conn, table).await?;
        Ok(cols.iter().any(|(name, ty, _)| {
            let ty = ty.to_ascii_uppercase();
            name == column && ty.starts_with("FLOAT") && ty.contains('(')
        }))
    }

    /// The row with primary key `pk` as column → value, if it exists.
    async fn lookup(
        &mut self,
        conn: &Connection,
        table: &str,
        pk: &[Value],
    ) -> Result<Option<HashMap<String, Value>>> {
        let mut pk_cols: Vec<_> = self
            .columns(conn, table)
            .await?
            .iter()
            .filter(|(_, _, pk)| *pk > 0)
            .map(|(name, _, pk)| (*pk, name.clone()))
            .collect();
        pk_cols.sort();
        let filter = pk_cols
            .iter()
            .map(|(_, name)| format!("{} = ?", ident(name)))
            .collect::<Vec<_>>()
            .join(" AND ");
        let sql = format!("SELECT * FROM {} WHERE {filter}", ident(table));
        let mut rows = conn.query(&sql, params_from_iter(pk.to_vec())).await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let mut out = HashMap::new();
        for i in 0..row.column_count() {
            if let Some(name) = row.column_name(i) {
                out.insert(name.to_string(), row.get_value(i)?);
            }
        }
        Ok(Some(out))
    }
}

async fn unpack(conn: &Connection, pk: &[u8]) -> Result<Vec<Value>> {
    let rows = all(
        conn,
        "SELECT cell FROM crsql_unpack_columns WHERE package = ?",
        params![pk.to_vec()],
    )
    .await?;
    Ok(rows.into_iter().map(|mut r| r.remove(0)).collect())
}

async fn distance(conn: &Connection, a: &[u8], b: &[u8]) -> Option<f64> {
    let value = one(
        conn,
        "SELECT vector_distance_cos(?, ?)",
        params![a.to_vec(), b.to_vec()],
    )
    .await
    .ok()?;
    match value {
        Value::Real(d) => Some(d),
        _ => None,
    }
}

struct SchemaObject {
    kind: String,
    name: String,
    sql: String,
}

/// The user's own tables, indexes, views and triggers, in creation order:
/// everything but SQLite's, cr-sqlite's and the vector index's internals, and
/// the branch's bookkeeping.
async fn schema_objects(conn: &Connection) -> Result<Vec<SchemaObject>> {
    let rows = all(
        conn,
        &format!(
            "SELECT type, name, sql FROM sqlite_master AS m \
             WHERE sql IS NOT NULL \
               AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
               AND name NOT LIKE 'crsql\\_%' ESCAPE '\\' \
               AND name NOT LIKE '%\\_\\_crsql\\_%' ESCAPE '\\' \
               AND tbl_name NOT LIKE 'crsql\\_%' ESCAPE '\\' \
               AND tbl_name NOT LIKE '%\\_\\_crsql\\_%' ESCAPE '\\' \
               AND tbl_name NOT IN ('sqlanywhere_vector_meta_shadow', '{META_TABLE}') \
               AND NOT EXISTS (SELECT 1 FROM sqlite_master AS i \
                               WHERE i.type = 'index' AND m.tbl_name = i.name || '_shadow') \
               AND NOT EXISTS (SELECT 1 FROM pragma_table_list AS t \
                               WHERE t.name = m.tbl_name AND t.type = 'shadow') \
             ORDER BY rowid"
        ),
        (),
    )
    .await?;
    rows.iter()
        .map(|r| {
            Ok(SchemaObject {
                kind: text(&r[0])?,
                name: text(&r[1])?,
                sql: text(&r[2])?,
            })
        })
        .collect()
}

/// Tables cr-sqlite tracks: the ones with a clock table.
async fn tracked_tables(conn: &Connection) -> Result<Vec<String>> {
    let rows = all(
        conn,
        "SELECT substr(name, 1, length(name) - length('__crsql_clock')) FROM sqlite_master \
         WHERE type = 'table' AND name LIKE '%\\_\\_crsql\\_clock' ESCAPE '\\' ORDER BY rowid",
        (),
    )
    .await?;
    rows.iter().map(|r| text(&r[0])).collect()
}

async fn all(
    conn: &Connection,
    sql: &str,
    params: impl crate::params::IntoParams,
) -> Result<Vec<Vec<Value>>> {
    let mut rows = conn.query(sql, params).await?;
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

async fn one(
    conn: &Connection,
    sql: &str,
    params: impl crate::params::IntoParams,
) -> Result<Value> {
    all(conn, sql, params)
        .await?
        .into_iter()
        .next()
        .and_then(|r| r.into_iter().next())
        .ok_or_else(|| Error::Misuse(format!("expected a row from `{sql}`")))
}

fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

fn text(v: &Value) -> Result<String> {
    match v {
        Value::Text(s) => Ok(s.clone()),
        Value::Null => Ok(String::new()),
        other => Err(Error::Misuse(format!("expected text, got {other:?}"))),
    }
}

fn blob(v: &Value) -> Result<Vec<u8>> {
    match v {
        Value::Blob(b) => Ok(b.clone()),
        other => Err(Error::Misuse(format!("expected a blob, got {other:?}"))),
    }
}

fn int(v: &Value) -> Result<i64> {
    match v {
        Value::Integer(i) => Ok(*i),
        other => Err(Error::Misuse(format!("expected an integer, got {other:?}"))),
    }
}
