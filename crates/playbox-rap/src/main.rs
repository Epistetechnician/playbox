use anyhow::{bail, Result};
use playbox_core::store;
use playbox_mcp::Server;
use playbox_rap::{serve, Rap};
use tokio::net::TcpListener;

/// `PLAYBOX_RAP_TOKEN` (required) is the bearer token runtimes must send.
/// `PLAYBOX_RAP_ADDR` (default 127.0.0.1:3010), `PLAYBOX_RAP_URL` (public base URL, default from addr),
/// `PLAYBOX_RAP_APPROVER_KEY` (path to an approver key file; turns on approvals through RAP `user_choice`),
/// `PLAYBOX_RAP_CALLBACK_HOSTS` (comma list of non-loopback hosts callbacks may name).
#[tokio::main]
async fn main() -> Result<()> {
    let Ok(token) = std::env::var("PLAYBOX_RAP_TOKEN") else {
        bail!("set PLAYBOX_RAP_TOKEN to the bearer token RAP runtimes will send");
    };
    if token.len() < 16 {
        bail!("PLAYBOX_RAP_TOKEN must be at least 16 characters");
    }
    let addr = std::env::var("PLAYBOX_RAP_ADDR").unwrap_or_else(|_| "127.0.0.1:3010".into());
    let listener = TcpListener::bind(&addr).await?;
    let base = std::env::var("PLAYBOX_RAP_URL")
        .unwrap_or_else(|_| format!("http://{}", listener.local_addr().unwrap()));
    let hosts = std::env::var("PLAYBOX_RAP_CALLBACK_HOSTS")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let server = Server::new(&store::home())?;
    let rap = match std::env::var("PLAYBOX_RAP_APPROVER_KEY") {
        Ok(path) => {
            let key = store::load_or_create_key(std::path::Path::new(&path))?;
            eprintln!(
                "approvals via RAP user_choice; set PLAYBOX_APPROVERS to {}",
                playbox_core::record::hex(&key.verifying_key().to_bytes())
            );
            Rap::with_approver(
                server,
                token,
                hosts,
                base.clone(),
                key,
                std::time::Duration::from_secs(600),
            )
        }
        Err(_) => Rap::new(server, token, hosts, base.clone()),
    };
    eprintln!("playbox-rap ready at {base}");
    serve(listener, rap).await
}
