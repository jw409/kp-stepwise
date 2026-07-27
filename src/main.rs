mod logging;
mod planner;
mod profiles;
mod server;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing to stderr (MCP uses stdout for JSON-RPC)
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("KP_STEPWISE_LOG_LEVEL")
                .or_else(|_| EnvFilter::try_from_env("KP_SEQTHINK_LOG_LEVEL")) // legacy name
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("kp-stepwise starting");

    server::run().await
}
