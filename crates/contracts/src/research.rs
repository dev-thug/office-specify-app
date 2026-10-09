//! research module DTOs (spec §6 — owned by the module worker).
//! Wire shapes follow source B.3.12; status enums equal the DDL CHECK
//! values verbatim (§12.4).

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

// ---------------------------------------------------------------- enums

/// `lead_proposals.status` CHECK values.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LeadProposalStatus {
    Pending,
    Accepted,
    Rejected,
    NeedsReview,
}

impl LeadProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::NeedsReview => "needs_review",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "accepted" => Self::Accepted,
            "rejected" => Self::Rejected,
            "needs_review" => Self::NeedsReview,
            _ => return None,
        })
    }
}

/// `research_evidence.evidence_type` CHECK values: a fact directly observed
/// at the source vs. a model inference. `inferred` never implies verified
/// (§13.4).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceType {
    Observed,
    Inferred,
}

impl EvidenceType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Inferred => "inferred",
        }
    }
}

/// `research_evidence.review_status` CHECK values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Pending,
    Verified,
    Rejected,
}

impl ReviewStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Verified => "verified",
            Self::Rejected => "rejected",
        }
    }
}

/// Decision kind for `POST /lead-proposals/{id}/decisions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Accept,
    Reject,
}

/// Decision kind for `POST /research-evidence/{id}/reviews` — only the two
/// terminal review states are reachable (pending → verified|rejected).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceReviewDecision {
    Verified,
    Rejected,
}

// ------------------------------------------------- lead-proposals input

/// One evidence entry attached to a proposal (B.3.12). `source_url` must
/// pass the research URL policy (public http(s) only — §15.1 SSRF table).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceInput {
    pub field_path: String,
    pub source_url: String,
    pub observed_at: DateTime<Utc>,
    pub value: serde_json::Value,
    pub evidence_type: EvidenceType,
    #[serde(default)]
    pub excerpt: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
}

/// A single lead candidate inside a `POST /lead-proposals` batch (B.3.12).
///
/// `proposed_data` is a free-form object; the canonical contact shape the
/// accept path understands is
/// `proposed_data.contacts: [{name, job_title?, department?, email?}]`.
/// Email candidates are stored as-is — never fabricated (§13.4) — and get a
/// `communication_bases` row with `status='pending'` on accept, so they are
/// not sendable until a human review validates a basis.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LeadProposalInput {
    #[serde(default)]
    pub external_key: Option<String>,
    pub company_name: String,
    #[serde(default)]
    pub domain: Option<String>,
    /// JSON object of proposed field values (e.g. industry, contacts[]).
    #[serde(default)]
    pub proposed_data: serde_json::Value,
    /// At least one evidence entry is required (§13.4 필수 항목).
    #[serde(default)]
    pub evidence: Vec<EvidenceInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateLeadProposalsRequest {
    /// Owning agent run (optional; must exist in this workspace).
    #[serde(default)]
    pub agent_run_id: Option<Uuid>,
    /// Max 100 per request (§13.4). All-or-nothing: one invalid item fails
    /// the whole batch with 422 (B.3.12).
    pub proposals: Vec<LeadProposalInput>,
}

/// Per-item result of a batch create (B.3.12).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LeadProposalResultItem {
    pub id: Uuid,
    pub status: LeadProposalStatus,
    /// Existing companies that look like the same entity (by domain/name).
    /// Never auto-merged — a human decides (§13.4).
    pub duplicate_company_ids: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreateLeadProposalsResponse {
    pub proposals: Vec<LeadProposalResultItem>,
}

/// `GET /lead-proposals` item / `GET /lead-proposals/{id}` body.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LeadProposalResponse {
    pub id: Uuid,
    pub agent_run_id: Option<Uuid>,
    pub proposed_by: Uuid,
    pub external_key: Option<String>,
    pub company_name: String,
    pub domain: Option<String>,
    pub proposed_data: serde_json::Value,
    pub status: LeadProposalStatus,
    pub accepted_company_id: Option<Uuid>,
    pub reviewed_by: Option<Uuid>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub duplicate_company_ids: Vec<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EvidenceResponse {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub lead_proposal_id: Option<Uuid>,
    pub field_path: String,
    pub source_url: String,
    pub observed_at: DateTime<Utc>,
    pub excerpt: Option<String>,
    pub value: serde_json::Value,
    pub evidence_type: EvidenceType,
    pub confidence: Option<f64>,
    pub review_status: ReviewStatus,
    pub created_by: Uuid,
    pub reviewed_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// `GET /lead-proposals/{id}` — proposal plus its attached evidence.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LeadProposalDetailResponse {
    #[serde(flatten)]
    pub proposal: LeadProposalResponse,
    pub evidence: Vec<EvidenceResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LeadProposalDecisionRequest {
    pub decision: DecisionKind,
    /// Accept-only merge choice: attach the accepted data to this existing
    /// company instead of creating a new one (§13.4 — never auto-merge).
    #[serde(default)]
    pub company_id: Option<Uuid>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LeadProposalDecisionResponse {
    pub id: Uuid,
    pub status: LeadProposalStatus,
    pub accepted_company_id: Option<Uuid>,
    #[serde(default)]
    pub contact_ids: Vec<Uuid>,
    #[serde(default)]
    pub email_address_ids: Vec<Uuid>,
    pub version: i64,
}

// -------------------------------------------------------- evidence input

/// `POST /research-evidence` (B.2). Exactly one of `company_id`,
/// `contact_id`, `lead_proposal_id` (DDL CHECK).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateEvidenceRequest {
    #[serde(default)]
    pub company_id: Option<Uuid>,
    #[serde(default)]
    pub contact_id: Option<Uuid>,
    #[serde(default)]
    pub lead_proposal_id: Option<Uuid>,
    pub field_path: String,
    pub source_url: String,
    pub observed_at: DateTime<Utc>,
    pub value: serde_json::Value,
    pub evidence_type: EvidenceType,
    #[serde(default)]
    pub excerpt: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
}

/// Existing CRM target accepted by the MCP `crm_propose_update` tool.
/// Updates are stored as pending lead proposals; this DTO never applies the
/// supplied field values to the company/contact row.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ResearchTargetKind {
    Company,
    Contact,
}

impl ResearchTargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Company => "company",
            Self::Contact => "contact",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResearchUpdateTarget {
    pub kind: ResearchTargetKind,
    pub id: Uuid,
}

/// MCP-only proposal request. `idempotency_key` is explicit because MCP
/// transport has no HTTP header; the application binds it to this principal
/// and the full request body before storing the pending diff.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposeUpdateRequest {
    pub idempotency_key: String,
    pub agent_run_id: Uuid,
    pub target: ResearchUpdateTarget,
    pub expected_version: i64,
    /// New field values only. The application captures current values into
    /// the stored `{before, after}` diff after locking the target row.
    pub field_changes: BTreeMap<String, serde_json::Value>,
    pub evidence: Vec<EvidenceInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceReviewRequest {
    pub decision: EvidenceReviewDecision,
    #[serde(default)]
    pub reason: Option<String>,
}
