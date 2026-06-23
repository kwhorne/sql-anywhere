# Build and run sqld

There are three ways to build and run sqld:

- [Download a prebuilt binary](#download-a-prebuilt-binary)
- [Using Homebrew](#build-and-install-with-homebrew)
- [From source using Rust](#build-from-source-using-rust)

## Running sqld

You can simply run launch the executable with no command line arguments to run
an instance of sqld. By default, sqld listens on 127.0.0.1 port 8080 and
persists database data in a directory `./data.sqld`.

Use the `--help` flag to discover how to change its runtime behavior.

## Query sqld

You can query sqld using one of the provided [client
libraries](../sqlanywhere-server#client-libraries).

You can also use the [elyra cli](https://elyracode.com/docs/sqlanywhere/reference/elyra-cli) to connect to the sqld instance:

```console
elyra db shell http://127.0.0.1:8080
```

## Download a prebuilt binary

The [release page](https://github.com/kwhorne/sql-anywhere/releases) for this
repository lists released versions of sqld along with prebuilt downloads. Each
release attaches a `sqld` binary for:

| Platform | Architecture | Asset |
|----------|--------------|-------|
| macOS | Apple Silicon (arm64) | `sqld-<tag>-aarch64-apple-darwin.tar.gz` |
| Ubuntu / Linux | Intel (x86_64) | `sqld-<tag>-x86_64-unknown-linux-gnu.tar.gz` |
| Ubuntu / Linux | ARM (aarch64) | `sqld-<tag>-aarch64-unknown-linux-gnu.tar.gz` |

Download the archive for your platform, extract it, and run the `sqld`
executable.

> Windows is not currently supported as a prebuilt target — sqld's
> replication layer depends on Unix-only file APIs. Build from source with
> Rust if you need to run on other platforms.

## Build and install with Homebrew

The sqld formulae for Homebrew works with macOS, Linux (including WSL).

### 1. Add the tap `kwhorne/sqld` to Homebrew

```bash
brew tap kwhorne/sqld
```

### 2. Install the formulae `sqld`

```bash
brew install sqld
```

This builds and installs the binary `sqld` into `$HOMEBREW_PREFIX/bin/sqld`,
which should be in your PATH.

### 3. Verify that `sqld` works

```bash
sqld --help
```

## Build from source using Rust

To build from source, you must have a Rust development environment installed and
available in your PATH.

Currently we only support building sqld on macOS and Linux (including WSL).
Native Windows is not supported because sqld's replication layer relies on
Unix-only file APIs; use WSL on Windows.

### 1. Clone this repo

Clone this repo using your preferred mechanism. You may want to use one of the
[sqld release tags].

Change to the `sqlanywhere-server` directory.

### 2. Build with cargo

```bash
cargo build
```

The sqld binary will be in `./target/debug/sqld`.

### 3. Verify the build

Check that sqld built successfully using its --help flag:

```bash
./target/debug/sqld --help
```

### 4. Run sqld with all defaults

The following starts sqld, taking the following defaults:

- Local files stored in the directory `./data.sqld`
- Client HTTP requests on 127.0.0.1:8080

```bash
./target/debug/sqld
```

8080 is the default port for the sqld HTTP service that handles client queries.
With this container running, you can use the URL `http://127.0.0.1:8080` or
`ws://127.0.0.1:8080` to configure one of the SQL Anywhere client SDKs for local
development.

### 5. Run tests (optional)

```console
cargo xtask test
```

[sqld release tags]: https://github.com/kwhorne/sql-anywhere/releases
