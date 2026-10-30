//! Session history on disk: an append-only text file plus a key file.

use crate::attest::{self, PlatformVerifier, Tier};
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
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".playbox")
        })
}

#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn bad(msg: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Load the signing key at `path`, creating it (mode 0600) if missing.
///
/// # Errors
///
/// Returns an I/O error if reading or writing fails or the data is invalid.
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
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(text::hex(&seed).as_bytes())?;
    Ok(SigningKey::from_bytes(&seed))
}

/// # Errors
///
/// Returns an I/O error if reading or writing fails or the data is invalid.
pub fn load(path: &Path) -> io::Result<Log> {
    text::from_text(&fs::read_to_string(path)?).map_err(bad)
}

/// Append one signed entry to the log at `path`, creating it if needed. Takes an exclusive file
/// lock so concurrent hooks cannot interleave. The log must already be a valid chain from this key.
///
/// # Errors
///
/// Returns an I/O error if reading or writing fails or the data is invalid.
pub fn append(
    path: &Path,
    key: &SigningKey,
    kind: &str,
    payload: &[u8],
    time_ms: u64,
) -> io::Result<Entry> {
    if !text::valid_kind(kind) {
        return Err(bad(format!("kind {kind:?} must match [a-z0-9_]+")));
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut f = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)?;
    f.lock()?;
    let mut existing = String::new();
    f.read_to_string(&mut existing)?;
    let mut log = text::from_text(&existing).map_err(bad)?;
    log.verify()
        .map_err(|e| bad(format!("existing log is invalid: {e}")))?;
    let entry = log.append(key, kind, payload, time_ms).clone();
    let line = text::entry_to_line(&entry).map_err(bad)?;
    f.write_all(format!("{line}\n").as_bytes())?;
    f.flush()?;
    Ok(entry)
}

/// Where payload bodies kept for `log` live: `<log>.payloads/<payload hash>`.
#[must_use]
pub fn payload_dir(log: &Path) -> std::path::PathBuf {
    let mut name = log.file_name().unwrap_or_default().to_os_string();
    name.push(".payloads");
    log.with_file_name(name)
}

/// Like [`append`], and also keeps the payload body next to the log so a verifier can read it.
/// Used for entries a verifier needs to inspect (attestations); ordinary entries commit to a hash only.
///
/// # Errors
///
/// Returns an I/O error if reading or writing fails or the data is invalid.
pub fn append_kept(
    path: &Path,
    key: &SigningKey,
    kind: &str,
    payload: &[u8],
    time_ms: u64,
) -> io::Result<Entry> {
    let dir = payload_dir(path);
    fs::create_dir_all(&dir)?;
    let name = text::hex(blake3::hash(payload).as_bytes());
    fs::write(dir.join(&name), payload)?;
    append(path, key, kind, payload, time_ms)
}

/// What a deep check of a history found.
#[derive(Debug, Default)]
pub struct Report {
    pub entries: usize,
    pub head: String,
    /// `None` if the chain verified; otherwise why not. Nothing else is trustworthy then.
    pub chain_error: Option<String>,
    pub attestations: Vec<AttestationReport>,
    /// Problems with attestation entries (missing body, wrong subject, ...).
    pub problems: Vec<String>,
}

#[derive(Debug)]
pub struct AttestationReport {
    pub seq: u64,
    /// `self` for the log owner's own, `host` for one about a room host.
    pub about: &'static str,
    pub subject: String,
    pub claimed: Tier,
    pub achieved: Tier,
    pub notes: Vec<String>,
    /// `<kind>:<fingerprint>` of the device key behind verified platform evidence, if any.
    pub device: Option<String>,
}

impl Report {
    #[must_use]
    pub fn ok(&self) -> bool {
        self.chain_error.is_none() && self.problems.is_empty()
    }
}

/// Verify the chain and every attestation entry in it:
/// - `attestation` entries describe the log's own key, and must be issued at the position they sit
///   at (their `log_head` is the hash of the entry before them);
/// - `host_attestation` entries describe a room host and need only a valid signature.
///
/// Entries that commit to a payload hash but whose bodies are not kept are not inspected.
///
/// # Errors
///
/// Returns an I/O error if reading or writing fails or the data is invalid.
pub fn verify_deep(path: &Path, verifiers: &[&dyn PlatformVerifier]) -> io::Result<Report> {
    let log = load(path)?;
    let mut r = Report {
        entries: log.entries.len(),
        head: text::head_hex(&log),
        ..Default::default()
    };
    if let Err(e) = log.verify() {
        r.chain_error = Some(e.to_string());
        return Ok(r);
    }
    let dir = payload_dir(path);
    for e in &log.entries {
        let about = match e.kind.as_str() {
            "attestation" => "self",
            "host_attestation" => "host",
            _ => continue,
        };
        let name = text::hex(&e.payload_hash);
        let body = if let Ok(b) = fs::read(dir.join(&name)) {
            b
        } else {
            r.problems.push(format!(
                "entry {}: {} body {name} is missing",
                e.seq, e.kind
            ));
            continue;
        };
        if *blake3::hash(&body).as_bytes() != e.payload_hash {
            r.problems.push(format!(
                "entry {}: kept body does not match its hash",
                e.seq
            ));
            continue;
        }
        let v = match std::str::from_utf8(&body)
            .map_err(|e| attest::AttestError::Malformed(e.to_string()))
            .and_then(|t| attest::verify(t, verifiers))
        {
            Ok(v) => v,
            Err(err) => {
                r.problems.push(format!("entry {}: {err}", e.seq));
                continue;
            }
        };
        if about == "self" {
            if v.statement.subject != e.signer {
                r.problems.push(format!(
                    "entry {}: attestation is about a different key than the log's",
                    e.seq
                ));
                continue;
            }
            if v.statement.log_head != text::hex(&e.prev) {
                r.problems.push(format!(
                    "entry {}: attestation was issued for a different point in the history",
                    e.seq
                ));
                continue;
            }
        }
        r.attestations.push(AttestationReport {
            seq: e.seq,
            about,
            subject: text::hex(&v.statement.subject),
            claimed: v.statement.claimed,
            achieved: v.achieved,
            notes: v.notes,
            device: v.device,
        });
    }
    Ok(r)
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

    #[test]
    fn deep_verify_checks_attestations_and_their_position() {
        let dir = tmp("deep");
        let key = load_or_create_key(&dir.join("k")).unwrap();
        let log = dir.join("h.log");
        append(&log, &key, "session_start", b"x", 1).unwrap();
        let head = text::hex(&load(&log).unwrap().head());
        let st = attest::new_statement(&key, "", &head, 2, attest::measure_self("none", "t"))
            .sign(&key)
            .unwrap();
        append_kept(&log, &key, "attestation", st.as_bytes(), 2).unwrap();
        append(&log, &key, "tool_call", b"y", 3).unwrap();

        let r = verify_deep(&log, &[]).unwrap();
        assert!(r.ok(), "{r:?}");
        assert_eq!(r.attestations.len(), 1);
        assert_eq!(r.attestations[0].achieved, Tier::T0);

        // An attestation replayed at a later position is caught.
        append_kept(&log, &key, "attestation", st.as_bytes(), 4).unwrap();
        let r = verify_deep(&log, &[]).unwrap();
        assert!(
            r.problems.iter().any(|p| p.contains("different point")),
            "{r:?}"
        );

        // A missing body is a problem, not a silent pass.
        fs::remove_dir_all(payload_dir(&log)).unwrap();
        assert!(verify_deep(&log, &[])
            .unwrap()
            .problems
            .iter()
            .any(|p| p.contains("missing")));
    }

    #[test]
    fn deep_verify_reports_a_broken_chain_first() {
        let dir = tmp("deepbad");
        let key = load_or_create_key(&dir.join("k")).unwrap();
        let log = dir.join("h.log");
        append(&log, &key, "a", b"1", 1).unwrap();
        append(&log, &key, "b", b"2", 2).unwrap();
        fs::write(
            &log,
            fs::read_to_string(&log).unwrap().replacen(" a ", " c ", 1),
        )
        .unwrap();
        let r = verify_deep(&log, &[]).unwrap();
        assert!(r.chain_error.is_some() && !r.ok());
    }
}
