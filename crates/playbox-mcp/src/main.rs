use playbox_core::store;
use playbox_mcp::Server;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut server = Server::new(&store::home())?;
    eprintln!(
        "playbox-mcp {} ready; history log: {}",
        env!("CARGO_PKG_VERSION"),
        server.log_path().display()
    );
    server.join_from_env().await;
    server.resume_from_store().await;
    server.serve_stdio().await
}
