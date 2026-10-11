//! Org/auth DTOs (spec §4.5, B.3.1). Requests use `deny_unknown_fields` and
//! never accept system-owned fields (B.1.1).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::shared::{PrincipalKind, Role};

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// `GET /me` payload (B.3.1).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MeResponse {
    pub principal_id: Uuid,
    pub kind: PrincipalKind,
    pub role: Role,
    pub workspace_id: Uuid,
    pub display_name: String,
    pub permission_version: i64,
    pub mail_accounts: Vec<MeMailAccount>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MeMailAccount {
    pub id: Uuid,
    pub address: String,
    pub allowed_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InvitationPreviewResponse {
    pub email: String,
    pub role: Role,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AcceptInvitationRequest {
    pub display_name: String,
    pub password: String,
}

/// Own-session metadata shown in Security settings. Never includes the
/// session token hash or any bearer secret.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UserSessionResponse {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub current: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UserSessionRevokedResponse {
    pub id: Uuid,
    pub revoked_at: DateTime<Utc>,
}

/// `PUT /me/password`. Intentionally does not derive `Debug` so accidental
/// debug formatting cannot reveal either password.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

/// Status-only response. Password values and hashes are never echoed.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PasswordChangedResponse {
    pub changed: bool,
}

// --------------------------------------------------------------------------
// principals (B.2 조직·연동)
// --------------------------------------------------------------------------

/// Principal row as exposed over the API. `version` drives the ETag.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PrincipalResponse {
    pub id: Uuid,
    pub kind: PrincipalKind,
    pub user_id: Option<Uuid>,
    pub name: String,
    pub role: Role,
    /// DDL CHECK: `invited` | `active` | `disabled`.
    pub status: String,
    pub permission_version: i64,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    /// Set when a profile photo exists; GET `/principals/{id}/avatar`
    /// (append `?v=<this>` to bust caches after a change).
    pub avatar_updated_at: Option<DateTime<Utc>>,
    /// Job title, e.g. "대표" or "Sales Manager".
    pub title: Option<String>,
}

/// `PUT /principals/{id}/avatar` result.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AvatarResponse {
    pub principal_id: Uuid,
    pub content_type: String,
    pub avatar_updated_at: DateTime<Utc>,
}

/// Limited active-human roster fields for assignment pickers. Non-admin
/// human sessions never receive user ids or permission state.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PrincipalPickerResponse {
    pub id: Uuid,
    pub kind: PrincipalKind,
    pub name: String,
    pub role: Role,
    pub status: String,
}

/// Admin roster reads return details; other human sessions receive only the
/// fields needed to choose an active owner or host.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(untagged)]
pub enum PrincipalListItem {
    Detail(PrincipalResponse),
    Picker(PrincipalPickerResponse),
}

/// `PATCH /principals/{id}` — role and/or status, If-Match required.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchPrincipalRequest {
    /// Human roles only (`admin` | `manager` | `member`). Agent/system
    /// principals keep their fixed role.
    #[serde(default)]
    pub role: Option<Role>,
    /// `active` | `disabled` only — `invited` is never settable.
    #[serde(default)]
    pub status: Option<String>,
    /// Display name (1..=100 characters), shown in the app and to guests on
    /// public booking pages when the principal hosts meetings.
    #[serde(default)]
    pub name: Option<String>,
    /// Job title (at most 100 characters), shown under the host name on
    /// public booking pages. An empty string clears it.
    #[serde(default)]
    pub title: Option<String>,
}

/// `POST /principals/{id}/disable-requests` — reassignment + pending-work
/// policy for the async disable cleanup (A.7).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DisableRequestRequest {
    /// Active human principal that receives owned records (A.7).
    #[serde(default)]
    pub reassign_to: Option<Uuid>,
    /// Cancel not-yet-dispatched work owned by the principal. Default true
    /// (§5.5 — credential revocation stops pending work by default).
    #[serde(default)]
    pub cancel_pending: Option<bool>,
}

/// `202 Accepted` body for async disable — carries the job resource id.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DisableRequestResponse {
    pub job_id: Uuid,
    pub principal_id: Uuid,
}

// --------------------------------------------------------------------------
// agents + service credentials
// --------------------------------------------------------------------------

/// `POST /agents` — create an agent principal.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAgentRequest {
    pub name: String,
    /// Intended scopes the agent's credentials may carry. Any known scope
    /// is accepted, including `mail.send` and `mail.approve`.
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
}

/// `POST /agents/{id}/credentials` — issue a service credential.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateCredentialRequest {
    pub name: String,
    pub scopes: Vec<String>,
    /// Days until expiry; default 365, max 730.
    #[serde(default)]
    pub expires_in_days: Option<u32>,
    /// Issue a credential without a practical expiry (stored as a far-future
    /// instant; revoke it to end access). Overrides `expires_in_days`.
    #[serde(default)]
    pub never_expires: bool,
}

/// `201` response for credential creation — `secret` appears exactly once
/// and is never stored or replayed (hashed at rest).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CredentialCreatedResponse {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub name: String,
    pub key_prefix: String,
    pub scopes: Vec<String>,
    pub expires_at: DateTime<Utc>,
    /// `osk_…` bearer token — returned ONCE. Absent on idempotent replays
    /// (the secret is never persisted for replay, B.1.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CredentialResponse {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub name: String,
    pub key_prefix: String,
    pub scopes: Vec<String>,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CredentialRevokedResponse {
    pub id: Uuid,
    pub revoked_at: DateTime<Utc>,
}

// --------------------------------------------------------------------------
// member invitations (X2 — one-time link, never emailed)
// --------------------------------------------------------------------------

/// `POST /member-invitations` (Admin).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateInvitationRequest {
    pub email: String,
    /// Human role: `admin` | `manager` | `member`.
    pub role: Role,
}

/// `201` response — `invite_url` embeds the one-time token and is shown
/// once, to copy manually (X2).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InvitationCreatedResponse {
    pub id: Uuid,
    pub email: String,
    pub role: Role,
    pub expires_at: DateTime<Utc>,
    /// One-time invitation URL — the raw token appears only on the fresh
    /// response, never in persisted idempotency storage (B.1.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InvitationResponse {
    pub id: Uuid,
    pub email: String,
    pub role: Role,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InvitationRevokedResponse {
    pub id: Uuid,
    pub revoked_at: DateTime<Utc>,
}

// --------------------------------------------------------------------------
// emergency stop (B.2 운영 row)
// --------------------------------------------------------------------------

/// `PUT /operations/emergency-stop` — `reason` is required and audited.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EmergencyStopRequest {
    /// true = engage the emergency stop (new submissions halt); false =
    /// resume (Admin + reason + audit, A.7/task brief).
    pub enabled: bool,
    pub reason: String,
}

/// Workspace emergency-stop state. `policy_version` is the ETag source.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EmergencyStopResponse {
    pub enabled: bool,
    pub policy_version: i64,
}
