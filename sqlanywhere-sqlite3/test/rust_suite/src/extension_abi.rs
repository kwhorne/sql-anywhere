//! Conformance test for the SQL Anywhere loadable-extension thunk
//! (`sqlanywhere_api_routines`).
//!
//! The thunk is an ABI boundary: a third-party extension is compiled against
//! some copy of `sqlite3ext.h` and then loaded by whatever host library the
//! user happens to have. The host therefore advertises what it implements in
//! `iVersion`, which must stay the first member so that an extension built
//! against any version of the header can read it. This test compiles a real
//! loadable extension out-of-tree and checks the contract from the outside.

#[cfg(all(test, not(target_os = "windows")))]
mod tests {
    use sqlanywhere_sys::rusqlite::{Connection, LoadExtensionGuard};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const EXT_SRC: &str = "src/extension_abi_ext.c";
    const ENTRY_POINT: &str = "sqlite3_sqlanywhereabitest_init";

    /// Directory holding a `sqlite3.h` / `sqlite3ext.h` pair to compile
    /// against. The Makefile targets point at the built sqlite tree; a bare
    /// `cargo test` falls back to it, then to the committed amalgamation.
    fn include_dir() -> PathBuf {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let sqlite_tree = manifest.join("../..");
        let bundled = manifest.join("../../../sqlanywhere-ffi/bundled/src");

        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(dir) = std::env::var("SQLITE3_INCLUDE_DIR") {
            candidates.push(PathBuf::from(dir));
        }
        candidates.push(sqlite_tree);
        candidates.push(bundled);

        for dir in &candidates {
            if dir.join("sqlite3.h").is_file() && dir.join("sqlite3ext.h").is_file() {
                return dir.clone();
            }
        }
        panic!(
            "no directory with both sqlite3.h and sqlite3ext.h among {:?}",
            candidates
        );
    }

    /// The interface version the header we compile against declares. Reading
    /// it here means a version bump does not have to be restated in the test,
    /// while a host library that disagrees with its own header still fails.
    fn header_api_version(include: &Path) -> i32 {
        let header = std::fs::read_to_string(include.join("sqlite3ext.h")).unwrap();
        let line = header
            .lines()
            .find(|l| l.starts_with("#define SQLANYWHERE_API_VERSION"))
            .expect("sqlite3ext.h declares SQLANYWHERE_API_VERSION");
        line.split_whitespace()
            .nth(2)
            .expect("SQLANYWHERE_API_VERSION has a value")
            .parse()
            .expect("SQLANYWHERE_API_VERSION is an integer")
    }

    fn build_extension(include: &Path, out: &Path) {
        let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
        let mut cmd = Command::new(&cc);
        cmd.current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["-O1", "-fPIC", "-shared", "-Wall", "-Werror"])
            .arg("-I")
            .arg(include)
            .arg(EXT_SRC)
            .arg("-o")
            .arg(out);
        // A loadable extension resolves sqlite3_* from the process that loads
        // it. Linkers differ on whether that needs saying out loud.
        if cfg!(target_os = "macos") {
            cmd.args(["-undefined", "dynamic_lookup"]);
        }

        let output = cmd.output().unwrap_or_else(|e| {
            panic!("could not run C compiler {cc:?}: {e}");
        });
        assert!(
            output.status.success(),
            "compiling {EXT_SRC} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn scalar(conn: &Connection, sql: &str) -> i32 {
        conn.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    #[test]
    fn extension_reads_thunk_version() {
        let include = include_dir();
        let expected = header_api_version(&include);
        assert!(expected >= 1, "interface version starts at 1");

        let tmp = tempfile::tempdir().unwrap();
        let ext = tmp.path().join(if cfg!(target_os = "macos") {
            "libsqlanywhereabitest.dylib"
        } else {
            "libsqlanywhereabitest.so"
        });
        build_extension(&include, &ext);

        let conn = Connection::open_in_memory().unwrap();
        unsafe {
            let _guard = LoadExtensionGuard::new(&conn).unwrap();
            conn.load_extension(&ext, Some(ENTRY_POINT)).unwrap();
        }

        // The host handed over a thunk, and it describes this build.
        assert_eq!(
            scalar(&conn, "SELECT sa_abi_version()"),
            expected,
            "host thunk iVersion disagrees with SQLANYWHERE_API_VERSION in \
             sqlite3ext.h; the two are set in the same place and must match"
        );

        // iVersion is readable at all, which only holds while it is the first
        // member of the struct.
        assert_eq!(
            scalar(&conn, &format!("SELECT sa_abi_atleast({expected})")),
            1,
            "host should satisfy its own advertised interface version"
        );

        // The case the version field exists for: an extension compiled against
        // a newer header must decline instead of reading a member this host
        // never wrote.
        assert_eq!(
            scalar(&conn, &format!("SELECT sa_abi_atleast({})", expected + 1)),
            0,
            "host must not claim an interface version it does not implement"
        );
    }

    #[test]
    fn close_hook_reachable_through_thunk() {
        let include = include_dir();
        let tmp = tempfile::tempdir().unwrap();
        let ext = tmp.path().join(if cfg!(target_os = "macos") {
            "libsqlanywhereabitest.dylib"
        } else {
            "libsqlanywhereabitest.so"
        });
        build_extension(&include, &ext);

        let conn = Connection::open_in_memory().unwrap();
        unsafe {
            let _guard = LoadExtensionGuard::new(&conn).unwrap();
            conn.load_extension(&ext, Some(ENTRY_POINT)).unwrap();
        }

        assert_eq!(
            scalar(&conn, "SELECT sa_abi_install_close_hook()"),
            1,
            "close_hook has been in the thunk since interface version 1"
        );
        // The hook itself runs during close, which must still succeed.
        conn.close().unwrap();
    }
}
