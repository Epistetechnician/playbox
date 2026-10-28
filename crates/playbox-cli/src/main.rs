use anyhow::{bail, Result};
use ed25519_dalek::SigningKey;
use playbox_core::history::Log;
use playbox_net::{join_link, new_secret_key, Host};
use std::time::Duration;

const HELP: &str = "playbox (scaffold)
  up [--local]              start a room and print a join link (valid 1h, 1 use)
  join <link> [--local]     join a room and send an echo task
  demo                      build and verify a sample history log
planned: status | delegate | verify | revoke (see PRD.md)";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let local = args.iter().any(|a| a == "--local");
    let pos: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    match pos.first().map(|s| s.as_str()) {
        Some("up") => up(local).await,
        Some("join") => match pos.get(1) {
            Some(link) => join(link, local).await,
            None => bail!("usage: playbox join <link>"),
        },
        Some("demo") => demo(),
        _ => {
            println!("{HELP}");
            Ok(())
        }
    }
}

async fn up(local: bool) -> Result<()> {
    let host = Host::start(new_secret_key()?, local).await?;
    let link = host.create_link(Duration::from_secs(3600), 1)?;
    println!("room is up. share this link:\n{}", link.encode());
    println!("(ctrl-c to stop)");
    tokio::signal::ctrl_c().await?;
    host.shutdown().await
}

async fn join(link: &str, local: bool) -> Result<()> {
    let client = join_link(link, new_secret_key()?, local).await?;
    let reply = client.echo("hello from a guest agent").await?;
    println!("joined. echo task returned: {reply}");
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
