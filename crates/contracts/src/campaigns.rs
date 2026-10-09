//! campaigns module DTOs (spec §6, source B.2/B.3.6–B.3.8, §7.5).
//! Requests use `deny_unknown_fields` and never accept system-owned fields
//! (B.1.1). Status strings equal the DDL CHECK values (§12.4).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;
use uuid::Uuid;

/// Per-step definition inside a campaign (re)vision (B.3.6). `step_order` is
/// 1-based, max 3 (DDL CHECK); `delay_hours` is relative to the previous
/// step's `accepted_at` (step 1 is relative to launch activation).
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CampaignStepInput {
    pub step_order: i32,
    pub template_version_id: Uuid,
    pub delay_hours: i32,
}

/// Sending window/rate rules stored on the revision (B.3.6 `schedule_rules`).
/// Weekdays are ISO-8601 numbers (1 = Monday … 7 = Sunday).
#[derive(Debug, Clone, Default, Deserialize, Serialize, ToSchema, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleRulesInput {
    #[serde(default)]
    pub weekdays: Vec<u8>,
    /// Local "HH:MM" window start in the revision timezone.
    pub window_start: Option<String>,
    /// Local "HH:MM" window end in the revision timezone.
    pub window_end: Option<String>,
    pub internal_daily_cap: Option<u32>,
    pub minimum_gap_seconds: Option<u32>,
    pub launch_not_before: Option<DateTime<Utc>>,
    pub launch_expires_at: Option<DateTime<Utc>>,
    /// Optional host binding for server-generated `meeting.booking_url`.
    pub meeting_type_id: Option<Uuid>,
}

/// `POST /campaigns` (B.3.6) — creates the campaign plus revision 1 and its
/// steps in one transaction.
#[derive(Debug, Deserialize, ToSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateCampaignRequest {
    pub name: String,
    pub objective: Option<String>,
    pub mail_account_id: Uuid,
    pub delivery_channel_id: Uuid,
    pub timezone: String,
    pub schedule_rules: ScheduleRulesInput,
    pub steps: Vec<CampaignStepInput>,
    #[serde(default = "default_true")]
    pub stop_on_reply: bool,
    #[serde(default = "default_true")]
    pub stop_on_booking: bool,
}

fn default_true() -> bool {
    true
}

/// `POST /campaigns/{id}/revisions` — same shape minus the campaign fields.
#[derive(Debug, Deserialize, ToSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRevisionRequest {
    pub delivery_channel_id: Uuid,
    pub timezone: String,
    pub schedule_rules: ScheduleRulesInput,
    pub steps: Vec<CampaignStepInput>,
    #[serde(default = "default_true")]
    pub stop_on_reply: bool,
    #[serde(default = "default_true")]
    pub stop_on_booking: bool,
}

/// `POST /campaigns/{id}/preflight` (B.3.7) — non-binding validation.
#[derive(Debug, Deserialize, ToSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreflightRequest {
    pub revision_id: Uuid,
    pub email_address_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize, schemars::JsonSchema)]
pub struct PreflightRecipient {
    pub email_address_id: Uuid,
    pub recipient_checks_passed: bool,
    /// B.5 code names + module-local reason codes (ADDRESS_NOT_FOUND,
    /// ADDRESS_INVALID).
    pub reason_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize, schemars::JsonSchema)]
pub struct PreflightResponse {
    pub checked_at: DateTime<Utc>,
    /// Always false — preflight is never an execution authorization (B.3.7).
    pub is_execution_authorization: bool,
    pub global_blocking_reasons: Vec<String>,
    pub recipients: Vec<PreflightRecipient>,
}

/// `POST /campaigns/{id}/launches` (B.3.8) → 202 building + job id.
#[derive(Debug, Deserialize, ToSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateLaunchRequest {
    pub revision_id: Uuid,
    pub email_address_ids: Vec<Uuid>,
    /// Allowed personalization values — applied to every recipient unless a
    /// system-resolved variable of the same name exists (system wins).
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize)]
pub struct CreateLaunchResponse {
    pub launch_id: Uuid,
    pub status: String,
    pub job_id: Uuid,
    pub version: i64,
}

/// `POST /campaign-launches/{id}/approval-requests` (B.3.8).
#[derive(Debug, Deserialize, ToSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateLaunchApprovalRequest {
    /// sha256 hex of the manifest the caller reviewed.
    pub expected_manifest_hash: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize, schemars::JsonSchema)]
pub struct LaunchApprovalRequestResponse {
    pub approval_request_id: Uuid,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub version: i64,
    pub launch_id: Uuid,
}

/// `POST /campaign-launches/{id}/pause` body (B.2) — If-Match on the launch.
#[derive(Debug, Deserialize, ToSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PauseLaunchRequest {
    pub reason: Option<String>,
}

/// `POST /campaign-launches/{id}/cancel` body (B.2) — If-Match on the launch.
#[derive(Debug, Deserialize, ToSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CancelLaunchRequest {
    pub reason: Option<String>,
}

// ------------------------------------------------------------- responses

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize, schemars::JsonSchema)]
pub struct CreateCampaignResponse {
    pub campaign_id: Uuid,
    pub revision_id: Uuid,
    pub status: String,
    pub revision_no: i32,
    pub version: i64,
    /// true only when the delivery channel is enabled and policy-allowed.
    pub channel_ready: bool,
    /// B.5 code names that currently block operational sending.
    pub blocking_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize)]
pub struct CampaignSummary {
    pub id: Uuid,
    pub name: String,
    pub objective: Option<String>,
    pub mail_account_id: Uuid,
    pub owner_principal_id: Uuid,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize)]
pub struct CampaignStepSummary {
    pub id: Uuid,
    pub step_order: i32,
    pub template_version_id: Uuid,
    pub delay_hours: i32,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize)]
pub struct RevisionSummary {
    pub id: Uuid,
    pub campaign_id: Uuid,
    pub revision_no: i32,
    pub status: String,
    pub delivery_channel_id: Uuid,
    pub mail_account_id: Uuid,
    pub timezone: String,
    pub schedule_rules: serde_json::Value,
    pub stop_on_reply: bool,
    pub stop_on_booking: bool,
    pub step_count: i64,
    /// Ordered immutable step definitions, sufficient to review or clone a revision.
    pub steps: Vec<CampaignStepSummary>,
    pub frozen_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize, schemars::JsonSchema)]
pub struct LaunchSummary {
    pub id: Uuid,
    pub revision_id: Uuid,
    pub delivery_channel_id: Uuid,
    pub requested_by: Uuid,
    pub status: String,
    /// sha256 hex (B.1.1 `Hash` wire format); absent while `building`.
    pub manifest_hash: Option<String>,
    pub frozen_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize)]
pub struct CampaignDetail {
    #[serde(flatten)]
    pub campaign: CampaignSummary,
    pub revisions: Vec<RevisionSummary>,
    pub launches: Vec<LaunchSummary>,
    /// UI hint only — the server re-checks every action (B.1.2).
    pub allowed_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize)]
pub struct CreateRevisionResponse {
    pub revision_id: Uuid,
    pub campaign_id: Uuid,
    pub revision_no: i32,
    pub status: String,
    pub channel_ready: bool,
    pub blocking_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize)]
pub struct EnrollmentSummary {
    pub total: i64,
    pub pending: i64,
    pub active: i64,
    pub completed: i64,
    pub stopped: i64,
    pub suppressed: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize)]
pub struct LaunchMessageItem {
    pub outbound_message_id: Uuid,
    pub enrollment_id: Uuid,
    pub step_id: Uuid,
    pub step_order: i32,
    pub to_address_snapshot: String,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub status: String,
    pub not_before: Option<DateTime<Utc>>,
    pub payload_hash: String,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize)]
pub struct LaunchMessagePage {
    pub items: Vec<LaunchMessageItem>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema, Deserialize)]
pub struct LaunchDetail {
    #[serde(flatten)]
    pub launch: LaunchSummary,
    pub campaign_id: Uuid,
    pub revision_no: i32,
    pub mail_account_id: Uuid,
    pub step_count: i64,
    pub enrollments: EnrollmentSummary,
    /// Frozen per-step outbound snapshots (B.3.8) — cursor-paginated.
    pub messages: LaunchMessagePage,
}
