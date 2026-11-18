//! One desktop app per user. The claim is an advisory lock on a file, so the operating system releases it when the process ends for any
//! reason (a crash included): there is no stale-lock cleanup to get wrong. The holder's pid is written for people debugging.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::Path;

#[derive(Debug)]
pub enum ClaimError {
    /// Another Playbox Desktop holds it; the pid is the holder's if it could be read.
    Held(Option<u32>),
    Io(io::Error),
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaimError::Held(Some(p)) => write!(f, "Playbox Desktop is already running (pid {p})"),
            ClaimError::Held(None) => write!(f, "Playbox Desktop is already running"),
            ClaimError::Io(e) => write!(f, "could not take the app claim: {e}"),
        }
    }
}

impl std::error::Error for ClaimError {}

/// Held for as long as the value lives.
#[derive(Debug)]
pub struct Claim {
    _file: File,
}

impl Claim {
    pub fn acquire(dir: &Path) -> Result<Claim, ClaimError> {
        std::fs::create_dir_all(dir).map_err(ClaimError::Io)?;
        let path = dir.join("desktop.lock");
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(ClaimError::Io)?;
        match f.try_lock() {
            Ok(()) => {
                let _ = f.set_len(0);
                let _ = write!(f, "{}", std::process::id());
                let _ = f.flush();
                Ok(Claim { _file: f })
            }
            Err(TryLockError::WouldBlock) => {
                let pid = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| s.trim().parse().ok());
                Err(ClaimError::Held(pid))
            }
            Err(TryLockError::Error(e)) => Err(ClaimError::Io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("pbd-claim-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_second_claim_is_refused_until_the_first_is_dropped() {
        let d = tmp("a");
        let first = Claim::acquire(&d).unwrap();
        match Claim::acquire(&d) {
            Err(ClaimError::Held(Some(p))) => assert_eq!(p, std::process::id()),
            other => panic!("expected Held, got {other:?}"),
        }
        drop(first);
        // A child forked by a concurrent test holds a copy of the file until its exec closes it, so release can lag by a moment.
        let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut ok = Claim::acquire(&d).is_ok();
        while !ok && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(20));
            ok = Claim::acquire(&d).is_ok();
        }
        assert!(ok, "released when dropped");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_leftover_lock_file_from_a_crash_does_not_block() {
        let d = tmp("b");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("desktop.lock"), "99999").unwrap(); // a dead process's pid, no lock held
        assert!(Claim::acquire(&d).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }
}
