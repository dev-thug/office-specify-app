//! `outreach-api` — REST API server (spec §4.4).

use std::sync::Arc;

use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_API};
use infrastructure::ratelimit::RateLimiter;
use infrastructure::redis::RedisClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();

    let config = Arc::new(Config::from_env()?);
    let mut db = Db::connect(&config.database_url, Some(ROLE_API))
        .await?
        .with_default_workspace(config.default_workspace_id);

    // Resolve the default workspace lazily when unset (single-workspace MVP).
    if db.default_workspace_id().is_none() {
        if let Some(ws) = infrastructure::repo::auth::first_workspace_id(&db).await? {
            db = db.with_default_workspace(Some(ws));
            tracing::info!(workspace_id = %ws, "default workspace resolved");
        } else {
            tracing::warn!("no workspace exists — run `outreach-cli bootstrap` first");
        }
    }

    let rate_limiter = match &config.redis_url {
        Some(url) => Some(RateLimiter::new(RedisClient::connect(url).await?)),
        None => {
            if config.app_env == infrastructure::config::AppEnv::Prod {
                anyhow::bail!("REDIS_URL is required in prod (rate limits fail closed)");
            }
            tracing::warn!("REDIS_URL unset — rate limiting disabled (dev/test only)");
            None
        }
    };

    let state = api::AppState::from_config(db, config.clone(), rate_limiter);
    let app = api::build_app(state);

    let listener = tokio::net::TcpListener::bind(config.api_addr).await?;
    tracing::info!(addr = %config.api_addr, "outreach-api listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}
