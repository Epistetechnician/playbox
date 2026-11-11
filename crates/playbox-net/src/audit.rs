//! The host's audit log: what happened in the room, in order, tamper-evident.
//!
//! One line per event: `<json>\t<signature>`. Each JSON body holds a sequence number, the time, the
//! blake3 hash of the *previous* body, the event and the peer, and event details. The signature is the host's
//! key over the body's hash. So:
//!
//! - editing or deleting a line breaks the chain;
//! - appending forged lines needs the host's key;
//! - truncating the tail is **not** detectable from the file alone (keep the last `head` somewhere else if
//!   that matters to you).
//!
//! What goes in: who joined or was refused, every denial, pushes, pulls, tasks (runner, resources, exit code,
//! duration, the program name and a hash of the full command), revocations, refused connections.
//! What stays out: grants, invites (only an 8-character prefix of the invite's id), and command arguments,
//! which often contain secrets. Anything the operator needs those for can be re-derived from the hash only
//! by someone who already has the command.

use anyhow::{anyhow, bail, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde_json::{json, Value};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
};

const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const MAX_FIELD: usize = 240;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Shorten and flatten a free-text field so one event cannot flood or break the log.
#[must_use]
pub fn clip(s: &str) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if flat.chars().count() <= MAX_FIELD {
        flat
    } else {
        format!("{}…", flat.chars().take(MAX_FIELD).collect::<String>())
    }
}

enum Sink {
    Off,
    Stderr,
    File(File),
}

struct State {
    sink: Sink,
    seq: u64,
    prev: String,
}

pub struct Audit {
    key: SigningKey,
    state: Mutex<State>,
    path: Option<PathBuf>,
    /// The room's live feed; every audit event is mirrored into it as a short summary.
    pub chat: crate::chat::Chat,
}

impl Audit {
    /// No log at all.
    #[must_use]
    pub fn off(key: SigningKey) -> Self {
        Self {
            key,
            state: Mutex::new(State {
                sink: Sink::Off,
                seq: 0,
                prev: ZERO.into(),
            }),
            path: None,
            chat: Default::default(),
        }
    }

    /// Lines go to standard error (for hosts that keep no state directory).
    #[must_use]
    pub fn stderr(key: SigningKey) -> Self {
        Self {
            key,
            state: Mutex::new(State {
                sink: Sink::Stderr,
                seq: 0,
                prev: ZERO.into(),
            }),
            path: None,
            chat: Default::default(),
        }
    }

    /// Append to `path`, creating it (mode 0600) if needed and continuing its chain if it exists.
    /// An existing file that is not a valid chain from this host is left alone and a new file is not
    /// started silently: this fails, so a corrupted or foreign log is noticed rather than extended.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn open(path: &Path, key: SigningKey) -> Result<Self> {
        let (seq, prev) = if path.exists() && std::fs::metadata(path)?.len() > 0 {
            let s =
                verify_with(path, Some(&key.verifying_key().to_bytes())).with_context(|| {
                    format!("{} is not a valid audit log for this host", path.display())
                })?;
            (s.entries, s.head)
        } else {
            (0, ZERO.to_string())
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        Ok(Self {
            key,
            state: Mutex::new(State {
                sink: Sink::File(file),
                seq,
                prev,
            }),
            path: Some(path.to_path_buf()),
            chat: Default::default(),
        })
    }

    /// Use `chat` (for example one opened on a file) as the room's feed.
    #[must_use]
    pub fn with_chat(mut self, chat: crate::chat::Chat) -> Self {
        self.chat = chat;
        self
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Record one event. A failure to write is reported on stderr and never stops the host serving.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned, which only happens after another thread panicked while holding it.
    pub fn record(&self, event: &str, peer: Option<&str>, details: Value) {
        self.chat.event(event, peer, &details);
        let mut st = self.state.lock().expect("audit lock");
        if matches!(st.sink, Sink::Off) {
            return;
        }
        let body = json!({
            "seq": st.seq,
            "t": now_ms(),
            "prev": st.prev,
            "event": event,
            "peer": peer.map(|p| p.chars().take(16).collect::<String>()),
            "f": details,
        })
        .to_string();
        let hash = blake3::hash(body.as_bytes());
        let sig = self.key.sign(hash.as_bytes());
        let line = format!("{body}\t{}\n", hex(&sig.to_bytes()));
        let written = match &mut st.sink {
            Sink::File(f) => f.write_all(line.as_bytes()).and_then(|()| f.flush()),
            Sink::Stderr => std::io::stderr().write_all(format!("audit: {line}").as_bytes()),
            Sink::Off => Ok(()),
        };
        match written {
            Ok(()) => {
                st.seq += 1;
                st.prev = hex(hash.as_bytes());
            }
            Err(e) => eprintln!("playbox: could not write the audit log: {e}"),
        }
    }
}

/// What a successful check found.
#[derive(Debug, Clone)]
pub struct Summary {
    pub entries: u64,
    /// Hash of the last body: the value to keep elsewhere to detect later truncation.
    pub head: String,
    pub host_key: String,
}

/// Check the chain and every signature. The signing key is taken from the first entry unless one is given.
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub fn verify(path: &Path) -> Result<Summary> {
    verify_with(path, None)
}

/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
///
/// # Panics
///
/// Panics only if an internal invariant is broken, which indicates a bug.
pub fn verify_with(path: &Path, expected_key: Option<&[u8; 32]>) -> Result<Summary> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let (mut prev, mut seq) = (ZERO.to_string(), 0u64);
    let mut key: Option<VerifyingKey> = expected_key
        .map(VerifyingKey::from_bytes)
        .transpose()
        .map_err(|e| anyhow!("bad key: {e}"))?;
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        let at = |why: &str| anyhow!("line {}: {why}", i + 1);
        let (body, sig) = line
            .rsplit_once('\t')
            .ok_or_else(|| at("not an audit line"))?;
        let v: Value = serde_json::from_str(body).map_err(|e| at(&format!("bad json: {e}")))?;
        if v["seq"].as_u64() != Some(seq) {
            bail!("line {}: expected entry {seq}, found {} (a line was removed, reordered or inserted)", i + 1, v["seq"]);
        }
        if v["prev"].as_str() != Some(prev.as_str()) {
            bail!(
                "line {}: the chain is broken here (an earlier line was changed or removed)",
                i + 1
            );
        }
        if key.is_none() {
            // The first entry names the host's key.
            let k = v["f"]["host_key"]
                .as_str()
                .and_then(unhex::<32>)
                .ok_or_else(|| at("the first entry does not name the host's key"))?;
            key = Some(VerifyingKey::from_bytes(&k).map_err(|_| at("bad host key"))?);
        }
        let hash = blake3::hash(body.as_bytes());
        let sig = unhex::<64>(sig).ok_or_else(|| at("bad signature encoding"))?;
        key.as_ref()
            .expect("set above")
            .verify(hash.as_bytes(), &Signature::from_bytes(&sig))
            .map_err(|_| at("the signature does not verify (forged or edited)"))?;
        prev = hex(hash.as_bytes());
        seq += 1;
    }
    let key = key.ok_or_else(|| anyhow!("the log is empty"))?;
    Ok(Summary {
        entries: seq,
        head: prev,
        host_key: hex(&key.to_bytes()),
    })
}

/// The last `n` entries, parsed (without re-verifying; call [`verify`] first).
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub fn tail(path: &Path, n: usize) -> Result<Vec<Value>> {
    let lines: Vec<String> = BufReader::new(File::open(path)?)
        .lines()
        .collect::<std::io::Result<_>>()?;
    Ok(lines
        .iter()
        .rev()
        .take(n)
        .rev()
        .filter_map(|l| {
            l.rsplit_once('\t')
                .and_then(|(b, _)| serde_json::from_str(b).ok())
        })
        .collect())
}

/// One entry as a single readable line.
#[must_use]
pub fn describe(v: &Value) -> String {
    let t = v["t"].as_u64().unwrap_or(0);
    let secs = t / 1000;
    format!(
        "#{} {:02}:{:02}:{:02} {:<14} {:<16} {}",
        v["seq"],
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60,
        v["event"].as_str().unwrap_or("?"),
        v["peer"].as_str().unwrap_or("-"),
        v["f"]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("playbox-audit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("audit.log")
    }

    fn start(a: &Audit, k: &SigningKey) {
        a.record(
            "host_start",
            None,
            json!({ "host_key": hex(&k.verifying_key().to_bytes()) }),
        );
    }

    #[test]
    fn a_log_verifies_and_survives_a_restart() {
        let (p, k) = (tmp("ok"), key(1));
        let a = Audit::open(&p, k.clone()).unwrap();
        start(&a, &k);
        a.record("join", Some("abcdef0123456789abcdef"), json!({"ok": true}));
        drop(a);
        let a = Audit::open(&p, k.clone()).unwrap(); // continues the chain
        a.record("push", Some("abcdef0123456789"), json!({"bytes": 5}));
        let s = verify(&p).unwrap();
        assert_eq!(s.entries, 3);
        assert_eq!(tail(&p, 2).unwrap()[1]["event"], "push");
        assert_eq!(
            tail(&p, 1).unwrap()[0]["peer"],
            "abcdef0123456789",
            "peer ids are shortened"
        );
    }

    #[test]
    fn edits_deletions_insertions_and_forgeries_are_detected() {
        let (p, k) = (tmp("tamper"), key(1));
        let a = Audit::open(&p, k.clone()).unwrap();
        start(&a, &k);
        for i in 0..4 {
            a.record("task", Some("peer"), json!({"i": i, "exit_code": 0}));
        }
        let good = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = good.lines().collect();

        let edited = good.replacen("\"exit_code\":0", "\"exit_code\":9", 1);
        std::fs::write(&p, edited).unwrap();
        assert!(verify(&p).is_err(), "an edited entry went unnoticed");

        std::fs::write(&p, [&lines[..2], &lines[3..]].concat().join("\n") + "\n").unwrap();
        assert!(verify(&p).is_err(), "a deleted entry went unnoticed");

        std::fs::write(
            &p,
            [&lines[..3], &[lines[1]], &lines[3..]].concat().join("\n") + "\n",
        )
        .unwrap();
        assert!(verify(&p).is_err(), "an inserted entry went unnoticed");

        // Someone with file access but not the key appends a perfectly chained line signed by their own key.
        std::fs::write(&p, &good).unwrap();
        let other = Audit::open(&tmp("other"), key(2)).unwrap();
        start(&other, &key(2));
        let forged = std::fs::read_to_string(other.path().unwrap()).unwrap();
        std::fs::write(&p, good.clone() + &forged).unwrap();
        assert!(
            verify(&p).is_err(),
            "a line signed by another key was accepted"
        );
        // And a host will not silently extend a log that is not its own.
        assert!(Audit::open(&p, k.clone()).is_err());
    }

    #[test]
    fn free_text_is_flattened_and_clipped() {
        assert_eq!(clip("a\nb\tc"), "a b c");
        assert!(clip(&"x".repeat(1000)).chars().count() <= MAX_FIELD + 1);
    }

    #[test]
    fn the_log_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let (p, k) = (tmp("mode"), key(1));
        let a = Audit::open(&p, k.clone()).unwrap();
        start(&a, &k);
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
