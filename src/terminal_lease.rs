use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

use fs2::FileExt;
use sha2::{Digest, Sha256};
use tmux_agent_workbench::paths::Paths;

/// Held across both SSH connections. Different terminal devices get independent
/// leases; reconnecting from the same terminal retains its server-side identity.
pub(crate) struct TerminalLease {
    pub(crate) id: String,
    _lock: File,
}

fn identity(tty: &[u8]) -> String {
    let hash = Sha256::digest(tty);
    uuid::Uuid::from_bytes(hash[..16].try_into().unwrap()).to_string()
}

impl TerminalLease {
    pub(crate) fn acquire(paths: &Paths) -> Result<Self, Box<dyn std::error::Error>> {
        let mut terminal = None;
        for fd in [0, 1, 2] {
            let mut name = [0u8; 1024];
            if unsafe { libc::ttyname_r(fd, name.as_mut_ptr().cast(), name.len()) } == 0 {
                let length = name.iter().position(|b| *b == 0).unwrap_or(name.len());
                if &name[..length] != b"/dev/tty" {
                    let session = unsafe { libc::tcgetsid(fd) };
                    terminal = Some(identity(
                        format!("{}:{session}", String::from_utf8_lossy(&name[..length]))
                            .as_bytes(),
                    ));
                    break;
                }
            }
        }
        let id = terminal
            .or_else(|| {
                let tty = File::open("/dev/tty").ok()?;
                let session = unsafe { libc::tcgetsid(tty.as_raw_fd()) };
                (session >= 0).then(|| identity(format!("tty-session:{session}").as_bytes()))
            })
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        Self::acquire_id(paths, id)
    }

    fn acquire_id(paths: &Paths, id: String) -> Result<Self, Box<dyn std::error::Error>> {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(paths.runtime_dir.join(format!("terminal-{id}.lock")))?;
        lock.try_lock_exclusive().map_err(|error| {
            format!(
                "cannot own this terminal (another workbench connection may be active): {error}"
            )
        })?;
        // Keep the inode after close: unlinking allows concurrent owners to lock
        // different files at the same pathname.
        Ok(Self { id, _lock: lock })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separate_terminals_coexist_and_same_terminal_reconnects_after_release() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_dir: dir.path().into(),
            state_dir: dir.path().into(),
            cache_dir: dir.path().into(),
            runtime_dir: dir.path().into(),
        };
        let a = identity(b"/dev/pts/1");
        let b = identity(b"/dev/pts/2");
        assert_ne!(a, b);
        let first = TerminalLease::acquire_id(&paths, a.clone()).unwrap();
        let _second = TerminalLease::acquire_id(&paths, b).unwrap();
        assert!(TerminalLease::acquire_id(&paths, a.clone()).is_err());
        drop(first);
        assert!(TerminalLease::acquire_id(&paths, a).is_ok());
    }
}
