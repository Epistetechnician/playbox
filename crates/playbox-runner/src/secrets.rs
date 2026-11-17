//! Credentials for a task that never enter its workspace.
//!
//! The host owner keeps secrets as files in a directory (the file name is the secret's name). A task asks for some by name in
//! `TaskSpec::secrets`; the peer never sees a value and cannot name a path. Before the task starts the values are copied into a
//! private staging directory, which the sandbox shows at `/run/secrets` (read-only), outside `/workspace` so a returned patch cannot
//! contain them. The staging directory is removed when the task ends. What the audit log gets is the names and a hash of each value,
//! never the value, so a later reader can tell which credentials a task could use and that they were the ones the owner had then.
//!
//! Limits follow Peerspan's protocol: 16 secrets per task, 64 KiB each.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const MAX_PER_TASK: usize = 16;
pub const MAX_BYTES: u64 = 64 * 1024;

/// What the audit log records about one secret a task could read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretUse {
    pub name: String,
    /// blake3 of the value, hex.
    pub blake3: String,
}

/// A name is a plain file name: letters, digits, `.`, `_`, `-`, not starting with a dot.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The host's secrets directory.
#[derive(Debug, Clone)]
pub struct SecretStore {
    dir: PathBuf,
}

/// Secrets copied for one task. Removed when dropped.
#[derive(Debug)]
pub struct Staged {
    dir: PathBuf,
    pub used: Vec<SecretUse>,
}

impl Staged {
    /// The directory to show at `/run/secrets`.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl SecretStore {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// `$PLAYBOX_SECRETS_DIR`, else `$PLAYBOX_HOME/secrets`, else `~/.playbox/secrets`.
    #[must_use]
    pub fn from_env() -> Self {
        let dir = std::env::var_os("PLAYBOX_SECRETS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("PLAYBOX_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".playbox")
                    })
                    .join("secrets")
            });
        Self { dir }
    }

    /// Check that `name` is valid and points to a regular file in the store within [`MAX_BYTES`].
    ///
    /// # Errors
    ///
    /// Fails for a bad name, or a missing, non-regular or oversized secret.
    pub fn check(&self, name: &str) -> Result<()> {
        if !valid_name(name) {
            bail!("bad secret name {name:?}");
        }
        let meta = std::fs::symlink_metadata(self.dir.join(name))
            .with_context(|| format!("secret {name:?} is not in the host's store"))?;
        if !meta.is_file() {
            bail!("secret {name:?} is not a regular file");
        }
        if meta.len() > MAX_BYTES {
            bail!("secret {name:?} is larger than {MAX_BYTES} bytes");
        }
        Ok(())
    }

    /// Whether `name` exists and passes [`Self::check`], for refusing a task before it is queued.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.check(name).is_ok()
    }

    /// Check that the secrets directory exists and is not readable by other users.
    ///
    /// # Errors
    ///
    /// Fails if the directory cannot be read or is accessible to group or others (`mode & 0o077 != 0`).
    pub fn check_dir(&self) -> Result<()> {
        let mode = std::fs::metadata(&self.dir)
            .with_context(|| format!("secrets directory {}", self.dir.display()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            bail!(
                "the secrets directory {} is readable by others (mode {:o}); chmod 700 it",
                self.dir.display(),
                mode & 0o777
            );
        }
        Ok(())
    }

    /// Copy the named secrets into a fresh private directory under `scratch`.
    ///
    /// # Errors
    ///
    /// Fails for a bad or repeated name, too many names, a missing, non-regular or oversized secret, or a store directory that
    /// other users can read (a secret a stranger can read is not one).
    pub fn stage(&self, names: &[String], scratch: &Path) -> Result<Staged> {
        if names.len() > MAX_PER_TASK {
            bail!("a task may use at most {MAX_PER_TASK} secrets");
        }
        let mut seen = std::collections::BTreeSet::new();
        for n in names {
            if !valid_name(n) {
                bail!("bad secret name {n:?}");
            }
            if !seen.insert(n) {
                bail!("secret {n:?} listed twice");
            }
        }
        if !names.is_empty() {
            self.check_dir()?;
        }
        let mut rnd = [0u8; 8];
        std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut rnd)?;
        let dir = scratch.join(format!(
            "secrets-{}",
            rnd.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ));
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        let staged = Staged {
            dir,
            used: Vec::new(),
        };
        let mut staged = staged;
        for n in names {
            self.check(n)?;
            let value = std::fs::read(self.dir.join(n))?;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o444)
                .open(staged.dir.join(n))?;
            std::io::Write::write_all(&mut f, &value)?;
            staged.used.push(SecretUse {
                name: n.clone(),
                blake3: blake3::hash(&value).to_hex().to_string(),
            });
        }
        Ok(staged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (PathBuf, SecretStore) {
        let d = std::env::temp_dir().join(format!(
            "playbox-secrets-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::DirBuilder::new().mode(0o700).create(&d).unwrap();
        (d.clone(), SecretStore::new(d))
    }

    #[test]
    fn a_staged_secret_is_copied_hashed_and_removed() {
        let (d, s) = store();
        std::fs::write(d.join("API_KEY"), b"hunter2").unwrap();
        let scratch = d.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let path;
        {
            let st = s.stage(&["API_KEY".into()], &scratch).unwrap();
            path = st.path().to_path_buf();
            assert_eq!(std::fs::read(path.join("API_KEY")).unwrap(), b"hunter2");
            assert_eq!(
                st.used[0].blake3,
                blake3::hash(b"hunter2").to_hex().to_string()
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert!(!path.exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn bad_names_and_limits_are_refused() {
        let (d, s) = store();
        let scratch = d.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        for bad in ["", ".hidden", "../x", "a/b", "a b", "x\0y", &"n".repeat(65)] {
            assert!(s.stage(&[bad.into()], &scratch).is_err(), "{bad:?}");
        }
        assert!(s.stage(&["missing".into()], &scratch).is_err());
        assert!(s.stage(&["a".into(), "a".into()], &scratch).is_err());
        let many: Vec<String> = (0..=MAX_PER_TASK).map(|i| format!("s{i}")).collect();
        assert!(s.stage(&many, &scratch).is_err());
        std::fs::write(d.join("big"), vec![0u8; MAX_BYTES as usize + 1]).unwrap();
        assert!(s.stage(&["big".into()], &scratch).is_err());
        std::os::unix::fs::symlink("/etc/passwd", d.join("link")).unwrap();
        assert!(s.stage(&["link".into()], &scratch).is_err());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_store_others_can_read_is_refused() {
        let (d, s) = store();
        std::fs::write(d.join("k"), b"v").unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        let scratch = std::env::temp_dir();
        assert!(s.stage(&["k".into()], &scratch).is_err());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn check_dir_requires_a_private_directory() {
        let (d, s) = store();
        assert!(s.check_dir().is_ok());
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(s.check_dir().is_err());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn has_reports_missing_and_invalid_names() {
        let (d, s) = store();
        std::fs::write(d.join("API_KEY"), b"v").unwrap();
        assert!(s.has("API_KEY"));
        assert!(!s.has("MISSING"));
        assert!(!s.has(".hidden"));
        std::os::unix::fs::symlink("/etc/passwd", d.join("LINK")).unwrap();
        assert!(!s.has("LINK"));
        assert!(s.check("LINK").is_err());
        std::fs::write(d.join("BIG"), vec![0u8; MAX_BYTES as usize + 1]).unwrap();
        assert!(!s.has("BIG"));
        assert!(s.check("BIG").is_err());
        std::fs::remove_dir_all(d).unwrap();
    }
}
