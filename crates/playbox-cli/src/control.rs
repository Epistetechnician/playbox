//! A running `playbox up` that keeps its room's identity (`--state DIR`) publishes itself on this machine so the hub can adopt it:
//! a record `<hosting dir>/<id>.json` and a private control socket `<hosting dir>/<id>.sock`. The hosting directory is
//! `$PLAYBOX_HOSTING_DIR`, else `~/.playbox/hosting`, and is created `0700`; only this user can reach the socket.
//!
//! The socket speaks one line each way: `ping` answers `ok`; `link [GUEST ID]` answers a fresh single-use join link (or `error: ...`).
//! The record never holds a link or a key. Both files are removed when the host stops; a record whose socket does not answer is stale.

use anyhow::{Context, Result};
use playbox_net::Host;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where hosts publish themselves.
pub fn hosting_dir() -> PathBuf {
    std::env::var_os("PLAYBOX_HOSTING_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".playbox").join("hosting"))
}

#[cfg(unix)]
pub struct Published {
    sock: PathBuf,
    record: PathBuf,
}

#[cfg(unix)]
impl Drop for Published {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.sock);
        let _ = std::fs::remove_file(&self.record);
    }
}

/// Publish this host. `root` is the room's root public key in hex; the first 16 characters name the files.
#[cfg(unix)]
pub fn publish(root: &str, state: &Path, repo: Option<&Path>) -> Result<(Published, tokio::net::UnixListener)> {
    use std::os::unix::fs::PermissionsExt;
    let id = &root[..16.min(root.len())];
    let dir = hosting_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let (sock, record) = (dir.join(format!("{id}.sock")), dir.join(format!("{id}.json")));
    let _ = std::fs::remove_file(&sock);
    let listener = tokio::net::UnixListener::bind(&sock).with_context(|| format!("bind {}", sock.display()))?;
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;
    let abs = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().into_owned();
    let name = state.file_name().map_or_else(|| "room".to_string(), |n| n.to_string_lossy().into_owned());
    let started_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let rec = serde_json::json!({"id": id, "name": name, "state": abs(state), "repo": repo.map(abs), "sock": sock.to_string_lossy(), "pid": std::process::id(), "started_ms": started_ms});
    std::fs::write(&record, serde_json::to_vec_pretty(&rec)?)?;
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600))?;
    Ok((Published { sock, record }, listener))
}

/// Answer control connections for as long as the host runs. Never returns.
#[cfg(unix)]
pub async fn serve(host: &Host, listener: &tokio::net::UnixListener, link_opts: impl Fn(Option<&str>) -> Result<playbox_net::LinkOpts>) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        let (rd, mut wr) = stream.into_split();
        let mut line = String::new();
        let got = tokio::time::timeout(Duration::from_secs(5), BufReader::new(rd.take(512)).read_line(&mut line)).await;
        if !matches!(got, Ok(Ok(n)) if n > 0) {
            continue;
        }
        let mut w = line.split_whitespace();
        let reply = match (w.next(), w.next()) {
            (Some("ping"), _) => "ok".to_string(),
            (Some("link"), who) => match link_opts(who).and_then(|o| host.create_link_with(Duration::from_secs(3600), 1, o)) {
                Ok(l) => l.encode(),
                Err(e) => format!("error: {e:#}").replace('\n', " "),
            },
            _ => "error: ping | link [GUEST ID]".to_string(),
        };
        let _ = wr.write_all(format!("{reply}\n").as_bytes()).await;
    }
}
