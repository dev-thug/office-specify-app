//! approvals module DTOs (spec §6, source B.2/B.3.9, §5.5, A.6.3).
//! Status strings equal the DDL CHECK values (§12.4).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// `POST /approvals/{id}/decisions` (B.3.9) — requires If-Match on the
/// request version and an Idempotency-Key.
#[derive(Debug, Deserialize, ToSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateDecisionRequest {
    /// `approved` | `rejected` (DDL CHECK on approval_decisions.decision).
    pub decision: String,
    /// sha256 hex of the snapshot the approver actually reviewed.
    pub expected_snapshot_hash: String,
    /// Required when approving a prepared native Campaigns export.
    #[serde(default)]
    pub expected_export_hash: Option<String>,
    pub reason: Option<String>,
}

/// `POST /approvals/{id}/revoke` body — If-Match on the request version.
#[derive(Debug, Deserialize, ToSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeApprovalRequest {
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize, schemars::JsonSchema)]
pub struct ApprovalRequestSummary {
    pub id: Uuid,
    /// `campaign_launch` | `message_submit` | `booking_change` |
    /// `suppression_release` (DDL CHECK).
    pub action_type: String,
    /// The concrete target FK value (launch / outbound / booking change /
    /// suppression id) — exactly one exists per row.
    pub target_id: Uuid,
    pub requested_by: Uuid,
    pub status: String,
    /// sha256 hex of the pinned snapshot.
    pub snapshot_hash: String,
    pub expires_at: DateTime<Utc>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize, schemars::JsonSchema)]
pub struct ApprovalDecisionView {
    pub id: Uuid,
    pub decision: String,
    pub decided_by: Uuid,
    pub decided_at: DateTime<Utc>,
    pub reason: Option<String>,
}

/// Non-sensitive target summary for `GET /approvals/{id}` (B.2 "동결
/// 스냅샷·policy·권한"). Content-bearing fields are only present when the
/// caller holds a mailbox read grant on the target's mailbox (§5.6).
#[derive(Debug, Clone, Serialize, ToSchema, Deserialize, schemars::JsonSchema)]
pub struct ApprovalTargetSummary {
    /// Current business status of the target row (launch status, outbound
    /// status, booking_change status, or `active`/`released` for
    /// suppressions). Absent when the target no longer resolves.
    pub status: Option<String>,
    /// Recomputed hash of the live target — compared to `snapshot_hash` at
    /// decision time (A.6.3).
    pub current_hash: Option<String>,
    /// The mailbox this target sends through, when mail-bound (launch /
    /// outbound approvals).
    pub mail_account_id: Option<Uuid>,
    /// Only with a mailbox read grant: launch → recipient/step counts,
    /// outbound → to-address + subject.
    pub preview: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize, schemars::JsonSchema)]
pub struct ApprovalRequestDetail {
    #[serde(default)]
    pub native_export_required: bool,
    #[serde(default)]
    pub native_export: Option<NativeExportApprovalView>,
    #[serde(flatten)]
    pub request: ApprovalRequestSummary,
    pub decision: Option<ApprovalDecisionView>,
    pub target: ApprovalTargetSummary,
    /// Frozen mail snapshot, included only when the caller also has mailbox
    /// `mail.read` authorization. Contents are the payload pinned by hash.
    pub snapshot: Option<serde_json::Value>,
    /// Current approval checks; unavailable checks are labeled explicitly.
    pub policy_results: Vec<serde_json::Value>,
    pub self_request: bool,
    pub can_decide: bool,
    pub recipients_count: Option<i64>,
    /// The pinned snapshot/manifest hash, repeated for the approval UI.
    pub manifest_hash: String,
}

/// `POST /approvals/{id}/decisions` response (B.3.9). `execution_status` is
/// `queued` when a follow-up job was enqueued — never "sent" or "done".
#[derive(Debug, Clone, Serialize, ToSchema, Deserialize)]
pub struct DecisionResponse {
    pub approval_request_id: Uuid,
    pub approval_status: String,
    pub decision_id: Uuid,
    /// `queued` (approved) | `none` (rejected — nothing to execute).
    pub execution_status: String,
    pub job_id: Option<Uuid>,
    /// Always 0 — a decision never means provider acceptance (B.3.9).
    pub provider_accepted_count: i64,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize)]
pub struct RevokeResponse {
    pub approval_request_id: Uuid,
    pub status: String,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct NativeExportApprovalView {
    pub id: Uuid,
    pub export_hash: String,
    pub state: String,
    pub subject: String,
    pub body_html: String,
    pub field_map: serde_json::Value,
    pub recipient_count: i64,
    pub sender_address: String,
    pub provider_list_key: Option<String>,
    pub topic_id: Option<String>,
}
