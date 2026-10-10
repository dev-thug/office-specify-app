//! Personalized sales campaign DTOs.
//!
//! Campaigns group independently authored company drafts around one pinned
//! workspace SMTP sender. Board stages after `review` are derived from the
//! immutable outbound record and cannot be set by clients.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::sending::{MessageDraftResponse, SubmitOutboundResponse};

/// Detail responses include every target; the application rejects target 201.
pub const MAX_SALES_CAMPAIGN_TARGETS: usize = 200;

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSalesCampaignRequest {
    pub name: String,
    pub objective: Option<String>,
    pub meeting_type_id: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetSalesCampaignSenderRequest {
    pub smtp_profile_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct SalesCampaignSenderResponse {
    /// `None` means Mail accounts has no explicit default sender configured.
    pub sender: Option<SalesCampaignSenderSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct SalesCampaignSenderSummary {
    pub smtp_profile_id: Uuid,
    pub profile_name: String,
    pub mail_account_id: Uuid,
    pub mailbox_name: String,
    pub delivery_channel_id: Uuid,
    pub sender_identity_id: Uuid,
    pub from_address: String,
    pub from_name: String,
    pub reply_to: Option<String>,
    pub status: String,
    pub ready: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct SalesCampaignSummary {
    pub id: Uuid,
    pub name: String,
    pub objective: Option<String>,
    pub mail_account_id: Uuid,
    pub delivery_channel_id: Uuid,
    pub sender_identity_id: Uuid,
    pub smtp_profile_id: Uuid,
    pub meeting_type_id: Option<Uuid>,
    pub from_address: String,
    pub from_name: String,
    pub reply_to: Option<String>,
    pub sender_status: String,
    pub sender_ready: bool,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct SalesCampaignResponse {
    pub campaign: SalesCampaignSummary,
    pub targets: Vec<SalesCampaignTargetResponse>,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddSalesCampaignTargetRequest {
    pub company_id: Uuid,
    pub contact_id: Uuid,
    pub email_address_id: Uuid,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateSalesCampaignTargetRequest {
    /// Only `research`, `draft`, and `review` are writable. Queued,
    /// accepted, and attention are derived from approval and outbound state.
    pub stage: String,
    pub expected_target_version: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSalesCampaignDraftRequest {
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub template_version_id: Option<Uuid>,
    #[serde(default)]
    pub variables: Option<serde_json::Map<String, serde_json::Value>>,
    /// Required to make a new draft after provider acceptance a deliberate
    /// follow-up instead of a silent retry.
    #[serde(default)]
    pub explicit_follow_up: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueueSalesCampaignTargetRequest {
    pub outbound_message_id: Uuid,
    pub expected_payload_hash: String,
    pub expected_target_version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct QueueSalesCampaignTargetResponse {
    pub submission: SubmitOutboundResponse,
    pub batch_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct SalesCampaignTargetResponse {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub email_address_id: Option<Uuid>,
    pub company_name: Option<String>,
    pub contact_name: Option<String>,
    pub email_address: Option<String>,
    /// Effective board column. Later delivery columns are server-derived.
    pub stage: String,
    /// Last editable workflow column, retained when a send is queued.
    pub workflow_stage: String,
    pub version: i64,
    pub current_draft: Option<SalesCampaignDraftSummary>,
    pub outbound: Option<SalesCampaignOutboundSummary>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct SalesCampaignDraftSummary {
    pub draft_id: Uuid,
    pub subject: String,
    pub status: String,
    pub version: i64,
    pub outbound_message_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct SalesCampaignOutboundSummary {
    pub outbound_message_id: Uuid,
    pub status: String,
    pub payload_hash: String,
    /// Persisted ten-minute batch boundary. Kept on the card after reload.
    pub not_before: Option<DateTime<Utc>>,
    pub approval_status: Option<String>,
    pub approval_expires_at: Option<DateTime<Utc>>,
    pub approved_at: Option<DateTime<Utc>>,
    pub provider_accepted: bool,
    pub accepted_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
    pub open_count: i64,
    pub click_count: i64,
    pub known_reply_count: i64,
    pub last_reply_at: Option<DateTime<Utc>>,
}

/// Target-draft creation returns the existing immutable-message API response,
/// including the draft id and mailbox required by the shared editor flow.
pub type SalesCampaignTargetDraftResponse = MessageDraftResponse;
