//! `outreach-mcp` — MCP Streamable HTTP server on :8081 (spec §13.3).

use std::sync::Arc;

use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_API};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();

    let config = Config::from_env().map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut db = Db::connect(&config.database_url, Some(ROLE_API))
        .await?
        .with_default_workspace(config.default_workspace_id);
    if db.default_workspace_id().is_none()
        && let Some(ws) = infrastructure::repo::auth::first_workspace_id(&db).await?
    {
        db = db.with_default_workspace(Some(ws));
    }

    let calendar = Arc::new(match &config.fake_provider_script {
        Some(path) => infrastructure::providers::fake::FakeMailProvider::from_script(path),
        None => infrastructure::providers::fake::FakeMailProvider::new(),
    });
    let state = mcp::server::McpState {
        calendar,
        db,
        allowed_origins: Arc::new(config.mcp_allowed_origins.clone()),
        tools: Arc::new(mcp::all_tools_with_key(&config.hmac_key)),
    };
    let app = mcp::server::build_router(state);
    let listener = tokio::net::TcpListener::bind(config.mcp_addr).await?;
    tracing::info!(addr = %config.mcp_addr, "outreach-mcp listening");
    axum::serve(listener, app).await?;
    Ok(())
}
