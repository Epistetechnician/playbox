use playbox_core::store;
use playbox_mcp::Server;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut server = Server::new(&store::home())?;
    eprintln!(
        "playbox-mcp {} ready; history log: {}",
        env!("CARGO_PKG_VERSION"),
        server.log_path().display()
    );
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    // With live delivery on, new room messages are pushed between requests; otherwise this never ticks.
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let line = tokio::select! {
            l = lines.next_line() => match l? { Some(l) => l, None => break },
            _ = tick.tick(), if server.live_enabled() => {
                let mut pushes: Vec<Value> = server.live_hello().into_iter().collect();
                pushes.extend(server.live_poll().await);
                for n in pushes {
                    out.write_all(format!("{n}\n").as_bytes()).await?;
                    out.flush().await?;
                }
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => server.handle(msg).await,
            Err(e) => Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}}),
            ),
        };
        if let Some(r) = reply {
            out.write_all(format!("{r}\n").as_bytes()).await?;
            out.flush().await?;
        }
    }
    Ok(())
}
