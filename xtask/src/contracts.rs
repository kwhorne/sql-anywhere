//! `cargo xtask gen-contracts`: compile the storage-primitive contracts in
//! `docs/contracts/` into a typed Rust API and a language-neutral manifest.
//!
//! The documents stay the single source. Each statement in a contract's
//! Operations section carries an sqlc-style annotation, `-- name: Claim :opt`,
//! and every parameter is typed once, in the table under `## Parameters` in
//! `docs/contracts/README.md`. From those this writes:
//!
//! - `sqlanywhere/src/contracts.rs`, one module per contract with `install`,
//!   the statements as constants, and one typed function per operation;
//! - `docs/contracts/contracts.json`, the same operations as data;
//! - `clients/php/src/`, the same API for PHP on PDO: one class per contract
//!   and a small runtime;
//! - `clients/typescript/src/`, the same API for TypeScript: one module per
//!   contract and a runtime with adapters for `@libsql/client` and
//!   `node:sqlite`;
//! - `clients/python/sqlanywhere_contracts/`, the same API for Python: one
//!   module per contract and a runtime with drivers for `sqlite3` and for
//!   `sqld` over HTTP, standard library only.
//!
//! `--check` compares instead of writing, and the `generated_contracts_are_fresh`
//! test runs it in CI, so a contract cannot change without its API following.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde::Serialize;

const CONTRACTS: &[(&str, &str)] = &[
    ("queue", "QUEUE_CONTRACT.md"),
    ("cache", "CACHE_CONTRACT.md"),
    ("pubsub", "PUBSUB_CONTRACT.md"),
];

const RUST_OUT: &str = "sqlanywhere/src/contracts.rs";
const JSON_OUT: &str = "docs/contracts/contracts.json";
const PHP_OUT: &str = "clients/php/src";
const TS_OUT: &str = "clients/typescript/src";
const PY_OUT: &str = "clients/python/sqlanywhere_contracts";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
enum SqlType {
    Integer,
    Text,
    Blob,
}

#[derive(Debug, Clone, Serialize)]
struct Param {
    name: String,
    #[serde(rename = "type")]
    ty: SqlType,
    nullable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Returns {
    Exec,
    One,
    Opt,
    Many,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Transaction {
    Deferred,
    Immediate,
}

#[derive(Debug, Clone, Serialize)]
struct Statement {
    sql: String,
    params: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct Operation {
    name: String,
    heading: String,
    returns: Returns,
    #[serde(skip_serializing_if = "Option::is_none")]
    transaction: Option<Transaction>,
    params: Vec<String>,
    statements: Vec<Statement>,
    #[serde(skip)]
    source: String,
}

#[derive(Debug, Clone, Serialize)]
struct Contract {
    module: String,
    document: String,
    version: u32,
    schema: Vec<String>,
    operations: Vec<Operation>,
}

#[derive(Debug, Serialize)]
struct Manifest {
    #[serde(rename = "$comment")]
    comment: &'static str,
    parameters: Vec<Param>,
    contracts: Vec<Contract>,
}

/// `cargo xtask gen-contracts [--check]`.
pub fn gen_contracts(root: &Path, check: bool) -> Result<()> {
    let (rust, json, php, ts, py) = generate(root)?;
    let mut outputs = vec![(root.join(RUST_OUT), rust), (root.join(JSON_OUT), json)];
    outputs.extend(
        php.into_iter()
            .map(|(file, text)| (root.join(PHP_OUT).join(file), text)),
    );
    outputs.extend(
        ts.into_iter()
            .map(|(file, text)| (root.join(TS_OUT).join(file), text)),
    );
    outputs.extend(
        py.into_iter()
            .map(|(file, text)| (root.join(PY_OUT).join(file), text)),
    );
    if check {
        let stale: Vec<_> = outputs
            .iter()
            .filter(|(path, want)| std::fs::read_to_string(path).ok().as_deref() != Some(want))
            .map(|(path, _)| {
                path.strip_prefix(root)
                    .unwrap_or(path)
                    .display()
                    .to_string()
            })
            .collect();
        if !stale.is_empty() {
            bail!(
                "out of date with docs/contracts: {}; run `cargo xtask gen-contracts`",
                stale.join(", ")
            );
        }
        println!("contract API is up to date");
    } else {
        for (path, text) in &outputs {
            std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
            println!(
                "wrote {}",
                path.strip_prefix(root).unwrap_or(path).display()
            );
        }
    }
    Ok(())
}

/// Generated files: (file name in `clients/<language>/src`, contents).
type PhpFiles = Vec<(String, String)>;
type TsFiles = Vec<(String, String)>;
type PyFiles = Vec<(String, String)>;

/// The Rust API, the JSON manifest, and the PHP, TypeScript and Python files.
fn generate(root: &Path) -> Result<(String, String, PhpFiles, TsFiles, PyFiles)> {
    let dir = root.join("docs/contracts");
    let readme = read(&dir.join("README.md"))?;
    let glossary = parse_glossary(&readme)?;
    let mut contracts = Vec::new();
    for (module, file) in CONTRACTS {
        let text = read(&dir.join(file))?;
        contracts.push(parse_contract(module, file, &text).with_context(|| format!("in {file}"))?);
    }
    check_params(&contracts, &glossary)?;

    let rust = rustfmt(root, &render_rust(&contracts, &glossary))?;
    let php = render_php(&contracts, &glossary);
    let ts = render_ts(&contracts, &glossary);
    let py = render_py(&contracts, &glossary)?;
    let manifest = Manifest {
        comment: "Generated by `cargo xtask gen-contracts` from docs/contracts. Do not edit.",
        parameters: glossary.values().cloned().collect(),
        contracts,
    };
    let json = serde_json::to_string_pretty(&manifest)? + "\n";
    Ok((rust, json, php, ts, py))
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

/// Rows of the `## Parameters` table: `` | `name` | TYPE [NULL] | meaning | ``.
fn parse_glossary(readme: &str) -> Result<BTreeMap<String, Param>> {
    let section = section(readme, "## Parameters").context("README.md has no `## Parameters`")?;
    let mut out = BTreeMap::new();
    for line in section.lines() {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        if cells.len() < 4 || !cells[1].starts_with('`') {
            continue;
        }
        let name = cells[1].trim_matches('`').to_string();
        let (ty, nullable) = match cells[2].split_once(' ') {
            Some((ty, "NULL")) => (ty, true),
            None => (cells[2], false),
            Some(_) => bail!(
                "parameter {name}: type {:?} is not `TYPE` or `TYPE NULL`",
                cells[2]
            ),
        };
        let ty = match ty {
            "INTEGER" => SqlType::Integer,
            "TEXT" => SqlType::Text,
            "BLOB" => SqlType::Blob,
            other => bail!("parameter {name}: unknown type {other}"),
        };
        if out
            .insert(
                name.clone(),
                Param {
                    name: name.clone(),
                    ty,
                    nullable,
                },
            )
            .is_some()
        {
            bail!("parameter {name} is listed twice");
        }
    }
    if out.is_empty() {
        bail!("the `## Parameters` table is empty");
    }
    Ok(out)
}

/// The text from `heading` to the next heading of the same level.
fn section<'a>(doc: &'a str, heading: &str) -> Option<&'a str> {
    let start = doc.find(&format!("\n{heading}"))? + 1;
    let level = heading.split(' ').next().unwrap();
    let body = &doc[start + heading.len()..];
    let end = body.find(&format!("\n{level} ")).unwrap_or(body.len());
    Some(&body[..end])
}

/// Every ```sql block in `text`, with the `### ` heading it sits under.
fn sql_blocks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut heading = String::new();
    let mut block: Option<String> = None;
    for line in text.lines() {
        match &mut block {
            Some(body) if line.trim_start().starts_with("```") => {
                out.push((heading.clone(), std::mem::take(body)));
                block = None;
            }
            Some(body) => {
                body.push_str(line);
                body.push('\n');
            }
            None if line.trim_start().starts_with("```sql") => block = Some(String::new()),
            None => {
                if let Some(h) = line.strip_prefix("### ") {
                    heading = h.trim().to_string();
                }
            }
        }
    }
    out
}

fn parse_contract(module: &str, file: &str, text: &str) -> Result<Contract> {
    let title = text.lines().next().unwrap_or_default();
    let version = title
        .split("(v")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .and_then(|v| v.parse().ok())
        .with_context(|| format!("no `(vN)` version in the title {title:?}"))?;

    let schema_text = section(text, "## Schema").context("no `## Schema` section")?;
    let schema_block = sql_blocks(schema_text)
        .into_iter()
        .next()
        .context("no sql block under `## Schema`")?
        .1;
    let schema = statements(&schema_block);

    let ops_text = section(text, "## Operations").context("no `## Operations` section")?;
    let mut operations: Vec<Operation> = Vec::new();
    for (heading, block) in sql_blocks(ops_text) {
        let mut current: Option<(String, Returns, String)> = None;
        let mut finished = Vec::new();
        for line in block.lines() {
            if let Some(annotation) = line.trim().strip_prefix("-- name:") {
                if let Some(op) = current.take() {
                    finished.push(op);
                }
                let mut parts = annotation.split_whitespace();
                let name = parts.next().context("`-- name:` without a name")?;
                let returns = match parts.next() {
                    Some(":exec") => Returns::Exec,
                    Some(":one") => Returns::One,
                    Some(":opt") => Returns::Opt,
                    Some(":many") => Returns::Many,
                    other => bail!("{name}: expected :exec, :one, :opt or :many, got {other:?}"),
                };
                current = Some((name.to_string(), returns, String::new()));
                continue;
            }
            match &mut current {
                Some((_, _, body)) => {
                    body.push_str(line);
                    body.push('\n');
                }
                None if strip_comments(line).trim().is_empty() => {}
                None => bail!("{heading:?}: SQL before any `-- name:` annotation: {line:?}"),
            }
        }
        finished.extend(current);
        for (name, returns, body) in finished {
            operations.push(operation(&heading, name, returns, &body)?);
        }
    }

    let mut seen = HashSet::new();
    for op in &operations {
        if !seen.insert(&op.name) {
            bail!("operation {} is named twice", op.name);
        }
    }
    if operations.is_empty() {
        bail!("no annotated operations");
    }

    Ok(Contract {
        module: module.to_string(),
        document: file.to_string(),
        version,
        schema,
        operations,
    })
}

fn operation(heading: &str, name: String, returns: Returns, body: &str) -> Result<Operation> {
    let mut stmts = statements(body);
    let mut transaction = None;
    if stmts
        .first()
        .is_some_and(|s| s.to_uppercase().starts_with("BEGIN"))
    {
        if stmts.last().map(|s| s.to_uppercase()) != Some("COMMIT".into()) {
            bail!("{name}: BEGIN without a final COMMIT");
        }
        transaction = Some(if stmts[0].to_uppercase().contains("IMMEDIATE") {
            Transaction::Immediate
        } else {
            Transaction::Deferred
        });
        stmts.remove(0);
        stmts.pop();
    }
    if stmts.is_empty() {
        bail!("{name}: no statements");
    }
    if stmts.len() > 1 && transaction.is_none() {
        transaction = Some(Transaction::Deferred);
    }
    let returns_rows = |s: &str| {
        let upper = s.to_uppercase();
        upper.starts_with("SELECT") || upper.contains("RETURNING")
    };
    match returns {
        Returns::Exec => {
            if let Some(s) = stmts.iter().find(|s| returns_rows(s)) {
                bail!("{name} is :exec but returns rows: {s}");
            }
        }
        _ => {
            if stmts.len() != 1 || !returns_rows(&stmts[0]) {
                bail!("{name} returns rows, so it must be one statement that does");
            }
        }
    }
    let statements: Vec<Statement> = stmts
        .into_iter()
        .map(|sql| Statement {
            params: params_of(&sql),
            sql,
        })
        .collect();
    let mut params: Vec<String> = Vec::new();
    for p in statements.iter().flat_map(|s| &s.params) {
        if !params.contains(p) {
            params.push(p.clone());
        }
    }
    Ok(Operation {
        name,
        heading: heading.to_string(),
        returns,
        transaction,
        params,
        statements,
        source: body.trim_end().to_string(),
    })
}

fn strip_comments(line: &str) -> &str {
    match line.find("--") {
        Some(i) => &line[..i],
        None => line,
    }
}

/// The statements of a block: comments removed, split on `;`, each trimmed.
fn statements(block: &str) -> Vec<String> {
    let code: String = block
        .lines()
        .map(strip_comments)
        .collect::<Vec<_>>()
        .join("\n");
    code.split(';')
        .map(|s| {
            s.lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Named parameters (`:name`) in order of first use.
fn params_of(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut out: Vec<String> = Vec::new();
    for (i, &b) in bytes.iter().enumerate() {
        let prev_ok = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b':');
        if b == b':' && prev_ok {
            let name: String = sql[i + 1..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

fn check_params(contracts: &[Contract], glossary: &BTreeMap<String, Param>) -> Result<()> {
    let used: HashSet<&String> = contracts
        .iter()
        .flat_map(|c| &c.operations)
        .flat_map(|o| &o.params)
        .collect();
    let missing: Vec<_> = used
        .iter()
        .filter(|p| !glossary.contains_key(**p))
        .collect();
    if !missing.is_empty() {
        bail!("parameters used but missing from the `## Parameters` table: {missing:?}");
    }
    let unused: Vec<_> = glossary.keys().filter(|p| !used.contains(p)).collect();
    if !unused.is_empty() {
        bail!("parameters in the `## Parameters` table that no operation uses: {unused:?}");
    }
    Ok(())
}

fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn rust_type(p: &Param) -> String {
    let base = match p.ty {
        SqlType::Integer => "i64",
        SqlType::Text => "&str",
        SqlType::Blob => "&[u8]",
    };
    if p.nullable {
        format!("Option<{base}>")
    } else {
        base.to_string()
    }
}

fn rust_value(p: &Param) -> String {
    let n = &p.name;
    let wrap = |x: &str| match p.ty {
        SqlType::Integer => format!("Value::Integer({x})"),
        SqlType::Text => format!("Value::Text({x}.to_string())"),
        SqlType::Blob => format!("Value::Blob({x}.to_vec())"),
    };
    match (p.nullable, p.ty) {
        (true, SqlType::Integer) => format!("{n}.map_or(Value::Null, Value::Integer)"),
        (true, _) => format!("{n}.map_or(Value::Null, |v| {})", wrap("v")),
        (false, _) => wrap(n),
    }
}

fn rust_str(s: &str) -> String {
    format!("{s:?}")
}

fn render_rust(contracts: &[Contract], glossary: &BTreeMap<String, Param>) -> String {
    let mut o = String::new();
    o.push_str(
        "// @generated by `cargo xtask gen-contracts` from docs/contracts. Do not edit.\n\
         \n\
         //! Typed API for the storage-primitive contracts in `docs/contracts/`.\n\
         //!\n\
         //! One module per contract. Each has `install`, which creates its tables, and\n\
         //! one function per operation, taking the operation's parameters typed from\n\
         //! the contract's parameter table. The SQL is the contract's own, compiled\n\
         //! from the documents and proved by `tests/contract_conformance.rs`; the\n\
         //! statements are also exported as constants for callers that bind them\n\
         //! themselves.\n\
         //!\n\
         //! An operation of several statements runs them in one transaction on the\n\
         //! connection it is given, so call it outside any transaction of your own.\n\
         \n\
         #![allow(clippy::too_many_arguments)]\n\n",
    );
    for c in contracts {
        let _ = writeln!(
            o,
            "/// The {} contract, v{}: `docs/contracts/{}`.\npub mod {} {{",
            c.module, c.version, c.document, c.module
        );
        o.push_str(
            "#[allow(unused_imports)]\n\
             use crate::{params::Params, Connection, Error, Result, Row, Rows, TransactionBehavior, Value};\n\n",
        );
        let _ = writeln!(
            o,
            "/// Contract version.\npub const VERSION: u32 = {};\n",
            c.version
        );
        o.push_str("/// The contract's schema, in order. Every statement is `IF NOT EXISTS`.\npub const SCHEMA: &[&str] = &[\n");
        for s in &c.schema {
            let _ = writeln!(o, "{},", rust_str(s));
        }
        o.push_str("];\n\n");
        o.push_str(
            "/// Create the contract's tables, indexes and views if they do not exist.\n\
             pub async fn install(conn: &Connection) -> Result<()> {\n\
             for sql in SCHEMA {\n conn.execute(sql, ()).await?;\n }\n Ok(())\n}\n\n",
        );
        for op in &c.operations {
            render_op(&mut o, op, glossary);
        }
        o.push_str("}\n\n");
    }
    o
}

fn render_op(o: &mut String, op: &Operation, glossary: &BTreeMap<String, Param>) {
    let konst = snake(&op.name).to_uppercase();
    let doc_sql: String = op
        .source
        .lines()
        .map(|l| format!("/// {l}\n").replace("/// \n", "///\n"))
        .collect();
    let header = format!(
        "/// {}, from *{}*:\n///\n/// ```sql\n{doc_sql}/// ```\n",
        op.name, op.heading
    );

    if op.statements.len() == 1 {
        let _ = writeln!(
            o,
            "{header}pub const {konst}: &str = {};\n",
            rust_str(&op.statements[0].sql)
        );
    } else {
        let _ = writeln!(o, "{header}pub const {konst}: &[&str] = &[");
        for s in &op.statements {
            let _ = writeln!(o, "{},", rust_str(&s.sql));
        }
        o.push_str("];\n\n");
    }

    let args: String = op
        .params
        .iter()
        .map(|p| format!(", {p}: {}", rust_type(&glossary[p])))
        .collect::<Vec<_>>()
        .concat();
    let (ret, what) = match op.returns {
        Returns::Exec => ("u64", "the number of rows it changed"),
        Returns::One => ("Row", "its one row"),
        Returns::Opt => ("Option<Row>", "its row, or `None` when there is none"),
        Returns::Many => ("Rows", "its rows"),
    };
    let _ = writeln!(
        o,
        "/// Run [`{konst}`] and return {what}.\npub async fn {}(conn: &Connection{args}) -> Result<{ret}> {{",
        snake(&op.name)
    );

    let bind = |s: &Statement| -> String {
        let pairs: String = s
            .params
            .iter()
            .map(|p| format!("(\":{p}\".to_string(), {}),", rust_value(&glossary[p])))
            .collect::<Vec<_>>()
            .concat();
        format!("Params::Named(vec![{pairs}])")
    };

    match (op.returns, op.statements.len()) {
        (Returns::Exec, 1) => {
            let _ = writeln!(
                o,
                "conn.execute({konst}, {}).await",
                bind(&op.statements[0])
            );
        }
        (Returns::Exec, _) => {
            let behaviour = match op.transaction {
                Some(Transaction::Immediate) => "Immediate",
                _ => "Deferred",
            };
            let _ = writeln!(
                o,
                "let tx = conn.transaction_with_behavior(TransactionBehavior::{behaviour}).await?;\nlet mut changed = 0;"
            );
            for (i, s) in op.statements.iter().enumerate() {
                let _ = writeln!(
                    o,
                    "changed += tx.execute({konst}[{i}], {}).await?;",
                    bind(s)
                );
            }
            o.push_str("tx.commit().await?;\nOk(changed)\n");
        }
        (returns, _) => {
            let query = format!("conn.query({konst}, {}).await", bind(&op.statements[0]));
            if returns == Returns::Many {
                let _ = writeln!(o, "{query}");
                o.push_str("}\n\n");
                return;
            }
            // Copy the row out and run the statement to completion, so the
            // caller can hold the result without holding the statement open,
            // which would make any later COMMIT on the connection fail.
            let _ = writeln!(o, "let mut rows = {query}?;");
            o.push_str(
                "let row = match rows.next().await? {\n Some(row) => Some(row.detach()?),\n None => None,\n };\n\
                 while rows.next().await?.is_some() {}\n",
            );
            match returns {
                Returns::Opt => o.push_str("Ok(row)\n"),
                _ => {
                    let _ = writeln!(
                        o,
                        "row.ok_or_else(|| Error::Misuse({}.into()))",
                        rust_str(&format!("{} returned no row", op.name))
                    );
                }
            }
        }
    }
    o.push_str("}\n\n");
}

const PHP_HEADER: &str = "<?php\n\n// @generated by `cargo xtask gen-contracts` from docs/contracts. Do not edit.\n\ndeclare(strict_types=1);\n\nnamespace SqlAnywhere\\Contracts;\n\n";

const PHP_RUNTIME: &str = r#"/**
 * Binds and runs the contracts' statements on PDO. Every operation of the
 * generated classes goes through here.
 */
final class Runtime
{
    /**
     * Prepare $sql, bind $params (name => [value, PDO::PARAM_*]) and execute it.
     *
     * @param array<string, array{0: mixed, 1: int}> $params
     */
    public static function run(\PDO $db, string $sql, array $params): \PDOStatement
    {
        $statement = $db->prepare($sql);
        foreach ($params as $name => [$value, $type]) {
            $statement->bindValue($name, $value, $value === null ? \PDO::PARAM_NULL : $type);
        }
        $statement->execute();
        return $statement;
    }

    /**
     * The first row, then the statement run to completion, so the caller can
     * keep the row without keeping the statement open.
     *
     * @return array<string, mixed>|null
     */
    public static function first(\PDOStatement $statement): ?array
    {
        $row = $statement->fetch(\PDO::FETCH_ASSOC);
        $statement->closeCursor();
        return $row === false ? null : $row;
    }

    /**
     * Run $body between $begin ("BEGIN" or "BEGIN IMMEDIATE") and COMMIT,
     * rolling back if it throws. Call it outside any transaction of your own.
     *
     * @template T
     * @param callable(): T $body
     * @return T
     */
    public static function transaction(\PDO $db, string $begin, callable $body): mixed
    {
        $db->exec($begin);
        try {
            $result = $body();
            $db->exec('COMMIT');
            return $result;
        } catch (\Throwable $e) {
            $db->exec('ROLLBACK');
            throw $e;
        }
    }
}
"#;

/// One PHP class per contract, and the runtime they share.
fn render_php(contracts: &[Contract], glossary: &BTreeMap<String, Param>) -> PhpFiles {
    let mut files = vec![(
        "Runtime.php".to_string(),
        format!("{PHP_HEADER}{PHP_RUNTIME}"),
    )];
    for c in contracts {
        let class = php_class(&c.module);
        let mut o = String::from(PHP_HEADER);
        let _ = writeln!(
            o,
            "/**\n * The {} contract, v{}: docs/contracts/{}.\n *\n * Pass a PDO connection to a SQLite or SQL Anywhere database, with errors\n * as exceptions (the default since PHP 8).\n */\nfinal class {class}\n{{\n    public const VERSION = {};\n",
            c.module, c.version, c.document, c.version
        );
        o.push_str("    /** The contract's schema, in order. Every statement is IF NOT EXISTS. */\n    public const SCHEMA = [\n");
        for sql in &c.schema {
            let _ = writeln!(o, "        {},", php_str(sql));
        }
        o.push_str("    ];\n");
        for op in &c.operations {
            let konst = snake(&op.name).to_uppercase();
            if op.statements.len() == 1 {
                let _ = writeln!(
                    o,
                    "\n    public const {konst} = {};",
                    php_str(&op.statements[0].sql)
                );
            } else {
                let _ = writeln!(o, "\n    public const {konst} = [");
                for st in &op.statements {
                    let _ = writeln!(o, "        {},", php_str(&st.sql));
                }
                o.push_str("    ];\n");
            }
        }
        o.push_str(
            "\n    /** Create the contract's tables, indexes and views if they do not exist. */\n    public static function install(\\PDO $db): void\n    {\n        foreach (self::SCHEMA as $sql) {\n            $db->exec($sql);\n        }\n    }\n",
        );
        for op in &c.operations {
            render_php_op(&mut o, op, glossary);
        }
        o.push_str("}\n");
        files.push((format!("{class}.php"), o));
    }
    files
}

fn render_php_op(o: &mut String, op: &Operation, glossary: &BTreeMap<String, Param>) {
    let konst = snake(&op.name).to_uppercase();
    let mut args = String::new();
    for p in &op.params {
        let _ = write!(args, ", {} ${}", php_type(&glossary[p]), camel(p));
    }
    let (ret, what) = match op.returns {
        Returns::Exec => ("int", "the number of rows it changed"),
        Returns::One => ("array", "its one row, by column name"),
        Returns::Opt => (
            "?array",
            "its row by column name, or null when there is none",
        ),
        Returns::Many => ("array", "its rows, by column name"),
    };
    let _ = writeln!(
        o,
        "\n    /**\n     * {}, from \"{}\": run self::{konst} and return {what}.\n     */\n    public static function {}(\\PDO $db{args}): {ret}\n    {{",
        op.name,
        op.heading,
        camel(&snake(&op.name))
    );
    let bind = |st: &Statement| -> String {
        let pairs: Vec<String> = st
            .params
            .iter()
            .map(|p| format!("':{p}' => [${}, {}]", camel(p), php_pdo_type(&glossary[p])))
            .collect();
        format!("[{}]", pairs.join(", "))
    };
    match (op.returns, op.statements.len()) {
        (Returns::Exec, 1) => {
            let _ = writeln!(
                o,
                "        return Runtime::run($db, self::{konst}, {})->rowCount();",
                bind(&op.statements[0])
            );
        }
        (Returns::Exec, _) => {
            let begin = match op.transaction {
                Some(Transaction::Immediate) => "BEGIN IMMEDIATE",
                _ => "BEGIN",
            };
            let used: Vec<String> = std::iter::once("$db".to_string())
                .chain(op.params.iter().map(|p| format!("${}", camel(p))))
                .collect();
            let _ = writeln!(
                o,
                "        return Runtime::transaction($db, '{begin}', function () use ({}): int {{\n            $changed = 0;",
                used.join(", ")
            );
            for (i, st) in op.statements.iter().enumerate() {
                let _ = writeln!(
                    o,
                    "            $changed += Runtime::run($db, self::{konst}[{i}], {})->rowCount();",
                    bind(st)
                );
            }
            o.push_str("            return $changed;\n        });\n");
        }
        (Returns::Many, _) => {
            let _ = writeln!(
                o,
                "        return Runtime::run($db, self::{konst}, {})->fetchAll(\\PDO::FETCH_ASSOC);",
                bind(&op.statements[0])
            );
        }
        (Returns::Opt, _) => {
            let _ = writeln!(
                o,
                "        return Runtime::first(Runtime::run($db, self::{konst}, {}));",
                bind(&op.statements[0])
            );
        }
        (Returns::One, _) => {
            let _ = writeln!(
                o,
                "        return Runtime::first(Runtime::run($db, self::{konst}, {}))\n            ?? throw new \\RuntimeException('{} returned no row');",
                bind(&op.statements[0]),
                op.name
            );
        }
    }
    o.push_str("    }\n");
}

fn php_class(module: &str) -> &'static str {
    match module {
        "queue" => "Queue",
        "cache" => "Cache",
        "pubsub" => "PubSub",
        other => panic!("no PHP class name for contract `{other}`"),
    }
}

fn php_type(p: &Param) -> String {
    let base = match p.ty {
        SqlType::Integer => "int",
        SqlType::Text | SqlType::Blob => "string",
    };
    if p.nullable {
        format!("?{base}")
    } else {
        base.to_string()
    }
}

fn php_pdo_type(p: &Param) -> &'static str {
    match p.ty {
        SqlType::Integer => "\\PDO::PARAM_INT",
        SqlType::Text => "\\PDO::PARAM_STR",
        SqlType::Blob => "\\PDO::PARAM_LOB",
    }
}

/// A single-quoted PHP string: only `\\` and `'` need escaping.
fn php_str(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

const TS_HEADER: &str =
    "// @generated by `cargo xtask gen-contracts` from docs/contracts. Do not edit.\n\n";

const TS_RUNTIME: &str = r#"/** A value bound to a statement or read from a row. */
export type Value = null | number | bigint | string | Uint8Array;

/** A row, by column name. */
export type Row = Record<string, Value>;

/** A statement and its named arguments, keyed without the `:`. */
export interface Statement {
  sql: string;
  args: Record<string, Value>;
}

/** What a statement returned: its rows, and the rows it changed. */
export interface Result {
  rows: Row[];
  changes: number;
}

/**
 * What the contracts run on. `fromLibsql` adapts `@libsql/client` (a `sqld`
 * server over HTTP, or a local file) and `fromNodeSqlite` adapts `node:sqlite`;
 * anything that can run a statement, and a list of them in one transaction,
 * will do.
 */
export interface Driver {
  execute(statement: Statement): Promise<Result>;
  /**
   * Run `statements` in order in one transaction, `BEGIN` for "deferred" and
   * `BEGIN IMMEDIATE` for "immediate", and roll back if one fails.
   */
  batch(statements: Statement[], mode: "deferred" | "immediate"): Promise<Result[]>;
}

/** The first row, or null when there is none. */
export function first(result: Result): Row | null {
  return result.rows[0] ?? null;
}

/** The first row, or an error naming `operation` when there is none. */
export function one(result: Result, operation: string): Row {
  const row = first(result);
  if (row === null) {
    throw new Error(`${operation} returned no row`);
  }
  return row;
}

/** Bytes as they are, a string as its UTF-8 bytes: a blob either way. */
export function blob(value: Uint8Array | string): Uint8Array {
  return typeof value === "string" ? new TextEncoder().encode(value) : value;
}

/** The parts of `@libsql/client`'s `Client` that `fromLibsql` uses. */
export interface LibsqlClient {
  execute(statement: { sql: string; args: Record<string, Value> }): Promise<LibsqlResultSet>;
  batch(
    statements: { sql: string; args: Record<string, Value> }[],
    mode: "write" | "deferred",
  ): Promise<LibsqlResultSet[]>;
}

/** The parts of `@libsql/client`'s `ResultSet` that `fromLibsql` reads. */
export interface LibsqlResultSet {
  columns: string[];
  rows: ArrayLike<unknown>[];
  rowsAffected: number;
}

/** A driver on `@libsql/client`, for `sqld` or a local file. */
export function fromLibsql(client: LibsqlClient): Driver {
  const result = (set: LibsqlResultSet): Result => ({
    rows: set.rows.map((row) =>
      Object.fromEntries(set.columns.map((column, i) => [column, libsqlValue(row[i])])),
    ),
    changes: set.rowsAffected,
  });
  return {
    async execute(statement) {
      return result(await client.execute(statement));
    },
    async batch(statements, mode) {
      // "write" is libsql's name for BEGIN IMMEDIATE.
      const sets = await client.batch(statements, mode === "immediate" ? "write" : "deferred");
      return sets.map(result);
    },
  };
}

function libsqlValue(value: unknown): Value {
  // libsql returns blobs as ArrayBuffers.
  return value instanceof ArrayBuffer ? new Uint8Array(value) : (value as Value);
}

/** The parts of `node:sqlite`'s `DatabaseSync` that `fromNodeSqlite` uses. */
export interface NodeSqliteDatabase {
  exec(sql: string): void;
  prepare(sql: string): {
    all(args: Record<string, Value>): unknown[];
    get(): unknown;
  };
}

/** A driver on `node:sqlite`'s `DatabaseSync`, for a local file. */
export function fromNodeSqlite(db: NodeSqliteDatabase): Driver {
  const execute = (statement: Statement): Result => {
    const rows = db.prepare(statement.sql).all(statement.args) as Row[];
    const { n } = db.prepare("SELECT changes() AS n").get() as { n: number };
    return { rows, changes: n };
  };
  return {
    async execute(statement) {
      return execute(statement);
    },
    async batch(statements, mode) {
      db.exec(mode === "immediate" ? "BEGIN IMMEDIATE" : "BEGIN");
      try {
        const results = statements.map(execute);
        db.exec("COMMIT");
        return results;
      } catch (e) {
        db.exec("ROLLBACK");
        throw e;
      }
    },
  };
}
"#;

/// One TypeScript module per contract, the runtime they share, and an index.
fn render_ts(contracts: &[Contract], glossary: &BTreeMap<String, Param>) -> TsFiles {
    let mut files = vec![("runtime.ts".to_string(), format!("{TS_HEADER}{TS_RUNTIME}"))];
    let mut index = String::from(TS_HEADER);
    for c in contracts {
        let mut o = String::from(TS_HEADER);
        o.push_str("import { blob, first, one, type Driver, type Row } from \"./runtime.ts\";\n\n");
        let _ = writeln!(
            o,
            "/** The {} contract's version: docs/contracts/{}. */\nexport const VERSION = {};\n",
            c.module, c.document, c.version
        );
        o.push_str("/** The contract's schema, in order. Every statement is IF NOT EXISTS. */\nexport const SCHEMA: readonly string[] = [\n");
        for sql in &c.schema {
            let _ = writeln!(o, "  {},", ts_str(sql));
        }
        o.push_str("];\n");
        for op in &c.operations {
            let konst = snake(&op.name).to_uppercase();
            if op.statements.len() == 1 {
                let _ = writeln!(
                    o,
                    "\nexport const {konst} = {};",
                    ts_str(&op.statements[0].sql)
                );
            } else {
                let _ = writeln!(o, "\nexport const {konst}: readonly string[] = [");
                for st in &op.statements {
                    let _ = writeln!(o, "  {},", ts_str(&st.sql));
                }
                o.push_str("];\n");
            }
        }
        o.push_str(
            "\n/** Create the contract's tables, indexes and views if they do not exist. */\nexport async function install(db: Driver): Promise<void> {\n  for (const sql of SCHEMA) {\n    await db.execute({ sql, args: {} });\n  }\n}\n",
        );
        for op in &c.operations {
            render_ts_op(&mut o, op, glossary);
        }
        // Not every module uses every helper.
        o = ts_prune_imports(o);
        files.push((format!("{}.ts", c.module), o));
        let _ = writeln!(
            index,
            "export * as {} from \"./{}.ts\";",
            c.module, c.module
        );
    }
    index.push_str("export * from \"./runtime.ts\";\n");
    files.push(("index.ts".to_string(), index));
    files
}

fn render_ts_op(o: &mut String, op: &Operation, glossary: &BTreeMap<String, Param>) {
    let konst = snake(&op.name).to_uppercase();
    let fields: Vec<String> = op
        .params
        .iter()
        .map(|p| {
            let param = &glossary[p];
            let ty = match param.ty {
                SqlType::Integer => "number",
                SqlType::Text => "string",
                SqlType::Blob => "Uint8Array | string",
            };
            if param.nullable {
                format!("{}?: {ty} | null", camel(p))
            } else {
                format!("{}: {ty}", camel(p))
            }
        })
        .collect();
    let args = if fields.is_empty() {
        String::new()
    } else {
        format!(", args: {{ {} }}", fields.join("; "))
    };
    let (ret, what) = match op.returns {
        Returns::Exec => ("number", "the number of rows it changed"),
        Returns::One => ("Row", "its one row, by column name"),
        Returns::Opt => (
            "Row | null",
            "its row by column name, or null when there is none",
        ),
        Returns::Many => ("Row[]", "its rows, by column name"),
    };
    let _ = writeln!(
        o,
        "\n/** {}, from \"{}\": run `{konst}` and return {what}. */\nexport async function {}(db: Driver{args}): Promise<{ret}> {{",
        op.name,
        op.heading,
        camel(&snake(&op.name))
    );
    let bind = |st: &Statement| -> String {
        let pairs: Vec<String> = st
            .params
            .iter()
            .map(|p| {
                let param = &glossary[p];
                let value = match (param.ty, param.nullable) {
                    (SqlType::Blob, false) => format!("blob(args.{})", camel(p)),
                    (SqlType::Blob, true) => {
                        format!("args.{0} == null ? null : blob(args.{0})", camel(p))
                    }
                    (_, true) => format!("args.{} ?? null", camel(p)),
                    (_, false) => format!("args.{}", camel(p)),
                };
                format!("{p}: {value}")
            })
            .collect();
        if pairs.is_empty() {
            "{}".to_string()
        } else {
            format!("{{ {} }}", pairs.join(", "))
        }
    };
    match (op.returns, op.statements.len()) {
        (Returns::Exec, 1) => {
            let _ = writeln!(
                o,
                "  return (await db.execute({{ sql: {konst}, args: {} }})).changes;",
                bind(&op.statements[0])
            );
        }
        (Returns::Exec, _) => {
            let mode = match op.transaction {
                Some(Transaction::Immediate) => "immediate",
                _ => "deferred",
            };
            o.push_str("  const results = await db.batch(\n    [\n");
            for (i, st) in op.statements.iter().enumerate() {
                let _ = writeln!(o, "      {{ sql: {konst}[{i}]!, args: {} }},", bind(st));
            }
            let _ = writeln!(
                o,
                "    ],\n    \"{mode}\",\n  );\n  return results.reduce((changed, result) => changed + result.changes, 0);"
            );
        }
        (Returns::Many, _) => {
            let _ = writeln!(
                o,
                "  return (await db.execute({{ sql: {konst}, args: {} }})).rows;",
                bind(&op.statements[0])
            );
        }
        (Returns::Opt, _) => {
            let _ = writeln!(
                o,
                "  return first(await db.execute({{ sql: {konst}, args: {} }}));",
                bind(&op.statements[0])
            );
        }
        (Returns::One, _) => {
            let _ = writeln!(
                o,
                "  return one(await db.execute({{ sql: {konst}, args: {} }}), \"{}\");",
                bind(&op.statements[0]),
                op.name
            );
        }
    }
    o.push_str("}\n");
}

/// Drop the runtime helpers a module does not call from its import line.
fn ts_prune_imports(module: String) -> String {
    let (head, body) = module
        .split_once("\n\n/** The ")
        .map(|(h, b)| (h.to_string(), format!("\n\n/** The {b}")))
        .expect("module has a header");
    let used: Vec<&str> = ["blob", "first", "one"]
        .into_iter()
        .filter(|name| body.contains(&format!("{name}(")))
        .collect();
    let mut names: Vec<String> = used.iter().map(|n| n.to_string()).collect();
    names.push("type Driver".into());
    if body.contains("Row") {
        names.push("type Row".into());
    }
    let import = format!("import {{ {} }} from \"./runtime.ts\";", names.join(", "));
    let head = head
        .lines()
        .map(|line| {
            if line.starts_with("import ") {
                import.clone()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{head}{body}")
}

/// A TypeScript template literal: `\`, `` ` `` and `${` need escaping.
fn ts_str(s: &str) -> String {
    format!(
        "`{}`",
        s.replace('\\', "\\\\")
            .replace('`', "\\`")
            .replace("${", "\\${")
    )
}

/// `max_attempts` → `maxAttempts`.
fn camel(name: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for ch in name.chars() {
        if ch == '_' {
            upper = true;
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

const PY_HEADER: &str =
    "# @generated by `cargo xtask gen-contracts` from docs/contracts. Do not edit.\n\n";

const PY_RUNTIME: &str = r#""""What the contract modules run on: a `Driver`, and two of them."""

from __future__ import annotations

import base64
import json
import sqlite3
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import Any, Dict, List, Mapping, NamedTuple, Optional, Protocol, Sequence, Union

#: A value bound to a statement or read from a row.
Value = Union[None, int, float, str, bytes]

#: A row, by column name.
Row = Dict[str, Value]


class Statement(NamedTuple):
    """A statement and its named arguments, keyed without the `:`."""

    sql: str
    args: Mapping[str, Value]


@dataclass
class Result:
    """What a statement returned: its rows, and the rows it changed."""

    rows: List[Row] = field(default_factory=list)
    changes: int = 0


class Driver(Protocol):
    """What the contracts run on.

    `from_sqlite3` adapts the standard library's `sqlite3` (a local file) and
    `from_sqld` a `sqld` server over HTTP; anything that can run a statement,
    and a list of them in one transaction, will do.
    """

    def execute(self, statement: Statement) -> Result: ...

    def batch(self, statements: Sequence[Statement], mode: str) -> List[Result]:
        """Run `statements` in order in one transaction, `BEGIN` for "deferred"
        and `BEGIN IMMEDIATE` for "immediate", and roll back if one fails."""
        ...


def first(result: Result) -> Optional[Row]:
    """The first row, or None when there is none."""
    return result.rows[0] if result.rows else None


def one(result: Result, operation: str) -> Row:
    """The first row, or an error naming `operation` when there is none."""
    row = first(result)
    if row is None:
        raise LookupError(f"{operation} returned no row")
    return row


def blob(value: Union[bytes, str]) -> bytes:
    """Bytes as they are, a string as its UTF-8 bytes: a blob either way."""
    return value.encode() if isinstance(value, str) else bytes(value)


def _begin(mode: str) -> str:
    return "BEGIN IMMEDIATE" if mode == "immediate" else "BEGIN"


class _Sqlite3Driver:
    def __init__(self, conn: sqlite3.Connection) -> None:
        if conn.isolation_level is not None:
            raise ValueError(
                "from_sqlite3 needs a connection in autocommit mode, "
                "sqlite3.connect(path, isolation_level=None): with the default, "
                "sqlite3 opens transactions of its own and nothing is committed"
            )
        self._conn = conn

    def execute(self, statement: Statement) -> Result:
        cursor = self._conn.execute(statement.sql, dict(statement.args))
        columns = [d[0] for d in cursor.description or ()]
        rows = [dict(zip(columns, row)) for row in cursor.fetchall()]
        (changes,) = self._conn.execute("SELECT changes()").fetchone()
        return Result(rows, changes)

    def batch(self, statements: Sequence[Statement], mode: str) -> List[Result]:
        self._conn.execute(_begin(mode))
        try:
            results = [self.execute(s) for s in statements]
            self._conn.execute("COMMIT")
            return results
        except BaseException:
            self._conn.execute("ROLLBACK")
            raise


def from_sqlite3(conn: sqlite3.Connection) -> Driver:
    """A driver on the standard library's `sqlite3`, for a local file. The
    connection must be in autocommit mode (`isolation_level=None`)."""
    return _Sqlite3Driver(conn)


class SqldError(Exception):
    """A statement `sqld` refused, with its error code (`SQLITE_CONSTRAINT`, ...)."""

    def __init__(self, message: str, code: Optional[str] = None) -> None:
        super().__init__(message)
        self.code = code


def _encode(value: Value) -> Dict[str, Any]:
    if value is None:
        return {"type": "null"}
    if isinstance(value, bool):
        return {"type": "integer", "value": str(int(value))}
    if isinstance(value, int):
        return {"type": "integer", "value": str(value)}
    if isinstance(value, float):
        return {"type": "float", "value": value}
    if isinstance(value, str):
        return {"type": "text", "value": value}
    if isinstance(value, (bytes, bytearray, memoryview)):
        return {"type": "blob", "base64": base64.b64encode(bytes(value)).decode()}
    raise TypeError(f"cannot bind {type(value).__name__}")


def _decode(value: Mapping[str, Any]) -> Value:
    kind = value["type"]
    if kind == "null":
        return None
    if kind == "integer":
        return int(value["value"])
    if kind == "float":
        return float(value["value"])
    if kind == "text":
        return str(value["value"])
    if kind == "blob":
        # sqld writes base64 without padding, which b64decode wants back.
        data = value.get("base64", "")
        return base64.b64decode(data + "=" * (-len(data) % 4))
    raise ValueError(f"unknown value type {kind}")


def _stmt(statement: Statement) -> Dict[str, Any]:
    return {
        "sql": statement.sql,
        "named_args": [{"name": f":{k}", "value": _encode(v)} for k, v in statement.args.items()],
        "want_rows": True,
    }


def _result(result: Mapping[str, Any]) -> Result:
    columns = [c.get("name") or "" for c in result["cols"]]
    rows = [dict(zip(columns, (_decode(v) for v in row))) for row in result["rows"]]
    return Result(rows, int(result["affected_row_count"]))


def _error(error: Mapping[str, Any]) -> SqldError:
    return SqldError(error.get("message", "sqld error"), error.get("code"))


class _SqldDriver:
    def __init__(self, url: str, auth_token: Optional[str], timeout: float) -> None:
        self._url = url.rstrip("/") + "/v2/pipeline"
        self._headers = {"Content-Type": "application/json"}
        if auth_token:
            self._headers["Authorization"] = f"Bearer {auth_token}"
        self._timeout = timeout

    def _pipeline(self, request: Dict[str, Any]) -> Mapping[str, Any]:
        body = json.dumps({"requests": [request, {"type": "close"}]}).encode()
        req = urllib.request.Request(self._url, data=body, headers=self._headers, method="POST")
        try:
            with urllib.request.urlopen(req, timeout=self._timeout) as response:
                reply = json.load(response)
        except urllib.error.HTTPError as e:
            raise SqldError(f"sqld answered {e.code}: {e.read().decode(errors='replace')}") from e
        result = reply["results"][0]
        if result["type"] == "error":
            raise _error(result["error"])
        answer: Mapping[str, Any] = result["response"]
        return answer

    def execute(self, statement: Statement) -> Result:
        return _result(self._pipeline({"type": "execute", "stmt": _stmt(statement)})["result"])

    def batch(self, statements: Sequence[Statement], mode: str) -> List[Result]:
        # One request, so it holds over HTTP, where separate BEGIN and COMMIT
        # requests could land on different connections: each statement runs
        # only if the one before it succeeded, then COMMIT, or ROLLBACK when
        # the COMMIT step did not run or failed. (Closing the stream, as the
        # request also does, would roll back too; the ROLLBACK says so.)
        steps: List[Dict[str, Any]] = [{"stmt": {"sql": _begin(mode)}}]
        for s in statements:
            steps.append({"condition": {"type": "ok", "step": len(steps) - 1}, "stmt": _stmt(s)})
        commit = len(steps)
        steps.append({"condition": {"type": "ok", "step": commit - 1}, "stmt": {"sql": "COMMIT"}})
        steps.append(
            {"condition": {"type": "not", "cond": {"type": "ok", "step": commit}}, "stmt": {"sql": "ROLLBACK"}}
        )
        result = self._pipeline({"type": "batch", "batch": {"steps": steps}})["result"]
        for error in result["step_errors"]:
            if error is not None:
                raise _error(error)
        return [_result(r) for r in result["step_results"][1:commit]]


def from_sqld(url: str, auth_token: Optional[str] = None, timeout: float = 30.0) -> Driver:
    """A driver on a `sqld` server over HTTP (`http://127.0.0.1:8080`), with
    nothing but the standard library."""
    return _SqldDriver(url, auth_token, timeout)
"#;

/// One Python module per contract, the runtime they share, and the package's
/// `__init__`.
fn render_py(contracts: &[Contract], glossary: &BTreeMap<String, Param>) -> Result<PyFiles> {
    // Parameters become keyword arguments: a Python keyword cannot be one.
    const KEYWORDS: &[&str] = &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
        "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
        "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return",
        "try", "while", "with", "yield",
    ];
    if let Some(p) = glossary.keys().find(|p| KEYWORDS.contains(&p.as_str())) {
        bail!("parameter `{p}` is a Python keyword and cannot be an argument name");
    }

    let mut files = vec![("runtime.py".to_string(), format!("{PY_HEADER}{PY_RUNTIME}"))];
    let mut init = format!(
        "{PY_HEADER}\"\"\"The SQL Anywhere storage contracts: a durable queue, a KV cache with TTL\nand locks, and pub/sub over an event log, on any `Driver`.\"\"\"\n\n"
    );
    let modules: Vec<&str> = contracts.iter().map(|c| c.module.as_str()).collect();
    let _ = writeln!(init, "from . import {}", modules.join(", "));
    init.push_str("from .runtime import (\n");
    let exports = [
        "Driver",
        "Result",
        "Row",
        "SqldError",
        "Statement",
        "Value",
        "from_sqld",
        "from_sqlite3",
    ];
    for name in exports {
        let _ = writeln!(init, "    {name},");
    }
    init.push_str(")\n\n__all__ = [\n");
    for name in modules.iter().copied().chain(exports) {
        let _ = writeln!(init, "    \"{name}\",");
    }
    init.push_str("]\n");

    for c in contracts {
        let mut body = String::new();
        let _ = writeln!(
            body,
            "#: The {} contract's version: docs/contracts/{}.\nVERSION = {}\n",
            c.module, c.document, c.version
        );
        body.push_str("#: The contract's schema, in order. Every statement is IF NOT EXISTS.\nSCHEMA: Tuple[str, ...] = (\n");
        for sql in &c.schema {
            let _ = writeln!(body, "    {},", py_str(sql));
        }
        body.push_str(")\n");
        for op in &c.operations {
            let konst = snake(&op.name).to_uppercase();
            if op.statements.len() == 1 {
                let _ = writeln!(body, "\n{konst} = {}", py_str(&op.statements[0].sql));
            } else {
                let _ = writeln!(body, "\n{konst}: Tuple[str, ...] = (");
                for st in &op.statements {
                    let _ = writeln!(body, "    {},", py_str(&st.sql));
                }
                body.push_str(")\n");
            }
        }
        body.push_str(
            "\n\ndef install(db: Driver) -> None:\n    \"\"\"Create the contract's tables, indexes and views if they do not exist.\"\"\"\n    for sql in SCHEMA:\n        db.execute(Statement(sql, {}))\n",
        );
        for op in &c.operations {
            render_py_op(&mut body, op, glossary);
        }

        // Import only what the module uses.
        let mut runtime = vec!["Driver", "Statement"];
        for name in ["Row", "blob", "first", "one"] {
            let used = if name == "Row" {
                body.contains("Row")
            } else {
                body.contains(&format!("{name}("))
            };
            if used {
                runtime.push(name);
            }
        }
        runtime.sort_by_key(|n| (n.chars().next().unwrap().is_lowercase(), *n));
        let mut typing = vec!["Tuple"];
        for name in ["List", "Optional", "Union"] {
            if body.contains(&format!("{name}[")) {
                typing.push(name);
            }
        }
        typing.sort();
        let mut o = String::from(PY_HEADER);
        let _ = writeln!(
            o,
            "\"\"\"The {} contract: docs/contracts/{}.\"\"\"\n\nfrom __future__ import annotations\n\nfrom typing import {}\n\nfrom .runtime import {}\n",
            c.module,
            c.document,
            typing.join(", "),
            runtime.join(", ")
        );
        o.push_str(&body);
        files.push((format!("{}.py", c.module), o));
    }
    files.push(("__init__.py".to_string(), init));
    Ok(files)
}

fn render_py_op(o: &mut String, op: &Operation, glossary: &BTreeMap<String, Param>) {
    let konst = snake(&op.name).to_uppercase();
    // Required arguments first, then the nullable ones, which default to None.
    let mut required = Vec::new();
    let mut optional = Vec::new();
    for p in &op.params {
        let param = &glossary[p];
        let ty = match param.ty {
            SqlType::Integer => "int",
            SqlType::Text => "str",
            SqlType::Blob => "Union[bytes, str]",
        };
        if param.nullable {
            optional.push(format!("{p}: Optional[{ty}] = None"));
        } else {
            required.push(format!("{p}: {ty}"));
        }
    }
    let args: Vec<String> = required.into_iter().chain(optional).collect();
    let args = if args.is_empty() {
        String::new()
    } else {
        format!(", *, {}", args.join(", "))
    };
    let (ret, what) = match op.returns {
        Returns::Exec => ("int", "the number of rows it changed"),
        Returns::One => ("Row", "its one row, by column name"),
        Returns::Opt => (
            "Optional[Row]",
            "its row by column name, or None when there is none",
        ),
        Returns::Many => ("List[Row]", "its rows, by column name"),
    };
    let _ = writeln!(
        o,
        "\n\ndef {}(db: Driver{args}) -> {ret}:\n    \"\"\"{}, from \"{}\": run `{konst}` and return {what}.\"\"\"",
        snake(&op.name),
        op.name,
        op.heading
    );
    let bind = |st: &Statement| -> String {
        let pairs: Vec<String> = st
            .params
            .iter()
            .map(|p| {
                let param = &glossary[p];
                let value = match (param.ty, param.nullable) {
                    (SqlType::Blob, false) => format!("blob({p})"),
                    (SqlType::Blob, true) => format!("None if {p} is None else blob({p})"),
                    _ => p.to_string(),
                };
                format!("\"{p}\": {value}")
            })
            .collect();
        format!("{{{}}}", pairs.join(", "))
    };
    match (op.returns, op.statements.len()) {
        (Returns::Exec, 1) => {
            let _ = writeln!(
                o,
                "    return db.execute(Statement({konst}, {})).changes",
                bind(&op.statements[0])
            );
        }
        (Returns::Exec, _) => {
            let mode = match op.transaction {
                Some(Transaction::Immediate) => "immediate",
                _ => "deferred",
            };
            o.push_str("    results = db.batch(\n        [\n");
            for (i, st) in op.statements.iter().enumerate() {
                let _ = writeln!(o, "            Statement({konst}[{i}], {}),", bind(st));
            }
            let _ = writeln!(
                o,
                "        ],\n        \"{mode}\",\n    )\n    return sum(result.changes for result in results)"
            );
        }
        (Returns::Many, _) => {
            let _ = writeln!(
                o,
                "    return db.execute(Statement({konst}, {})).rows",
                bind(&op.statements[0])
            );
        }
        (Returns::Opt, _) => {
            let _ = writeln!(
                o,
                "    return first(db.execute(Statement({konst}, {})))",
                bind(&op.statements[0])
            );
        }
        (Returns::One, _) => {
            let _ = writeln!(
                o,
                "    return one(db.execute(Statement({konst}, {})), \"{}\")",
                bind(&op.statements[0]),
                op.name
            );
        }
    }
}

/// A Python string literal on one line.
fn py_str(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn rustfmt(root: &Path, code: &str) -> Result<String> {
    let mut child = Command::new("rustfmt")
        .args(["--edition", "2021", "--emit", "stdout"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running rustfmt")?;
    child.stdin.take().unwrap().write_all(code.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "rustfmt rejected the generated code:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8(out.stdout)?)
}

/// The repository root, for the test and the CLI.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CI's guard: the checked-in API and manifest are what the documents
    /// generate now.
    #[test]
    fn generated_contracts_are_fresh() {
        gen_contracts(&repo_root(), true).unwrap();
    }

    #[test]
    fn python_strings_are_one_escaped_line() {
        assert_eq!(py_str("a 'b'\n\"c\" \\d"), r#""a 'b'\n\"c\" \\d""#);
    }

    #[test]
    fn params_are_found_in_order_without_casts_or_words() {
        assert_eq!(
            params_of("SELECT :a, x::text, 'a:b', :b_c, :a"),
            vec!["a", "b_c"]
        );
    }

    #[test]
    fn unannotated_sql_is_refused() {
        let doc = "# T contract (v1)\n\n## Schema\n\n```sql\nCREATE TABLE t (x);\n```\n\n\
                   ## Operations\n\n### Op\n\n```sql\nDELETE FROM t;\n```\n";
        let err = parse_contract("t", "T.md", doc).unwrap_err();
        assert!(
            format!("{err:#}").contains("before any `-- name:`"),
            "{err:#}"
        );
    }

    #[test]
    fn exec_that_returns_rows_is_refused() {
        let doc = "# T contract (v1)\n\n## Schema\n\n```sql\nCREATE TABLE t (x);\n```\n\n\
                   ## Operations\n\n### Op\n\n```sql\n-- name: Op :exec\nDELETE FROM t RETURNING x;\n```\n";
        assert!(parse_contract("t", "T.md", doc).is_err());
    }

    #[test]
    fn begin_commit_becomes_a_transaction() {
        let op = operation(
            "h",
            "Move".into(),
            Returns::Exec,
            "BEGIN IMMEDIATE;\nINSERT INTO a SELECT 1;\nDELETE FROM b;\nCOMMIT;",
        )
        .unwrap();
        assert_eq!(op.transaction, Some(Transaction::Immediate));
        assert_eq!(op.statements.len(), 2);
    }
}
