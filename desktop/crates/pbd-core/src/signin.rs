//! Signing the app's window in to the local hub without a passkey (the embedded webview cannot use platform passkeys for `localhost`).
//!
//! The app proves it is the owner of this machine's hub state the same way `playbox_hub.py --pair` does: by being able to write into
//! the hub's owner-only home directory. It creates `desktop-login/<sha256(nonce)>` (owner-only, one use, valid for a minute), then
//! sends the window to `/desktop/signin?n=<nonce>`; the hub checks the file, deletes it, and opens a weak, local-only session.
//! The session is ignored on any request that is not loopback, and sensitive actions still need a real passkey from a browser.

use crate::probe::http_status;
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

const DIR: &str = "desktop-login";

fn b64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 2);
    for c in bytes.chunks(3) {
        let n = c.iter().fold(0u32, |a, b| a << 8 | u32::from(*b)) << (8 * (3 - c.len()));
        for i in 0..=c.len() {
            out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Create the one-use proof file for a fresh nonce under `hub_home` and return the nonce.
pub fn prepare(hub_home: &Path) -> io::Result<String> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|e| io::Error::other(e.to_string()))?;
    let nonce = b64url(&raw);
    let dir = hub_home.join(DIR);
    make_private_dir(&dir)?;
    let name = hex(&Sha256::digest(nonce.as_bytes()));
    create_private_file(&dir.join(name))?;
    Ok(nonce)
}

#[cfg(unix)]
fn make_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}
#[cfg(not(unix))]
fn make_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map(drop)
}
#[cfg(not(unix))]
fn create_private_file(path: &Path) -> io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(drop)
}

pub fn url(port: u16, nonce: &str) -> String {
    url_with_next(port, nonce, None)
}

pub fn url_with_next(port: u16, nonce: &str, next: Option<&str>) -> String {
    match next {
        Some(n) if n != "/" && n.starts_with('/') => {
            let (path_and_query, frag) = match n.split_once('#') {
                Some((p, f)) => (p, Some(f)),
                None => (n, None),
            };
            let encoded: String = path_and_query
                .bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.') {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect();
            match frag {
                Some(f) => {
                    format!("http://localhost:{port}/desktop/signin?n={nonce}&next={encoded}#{f}")
                }
                None => format!("http://localhost:{port}/desktop/signin?n={nonce}&next={encoded}"),
            }
        }
        _ => format!("http://localhost:{port}/desktop/signin?n={nonce}"),
    }
}

/// Does this hub know the `/desktop/signin` flow? (An older hub does not: then the window just opens the hub.)
pub fn supported(port: u16) -> bool {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let limit = Duration::from_secs(2);
    if http_status(addr, crate::hub::HEALTH_PATH, limit) != Some(200) {
        return false;
    }
    let Ok(mut s) = TcpStream::connect_timeout(&addr, limit) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(limit));
    let _ = s.set_write_timeout(Some(limit));
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n",
        crate::hub::HEALTH_PATH
    );
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut body = String::new();
    let _ = s.take(64 * 1024).read_to_string(&mut body);
    body.contains("\"local_signin\": true") || body.contains("\"local_signin\":true")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_matches_known_vectors() {
        assert_eq!(b64url(b""), "");
        assert_eq!(b64url(b"f"), "Zg");
        assert_eq!(b64url(b"fo"), "Zm8");
        assert_eq!(b64url(b"foo"), "Zm9v");
        assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn prepare_makes_a_private_one_use_file_named_by_the_nonce_hash() {
        let home = std::env::temp_dir().join(format!("pbd-signin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let n = prepare(&home).unwrap();
        assert!(
            (32..=64).contains(&n.len())
                && n.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        let f = home.join(DIR).join(hex(&Sha256::digest(n.as_bytes())));
        assert!(f.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(f.metadata().unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(
                home.join(DIR).metadata().unwrap().permissions().mode() & 0o077,
                0
            );
        }
        assert_ne!(n, prepare(&home).unwrap());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn url_carries_the_nonce_to_localhost() {
        assert_eq!(
            url(7717, "abc"),
            "http://localhost:7717/desktop/signin?n=abc"
        );
    }
}
