//! Shared test helper — same contract as infrastructure's tests/common:
//! `DATABASE_URL` must point at a dedicated test DB.

use infrastructure::db::{Db, ROLE_API, ROLE_WORKER};

/// Migrate as owner, then connect as `worker_runtime` so tests exercise RLS
/// exactly like the production runner.
pub async fn test_db() -> Option<Db> {
    // DB tests must never silently pass — a missing DATABASE_URL is a
    // failure, not a skip.
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
    let owner = Db::connect(&url, None).await.expect("connect failed");
    owner.migrate().await.expect("migrations failed");
    Some(
        Db::connect(&url, Some(ROLE_WORKER))
            .await
            .expect("connect failed"),
    )
}

/// Fixtures run as `api_runtime` — `worker_runtime` correctly cannot write
/// workspaces/users after the R22 grant tightening, so seeding must not use
/// the worker connection.
pub async fn seed_db() -> Db {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
    Db::connect(&url, Some(ROLE_API))
        .await
        .expect("connect failed")
}
