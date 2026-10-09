//! Shared test helper — same contract as infrastructure's tests/common:
//! `DATABASE_URL` must point at a dedicated test DB.

use infrastructure::db::{Db, ROLE_WORKER};

/// Migrate as owner, then connect as `worker_runtime` so tests exercise RLS
/// exactly like the production runner.
pub async fn test_db() -> Option<Db> {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            eprintln!("DATABASE_URL not set — skipping DB test");
            return None;
        }
    };
    let owner = Db::connect(&url, None).await.expect("connect failed");
    owner.migrate().await.expect("migrations failed");
    Some(
        Db::connect(&url, Some(ROLE_WORKER))
            .await
            .expect("connect failed"),
    )
}
