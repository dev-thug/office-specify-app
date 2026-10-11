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
use crate::outbox;

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
    outbox::emit(
        tx,
        "approval.requested",
        "approval_request",
        id,
        1,
        serde_json::json!({
            "approval_request_id": id,
            "action_type": target.action_type(),
            "target_id": target.id(),
            "status": "pending",
        }),
    )
    .await?;
    Ok(id)
}

// ------------------------------------------------------------------------
// approvals module functions (owned by this file alongside `insert_request`)
// ------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ApprovalRequestRow {
    pub id: Uuid,
    pub requested_by: Uuid,
    pub launch_id: Option<Uuid>,
    pub outbound_message_id: Option<Uuid>,
    pub booking_change_id: Option<Uuid>,
    pub suppression_id: Option<Uuid>,
    pub action_type: String,
    pub snapshot_hash: Vec<u8>,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

impl ApprovalRequestRow {
    /// The single concrete target FK (DDL CHECK guarantees exactly one).
    pub fn target_id(&self) -> Uuid {
        self.launch_id
            .or(self.outbound_message_id)
            .or(self.booking_change_id)
            .or(self.suppression_id)
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ApprovalDecisionRow {
    pub id: Uuid,
    pub approval_request_id: Uuid,
    pub decided_by: Uuid,
    pub decision: String,
    pub reason: Option<String>,
    pub version_at_decision: i64,
    pub decided_at: DateTime<Utc>,
}

const REQUEST_COLS: &str = "id, requested_by, launch_id, outbound_message_id, booking_change_id,
     suppression_id, action_type, snapshot_hash, status, expires_at, version, created_at";

pub async fn get_request(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<ApprovalRequestRow>, AppError> {
    sqlx::query_as::<_, ApprovalRequestRow>(&format!(
        "SELECT {REQUEST_COLS} FROM approval_requests
          WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Lock the request row (A.6.3 — the decision transaction serializes on it).
pub async fn get_request_for_update(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<ApprovalRequestRow>, AppError> {
    sqlx::query_as::<_, ApprovalRequestRow>(&format!(
        "SELECT {REQUEST_COLS} FROM approval_requests
          WHERE workspace_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// The currently live (pending/approved) request for a launch — the
/// `approval_launch_active_uq` partial index makes this at most one row.
pub async fn active_request_for_launch(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
) -> Result<Option<ApprovalRequestRow>, AppError> {
    sqlx::query_as::<_, ApprovalRequestRow>(&format!(
        "SELECT {REQUEST_COLS} FROM approval_requests
          WHERE workspace_id = $1 AND launch_id = $2
            AND status IN ('pending','approved')
          ORDER BY created_at DESC LIMIT 1"
    ))
    .bind(tx.workspace_id())
    .bind(launch_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Request list. `see_all` (admin/manager) lists the workspace's requests;
/// otherwise callers only see their own (B.2 "승인자·본인 요청").
/// Keyset pagination on (created_at DESC, id DESC).
pub async fn list_requests(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    see_all: bool,
    status: Option<&str>,
    action_type: Option<&str>,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<ApprovalRequestRow>, AppError> {
    sqlx::query_as::<_, ApprovalRequestRow>(&format!(
        "SELECT {REQUEST_COLS} FROM approval_requests
          WHERE workspace_id = $1
            AND ($2::bool OR requested_by = $3)
            AND ($4::text IS NULL OR status = $4)
            AND ($5::text IS NULL OR action_type = $5)
            AND ($6::timestamptz IS NULL OR (created_at, id) < ($6, $7))
          ORDER BY created_at DESC, id DESC
          LIMIT $8"
    ))
    .bind(tx.workspace_id())
    .bind(see_all)
    .bind(principal_id)
    .bind(status)
    .bind(action_type)
    .bind(after.map(|(t, _)| t))
    .bind(after.map(|(_, i)| i))
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Insert the decision row. The `approval_decision_guard` trigger
/// re-validates pending/not-expired/version/snapshot/approver-role and the
/// active-human admin/manager rule inside this tx (T09 backstop — the
/// application performs the same checks first for precise error codes).
pub async fn insert_decision(
    tx: &mut Tx<'_>,
    approval_request_id: Uuid,
    decided_by: Uuid,
    decision: &str,
    snapshot_hash: &[u8],
    reason: Option<&str>,
    version_at_decision: i64,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO approval_decisions
            (workspace_id, approval_request_id, decided_by, decision,
             snapshot_hash, reason, version_at_decision)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(approval_request_id)
    .bind(decided_by)
    .bind(decision)
    .bind(snapshot_hash)
    .bind(reason)
    .bind(version_at_decision)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// pending → approved/rejected (or → revoked/expired) with a version bump.
/// Target/snapshot/requester are immutable by trigger — only status and
/// version change here.
pub async fn set_request_status(
    tx: &mut Tx<'_>,
    id: Uuid,
    status: &str,
    expected_version: i64,
) -> Result<u64, AppError> {
    let n = sqlx::query(
        "UPDATE approval_requests
            SET status = $3, version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND version = $4",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(status)
    .bind(expected_version)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n)
}

pub async fn get_decision(
    tx: &mut Tx<'_>,
    approval_request_id: Uuid,
) -> Result<Option<ApprovalDecisionRow>, AppError> {
    sqlx::query_as::<_, ApprovalDecisionRow>(
        "SELECT id, approval_request_id, decided_by, decision, reason,
                version_at_decision, decided_at
           FROM approval_decisions
          WHERE workspace_id = $1 AND approval_request_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(approval_request_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ------------------------------------------------- target-side state reads

/// campaign_launch target state for snapshot re-verification + mail grant:
/// (status, manifest_hash, expires_at, mail_account_id via the revision,
/// delivery_channel_id).
pub async fn launch_state(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
) -> Result<Option<(String, Option<Vec<u8>>, DateTime<Utc>, Uuid, Uuid)>, AppError> {
    sqlx::query_as(
        "SELECT l.status, l.manifest_hash, l.expires_at, r.mail_account_id,
                l.delivery_channel_id
           FROM campaign_launches l
           JOIN campaign_revisions r ON r.workspace_id = l.workspace_id
                                    AND r.id = l.revision_id
          WHERE l.workspace_id = $1 AND l.id = $2",
    )
    .bind(tx.workspace_id())
    .bind(launch_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// (enabled, policy_status) for a delivery channel — decision-time policy
/// re-verification (A.6.3 "채널 … 재검사").
pub async fn channel_policy_state(
    tx: &mut Tx<'_>,
    delivery_channel_id: Uuid,
) -> Result<Option<(bool, String)>, AppError> {
    sqlx::query_as(
        "SELECT enabled, policy_status
           FROM delivery_channels
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(delivery_channel_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Guarded launch transition used by decision/revoke side effects.
pub async fn set_launch_status(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
    to: &str,
    from_statuses: &[&str],
) -> Result<u64, AppError> {
    let n = sqlx::query(
        "UPDATE campaign_launches
            SET status = $3, version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND status = ANY($4)",
    )
    .bind(tx.workspace_id())
    .bind(launch_id)
    .bind(to)
    .bind(from_statuses)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n)
}

/// message_submit target state: (status, payload_hash, mail_account_id,
/// delivery_channel_id).
pub async fn outbound_state(
    tx: &mut Tx<'_>,
    outbound_message_id: Uuid,
) -> Result<Option<(String, Vec<u8>, Uuid, Uuid)>, AppError> {
    sqlx::query_as(
        "SELECT status, payload_hash, mail_account_id, delivery_channel_id
           FROM outbound_messages
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(outbound_message_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// draft → queued on approval (integration contract decision side effect).
/// Only `status`/`version` change — the snapshot trigger permits them.
pub async fn set_outbound_status(
    tx: &mut Tx<'_>,
    outbound_message_id: Uuid,
    to: &str,
    from_statuses: &[&str],
) -> Result<u64, AppError> {
    let n = sqlx::query(
        "UPDATE outbound_messages
            SET status = $3, version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND status = ANY($4)",
    )
    .bind(tx.workspace_id())
    .bind(outbound_message_id)
    .bind(to)
    .bind(from_statuses)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n)
}

/// booking_change target status.
pub async fn booking_change_status(
    tx: &mut Tx<'_>,
    booking_change_id: Uuid,
) -> Result<Option<String>, AppError> {
    sqlx::query_scalar(
        "SELECT status FROM booking_changes
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(booking_change_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// suppression_release target state: `active` | `released` | missing.
pub async fn suppression_status(
    tx: &mut Tx<'_>,
    suppression_id: Uuid,
) -> Result<Option<String>, AppError> {
    let row: Option<(Option<DateTime<Utc>>,)> = sqlx::query_as(
        "SELECT released_at FROM suppressions
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(suppression_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(row.map(|(released,)| {
        if released.is_some() {
            "released".to_string()
        } else {
            "active".to_string()
        }
    }))
}

/// Recompute a target's current content hash for re-verification (A.6.3):
/// the launch `manifest_hash` or the outbound `payload_hash` — both stored
/// frozen by trigger. Booking-change / suppression-release requests pin
/// requester-defined snapshots that this module cannot recompute; their
/// re-verification is the target-status check instead (see decide()).
pub async fn target_current_hash(
    tx: &mut Tx<'_>,
    action_type: &str,
    target_id: Uuid,
) -> Result<Option<Vec<u8>>, AppError> {
    match action_type {
        "campaign_launch" => sqlx::query_scalar(
            "SELECT manifest_hash FROM campaign_launches
                  WHERE workspace_id = $1 AND id = $2",
        )
        .bind(tx.workspace_id())
        .bind(target_id)
        .fetch_optional(tx.conn())
        .await
        .map_err(map_sqlx)
        .map(|o: Option<Option<Vec<u8>>>| o.flatten()),
        "message_submit" => sqlx::query_scalar(
            "SELECT payload_hash FROM outbound_messages
                  WHERE workspace_id = $1 AND id = $2",
        )
        .bind(tx.workspace_id())
        .bind(target_id)
        .fetch_optional(tx.conn())
        .await
        .map_err(map_sqlx),
        "suppression_release" => {
            let row = super::compliance::get_suppression(tx, target_id).await?;
            Ok(row.map(|supp| {
                crate::crypto::sha256_bytes(
                    serde_json::json!({
                        "suppression_id": supp.id,
                        "version": supp.version,
                        "email_address_id": supp.email_address_id,
                        "purpose_scope": supp.purpose_scope,
                        "reason": supp.reason,
                    })
                    .to_string()
                    .as_bytes(),
                )
                .to_vec()
            }))
        }
        _ => Ok(None),
    }
}

/// Cancel a still-queued side-effect job by dedupe key (revoke path).
pub async fn cancel_queued_job(
    tx: &mut Tx<'_>,
    kind: &str,
    dedupe_key: &str,
) -> Result<u64, AppError> {
    let n = sqlx::query(
        "UPDATE jobs SET state = 'canceled', finished_at = now()
          WHERE workspace_id = $1 AND kind = $2 AND dedupe_key = $3
            AND state IN ('queued','retry_wait')",
    )
    .bind(tx.workspace_id())
    .bind(kind)
    .bind(dedupe_key)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n)
}

/// True when the side-effect job for this dedupe key already left the
/// queued state (executed or running — revoke is then too late).
pub async fn job_ran(tx: &mut Tx<'_>, kind: &str, dedupe_key: &str) -> Result<bool, AppError> {
    let n: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM jobs
          WHERE workspace_id = $1 AND kind = $2 AND dedupe_key = $3
            AND state NOT IN ('queued','retry_wait','canceled')",
    )
    .bind(tx.workspace_id())
    .bind(kind)
    .bind(dedupe_key)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(n.is_some())
}

/// Principal row status+role check inside the decision tx (A.6.3 — the
/// worker/auth path re-verifies the approver is still an active human).
pub async fn principal_kind_role_status(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
) -> Result<Option<(String, String, String)>, AppError> {
    sqlx::query_as(
        "SELECT kind, role, status FROM principals
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}
