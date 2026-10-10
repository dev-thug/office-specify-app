//! Repositories for recipient-bound, immutable outbound engagement.

use contracts::AppError;
use uuid::Uuid;

use crate::db::{Db, Tx};
use crate::error::map_sqlx;

#[derive(Debug, Clone, Copy)]
pub struct CapabilityLocation {
    pub workspace_id: Uuid,
    pub capability_id: Uuid,
}

pub struct RecordedCapability {
    pub target_url: Option<String>,
    pub tracking_active: bool,
}

pub struct EngagementCounts {
    pub tracking_enabled: bool,
    pub open_request_count: i64,
    pub click_request_count: i64,
}

/// Workspace opt-out is a hard gate. Missing settings preserve the default-on
/// workspace behavior; a malformed non-boolean value fails closed.
pub async fn tracking_enabled(tx: &mut Tx<'_>) -> Result<bool, AppError> {
    sqlx::query_scalar(
        "SELECT COALESCE(settings -> 'tracking_enabled' = 'true'::jsonb, true)
           FROM workspaces WHERE id = $1",
    )
    .bind(tx.workspace_id())
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Store only a one-way token hash. The caller never logs the raw token.
pub async fn insert_capability(
    tx: &mut Tx<'_>,
    outbound_message_id: Uuid,
    recipient_email_address_id: Uuid,
    capability_type: &str,
    token: &str,
    redirect_target: Option<&str>,
) -> Result<(), AppError> {
    let token_hash = crate::crypto::sha256_bytes(token.as_bytes());
    sqlx::query(
        "INSERT INTO outbound_engagement_capabilities
            (workspace_id, outbound_message_id, recipient_email_address_id,
             capability_type, token_hash, redirect_target)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(tx.workspace_id())
    .bind(outbound_message_id)
    .bind(recipient_email_address_id)
    .bind(capability_type)
    .bind(token_hash.to_vec())
    .bind(redirect_target)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

/// Resolve a public capability with a hash-only SECURITY DEFINER lookup.
/// The function returns no target or recipient data before a workspace tx is
/// opened under RLS.
pub async fn resolve_token(db: &Db, token: &str) -> Result<Option<CapabilityLocation>, AppError> {
    if token.len() != 43
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Ok(None);
    }
    let token_hash = crate::crypto::sha256_bytes(token.as_bytes());
    let mut tx = db.begin_global().await?;
    let location = sqlx::query_as::<_, (Uuid, Uuid)>(
        "SELECT workspace_id, capability_id
           FROM resolve_outbound_engagement_capability($1)",
    )
    .bind(token_hash.to_vec())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?
    .map(|(workspace_id, capability_id)| CapabilityLocation {
        workspace_id,
        capability_id,
    });
    tx.commit().await?;
    Ok(location)
}

/// Store a request only for the expected active capability kind. The row is
/// an aggregate counter, capped at 1,000 requests per UTC day per token.
pub async fn record_request(
    tx: &mut Tx<'_>,
    capability_id: Uuid,
    expected_type: &str,
) -> Result<Option<RecordedCapability>, AppError> {
    let capability = sqlx::query_as::<_, (String, Option<String>, bool)>(
        "SELECT capability_type, redirect_target, expires_at > now()
           FROM outbound_engagement_capabilities
          WHERE workspace_id = $1 AND id = $2 AND revoked_at IS NULL",
    )
    .bind(tx.workspace_id())
    .bind(capability_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let Some((capability_type, target_url, tracking_active)) = capability else {
        return Ok(None);
    };
    if capability_type != expected_type {
        return Ok(None);
    }

    if tracking_active {
        sqlx::query(
            "INSERT INTO outbound_engagement_request_buckets
                (workspace_id, capability_id, request_day, request_count, updated_at)
             VALUES ($1, $2, (timezone('UTC', now()))::date, 1, now())
             ON CONFLICT (workspace_id, capability_id, request_day)
             DO UPDATE SET request_count = LEAST(
                                outbound_engagement_request_buckets.request_count + 1, 1000
                            ),
                           updated_at = now()
             WHERE outbound_engagement_request_buckets.request_count < 1000",
        )
        .bind(tx.workspace_id())
        .bind(capability_id)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    }

    Ok(Some(RecordedCapability {
        target_url,
        tracking_active,
    }))
}

/// Read request counters only when the principal can read the outbound's
/// mailbox. Inaccessible and nonexistent outbounds have the same result.
pub async fn outbound_counts(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    outbound_message_id: Uuid,
) -> Result<Option<EngagementCounts>, AppError> {
    sqlx::query_as::<_, (bool, i64, i64)>(
        "SELECT
            EXISTS (
                SELECT 1 FROM outbound_engagement_capabilities c
                 WHERE c.workspace_id = o.workspace_id
                   AND c.outbound_message_id = o.id
                   AND c.revoked_at IS NULL
            ) AS tracking_enabled,
            COALESCE((
                SELECT sum(b.request_count)::bigint
                  FROM outbound_engagement_capabilities c
                  JOIN outbound_engagement_request_buckets b
                    ON b.workspace_id = c.workspace_id AND b.capability_id = c.id
                 WHERE c.workspace_id = o.workspace_id
                   AND c.outbound_message_id = o.id
                   AND c.capability_type = 'open'
            ), 0) AS open_request_count,
            COALESCE((
                SELECT sum(b.request_count)::bigint
                  FROM outbound_engagement_capabilities c
                  JOIN outbound_engagement_request_buckets b
                    ON b.workspace_id = c.workspace_id AND b.capability_id = c.id
                 WHERE c.workspace_id = o.workspace_id
                   AND c.outbound_message_id = o.id
                   AND c.capability_type = 'click'
            ), 0) AS click_request_count
           FROM outbound_messages o
          WHERE o.workspace_id = $1 AND o.id = $2
            AND EXISTS (
                SELECT 1 FROM mail_account_grants g
                 WHERE g.workspace_id = o.workspace_id
                   AND g.mail_account_id = o.mail_account_id
                   AND g.principal_id = $3 AND g.can_read
            )",
    )
    .bind(tx.workspace_id())
    .bind(outbound_message_id)
    .bind(principal_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
    .map(|row| {
        row.map(
            |(tracking_enabled, open_request_count, click_request_count)| EngagementCounts {
                tracking_enabled,
                open_request_count,
                click_request_count,
            },
        )
    })
}
