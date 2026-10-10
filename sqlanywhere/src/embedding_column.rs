//! Embedding columns: a vector column the database keeps in step with a text
//! column, filled in the background through the durable queue.
//!
//! ```text
//! INSERT / UPDATE OF body ──trigger──▶ askr_jobs ──process()──▶ docs.emb
//! ```
//!
//! Writing a row never waits for a model. A trigger enqueues a job in the
//! queue contract's own tables (`docs/contracts/QUEUE_CONTRACT.md`), so the
//! backlog is durable, replicated, and visible to the same metrics as every
//! other queue. A worker calls [`EmbeddingColumn::process`], which claims jobs
//! with the contract's atomic claim, embeds without holding a transaction open,
//! and writes the vector only if the text it embedded is still the row's text.
//! A row edited while its job was in flight is simply embedded again.
//!
//! Each column records the [`Embedder::model_id`] it was installed with, and a
//! worker running any other model is refused, so one column never ends up
//! holding vectors from two models by accident. Changing models is an explicit
//! migration: [`EmbeddingColumn::reembed`] switches the recorded model and
//! enqueues every row.
//!
//! ```rust
//! # async fn run() -> sqlanywhere::Result<()> {
//! use sqlanywhere::{embedding_column::EmbeddingColumn, Builder, LexicalEmbedder};
//!
//! let db = Builder::new_local(":memory:").build().await?;
//! let conn = db.connect()?;
//! conn.execute("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, emb FLOAT32(64))", ())
//!     .await?;
//!
//! let model = LexicalEmbedder::new(64);
//! let column = EmbeddingColumn::new("docs", "body", "emb");
//! column.install(&conn, &model).await?;
//!
//! conn.execute("INSERT INTO docs (body) VALUES ('memory safety')", ()).await?;
//! column.process(&conn, &model, 100).await?; // in a worker, on a loop
//! # Ok(())
//! # }
//! ```

use crate::{named_params, params, Connection, Embedder, Error, Result, Value};

/// The registry of installed embedding columns.
const REGISTRY: &str = "sqlanywhere_embedding_columns";

/// How long a claimed job stays reserved before another worker may take it.
const VISIBILITY_SECONDS: i64 = 300;

/// Queue contract v1 schema, verbatim.
const QUEUE_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS askr_jobs (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  queue          TEXT    NOT NULL DEFAULT 'default',
  payload        BLOB    NOT NULL,
  priority       INTEGER NOT NULL DEFAULT 0,
  available_at   INTEGER NOT NULL,
  reserved_until INTEGER,
  attempts       INTEGER NOT NULL DEFAULT 0,
  max_attempts   INTEGER NOT NULL DEFAULT 25,
  created_at     INTEGER NOT NULL DEFAULT (unixepoch())
)",
    "CREATE INDEX IF NOT EXISTS askr_jobs_claim
  ON askr_jobs (queue, reserved_until, priority DESC, available_at, id)",
    "CREATE TABLE IF NOT EXISTS askr_failed_jobs (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  uuid       TEXT,
  queue      TEXT    NOT NULL,
  payload    BLOB    NOT NULL,
  exception  TEXT,
  attempts   INTEGER NOT NULL,
  failed_at  INTEGER NOT NULL DEFAULT (unixepoch())
)",
];

/// Queue contract v1, "Claim (pop)", verbatim.
const CLAIM: &str = "UPDATE askr_jobs
SET reserved_until = unixepoch() + :visibility,
    attempts       = attempts + 1
WHERE id = (
  SELECT id FROM askr_jobs
  WHERE queue = :queue
    AND available_at <= unixepoch()
    AND (reserved_until IS NULL OR reserved_until <= unixepoch())
  ORDER BY priority DESC, available_at, id
  LIMIT 1
)
RETURNING id, payload, attempts, max_attempts";

/// Queue contract v1, "Ack", verbatim.
const ACK: &str = "DELETE FROM askr_jobs WHERE id = :id";

/// Queue contract v1, "Release", verbatim.
const RELEASE: &str = "UPDATE askr_jobs
SET reserved_until = NULL,
    available_at   = unixepoch() + :backoff
WHERE id = :id";

/// Queue contract v1, "Move to dead-letter", the two statements between
/// `BEGIN IMMEDIATE` and `COMMIT`, verbatim.
const DEAD_LETTER: &[&str] = &[
    "INSERT INTO askr_failed_jobs (uuid, queue, payload, exception, attempts)
SELECT :uuid, queue, payload, :exception, attempts FROM askr_jobs WHERE id = :id",
    "DELETE FROM askr_jobs WHERE id = :id",
];

/// Queue contract v1, "Backlog", verbatim.
const BACKLOG: &str = "SELECT
  count(*)                                                          AS total,
  count(*) FILTER (
    WHERE available_at <= unixepoch()
      AND (reserved_until IS NULL OR reserved_until <= unixepoch())
  )                                                                 AS ready,
  coalesce(unixepoch() - min(available_at) FILTER (
    WHERE available_at <= unixepoch()
      AND (reserved_until IS NULL OR reserved_until <= unixepoch())
  ), 0)                                                             AS oldest_seconds
FROM askr_jobs
WHERE queue = :queue";

/// A vector column kept in step with a text column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingColumn {
    table: String,
    source: String,
    target: String,
}

/// What one call to [`EmbeddingColumn::process`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessReport {
    /// Rows whose vector was written.
    pub embedded: usize,
    /// Jobs finished without a write: the row was deleted, or its text changed
    /// after the job read it (a newer job is queued for that).
    pub skipped: usize,
    /// Jobs put back with a backoff after a failure.
    pub retried: usize,
    /// Jobs moved to `askr_failed_jobs` after their last attempt.
    pub dead_lettered: usize,
}

/// Where an embedding column stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingStatus {
    /// The model the column's vectors come from (or are being moved to).
    pub model: String,
    pub dims: usize,
    /// Rows waiting to be embedded, ready or not.
    pub pending: i64,
    /// Rows waiting that a worker could claim now.
    pub ready: i64,
    /// Age of the oldest ready job, in seconds.
    pub oldest_seconds: i64,
    /// Jobs that gave up, in `askr_failed_jobs`.
    pub failed: i64,
}

impl EmbeddingStatus {
    /// True when every row's vector matches its current text.
    pub fn is_current(&self) -> bool {
        self.pending == 0
    }
}

impl EmbeddingColumn {
    /// Keep `table.target` (a vector column such as `FLOAT32(384)`) as the
    /// embedding of `table.source` (a text column).
    pub fn new(
        table: impl Into<String>,
        source: impl Into<String>,
        target: impl Into<String>,
    ) -> Self {
        Self {
            table: table.into(),
            source: source.into(),
            target: target.into(),
        }
    }

    /// The queue this column's jobs go on.
    pub fn queue(&self) -> String {
        format!("sqlanywhere.embed:{}.{}", self.table, self.target)
    }

    /// Create the queue tables if needed, record the column and its model,
    /// install the triggers, and enqueue every existing row that has text.
    /// Returns how many rows were enqueued.
    ///
    /// Fails if the table has no rowid, the target is not a vector column, or
    /// its dimensions differ from the embedder's.
    pub async fn install<E: Embedder + ?Sized>(
        &self,
        conn: &Connection,
        embedder: &E,
    ) -> Result<u64> {
        self.check_target(conn, embedder).await?;
        let tx = conn.transaction().await?;
        for sql in QUEUE_SCHEMA {
            tx.execute(sql, ()).await?;
        }
        tx.execute(
            &format!(
                "CREATE TABLE IF NOT EXISTS {REGISTRY} (\
                 tbl TEXT NOT NULL, target TEXT NOT NULL, source TEXT NOT NULL, \
                 model TEXT NOT NULL, dims INTEGER NOT NULL, PRIMARY KEY (tbl, target))"
            ),
            (),
        )
        .await?;
        tx.execute(
            &format!(
                "INSERT INTO {REGISTRY} (tbl, target, source, model, dims) VALUES (?, ?, ?, ?, ?)"
            ),
            params![
                self.table.as_str(),
                self.target.as_str(),
                self.source.as_str(),
                embedder.model_id(),
                embedder.dims() as i64
            ],
        )
        .await
        .map_err(|e| match e {
            Error::SqliteFailure(..) => Error::Misuse(format!(
                "{}.{} is already an embedding column ({e})",
                self.table, self.target
            )),
            e => e,
        })?;
        let enqueue = self.enqueue_sql("new.rowid");
        let (table, source) = (ident(&self.table), ident(&self.source));
        tx.execute(
            &format!(
                "CREATE TRIGGER {} AFTER INSERT ON {table} WHEN new.{source} IS NOT NULL \
                 BEGIN {enqueue}; END",
                ident(&self.trigger("ins"))
            ),
            (),
        )
        .await?;
        tx.execute(
            &format!(
                "CREATE TRIGGER {} AFTER UPDATE OF {source} ON {table} \
                 WHEN new.{source} IS NOT old.{source} BEGIN {enqueue}; END",
                ident(&self.trigger("upd"))
            ),
            (),
        )
        .await?;
        let queued = self.enqueue_all(&tx).await?;
        tx.commit().await?;
        Ok(queued)
    }

    /// Remove the triggers, the pending jobs and the registry entry. The
    /// vectors already written stay.
    pub async fn uninstall(&self, conn: &Connection) -> Result<()> {
        let tx = conn.transaction().await?;
        for kind in ["ins", "upd"] {
            tx.execute(
                &format!("DROP TRIGGER IF EXISTS {}", ident(&self.trigger(kind))),
                (),
            )
            .await?;
        }
        tx.execute(
            "DELETE FROM askr_jobs WHERE queue = ?",
            params![self.queue()],
        )
        .await?;
        tx.execute(
            &format!("DELETE FROM {REGISTRY} WHERE tbl = ? AND target = ?"),
            params![self.table.as_str(), self.target.as_str()],
        )
        .await?;
        tx.commit().await
    }

    /// Embed up to `max_jobs` queued rows with `embedder`. Run it from a
    /// worker loop; any number of workers may run it at once, since each job
    /// is claimed atomically.
    ///
    /// Refuses to run when `embedder` is not the model the column records, so
    /// a worker deployed with the wrong model cannot mix two vector spaces in
    /// one column.
    pub async fn process<E: Embedder + ?Sized>(
        &self,
        conn: &Connection,
        embedder: &E,
        max_jobs: usize,
    ) -> Result<ProcessReport> {
        let (model, dims) = self.registered(conn).await?;
        if model != embedder.model_id() || dims != embedder.dims() {
            return Err(Error::Misuse(format!(
                "{}.{} is embedded with {model} ({dims} dims), not {} ({} dims); \
                 use reembed to change models",
                self.table,
                self.target,
                embedder.model_id(),
                embedder.dims()
            )));
        }
        let vector_fn = vector_function(&self.declared_type(conn).await?)?;
        let read = format!(
            "SELECT {} FROM {} WHERE rowid = ?",
            ident(&self.source),
            ident(&self.table)
        );
        let write = format!(
            "UPDATE {table} SET {target} = CASE WHEN ?1 IS NULL THEN NULL ELSE {vector_fn}(?1) END \
             WHERE rowid = ?2 AND {source} IS ?3",
            table = ident(&self.table),
            target = ident(&self.target),
            source = ident(&self.source),
        );

        let mut report = ProcessReport::default();
        for _ in 0..max_jobs {
            let Some(job) = self.claim(conn).await? else {
                break;
            };
            let text = first(conn, &read, params![job.rowid]).await?;
            let Some(text) = text else {
                ack(conn, job.id).await?;
                report.skipped += 1;
                continue;
            };
            let literal = match &text {
                Value::Null => Ok(None),
                Value::Text(t) => {
                    let v = embedder.embed(t);
                    if v.len() == dims {
                        Ok(Some(crate::to_vector_literal(&v)))
                    } else {
                        Err(format!(
                            "embedder returned {} dims, expected {dims}",
                            v.len()
                        ))
                    }
                }
                other => Err(format!("{} is not text: {other:?}", self.source)),
            };
            match literal {
                Ok(literal) => {
                    let tx = conn.transaction().await?;
                    let written = tx
                        .execute(
                            &write,
                            params![literal.map_or(Value::Null, Value::Text), job.rowid, text],
                        )
                        .await?;
                    tx.execute(ACK, named_params! { ":id": job.id }).await?;
                    tx.commit().await?;
                    if written == 1 {
                        report.embedded += 1;
                    } else {
                        report.skipped += 1;
                    }
                }
                Err(reason) if job.attempts >= job.max_attempts => {
                    self.dead_letter(conn, job.id, &reason).await?;
                    report.dead_lettered += 1;
                }
                Err(_) => {
                    conn.execute(
                        RELEASE,
                        named_params! { ":backoff": backoff(job.attempts), ":id": job.id },
                    )
                    .await?;
                    report.retried += 1;
                }
            }
        }
        Ok(report)
    }

    /// Switch the column to a new model and enqueue every row, returning how
    /// many were enqueued. Until the backlog drains, the column holds vectors
    /// from both models; [`status`](Self::status) says when it has.
    pub async fn reembed<E: Embedder + ?Sized>(
        &self,
        conn: &Connection,
        embedder: &E,
    ) -> Result<u64> {
        self.check_target(conn, embedder).await?;
        self.registered(conn).await?;
        let tx = conn.transaction().await?;
        tx.execute(
            &format!("UPDATE {REGISTRY} SET model = ?, dims = ? WHERE tbl = ? AND target = ?"),
            params![
                embedder.model_id(),
                embedder.dims() as i64,
                self.table.as_str(),
                self.target.as_str()
            ],
        )
        .await?;
        let queued = self.enqueue_all(&tx).await?;
        tx.commit().await?;
        Ok(queued)
    }

    /// The column's model and backlog.
    pub async fn status(&self, conn: &Connection) -> Result<EmbeddingStatus> {
        let (model, dims) = self.registered(conn).await?;
        let mut rows = conn
            .query(BACKLOG, named_params! { ":queue": self.queue() })
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| Error::Misuse("backlog query returned no row".into()))?;
        let (pending, ready, oldest_seconds) = (row.get(0)?, row.get(1)?, row.get(2)?);
        let failed = match first(
            conn,
            "SELECT count(*) FROM askr_failed_jobs WHERE queue = ?",
            params![self.queue()],
        )
        .await?
        {
            Some(Value::Integer(n)) => n,
            _ => 0,
        };
        Ok(EmbeddingStatus {
            model,
            dims,
            pending,
            ready,
            oldest_seconds,
            failed,
        })
    }

    fn trigger(&self, kind: &str) -> String {
        format!("{}__embed_{}_{kind}", self.table, self.target)
    }

    /// The enqueue a trigger or a backfill runs for one row. It skips a row
    /// that already has a job waiting and unclaimed, since that job will read
    /// the row's latest text anyway. A job already claimed may have read older
    /// text, so a second one is queued behind it.
    fn enqueue_sql(&self, rowid: &str) -> String {
        let queue = literal(&self.queue());
        format!(
            "INSERT INTO askr_jobs (queue, payload, priority, available_at) \
             SELECT {queue}, {rowid}, 0, unixepoch() \
             WHERE NOT EXISTS (SELECT 1 FROM askr_jobs \
               WHERE queue = {queue} AND payload = {rowid} AND reserved_until IS NULL)"
        )
    }

    async fn enqueue_all(&self, conn: &Connection) -> Result<u64> {
        let rowids = format!(
            "SELECT rowid FROM {} WHERE {} IS NOT NULL",
            ident(&self.table),
            ident(&self.source)
        );
        let mut rows = conn.query(&rowids, ()).await?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await? {
            ids.push(row.get::<i64>(0)?);
        }
        drop(rows);
        let enqueue = self.enqueue_sql("?1");
        let mut queued = 0;
        for id in ids {
            queued += conn.execute(&enqueue, params![id]).await?;
        }
        Ok(queued)
    }

    async fn claim(&self, conn: &Connection) -> Result<Option<Job>> {
        let mut rows = conn
            .query(
                CLAIM,
                named_params! { ":visibility": VISIBILITY_SECONDS, ":queue": self.queue() },
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(Some(Job {
            id: row.get(0)?,
            rowid: row.get(1)?,
            attempts: row.get(2)?,
            max_attempts: row.get(3)?,
        }))
    }

    async fn dead_letter(&self, conn: &Connection, id: i64, reason: &str) -> Result<()> {
        let tx = conn
            .transaction_with_behavior(crate::TransactionBehavior::Immediate)
            .await?;
        tx.execute(
            DEAD_LETTER[0],
            named_params! { ":uuid": Value::Null, ":exception": reason, ":id": id },
        )
        .await?;
        tx.execute(DEAD_LETTER[1], named_params! { ":id": id })
            .await?;
        tx.commit().await
    }

    async fn registered(&self, conn: &Connection) -> Result<(String, usize)> {
        let exists = first(
            conn,
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
            params![REGISTRY],
        )
        .await?;
        let found = if exists == Some(Value::Integer(1)) {
            let mut rows = conn
                .query(
                    &format!("SELECT model, dims FROM {REGISTRY} WHERE tbl = ? AND target = ?"),
                    params![self.table.as_str(), self.target.as_str()],
                )
                .await?;
            match rows.next().await? {
                Some(row) => Some((row.get::<String>(0)?, row.get::<i64>(1)? as usize)),
                None => None,
            }
        } else {
            None
        };
        found.ok_or_else(|| {
            Error::Misuse(format!(
                "{}.{} is not an embedding column; install it first",
                self.table, self.target
            ))
        })
    }

    async fn declared_type(&self, conn: &Connection) -> Result<String> {
        match first(
            conn,
            "SELECT type FROM pragma_table_info(?) WHERE name = ?",
            params![self.table.as_str(), self.target.as_str()],
        )
        .await?
        {
            Some(Value::Text(t)) => Ok(t),
            _ => Err(Error::Misuse(format!(
                "{}.{} does not exist",
                self.table, self.target
            ))),
        }
    }

    async fn check_target<E: Embedder + ?Sized>(
        &self,
        conn: &Connection,
        embedder: &E,
    ) -> Result<()> {
        let declared = self.declared_type(conn).await?;
        vector_function(&declared)?;
        let dims = declared
            .split_once('(')
            .and_then(|(_, rest)| rest.trim_end_matches(')').trim().parse::<usize>().ok());
        if dims != Some(embedder.dims()) {
            return Err(Error::Misuse(format!(
                "{}.{} is {declared}, but {} makes {} dims",
                self.table,
                self.target,
                embedder.model_id(),
                embedder.dims()
            )));
        }
        let without_rowid = first(
            conn,
            "SELECT sql LIKE '%WITHOUT ROWID%' FROM sqlite_master WHERE type = 'table' AND name = ?",
            params![self.table.as_str()],
        )
        .await?;
        if without_rowid == Some(Value::Integer(1)) {
            return Err(Error::Misuse(format!(
                "{} is WITHOUT ROWID; embedding columns need a rowid",
                self.table
            )));
        }
        Ok(())
    }
}

struct Job {
    id: i64,
    rowid: i64,
    attempts: i64,
    max_attempts: i64,
}

async fn ack(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(ACK, named_params! { ":id": id }).await?;
    Ok(())
}

/// The queue contract's reference backoff: `min(1 * 2^(attempts - 1), 3600)`.
fn backoff(attempts: i64) -> i64 {
    let exp = (attempts - 1).clamp(0, 12) as u32;
    (1i64 << exp).min(3600)
}

/// The SQL function that turns a vector literal into the column's type.
fn vector_function(declared: &str) -> Result<&'static str> {
    let base = declared
        .split('(')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase();
    Ok(match base.as_str() {
        "FLOAT32" | "F32_BLOB" => "vector32",
        "FLOAT64" | "F64_BLOB" => "vector64",
        "FLOAT16" | "F16_BLOB" => "vector16",
        "FLOATB16" | "FB16_BLOB" => "vectorb16",
        "FLOAT8" | "F8_BLOB" => "vector8",
        "FLOAT1BIT" | "F1BIT_BLOB" => "vector1bit",
        _ => {
            return Err(Error::Misuse(format!(
                "{declared} is not a vector column type"
            )))
        }
    })
}

async fn first(
    conn: &Connection,
    sql: &str,
    params: impl crate::params::IntoParams,
) -> Result<Option<Value>> {
    let mut rows = conn.query(sql, params).await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row.get_value(0)?)),
        None => Ok(None),
    }
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

    /// The contract document, read at test time so the library itself does
    /// not depend on a file outside the crate.
    fn contract() -> String {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../docs/contracts/QUEUE_CONTRACT.md"
        );
        normalize(&std::fs::read_to_string(path).unwrap())
    }

    /// Drop `--` comments and collapse whitespace, so layout is not content.
    fn normalize(sql: &str) -> String {
        sql.lines()
            .map(|l| l.split("--").next().unwrap())
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The queue statements this module runs are the contract's, not a
    /// paraphrase: editing either side without the other turns this red.
    #[test]
    fn queue_sql_is_the_contract_verbatim() {
        let contract = contract();
        let ours = QUEUE_SCHEMA
            .iter()
            .chain([CLAIM, ACK, RELEASE, BACKLOG].iter())
            .chain(DEAD_LETTER.iter());
        for sql in ours {
            assert!(
                contract.contains(&normalize(sql)),
                "not in QUEUE_CONTRACT.md verbatim:\n{sql}"
            );
        }
    }

    #[test]
    fn backoff_follows_the_reference_policy() {
        assert_eq!(
            [1, 2, 3, 4, 12, 13, 25].map(backoff),
            [1, 2, 4, 8, 2048, 3600, 3600]
        );
    }

    #[test]
    fn vector_types_map_to_their_constructor() {
        assert_eq!(vector_function("FLOAT32(384)").unwrap(), "vector32");
        assert_eq!(vector_function("float1bit(64)").unwrap(), "vector1bit");
        assert!(vector_function("TEXT").is_err());
    }
}
