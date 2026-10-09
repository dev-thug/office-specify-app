//! `outreach-worker` — durable job runner (spec §4.7).

use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_WORKER};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();

    let config = Config::from_env().map_err(|e| anyhow::anyhow!("{e}"))?;
    let db = Db::connect(&config.database_url, Some(ROLE_WORKER)).await?;
    let mut runner = worker::Runner::new(db, worker::registry());
    runner.concurrency = config.worker_concurrency;
    tracing::info!(owner = %runner.owner, "outreach-worker started");
    runner.run().await?;
    Ok(())
}
