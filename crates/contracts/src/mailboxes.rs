//! mailboxes module DTOs (spec §6 — owned by the module worker). Requests
//! use `deny_unknown_fields` and never accept system-owned fields (B.1.1).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Delivery purpose — equals the DDL CHECK values verbatim (§12.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Correspondence,
    Marketing,
    Transactional,
}

impl Purpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Correspondence => "correspondence",
            Self::Marketing => "marketing",
            Self::Transactional => "transactional",
        }
    }
}

/// `channel_reviews.result` — equals the DDL CHECK verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewResult {
    Allowed,
    Blocked,
    Unverified,
}

impl ReviewResult {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Blocked => "blocked",
            Self::Unverified => "unverified",
        }
    }
}

// --------------------------------------------------------------------------
// mail accounts
// --------------------------------------------------------------------------

/// Mail-account metadata. `allowed_actions` is the caller's grant-derived,
/// principal-filtered action list (B.3.1 `mail.*` strings) — a UI hint only;
/// the server re-checks the grant on every content-bearing operation.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MailAccountResponse {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub provider_account_id: String,
    pub primary_address: String,
    pub name: String,
    /// DDL CHECK: `pending` | `active` | `reauth_required` | `disabled`.
    pub status: String,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub allowed_actions: Vec<String>,
}

// --------------------------------------------------------------------------
// Zoho OAuth connection requests (§14.1 — no network at request time)
// --------------------------------------------------------------------------

/// `POST /mail-accounts/connection-requests` (Admin).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionRequestRequest {
    /// Zoho datacenter region — allowlisted server-side (§14.1).
    pub region: String,
}

/// OAuth start info: the provider authorization URL carries a signed,
/// expiring `state` binding this connection request.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OAuthStartResponse {
    pub connection_id: Uuid,
    pub authorization_url: String,
    pub state_expires_at: DateTime<Utc>,
}

/// `POST /calendar-connections/connection-requests`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CalendarConnectionRequestRequest {
    /// Host the calendar belongs to. Defaults to the caller; setting
    /// another principal requires Admin.
    #[serde(default)]
    pub host_principal_id: Option<Uuid>,
}

/// `POST /calendar-connections/local` — the caller connects their own
/// Office-owned calendar. The body is deliberately empty; no tenant or
/// external credential input is accepted.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LocalCalendarConnectionRequest {}

/// `GET /integrations/zoho/callback` query params (public, browser hit).
#[derive(Debug, Deserialize)]
pub struct ZohoCallbackQuery {
    #[serde(default)]
    pub code: Option<String>,
    pub state: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub error_description: Option<String>,
}

// --------------------------------------------------------------------------
// mailbox grants (B.3.2)
// --------------------------------------------------------------------------

/// `PUT /mail-accounts/{id}/grants/{principal_id}` — full flag replace.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PutGrantRequest {
    pub can_read: bool,
    pub can_compose: bool,
    /// Mailbox send ACL. For Agent principals this only satisfies the
    /// approved-submit path; agents cannot use direct submit or `mail.send`.
    pub can_send: bool,
    /// Human Admin/Manager approver authority. Agents cannot receive it.
    pub can_approve: bool,
    pub can_assign: bool,
}

/// Grant row (B.3.2). `version` is the grant's ETag; `permission_version`
/// is the target principal's bumped cache-invalidation counter (§5.5).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GrantResponse {
    pub mail_account_id: Uuid,
    pub principal_id: Uuid,
    pub can_read: bool,
    pub can_compose: bool,
    /// Agent grants satisfy approved-submit ACL checks, not direct sending.
    pub can_send: bool,
    /// Human Admin/Manager approval authority.
    pub can_approve: bool,
    pub can_assign: bool,
    pub version: i64,
    pub permission_version: i64,
    /// Pending work is revalidated against the new grant (§5.5).
    pub pending_actions_revalidation_requested: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GrantListItem {
    pub principal_id: Uuid,
    pub principal_name: String,
    pub principal_kind: String,
    pub can_read: bool,
    pub can_compose: bool,
    /// Agent grants satisfy approved-submit ACL checks, not direct sending.
    pub can_send: bool,
    /// Human Admin/Manager approval authority.
    pub can_approve: bool,
    pub can_assign: bool,
    pub version: i64,
}

// --------------------------------------------------------------------------
// delivery channels (+ reviews)
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeliveryChannelResponse {
    pub id: Uuid,
    pub mail_account_id: Uuid,
    pub sender_identity_id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub purpose: String,
    pub name: String,
    /// `unverified` | `allowed` | `blocked` — set only via reviews.
    pub policy_status: String,
    pub enabled: bool,
    pub capabilities: serde_json::Value,
    pub internal_daily_cap: Option<i32>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

/// `POST /delivery-channels` (Admin). The channel's provider is taken from
/// the referenced `integration_connections` row.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateDeliveryChannelRequest {
    /// Delivery-side `integration_connections` id (provider credentials).
    pub connection_id: Uuid,
    pub mail_account_id: Uuid,
    /// Sender identity on that mail account.
    pub sender_identity_id: Uuid,
    pub purpose: Purpose,
    pub name: String,
    #[serde(default)]
    pub internal_daily_cap: Option<i32>,
    #[serde(default)]
    pub capabilities: Option<serde_json::Value>,
}

/// `PATCH /delivery-channels/{id}` (Admin) — verified-scope fields only,
/// If-Match required.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchDeliveryChannelRequest {
    /// Enabling requires `policy_status=allowed` + unexpired gate evidence +
    /// provider/purpose policy (T33: zoho_mail marketing/transactional is
    /// never enable-able).
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub internal_daily_cap: Option<i32>,
}

/// `POST /delivery-channels/{id}/reviews` (Admin + policy review).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateChannelReviewRequest {
    pub result: ReviewResult,
    #[serde(default)]
    pub policy_url: Option<String>,
    #[serde(default)]
    pub contract_evidence_ref: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    /// When the verification expires (re-review due).
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChannelReviewResponse {
    pub id: Uuid,
    pub delivery_channel_id: Uuid,
    pub reviewed_by: Uuid,
    pub result: String,
    pub purpose: String,
    pub policy_url: Option<String>,
    pub contract_evidence_ref: Option<String>,
    pub notes: Option<String>,
    pub reviewed_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

// --------------------------------------------------------------------------
// calendar connections
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CalendarConnectionResponse {
    pub id: Uuid,
    pub host_principal_id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub provider_calendar_id: String,
    /// `pending` | `active` | `reauth_required` | `disabled`.
    pub status: String,
    pub include_in_busy: bool,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

// --------------------------------------------------------------------------
// Zoho Mail OAuth client settings (`/mail-accounts/oauth-settings`)
// --------------------------------------------------------------------------

/// `PUT /mail-accounts/oauth-settings` (Admin). The client secret is
/// write-only: omit it to keep the stored secret.
#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ZohoOAuthSettingsRequest {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Exact callback URI registered in the Zoho API Console. Omit to use the
    /// generated `<public origin>/api/v1/integrations/zoho/callback`.
    #[serde(default)]
    pub redirect_uri: Option<String>,
}

impl std::fmt::Debug for ZohoOAuthSettingsRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZohoOAuthSettingsRequest")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("redirect_uri", &self.redirect_uri)
            .finish()
    }
}

/// Effective Zoho Mail OAuth client configuration. The secret is never returned.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ZohoOAuthSettingsResponse {
    /// True when client ID, secret and a valid callback URI are all present.
    pub configured: bool,
    /// `workspace` (saved in Settings), `environment` (server env) or `none`.
    pub source: String,
    pub client_id: Option<String>,
    pub has_secret: bool,
    /// Callback URI currently in effect (stored, env, or generated).
    pub redirect_uri: Option<String>,
    /// Generated default callback URI to register in the Zoho API Console.
    pub default_redirect_uri: String,
    /// Fields still missing/invalid: `client_id`, `client_secret`, `redirect_uri`.
    pub missing: Vec<String>,
    pub updated_at: Option<DateTime<Utc>>,
}
