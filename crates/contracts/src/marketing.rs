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
