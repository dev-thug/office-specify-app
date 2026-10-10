//! sending module DTOs (spec §6, B.3.10/B.3.11 — owned by the module worker).
//!
//! Request DTOs use `deny_unknown_fields` and never accept system-owned
//! fields (workspace_id, requested_by, status, payload_hash, provider_*,
//! verification fields) — B.1.1.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// `POST /api/v1/message-drafts` (B.3.10).
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateMessageDraftRequest {
    pub mail_account_id: Uuid,
    pub to_email_address_id: Uuid,
    /// marketing | correspondence | transactional
    pub purpose: String,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub template_version_id: Option<Uuid>,
    /// Template variable values. Keys must be declared in the version's
    /// `variable_schema`; system variables may not be supplied.
    pub variables: Option<serde_json::Map<String, serde_json::Value>>,
    /// Reply linkage — all three are required together (DDL CHECK).
    pub thread_id: Option<Uuid>,
    pub reply_to_message_id: Option<Uuid>,
    pub expected_last_inbound_id: Option<Uuid>,
    /// "private" (default) | "mailbox" — shared drafts for the account team.
    pub visibility: Option<String>,
}

/// `PATCH /api/v1/message-drafts/{id}` — content allowlist only.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchMessageDraftRequest {
    pub subject: Option<String>,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub variables: Option<serde_json::Map<String, serde_json::Value>>,
    pub to_email_address_id: Option<Uuid>,
    /// Confirming a reviewed newer inbound — the re-review action after
    /// REPLY_CONTEXT_CHANGED (T16).
    pub expected_last_inbound_id: Option<Uuid>,
    pub visibility: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MessageDraftResponse {
    pub draft_id: Uuid,
    /// Present after this draft has been frozen into an immutable snapshot.
    pub outbound_message_id: Option<Uuid>,
    pub mail_account_id: Uuid,
    pub owner_principal_id: Uuid,
    pub visibility: String,
    pub thread_id: Option<Uuid>,
    pub reply_to_message_id: Option<Uuid>,
    pub expected_last_inbound_id: Option<Uuid>,
    pub to_email_address_id: Uuid,
    pub template_version_id: Option<Uuid>,
    pub purpose: String,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub variables: serde_json::Value,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MessageDraftListItem {
    pub draft_id: Uuid,
    pub thread_id: Option<Uuid>,
    /// Present after the draft is frozen.
    pub outbound_message_id: Option<Uuid>,
    pub mail_account_id: Uuid,
    pub owner_principal_id: Uuid,
    pub visibility: String,
    pub purpose: String,
    pub subject: String,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `POST /api/v1/message-drafts/{id}/freeze` (B.3.10): channel (+sender)
/// selection produces the immutable outbound snapshot.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FreezeMessageDraftRequest {
    pub delivery_channel_id: Uuid,
    /// Must equal the channel's bound sender_identity_id when supplied
    /// (fk_060 pins the sender to the channel).
    pub sender_identity_id: Option<Uuid>,
    /// Meeting type used to mint the recipient-bound system booking URL when
    /// the selected template requires `meeting.booking_url`.
    #[serde(default)]
    pub meeting_type_id: Option<Uuid>,
    /// Explicitly request recipient-bound open/click request tracking.
    /// Workspace settings can disable tracking regardless of this value.
    #[serde(default)]
    pub tracking_requested: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FreezeMessageDraftResponse {
    pub outbound_message_id: Uuid,
    pub status: String,
    /// SHA-256 hex of the immutable snapshot — passed back as
    /// `expected_payload_hash` on approval-requests / submit.
    pub payload_hash: String,
    pub version: i64,
}

/// `POST /api/v1/outbound-messages/{id}/preflight` (B.3.10). Mirrors the
/// B.3.7 evaluation shape; explicitly non-binding.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OutboundPreflightResponse {
    pub checked_at: DateTime<Utc>,
    /// Never `true` — policy evaluation is not an execution authorization.
    pub is_execution_authorization: bool,
    pub global_blocking_reasons: Vec<String>,
    pub checks: Vec<OutboundPreflightCheck>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OutboundPreflightCheck {
    /// Stable predicate name (see application::sending::policy).
    pub name: String,
    pub passed: bool,
    /// Stable failure code (B.5-style) when `passed` is false.
    pub code: Option<String>,
}

/// `POST /api/v1/outbound-messages/{id}/approval-requests` (B.3.10).
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateOutboundApprovalRequest {
    pub reason: String,
    /// Must equal the outbound's current `payload_hash` — the approval is
    /// bound to this exact snapshot.
    pub expected_payload_hash: String,
    /// Defaults to now + 7 days.
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OutboundApprovalRequestResponse {
    pub approval_request_id: Uuid,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub version: i64,
}

/// `POST /api/v1/outbound-messages/{id}/submit` (B.3.10). Requires
/// `Idempotency-Key`; agents are rejected (AGENT_DIRECT_SEND_FORBIDDEN).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitOutboundRequest {
    /// Client must resend the payload hash it expects to submit — a changed
    /// snapshot yields SNAPSHOT_MISMATCH.
    pub expected_payload_hash: String,
    pub not_before: Option<DateTime<Utc>>,
}

/// B.3.10 response: `{outbound_message_id, status, job_id,
/// provider_accepted}` — accepted is `false` because provider acceptance is
/// only confirmed asynchronously.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct SubmitOutboundResponse {
    pub outbound_message_id: Uuid,
    pub status: String,
    pub job_id: Uuid,
    /// Always `false` at enqueue — set only after provider confirmation.
    pub provider_accepted: bool,
}

/// `POST /api/v1/outbound-messages/{id}/cancel` — reason is optional and
/// recorded in the audit trail.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelOutboundRequest {
    pub reason: Option<String>,
}

/// Minimal cancellation result for callers whose cancel permission does not
/// include permission to read the outbound body or recipient address.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CancelOutboundResponse {
    pub outbound_message_id: Uuid,
    pub status: String,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema, schemars::JsonSchema)]
pub struct OutboundReplyReference {
    pub inbound_message_id: Uuid,
    pub thread_id: Uuid,
    pub from_address: String,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema, schemars::JsonSchema)]
pub struct OutboundEngagementSummary {
    /// False means tracking was disabled by either this freeze request or
    /// workspace policy; zero counts alone do not imply that tracking ran.
    pub tracking_enabled: bool,
    /// Capability requests, not proof of a person opening the message.
    pub open_request_count: i64,
    /// Capability requests, not proof of a person following a link.
    pub click_request_count: i64,
}

/// `GET /api/v1/outbound-messages/{id}` (B.3.11). `delivery_status` is built
/// only from provider-confirmed delivery_events — `accepted` never implies
/// delivered/read (T34).
#[derive(Debug, Clone, Serialize, ToSchema, schemars::JsonSchema)]
pub struct OutboundMessageResponse {
    pub outbound_message_id: Uuid,
    /// Mailbox-local thread containing the sent message and verified replies.
    pub thread_id: Option<Uuid>,
    pub status: String,
    /// SHA-256 of the immutable frozen snapshot, required on submit/approval.
    pub payload_hash: String,
    pub failure_code: Option<String>,
    /// true → provider confirmed acceptance; false → definitively failed;
    /// null → outcome not established.
    pub provider_accepted: Option<bool>,
    /// unknown | accepted | delivered | bounced | complained | deferred —
    /// provider evidence only.
    pub delivery_status: String,
    /// ADR-006: unknown outcomes are never resubmitted automatically.
    pub automatic_resubmit_allowed: bool,
    pub next_action: String,
    pub last_attempt: Option<OutboundAttemptInfo>,
    /// Latest request state; null means no approval request exists.
    pub approval_status: Option<String>,
    /// Current decision, expiry, snapshot, and human approver permission all
    /// remain valid for the immutable outbound snapshot.
    pub approval_valid: bool,
    /// Whether the latest approval was issued for this exact frozen hash.
    pub approval_snapshot_matches: bool,
    pub approval_expires_at: Option<DateTime<Utc>>,
    pub known_replies: Vec<OutboundReplyReference>,
    pub engagement: OutboundEngagementSummary,
    pub purpose: String,
    /// Frozen sender identity shown in the submit confirmation preview.
    pub from_address: String,
    pub subject: String,
    /// Frozen body snapshot shown in the submit confirmation preview.
    pub body_text: String,
    pub body_html: Option<String>,
    pub to_address: String,
    pub not_before: Option<DateTime<Utc>>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema, schemars::JsonSchema)]
pub struct OutboundAttemptInfo {
    pub attempt_no: i64,
    pub status: String,
    pub error_code: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}
