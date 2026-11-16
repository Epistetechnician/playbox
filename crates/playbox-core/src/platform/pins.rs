//! The pin store: trust-on-first-use pins for platform attestation keys.

use std::{
    collections::BTreeSet,
    fs,
    io::{self},
    path::{Path, PathBuf},
};

/// The set of device keys an operator trusts, as `<kind> <fingerprint>` lines in a file.
#[derive(Debug, Clone, Default)]
pub struct PinStore {
    path: Option<PathBuf>,
    pins: BTreeSet<(String, String)>,
}

impl PinStore {
    #[must_use]
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Load `path`; a missing file is an empty store. Saves go back to the same path.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if reading or writing fails or the data is invalid.
    pub fn load(path: &Path) -> io::Result<Self> {
        let mut s = PinStore {
            path: Some(path.to_path_buf()),
            pins: BTreeSet::new(),
        };
        match fs::read_to_string(path) {
            Ok(t) => {
                for line in t
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                {
                    let (k, f) = line.split_once(' ').ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, format!("bad pin line {line:?}"))
                    })?;
                    s.pins.insert((k.to_string(), f.trim().to_string()));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(s)
    }

    #[must_use]
    pub fn contains(&self, kind: &str, fingerprint: &str) -> bool {
        self.pins
            .contains(&(kind.to_string(), fingerprint.to_string()))
    }

    #[must_use]
    pub fn list(&self) -> Vec<(String, String)> {
        self.pins.iter().cloned().collect()
    }

    /// # Errors
    ///
    /// Returns an I/O error if reading or writing fails or the data is invalid.
    pub fn add(&mut self, kind: &str, fingerprint: &str) -> io::Result<()> {
        if kind.is_empty()
            || fingerprint.is_empty()
            || kind.contains(char::is_whitespace)
            || fingerprint.contains(char::is_whitespace)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "kind and fingerprint must be non-empty words",
            ));
        }
        self.pins
            .insert((kind.to_string(), fingerprint.to_string()));
        self.save()
    }

    /// # Errors
    ///
    /// Returns an I/O error if reading or writing fails or the data is invalid.
    pub fn remove(&mut self, kind: &str, fingerprint: &str) -> io::Result<bool> {
        let had = self
            .pins
            .remove(&(kind.to_string(), fingerprint.to_string()));
        self.save()?;
        Ok(had)
    }

    fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let body: String = self
            .pins
            .iter()
            .map(|(k, f)| format!("{k} {f}\n"))
            .collect();
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, body)?;
        fs::rename(&tmp, path)
    }
}

// ----------------------------------------------------------------------------------------------
// Secure Enclave (macOS)
// ----------------------------------------------------------------------------------------------
