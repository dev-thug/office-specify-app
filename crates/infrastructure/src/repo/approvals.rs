//! approvals module repository (spec §6 — owned by the module worker).
//!
//! `insert_request` is the cross-module contract from
//! docs/design/integration-contracts.md ("Approval request creation"):
//! requesting modules insert their own rows through it. Signature is
//! binding — other modules depend on it before this module is implemented.

use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::audit::{self, AuditEntry};
use crate::db::Tx;
use crate::error::map_sqlx;

/// Concrete approval target — maps to `action_type` + exactly one FK column
/// (DDL `approval_requests` CHECK at migration 0001).
#[derive(Debug, Clone, Copy)]
pub enum ApprovalTarget {
    CampaignLaunch(Uuid),
    OutboundMessage(Uuid),
    BookingChange(Uuid),
    SuppressionRelease(Uuid),
}

impl ApprovalTarget {
    pub fn action_type(self) -> &'static str {
        match self {
            Self::CampaignLaunch(_) => "campaign_launch",
            Self::OutboundMessage(_) => "message_submit",
            Self::BookingChange(_) => "booking_change",
            Self::SuppressionRelease(_) => "suppression_release",
        }
    }

    pub fn id(self) -> Uuid {
        match self {
            Self::CampaignLaunch(id)
            | Self::OutboundMessage(id)
            | Self::BookingChange(id)
            | Self::SuppressionRelease(id) => id,
        }
    }
}

/// Create a pending approval request and audit `approval.requested` in the
/// same tx. `snapshot_hash` pins the exact approved content (A.6.3) —
/// approval is invalid if the target changes afterwards.
///
/// The requester's free-text reason (B.3.8) belongs in the audit metadata;
/// the table has no column for it.
pub async fn insert_request(
    tx: &mut Tx<'_>,
    requested_by: Uuid,
    target: ApprovalTarget,
    snapshot_hash: &[u8; 32],
    expires_at: DateTime<Utc>,
) -> Result<Uuid, AppError> {
    let (launch_id, outbound_message_id, booking_change_id, suppression_id) = match target {
        ApprovalTarget::CampaignLaunch(id) => (Some(id), None, None, None),
        ApprovalTarget::OutboundMessage(id) => (None, Some(id), None, None),
        ApprovalTarget::BookingChange(id) => (None, None, Some(id), None),
        ApprovalTarget::SuppressionRelease(id) => (None, None, None, Some(id)),
    };
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO approval_requests
            (workspace_id, requested_by, launch_id, outbound_message_id,
             booking_change_id, suppression_id, action_type, snapshot_hash, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(requested_by)
    .bind(launch_id)
    .bind(outbound_message_id)
    .bind(booking_change_id)
    .bind(suppression_id)
    .bind(target.action_type())
    .bind(&snapshot_hash[..])
    .bind(expires_at)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;

    // No request-scoped request_id is available at this layer — the audit
    // correlates on target_id + actor instead.
    audit::record(
        tx,
        AuditEntry::new(requested_by, "approval.requested", "approval_request", "").with_target(id),
    )
    .await?;
    Ok(id)
}
