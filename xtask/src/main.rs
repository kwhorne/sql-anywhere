use std::{env, process::Command};

use anyhow::{bail, Context, Result};

mod extensions;

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
        Some("extension-keygen") => extensions::keygen(if arg.is_empty() {
            "sqlanywhere-sqlite3/ext"
        } else {
            &arg
        })?,
        Some("sign-extensions") => sign_extensions()?,
        Some("verify-extensions") => verify_extensions()?,
        _ => print_help(),
    }
    Ok(())
}

/// `sign-extensions <dir> <release>`
fn sign_extensions() -> Result<()> {
    let args: Vec<String> = env::args().skip(2).collect();
    let dir = args.first().context("usage: sign-extensions <dir> <release>")?;
    let release = args.get(1).context("usage: sign-extensions <dir> <release>")?;
    extensions::sign(dir, release)
}

/// `verify-extensions <dir> [--pubkey PATH] [--allow-unsigned]`
fn verify_extensions() -> Result<()> {
    let args: Vec<String> = env::args().skip(2).collect();
    let mut dir = None;
    let mut pubkey = None;
    let mut allow_unsigned = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--allow-unsigned" => allow_unsigned = true,
            "--pubkey" => {
                i += 1;
                pubkey = Some(
                    args.get(i)
                        .context("--pubkey needs a path")?
                        .clone(),
                );
            }
            other if other.starts_with('-') => bail!("unknown flag {other}"),
            other => dir = Some(other.to_string()),
        }
        i += 1;
    }

    let dir = dir.context(
        "usage: verify-extensions <dir> [--pubkey PATH] [--allow-unsigned]",
    )?;
    extensions::verify(&dir, pubkey.as_deref(), allow_unsigned)
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

Extension repository:
extension-keygen [dir]           generate an extension signing key pair (run once, locally)
sign-extensions <dir> <release>  write MANIFEST.json + SHA256SUMS for a release
                                 directory and sign them when EXTENSION_SIGNING_KEY is set
verify-extensions <dir>          verify MANIFEST.json's signature and every artifact digest
                                 [--pubkey PATH] [--allow-unsigned]
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
    let mut out = Command::new(cmd)
        .current_dir("sqlanywhere-sqlite3")
        .spawn()?;

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
