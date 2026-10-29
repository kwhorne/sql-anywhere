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
    probe.recall(search_l).await
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
        let recall = probe.recall(Some(search_l)).await?;
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

/// The queries and their exact neighbours, worked out once.
struct Probe<'a> {
    conn: &'a Connection,
    index: String,
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
        Ok(Probe {
            conn,
            index: index.to_string(),
            k,
            queries,
        })
    }

    async fn recall(&self, search_l: Option<u32>) -> Result<f64> {
        let (mut found, mut wanted) = (0usize, 0usize);
        for (vector, exact) in &self.queries {
            let got = ids(
                self.conn,
                "SELECT id FROM vector_top_k(?1, ?2, ?3, ?4)",
                params![self.index.as_str(), vector.clone(), self.k, search_l],
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
