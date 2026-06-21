#!/bin/sh
# Regenerate the bundled SQLite amalgamation and Rust bindings used by
# sqlanywhere-ffi from the sqlanywhere-sqlite3 source tree.
#
# Outputs (relative to sqlanywhere-ffi/):
#   bundled/src/sqlite3.c
#   bundled/src/sqlite3.h
#   bundled/bindings/bindgen.rs
#   bundled/bindings/session_bindgen.rs
#   bundled/SQLite3MultipleCiphers/src/sqlite3.c
#   bundled/SQLite3MultipleCiphers/src/sqlite3.h
#
# The CI job in .github/workflows/c-bindings.yml validates the bundle by
# running `cargo xtask build-bundled` and failing if it produces any diff.
# That xtask only regenerates the C amalgamation (and copies it into both
# bundled/src/ and bundled/SQLite3MultipleCiphers/src/); it does NOT
# regenerate the bindgen files. So we do both here:
#
#   1. The SQLANYWHERE_DEV builds regenerate the Rust bindings (bindgen.rs /
#      session_bindgen.rs) via build.rs.
#   2. `cargo xtask build-bundled` produces the canonical sqlite3.{c,h} in
#      both bundled locations, byte-for-byte matching what CI checks.
#
# Run xtask last so the committed sqlite3.{c,h} are exactly the CI output.

set -eux

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Step 1: regenerate the Rust bindings.
cd "$REPO_ROOT/sqlanywhere-ffi"
SQLANYWHERE_DEV=1 cargo build
SQLANYWHERE_DEV=1 cargo build --features session
SQLANYWHERE_DEV=1 cargo build --features multiple-ciphers
SQLANYWHERE_DEV=1 cargo build --features session,multiple-ciphers

# Step 2: regenerate the C amalgamation in both bundled locations using the
# exact command CI validates against.
cd "$REPO_ROOT"
cargo xtask build-bundled
