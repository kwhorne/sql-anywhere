//! How often a DiskANN index finds the true nearest neighbours, and the
//! `search_l` that makes it find enough of them.
//!
//! `search_l` is how many candidates a search keeps while it walks the graph:
//! more read more nodes and find more of the true nearest rows. The index has
//! its own (200 unless given at `CREATE INDEX`), and a query can ask for
//! another as `vector_top_k`'s fourth argument, so recall is traded for speed
//! per query, without rebuilding anything:
//!
//! ```sql
//! SELECT id FROM vector_top_k('docs_vec', :query, 10, 64);
//! ```
//!
//! [`measure`] gives recall@k at a `search_l`, and [`tune`] the smallest
//! `search_l` that reaches a target:
//!
//! ```rust,no_run
//! # async fn run(conn: sqlanywhere::Connection) -> sqlanywhere::Result<()> {
//! use sqlanywhere::recall;
//!
//! let tuning = recall::tune(&conn, "docs_vec", 10, 0.95, 200).await?;
//! for step in &tuning.steps {
//!     println!("search_l {:>5}: recall@10 {:.3}", step.search_l, step.recall);
//! }
//! if let Some(search_l) = tuning.search_l {
//!     println!("use vector_top_k('docs_vec', :q, 10, {search_l})");
//! }
//! # Ok(())
//! # }
//! ```
//!
//! The queries are the table's own vectors, `sample` of them spread evenly
//! over its rows, and the exact neighbours come from a full scan per query:
//! measure on a sample, not on every row. A search starts from a random node,
//! so two measurements of the same index differ a little.
//!
//! [`tune_compression`] answers the other question, how small the index can
//! be: it builds the index again with each `compress_neighbors`, measures
//! each one's size and recall, and rolls every build back.
//!
//! ```rust,no_run
//! # async fn run(conn: sqlanywhere::Connection) -> sqlanywhere::Result<()> {
//! use sqlanywhere::recall;
//!
//! let tuning =
//!     recall::tune_compression(&conn, "docs_vec", 10, 0.9, 200, &recall::COMPRESSIONS).await?;
//! for c in &tuning.candidates {
//!     println!("{:<10} {:>10} bytes  recall@10 {:.3}",
//!         c.compress_neighbors.as_deref().unwrap_or("none"), c.bytes, c.recall);
//! }
//! # Ok(())
//! # }
//! ```

use std::collections::HashSet;

use crate::{params, Connection, Error, Result, Value};

/// Recall@k at one `search_l`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measured {
    pub search_l: u32,
    /// The share of the exact k nearest neighbours the index returned, over
    /// every query.
    pub recall: f64,
}

/// What [`tune`] found.
#[derive(Debug, Clone, PartialEq)]
pub struct Tuning {
    /// The smallest `search_l` tried that reached the target, or `None` when
    /// none up to the limit did.
    pub search_l: Option<u32>,
    /// Every `search_l` tried, in order, with its recall.
    pub steps: Vec<Measured>,
}

/// The largest `search_l` [`tune`] tries.
pub const TUNE_LIMIT: u32 = 4096;

/// The `compress_neighbors` values [`tune_compression`] usually compares:
/// none (neighbours stored as the column's own type), then smaller and
/// smaller.
pub const COMPRESSIONS: [Option<&str>; 4] =
    [None, Some("float16"), Some("float8"), Some("float1bit")];

/// One `compress_neighbors`, built and measured by [`tune_compression`].
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// `None`: no `compress_neighbors`.
    pub compress_neighbors: Option<String>,
    /// The index's size, as `vector_index_stats` gives it.
    pub bytes: u64,
    /// Recall@k at the index's own `search_l`.
    pub recall: f64,
}

/// What [`tune_compression`] found.
#[derive(Debug, Clone, PartialEq)]
pub struct Compression {
    /// The smallest candidate that reached the target, or `None` when none did.
    pub chosen: Option<Candidate>,
    /// Every candidate, in the order given.
    pub candidates: Vec<Candidate>,
}

/// Recall@k of `index` at `search_l` (`None`: the index's own), over `sample`
/// of the table's rows as queries.
pub async fn measure(
    conn: &Connection,
    index: &str,
    k: u32,
    search_l: Option<u32>,
    sample: u32,
) -> Result<f64> {
    let probe = Probe::new(conn, index, k, sample).await?;
    probe.recall(index, search_l).await
}

/// The smallest `search_l` at which `index` reaches recall@k of `target`
/// (0 to 1): `k`, then doubling, up to [`TUNE_LIMIT`]. The queries are the
/// same at every step.
pub async fn tune(
    conn: &Connection,
    index: &str,
    k: u32,
    target: f64,
    sample: u32,
) -> Result<Tuning> {
    if !(0.0..=1.0).contains(&target) {
        return Err(Error::Misuse(format!(
            "recall::tune: target {target} is not between 0 and 1"
        )));
    }
    let probe = Probe::new(conn, index, k, sample).await?;
    let mut steps = Vec::new();
    let mut search_l = k.max(1);
    loop {
        let recall = probe.recall(index, Some(search_l)).await?;
        steps.push(Measured { search_l, recall });
        if recall >= target {
            return Ok(Tuning {
                search_l: Some(search_l),
                steps,
            });
        }
        if search_l >= TUNE_LIMIT {
            return Ok(Tuning {
                search_l: None,
                steps,
            });
        }
        search_l = (search_l * 2).min(TUNE_LIMIT);
    }
}

/// Which `compress_neighbors` keeps recall@k of `index` at `target` (0 to 1)
/// in the fewest bytes. Each candidate is a build of the same index, with the
/// options its `CREATE INDEX` gave and `compress_neighbors` replaced (`None`
/// leaves it out), measured at the index's own `search_l` over the same
/// queries. The builds are made inside a savepoint and rolled back, so the
/// database is left as it was; each one takes as long as building the index.
pub async fn tune_compression(
    conn: &Connection,
    index: &str,
    k: u32,
    target: f64,
    sample: u32,
    candidates: &[Option<&str>],
) -> Result<Compression> {
    if !(0.0..=1.0).contains(&target) {
        return Err(Error::Misuse(format!(
            "recall::tune_compression: target {target} is not between 0 and 1"
        )));
    }
    let probe = Probe::new(conn, index, k, sample).await?;
    let (table, column) = indexed_column(conn, index).await?;
    let options: Vec<String> = index_options(conn, index)
        .await?
        .into_iter()
        .filter(|o| {
            !o.trim_start()
                .to_ascii_lowercase()
                .starts_with("compress_neighbors")
        })
        .collect();

    conn.execute("SAVEPOINT sqlanywhere_recall_compression", ())
        .await?;
    let built = build_candidates(conn, &probe, index, &table, &column, &options, candidates).await;
    let rolled_back = async {
        conn.execute("ROLLBACK TO sqlanywhere_recall_compression", ())
            .await?;
        conn.execute("RELEASE sqlanywhere_recall_compression", ())
            .await
    }
    .await;
    let candidates = built?;
    rolled_back?;

    let chosen = candidates
        .iter()
        .filter(|c| c.recall >= target)
        .min_by_key(|c| c.bytes)
        .cloned();
    Ok(Compression { chosen, candidates })
}

async fn build_candidates(
    conn: &Connection,
    probe: &Probe<'_>,
    index: &str,
    table: &str,
    column: &str,
    options: &[String],
    candidates: &[Option<&str>],
) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    for (n, compress) in candidates.iter().enumerate() {
        let name = format!("{index}_recall_candidate_{n}");
        let mut args = vec![ident(column)];
        args.extend(options.iter().map(|o| literal(o)));
        if let Some(c) = compress {
            args.push(literal(&format!("compress_neighbors={c}")));
        }
        conn.execute(
            &format!(
                "CREATE INDEX {} ON {}(sqlanywhere_vector_idx({}))",
                ident(&name),
                ident(table),
                args.join(", ")
            ),
            (),
        )
        .await?;
        let mut rows = conn
            .query(
                "SELECT json_extract(vector_index_stats(?1), '$.bytes')",
                [name.as_str()],
            )
            .await?;
        let bytes: i64 = match rows.next().await? {
            Some(row) => row.get(0)?,
            None => return Err(Error::Misuse(format!("recall: no stats for {name}"))),
        };
        drop(rows);
        let recall = probe.recall(&name, None).await?;
        // One candidate at a time on disk.
        conn.execute("ROLLBACK TO sqlanywhere_recall_compression", ())
            .await?;
        out.push(Candidate {
            compress_neighbors: compress.map(str::to_string),
            bytes: bytes as u64,
            recall,
        });
    }
    Ok(out)
}

/// The queries and their exact neighbours, worked out once.
struct Probe<'a> {
    conn: &'a Connection,
    k: u32,
    /// Each query vector, with the rowids of its exact k nearest rows.
    queries: Vec<(Vec<u8>, HashSet<i64>)>,
}

impl<'a> Probe<'a> {
    async fn new(conn: &'a Connection, index: &str, k: u32, sample: u32) -> Result<Probe<'a>> {
        if k == 0 || sample == 0 {
            return Err(Error::Misuse(
                "recall: k and sample must be at least 1".into(),
            ));
        }
        let (table, column) = indexed_column(conn, index).await?;
        let distance = match metric(conn, index).await?.as_str() {
            "cosine" => "vector_distance_cos",
            "l2" => "vector_distance_l2",
            other => {
                return Err(Error::Misuse(format!(
                    "recall: index {index} has metric {other}, which has no distance function"
                )))
            }
        };
        let (t, c) = (ident(&table), ident(&column));

        // Evenly spread over the rows, so the sample is the same every time.
        let mut rows = conn
            .query(
                &format!("SELECT count(*) FROM {t} WHERE {c} IS NOT NULL"),
                (),
            )
            .await?;
        let count: i64 = rows
            .next()
            .await?
            .map(|r| r.get(0))
            .transpose()?
            .unwrap_or(0);
        drop(rows);
        if count == 0 {
            return Err(Error::Misuse(format!(
                "recall: {table}.{column} has no vectors"
            )));
        }
        let stride = (count / i64::from(sample)).max(1);
        let mut rows = conn
            .query(
                &format!(
                    "SELECT v FROM (SELECT {c} AS v, row_number() OVER (ORDER BY rowid) AS n \
                     FROM {t} WHERE {c} IS NOT NULL) WHERE (n - 1) % ?1 = 0 LIMIT ?2"
                ),
                params![stride, sample],
            )
            .await?;
        let mut vectors = Vec::new();
        while let Some(row) = rows.next().await? {
            match row.get_value(0)? {
                Value::Blob(v) => vectors.push(v),
                other => {
                    return Err(Error::Misuse(format!(
                        "recall: {table}.{column} holds {other:?}, not a vector"
                    )))
                }
            }
        }
        drop(rows);

        let exact = format!(
            "SELECT rowid FROM {t} WHERE {c} IS NOT NULL ORDER BY {distance}({c}, ?1) LIMIT ?2"
        );
        let mut queries = Vec::new();
        for vector in vectors {
            let ids = ids(conn, &exact, params![vector.clone(), k]).await?;
            queries.push((vector, ids.into_iter().collect()));
        }
        Ok(Probe { conn, k, queries })
    }

    /// Recall@k of `index`, which must be on the same column, at `search_l`.
    async fn recall(&self, index: &str, search_l: Option<u32>) -> Result<f64> {
        let (mut found, mut wanted) = (0usize, 0usize);
        for (vector, exact) in &self.queries {
            let got = ids(
                self.conn,
                "SELECT id FROM vector_top_k(?1, ?2, ?3, ?4)",
                params![index, vector.clone(), self.k, search_l],
            )
            .await?;
            found += got.iter().filter(|id| exact.contains(id)).count();
            wanted += exact.len();
        }
        Ok(found as f64 / wanted as f64)
    }
}

/// The table and column a vector index is on, from its `CREATE INDEX`.
async fn indexed_column(conn: &Connection, index: &str) -> Result<(String, String)> {
    let mut rows = conn
        .query(
            "SELECT tbl_name, sql FROM sqlite_schema WHERE type = 'index' AND name = ?1 COLLATE NOCASE",
            [index],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(Error::Misuse(format!("recall: no index named {index}")));
    };
    let table: String = row.get(0)?;
    let sql: String = row.get::<Option<String>>(1)?.unwrap_or_default();
    let marker = "sqlanywhere_vector_idx(";
    let Some(start) = sql.to_ascii_lowercase().find(marker) else {
        return Err(Error::Misuse(format!(
            "recall: {index} is not a vector index"
        )));
    };
    let rest = &sql[start + marker.len()..];
    let end = rest.find([',', ')']).unwrap_or(rest.len());
    let column = rest[..end]
        .trim()
        .trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']')
        .to_string();
    Ok((table, column))
}

/// The options a vector index's `CREATE INDEX` gave, after the column: the
/// string literals `'metric=cosine'`, `'max_neighbors=8'` and so on.
async fn index_options(conn: &Connection, index: &str) -> Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT sql FROM sqlite_schema WHERE type = 'index' AND name = ?1 COLLATE NOCASE",
            [index],
        )
        .await?;
    let sql: String = match rows.next().await? {
        Some(row) => row.get::<Option<String>>(0)?.unwrap_or_default(),
        None => return Err(Error::Misuse(format!("recall: no index named {index}"))),
    };
    let marker = "sqlanywhere_vector_idx(";
    let Some(start) = sql.to_ascii_lowercase().find(marker) else {
        return Err(Error::Misuse(format!(
            "recall: {index} is not a vector index"
        )));
    };
    Ok(string_literals(&sql[start + marker.len()..]))
}

/// The SQL string literals in `sql` up to its first unquoted `)`, unescaped.
fn string_literals(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    if c == '\'' {
                        if chars.peek() == Some(&'\'') {
                            chars.next();
                            s.push('\'');
                        } else {
                            break;
                        }
                    } else {
                        s.push(c);
                    }
                }
                out.push(s);
            }
            ')' => break,
            _ => {}
        }
    }
    out
}

fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

async fn metric(conn: &Connection, index: &str) -> Result<String> {
    let mut rows = conn
        .query(
            "SELECT json_extract(vector_index_stats(?1), '$.metric')",
            [index],
        )
        .await?;
    match rows.next().await? {
        Some(row) => row.get(0),
        None => Err(Error::Misuse(format!("recall: no stats for {index}"))),
    }
}

async fn ids(
    conn: &Connection,
    sql: &str,
    params: impl crate::params::IntoParams,
) -> Result<Vec<i64>> {
    let mut rows = conn.query(sql, params).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(row.get(0)?);
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
    fn string_literals_stop_at_the_call() {
        assert_eq!(
            string_literals("emb, 'metric=cosine', 'a=it''s' ) WHERE x = 'no'"),
            vec!["metric=cosine".to_string(), "a=it's".to_string()]
        );
        assert!(string_literals("emb)").is_empty());
        assert_eq!(literal("it's"), "'it''s'");
    }
}
