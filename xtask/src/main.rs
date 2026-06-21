use std::{env, process::Command};

use anyhow::{Context, Result};

fn main() {
    if let Err(e) = try_main() {
        eprintln!("{:?}", e);
        std::process::exit(-1);
    }
}

fn try_main() -> Result<()> {
    let task = env::args().nth(1);
    let arg = env::args().nth(2).unwrap_or("".to_string());
    match task.as_deref() {
        Some("build") => build()?,
        Some("build-bundled") => build_bundled()?,
        Some("build-wasm") => build_wasm(&arg)?,
        Some("sim-tests") => sim_tests(&arg)?,
        Some("test") => run_tests(&arg)?,
        Some("test-encryption") => run_tests_encryption(&arg)?,
        Some("publish") => publish(&arg)?,
        _ => print_help(),
    }
    Ok(())
}

fn print_help() {
    eprintln!(
        "Tasks:

build                  builds all languages 
build-wasm             builds the wasm components in wasm32-unknown-unknown
build-bundled          builds sqlite3 and updates the bundeled code for ffi
test                   runs the entire sqlanywhere test suite using nextest
test-encryption        runs encryption tests for embedded replicas
sim-tests <test name>  runs the sqlanywhere-server simulation test suite
publish-cratesio       publish sqlanywhere client crates to crates.io
"
    )
}

fn publish(arg: &str) -> Result<()> {
    let pkgs = [
        "sqlanywhere-ffi",
        "sqlanywhere-sqlite3-parser",
        "sqlanywhere-rusqlite",
        "sqlanywhere-sys",
        "sqlanywhere",
    ];

    for pkg in pkgs {
        println!("publishing {pkg}");
        run_cargo(&["publish", "-p", pkg, arg])?;
    }

    println!("all sqlanywhere packges published");

    Ok(())
}

fn build_wasm(_arg: &str) -> Result<()> {
    run_cargo(&[
        "check",
        "-p",
        "sqlanywhere",
        "--target",
        "wasm32-unknown-unknown",
        "--no-default-features",
        "--features",
        "cloudflare",
    ])?;

    Ok(())
}

fn run_tests(arg: &str) -> Result<()> {
    println!("installing nextest");
    run_cargo(&[
        "install",
        "--locked",
        "--version",
        "0.9.98",
        "cargo-nextest",
    ])?;
    println!("running nextest run");
    run_cargo(&["nextest", "run", arg])?;

    Ok(())
}

fn run_tests_encryption(arg: &str) -> Result<()> {
    println!("installing nextest");
    run_cargo(&[
        "install",
        "--force",
        "--locked",
        "--version",
        "0.9.98",
        "cargo-nextest",
    ])?;
    println!("running nextest run");
    run_cargo(&[
        "nextest",
        "run",
        "-F",
        "test-encryption",
        "-p",
        "sqlanywhere-server",
        "--test",
        "tests",
        "embedded_replica",
        arg,
    ])?;

    Ok(())
}

fn sim_tests(arg: &str) -> Result<()> {
    run_cargo(&["test", "--test", "tests", arg])?;

    Ok(())
}

fn build() -> Result<()> {
    run_sqlanywhere_sqlite3("./configure")?;
    run_sqlanywhere_sqlite3("make")?;

    Ok(())
}

fn build_bundled() -> Result<()> {
    build()?;

    run_cp(&[
        "sqlanywhere-sqlite3/sqlite3.c",
        "sqlanywhere-ffi/bundled/src/sqlite3.c",
    ])?;

    run_cp(&[
        "sqlanywhere-sqlite3/sqlite3.h",
        "sqlanywhere-ffi/bundled/src/sqlite3.h",
    ])?;

    // Also update SQLite3MultipleCiphers bundled files
    // These are used when building with --features multiple-ciphers
    run_cp(&[
        "sqlanywhere-sqlite3/sqlite3.c",
        "sqlanywhere-ffi/bundled/SQLite3MultipleCiphers/src/sqlite3.c",
    ])?;

    run_cp(&[
        "sqlanywhere-sqlite3/sqlite3.h",
        "sqlanywhere-ffi/bundled/SQLite3MultipleCiphers/src/sqlite3.h",
    ])?;

    Ok(())
}

fn run_cargo(cmd: &[&str]) -> Result<()> {
    let mut out = Command::new("cargo").args(cmd).spawn().context("spawn")?;

    let exit = out.wait().context("wait")?;

    if !exit.success() {
        anyhow::bail!("non 0 exit code: {}", exit);
    }

    Ok(())
}

fn run_sqlanywhere_sqlite3(cmd: &str) -> Result<()> {
    let mut out = Command::new(cmd).current_dir("sqlanywhere-sqlite3").spawn()?;

    let exit = out.wait()?;

    if !exit.success() {
        anyhow::bail!("non 0 exit code: {}", exit);
    }

    Ok(())
}

fn run_cp(cmd: &[&str]) -> Result<()> {
    let mut out = Command::new("cp").args(cmd).spawn().context("spawn")?;

    let exit = out.wait().context("wait")?;

    if !exit.success() {
        anyhow::bail!("non 0 exit code: {}", exit);
    }

    Ok(())
}
