//! Shared test helpers. `DATABASE_URL` must point at a dedicated test DB
//! (verification uses `postgres://hyunjoong@localhost:5432/outreach_test`).

use infrastructure::db::{Db, ROLE_API};

/// Connect as the test identity with `SET LOCAL ROLE api_runtime` applied
/// per transaction — tests exercise RLS exactly like production even though
/// the connection user is a superuser. Applies pending migrations first.
pub async fn test_db() -> Option<Db> {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            eprintln!("DATABASE_URL not set — skipping DB test");
            return None;
        }
    };
    // Migrate as the owner (no runtime role), then build the RLS-enforcing
    // handle for the test itself.
    let owner = Db::connect(&url, None).await.expect("connect failed");
    owner.migrate().await.expect("migrations failed");
    Some(
        Db::connect(&url, Some(ROLE_API))
            .await
            .expect("connect failed"),
    )
}
