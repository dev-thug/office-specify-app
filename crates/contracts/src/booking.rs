//! Booking module REST/MCP DTOs (spec §10, B.2, B.4).

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateMeetingTypeRequest {
    pub calendar_connection_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_duration")]
    pub duration_minutes: i32,
    #[serde(default = "default_buffer")]
    pub buffer_before_minutes: i32,
    #[serde(default = "default_buffer")]
    pub buffer_after_minutes: i32,
    #[serde(default = "default_notice")]
    pub minimum_notice_minutes: i32,
    #[serde(default = "default_horizon")]
    pub booking_horizon_days: i32,
    /// `online` or `offline`.
    #[serde(default = "default_location_kind")]
    pub location_kind: String,
    /// Video link / tool note (online) or address (offline), shown to guests.
    #[serde(default)]
    pub location_detail: Option<String>,
}

fn default_duration() -> i32 {
    30
}
fn default_buffer() -> i32 {
    10
}
fn default_notice() -> i32 {
    1440
}
fn default_horizon() -> i32 {
    30
}
fn default_location_kind() -> String {
    "online".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateMeetingTypeRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<Option<String>>,
    #[serde(default)]
    pub duration_minutes: Option<i32>,
    #[serde(default)]
    pub buffer_before_minutes: Option<i32>,
    #[serde(default)]
    pub buffer_after_minutes: Option<i32>,
    #[serde(default)]
    pub minimum_notice_minutes: Option<i32>,
    #[serde(default)]
    pub booking_horizon_days: Option<i32>,
    #[serde(default)]
    pub location_kind: Option<String>,
    #[serde(default)]
    pub location_detail: Option<Option<String>>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct MeetingTypeResponse {
    pub id: Uuid,
    pub host_principal_id: Uuid,
    pub calendar_connection_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub duration_minutes: i32,
    pub buffer_before_minutes: i32,
    pub buffer_after_minutes: i32,
    pub minimum_notice_minutes: i32,
    pub booking_horizon_days: i32,
    pub location_kind: String,
    pub location_detail: Option<String>,
    pub status: String,
    pub version: i64,
}

/// Agent-safe workspace inventory. Never contains calendar connection/provider IDs.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MeetingTypeDiscoveryResponse {
    pub id: Uuid,
    pub host_principal_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub duration_minutes: i32,
    pub buffer_before_minutes: i32,
    pub buffer_after_minutes: i32,
    pub minimum_notice_minutes: i32,
    pub booking_horizon_days: i32,
    pub status: String,
    pub version: i64,
}

impl From<MeetingTypeResponse> for MeetingTypeDiscoveryResponse {
    fn from(value: MeetingTypeResponse) -> Self {
        Self {
            id: value.id,
            host_principal_id: value.host_principal_id,
            name: value.name,
            description: value.description,
            duration_minutes: value.duration_minutes,
            buffer_before_minutes: value.buffer_before_minutes,
            buffer_after_minutes: value.buffer_after_minutes,
            minimum_notice_minutes: value.minimum_notice_minutes,
            booking_horizon_days: value.booking_horizon_days,
            status: value.status,
            version: value.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityRuleRequest {
    pub timezone: String,
    pub weekday: i16,
    /// Local time in `HH:MM` format.
    pub start_local: String,
    pub end_local: String,
    #[serde(default)]
    pub valid_from: Option<NaiveDate>,
    #[serde(default)]
    pub valid_until: Option<NaiveDate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityExceptionRequest {
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    /// `blocked` or `available`.
    pub kind: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateAvailabilityRequest {
    pub rules: Vec<AvailabilityRuleRequest>,
    #[serde(default)]
    pub exceptions: Vec<AvailabilityExceptionRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AvailabilityResponse {
    pub host_principal_id: Uuid,
    pub rules: Vec<AvailabilityRuleRequest>,
    pub exceptions: Vec<AvailabilityExceptionRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateBookingLinkRequest {
    pub meeting_type_id: Uuid,
    #[serde(default)]
    pub bound_email_address_id: Option<Uuid>,
    #[serde(default)]
    pub campaign_enrollment_id: Option<Uuid>,
    #[serde(default = "default_max_uses")]
    pub max_uses: i32,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

fn default_max_uses() -> i32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BookingLinkCreatedResponse {
    pub id: Uuid,
    pub token_url: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PublicBookingLinkResponse {
    pub meeting_type_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub host_name: String,
    pub duration_minutes: i32,
    pub timezone: String,
    pub minimum_notice_minutes: i32,
    pub booking_horizon_days: i32,
    /// The exact notice version the guest must acknowledge in a booking POST.
    pub privacy_notice_version: String,
    /// `online` or `offline`, plus the guest-facing link note or address.
    #[serde(default = "default_location_kind")]
    pub location_kind: String,
    #[serde(default)]
    pub location_detail: Option<String>,
    /// Host profile photo, served through this same link capability.
    #[serde(default)]
    pub host_avatar_url: Option<String>,
    /// Host job title shown under the name.
    #[serde(default)]
    pub host_title: Option<String>,
    /// Invited recipient for a link bound to one address (outreach links),
    /// so the guest form can be prefilled. Absent for open links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invitee: Option<PublicBookingInvitee>,
}

/// Where the guest manages the booking a used-up invitation already made.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PublicLinkBookingResponse {
    pub manage_url: String,
}

/// The recipient a bound booking link was issued to — only ever shown to the
/// holder of that recipient's own capability link.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PublicBookingInvitee {
    pub email: String,
    pub name: Option<String>,
    pub company_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SlotResponse {
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SlotsResponse {
    pub timezone: String,
    pub availability_checked_at: DateTime<Utc>,
    pub slots: Vec<SlotResponse>,
    pub reservation_guaranteed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreatePublicBookingRequest {
    pub guest_name: String,
    pub guest_email: String,
    pub starts_at: DateTime<Utc>,
    pub timezone: String,
    pub privacy_notice_version: String,
    /// Optional guest-provided context (max 2,000 characters).
    #[serde(default)]
    pub memo: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BookingAcceptedResponse {
    pub id: Uuid,
    pub status: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub timezone: String,
    pub manage_url: String,
    pub status_url: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BookingResponse {
    pub id: Uuid,
    pub booking_link_id: Uuid,
    pub host_principal_id: Uuid,
    pub guest_email_address_id: Uuid,
    pub guest_email: Option<String>,
    pub guest_name: String,
    pub guest_memo: Option<String>,
    pub guest_timezone: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub failure_code: Option<String>,
    pub provider_event_id: Option<String>,
    pub attendance_status: Option<String>,
    pub outcome_note: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BookingAttendanceRequest {
    /// `attended` or `no_show`.
    pub attendance_status: String,
    #[serde(default)]
    pub outcome_note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ManageBookingResponse {
    pub id: Uuid,
    pub guest_name: String,
    pub meeting_name: String,
    #[serde(default)]
    pub meeting_description: Option<String>,
    /// Meeting length and booking window, so a reschedule can reuse the
    /// booking calendar.
    #[serde(default)]
    pub duration_minutes: i32,
    #[serde(default)]
    pub booking_horizon_days: i32,
    #[serde(default = "default_location_kind")]
    pub location_kind: String,
    #[serde(default)]
    pub location_detail: Option<String>,
    #[serde(default)]
    pub host_avatar_url: Option<String>,
    #[serde(default)]
    pub host_title: Option<String>,
    pub host_display_name: String,
    pub guest_memo: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub guest_timezone: String,
    pub status: String,
    pub version: i64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PublicRescheduleRequest {
    pub starts_at: DateTime<Utc>,
    pub timezone: String,
    pub expected_version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PublicCancelRequest {
    pub expected_version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BookingChangeProposalRequest {
    /// `create`, `reschedule` or `cancel`.
    pub kind: String,
    #[serde(default)]
    pub booking_id: Option<Uuid>,
    #[serde(default)]
    pub starts_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// MCP `booking_propose` input. The only side effect is an approval proposal;
/// the calendar is never written by the tool.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct McpBookingProposalRequest {
    pub meeting_type_id: Uuid,
    pub guest_name: String,
    pub guest_email: String,
    pub desired_starts_at: DateTime<Utc>,
    pub timezone: String,
    pub purpose: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BookingChangeResponse {
    pub id: Uuid,
    pub booking_id: Option<Uuid>,
    pub kind: String,
    pub status: String,
    pub approval_request_id: Option<Uuid>,
}
