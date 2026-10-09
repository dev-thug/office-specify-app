//! templates module DTOs (spec §6, B.3.5 — owned by the module worker).
//!
//! Request DTOs use `deny_unknown_fields` and never accept system-owned
//! fields (workspace_id, created_by, content_hash, version_no) — B.1.1.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// `POST /api/v1/email-templates` (B.3.5).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateEmailTemplateRequest {
    pub name: String,
}

/// `PATCH /api/v1/email-templates/{id}` — metadata only; content changes go
/// through new template_versions (immutable).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchEmailTemplateRequest {
    pub name: Option<String>,
    /// Set `archived` to soft-archive/unarchive the logical template.
    pub archived: Option<bool>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EmailTemplateResponse {
    pub template_id: Uuid,
    pub name: String,
    pub created_by: Uuid,
    pub archived_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EmailTemplateListItem {
    pub template_id: Uuid,
    pub name: String,
    pub archived_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// `POST /api/v1/email-templates/{id}/versions` (B.3.5). `variable_schema`
/// follows the JSON-Schema-shaped allowlist (object/properties/required,
/// `additionalProperties: false`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTemplateVersionRequest {
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub variable_schema: serde_json::Value,
}

/// B.3.5 response: `{template_version_id, version_no, content_hash}`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct CreateTemplateVersionResponse {
    pub template_version_id: Uuid,
    pub version_no: i64,
    /// SHA-256 over canonical content, 64-char lowercase hex.
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TemplateVersionResponse {
    pub template_version_id: Uuid,
    pub template_id: Uuid,
    pub version_no: i64,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub variable_schema: serde_json::Value,
    pub content_hash: String,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TemplateVersionListItem {
    pub template_version_id: Uuid,
    pub version_no: i64,
    pub content_hash: String,
    pub created_at: DateTime<Utc>,
}

/// `POST /api/v1/template-versions/{id}/render` (B.2). Sample variables only;
/// system-generated URLs are injected server-side.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RenderTemplateVersionRequest {
    #[serde(default)]
    pub variables: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RenderTemplateVersionResponse {
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub warnings: Vec<String>,
}
