//! Idempotency records (table `idempotency_records`, §4.4, B.5).
//!
//! `run` is the canonical helper for POST semantics: the first caller holds
//! the `(workspace, scope_key, endpoint, idempotency_key)` unique claim and
//! executes `f` in a business tx; concurrent callers with the same key and a
//! matching `request_hash` observe `processing` and poll briefly (bounded —
//! long handlers keep claim status instead of stacking followers); the same
//! key with a different `request_hash` is `409 IDEMPOTENCY_KEY_REUSED`.
//!
//! The claim row commits before `f` runs, so a crash leaves a stale
//! `processing` row until `expires_at` — intentional (safe) per §4.4.

use std::future::Future;
use std::time::Duration;

use chrono::Utc;
use contracts::{AppError, ErrorCode};
use serde::{de::DeserializeOwned, Serialize};
use uuid::Uuid;

use crate::crypto;
use crate::db::{Db, Tx};
use crate::error::map_sqlx;

pub const RETENTION: Duration = Duration::from_secs(72 * 3600);
pub const KEY_MAX_LEN: usize = 128;
/// How long followers poll a `processing` claim before 409 REQUEST_IN_PROGRESS.
pub const POLL_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Identity of one idempotent call. `endpoint` must be the canonical route
/// pattern (e.g. `POST /api/v1/imports`), never a raw URI containing tokens.
#[derive(Debug, Clone)]
pub struct Scope {
    pub workspace_id: Uuid,
    /// Principal id or capability-token hash identifying the actor.
    pub principal_or_capability: String,
    /// Canonical "METHOD /route/pattern".
    pub endpoint: String,
    /// Client-supplied key; ≤128 chars (B.5).
    pub key: String,
}

impl Scope {
    pub fn scope_key(&self) -> String {
        crypto::sha256_hex(self.principal_or_capability.as_bytes())
    }
}

/// Response stored for replay: status + JSON body.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct Stored<T> {
    pub status: u16,
    pub body: T,
}

pub enum Outcome<T> {
    /// `f` ran and its result was persisted.
    Fresh(Stored<T>),
    /// A prior identical call's stored result.
    Replayed(Stored<T>),
}

impl<T> Outcome<T> {
    pub fn into_inner(self) -> Stored<T> {
        match self {
            Self::Fresh(s) | Self::Replayed(s) => s,
        }
    }
    pub fn is_replay(&self) -> bool {
        matches!(self, Self::Replayed(_))
    }
}

enum Claim {
    Leader,
    Existing {
        request_hash: Vec<u8>,
        state: String,
        response_status: Option<i32>,
        response_body: Option<serde_json::Value>,
    },
}

async fn try_claim(db: &Db, scope: &Scope, request_hash: &[u8]) -> Result<Claim, AppError> {
    // Claim or observe — separate short tx so the claim is visible to
    // followers before the business tx starts.
    let mut tx = db.begin(scope.workspace_id).await?;
    let claimed: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO idempotency_records
            (workspace_id, scope_key, endpoint, idempotency_key, request_hash, state, expires_at)
         VALUES ($1, $2, $3, $4, $5, 'processing', $6)
         ON CONFLICT (workspace_id, scope_key, endpoint, idempotency_key) DO NOTHING
         RETURNING id",
    )
    .bind(scope.workspace_id)
    .bind(scope.scope_key())
    .bind(&scope.endpoint)
    .bind(&scope.key)
    .bind(request_hash)
    .bind(Utc::now() + chrono::Duration::from_std(RETENTION).unwrap())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if claimed.is_some() {
        tx.commit().await?;
        return Ok(Claim::Leader);
    }
    let row: Option<(Vec<u8>, String, Option<i32>, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT request_hash, state, response_status, response_body FROM idempotency_records
          WHERE workspace_id = $1 AND scope_key = $2 AND endpoint = $3 AND idempotency_key = $4",
    )
    .bind(scope.workspace_id)
    .bind(scope.scope_key())
    .bind(&scope.endpoint)
    .bind(&scope.key)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let _ = tx.rollback().await;
    match row {
        Some((request_hash, state, response_status, response_body)) => Ok(Claim::Existing {
            request_hash,
            state,
            response_status,
            response_body,
        }),
        // Row existed for conflict but vanished (expired cleanup) — retry as leader.
        None => Box::pin(try_claim(db, scope, request_hash)).await,
    }
}

/// Run `f` at most once per `scope`. `request_hash` must cover the full
/// semantic request (body + relevant headers) — `sha256` of canonical JSON.
///
/// `f` receives an open workspace tx; it MUST perform all business writes,
/// audit and outbox emits in that tx. `run` commits it and stores the
/// serialized response in the same record.
pub async fn run<F, Fut, T>(
    db: &Db,
    scope: &Scope,
    request_hash: &[u8],
    f: F,
) -> Result<Outcome<T>, AppError>
where
    F: FnOnce(&mut Tx<'_>) -> Fut,
    Fut: Future<Output = Result<Stored<T>, AppError>>,
    T: Serialize + DeserializeOwned,
{
    if scope.key.is_empty() || scope.key.len() > KEY_MAX_LEN {
        return Err(AppError::new(
            ErrorCode::ValidationFailed,
            "Idempotency-Key must be 1..=128 characters",
        )
        .with_status(400));
    }
    let deadline = std::time::Instant::now() + POLL_TIMEOUT;
    loop {
        match try_claim(db, scope, request_hash).await? {
            Claim::Leader => {
                let mut tx = db.begin(scope.workspace_id).await?;
                match f(&mut tx).await {
                    Ok(stored) => {
                        let body = serde_json::to_value(&stored.body)
                            .map_err(|e| AppError::internal(format!("idempotency serialize: {e}")))?;
                        sqlx::query(
                            "UPDATE idempotency_records
                                SET state = 'completed', response_status = $5, response_body = $6
                              WHERE workspace_id = $1 AND scope_key = $2
                                AND endpoint = $3 AND idempotency_key = $4",
                        )
                        .bind(scope.workspace_id)
                        .bind(scope.scope_key())
                        .bind(&scope.endpoint)
                        .bind(&scope.key)
                        .bind(i32::from(stored.status))
                        .bind(&body)
                        .execute(tx.conn())
                        .await
                        .map_err(map_sqlx)?;
                        tx.commit().await?;
                        return Ok(Outcome::Fresh(stored));
                    }
                    Err(e) => {
                        let _ = tx.rollback().await;
                        // Release the claim so a retry may proceed.
                        let mut cleanup = db.begin(scope.workspace_id).await?;
                        let _ = sqlx::query(
                            "DELETE FROM idempotency_records
                              WHERE workspace_id = $1 AND scope_key = $2
                                AND endpoint = $3 AND idempotency_key = $4
                                AND state = 'processing'",
                        )
                        .bind(scope.workspace_id)
                        .bind(scope.scope_key())
                        .bind(&scope.endpoint)
                        .bind(&scope.key)
                        .execute(cleanup.conn())
                        .await;
                        let _ = cleanup.commit().await;
                        return Err(e);
                    }
                }
            }
            Claim::Existing {
                request_hash: stored_hash,
                state,
                response_status,
                response_body,
            } => {
                if stored_hash != request_hash {
                    return Err(AppError::new(
                        ErrorCode::IdempotencyKeyReused,
                        "Idempotency-Key was reused with a different request",
                    ));
                }
                if state == "completed" {
                    let body: T = serde_json::from_value(
                        response_body.unwrap_or(serde_json::Value::Null),
                    )
                    .map_err(|e| {
                        AppError::internal(format!("idempotency deserialize: {e}"))
                    })?;
                    return Ok(Outcome::Replayed(Stored {
                        status: response_status.unwrap_or(200) as u16,
                        body,
                    }));
                }
                if std::time::Instant::now() >= deadline {
                    return Err(AppError::new(
                        ErrorCode::RequestInProgress,
                        "identical request is still in progress",
                    ));
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    }
}
