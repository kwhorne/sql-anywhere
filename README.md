<!-- markdownlint-disable MD033 MD041 -->

<h1 align="center">SQL Anywhere</h1>

<p align="center">
  <strong>An embeddable, replication-ready SQL engine built on SQLite — maintained by <a href="https://elyracode.com/sqlanywhere">Elyra</a>.</strong>
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="#whats-in-the-box">What's in the box</a> ·
  <a href="#repository-layout">Repository layout</a> ·
  <a href="#building-from-source">Building</a> ·
  <a href="https://elyracode.com/docs/sqlanywhere">Docs</a> ·
  <a href="CHANGELOG.md">Changelog</a> ·
  <a href="LICENSE.md"><img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT" /></a>
</p>

---

## What is SQL Anywhere?

SQL Anywhere is a fork of SQLite that keeps full SQLite file-format and API
compatibility while adding the pieces you need to run SQLite *beyond* a single
local file:

- **Embedded replicas** — keep a live, local copy of a remote database inside
  your process for sub-millisecond reads.
- **A server mode** (`sqld`) — expose SQLite over HTTP and WebSockets so remote
  clients can talk to it like they would PostgreSQL or MySQL.
- **Bottomless storage** — continuously stream the write-ahead log to object
  storage (S3-compatible) for durable, point-in-time-recoverable databases.
- **At-rest & in-transit encryption**, **offline writes**, and a **pluggable
  virtual WAL** interface.

If you just want an embedded database in a Rust application, reach for the
[`sqlanywhere`](sqlanywhere) crate. If you want a network-accessible database,
run the [`sqld`](sqlanywhere-server) server.

> SQL Anywhere stays a *drop-in* SQLite: databases that don't use the extra
> features remain byte-compatible with stock SQLite tooling.

## Quick start

Add the crate and open an in-memory (or on-disk) database:

```rust
use sqlanywhere::Builder;

#[tokio::main]
async fn main() {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();

    conn.execute("CREATE TABLE users (email TEXT)", ()).await.unwrap();
    conn.execute("INSERT INTO users (email) VALUES ('alice@example.org')", ())
        .await
        .unwrap();

    let mut rows = conn.query("SELECT email FROM users", ()).await.unwrap();
    while let Some(row) = rows.next().await.unwrap() {
        println!("{}", row.get_str(0).unwrap());
    }
}
```

Point the same API at a remote primary to get an **embedded replica** that syncs
in the background:

```rust
let db = Builder::new_local_replica("/tmp/local.db").build().await.unwrap();
db.sync().await.unwrap();        // pull the latest frames
let conn = db.connect().unwrap(); // reads are served locally
```

More end-to-end examples (encryption, offline writes, local/remote sync,
Flutter, serialization) live in [`sqlanywhere/examples`](sqlanywhere/examples).

## What's in the box

| Capability | Where it lives | Notes |
|------------|----------------|-------|
| Rust embedded API | [`sqlanywhere`](sqlanywhere) | Async, batteries-included wrapper over the SQLite C API |
| Server (`sqld`) | [`sqlanywhere-server`](sqlanywhere-server) | HTTP + WebSocket access, namespaces, admin API |
| Remote protocol (Hrana) | [`sqlanywhere-hrana`](sqlanywhere-hrana) | Wire protocol — see [docs/HRANA_3_SPEC.md](docs/HRANA_3_SPEC.md) |
| Replication primitives | [`sqlanywhere-replication`](sqlanywhere-replication) | Frame injection / log shipping |
| WAL → object storage | [`bottomless`](bottomless), [`bottomless-cli`](bottomless-cli) | S3-compatible continuous backup & restore |
| SQLite C library (fork) | [`sqlanywhere-sqlite3`](sqlanywhere-sqlite3) | Amalgamation + extensions ([docs](sqlanywhere-sqlite3/doc/sqlanywhere_extensions.md)) |
| Low-level FFI / sys | [`sqlanywhere-ffi`](sqlanywhere-ffi), [`sqlanywhere-sys`](sqlanywhere-sys) | Bundled amalgamation & bindings |
| Other-language bindings | [`bindings/c`](bindings/c), [`bindings/wasm`](bindings/wasm) | C and WebAssembly |

## Extensions over stock SQLite

SQL Anywhere ships several additions on top of the core engine — the full
reference is in
[`sqlanywhere_extensions.md`](sqlanywhere-sqlite3/doc/sqlanywhere_extensions.md):

- `ALTER TABLE` support for changing column types and constraints
- Randomized `ROWID`
- WebAssembly user-defined functions
- A virtual write-ahead log interface (pluggable WAL backends)
- `xPreparedSql` — pass the original SQL string down to virtual tables

## Repository layout

```
sqlanywhere/            Rust embedded database API (start here)
sqlanywhere-server/     sqld — the standalone server
sqlanywhere-hrana/      Hrana remote wire protocol
sqlanywhere-replication/ replication / frame injection
sqlanywhere-sys/        safe-ish Rust bindings to the C engine
sqlanywhere-ffi/        bundled SQLite amalgamation + bindgen
sqlanywhere-sqlite3/    the SQLite C fork (amalgamation, tests, extensions)
bottomless/             S3-backed WAL replication library
bottomless-cli/         CLI for inspecting/restoring bottomless backups
bindings/               C and WASM language bindings
vendored/               in-tree rusqlite + SQL parser
docs/                   protocol specs, design notes, admin & user guides
xtask/                  build automation (cargo xtask ...)
```

## Building from source

### Prerequisites

| Tool | Needed for |
|------|------------|
| **Rust** (pinned in [`rust-toolchain.toml`](rust-toolchain.toml), currently **1.85.0**) | everything — `rustup` installs it automatically |
| **C compiler** (`cc`/`clang`) | the bundled SQLite amalgamation |
| **`libclang`** | `bindgen` (FFI bindings in `sqlanywhere-sys`) |
| **`protoc`** (Protocol Buffers compiler) | `sqlanywhere-server`'s gRPC interfaces |
| **`cmake`** | only the `encryption` feature (builds SQLite3MultipleCiphers) |

Install the system packages:

```sh
# Debian/Ubuntu
sudo apt-get install -y build-essential cmake libclang-dev protobuf-compiler

# macOS (Homebrew)
brew install cmake llvm protobuf
```

### Build

```sh
# Build the Rust workspace (no cmake required)
cargo build --release

# Build with at-rest encryption support (requires cmake)
cargo build --release -p sqlanywhere --features encryption

# Build the SQLite-compatible C library and CLI tools
cargo xtask build

# Launch the interactive shell
cd sqlanywhere-sqlite3 && ./sqlanywhere
```

```console
SQL Anywhere version 0.2.0 (based on SQLite version 3.43.0)
Enter ".help" for usage hints.
Connected to a transient in-memory database.
sqlanywhere>
```

Running the server and Docker images are covered in
[docs/BUILD-RUN.md](docs/BUILD-RUN.md) and [docs/DOCKER.md](docs/DOCKER.md).

## Documentation

Full documentation lives at **<https://elyracode.com/docs/sqlanywhere>**. The
in-repo specs and guides below are the source references:

- [User guide](docs/USER_GUIDE.md) · [Design overview](docs/DESIGN.md) · [Consistency model](docs/CONSISTENCY_MODEL.md)
- Protocol specs: [Hrana 3](docs/HRANA_3_SPEC.md), [HTTP v2](docs/HTTP_V2_SPEC.md), [Admin API](docs/ADMIN_API.md)
- C engine extensions: [sqlanywhere_extensions.md](sqlanywhere-sqlite3/doc/sqlanywhere_extensions.md)

## Compatibility promise

- **File format** — SQL Anywhere reads and writes the standard SQLite file
  format. Features that change the file (e.g. encryption) are always opt-in;
  without them you get ordinary SQLite files.
- **API** — 100% of the SQLite C API keeps working; we only *add* APIs.
- **Embeddable** — it always runs in-process, no network required.

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for the release history. The current release is
**0.2.0**.

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) and our
[Code of Conduct](CODE_OF_CONDUCT.md) before opening a pull request.

## License

SQL Anywhere is released under the [MIT License](LICENSE.md). It builds on
SQLite and other open-source projects; see the respective source trees for their
licenses.
