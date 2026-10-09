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
    /// True when the handler performs external side effects — reclaim never
    /// requeues these (R12; migration 0005).
    pub external_effect: bool,
    pub created_at: DateTime<Utc>,
}

/// Job kinds whose handlers perform external side effects (integration
/// contracts: `external_effect=true`). Durable and visible to SQL via the
/// `external_effect` column — lease-expiry reclaim must never blindly
/// re-submit them (R12). Kept here so `enqueue`'s frozen signature can set
/// the flag without a caller-supplied parameter.
pub fn kind_has_external_effect(kind: &str) -> bool {
    matches!(kind, "sending.dispatch_outbound" | "booking.confirm")
}

/// Enqueue inside the business transaction (R11). `dedupe_key = None`
/// generates a unique key ("once:<uuid>"); `Some(key)` makes the enqueue
/// idempotent on UNIQUE (workspace_id, kind, dedupe_key) — a duplicate
/// returns the existing job's id instead of failing.
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
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO jobs
            (workspace_id, kind, payload_version, payload, available_at, dedupe_key, external_effect)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (workspace_id, kind, dedupe_key) DO NOTHING
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(kind)
    .bind(payload_version)
    .bind(&payload)
    .bind(available_at)
    .bind(&key)
    .bind(kind_has_external_effect(kind))
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if let Some(id) = id {
        return Ok(id);
    }
    // Duplicate dedupe key — return the existing job's id.
    sqlx::query_scalar(
        "SELECT id FROM jobs WHERE workspace_id = $1 AND kind = $2 AND dedupe_key = $3",
    )
    .bind(tx.workspace_id())
    .bind(kind)
    .bind(&key)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Lease up to `limit` runnable jobs whose `kind` is in `kinds` —
/// A.6.7 verbatim. Runs inside a workspace tx (`Db::begin`). Rows at
/// MAX_ATTEMPTS are moved to `dead` first and never leased again
/// (R12 — the cap bounds every retry path, not just handler-error paths).
/// `kinds` is the runner's registered kind list (F1): jobs of any other
/// kind are left completely untouched — never claimed and never
/// dead-lettered by a runner that lacks the handler.
pub async fn claim(
    tx: &mut Tx<'_>,
    lease_owner: &str,
    limit: i64,
    kinds: &[&str],
) -> Result<Vec<Job>, AppError> {
    claim_lease(tx, lease_owner, limit, LEASE_SECONDS, kinds).await
}

/// `claim` with an explicit lease duration in seconds.
pub async fn claim_lease(
    tx: &mut Tx<'_>,
    lease_owner: &str,
    limit: i64,
    lease_seconds: i64,
    kinds: &[&str],
) -> Result<Vec<Job>, AppError> {
    let ws = tx
        .workspace_id()
        .ok_or_else(|| AppError::internal("jobs::claim requires a workspace tx"))?;
    // Attempt-cap enforcement lives here so crash/panic-reclaimed rows
    // cannot cycle forever (I3/I5 → R12). Kind-scoped like the claim (F1):
    // a runner must not dead-letter jobs it was never meant to run.
    sqlx::query(
        "UPDATE jobs SET state = 'dead', finished_at = now(),
                last_error_code = 'MAX_ATTEMPTS',
                lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
          WHERE workspace_id = $1 AND state IN ('queued','retry_wait')
            AND attempt_count >= $2 AND kind = ANY($3)",
    )
    .bind(ws)
    .bind(MAX_ATTEMPTS)
    .bind(kinds)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let rows = sqlx::query_as::<_, Job>(
        "WITH candidates AS (
            SELECT id FROM jobs
             WHERE workspace_id = $1
               AND state IN ('queued','retry_wait')
               AND available_at <= now()
               AND attempt_count < $4
               AND kind = ANY($6)
             ORDER BY available_at, id
             LIMIT $3
             FOR UPDATE SKIP LOCKED
        )
        UPDATE jobs AS j
           SET state = 'running',
               lease_owner = $2,
               lease_token = gen_random_uuid(),
               lease_expires_at = now() + make_interval(secs => $5),
               attempt_count = attempt_count + 1
          FROM candidates AS c
         WHERE j.workspace_id = $1 AND j.id = c.id
        RETURNING j.*",
    )
    .bind(ws)
    .bind(lease_owner)
    .bind(limit)
    .bind(MAX_ATTEMPTS)
    .bind(lease_seconds as f64)
    .bind(kinds)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(rows)
}

/// Release a claimed job back to `queued` without consuming the attempt
/// (F1): the claim's `attempt_count` increment is refunded and the lease
/// cleared, fenced by the lease token. Defense in depth — kind-scoped
/// claims mean a runner should never hold a job it has no handler for.
pub async fn release(tx: &mut Tx<'_>, job_id: Uuid, lease_token: Uuid) -> Result<bool, AppError> {
    let n = sqlx::query(
        "UPDATE jobs
            SET state = 'queued',
                attempt_count = GREATEST(attempt_count - 1, 0),
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

/// Extend the lease. False = fencing token stale (job reclaimed) — stop.
pub async fn heartbeat(tx: &mut Tx<'_>, job_id: Uuid, lease_token: Uuid) -> Result<bool, AppError> {
    heartbeat_lease(tx, job_id, lease_token, LEASE_SECONDS).await
}

/// `heartbeat` with an explicit lease duration in seconds (runner uses the
/// same duration it claimed with).
pub async fn heartbeat_lease(
    tx: &mut Tx<'_>,
    job_id: Uuid,
    lease_token: Uuid,
    lease_seconds: i64,
) -> Result<bool, AppError> {
    let n = sqlx::query(
        "UPDATE jobs
            SET lease_expires_at = now() + make_interval(secs => $4)
          WHERE workspace_id = $1 AND id = $2 AND lease_token = $3",
    )
    .bind(tx.workspace_id())
    .bind(job_id)
    .bind(lease_token)
    .bind(lease_seconds as f64)
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

/// Workspaces with runnable jobs of the runner's registered `kinds` —
/// the runner iterates these so every job executes under its own RLS
/// context. The scan goes through the `runnable_job_workspaces(kinds)`
/// SECURITY DEFINER function (migrations 0004/0007): FORCE RLS would
/// otherwise hide every row when no workspace GUC is set. Kinds this
/// runner cannot handle stay invisible to it (F1).
pub async fn runnable_workspaces(db: &Db, kinds: &[&str]) -> Result<Vec<Uuid>, AppError> {
    sqlx::query_scalar("SELECT runnable_job_workspaces($1)")
        .bind(kinds)
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
