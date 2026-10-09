//! Durable PostgreSQL-backed job queue (table `jobs`, spec §4.7, A.6.7).
//!
//! - `enqueue` runs inside the business transaction; `dedupe_key` is required
//!   by the schema — pass `Some(key)` to coalesce equivalent work or `None`
//!   to generate a unique key.
//! - `claim` follows A.6.7 verbatim: SKIP LOCKED, batch 20, 60s lease,
//!   fencing token. `complete`/`fail` require the token so a slow worker
//!   cannot resurrect a job that was reclaimed.
//! - Workers must dedupe side effects by job id and dedupe key — lease
//!   liveness is bounded, not exclusive.
//! - `unknown` provider outcomes never auto-retry (§10.2, §16.4): handlers
//!   transition the business row to `unknown` and finish the job; retry is
//!   user-driven.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::db::{Db, Tx};
use crate::error::map_sqlx;

pub const BATCH_SIZE: i64 = 20;
pub const LEASE_SECONDS: i64 = 60;
pub const MAX_ATTEMPTS: i32 = 5;

/// Job `state` values per the A.4 CHECK constraint.
pub const STATE_QUEUED: &str = "queued";
pub const STATE_RUNNING: &str = "running";
pub const STATE_RETRY_WAIT: &str = "retry_wait";
pub const STATE_COMPLETED: &str = "completed";
pub const STATE_DEAD: &str = "dead";
pub const STATE_CANCELED: &str = "canceled";
/// Business-level "outcome unknown" — never retried implicitly.
pub const STATE_UNKNOWN: &str = "unknown";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Job {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub kind: String,
    pub payload_version: i32,
    pub payload: serde_json::Value,
    pub dedupe_key: String,
    pub requested_by: Option<Uuid>,
    pub source_credential_id: Option<Uuid>,
    pub state: String,
    pub available_at: DateTime<Utc>,
    pub lease_owner: Option<String>,
    pub lease_token: Option<Uuid>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub attempt_count: i32,
    pub last_error_code: Option<String>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Enqueue inside the business transaction. `dedupe_key = None` generates a
/// unique key ("once:<uuid>"); `Some(key)` makes a second equivalent enqueue
/// fail with Conflict (UNIQUE (workspace_id, kind, dedupe_key)).
pub async fn enqueue(
    tx: &mut Tx<'_>,
    kind: &str,
    payload_version: i32,
    payload: serde_json::Value,
    available_at: DateTime<Utc>,
    dedupe_key: Option<&str>,
) -> Result<Uuid, AppError> {
    let key = dedupe_key
        .map(str::to_string)
        .unwrap_or_else(|| format!("once:{}", Uuid::new_v4()));
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO jobs (workspace_id, kind, payload_version, payload, available_at, dedupe_key)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(kind)
    .bind(payload_version)
    .bind(&payload)
    .bind(available_at)
    .bind(&key)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(id)
}

/// Lease up to `limit` runnable jobs — A.6.7 verbatim. Runs inside a
/// workspace tx (`Db::begin`); the runner keeps it open while handling.
pub async fn claim(tx: &mut Tx<'_>, lease_owner: &str, limit: i64) -> Result<Vec<Job>, AppError> {
    let ws = tx
        .workspace_id()
        .ok_or_else(|| AppError::internal("jobs::claim requires a workspace tx"))?;
    let rows = sqlx::query_as::<_, Job>(
        "WITH candidates AS (
            SELECT id FROM jobs
             WHERE workspace_id = $1
               AND state IN ('queued','retry_wait')
               AND available_at <= now()
             ORDER BY available_at, id
             LIMIT $3
             FOR UPDATE SKIP LOCKED
        )
        UPDATE jobs AS j
           SET state = 'running',
               lease_owner = $2,
               lease_token = gen_random_uuid(),
               lease_expires_at = now() + interval '60 seconds',
               attempt_count = attempt_count + 1
          FROM candidates AS c
         WHERE j.workspace_id = $1 AND j.id = c.id
        RETURNING j.*",
    )
    .bind(ws)
    .bind(lease_owner)
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(rows)
}

/// Extend the lease. False = fencing token stale (job reclaimed) — stop.
pub async fn heartbeat(tx: &mut Tx<'_>, job_id: Uuid, lease_token: Uuid) -> Result<bool, AppError> {
    let n = sqlx::query(
        "UPDATE jobs
            SET lease_expires_at = now() + interval '60 seconds'
          WHERE workspace_id = $1 AND id = $2 AND lease_token = $3",
    )
    .bind(tx.workspace_id())
    .bind(job_id)
    .bind(lease_token)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

/// Fenced transition to `completed`. False = stale token.
pub async fn complete(tx: &mut Tx<'_>, job_id: Uuid, lease_token: Uuid) -> Result<bool, AppError> {
    let n = sqlx::query(
        "UPDATE jobs SET state = 'completed', finished_at = now(),
                lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
          WHERE workspace_id = $1 AND id = $2 AND lease_token = $3",
    )
    .bind(tx.workspace_id())
    .bind(job_id)
    .bind(lease_token)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

/// Bounded exponential backoff in seconds: base 30s, doubling, with ≤25%
/// jitter. Attempt 1 → ~30s, 2 → ~60s, 3 → ~120s, 4 → ~240s.
pub fn backoff_seconds(attempt: i32) -> f64 {
    let base = 30.0 * 2f64.powi(attempt.saturating_sub(1).clamp(0, 6));
    let jitter = rand::Rng::random_range(&mut rand::rng(), 0.0..base * 0.25);
    base + jitter
}

/// Fenced transition of a job to an explicit terminal/idle state
/// (`retry_wait`, `dead`, `unknown`, `canceled`) with optional re-schedule.
/// This is the single state-transition helper the runner and handlers share;
/// false = fencing token stale.
pub async fn transition(
    tx: &mut Tx<'_>,
    job_id: Uuid,
    lease_token: Uuid,
    new_state: &str,
    next_available_at: Option<DateTime<Utc>>,
    error_code: Option<&str>,
) -> Result<bool, AppError> {
    let terminal = matches!(new_state, STATE_DEAD | STATE_UNKNOWN | STATE_CANCELED);
    let n = sqlx::query(
        "UPDATE jobs
            SET state = $4,
                finished_at = CASE WHEN $5 THEN now() ELSE NULL END,
                available_at = COALESCE($6, available_at),
                last_error_code = COALESCE($7, last_error_code),
                lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
          WHERE workspace_id = $1 AND id = $2 AND lease_token = $3",
    )
    .bind(tx.workspace_id())
    .bind(job_id)
    .bind(lease_token)
    .bind(new_state)
    .bind(terminal)
    .bind(next_available_at)
    .bind(error_code)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

/// Standard retry-or-dead transition used by the runner on handler error.
pub async fn fail_or_retry(
    tx: &mut Tx<'_>,
    job: &Job,
    lease_token: Uuid,
    error_code: &str,
) -> Result<bool, AppError> {
    if job.attempt_count >= MAX_ATTEMPTS {
        transition(tx, job.id, lease_token, STATE_DEAD, None, Some(error_code)).await
    } else {
        let next = Utc::now()
            + chrono::Duration::milliseconds((backoff_seconds(job.attempt_count) * 1000.0) as i64);
        transition(
            tx,
            job.id,
            lease_token,
            STATE_RETRY_WAIT,
            Some(next),
            Some(error_code),
        )
        .await
    }
}

/// Workspaces with runnable jobs — the runner iterates these so every job
/// executes under its own RLS context. The scan itself goes through the
/// `runnable_job_workspaces()` SECURITY DEFINER function (migration 0004):
/// FORCE RLS would otherwise hide every row when no workspace GUC is set.
pub async fn runnable_workspaces(db: &Db) -> Result<Vec<Uuid>, AppError> {
    sqlx::query_scalar("SELECT runnable_job_workspaces()")
        .fetch_all(db.pool())
        .await
        .map_err(map_sqlx)
}

/// Crash recovery: expired leases return to `queued` (attempt_count already
/// incremented at claim time, so MAX_ATTEMPTS still bounds retries).
/// Definer function for the same reason as `runnable_workspaces`.
pub async fn reclaim_expired(db: &Db) -> Result<u64, AppError> {
    let n: i64 = sqlx::query_scalar("SELECT reclaim_expired_job_leases()")
        .fetch_one(db.pool())
        .await
        .map_err(map_sqlx)?;
    Ok(n as u64)
}

/// Worker-side context passed to handlers.
#[derive(Debug, Clone)]
pub struct JobCtx {
    pub job_id: Uuid,
    pub workspace_id: Uuid,
    /// Fencing token — required for `complete`/`transition`.
    pub lease_token: Uuid,
    pub attempt_count: i32,
    pub lease_owner: String,
}

/// A durable job handler (spec §4.7). Implementations must be version-
/// tolerant, idempotent by job id + dedupe key, and must never implicitly
/// retry `unknown` provider outcomes.
#[async_trait]
pub trait JobHandler: Send + Sync {
    /// Job kind string, e.g. "mailboxes.sync".
    fn kind(&self) -> &'static str;
    /// Whether the handler performs external side effects (provider calls).
    /// `external_effect = true` handlers are not auto-retried by the runner
    /// after the submitting boundary (§16.4).
    fn external_effect(&self) -> bool {
        false
    }
    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError>;
}
