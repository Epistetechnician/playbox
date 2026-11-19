//! Secret keys: OS randomness, a guest's stable identity key, and seeds kept in owner-only files.

use anyhow::{anyhow, bail, Context, Result};
use iroh::SecretKey;
use std::io::Read;

/// 32 random bytes from the OS.
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub fn random_bytes() -> Result<[u8; 32]> {
    let mut b = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}

/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub fn new_secret_key() -> Result<SecretKey> {
    Ok(SecretKey::from_bytes(&random_bytes()?))
}

/// Where a guest's stable identity key lives, for links bound to it.
///
/// # Errors
///
/// Returns an error if the operation fails; the error carries the cause.
pub fn identity_key(path: &std::path::Path) -> Result<SecretKey> {
    match std::fs::read(path) {
        Ok(b) => Ok(SecretKey::from_bytes(
            &<[u8; 32]>::try_from(b.as_slice())
                .map_err(|_| anyhow!("{} is not a 32-byte key", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let k = random_bytes()?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, k)?;
            #[cfg(unix)]
            std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
            Ok(SecretKey::from_bytes(&k))
        }
        Err(e) => Err(e).context("reading identity key"),
    }
}

/// Read an existing 32-byte hex seed file without creating one if missing.
///
/// # Errors
///
/// Fails if the file cannot be read or does not contain 64 hex characters.
pub fn load_seed_hex(path: &std::path::Path) -> Result<[u8; 32]> {
    let t = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let t = t.trim();
    if t.len() != 64 || !t.is_ascii() {
        bail!("{} must hold 64 hex characters", path.display());
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&t[i * 2..i * 2 + 2], 16).context("key file is not hex")?;
    }
    Ok(out)
}

/// A 32-byte secret stored as hex in a file readable only by the owner; created if missing.
pub(crate) fn load_or_create_seed(path: &std::path::Path) -> Result<[u8; 32]> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    if path.exists() {
        return load_seed_hex(path);
    }
    let seed = random_bytes()?;
    let hex: String = seed.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(hex.as_bytes())?;
    Ok(seed)
}
