//! Durable native Zoho Campaigns exports, recipient snapshots, and provider
//! operation journal. Provider writes are claimed here before any network
//! call and unknown results remain non-reclaimable.

use chrono::{DateTime, Utc};
use contracts::AppError;
use domain::marketing::{MarketingExportState, OperationClaim, OperationResolution};
use serde_json::Value;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;

const EXPORT_COLUMNS: &str = "id, workspace_id, launch_id, revision_id, step_id,
    delivery_channel_id, connection_id, source_manifest_hash, content_hash,
    audience_hash, export_hash, subject, body_text, body_html, sender_address,
    sender_name, field_map, provider_list_key, topic_id, campaign_key, state,
    version, prepared_by, created_at, expires_at, last_checked_at, report,
    recipient_count";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MarketingExportRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub launch_id: Uuid,
    pub revision_id: Uuid,
    pub step_id: Uuid,
    pub delivery_channel_id: Uuid,
    pub connection_id: Uuid,
    pub source_manifest_hash: Vec<u8>,
    pub content_hash: Vec<u8>,
    pub audience_hash: Vec<u8>,
    pub export_hash: Vec<u8>,
    pub subject: String,
    pub body_text: String,
    pub body_html: String,
    pub sender_address: String,
    pub sender_name: String,
    pub field_map: Value,
    pub provider_list_key: Option<String>,
    pub topic_id: Option<String>,
    pub campaign_key: Option<String>,
    pub state: String,
    pub version: i64,
    pub prepared_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub report: Value,
    pub recipient_count: i64,
}

#[derive(Debug, Clone)]
pub struct NewMarketingExport {
    pub launch_id: Uuid,
    pub revision_id: Uuid,
    pub step_id: Uuid,
    pub delivery_channel_id: Uuid,
    pub connection_id: Uuid,
    pub source_manifest_hash: Vec<u8>,
    pub content_hash: Vec<u8>,
    pub audience_hash: Vec<u8>,
    pub export_hash: Vec<u8>,
    pub subject: String,
    pub body_text: String,
    pub body_html: String,
    pub sender_address: String,
    pub sender_name: String,
    pub field_map: Value,
    pub prepared_by: Uuid,
    pub expires_at: DateTime<Utc>,
}

/// Execution-only provider/report columns. A missing field means preserve
/// the stored value; callers cannot clear a previously observed provider ID.
#[derive(Debug, Clone, Default)]
pub struct ExecutionMetadata {
    pub provider_list_key: Option<String>,
    pub topic_id: Option<String>,
    pub campaign_key: Option<String>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub report: Option<Value>,
    pub recipient_count: Option<i64>,
    /// Preserve only a pending SEND scan checkpoint during read-only refreshes.
    pub preserve_send_scan: bool,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MarketingExportRecipientRow {
    pub workspace_id: Uuid,
    pub export_id: Uuid,
    pub email_address_id: Uuid,
    pub outbound_message_id: Uuid,
    pub normalized_address: String,
    pub variables: Value,
    pub sync_status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewExportRecipient {
    pub email_address_id: Uuid,
    pub outbound_message_id: Uuid,
    pub normalized_address: String,
    pub variables: Value,
    pub sync_status: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MarketingLaunchValueRow {
    pub workspace_id: Uuid,
    pub launch_id: Uuid,
    pub email_address_id: Uuid,
    pub variables: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewLaunchValue {
    pub email_address_id: Uuid,
    pub variables: Value,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MarketingOperationRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub export_id: Uuid,
    pub kind: String,
    pub correlation_key: String,
    pub request_hash: Vec<u8>,
    pub status: String,
    pub provider_id: Option<String>,
    pub error_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MarketingExportReviewRow {
    pub workspace_id: Uuid,
    pub export_id: Uuid,
    pub approval_request_id: Uuid,
    pub approver_id: Uuid,
    pub export_hash: Vec<u8>,
    pub reviewed_at: DateTime<Utc>,
}

pub async fn insert_export(
    tx: &mut Tx<'_>,
    new: NewMarketingExport,
) -> Result<MarketingExportRow, AppError> {
    let sql = format!(
        "INSERT INTO marketing_exports
            (workspace_id, launch_id, revision_id, step_id, delivery_channel_id,
             connection_id, source_manifest_hash, content_hash, audience_hash,
             export_hash, subject, body_text, body_html, sender_address,
             sender_name, field_map, prepared_by, expires_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)
         RETURNING {EXPORT_COLUMNS}"
    );
    sqlx::query_as::<_, MarketingExportRow>(&sql)
        .bind(tx.workspace_id())
        .bind(new.launch_id)
        .bind(new.revision_id)
        .bind(new.step_id)
        .bind(new.delivery_channel_id)
        .bind(new.connection_id)
        .bind(new.source_manifest_hash)
        .bind(new.content_hash)
        .bind(new.audience_hash)
        .bind(new.export_hash)
        .bind(new.subject)
        .bind(new.body_text)
        .bind(new.body_html)
        .bind(new.sender_address)
        .bind(new.sender_name)
        .bind(new.field_map)
        .bind(new.prepared_by)
        .bind(new.expires_at)
        .fetch_one(tx.conn())
        .await
        .map_err(map_sqlx)
}

pub async fn get_export(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<MarketingExportRow>, AppError> {
    let sql = format!(
        "SELECT {EXPORT_COLUMNS} FROM marketing_exports
          WHERE workspace_id = $1 AND id = $2"
    );
    sqlx::query_as::<_, MarketingExportRow>(&sql)
        .bind(tx.workspace_id())
        .bind(id)
        .fetch_optional(tx.conn())
        .await
        .map_err(map_sqlx)
}

pub async fn get_export_for_update(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<MarketingExportRow>, AppError> {
    let sql = format!(
        "SELECT {EXPORT_COLUMNS} FROM marketing_exports
          WHERE workspace_id = $1 AND id = $2 FOR UPDATE"
    );
    sqlx::query_as::<_, MarketingExportRow>(&sql)
        .bind(tx.workspace_id())
        .bind(id)
        .fetch_optional(tx.conn())
        .await
        .map_err(map_sqlx)
}

pub async fn get_export_by_launch(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
) -> Result<Option<MarketingExportRow>, AppError> {
    let sql = format!(
        "SELECT {EXPORT_COLUMNS} FROM marketing_exports
          WHERE workspace_id = $1 AND launch_id = $2"
    );
    sqlx::query_as::<_, MarketingExportRow>(&sql)
        .bind(tx.workspace_id())
        .bind(launch_id)
        .fetch_optional(tx.conn())
        .await
        .map_err(map_sqlx)
}

pub async fn list_exports(
    tx: &mut Tx<'_>,
    limit: i64,
) -> Result<Vec<MarketingExportRow>, AppError> {
    let sql = format!(
        "SELECT {EXPORT_COLUMNS} FROM marketing_exports
          WHERE workspace_id = $1
          ORDER BY created_at DESC, id DESC LIMIT $2"
    );
    sqlx::query_as::<_, MarketingExportRow>(&sql)
        .bind(tx.workspace_id())
        .bind(limit.clamp(1, 100))
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)
}

/// Optimistic, domain-checked status transition. `false` means the row was
/// missing or the caller's expected version had gone stale.
pub async fn update_state(
    tx: &mut Tx<'_>,
    id: Uuid,
    expected_version: i64,
    next: MarketingExportState,
) -> Result<bool, AppError> {
    let Some(row) = get_export_for_update(tx, id).await? else {
        return Ok(false);
    };
    if row.version != expected_version {
        return Ok(false);
    }
    let Some(current) = MarketingExportState::from_str(&row.state) else {
        return Err(AppError::internal(
            "unknown marketing export state in database",
        ));
    };
    if current == next {
        return Ok(true);
    }
    if !current.can_transition(next) {
        return Err(AppError::conflict(format!(
            "invalid marketing export state transition: {} -> {}",
            current.as_str(),
            next.as_str()
        )));
    }
    let updated = sqlx::query(
        "UPDATE marketing_exports
            SET state = $3, version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND version = $4",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(next.as_str())
    .bind(expected_version)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(updated == 1)
}

pub async fn update_execution_metadata(
    tx: &mut Tx<'_>,
    id: Uuid,
    update: ExecutionMetadata,
) -> Result<bool, AppError> {
    let updated = sqlx::query(
        "UPDATE marketing_exports
            SET provider_list_key = COALESCE($3, provider_list_key),
                topic_id = COALESCE($4, topic_id),
                campaign_key = COALESCE($5, campaign_key),
                last_checked_at = COALESCE($6, last_checked_at),
                report = CASE WHEN $7::jsonb IS NULL THEN report
                              WHEN $9 AND state = 'ready' THEN
                                jsonb_strip_nulls(jsonb_build_object(
                                    'send_scan_epoch_ms', report->'send_scan_epoch_ms',
                                    'send_scan_more', report->'send_scan_more')) || $7
                              ELSE $7 END,
                recipient_count = COALESCE($8, recipient_count),
                version = version + 1
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(update.provider_list_key)
    .bind(update.topic_id)
    .bind(update.campaign_key)
    .bind(update.last_checked_at)
    .bind(update.report)
    .bind(update.recipient_count)
    .bind(update.preserve_send_scan)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(updated == 1)
}

/// Replace an exact recipient snapshot only before any provider operation
/// has been journaled. The database trigger independently enforces this
/// boundary and validates the outbound-to-launch binding.
pub async fn replace_recipients(
    tx: &mut Tx<'_>,
    export_id: Uuid,
    recipients: &[NewExportRecipient],
) -> Result<(), AppError> {
    let row = get_export_for_update(tx, export_id)
        .await?
        .ok_or_else(|| AppError::not_found("marketing export not found"))?;
    if row.state != MarketingExportState::Preparing.as_str() {
        return Err(AppError::conflict(
            "marketing export recipients can only be replaced while preparing",
        ));
    }
    let has_operations: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM marketing_operations
          WHERE workspace_id = $1 AND export_id = $2)",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if has_operations {
        return Err(AppError::conflict(
            "marketing export recipients cannot change after a provider operation",
        ));
    }
    sqlx::query(
        "DELETE FROM marketing_export_recipients
          WHERE workspace_id = $1 AND export_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    for recipient in recipients {
        sqlx::query(
            "INSERT INTO marketing_export_recipients
                (workspace_id, export_id, email_address_id, outbound_message_id,
                 normalized_address, variables, sync_status)
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(tx.workspace_id())
        .bind(export_id)
        .bind(recipient.email_address_id)
        .bind(recipient.outbound_message_id)
        .bind(&recipient.normalized_address)
        .bind(&recipient.variables)
        .bind(&recipient.sync_status)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn list_recipients(
    tx: &mut Tx<'_>,
    export_id: Uuid,
) -> Result<Vec<MarketingExportRecipientRow>, AppError> {
    sqlx::query_as::<_, MarketingExportRecipientRow>(
        "SELECT workspace_id, export_id, email_address_id, outbound_message_id,
                normalized_address, variables, sync_status, created_at, updated_at
           FROM marketing_export_recipients
          WHERE workspace_id = $1 AND export_id = $2
          ORDER BY normalized_address, email_address_id",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn set_recipient_status(
    tx: &mut Tx<'_>,
    export_id: Uuid,
    email_address_id: Uuid,
    status: &str,
) -> Result<bool, AppError> {
    let updated = sqlx::query(
        "UPDATE marketing_export_recipients
            SET sync_status = $4
          WHERE workspace_id = $1 AND export_id = $2 AND email_address_id = $3",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .bind(email_address_id)
    .bind(status)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(updated == 1)
}

/// Append launch values. The primary key and immutable trigger intentionally
/// reject duplicates instead of replacing a previously frozen value.
pub async fn record_launch_values(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
    values: &[NewLaunchValue],
) -> Result<(), AppError> {
    for value in values {
        sqlx::query(
            "INSERT INTO marketing_launch_values
                (workspace_id, launch_id, email_address_id, variables)
             VALUES ($1,$2,$3,$4)",
        )
        .bind(tx.workspace_id())
        .bind(launch_id)
        .bind(value.email_address_id)
        .bind(&value.variables)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn list_launch_values(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
) -> Result<Vec<MarketingLaunchValueRow>, AppError> {
    sqlx::query_as::<_, MarketingLaunchValueRow>(
        "SELECT workspace_id, launch_id, email_address_id, variables, created_at
           FROM marketing_launch_values
          WHERE workspace_id = $1 AND launch_id = $2
          ORDER BY email_address_id",
    )
    .bind(tx.workspace_id())
    .bind(launch_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_operation(
    tx: &mut Tx<'_>,
    export_id: Uuid,
    kind: &str,
    correlation_key: &str,
) -> Result<Option<MarketingOperationRow>, AppError> {
    sqlx::query_as::<_, MarketingOperationRow>(
        "SELECT id, workspace_id, export_id, kind, correlation_key, request_hash,
                status, provider_id, error_code, created_at, updated_at, completed_at
           FROM marketing_operations
          WHERE workspace_id = $1 AND export_id = $2
            AND kind = $3 AND correlation_key = $4",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .bind(kind)
    .bind(correlation_key)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn list_operations(
    tx: &mut Tx<'_>,
    export_id: Uuid,
) -> Result<Vec<MarketingOperationRow>, AppError> {
    sqlx::query_as::<_, MarketingOperationRow>(
        "SELECT id, workspace_id, export_id, kind, correlation_key, request_hash,
                status, provider_id, error_code, created_at, updated_at, completed_at
           FROM marketing_operations
          WHERE workspace_id = $1 AND export_id = $2
          ORDER BY created_at, id",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Short-lived, hash-bound operator evidence supplements omitted fields only.
/// Any later or unresolved contact mutation invalidates the observation.
pub async fn has_recent_field_evidence(
    tx: &mut Tx<'_>,
    export_id: Uuid,
    export_hash: &str,
    values_hash: &str,
) -> Result<bool, AppError> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM audit_logs a
          JOIN principals p ON p.workspace_id=a.workspace_id AND p.id=a.actor_principal_id
          WHERE a.workspace_id=$1 AND a.target_id=$2 AND a.action='marketing.export_fields_verified'
            AND p.kind='human' AND p.role='admin' AND p.status='active'
            AND a.details->>'export_hash'=$3 AND a.details->>'values_hash'=$4
            AND a.details->>'source'='operator_observation'
            AND a.created_at BETWEEN now()-interval '10 minutes' AND now()
            AND NOT EXISTS(SELECT 1 FROM marketing_operations o
              WHERE o.workspace_id=a.workspace_id AND o.export_id=$2
                AND o.kind IN ('subscribe','add_contacts')
                AND (o.status<>'completed' OR o.updated_at>a.created_at)))",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .bind(export_hash)
    .bind(values_hash)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// A transport replay must preserve the original observation timestamp.
pub async fn field_evidence_recorded(
    tx: &mut Tx<'_>,
    export_id: Uuid,
    actor_id: Uuid,
    key_hash: &str,
) -> Result<bool, AppError> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM audit_logs WHERE workspace_id=$1
        AND target_id=$2 AND actor_principal_id=$3 AND action='marketing.export_fields_verified'
        AND details->>'request_key_hash'=$4)",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .bind(actor_id)
    .bind(key_hash)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Record the immutable approval binding for a ready provider-bound export.
/// The database trigger rechecks the current approved decision, exact hash,
/// launch target, and independent active human approver in this transaction.
pub async fn record_export_review(
    tx: &mut Tx<'_>,
    export_id: Uuid,
    approval_request_id: Uuid,
    approver_id: Uuid,
    export_hash: &[u8],
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO marketing_export_reviews
            (workspace_id, export_id, approval_request_id, approver_id, export_hash)
         VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .bind(approval_request_id)
    .bind(approver_id)
    .bind(export_hash)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

pub async fn get_export_review(
    tx: &mut Tx<'_>,
    export_id: Uuid,
) -> Result<Option<MarketingExportReviewRow>, AppError> {
    sqlx::query_as::<_, MarketingExportReviewRow>(
        "SELECT workspace_id, export_id, approval_request_id, approver_id,
                export_hash, reviewed_at
           FROM marketing_export_reviews
          WHERE workspace_id = $1 AND export_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn claim_operation(
    tx: &mut Tx<'_>,
    export_id: Uuid,
    kind: &str,
    correlation_key: &str,
    request_hash: &[u8; 32],
) -> Result<OperationClaim, AppError> {
    let inserted = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO marketing_operations
            (workspace_id, export_id, kind, correlation_key, request_hash)
         VALUES ($1,$2,$3,$4,$5)
         ON CONFLICT (workspace_id, export_id, kind, correlation_key) DO NOTHING
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .bind(kind)
    .bind(correlation_key)
    .bind(&request_hash[..])
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if inserted.is_some() {
        return Ok(OperationClaim::New);
    }

    let row = sqlx::query_as::<_, MarketingOperationRow>(
        "SELECT id, workspace_id, export_id, kind, correlation_key, request_hash,
                status, provider_id, error_code, created_at, updated_at, completed_at
           FROM marketing_operations
          WHERE workspace_id = $1 AND export_id = $2
            AND kind = $3 AND correlation_key = $4
          FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .bind(kind)
    .bind(correlation_key)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?
    .ok_or_else(|| AppError::internal("marketing operation conflict row disappeared"))?;

    if row.request_hash.as_slice() != &request_hash[..] {
        return Err(AppError::conflict(
            "marketing operation correlation key was reused with a different request hash",
        ));
    }
    match row.status.as_str() {
        "started" => Ok(OperationClaim::InProgress),
        "completed" => Ok(OperationClaim::Completed {
            provider_id: row.provider_id,
        }),
        "unknown" => Ok(OperationClaim::Unknown),
        "rejected" => Ok(OperationClaim::Rejected {
            error_code: row.error_code,
        }),
        _ => Err(AppError::internal(
            "unknown marketing operation status in database",
        )),
    }
}

pub async fn complete_operation(
    tx: &mut Tx<'_>,
    export_id: Uuid,
    kind: &str,
    correlation_key: &str,
    provider_id: Option<&str>,
) -> Result<bool, AppError> {
    let updated = sqlx::query(
        "UPDATE marketing_operations
            SET status = 'completed', provider_id = $5,
                error_code = NULL, completed_at = now()
          WHERE workspace_id = $1 AND export_id = $2
            AND kind = $3 AND correlation_key = $4 AND status = 'started'",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .bind(kind)
    .bind(correlation_key)
    .bind(provider_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    if updated == 1 {
        return Ok(true);
    }
    Ok(get_operation(tx, export_id, kind, correlation_key)
        .await?
        .is_some_and(|row| {
            row.status == "completed"
                && (provider_id.is_none() || row.provider_id.as_deref() == provider_id)
        }))
}

/// Resolve an unknown journal row only from a caller that has completed a
/// read-only provider reconciliation. This method never creates a claim and
/// never changes an already-bound provider ID.
pub async fn reconcile_operation(
    tx: &mut Tx<'_>,
    export_id: Uuid,
    kind: &str,
    correlation_key: &str,
    provider_id: Option<&str>,
    resolved_status: OperationResolution,
    provider_code: Option<&str>,
) -> Result<bool, AppError> {
    let Some(current) = get_operation(tx, export_id, kind, correlation_key).await? else {
        return Ok(false);
    };
    let status = match resolved_status {
        OperationResolution::Completed => "completed",
        OperationResolution::Rejected => "rejected",
    };
    if current.status != "unknown" {
        return Ok(current.status == status
            && (provider_id.is_none() || current.provider_id.as_deref() == provider_id)
            && (resolved_status == OperationResolution::Completed
                || provider_code.is_none()
                || current.error_code.as_deref() == provider_code));
    }
    if current.provider_id.is_some()
        && provider_id.is_some()
        && current.provider_id.as_deref() != provider_id
    {
        return Err(AppError::conflict(
            "marketing operation reconciliation cannot replace a provider ID",
        ));
    }

    let updated = sqlx::query(
        "UPDATE marketing_operations
            SET status = $5,
                provider_id = COALESCE($6, provider_id),
                error_code = CASE WHEN $5 = 'completed' THEN NULL
                                  ELSE COALESCE($7, error_code) END,
                completed_at = now()
          WHERE workspace_id = $1 AND export_id = $2
            AND kind = $3 AND correlation_key = $4 AND status = 'unknown'",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .bind(kind)
    .bind(correlation_key)
    .bind(status)
    .bind(provider_id)
    .bind(provider_code)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(updated == 1)
}

pub async fn mark_operation_unknown(
    tx: &mut Tx<'_>,
    export_id: Uuid,
    kind: &str,
    correlation_key: &str,
    error_code: Option<&str>,
) -> Result<bool, AppError> {
    mark_operation_terminal(
        tx,
        export_id,
        kind,
        correlation_key,
        "unknown",
        None,
        error_code,
    )
    .await
}

pub async fn mark_operation_rejected(
    tx: &mut Tx<'_>,
    export_id: Uuid,
    kind: &str,
    correlation_key: &str,
    error_code: &str,
) -> Result<bool, AppError> {
    mark_operation_terminal(
        tx,
        export_id,
        kind,
        correlation_key,
        "rejected",
        None,
        Some(error_code),
    )
    .await
}

async fn mark_operation_terminal(
    tx: &mut Tx<'_>,
    export_id: Uuid,
    kind: &str,
    correlation_key: &str,
    status: &str,
    provider_id: Option<&str>,
    error_code: Option<&str>,
) -> Result<bool, AppError> {
    let updated = sqlx::query(
        "UPDATE marketing_operations
            SET status = $5, provider_id = $6, error_code = $7, completed_at = now()
          WHERE workspace_id = $1 AND export_id = $2
            AND kind = $3 AND correlation_key = $4 AND status = 'started'",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .bind(kind)
    .bind(correlation_key)
    .bind(status)
    .bind(provider_id)
    .bind(error_code)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(updated == 1)
}
