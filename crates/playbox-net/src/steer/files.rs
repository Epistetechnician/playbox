//! Signed decisions, the weights manifest, prompt sets and scratch directories.

use super::MAX_OUTPUT_BYTES;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::{
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

/// The text a host signs to say what it decided about one handoff.
pub(super) fn decision_body(
    host: &[u8],
    signer: &[u8],
    weights: &str,
    envelope: &[u8],
    accepted: bool,
    audited: bool,
) -> String {
    format!(
        "playbox.handoff-decision.v1\nhost {}\nsigner {}\nweights {weights}\nenvelope {}\naccepted {accepted}\naudited {audited}\n",
        hex(host),
        hex(signer),
        hex(blake3::hash(envelope).as_bytes()),
    )
}

/// Check a host's signed statement that it decided this about exactly these handoff bytes.
/// Returns whether the host accepted.
pub fn verify_decision(
    text: &str,
    host: &[u8; 32],
    signer: &[u8; 32],
    weights: &str,
    envelope: &[u8],
) -> Result<bool> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let (body, sig) = text
        .rsplit_once("sig ")
        .ok_or_else(|| anyhow!("the decision has no signature"))?;
    let sig = hex_to_64(sig.trim())?;
    VerifyingKey::from_bytes(host)
        .map_err(|e| anyhow!("{e}"))?
        .verify(body.as_bytes(), &Signature::from_bytes(&sig))
        .map_err(|_| anyhow!("the decision is not signed by the host"))?;
    let accepted = body.contains("\naccepted true\n");
    let audited = body.contains("\naudited true\n");
    if body != decision_body(host, signer, weights, envelope, accepted, audited) {
        bail!("the decision is about a different handoff");
    }
    Ok(accepted)
}

pub(crate) fn hex_to_64(s: &str) -> Result<[u8; 64]> {
    if s.len() != 128 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("bad signature encoding");
    }
    let mut out = [0u8; 64];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16)?;
    }
    Ok(out)
}

#[must_use]
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(super) fn unhex32(s: &str) -> Result<[u8; 32]> {
    playbox_core::record::unhex::<32>(s).map_err(|e| anyhow!("{e}"))
}

pub(super) fn hash_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().to_hex().to_string())
}

pub(super) fn collect(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let ty = e.file_type()?; // does not follow symlinks
        let r = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        if ty.is_dir() {
            collect(&e.path(), &r, out)?;
        } else if ty.is_file() {
            out.push((r, e.path()));
        }
    }
    Ok(())
}

/// Hash of a weights directory: blake3 of `relpath<space>blake3(file)<newline>` lines sorted by the UTF-8
/// bytes of the path, over regular files only, skipping dot-files and symlinks. The Python engine computes
/// the same value (`playbox_steer manifest`).
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub fn weights_manifest(dir: &Path) -> Result<[u8; 32]> {
    let mut files = Vec::new();
    collect(dir, "", &mut files)?;
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut text = String::new();
    for (rel, p) in files {
        text.push_str(&format!("{rel} {}\n", hash_file(&p)?));
    }
    Ok(*blake3::hash(text.as_bytes()).as_bytes())
}

/// The prompt set as the host reads it, mirroring the engine's checks so a bad one is refused before a
/// container starts.
#[derive(Debug, PartialEq, Eq)]
pub struct PromptSet {
    pub prompts: usize,
    pub max_new_tokens: u64,
}

impl PromptSet {
    /// # Errors
    ///
    /// Returns an error if the operation fails; the error carries the cause.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let v: Value = serde_json::from_slice(bytes).context("prompt set is not JSON")?;
        let obj = v
            .as_object()
            .ok_or_else(|| anyhow!("prompt set must be an object"))?;
        if let Some(k) = obj
            .keys()
            .find(|k| !matches!(k.as_str(), "prompts" | "max_new_tokens" | "temperature"))
        {
            bail!("unexpected prompt set field {k:?}");
        }
        let list = obj
            .get("prompts")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("prompt set needs a prompts list"))?;
        if list.is_empty() || list.len() > 256 {
            bail!("need 1 to 256 prompts");
        }
        for p in list {
            match p.as_str() {
                Some(s) if !s.is_empty() && s.chars().count() <= 8192 => {}
                _ => bail!("each prompt must be a non-empty string under 8192 characters"),
            }
        }
        let n = match obj.get("max_new_tokens") {
            None => 32,
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=512).contains(n))
                .ok_or_else(|| anyhow!("max_new_tokens must be 1..512"))?,
        };
        if let Some(t) = obj.get("temperature") {
            if !t.as_f64().is_some_and(|t| (0.0..=2.0).contains(&t)) {
                bail!("temperature must be 0..2");
            }
        }
        Ok(Self {
            prompts: list.len(),
            max_new_tokens: n,
        })
    }

    /// Tokens a run generates: every prompt twice (steered and clean).
    #[must_use]
    pub fn tokens(&self) -> u64 {
        self.prompts as u64 * self.max_new_tokens * 2
    }
}

/// A scratch directory removed on drop, even if the run is cancelled.
pub(super) struct Scratch(pub(super) PathBuf);

impl Scratch {
    pub(super) fn new() -> Result<Self> {
        let r = crate::random_bytes()?;
        let dir = std::env::temp_dir().join(format!("playbox-steer-{}", hex(&r[..8])));
        std::fs::create_dir(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn read_output(path: &Path) -> Result<Vec<u8>> {
    // The task wrote here, so treat the path as untrusted: no symlinks, regular file, bounded.
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .context("the engine wrote no output")?;
    let meta = f.metadata()?;
    if !meta.is_file() || meta.len() > MAX_OUTPUT_BYTES {
        bail!(
            "engine output is not a regular file under {} MiB",
            MAX_OUTPUT_BYTES >> 20
        );
    }
    let mut buf = Vec::new();
    f.take(MAX_OUTPUT_BYTES).read_to_end(&mut buf)?;
    let v: Value = serde_json::from_slice(&buf).context("engine output is not JSON")?;
    if !v.is_object() {
        bail!("engine output is not a JSON object");
    }
    Ok(buf)
}
