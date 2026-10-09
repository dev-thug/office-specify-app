//! compliance module DTOs (spec §6 — owned by the module worker).
//!
//! Covers B.2 `/communication-bases`, `/suppressions` and the B.4.4 public
//! unsubscribe endpoints. Request DTOs are `deny_unknown_fields` and never
//! accept system-owned fields (B.1.1): `status`, `reviewed_by`,
//! `released_by`, `created_by`, `workspace_id` are server-assigned.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ------------------------------------------------------------- requests

/// `POST /api/v1/communication-bases` — new basis, always stored `pending`
/// (B.2: "신규는 pending"). A human review transitions it later.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateCommunicationBasisRequest {
    pub email_address_id: Uuid,
    /// DDL CHECK: marketing | correspondence | transactional.
    pub purpose: String,
    /// Policy jurisdiction key, e.g. "KR", "US", "default" (§11.4).
    pub jurisdiction: String,
    /// DDL CHECK: explicit_consent | requested_reply |
    /// requested_transaction | reviewed_exception.
    pub basis_type: String,
    /// Pointer to stored consent/exception evidence (§11.4, §15.2).
    pub evidence_ref: String,
    pub collected_at: DateTime<Utc>,
    #[serde(default)]
    pub valid_until: Option<DateTime<Utc>>,
}

/// `POST /api/v1/communication-bases/{id}/reviews` — human judgment
/// (B.2: "valid/rejected 판단, 유효 기간"). Requires `If-Match`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewBasisRequest {
    /// `valid` | `rejected`.
    pub decision: String,
    /// Optional expiry for a `valid` decision; rejected bases keep NULL.
    #[serde(default)]
    pub valid_until: Option<DateTime<Utc>>,
}

/// `POST /api/v1/suppressions` — record a workspace-wide block
/// (B.2: "주소·목적·이유 → 차단"). Reaffirming an identical active
/// suppression is idempotent by design (§11.2).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSuppressionRequest {
    pub email_address_id: Uuid,
    /// DDL CHECK: marketing | all.
    pub purpose_scope: String,
    /// DDL CHECK: unsubscribe | complaint | hard_bounce | manual.
    pub reason: String,
    /// Originating outbound message, when known (FK-checked).
    #[serde(default)]
    pub source_outbound_id: Option<Uuid>,
}

/// `POST /api/v1/suppressions/{id}/release-requests` — evidence + reason go
/// to the audit entry metadata (the table has no column; integration
/// contract). Never a direct release (B.2).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReleaseSuppressionRequest {
    /// Required evidence pointer for the compliance review.
    pub evidence_ref: String,
    /// Required human-readable reason.
    pub reason: String,
}

// ------------------------------------------------------------ responses

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CommunicationBasisResponse {
    pub id: Uuid,
    pub email_address_id: Uuid,
    /// Display convenience — the joined `email_addresses.address_original`.
    pub email_address: Option<String>,
    pub purpose: String,
    pub jurisdiction: String,
    pub basis_type: String,
    /// pending | valid | rejected | revoked | expired.
    pub status: String,
    pub evidence_ref: String,
    pub collected_at: DateTime<Utc>,
    pub reviewed_by: Option<Uuid>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub valid_until: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SuppressionResponse {
    pub id: Uuid,
    pub email_address_id: Uuid,
    /// Display convenience — the joined `email_addresses.address_original`.
    pub email_address: Option<String>,
    /// marketing | all.
    pub purpose_scope: String,
    /// unsubscribe | complaint | hard_bounce | manual.
    pub reason: String,
    /// active | released.
    pub status: String,
    pub source_outbound_id: Option<Uuid>,
    pub created_by: Option<Uuid>,
    pub released_at: Option<DateTime<Utc>>,
    pub released_by: Option<Uuid>,
    pub release_evidence_ref: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

/// `POST /suppressions/{id}/release-requests` result (B.2 → approvals).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ReleaseRequestResponse {
    pub approval_request_id: Uuid,
    pub suppression_id: Uuid,
    /// Always `pending` at creation.
    pub status: String,
    pub expires_at: DateTime<Utc>,
}

// --------------------------------------------------- public unsubscribe

/// `GET /public/v1/unsubscribe/{token}` — confirmation data only, no state
/// change (§11.2). Never reveals the underlying address.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UnsubscribeConfirmationResponse {
    /// `pending_confirmation` (link not used yet) or `unsubscribed`
    /// (already processed — same GET stays side-effect free).
    pub status: String,
    /// Always `workspace_marketing` — tokens are marketing-scoped (DDL).
    pub scope: String,
    pub message: String,
}

/// `POST /public/v1/unsubscribe/{token}` result (B.4.4 verbatim).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UnsubscribeResultResponse {
    /// Always `unsubscribed`.
    pub status: String,
    /// Always `workspace_marketing`.
    pub scope: String,
    pub message: String,
}
