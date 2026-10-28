use anyhow::{bail, Result};
use ed25519_dalek::SigningKey;
use playbox_core::history::Log;
use playbox_net::{join_link, new_secret_key, Host, HostConfig};
use playbox_runner::{DockerRunner, TaskSpec};
use std::{path::PathBuf, sync::Arc, time::Duration};

const HELP: &str = "playbox (scaffold)
  up [--local] [--repo DIR] [--docker]
                            start a room and print a join link (valid 1h, 1 use).
                            --repo shares a git repo; --docker lets members run tasks in containers
  join <link> [--local] [--task <shell cmd>] [--base REF]
                            join a room; with --task, run it remotely on REF (default main) and print the patch
  demo                      build and verify a sample history log
planned: status | delegate | verify | revoke (see PRD.md)";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let local = args.iter().any(|a| a == "--local");
    let docker = args.iter().any(|a| a == "--docker");
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let (repo, task, base) = (opt("--repo"), opt("--task"), opt("--base"));
    let mut skip = false;
    let pos: Vec<&String> = args
        .iter()
        .filter(|a| {
            if skip {
                skip = false;
                return false;
            }
            if matches!(a.as_str(), "--repo" | "--task" | "--base") {
                skip = true;
                return false;
            }
            !a.starts_with("--")
        })
        .collect();
    match pos.first().map(|s| s.as_str()) {
        Some("up") => up(local, repo, docker).await,
        Some("join") => match pos.get(1) {
            Some(link) => join(link, local, task, base).await,
            None => bail!("usage: playbox join <link>"),
        },
        Some("demo") => demo(),
        _ => {
            println!("{HELP}");
            Ok(())
        }
    }
}

async fn up(local: bool, repo: Option<String>, docker: bool) -> Result<()> {
    let cfg = HostConfig {
        repo: repo.map(PathBuf::from),
        runner: if docker { Some(Arc::new(DockerRunner)) } else { None },
        ..Default::default()
    };
    let host = Host::start_with(new_secret_key()?, local, cfg).await?;
    let link = host.create_link(Duration::from_secs(3600), 1)?;
    println!("room is up. share this link:\n{}", link.encode());
    println!("(ctrl-c to stop)");
    tokio::signal::ctrl_c().await?;
    host.shutdown().await
}

async fn join(link: &str, local: bool, task: Option<String>, base: Option<String>) -> Result<()> {
    let client = join_link(link, new_secret_key()?, local).await?;
    if let Some(cmd) = task {
        let (runner, out, patch) = client
            .run_task(base.as_deref().unwrap_or("main"), TaskSpec::new(&["sh", "-c", &cmd]))
            .await?;
        println!("ran on {runner}: exit {:?}{}", out.exit_code, if out.timed_out { " (timed out)" } else { "" });
        print!("{}", out.stdout);
        eprint!("{}", out.stderr);
        println!("--- patch ({} bytes) ---\n{}", patch.len(), String::from_utf8_lossy(&patch));
    } else {
        let reply = client.echo("hello from a guest agent").await?;
        println!("joined. echo task returned: {reply}");
    }
    client.close().await;
    Ok(())
}

fn demo() -> Result<()> {
    let key = SigningKey::from_bytes(&playbox_net::random_bytes()?);
    let mut log = Log::new();
    for (kind, payload) in [("session_start", "demo"), ("tool_call", "cargo test"), ("commit", "tree:demo")] {
        log.append(&key, kind, payload.as_bytes(), 0);
    }
    match log.verify() {
        Ok(()) => println!("ok: {} entries verified", log.entries.len()),
        Err(e) => bail!("invalid: {e}"),
    }
    Ok(())
}
