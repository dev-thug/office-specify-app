//! Worker-side persistence for Campaigns suppression propagation. Provider
//! writes are journaled before I/O and terminal/unknown rows are never reset.

use chrono::{DateTime, Duration, Utc};
use contracts::AppError;
use domain::marketing::OperationClaim;
use uuid::Uuid;

use crate::{db::Tx, error::map_sqlx};

pub const SUPPRESSION_BACKFILL_JOB_KIND: &str = "marketing.backfill_suppressions";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ActiveSuppressionRow {
    pub id: Uuid,
    pub email_address_id: Uuid,
    pub purpose_scope: String,
    pub reason: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EmailAddressRow {
    pub address_original: String,
    pub normalized_key: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ActiveCampaignConnectionRow {
    pub id: Uuid,
    pub secret_ref: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SuppressionOperationRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub suppression_id: Uuid,
    pub email_address_id: Uuid,
    pub connection_id: Uuid,
    pub status: String,
    pub error_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SuppressionSyncStatusRow {
    pub connection_id: Uuid,
    pub connection_name: String,
    pub status: String,
    pub provider_code: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
}

pub async fn active_suppressions_for_email(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
) -> Result<Vec<ActiveSuppressionRow>, AppError> {
    sqlx::query_as::<_, ActiveSuppressionRow>(
        "SELECT id, email_address_id, purpose_scope, reason
           FROM suppressions
          WHERE workspace_id = $1 AND email_address_id = $2
            AND released_at IS NULL
            AND purpose_scope IN ('marketing','all')
          ORDER BY created_at, id",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn email_address(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
) -> Result<Option<EmailAddressRow>, AppError> {
    sqlx::query_as::<_, EmailAddressRow>(
        "SELECT address_original, normalized_key
           FROM email_addresses
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn find_email_address_id(
    tx: &mut Tx<'_>,
    normalized_key: &str,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM email_addresses
          WHERE workspace_id = $1 AND normalized_key = $2",
    )
    .bind(tx.workspace_id())
    .bind(normalized_key)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Active Campaigns connections used for propagation. The caller requests one
/// bounded page at a time; an extra row indicates that the configured cap was
/// exceeded and must not be silently ignored.
pub async fn list_active_campaign_connections(
    tx: &mut Tx<'_>,
    after: Option<Uuid>,
    limit: i64,
    only_connection_id: Option<Uuid>,
) -> Result<Vec<ActiveCampaignConnectionRow>, AppError> {
    sqlx::query_as::<_, ActiveCampaignConnectionRow>(
        "SELECT m.id, c.secret_ref
           FROM marketing_connections m
           JOIN integration_connections c
             ON c.workspace_id = m.workspace_id AND c.id = m.id
          WHERE m.workspace_id = $1
            AND c.provider = 'zoho_campaigns'
            AND c.status = 'active'
            AND ($2::uuid IS NULL OR m.id > $2)
            AND ($4::uuid IS NULL OR m.id = $4)
          ORDER BY m.id
          LIMIT $3",
    )
    .bind(tx.workspace_id())
    .bind(after)
    .bind(limit.clamp(1, 101))
    .bind(only_connection_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Provider status for a local suppression: include every journaled operation
/// plus active Campaigns connections with no operation yet. Released local
/// suppressions remain visible through their historical journal rows, but do
/// not appear pending on connections that never received a push.
pub async fn list_suppression_sync_statuses(
    tx: &mut Tx<'_>,
    suppression_id: Uuid,
) -> Result<Vec<SuppressionSyncStatusRow>, AppError> {
    sqlx::query_as::<_, SuppressionSyncStatusRow>(
        "SELECT m.id AS connection_id, m.name AS connection_name,
                COALESCE(o.status, 'pending') AS status,
                o.error_code AS provider_code, o.updated_at
           FROM suppressions s
           JOIN marketing_connections m
             ON m.workspace_id = s.workspace_id
           JOIN integration_connections c
             ON c.workspace_id = m.workspace_id AND c.id = m.id
            AND c.provider = 'zoho_campaigns'
           LEFT JOIN marketing_suppression_operations o
             ON o.workspace_id = s.workspace_id
            AND o.suppression_id = s.id
            AND o.connection_id = m.id
          WHERE s.workspace_id = $1 AND s.id = $2
            AND (o.id IS NOT NULL OR (c.status = 'active' AND s.released_at IS NULL))
          ORDER BY m.name, m.id",
    )
    .bind(tx.workspace_id())
    .bind(suppression_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// One bounded keyset page of active suppressed addresses for a newly
/// connected Campaigns account. No contact data is read or sent here.
pub async fn list_active_suppression_addresses_for_backfill(
    tx: &mut Tx<'_>,
    connection_id: Uuid,
    after_email_address_id: Option<Uuid>,
    limit: i64,
) -> Result<Vec<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT DISTINCT s.email_address_id
           FROM suppressions s
           JOIN marketing_connections m
             ON m.workspace_id = s.workspace_id AND m.id = $2
           JOIN integration_connections c
             ON c.workspace_id = m.workspace_id AND c.id = m.id
            AND c.provider = 'zoho_campaigns' AND c.status = 'active'
          WHERE s.workspace_id = $1 AND s.released_at IS NULL
            AND s.purpose_scope IN ('marketing','all')
            AND ($3::uuid IS NULL OR s.email_address_id > $3)
          ORDER BY s.email_address_id
          LIMIT $4",
    )
    .bind(tx.workspace_id())
    .bind(connection_id)
    .bind(after_email_address_id)
    .bind(limit.clamp(1, 501))
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Enqueue a DB-only backfill after a Campaigns OAuth connection succeeds.
/// A fresh run id permits reconnects of the same connection to backfill again;
/// each provider mutation is still guarded by the operation journal.
pub async fn enqueue_suppression_backfill_in_tx(
    tx: &mut Tx<'_>,
    connection_id: Uuid,
) -> Result<(), AppError> {
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM marketing_connections m
            JOIN integration_connections c
              ON c.workspace_id = m.workspace_id AND c.id = m.id
             AND c.provider = 'zoho_campaigns' AND c.status = 'active'
            WHERE m.workspace_id = $1 AND m.id = $2
        )",
    )
    .bind(tx.workspace_id())
    .bind(connection_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if !active {
        return Ok(());
    }
    let run_id = Uuid::new_v4();
    let key = format!("marketing-suppression-backfill:{connection_id}:{run_id}:start");
    crate::jobs::enqueue(
        tx,
        SUPPRESSION_BACKFILL_JOB_KIND,
        1,
        serde_json::json!({"connection_id": connection_id, "backfill_id": run_id}),
        Utc::now(),
        Some(&key),
    )
    .await?;
    Ok(())
}

/// Insert a non-resubmittable operation before provider I/O. An absent result
/// means the suppression is no longer active, the address does not match it,
/// or the Campaigns connection is not currently active.
pub async fn claim_suppression_operation(
    tx: &mut Tx<'_>,
    suppression_id: Uuid,
    email_address_id: Uuid,
    connection_id: Uuid,
) -> Result<Option<OperationClaim>, AppError> {
    let inserted = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO marketing_suppression_operations
            (workspace_id, suppression_id, email_address_id, connection_id)
         SELECT $1, s.id, s.email_address_id, $4
           FROM suppressions s
           JOIN marketing_connections m
             ON m.workspace_id = s.workspace_id
            AND m.id = $4
           JOIN integration_connections c
             ON c.workspace_id = m.workspace_id
            AND c.id = m.id
            AND c.provider = 'zoho_campaigns'
            AND c.status = 'active'
          WHERE s.workspace_id = $1
            AND s.id = $2
            AND s.email_address_id = $3
            AND s.released_at IS NULL
            AND s.purpose_scope IN ('marketing','all')
         ON CONFLICT (workspace_id, suppression_id, connection_id) DO NOTHING
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(suppression_id)
    .bind(email_address_id)
    .bind(connection_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if inserted.is_some() {
        return Ok(Some(OperationClaim::New));
    }

    let row = sqlx::query_as::<_, SuppressionOperationRow>(
        "SELECT id, workspace_id, suppression_id, email_address_id,
                connection_id, status, error_code, created_at, updated_at,
                completed_at
           FROM marketing_suppression_operations
          WHERE workspace_id = $1 AND suppression_id = $2 AND connection_id = $3
          FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(suppression_id)
    .bind(connection_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let Some(row) = row else {
        return Ok(None);
    };
    if row.email_address_id != email_address_id {
        return Err(AppError::conflict(
            "suppression propagation operation address changed",
        ));
    }
    let claim = match row.status.as_str() {
        "started" if row.updated_at <= Utc::now() - Duration::minutes(2) => {
            mark_suppression_operation_unknown(
                tx,
                suppression_id,
                connection_id,
                Some("stale_provider_outcome"),
            )
            .await?;
            OperationClaim::Unknown
        }
        "started" => OperationClaim::InProgress,
        "completed" => OperationClaim::Completed { provider_id: None },
        "unknown" => OperationClaim::Unknown,
        "rejected" => OperationClaim::Rejected {
            error_code: row.error_code,
        },
        _ => return Err(AppError::internal("unknown suppression operation status")),
    };
    Ok(Some(claim))
}

pub async fn complete_suppression_operation(
    tx: &mut Tx<'_>,
    suppression_id: Uuid,
    connection_id: Uuid,
) -> Result<bool, AppError> {
    update_terminal(tx, suppression_id, connection_id, "completed", None).await
}

pub async fn mark_suppression_operation_unknown(
    tx: &mut Tx<'_>,
    suppression_id: Uuid,
    connection_id: Uuid,
    error_code: Option<&str>,
) -> Result<bool, AppError> {
    update_terminal(tx, suppression_id, connection_id, "unknown", error_code).await
}

pub async fn reject_suppression_operation(
    tx: &mut Tx<'_>,
    suppression_id: Uuid,
    connection_id: Uuid,
    error_code: &str,
) -> Result<bool, AppError> {
    update_terminal(
        tx,
        suppression_id,
        connection_id,
        "rejected",
        Some(error_code),
    )
    .await
}

async fn update_terminal(
    tx: &mut Tx<'_>,
    suppression_id: Uuid,
    connection_id: Uuid,
    status: &str,
    error_code: Option<&str>,
) -> Result<bool, AppError> {
    let rows = sqlx::query(
        "UPDATE marketing_suppression_operations
            SET status = $4, error_code = $5, completed_at = now()
          WHERE workspace_id = $1 AND suppression_id = $2
            AND connection_id = $3 AND status = 'started'",
    )
    .bind(tx.workspace_id())
    .bind(suppression_id)
    .bind(connection_id)
    .bind(status)
    .bind(error_code)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(rows == 1)
}
