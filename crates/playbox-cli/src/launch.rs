//! `playbox launch`: start an agent process that the room's host can vouch for.
//!
//! The host's owner runs this on the host's machine. It makes the agent's identity key itself (in a private home it creates for that
//! process), asks the running host for a join link bound to exactly that key, starts the command with that home and the link, and, once the
//! key shows up as a member, has the host sign a launch record: this key was started by the host's owner with this harness, this model and
//! an executable with this digest. That record is the only thing the hub will call "host-observed".
//!
//! What it does not prove: that the command really is the harness it is named after, or what the process does after it starts. The record
//! says who started it and what file was run, and nothing more.

use crate::cli::LaunchArgs;
use anyhow::{anyhow, bail, Context, Result};
use playbox_core::store;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// One line to a host's control socket and its one-line answer.
#[cfg(unix)]
fn ask(sock: &Path, line: &str) -> Result<String> {
    use std::io::{Read, Write};
    let mut s = std::os::unix::net::UnixStream::connect(sock)
        .with_context(|| format!("the host did not answer at {}", sock.display()))?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    s.write_all(format!("{line}\n").as_bytes())?;
    let mut out = String::new();
    s.take(1 << 20).read_to_string(&mut out)?;
    Ok(out.trim().to_string())
}

#[cfg(not(unix))]
fn ask(_: &Path, _: &str) -> Result<String> {
    bail!("launching needs a host control socket, which this platform does not have")
}

/// The running host named `room` (its label or the start of its id).
fn find_host(room: &str) -> Result<(PathBuf, String)> {
    let dir = crate::control::hosting_dir();
    let mut found = Vec::new();
    for e in std::fs::read_dir(&dir)
        .with_context(|| format!("no hosts are running here ({})", dir.display()))?
        .flatten()
    {
        if e.path().extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Ok(rec) = std::fs::read_to_string(e.path())
            .map_err(anyhow::Error::from)
            .and_then(|t| Ok(serde_json::from_str::<serde_json::Value>(&t)?))
        else {
            continue;
        };
        let (id, name, sock) = (
            rec["id"].as_str().unwrap_or(""),
            rec["name"].as_str().unwrap_or(""),
            rec["sock"].as_str().unwrap_or(""),
        );
        if !sock.is_empty() && (name == room || (!id.is_empty() && id.starts_with(room))) {
            found.push((PathBuf::from(sock), name.to_string()));
        }
    }
    match found.len() {
        0 => bail!("no running host called {room:?}: start one with `playbox up`, or check `playbox service list`"),
        1 => Ok(found.remove(0)),
        _ => bail!("more than one running host matches {room:?}: use more of its id"),
    }
}

/// A single word the control protocol can carry: no whitespace or control characters.
fn word(s: &str, max: usize) -> String {
    s.chars()
        .map(|c| {
            if c.is_whitespace() || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .take(max)
        .collect()
}

/// `sha256:<hex>` of the executable that will run, so the record names the exact file. Empty if it cannot be found or read.
fn exe_digest(cmd: &str) -> String {
    let path = if cmd.contains('/') {
        Some(PathBuf::from(cmd))
    } else {
        std::env::var_os("PATH").and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join(cmd))
                .find(|c| c.is_file())
        })
    };
    path.and_then(|p| std::fs::read(p).ok())
        .map(|b| format!("sha256:{:x}", Sha256::digest(b)))
        .unwrap_or_default()
}

/// Wait for `id` to be a member, then have the host sign the record. Gives up after a few minutes.
fn record_when_joined(
    sock: &Path,
    id: &str,
    harness: &str,
    model: &str,
    image: &str,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    for _ in 0..300 {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            bail!("stopped");
        }
        let members = ask(sock, "members")?;
        if members.contains(id) {
            let line = format!("launch {id} {harness} {model} {image}");
            let reply = ask(sock, line.trim_end())?;
            if reply.starts_with("ok") {
                eprintln!(
                    "launch recorded: the host signed that it started {}",
                    &id[..12]
                );
                return Ok(());
            }
            bail!("the host would not record the launch: {reply}");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    bail!("the agent did not join within five minutes, so no launch record was made")
}

pub async fn launch(a: LaunchArgs) -> Result<()> {
    let (sock, host_name) = find_host(&a.room)?;
    let harness = word(&a.harness, 30);
    if harness.is_empty() {
        bail!("--harness needs a name");
    }
    let model = word(a.model.as_deref().unwrap_or(""), 40);
    let cmd = a
        .command
        .first()
        .ok_or_else(|| anyhow!("give the command to run after --"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tag: String = harness
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let home = store::home()
        .join("launched")
        .join(format!("{tag}-{stamp:x}"));
    std::fs::create_dir_all(&home)?;
    #[cfg(unix)]
    std::fs::set_permissions(&home, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let id = playbox_net::identity_key(&home.join("guest.key"))?
        .public()
        .to_string();
    let link = ask(&sock, &format!("link {id}"))?;
    if !link.starts_with("playbox://") {
        bail!("the host would not make a link: {link}");
    }
    let link_file = home.join("join-link");
    std::fs::write(&link_file, &link)?;
    #[cfg(unix)]
    std::fs::set_permissions(
        &link_file,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )?;
    let image = exe_digest(cmd);
    eprintln!(
        "launching {harness} in {host_name}: key {} (home {})",
        &id[..12],
        home.display()
    );
    let mut c = tokio::process::Command::new(cmd);
    c.args(&a.command[1..])
        .env("PLAYBOX_HOME", &home)
        .env("PLAYBOX_JOIN_LINK_FILE", &link_file)
        .env("PLAYBOX_AGENT_HARNESS", &harness)
        .env("PLAYBOX_AGENT_MODEL", &model);
    if let Some(d) = &a.cwd {
        c.current_dir(d);
    }
    let mut child = c.spawn().with_context(|| format!("starting {cmd}"))?;
    let (s2, id2, h2, m2, i2) = (
        sock.clone(),
        id.clone(),
        harness.clone(),
        model.clone(),
        image.clone(),
    );
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let rec =
        tokio::task::spawn_blocking(move || record_when_joined(&s2, &id2, &h2, &m2, &i2, &stop2));
    let status = child.wait().await?;
    // A short agent can exit before the next poll sees it as a member: give the record a moment.
    match tokio::time::timeout(Duration::from_secs(5), rec).await {
        Ok(Ok(Err(e))) => eprintln!("warning: {e:#}"),
        Ok(_) => {}
        Err(_) => {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            eprintln!("the agent exited before it joined, so no launch record was made");
        }
    }
    if !status.success() {
        bail!("{cmd} exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_carry_no_whitespace_and_are_clipped() {
        assert_eq!(word("Claude Code\n1", 30), "Claude-Code-1");
        assert_eq!(word("abcdef", 3), "abc");
    }

    #[test]
    fn an_executable_digest_names_the_file_and_a_missing_one_is_empty() {
        assert!(exe_digest("sh").starts_with("sha256:") && exe_digest("sh").len() == 71);
        assert_eq!(exe_digest("definitely-not-a-command-here"), "");
    }
}
