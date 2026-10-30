//! The Grafana dashboard in `tools/observability` charts what sqld exports.
//!
//! A real `sqld` is started with its admin API, given a few statements and a
//! failing one, and its `/metrics` scraped: every metric the dashboard names
//! must be in the scrape, and every metric in the scrape must be described in
//! `docs/OBSERVABILITY.md`. A renamed metric, or a new one nobody documented,
//! fails here rather than as an empty panel.

use std::collections::BTreeSet;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const PREFIXES: [&str; 2] = ["sqlanywhere_", "tokio_runtime_"];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Every metric name in `text`: the identifiers that start with one of
/// sqld's prefixes.
fn metric_names(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let bytes = text.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b':';
    let mut i = 0;
    while i < bytes.len() {
        let starts = i == 0 || !ident(bytes[i - 1]);
        if starts {
            if let Some(prefix) = PREFIXES
                .iter()
                .find(|p| bytes[i..].starts_with(p.as_bytes()))
            {
                let mut end = i + prefix.len();
                while end < bytes.len() && ident(bytes[end]) {
                    end += 1;
                }
                names.insert(text[i..end].to_string());
                i = end;
                continue;
            }
        }
        i += 1;
    }
    names
}

/// The metric names a dashboard's queries use: in panel targets and in
/// template variables.
fn dashboard_metrics(dashboard: &Value) -> BTreeSet<String> {
    fn walk(v: &Value, out: &mut BTreeSet<String>) {
        match v {
            Value::Object(map) => {
                for (key, v) in map {
                    match (key.as_str(), v) {
                        ("expr" | "query" | "definition", Value::String(s)) => {
                            out.extend(metric_names(s))
                        }
                        _ => walk(v, out),
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(dashboard, &mut out);
    out
}

/// The metric names in a Prometheus text exposition.
fn scraped_metrics(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| l.split(['{', ' ']).next().unwrap().to_string())
        .collect()
}

/// A summary is exported as `name{quantile=...}`, `name_sum` and
/// `name_count`; the documentation describes it once, by its own name. A
/// counter that merely ends in `_count` is its own name.
fn base_name<'a>(name: &'a str, scraped: &BTreeSet<String>) -> &'a str {
    name.strip_suffix("_sum")
        .or_else(|| name.strip_suffix("_count"))
        .filter(|base| scraped.contains(*base))
        .unwrap_or(name)
}

struct Sqld {
    child: Child,
    log: PathBuf,
}

impl Sqld {
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Sqld {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn every_metric_on_the_dashboard_is_exported_and_every_export_documented() {
    let dashboard: Value = serde_json::from_str(
        &std::fs::read_to_string(repo().join("tools/observability/grafana/dashboards/sqld.json"))
            .unwrap(),
    )
    .unwrap();
    let wanted = dashboard_metrics(&dashboard);
    assert!(wanted.len() > 10, "{wanted:?}");

    let tmp = tempfile::tempdir().unwrap();
    let (http, admin) = (free_port(), free_port());
    let log = tmp.path().join("sqld.log");
    let sqld = Sqld {
        child: Command::new(env!("CARGO_BIN_EXE_sqld"))
            .arg("--db-path")
            .arg(tmp.path().join("data.sqld"))
            .args(["--http-listen-addr", &format!("127.0.0.1:{http}")])
            .args(["--admin-listen-addr", &format!("127.0.0.1:{admin}")])
            .args(["--checkpoint-interval-s", "1", "--no-welcome"])
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap(),
        log,
    };

    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while client
        .get(format!("http://127.0.0.1:{http}/health"))
        .send()
        .await
        .map_or(true, |r| !r.status().is_success())
    {
        assert!(
            Instant::now() < deadline,
            "sqld did not start:\n{}",
            sqld.log()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Reads, writes, a vector search and a statement that fails.
    let execute = |sql: &str| json!({"type": "execute", "stmt": {"sql": sql}});
    let body = json!({"requests": [
        execute("CREATE TABLE docs (id INTEGER PRIMARY KEY, emb FLOAT32(2))"),
        execute("CREATE INDEX docs_vec ON docs(sqlanywhere_vector_idx(emb))"),
        execute("INSERT INTO docs VALUES (1, vector32('[1,0]')), (2, vector32('[0,1]'))"),
        execute("SELECT id FROM vector_top_k('docs_vec', vector32('[1,0]'), 1)"),
        execute("SELECT no_such_column FROM docs"),
        {"type": "close"},
    ]});
    let response: Value = client
        .post(format!("http://127.0.0.1:{http}/v2/pipeline"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let results = response["results"].as_array().unwrap();
    assert!(
        results[..4].iter().all(|r| r["type"] == "ok"),
        "{response:#}"
    );
    assert_eq!(results[4]["type"], "error", "{response:#}");

    // Gauges and checkpoints are updated in the background: scrape until
    // everything the dashboard wants is there.
    let deadline = Instant::now() + Duration::from_secs(30);
    let scraped = loop {
        let text = client
            .get(format!("http://127.0.0.1:{admin}/metrics"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let scraped = scraped_metrics(&text);
        if wanted.is_subset(&scraped) || Instant::now() > deadline {
            break scraped;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let missing: Vec<_> = wanted.difference(&scraped).collect();
    assert!(
        missing.is_empty(),
        "on the dashboard but not exported: {missing:?}\nexported: {scraped:?}"
    );

    let documented =
        metric_names(&std::fs::read_to_string(repo().join("docs/OBSERVABILITY.md")).unwrap());
    let undocumented: BTreeSet<_> = scraped
        .iter()
        .map(|n| base_name(n, &scraped))
        .filter(|n| !documented.contains(*n))
        .collect();
    assert!(
        undocumented.is_empty(),
        "exported but not in docs/OBSERVABILITY.md: {undocumented:?}"
    );
}

#[test]
fn metric_names_are_whole_identifiers() {
    assert_eq!(
        metric_names(
            "sum(rate(sqlanywhere_server_reads_count{a=\"b\"}[5m])) / x_sqlanywhere_no … \
             + tokio_runtime_num_workers`"
        ),
        BTreeSet::from([
            "sqlanywhere_server_reads_count".to_string(),
            "tokio_runtime_num_workers".to_string()
        ])
    );
    assert_eq!(
        scraped_metrics("# TYPE a summary\na{quantile=\"0.5\"} 1\na_sum 2\nb 3\n"),
        BTreeSet::from(["a".into(), "a_sum".into(), "b".into()])
    );
    let scraped = BTreeSet::from(["a".to_string(), "a_count".into(), "b_count".into()]);
    assert_eq!(base_name("a_count", &scraped), "a");
    assert_eq!(base_name("a_sum", &scraped), "a");
    assert_eq!(base_name("b_count", &scraped), "b_count");
}
