//! Transactional outbox (table `outbox_events`, spec §4.7, A.6.6).
//! `emit` runs inside the business transaction. Publishing is at-least-once;
//! consumers deduplicate by `(workspace_id, consumer, event_id)` via
//! `processed_events`.
//!
//! Signature note: the spec sketch is `emit(tx, event_type, aggregate_type,
//! aggregate_id, payload)`; `aggregate_version` is added because the DDL
//! column is NOT NULL and is the consumers' dedupe/ordering anchor.

use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::db::{Db, Tx};
use crate::error::map_sqlx;

/// Well-known event types (source §16.2).
pub mod event_types {
    pub const LEAD_ACCEPTED: &str = "lead.accepted";
    pub const APPROVAL_DECIDED: &str = "approval.decided";
    pub const MESSAGE_SUBMITTED: &str = "message.submitted";
    pub const MESSAGE_DELIVERY_OUTCOME: &str = "message.delivery_outcome";
    pub const MAILBOX_REAUTH_REQUIRED: &str = "mailbox.reauth_required";
    pub const PROVIDER_POLICY_VERIFIED: &str = "provider.policy_verified";
    pub const SUPPRESSION_CREATED: &str = "suppression.created";
    pub const SUPPRESSION_RELEASED: &str = "suppression.released";
    pub const JOB_FAILED: &str = "job.failed";
    pub const BOOKING_CONFIRMED: &str = "booking.confirmed";
}

/// Emit inside the business transaction. `aggregate_version` is the entity
/// version/manifest version after the business change. Returns the event id.
pub async fn emit(
    tx: &mut Tx<'_>,
    event_type: &str,
    aggregate_type: &str,
    aggregate_id: Uuid,
    aggregate_version: i64,
    payload: serde_json::Value,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO outbox_events
            (workspace_id, event_type, aggregate_type, aggregate_id, aggregate_version, payload)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(event_type)
    .bind(aggregate_type)
    .bind(aggregate_id)
    .bind(aggregate_version)
    .bind(&payload)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

#[derive(Debug, sqlx::FromRow)]
pub struct OutboxRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub event_type: String,
    pub aggregate_type: String,
    pub aggregate_id: Uuid,
    pub aggregate_version: i64,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Workspaces with unpublished events — dispatcher entry point. Goes
/// through the `pending_outbox_workspaces()` SECURITY DEFINER function
/// (migration 0004); rows are then claimed per workspace under RLS.
pub async fn pending_workspaces(db: &Db) -> Result<Vec<Uuid>, AppError> {
    sqlx::query_scalar("SELECT pending_outbox_workspaces()")
        .fetch_all(db.pool())
        .await
        .map_err(map_sqlx)
}

/// Claim unpublished events of one workspace inside its RLS transaction.
/// `FOR UPDATE SKIP LOCKED` makes concurrent dispatchers safe. `exclude`
/// holds event ids whose every subscriber is in active backoff (R17) so a
/// run of poison events cannot starve the batch.
pub async fn claim_unpublished(
    tx: &mut Tx<'_>,
    limit: i64,
    exclude: &[Uuid],
) -> Result<Vec<OutboxRow>, AppError> {
    sqlx::query_as::<_, OutboxRow>(
        "SELECT id, workspace_id, event_type, aggregate_type, aggregate_id,
                aggregate_version, payload, created_at
           FROM outbox_events
          WHERE workspace_id = $1 AND published_at IS NULL
            AND NOT (id = ANY($3))
          ORDER BY created_at, id
          LIMIT $2
          FOR UPDATE SKIP LOCKED",
    )
    .bind(tx.workspace_id())
    .bind(limit)
    .bind(exclude)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn mark_published(tx: &mut Tx<'_>, id: Uuid) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE outbox_events SET published_at = now()
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

/// Consumer-side dedupe (A.6.6): true the first time this consumer sees the
/// event in this workspace.
pub async fn processed_once(
    tx: &mut Tx<'_>,
    consumer: &str,
    event_id: Uuid,
) -> Result<bool, AppError> {
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO processed_events (workspace_id, consumer, event_id)
         VALUES ($1, $2, $3)
         ON CONFLICT (workspace_id, consumer, event_id) DO NOTHING
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(consumer)
    .bind(event_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(inserted.is_some())
}
