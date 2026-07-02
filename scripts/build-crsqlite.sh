#!/usr/bin/env bash
#
# Build the cr-sqlite (CRDT) loadable extension for SQL Anywhere.
#
# cr-sqlite turns ordinary tables into conflict-free replicated relations (CRRs)
# so multiple databases can be edited offline and merged deterministically. It
# ships as a loadable SQLite extension built from the vendored sources in
# sqlanywhere-sqlite3/ext/crr, which require a pinned nightly toolchain plus
# build-std (the extension is no_std).
#
# Usage:
#   scripts/build-crsqlite.sh          # build for the host target
#
# Output:
#   sqlanywhere-sqlite3/ext/crr/dist/crsqlite.{dylib,so}
#
set -euo pipefail

TOOLCHAIN="nightly-2023-10-05"
CRR_DIR="$(cd "$(dirname "$0")/../sqlanywhere-sqlite3/ext/crr" && pwd)"

# Host target triple (build-std requires an explicit --target).
TARGET="$(rustc -vV | awk '/host:/ {print $2}')"

echo ">> Ensuring toolchain $TOOLCHAIN with rust-src is installed"
if ! rustup toolchain list | grep -q "$TOOLCHAIN"; then
  rustup toolchain install "$TOOLCHAIN" --component rust-src --profile minimal
else
  rustup component add rust-src --toolchain "$TOOLCHAIN" >/dev/null 2>&1 || true
fi

echo ">> Building cr-sqlite loadable extension for $TARGET"
cd "$CRR_DIR"
# The Makefile passes --target=<triple> to the C compiler for cross builds, so
# the compiler must understand that flag. gcc doesn't; clang does. macOS's `gcc`
# is already clang; on Linux force clang via CI_GCC.
CC_OVERRIDE=""
[ "$(uname -s)" = "Linux" ] && CC_OVERRIDE="clang"
RUSTUP_TOOLCHAIN="$TOOLCHAIN" CI_MAYBE_TARGET="$TARGET" ${CC_OVERRIDE:+CI_GCC=$CC_OVERRIDE} make loadable

ext="dylib"
[ "$(uname -s)" = "Linux" ] && ext="so"
echo ">> Done: $CRR_DIR/dist/crsqlite.$ext"
