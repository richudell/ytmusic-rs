mod config;
mod oauth;
mod token_store;
mod tools;
mod ytdata_client;
mod ytmusic_client;

use config::Config;
use rmcp::{transport::stdio, ServiceExt};
use tools::YtMusicServer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // MCP stdio transport uses stdout for protocol traffic — all logging
    // must go to stderr or it'll corrupt the JSON-RPC stream.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let config = Config::load()?;
    tracing::info!("Starting ytmusic-rs MCP server (stdio transport)");

    let server = YtMusicServer::new(config)?;
    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("serving error: {e:?}");
    })?;

    service.waiting().await?;
    Ok(())
}
