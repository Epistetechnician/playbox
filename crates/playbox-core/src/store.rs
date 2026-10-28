//! Session history on disk: an append-only text file plus a key file.

use crate::history::{text, Entry, Log};
use ed25519_dalek::SigningKey;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

/// `$PLAYBOX_HOME`, or `~/.playbox`.
pub fn home() -> std::path::PathBuf {
    std::env::var_os("PLAYBOX_HOME")
        .map(Into::into)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".playbox"))
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Load the signing key at `path`, creating it (mode 0600) if missing.
pub fn load_or_create_key(path: &Path) -> io::Result<SigningKey> {
    if let Ok(s) = fs::read_to_string(path) {
        let b = s.trim();
        let mut seed = [0u8; 32];
        if b.len() != 64 || !b.is_ascii() {
            return Err(bad("key file must hold 64 hex chars"));
        }
        for (i, o) in seed.iter_mut().enumerate() {
            *o = u8::from_str_radix(&b[i * 2..i * 2 + 2], 16).map_err(|e| bad(e.to_string()))?;
        }
        return Ok(SigningKey::from_bytes(&seed));
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut seed = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut seed)?;
    let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(text::hex(&seed).as_bytes())?;
    Ok(SigningKey::from_bytes(&seed))
}

pub fn load(path: &Path) -> io::Result<Log> {
    text::from_text(&fs::read_to_string(path)?).map_err(bad)
}

/// Append one signed entry to the log at `path`, creating it if needed. Takes an exclusive file
/// lock so concurrent hooks cannot interleave. The log must already be a valid chain from this key.
pub fn append(path: &Path, key: &SigningKey, kind: &str, payload: &[u8], time_ms: u64) -> io::Result<Entry> {
    if !text::valid_kind(kind) {
        return Err(bad(format!("kind {kind:?} must match [a-z0-9_]+")));
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut f = OpenOptions::new().read(true).append(true).create(true).mode(0o600).open(path)?;
    f.lock()?;
    let mut existing = String::new();
    f.read_to_string(&mut existing)?;
    let mut log = text::from_text(&existing).map_err(bad)?;
    log.verify().map_err(|e| bad(format!("existing log is invalid: {e}")))?;
    let entry = log.append(key, kind, payload, time_ms).clone();
    let line = text::entry_to_line(&entry).map_err(bad)?;
    f.write_all(format!("{line}\n").as_bytes())?;
    f.flush()?;
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("playbox-core-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn key_is_created_once_and_reloaded() {
        let p = tmp("key").join("k");
        let a = load_or_create_key(&p).unwrap();
        let b = load_or_create_key(&p).unwrap();
        assert_eq!(a.to_bytes(), b.to_bytes());
    }

    #[test]
    fn appended_entries_form_a_valid_chain_on_disk() {
        let dir = tmp("log");
        let key = load_or_create_key(&dir.join("k")).unwrap();
        let log = dir.join("h.log");
        for (i, k) in ["session_start", "tool_call", "commit"].iter().enumerate() {
            append(&log, &key, k, k.as_bytes(), i as u64).unwrap();
        }
        let l = load(&log).unwrap();
        assert_eq!(l.entries.len(), 3);
        assert_eq!(l.verify(), Ok(()));
    }

    #[test]
    fn editing_the_file_is_detected_and_blocks_appends() {
        let dir = tmp("tamper");
        let key = load_or_create_key(&dir.join("k")).unwrap();
        let log = dir.join("h.log");
        append(&log, &key, "a", b"1", 1).unwrap();
        append(&log, &key, "b", b"2", 2).unwrap();
        let s = fs::read_to_string(&log).unwrap().replacen(" a ", " c ", 1);
        fs::write(&log, s).unwrap();
        assert!(load(&log).unwrap().verify().is_err());
        assert!(append(&log, &key, "d", b"3", 3).is_err());
    }
}
