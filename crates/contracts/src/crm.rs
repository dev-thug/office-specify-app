//! crm module DTOs (spec §6 — source B.2 CRM rows 3552–3563, B.3.3/B.3.4).
//!
//! Request DTOs use `deny_unknown_fields` and never accept system-owned
//! fields (`workspace_id`, `created_by`, `status=accepted`, …) per B.1.1.
//! Status enums serialize to the exact DDL CHECK values (§12.4).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

// ------------------------------------------------------------------ enums

/// `companies.lifecycle_stage` — mirrors the DDL CHECK (§12.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStage {
    Prospect,
    Engaged,
    Qualified,
    Customer,
    Inactive,
}

impl LifecycleStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prospect => "prospect",
            Self::Engaged => "engaged",
            Self::Qualified => "qualified",
            Self::Customer => "customer",
            Self::Inactive => "inactive",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "prospect" => Self::Prospect,
            "engaged" => Self::Engaged,
            "qualified" => Self::Qualified,
            "customer" => Self::Customer,
            "inactive" => Self::Inactive,
            _ => return None,
        })
    }
}

/// `contacts.status` — mirrors the DDL CHECK.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContactStatus {
    New,
    Active,
    Departed,
    Inactive,
}

impl ContactStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Active => "active",
            Self::Departed => "departed",
            Self::Inactive => "inactive",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "new" => Self::New,
            "active" => Self::Active,
            "departed" => Self::Departed,
            "inactive" => Self::Inactive,
            _ => return None,
        })
    }
}

/// `tasks.status` — mirrors the DDL CHECK.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    Done,
    Canceled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Done => "done",
            Self::Canceled => "canceled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "open" => Self::Open,
            "done" => Self::Done,
            "canceled" => Self::Canceled,
            _ => return None,
        })
    }
}

/// `email_addresses.verification_status` — mirrors the DDL CHECK.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Unknown,
    Verified,
    Invalid,
    Risky,
}

/// `email_addresses.kind` — mirrors the DDL CHECK.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AddressKind {
    Person,
    Role,
    Unknown,
}

impl AddressKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Role => "role",
            Self::Unknown => "unknown",
        }
    }
}

/// Send-eligibility preview for an address (B.3.4). Reason codes reuse the
/// stable B.5 codes (`RECIPIENT_SUPPRESSED`, `SEND_BASIS_UNVERIFIED`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MarketingEligibility {
    pub eligible: bool,
    pub reason_codes: Vec<String>,
}

// --------------------------------------------------------------- companies

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateCompanyRequest {
    pub name: String,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub website_url: Option<String>,
    #[serde(default)]
    pub industry: Option<String>,
    #[serde(default)]
    pub country_code: Option<String>,
    #[serde(default)]
    pub lifecycle_stage: Option<LifecycleStage>,
    #[serde(default)]
    pub owner_principal_id: Option<Uuid>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub custom_fields: Option<serde_json::Map<String, serde_json::Value>>,
}

/// PATCH allowlist (B.2: "allowlist 필드·If-Match"). `owner_principal_id` is
/// deliberately absent — owner changes are a separate privileged command
/// (A.6.1). `Option<Option<T>>` distinguishes absent from explicit null
/// (null clears the column).
/// Clearable-field semantics: field absent → `None` (keep), explicit `null`
/// → `Some(None)` (clear), value → `Some(Some(v))`. Plain serde collapses
/// absent and `null` into the same `None`, so these fields need a custom
/// deserializer; the serializer keeps them hash-distinct for idempotency.
fn de_nullable<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

fn ser_nullable<S, T>(v: &Option<Option<T>>, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
    T: serde::Serialize,
{
    match v {
        Some(Some(x)) => x.serialize(s),
        Some(None) => s.serialize_str("$clear"),
        None => s.serialize_none(),
    }
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchCompanyRequest {
    #[serde(default)]
    pub name: Option<String>,
    /// Absent = keep, `null` = clear.
    #[serde(
        default,
        deserialize_with = "de_nullable",
        serialize_with = "ser_nullable"
    )]
    pub domain: Option<Option<String>>,
    /// Absent = keep, `null` = clear.
    #[serde(
        default,
        deserialize_with = "de_nullable",
        serialize_with = "ser_nullable"
    )]
    pub website_url: Option<Option<String>>,
    /// Absent = keep, `null` = clear.
    #[serde(
        default,
        deserialize_with = "de_nullable",
        serialize_with = "ser_nullable"
    )]
    pub industry: Option<Option<String>>,
    /// Absent = keep, `null` = clear.
    #[serde(
        default,
        deserialize_with = "de_nullable",
        serialize_with = "ser_nullable"
    )]
    pub country_code: Option<Option<String>>,
    #[serde(default)]
    pub lifecycle_stage: Option<LifecycleStage>,
    /// Absent = keep, `null` = clear.
    #[serde(
        default,
        deserialize_with = "de_nullable",
        serialize_with = "ser_nullable"
    )]
    pub description: Option<Option<String>>,
    /// Replaces the whole custom_fields object when present.
    #[serde(default)]
    pub custom_fields: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchiveCompanyRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

/// `POST /companies/{id}/merge-requests` — `{id}` is the surviving target.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MergeCompanyRequest {
    pub source_id: Uuid,
    /// Per-field winner: `"source"` or `"target"`; fields not listed keep
    /// the target value. Allowed keys: name, domain, website_url, industry,
    /// country_code, lifecycle_stage, description, custom_fields,
    /// owner_principal_id.
    #[serde(default)]
    pub field_choices: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MergeCompanyResponse {
    pub job_id: Uuid,
    pub source_id: Uuid,
    pub target_id: Uuid,
    /// Always `queued` — the merge is applied by `crm.merge_companies`.
    pub status: String,
}

/// Same-domain / same-name companies surfaced at create time (B.3.3) —
/// advisory only, never auto-merged.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CompanyDuplicateCandidate {
    pub id: Uuid,
    pub name: String,
    pub domain: Option<String>,
    /// Which attributes matched: "domain" and/or "name".
    pub matched_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CompanyResponse {
    pub id: Uuid,
    pub name: String,
    pub domain: Option<String>,
    pub website_url: Option<String>,
    pub industry: Option<String>,
    pub country_code: Option<String>,
    pub lifecycle_stage: LifecycleStage,
    pub owner_principal_id: Option<Uuid>,
    pub description: Option<String>,
    pub custom_fields: serde_json::Value,
    pub archived_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Populated only on `POST /companies` (B.3.3 — `[]` when none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate_candidates: Option<Vec<CompanyDuplicateCandidate>>,
}

// ---------------------------------------------------------------- contacts

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateContactRequest {
    pub name: String,
    #[serde(default)]
    pub company_id: Option<Uuid>,
    #[serde(default)]
    pub job_title: Option<String>,
    #[serde(default)]
    pub department: Option<String>,
    #[serde(default)]
    pub status: Option<ContactStatus>,
    #[serde(default)]
    pub owner_principal_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchContactRequest {
    #[serde(default)]
    pub name: Option<String>,
    /// Absent = keep, null = unlink from the company.
    #[serde(
        default,
        deserialize_with = "de_nullable",
        serialize_with = "ser_nullable"
    )]
    pub company_id: Option<Option<Uuid>>,
    /// Absent = keep, null = clear.
    #[serde(
        default,
        deserialize_with = "de_nullable",
        serialize_with = "ser_nullable"
    )]
    pub job_title: Option<Option<String>>,
    /// Absent = keep, null = clear.
    #[serde(
        default,
        deserialize_with = "de_nullable",
        serialize_with = "ser_nullable"
    )]
    pub department: Option<Option<String>>,
    #[serde(default)]
    pub status: Option<ContactStatus>,
}

/// One `contact_emails` link joined with its `email_addresses` row.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ContactEmailItem {
    pub email_address_id: Uuid,
    pub address: String,
    pub label: Option<String>,
    pub is_primary: bool,
    pub verification_status: VerificationStatus,
    /// Other contacts linked to the same address (role addresses, §6.3).
    pub linked_contact_ids: Vec<Uuid>,
    pub marketing_eligibility: MarketingEligibility,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ContactResponse {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub name: String,
    pub job_title: Option<String>,
    pub department: Option<String>,
    pub owner_principal_id: Option<Uuid>,
    pub status: ContactStatus,
    pub archived_at: Option<DateTime<Utc>>,
    pub emails: Vec<ContactEmailItem>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `POST /contacts/{id}/emails` (B.3.4).
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddContactEmailRequest {
    pub address: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub is_primary: Option<bool>,
    #[serde(default)]
    pub kind: Option<AddressKind>,
}

/// B.3.4 response shape.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AddContactEmailResponse {
    pub email_address_id: Uuid,
    pub address: String,
    pub label: Option<String>,
    pub is_primary: bool,
    pub verification_status: VerificationStatus,
    /// Other contacts already linked to this address (role addresses).
    pub linked_contact_ids: Vec<Uuid>,
    pub marketing_eligibility: MarketingEligibility,
}

/// `PUT /contacts/{id}/primary-email`.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPrimaryEmailRequest {
    pub email_address_id: Uuid,
}

// ------------------------------------------------------------------- notes

/// `POST /notes` — exactly one of `company_id` / `contact_id` / `thread_id`
/// (DDL CHECK; validated as 400 before the DB sees it).
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateNoteRequest {
    #[serde(default)]
    pub company_id: Option<Uuid>,
    #[serde(default)]
    pub contact_id: Option<Uuid>,
    #[serde(default)]
    pub thread_id: Option<Uuid>,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NoteResponse {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub mail_account_id: Option<Uuid>,
    pub created_by: Uuid,
    pub body: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ------------------------------------------------------------------- tasks

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTaskRequest {
    pub title: String,
    /// Defaults to the requesting principal. Assigning another principal
    /// requires Manager/Admin (owner reassignment is privileged, §5.3).
    #[serde(default)]
    pub owner_principal_id: Option<Uuid>,
    #[serde(default)]
    pub company_id: Option<Uuid>,
    #[serde(default)]
    pub contact_id: Option<Uuid>,
    #[serde(default)]
    pub thread_id: Option<Uuid>,
    #[serde(default)]
    pub due_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchTaskRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub status: Option<TaskStatus>,
    /// Absent = keep, null = clear the due date.
    #[serde(
        default,
        deserialize_with = "de_nullable",
        serialize_with = "ser_nullable"
    )]
    pub due_at: Option<Option<DateTime<Utc>>>,
    /// Reassignment — Manager/Admin only.
    #[serde(default)]
    pub owner_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TaskResponse {
    pub id: Uuid,
    pub title: String,
    pub owner_principal_id: Uuid,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub status: TaskStatus,
    pub due_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

// -------------------------------------------------------------- activities

/// One `activities` timeline row (B.2 `/companies/{id}/activities`).
/// `summary` carries non-sensitive labels only — mail-derived rows never
/// expose subject/body here (§5.6, integration-contracts §activities).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ActivityResponse {
    pub id: Uuid,
    pub kind: String,
    pub occurred_at: DateTime<Utc>,
    pub summary: serde_json::Value,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub mail_account_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub booking_id: Option<Uuid>,
    pub actor_principal_id: Option<Uuid>,
}
