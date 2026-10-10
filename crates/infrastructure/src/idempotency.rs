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
//! `processing` row; `locked_until` (R15, migration 0005) bounds the wait —
//! a same-key same-hash request takes the claim over after 5 minutes, and
//! reads ignore `expires_at`-passed records entirely.

use std::future::Future;
use std::time::Duration;

use chrono::Utc;
use contracts::{AppError, ErrorCode};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::crypto;
use crate::db::{Db, Tx};
use crate::error::map_sqlx;

pub const RETENTION: Duration = Duration::from_secs(72 * 3600);
pub const KEY_MAX_LEN: usize = 128;
/// How long followers poll a `processing` claim before 409 REQUEST_IN_PROGRESS.
pub const POLL_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// R15: lease on `processing` claims. A crashed leader's key becomes
/// claimable by an identical request after this, not after `RETENTION`.
pub const PROCESSING_LEASE: Duration = Duration::from_secs(5 * 60);

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

/// Read an unexpired completed response for authorization-aware replay
/// preflights. Callers must still invoke `run` to verify the request hash and
/// receive the canonical replay result.
pub async fn completed_response(
    db: &Db,
    scope: &Scope,
    request_hash: &[u8],
) -> Result<Option<serde_json::Value>, AppError> {
    let mut tx = db.begin(scope.workspace_id).await?;
    let response = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT response_body FROM idempotency_records
          WHERE workspace_id = $1 AND scope_key = $2 AND endpoint = $3
            AND idempotency_key = $4 AND request_hash = $5
            AND state = 'completed' AND expires_at > now()",
    )
    .bind(scope.workspace_id)
    .bind(scope.scope_key())
    .bind(&scope.endpoint)
    .bind(&scope.key)
    .bind(request_hash)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    tx.commit().await?;
    Ok(response)
}

#[derive(Debug)]
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

type RecordRow = (
    Vec<u8>,
    String,
    Option<i32>,
    Option<serde_json::Value>,
    Option<chrono::DateTime<Utc>>,
    chrono::DateTime<Utc>,
);

enum Claim {
    /// We hold the claim; carries the lease deadline we wrote — used to
    /// fence the failure-cleanup so a takeover's row is never deleted.
    Leader(chrono::DateTime<Utc>),
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
    let my_lease = Utc::now() + chrono::Duration::from_std(PROCESSING_LEASE).unwrap();
    let claimed: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO idempotency_records
            (workspace_id, scope_key, endpoint, idempotency_key, request_hash,
             state, locked_until, expires_at)
         VALUES ($1, $2, $3, $4, $5, 'processing', $6, $7)
         ON CONFLICT (workspace_id, scope_key, endpoint, idempotency_key) DO NOTHING
         RETURNING id",
    )
    .bind(scope.workspace_id)
    .bind(scope.scope_key())
    .bind(&scope.endpoint)
    .bind(&scope.key)
    .bind(request_hash)
    .bind(my_lease)
    .bind(Utc::now() + chrono::Duration::from_std(RETENTION).unwrap())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if claimed.is_some() {
        tx.commit().await?;
        return Ok(Claim::Leader(my_lease));
    }
    let row: Option<RecordRow> = sqlx::query_as(
        "SELECT request_hash, state, response_status, response_body, locked_until, expires_at
           FROM idempotency_records
          WHERE workspace_id = $1 AND scope_key = $2 AND endpoint = $3 AND idempotency_key = $4",
    )
    .bind(scope.workspace_id)
    .bind(scope.scope_key())
    .bind(&scope.endpoint)
    .bind(&scope.key)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    match row {
        Some((stored_hash, state, response_status, response_body, locked_until, expires_at)) => {
            // R15: reads ignore records past expires_at — delete the corpse
            // and claim as leader.
            if expires_at <= Utc::now() {
                sqlx::query(
                    "DELETE FROM idempotency_records
                      WHERE workspace_id = $1 AND scope_key = $2 AND endpoint = $3
                        AND idempotency_key = $4 AND expires_at <= now()",
                )
                .bind(scope.workspace_id)
                .bind(scope.scope_key())
                .bind(&scope.endpoint)
                .bind(&scope.key)
                .execute(tx.conn())
                .await
                .map_err(map_sqlx)?;
                tx.commit().await?;
                return Box::pin(try_claim(db, scope, request_hash)).await;
            }
            // R15: an expired processing lease is taken over by an identical
            // request (same key + same request_hash). A different hash is
            // still IDEMPOTENCY_KEY_REUSED — decided by the caller.
            let lease_expired = locked_until.is_none_or(|l| l <= Utc::now());
            if state == "processing" && lease_expired && stored_hash == request_hash {
                let took = sqlx::query(
                    "UPDATE idempotency_records
                        SET locked_until = $5
                      WHERE workspace_id = $1 AND scope_key = $2 AND endpoint = $3
                        AND idempotency_key = $4 AND state = 'processing'
                        AND (locked_until IS NULL OR locked_until <= now())",
                )
                .bind(scope.workspace_id)
                .bind(scope.scope_key())
                .bind(&scope.endpoint)
                .bind(&scope.key)
                .bind(my_lease)
                .execute(tx.conn())
                .await
                .map_err(map_sqlx)?
                .rows_affected();
                if took == 1 {
                    tx.commit().await?;
                    return Ok(Claim::Leader(my_lease));
                }
                // Lost the takeover race — observe the winner instead.
                let _ = tx.rollback().await;
                return Ok(Claim::Existing {
                    request_hash: stored_hash,
                    state,
                    response_status,
                    response_body,
                });
            }
            tx.commit().await?;
            Ok(Claim::Existing {
                request_hash: stored_hash,
                state,
                response_status,
                response_body,
            })
        }
        // Row existed for conflict but vanished (expired cleanup) — retry as leader.
        None => {
            let _ = tx.rollback().await;
            Box::pin(try_claim(db, scope, request_hash)).await
        }
    }
}

/// Run `f` at most once per `scope`. `request_hash` must cover the full
/// semantic request (body + relevant headers) — `sha256` of canonical JSON.
///
/// `f` receives the open workspace tx by value and returns it with the
/// response. It MUST perform all business writes, audit and outbox emits in
/// that tx. `run` stores the serialized response and commits in the same tx.
/// Consuming the transaction gives the future a concrete borrow lifetime,
/// so its business writes remain atomic with the idempotency result.
pub async fn run<'db, F, Fut, T>(
    db: &'db Db,
    scope: &Scope,
    request_hash: &[u8],
    f: F,
) -> Result<Outcome<T>, AppError>
where
    F: FnOnce(Tx<'db>) -> Fut,
    Fut: Future<Output = Result<(Tx<'db>, Stored<T>), AppError>>,
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
            Claim::Leader(my_lease) => {
                let tx = db.begin(scope.workspace_id).await?;
                match f(tx).await {
                    Ok((mut tx, stored)) => {
                        let body = serde_json::to_value(&stored.body).map_err(|e| {
                            AppError::internal(format!("idempotency serialize: {e}"))
                        })?;
                        // A takeover may have replaced our lease while the
                        // business future was running. Do not commit its
                        // writes unless this leader still owns the claim.
                        let updated = sqlx::query(
                            "UPDATE idempotency_records
                                SET state = 'completed', locked_until = NULL,
                                    response_status = $5, response_body = $6
                              WHERE workspace_id = $1 AND scope_key = $2
                                AND endpoint = $3 AND idempotency_key = $4
                                AND state = 'processing' AND locked_until = $7",
                        )
                        .bind(scope.workspace_id)
                        .bind(scope.scope_key())
                        .bind(&scope.endpoint)
                        .bind(&scope.key)
                        .bind(i32::from(stored.status))
                        .bind(&body)
                        .bind(my_lease)
                        .execute(tx.conn())
                        .await
                        .map_err(map_sqlx)?
                        .rows_affected();
                        if updated != 1 {
                            let _ = tx.rollback().await;
                            return Err(AppError::new(
                                ErrorCode::RequestInProgress,
                                "idempotency claim lease was lost before commit",
                            ));
                        }
                        tx.commit().await?;
                        return Ok(Outcome::Fresh(stored));
                    }
                    Err(e) => {
                        // The owned tx was consumed by `f` and dropped when
                        // its future returned the error, rolling back its
                        // uncommitted business writes.
                        // Release the claim so a retry may proceed — fenced
                        // to OUR lease value so a takeover's claim is never
                        // deleted from under it (R15).
                        let mut cleanup = db.begin(scope.workspace_id).await?;
                        let _ = sqlx::query(
                            "DELETE FROM idempotency_records
                              WHERE workspace_id = $1 AND scope_key = $2
                                AND endpoint = $3 AND idempotency_key = $4
                                AND state = 'processing'
                                AND (locked_until IS NULL
                                     OR locked_until <= now()
                                     OR locked_until = $5)",
                        )
                        .bind(scope.workspace_id)
                        .bind(scope.scope_key())
                        .bind(&scope.endpoint)
                        .bind(&scope.key)
                        .bind(my_lease)
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
                    let body: T =
                        serde_json::from_value(response_body.unwrap_or(serde_json::Value::Null))
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
