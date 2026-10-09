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
