//! imports module DTOs (spec §6 — owned by the module worker).
//! Wire shapes follow source B.2 `/imports*` + B.3.13; status enums equal
//! the DDL CHECK values verbatim (§12.4).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------- enums

/// `import_jobs.status` CHECK values. For export jobs (mapping
/// `.job_kind == "export"`) `uploaded` doubles as the queued state:
/// `uploaded → completed | failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportStatus {
    Uploaded,
    Validating,
    Ready,
    Committing,
    Completed,
    Failed,
    Canceled,
}

impl ImportStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uploaded => "uploaded",
            Self::Validating => "validating",
            Self::Ready => "ready",
            Self::Committing => "committing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "validating" => Self::Validating,
            "ready" => Self::Ready,
            "committing" => Self::Committing,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "canceled" => Self::Canceled,
            _ => Self::Uploaded,
        }
    }
}

/// `import_rows.status` CHECK values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportRowStatus {
    Valid,
    Invalid,
    Duplicate,
    Imported,
    Skipped,
}

impl ImportRowStatus {
    pub fn parse(s: &str) -> Self {
        match s {
            "invalid" => Self::Invalid,
            "duplicate" => Self::Duplicate,
            "imported" => Self::Imported,
            "skipped" => Self::Skipped,
            _ => Self::Valid,
        }
    }
}

/// How `imports.commit` treats rows still `invalid` after validation
/// (B.3.13: `invalid_row_policy=skip` or `reject`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InvalidRowPolicy {
    Skip,
    Reject,
}

/// How `imports.commit` treats rows marked `duplicate` at validation.
/// `review` (default, B.3.13) leaves them for a human — they are skipped;
/// `link` imports the row but attaches to the already-existing company /
/// email address instead of creating a new entity. Re-importing a
/// suppressed address under `link` therefore recreates the contact while
/// the suppression keeps applying (T32).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateStrategy {
    #[default]
    Review,
    Link,
}

impl DuplicateStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Link => "link",
        }
    }
}

// ------------------------------------------------------------- upload

/// `POST /imports/upload-requests` — name·MIME·size → private upload info.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateUploadRequest {
    /// Display name of the file (metadata only).
    pub filename: String,
    /// Declared media type; must be one of the CSV types the validator
    /// accepts (`text/csv`, `application/csv`, `application/vnd.ms-excel`).
    pub media_type: String,
    /// Declared byte size, 1..=20MB (B.1.1). The actual upload may be
    /// smaller; the stored size is what was received.
    pub byte_size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UploadRequestResponse {
    /// Opaque object key — knowing it grants nothing (§15.2).
    pub storage_key: String,
    /// API path the client PUTs the bytes to.
    pub upload_url: String,
    /// Single-use-style HMAC capability binding workspace, principal, key,
    /// media type, declared size and expiry. Passed as `?token=`.
    pub upload_token: String,
    pub expires_at: DateTime<Utc>,
    pub max_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct StoredObjectResponse {
    pub object_id: Uuid,
    pub storage_key: String,
    pub media_type: String,
    pub byte_size: i64,
    /// SHA-256 hex (B.1.1 `Hash`).
    pub content_hash: String,
}

// ------------------------------------------------------------- imports

/// Column mapping: `{<csv header>: <canonical field>}` where canonical
/// fields come from `domain::imports::CANONICAL_FIELDS`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportMapping {
    pub columns: BTreeMap<String, String>,
    /// First CSV row is a header (default true).
    #[serde(default = "default_true")]
    pub has_header: bool,
    #[serde(default)]
    pub duplicate_strategy: DuplicateStrategy,
}

fn default_true() -> bool {
    true
}

/// `POST /imports` (B.3.13).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateImportRequest {
    pub stored_object_id: Uuid,
    pub mapping: ImportMapping,
}

/// 202 response for `POST /imports` and `POST /exports`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct JobAcceptedResponse {
    pub import_id: Uuid,
    pub status: ImportStatus,
}

/// `GET /imports/{id}` — progress, validation result and (for exports) the
/// output download descriptor.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ImportResponse {
    pub id: Uuid,
    /// `import` | `export` (derived from `mapping.job_kind`).
    pub kind: String,
    pub stored_object_id: Uuid,
    pub requested_by: Uuid,
    pub status: ImportStatus,
    /// SHA-256 hex of the validated snapshot — required on commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_hash: Option<String>,
    pub summary: serde_json::Value,
    /// For completed exports: API path + short-lived token to fetch the
    /// produced CSV. `null` otherwise (§15.2 — never an object key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

/// `import_rows.errors[]` element (stable machine codes in `code`).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ImportRowError {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ImportRowResponse {
    pub row_no: i32,
    pub status: ImportRowStatus,
    pub normalized_data: serde_json::Value,
    pub errors: serde_json::Value,
    #[serde(default)]
    pub company_id: Option<Uuid>,
    #[serde(default)]
    pub contact_id: Option<Uuid>,
}

/// `POST /imports/{id}/commit` (B.3.13).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CommitImportRequest {
    /// Hex SHA-256 returned by the validation job — pins the snapshot.
    pub validation_hash: String,
    pub invalid_row_policy: InvalidRowPolicy,
}

// ------------------------------------------------------------- exports

/// `POST /exports` filters (B.2: 필터·필드 → 202).
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportFilters {
    #[serde(default)]
    pub company_ids: Option<Vec<Uuid>>,
    /// `companies.lifecycle_stage` value.
    #[serde(default)]
    pub lifecycle_stage: Option<String>,
    /// Substring match on company name or domain (case-insensitive).
    #[serde(default)]
    pub q: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateExportRequest {
    /// Non-empty subset of `domain::imports::EXPORTABLE_FIELDS`.
    pub fields: Vec<String>,
    #[serde(default)]
    pub filters: Option<ExportFilters>,
}
