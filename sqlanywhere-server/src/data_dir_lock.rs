//! One `sqld` per data directory.
//!
//! Two servers started on the same `--db-path` would each append to every
//! namespace's replication log, write its WAL, and back it up to bottomless,
//! with nothing to say the other exists: the log's frame numbers diverge and
//! replicas and backups follow a history neither server wrote. So a server
//! takes an exclusive `flock` on `<db-path>/.lock` before it touches anything
//! else there, and holds it until `Server::start` returns. The lock is the
//! kernel's: it goes away with the process, however that ends, so a crash
//! never leaves a stale lock behind.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use anyhow::Context;
use nix::errno::Errno;
use nix::fcntl::{flock, FlockArg};

const LOCK_FILE: &str = ".lock";

/// Holds the data directory for as long as it lives.
#[derive(Debug)]
pub struct DataDirLock {
    _file: Option<File>,
}

impl DataDirLock {
    /// Locks `db_path`, creating it if needed, or fails naming the process
    /// that holds it. A filesystem without locks (some network mounts) is
    /// warned about and not locked.
    pub fn acquire(db_path: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(db_path)
            .with_context(|| format!("could not create {}", db_path.display()))?;
        let path = db_path.join(LOCK_FILE);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("could not open {}", path.display()))?;
        match flock(file.as_raw_fd(), FlockArg::LockExclusiveNonblock) {
            Ok(()) => {}
            Err(Errno::EWOULDBLOCK) => {
                let mut holder = String::new();
                let _ = file.read_to_string(&mut holder);
                let holder = match holder.trim() {
                    "" => "another process".to_string(),
                    pid => format!("process {pid}"),
                };
                anyhow::bail!(
                    "{} is in use by {holder}: only one sqld can serve a data directory",
                    db_path.display()
                );
            }
            Err(e @ (Errno::ENOLCK | Errno::EOPNOTSUPP)) => {
                tracing::warn!(
                    "could not lock {} ({e}); nothing stops a second sqld from using it",
                    path.display()
                );
                return Ok(Self { _file: None });
            }
            Err(e) => return Err(e).with_context(|| format!("could not lock {}", path.display())),
        }
        // Who holds it, for the error above; only the holder writes it.
        file.set_len(0)?;
        file.rewind()?;
        writeln!(file, "{}", std::process::id())?;
        Ok(Self { _file: Some(file) })
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn a_second_lock_is_refused_until_the_first_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("data.sqld");

        let first = DataDirLock::acquire(&path).unwrap();
        let err = DataDirLock::acquire(&path).unwrap_err().to_string();
        assert!(
            err.contains(&format!("in use by process {}", std::process::id())),
            "{err}"
        );
        assert!(err.contains("data.sqld"), "{err}");

        drop(first);
        let again = DataDirLock::acquire(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(path.join(LOCK_FILE)).unwrap(),
            format!("{}\n", std::process::id())
        );
        drop(again);
    }

    #[test]
    fn a_lock_left_by_a_dead_process_is_taken() {
        let tmp = tempfile::tempdir().unwrap();
        // What a crashed server leaves: the file, with its pid, and no lock.
        std::fs::write(tmp.path().join(LOCK_FILE), "4194304\n").unwrap();
        let _lock = DataDirLock::acquire(tmp.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(LOCK_FILE)).unwrap(),
            format!("{}\n", std::process::id())
        );
    }
}
