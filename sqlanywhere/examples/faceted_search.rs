//! Faceted full-text search — a chapter of SQL Anywhere, not a separate product.
//!
//! Faceted search (the "filter by category, show counts per facet, drill down"
//! UX behind every store and docs site) needs no dedicated search server. It is
//! the *same engine* that does vector search, plus an FTS5 inverted index, plus
//! plain SQL `GROUP BY`:
//!
//! - **Full-text** = `FTS5 MATCH` over an inverted index.
//! - **Facets** = `GROUP BY facet_column` over the matched result set.
//! - **Drill-down** = the same `MATCH` with extra `WHERE` constraints.
//!
//! Run it:
//!
//! ```sh
//! cargo run -p sqlanywhere --example faceted_search
//! ```

use sqlanywhere::{Builder, Connection};

async fn rows2(conn: &Connection, sql: &str) -> Vec<(String, String)> {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        let a = row.get_value(0).unwrap();
        let b = row.get_value(1).unwrap();
        out.push((value_to_string(&a), value_to_string(&b)));
    }
    out
}

fn value_to_string(v: &sqlanywhere::Value) -> String {
    match v {
        sqlanywhere::Value::Text(s) => s.clone(),
        sqlanywhere::Value::Integer(i) => i.to_string(),
        sqlanywhere::Value::Real(f) => format!("{f}"),
        other => format!("{other:?}"),
    }
}

async fn seed(conn: &Connection) {
    conn.execute(
        "CREATE TABLE products (id INTEGER PRIMARY KEY, title TEXT, brand TEXT, \
         category TEXT, price REAL)",
        (),
    )
    .await
    .unwrap();
    // FTS5 inverted index over the title, backed by the products table.
    conn.execute(
        "CREATE VIRTUAL TABLE products_fts USING fts5(title, content='products', content_rowid='id')",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO products VALUES \
         (1,'wireless running headphones','Acme','audio',89), \
         (2,'wireless gaming mouse','Acme','peripherals',59), \
         (3,'running shoes trail','Trailix','footwear',120), \
         (4,'wireless keyboard','Kinetic','peripherals',49), \
         (5,'noise cancelling headphones','Acme','audio',199), \
         (6,'running watch gps','Trailix','wearables',149)",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO products_fts(rowid, title) SELECT id, title FROM products",
        (),
    )
    .await
    .unwrap();
}

#[tokio::main]
async fn main() {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    seed(&conn).await;

    println!("== Faceted search: full-text + facets + drill-down, all pure SQL ==\n");

    println!("1. Full-text results for MATCH 'wireless':");
    for (title, cat) in rows2(
        &conn,
        "SELECT p.title, p.category FROM products_fts f \
         JOIN products p ON p.id = f.rowid \
         WHERE products_fts MATCH 'wireless' ORDER BY rank",
    )
    .await
    {
        println!("   - {title}  [{cat}]");
    }

    println!("\n2. Facets for 'wireless' — counts by category, then brand:");
    for (facet, sql) in [("category", "p.category"), ("brand", "p.brand")] {
        print!("   {facet}: ");
        let counts = rows2(
            &conn,
            &format!(
                "SELECT {sql}, count(*) FROM products_fts f \
                 JOIN products p ON p.id = f.rowid \
                 WHERE products_fts MATCH 'wireless' GROUP BY {sql} ORDER BY 2 DESC, 1"
            ),
        )
        .await;
        println!(
            "{}",
            counts
                .iter()
                .map(|(k, n)| format!("{k}({n})"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    println!("\n3. Drill-down: 'wireless' AND category='peripherals' AND price < 55:");
    for (title, price) in rows2(
        &conn,
        "SELECT p.title, p.price FROM products_fts f \
         JOIN products p ON p.id = f.rowid \
         WHERE products_fts MATCH 'wireless' AND p.category = 'peripherals' AND p.price < 55",
    )
    .await
    {
        println!("   - {title}  (${price})");
    }
}
