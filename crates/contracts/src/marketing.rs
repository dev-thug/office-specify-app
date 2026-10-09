//! Zoho Campaigns marketing setup and connection DTOs. Never contain secrets.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Serialize, ToSchema)]
pub struct CampaignsSetupResponse {
    pub configured: bool,
    pub missing_fields: Vec<String>,
    pub callback_uri: String,
    pub accounts_host: String,
    pub campaigns_host: String,
    pub plan: String,
    pub scheduled_send: bool,
    pub webhooks: bool,
    pub mail_oauth_configured: bool,
    pub email_api_configured: bool,
    pub workspace_id: Uuid,
    pub connections: Vec<MarketingConnectionResponse>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MarketingConnectionResponse {
    pub id: Uuid,
    #[serde(default)]
    pub write_scopes_ready: bool,
    pub name: String,
    pub status: String,
    pub plan: String,
    pub region: String,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub lists_count: i32,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MarketingConnectResponse {
    pub connection_id: Uuid,
    pub authorization_url: Option<String>,
    pub state_expires_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MarketingListResponse {
    pub key: String,
    pub name: String,
    pub contacts: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MarketingDisconnectedResponse {
    pub disconnected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NativeCampaignResponse {
    pub key: String,
    pub name: String,
    pub status: String,
}

#[derive(Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ZohoTemplateWriteRequest {
    pub name: String,
    pub content: String,
    pub content_type: String,
}
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct ZohoTemplateSummaryResponse {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct ZohoTemplateDetailsResponse {
    pub id: String,
    pub name: String,
    pub content: String,
    pub content_type: String,
    pub content_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ZohoTemplateMutationResponse {
    pub operation_id: Uuid,
    pub template_id: Option<String>,
    pub deleted: bool,
}

/// Explicit account-verified mapping for an Office variable with distinct
/// values across recipients. Office retains the authoritative variable values.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MarketingFieldMapping {
    pub contact_field: String,
    pub merge_tag: String,
}
#[derive(Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PrepareMarketingExportRequest {
    pub launch_id: Uuid,
    pub topic_id: Option<String>,
    #[serde(default)]
    pub field_map: std::collections::BTreeMap<String, MarketingFieldMapping>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MarketingExportResponse {
    pub id: Uuid,
    pub launch_id: Uuid,
    pub connection_id: Uuid,
    pub state: String,
    pub version: i64,
    pub source_manifest_hash: String,
    pub export_hash: String,
    pub content_hash: String,
    pub recipient_count: i32,
    pub active_count: i32,
    pub pending_count: i32,
    pub excluded_count: i32,
    pub sender_address: String,
    pub sender_name: String,
    pub subject: String,
    pub provider_list_key: Option<String>,
    pub topic_id: Option<String>,
    pub provider_campaign_key: Option<String>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub report: serde_json::Value,
}
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct MarketingExportContentResponse {
    pub export_id: Uuid,
    pub subject: String,
    pub html: String,
    pub text: String,
    pub export_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SendMarketingExportRequest {
    pub expected_export_hash: String,
    pub confirmation: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MarketingChannelSetupRequest {
    pub mail_account_id: Uuid,
    pub sender_identity_id: Uuid,
    pub connection_id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MarketingSenderResponse {
    pub id: Uuid,
    pub address: String,
    pub display_name: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RegisterMarketingSenderRequest {
    pub mail_account_id: Uuid,
    pub address: String,
    pub display_name: String,
    pub verification_evidence_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MarketingContactFieldResponse {
    pub display_name: String,
    pub field_name: String,
    pub field_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MarketingTopicResponse {
    pub id: String,
    pub name: String,
    pub primary_list: Option<String>,
}
