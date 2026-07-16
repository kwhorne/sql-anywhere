//! Faceted full-text search is a chapter of SQL Anywhere, not a product: the
//! same engine that does vector search, plus an FTS5 inverted index, plus plain
//! SQL `GROUP BY`. These tests assert full-text matching, facet counts, and
//! drill-down all compose in ordinary queries.

use sqlanywhere::{Builder, Connection};

async fn conn() -> Connection {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();

    conn.execute(
        "CREATE TABLE products (id INTEGER PRIMARY KEY, title TEXT, brand TEXT, \
         category TEXT, price REAL)",
        (),
    )
    .await
    .unwrap();
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
    conn
}

async fn ids(conn: &Connection, sql: &str) -> Vec<i64> {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get::<i64>(0).unwrap());
    }
    out
}

/// Facet counts as (key, count) pairs.
async fn facet(conn: &Connection, column: &str, query: &str) -> Vec<(String, i64)> {
    let sql = format!(
        "SELECT p.{column}, count(*) FROM products_fts f \
         JOIN products p ON p.id = f.rowid \
         WHERE products_fts MATCH '{query}' GROUP BY p.{column} ORDER BY 2 DESC, 1"
    );
    let mut rows = conn.query(&sql, ()).await.unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push((row.get::<String>(0).unwrap(), row.get::<i64>(1).unwrap()));
    }
    out
}

#[tokio::test]
async fn full_text_match() {
    let conn = conn().await;
    let hits = ids(
        &conn,
        "SELECT f.rowid FROM products_fts f WHERE products_fts MATCH 'wireless' ORDER BY rowid",
    )
    .await;
    assert_eq!(hits, vec![1, 2, 4], "three products mention 'wireless'");
}

#[tokio::test]
async fn facet_counts() {
    let conn = conn().await;

    let by_category = facet(&conn, "category", "wireless").await;
    assert_eq!(
        by_category,
        vec![("peripherals".into(), 2), ("audio".into(), 1)]
    );

    let by_brand = facet(&conn, "brand", "wireless").await;
    assert_eq!(by_brand, vec![("Acme".into(), 2), ("Kinetic".into(), 1)]);
}

#[tokio::test]
async fn drill_down_filters_facet_and_range() {
    let conn = conn().await;
    // Full-text + facet constraint + price range, in one query.
    let hits = ids(
        &conn,
        "SELECT p.id FROM products_fts f JOIN products p ON p.id = f.rowid \
         WHERE products_fts MATCH 'wireless' AND p.category = 'peripherals' AND p.price < 55",
    )
    .await;
    assert_eq!(hits, vec![4], "only the $49 wireless keyboard qualifies");
}
