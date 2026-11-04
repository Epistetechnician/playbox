//! `playbox latent session LINK`: join a room once with a key you hold and relay hub latent messages as JSON lines.
//!
//! Python (or anything else) owns the key file, signs `PBL2` envelopes itself and talks to this process over
//! stdin/stdout. One request per line, one reply per line:
//!
//! ```text
//! {"send": "<base64 envelope>"}            -> {"ok": true, "seq": 7}
//! {"pull": "<hub id>", "since": 0}         -> {"ok": true, "next": 9, "items": [{"seq": 8, "from": "<id>", "envelope": "<base64>"}]}
//! {"reveal": "<base64 {salt, sentences}>", "seq": 7} -> {"ok": true, "seq": 7}
//! {"say": "[challenge] seq=7"}            -> {"ok": true, "seq": 31}
//! {"feed": 0}                              -> {"ok": true, "next": 12, "lines": ["[latent] hub ...", ...]}
//! ```
//!
//! Errors come back as `{"ok": false, "error": "..."}`. The first line printed is `{"ready": true, "id": "<member id>"}`.

use crate::{cli::TransportArgs, rooms};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use playbox_core::store;
use playbox_net::{identity_key, join_link};
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};

pub async fn session(link: &str, transport: &TransportArgs, key: Option<String>) -> Result<()> {
    let key_path = key.map_or_else(|| store::home().join("guest.key"), PathBuf::from);
    let client = join_link(link, identity_key(&key_path)?, rooms::join_choice(transport)?).await?;
    println!("{}", json!({ "ready": true, "id": client.id().to_string() }));
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match handle(&client, &line).await {
            Ok(mut v) => {
                v["ok"] = json!(true);
                v
            }
            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
        };
        println!("{reply}");
    }
    client.close().await;
    Ok(())
}

async fn handle(client: &playbox_net::Client, line: &str) -> Result<Value> {
    let req: Value = serde_json::from_str(line).map_err(|e| anyhow!("bad request: {e}"))?;
    if let Some(b) = req["send"].as_str() {
        let seq = client.latent_send(&B64.decode(b)?).await?;
        return Ok(json!({ "seq": seq }));
    }
    if let Some(text) = req["say"].as_str() {
        let seq = client.say(text, None).await?;
        return Ok(json!({ "seq": seq }));
    }
    if let Some(r) = req["reveal"].as_str() {
        let seq = req["seq"].as_u64().ok_or_else(|| anyhow!("reveal needs a seq"))?;
        client.latent_reveal(seq, &B64.decode(r)?).await?;
        return Ok(json!({ "seq": seq }));
    }
    if let Some(hub) = req["pull"].as_str() {
        let (items, next) = client.latent_pull(hub, req["since"].as_u64().unwrap_or(0)).await?;
        let items: Vec<Value> = items
            .into_iter()
            .map(|d| {
                json!({ "seq": d.seq, "from": d.from, "src": d.envelope.src, "envelope": B64.encode(d.raw),
                        "reveal": d.reveal.map(|r| B64.encode(r)) })
            })
            .collect();
        return Ok(json!({ "next": next, "items": items }));
    }
    if let Some(since) = req["feed"].as_u64() {
        let f = client.feed(since).await?;
        let lines: Vec<&str> = f.items.iter().map(|i| i.text.as_str()).collect();
        return Ok(json!({ "next": f.next, "lines": lines }));
    }
    Err(anyhow!("unknown request"))
}
